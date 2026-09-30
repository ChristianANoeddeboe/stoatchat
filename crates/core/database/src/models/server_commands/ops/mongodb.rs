use futures::StreamExt;
use revolt_result::Result;

use crate::{Command, FieldsCommand, IntoDocumentPath, MongoDb, PartialCommand};

use super::AbstractCommands;

static COL: &str = "server_commands";

#[async_trait]
impl AbstractCommands for MongoDb {
    /// Insert new command into the database
    async fn insert_command(&self, command: &Command) -> Result<()> {
        query!(self, insert_one, COL, &command).map(|_| ())
    }

    /// Fetch command by id
    async fn fetch_command(&self, command_id: &str) -> Result<Command> {
        query!(self, find_one_by_id, COL, command_id)?.ok_or_else(|| create_error!(NotFound))
    }

    /// Fetch commands for a server
    async fn fetch_commands_for_server(&self, server_id: &str) -> Result<Vec<Command>> {
        Ok(self
            .col::<Command>(COL)
            .find(doc! {
                "server_id": server_id,
            })
            .await
            .map_err(|_| create_database_error!("find", COL))?
            .filter_map(|s| async {
                if cfg!(debug_assertions) {
                    Some(s.unwrap())
                } else {
                    s.ok()
                }
            })
            .collect()
            .await)
    }

    /// Update command with new information
    async fn update_command(
        &self,
        command_id: &str,
        partial: &PartialCommand,
        remove: &[FieldsCommand],
    ) -> Result<()> {
        query!(
            self,
            update_one_by_id,
            COL,
            command_id,
            partial,
            remove.iter().map(|x| x as &dyn IntoDocumentPath).collect(),
            None
        )
        .map(|_| ())
    }

    /// Delete command by id
    async fn delete_command(&self, command_id: &str) -> Result<()> {
        query!(self, delete_one_by_id, COL, command_id).map(|_| ())
    }
}

impl IntoDocumentPath for FieldsCommand {
    fn as_path(&self) -> Option<&'static str> {
        Some(match self {
            FieldsCommand::Name => "name",
            FieldsCommand::Description => "description",
        })
    }
}
