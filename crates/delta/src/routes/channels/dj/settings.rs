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
}
