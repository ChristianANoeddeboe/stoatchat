//! Streaming of DJ audio files.
//!
//! Rocket 0.5's `NamedFile` has no `Range` support at all: a `Range: bytes=0-3`
//! request returns the whole body with `200`, which breaks mid-song seeking and
//! `seekTo(position_ms)` when joining a track which is already playing. Media
//! players rely on `206 Partial Content` for both.
//!
//! This module therefore serves the file itself: it parses a single byte range,
//! seeks the file and answers `206` with an exact `Content-Length` and
//! `Content-Range`, or `416` when the range cannot be satisfied. Access is still
//! granted by the HMAC token in the query string, because `<audio>` elements
//! cannot send an `Authorization` header.

use std::io::SeekFrom;
use std::pin::Pin;
use std::task::{Context, Poll};

use revolt_result::{create_error, Result};
use rocket::{
    http::{Header, Status},
    request::Request,
    response::{self, Responder, Response},
    tokio::{
        fs::File,
        io::{AsyncRead, AsyncSeek, AsyncSeekExt, ReadBuf},
    },
};

use crate::dj::{paths, token};

/// Content type of every DJ track (the transcoder only ever emits MP3)
const CONTENT_TYPE: &str = "audio/mpeg";

/// `Cache-Control` for track audio
///
/// The URL contains a short-lived token and the audio is ephemeral, so nothing
/// may be cached by a proxy or written to a shared cache.
const CACHE_CONTROL: &str = "private, no-store";

/// A parsed `Range` request header
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteRange {
    /// `bytes=a-b`: both ends included, already clamped to the file
    Bounded(u64, u64),
    /// `bytes=a-`: from `a` to the end of the file
    OpenEnded(u64),
    /// `bytes=-n`: the last `n` bytes of the file
    Suffix(u64),
}

impl ByteRange {
    /// Resolve the range against a file of `len` bytes
    ///
    /// Returns `None` when the range cannot be satisfied (`416`): a start at or
    /// past the end, an inverted or zero-length range, or a zero-byte suffix.
    pub fn resolve(self, len: u64) -> Option<(u64, u64)> {
        if len == 0 {
            return None;
        }

        match self {
            ByteRange::Bounded(start, end) => {
                if start > end || start >= len {
                    None
                } else {
                    Some((start, end.min(len - 1)))
                }
            }
            ByteRange::OpenEnded(start) => {
                if start >= len {
                    None
                } else {
                    Some((start, len - 1))
                }
            }
            ByteRange::Suffix(length) => {
                if length == 0 {
                    None
                } else {
                    let start = len.saturating_sub(length);
                    Some((start, len - 1))
                }
            }
        }
    }
}

/// Parse a `Range` header value
///
/// Only a single `bytes=` range is supported. Anything else (multi-range, other
/// units, unparsable values) is reported as `None` so the caller can answer
/// `416` rather than silently sending the whole file to a client which asked
/// for a part of it.
pub fn parse_range(header: &str) -> Option<ByteRange> {
    let spec = header.strip_prefix("bytes=")?;

    // Multi-range requests are not implemented; reject instead of guessing
    if spec.contains(',') {
        return None;
    }

    let (start, end) = spec.split_once('-')?;

    match (start.trim(), end.trim()) {
        ("", "") => None,
        ("", suffix) => suffix.parse::<u64>().ok().map(ByteRange::Suffix),
        (start, "") => start.parse::<u64>().ok().map(ByteRange::OpenEnded),
        (start, end) => {
            let start = start.parse::<u64>().ok()?;
            let end = end.parse::<u64>().ok()?;

            Some(ByteRange::Bounded(start, end))
        }
    }
}

/// A file limited to `remaining` bytes
///
/// Rocket reads a sized body to EOF and only uses the preset size for the
/// `Content-Length` header, so the reader itself has to stop at the end of the
/// requested range. `AsyncSeek` is required by `sized_body` and simply delegates
/// to the underlying file.
pub struct RangeReader {
    file: File,
    remaining: u64,
}

impl RangeReader {
    /// Read at most `len` bytes from the file's current position
    pub fn new(file: File, len: u64) -> Self {
        Self {
            file,
            remaining: len,
        }
    }
}

/// Chunk size used to bridge the file into the caller's buffer
const READ_CHUNK: usize = 4096;

impl AsyncRead for RangeReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();

        if this.remaining == 0 {
            return Poll::Ready(Ok(()));
        }

        // Never hand the file more room than the caller asked for, or more than
        // is left in the range.
        let len = READ_CHUNK
            .min(this.remaining as usize)
            .min(buf.remaining());

        if len == 0 {
            return Poll::Ready(Ok(()));
        }

        let mut scratch = [0u8; READ_CHUNK];
        let mut limited = ReadBuf::new(&mut scratch[..len]);

        match Pin::new(&mut this.file).poll_read(cx, &mut limited) {
            Poll::Ready(Ok(())) => {
                let filled = limited.filled();
                this.remaining = this.remaining.saturating_sub(filled.len() as u64);
                buf.put_slice(filled);

                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

impl AsyncSeek for RangeReader {
    fn start_seek(self: Pin<&mut Self>, position: SeekFrom) -> std::io::Result<()> {
        Pin::new(&mut self.get_mut().file).start_seek(position)
    }

    fn poll_complete(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<u64>> {
        Pin::new(&mut self.get_mut().file).poll_complete(cx)
    }
}

/// Response body for a stream request
pub enum AudioResponse {
    /// A whole file or a single range of it
    File {
        status: Status,
        file: RangeReader,
        len: u64,
        content_range: Option<String>,
    },
    /// Nothing to send, with the headers explaining why (`416`)
    Empty {
        status: Status,
        headers: Vec<(&'static str, String)>,
    },
}

impl AudioResponse {
    /// `200` with the whole file
    pub fn whole(file: File, len: u64) -> Self {
        Self::File {
            status: Status::Ok,
            file: RangeReader::new(file, len),
            len,
            content_range: None,
        }
    }

    /// `206` with `start..=end` of the file
    ///
    /// The caller must have seeked the file to `start` already: Rocket sends the
    /// body from the reader's current position, which is what makes a partial
    /// response cheap and avoids buffering the whole track.
    pub fn partial(file: File, start: u64, end: u64, total: u64) -> Self {
        let len = end - start + 1;

        Self::File {
            status: Status::PartialContent,
            file: RangeReader::new(file, len),
            len,
            content_range: Some(format!("bytes {start}-{end}/{total}")),
        }
    }

    /// Exact `Content-Length` this response carries
    ///
    /// Rocket writes the header itself from the preset body size (it is not
    /// present in `Response::headers()`), so this is the value which goes out on
    /// the wire and the one the tests pin.
    pub fn content_length(&self) -> u64 {
        match self {
            AudioResponse::File { len, .. } => *len,
            AudioResponse::Empty { .. } => 0,
        }
    }

    /// `416` with `Content-Range: bytes */{total}`
    pub fn unsatisfiable(total: u64) -> Self {
        Self::Empty {
            status: Status::RangeNotSatisfiable,
            headers: vec![("Content-Range", format!("bytes */{total}"))],
        }
    }
}

impl<'r> Responder<'r, 'static> for AudioResponse {
    fn respond_to(self, _: &'r Request<'_>) -> response::Result<'static> {
        match self {
            AudioResponse::File {
                status,
                file,
                len,
                content_range,
            } => {
                let mut response = Response::build();

                response
                    .status(status)
                    .header(Header::new("Accept-Ranges", "bytes"))
                    .header(Header::new("Cache-Control", CACHE_CONTROL))
                    .raw_header("Content-Type", CONTENT_TYPE)
                    // `sized_body` also sets the exact `Content-Length`, which
                    // must be the *range* length and not the file size.
                    .sized_body(Some(len as usize), file);

                if let Some(content_range) = content_range {
                    response.raw_header("Content-Range", content_range);
                }

                response.ok()
            }
            AudioResponse::Empty { status, headers } => {
                let mut response = Response::build();

                response
                    .status(status)
                    .header(Header::new("Accept-Ranges", "bytes"))
                    .header(Header::new("Cache-Control", CACHE_CONTROL));

                for (name, value) in headers {
                    response.raw_header(name, value);
                }

                response.ok()
            }
        }
    }
}

/// # Stream DJ Track
///
/// Serves the audio of a DJ track, honouring a single `Range` header so players
/// can seek and resume mid-track. Requires a signed token which is only ever
/// sent to joined members of the channel.
// Binary audio with a token in the query string: not a JSON API, so it is kept
// out of the generated schema (like the multipart upload route).
#[openapi(tag = "DJ", skip)]
#[get("/<channel>/stream?<token>")]
pub async fn stream(
    channel: &str,
    token: Option<String>,
    range: Option<RangeHeader>,
) -> Result<AudioResponse> {
    let Some(token) = token else {
        return Err(create_error!(InvalidDjToken));
    };

    // Reject anything which is not a plain channel id before touching the disk
    if !paths::is_safe_id(channel) {
        return Err(create_error!(InvalidDjToken));
    }

    let Some(track) = token::verify(channel, &token) else {
        return Err(create_error!(InvalidDjToken));
    };

    let path = paths::track_path(channel, &track);

    // Defence in depth: the path must resolve inside the channel's directory
    if path.parent() != Some(paths::channel_dir(channel).as_path()) {
        return Err(create_error!(InvalidDjToken));
    }

    let mut file = rocket::tokio::fs::File::open(&path).await.map_err(|error| {
        if error.kind() != std::io::ErrorKind::NotFound {
            log::warn!("Failed to open DJ track {}: {error}", path.display());
        }

        create_error!(NotFound)
    })?;

    let total = file
        .metadata()
        .await
        .map_err(|error| {
            log::warn!("Failed to stat DJ track {}: {error}", path.display());
            create_error!(NotFound)
        })?
        .len();

    let Some(range) = range else {
        return Ok(AudioResponse::whole(file, total));
    };

    let parsed = match parse_range(range.0.as_str()) {
        Some(parsed) => parsed,
        // Unparsable or multi-range: refuse rather than send everything
        None => return Ok(AudioResponse::unsatisfiable(total)),
    };

    let Some((start, end)) = parsed.resolve(total) else {
        return Ok(AudioResponse::unsatisfiable(total));
    };

    file.seek(SeekFrom::Start(start)).await.map_err(|error| {
        log::warn!("Failed to seek DJ track {}: {error}", path.display());
        create_error!(InternalError)
    })?;

    Ok(AudioResponse::partial(file, start, end, total))
}

/// The `Range` request header, if present
pub struct RangeHeader(pub String);

#[rocket::async_trait]
impl<'r> rocket::request::FromRequest<'r> for RangeHeader {
    type Error = std::convert::Infallible;

    async fn from_request(
        request: &'r Request<'_>,
    ) -> rocket::request::Outcome<Self, Self::Error> {
        match request.headers().get_one("Range") {
            Some(value) => rocket::request::Outcome::Success(RangeHeader(value.to_string())),
            None => rocket::request::Outcome::Forward(Status::NotFound),
        }
    }
}

#[cfg(test)]
mod tests {
    use rocket::http::{Header, Status};
    use rocket::local::asynchronous::Client;

    use super::{parse_range, AudioResponse, ByteRange};
    use crate::dj::{paths, token, ENV_LOCK};

    const AUDIO: &[u8] = b"0123456789abcdef";

    fn temp_channel() -> (String, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "dj-stream-{}",
            ulid::Ulid::new().to_string().to_lowercase()
        ));
        std::env::set_var(paths::DJ_DIR_ENV, &dir);

        let channel = format!("chan-{}", ulid::Ulid::new());
        std::fs::create_dir_all(paths::channel_dir(&channel)).expect("channel dir");
        std::fs::write(paths::track_path(&channel, "t1"), AUDIO).expect("write track");

        (channel, dir)
    }

    async fn client() -> Client {
        let rocket = rocket::build().mount("/dj", rocket::routes![super::stream]);
        Client::tracked(rocket).await.expect("valid rocket")
    }

    fn signed(channel: &str) -> String {
        token::sign(channel, "t1", token::unix_secs() + 60)
    }

    #[rocket::async_test]
    async fn stream_requires_a_valid_token() {
        let _guard = ENV_LOCK.lock().await;
        let (channel, dir) = temp_channel();
        let client = client().await;

        // The result crate maps `InvalidDjToken` to 403 Forbidden
        let forbidden = Status::Forbidden;

        // No token at all
        let response = client.get(format!("/dj/{channel}/stream")).dispatch().await;
        assert_eq!(response.status(), forbidden);

        // Garbage token
        let response = client
            .get(format!("/dj/{channel}/stream?token=nope"))
            .dispatch()
            .await;
        assert_eq!(response.status(), forbidden);

        // A token minted for a different channel
        let response = client
            .get(format!("/dj/{channel}/stream?token={}", signed("other")))
            .dispatch()
            .await;
        assert_eq!(response.status(), forbidden);

        // An expired token
        let expired = token::sign(&channel, "t1", token::unix_secs() - 10);
        let response = client
            .get(format!("/dj/{channel}/stream?token={expired}"))
            .dispatch()
            .await;
        assert_eq!(response.status(), forbidden);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[rocket::async_test]
    async fn stream_serves_the_whole_file() {
        let _guard = ENV_LOCK.lock().await;
        let (channel, dir) = temp_channel();
        let client = client().await;

        let uri = format!("/dj/{channel}/stream?token={}", signed(&channel));

        let response = client.get(uri).dispatch().await;

        assert_eq!(response.status(), Status::Ok);
        assert!(
            response
                .headers()
                .get_one("Content-Type")
                .is_some_and(|value| value.starts_with("audio/")),
            "the file must be served as audio, not as a download"
        );
        assert_eq!(response.headers().get_one("Accept-Ranges"), Some("bytes"));
        assert_eq!(
            response.headers().get_one("Cache-Control"),
            Some("private, no-store"),
            "signed audio must never be stored by a shared cache"
        );
        // Rocket emits `Content-Length` from the preset body size, so the body
        // itself is what pins the exact length here.
        assert!(response.headers().get_one("Content-Range").is_none());
        assert_eq!(response.into_bytes().await.as_deref(), Some(AUDIO));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[rocket::async_test]
    async fn stream_answers_a_bounded_range_with_206() {
        let _guard = ENV_LOCK.lock().await;
        let (channel, dir) = temp_channel();
        let client = client().await;

        let uri = format!("/dj/{channel}/stream?token={}", signed(&channel));

        let response = client
            .get(&uri)
            .header(Header::new("Range", "bytes=0-3"))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::PartialContent);
        assert_eq!(
            response.headers().get_one("Content-Range"),
            Some("bytes 0-3/16")
        );
        assert_eq!(response.headers().get_one("Accept-Ranges"), Some("bytes"));

        assert_eq!(
            response.headers().get_one("Cache-Control"),
            Some("private, no-store")
        );
        assert_eq!(response.into_bytes().await.as_deref(), Some(&AUDIO[0..4]));

        // A range which runs past the end is clamped, not rejected
        let response = client
            .get(&uri)
            .header(Header::new("Range", "bytes=10-999"))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::PartialContent);
        assert_eq!(
            response.headers().get_one("Content-Range"),
            Some("bytes 10-15/16")
        );

        assert_eq!(response.into_bytes().await.as_deref(), Some(&AUDIO[10..]));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[rocket::async_test]
    async fn stream_answers_open_ended_and_suffix_ranges() {
        let _guard = ENV_LOCK.lock().await;
        let (channel, dir) = temp_channel();
        let client = client().await;

        let uri = format!("/dj/{channel}/stream?token={}", signed(&channel));

        // `bytes=10-` : from byte 10 to the end
        let response = client
            .get(&uri)
            .header(Header::new("Range", "bytes=10-"))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::PartialContent);
        assert_eq!(
            response.headers().get_one("Content-Range"),
            Some("bytes 10-15/16")
        );
        assert_eq!(response.into_bytes().await.as_deref(), Some(&AUDIO[10..]));

        // `bytes=-4` : the last four bytes
        let response = client
            .get(&uri)
            .header(Header::new("Range", "bytes=-4"))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::PartialContent);
        assert_eq!(
            response.headers().get_one("Content-Range"),
            Some("bytes 12-15/16")
        );

        assert_eq!(response.into_bytes().await.as_deref(), Some(&AUDIO[12..]));

        // A suffix longer than the file is the whole file
        let response = client
            .get(&uri)
            .header(Header::new("Range", "bytes=-999"))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::PartialContent);
        assert_eq!(
            response.headers().get_one("Content-Range"),
            Some("bytes 0-15/16")
        );
        assert_eq!(response.into_bytes().await.as_deref(), Some(AUDIO));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[rocket::async_test]
    async fn stream_rejects_unsatisfiable_ranges_with_416() {
        let _guard = ENV_LOCK.lock().await;
        let (channel, dir) = temp_channel();
        let client = client().await;

        let uri = format!("/dj/{channel}/stream?token={}", signed(&channel));

        for header in [
            "bytes=99-100",
            "bytes=16-",
            "bytes=5-1",
            "bytes=-0",
            // Multi-range: rejected rather than answered with the whole file
            "bytes=0-1,4-5",
            "nonsense",
        ] {
            let response = client
                .get(&uri)
                .header(Header::new("Range", header))
                .dispatch()
                .await;

            assert_eq!(
                response.status(),
                Status::RangeNotSatisfiable,
                "range {header:?} should be unsatisfiable"
            );
            assert_eq!(
                response.headers().get_one("Content-Range"),
                Some("bytes */16"),
                "range {header:?} must report the file size"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[rocket::async_test]
    async fn stream_rejects_traversal_and_reports_missing_files() {
        let _guard = ENV_LOCK.lock().await;
        let (channel, dir) = temp_channel();
        let client = client().await;

        // Percent-encoded `..` must not become a channel directory
        let response = client
            .get("/dj/%2e%2e/stream?token=whatever")
            .dispatch()
            .await;
        assert_eq!(
            response.status(),
            Status::Forbidden,
            "an unsafe channel id is rejected before the disk is touched"
        );

        // A valid token for a track which is no longer on disk
        let response = client
            .get(format!("/dj/{channel}/stream?token={}", signed(&channel)))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);

        std::fs::remove_file(paths::track_path(&channel, "t1")).expect("remove");

        let response = client
            .get(format!("/dj/{channel}/stream?token={}", signed(&channel)))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::NotFound);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_single_byte_ranges() {
        assert_eq!(parse_range("bytes=0-3"), Some(ByteRange::Bounded(0, 3)));
        assert_eq!(parse_range("bytes=10-"), Some(ByteRange::OpenEnded(10)));
        assert_eq!(parse_range("bytes=-4"), Some(ByteRange::Suffix(4)));
        assert_eq!(parse_range("bytes=5-5"), Some(ByteRange::Bounded(5, 5)));

        for rejected in [
            "bytes=0-1,4-5",
            "bytes=",
            "bytes=-",
            "items=0-1",
            "0-1",
            "bytes=a-b",
            "",
        ] {
            assert_eq!(parse_range(rejected), None, "{rejected:?} parsed");
        }
    }

    #[tokio::test]
    async fn responses_report_the_exact_content_length() {
        let dir = std::env::temp_dir().join(format!(
            "dj-stream-len-{}",
            ulid::Ulid::new().to_string().to_lowercase()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("t.mp3");
        std::fs::write(&path, AUDIO).expect("write");

        let file = rocket::tokio::fs::File::open(&path).await.expect("open");
        assert_eq!(AudioResponse::whole(file, AUDIO.len() as u64).content_length(), 16);

        let file = rocket::tokio::fs::File::open(&path).await.expect("open");
        assert_eq!(AudioResponse::partial(file, 4, 7, 16).content_length(), 4);

        let file = rocket::tokio::fs::File::open(&path).await.expect("open");
        assert_eq!(AudioResponse::partial(file, 10, 15, 16).content_length(), 6);

        assert_eq!(AudioResponse::unsatisfiable(16).content_length(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolves_ranges_against_the_file_length() {
        assert_eq!(ByteRange::Bounded(0, 3).resolve(16), Some((0, 3)));
        assert_eq!(ByteRange::Bounded(10, 999).resolve(16), Some((10, 15)));
        assert_eq!(ByteRange::OpenEnded(10).resolve(16), Some((10, 15)));
        assert_eq!(ByteRange::Suffix(4).resolve(16), Some((12, 15)));
        assert_eq!(ByteRange::Suffix(999).resolve(16), Some((0, 15)));

        // Unsatisfiable
        assert_eq!(ByteRange::Bounded(16, 20).resolve(16), None);
        assert_eq!(ByteRange::Bounded(5, 1).resolve(16), None);
        assert_eq!(ByteRange::OpenEnded(16).resolve(16), None);
        assert_eq!(ByteRange::Suffix(0).resolve(16), None);
        // An empty file cannot satisfy any range
        assert_eq!(ByteRange::Bounded(0, 0).resolve(0), None);
    }
}
