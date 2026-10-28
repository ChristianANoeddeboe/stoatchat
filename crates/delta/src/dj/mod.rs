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

use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use revolt_database::{dj, events::client::EventV1};
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

/// How often the engine re-reads state while idle
const IDLE_POLL_MS: u64 = 1_000;

/// How often orphaned files are swept from `DJ_DIR`
const SWEEP_INTERVAL: Duration = Duration::from_secs(600);

/// Files younger than this are never swept, an upload may still be in flight
const SWEEP_MIN_AGE: Duration = Duration::from_secs(300);

/// Per-channel runtime state held by the engine
pub struct DjRuntime {
    /// Id of the channel this runtime belongs to
    pub channel: String,
    settings: RwLock<v0::DjInformation>,
    notify: Notify,
}

impl DjRuntime {
    /// Current settings of the channel
    pub async fn settings(&self) -> v0::DjInformation {
        self.settings.read().await.clone()
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
        self.inner.channels.contains_key(channel)
    }

    /// Start (or update) the playback task for a channel
    ///
    /// The task is started on the first join and keeps running until the
    /// channel has been empty past the grace period.
    pub async fn start(&self, channel: &str, settings: v0::DjInformation) -> Arc<DjRuntime> {
        let (runtime, is_new) = {
            match self.inner.channels.entry(channel.to_string()) {
                dashmap::mapref::entry::Entry::Occupied(entry) => (entry.get().clone(), false),
                dashmap::mapref::entry::Entry::Vacant(entry) => {
                    let runtime = Arc::new(DjRuntime {
                        channel: channel.to_string(),
                        settings: RwLock::new(settings.clone()),
                        notify: Notify::new(),
                    });

                    entry.insert(runtime.clone());

                    (runtime, true)
                }
            }
        };

        if !is_new {
            *runtime.settings.write().await = settings;
        }

        if is_new {
            let engine = self.clone();
            let task_runtime = runtime.clone();

            tokio::spawn(async move {
                if let Err(error) = run_channel(engine.clone(), task_runtime.clone()).await {
                    log::error!(
                        "DJ playback for {} stopped unexpectedly: {error}",
                        task_runtime.channel
                    );
                }

                engine.inner.channels.remove(&task_runtime.channel);
            });
        }

        runtime.notify.notify_one();

        runtime
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

        paths::wipe_dir();
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

            let mut keep = dj::get_queue(&channel)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|track| format!("{}.mp3", track.id))
                .collect::<Vec<_>>();

            if let Ok(Some(current)) = dj::get_now(&channel).await {
                keep.push(format!("{}.mp3", current.track.id));
            }

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

    /// Signed URL for a track, valid for this channel's track duration limit
    pub fn stream_url(&self, channel: &str, track: &str, settings: &v0::DjInformation) -> String {
        token::stream_url(
            &self.inner.api_url,
            channel,
            track,
            settings.max_track_seconds,
        )
    }
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
    let members = dj::get_members(channel).await.unwrap_or_default();

    for member in members {
        event.clone().private(member).await;
    }
}

/// Playback loop for a single channel
async fn run_channel(engine: DjEngine, runtime: Arc<DjRuntime>) -> Result<()> {
    let channel = runtime.channel.clone();

    // Track which the loop is currently playing (and its heartbeat schedule)
    let mut playing: Option<String> = None;
    let mut last_heartbeat = 0u64;

    loop {
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
                    clear_channel(&channel).await;
                    return Ok(());
                }
                Some(_) => {}
            }

            wait(&runtime, IDLE_POLL_MS).await;
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
                    wait(&runtime, IDLE_POLL_MS).await;
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

        // Wake up at the next heartbeat or when the track ends, but re-check
        // votes and membership at least once a second.
        let next = (last_heartbeat + HEARTBEAT_MS).min(end);
        wait(
            &runtime,
            next.saturating_sub(server_time).clamp(50, IDLE_POLL_MS),
        )
        .await;
    }
}

/// Delete every file and all Redis state belonging to a channel
///
/// Used when a channel is emptied past its grace period or deleted outright.
pub async fn clear_channel_state(channel: &str) {
    if let Ok(Some(current)) = dj::get_now(channel).await {
        delete_file_later(channel, &current.track.id, 0);
    }

    paths::remove_channel_files(channel);

    if let Err(error) = dj::clear_channel(channel).await {
        log::warn!("Failed to clear DJ state of {channel}: {error}");
    }
}

/// Clear the queue and files of a channel which has been empty past the grace
async fn clear_channel(channel: &str) {
    clear_channel_state(channel).await;

    // Nobody is left to notify privately, so announce it on the channel topic
    // (this event carries no track data or URLs).
    EventV1::DjQueueCleared {
        channel: channel.to_string(),
    }
    .p(channel.to_string())
    .await;

    log::debug!("Cleared DJ state of {channel}, nobody joined within the grace period");
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
}
