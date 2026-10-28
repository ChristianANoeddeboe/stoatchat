//! Membership events for DJ channels.
//!
//! DJ events are intentionally **not** published on the channel topic: that
//! reaches every member with `ViewChannel` and would leak queue contents and
//! signed stream URLs. Everything here fans out privately to the users which
//! have joined the channel instead.
//!
//! These helpers live in the database crate (rather than in delta) because the
//! websocket gateway is the one which notices an unclean disconnect, and it must
//! be able to tell the remaining members that somebody left.

use crate::{
    dj,
    events::client::EventV1,
    Channel, Database,
};

/// Publish an event to every member which joined the channel
pub async fn publish_to_members(channel: &str, event: EventV1) {
    let members = dj::get_members(channel).await.unwrap_or_default();

    for member in members {
        event.clone().private(member).await;
    }
}

/// Recompute and publish the skip vote tally of the channel's current track
///
/// Call this whenever the set of present members changed: the number of votes
/// needed depends on how many members are left.
pub async fn publish_vote(channel: &Channel) -> revolt_result::Result<()> {
    let Some(settings) = channel.dj() else {
        return Ok(());
    };

    let Some(current) = dj::get_now(channel.id()).await? else {
        return Ok(());
    };

    let members = dj::get_members(channel.id()).await?;
    let voters = dj::get_voters(channel.id(), &current.track.id).await?;
    let (votes, needed, _) = dj::tally(&voters, &members, settings.skip_threshold);

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

    Ok(())
}

/// Remove the user from the channel and tell everyone about it
///
/// Emits `DjMemberLeft` to the remaining members and to the user's other
/// sessions, starts the empty-channel grace period when the last member leaves
/// and refreshes the skip tally while a track is playing.
pub async fn remove_member_and_announce(
    channel: &Channel,
    user: &str,
) -> revolt_result::Result<u32> {
    let count = dj::remove_member(channel.id(), user).await?;

    if count == 0 {
        dj::set_empty_since(channel.id(), dj::unix_ms()).await.ok();
    }

    let event = EventV1::DjMemberLeft {
        channel: channel.id().to_string(),
        user: user.to_string(),
        count,
    };

    publish_to_members(channel.id(), event.clone()).await;
    // Also tell the user's other sessions to close their player
    event.private(user.to_string()).await;

    publish_vote(channel).await.ok();

    Ok(count)
}

/// Remove the user from every DJ channel they joined, announcing each removal
///
/// Used when their websocket session closes; channels which no longer exist (or
/// are no longer DJ channels) are cleaned up silently.
pub async fn remove_user_from_dj_channels(db: &Database, user: &str) -> revolt_result::Result<Vec<String>> {
    let channels = dj::get_user_channels(user).await?;

    for channel_id in &channels {
        match db.fetch_channel(channel_id).await {
            Ok(channel) if channel.dj().is_some() => {
                remove_member_and_announce(&channel, user).await?;
            }
            _ => {
                dj::remove_member(channel_id, user).await?;
            }
        }
    }

    Ok(channels)
}

/// Remove the user from every DJ channel of `server` which they had joined
///
/// Used when they leave, are kicked or are banned: a departed member must not
/// keep a slot (or a vote) in a channel they can no longer see. Channels which
/// belong to other servers are left alone.
pub async fn remove_user_from_server_dj_channels(
    db: &Database,
    server: &crate::Server,
    user: &str,
) -> revolt_result::Result<Vec<String>> {
    let channels = dj::get_user_channels(user)
        .await?
        .into_iter()
        .filter(|channel| server.channels.contains(channel))
        .collect::<Vec<_>>();

    for channel_id in &channels {
        match db.fetch_channel(channel_id).await {
            Ok(channel) if channel.dj().is_some() => {
                remove_member_and_announce(&channel, user).await?;
            }
            _ => {
                dj::remove_member(channel_id, user).await?;
            }
        }
    }

    Ok(channels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vote_refresh_is_a_no_op_without_dj_settings() {
        // Pure assertion of the early return: a channel without DJ settings
        // must not publish anything, which is what the `let Some(..) else`
        // above guarantees. Keeps the guard from being "simplified" away.
        let channel = Channel::SavedMessages {
            id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
            user: "01ARZ3NDEKTSV4RRFFQ69G5FAW".to_string(),
        };

        assert!(channel.dj().is_none());
    }

    #[test]
    fn member_left_event_carries_the_new_count() {
        let event = EventV1::DjMemberLeft {
            channel: "chan".to_string(),
            user: "user".to_string(),
            count: 0,
        };

        let value = serde_json::to_value(&event).expect("serialise");

        assert_eq!(value["type"], "DjMemberLeft");
        assert_eq!(value["channel"], "chan");
        assert_eq!(value["user"], "user");
        assert_eq!(value["count"], 0);
    }
}
