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
