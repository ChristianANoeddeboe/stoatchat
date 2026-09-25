use revolt_models::v0::ThreadNotify;
use revolt_result::Result;

use crate::ThreadMember;

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractThreadMembers: Sync + Send {
    /// Insert a thread member, returns false if they were already a member
    async fn insert_thread_member(&self, member: &ThreadMember) -> Result<bool>;

    /// Fetch a thread member
    async fn fetch_thread_member(&self, thread_id: &str, user_id: &str) -> Result<ThreadMember>;

    /// Fetch all members of a thread
    async fn fetch_thread_members(&self, thread_id: &str) -> Result<Vec<ThreadMember>>;

    /// Fetch all thread memberships of a user
    async fn fetch_thread_memberships(&self, user_id: &str) -> Result<Vec<ThreadMember>>;

    /// Change the notification setting of a thread member
    async fn update_thread_member_notify(
        &self,
        thread_id: &str,
        user_id: &str,
        notify: &ThreadNotify,
    ) -> Result<()>;

    /// Delete a thread member, returns false if they were not a member
    async fn delete_thread_member(&self, thread_id: &str, user_id: &str) -> Result<bool>;

    /// Delete all members of the given threads
    async fn delete_thread_members(&self, thread_ids: &[String]) -> Result<()>;
}
