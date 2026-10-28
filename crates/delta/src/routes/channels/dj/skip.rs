use revolt_database::{
    dj,
    events::client::EventV1,
    util::{permissions::perms, reference::Reference},
    Database, User,
};
use revolt_models::v0;
use revolt_permissions::calculate_channel_permissions;
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};

use super::check_vote_permissions;
use crate::dj::{publish_to_members, resolve_dj_channel, DjEngine};

/// # Vote to Skip
///
/// Votes to skip the track which is currently playing. Idempotent per
/// `(track, user)`.
///
/// The track is skipped once `max(1, ceil(present_members * skip_threshold))`
/// votes are cast; votes are re-evaluated when members join or leave, and votes
/// from users which are no longer members are ignored.
#[openapi(tag = "DJ")]
#[post("/<target>/dj/skip")]
pub async fn skip(
    db: &State<Database>,
    engine: &State<DjEngine>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<v0::DjChannelState>> {
    let channel = target.as_channel(db).await?;
    let settings = resolve_dj_channel(&channel)?;

    // A kicked or banned user keeps their Redis membership until the socket
    // cleanup or the reconciliation sweep runs, so permissions are re-checked
    // on every vote rather than trusted from the membership set.
    let mut query = perms(db, &user).channel(&channel);
    let permissions = calculate_channel_permissions(&mut query).await;
    check_vote_permissions(&permissions)?;

    if !dj::is_member(channel.id(), &user.id).await? {
        return Err(create_error!(NotInDjChannel));
    }

    let Some(current) = dj::get_now(channel.id()).await? else {
        // Nothing is playing, so there is nothing to vote against
        return Ok(Json(engine.state(channel.id(), &settings).await?));
    };

    // Compare-and-set: the vote only counts while `dj_now` still holds this
    // track, so a vote which races the end of a track neither lands on a dead
    // track nor leaks a `dj_votes` key nobody will clear.
    match dj::cast_vote(channel.id(), &current.track.id, &user.id).await? {
        dj::VoteOutcome::Cast(_) => {}
        dj::VoteOutcome::NoTrack | dj::VoteOutcome::TrackChanged => {
            return Ok(Json(engine.state(channel.id(), &settings).await?));
        }
    }

    let members = dj::get_members(channel.id()).await?;
    let voters = dj::get_voters(channel.id(), &current.track.id).await?;
    let (votes, needed, should_skip) = dj::tally(&voters, &members, settings.skip_threshold);

    let voters = voters
        .into_iter()
        .filter(|voter| members.contains(voter))
        .collect::<Vec<_>>();

    publish_to_members(
        channel.id(),
        EventV1::DjVote {
            channel: channel.id().to_string(),
            votes,
            needed,
            voters,
        },
    )
    .await;

    // The engine re-evaluates the vote on its next wake-up; nudge it so a track
    // skips without waiting for the heartbeat.
    engine.notify(channel.id());

    if should_skip {
        log::debug!(
            "DJ track {} in {} reached {votes}/{needed} votes",
            current.track.id,
            channel.id()
        );
    }

    Ok(Json(engine.state(channel.id(), &settings).await?))
}
