use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    Command, Database, PartialCommand, User,
};
use revolt_models::v0::{Command as V0Command, DataCreateCommand};
use revolt_permissions::{calculate_server_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use ulid::Ulid;
use validator::Validate;

/// # Creates (or updates) a bot slash command on a server
///
/// Registers a slash command for a server. Only bots, or users with
/// ManageServer permission, may register commands. If a command with the
/// same name already exists and is owned by the same bot, it is updated;
/// otherwise a new command is inserted.
#[openapi(tag = "Servers")]
#[put("/<server_id>/<command_name>", data = "<data>")]
pub async fn create_command(
    db: &State<Database>,
    user: User,
    server_id: Reference<'_>,
    command_name: String,
    data: Json<DataCreateCommand>,
) -> Result<Json<V0Command>> {
    let data = data.into_inner();
    data.validate().map_err(|error| {
        create_error!(FailedValidation {
            error: error.to_string()
        })
    })?;

    // Strip a leading slash if provided by a client.
    let name = command_name.trim_start_matches('/').to_string();
    if name.is_empty() {
        return Err(create_error!(InvalidOperation));
    }

    let server = server_id.as_server(db).await?;

    // Only bots, or users with ManageServer permission, may register commands.
    let is_bot = user.bot.is_some();
    if !is_bot {
        let mut query = DatabasePermissionQuery::new(db, &user).server(&server);
        calculate_server_permissions(&mut query)
            .await
            .throw_if_lacking_channel_permission(ChannelPermission::ManageServer)?;
    }

    let owner_id = user.id;
    let commands = db.fetch_commands_for_server(&server.id).await?;

    // Upsert by (server_id, command_name): update if owned by same bot, else insert.
    if let Some(existing) = commands
        .iter()
        .find(|command| command.name == name)
        .cloned()
    {
        if existing.owner_id != owner_id {
            return Err(create_error!(NotFound));
        }

        let mut command = existing;
        let partial = PartialCommand {
            description: data.description,
            ..Default::default()
        };
        command.update(db, partial, vec![]).await?;
        Ok(Json(command.into()))
    } else {
        let command = Command {
            id: Ulid::new().to_string(),
            name: name.clone(),
            description: data.description,
            server_id: Some(server.id.clone()),
            owner_id,
        };
        db.insert_command(&command).await?;
        Ok(Json(command.into()))
    }
}
