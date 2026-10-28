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

/// Lifetime of a stream token, in seconds
///
/// Deliberately short: the engine re-signs a URL on every heartbeat (every
/// [`crate::dj::HEARTBEAT_MS`] ms), so a leaked URL stops working within a few
/// minutes instead of staying valid for the whole track plus an hour.
pub const TOKEN_TTL_SECS: u64 = 180;

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
pub fn stream_url(api_url: &str, channel: &str, track: &str) -> String {
    let expiry = unix_secs() + TOKEN_TTL_SECS;
    let token = sign(channel, track, expiry);

    format!(
        "{}/dj/{}/stream?token={token}",
        api_url.trim_end_matches('/'),
        channel
    )
}

/// Lifetime of a stream token
pub fn token_ttl() -> Duration {
    Duration::from_secs(TOKEN_TTL_SECS)
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
        let url = stream_url("https://api.example.com/", "chan", "track");

        assert!(url.starts_with("https://api.example.com/dj/chan/stream?token="));
    }

    #[test]
    fn stream_url_signature_verifies_and_carries_the_track() {
        let url = stream_url("https://api.example.com", "chan", "track-1");
        let token = url.split_once("token=").expect("token in url").1;

        assert_eq!(verify("chan", token).as_deref(), Some("track-1"));
        // The URL embeds the channel in the path, so a token is not portable
        assert!(verify("chan2", token).is_none());
    }

    #[test]
    fn token_rejects_a_single_flipped_mac_nibble() {
        let token = sign("channel", "track", future(60));

        for index in [token.len() - 1, token.len() - 32, token.len() - 64] {
            let mut bytes = token.clone().into_bytes();
            bytes[index] = if bytes[index] == b'0' { b'1' } else { b'0' };

            let forged = String::from_utf8(bytes).expect("still utf-8");
            assert_ne!(forged, token);
            assert!(
                verify("channel", &forged).is_none(),
                "forged mac at byte {} was accepted",
                index
            );
        }
    }

    #[test]
    fn token_rejects_a_truncated_or_extended_mac() {
        let token = sign("channel", "track", future(60));

        let truncated = &token[..token.len() - 1];
        assert!(verify("channel", truncated).is_none());

        let extended = format!("{token}00");
        assert!(verify("channel", &extended).is_none());
    }

    #[test]
    fn token_rejects_extra_components() {
        let token = sign("channel", "track", future(60));
        let forged = format!("{token}.1");

        assert!(verify("channel", &forged).is_none());
    }

    #[test]
    fn token_expiry_is_only_accepted_in_the_future() {
        assert!(verify("channel", &sign("channel", "track", unix_secs() + 2)).is_some());
        assert!(verify("channel", &sign("channel", "track", unix_secs() - 1)).is_none());
        // Epoch zero is long expired
        assert!(verify("channel", &sign("channel", "track", 0)).is_none());
    }

    #[test]
    fn token_rejects_a_negative_expiry() {
        // "-1" does not parse as u64 and must not be treated as "no expiry"
        let structure = sign("channel", "track", 1000);
        let rest = structure.splitn(2, '.').nth(1).expect("track and mac");
        let forged = format!("-1.{rest}");

        assert!(verify("channel", &forged).is_none());
    }

    #[test]
    fn track_id_length_boundary() {
        let allowed = "a".repeat(128);
        let rejected = "a".repeat(129);

        assert!(verify("channel", &sign("channel", &allowed, future(60))).is_some());
        assert!(verify("channel", &sign("channel", &rejected, future(60))).is_none());
    }

    #[test]
    fn track_ids_with_path_separators_never_verify() {
        for id in ["../x", "a/b", "a\\b", "a.b", "a b", "", "%2e%2e"] {
            let token = sign("channel", id, future(60));
            assert!(
                verify("channel", &token).is_none(),
                "unsafe track id {:?} was accepted",
                id
            );
        }
    }

    #[test]
    fn hmac_hashes_keys_longer_than_the_block_size() {
        // RFC 4231 test case 6: a 131 byte key is hashed down to 32 bytes
        let mac = hmac_sha256(
            &[0xaa; 131],
            b"Test Using Larger Than Block-Size Key - Hash Key First",
        );

        assert_eq!(
            hex(&mac),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn token_ttl_is_short_but_outlives_several_heartbeats() {
        let ttl = token_ttl();

        assert_eq!(ttl, Duration::from_secs(TOKEN_TTL_SECS));
        // Long enough that a client which misses a heartbeat or two can still
        // seek, short enough that a leaked URL is not usable for an hour.
        assert!(ttl >= Duration::from_millis(crate::dj::HEARTBEAT_MS * 4));
        assert!(ttl <= Duration::from_secs(300));
    }
}
