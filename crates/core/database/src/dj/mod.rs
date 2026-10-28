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

pub mod skip;
pub use skip::*;

/// Prefix shared by every key owned by this module
pub const KEY_PREFIX: &str = "dj_";

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

/// Delete every `dj_*` key in the database
///
/// Called on process start: DJ queues are intentionally not durable.
pub async fn clear_all() -> Result<()> {
    let mut conn = get_connection().await?;

    let keys: Vec<String> = conn
        .keys(format!("{KEY_PREFIX}*"))
        .await
        .to_internal_error()?;

    if !keys.is_empty() {
        let _: u32 = conn.del(keys).await.to_internal_error()?;
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
