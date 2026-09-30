use revolt_result::Result;

use crate::{Command, FieldsCommand, PartialCommand, ReferenceDb};

use super::AbstractCommands;

#[async_trait]
impl AbstractCommands for ReferenceDb {
    /// Insert new command into the database
    async fn insert_command(&self, command: &Command) -> Result<()> {
        let mut commands = self.server_commands.lock().await;
        if commands.contains_key(&command.id) {
            Err(create_database_error!("insert", "server_command"))
        } else {
            commands.insert(command.id.to_string(), command.clone());
            Ok(())
        }
    }

    /// Fetch command by id
    async fn fetch_command(&self, command_id: &str) -> Result<Command> {
        let commands = self.server_commands.lock().await;
        commands
            .get(command_id)
            .cloned()
            .ok_or_else(|| create_error!(NotFound))
    }

    /// Fetch commands for a server
    async fn fetch_commands_for_server(&self, server_id: &str) -> Result<Vec<Command>> {
        let commands = self.server_commands.lock().await;
        Ok(commands
            .values()
            .filter(|command| command.server_id.as_deref() == Some(server_id))
            .cloned()
            .collect())
    }

    /// Update command with new information
    async fn update_command(
        &self,
        command_id: &str,
        partial: &PartialCommand,
        remove: &[FieldsCommand],
    ) -> Result<()> {
        let mut commands = self.server_commands.lock().await;
        if let Some(command) = commands.get_mut(command_id) {
            for field in remove {
                command.remove_field(field);
            }

            command.apply_options(partial.clone());
            Ok(())
        } else {
            Err(create_error!(NotFound))
        }
    }

    /// Delete command by id
    async fn delete_command(&self, command_id: &str) -> Result<()> {
        let mut commands = self.server_commands.lock().await;
        if commands.remove(command_id).is_some() {
            Ok(())
        } else {
            Err(create_error!(NotFound))
        }
    }
}
