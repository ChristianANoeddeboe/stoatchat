use revolt_database::{Bot, Database, User};
use revolt_models::v0;
use revolt_result::{create_error, Result};
use rocket::serde::json::Json;
use rocket::State;
use validator::Validate;

/// # Create Bot
///
/// Create a new Revolt bot.
///
/// If the caller is a human user, the new bot is owned by that user.
/// If the caller is itself a bot (authenticated via `x-bot-token`),
/// the new bot is created on behalf of that bot's human owner, so it
/// appears in the owner's bot list and counts against the owner's bot
/// limit. The created bot's token is returned so the calling bot can
/// use it.
#[openapi(tag = "Bots")]
#[post("/create", data = "<info>")]
pub async fn create_bot(
    db: &State<Database>,
    user: User,
    info: Json<v0::DataCreateBot>,
) -> Result<Json<v0::BotWithUserResponse>> {
    let info = info.into_inner();
    info.validate().map_err(|error| {
        create_error!(FailedValidation {
            error: error.to_string()
        })
    })?;

    // If the requester is a bot, resolve and use its human owner so the
    // newly created bot is attributed to a real user (which also keeps
    // `Bot::create`'s IsBot invariant intact).
    let (bot, bot_user) = if user.bot.is_some() {
        let bot = db.fetch_bot(&user.id).await?;
        let owner = db.fetch_user(&bot.owner).await?;
        Bot::create(db, info.name, &owner, None).await?
    } else {
        Bot::create(db, info.name, &user, None).await?
    };

    Ok(Json(v0::BotWithUserResponse {
        bot: bot.into(),
        user: bot_user.into_self(false).await,
    }))
}

#[cfg(test)]
mod test {
    use crate::{rocket, util::test::TestHarness};
    use revolt_models::v0;
    use rocket::http::{ContentType, Header, Status};

    #[rocket::async_test]
    async fn create_bot() {
        let harness = TestHarness::new().await;
        let (_, session, _) = harness.new_user().await;

        let response = harness
            .client
            .post("/bots/create")
            .header(Header::new("x-session-token", session.token.to_string()))
            .header(ContentType::JSON)
            .body(
                json!(v0::DataCreateBot {
                    name: TestHarness::rand_string(),
                })
                .to_string(),
            )
            .dispatch()
            .await;

        assert_eq!(response.status(), Status::Ok);

        let bot: v0::Bot = response.into_json().await.expect("`Bot`");
        assert!(harness.db.fetch_bot(&bot.id).await.is_ok());
    }
}
