use std::path::{Path, PathBuf};
use std::sync::Arc;

use dashmap::DashMap;
use once_cell::sync::OnceCell;
use revolt_database::{
    dj,
    events::client::EventV1,
    util::{idempotency::IdempotencyKey, reference::Reference},
    Database, User,
};
use revolt_models::v0;
use revolt_result::{create_error, Error, Result};
use rocket::{form::Form, fs::TempFile, serde::json::Json, State};
use tokio::sync::Mutex;

use crate::dj::{paths, publish_to_members, resolve_dj_channel, transcode, DjEngine};

/// Upper bound on the amount of time an upload may take, including the queue
/// wait for the transcode semaphore
const UPLOAD_DEADLINE: std::time::Duration = std::time::Duration::from_secs(300);

static UPLOAD_LOCKS: OnceCell<DashMap<String, Arc<Mutex<()>>>> = OnceCell::new();

fn upload_lock(channel: &str) -> Arc<Mutex<()>> {
    UPLOAD_LOCKS
        .get_or_init(DashMap::new)
        .entry(channel.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// # Add Track to DJ Queue
///
/// Uploads an audio file to the DJ channel's queue.
///
/// The upload is transcoded to MP3 (`-vn`, capped at the channel's maximum
/// track duration) and stored unencrypted on local disk until it has played.
/// Requires an active membership of the channel.
// The multipart body is not representable in the OpenAPI schema
#[openapi(tag = "DJ", skip)]
#[post("/<target>/dj/queue?<title>", data = "<file>")]
pub async fn queue(
    db: &State<Database>,
    engine: &State<DjEngine>,
    user: User,
    target: Reference<'_>,
    title: Option<String>,
    file: Form<TempFile<'_>>,
    _idempotency: IdempotencyKey,
) -> Result<Json<v0::DjTrack>> {
    tokio::time::timeout(
        UPLOAD_DEADLINE,
        upload(db, engine, &user, target, title, file),
    )
    .await
    .map_err(|_| create_error!(DjTranscodeFailed))?
}

async fn upload(
    db: &Database,
    engine: &DjEngine,
    user: &User,
    target: Reference<'_>,
    title: Option<String>,
    file: Form<TempFile<'_>>,
) -> Result<Json<v0::DjTrack>> {
    let file = file.into_inner();
    let channel = target.as_channel(db).await?;
    let settings = resolve_dj_channel(&channel)?;

    if !dj::is_member(channel.id(), &user.id).await? {
        return Err(create_error!(NotInDjChannel));
    }

    let Some(source) = file.path().map(Path::to_path_buf) else {
        return Err(create_error!(FailedValidation {
            error: "file: required".to_string()
        }));
    };

    if file.len() == 0 {
        return Err(create_error!(FileTooSmall));
    }

    // The channel limit is clamped to the uploader's tier limit
    let max_upload_bytes = max_upload_bytes(&settings, user).await;

    if file.len() > max_upload_bytes {
        return Err(create_error!(FileTooLarge {
            max: max_upload_bytes as usize
        }));
    }

    let id = format!(
        "{}-{}",
        dj::next_seq(channel.id()).await?,
        ulid::Ulid::new()
    );
    let destination = paths::track_path(channel.id(), &id);

    paths::create_channel_dir(channel.id()).map_err(|error| {
        log::error!("Failed to create DJ storage directory: {error}");
        create_error!(DjUnavailable)
    })?;

    if let Err(error) = transcode::to_mp3(&source, &destination, settings.max_track_seconds).await {
        return Err(upload_failed(channel.id(), &user.id, &destination, error).await);
    }

    let duration_ms = match transcode::probe_duration_ms(&destination).await {
        Ok(duration_ms) => duration_ms,
        Err(error) => return Err(upload_failed(channel.id(), &user.id, &destination, error).await),
    };

    if let Err(error) = dj::check_duration(&settings, duration_ms) {
        return Err(upload_failed(channel.id(), &user.id, &destination, error).await);
    }

    let track = v0::DjTrack {
        id: id.clone(),
        file: paths::track_url_path(channel.id(), &id),
        title: title
            .map(|title| title.trim().to_string())
            .filter(|title| !title.is_empty())
            .or_else(|| {
                file.raw_name()
                    .and_then(|name| name.as_str().map(str::to_string))
                    .filter(|name| !name.is_empty())
            })
            .unwrap_or_else(|| "Untitled".to_string()),
        duration_ms,
        uploader: user.id.clone(),
    };

    // Caps are checked under a per-channel lock so concurrent uploads cannot
    // both pass the check and then both be queued.
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
            return Err(upload_failed(channel.id(), &user.id, &destination, error).await);
        }

        match dj::push_track(channel.id(), &track).await {
            Ok(position) => position,
            Err(error) => {
                return Err(upload_failed(channel.id(), &user.id, &destination, error).await)
            }
        }
    };

    engine.start(channel.id(), settings.clone()).await;

    publish_to_members(
        channel.id(),
        EventV1::DjTrackQueued {
            channel: channel.id().to_string(),
            track: track.clone(),
            position,
        },
    )
    .await;

    Ok(Json(track))
}

/// Maximum size of an upload, in bytes, after clamping to the user's tier
async fn max_upload_bytes(settings: &v0::DjInformation, user: &User) -> u64 {
    const MIB: u64 = 1024 * 1024;

    let channel_limit = settings.max_file_mb as u64 * MIB;
    let tier_limit = user
        .limits()
        .await
        .file_upload_size_limit
        .get("attachments")
        .copied();

    match tier_limit {
        Some(tier_limit) => channel_limit.min(tier_limit as u64),
        None => channel_limit,
    }
}

/// Amount of tracks the user has queued or is currently playing
fn member_track_count(
    queue: &[v0::DjTrack],
    current: Option<&v0::DjNowPlaying>,
    user: &str,
) -> u32 {
    let queued = queue.iter().filter(|track| track.uploader == user).count() as u32;
    let playing = current
        .filter(|current| current.track.uploader == user)
        .map(|_| 1)
        .unwrap_or_default();

    queued + playing
}

async fn directory_size(dir: &Path) -> u64 {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return 0;
    };

    let mut total = 0u64;

    while let Ok(Some(entry)) = entries.next_entry().await {
        if let Ok(metadata) = entry.metadata().await {
            if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            }
        }
    }

    total
}

/// Delete a partially written file and tell the uploader why it was rejected
async fn upload_failed(channel: &str, user: &str, path: &PathBuf, error: Error) -> Error {
    if let Err(error) = tokio::fs::remove_file(path).await {
        if error.kind() != std::io::ErrorKind::NotFound {
            log::warn!(
                "Failed to remove rejected DJ upload {}: {error}",
                path.display()
            );
        }
    }

    let reason = format!("{:?}", error.error_type);

    EventV1::DjUploadFailed {
        channel: channel.to_string(),
        reason,
    }
    .private(user.to_string())
    .await;

    error
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(uploader: &str) -> v0::DjTrack {
        v0::DjTrack {
            id: ulid::Ulid::new().to_string(),
            file: "/dj/c/x.mp3".to_string(),
            title: "t".to_string(),
            duration_ms: 1000,
            uploader: uploader.to_string(),
        }
    }

    #[test]
    fn counts_member_tracks_including_the_one_playing() {
        let queue = vec![track("a"), track("b"), track("a")];
        let current = v0::DjNowPlaying {
            track: track("a"),
            started_at: 0,
        };

        assert_eq!(member_track_count(&queue, Some(&current), "a"), 3);
        assert_eq!(member_track_count(&queue, Some(&current), "b"), 1);
        assert_eq!(member_track_count(&queue, None, "a"), 2);
        assert_eq!(member_track_count(&queue, None, "c"), 0);
    }
}
