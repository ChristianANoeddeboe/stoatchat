use revolt_database::{dj, util::reference::Reference, Database, User};
use revolt_result::{create_error, Result};
use rocket::State;
use rocket_empty::EmptyResponse;

use crate::dj::DjEngine;

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

    // Removes the member, announces it to the remaining members (and to the
    // leaver's other sessions), arms the empty-channel grace period and
    // refreshes the skip tally while a track is playing.
    dj::events::remove_member_and_announce(&channel, &user.id).await?;

    engine.notify(channel.id());

    Ok(EmptyResponse)
}
