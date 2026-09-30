use revolt_result::Result;

use crate::Database;

auto_derived_partial!(
    /// Bot slash command registered on a server
    pub struct Command {
        /// Command Id
        #[serde(rename = "_id")]
        pub id: String,

        /// The name of the command (without leading slash)
        pub name: String,

        /// Short description of what the command does
        #[serde(skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,

        /// The server this command is registered on (None = global)
        #[serde(skip_serializing_if = "Option::is_none")]
        pub server_id: Option<String>,

        /// Bot user that owns this command
        pub owner_id: String,
    },
    "PartialCommand"
);

auto_derived!(
    /// Optional fields on command object
    pub enum FieldsCommand {
        Name,
        Description,
    }
);

#[allow(clippy::derivable_impls)]
impl Default for Command {
    fn default() -> Self {
        Self {
            id: Default::default(),
            name: Default::default(),
            description: None,
            server_id: None,
            owner_id: Default::default(),
        }
    }
}

impl Command {
    pub fn remove_field(&mut self, field: &FieldsCommand) {
        match field {
            FieldsCommand::Name => self.name = Default::default(),
            FieldsCommand::Description => self.description = None,
        }
    }

    pub async fn create(&self, db: &Database) -> Result<()> {
        db.insert_command(self).await
    }

    pub async fn update(
        &mut self,
        db: &Database,
        partial: PartialCommand,
        remove: Vec<FieldsCommand>,
    ) -> Result<()> {
        for field in &remove {
            self.remove_field(field)
        }

        self.apply_options(partial.clone());
        db.update_command(&self.id, &partial, &remove).await
    }

    pub async fn delete(&self, db: &Database) -> Result<()> {
        db.delete_command(&self.id).await
    }
}
