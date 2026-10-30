use super::User;

auto_derived!(
    /// Bot
    #[derive(Default)]
    pub struct Bot {
        /// Bot Id
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,

        /// User Id of the bot owner
        #[cfg_attr(feature = "serde", serde(rename = "owner"))]
        pub owner_id: String,
        /// Token used to authenticate requests for this bot
        pub token: String,
        /// Whether the bot is public
        /// (may be invited by anyone)
        pub public: bool,

        /// Whether to enable analytics
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "crate::if_false", default)
        )]
        pub analytics: bool,
        /// Whether this bot should be publicly discoverable
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "crate::if_false", default)
        )]
        pub discoverable: bool,
        /// Reserved; URL for handling interactions
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "String::is_empty", default)
        )]
        pub interactions_url: String,
        /// URL for terms of service
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "String::is_empty", default)
        )]
        pub terms_of_service_url: String,
        /// URL for privacy policy
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "String::is_empty", default)
        )]
        pub privacy_policy_url: String,

        /// Enum of bot flags
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "crate::if_zero_u32", default)
        )]
        pub flags: u32,

        /// Usage limits this bot reports for its inference account
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub usage: Option<BotUsage>,
    }

    /// A single usage window: a provider quota or one of the bot's own
    /// consumption windows.
    #[derive(Default)]
    pub struct BotUsageWindow {
        /// Human readable label, e.g. "Current week"
        pub label: String,
        /// Machine readable period: session, rolling, daily, weekly, monthly, other
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "String::is_empty", default)
        )]
        pub period: String,

        /// Percent of the window consumed (0-100)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub used_percent: Option<u16>,
        /// RFC3339 instant at which this window resets
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub resets_at: Option<String>,
        /// Free-form detail, e.g. "$8.54 of $10.00 remaining"
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub detail: Option<String>,

        /// Tokens consumed inside this window (bot-local windows)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub tokens: Option<u64>,
        /// API calls made inside this window (bot-local windows)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub calls: Option<u64>,
        /// Spend inside this window, in US cents (bot-local windows)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub cost_cents: Option<u32>,
    }

    /// Usage limits as published by the bot itself.
    #[derive(Default)]
    pub struct BotUsage {
        /// Inference provider the bot runs on, e.g. "opencode-go"
        pub provider: String,
        /// Model the bot currently runs
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "String::is_empty", default)
        )]
        pub model: String,
        /// Plan name when the provider reports one
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub plan: Option<String>,
        /// RFC3339 instant this block was last refreshed
        pub updated_at: String,

        /// Quota windows reported by the provider (with reset times)
        #[cfg_attr(feature = "serde", serde(default))]
        pub windows: Vec<BotUsageWindow>,
        /// The bot's own recent consumption (today / week / month)
        #[cfg_attr(feature = "serde", serde(default))]
        pub local: Vec<BotUsageWindow>,
        /// Extra notes, e.g. "limits unavailable for this provider"
        #[cfg_attr(feature = "serde", serde(default))]
        pub notes: Vec<String>,
    }

    /// Optional fields on bot object
    pub enum FieldsBot {
        Token,
        InteractionsURL,
    }

    /// Flags that may be attributed to a bot
    #[repr(u32)]
    pub enum BotFlags {
        Verified = 1,
        Official = 2,
    }

    /// Public Bot
    pub struct PublicBot {
        /// Bot Id
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,

        /// Bot Username
        pub username: String,
        /// Profile Avatar
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "String::is_empty", default)
        )]
        pub avatar: String,
        /// Profile Description
        #[cfg_attr(
            feature = "serde",
            serde(skip_serializing_if = "String::is_empty", default)
        )]
        pub description: String,
    }

    /// Bot Response
    pub struct FetchBotResponse {
        /// Bot object
        pub bot: Bot,
        /// User object
        pub user: User,
    }

    /// Bot Details
    #[derive(Default)]
    #[cfg_attr(feature = "validator", derive(validator::Validate))]
    pub struct DataCreateBot {
        /// Bot username
        #[cfg_attr(
            feature = "validator",
            validate(length(min = 2, max = 32), regex = "super::RE_USERNAME")
        )]
        pub name: String,
    }

    /// New Bot Details
    #[derive(Default)]
    #[cfg_attr(feature = "validator", derive(validator::Validate))]
    pub struct DataEditBot {
        /// Bot username
        #[cfg_attr(
            feature = "validator",
            validate(length(min = 2, max = 32), regex = "super::RE_USERNAME")
        )]
        #[serde(skip_serializing_if = "Option::is_none")]
        pub name: Option<String>,
        /// Whether the bot can be added by anyone
        pub public: Option<bool>,
        /// Whether analytics should be gathered for this bot
        ///
        /// Must be enabled in order to show up on [Revolt Discover](https://rvlt.gg).
        pub analytics: Option<bool>,
        /// Interactions URL
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 2048)))]
        pub interactions_url: Option<String>,
        /// Usage limits this bot reports for its inference account
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub usage: Option<BotUsage>,
        /// Fields to remove from bot object
        #[cfg_attr(feature = "serde", serde(default))]
        pub remove: Vec<FieldsBot>,
    }

    /// Where we are inviting a bot to
    #[serde(untagged)]
    pub enum InviteBotDestination {
        /// Invite to a server
        Server {
            /// Server Id
            server: String,
        },
        /// Invite to a group
        Group {
            /// Group Id
            group: String,
        },
    }

    /// Owned Bots Response
    ///
    /// Both lists are sorted by their IDs.
    ///
    /// TODO: user should be in bot object
    pub struct OwnedBotsResponse {
        /// Bot objects
        pub bots: Vec<Bot>,
        /// User objects
        pub users: Vec<User>,
    }

    /// Bot with user response
    pub struct BotWithUserResponse {
        #[serde(flatten)]
        pub bot: Bot,
        pub user: User,
    }
);
