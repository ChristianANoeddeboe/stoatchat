use revolt_models::v0::ThreadNotify;
use revolt_result::Result;

use crate::{ReferenceDb, ThreadMember, ThreadMemberCompositeKey};

use super::AbstractThreadMembers;

fn key(thread_id: &str, user_id: &str) -> ThreadMemberCompositeKey {
    ThreadMemberCompositeKey {
        thread: thread_id.to_string(),
        user: user_id.to_string(),
    }
}

#[async_trait]
impl AbstractThreadMembers for ReferenceDb {
    /// Insert a thread member, returns false if they were already a member
    async fn insert_thread_member(&self, member: &ThreadMember) -> Result<bool> {
        let mut members = self.thread_members.lock().await;
        if members.contains_key(&member.id) {
            Ok(false)
        } else {
            members.insert(member.id.clone(), member.clone());
            Ok(true)
        }
    }

    /// Fetch a thread member
    async fn fetch_thread_member(&self, thread_id: &str, user_id: &str) -> Result<ThreadMember> {
        let members = self.thread_members.lock().await;
        members
            .get(&key(thread_id, user_id))
            .cloned()
            .ok_or_else(|| create_error!(NotFound))
    }

    /// Fetch all members of a thread
    async fn fetch_thread_members(&self, thread_id: &str) -> Result<Vec<ThreadMember>> {
        let members = self.thread_members.lock().await;
        Ok(members
            .values()
            .filter(|member| member.id.thread == thread_id)
            .cloned()
            .collect())
    }

    /// Fetch all thread memberships of a user
    async fn fetch_thread_memberships(&self, user_id: &str) -> Result<Vec<ThreadMember>> {
        let members = self.thread_members.lock().await;
        Ok(members
            .values()
            .filter(|member| member.id.user == user_id)
            .cloned()
            .collect())
    }

    /// Change the notification setting of a thread member
    async fn update_thread_member_notify(
        &self,
        thread_id: &str,
        user_id: &str,
        notify: &ThreadNotify,
    ) -> Result<()> {
        let mut members = self.thread_members.lock().await;
        if let Some(member) = members.get_mut(&key(thread_id, user_id)) {
            member.notify = notify.clone();
            Ok(())
        } else {
            Err(create_error!(NotFound))
        }
    }

    /// Delete a thread member, returns false if they were not a member
    async fn delete_thread_member(&self, thread_id: &str, user_id: &str) -> Result<bool> {
        let mut members = self.thread_members.lock().await;
        Ok(members.remove(&key(thread_id, user_id)).is_some())
    }

    /// Delete all members of the given threads
    async fn delete_thread_members(&self, thread_ids: &[String]) -> Result<()> {
        let mut members = self.thread_members.lock().await;
        members.retain(|id, _| !thread_ids.contains(&id.thread));
        Ok(())
    }
}
