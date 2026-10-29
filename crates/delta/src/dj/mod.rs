//! DJ channel playback engine.
//!
//! One `tokio` task is spawned per DJ channel which has members. The task owns
//! playback for that channel: it pops the queue, publishes events and deletes
//! files which have finished playing.
//!
//! **This is single-instance only.** There is no worker lease and no durable
//! queue: state lives in Redis and files live on the local disk of this delta
//! instance. On start up every `dj_*` key and every file under `DJ_DIR` is
//! removed (see [`DjEngine::wipe_all`]).

pub mod paths;
pub mod token;
pub mod transcode;
pub mod youtube;

#[cfg(test)]
mod engine_tests;

/// Serialises tests which change process-wide DJ configuration (`DJ_DIR`)
#[cfg(test)]
pub(crate) static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use revolt_database::{dj, events::client::EventV1, Database};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use tokio::sync::{Notify, RwLock};

/// Amount of time between `Dj.NowPlaying` heartbeats
pub const HEARTBEAT_MS: u64 = 15_000;

/// Safety margin added to the end of a track before it is considered finished
pub const END_MARGIN_MS: u64 = 5_000;

/// Delay before a finished track's file is deleted, leaving slow clients time
/// to finish buffering
pub const FILE_DELETE_DELAY_SECS: u64 = 60;

/// Grace period after the last member leaves, before the queue is cleared
pub const EMPTY_GRACE_MS: u64 = 30_000;

/// How long the engine sleeps when there is nothing to do
///
/// Every state change (`join`, `leave`, `queue`, `skip`, reconciliation) calls
/// [`DjEngine::notify`], so this is only a safety net for anything which does
/// not: it is not a poll interval and must not be used as one.
const IDLE_WAIT_MS: u64 = 5_000;

/// How often orphaned files are swept from `DJ_DIR`
const SWEEP_INTERVAL: Duration = Duration::from_secs(600);

/// How often membership is reconciled against live websocket sessions
const RECONCILE_INTERVAL: Duration = Duration::from_secs(60);

/// Files younger than this are never swept
///
/// Uploads are staged as `*.part` and only renamed after the output has been
/// probed, so this no longer has to outlive an in-flight upload: the coincidence
/// with `UPLOAD_DEADLINE` is gone, and a partially transcoded file is never part
/// of the keep set in the first place.
const SWEEP_MIN_AGE: Duration = Duration::from_secs(300);

/// Per-channel runtime state held by the engine
pub struct DjRuntime {
    /// Id of the channel this runtime belongs to
    pub channel: String,
    settings: RwLock<v0::DjInformation>,
    notify: Notify,
    /// Set once the task returned, panicked or was cancelled
    ///
    /// `start` replaces a finished entry so a panic can never leave a channel
    /// permanently without playback.
    finished: AtomicBool,
    /// Set when playback was explicitly stopped (channel deleted, DJ disabled)
    ///
    /// Unlike `finished` this is set *by* the stopper, so the task exits on its
    /// own terms instead of being aborted mid-write.
    stopped: AtomicBool,
}

impl DjRuntime {
    fn new(channel: &str, settings: v0::DjInformation) -> Self {
        Self {
            channel: channel.to_string(),
            settings: RwLock::new(settings),
            notify: Notify::new(),
            finished: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
        }
    }

    /// Current settings of the channel
    pub async fn settings(&self) -> v0::DjInformation {
        self.settings.read().await.clone()
    }

    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    fn is_finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }

    /// Whether the task is still expected to make progress
    ///
    /// A stopped runtime is as dead as a finished one: it wakes up, notices and
    /// returns, so `start` must replace it rather than hand it to a new joiner.
    fn is_alive(&self) -> bool {
        !self.is_finished() && !self.is_stopped()
    }
}

struct Inner {
    channels: DashMap<String, Arc<DjRuntime>>,
    api_url: String,
}

/// In-process manager of DJ channel playback tasks
#[derive(Clone)]
pub struct DjEngine {
    inner: Arc<Inner>,
}

impl DjEngine {
    /// Create a new engine, using `api_url` as the base for signed stream URLs
    pub fn new(api_url: String) -> Self {
        Self {
            inner: Arc::new(Inner {
                channels: DashMap::new(),
                api_url: api_url.trim_end_matches('/').to_string(),
            }),
        }
    }

    /// Base URL of the API, used to build signed stream URLs
    pub fn api_url(&self) -> &str {
        &self.inner.api_url
    }

    /// Whether a playback task is running for this channel
    pub fn is_running(&self, channel: &str) -> bool {
        self.inner
            .channels
            .get(channel)
            .is_some_and(|runtime| runtime.is_alive())
    }

    /// Start (or update) the playback task for a channel
    ///
    /// The task is started on the first join and keeps running until the
    /// channel has been empty past the grace period. A finished entry (task
    /// returned, panicked or was cancelled) is replaced, so a channel can never
    /// be left permanently dead.
    pub async fn start(&self, channel: &str, settings: v0::DjInformation) -> Arc<DjRuntime> {
        // Drop a dead entry, or update the live one
        loop {
            let Some(existing) = self.inner.channels.get(channel).map(|entry| entry.clone()) else {
                break;
            };

            if existing.is_alive() {
                *existing.settings.write().await = settings;
                existing.notify.notify_one();
                return existing;
            }

            drop(existing);
            self.inner
                .channels
                .remove_if(channel, |_, runtime| !runtime.is_alive());
        }

        let runtime = Arc::new(DjRuntime::new(channel, settings.clone()));

        // Do not clobber a task which another request started concurrently
        match self.inner.channels.entry(channel.to_string()) {
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                entry.insert(runtime.clone());
            }
            dashmap::mapref::entry::Entry::Occupied(entry) => {
                let existing = entry.get().clone();
                *existing.settings.write().await = settings;
                existing.notify.notify_one();
                return existing;
            }
        }

        self.spawn_task(runtime.clone());
        runtime.notify.notify_one();

        runtime
    }

    /// Spawn the playback task and a watcher which always cleans up after it
    ///
    /// The cleanup cannot live inside the task itself: on a panic (or an abort)
    /// the code after the `.await` never runs, which would leave the entry in
    /// the map forever and prevent any respawn. Watching the `JoinHandle` from a
    /// second task makes the cleanup unconditional.
    fn spawn_task(&self, runtime: Arc<DjRuntime>) {
        let task_engine = self.clone();
        let task_runtime = runtime.clone();
        let handle =
            tokio::spawn(async move { run_channel(task_engine, task_runtime).await });

        let engine = self.clone();
        let watch_runtime = runtime;

        tokio::spawn(async move {
            match handle.await {
                Ok(Ok(())) => log::debug!("DJ playback for {} stopped", watch_runtime.channel),
                Ok(Err(error)) => log::error!(
                    "DJ playback for {} stopped unexpectedly: {error}",
                    watch_runtime.channel
                ),
                Err(error) if error.is_cancelled() => {
                    log::debug!("DJ playback for {} was cancelled", watch_runtime.channel)
                }
                Err(error) => log::error!(
                    "DJ playback for {} panicked: {error}",
                    watch_runtime.channel
                ),
            }

            watch_runtime.finished.store(true, Ordering::SeqCst);

            engine
                .inner
                .channels
                .remove_if(&watch_runtime.channel, |_, current| {
                    Arc::ptr_eq(current, &watch_runtime)
                });
        });
    }

    /// Ask the playback task of a channel to stop and drop its entry
    ///
    /// Used when the channel is deleted or its DJ is disabled/unset: the task
    /// wakes up, sees `stopped` and returns without touching state again.
    pub fn stop(&self, channel: &str) {
        if let Some(runtime) = self.inner.channels.get(channel).map(|entry| entry.clone()) {
            runtime.stopped.store(true, Ordering::SeqCst);
            runtime.notify.notify_one();
        }
    }

    /// Mark a channel's runtime as dead without removing it from the map
    ///
    /// Simulates the state a panicking task leaves behind before the watcher
    /// has run, so the respawn path of [`DjEngine::start`] can be tested.
    #[cfg(test)]
    pub(crate) fn mark_finished(&self, channel: &str) {
        if let Some(runtime) = self.inner.channels.get(channel) {
            runtime.finished.store(true, Ordering::SeqCst);
        }
    }

    /// Wake the playback task of a channel, if it is running
    pub fn notify(&self, channel: &str) {
        if let Some(runtime) = self.inner.channels.get(channel) {
            runtime.notify.notify_one();
        }
    }

    /// Update the settings of a running channel
    pub async fn update_settings(&self, channel: &str, settings: v0::DjInformation) {
        // Clone the runtime out of the map before awaiting: holding a dashmap
        // guard across an await can deadlock against a concurrent insert
        let runtime = self
            .inner
            .channels
            .get(channel)
            .map(|runtime| runtime.clone());

        if let Some(runtime) = runtime {
            *runtime.settings.write().await = settings;
            runtime.notify.notify_one();
        }
    }

    /// Delete all DJ state on start up
    ///
    /// There is no crash recovery: a restart clears every queue and every file.
    pub async fn wipe_all(&self) {
        self.inner.channels.clear();

        if let Err(error) = dj::clear_all().await {
            log::error!("Failed to clear DJ keys on start up: {error}");
        }

        paths::wipe_dir().await;
    }

    /// Build the state snapshot of a channel
    pub async fn state(
        &self,
        channel: &str,
        settings: &v0::DjInformation,
    ) -> Result<v0::DjChannelState> {
        build_state(channel, settings).await
    }

    /// Spawn the periodic sweep which deletes orphaned files from `DJ_DIR`
    pub fn spawn_sweeper(&self) {
        let engine = self.clone();

        tokio::spawn(async move {
            loop {
                tokio::time::sleep(SWEEP_INTERVAL).await;
                engine.sweep_orphans().await;
            }
        });
    }

    /// Delete files under `DJ_DIR` which have no queue or now entry
    ///
    /// Failed uploads and interrupted playback can leave files behind; only
    /// files older than [`SWEEP_MIN_AGE`] are removed so an upload which is
    /// still being transcoded is never deleted underneath itself.
    ///
    /// The sweep **fails closed**: a channel whose queue or now entry cannot be
    /// read is skipped entirely, because an empty keep set would delete the
    /// audio which is playing right now.
    async fn sweep_orphans(&self) {
        let Ok(mut channels) = tokio::fs::read_dir(paths::dj_dir()).await else {
            return;
        };

        while let Ok(Some(entry)) = channels.next_entry().await {
            if !entry.path().is_dir() {
                continue;
            }

            let Some(channel) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };

            let queue = dj::get_queue(&channel).await;
            let current = dj::get_now(&channel).await;

            let Some(keep) = keep_set(queue, current) else {
                log::warn!("Skipping DJ sweep of {channel}, its state could not be read");
                continue;
            };

            let Ok(mut files) = tokio::fs::read_dir(entry.path()).await else {
                continue;
            };

            while let Ok(Some(file)) = files.next_entry().await {
                let name = file.file_name();
                let Some(name) = name.to_str() else { continue };

                if keep.iter().any(|kept| kept == name) {
                    continue;
                }

                let Ok(metadata) = file.metadata().await else {
                    continue;
                };

                let recent = metadata
                    .modified()
                    .ok()
                    .and_then(|modified| modified.elapsed().ok())
                    .is_some_and(|age| age < SWEEP_MIN_AGE);

                if recent {
                    continue;
                }

                if let Err(error) = tokio::fs::remove_file(file.path()).await {
                    log::warn!("Failed to sweep DJ file {}: {error}", file.path().display());
                }
            }
        }
    }

    /// Signed URL for a track
    ///
    /// Short lived on purpose; the engine re-signs on every heartbeat.
    pub fn stream_url(&self, channel: &str, track: &str, _settings: &v0::DjInformation) -> String {
        token::stream_url(&self.inner.api_url, channel, track)
    }
}

/// Files of a channel which a sweep must keep
///
/// Returns `None` when the queue or the now entry could not be read: the sweep
/// must then skip the channel entirely rather than treat a Redis error as "this
/// channel references nothing", which would delete the audio which is playing.
pub(crate) fn keep_set(
queue: Result<Vec<v0::DjTrack>>,
current: Result<Option<v0::DjNowPlaying>>,
) -> Option<Vec<String>> {
let queue = queue.ok()?;
let current = current.ok()?;

let mut keep = queue
    .into_iter()
    .map(|track| format!("{}.mp3", track.id))
    .collect::<Vec<_>>();

if let Some(current) = current {
    keep.push(format!("{}.mp3", current.track.id));
}

Some(keep)
}

/// Build the state snapshot of a channel
pub async fn build_state(
    channel: &str,
    settings: &v0::DjInformation,
) -> Result<v0::DjChannelState> {
    let server_time = dj::unix_ms();
    let members = dj::get_members(channel).await?;
    let current = dj::get_now(channel).await?;
    let queue = dj::get_queue(channel).await?;

    let (votes, voters) = match &current {
        Some(current) => {
            let voters = dj::get_voters(channel, &current.track.id).await?;
            let (votes, _, _) = dj::tally(&voters, &members, settings.skip_threshold);

            let voters = voters
                .into_iter()
                .filter(|voter| members.contains(voter))
                .collect::<Vec<_>>();

            (votes, voters)
        }
        None => (0, Vec::new()),
    };

    let position_ms = current
        .as_ref()
        .map(|current| {
            server_time
                .saturating_sub(current.started_at)
                .min(current.track.duration_ms)
        })
        .unwrap_or_default();

    Ok(v0::DjChannelState {
        id: channel.to_string(),
        current,
        position_ms,
        server_time,
        queue,
        votes,
        voters,
        members,
        settings: settings.clone(),
    })
}

/// Publish an event to every member which joined the channel
///
/// DJ events intentionally do not go to the channel topic: that reaches every
/// member with `ViewChannel` and would leak queue contents and signed URLs.
pub async fn publish_to_members(channel: &str, event: EventV1) {
    dj::events::publish_to_members(channel, event).await;
}

/// Spawn the 60 second membership reconciliation sweep
///
/// Membership is not heartbeat based: a socket which dies without closing
/// cleanly never runs the disconnect cleanup, which leaves a phantom member
/// behind forever (wrong member count, inflated skip denominator, grace period
/// which never fires). This sweep intersects every channel's members with the
/// live websocket sessions and drops the ones which are gone, announcing each
/// removal exactly like a clean disconnect would.
pub fn spawn_reconciler(engine: DjEngine, db: Database) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(RECONCILE_INTERVAL).await;

            match reconcile_members(&db, &engine).await {
                Ok(0) => {}
                Ok(dropped) => log::debug!("DJ reconciliation dropped {dropped} dead member(s)"),
                Err(error) => log::warn!("DJ reconciliation failed: {error}"),
            }
        }
    });
}

/// Drop members of DJ channels which have no live websocket session
///
/// Returns the amount of members which were removed.
pub async fn reconcile_members(db: &Database, engine: &DjEngine) -> Result<usize> {
    let mut dropped = 0;

    for channel_id in dj::member_channels().await? {
        let members = dj::get_members(&channel_id).await?;

        if members.is_empty() {
            continue;
        }

        let online = revolt_presence::filter_online(&members).await;
        let dead = dead_members(&members, &online);

        if dead.is_empty() {
            continue;
        }

        let channel = match db.fetch_channel(&channel_id).await {
            Ok(channel) if channel.dj().is_some() => Some(channel),
            Ok(_) => None,
            Err(error) => {
                // The channel is gone: drop the state without pretending to
                // have a channel to announce on.
                log::debug!("DJ channel {channel_id} no longer exists: {error}");
                None
            }
        };

        for member in dead {
            match &channel {
                Some(channel) => {
                    dj::events::remove_member_and_announce(channel, &member).await?;
                }
                None => {
                    dj::remove_member(&channel_id, &member).await?;
                }
            }

            dropped += 1;
        }

        // The engine re-tallies the vote and notices an empty channel
        engine.notify(&channel_id);
    }

    Ok(dropped)
}

/// Members which have no live websocket session
fn dead_members(members: &[String], online: &std::collections::HashSet<String>) -> Vec<String> {
    members
        .iter()
        .filter(|member| !online.contains(*member))
        .cloned()
        .collect()
}

/// Playback loop for a single channel
async fn run_channel(engine: DjEngine, runtime: Arc<DjRuntime>) -> Result<()> {
    let channel = runtime.channel.clone();

    // Track which the loop is currently playing (and its heartbeat schedule)
    let mut playing: Option<String> = None;
    let mut last_heartbeat = 0u64;

    loop {
        // Deleted channels and disabled DJs wake us up with `stop`
        if runtime.is_stopped() {
            log::debug!("DJ playback for {channel} was stopped");
            return Ok(());
        }

        let members = match dj::get_members(&channel).await {
            Ok(members) => members,
            Err(error) => {
                log::warn!("Failed to read DJ members of {channel}: {error}");
                sleep(5_000).await;
                continue;
            }
        };

        let server_time = dj::unix_ms();

        if members.is_empty() {
            match dj::get_empty_since(&channel).await.unwrap_or(None) {
                None => {
                    dj::set_empty_since(&channel, server_time).await.ok();
                }
                Some(since) if server_time.saturating_sub(since) >= EMPTY_GRACE_MS => {
                    if clear_channel_if_empty(&channel).await {
                        // Mark the runtime dead before returning: a join which
                        // lands between the clear and the watcher must not be
                        // handed this task, which is about to exit.
                        runtime.stopped.store(true, Ordering::SeqCst);
                        return Ok(());
                    }

                    // A join raced the clear: keep running for the new member
                    continue;
                }
                Some(_) => {}
            }

            wait(&runtime, IDLE_WAIT_MS).await;
            continue;
        }

        dj::clear_empty_since(&channel).await.ok();

        let settings = runtime.settings().await;

        let Some(current) = dj::get_now(&channel).await? else {
            // Nothing playing: pull the next track off the queue
            match dj::pop_track(&channel).await? {
                Some(track) => {
                    let now = v0::DjNowPlaying {
                        track: track.clone(),
                        started_at: dj::unix_ms(),
                    };

                    dj::set_now(&channel, &now).await?;

                    publish_to_members(
                        &channel,
                        EventV1::DjTrackStarted {
                            channel: channel.clone(),
                            track: track.clone(),
                            position_ms: 0,
                            server_time: now.started_at,
                            signed_url: engine.stream_url(&channel, &track.id, &settings),
                        },
                    )
                    .await;

                    playing = Some(track.id);
                    last_heartbeat = now.started_at;

                    continue;
                }
                None => {
                    wait(&runtime, IDLE_WAIT_MS).await;
                    continue;
                }
            }
        };

        if playing.as_deref() != Some(current.track.id.as_str()) {
            playing = Some(current.track.id.clone());
            last_heartbeat = 0;
        }

        let elapsed = server_time.saturating_sub(current.started_at);
        let end = current
            .started_at
            .saturating_add(current.track.duration_ms)
            .saturating_add(END_MARGIN_MS);

        // Re-evaluate the skip vote on every tick so votes cast by departed
        // members, joins and leaves are all accounted for.
        let voters = dj::get_voters(&channel, &current.track.id)
            .await
            .unwrap_or_default();
        let (votes, needed, should_skip) = dj::tally(&voters, &members, settings.skip_threshold);

        if should_skip {
            publish_to_members(
                &channel,
                EventV1::DjSkipped {
                    channel: channel.clone(),
                    track: current.track.clone(),
                },
            )
            .await;

            log::debug!(
                "Skipped DJ track {} in {channel} with {votes}/{needed} votes",
                current.track.id
            );

            finish_track(&channel, &current.track).await;
            playing = None;
            continue;
        }

        if server_time >= end {
            publish_to_members(
                &channel,
                EventV1::DjTrackEnded {
                    channel: channel.clone(),
                    track: current.track.clone(),
                },
            )
            .await;

            finish_track(&channel, &current.track).await;
            playing = None;
            continue;
        }

        if server_time.saturating_sub(last_heartbeat) >= HEARTBEAT_MS {
            last_heartbeat = server_time;

            publish_to_members(
                &channel,
                EventV1::DjNowPlaying {
                    channel: channel.clone(),
                    track: current.track.clone(),
                    position_ms: elapsed.min(current.track.duration_ms),
                    server_time,
                    signed_url: engine.stream_url(&channel, &current.track.id, &settings),
                },
            )
            .await;
        }

        // Wake up at the next heartbeat or when the track ends. There is no
        // polling floor: joins, leaves, votes and uploads all call `notify`, and
        // the reconciliation sweep wakes the task when it drops a member.
        let next = (last_heartbeat + HEARTBEAT_MS).min(end);
        wait(&runtime, next.saturating_sub(server_time).max(1)).await;
    }
}

/// Delete every file and all Redis state belonging to a channel
///
/// Used when a channel is emptied past its grace period or deleted outright.
pub async fn clear_channel_state(channel: &str) {
    if let Ok(Some(current)) = dj::get_now(channel).await {
        delete_file_later(channel, &current.track.id, 0);
    }

    paths::remove_channel_files(channel).await;

    if let Err(error) = dj::clear_channel(channel).await {
        log::warn!("Failed to clear DJ state of {channel}: {error}");
    }
}

/// Clear a channel which has been empty past the grace period
///
/// The emptiness check and the deletion are one atomic Redis operation, so a
/// member who joins at the wrong moment either keeps the channel alive or joins
/// a channel which is already clean; they can never end up in a channel whose
/// queue was cleared underneath them.
async fn clear_channel_if_empty(channel: &str) -> bool {
    match dj::clear_channel_if_empty(channel).await {
        Ok(true) => {
            paths::remove_channel_files(channel).await;

            // Nobody is left to notify privately, so announce it on the channel
            // topic (this event carries no track data or URLs).
            EventV1::DjQueueCleared {
                channel: channel.to_string(),
            }
            .p(channel.to_string())
            .await;

            log::debug!("Cleared DJ state of {channel}, nobody joined within the grace period");
            true
        }
        Ok(false) => false,
        Err(error) => {
            // Never delete files when the state could not be read: a Redis
            // error must not turn into "everything is unreferenced".
            log::warn!("Failed to clear empty DJ channel {channel}: {error}");
            false
        }
    }
}

/// Stop playing the given track, clearing its votes and deleting its file later
async fn finish_track(channel: &str, track: &v0::DjTrack) {
    if let Err(error) = dj::clear_now(channel).await {
        log::warn!("Failed to clear DJ now of {channel}: {error}");
    }

    if let Err(error) = dj::clear_votes(channel, &track.id).await {
        log::warn!("Failed to clear DJ votes of {channel}: {error}");
    }

    delete_file_later(channel, &track.id, FILE_DELETE_DELAY_SECS);
}

/// Delete a track's file after the given delay
///
/// Clients buffer ahead, so deleting immediately on advance would truncate
/// streams for listeners which are a few seconds behind.
fn delete_file_later(channel: &str, track: &str, delay_secs: u64) {
    let path = paths::track_path(channel, track);

    tokio::spawn(async move {
        if delay_secs > 0 {
            tokio::time::sleep(Duration::from_secs(delay_secs)).await;
        }

        match tokio::fs::remove_file(&path).await {
            Ok(()) => log::debug!("Deleted DJ file {}", path.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => log::warn!("Failed to delete DJ file {}: {error}", path.display()),
        }
    });
}

/// Sleep for the given amount of milliseconds, waking early on a notification
async fn wait(runtime: &DjRuntime, ms: u64) {
    tokio::select! {
        _ = runtime.notify.notified() => {}
        _ = sleep(ms) => {}
    }
}

/// Sleep for the given amount of milliseconds
async fn sleep(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

/// Reject channels which do not accept DJ joins
pub fn resolve_dj_channel(channel: &revolt_database::Channel) -> Result<v0::DjInformation> {
    let Some(settings) = channel.dj() else {
        return Err(create_error!(NotADjChannel));
    };

    if !settings.enabled {
        return Err(create_error!(DjDisabled));
    }

    Ok(settings.into_owned().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use revolt_database::dj::skip_needed;

    #[test]
    fn engine_state_keys_are_stable() {
        // Guards against accidental renames which would orphan Redis state
        assert_eq!(revolt_database::dj::members_key("c"), "dj_members:c");
        assert_eq!(revolt_database::dj::queue_key("c"), "dj_queue:c");
        assert_eq!(revolt_database::dj::now_key("c"), "dj_now:c");
        assert_eq!(revolt_database::dj::votes_key("c", "t"), "dj_votes:c:t");
        assert_eq!(revolt_database::dj::seq_key("c"), "dj_seq:c");
        assert_eq!(
            revolt_database::dj::empty_since_key("c"),
            "dj_empty_since:c"
        );
        assert_eq!(revolt_database::dj::user_channels_key("u"), "dj_user:u");
    }

    #[test]
    fn skip_needed_is_shared_with_the_engine() {
        assert_eq!(skip_needed(4, 0.5), 2);
    }

    fn queued(id: &str) -> v0::DjTrack {
        v0::DjTrack {
            id: id.to_string(),
            file: "/dj/c/stream".to_string(),
            title: "t".to_string(),
            duration_ms: 1000,
            uploader: "user-a".to_string(),
        }
    }

    #[test]
    fn sweep_keeps_queued_and_playing_tracks() {
        let keep = keep_set(
            Ok(vec![queued("a"), queued("b")]),
            Ok(Some(v0::DjNowPlaying {
                track: queued("now"),
                started_at: 0,
            })),
        )
        .expect("state readable");

        assert!(keep.contains(&"a.mp3".to_string()));
        assert!(keep.contains(&"b.mp3".to_string()));
        assert!(keep.contains(&"now.mp3".to_string()));
        assert!(!keep.contains(&"orphan.mp3".to_string()));
    }

    #[test]
    fn sweep_fails_closed_when_the_queue_cannot_be_read() {
        // A Redis error must never be treated as "nothing is referenced"
        assert!(keep_set(
            Err(revolt_result::create_error!(InternalError)),
            Ok(None)
        )
        .is_none());
    }

    #[test]
    fn sweep_fails_closed_when_the_now_entry_cannot_be_read() {
        assert!(keep_set(
            Ok(vec![queued("a")]),
            Err(revolt_result::create_error!(InternalError))
        )
        .is_none());
    }

    #[test]
    fn sweep_reads_an_empty_channel_as_an_empty_keep_set() {
        // No error, genuinely nothing referenced: everything old is an orphan
        let keep = keep_set(Ok(vec![]), Ok(None)).expect("state readable");
        assert!(keep.is_empty());
    }

    #[test]
    fn reconciliation_drops_only_members_without_a_session() {
        let members = vec![
            "online-a".to_string(),
            "dead".to_string(),
            "online-b".to_string(),
        ];
        let online = ["online-a", "online-b"]
            .iter()
            .map(|id| id.to_string())
            .collect::<std::collections::HashSet<_>>();

        assert_eq!(dead_members(&members, &online), vec!["dead"]);

        // Everyone online: nothing to do
        assert!(dead_members(&members[..1], &online).is_empty());

        // Nobody online: everybody goes, including the last member (which is
        // what arms the empty-channel grace period)
        assert_eq!(
            dead_members(&members, &std::collections::HashSet::new()).len(),
            3
        );
    }
}
