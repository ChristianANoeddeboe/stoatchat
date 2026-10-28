use revolt_database::{
    events::client::EventV1,
    util::{permissions::perms, reference::Reference},
    AuditLogEntryAction, Database, PartialChannel, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use crate::dj::{publish_to_members, DjEngine};
use crate::util::audit_log_reason::AuditLogReason;

/// # Edit DJ Channel Settings
///
/// Updates the settings of a DJ channel. Requires `ManageChannel`.
///
/// DJ settings are deliberately not part of `DataEditChannel`: they are only
/// valid on DJ channels and should not leak into audit log diffs of every other
/// channel type. Setting `enabled: false` stops playback and clears the
/// channel's queue and audio, exactly like [`disable`].
#[openapi(tag = "DJ")]
#[patch("/<target>/dj/settings", data = "<data>")]
pub async fn edit(
    db: &State<Database>,
    engine: &State<DjEngine>,
    user: User,
    reason: AuditLogReason,
    target: Reference<'_>,
    data: Json<v0::DataEditDjSettings>,
) -> Result<Json<v0::DjChannelState>> {
    let data = data.into_inner();
    let mut channel = target.as_channel(db).await?;

    let Some(current) = channel.dj().map(|dj| dj.into_owned()) else {
        return Err(create_error!(NotADjChannel));
    };

    let mut query = perms(db, &user).channel(&channel);
    calculate_channel_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ManageChannel)?;

    let settings = apply_settings(&current, data)?;

    write_settings(db, &mut channel, &user, Some(&current.into()), &settings, reason).await?;

    engine.update_settings(channel.id(), settings.clone()).await;

    if !settings.enabled {
        // Disabling is an off switch: stop playback and drop the ephemeral
        // queue and audio rather than leaving a silent task running.
        engine.stop(channel.id());
        crate::dj::clear_channel_state(channel.id()).await;
    }

    publish_to_members(
        channel.id(),
        EventV1::DjSettingsUpdate {
            channel: channel.id().to_string(),
            settings: settings.clone(),
        },
    )
    .await;

    Ok(Json(engine.state(channel.id(), &settings).await?))
}

/// # Enable DJ
///
/// Turns a text channel into a DJ channel, or re-enables a disabled one.
/// Requires `ManageChannel`.
#[openapi(tag = "DJ")]
#[post("/<target>/dj/enable")]
pub async fn enable(
    db: &State<Database>,
    engine: &State<DjEngine>,
    user: User,
    reason: AuditLogReason,
    target: Reference<'_>,
) -> Result<Json<v0::DjChannelState>> {
    let mut channel = target.as_channel(db).await?;

    if !matches!(channel, revolt_database::Channel::TextChannel { .. }) {
        return Err(create_error!(NotADjChannel));
    }

    let mut query = perms(db, &user).channel(&channel);
    calculate_channel_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ManageChannel)?;

    let current: Option<v0::DjInformation> = channel.dj().map(|dj| dj.into_owned().into());
    let settings = apply_enabled(current.as_ref(), true);

    write_settings(db, &mut channel, &user, current.as_ref(), &settings, reason).await?;

    engine.update_settings(channel.id(), settings.clone()).await;

    publish_to_members(
        channel.id(),
        EventV1::DjSettingsUpdate {
            channel: channel.id().to_string(),
            settings: settings.clone(),
        },
    )
    .await;

    Ok(Json(engine.state(channel.id(), &settings).await?))
}

/// # Disable DJ
///
/// Disables the DJ of a channel without forgetting its settings. The channel
/// behaves like a normal text channel again until it is re-enabled. Requires
/// `ManageChannel`.
#[openapi(tag = "DJ")]
#[post("/<target>/dj/disable")]
pub async fn disable(
    db: &State<Database>,
    engine: &State<DjEngine>,
    user: User,
    reason: AuditLogReason,
    target: Reference<'_>,
) -> Result<Json<v0::DjChannelState>> {
    let mut channel = target.as_channel(db).await?;

    let Some(current) = channel.dj().map(|dj| dj.into_owned()) else {
        return Err(create_error!(NotADjChannel));
    };

    let mut query = perms(db, &user).channel(&channel);
    calculate_channel_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ManageChannel)?;

    let current: v0::DjInformation = current.into();
    let settings = apply_enabled(Some(&current), false);

    write_settings(db, &mut channel, &user, Some(&current), &settings, reason).await?;

    engine.stop(channel.id());
    crate::dj::clear_channel_state(channel.id()).await;

    publish_to_members(
        channel.id(),
        EventV1::DjSettingsUpdate {
            channel: channel.id().to_string(),
            settings: settings.clone(),
        },
    )
    .await;

    Ok(Json(engine.state(channel.id(), &settings).await?))
}

/// Persist settings through `Channel::update`
///
/// Using the model (rather than `Database::update_channel`) is what makes the
/// change visible to non-joined members: it emits `EventV1::ChannelUpdate` on
/// the server topic. The audit log entry keeps before/after DJ settings.
async fn write_settings(
    db: &Database,
    channel: &mut revolt_database::Channel,
    user: &User,
    before: Option<&v0::DjInformation>,
    after: &v0::DjInformation,
    reason: AuditLogReason,
) -> Result<()> {
    let server = channel.server().map(str::to_string);

    channel
        .update(
            db,
            PartialChannel {
                dj: Some(after.clone().into()),
                ..Default::default()
            },
            vec![],
        )
        .await?;

    // Enabling a plain channel has no "before" state to diff against
    if let (Some(server), Some(before)) = (server, before) {
        AuditLogEntryAction::ChannelEdit {
            channel: channel.id().to_string(),
            before: PartialChannel {
                dj: Some(before.clone().into()),
                ..Default::default()
            },
            after: PartialChannel {
                dj: Some(after.clone().into()),
                ..Default::default()
            },
        }
        .insert(db, server, reason, user.id.clone(), None)
        .await;
    }

    Ok(())
}

/// Settings after enabling or disabling the DJ
///
/// Enabling a channel without DJ state installs the defaults; disabling keeps
/// every setting so the channel can be switched back on unchanged.
fn apply_enabled(current: Option<&v0::DjInformation>, enabled: bool) -> v0::DjInformation {
    let mut settings = current.cloned().unwrap_or_default();
    settings.enabled = enabled;
    settings
}


/// Apply a partial update to the settings of a DJ channel
fn apply_settings(
    current: &revolt_database::DjInformation,
    data: v0::DataEditDjSettings,
) -> Result<v0::DjInformation> {
    let mut settings: v0::DjInformation = current.clone().into();

    if let Some(skip_threshold) = data.skip_threshold {
        if !skip_threshold.is_finite() || !(0.0..=1.0).contains(&skip_threshold) {
            return Err(create_error!(FailedValidation {
                error: "skip_threshold: must be between 0.0 and 1.0".to_string()
            }));
        }

        settings.skip_threshold = skip_threshold;
    }

    if let Some(max_file_mb) = data.max_file_mb {
        if !(1..=128).contains(&max_file_mb) {
            return Err(create_error!(FailedValidation {
                error: "max_file_mb: must be between 1 and 128".to_string()
            }));
        }

        settings.max_file_mb = max_file_mb;
    }

    if let Some(max_track_seconds) = data.max_track_seconds {
        if !(5..=3600).contains(&max_track_seconds) {
            return Err(create_error!(FailedValidation {
                error: "max_track_seconds: must be between 5 and 3600".to_string()
            }));
        }

        settings.max_track_seconds = max_track_seconds;
    }

    if let Some(max_tracks) = data.max_tracks {
        if !(1..=1000).contains(&max_tracks) {
            return Err(create_error!(FailedValidation {
                error: "max_tracks: must be between 1 and 1000".to_string()
            }));
        }

        settings.max_tracks = max_tracks;
    }

    if let Some(max_total_bytes) = data.max_total_bytes {
        // At least one megabyte, and no more than 1 TiB
        if !(1024 * 1024..=1024 * 1024 * 1024 * 1024).contains(&max_total_bytes) {
            return Err(create_error!(FailedValidation {
                error: "max_total_bytes: must be between 1 MiB and 1 TiB".to_string()
            }));
        }

        settings.max_total_bytes = max_total_bytes;
    }

    if let Some(max_users) = data.max_users {
        if max_users > 1000 {
            return Err(create_error!(FailedValidation {
                error: "max_users: must be at most 1000 (0 removes the limit)".to_string()
            }));
        }

        settings.max_users = if max_users == 0 {
            None
        } else {
            Some(max_users)
        };
    }

    if let Some(enabled) = data.enabled {
        settings.enabled = enabled;
    }

    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::test::TestHarness;
    use revolt_result::ErrorType;
    use rocket::http::{Header, Status};

    #[test]
    fn enabling_a_plain_channel_installs_the_defaults() {
        let settings = apply_enabled(None, true);

        assert!(settings.enabled);
        assert_eq!(settings, v0::DjInformation::default());
    }

    #[test]
    fn enabling_keeps_the_settings_of_a_disabled_channel() {
        let disabled = v0::DjInformation {
            enabled: false,
            skip_threshold: 0.25,
            max_tracks: 7,
            ..Default::default()
        };

        let settings = apply_enabled(Some(&disabled), true);

        assert!(settings.enabled);
        assert_eq!(settings.skip_threshold, 0.25);
        assert_eq!(settings.max_tracks, 7);
    }

    #[test]
    fn disabling_never_forgets_the_settings() {
        let current = v0::DjInformation {
            enabled: true,
            max_file_mb: 12,
            max_users: Some(4),
            ..Default::default()
        };

        let settings = apply_enabled(Some(&current), false);

        assert!(!settings.enabled);
        assert_eq!(settings.max_file_mb, 12);
        assert_eq!(settings.max_users, Some(4));
        // Round-tripping through disable/enable is lossless
        assert_eq!(apply_enabled(Some(&settings), true), current);
    }

    /// Settings changes reach non-joined members and are audited
    ///
    /// Requires MongoDB; skipped when it is not running.
    #[rocket::async_test]
    async fn settings_update_emits_channel_update() {
        use revolt_database::events::client::EventV1;

        if !TestHarness::database_available().await {
            return;
        }

        let mut harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let (server, channels) = harness.new_server(&user).await;
        let channel = channels[0].id().to_string();

        // Make it a DJ channel first
        let response = harness
            .client
            .post(format!("/channels/{channel}/dj/enable"))
            .header(Header::new("x-session-token", session.token.to_string()))
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        drop(response);

        let response = harness
            .client
            .patch(format!("/channels/{channel}/dj/settings"))
            .header(Header::new("x-session-token", session.token.to_string()))
            .header(Header::new("Content-Type", "application/json"))
            .body(r#"{"max_tracks": 3}"#)
            .dispatch()
            .await;
        assert_eq!(response.status(), Status::Ok);
        drop(response);

        // The change is published on the server topic, not only to joined
        // members: everyone who can see the channel must see the new settings.
        let event = harness
            .wait_for_event(&server.id, |event| match event {
                EventV1::ChannelUpdate { id, data, .. } => {
                    id == &channel && data.dj.as_ref().is_some_and(|dj| dj.max_tracks == 3)
                }
                _ => false,
            })
            .await;

        assert!(
            matches!(event, EventV1::ChannelUpdate { .. }),
            "settings must emit ChannelUpdate on the server topic"
        );
    }

    /// A DJ channel can be created and switched off through the API
    ///
    /// Requires MongoDB; skipped when it is not running.
    #[rocket::async_test]
    async fn enable_and_disable_routes_round_trip() {
        if !TestHarness::database_available().await {
            return;
        }

        let harness = TestHarness::new().await;
        let (_, session, user) = harness.new_user().await;
        let (_, channels) = harness.new_server(&user).await;
        let channel = channels[0].id().to_string();

        // No API path could create a DJ channel before this route existed
        let response = harness
            .client
            .post(format!("/channels/{channel}/dj/enable"))
            .header(Header::new("x-session-token", session.token.to_string()))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Ok);

        let channel_doc = harness.db.fetch_channel(&channel).await.expect("channel");
        let settings = channel_doc.dj().expect("channel is now a DJ channel");
        assert!(settings.enabled);

        // Disabling keeps the settings but turns the DJ off
        let response = harness
            .client
            .post(format!("/channels/{channel}/dj/disable"))
            .header(Header::new("x-session-token", session.token.to_string()))
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Ok);

        let channel_doc = harness.db.fetch_channel(&channel).await.expect("channel");
        let settings = channel_doc.dj().expect("settings are kept");
        assert!(!settings.enabled);
    }

    fn base() -> revolt_database::DjInformation {
        revolt_database::DjInformation::default()
    }

    #[test]
    fn applies_partial_updates() {
        let settings = apply_settings(
            &base(),
            v0::DataEditDjSettings {
                skip_threshold: Some(1.0),
                max_tracks: Some(10),
                ..Default::default()
            },
        )
        .expect("valid settings");

        assert_eq!(settings.skip_threshold, 1.0);
        assert_eq!(settings.max_tracks, 10);
        // Untouched values are preserved
        assert_eq!(settings.max_track_seconds, 900);
    }

    #[test]
    fn rejects_out_of_range_threshold() {
        let error = apply_settings(
            &base(),
            v0::DataEditDjSettings {
                skip_threshold: Some(1.5),
                ..Default::default()
            },
        )
        .expect_err("threshold above one");

        assert!(matches!(
            error.error_type,
            ErrorType::FailedValidation { .. }
        ));
    }

    #[test]
    fn zero_max_users_removes_the_limit() {
        let settings = apply_settings(
            &base(),
            v0::DataEditDjSettings {
                max_users: Some(0),
                ..Default::default()
            },
        )
        .expect("valid settings");

        assert_eq!(settings.max_users, None);
    }

    fn edited(data: v0::DataEditDjSettings) -> Result<v0::DjInformation, revolt_result::Error> {
        apply_settings(&base(), data)
    }

    #[test]
    fn accepts_every_documented_boundary() {
        // Includes the exact lower and upper bounds of each field
        let settings = edited(v0::DataEditDjSettings {
            skip_threshold: Some(1.0),
            max_file_mb: Some(128),
            max_track_seconds: Some(3600),
            max_tracks: Some(1000),
            max_total_bytes: Some(1024 * 1024 * 1024 * 1024),
            max_users: Some(1000),
            enabled: Some(false),
        })
        .expect("upper bounds are valid");

        assert_eq!(settings.max_file_mb, 128);
        assert_eq!(settings.max_tracks, 1000);
        assert!(!settings.enabled);

        let settings = edited(v0::DataEditDjSettings {
            skip_threshold: Some(0.0),
            max_file_mb: Some(1),
            max_track_seconds: Some(5),
            max_tracks: Some(1),
            max_total_bytes: Some(1024 * 1024),
            max_users: Some(1),
            enabled: Some(true),
        })
        .expect("lower bounds are valid");

        assert_eq!(settings.skip_threshold, 0.0);
        assert_eq!(settings.max_track_seconds, 5);
        assert_eq!(settings.max_total_bytes, 1024 * 1024);
    }

    #[test]
    fn rejects_every_out_of_range_field() {
        let cases: Vec<(&str, v0::DataEditDjSettings)> = vec![
            (
                "max_file_mb 0",
                v0::DataEditDjSettings {
                    max_file_mb: Some(0),
                    ..Default::default()
                },
            ),
            (
                "max_file_mb 129",
                v0::DataEditDjSettings {
                    max_file_mb: Some(129),
                    ..Default::default()
                },
            ),
            (
                "max_track_seconds 4",
                v0::DataEditDjSettings {
                    max_track_seconds: Some(4),
                    ..Default::default()
                },
            ),
            (
                "max_track_seconds 3601",
                v0::DataEditDjSettings {
                    max_track_seconds: Some(3601),
                    ..Default::default()
                },
            ),
            (
                "max_tracks 0",
                v0::DataEditDjSettings {
                    max_tracks: Some(0),
                    ..Default::default()
                },
            ),
            (
                "max_tracks 1001",
                v0::DataEditDjSettings {
                    max_tracks: Some(1001),
                    ..Default::default()
                },
            ),
            (
                "max_total_bytes below 1 MiB",
                v0::DataEditDjSettings {
                    max_total_bytes: Some(1024 * 1024 - 1),
                    ..Default::default()
                },
            ),
            (
                "max_total_bytes above 1 TiB",
                v0::DataEditDjSettings {
                    max_total_bytes: Some(1024 * 1024 * 1024 * 1024 + 1),
                    ..Default::default()
                },
            ),
            (
                "max_users 1001",
                v0::DataEditDjSettings {
                    max_users: Some(1001),
                    ..Default::default()
                },
            ),
            (
                "skip_threshold -0.5",
                v0::DataEditDjSettings {
                    skip_threshold: Some(-0.5),
                    ..Default::default()
                },
            ),
            (
                "skip_threshold NaN",
                v0::DataEditDjSettings {
                    skip_threshold: Some(f32::NAN),
                    ..Default::default()
                },
            ),
            (
                "skip_threshold infinity",
                v0::DataEditDjSettings {
                    skip_threshold: Some(f32::INFINITY),
                    ..Default::default()
                },
            ),
        ];

        for (name, data) in cases {
            let error = edited(data).expect_err(name);

            match &error.error_type {
                ErrorType::FailedValidation { error: message } => {
                    assert!(
                        message.contains(':'),
                        "{}: message should name the field, got {:?}",
                        name,
                        message
                    );
                }
                other => panic!("{}: unexpected error {:?}", name, other),
            }
        }
    }

    #[test]
    fn preserves_fields_which_are_not_being_edited() {
        // A settings patch must not reset unrelated fields to their defaults
        let current = revolt_database::DjInformation {
            skip_threshold: 0.25,
            max_file_mb: 7,
            max_track_seconds: 120,
            max_tracks: 9,
            max_total_bytes: 5 * 1024 * 1024,
            max_users: Some(4),
            enabled: true,
        };

        let settings = apply_settings(
            &current,
            v0::DataEditDjSettings {
                enabled: Some(false),
                ..Default::default()
            },
        )
        .expect("valid settings");

        assert!(!settings.enabled);
        assert_eq!(settings.skip_threshold, 0.25);
        assert_eq!(settings.max_file_mb, 7);
        assert_eq!(settings.max_track_seconds, 120);
        assert_eq!(settings.max_tracks, 9);
        assert_eq!(settings.max_total_bytes, 5 * 1024 * 1024);
        assert_eq!(settings.max_users, Some(4));
    }

    #[test]
    fn max_users_takes_precedence_over_a_zero_value() {
        let current = revolt_database::DjInformation {
            max_users: Some(3),
            ..Default::default()
        };

        let settings = apply_settings(
            &current,
            v0::DataEditDjSettings {
                max_users: Some(0),
                ..Default::default()
            },
        )
        .expect("zero removes the limit");

        assert_eq!(settings.max_users, None);
    }
}
