//! Integration tests for the DJ playback engine and its on-disk state.
//!
//! Unlike the unit tests next to the code, these drive a real Redis and a real
//! `DJ_DIR`, because the interesting behaviour is in the interaction: the queue
//! is popped, `dj_now` is written, files are reaped and the empty-channel grace
//! period clears both Redis and the disk.
//!
//! Redis tests are skipped unless `REDIS_URI` is set explicitly:
//!
//! ```text
//! docker run -d --rm -p 6399:6379 redis:7-alpine
//! REDIS_URI=redis://127.0.0.1:6399/0 \
//!     cargo test -p revolt-delta --bin revolt-delta -- dj::engine_tests -- --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use revolt_database::dj;
use revolt_models::v0;
use revolt_result::ErrorType;

use crate::dj::{clear_channel_state, paths, resolve_dj_channel, DjEngine};

use crate::dj::ENV_LOCK;

fn unique_channel() -> String {
    format!("test-{}", ulid::Ulid::new())
}

/// Point `DJ_DIR` at a fresh directory and return it
fn use_temp_dj_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dj-engine-{}", ulid::Ulid::new()));
    std::env::set_var(paths::DJ_DIR_ENV, &dir);
    dir
}

fn settings() -> v0::DjInformation {
    v0::DjInformation::default()
}

fn track(id: &str, duration_ms: u64) -> v0::DjTrack {
    v0::DjTrack {
        id: id.to_string(),
        file: format!("/dj/c/{id}.mp3"),
        title: format!("track {id}"),
        duration_ms,
        uploader: "user-a".to_string(),
    }
}

/// Write a placeholder audio file; the engine never decodes the bytes
fn write_track_file(channel: &str, id: &str) {
    paths::create_channel_dir(channel).expect("channel dir");
    std::fs::write(paths::track_path(channel, id), b"ID3 fake mp3 payload").expect("write track");
}

fn backdate(path: &Path, spec: &str) {
    let status = std::process::Command::new("touch")
        .arg("-d")
        .arg(spec)
        .arg(path)
        .status()
        .expect("touch should be available");

    assert!(status.success(), "failed to backdate {}", path.display());
}

async fn redis() -> bool {
    match std::env::var("REDIS_URI") {
        Ok(uri) if !uri.trim().is_empty() => {}
        _ => {
            eprintln!("skipping DJ engine test: REDIS_URI is not set");
            return false;
        }
    }

    match revolt_database::dj::get_members("ping").await {
        Ok(_) => true,
        Err(error) => {
            eprintln!("skipping DJ engine test: cannot reach Redis ({error:?})");
            false
        }
    }
}

/// Poll until `predicate` holds or the deadline passes
async fn wait_for<F, Fut>(timeout: Duration, mut predicate: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline {
        if predicate().await {
            return true;
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    false
}

#[test]
fn resolve_dj_channel_rejects_channels_without_dj_state() {
    use revolt_database::Channel;

    let channel = Channel::SavedMessages {
        id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
        user: "01ARZ3NDEKTSV4RRFFQ69G5FAW".to_string(),
    };

    let error = resolve_dj_channel(&channel).expect_err("not a DJ channel");

    assert!(matches!(error.error_type, ErrorType::NotADjChannel));
}

#[test]
fn resolve_dj_channel_rejects_disabled_dj_channels() {
    use revolt_database::{Channel, DjInformation};

    let channel = Channel::TextChannel {
        id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
        server: "01ARZ3NDEKTSV4RRFFQ69G5FAW".to_string(),
        name: "music".to_string(),
        description: None,
        icon: None,
        last_message_id: None,
        default_permissions: None,
        role_permissions: Default::default(),
        nsfw: false,
        voice: None,
        dj: Some(DjInformation {
            enabled: false,
            ..Default::default()
        }),
        slowmode: None,
        default_auto_archive_minutes: None,
    };

    let error = resolve_dj_channel(&channel).expect_err("disabled DJ channel");

    assert!(matches!(error.error_type, ErrorType::DjDisabled));
}

#[test]
fn resolve_dj_channel_returns_the_stored_settings() {
    use revolt_database::{Channel, DjInformation};

    let channel = Channel::TextChannel {
        id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
        server: "01ARZ3NDEKTSV4RRFFQ69G5FAW".to_string(),
        name: "music".to_string(),
        description: None,
        icon: None,
        last_message_id: None,
        default_permissions: None,
        role_permissions: Default::default(),
        nsfw: false,
        voice: None,
        dj: Some(DjInformation {
            skip_threshold: 0.25,
            max_tracks: 4,
            enabled: true,
            ..Default::default()
        }),
        slowmode: None,
        default_auto_archive_minutes: None,
    };

    let settings = resolve_dj_channel(&channel).expect("enabled DJ channel");

    assert_eq!(settings.skip_threshold, 0.25);
    assert_eq!(settings.max_tracks, 4);
    assert!(settings.enabled);
}

#[test]
fn safe_ids_are_bounded_and_restricted_to_a_single_path_component() {
    assert!(paths::is_safe_id("a"));
    assert!(paths::is_safe_id("01J8Z-9abcDEF_1"));
    assert!(paths::is_safe_id(&"a".repeat(128)));

    // One over the length limit
    assert!(!paths::is_safe_id(&"a".repeat(129)));

    for id in [
        "", ".", "..", "../x", "a/b", "a\\b", "a.b", "a b", "a\nb", "a\0b", "%2e%2e", "ümlaut",
    ] {
        assert!(!paths::is_safe_id(id), "{:?} should be rejected", id);
    }
}

#[tokio::test]
async fn hostile_channel_ids_cannot_escape_the_dj_directory() {
    let _guard = ENV_LOCK.lock().await;
    let dir = use_temp_dj_dir();

    for channel in ["../..", "..", "a/../../etc", ""] {
        let path = paths::track_path(channel, "../escape");

        assert!(
            path.starts_with(&dir),
            "{} escaped {}",
            path.display(),
            dir.display()
        );
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("mp3"));
        assert!(
            !path.to_string_lossy().contains(".."),
            "{} kept a traversal component",
            path.display()
        );
    }

    // `remove_channel_files` only ever deletes a direct child of `DJ_DIR`
    let sibling = dir.parent().expect("parent").join(format!(
        "dj-sibling-{}",
        ulid::Ulid::new().to_string().to_lowercase()
    ));
    std::fs::create_dir_all(&sibling).expect("sibling dir");
    std::fs::write(sibling.join("keep.txt"), b"keep").expect("write");

    paths::remove_channel_files("..");
    assert!(
        sibling.exists(),
        "a hostile channel id must not delete outside DJ_DIR"
    );

    let _ = std::fs::remove_dir_all(&sibling);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn wipe_dir_clears_everything_and_recreates_the_root() {
    let _guard = ENV_LOCK.lock().await;
    let dir = use_temp_dj_dir();

    std::fs::create_dir_all(dir.join("chan-a")).expect("chan-a");
    std::fs::create_dir_all(dir.join("chan-b")).expect("chan-b");
    std::fs::write(dir.join("chan-a").join("t1.mp3"), b"x").expect("file");

    paths::wipe_dir();

    assert!(dir.exists(), "the root directory must be recreated");
    assert_eq!(
        std::fs::read_dir(&dir).expect("read dir").count(),
        0,
        "wipe_dir must leave no files behind"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn engine_starts_a_queued_track_and_clears_it_when_it_ends() {
    let _guard = ENV_LOCK.lock().await;
    if !redis().await {
        return;
    }

    let dir = use_temp_dj_dir();
    let channel = unique_channel();
    let engine = DjEngine::new("https://api.example.com/".to_string());

    dj::add_member(&channel, "user-a").await.expect("join");
    write_track_file(&channel, "t1");

    // Short track so the test does not have to wait for a real song
    dj::push_track(&channel, &track("t1", 1_200))
        .await
        .expect("queue");

    // A vote cast while the track plays must be cleared when it ends; a
    // unanimous threshold means the single vote does not skip it early
    let settings = v0::DjInformation {
        skip_threshold: 1.0,
        ..Default::default()
    };

    dj::add_member(&channel, "user-b")
        .await
        .expect("second join");
    dj::add_vote(&channel, "t1", "user-a").await.expect("vote");

    engine.start(&channel, settings.clone()).await;

    assert!(
        wait_for(Duration::from_secs(5), || async {
            dj::get_now(&channel)
                .await
                .map(|now| now.is_some())
                .unwrap_or(false)
        })
        .await,
        "the queued track should start playing"
    );

    let now = dj::get_now(&channel).await.unwrap().expect("now playing");
    assert_eq!(now.track.id, "t1");
    assert_eq!(now.track.duration_ms, 1_200);
    assert!(now.started_at > 0);
    assert_eq!(
        dj::queue_len(&channel).await.unwrap(),
        0,
        "the started track is popped off the queue"
    );
    assert!(paths::track_path(&channel, "t1").exists());

    // remaining = 1.2s track + 5s end margin, plus the poll interval
    assert!(
        wait_for(Duration::from_secs(15), || async {
            dj::get_now(&channel)
                .await
                .map(|now| now.is_none())
                .unwrap_or(false)
        })
        .await,
        "the track should end and clear dj_now"
    );

    assert!(
        dj::get_voters(&channel, "t1").await.unwrap().is_empty(),
        "votes are cleared with the track"
    );

    // The file is kept for the 60 s client buffer, it is not deleted on end
    assert!(
        paths::track_path(&channel, "t1").exists(),
        "finished tracks are only reaped after the 60 s buffer"
    );

    // The task keeps running so the next upload can be picked up
    assert!(engine.is_running(&channel));

    clear_channel_state(&channel).await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn empty_channel_is_cleared_after_the_grace_period() {
    let _guard = ENV_LOCK.lock().await;
    if !redis().await {
        return;
    }

    let dir = use_temp_dj_dir();
    let channel = unique_channel();
    let engine = DjEngine::new("https://api.example.com".to_string());

    write_track_file(&channel, "t1");
    dj::push_track(&channel, &track("t1", 60_000))
        .await
        .expect("queue");
    dj::next_seq(&channel).await.expect("seq");

    // Nobody joined: the engine arms the grace period
    engine.start(&channel, settings()).await;

    assert!(
        wait_for(Duration::from_secs(5), || async {
            dj::get_empty_since(&channel)
                .await
                .map(|since| since.is_some())
                .unwrap_or(false)
        })
        .await,
        "an empty channel should be marked as empty"
    );

    // Pretend the grace period already elapsed
    dj::set_empty_since(&channel, dj::unix_ms() - 31_000)
        .await
        .expect("backdate");
    engine.notify(&channel);

    assert!(
        wait_for(Duration::from_secs(10), || async {
            !engine.is_running(&channel)
        })
        .await,
        "the playback task should stop once the grace period elapsed"
    );

    assert!(dj::get_members(&channel).await.unwrap().is_empty());
    assert_eq!(dj::queue_len(&channel).await.unwrap(), 0);
    assert!(dj::get_now(&channel).await.unwrap().is_none());
    assert_eq!(dj::get_empty_since(&channel).await.unwrap(), None);
    assert!(
        !paths::channel_dir(&channel).exists(),
        "the channel's files are deleted with its state"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn rejoining_inside_the_grace_period_keeps_the_queue() {
    let _guard = ENV_LOCK.lock().await;
    if !redis().await {
        return;
    }

    let dir = use_temp_dj_dir();
    let channel = unique_channel();
    let engine = DjEngine::new("https://api.example.com".to_string());

    write_track_file(&channel, "t1");
    dj::push_track(&channel, &track("t1", 60_000))
        .await
        .expect("queue");

    // Start with a member so the empty grace is never armed, then leave
    dj::add_member(&channel, "user-a").await.expect("join");
    engine.start(&channel, settings()).await;
    dj::remove_member(&channel, "user-a").await.expect("leave");
    dj::set_empty_since(&channel, dj::unix_ms())
        .await
        .expect("arm");
    engine.notify(&channel);

    // Rejoin well inside the grace period
    tokio::time::sleep(Duration::from_millis(300)).await;
    dj::add_member(&channel, "user-b").await.expect("rejoin");
    engine.notify(&channel);

    tokio::time::sleep(Duration::from_secs(2)).await;

    assert_eq!(
        dj::get_empty_since(&channel).await.unwrap(),
        None,
        "rejoining cancels the grace period"
    );
    assert_eq!(
        dj::queue_len(&channel).await.unwrap()
            + dj::get_now(&channel).await.unwrap().is_some() as u32,
        1,
        "the queue survives a rejoin"
    );
    assert!(engine.is_running(&channel));

    clear_channel_state(&channel).await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn clear_channel_state_deletes_files_immediately() {
    let _guard = ENV_LOCK.lock().await;
    if !redis().await {
        return;
    }

    let dir = use_temp_dj_dir();
    let channel = unique_channel();

    write_track_file(&channel, "t1");
    dj::add_member(&channel, "user-a").await.expect("join");
    dj::set_now(
        &channel,
        &v0::DjNowPlaying {
            track: track("t1", 60_000),
            started_at: dj::unix_ms(),
        },
    )
    .await
    .expect("now");

    clear_channel_state(&channel).await;

    assert!(
        !paths::track_path(&channel, "t1").exists(),
        "channel deletion removes its files without the 60 s delay"
    );
    assert!(!paths::channel_dir(&channel).exists());
    assert!(dj::get_members(&channel).await.unwrap().is_empty());
    assert!(dj::get_now(&channel).await.unwrap().is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn orphan_sweep_removes_only_old_unreferenced_files() {
    let _guard = ENV_LOCK.lock().await;
    if !redis().await {
        return;
    }

    let dir = use_temp_dj_dir();
    let channel = unique_channel();
    let engine = DjEngine::new("https://api.example.com".to_string());

    // Referenced by the queue
    write_track_file(&channel, "queued");
    dj::push_track(&channel, &track("queued", 60_000))
        .await
        .expect("queue");

    // Unreferenced, old enough to reap
    write_track_file(&channel, "orphan-old");
    backdate(&paths::track_path(&channel, "orphan-old"), "10 minutes ago");

    // Unreferenced but fresh: an upload may still be in flight
    write_track_file(&channel, "orphan-fresh");

    // The referenced file is old too, and must still survive
    backdate(&paths::track_path(&channel, "queued"), "10 minutes ago");

    engine.sweep_orphans().await;

    assert!(
        !paths::track_path(&channel, "orphan-old").exists(),
        "old orphan should be swept"
    );
    assert!(
        paths::track_path(&channel, "orphan-fresh").exists(),
        "recent files must never be swept"
    );
    assert!(
        paths::track_path(&channel, "queued").exists(),
        "queued tracks must never be swept"
    );

    clear_channel_state(&channel).await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn startup_wipe_clears_redis_state_and_files() {
    let _guard = ENV_LOCK.lock().await;
    if !redis().await {
        return;
    }

    let dir = use_temp_dj_dir();
    let channel = unique_channel();
    let engine = DjEngine::new("https://api.example.com".to_string());

    dj::add_member(&channel, "user-a").await.expect("join");
    write_track_file(&channel, "t1");
    dj::push_track(&channel, &track("t1", 60_000))
        .await
        .expect("queue");
    dj::set_now(
        &channel,
        &v0::DjNowPlaying {
            track: track("t1", 60_000),
            started_at: dj::unix_ms(),
        },
    )
    .await
    .expect("now");

    engine.wipe_all().await;

    assert!(dj::get_members(&channel).await.unwrap().is_empty());
    assert_eq!(dj::queue_len(&channel).await.unwrap(), 0);
    assert!(dj::get_now(&channel).await.unwrap().is_none());
    assert!(!engine.is_running(&channel));

    assert!(dir.exists(), "DJ_DIR is recreated on start up");
    assert_eq!(
        std::fs::read_dir(&dir).expect("read dir").count(),
        0,
        "every DJ file is wiped on start up"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
