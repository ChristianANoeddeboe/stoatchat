use revolt_result::Result;

use crate::{Command, FieldsCommand, PartialCommand};

#[cfg(feature = "mongodb")]
mod mongodb;
mod reference;

#[async_trait]
pub trait AbstractCommands: Sync + Send {
    /// Insert new command into the database
    async fn insert_command(&self, command: &Command) -> Result<()>;

    /// Fetch command by id
    async fn fetch_command(&self, command_id: &str) -> Result<Command>;

    /// Fetch commands for a server
    async fn fetch_commands_for_server(&self, server_id: &str) -> Result<Vec<Command>>;

    /// Update command with new information
    async fn update_command(
        &self,
        command_id: &str,
        partial: &PartialCommand,
        remove: &[FieldsCommand],
    ) -> Result<()>;

    /// Delete command by id
    async fn delete_command(&self, command_id: &str) -> Result<()>;
}
