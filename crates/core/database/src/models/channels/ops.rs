use crate::{
    revolt_result::Result, util::ChunkedDatabaseGenerator, Channel, FieldsChannel, PartialChannel,
};
use iso8601_timestamp::Timestamp;
use revolt_models::v0::ForumSortOrder;
use revolt_permissions::OverrideField;

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractChannels: Sync + Send {
    /// Insert a new channel in the database
    async fn insert_channel(&self, channel: &Channel) -> Result<()>;

    /// Fetch a channel from the database
    async fn fetch_channel(&self, channel_id: &str) -> Result<Channel>;

    /// Fetch all channels from the database
    async fn fetch_channels<'a>(&self, ids: &'a [String]) -> Result<Vec<Channel>>;

    /// Fetch all direct messages for a user
    async fn find_direct_messages(&self, user_id: &str) -> Result<Vec<Channel>>;

    // Fetch all group dms for a user
    async fn find_group_message_channels(
        &self,
        user_id: &str,
    ) -> Result<ChunkedDatabaseGenerator<Channel>>;

    // Fetch saved messages channel
    async fn find_saved_messages_channel(&self, user_id: &str) -> Result<Channel>;

    // Fetch direct message channel (DM or Saved Messages)
    async fn find_direct_message_channel(&self, user_a: &str, user_b: &str) -> Result<Channel>;

    /// Insert a user to a group
    async fn add_user_to_group(&self, channel_id: &str, user_id: &str) -> Result<()>;

    /// Insert channel role permissions
    async fn set_channel_role_permission(
        &self,
        channel_id: &str,
        role_id: &str,
        permissions: OverrideField,
    ) -> Result<()>;

    // Update channel
    async fn update_channel(
        &self,
        id: &str,
        channel_id: &PartialChannel,
        remove: Vec<FieldsChannel>,
    ) -> Result<()>;

    // Remove a user from a group
    async fn remove_user_from_group(&self, channel_id: &str, user_id: &str) -> Result<()>;

    // Remove a user from all specified groups
    async fn remove_user_from_groups(&self, channel_ids: Vec<String>, user_id: &str) -> Result<()>;

    // Delete a channel
    async fn delete_channel(&self, channel_id: &Channel) -> Result<()>;

    /// Fetch all unarchived threads in the given servers
    async fn fetch_active_threads(&self, server_ids: &[String]) -> Result<Vec<Channel>>;

    /// Fetch all unarchived threads
    async fn fetch_all_active_threads(&self) -> Result<Vec<Channel>>;

    /// Fetch all threads of a channel, optionally filtered by archived state
    async fn fetch_threads(&self, parent_id: &str, archived: Option<bool>) -> Result<Vec<Channel>>;

    /// Fetch archived threads of a channel, most recently archived first
    async fn fetch_archived_threads(
        &self,
        parent_id: &str,
        private: bool,
        before: Option<Timestamp>,
        limit: i64,
    ) -> Result<Vec<Channel>>;

    /// Search the posts of a forum, pinned posts first
    ///
    /// `before` is the sort key (last message id or post id) of the last unpinned post seen,
    /// and excludes pinned posts.
    async fn search_threads(
        &self,
        parent_id: &str,
        tags: &[String],
        sort: &ForumSortOrder,
        archived: bool,
        before: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Channel>>;

    /// Atomically add to the message and member counts of a thread, returns the updated thread
    async fn add_to_thread_counts(
        &self,
        thread_id: &str,
        messages: i32,
        members: i32,
    ) -> Result<Channel>;
}
