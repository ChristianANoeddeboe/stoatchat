use async_trait::async_trait;
use log::info;
use rocket::fairing::{Fairing, Info, Kind};
use rocket::http::uri::Origin;
use rocket::http::{Method, Status};
use rocket::request::{FromRequest, Outcome};
use rocket::serde::json::Json;
use rocket::{Data, Request, Response, State};

use revolt_rocket_okapi::r#gen::OpenApiGenerator;
use revolt_rocket_okapi::request::{OpenApiFromRequest, RequestHeaderInput};
use revolt_config::config;
use revolt_database::{Session, User, util::ip::rocket::to_real_ip};

use crate::ratelimiter::RequestKind;
use crate::ratelimiter::{RatelimitInformation, Ratelimiter};

#[derive(Clone, Copy)]
pub struct RocketRequestKind;

impl RequestKind for RocketRequestKind {
    type R<'a> = Request<'a>;
}

pub type RatelimitStorage = crate::ratelimiter::RatelimitStorage<RocketRequestKind>;

#[async_trait]
impl<'r> FromRequest<'r> for Ratelimiter {
    type Error = Ratelimiter;

    async fn from_request<'a>(request: &'r rocket::Request<'a>) -> Outcome<Self, Self::Error> {
        let ratelimiter = request
            .local_cache_async(async {
                use rocket::outcome::Outcome;

                let storage = request.guard::<&State<RatelimitStorage>>().await.unwrap();

                let (bucket, resource) = storage.resolver.resolve_bucket(request);
                let mut limit = storage.resolver.resolve_bucket_limit(bucket);

                // Bots authenticate with a token rather than a session, so bucket them by bot id.
                // The `User` guard is cached per request, so routes don't look the bot up again.
                let bot_id = if request.headers().contains("x-bot-token") {
                    match request.guard::<User>().await {
                        Outcome::Success(user) if user.bot.is_some() => Some(user.id),
                        _ => None,
                    }
                } else {
                    None
                };

                let identifier = if let Some(bot_id) = bot_id {
                    let multiplier = config().await.api.ratelimits.bot_multiplier(&bot_id);
                    limit = limit.saturating_mul(multiplier);
                    bot_id
                } else if let Outcome::Success(session) = request.guard::<Session>().await {
                    session.id
                } else {
                    to_real_ip(request).await
                };

                Ratelimiter::from(&storage.map, &identifier, limit, (bucket, resource))
            })
            .await;

        match ratelimiter {
            Ok(ratelimiter) => Outcome::Success(*ratelimiter),
            Err(ratelimiter) => Outcome::Error((Status::TooManyRequests, *ratelimiter)),
        }
    }
}

impl OpenApiFromRequest<'_> for Ratelimiter {
    fn from_request_input(
        _gen: &mut OpenApiGenerator,
        _name: String,
        _required: bool,
    ) -> revolt_rocket_okapi::Result<RequestHeaderInput> {
        Ok(RequestHeaderInput::None)
    }
}

/// Attach ratelimiter to the Rocket application
pub struct RatelimitFairing;

#[async_trait]
impl Fairing for RatelimitFairing {
    fn info(&self) -> Info {
        Info {
            name: "Ratelimiter",
            kind: Kind::Request | Kind::Response,
        }
    }

    async fn on_request(&self, request: &mut Request<'_>, _: &mut Data<'_>) {
        use rocket::outcome::Outcome;
        if let Outcome::Error(_) = request.guard::<Ratelimiter>().await {
            info!(
                "User rate-limited on route {}! (IP = {:?})",
                request.uri(),
                to_real_ip(request).await
            );

            request.set_method(Method::Get);
            request.set_uri(Origin::parse("/ratelimit").unwrap())
        }
    }

    async fn on_response<'r>(&self, request: &'r Request<'_>, response: &mut Response<'r>) {
        let guard = request.guard::<Ratelimiter>().await;
        let (Outcome::Success(ratelimiter) | Outcome::Error((_, ratelimiter))) = guard else {
            unreachable!()
        };
        let Ratelimiter {
            key,
            limit,
            remaining,
            reset,
        } = ratelimiter;

        response.set_raw_header("X-RateLimit-Limit", limit.to_string());
        response.set_raw_header("X-RateLimit-Bucket", key.to_string());
        response.set_raw_header("X-RateLimit-Remaining", remaining.to_string());
        response.set_raw_header("X-RateLimit-Reset-After", reset.to_string());

        if guard.is_error() {
            response.set_status(Status::TooManyRequests);
        }
    }
}

#[async_trait]
impl<'r> FromRequest<'r> for RatelimitInformation {
    type Error = u128;

    async fn from_request(request: &'r rocket::Request<'_>) -> Outcome<Self, Self::Error> {
        let info = match request.guard::<Ratelimiter>().await {
            Outcome::Success(ratelimiter) => RatelimitInformation::Success(ratelimiter),
            Outcome::Error((_, ratelimiter)) => RatelimitInformation::Failure {
                retry_after: ratelimiter.reset,
            },
            _ => unreachable!(),
        };
        Outcome::Success(info)
    }
}

#[rocket::get("/ratelimit")]
fn ratelimit_info(info: RatelimitInformation) -> Json<RatelimitInformation> {
    Json(info)
}

pub fn routes() -> Vec<rocket::Route> {
    rocket::routes![ratelimit_info]
}
