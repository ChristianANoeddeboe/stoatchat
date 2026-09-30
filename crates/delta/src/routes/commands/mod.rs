use revolt_rocket_okapi::revolt_okapi::openapi3::OpenApi;
use rocket::Route;

mod create_command;
mod delete_command;
mod fetch_commands;

pub fn routes() -> (Vec<Route>, OpenApi) {
    openapi_get_routes_spec![
        create_command::create_command,
        fetch_commands::fetch_commands,
        delete_command::delete_command,
    ]
}
