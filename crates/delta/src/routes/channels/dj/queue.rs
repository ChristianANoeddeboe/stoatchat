use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use once_cell::sync::OnceCell;
use revolt_database::{
    dj,
    events::client::EventV1,
    util::{idempotency::IdempotencyKey, permissions::perms, reference::Reference},
    Database, User,
};
use revolt_models::v0;
use revolt_permissions::calculate_channel_permissions;
use revolt_result::{create_error, Error, ErrorType, Result};
use rocket::{
    form::Form,
    fs::TempFile,
    http::Status,
    request::{FromRequest, Outcome, Request},
    response::Responder,
    serde::json::Json,
    State,
};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

use super::check_queue_permissions;
use crate::dj::{paths, publish_to_members, resolve_dj_channel, transcode, DjEngine};

/// Upper bound on the amount of time an upload may take, including the queue
/// wait for the transcode semaphore
const UPLOAD_DEADLINE: Duration = Duration::from_secs(300);

/// Maximum amount of uploads which may be buffered or transcoded at once
///
/// Each in-flight upload can hold a temporary file of up to `max_file_mb`, so
/// this is the knob which keeps a handful of clients from filling the disk.
const MAX_CONCURRENT_UPLOADS: usize = 4;

/// Maximum amount of time a request waits for an upload slot
const UPLOAD_SLOT_TIMEOUT: Duration = Duration::from_secs(10);

/// Hard ceiling for a single DJ upload, in bytes
///
/// Mirrors the `max_file_mb` validation bound (128 MiB) and Rocket's file limit.
const MAX_UPLOAD_BYTES: u64 = 128 * 1024 * 1024;

/// Framing overhead allowed on top of `max_file_mb` for a multipart body
const MULTIPART_OVERHEAD_BYTES: u64 = 64 * 1024;

/// Maximum amount of characters kept from a track title
pub(super) const MAX_TITLE_CHARS: usize = 100;

static UPLOAD_LOCKS: OnceCell<DashMap<String, Arc<Mutex<()>>>> = OnceCell::new();
static UPLOAD_SLOTS: OnceCell<Arc<Semaphore>> = OnceCell::new();

pub(super) fn upload_lock(channel: &str) -> Arc<Mutex<()>> {
    UPLOAD_LOCKS
        .get_or_init(DashMap::new)
        .entry(channel.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Whether a declared body length fits inside `limit` (plus multipart framing)
fn length_fits(limit: u64, declared: u64) -> bool {
    declared <= limit.saturating_add(MULTIPART_OVERHEAD_BYTES)
}

fn upload_slots() -> Arc<Semaphore> {
    UPLOAD_SLOTS
        .get_or_init(|| Arc::new(Semaphore::new(MAX_CONCURRENT_UPLOADS)))
        .clone()
}

/// Rejection from [`DjUploadLimit`]
#[derive(Debug)]
pub struct DjUploadRejection(Error);

impl<'r> Responder<'r, 'static> for DjUploadRejection {
    fn respond_to(self, request: &'r Request<'_>) -> rocket::response::Result<'static> {
        // Delegates to the revolt error responder, so the body and status match
        // every other DJ error (`FileTooLarge` is a 413, validation a 400).
        self.0.respond_to(request)
    }
}

/// Pre-buffering upload guard
///
/// Rocket only enforces its global `file` limit while writing the body to disk
/// and `max_file_mb` is otherwise checked after the whole upload has landed in
/// a temporary file. This guard runs **before** the form data guard, so an
/// upload which declares a `Content-Length` larger than the channel allows is
/// rejected without a byte reaching the disk, and only a bounded number of
/// uploads may be in flight at all.
pub struct DjUploadLimit {
    #[allow(dead_code)]
    permit: OwnedSemaphorePermit,
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for DjUploadLimit {
    type Error = DjUploadRejection;

    async fn from_request(request: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        let Some(db) = request.rocket().state::<Database>() else {
            return Outcome::Error((
                Status::InternalServerError,
                DjUploadRejection(create_error!(InternalError)),
            ));
        };

        let reference = match request.param::<Reference<'r>>(0) {
            Some(Ok(reference)) => reference,
            // Not a channel route: let the handler produce the real error
            _ => return Outcome::Forward(Status::NotFound),
        };

        let channel = match reference.as_channel(db).await {
            Ok(channel) => channel,
            Err(error) => {
                return Outcome::Error((Status::BadRequest, DjUploadRejection(error)));
            }
        };

        // A channel without (enabled) DJ settings cannot be uploaded to at all;
        // the handler reports the precise error (`NotADjChannel`/`DjDisabled`),
        // this guard only bounds the body and never changes that outcome.
        let limit = channel
            .dj()
            .filter(|dj| dj.enabled)
            .map(|dj| (dj.max_file_mb as u64).saturating_mul(1024 * 1024))
            .unwrap_or(MAX_UPLOAD_BYTES)
            .min(MAX_UPLOAD_BYTES);

        let declared = request
            .headers()
            .get_one("Content-Length")
            .and_then(|value| value.parse::<u64>().ok());

        match declared {
            // A chunked upload has no declared length: refuse to buffer an
            // unbounded body rather than discovering its size on disk.
            None => {
                return Outcome::Error((
                    Status::LengthRequired,
                    DjUploadRejection(create_error!(FailedValidation {
                        error: "Content-Length: required for DJ uploads".to_string()
                    })),
                ));
            }
            Some(length) if !length_fits(limit, length) => {
                return Outcome::Error((
                    Status::PayloadTooLarge,
                    DjUploadRejection(create_error!(FileTooLarge {
                        max: limit as usize
                    })),
                ));
            }
            Some(_) => {}
        }

        match tokio::time::timeout(UPLOAD_SLOT_TIMEOUT, upload_slots().acquire_owned()).await
        {
            Ok(Ok(permit)) => Outcome::Success(DjUploadLimit { permit }),
            Ok(Err(_)) => Outcome::Error((
                Status::ServiceUnavailable,
                DjUploadRejection(create_error!(DjUnavailable)),
            )),
            Err(_) => Outcome::Error((
                Status::ServiceUnavailable,
                DjUploadRejection(create_error!(DjUnavailable)),
            )),
        }
    }
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
    // Must stay before `file`: request guards run before data guards, so this
    // rejects an oversized upload before Rocket buffers it to a temp file.
    _limit: DjUploadLimit,
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

    // Membership alone is not enough: a member who lost `ViewChannel` or the
    // right to upload must not be able to queue anything either.
    let mut query = perms(db, user).channel(&channel);
    let permissions = calculate_channel_permissions(&mut query).await;
    check_queue_permissions(&permissions)?;

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
    // Transcode to a `.part` file and rename once the output has been probed,
    // so a half-written MP3 is never visible to the queue or to the sweeper's
    // keep set.
    let partial = partial_path(&destination);

    paths::create_channel_dir(channel.id()).await.map_err(|error| {
        log::error!("Failed to create DJ storage directory: {error}");
        create_error!(DjUnavailable)
    })?;

    if let Err(error) = transcode::to_mp3(&source, &partial, settings.max_track_seconds).await {
        return Err(upload_failed(channel.id(), &user.id, &partial, error).await);
    }

    let duration_ms = match transcode::probe_duration_ms(&partial).await {
        Ok(duration_ms) => duration_ms,
        Err(error) => return Err(upload_failed(channel.id(), &user.id, &partial, error).await),
    };

    if let Err(error) = dj::check_duration(&settings, duration_ms) {
        return Err(upload_failed(channel.id(), &user.id, &partial, error).await);
    }

    if let Err(error) = tokio::fs::rename(&partial, &destination).await {
        log::error!(
            "Failed to publish DJ upload {}: {error}",
            destination.display()
        );

        return Err(upload_failed(
            channel.id(),
            &user.id,
            &partial,
            create_error!(DjUnavailable),
        )
        .await);
    }

    let track = v0::DjTrack {
        id: id.clone(),
        file: paths::track_url_path(channel.id()),
        title: resolve_title(title, &file),
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

/// Temporary path an upload is transcoded to before it is published
///
/// The suffix is deliberately not `.mp3`: nothing (queue, keep set, client)
/// must ever pick a half-written file up.
pub(super) fn partial_path(destination: &Path) -> PathBuf {
    let mut path = destination.as_os_str().to_os_string();
    path.push(".part");
    PathBuf::from(path)
}

/// Maximum size of an upload, in bytes, after clamping to the user's tier
pub(super) async fn max_upload_bytes(settings: &v0::DjInformation, user: &User) -> u64 {
    const MIB: u64 = 1024 * 1024;

    let channel_limit = (settings.max_file_mb as u64 * MIB).min(MAX_UPLOAD_BYTES);
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

/// Title shown in the queue: the given title, else the file name, else a default
fn resolve_title(title: Option<String>, file: &TempFile<'_>) -> String {
    title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_string)
        .or_else(|| {
            file.raw_name()
                .and_then(|name| name.as_str().map(str::to_string))
                .filter(|name| !name.is_empty())
        })
        .map(|title| truncate_title(&title))
        .unwrap_or_else(|| "Untitled".to_string())
}

/// Bound a title to [`MAX_TITLE_CHARS`] characters, on a character boundary
pub(super) fn truncate_title(title: &str) -> String {
    title.chars().take(MAX_TITLE_CHARS).collect::<String>()
}

/// Amount of tracks the user has queued or is currently playing
pub(super) fn member_track_count(
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

pub(super) async fn directory_size(dir: &Path) -> u64 {
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

/// A stable, user-facing explanation of why an upload was rejected
///
/// Deliberately not `{:?}` of the error type: that leaks Rust type names and
/// their internal fields to the client.
pub(super) fn upload_failure_reason(error: &ErrorType) -> String {
    match error {
        ErrorType::FileTooLarge { .. } => "the file is larger than this channel allows",
        ErrorType::FileTooSmall => "the file is empty",
        ErrorType::DjTrackTooLong { .. } => "the track is longer than this channel allows",
        ErrorType::DjUploadLimit { .. } => "you already have the maximum number of tracks queued",
        ErrorType::DjQueueFull { .. } => "the queue is full",
        ErrorType::DjStorageFull { .. } => "the channel has no storage left",
        ErrorType::DjTranscodeFailed => "the file could not be converted to audio",
        ErrorType::DjUnavailable => "DJ uploads are temporarily unavailable",
        ErrorType::DjYoutubeUnsupported => "the link is not a single YouTube video",
        ErrorType::DjYoutubeFailed => "the linked video could not be downloaded",
        ErrorType::DjYoutubeUnavailable => "YouTube downloads are unavailable on this server",
        _ => "the upload was rejected",
    }
    .to_string()
}

/// Delete a partially written file and tell the uploader why it was rejected
pub(super) async fn upload_failed(channel: &str, user: &str, path: &Path, error: Error) -> Error {
    if let Err(error) = tokio::fs::remove_file(path).await {
        if error.kind() != std::io::ErrorKind::NotFound {
            log::warn!(
                "Failed to remove rejected DJ upload {}: {error}",
                path.display()
            );
        }
    }

    EventV1::DjUploadFailed {
        channel: channel.to_string(),
        reason: upload_failure_reason(&error.error_type),
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
            file: "/dj/c/stream".to_string(),
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

    #[test]
    fn titles_are_bounded_and_never_empty() {
        let long = "a".repeat(MAX_TITLE_CHARS + 20);
        assert_eq!(truncate_title(&long).chars().count(), MAX_TITLE_CHARS);

        // Truncation happens on a character boundary, not inside a UTF-8 char
        let unicode = "é".repeat(MAX_TITLE_CHARS + 5);
        assert_eq!(truncate_title(&unicode).chars().count(), MAX_TITLE_CHARS);

        assert_eq!(truncate_title("  song  "), "  song  ");
    }

    #[test]
    fn upload_failures_do_not_leak_internal_type_names() {
        let reason = upload_failure_reason(&ErrorType::DjTranscodeFailed);

        assert!(
            !reason.contains("DjTranscodeFailed") && !reason.contains('{'),
            "reason leaked the error type: {}",
            reason
        );

        for error in [
            ErrorType::FileTooLarge { max: 1 },
            ErrorType::DjQueueFull { max: 3 },
            ErrorType::DjStorageFull { max_bytes: 10 },
            ErrorType::DjUploadLimit { max: 2 },
            ErrorType::DjTrackTooLong { max_seconds: 5 },
            ErrorType::DjUnavailable,
            ErrorType::FileTooSmall,
        ] {
            let reason = upload_failure_reason(&error);

            assert!(
                !reason.contains("ErrorType") && !reason.contains('{'),
                "{} leaked the error type",
                reason
            );
            assert!(!reason.is_empty());
        }
    }

    #[test]
    fn multipart_overhead_does_not_reject_a_maximum_file() {
        // The guard allows the file itself plus framing; a max-size file must
        // not be rejected because of the multipart boundary lines.
        let limit = 50 * 1024 * 1024u64;

        assert!(length_fits(limit, limit));
        assert!(length_fits(limit, limit + 1024));

        // Anything past the framing allowance is rejected before buffering
        assert!(!length_fits(limit, limit + MULTIPART_OVERHEAD_BYTES + 1));
        assert!(!length_fits(limit, u64::MAX));
        // A zero limit accepts nothing but the framing
        assert!(length_fits(0, 0));
        assert!(!length_fits(0, MULTIPART_OVERHEAD_BYTES + 1));
    }

    #[test]
    fn uploads_are_staged_outside_the_track_namespace() {
        let destination = paths::track_path("chan", "1-abc");

        let partial = partial_path(&destination);

        assert_eq!(partial.parent(), destination.parent());
        assert_eq!(
            partial.file_name().and_then(|name| name.to_str()),
            Some("1-abc.mp3.part")
        );
        assert_ne!(partial, destination);
        // The sweeper only deletes files which are not in the keep set, and the
        // queue only ever names `<id>.mp3`, so a `.part` file is never played.
        assert!(!partial
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .ends_with(".mp3"));
    }
}
