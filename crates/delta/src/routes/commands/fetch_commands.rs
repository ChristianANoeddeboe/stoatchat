use revolt_database::{util::reference::Reference, Database, User};
use revolt_models::v0::Command;
use revolt_result::Result;
use rocket::{serde::json::Json, State};

/// # Fetches bot slash commands for a server
///
/// Returns the registered slash commands for a server, used by clients to
/// populate the `/` autocomplete.
#[openapi(tag = "Servers")]
#[get("/<server_id>")]
pub async fn fetch_commands(
    db: &State<Database>,
    server_id: Reference<'_>,
    _user: User,
) -> Result<Json<Vec<Command>>> {
    let commands = db.fetch_commands_for_server(server_id.id).await?;
    Ok(Json(commands.into_iter().map(Into::into).collect()))
}
