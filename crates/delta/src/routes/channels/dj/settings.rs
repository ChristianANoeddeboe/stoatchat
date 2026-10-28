use revolt_database::{
    events::client::EventV1,
    util::{permissions::perms, reference::Reference},
    Database, PartialChannel, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use crate::dj::{publish_to_members, DjEngine};

/// # Edit DJ Channel Settings
///
/// Updates the settings of a DJ channel. Requires `ManageChannel`.
///
/// DJ settings are deliberately not part of `DataEditChannel`: they are only
/// valid on DJ channels and should not leak into audit log diffs of every other
/// channel type.
#[openapi(tag = "DJ")]
#[patch("/<target>/dj/settings", data = "<data>")]
pub async fn edit(
    db: &State<Database>,
    engine: &State<DjEngine>,
    user: User,
    target: Reference<'_>,
    data: Json<v0::DataEditDjSettings>,
) -> Result<Json<v0::DjChannelState>> {
    let data = data.into_inner();
    let channel = target.as_channel(db).await?;

    let Some(current) = channel.dj() else {
        return Err(create_error!(NotADjChannel));
    };

    let mut query = perms(db, &user).channel(&channel);
    calculate_channel_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ManageChannel)?;

    let settings = apply_settings(current.as_ref(), data)?;

    db.update_channel(
        channel.id(),
        &PartialChannel {
            dj: Some(settings.clone().into()),
            ..Default::default()
        },
        vec![],
    )
    .await?;

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
    use revolt_result::ErrorType;

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
