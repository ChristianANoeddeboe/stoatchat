//! Signed tokens for DJ stream URLs.
//!
//! `<audio>` elements and media players cannot send an `Authorization` header,
//! so stream URLs carry a short-lived HMAC token over `(channel, track, expiry)`
//! in the query string instead.
//!
//! The secret is read from `DJ_STREAM_SECRET`. If it is unset a random secret
//! is generated per process, which is acceptable because DJ queues are not
//! durable across restarts either: a restart clears the queue, so previously
//! issued URLs have nothing left to point at.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use once_cell::sync::OnceCell;
use sha2::{Digest, Sha256};

/// Environment variable holding the token signing secret
pub const DJ_STREAM_SECRET_ENV: &str = "DJ_STREAM_SECRET";

/// Extra lifetime added on top of the channel's maximum track duration
pub const TOKEN_TTL_MARGIN_SECS: u64 = 60;

static SECRET: OnceCell<Vec<u8>> = OnceCell::new();

fn secret() -> &'static [u8] {
    SECRET.get_or_init(|| {
        let from_env = std::env::var(DJ_STREAM_SECRET_ENV).unwrap_or_default();

        if from_env.is_empty() {
            log::warn!(
                "{DJ_STREAM_SECRET_ENV} is not set, DJ stream tokens will not survive a restart"
            );

            rand::random::<[u8; 32]>().to_vec()
        } else {
            from_env.into_bytes()
        }
    })
}

/// Current unix timestamp in seconds
pub fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

/// HMAC-SHA256 of the given message
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;

    let mut key_block = [0u8; BLOCK];

    if key.len() > BLOCK {
        key_block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut inner = Sha256::new();
    for byte in key_block {
        inner.update([byte ^ 0x36]);
    }
    inner.update(message);
    let inner = inner.finalize();

    let mut outer = Sha256::new();
    for byte in key_block {
        outer.update([byte ^ 0x5c]);
    }
    outer.update(inner);
    outer.finalize().into()
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);

    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }

    out
}

/// Compute a token for the given channel and track, valid until `expiry`
///
/// The track is embedded in the token so that the stream URL is a property of
/// the channel alone: `/dj/{channel}/stream?token=...`.
pub fn sign(channel: &str, track: &str, expiry: u64) -> String {
    let message = format!("{channel}|{track}|{expiry}");
    let mac = hmac_sha256(secret(), message.as_bytes());

    format!("{expiry}.{track}.{}", hex(&mac))
}

/// Verify a token belonging to the given channel, returning the track id
pub fn verify(channel: &str, token: &str) -> Option<String> {
    let mut parts = token.splitn(3, '.');

    let expiry = parts.next()?.parse::<u64>().ok()?;
    let track = parts.next()?;
    // Ensure a MAC is present, it is compared as part of the whole token below
    parts.next()?;

    if expiry < unix_secs() || !crate::dj::paths::is_safe_id(track) {
        return None;
    }

    let expected = sign(channel, track, expiry);

    // Compare in full to avoid leaking the expected value through timing
    if expected.len() != token.len() {
        return None;
    }

    let matches = expected
        .bytes()
        .zip(token.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0;

    matches.then(|| track.to_string())
}

/// Build a signed stream URL for the given track
pub fn stream_url(api_url: &str, channel: &str, track: &str, max_track_seconds: u32) -> String {
    let ttl = max_track_seconds as u64 + TOKEN_TTL_MARGIN_SECS;
    let expiry = unix_secs() + ttl;
    let token = sign(channel, track, expiry);

    format!(
        "{}/dj/{}/stream?token={token}",
        api_url.trim_end_matches('/'),
        channel
    )
}

/// Lifetime of a stream token for the given channel settings
pub fn token_ttl(max_track_seconds: u32) -> Duration {
    Duration::from_secs(max_track_seconds as u64 + TOKEN_TTL_MARGIN_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn future(seconds: u64) -> u64 {
        unix_secs() + seconds
    }

    #[test]
    fn token_round_trip() {
        let token = sign("channel", "track", future(60));

        assert_eq!(verify("channel", &token).as_deref(), Some("track"));
    }

    #[test]
    fn token_is_bound_to_channel_and_track() {
        let token = sign("channel", "track", future(60));

        assert!(verify("other", &token).is_none());

        // Rewriting the embedded track invalidates the MAC
        let forged = token.replacen("track", "other", 1);
        assert!(verify("channel", &forged).is_none());
    }

    #[test]
    fn expired_token_is_rejected() {
        let token = sign("channel", "track", unix_secs() - 1);

        assert!(verify("channel", &token).is_none());
    }

    #[test]
    fn malformed_tokens_are_rejected() {
        assert!(verify("channel", "").is_none());
        assert!(verify("channel", "not-a-token").is_none());
        assert!(verify("channel", "abc.def.ghi").is_none());
        assert!(verify("channel", &format!("{}.track.", future(60))).is_none());
    }

    #[test]
    fn tampered_expiry_is_rejected() {
        let token = sign("channel", "track", future(60));
        let mac = token.rsplit('.').next().unwrap();

        // Replace the expiry with a later one, keeping the MAC
        let tampered = format!("{}.track.{mac}", future(600));

        assert!(verify("channel", &tampered).is_none());
    }

    #[test]
    fn unsafe_track_ids_are_rejected() {
        let token = sign("channel", "../../etc/passwd", future(60));

        assert!(verify("channel", &token).is_none());
    }

    #[test]
    fn hmac_matches_known_vector() {
        // RFC 4231 test case 2
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");

        assert_eq!(
            hex(&mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn stream_url_contains_token() {
        let url = stream_url("https://api.example.com/", "chan", "track", 10);

        assert!(url.starts_with("https://api.example.com/dj/chan/stream?token="));
    }
}
