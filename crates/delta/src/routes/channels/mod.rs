use revolt_result::{create_error, Result};
use revolt_rocket_okapi::revolt_okapi::openapi3::OpenApi;
use rocket::Route;

mod channel_ack;
mod channel_delete;
mod channel_edit;
mod channel_fetch;
mod dj;
mod group_add_member;
mod group_create;
mod group_remove_member;
mod invite_create;
mod members_fetch;
mod message_bulk_delete;
mod message_clear_reactions;
mod message_delete;
mod message_edit;
mod message_fetch;
mod message_pin;
mod message_query;
mod message_react;
mod message_search;
mod message_send;
mod message_unpin;
mod message_unreact;
mod permissions_set;
mod permissions_set_default;
mod thread_create;
mod thread_list;
mod thread_members;
#[cfg(test)]
mod thread_tests;
mod voice_join;
mod voice_stop_ring;
mod webhook_create;
mod webhook_fetch_all;

pub fn routes() -> (Vec<Route>, OpenApi) {
    openapi_get_routes_spec![
        channel_ack::ack,
        channel_fetch::fetch,
        members_fetch::fetch_members,
        channel_delete::delete,
        channel_edit::edit,
        invite_create::create_invite,
        message_send::message_send,
        message_query::query,
        message_search::search,
        message_pin::message_pin,
        message_fetch::fetch,
        message_edit::edit,
        message_bulk_delete::bulk_delete_messages,
        message_delete::delete,
        message_unpin::message_unpin,
        group_create::create_group,
        group_add_member::add_member,
        group_remove_member::remove_member,
        voice_join::call,
        voice_stop_ring::stop_ring,
        permissions_set::set_role_permissions,
        permissions_set_default::set_default_channel_permissions,
        message_react::react_message,
        message_unreact::unreact_message,
        message_clear_reactions::clear_reactions,
        webhook_create::create_webhook,
        webhook_fetch_all::fetch_webhooks,
        thread_create::create_thread_from_message,
        thread_create::create_thread,
        thread_list::list_active_threads,
        thread_list::list_archived_threads,
        thread_list::search_threads,
        thread_members::fetch_thread_members,
        thread_members::add_thread_member,
        thread_members::remove_thread_member,
        thread_members::edit_thread_member,
        dj::join::join,
        dj::leave::leave,
        dj::state::fetch,
        dj::queue::queue,
        dj::skip::skip,
        dj::settings::edit,
    ]
}

/// Reject channels which are read-only for message-like content.
///
/// DJ channels do not accept messages, reactions, pins or webhooks: members
/// join voice-style and upload tracks instead. Old clients which still render
/// them as text channels get a clear error rather than silently accumulating
/// messages no client will show.
pub(crate) fn resolve_postable_channel(channel: &revolt_database::Channel) -> Result<()> {
    if channel.dj().is_some() {
        return Err(create_error!(CannotSendMessageToDjChannel));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use revolt_database::{Channel, DjInformation};
    use revolt_result::ErrorType;

    use super::resolve_postable_channel;

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
    fn dj_channels_reject_posts() {
        let error = resolve_postable_channel(&text_channel(Some(DjInformation::default())))
            .expect_err("DJ channels are read-only for messages");

        assert!(matches!(
            error.error_type,
            ErrorType::CannotSendMessageToDjChannel
        ));
    }

    #[test]
    fn text_channels_accept_posts() {
        resolve_postable_channel(&text_channel(None)).expect("text channels accept messages");
    }

    #[test]
    fn disabled_dj_channels_still_reject_posts() {
        // `enabled: false` blocks joins, but the channel is still a DJ channel
        // and must not silently become a normal text channel again.
        let error = resolve_postable_channel(&text_channel(Some(DjInformation {
            enabled: false,
            ..Default::default()
        })))
        .expect_err("a disabled DJ channel is still read-only");

        assert!(matches!(
            error.error_type,
            ErrorType::CannotSendMessageToDjChannel
        ));
    }

    #[test]
    fn non_text_channels_are_not_affected() {
        use revolt_database::Channel;

        let channels = [
            Channel::SavedMessages {
                id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
                user: "01ARZ3NDEKTSV4RRFFQ69G5FAW".to_string(),
            },
            Channel::DirectMessage {
                id: "01ARZ3NDEKTSV4RRFFQ69G5FAX".to_string(),
                active: true,
                recipients: vec![
                    "01ARZ3NDEKTSV4RRFFQ69G5FAW".to_string(),
                    "01ARZ3NDEKTSV4RRFFQ69G5FAY".to_string(),
                ],
                last_message_id: None,
            },
        ];

        for channel in channels {
            resolve_postable_channel(&channel)
                .expect("only text channels can carry DJ state, so others are unaffected");
        }
    }

    #[test]
    fn dj_state_is_only_read_from_text_channels() {
        use revolt_database::{Channel, VoiceInformation};

        // A voice channel with a `voice` field set is not a DJ channel
        let channel = Channel::TextChannel {
            id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
            server: "01ARZ3NDEKTSV4RRFFQ69G5FAW".to_string(),
            name: "voice".to_string(),
            description: None,
            icon: None,
            last_message_id: None,
            default_permissions: None,
            role_permissions: HashMap::new(),
            nsfw: false,
            voice: Some(VoiceInformation { max_users: Some(4) }),
            dj: None,
            slowmode: None,
            default_auto_archive_minutes: None,
        };

        assert!(channel.dj().is_none());
        assert!(!channel.is_dj_channel());
        resolve_postable_channel(&channel).expect("voice channels accept messages");
    }
}
