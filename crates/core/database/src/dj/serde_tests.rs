//! Wire and storage compatibility tests for the DJ channel model.
//!
//! The feature deliberately models DJ as an optional field on a text channel so
//! that already-shipped clients and already-stored Mongo documents keep working.
//! That only holds if the serde contract is exactly as intended, which is what
//! these tests pin down.

use std::collections::HashMap;

use revolt_models::v0;
use serde_json::{json, Value};

use crate::{Channel, DjInformation};

fn text_channel(dj: Option<DjInformation>) -> Channel {
    Channel::TextChannel {
        id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
        server: "01ARZ3NDEKTSV4RRFFQ69G5FAW".to_string(),
        name: "music".to_string(),
        description: None,
        icon: None,
        last_message_id: None,
        default_permissions: None,
        role_permissions: HashMap::new(),
        nsfw: false,
        voice: None,
        dj,
        slowmode: None,
        default_auto_archive_minutes: None,
    }
}

#[test]
fn channels_stored_before_the_feature_have_no_dj_state() {
    // A document written by the previous release has no `dj` key at all
    let legacy = json!({
        "channel_type": "TextChannel",
        "_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
        "server": "01ARZ3NDEKTSV4RRFFQ69G5FAW",
        "name": "general",
        "role_permissions": {},
        "nsfw": false
    });

    let channel: Channel = serde_json::from_value(legacy).expect("legacy document must load");

    assert!(channel.dj().is_none());
    assert!(!channel.is_dj_channel());
}

#[test]
fn non_dj_channels_never_serialise_a_dj_key() {
    let value = serde_json::to_value(text_channel(None)).expect("serialise");

    assert!(
        value.get("dj").is_none(),
        "an absent DJ capability must not appear in the payload: {value}"
    );
}

#[test]
fn dj_channels_serialise_every_setting() {
    let value = serde_json::to_value(text_channel(Some(DjInformation {
        skip_threshold: 0.25,
        max_file_mb: 10,
        max_track_seconds: 120,
        max_tracks: 5,
        max_total_bytes: 1024 * 1024,
        max_users: Some(3),
        enabled: true,
    })))
    .expect("serialise");

    let dj = value.get("dj").expect("dj present");
    assert_eq!(dj["skip_threshold"], json!(0.25));
    assert_eq!(dj["max_file_mb"], json!(10));
    assert_eq!(dj["max_track_seconds"], json!(120));
    assert_eq!(dj["max_tracks"], json!(5));
    assert_eq!(dj["max_total_bytes"], json!(1024 * 1024));
    assert_eq!(dj["max_users"], json!(3));
    assert_eq!(dj["enabled"], json!(true));
}

#[test]
fn dj_settings_default_when_a_client_sends_an_empty_object() {
    // `#[serde(default)]` on the struct: `{"dj": {}}` must yield the defaults,
    // not a channel with a threshold of zero.
    let payload = json!({
        "channel_type": "TextChannel",
        "_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
        "server": "01ARZ3NDEKTSV4RRFFQ69G5FAW",
        "name": "music",
        "role_permissions": {},
        "nsfw": false,
        "dj": {}
    });

    let channel: Channel = serde_json::from_value(payload).expect("empty dj object");
    let dj = channel.dj().expect("dj present");

    assert_eq!(dj.skip_threshold, 0.5);
    assert_eq!(dj.max_file_mb, 50);
    assert_eq!(dj.max_track_seconds, 900);
    assert_eq!(dj.max_tracks, 100);
    assert_eq!(dj.max_total_bytes, 20 * 1024 * 1024 * 1024);
    assert_eq!(dj.max_users, None);
    assert!(
        dj.enabled,
        "a DJ channel is enabled unless stated otherwise"
    );
}

#[test]
fn the_wire_model_matches_the_stored_model() {
    let stored = DjInformation {
        skip_threshold: 0.75,
        max_file_mb: 3,
        max_track_seconds: 42,
        max_tracks: 7,
        max_total_bytes: 9 * 1024 * 1024,
        max_users: Some(2),
        enabled: false,
    };

    let wire: v0::DjInformation = stored.clone().into();
    let back: DjInformation = wire.clone().into();

    assert_eq!(back, stored, "a settings round trip must be lossless");

    assert_eq!(wire.skip_threshold, 0.75);
    assert_eq!(wire.max_tracks, 7);
    assert_eq!(wire.max_users, Some(2));
    assert!(!wire.enabled);
}

#[test]
fn channel_state_uses_the_underscore_id_key() {
    let state = v0::DjChannelState {
        id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
        current: None,
        position_ms: 0,
        server_time: 1_700_000_000_000,
        queue: vec![v0::DjTrack {
            id: "1-01ARZ3NDEKTSV4RRFFQ69G5FAZ".to_string(),
            file: "/dj/c/1.mp3".to_string(),
            title: "Song".to_string(),
            duration_ms: 1000,
            uploader: "01ARZ3NDEKTSV4RRFFQ69G5FAW".to_string(),
        }],
        votes: 0,
        voters: vec![],
        members: vec!["01ARZ3NDEKTSV4RRFFQ69G5FAW".to_string()],
        settings: v0::DjInformation::default(),
    };

    let value: Value = serde_json::to_value(&state).expect("serialise");

    assert_eq!(value["_id"], json!("01ARZ3NDEKTSV4RRFFQ69G5FAV"));
    assert!(
        value.get("id").is_none(),
        "the state id is serialised as _id like every other channel"
    );
    // `current` is omitted while nothing plays
    assert!(value.get("current").is_none());
    assert_eq!(value["queue"][0]["title"], json!("Song"));
}

#[test]
fn settings_patches_deserialise_partially() {
    let patch: v0::DataEditDjSettings =
        serde_json::from_value(json!({ "max_tracks": 5 })).expect("partial patch");

    assert_eq!(patch.max_tracks, Some(5));
    assert_eq!(patch.skip_threshold, None);
    assert_eq!(patch.max_users, None);
    assert_eq!(patch.enabled, None);
}

#[test]
fn removing_the_dj_field_turns_the_channel_back_into_a_text_channel() {
    use crate::FieldsChannel;

    let mut channel = text_channel(Some(DjInformation::default()));
    assert!(channel.is_dj_channel());

    channel.remove_field(&FieldsChannel::Dj);

    assert!(channel.dj().is_none());
    assert!(!channel.is_dj_channel());

    // The wire variant of the same field maps to it, so `PATCH` with
    // `remove: ["Dj"]` reaches the same code path
    let wire: v0::FieldsChannel = serde_json::from_value(json!("Dj")).expect("variant");
    assert_eq!(crate::FieldsChannel::from(wire), FieldsChannel::Dj);
}

#[test]
fn partial_channel_carries_dj_through_the_bridge() {
    // `dj` must survive both directions or settings would silently disappear
    // on every round trip (and never appear in an audit log diff).
    let settings = DjInformation {
        skip_threshold: 0.75,
        max_tracks: 5,
        ..Default::default()
    };

    let wire = v0::PartialChannel {
        dj: Some(settings.clone().into()),
        ..Default::default()
    };

    let stored = crate::PartialChannel::from(wire.clone());
    assert_eq!(stored.dj.as_ref(), Some(&settings));

    let back = v0::PartialChannel::from(stored);
    assert_eq!(back.dj.expect("dj survived").max_tracks, 5);

    // And it serialises into the audit log diff the same way
    let value = serde_json::to_value(&wire).expect("serialise");
    assert_eq!(value["dj"]["skip_threshold"], json!(0.75));
    assert!(value["name"].is_null());
}
