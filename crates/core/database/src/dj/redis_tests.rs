//! Redis-backed tests for the DJ state helpers.
//!
//! These exercise the real Redis commands (`SADD`, `RPUSH`/`LPOP`, `HSET`,
//! `INCR`, `KEYS`/`DEL`) rather than a mock, because the interesting failures
//! live in the command semantics: set idempotency, FIFO ordering, key naming and
//! the wipe-everything path.
//!
//! They are skipped unless `REDIS_URI` is set explicitly, so a developer's
//! default `localhost` Redis is never touched by accident. Point it at a
//! disposable instance:
//!
//! ```text
//! docker run -d --rm -p 6399:6379 redis:7-alpine
//! REDIS_URI=redis://127.0.0.1:6399/0 cargo test -p revolt-database --lib dj::redis_tests
//! ```
//!
//! Every test uses a unique channel id, so they are safe to run in parallel;
//! `clear_all` is serialised against the rest through [`SERIAL`].

use redis_kiss::redis::AsyncCommands;
use revolt_models::v0::{DjNowPlaying, DjTrack};

use super::*;

/// Serialises tests which enumerate or delete every `dj_*` key
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn unique_channel() -> String {
    format!("test-{}", ulid::Ulid::new())
}

fn track(id: &str, uploader: &str) -> DjTrack {
    DjTrack {
        id: id.to_string(),
        file: format!("/dj/c/{id}.mp3"),
        title: format!("title {id}"),
        duration_ms: 183_000,
        uploader: uploader.to_string(),
    }
}

/// Whether a Redis test instance is configured and reachable
async fn redis() -> bool {
    match std::env::var("REDIS_URI") {
        Ok(uri) if !uri.trim().is_empty() => {}
        _ => {
            eprintln!("skipping DJ Redis test: REDIS_URI is not set");
            return false;
        }
    }

    match get_connection().await {
        Ok(_) => true,
        Err(error) => {
            eprintln!("skipping DJ Redis test: cannot reach Redis ({error:?})");
            false
        }
    }
}

/// Assert that none of the given keys exist
async fn assert_absent(keys: &[String]) {
    let mut conn = get_connection().await.expect("redis connection");

    for key in keys {
        let exists: u32 = conn.exists(key).await.expect("exists");
        assert_eq!(exists, 0, "key {key} should have been deleted");
    }
}

#[tokio::test]
async fn membership_lifecycle() {
    let _guard = SERIAL.lock().await;
    if !redis().await {
        return;
    }

    let channel = unique_channel();
    let other = unique_channel();
    let alice = "user-alice";
    let bob = "user-bob";

    assert_eq!(member_count(&channel).await.unwrap(), 0);
    assert!(!is_member(&channel, alice).await.unwrap());

    // First join
    assert_eq!(add_member(&channel, alice).await.unwrap(), 1);
    assert!(is_member(&channel, alice).await.unwrap());
    assert_eq!(
        get_members(&channel).await.unwrap(),
        vec![alice.to_string()]
    );
    assert_eq!(
        get_user_channels(alice).await.unwrap(),
        vec![channel.clone()]
    );

    // Re-joining is idempotent, it must not double count
    assert_eq!(add_member(&channel, alice).await.unwrap(), 1);

    assert_eq!(add_member(&channel, bob).await.unwrap(), 2);
    assert_eq!(get_members(&channel).await.unwrap().len(), 2);

    // A user can be in several channels at once
    add_member(&other, alice).await.unwrap();
    let mut channels = get_user_channels(alice).await.unwrap();
    channels.sort();
    let mut expected = vec![channel.clone(), other.clone()];
    expected.sort();
    assert_eq!(channels, expected);

    // Leaving is idempotent too
    assert_eq!(remove_member(&channel, alice).await.unwrap(), 1);
    assert_eq!(remove_member(&channel, alice).await.unwrap(), 1);
    assert!(!is_member(&channel, alice).await.unwrap());
    assert!(is_member(&other, alice).await.unwrap());

    // Socket close removes the user from every DJ channel
    let mut removed = remove_user_from_all_channels(alice).await.unwrap();
    removed.sort();
    assert_eq!(removed, vec![other.clone()]);
    assert!(get_user_channels(alice).await.unwrap().is_empty());
    assert!(!is_member(&other, alice).await.unwrap());
    // The membership set of the other channels this user was not in is intact
    assert_eq!(
        member_count(&channel).await.unwrap(),
        1,
        "bob is still here"
    );

    clear_channel(&channel).await.unwrap();
    clear_channel(&other).await.unwrap();
}

#[tokio::test]
async fn joining_cancels_the_empty_grace_period() {
    let _guard = SERIAL.lock().await;
    if !redis().await {
        return;
    }

    let channel = unique_channel();

    set_empty_since(&channel, unix_ms()).await.unwrap();
    assert!(get_empty_since(&channel).await.unwrap().is_some());

    add_member(&channel, "user-a").await.unwrap();
    assert_eq!(
        get_empty_since(&channel).await.unwrap(),
        None,
        "a join inside the grace period must resume the channel"
    );

    // Leaving again re-arms it
    remove_member(&channel, "user-a").await.unwrap();
    set_empty_since(&channel, 1234).await.unwrap();
    assert_eq!(get_empty_since(&channel).await.unwrap(), Some(1234));

    clear_empty_since(&channel).await.unwrap();
    assert_eq!(get_empty_since(&channel).await.unwrap(), None);

    clear_channel(&channel).await.unwrap();
}

#[tokio::test]
async fn queue_is_fifo_and_round_trips_fields() {
    let _guard = SERIAL.lock().await;
    if !redis().await {
        return;
    }

    let channel = unique_channel();

    assert_eq!(queue_len(&channel).await.unwrap(), 0);
    assert!(pop_track(&channel).await.unwrap().is_none());

    for (index, id) in ["t1", "t2", "t3"].iter().enumerate() {
        let depth = push_track(&channel, &track(id, "uploader")).await.unwrap();
        assert_eq!(depth, index as u32 + 1, "push returns the new depth");
    }

    let queue = get_queue(&channel).await.unwrap();
    assert_eq!(
        queue.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
        vec!["t1", "t2", "t3"],
        "queue is read in playback order"
    );

    // Fields survive the JSON round trip
    let first = &queue[0];
    assert_eq!(first.title, "title t1");
    assert_eq!(first.duration_ms, 183_000);
    assert_eq!(first.uploader, "uploader");
    assert_eq!(first.file, "/dj/c/t1.mp3");

    // FIFO pop
    assert_eq!(pop_track(&channel).await.unwrap().unwrap().id, "t1");
    assert_eq!(queue_len(&channel).await.unwrap(), 2);
    assert_eq!(pop_track(&channel).await.unwrap().unwrap().id, "t2");
    assert_eq!(pop_track(&channel).await.unwrap().unwrap().id, "t3");
    assert!(pop_track(&channel).await.unwrap().is_none());
    assert_eq!(queue_len(&channel).await.unwrap(), 0);

    clear_channel(&channel).await.unwrap();
}

#[tokio::test]
async fn queue_entries_which_are_not_json_are_skipped() {
    let _guard = SERIAL.lock().await;
    if !redis().await {
        return;
    }

    let channel = unique_channel();
    let mut conn = get_connection().await.unwrap();

    let _: u32 = conn.rpush(queue_key(&channel), "not-json").await.unwrap();
    push_track(&channel, &track("t1", "uploader"))
        .await
        .unwrap();

    // A corrupt entry must not poison the whole listing
    let queue = get_queue(&channel).await.unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].id, "t1");

    // ... and popping a corrupt head yields None instead of an error
    let mut conn = get_connection().await.unwrap();
    let _: u32 = conn.del(queue_key(&channel)).await.unwrap();
    let _: u32 = conn.rpush(queue_key(&channel), "{}").await.unwrap();

    assert!(pop_track(&channel).await.unwrap().is_none());

    clear_channel(&channel).await.unwrap();
}

#[tokio::test]
async fn now_playing_set_get_clear() {
    let _guard = SERIAL.lock().await;
    if !redis().await {
        return;
    }

    let channel = unique_channel();

    assert!(get_now(&channel).await.unwrap().is_none());

    let now = DjNowPlaying {
        track: track("t1", "uploader"),
        started_at: 1_700_000_000_000,
    };

    set_now(&channel, &now).await.unwrap();
    let stored = get_now(&channel).await.unwrap().expect("now playing");
    assert_eq!(stored.started_at, 1_700_000_000_000);
    assert_eq!(stored.track.id, "t1");
    assert_eq!(stored.track.title, "title t1");

    // Overwriting replaces the previous track
    let next = DjNowPlaying {
        track: track("t2", "uploader"),
        started_at: 1_700_000_100_000,
    };
    set_now(&channel, &next).await.unwrap();
    assert_eq!(get_now(&channel).await.unwrap().unwrap().track.id, "t2");

    clear_now(&channel).await.unwrap();
    assert!(get_now(&channel).await.unwrap().is_none());

    // A half written hash is treated as "nothing playing", not as a panic
    let mut conn = get_connection().await.unwrap();
    let _: u32 = conn
        .hset(now_key(&channel), "track", "{\"id\":\"t3\"}")
        .await
        .unwrap();
    assert!(get_now(&channel).await.unwrap().is_none());

    clear_channel(&channel).await.unwrap();
}

#[tokio::test]
async fn votes_are_idempotent_and_pruned() {
    let _guard = SERIAL.lock().await;
    if !redis().await {
        return;
    }

    let channel = unique_channel();
    let track_id = "t1";

    assert_eq!(
        add_vote(&channel, track_id, "alice").await.unwrap(),
        1,
        "first vote"
    );
    assert_eq!(
        add_vote(&channel, track_id, "alice").await.unwrap(),
        1,
        "repeat vote is idempotent"
    );
    assert_eq!(add_vote(&channel, track_id, "bob").await.unwrap(), 2);

    let mut voters = get_voters(&channel, track_id).await.unwrap();
    voters.sort();
    assert_eq!(voters, vec!["alice".to_string(), "bob".to_string()]);

    // Votes are scoped per track
    assert_eq!(add_vote(&channel, "t2", "alice").await.unwrap(), 1);
    assert_eq!(get_voters(&channel, track_id).await.unwrap().len(), 2);

    // Votes from users which are no longer members are dropped
    let members = vec!["alice".to_string()];
    drop_stale_votes(&channel, track_id, &members)
        .await
        .unwrap();

    assert_eq!(
        get_voters(&channel, track_id).await.unwrap(),
        vec!["alice".to_string()]
    );
    // Votes for the other track are untouched
    assert_eq!(
        get_voters(&channel, "t2").await.unwrap(),
        vec!["alice".to_string()]
    );

    clear_votes(&channel, track_id).await.unwrap();
    assert!(get_voters(&channel, track_id).await.unwrap().is_empty());

    clear_channel(&channel).await.unwrap();
}

#[tokio::test]
async fn sequence_numbers_are_per_channel() {
    let _guard = SERIAL.lock().await;
    if !redis().await {
        return;
    }

    let channel = unique_channel();
    let other = unique_channel();

    assert_eq!(next_seq(&channel).await.unwrap(), 1);
    assert_eq!(next_seq(&channel).await.unwrap(), 2);
    assert_eq!(next_seq(&channel).await.unwrap(), 3);
    assert_eq!(next_seq(&other).await.unwrap(), 1);

    clear_channel(&channel).await.unwrap();
    clear_channel(&other).await.unwrap();

    // Clearing resets the counter, ids restart at one on a fresh channel
    assert_eq!(next_seq(&channel).await.unwrap(), 1);

    clear_channel(&channel).await.unwrap();
}

#[tokio::test]
async fn clear_channel_removes_every_key_and_the_user_index() {
    let _guard = SERIAL.lock().await;
    if !redis().await {
        return;
    }

    let channel = unique_channel();
    let alice = format!("user-{}", ulid::Ulid::new());

    add_member(&channel, &alice).await.unwrap();
    push_track(&channel, &track("t1", &alice)).await.unwrap();
    set_now(
        &channel,
        &DjNowPlaying {
            track: track("t1", &alice),
            started_at: unix_ms(),
        },
    )
    .await
    .unwrap();
    add_vote(&channel, "t1", &alice).await.unwrap();
    next_seq(&channel).await.unwrap();
    set_empty_since(&channel, unix_ms()).await.unwrap();

    clear_channel(&channel).await.unwrap();

    assert_absent(&[
        members_key(&channel),
        queue_key(&channel),
        now_key(&channel),
        votes_key(&channel, "t1"),
        seq_key(&channel),
        empty_since_key(&channel),
    ])
    .await;

    assert!(
        get_user_channels(&alice).await.unwrap().is_empty(),
        "the per-user index must not keep pointing at a cleared channel"
    );
    assert!(get_members(&channel).await.unwrap().is_empty());
    assert!(get_queue(&channel).await.unwrap().is_empty());
}

#[tokio::test]
async fn clear_all_wipes_dj_keys_only() {
    let _guard = SERIAL.lock().await;
    if !redis().await {
        return;
    }

    let channel = unique_channel();
    add_member(&channel, "user-a").await.unwrap();
    push_track(&channel, &track("t1", "user-a")).await.unwrap();

    // A neighbouring key which must survive the start up wipe
    let survivor = format!("djx-test:{}", ulid::Ulid::new());
    let mut conn = get_connection().await.unwrap();
    let _: () = conn.set(&survivor, "keep me").await.unwrap();

    clear_all().await.unwrap();

    assert_absent(&[members_key(&channel), queue_key(&channel)]).await;

    let value: Option<String> = conn.get(&survivor).await.unwrap();
    assert_eq!(value.as_deref(), Some("keep me"));

    // Every dj_ key is gone, not just this channel's
    let remaining: Vec<String> = conn.keys("dj_*").await.unwrap();
    assert!(remaining.is_empty(), "leftover dj keys: {remaining:?}");

    let _: u32 = conn.del(&survivor).await.unwrap();
}

#[tokio::test]
async fn conditional_clear_only_fires_while_the_channel_is_empty() {
    let _guard = SERIAL.lock().await;
    if !redis().await {
        return;
    }

    let channel = unique_channel();

    // A track is playing and somebody is a member
    add_member(&channel, "user-a").await.unwrap();
    set_now(
        &channel,
        &DjNowPlaying {
            track: track("t1", "user-a"),
            started_at: unix_ms(),
        },
    )
    .await
    .unwrap();
    add_vote(&channel, "t1", "user-a").await.unwrap();

    // The engine must not be able to clear a channel which still has members
    assert!(!clear_channel_if_empty(&channel).await.unwrap());
    assert!(is_member(&channel, "user-a").await.unwrap());
    assert!(get_now(&channel).await.unwrap().is_some());

    // Once the last member is gone it clears atomically
    remove_member(&channel, "user-a").await.unwrap();

    assert!(clear_channel_if_empty(&channel).await.unwrap());

    assert_absent(&[
        members_key(&channel),
        queue_key(&channel),
        now_key(&channel),
        seq_key(&channel),
        empty_since_key(&channel),
        votes_key(&channel, "t1"),
    ])
    .await;

    // Clearing an already empty channel is idempotent
    assert!(clear_channel_if_empty(&channel).await.unwrap());
}

#[tokio::test]
async fn votes_only_count_while_the_track_is_playing() {
    let _guard = SERIAL.lock().await;
    if !redis().await {
        return;
    }

    let channel = unique_channel();

    // Nothing is playing
    assert_eq!(
        cast_vote(&channel, "t1", "user-a").await.unwrap(),
        VoteOutcome::NoTrack
    );

    set_now(
        &channel,
        &DjNowPlaying {
            track: track("t1", "user-a"),
            started_at: unix_ms(),
        },
    )
    .await
    .unwrap();

    // A vote for the playing track counts
    assert_eq!(
        cast_vote(&channel, "t1", "user-a").await.unwrap(),
        VoteOutcome::Cast(1)
    );
    // and is idempotent per user
    assert_eq!(
        cast_vote(&channel, "t1", "user-a").await.unwrap(),
        VoteOutcome::Cast(1)
    );
    assert_eq!(
        cast_vote(&channel, "t1", "user-b").await.unwrap(),
        VoteOutcome::Cast(2)
    );

    // A vote against a track which is not the current one must not land, and
    // must not leave a `dj_votes` key behind
    assert_eq!(
        cast_vote(&channel, "t0", "user-a").await.unwrap(),
        VoteOutcome::TrackChanged
    );
    assert_absent(&[votes_key(&channel, "t0")]).await;

    // Once the track ends its votes no longer count for anything
    clear_now(&channel).await.unwrap();
    assert_eq!(
        cast_vote(&channel, "t1", "user-a").await.unwrap(),
        VoteOutcome::NoTrack
    );

    clear_channel(&channel).await.unwrap();
}

#[tokio::test]
async fn member_channels_lists_only_channels_with_members() {
    let _guard = SERIAL.lock().await;
    if !redis().await {
        return;
    }

    let occupied = unique_channel();
    let emptied = unique_channel();

    add_member(&occupied, "user-a").await.unwrap();
    add_member(&emptied, "user-b").await.unwrap();
    remove_member(&emptied, "user-b").await.unwrap();

    let channels = member_channels().await.unwrap();

    assert!(channels.contains(&occupied));
    assert!(
        !channels.contains(&emptied),
        "a channel whose member set is empty must not be swept"
    );

    clear_channel(&occupied).await.unwrap();
}

/// DJ channel document used by the event tests
fn dj_channel(id: &str) -> crate::Channel {
    crate::Channel::TextChannel {
        id: id.to_string(),
        server: "server-1".to_string(),
        name: "music".to_string(),
        description: None,
        icon: None,
        last_message_id: None,
        default_permissions: None,
        role_permissions: Default::default(),
        nsfw: false,
        voice: None,
        dj: Some(crate::DjInformation::default()),
        slowmode: None,
        default_auto_archive_minutes: None,
    }
}

#[tokio::test]
async fn a_closed_socket_announces_a_member_leaving_and_refreshes_the_vote() {
    let _guard = SERIAL.lock().await;
    if !redis().await {
        return;
    }

    let channel = unique_channel();
    let channel_doc = dj_channel(&channel);

    // Two members, one of them playing a track, and one vote cast
    add_member(&channel, "leaver").await.unwrap();
    add_member(&channel, "stayer").await.unwrap();
    set_now(
        &channel,
        &DjNowPlaying {
            track: track("t1", "stayer"),
            started_at: unix_ms(),
        },
    )
    .await
    .unwrap();
    add_vote(&channel, "t1", "stayer").await.unwrap();

    // Subscribe *before* the removal, exactly like a connected client
    let mut sub = redis_kiss::open_pubsub_connection().await.expect("pubsub");
    sub.psubscribe("stayer!").await.expect("psubscribe");

    let count = crate::dj::events::remove_member_and_announce(&channel_doc, "leaver")
        .await
        .expect("remove member");
    assert_eq!(count, 1);

    use futures::StreamExt;

    let mut messages = sub.on_message();
    let mut seen_left = false;
    let mut seen_vote = false;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);

    while tokio::time::Instant::now() < deadline && !(seen_left && seen_vote) {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let Ok(Some(message)) = tokio::time::timeout(remaining, messages.next()).await else {
            break;
        };

        let Ok(payload) = message.get_payload::<String>() else {
            continue;
        };
        let Ok(event) = serde_json::from_str::<crate::events::client::EventV1>(&payload) else {
            continue;
        };

        match event {
            crate::events::client::EventV1::DjMemberLeft {
                user, count, channel, ..
            } => {
                assert_eq!(user, "leaver");
                assert_eq!(count, 1);
                assert_eq!(channel, channel_doc.id());
                seen_left = true;
            }
            crate::events::client::EventV1::DjVote {
                votes, needed, voters, ..
            } => {
                // One member left, so one vote is enough
                assert_eq!(votes, 1);
                assert_eq!(needed, 1);
                assert_eq!(voters, vec!["stayer".to_string()]);
                seen_vote = true;
            }
            _ => {}
        }
    }

    assert!(seen_left, "the remaining member must see DjMemberLeft");
    assert!(
        seen_vote,
        "the skip tally must be refreshed when a member leaves"
    );

    // The empty grace period starts when the last member leaves
    crate::dj::events::remove_member_and_announce(&channel_doc, "stayer")
        .await
        .expect("remove last member");

    assert!(
        get_empty_since(&channel).await.unwrap().is_some(),
        "an emptied channel arms the grace period"
    );

    clear_channel(&channel).await.unwrap();
}
