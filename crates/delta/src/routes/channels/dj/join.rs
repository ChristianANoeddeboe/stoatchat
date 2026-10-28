use revolt_database::{
    dj,
    events::client::EventV1,
    util::{permissions::perms, reference::Reference},
    Database, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::Result;
use rocket::{serde::json::Json, State};

use super::check_join_permissions;
use crate::dj::{publish_to_members, resolve_dj_channel, DjEngine};

/// # Join DJ Channel
///
/// Joins the DJ channel, returning its full state.
///
/// Mirrors joining a voice channel: requires `Connect` and respects the
/// channel's `max_users` limit.
#[openapi(tag = "DJ")]
#[post("/<target>/dj/join")]
pub async fn join(
    db: &State<Database>,
    engine: &State<DjEngine>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<v0::DjChannelState>> {
    let channel = target.as_channel(db).await?;
    let settings = resolve_dj_channel(&channel)?;

    let mut query = perms(db, &user).channel(&channel);
    let permissions = calculate_channel_permissions(&mut query).await;
    check_join_permissions(&permissions)?;

    let members = dj::get_members(channel.id()).await?;

    if !members.contains(&user.id) {
        if let Some(max_users) = settings.max_users {
            if members.len() >= max_users as usize
                && !permissions.has(ChannelPermission::ManageChannel as u64)
            {
                return Err(revolt_result::create_error!(CannotJoinDjChannel));
            }
        }

        let count = dj::add_member(channel.id(), &user.id).await?;

        // Joining can change the outcome of a skip vote
        engine.start(channel.id(), settings.clone()).await;

        publish_to_members(
            channel.id(),
            EventV1::DjMemberJoined {
                channel: channel.id().to_string(),
                user: user.id.clone(),
                count,
            },
        )
        .await;
    } else {
        // Re-joining is idempotent, it only ensures the engine is running
        engine.start(channel.id(), settings.clone()).await;
    }

    let state = engine.state(channel.id(), &settings).await?;

    // Give the joiner a signed stream URL immediately, so joining mid-track
    // starts playback without waiting for the next heartbeat
    if let Some(current) = &state.current {
        EventV1::DjNowPlaying {
            channel: channel.id().to_string(),
            track: current.track.clone(),
            position_ms: state.position_ms,
            server_time: state.server_time,
            signed_url: engine.stream_url(channel.id(), &current.track.id, &settings),
        }
        .private(user.id.clone())
        .await;
    }

    Ok(Json(state))
}
