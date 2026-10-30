//! Provider usage limits that a bot reports about the inference account it runs on.
//!
//! Hermes (and any other self-reporting bot) publishes this block through
//! `PATCH /bots/<id>` so the bot's profile can show its remaining quota and
//! when each window resets. Every field is optional-ish: a provider that exposes
//! no usage API leaves `windows` empty and explains itself in `notes`.

auto_derived!(
    /// A single usage window: a provider quota (percent based) or one of the
    /// bot's own consumption windows (tokens/calls/spend).
    pub struct BotUsageWindow {
        /// Human readable label, e.g. "Current week"
        pub label: String,
        /// Machine readable period: session, rolling, daily, weekly, monthly, other
        #[serde(skip_serializing_if = "String::is_empty", default)]
        pub period: String,

        /// Percent of the window consumed (0-100), absent when the provider
        /// does not report a percentage
        #[serde(skip_serializing_if = "Option::is_none")]
        pub used_percent: Option<u16>,
        /// RFC3339 instant at which this window resets
        #[serde(skip_serializing_if = "Option::is_none")]
        pub resets_at: Option<String>,
        /// Free-form detail, e.g. "$8.54 of $10.00 remaining"
        #[serde(skip_serializing_if = "Option::is_none")]
        pub detail: Option<String>,

        /// Tokens consumed inside this window (bot-local windows)
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tokens: Option<u64>,
        /// API calls made inside this window (bot-local windows)
        #[serde(skip_serializing_if = "Option::is_none")]
        pub calls: Option<u64>,
        /// Spend inside this window, in US cents (bot-local windows)
        #[serde(skip_serializing_if = "Option::is_none")]
        pub cost_cents: Option<u32>,
    }
);

auto_derived!(
    /// Usage limits as published by the bot itself.
    pub struct BotUsage {
        /// Inference provider the bot runs on, e.g. "opencode-go"
        pub provider: String,
        /// Model the bot currently runs
        #[serde(skip_serializing_if = "String::is_empty", default)]
        pub model: String,
        /// Plan name when the provider reports one
        #[serde(skip_serializing_if = "Option::is_none")]
        pub plan: Option<String>,
        /// RFC3339 instant this block was last refreshed
        pub updated_at: String,

        /// Quota windows reported by the provider (with reset times)
        #[serde(default)]
        pub windows: Vec<BotUsageWindow>,
        /// The bot's own recent consumption (today / week / month)
        #[serde(default)]
        pub local: Vec<BotUsageWindow>,
        /// Extra notes, e.g. "limits unavailable for this provider"
        #[serde(default)]
        pub notes: Vec<String>,
    }
);
