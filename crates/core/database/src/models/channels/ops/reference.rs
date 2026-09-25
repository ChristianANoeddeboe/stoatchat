use std::collections::hash_map::Entry;

use super::AbstractChannels;
use crate::util::ChunkedDatabaseGenerator;
use crate::ReferenceDb;
use crate::{Channel, FieldsChannel, PartialChannel};
use iso8601_timestamp::Timestamp;
use revolt_models::v0::ForumSortOrder;
use revolt_permissions::OverrideField;
use revolt_result::Result;

#[async_trait]
impl AbstractChannels for ReferenceDb {
    /// Insert a new channel in the database
    async fn insert_channel(&self, channel: &Channel) -> Result<()> {
        let mut channels = self.channels.lock().await;
        if let Entry::Vacant(entry) = channels.entry(channel.id().to_string()) {
            entry.insert(channel.clone());
            Ok(())
        } else {
            Err(create_database_error!("insert", "channel"))
        }
    }

    /// Fetch a channel from the database
    async fn fetch_channel(&self, channel_id: &str) -> Result<Channel> {
        let channels = self.channels.lock().await;
        channels
            .get(channel_id)
            .cloned()
            .ok_or_else(|| create_error!(NotFound))
    }

    /// Fetch all channels from the database
    async fn fetch_channels<'a>(&self, ids: &'a [String]) -> Result<Vec<Channel>> {
        let channels = self.channels.lock().await;
        ids.iter()
            .map(|id| {
                channels
                    .get(id)
                    .cloned()
                    .ok_or_else(|| create_error!(NotFound))
            })
            .collect()
    }

    /// Fetch all direct messages for a user
    async fn find_direct_messages(&self, user_id: &str) -> Result<Vec<Channel>> {
        let channels = self.channels.lock().await;
        Ok(channels
            .values()
            .filter(|channel| channel.contains_user(user_id))
            .cloned()
            .collect())
    }

    // Fetch all group dms for a user
    async fn find_group_message_channels(
        &self,
        user_id: &str,
    ) -> Result<ChunkedDatabaseGenerator<Channel>> {
        let channels = self.channels.lock().await;
        let groups = channels
            .values()
            .filter(|channel| match channel {
                Channel::Group { recipients, .. } => {
                    recipients.iter().any(|recipient| recipient == user_id)
                }
                _ => false,
            })
            .cloned()
            .collect();

        Ok(ChunkedDatabaseGenerator::new_reference(groups))
    }

    // Fetch saved messages channel
    async fn find_saved_messages_channel(&self, user_id: &str) -> Result<Channel> {
        let channels = self.channels.lock().await;
        channels
            .get(user_id)
            .cloned()
            .ok_or_else(|| create_database_error!("fetch", "channel"))
    }

    // Fetch direct message channel (DM or Saved Messages)
    async fn find_direct_message_channel(&self, user_a: &str, user_b: &str) -> Result<Channel> {
        let channels = self.channels.lock().await;
        for (_, data) in channels.iter() {
            if data.contains_user(user_a) && data.contains_user(user_b) {
                return Ok(data.to_owned());
            }
        }
        Err(create_error!(NotFound))
    }
    /// Insert a user to a group
    async fn add_user_to_group(&self, channel_id: &str, user_id: &str) -> Result<()> {
        let mut channels = self.channels.lock().await;

        if let Some(Channel::Group { recipients, .. }) = channels.get_mut(channel_id) {
            recipients.push(String::from(user_id));
            Ok(())
        } else {
            Err(create_error!(InvalidOperation))
        }
    }
    /// Insert channel role permissions
    async fn set_channel_role_permission(
        &self,
        channel_id: &str,
        role_id: &str,
        permissions: OverrideField,
    ) -> Result<()> {
        let mut channels = self.channels.lock().await;

        if let Some(mut channel) = channels.get_mut(channel_id) {
            match &mut channel {
                Channel::TextChannel {
                    role_permissions, ..
                } => {
                    if role_permissions.get(role_id).is_some() {
                        role_permissions.remove(role_id);
                        role_permissions.insert(String::from(role_id), permissions);

                        Ok(())
                    } else {
                        Err(create_error!(NotFound))
                    }
                }
                _ => Err(create_error!(NotFound)),
            }
        } else {
            Err(create_error!(NotFound))
        }
    }

    // Update channel
    async fn update_channel(
        &self,
        id: &str,
        channel: &PartialChannel,
        remove: Vec<FieldsChannel>,
    ) -> Result<()> {
        let mut channels = self.channels.lock().await;
        if let Some(channel_data) = channels.get_mut(id) {
            channel_data.apply_options(channel.to_owned());
            channel_data.remove_fields(remove);
            Ok(())
        } else {
            Err(create_error!(NotFound))
        }
    }

    // Remove a user from a group
    async fn remove_user_from_group(&self, channel: &str, user: &str) -> Result<()> {
        let mut channels = self.channels.lock().await;
        if let Some(Channel::Group { recipients, .. }) = channels.get_mut(channel) {
            if let Some(index) = recipients.iter().position(|recipient| recipient == user) {
                recipients.remove(index);
                return Ok(());
            } else {
                return Err(create_error!(NotFound));
            }
        }
        Err(create_error!(NotFound))
    }

    // Remove a user from all specified groups
    async fn remove_user_from_groups(&self, channel_ids: Vec<String>, user_id: &str) -> Result<()> {
        let mut channels = self.channels.lock().await;

        for channel_id in channel_ids {
            if let Some(Channel::Group { recipients, .. }) = channels.get_mut(&channel_id) {
                recipients.retain(|recipient| recipient != user_id);
            }
        }

        Ok(())
    }

    // Delete a channel
    async fn delete_channel(&self, channel: &Channel) -> Result<()> {
        let mut channels = self.channels.lock().await;
        if channels.remove(channel.id()).is_some() {
            Ok(())
        } else {
            Err(create_error!(NotFound))
        }
    }

    /// Fetch all unarchived threads in the given servers
    async fn fetch_active_threads(&self, server_ids: &[String]) -> Result<Vec<Channel>> {
        let channels = self.channels.lock().await;
        Ok(channels
            .values()
            .filter(|channel| {
                matches!(channel, Channel::Thread { server, archived: false, .. } if server_ids.contains(server))
            })
            .cloned()
            .collect())
    }

    /// Fetch all unarchived threads
    async fn fetch_all_active_threads(&self) -> Result<Vec<Channel>> {
        let channels = self.channels.lock().await;
        Ok(channels
            .values()
            .filter(|channel| {
                matches!(
                    channel,
                    Channel::Thread {
                        archived: false,
                        ..
                    }
                )
            })
            .cloned()
            .collect())
    }

    /// Fetch all threads of a channel, optionally filtered by archived state
    async fn fetch_threads(&self, parent_id: &str, archived: Option<bool>) -> Result<Vec<Channel>> {
        let channels = self.channels.lock().await;
        Ok(channels
            .values()
            .filter(|channel| {
                matches!(channel, Channel::Thread { parent, archived: a, .. }
                    if parent == parent_id && archived.is_none_or(|archived| archived == *a))
            })
            .cloned()
            .collect())
    }

    /// Fetch archived threads of a channel, most recently archived first
    async fn fetch_archived_threads(
        &self,
        parent_id: &str,
        private: bool,
        before: Option<Timestamp>,
        limit: i64,
    ) -> Result<Vec<Channel>> {
        let channels = self.channels.lock().await;
        let mut threads: Vec<(Option<Timestamp>, Channel)> = channels
            .values()
            .filter_map(|channel| match channel {
                Channel::Thread {
                    parent,
                    archived: true,
                    private: p,
                    archived_at,
                    ..
                } if parent == parent_id
                    && *p == private
                    && before.is_none_or(|before| archived_at.is_some_and(|at| at < before)) =>
                {
                    Some((*archived_at, channel.clone()))
                }
                _ => None,
            })
            .collect();

        threads.sort_by(|a, b| b.0.cmp(&a.0));
        Ok(threads
            .into_iter()
            .take(limit as usize)
            .map(|(_, channel)| channel)
            .collect())
    }

    /// Search the posts of a forum, pinned posts first
    async fn search_threads(
        &self,
        parent_id: &str,
        tags: &[String],
        sort: &ForumSortOrder,
        archived: bool,
        before: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Channel>> {
        let channels = self.channels.lock().await;
        let mut threads: Vec<(bool, String, Channel)> = channels
            .values()
            .filter_map(|channel| match channel {
                Channel::Thread {
                    id,
                    parent,
                    archived: a,
                    applied_tags,
                    pinned,
                    last_message_id,
                    ..
                } if parent == parent_id
                    && *a == archived
                    && tags.iter().all(|tag| applied_tags.contains(tag)) =>
                {
                    let key = match sort {
                        ForumSortOrder::LatestActivity => {
                            last_message_id.clone().unwrap_or_default()
                        }
                        ForumSortOrder::CreationDate => id.clone(),
                    };

                    if before.is_some_and(|before| *pinned || key.as_str() >= before) {
                        None
                    } else {
                        Some((*pinned, key, channel.clone()))
                    }
                }
                _ => None,
            })
            .collect();

        threads.sort_by(|a, b| (b.0, &b.1).cmp(&(a.0, &a.1)));
        Ok(threads
            .into_iter()
            .take(limit as usize)
            .map(|(_, _, channel)| channel)
            .collect())
    }

    /// Atomically add to the message and member counts of a thread, returns the updated thread
    async fn add_to_thread_counts(
        &self,
        thread_id: &str,
        messages: i32,
        members: i32,
    ) -> Result<Channel> {
        let mut channels = self.channels.lock().await;
        match channels.get_mut(thread_id) {
            Some(channel @ Channel::Thread { .. }) => {
                if let Channel::Thread {
                    message_count,
                    member_count,
                    ..
                } = channel
                {
                    *message_count = message_count.saturating_add_signed(messages);
                    *member_count = member_count.saturating_add_signed(members);
                }

                Ok(channel.clone())
            }
            _ => Err(create_error!(NotFound)),
        }
    }
}
