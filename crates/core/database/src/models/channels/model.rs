#![allow(deprecated)]
use std::{borrow::Cow, collections::HashMap};

use iso8601_timestamp::Timestamp;
use redis_kiss::get_connection;
use revolt_config::config;
use revolt_models::v0::{self, ForumLayout, ForumSortOrder, ForumTag, MessageAuthor, MessageFlags};
use revolt_permissions::OverrideField;
use revolt_result::Result;
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::{
    events::client::EventV1, Database, File, MessageFlagsValue, PartialMessage, PartialServer,
    Server, SystemMessage, ThreadMember, User, AMQP,
};

#[cfg(feature = "mongodb")]
use crate::IntoDocumentPath;

auto_derived!(
    #[serde(tag = "channel_type")]
    pub enum Channel {
        /// Personal "Saved Notes" channel which allows users to save messages
        SavedMessages {
            /// Unique Id
            #[serde(rename = "_id")]
            id: String,
            /// Id of the user this channel belongs to
            user: String,
        },
        /// Direct message channel between two users
        DirectMessage {
            /// Unique Id
            #[serde(rename = "_id")]
            id: String,

            /// Whether this direct message channel is currently open on both sides
            active: bool,
            /// 2-tuple of user ids participating in direct message
            recipients: Vec<String>,
            /// Id of the last message sent in this channel
            #[serde(skip_serializing_if = "Option::is_none")]
            last_message_id: Option<String>,
        },
        /// Group channel between 1 or more participants
        Group {
            /// Unique Id
            #[serde(rename = "_id")]
            id: String,

            /// Display name of the channel
            name: String,
            /// User id of the owner of the group
            owner: String,
            /// Channel description
            #[serde(skip_serializing_if = "Option::is_none")]
            description: Option<String>,
            /// Array of user ids participating in channel
            recipients: Vec<String>,

            /// Custom icon attachment
            #[serde(skip_serializing_if = "Option::is_none")]
            icon: Option<File>,
            /// Id of the last message sent in this channel
            #[serde(skip_serializing_if = "Option::is_none")]
            last_message_id: Option<String>,

            /// Permissions assigned to members of this group
            /// (does not apply to the owner of the group)
            #[serde(skip_serializing_if = "Option::is_none")]
            permissions: Option<i64>,

            /// Whether this group is marked as not safe for work
            #[serde(skip_serializing_if = "crate::if_false", default)]
            nsfw: bool,
        },
        /// Text channel belonging to a server
        TextChannel {
            /// Unique Id
            #[serde(rename = "_id")]
            id: String,
            /// Id of the server this channel belongs to
            server: String,

            /// Display name of the channel
            name: String,
            /// Channel description
            #[serde(skip_serializing_if = "Option::is_none")]
            description: Option<String>,

            /// Custom icon attachment
            #[serde(skip_serializing_if = "Option::is_none")]
            icon: Option<File>,
            /// Id of the last message sent in this channel
            #[serde(skip_serializing_if = "Option::is_none")]
            last_message_id: Option<String>,

            /// Default permissions assigned to users in this channel
            #[serde(skip_serializing_if = "Option::is_none")]
            default_permissions: Option<OverrideField>,
            /// Permissions assigned based on role to this channel
            #[serde(
                default = "HashMap::<String, OverrideField>::new",
                skip_serializing_if = "HashMap::<String, OverrideField>::is_empty"
            )]
            role_permissions: HashMap<String, OverrideField>,

            /// Whether this channel is marked as not safe for work
            #[serde(skip_serializing_if = "crate::if_false", default)]
            nsfw: bool,

            /// Voice Information for when this channel is also a voice channel
            #[serde(skip_serializing_if = "Option::is_none")]
            voice: Option<VoiceInformation>,

            /// The channel's slowmode delay in seconds
            #[serde(skip_serializing_if = "Option::is_none")]
            slowmode: Option<u64>,

            /// Default auto archive duration (in minutes) for new threads
            #[serde(skip_serializing_if = "Option::is_none")]
            default_auto_archive_minutes: Option<u32>,
        },
        /// Forum channel belonging to a server, which holds posts (threads)
        ForumChannel {
            /// Unique Id
            #[serde(rename = "_id")]
            id: String,
            /// Id of the server this channel belongs to
            server: String,

            /// Display name of the channel
            name: String,
            /// Post guidelines
            #[serde(skip_serializing_if = "Option::is_none")]
            description: Option<String>,

            /// Custom icon attachment
            #[serde(skip_serializing_if = "Option::is_none")]
            icon: Option<File>,
            /// Id of the most recently created post
            #[serde(skip_serializing_if = "Option::is_none")]
            last_message_id: Option<String>,

            /// Default permissions assigned to users in this channel
            #[serde(skip_serializing_if = "Option::is_none")]
            default_permissions: Option<OverrideField>,
            /// Permissions assigned based on role to this channel
            #[serde(
                default = "HashMap::<String, OverrideField>::new",
                skip_serializing_if = "HashMap::<String, OverrideField>::is_empty"
            )]
            role_permissions: HashMap<String, OverrideField>,

            /// Whether this channel is marked as not safe for work
            #[serde(skip_serializing_if = "crate::if_false", default)]
            nsfw: bool,

            /// Delay in seconds between creating posts
            #[serde(skip_serializing_if = "Option::is_none")]
            slowmode: Option<u64>,

            /// Tags that can be applied to posts
            #[serde(default, skip_serializing_if = "Vec::is_empty")]
            available_tags: Vec<ForumTag>,
            /// Whether posts must have at least one tag
            #[serde(skip_serializing_if = "crate::if_false", default)]
            require_tag: bool,
            /// Emoji shown as the reaction button on posts
            #[serde(skip_serializing_if = "Option::is_none")]
            default_reaction_emoji: Option<String>,
            /// Default order of posts
            #[serde(default)]
            default_sort_order: ForumSortOrder,
            /// Default layout of posts
            #[serde(default)]
            default_layout: ForumLayout,
            /// Slowmode applied to new posts
            #[serde(skip_serializing_if = "Option::is_none")]
            default_thread_slowmode: Option<u64>,
            /// Default auto archive duration (in minutes) for new posts
            #[serde(skip_serializing_if = "Option::is_none")]
            default_auto_archive_minutes: Option<u32>,
        },
        /// Thread in a text channel, or a post in a forum channel
        Thread {
            /// Unique Id (same as the starter message)
            #[serde(rename = "_id")]
            id: String,
            /// Id of the server this thread belongs to
            server: String,
            /// Id of the channel this thread belongs to
            parent: String,
            /// User id of the thread creator
            owner: String,

            /// Display name of the thread
            name: String,

            /// Whether only members (and ManageThreads) can see the thread
            #[serde(skip_serializing_if = "crate::if_false", default)]
            private: bool,
            /// Whether non-moderators can add members to a private thread
            #[serde(skip_serializing_if = "crate::if_false", default)]
            invitable: bool,

            /// Ids of the forum tags applied to this post
            #[serde(default, skip_serializing_if = "Vec::is_empty")]
            applied_tags: Vec<String>,
            /// Whether this post is pinned to the top of the forum
            #[serde(skip_serializing_if = "crate::if_false", default)]
            pinned: bool,

            /// Whether the thread is archived
            #[serde(default)]
            archived: bool,
            /// Whether the thread is locked
            #[serde(skip_serializing_if = "crate::if_false", default)]
            locked: bool,
            /// Minutes of inactivity after which the thread is archived
            auto_archive_minutes: u32,
            /// When the thread was last archived or unarchived
            #[serde(skip_serializing_if = "Option::is_none")]
            archived_at: Option<Timestamp>,

            /// The thread's slowmode delay in seconds
            #[serde(skip_serializing_if = "Option::is_none")]
            slowmode: Option<u64>,

            /// Id of the last message sent in this thread
            #[serde(skip_serializing_if = "Option::is_none")]
            last_message_id: Option<String>,
            /// Number of messages in the thread
            #[serde(default)]
            message_count: u32,
            /// Number of members in the thread
            #[serde(default)]
            member_count: u32,
        },
    }

    #[derive(Default)]
    pub struct VoiceInformation {
        /// Maximium amount of users allowed in the voice channel at once
        #[serde(skip_serializing_if = "Option::is_none")]
        pub max_users: Option<usize>,
    }
);

auto_derived!(
    #[derive(Default)]
    pub struct PartialChannel {
        #[serde(skip_serializing_if = "Option::is_none")]
        pub name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub owner: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub icon: Option<File>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub nsfw: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub active: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub permissions: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub role_permissions: Option<HashMap<String, OverrideField>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub default_permissions: Option<OverrideField>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub last_message_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub voice: Option<VoiceInformation>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub slowmode: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub available_tags: Option<Vec<ForumTag>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub require_tag: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub default_reaction_emoji: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub default_sort_order: Option<ForumSortOrder>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub default_layout: Option<ForumLayout>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub default_thread_slowmode: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub default_auto_archive_minutes: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub invitable: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub applied_tags: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub pinned: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub archived: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub locked: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub auto_archive_minutes: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub archived_at: Option<Timestamp>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub message_count: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub member_count: Option<u32>,
    }

    /// Optional fields on channel object
    pub enum FieldsChannel {
        Description,
        Icon,
        DefaultPermissions,
        Voice,
        Slowmode,
        DefaultReactionEmoji,
        DefaultThreadSlowmode,
        DefaultAutoArchiveMinutes,
    }
);

/// Default auto archive duration of new threads (3 days)
pub const DEFAULT_AUTO_ARCHIVE_MINUTES: u32 = 4320;

/// Allowed auto archive durations of threads (1 hour, 1 day, 3 days, 1 week)
pub const AUTO_ARCHIVE_MINUTES: [u32; 4] = [60, 1440, 4320, 10080];

#[allow(clippy::disallowed_methods)]
impl Channel {
    /* /// Create a channel
    pub async fn create(&self, db: &Database) -> Result<()> {
        db.insert_channel(self).await?;

        let event = EventV1::ChannelCreate(self.clone().into());
        match self {
            Self::SavedMessages { user, .. } => event.private(user.clone()).await,
            Self::DirectMessage { recipients, .. } | Self::Group { recipients, .. } => {
                for recipient in recipients {
                    event.clone().private(recipient.clone()).await;
                }
            }
            Self::TextChannel { server, .. } | Self::VoiceChannel { server, .. } => {
                event.p(server.clone()).await;
            }
        }

        Ok(())
    }*/

    /// Create a new server channel
    pub async fn create_server_channel(
        db: &Database,
        server: &mut Server,
        data: v0::DataCreateServerChannel,
        update_server: bool,
    ) -> Result<Channel> {
        let config = config().await;
        if server.channels.len() > config.features.limits.global.server_channels {
            return Err(create_error!(TooManyChannels {
                max: config.features.limits.global.server_channels,
            }));
        };

        let id = ulid::Ulid::new().to_string();
        let channel = match data.channel_type {
            v0::LegacyServerChannelType::Text => Channel::TextChannel {
                id: id.clone(),
                server: server.id.to_owned(),
                name: data.name,
                description: data.description,
                icon: None,
                last_message_id: None,
                default_permissions: None,
                role_permissions: HashMap::new(),
                nsfw: data.nsfw.unwrap_or(false),
                voice: data.voice.map(|voice| voice.into()),
                slowmode: None,
                default_auto_archive_minutes: None,
            },
            v0::LegacyServerChannelType::Voice => Channel::TextChannel {
                id: id.clone(),
                server: server.id.to_owned(),
                name: data.name,
                description: data.description,
                icon: None,
                last_message_id: None,
                default_permissions: None,
                role_permissions: HashMap::new(),
                nsfw: data.nsfw.unwrap_or(false),
                voice: Some(data.voice.unwrap_or_default().into()),
                slowmode: None,
                default_auto_archive_minutes: None,
            },
            v0::LegacyServerChannelType::Forum => Channel::ForumChannel {
                id: id.clone(),
                server: server.id.to_owned(),
                name: data.name,
                description: data.description,
                icon: None,
                last_message_id: None,
                default_permissions: None,
                role_permissions: HashMap::new(),
                nsfw: data.nsfw.unwrap_or(false),
                slowmode: None,
                available_tags: vec![],
                require_tag: false,
                default_reaction_emoji: None,
                default_sort_order: ForumSortOrder::default(),
                default_layout: ForumLayout::default(),
                default_thread_slowmode: None,
                default_auto_archive_minutes: None,
            },
        };

        db.insert_channel(&channel).await?;

        if update_server {
            server
                .update(
                    db,
                    PartialServer {
                        channels: Some([server.channels.clone(), [id].into()].concat()),
                        ..Default::default()
                    },
                    vec![],
                )
                .await?;

            EventV1::ChannelCreate(channel.clone().into())
                .p(server.id.clone())
                .await;
        }

        Ok(channel)
    }

    /// Create a thread in a text or forum channel
    ///
    /// The thread id is the id of the message it was started from,
    /// the "thread created" system message or the forum post's first message.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_thread(
        db: &Database,
        parent: &Channel,
        owner: &str,
        id: String,
        name: String,
        private: bool,
        invitable: bool,
        auto_archive_minutes: Option<u32>,
        slowmode: Option<u64>,
        applied_tags: Vec<String>,
    ) -> Result<Channel> {
        let (server, slowmode) = match parent {
            Channel::TextChannel { server, .. } => (server, slowmode),
            Channel::ForumChannel {
                server,
                default_thread_slowmode,
                ..
            } => (server, slowmode.or(*default_thread_slowmode)),
            _ => return Err(create_error!(InvalidOperation)),
        };

        // A forum post's first message has the post's id
        let is_post = matches!(parent, Channel::ForumChannel { .. });

        let thread = Channel::Thread {
            id: id.clone(),
            server: server.clone(),
            parent: parent.id().to_string(),
            owner: owner.to_string(),
            name,
            private,
            invitable,
            applied_tags,
            pinned: false,
            archived: false,
            locked: false,
            auto_archive_minutes: auto_archive_minutes
                .unwrap_or_else(|| parent.default_auto_archive_minutes()),
            archived_at: None,
            slowmode: slowmode.filter(|v| *v > 0),
            last_message_id: is_post.then(|| id.clone()),
            message_count: 0,
            member_count: 1,
        };

        db.insert_channel(&thread).await?;

        // The owner is the first member
        let member = ThreadMember::new(&id, owner);
        db.insert_thread_member(&member).await?;

        // Forum channels track their latest post
        if is_post {
            db.update_channel(
                parent.id(),
                &PartialChannel {
                    last_message_id: Some(id.clone()),
                    ..Default::default()
                },
                vec![],
            )
            .await?;
        }

        EventV1::ChannelCreate(thread.clone().into())
            .p(server.clone())
            .await;

        EventV1::ThreadMemberUpdate {
            id,
            member: Some(member.into()),
        }
        .private(owner.to_string())
        .await;

        Ok(thread)
    }

    /// Add a user to this thread, returns false if they already were a member
    pub async fn add_thread_member(&self, db: &Database, user_id: &str) -> Result<bool> {
        let Channel::Thread { id, server, .. } = self else {
            return Err(create_error!(InvalidOperation));
        };

        let member = ThreadMember::new(id, user_id);
        if !db.insert_thread_member(&member).await? {
            return Ok(false);
        }

        let member_count = db.add_to_thread_counts(id, 0, 1).await?.thread_member_count();

        EventV1::ThreadMemberUpdate {
            id: id.clone(),
            member: Some(member.clone().into()),
        }
        .private(user_id.to_string())
        .await;

        EventV1::ThreadMembersUpdate {
            id: id.clone(),
            server: server.clone(),
            member_count,
            added: vec![member.into()],
            removed: vec![],
        }
        .p(id.clone())
        .await;

        Ok(true)
    }

    /// Remove a user from this thread, returns false if they were not a member
    pub async fn remove_thread_member(&self, db: &Database, user_id: &str) -> Result<bool> {
        let Channel::Thread { id, server, .. } = self else {
            return Err(create_error!(InvalidOperation));
        };

        if !db.delete_thread_member(id, user_id).await? {
            return Ok(false);
        }

        let member_count = db.add_to_thread_counts(id, 0, -1).await?.thread_member_count();

        EventV1::ThreadMemberUpdate {
            id: id.clone(),
            member: None,
        }
        .private(user_id.to_string())
        .await;

        EventV1::ThreadMembersUpdate {
            id: id.clone(),
            server: server.clone(),
            member_count,
            added: vec![],
            removed: vec![user_id.to_string()],
        }
        .p(id.clone())
        .await;

        Ok(true)
    }

    /// Number of members of this thread
    pub fn thread_member_count(&self) -> u32 {
        match self {
            Channel::Thread { member_count, .. } => *member_count,
            _ => 0,
        }
    }

    /// Create a group
    pub async fn create_group(
        db: &Database,
        mut data: v0::DataCreateGroup,
        owner_id: String,
    ) -> Result<Channel> {
        data.users.insert(owner_id.to_string());

        let config = config().await;
        if data.users.len() > config.features.limits.global.group_size {
            return Err(create_error!(GroupTooLarge {
                max: config.features.limits.global.group_size,
            }));
        }

        let id = ulid::Ulid::new().to_string();

        let icon = if let Some(icon_id) = data.icon {
            Some(File::use_channel_icon(db, &icon_id, &id, &owner_id).await?)
        } else {
            None
        };

        let recipients = data.users.into_iter().collect::<Vec<String>>();
        let channel = Channel::Group {
            id,

            name: data.name,
            owner: owner_id,
            description: data.description,
            recipients: recipients.clone(),

            icon,
            last_message_id: None,

            permissions: None,

            nsfw: data.nsfw.unwrap_or(false),
        };

        db.insert_channel(&channel).await?;

        let event = EventV1::ChannelCreate(channel.clone().into());
        for recipient in recipients {
            event.clone().private(recipient).await;
        }

        Ok(channel)
    }

    /// Create a DM (or return the existing one / saved messages)
    pub async fn create_dm(db: &Database, user_a: &User, user_b: &User) -> Result<Channel> {
        // Try to find existing channel
        if let Ok(channel) = db.find_direct_message_channel(&user_a.id, &user_b.id).await {
            Ok(channel)
        } else {
            let channel = if user_a.id == user_b.id {
                // Create a new saved messages channel
                Channel::SavedMessages {
                    id: Ulid::new().to_string(),
                    user: user_a.id.to_string(),
                }
            } else {
                // Create a new DM channel
                Channel::DirectMessage {
                    id: Ulid::new().to_string(),
                    active: true, // show by default
                    recipients: vec![user_a.id.clone(), user_b.id.clone()],
                    last_message_id: None,
                }
            };

            db.insert_channel(&channel).await?;

            if let Channel::DirectMessage { .. } = &channel {
                let event = EventV1::ChannelCreate(channel.clone().into());
                event.clone().private(user_a.id.clone()).await;
                event.private(user_b.id.clone()).await;
            };

            Ok(channel)
        }
    }

    /// Add user to a group
    pub async fn add_user_to_group(
        &mut self,
        db: &Database,
        amqp: &AMQP,
        user: &User,
        by_id: &str,
    ) -> Result<()> {
        if let Channel::Group { recipients, .. } = self {
            if recipients.contains(&String::from(&user.id)) {
                return Err(create_error!(AlreadyInGroup));
            }

            let config = config().await;
            if recipients.len() >= config.features.limits.global.group_size {
                return Err(create_error!(GroupTooLarge {
                    max: config.features.limits.global.group_size
                }));
            }

            recipients.push(String::from(&user.id));
        }

        match &self {
            Channel::Group { id, .. } => {
                db.add_user_to_group(id, &user.id).await?;

                EventV1::ChannelGroupJoin {
                    id: id.to_string(),
                    user: user.id.to_string(),
                }
                .p(id.to_string())
                .await;

                SystemMessage::UserAdded {
                    id: user.id.to_string(),
                    by: by_id.to_string(),
                }
                .into_message(id.to_string())
                .send(
                    db,
                    Some(amqp),
                    MessageAuthor::System {
                        username: &user.username,
                        avatar: user.avatar.as_ref().map(|file| file.id.as_ref()),
                    },
                    None,
                    None,
                    self,
                    false,
                )
                .await
                .ok();

                EventV1::ChannelCreate(self.clone().into())
                    .private(user.id.to_string())
                    .await;

                Ok(())
            }
            _ => Err(create_error!(InvalidOperation)),
        }
    }

    /// Map out whether it is a direct DM
    pub fn is_direct_dm(&self) -> bool {
        matches!(self, Channel::DirectMessage { .. })
    }

    /// Check whether has a user as a recipient
    pub fn contains_user(&self, user_id: &str) -> bool {
        match self {
            Channel::Group { recipients, .. } | Channel::DirectMessage { recipients, .. } => {
                recipients.iter().any(|recipient| recipient == user_id)
            }
            Channel::SavedMessages { user, .. } => user == user_id,
            _ => false,
        }
    }

    /// Get list of recipients
    pub fn users(&self) -> Result<Vec<String>> {
        match self {
            Channel::Group { recipients, .. } | Channel::DirectMessage { recipients, .. } => {
                Ok(recipients.to_owned())
            }
            _ => Err(create_error!(NotFound)),
        }
    }

    /// Clone this channel's id
    pub fn id(&self) -> &str {
        match self {
            Channel::DirectMessage { id, .. }
            | Channel::Group { id, .. }
            | Channel::SavedMessages { id, .. }
            | Channel::TextChannel { id, .. }
            | Channel::ForumChannel { id, .. }
            | Channel::Thread { id, .. } => id,
        }
    }

    /// Clone this channel's server id
    pub fn server(&self) -> Option<&str> {
        match self {
            Channel::TextChannel { server, .. }
            | Channel::ForumChannel { server, .. }
            | Channel::Thread { server, .. } => Some(server),
            _ => None,
        }
    }

    /// Get the id of the channel this thread belongs to
    pub fn parent(&self) -> Option<&str> {
        match self {
            Channel::Thread { parent, .. } => Some(parent),
            _ => None,
        }
    }

    /// Whether this is a thread
    pub fn is_thread(&self) -> bool {
        matches!(self, Channel::Thread { .. })
    }

    /// Default auto archive duration (in minutes) for new threads in this channel
    pub fn default_auto_archive_minutes(&self) -> u32 {
        match self {
            Channel::TextChannel {
                default_auto_archive_minutes,
                ..
            }
            | Channel::ForumChannel {
                default_auto_archive_minutes,
                ..
            } => default_auto_archive_minutes.unwrap_or(DEFAULT_AUTO_ARCHIVE_MINUTES),
            _ => DEFAULT_AUTO_ARCHIVE_MINUTES,
        }
    }

    /// Gets this channel's voice information
    pub fn voice(&self) -> Option<Cow<VoiceInformation>> {
        match self {
            Self::DirectMessage { .. } | Self::Group { .. } => {
                Some(Cow::Owned(VoiceInformation::default()))
            }
            Self::TextChannel {
                voice: Some(voice), ..
            } => Some(Cow::Borrowed(voice)),
            _ => None,
        }
    }

    /// Set role permission on a channel
    pub async fn set_role_permission(
        &mut self,
        db: &Database,
        role_id: &str,
        permissions: OverrideField,
    ) -> Result<()> {
        match self {
            Channel::TextChannel {
                id,
                server,
                role_permissions,
                ..
            }
            | Channel::ForumChannel {
                id,
                server,
                role_permissions,
                ..
            } => {
                db.set_channel_role_permission(id, role_id, permissions)
                    .await?;

                role_permissions.insert(role_id.to_string(), permissions);

                EventV1::ChannelUpdate {
                    id: id.clone(),
                    data: PartialChannel {
                        role_permissions: Some(role_permissions.clone()),
                        ..Default::default()
                    }
                    .into(),
                    clear: vec![],
                }
                .p(server.clone())
                .await;

                Ok(())
            }
            _ => Err(create_error!(InvalidOperation)),
        }
    }

    /// Update channel data
    pub async fn update(
        &mut self,
        db: &Database,
        partial: PartialChannel,
        remove: Vec<FieldsChannel>,
    ) -> Result<()> {
        for field in &remove {
            self.remove_field(field);
        }

        self.apply_options(partial.clone());

        let id = self.id().to_string();
        db.update_channel(&id, &partial, remove.clone()).await?;

        EventV1::ChannelUpdate {
            id: id.clone(),
            data: partial.into(),
            clear: remove.into_iter().map(|v| v.into()).collect(),
        }
        .p(match self.server() {
            Some(server) => server.to_string(),
            None => id,
        })
        .await;

        Ok(())
    }

    /// Remove a field from Channel object
    pub fn remove_field(&mut self, field: &FieldsChannel) {
        match field {
            FieldsChannel::Description => match self {
                Self::Group { description, .. }
                | Self::TextChannel { description, .. }
                | Self::ForumChannel { description, .. } => {
                    description.take();
                }
                _ => {}
            },
            FieldsChannel::Icon => match self {
                Self::Group { icon, .. }
                | Self::TextChannel { icon, .. }
                | Self::ForumChannel { icon, .. } => {
                    icon.take();
                }
                _ => {}
            },
            FieldsChannel::DefaultPermissions => match self {
                Self::TextChannel {
                    default_permissions,
                    ..
                }
                | Self::ForumChannel {
                    default_permissions,
                    ..
                } => {
                    default_permissions.take();
                }
                _ => {}
            },
            FieldsChannel::Voice => match self {
                Self::TextChannel { voice, .. } => {
                    voice.take();
                }
                _ => {}
            },
            FieldsChannel::Slowmode => match self {
                Self::TextChannel { slowmode, .. }
                | Self::ForumChannel { slowmode, .. }
                | Self::Thread { slowmode, .. } => {
                    slowmode.take();
                }
                _ => {}
            },
            FieldsChannel::DefaultReactionEmoji => {
                if let Self::ForumChannel {
                    default_reaction_emoji,
                    ..
                } = self
                {
                    default_reaction_emoji.take();
                }
            }
            FieldsChannel::DefaultThreadSlowmode => {
                if let Self::ForumChannel {
                    default_thread_slowmode,
                    ..
                } = self
                {
                    default_thread_slowmode.take();
                }
            }
            FieldsChannel::DefaultAutoArchiveMinutes => match self {
                Self::TextChannel {
                    default_auto_archive_minutes,
                    ..
                }
                | Self::ForumChannel {
                    default_auto_archive_minutes,
                    ..
                } => {
                    default_auto_archive_minutes.take();
                }
                _ => {}
            },
        }
    }

    /// Remove multiple fields from Channel object
    pub fn remove_fields(&mut self, partial: Vec<FieldsChannel>) {
        for field in partial {
            self.remove_field(&field)
        }
    }

    /// Apply partial channel to channel
    #[allow(deprecated)]
    pub fn apply_options(&mut self, partial: PartialChannel) {
        match self {
            Self::SavedMessages { .. } => {}
            Self::DirectMessage { active, .. } => {
                if let Some(v) = partial.active {
                    *active = v;
                }
            }
            Self::Group {
                name,
                owner,
                description,
                icon,
                nsfw,
                permissions,
                ..
            } => {
                if let Some(v) = partial.name {
                    *name = v;
                }

                if let Some(v) = partial.owner {
                    *owner = v;
                }

                if let Some(v) = partial.description {
                    description.replace(v);
                }

                if let Some(v) = partial.icon {
                    icon.replace(v);
                }

                if let Some(v) = partial.nsfw {
                    *nsfw = v;
                }

                if let Some(v) = partial.permissions {
                    permissions.replace(v);
                }
            }
            Self::TextChannel {
                name,
                description,
                icon,
                nsfw,
                default_permissions,
                role_permissions,
                voice,
                slowmode,
                default_auto_archive_minutes,
                ..
            } => {
                if let Some(v) = partial.name {
                    *name = v;
                }

                if let Some(v) = partial.slowmode {
                    slowmode.replace(v);
                }

                if let Some(v) = partial.default_auto_archive_minutes {
                    default_auto_archive_minutes.replace(v);
                }

                if let Some(v) = partial.description {
                    description.replace(v);
                }

                if let Some(v) = partial.icon {
                    icon.replace(v);
                }

                if let Some(v) = partial.nsfw {
                    *nsfw = v;
                }

                if let Some(v) = partial.role_permissions {
                    *role_permissions = v;
                }

                if let Some(v) = partial.default_permissions {
                    default_permissions.replace(v);
                }

                if let Some(v) = partial.voice {
                    voice.replace(v);
                }
            }
            Self::ForumChannel {
                name,
                description,
                icon,
                nsfw,
                default_permissions,
                role_permissions,
                slowmode,
                available_tags,
                require_tag,
                default_reaction_emoji,
                default_sort_order,
                default_layout,
                default_thread_slowmode,
                default_auto_archive_minutes,
                last_message_id,
                ..
            } => {
                if let Some(v) = partial.name {
                    *name = v;
                }

                if let Some(v) = partial.description {
                    description.replace(v);
                }

                if let Some(v) = partial.icon {
                    icon.replace(v);
                }

                if let Some(v) = partial.nsfw {
                    *nsfw = v;
                }

                if let Some(v) = partial.role_permissions {
                    *role_permissions = v;
                }

                if let Some(v) = partial.default_permissions {
                    default_permissions.replace(v);
                }

                if let Some(v) = partial.slowmode {
                    slowmode.replace(v);
                }

                if let Some(v) = partial.available_tags {
                    *available_tags = v;
                }

                if let Some(v) = partial.require_tag {
                    *require_tag = v;
                }

                if let Some(v) = partial.default_reaction_emoji {
                    default_reaction_emoji.replace(v);
                }

                if let Some(v) = partial.default_sort_order {
                    *default_sort_order = v;
                }

                if let Some(v) = partial.default_layout {
                    *default_layout = v;
                }

                if let Some(v) = partial.default_thread_slowmode {
                    default_thread_slowmode.replace(v);
                }

                if let Some(v) = partial.default_auto_archive_minutes {
                    default_auto_archive_minutes.replace(v);
                }

                if let Some(v) = partial.last_message_id {
                    last_message_id.replace(v);
                }
            }
            Self::Thread {
                name,
                owner,
                invitable,
                applied_tags,
                pinned,
                archived,
                locked,
                auto_archive_minutes,
                archived_at,
                slowmode,
                last_message_id,
                message_count,
                member_count,
                ..
            } => {
                if let Some(v) = partial.name {
                    *name = v;
                }

                if let Some(v) = partial.owner {
                    *owner = v;
                }

                if let Some(v) = partial.invitable {
                    *invitable = v;
                }

                if let Some(v) = partial.applied_tags {
                    *applied_tags = v;
                }

                if let Some(v) = partial.pinned {
                    *pinned = v;
                }

                if let Some(v) = partial.archived {
                    *archived = v;
                }

                if let Some(v) = partial.locked {
                    *locked = v;
                }

                if let Some(v) = partial.auto_archive_minutes {
                    *auto_archive_minutes = v;
                }

                if let Some(v) = partial.archived_at {
                    archived_at.replace(v);
                }

                if let Some(v) = partial.slowmode {
                    slowmode.replace(v);
                }

                if let Some(v) = partial.last_message_id {
                    last_message_id.replace(v);
                }

                if let Some(v) = partial.message_count {
                    *message_count = v;
                }

                if let Some(v) = partial.member_count {
                    *member_count = v;
                }
            }
        }
    }

    /// Generates a PartialChannel containing the data which has changed in an update
    pub fn generate_diff(
        &self,
        partial: &PartialChannel,
        remove: &[FieldsChannel],
    ) -> PartialChannel {
        let mut before = PartialChannel::default();

        match self {
            Channel::SavedMessages { .. } => {}
            Channel::DirectMessage {
                active,
                last_message_id,
                ..
            } => {
                if partial.active.is_some() {
                    before.active = Some(*active);
                };

                if partial.last_message_id.is_some() {
                    before.last_message_id = last_message_id.clone()
                };
            }
            Channel::Group {
                name,
                owner,
                description,
                icon,
                last_message_id,
                permissions,
                nsfw,
                ..
            } => {
                if partial.name.is_some() {
                    before.name = Some(name.clone());
                };

                if partial.owner.is_some() {
                    before.owner = Some(owner.clone());
                };

                if partial.description.is_some() || remove.contains(&FieldsChannel::Description) {
                    before.description = description.clone();
                };

                if partial.icon.is_some() || remove.contains(&FieldsChannel::Icon) {
                    before.icon = icon.clone();
                };

                if partial.last_message_id.is_some() {
                    before.last_message_id = last_message_id.clone()
                };

                if partial.permissions.is_some() {
                    before.permissions = *permissions;
                };

                if partial.nsfw.is_some() {
                    before.nsfw = Some(*nsfw);
                };
            }
            Channel::TextChannel {
                name,
                description,
                icon,
                last_message_id,
                default_permissions,
                role_permissions,
                nsfw,
                voice,
                slowmode,
                default_auto_archive_minutes,
                ..
            } => {
                if partial.name.is_some() {
                    before.name = Some(name.clone());
                };

                if partial.description.is_some() || remove.contains(&FieldsChannel::Description) {
                    before.description = description.clone();
                };

                if partial.icon.is_some() || remove.contains(&FieldsChannel::Icon) {
                    before.icon = icon.clone();
                };

                if partial.last_message_id.is_some() {
                    before.last_message_id = last_message_id.clone()
                };

                if partial.default_permissions.is_some()
                    || remove.contains(&FieldsChannel::DefaultPermissions)
                {
                    before.default_permissions = *default_permissions;
                };

                if partial.role_permissions.is_some() {
                    before.role_permissions = Some(role_permissions.clone());
                };

                if partial.nsfw.is_some() {
                    before.nsfw = Some(*nsfw);
                };

                if partial.voice.is_some() || remove.contains(&FieldsChannel::Voice) {
                    before.voice = voice.clone();
                };

                if partial.slowmode.is_some() || remove.contains(&FieldsChannel::Slowmode) {
                    before.slowmode = *slowmode;
                }

                if partial.default_auto_archive_minutes.is_some()
                    || remove.contains(&FieldsChannel::DefaultAutoArchiveMinutes)
                {
                    before.default_auto_archive_minutes = *default_auto_archive_minutes;
                }
            }
            Channel::ForumChannel {
                name,
                description,
                icon,
                last_message_id,
                default_permissions,
                role_permissions,
                nsfw,
                slowmode,
                available_tags,
                require_tag,
                default_reaction_emoji,
                default_sort_order,
                default_layout,
                default_thread_slowmode,
                default_auto_archive_minutes,
                ..
            } => {
                if partial.name.is_some() {
                    before.name = Some(name.clone());
                };

                if partial.description.is_some() || remove.contains(&FieldsChannel::Description) {
                    before.description = description.clone();
                };

                if partial.icon.is_some() || remove.contains(&FieldsChannel::Icon) {
                    before.icon = icon.clone();
                };

                if partial.last_message_id.is_some() {
                    before.last_message_id = last_message_id.clone()
                };

                if partial.default_permissions.is_some()
                    || remove.contains(&FieldsChannel::DefaultPermissions)
                {
                    before.default_permissions = *default_permissions;
                };

                if partial.role_permissions.is_some() {
                    before.role_permissions = Some(role_permissions.clone());
                };

                if partial.nsfw.is_some() {
                    before.nsfw = Some(*nsfw);
                };

                if partial.slowmode.is_some() || remove.contains(&FieldsChannel::Slowmode) {
                    before.slowmode = *slowmode;
                }

                if partial.available_tags.is_some() {
                    before.available_tags = Some(available_tags.clone());
                }

                if partial.require_tag.is_some() {
                    before.require_tag = Some(*require_tag);
                }

                if partial.default_reaction_emoji.is_some()
                    || remove.contains(&FieldsChannel::DefaultReactionEmoji)
                {
                    before.default_reaction_emoji = default_reaction_emoji.clone();
                }

                if partial.default_sort_order.is_some() {
                    before.default_sort_order = Some(default_sort_order.clone());
                }

                if partial.default_layout.is_some() {
                    before.default_layout = Some(default_layout.clone());
                }

                if partial.default_thread_slowmode.is_some()
                    || remove.contains(&FieldsChannel::DefaultThreadSlowmode)
                {
                    before.default_thread_slowmode = *default_thread_slowmode;
                }

                if partial.default_auto_archive_minutes.is_some()
                    || remove.contains(&FieldsChannel::DefaultAutoArchiveMinutes)
                {
                    before.default_auto_archive_minutes = *default_auto_archive_minutes;
                }
            }
            Channel::Thread {
                name,
                invitable,
                applied_tags,
                pinned,
                archived,
                locked,
                auto_archive_minutes,
                slowmode,
                ..
            } => {
                if partial.name.is_some() {
                    before.name = Some(name.clone());
                };

                if partial.invitable.is_some() {
                    before.invitable = Some(*invitable);
                }

                if partial.applied_tags.is_some() {
                    before.applied_tags = Some(applied_tags.clone());
                }

                if partial.pinned.is_some() {
                    before.pinned = Some(*pinned);
                }

                if partial.archived.is_some() {
                    before.archived = Some(*archived);
                }

                if partial.locked.is_some() {
                    before.locked = Some(*locked);
                }

                if partial.auto_archive_minutes.is_some() {
                    before.auto_archive_minutes = Some(*auto_archive_minutes);
                }

                if partial.slowmode.is_some() || remove.contains(&FieldsChannel::Slowmode) {
                    before.slowmode = *slowmode;
                }
            }
        }

        before
    }

    /// Acknowledge a message
    pub async fn ack(&self, user: &str, message: &str, amqp: &AMQP) -> Result<()> {
        EventV1::ChannelAck {
            id: self.id().to_string(),
            user: user.to_string(),
            message_id: message.to_string(),
        }
        .private(user.to_string())
        .await;

        crate::util::acker::ack_channel(user, self.id(), message, amqp).await
    }

    /// Remove user from a group
    pub async fn remove_user_from_group(
        &self,
        db: &Database,
        amqp: &AMQP,
        user: &User,
        by_id: Option<&str>,
        silent: bool,
    ) -> Result<()> {
        match &self {
            Channel::Group {
                id,
                name,
                owner,
                recipients,
                ..
            } => {
                if &user.id == owner {
                    if let Some(new_owner) = recipients.iter().find(|x| *x != &user.id) {
                        db.update_channel(
                            id,
                            &PartialChannel {
                                owner: Some(new_owner.into()),
                                ..Default::default()
                            },
                            vec![],
                        )
                        .await?;

                        SystemMessage::ChannelOwnershipChanged {
                            from: owner.to_string(),
                            to: new_owner.to_string(),
                        }
                        .into_message(id.to_string())
                        .send(
                            db,
                            Some(amqp),
                            MessageAuthor::System {
                                username: name,
                                avatar: None,
                            },
                            None,
                            None,
                            self,
                            false,
                        )
                        .await
                        .ok();
                    } else {
                        return self.delete(db).await;
                    }
                }

                db.remove_user_from_group(id, &user.id).await?;

                EventV1::ChannelGroupLeave {
                    id: id.to_string(),
                    user: user.id.to_string(),
                }
                .p(id.to_string())
                .await;

                if !silent {
                    if let Some(by) = by_id {
                        SystemMessage::UserRemove {
                            id: user.id.to_string(),
                            by: by.to_string(),
                        }
                    } else {
                        SystemMessage::UserLeft {
                            id: user.id.to_string(),
                        }
                    }
                    .into_message(id.to_string())
                    .send(
                        db,
                        Some(amqp),
                        MessageAuthor::System {
                            username: &user.username,
                            avatar: user.avatar.as_ref().map(|file| file.id.as_ref()),
                        },
                        None,
                        None,
                        self,
                        false,
                    )
                    .await
                    .ok();
                }

                Ok(())
            }

            _ => Err(create_error!(InvalidOperation)),
        }
    }

    /// Delete a channel
    pub async fn delete(&self, db: &Database) -> Result<()> {
        let id = self.id().to_string();

        match self {
            // Threads go away together with their parent channel
            Channel::TextChannel { .. } | Channel::ForumChannel { .. } => {
                for thread in db.fetch_threads(&id, None).await? {
                    let thread_id = thread.id().to_string();
                    db.delete_channel(&thread).await?;
                    EventV1::ChannelDelete {
                        id: thread_id.clone(),
                    }
                    .p(thread_id)
                    .await;
                }
            }
            // The starter message no longer has a thread
            Channel::Thread { parent, .. } => {
                if let Ok(mut message) = db.fetch_message(&id).await {
                    if &message.channel == parent {
                        let mut flags = MessageFlagsValue(message.flags.unwrap_or_default());
                        if flags.has(MessageFlags::HasThread) {
                            flags.set(MessageFlags::HasThread, false);
                            message
                                .update(
                                    db,
                                    PartialMessage {
                                        flags: Some(flags.0),
                                        ..Default::default()
                                    },
                                    vec![],
                                )
                                .await?;
                        }
                    }
                }
            }
            _ => {}
        }

        EventV1::ChannelDelete { id: id.clone() }.p(id).await;
        // TODO: missing functionality:
        // - group invites
        // - channels list / categories list on server
        db.delete_channel(self).await
    }
}

#[cfg(feature = "mongodb")]
impl IntoDocumentPath for FieldsChannel {
    fn as_path(&self) -> Option<&'static str> {
        Some(match self {
            FieldsChannel::Description => "description",
            FieldsChannel::Icon => "icon",
            FieldsChannel::DefaultPermissions => "default_permissions",
            FieldsChannel::Voice => "voice",
            FieldsChannel::Slowmode => "slowmode",
            FieldsChannel::DefaultReactionEmoji => "default_reaction_emoji",
            FieldsChannel::DefaultThreadSlowmode => "default_thread_slowmode",
            FieldsChannel::DefaultAutoArchiveMinutes => "default_auto_archive_minutes",
        })
    }
}

#[cfg(test)]
mod tests {
    use revolt_permissions::{calculate_channel_permissions, ChannelPermission};

    use crate::{fixture, util::permissions::DatabasePermissionQuery};

    #[tokio::test]
    async fn permissions_group_channel() {
        database_test!(|db| async move {
            fixture!(db, "group_with_members",
                owner user 0
                member1 user 1
                member2 user 2
                channel channel 3);

            let mut query = DatabasePermissionQuery::new(&db, &owner).channel(&channel);
            assert!(calculate_channel_permissions(&mut query)
                .await
                .has_channel_permission(ChannelPermission::SendMessage));

            let mut query = DatabasePermissionQuery::new(&db, &member1).channel(&channel);
            assert!(calculate_channel_permissions(&mut query)
                .await
                .has_channel_permission(ChannelPermission::SendMessage));

            let mut query = DatabasePermissionQuery::new(&db, &member2).channel(&channel);
            assert!(!calculate_channel_permissions(&mut query)
                .await
                .has_channel_permission(ChannelPermission::SendMessage));
        });
    }

    #[tokio::test]
    async fn permissions_text_channel() {
        database_test!(|db| async move {
            fixture!(db, "server_with_roles",
                owner user 0
                moderator user 1
                user user 2
                channel channel 3);

            let mut query = DatabasePermissionQuery::new(&db, &owner).channel(&channel);
            assert!(calculate_channel_permissions(&mut query)
                .await
                .has_channel_permission(ChannelPermission::SendMessage));

            let mut query = DatabasePermissionQuery::new(&db, &moderator).channel(&channel);
            assert!(calculate_channel_permissions(&mut query)
                .await
                .has_channel_permission(ChannelPermission::SendMessage));

            let mut query = DatabasePermissionQuery::new(&db, &user).channel(&channel);
            assert!(!calculate_channel_permissions(&mut query)
                .await
                .has_channel_permission(ChannelPermission::SendMessage));
        });
    }
}
