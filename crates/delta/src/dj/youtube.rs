//! YouTube-link queueing: URL validation, metadata and audio download.
//!
//! A queue request may carry a YouTube link instead of an uploaded file. The
//! link is parsed into a video id and **canonicalised** to
//! `https://www.youtube.com/watch?v=<id>`; that canonical URL is the only
//! string which ever reaches `yt-dlp`. Playlist parameters (`list`, `index`,
//! `start_radio`, ...) are dropped, so a link to a video inside a playlist
//! queues just that video.
//!
//! The link is never fetched by delta itself: that is the SSRF guard, because
//! `yt-dlp` only ever sees a URL built from an 11 character `[A-Za-z0-9_-]` id
//! on a fixed host. Arguments are always passed as an argv vector (never
//! through a shell), so a hostile URL cannot inject a second command.
//!
//! Downloads are bounded by a dedicated semaphore, a hard timeout and a byte
//! limit; the resulting file is transcoded by [`super::transcode`], so the
//! duration and storage caps are the same ones an upload goes through.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use once_cell::sync::OnceCell;
use revolt_result::{create_error, Error, Result};
use tokio::process::Command;
use tokio::sync::Semaphore;
use url::Url;

/// Environment variable overriding the yt-dlp binary
pub const YTDLP_ENV: &str = "YTDLP_BIN";

/// Maximum amount of concurrent downloads
///
/// Downloads are network-bound rather than CPU-bound, but each one holds a
/// temporary file of up to `max_file_mb`, so the same disk argument applies.
pub const MAX_CONCURRENT_DOWNLOADS: usize = 2;

/// Hard timeout for a single metadata lookup
pub const METADATA_TIMEOUT: Duration = Duration::from_secs(30);

/// Hard timeout for a single download
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(180);

/// Maximum amount of time a request waits for a download slot
pub const DOWNLOAD_SLOT_TIMEOUT: Duration = Duration::from_secs(10);

/// Longest URL accepted, in bytes
const MAX_URL_LENGTH: usize = 2048;

/// Hosts a YouTube link may use
///
/// Anything else is rejected rather than handed to `yt-dlp`: an arbitrary host
/// would turn a queue request into an outbound request from the server.
const ALLOWED_HOSTS: [&str; 8] = [
    "youtube.com",
    "www.youtube.com",
    "m.youtube.com",
    "music.youtube.com",
    "youtu.be",
    "www.youtu.be",
    "youtube-nocookie.com",
    "www.youtube-nocookie.com",
];

/// Path prefixes which take the video id as the next segment
const ID_PATH_PREFIXES: [&str; 4] = ["embed", "shorts", "live", "v"];

/// Slack allowed on top of `max_track_seconds` before a link is refused
///
/// Mirrors [`super::END_MARGIN_MS`]: an upload is truncated at the cap and the
/// engine then allows the end margin, so a link a few seconds over is still
/// playable and is truncated rather than refused.
const DURATION_SLACK_MS: u64 = super::END_MARGIN_MS;

static SEMAPHORE: OnceCell<Arc<Semaphore>> = OnceCell::new();

fn semaphore() -> Arc<Semaphore> {
    SEMAPHORE
        .get_or_init(|| Arc::new(Semaphore::new(MAX_CONCURRENT_DOWNLOADS)))
        .clone()
}

/// A validated YouTube video id
///
/// Constructing one is the only way to produce a URL for `yt-dlp`, so every
/// code path downstream of the parser is known to point at YouTube.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoId(String);

impl VideoId {
    /// The bare video id
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Canonical watch URL, the only form handed to `yt-dlp`
    pub fn canonical_url(&self) -> String {
        format!("https://www.youtube.com/watch?v={}", self.0)
    }
}

/// Whether `id` is a syntactically valid YouTube video id
fn is_valid_video_id(id: &str) -> bool {
    id.len() == 11
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn unsupported(reason: &str) -> Error {
    // The client gets a stable error type; the reason is for the operator, so a
    // rejected link can be understood from the logs without the request body.
    log::debug!("Rejected a YouTube queue link: {reason}");
    create_error!(DjYoutubeUnsupported)
}

/// Parse a YouTube link into a [`VideoId`], rejecting anything else
///
/// Accepts `youtube.com`/`www.`/`m.`/`music.`/`youtube-nocookie.com` watch,
/// embed, shorts, live and `/v/` links plus `youtu.be/<id>`. Playlist, channel
/// and search pages are refused, as are other hosts and malformed ids.
pub fn parse_video_url(input: &str) -> Result<VideoId> {
    let input = input.trim();

    if input.is_empty() {
        return Err(unsupported("empty link"));
    }

    if input.len() > MAX_URL_LENGTH {
        return Err(unsupported("link is too long"));
    }

    // Whitespace or control characters have no business in a URL, and refusing
    // them early keeps the parser from having to care about them at all.
    if input.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(unsupported("link contains whitespace or control characters"));
    }

    let url = Url::parse(input).map_err(|_| unsupported("not a URL"))?;

    if !matches!(url.scheme(), "http" | "https") {
        return Err(unsupported("only http(s) links are supported"));
    }

    // `https://evil.example@youtube.com/watch?v=…` parses with the YouTube host
    // but reads as an entirely different site to a human, and an explicit port
    // is not something a real YouTube link carries.
    if !url.username().is_empty() || url.password().is_some() || url.port().is_some() {
        return Err(unsupported("link has userinfo or an explicit port"));
    }

    let host = url
        .host_str()
        .ok_or_else(|| unsupported("link has no host"))?;

    if !ALLOWED_HOSTS.contains(&host) {
        return Err(unsupported("host is not YouTube"));
    }

    let segments: Vec<&str> = url
        .path_segments()
        .map(|segments| segments.filter(|segment| !segment.is_empty()).collect())
        .unwrap_or_default();

    let candidate = if host.ends_with("youtu.be") {
        // https://youtu.be/<id>
        segments.first().map(|segment| (*segment).to_string())
    } else {
        match segments.first().copied() {
            // https://www.youtube.com/watch?v=<id>
            Some("watch") => url
                .query_pairs()
                .find(|(key, _)| key == "v")
                .map(|(_, value)| value.into_owned()),
            // https://www.youtube.com/embed/<id> (and shorts/live/v)
            Some(prefix) if ID_PATH_PREFIXES.contains(&prefix) => {
                segments.get(1).map(|segment| (*segment).to_string())
            }
            // `/playlist`, `/results`, `/@channel`, `/` and friends
            _ => None,
        }
    };

    let Some(candidate) = candidate else {
        return Err(unsupported(
            "no video id in the link (playlists and channel pages are not supported)",
        ));
    };

    if !is_valid_video_id(&candidate) {
        return Err(unsupported(
            "video id is not 11 characters of [A-Za-z0-9_-]",
        ));
    }

    Ok(VideoId(candidate))
}

/// Metadata read from `yt-dlp --dump-single-json`
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VideoMetadata {
    /// Video title, as reported by YouTube
    pub title: Option<String>,
    /// Duration in milliseconds, when YouTube reports one
    pub duration_ms: Option<u64>,
}

/// Arguments for the metadata lookup
///
/// Kept as a function so tests can assert the exact argv: the canonical URL is
/// the last argument, no flag accepts a command, and nothing is shell-quoted
/// because no shell is involved.
fn metadata_args(url: &str) -> Vec<OsString> {
    [
        "--ignore-config",
        "--no-playlist",
        "--dump-single-json",
        "--no-warnings",
        "--no-progress",
        "--socket-timeout",
        "15",
        url,
    ]
    .iter()
    .map(OsString::from)
    .collect()
}

/// Arguments for the audio download
fn download_args(url: &str, max_bytes: u64, output: &Path) -> Vec<OsString> {
    let args: Vec<OsString> = vec![
        "--ignore-config".to_string(),
        "--no-playlist".to_string(),
        // Audio only: `bestaudio` never selects a video stream. The conversion
        // itself is left to `transcode::to_mp3`.
        "-f".to_string(),
        "bestaudio".to_string(),
        // A size hint to yt-dlp; the produced file is measured again afterwards
        // because the hint is unavailable for some streamed formats.
        "--max-filesize".to_string(),
        max_bytes.to_string(),
        "--no-progress".to_string(),
        "--no-warnings".to_string(),
        "--no-part".to_string(),
        "--socket-timeout".to_string(),
        "15".to_string(),
        "-o".to_string(),
    ]
    .into_iter()
    .map(OsString::from)
    .collect::<Vec<_>>();

    let mut args = args;
    args.push(output.as_os_str().to_os_string());
    args.push(OsString::from(url));

    args
}

/// Parse `yt-dlp --dump-single-json` output
pub fn parse_metadata(json: &str) -> Result<VideoMetadata> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(|error| {
        log::warn!("yt-dlp returned invalid JSON: {error}");
        create_error!(DjYoutubeFailed)
    })?;

    let title = value
        .get("title")
        .and_then(|title| title.as_str())
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_string);

    // `duration` is in seconds, and is `null` for live streams and premieres.
    let duration_ms = value
        .get("duration")
        .and_then(serde_json::Value::as_f64)
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        .map(|seconds| (seconds * 1000.0).round() as u64);

    Ok(VideoMetadata { title, duration_ms })
}

/// Refuse a link which is longer than the channel allows, before downloading
///
/// Without a reported duration there is nothing to check here; the transcoded
/// output is probed afterwards, so a live stream still ends up capped.
pub fn check_duration(
    settings: &revolt_models::v0::DjInformation,
    metadata: &VideoMetadata,
) -> Result<()> {
    let Some(duration_ms) = metadata.duration_ms else {
        return Ok(());
    };

    let limit = (settings.max_track_seconds as u64)
        .saturating_mul(1000)
        .saturating_add(DURATION_SLACK_MS);

    if duration_ms > limit {
        return Err(create_error!(DjTrackTooLong {
            max_seconds: settings.max_track_seconds
        }));
    }

    Ok(())
}

/// Title for a link: the requested title, else the video title, else the id
pub fn resolve_title(requested: Option<&str>, metadata: &VideoMetadata, id: &str) -> String {
    requested
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_string)
        .or_else(|| metadata.title.clone())
        .unwrap_or_else(|| id.to_string())
}

/// Last `limit` bytes of a command's stderr, for logging
fn stderr_tail(stderr: &[u8], limit: usize) -> String {
    let text = String::from_utf8_lossy(stderr);
    let trimmed = text.trim();

    // yt-dlp is chatty on failure; keep the log line bounded.
    if trimmed.len() <= limit {
        return trimmed.to_string();
    }

    let mut start = trimmed.len() - limit;

    // Never split a UTF-8 character
    while start < trimmed.len() && !trimmed.is_char_boundary(start) {
        start += 1;
    }

    trimmed[start..].to_string()
}

/// A yt-dlp binary
///
/// The binary is held in one place so tests can exercise the failure paths
/// against a name which cannot exist, without mutating the process environment
/// while other tests run.
#[derive(Debug, Clone)]
pub struct YtDlp {
    binary: String,
}

impl YtDlp {
    /// The binary configured for this deployment
    pub fn from_env() -> Self {
        Self {
            binary: std::env::var(YTDLP_ENV).unwrap_or_else(|_| "yt-dlp".to_string()),
        }
    }

    /// A specific binary, for tests
    pub fn named(binary: &str) -> Self {
        Self {
            binary: binary.to_string(),
        }
    }

    /// Version reported by `--version`, if the binary runs at all
    pub async fn version(&self) -> Option<String> {
        let output = Command::new(&self.binary)
            .arg("--version")
            .stdin(Stdio::null())
            .output()
            .await
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let version = String::from_utf8_lossy(&output.stdout);

        Some(
            version
                .lines()
                .next()
                .unwrap_or("unknown version")
                .to_string(),
        )
    }

    /// Fetch the metadata of a validated video
    pub async fn metadata(&self, video: &VideoId) -> Result<VideoMetadata> {
        let canonical = video.canonical_url();

        let output = tokio::time::timeout(
            METADATA_TIMEOUT,
            Command::new(&self.binary)
                .args(metadata_args(&canonical))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| {
            log::warn!(
                "yt-dlp timed out after {}s reading metadata for {}",
                METADATA_TIMEOUT.as_secs(),
                canonical
            );

            create_error!(DjYoutubeFailed)
        })?
        .map_err(|error| {
            // A spawn failure on this path means the binary is missing or not
            // executable, which is a deployment problem, not a bad link.
            log::error!("Failed to spawn yt-dlp: {error}");

            create_error!(DjYoutubeUnavailable)
        })?;

        if !output.status.success() {
            log::warn!(
                "yt-dlp failed to read metadata for {}: {}",
                canonical,
                stderr_tail(&output.stderr, 512)
            );

            return Err(create_error!(DjYoutubeFailed));
        }

        parse_metadata(&String::from_utf8_lossy(&output.stdout))
    }

    /// Download the best audio stream of a validated video into `directory`
    ///
    /// Files are named `<stem>.<ext>`; the caller owns the directory and must
    /// delete the result whatever happens next.
    pub async fn download(
        &self,
        video: &VideoId,
        directory: &Path,
        stem: &str,
        max_bytes: u64,
    ) -> Result<PathBuf> {
        let permit = tokio::time::timeout(DOWNLOAD_SLOT_TIMEOUT, semaphore().acquire_owned())
            .await
            .map_err(|_| {
                log::warn!("Timed out waiting for a DJ download slot");
                create_error!(DjYoutubeUnavailable)
            })?
            .map_err(|_| create_error!(DjYoutubeUnavailable))?;

        tokio::fs::create_dir_all(directory).await.map_err(|error| {
            log::error!(
                "Failed to create DJ download directory {}: {error}",
                directory.display()
            );

            create_error!(DjUnavailable)
        })?;

        let canonical = video.canonical_url();
        let template = directory.join(format!("{stem}.%(ext)s"));
        let args = download_args(&canonical, max_bytes, &template);

        let child = Command::new(&self.binary)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                log::error!("Failed to spawn yt-dlp: {error}");
                create_error!(DjYoutubeUnavailable)
            })?;

        let output = match tokio::time::timeout(DOWNLOAD_TIMEOUT, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                log::error!("Failed to run yt-dlp: {error}");
                return Err(create_error!(DjYoutubeFailed));
            }
            Err(_) => {
                // `kill_on_drop` reaps the child when it is dropped
                log::warn!(
                    "yt-dlp timed out after {}s downloading {}",
                    DOWNLOAD_TIMEOUT.as_secs(),
                    canonical
                );

                return Err(create_error!(DjYoutubeFailed));
            }
        };

        // The slot is held until the process is gone, not merely until it was
        // spawned: the temporary file is what the semaphore is protecting.
        drop(permit);

        if !output.status.success() {
            log::warn!(
                "yt-dlp failed to download {}: {}",
                canonical,
                stderr_tail(&output.stderr, 512)
            );

            return Err(create_error!(DjYoutubeFailed));
        }

        let downloaded = find_downloaded(directory, stem).await?;
        let len = tokio::fs::metadata(&downloaded)
            .await
            .map(|meta| meta.len())
            .unwrap_or_default();

        if len == 0 {
            let _ = tokio::fs::remove_file(&downloaded).await;
            log::warn!("yt-dlp produced an empty file for {canonical}");

            return Err(create_error!(DjYoutubeFailed));
        }

        if len > max_bytes {
            let _ = tokio::fs::remove_file(&downloaded).await;

            return Err(create_error!(FileTooLarge {
                max: max_bytes as usize
            }));
        }

        Ok(downloaded)
    }
}

/// Whether yt-dlp answers `--version`, and with what
pub async fn version() -> Option<String> {
    YtDlp::from_env().version().await
}

/// Check that yt-dlp is available, logging loudly if not
///
/// A missing binary is not fatal: instances which do not use YouTube links
/// should still boot, but every link queue request will fail with
/// `DjYoutubeUnavailable`.
pub async fn check_available() -> bool {
    match version().await {
        Some(version) => {
            log::info!("DJ YouTube downloads available: yt-dlp {version}");
            true
        }
        None => {
            log::error!(
                "############################################################\n\
                 # yt-dlp is NOT available on this instance!                #\n\
                 # DJ YouTube links will fail with DjYoutubeUnavailable.    #\n\
                 # Install yt-dlp in the delta image (see Dockerfile).      #\n\
                 ############################################################"
            );

            false
        }
    }
}

/// Fetch the metadata of a validated video
pub async fn fetch_metadata(video: &VideoId) -> Result<VideoMetadata> {
    YtDlp::from_env().metadata(video).await
}

/// Download the best audio stream of a validated video
pub async fn download_audio(
    video: &VideoId,
    directory: &Path,
    stem: &str,
    max_bytes: u64,
) -> Result<PathBuf> {
    YtDlp::from_env()
        .download(video, directory, stem, max_bytes)
        .await
}

/// Find the file `yt-dlp` produced for `stem`
async fn find_downloaded(directory: &Path, stem: &str) -> Result<PathBuf> {
    let prefix = format!("{stem}.");
    let mut entries = tokio::fs::read_dir(directory)
        .await
        .map_err(|_| create_error!(DjYoutubeFailed))?;

    let mut best: Option<(u64, PathBuf)> = None;

    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };

        // `<stem>.fmt` / `<stem>.fmt.meta` are sidecars, and a `.part` file is
        // never the finished download.
        if !name.starts_with(&prefix) || name.ends_with(".part") || name.ends_with(".ytdl") {
            continue;
        }

        let Ok(metadata) = entry.metadata().await else {
            continue;
        };

        if !metadata.is_file() {
            continue;
        }

        if best
            .as_ref()
            .map(|(len, _)| metadata.len() > *len)
            .unwrap_or(true)
        {
            best = Some((metadata.len(), entry.path()));
        }
    }

    best.map(|(_, path)| path)
        .ok_or_else(|| create_error!(DjYoutubeFailed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use revolt_models::v0::DjInformation;
    use revolt_result::ErrorType;

    const ID: &str = "bhupEBurUQI";

    fn settings(max_track_seconds: u32) -> DjInformation {
        DjInformation {
            max_track_seconds,
            ..Default::default()
        }
    }

    fn rejected(url: &str) -> ErrorType {
        parse_video_url(url)
            .expect_err(&format!("{url:?} must be rejected"))
            .error_type
    }

    #[test]
    fn parses_a_watch_link() {
        let video = parse_video_url(&format!(
            "https://youtube.com/watch?v={ID}&list=OLAK5uy_nN11uj5CGEhdhx6YjlBGuux6wiWrYx_ak"
        ))
        .expect("watch link");

        assert_eq!(video.as_str(), ID);
        assert_eq!(
            video.canonical_url(),
            format!("https://www.youtube.com/watch?v={ID}")
        );
    }

    #[test]
    fn ignores_playlist_parameters() {
        // The example link from the issue: a video inside an album playlist.
        let with_playlist = parse_video_url(&format!(
            "https://www.youtube.com/watch?v={ID}&list=OLAK5uy_nN11uj5CGEhdhx6YjlBGuux6wiWrYx_ak&index=3&start_radio=1"
        ))
        .expect("video inside a playlist");

        let plain = parse_video_url(&format!("https://www.youtube.com/watch?v={ID}"))
            .expect("plain watch link");

        // Same id, same canonical URL: nothing of the playlist survives.
        assert_eq!(with_playlist, plain);
        assert!(!with_playlist.canonical_url().contains("list="));
        assert!(!with_playlist.canonical_url().contains("index="));
    }

    #[test]
    fn parses_short_links() {
        for url in [
            format!("https://youtu.be/{ID}"),
            format!("https://youtu.be/{ID}?list=PL123&index=2"),
            format!("https://www.youtu.be/{ID}"),
            format!("https://m.youtube.com/watch?v={ID}"),
            format!("https://music.youtube.com/watch?v={ID}&list=OLAK5uy_x"),
            format!("https://www.youtube-nocookie.com/embed/{ID}"),
            format!("https://youtube-nocookie.com/embed/{ID}?start=30"),
            format!("https://www.youtube.com/shorts/{ID}"),
            format!("https://www.youtube.com/live/{ID}"),
            format!("https://www.youtube.com/v/{ID}"),
            format!("https://www.youtube.com/embed/{ID}"),
            format!("  https://www.youtube.com/watch?v={ID}  "),
            format!("http://www.youtube.com/watch?v={ID}"),
            format!("https://www.youtube.com/watch/?v={ID}"),
            format!("https://www.youtube.com/watch?feature=share&v={ID}"),
        ] {
            let video = parse_video_url(&url)
                .unwrap_or_else(|error| panic!("{url:?} must parse, got {:?}", error.error_type));

            assert_eq!(video.as_str(), ID, "{url:?}");
        }
    }

    #[test]
    fn rejects_playlist_only_links() {
        for url in [
            "https://www.youtube.com/playlist?list=OLAK5uy_nN11uj5CGEhdhx6YjlBGuux6wiWrYx_ak",
            "https://youtube.com/playlist?list=PL123",
            "https://music.youtube.com/playlist?list=OLAK5uy_x",
            "https://www.youtube.com/watch?list=PL123",
            "https://youtu.be/?list=PL123",
            "https://www.youtube.com/@somechannel",
            "https://www.youtube.com/results?search_query=music",
            "https://www.youtube.com/",
        ] {
            assert!(
                matches!(rejected(url), ErrorType::DjYoutubeUnsupported),
                "{:?} must be rejected as unsupported",
                url
            );
        }
    }

    #[test]
    fn rejects_non_youtube_links() {
        for url in [
            "https://example.com/watch?v=bhupEBurUQI",
            "https://youtube.com.evil.example/watch?v=bhupEBurUQI",
            "https://evil.example/youtu.be/watch?v=bhupEBurUQI",
            "https://notyoutube.com/watch?v=bhupEBurUQI",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "http://127.0.0.1:6379/",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::1]/",
            "//youtube.com/watch?v=bhupEBurUQI",
            "youtube.com/watch?v=bhupEBurUQI",
            "",
            "   ",
        ] {
            assert!(
                parse_video_url(url).is_err(),
                "{:?} must be rejected",
                url
            );
        }
    }

    #[test]
    fn rejects_userinfo_and_ports() {
        // These read as an evil.example link to a human but parse with the
        // YouTube host: none of them may reach yt-dlp.
        for url in [
            "https://evil.example@youtube.com/watch?v=bhupEBurUQI",
            "https://user:pass@www.youtube.com/watch?v=bhupEBurUQI",
            "https://www.youtube.com:8443/watch?v=bhupEBurUQI",
        ] {
            assert!(
                parse_video_url(url).is_err(),
                "{:?} must be rejected",
                url
            );
        }
    }

    #[test]
    fn rejects_malformed_video_ids() {
        for id in [
            "bhupEBurUQ",     // 10 characters
            "bhupEBurUQIx",   // 12 characters
            "bhupEBurUQ!",    // invalid character
            "bhupEBurUQ%2F",  // encoded separator
            "../../etc/pass", // traversal
            "--help",
            "-x",
        ] {
            for url in [
                format!("https://www.youtube.com/watch?v={id}"),
                format!("https://youtu.be/{id}"),
            ] {
                assert!(
                    parse_video_url(&url).is_err(),
                    "{:?} must be rejected",
                    url
                );
            }
        }
    }

    #[test]
    fn rejects_whitespace_and_control_characters() {
        let injected = format!("https://www.youtube.com/watch?v={ID}\n--exec=id");

        assert!(parse_video_url(&injected).is_err());
    }

    #[test]
    fn metadata_argv_has_no_shell_or_injection_surface() {
        let video = parse_video_url(&format!("https://www.youtube.com/watch?v={ID}")).unwrap();
        let args = metadata_args(&video.canonical_url());

        let rendered: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        // The URL is the last argument and appears exactly once
        assert_eq!(rendered.last().unwrap(), &video.canonical_url());
        assert_eq!(
            rendered
                .iter()
                .filter(|arg| arg.contains("youtube.com"))
                .count(),
            1
        );

        for arg in &rendered {
            for forbidden in [";", "|", "&&", "$(", "`", ">", "<", "\n", "\"", "'"] {
                assert!(
                    !arg.contains(forbidden),
                    "{:?} contains {:?}",
                    arg,
                    forbidden
                );
            }
        }

        // Only flags yt-dlp knows, and no flag which runs a command
        assert!(rendered.contains(&"--no-playlist".to_string()));
        assert!(rendered.contains(&"--dump-single-json".to_string()));
        assert!(!rendered
            .iter()
            .any(|arg| arg.starts_with("--exec") || arg.starts_with("--postprocessor-args")));
    }

    #[test]
    fn download_argv_is_bounded_and_has_no_shell_surface() {
        let video = parse_video_url(&format!("https://youtu.be/{ID}?list=PL1")).unwrap();
        let output = PathBuf::from("/var/lib/revolt/dj/c1/12-abc.%(ext)s");
        let args = download_args(&video.canonical_url(), 50 * 1024 * 1024, &output);

        let rendered: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        // `-f bestaudio` never selects a video stream
        let format = rendered
            .iter()
            .position(|arg| arg == "-f")
            .expect("-f present");
        assert_eq!(rendered[format + 1], "bestaudio");

        // The byte cap is argv, not shell interpolation
        let max = rendered
            .iter()
            .position(|arg| arg == "--max-filesize")
            .expect("--max-filesize present");
        assert_eq!(rendered[max + 1], (50 * 1024 * 1024u64).to_string());

        // Output template, then the canonical URL last
        let template = rendered
            .iter()
            .position(|arg| arg == "-o")
            .expect("-o present");
        assert_eq!(rendered[template + 1], output.to_string_lossy());
        assert_eq!(rendered.last().unwrap(), &video.canonical_url());

        assert!(!rendered.iter().any(|arg| arg.contains("list=")));
        assert!(!rendered.iter().any(|arg| arg.starts_with("--exec")));
    }

    #[test]
    fn parses_metadata_json() {
        let metadata = parse_metadata(
            r#"{"id":"bhupEBurUQI","title":"  Never Gonna Give You Up  ","duration":213.5,"formats":[]}"#,
        )
        .expect("metadata");

        assert_eq!(metadata.title.as_deref(), Some("Never Gonna Give You Up"));
        assert_eq!(metadata.duration_ms, Some(213_500));
    }

    #[test]
    fn tolerates_missing_title_and_duration() {
        // Live streams and premieres report `duration: null`
        let metadata = parse_metadata(r#"{"id":"x","title":null,"duration":null}"#).expect("meta");

        assert_eq!(metadata.title, None);
        assert_eq!(metadata.duration_ms, None);

        // A video with no duration must not be refused before the download
        check_duration(&settings(1), &metadata).expect("no duration to check");
    }

    #[test]
    fn rejects_invalid_metadata_json() {
        for json in ["", "not json", "{"] {
            assert!(
                parse_metadata(json).is_err(),
                "{:?} must not parse into metadata",
                json
            );
        }

        // An object without the fields is valid, just empty
        assert_eq!(parse_metadata("{}").unwrap(), VideoMetadata::default());
    }

    #[test]
    fn duration_is_checked_before_downloading() {
        let cap = settings(300);

        check_duration(
            &cap,
            &VideoMetadata {
                duration_ms: Some(300_000),
                ..Default::default()
            },
        )
        .expect("exactly at the cap");

        // The end margin is allowed, and the transcode truncates it
        check_duration(
            &cap,
            &VideoMetadata {
                duration_ms: Some(305_000),
                ..Default::default()
            },
        )
        .expect("within the end margin");

        let error = check_duration(
            &cap,
            &VideoMetadata {
                duration_ms: Some(305_001),
                ..Default::default()
            },
        )
        .expect_err("past the end margin");

        assert!(matches!(
            error.error_type,
            ErrorType::DjTrackTooLong { max_seconds: 300 }
        ));

        // A hostile duration saturates rather than wrapping into a pass
        assert!(check_duration(
            &cap,
            &VideoMetadata {
                duration_ms: Some(u64::MAX),
                ..Default::default()
            },
        )
        .is_err());
    }

    #[test]
    fn titles_prefer_the_request_then_the_video() {
        let metadata = VideoMetadata {
            title: Some("Video title".to_string()),
            ..Default::default()
        };

        assert_eq!(
            resolve_title(Some("  My title  "), &metadata, ID),
            "My title"
        );
        assert_eq!(resolve_title(Some("   "), &metadata, ID), "Video title");
        assert_eq!(resolve_title(None, &metadata, ID), "Video title");
        assert_eq!(
            resolve_title(None, &VideoMetadata::default(), ID),
            ID,
            "a video without a title falls back to its id"
        );
    }

    #[test]
    fn stderr_tail_is_bounded_and_utf8_safe() {
        let stderr = "é".repeat(500);

        let tail = stderr_tail(stderr.as_bytes(), 100);

        assert!(tail.len() <= 100);
        assert!(tail.chars().all(|c| c == 'é'));
        assert_eq!(stderr_tail(b"  short  ", 100), "short");
    }

    #[tokio::test]
    async fn a_missing_binary_is_reported_as_unavailable() {
        // The binary name cannot exist, so this test never touches the network
        // and never depends on yt-dlp being installed.
        let ytdlp = YtDlp::named("dj-test-definitely-not-yt-dlp");

        assert!(ytdlp.version().await.is_none());

        let error = ytdlp
            .metadata(&VideoId(ID.to_string()))
            .await
            .expect_err("missing binary");

        assert!(matches!(error.error_type, ErrorType::DjYoutubeUnavailable));

        let error = ytdlp
            .download(
                &VideoId(ID.to_string()),
                &std::env::temp_dir().join("dj-ytdlp-missing"),
                "stem",
                1024,
            )
            .await
            .expect_err("missing binary");

        assert!(matches!(error.error_type, ErrorType::DjYoutubeUnavailable));

        let _ = std::fs::remove_dir_all(std::env::temp_dir().join("dj-ytdlp-missing"));
    }
}
