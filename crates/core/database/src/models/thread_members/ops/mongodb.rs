use bson::Document;
use futures::StreamExt;
use revolt_models::v0::ThreadNotify;
use revolt_result::Result;

use crate::{MongoDb, ThreadMember};

use super::AbstractThreadMembers;

static COL: &str = "thread_members";

#[async_trait]
impl AbstractThreadMembers for MongoDb {
    /// Insert a thread member, returns false if they were already a member
    async fn insert_thread_member(&self, member: &ThreadMember) -> Result<bool> {
        let result = self
            .col::<Document>(COL)
            .update_one(
                // Match the whole id so the upsert takes it from the filter
                doc! {
                    "_id": {
                        "thread": &member.id.thread,
                        "user": &member.id.user,
                    }
                },
                doc! {
                    "$setOnInsert": {
                        "joined_at": bson::to_bson(&member.joined_at)
                            .map_err(|_| create_database_error!("to_bson", COL))?,
                        "notify": bson::to_bson(&member.notify)
                            .map_err(|_| create_database_error!("to_bson", COL))?,
                    }
                },
            )
            .upsert(true)
            .await
            .map_err(|_| create_database_error!("update_one", COL))?;

        Ok(result.upserted_id.is_some())
    }

    /// Fetch a thread member
    async fn fetch_thread_member(&self, thread_id: &str, user_id: &str) -> Result<ThreadMember> {
        query!(
            self,
            find_one,
            COL,
            doc! {
                "_id.thread": thread_id,
                "_id.user": user_id
            }
        )?
        .ok_or_else(|| create_error!(NotFound))
    }

    /// Fetch all members of a thread
    async fn fetch_thread_members(&self, thread_id: &str) -> Result<Vec<ThreadMember>> {
        query!(
            self,
            find,
            COL,
            doc! {
                "_id.thread": thread_id
            }
        )
    }

    /// Fetch all thread memberships of a user
    async fn fetch_thread_memberships(&self, user_id: &str) -> Result<Vec<ThreadMember>> {
        Ok(self
            .col::<ThreadMember>(COL)
            .find(doc! {
                "_id.user": user_id
            })
            .await
            .map_err(|_| create_database_error!("find", COL))?
            .filter_map(|s| async { s.ok() })
            .collect()
            .await)
    }

    /// Change the notification setting of a thread member
    async fn update_thread_member_notify(
        &self,
        thread_id: &str,
        user_id: &str,
        notify: &ThreadNotify,
    ) -> Result<()> {
        self.col::<Document>(COL)
            .update_one(
                doc! {
                    "_id.thread": thread_id,
                    "_id.user": user_id,
                },
                doc! {
                    "$set": {
                        "notify": bson::to_bson(notify)
                            .map_err(|_| create_database_error!("to_bson", COL))?
                    }
                },
            )
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("update_one", COL))
    }

    /// Delete a thread member, returns false if they were not a member
    async fn delete_thread_member(&self, thread_id: &str, user_id: &str) -> Result<bool> {
        self.col::<Document>(COL)
            .delete_one(doc! {
                "_id.thread": thread_id,
                "_id.user": user_id,
            })
            .await
            .map(|result| result.deleted_count > 0)
            .map_err(|_| create_database_error!("delete_one", COL))
    }

    /// Delete all members of the given threads
    async fn delete_thread_members(&self, thread_ids: &[String]) -> Result<()> {
        self.col::<Document>(COL)
            .delete_many(doc! {
                "_id.thread": {
                    "$in": thread_ids
                }
            })
            .await
            .map(|_| ())
            .map_err(|_| create_database_error!("delete_many", COL))
    }
}
