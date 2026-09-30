use revolt_database::{util::reference::Reference, Database, User};
use revolt_result::{create_error, Result};
use rocket::State;

/// # Deletes a bot slash command on a server
///
/// Only the owning bot (or its owner) may delete a command.
#[openapi(tag = "Servers")]
#[delete("/<server_id>/<command_name>")]
pub async fn delete_command(
    db: &State<Database>,
    user: User,
    server_id: Reference<'_>,
    command_name: String,
) -> Result<()> {
    let name = command_name.trim_start_matches('/').to_string();

    let commands = db.fetch_commands_for_server(server_id.id).await?;
    let command = commands
        .into_iter()
        .find(|command| command.name == name)
        .ok_or_else(|| create_error!(NotFound))?;

    // Only the owning bot may delete the command.
    if command.owner_id != user.id {
        return Err(create_error!(NotFound));
    }

    db.delete_command(&command.id).await
}
