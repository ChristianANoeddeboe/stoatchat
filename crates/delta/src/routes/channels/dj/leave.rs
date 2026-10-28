use revolt_database::{dj, events::client::EventV1, util::reference::Reference, Database, User};
use revolt_result::{create_error, Result};
use rocket::State;
use rocket_empty::EmptyResponse;

use crate::dj::{publish_to_members, DjEngine};

/// # Leave DJ Channel
///
/// Leaves the DJ channel, mirroring leaving a voice call.
///
/// Leaving is idempotent. When the last member leaves the channel's empty grace
/// period starts; if nobody rejoins within it, the queue and files are cleared.
#[openapi(tag = "DJ")]
#[post("/<target>/dj/leave")]
pub async fn leave(
    db: &State<Database>,
    engine: &State<DjEngine>,
    user: User,
    target: Reference<'_>,
) -> Result<EmptyResponse> {
    let channel = target.as_channel(db).await?;

    if channel.dj().is_none() {
        return Err(create_error!(NotADjChannel));
    }

    let count = dj::remove_member(channel.id(), &user.id).await?;

    if count == 0 {
        dj::set_empty_since(channel.id(), dj::unix_ms()).await?;
    }

    publish_to_members(
        channel.id(),
        EventV1::DjMemberLeft {
            channel: channel.id().to_string(),
            user: user.id.clone(),
            count,
        },
    )
    .await;

    // Also tell the leaving user's other sessions to close their player
    EventV1::DjMemberLeft {
        channel: channel.id().to_string(),
        user: user.id.clone(),
        count,
    }
    .private(user.id.clone())
    .await;

    engine.notify(channel.id());

    Ok(EmptyResponse)
}
