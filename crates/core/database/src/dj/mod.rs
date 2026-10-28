//! Redis-backed state for DJ channels.
//!
//! The DJ queue is intentionally ephemeral: there is no durable store and no
//! crash recovery. Everything lives in Redis under `dj_*` keys and on the local
//! disk of the delta instance which owns playback, so any restart wipes the
//! queue (see [`clear_all`]).
//!
//! Keys:
//! ```text
//! dj_members:{channel}          SET    user ids which joined the channel
//! dj_queue:{channel}            LIST   JSON entries, FIFO (LPOP / RPUSH)
//! dj_now:{channel}              HASH   current track + started_at
//! dj_votes:{channel}:{track}    SET    user ids which voted to skip a track
//! dj_seq:{channel}              STRING counter for track ids
//! dj_empty_since:{channel}      STRING unix ms at which the channel last emptied
//! dj_user:{user}                SET    channel ids the user has joined
//! ```

use redis_kiss::{get_connection as _get_connection, redis::AsyncCommands, Conn};
use revolt_models::v0::{DjNowPlaying, DjTrack};
use revolt_result::{create_error, Result, ToRevoltError};

pub mod events;
pub mod skip;
pub use skip::*;

#[cfg(test)]
mod redis_tests;

#[cfg(test)]
mod serde_tests;

/// Prefix shared by every key owned by this module
pub const KEY_PREFIX: &str = "dj_";

/// Amount of keys requested per `SCAN` iteration
///
/// `SCAN` is used instead of `KEYS` everywhere so that a large DJ keyspace can
/// never block the Redis server for the whole application.
const SCAN_COUNT: usize = 512;

/// Collect every key matching `pattern` using `SCAN`
async fn scan_keys(pattern: &str) -> Result<Vec<String>> {
    let mut conn = get_connection().await?;
    let mut cursor: u64 = 0;
    let mut keys = Vec::new();

    loop {
        let (next, batch): (u64, Vec<String>) = redis_kiss::redis::cmd("SCAN")
            .arg(cursor)
            .arg("MATCH")
            .arg(pattern)
            .arg("COUNT")
            .arg(SCAN_COUNT)
            .query_async(&mut *conn)
            .await
            .to_internal_error()?;

        keys.extend(batch);
        cursor = next;

        if cursor == 0 {
            break;
        }
    }

    Ok(keys)
}

/// Read the id of the currently playing track inside a Lua script.
///
/// Compare-and-set helpers below read `dj_now` inside Lua, where the track id is
/// extracted from the stored JSON. Track ids only ever contain URL-safe
/// characters (see [`events`] and `crate::dj::KEY_PREFIX` users), so they are
/// never escaped by serde_json and the pattern below is exact.
///
/// ```lua
/// local current = redis.call('HGET', KEYS[1], 'track')
/// if not current then return false, nil end
/// ```
///
/// Recorded here so the duplicated snippet in each script stays identical.
const _TRACK_ID_PATTERN_DOC: &str = r#""id":"([^"]+)""#;

async fn get_connection() -> Result<Conn> {
    _get_connection()
        .await
        .map_err(|_| create_error!(InternalError))
}

/// Set of users which joined a channel
pub fn members_key(channel: &str) -> String {
    format!("dj_members:{channel}")
}

/// FIFO list of queued tracks
pub fn queue_key(channel: &str) -> String {
    format!("dj_queue:{channel}")
}

/// Hash containing the currently playing track
pub fn now_key(channel: &str) -> String {
    format!("dj_now:{channel}")
}

/// Set of users which voted to skip a track
pub fn votes_key(channel: &str, track: &str) -> String {
    format!("dj_votes:{channel}:{track}")
}

/// Counter used to generate track ids
pub fn seq_key(channel: &str) -> String {
    format!("dj_seq:{channel}")
}

/// Timestamp at which the channel became empty
pub fn empty_since_key(channel: &str) -> String {
    format!("dj_empty_since:{channel}")
}

/// Set of channels a user has joined
pub fn user_channels_key(user: &str) -> String {
    format!("dj_user:{user}")
}

/// Fetch the ids of users which joined the channel
pub async fn get_members(channel: &str) -> Result<Vec<String>> {
    get_connection()
        .await?
        .smembers(members_key(channel))
        .await
        .to_internal_error()
}

/// Amount of users which joined the channel
pub async fn member_count(channel: &str) -> Result<u32> {
    get_connection()
        .await?
        .scard(members_key(channel))
        .await
        .to_internal_error()
}

/// Whether the user has joined the channel
pub async fn is_member(channel: &str, user: &str) -> Result<bool> {
    get_connection()
        .await?
        .sismember(members_key(channel), user)
        .await
        .to_internal_error()
}

/// Add the user to the channel, returning the new member count
pub async fn add_member(channel: &str, user: &str) -> Result<u32> {
    let mut conn = get_connection().await?;

    let _: u32 = conn
        .sadd(members_key(channel), user)
        .await
        .to_internal_error()?;
    let _: u32 = conn
        .sadd(user_channels_key(user), channel)
        .await
        .to_internal_error()?;

    // Joining cancels the empty-channel grace period
    let _: u32 = conn
        .del(empty_since_key(channel))
        .await
        .to_internal_error()?;

    conn.scard(members_key(channel)).await.to_internal_error()
}

/// Remove the user from the channel, returning the new member count
pub async fn remove_member(channel: &str, user: &str) -> Result<u32> {
    let mut conn = get_connection().await?;

    let _: u32 = conn
        .srem(members_key(channel), user)
        .await
        .to_internal_error()?;
    let _: u32 = conn
        .srem(user_channels_key(user), channel)
        .await
        .to_internal_error()?;

    conn.scard(members_key(channel)).await.to_internal_error()
}

/// Fetch the channels a user has joined
pub async fn get_user_channels(user: &str) -> Result<Vec<String>> {
    get_connection()
        .await?
        .smembers(user_channels_key(user))
        .await
        .to_internal_error()
}

/// Every channel which currently has at least one member
///
/// Used by the reconciliation sweep, which has to start from the channels that
/// exist rather than from the users Redis happens to know about.
pub async fn member_channels() -> Result<Vec<String>> {
    let prefix = members_key("");

    let mut channels = scan_keys(&format!("{prefix}*"))
        .await?
        .into_iter()
        .filter_map(|key| key.strip_prefix(&prefix).map(str::to_string))
        .collect::<Vec<_>>();

    channels.sort();
    channels.dedup();

    Ok(channels)
}

/// Remove the user from every channel they joined
///
/// Used when their websocket session closes.
pub async fn remove_user_from_all_channels(user: &str) -> Result<Vec<String>> {
    let channels = get_user_channels(user).await?;

    for channel in &channels {
        remove_member(channel, user).await?;
    }

    Ok(channels)
}

/// Fetch the queued tracks, in playback order
pub async fn get_queue(channel: &str) -> Result<Vec<DjTrack>> {
    let entries: Vec<String> = get_connection()
        .await?
        .lrange(queue_key(channel), 0, -1)
        .await
        .to_internal_error()?;

    Ok(entries
        .iter()
        .filter_map(|entry| serde_json::from_str(entry).ok())
        .collect())
}

/// Amount of tracks currently queued
pub async fn queue_len(channel: &str) -> Result<u32> {
    get_connection()
        .await?
        .llen(queue_key(channel))
        .await
        .to_internal_error()
}

/// Append a track to the back of the queue, returning the new queue depth
pub async fn push_track(channel: &str, track: &DjTrack) -> Result<u32> {
    let entry = serde_json::to_string(track).to_internal_error()?;

    get_connection()
        .await?
        .rpush(queue_key(channel), entry)
        .await
        .to_internal_error()
}

/// Pop the track at the front of the queue
pub async fn pop_track(channel: &str) -> Result<Option<DjTrack>> {
    let entry: Option<String> = get_connection()
        .await?
        .lpop(queue_key(channel), None)
        .await
        .to_internal_error()?;

    Ok(entry.and_then(|entry| serde_json::from_str(&entry).ok()))
}

/// Fetch the currently playing track
pub async fn get_now(channel: &str) -> Result<Option<DjNowPlaying>> {
    let mut conn = get_connection().await?;

    let track: Option<String> = conn
        .hget(now_key(channel), "track")
        .await
        .to_internal_error()?;
    let started_at: Option<u64> = conn
        .hget(now_key(channel), "started_at")
        .await
        .to_internal_error()?;

    let (Some(track), Some(started_at)) = (track, started_at) else {
        return Ok(None);
    };

    let Some(track) = serde_json::from_str(&track).ok() else {
        return Ok(None);
    };

    Ok(Some(DjNowPlaying { track, started_at }))
}

/// Mark a track as the currently playing track
pub async fn set_now(channel: &str, now: &DjNowPlaying) -> Result<()> {
    let track = serde_json::to_string(&now.track).to_internal_error()?;

    let mut conn = get_connection().await?;

    let _: u32 = conn
        .hset(now_key(channel), "track", track)
        .await
        .to_internal_error()?;
    let _: u32 = conn
        .hset(now_key(channel), "started_at", now.started_at)
        .await
        .to_internal_error()?;

    Ok(())
}

/// Clear the currently playing track
pub async fn clear_now(channel: &str) -> Result<()> {
    let _: u32 = get_connection()
        .await?
        .del(now_key(channel))
        .await
        .to_internal_error()?;

    Ok(())
}

/// Record a vote to skip the given track, returning the amount of votes cast
///
/// This does **not** check that the track is still playing: routes must use
/// [`cast_vote`] so a vote can never land on a track which has already ended.
pub async fn add_vote(channel: &str, track: &str, user: &str) -> Result<u32> {
    let mut conn = get_connection().await?;

    let _: u32 = conn
        .sadd(votes_key(channel, track), user)
        .await
        .to_internal_error()?;

    conn.scard(votes_key(channel, track))
        .await
        .to_internal_error()
}

/// Outcome of a compare-and-set skip vote
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoteOutcome {
    /// The vote was recorded, this is the total amount of votes now cast
    Cast(u32),
    /// Nothing is playing, so there is nothing to vote against
    NoTrack,
    /// The track changed between reading `dj_now` and casting the vote
    TrackChanged,
}

/// Vote to skip `track`, but only while it is still the track in `dj_now`
///
/// Reading `dj_now` and recording the vote happen inside one Lua script, so a
/// vote can neither land on a track which ended microseconds ago (which would
/// leak a `dj_votes:*` key nobody ever clears) nor count towards the next one.
pub async fn cast_vote(channel: &str, track: &str, user: &str) -> Result<VoteOutcome> {
    // KEYS[1] = dj_now, KEYS[2] = dj_votes:{channel}:{track}
    // ARGV[1] = expected track id, ARGV[2] = user id
    const SCRIPT: &str = r#"
local current = redis.call('HGET', KEYS[1], 'track')
if not current then return -1 end
local id = string.match(current, '"id":"([^"]+)"')
if not id or id ~= ARGV[1] then return -2 end
redis.call('SADD', KEYS[2], ARGV[2])
return redis.call('SCARD', KEYS[2])
"#;

    let mut conn = get_connection().await?;

    let result: i64 = redis_kiss::redis::Script::new(SCRIPT)
        .key(now_key(channel))
        .key(votes_key(channel, track))
        .arg(track)
        .arg(user)
        .invoke_async(&mut *conn)
        .await
        .to_internal_error()?;

    Ok(match result {
        -1 => VoteOutcome::NoTrack,
        -2 => VoteOutcome::TrackChanged,
        votes => VoteOutcome::Cast(votes.max(0) as u32),
    })
}

/// Fetch the users which voted to skip the given track
pub async fn get_voters(channel: &str, track: &str) -> Result<Vec<String>> {
    get_connection()
        .await?
        .smembers(votes_key(channel, track))
        .await
        .to_internal_error()
}

/// Drop votes cast by users which are no longer members of the channel
pub async fn drop_stale_votes(channel: &str, track: &str, members: &[String]) -> Result<()> {
    let voters = get_voters(channel, track).await?;

    let stale = voters
        .into_iter()
        .filter(|voter| !members.contains(voter))
        .collect::<Vec<_>>();

    if stale.is_empty() {
        return Ok(());
    }

    let mut conn = get_connection().await?;

    for voter in stale {
        let _: u32 = conn
            .srem(votes_key(channel, track), voter)
            .await
            .to_internal_error()?;
    }

    Ok(())
}

/// Clear the votes for the given track
pub async fn clear_votes(channel: &str, track: &str) -> Result<()> {
    let _: u32 = get_connection()
        .await?
        .del(votes_key(channel, track))
        .await
        .to_internal_error()?;

    Ok(())
}

/// Increment and return the track id counter for this channel
pub async fn next_seq(channel: &str) -> Result<u64> {
    let seq: i64 = get_connection()
        .await?
        .incr(seq_key(channel), 1)
        .await
        .to_internal_error()?;

    Ok(seq.max(0) as u64)
}

/// Mark the channel as having become empty at the given unix timestamp (ms)
pub async fn set_empty_since(channel: &str, timestamp: u64) -> Result<()> {
    let _: () = get_connection()
        .await?
        .set(empty_since_key(channel), timestamp)
        .await
        .to_internal_error()?;

    Ok(())
}

/// Fetch the unix timestamp (ms) at which the channel became empty
pub async fn get_empty_since(channel: &str) -> Result<Option<u64>> {
    get_connection()
        .await?
        .get(empty_since_key(channel))
        .await
        .to_internal_error()
}

/// Clear the empty-channel grace period
pub async fn clear_empty_since(channel: &str) -> Result<()> {
    let _: u32 = get_connection()
        .await?
        .del(empty_since_key(channel))
        .await
        .to_internal_error()?;

    Ok(())
}

/// Delete every key belonging to the given channel
pub async fn clear_channel(channel: &str) -> Result<()> {
    let members = get_members(channel).await?;
    let now = get_now(channel).await?;

    let mut conn = get_connection().await?;

    let _: u32 = conn
        .del(&[
            members_key(channel),
            queue_key(channel),
            now_key(channel),
            seq_key(channel),
            empty_since_key(channel),
        ])
        .await
        .to_internal_error()?;

    if let Some(now) = now {
        let _: u32 = conn
            .del(votes_key(channel, &now.track.id))
            .await
            .to_internal_error()?;
    }

    for member in members {
        let _: u32 = conn
            .srem(user_channels_key(&member), channel)
            .await
            .to_internal_error()?;
    }

    Ok(())
}

/// Delete the channel's state only if it is still empty
///
/// The emptiness check and the deletion happen in one Lua script, so a member
/// who joins while the engine is deciding to clear cannot end up in a channel
/// whose queue, now and files were just wiped. Returns whether it cleared.
pub async fn clear_channel_if_empty(channel: &str) -> Result<bool> {
    // KEYS = members, queue, now, seq, empty_since
    // ARGV[1] = `dj_votes:{channel}` prefix used to clean up the votes key
    const SCRIPT: &str = r#"
if redis.call('SCARD', KEYS[1]) > 0 then return 0 end

local current = redis.call('HGET', KEYS[3], 'track')
if current then
    local id = string.match(current, '"id":"([^"]+)"')
    if id then redis.call('DEL', ARGV[1] .. ':' .. id) end
end

redis.call('DEL', KEYS[1], KEYS[2], KEYS[3], KEYS[4], KEYS[5])
return 1
"#;

    let mut conn = get_connection().await?;

    let cleared: i64 = redis_kiss::redis::Script::new(SCRIPT)
        .key(members_key(channel))
        .key(queue_key(channel))
        .key(now_key(channel))
        .key(seq_key(channel))
        .key(empty_since_key(channel))
        .arg(format!("dj_votes:{channel}"))
        .invoke_async(&mut *conn)
        .await
        .to_internal_error()?;

    Ok(cleared == 1)
}

/// Delete every `dj_*` key in the database
///
/// Called on process start: DJ queues are intentionally not durable.
pub async fn clear_all() -> Result<()> {
    let mut conn = get_connection().await?;

    for key in scan_keys(&format!("{KEY_PREFIX}*")).await? {
        let _: u32 = conn.del(&key).await.to_internal_error()?;
    }

    Ok(())
}

/// Current unix timestamp in milliseconds
pub fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}
