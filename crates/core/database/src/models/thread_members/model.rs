use iso8601_timestamp::Timestamp;
use revolt_models::v0::ThreadNotify;

auto_derived!(
    /// Membership of a thread
    pub struct ThreadMember {
        /// Composite key of thread and user id
        #[serde(rename = "_id")]
        pub id: ThreadMemberCompositeKey,
        /// Time at which this user joined the thread
        pub joined_at: Timestamp,
        /// Notification setting for this thread
        #[serde(default)]
        pub notify: ThreadNotify,
    }

    /// Composite primary key consisting of thread and user id
    #[derive(Hash, Default)]
    pub struct ThreadMemberCompositeKey {
        /// Thread Id
        pub thread: String,
        /// User Id
        pub user: String,
    }
);

impl ThreadMember {
    /// Create a new membership starting now
    pub fn new(thread: &str, user: &str) -> ThreadMember {
        ThreadMember {
            id: ThreadMemberCompositeKey {
                thread: thread.to_string(),
                user: user.to_string(),
            },
            joined_at: Timestamp::now_utc(),
            notify: ThreadNotify::Default,
        }
    }
}
