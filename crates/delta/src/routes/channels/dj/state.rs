use revolt_database::{
    util::{permissions::perms, reference::Reference},
    Database, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use crate::dj::DjEngine;

/// # Fetch DJ Channel State
///
/// Returns the current track, playback position, queue, votes and members of a
/// DJ channel. Read-only for users which have not joined.
#[openapi(tag = "DJ")]
#[get("/<target>/dj")]
pub async fn fetch(
    db: &State<Database>,
    engine: &State<DjEngine>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<v0::DjChannelState>> {
    let channel = target.as_channel(db).await?;

    let Some(settings) = channel.dj() else {
        return Err(create_error!(NotADjChannel));
    };

    let mut query = perms(db, &user).channel(&channel);
    calculate_channel_permissions(&mut query)
        .await
        .throw_if_lacking_channel_permission(ChannelPermission::ViewChannel)?;

    let settings: v0::DjInformation = settings.into_owned().into();

    Ok(Json(engine.state(channel.id(), &settings).await?))
}
