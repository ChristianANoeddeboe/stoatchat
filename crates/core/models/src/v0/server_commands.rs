#[cfg(feature = "validator")]
use validator::Validate;

auto_derived_partial!(
    /// Bot slash command registered on a server
    pub struct Command {
        /// Command Id
        pub id: String,

        /// The name of the command (without leading slash)
        pub name: String,

        /// Short description of what the command does
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub description: Option<String>,

        /// The server this command is registered on (None = global)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
        pub server_id: Option<String>,

        /// Bot user that owns this command
        pub owner_id: String,
    },
    "PartialCommand"
);

auto_derived!(
    /// Data to create or update a command
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataCreateCommand {
        /// Short description of what the command does
        #[cfg_attr(feature = "validator", validate(length(min = 0, max = 100)))]
        pub description: Option<String>,
    }

    /// Optional fields on command object
    pub enum FieldsCommand {
        Name,
        Description,
    }
);
