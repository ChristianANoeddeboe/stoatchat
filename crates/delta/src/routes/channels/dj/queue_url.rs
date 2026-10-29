//! Queue a YouTube link instead of an uploaded file.
//!
//! This is the link sibling of [`super::queue`]: the same permission checks,
//! ratelimit bucket, duration and storage caps, and the same
//! `DjTrackQueued` → engine-start path. The only difference is where the audio
//! comes from: `yt-dlp` fetches one video's audio stream, which is then
//! transcoded exactly like an upload.
//!
//! Only the **single linked video** is queued. Playlist parameters in the URL
//! are dropped by [`crate::dj::youtube::parse_video_url`]; there is no playlist
//! import.

use std::path::Path;
use std::time::Duration;

use revolt_database::{
    dj,
    events::client::EventV1,
    util::{idempotency::IdempotencyKey, permissions::perms, reference::Reference},
    Database, User,
};
use revolt_models::v0;
use revolt_permissions::calculate_channel_permissions;
use revolt_result::{create_error, Error, Result};
use rocket::{post, serde::json::Json, State};
use serde::Deserialize;

use super::check_queue_permissions;
use super::queue::{
    directory_size, max_upload_bytes, member_track_count, partial_path, truncate_title,
    upload_failed, upload_lock,
};
use crate::dj::{paths, publish_to_members, resolve_dj_channel, transcode, youtube, DjEngine};

/// Upper bound on the whole link queue operation, including the download
///
/// Downloads have their own (shorter) timeout; this is the backstop which keeps
/// a stuck request from holding its task forever.
const QUEUE_URL_DEADLINE: Duration = Duration::from_secs(300);

/// Body of a link queue request
#[derive(Debug, Clone, Deserialize)]
pub struct DataQueueDjUrl {
    /// https://youtube.com/watch?v=…, https://youtu.be/…, …
    pub url: String,
    /// Display title, defaults to the video's own title
    #[serde(default)]
    pub title: Option<String>,
}

/// # Add a YouTube Link to the DJ Queue
///
/// Queues the single video a YouTube link points at. Playlist, channel and
/// search links are rejected; playlist parameters on a video link are ignored.
///
/// The audio is downloaded with `yt-dlp` and transcoded to MP3 (capped at the
/// channel's maximum track duration) exactly like an upload, so the same
/// per-member, queue-depth and storage caps apply. Requires an active
/// membership of the channel.
#[openapi(tag = "DJ", skip)]
#[post("/<target>/dj/queue/url", data = "<data>")]
pub async fn queue_url(
    db: &State<Database>,
    engine: &State<DjEngine>,
    user: User,
    target: Reference<'_>,
    data: Json<DataQueueDjUrl>,
    _idempotency: IdempotencyKey,
) -> Result<Json<v0::DjTrack>> {
    let data = data.into_inner();

    tokio::time::timeout(QUEUE_URL_DEADLINE, enqueue(db, engine, &user, target, data))
        .await
        .map_err(|_| create_error!(DjYoutubeFailed))?
}

async fn enqueue(
    db: &Database,
    engine: &DjEngine,
    user: &User,
    target: Reference<'_>,
    data: DataQueueDjUrl,
) -> Result<Json<v0::DjTrack>> {
    let channel = target.as_channel(db).await?;
    let settings = resolve_dj_channel(&channel)?;

    // Membership alone is not enough: a member who lost `ViewChannel` or the
    // right to upload must not be able to queue anything either.
    let mut query = perms(db, user).channel(&channel);
    let permissions = calculate_channel_permissions(&mut query).await;
    check_queue_permissions(&permissions)?;

    if !dj::is_member(channel.id(), &user.id).await? {
        return Err(create_error!(NotInDjChannel));
    }

    // Validate before anything is spawned: an unsupported link is a 400 and
    // never reaches a binary.
    let video = youtube::parse_video_url(&data.url)?;

    let metadata = youtube::fetch_metadata(&video).await?;

    // Reject a too-long track before downloading it: the download is the
    // expensive part, and the cap is already known from the metadata.
    youtube::check_duration(&settings, &metadata)?;

    let max_bytes = max_upload_bytes(&settings, user).await;

    let id = format!(
        "{}-{}",
        dj::next_seq(channel.id()).await?,
        ulid::Ulid::new()
    );
    let destination = paths::track_path(channel.id(), &id);
    // Transcoded to a `.part` file and renamed once probed, so a half-written
    // MP3 is never visible to the queue or to the sweeper's keep set.
    let partial = partial_path(&destination);
    let directory = paths::channel_dir(channel.id());

    paths::create_channel_dir(channel.id())
        .await
        .map_err(|error| {
            log::error!("Failed to create DJ storage directory: {error}");
            create_error!(DjUnavailable)
        })?;

    let stem = format!("{id}.source");
    let source = match youtube::download_audio(&video, &directory, &stem, max_bytes).await {
        Ok(source) => source,
        Err(error) => {
            return Err(reject(channel.id(), &user.id, &[&partial], &destination, error).await)
        }
    };

    if let Err(error) = transcode::to_mp3(&source, &partial, settings.max_track_seconds).await {
        return Err(reject(
            channel.id(),
            &user.id,
            &[&source, &partial],
            &destination,
            error,
        )
        .await);
    }

    // The downloaded file is no longer needed once it has been transcoded
    remove_file(&source).await;

    let duration_ms = match transcode::probe_duration_ms(&partial).await {
        Ok(duration_ms) => duration_ms,
        Err(error) => {
            return Err(reject(channel.id(), &user.id, &[&partial], &destination, error).await)
        }
    };

    if let Err(error) = dj::check_duration(&settings, duration_ms) {
        return Err(reject(channel.id(), &user.id, &[&partial], &destination, error).await);
    }

    // `-t` already bounds the output, but a source which ignored the duration
    // hint can still transcode past the cap; the final file is the authority.
    if let Ok(metadata) = tokio::fs::metadata(&partial).await {
        if metadata.len() > max_bytes {
            let error = create_error!(FileTooLarge {
                max: max_bytes as usize
            });

            return Err(reject(channel.id(), &user.id, &[&partial], &destination, error).await);
        }
    }

    if let Err(error) = tokio::fs::rename(&partial, &destination).await {
        log::error!(
            "Failed to publish DJ link track {}: {error}",
            destination.display()
        );

        return Err(reject(
            channel.id(),
            &user.id,
            &[&partial],
            &destination,
            create_error!(DjUnavailable),
        )
        .await);
    }

    let track = v0::DjTrack {
        id: id.clone(),
        file: paths::track_url_path(channel.id()),
        title: resolve_title(data.title.as_deref(), &metadata, video.as_str()),
        duration_ms,
        uploader: user.id.clone(),
    };

    // Caps are checked under the same per-channel lock as uploads, so a link
    // and an upload racing each other cannot both pass the check.
    let lock = upload_lock(channel.id());

    let position = {
        let _guard = lock.lock().await;

        let queue = dj::get_queue(channel.id()).await?;
        let current = dj::get_now(channel.id()).await?;
        let new_track_bytes = tokio::fs::metadata(&destination)
            .await
            .map(|meta| meta.len())
            .unwrap_or_default();

        let total_bytes = directory_size(&paths::channel_dir(channel.id()))
            .await
            .saturating_sub(new_track_bytes);

        if let Err(error) = dj::check_queue_caps(
            &settings,
            dj::QueueCaps {
                member_tracks: member_track_count(&queue, current.as_ref(), &user.id),
                queued: queue.len() as u32,
                total_bytes,
                new_track_bytes,
            },
        ) {
            return Err(reject(channel.id(), &user.id, &[], &destination, error).await);
        }

        match dj::push_track(channel.id(), &track).await {
            Ok(position) => position,
            Err(error) => {
                return Err(reject(channel.id(), &user.id, &[], &destination, error).await)
            }
        }
    };

    // Publish before waking the engine: otherwise `TrackStarted` can reach
    // clients before the `TrackQueued` which introduced the track.
    publish_to_members(
        channel.id(),
        EventV1::DjTrackQueued {
            channel: channel.id().to_string(),
            track: track.clone(),
            position,
        },
    )
    .await;

    engine.start(channel.id(), settings.clone()).await;

    Ok(Json(track))
}

/// Title for a link: the requested title, else the video title, else the id
fn resolve_title(requested: Option<&str>, metadata: &youtube::VideoMetadata, id: &str) -> String {
    truncate_title(&youtube::resolve_title(requested, metadata, id))
}

async fn remove_file(path: &Path) {
    if let Err(error) = tokio::fs::remove_file(path).await {
        if error.kind() != std::io::ErrorKind::NotFound {
            log::warn!("Failed to remove DJ file {}: {error}", path.display());
        }
    }
}

/// Delete every staged file and tell the uploader why the link was rejected
async fn reject(
    channel: &str,
    user: &str,
    staged: &[&Path],
    destination: &Path,
    error: Error,
) -> Error {
    for path in staged {
        remove_file(path).await;
    }

    upload_failed(channel, user, destination, error).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use revolt_result::ErrorType;

    use super::super::queue::{upload_failure_reason, MAX_TITLE_CHARS};

    fn metadata(title: &str) -> youtube::VideoMetadata {
        youtube::VideoMetadata {
            title: Some(title.to_string()),
            duration_ms: Some(1000),
        }
    }

    #[test]
    fn the_video_title_is_used_when_none_is_given() {
        assert_eq!(
            resolve_title(None, &metadata("Video title"), "bhupEBurUQI"),
            "Video title"
        );
    }

    #[test]
    fn an_explicit_title_wins() {
        assert_eq!(
            resolve_title(
                Some("  My title  "),
                &metadata("Video title"),
                "bhupEBurUQI"
            ),
            "My title"
        );
    }

    #[test]
    fn a_blank_title_falls_back_to_the_video() {
        assert_eq!(
            resolve_title(Some("   "), &metadata("Video title"), "bhupEBurUQI"),
            "Video title"
        );
    }

    #[test]
    fn the_link_is_the_fallback_title() {
        let empty = youtube::VideoMetadata::default();

        assert_eq!(resolve_title(None, &empty, "bhupEBurUQI"), "bhupEBurUQI");
    }

    #[test]
    fn long_titles_are_truncated_on_a_character_boundary() {
        let long = "é".repeat(MAX_TITLE_CHARS + 40);

        let title = resolve_title(None, &metadata(&long), "bhupEBurUQI");

        assert_eq!(title.chars().count(), MAX_TITLE_CHARS);
        assert!(title.chars().all(|c| c == 'é'));
    }

    #[test]
    fn a_failed_link_is_reported_without_leaking_internal_types() {
        for error in [
            ErrorType::DjYoutubeUnsupported,
            ErrorType::DjYoutubeFailed,
            ErrorType::DjYoutubeUnavailable,
            ErrorType::DjTrackTooLong { max_seconds: 5 },
            ErrorType::DjUploadLimit { max: 3 },
        ] {
            let reason = upload_failure_reason(&error);

            assert!(!reason.is_empty());
            assert!(
                !reason.contains("ErrorType") && !reason.contains('{'),
                "{} leaked the error type",
                reason
            );
        }
    }
}
