#![allow(deprecated)]
use super::{File, UserVoiceState};

use iso8601_timestamp::Timestamp;
use revolt_permissions::{Override, OverrideField};
use std::collections::{HashMap, HashSet};

#[cfg(feature = "rocket")]
use rocket::FromForm;

auto_derived!(
    /// Channel
    #[serde(tag = "channel_type")]
    pub enum Channel {
        /// Personal "Saved Notes" channel which allows users to save messages
        SavedMessages {
            /// Unique Id
            #[cfg_attr(feature = "serde", serde(rename = "_id"))]
            id: String,
            /// Id of the user this channel belongs to
            user: String,
        },
        /// Direct message channel between two users
        DirectMessage {
            /// Unique Id
            #[cfg_attr(feature = "serde", serde(rename = "_id"))]
            id: String,

            /// Whether this direct message channel is currently open on both sides
            active: bool,
            /// 2-tuple of user ids participating in direct message
            recipients: Vec<String>,
            /// Id of the last message sent in this channel
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            last_message_id: Option<String>,
        },
        /// Group channel between 1 or more participants
        Group {
            /// Unique Id
            #[cfg_attr(feature = "serde", serde(rename = "_id"))]
            id: String,

            /// Display name of the channel
            name: String,
            /// User id of the owner of the group
            owner: String,
            /// Channel description
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            description: Option<String>,
            /// Array of user ids participating in channel
            recipients: Vec<String>,

            /// Custom icon attachment
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            icon: Option<File>,
            /// Id of the last message sent in this channel
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            last_message_id: Option<String>,

            /// Permissions assigned to members of this group
            /// (does not apply to the owner of the group)
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            permissions: Option<i64>,

            /// Whether this group is marked as not safe for work
            #[cfg_attr(
                feature = "serde",
                serde(skip_serializing_if = "crate::if_false", default)
            )]
            nsfw: bool,
        },
        /// Text channel belonging to a server
        TextChannel {
            /// Unique Id
            #[cfg_attr(feature = "serde", serde(rename = "_id"))]
            id: String,
            /// Id of the server this channel belongs to
            server: String,

            /// Display name of the channel
            name: String,
            /// Channel description
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            description: Option<String>,

            /// Custom icon attachment
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            icon: Option<File>,
            /// Id of the last message sent in this channel
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            last_message_id: Option<String>,

            /// Default permissions assigned to users in this channel
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            default_permissions: Option<OverrideField>,
            /// Permissions assigned based on role to this channel
            #[cfg_attr(
                feature = "serde",
                serde(
                    default = "HashMap::<String, OverrideField>::new",
                    skip_serializing_if = "HashMap::<String, OverrideField>::is_empty"
                )
            )]
            role_permissions: HashMap<String, OverrideField>,

            /// Whether this channel is marked as not safe for work
            #[cfg_attr(
                feature = "serde",
                serde(skip_serializing_if = "crate::if_false", default)
            )]
            nsfw: bool,

            /// Voice Information for when this channel is also a voice channel
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            voice: Option<VoiceInformation>,

            /// DJ Information for when this channel is also a DJ channel
            #[cfg_attr(
                feature = "serde",
                serde(default, skip_serializing_if = "Option::is_none")
            )]
            dj: Option<DjInformation>,

            /// The channel's slowmode delay in seconds
            #[serde(skip_serializing_if = "Option::is_none")]
            slowmode: Option<u64>,

            /// Default auto archive duration (in minutes) for new threads
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            default_auto_archive_minutes: Option<u32>,
        },
        /// Forum channel belonging to a server, which holds posts (threads)
        ForumChannel {
            /// Unique Id
            #[cfg_attr(feature = "serde", serde(rename = "_id"))]
            id: String,
            /// Id of the server this channel belongs to
            server: String,

            /// Display name of the channel
            name: String,
            /// Post guidelines
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            description: Option<String>,

            /// Custom icon attachment
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            icon: Option<File>,
            /// Id of the most recently created post
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            last_message_id: Option<String>,

            /// Default permissions assigned to users in this channel
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            default_permissions: Option<OverrideField>,
            /// Permissions assigned based on role to this channel
            #[cfg_attr(
                feature = "serde",
                serde(
                    default = "HashMap::<String, OverrideField>::new",
                    skip_serializing_if = "HashMap::<String, OverrideField>::is_empty"
                )
            )]
            role_permissions: HashMap<String, OverrideField>,

            /// Whether this channel is marked as not safe for work
            #[cfg_attr(
                feature = "serde",
                serde(skip_serializing_if = "crate::if_false", default)
            )]
            nsfw: bool,

            /// Delay in seconds between creating posts
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            slowmode: Option<u64>,

            /// Tags that can be applied to posts
            #[cfg_attr(
                feature = "serde",
                serde(default, skip_serializing_if = "Vec::is_empty")
            )]
            available_tags: Vec<ForumTag>,
            /// Whether posts must have at least one tag
            #[cfg_attr(
                feature = "serde",
                serde(skip_serializing_if = "crate::if_false", default)
            )]
            require_tag: bool,
            /// Emoji shown as the reaction button on posts (unicode or custom emoji id)
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            default_reaction_emoji: Option<String>,
            /// Default order of posts
            #[cfg_attr(feature = "serde", serde(default))]
            default_sort_order: ForumSortOrder,
            /// Default layout of posts
            #[cfg_attr(feature = "serde", serde(default))]
            default_layout: ForumLayout,
            /// Slowmode applied to new posts
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            default_thread_slowmode: Option<u64>,
            /// Default auto archive duration (in minutes) for new posts
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            default_auto_archive_minutes: Option<u32>,
        },
        /// Thread in a text channel, or a post in a forum channel
        ///
        /// Threads use the permissions of their parent channel.
        Thread {
            /// Unique Id
            ///
            /// Equal to the id of the message the thread was started from,
            /// or of the first message of a forum post.
            #[cfg_attr(feature = "serde", serde(rename = "_id"))]
            id: String,
            /// Id of the server this thread belongs to
            server: String,
            /// Id of the channel this thread belongs to
            parent: String,
            /// User id of the thread creator
            owner: String,

            /// Display name of the thread
            name: String,

            /// Whether only members (and users with ManageThreads) can see the thread
            #[cfg_attr(
                feature = "serde",
                serde(skip_serializing_if = "crate::if_false", default)
            )]
            private: bool,
            /// Whether non-moderators can add other members to a private thread
            #[cfg_attr(
                feature = "serde",
                serde(skip_serializing_if = "crate::if_false", default)
            )]
            invitable: bool,

            /// Ids of the forum tags applied to this post
            #[cfg_attr(
                feature = "serde",
                serde(default, skip_serializing_if = "Vec::is_empty")
            )]
            applied_tags: Vec<String>,
            /// Whether this post is pinned to the top of the forum
            #[cfg_attr(
                feature = "serde",
                serde(skip_serializing_if = "crate::if_false", default)
            )]
            pinned: bool,

            /// Whether the thread is archived
            #[cfg_attr(
                feature = "serde",
                serde(skip_serializing_if = "crate::if_false", default)
            )]
            archived: bool,
            /// Whether the thread is locked (only ManageThreads can post or unarchive)
            #[cfg_attr(
                feature = "serde",
                serde(skip_serializing_if = "crate::if_false", default)
            )]
            locked: bool,
            /// Minutes of inactivity after which the thread is archived
            auto_archive_minutes: u32,
            /// When the thread was last archived or unarchived
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            archived_at: Option<Timestamp>,

            /// The thread's slowmode delay in seconds
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            slowmode: Option<u64>,

            /// Id of the last message sent in this thread
            #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
            last_message_id: Option<String>,
            /// Number of messages in the thread (not counting the starter message)
            #[cfg_attr(feature = "serde", serde(default))]
            message_count: u32,
            /// Number of members in the thread
            #[cfg_attr(feature = "serde", serde(default))]
            member_count: u32,
        },
    }

    /// Tag that can be applied to forum posts
    #[cfg_attr(feature = "validator", derive(validator::Validate))]
    pub struct ForumTag {
        /// Unique Id (left empty when creating a tag)
        #[cfg_attr(feature = "serde", serde(default))]
        #[cfg_attr(feature = "validator", validate(length(max = 26)))]
        pub id: String,
        /// Tag name
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 20)))]
        pub name: String,
        /// Emoji (unicode or custom emoji id)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub emoji: Option<String>,
        /// Whether only users with ManageThreads can apply this tag
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "crate::if_false", default)
        )]
        pub moderated: bool,
    }

    /// Order of posts in a forum
    #[derive(Default)]
    #[cfg_attr(feature = "rocket", derive(rocket::FromFormField))]
    pub enum ForumSortOrder {
        /// Most recent activity first
        #[default]
        LatestActivity,
        /// Most recently created first
        CreationDate,
    }

    /// Layout of posts in a forum
    #[derive(Default)]
    pub enum ForumLayout {
        /// List of posts
        #[default]
        List,
        /// Grid of posts with previews
        Gallery,
    }

    /// Voice information for a channel
    #[derive(Default)]
    #[cfg_attr(feature = "validator", derive(validator::Validate))]
    pub struct VoiceInformation {
        /// Maximium amount of users allowed in the voice channel at once
        #[cfg_attr(feature = "validator", validate(range(min = 1)))]
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub max_users: Option<usize>,
    }

    /// A track in the DJ queue
    #[derive(Default)]
    pub struct DjTrack {
        /// Unique id of this track (`<seq>-<ulid>`)
        pub id: String,
        /// Route of the streaming endpoint for this track
        ///
        /// `/dj/<channel>/stream`: the audio itself requires the signed token
        /// which is delivered in `DjNowPlaying`/`DjTrackStarted`, so this is
        /// informational and must not be fetched directly.
        pub file: String,
        /// Display title of the track
        #[cfg_attr(feature = "serde", serde(default))]
        pub title: String,
        /// Duration of the track in milliseconds
        #[cfg_attr(feature = "serde", serde(default))]
        pub duration_ms: u64,
        /// Id of the user who uploaded the track
        pub uploader: String,
    }

    /// The track currently playing in a DJ channel
    #[derive(Default)]
    pub struct DjNowPlaying {
        /// Track which is currently playing
        pub track: DjTrack,
        /// Unix timestamp (in milliseconds) at which the track started
        pub started_at: u64,
    }

    /// State of a DJ channel
    #[derive(Default)]
    pub struct DjChannelState {
        /// Id of the channel this state belongs to
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        /// Track which is currently playing, if any
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub current: Option<DjNowPlaying>,
        /// Playback position of the current track, in milliseconds
        #[cfg_attr(feature = "serde", serde(default))]
        pub position_ms: u64,
        /// Server time at which this state was generated (unix milliseconds)
        #[cfg_attr(feature = "serde", serde(default))]
        pub server_time: u64,
        /// Tracks which are queued up, in playback order
        #[cfg_attr(feature = "serde", serde(default))]
        pub queue: Vec<DjTrack>,
        /// Number of (valid) votes to skip the current track
        #[cfg_attr(feature = "serde", serde(default))]
        pub votes: u32,
        /// Users who have voted to skip the current track
        #[cfg_attr(feature = "serde", serde(default))]
        pub voters: Vec<String>,
        /// Users which have joined the channel
        #[cfg_attr(feature = "serde", serde(default))]
        pub members: Vec<String>,
        /// Settings of this DJ channel
        pub settings: DjInformation,
    }

    /// Membership of the current user in a DJ channel
    #[derive(Default)]
    pub struct DjChannelMembers {
        /// Id of the channel
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        /// Users which have joined the channel
        #[cfg_attr(feature = "serde", serde(default))]
        pub members: Vec<String>,
    }

    /// Partial representation of a channel
    #[derive(Default)]
    pub struct PartialChannel {
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub name: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub owner: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub description: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub icon: Option<File>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub nsfw: Option<bool>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub active: Option<bool>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub permissions: Option<i64>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub role_permissions: Option<HashMap<String, OverrideField>>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub default_permissions: Option<OverrideField>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub last_message_id: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub voice: Option<VoiceInformation>,
        /// DJ settings, present only on DJ-enabled text channels
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub dj: Option<DjInformation>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub slowmode: Option<u64>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub available_tags: Option<Vec<ForumTag>>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub require_tag: Option<bool>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub default_reaction_emoji: Option<String>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub default_sort_order: Option<ForumSortOrder>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub default_layout: Option<ForumLayout>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub default_thread_slowmode: Option<u64>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub default_auto_archive_minutes: Option<u32>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub invitable: Option<bool>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub applied_tags: Option<Vec<String>>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub pinned: Option<bool>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub archived: Option<bool>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub locked: Option<bool>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub auto_archive_minutes: Option<u32>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub archived_at: Option<Timestamp>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub message_count: Option<u32>,
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub member_count: Option<u32>,
    }

    /// Optional fields on channel object
    pub enum FieldsChannel {
        Description,
        Icon,
        DefaultPermissions,
        Voice,
        /// Removes DJ state, turning the channel back into a normal text channel
        Dj,
        Slowmode,
        DefaultReactionEmoji,
        DefaultThreadSlowmode,
        DefaultAutoArchiveMinutes,
    }

    /// New webhook information
    #[derive(Default)]
    #[cfg_attr(feature = "validator", derive(validator::Validate))]
    pub struct DataEditChannel {
        /// Channel name (up to 32 characters, or 100 for threads)
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 100)))]
        pub name: Option<String>,

        /// Channel description
        #[cfg_attr(feature = "validator", validate(length(min = 0, max = 1024)))]
        pub description: Option<String>,

        /// Group owner
        pub owner: Option<String>,

        /// Icon
        ///
        /// Provide an Autumn attachment Id.
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 128)))]
        pub icon: Option<String>,

        /// Whether this channel is age-restricted
        pub nsfw: Option<bool>,

        /// Whether this thread is archived
        pub archived: Option<bool>,

        /// Whether this thread is locked
        pub locked: Option<bool>,

        /// Whether this forum post is pinned
        pub pinned: Option<bool>,

        /// Whether non-moderators can add members to this private thread
        pub invitable: Option<bool>,

        /// Minutes of inactivity after which this thread is archived
        pub auto_archive_minutes: Option<u32>,

        /// Ids of forum tags to apply to this post
        #[cfg_attr(feature = "validator", validate(length(max = 5)))]
        pub applied_tags: Option<Vec<String>>,

        /// Voice Information for voice channels
        pub voice: Option<VoiceInformation>,

        /// The channel's slow mode delay in seconds, up to 6 hours
        #[cfg_attr(feature = "validator", validate(range(min = 0, max = 21600)))]
        pub slowmode: Option<u64>,

        /// Tags that can be applied to posts in this forum
        #[cfg_attr(feature = "validator", validate(length(max = 20)))]
        pub available_tags: Option<Vec<ForumTag>>,

        /// Whether posts in this forum must have a tag
        pub require_tag: Option<bool>,

        /// Emoji shown as the reaction button on posts
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 64)))]
        pub default_reaction_emoji: Option<String>,

        /// Default order of posts in this forum
        pub default_sort_order: Option<ForumSortOrder>,

        /// Default layout of posts in this forum
        pub default_layout: Option<ForumLayout>,

        /// Slowmode applied to new posts, up to 6 hours
        #[cfg_attr(feature = "validator", validate(range(min = 0, max = 21600)))]
        pub default_thread_slowmode: Option<u64>,

        /// Default auto archive duration (in minutes) for new threads
        pub default_auto_archive_minutes: Option<u32>,

        /// Fields to remove from channel
        #[cfg_attr(feature = "serde", serde(default))]
        pub remove: Vec<FieldsChannel>,
    }

    /// Create new group
    #[derive(Default)]
    #[cfg_attr(feature = "validator", derive(validator::Validate))]
    pub struct DataCreateGroup {
        /// Group name
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub name: String,
        /// Group description
        #[cfg_attr(feature = "validator", validate(length(min = 0, max = 1024)))]
        pub description: Option<String>,
        /// Group icon
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 128)))]
        pub icon: Option<String>,
        /// Array of user IDs to add to the group
        ///
        /// Must be friends with these users.
        #[cfg_attr(feature = "validator", validate(length(min = 0, max = 49)))]
        #[serde(default)]
        pub users: HashSet<String>,
        /// Whether this group is age-restricted
        #[serde(skip_serializing_if = "Option::is_none")]
        pub nsfw: Option<bool>,
    }

    /// Server Channel Type
    #[derive(Default)]
    pub enum LegacyServerChannelType {
        /// Text Channel
        #[default]
        Text,
        /// Voice Channel
        Voice,
        /// Forum Channel
        Forum,
    }

    /// Create new server channel
    #[derive(Default)]
    #[cfg_attr(feature = "validator", derive(validator::Validate))]
    pub struct DataCreateServerChannel {
        /// Channel type
        #[serde(rename = "type", default = "LegacyServerChannelType::default")]
        pub channel_type: LegacyServerChannelType,
        /// Channel name
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 32)))]
        pub name: String,
        /// Channel description
        #[cfg_attr(feature = "validator", validate(length(min = 0, max = 1024)))]
        pub description: Option<String>,
        /// Whether this channel is age restricted
        #[serde(skip_serializing_if = "Option::is_none")]
        pub nsfw: Option<bool>,

        /// Voice Information for when this channel is also a voice channel
        #[serde(skip_serializing_if = "Option::is_none")]
        pub voice: Option<VoiceInformation>,
    }

    /// New default permissions
    #[serde(untagged)]
    pub enum DataDefaultChannelPermissions {
        Value {
            /// Permission values to set for members in a `Group`
            permissions: u64,
        },
        Field {
            /// Allow / deny values to set for members in this server channel
            permissions: Override,
        },
    }

    /// New role permissions
    pub struct DataSetRolePermissions {
        /// Allow / deny values to set for this role
        pub permissions: Override,
    }

    /// Options when deleting a channel
    #[cfg_attr(feature = "rocket", derive(FromForm))]
    pub struct OptionsChannelDelete {
        /// Whether to not send a leave message
        pub leave_silently: Option<bool>,
    }

    /// Voice server token response
    pub struct CreateVoiceUserResponse {
        /// Token for authenticating with the voice server
        pub token: String,
        /// Url of the livekit server to connect to
        pub url: String,
    }

    /// Voice state for a channel
    pub struct ChannelVoiceState {
        pub id: String,
        /// The states of the users who are connected to the channel
        pub participants: Vec<UserVoiceState>,
    }

    /// Join a voice channel
    pub struct DataJoinCall {
        /// Name of the node to join
        pub node: Option<String>,
        /// Whether to force disconnect any other existing voice connections
        ///
        /// Useful for disconnecting on another device and joining on a new.
        pub force_disconnect: Option<bool>,
        /// Users which should be notified of the call starting
        ///
        /// Only used when the user is the first one connected.
        pub recipients: Option<Vec<String>>,
    }

    /// Start a thread
    #[cfg_attr(feature = "validator", derive(validator::Validate))]
    pub struct DataCreateThread {
        /// Thread name
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 100)))]
        pub name: String,
        /// Whether the thread is private (text channels only, not when starting from a message)
        #[cfg_attr(feature = "serde", serde(default))]
        pub private: bool,
        /// Whether non-moderators can add members to a private thread
        #[cfg_attr(feature = "serde", serde(default = "crate::default_true"))]
        pub invitable: bool,
        /// Minutes of inactivity after which the thread is archived
        pub auto_archive_minutes: Option<u32>,
        /// The thread's slow mode delay in seconds, up to 6 hours
        #[cfg_attr(feature = "validator", validate(range(min = 0, max = 21600)))]
        pub slowmode: Option<u64>,
        /// First message of a forum post (forum channels only)
        pub message: Option<super::DataMessageSend>,
        /// Ids of forum tags to apply (forum channels only)
        #[cfg_attr(feature = "validator", validate(length(max = 5)))]
        #[cfg_attr(feature = "serde", serde(default))]
        pub applied_tags: Vec<String>,
    }

    /// Options when listing archived threads
    #[cfg_attr(feature = "rocket", derive(FromForm))]
    pub struct OptionsArchivedThreads {
        /// List private threads instead of public ones
        pub private: Option<bool>,
        /// Only list threads archived before this timestamp
        pub before: Option<String>,
        /// Maximum number of threads to return (1-100, default 50)
        pub limit: Option<i64>,
    }

    /// Options when searching forum posts
    #[cfg_attr(feature = "rocket", derive(FromForm))]
    pub struct OptionsThreadSearch {
        /// Only return posts with all of these tag ids
        pub tags: Option<Vec<String>>,
        /// Sort order
        pub sort: Option<ForumSortOrder>,
        /// Include archived posts
        pub archived: Option<bool>,
        /// Only return posts ordered before this id
        pub before: Option<String>,
        /// Maximum number of posts to return (1-100, default 25)
        pub limit: Option<i64>,
    }

    /// List of threads, with our memberships
    pub struct ThreadList {
        /// Threads
        pub threads: Vec<Channel>,
        /// Our memberships of the listed threads
        pub members: Vec<ThreadMember>,
        /// Whether there are more threads to fetch
        pub has_more: bool,
        /// First messages of the listed forum posts (only when searching a forum)
        #[cfg_attr(
            feature = "serde",
            serde(default, skip_serializing_if = "Vec::is_empty")
        )]
        pub messages: Vec<super::Message>,
        /// Authors of the listed posts and their first messages (only when searching a forum)
        #[cfg_attr(
            feature = "serde",
            serde(default, skip_serializing_if = "Vec::is_empty")
        )]
        pub users: Vec<super::User>,
    }

    /// Composite primary key of a thread member
    #[derive(Hash, Default)]
    pub struct ThreadMemberCompositeKey {
        /// Thread Id
        pub thread: String,
        /// User Id
        pub user: String,
    }

    /// Membership of a thread
    pub struct ThreadMember {
        /// Unique member id
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: ThreadMemberCompositeKey,
        /// Time at which this user joined the thread
        pub joined_at: Timestamp,
        /// Notification setting for this thread
        #[cfg_attr(feature = "serde", serde(default))]
        pub notify: ThreadNotify,
    }

    /// Notification setting for a thread
    #[derive(Default)]
    pub enum ThreadNotify {
        /// Use the parent channel's setting
        #[default]
        Default,
        /// Notify on all messages
        All,
        /// Notify only on mentions
        Mentions,
        /// Never notify
        None,
    }

    /// Change our thread membership settings
    pub struct DataEditThreadMember {
        /// Notification setting
        pub notify: Option<ThreadNotify>,
    }

    pub struct ChannelSlowmode {
        pub channel_id: String,
        pub duration: u64,
        pub retry_after: u64,
    }
);

/// Default skip threshold for a DJ channel (half of the present members)
pub const DJ_DEFAULT_SKIP_THRESHOLD: f32 = 0.5;
/// Default maximum upload size for a DJ track, in megabytes
pub const DJ_DEFAULT_MAX_FILE_MB: u32 = 50;
/// Default maximum duration of a DJ track, in seconds
pub const DJ_DEFAULT_MAX_TRACK_SECONDS: u32 = 900;
/// Default maximum queue depth of a DJ channel
pub const DJ_DEFAULT_MAX_TRACKS: u32 = 100;
/// Default maximum total size of queued audio for a DJ channel, in bytes (20 GiB)
pub const DJ_DEFAULT_MAX_TOTAL_BYTES: u64 = 20 * 1024 * 1024 * 1024;

/// DJ information for a channel
///
/// Presence of this field marks the channel as a DJ channel. This is modelled
/// as a capability on a text channel (like voice) so that the `channel_type`
/// enum stays unchanged for already-released clients.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
#[cfg_attr(feature = "schemas", derive(JsonSchema))]
#[derive(Debug, Clone, PartialEq)]
pub struct DjInformation {
    /// Fraction of present members required to skip the current track (0.0 - 1.0)
    pub skip_threshold: f32,
    /// Maximum size of a single upload, in megabytes
    pub max_file_mb: u32,
    /// Maximum duration of a single track, in seconds
    pub max_track_seconds: u32,
    /// Maximum amount of tracks queued at once
    pub max_tracks: u32,
    /// Maximum total amount of bytes stored for this channel
    pub max_total_bytes: u64,
    /// Maximum amount of users which may join at once
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub max_users: Option<u32>,
    /// Whether the DJ is currently enabled
    pub enabled: bool,
}

impl Default for DjInformation {
    fn default() -> Self {
        Self {
            skip_threshold: DJ_DEFAULT_SKIP_THRESHOLD,
            max_file_mb: DJ_DEFAULT_MAX_FILE_MB,
            max_track_seconds: DJ_DEFAULT_MAX_TRACK_SECONDS,
            max_tracks: DJ_DEFAULT_MAX_TRACKS,
            max_total_bytes: DJ_DEFAULT_MAX_TOTAL_BYTES,
            max_users: None,
            enabled: true,
        }
    }
}

// `f32` is not `Eq`, but `DjInformation` is only ever compared with `PartialEq`.
impl Eq for DjInformation {}

/// Partial update of the settings of a DJ channel
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schemas", derive(JsonSchema))]
#[cfg_attr(feature = "validator", derive(validator::Validate))]
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DataEditDjSettings {
    /// Fraction of present members required to skip the current track (0.0 - 1.0)
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub skip_threshold: Option<f32>,
    /// Maximum size of a single upload, in megabytes
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub max_file_mb: Option<u32>,
    /// Maximum duration of a single track, in seconds
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub max_track_seconds: Option<u32>,
    /// Maximum amount of tracks queued at once
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub max_tracks: Option<u32>,
    /// Maximum total amount of bytes stored for this channel
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub max_total_bytes: Option<u64>,
    /// Maximum amount of users which may join at once (0 removes the limit)
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub max_users: Option<u32>,
    /// Whether the DJ is enabled
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub enabled: Option<bool>,
}

impl Eq for DataEditDjSettings {}

impl Channel {
    /// Get a reference to this channel's id
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

    /// Get a reference to this channel's server id
    pub fn server(&self) -> Option<&str> {
        match self {
            Channel::TextChannel { server, .. }
            | Channel::ForumChannel { server, .. }
            | Channel::Thread { server, .. } => Some(server),
            _ => None,
        }
    }

    /// Gets this channel's DJ information, if it is a DJ channel
    pub fn dj(&self) -> Option<&DjInformation> {
        match self {
            Channel::TextChannel { dj: Some(dj), .. } => Some(dj),
            _ => None,
        }
    }

    /// Whether this channel is a DJ channel which is currently enabled
    pub fn is_dj_channel(&self) -> bool {
        self.dj().is_some_and(|dj| dj.enabled)
    }

    /// Gets this channel's voice information, if it is a voice channel
    pub fn voice(&self) -> Option<&VoiceInformation> {
        match self {
            Channel::TextChannel {
                voice: Some(voice), ..
            } => Some(voice),
            _ => None,
        }
    }

    /// This returns a Result because the recipient name can't be determined here without a db call,
    /// which can't be done since this is models, which can't reference the database crate.
    ///
    /// If it returns None, you need to fetch the name from the db.
    pub fn name(&self) -> Option<&str> {
        match self {
            Channel::DirectMessage { .. } => None,
            Channel::SavedMessages { .. } => Some("Saved Messages"),
            Channel::TextChannel { name, .. }
            | Channel::ForumChannel { name, .. }
            | Channel::Thread { name, .. }
            | Channel::Group { name, .. } => Some(name),
        }
    }
}
