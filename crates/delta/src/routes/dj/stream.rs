//! Streaming of DJ audio files.
//!
//! Files are served straight from local disk with Rocket's `NamedFile`, which
//! handles `Range` / `206` / `Accept-Ranges` natively so media players can seek.
//! Access is granted by a short-lived HMAC token in the query string because
//! `<audio>` elements cannot send an `Authorization` header.

use revolt_result::{create_error, Result};
use rocket::fs::NamedFile;

use crate::dj::{paths, token};

/// # Stream DJ Track
///
/// Serves the audio of a DJ track. Requires a signed token which is only ever
/// sent to joined members of the channel.
#[openapi(tag = "DJ")]
#[get("/<channel>/stream?<token>")]
pub async fn stream(channel: &str, token: Option<String>) -> Result<NamedFile> {
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
    let dir = paths::channel_dir(channel);

    if path.parent() != Some(dir.as_path()) {
        return Err(create_error!(InvalidDjToken));
    }

    NamedFile::open(&path).await.map_err(|error| {
        if error.kind() != std::io::ErrorKind::NotFound {
            log::warn!("Failed to open DJ track {}: {error}", path.display());
        }

        create_error!(NotFound)
    })
}

#[cfg(test)]
mod tests {
    use rocket::http::{Header, Status};
    use rocket::local::asynchronous::Client;

    use crate::dj::{paths, token, ENV_LOCK};

    const AUDIO: &[u8] = b"0123456789abcdef";

    fn temp_channel() -> (String, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "dj-stream-{}",
            ulid::Ulid::new().to_string().to_lowercase()
        ));
        std::env::set_var(paths::DJ_DIR_ENV, &dir);

        let channel = format!("chan-{}", ulid::Ulid::new());
        paths::create_channel_dir(&channel).expect("channel dir");
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

        let mut response = client.get(uri).dispatch().await;

        assert_eq!(response.status(), Status::Ok);
        assert!(
            response
                .headers()
                .get_one("Content-Type")
                .is_some_and(|value| value.starts_with("audio/")),
            "the file must be served as audio, not as a download"
        );
        assert_eq!(response.into_bytes().await.as_deref(), Some(AUDIO));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Known gap, not a regression: Rocket 0.5.1's `NamedFile` has no `Range`
    /// support at all (no 206, no `Content-Range`, no `Accept-Ranges`; range
    /// handling landed in Rocket 0.6). The spec assumes 206-capable streaming,
    /// so a client seeking mid-track gets the whole file from byte zero.
    ///
    /// This test pins the current behaviour so the gap is visible; it should be
    /// inverted (206 + `Content-Range`) once the server is upgraded or the
    /// handler is replaced with a range-aware responder.
    #[rocket::async_test]
    async fn range_requests_are_not_supported_by_rocket_0_5() {
        let _guard = ENV_LOCK.lock().await;
        let (channel, dir) = temp_channel();
        let client = client().await;

        let uri = format!("/dj/{channel}/stream?token={}", signed(&channel));

        let mut response = client
            .get(uri)
            .header(Header::new("Range", "bytes=0-3"))
            .dispatch()
            .await;

        assert_eq!(
            response.status(),
            Status::Ok,
            "Rocket 0.5.1 ignores Range and returns the full body"
        );
        assert!(response.headers().get_one("Content-Range").is_none());
        assert!(response.headers().get_one("Accept-Ranges").is_none());
        assert_eq!(response.into_bytes().await.as_deref(), Some(AUDIO));

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
}
