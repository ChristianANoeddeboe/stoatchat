#[macro_use]
extern crate rocket;
#[macro_use]
extern crate revolt_rocket_okapi;
#[macro_use]
extern crate serde_json;

pub mod dj;
pub mod routes;
pub mod util;

use revolt_config::config;
use revolt_database::AMQP;
use revolt_ratelimits::rocket as ratelimiter;
use rocket::{Build, Rocket};
use rocket_cors::{AllowedOrigins, CorsOptions};
use rocket_prometheus::PrometheusMetrics;
use std::net::Ipv4Addr;
use std::str::FromStr;

use revolt_database::voice::VoiceClient;
use rocket::data::ToByteUnit;

pub async fn web() -> Rocket<Build> {
    // Get settings
    let config = config().await;

    // Ensure environment variables are present
    config.preflight_checks();

    // Setup database
    let db = revolt_database::DatabaseInfo::Auto.connect().await.unwrap();
    db.migrate_database().await.unwrap();

    // Configure CORS
    let cors = CorsOptions {
        allowed_origins: AllowedOrigins::All,
        allowed_methods: [
            "Get", "Put", "Post", "Delete", "Options", "Head", "Trace", "Connect", "Patch",
        ]
        .iter()
        .map(|s| FromStr::from_str(s).unwrap())
        .collect(),
        expose_headers: [
            "X-Ratelimit-Limit",
            "X-Ratelimit-Bucket",
            "X-Ratelimit-Remaining",
            "X-Ratelimit-Reset-After",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        ..Default::default()
    }
    .to_cors()
    .expect("Failed to create CORS.");

    // Configure Swagger
    let swagger = revolt_rocket_okapi::swagger_ui::make_swagger_ui(
        &revolt_rocket_okapi::swagger_ui::SwaggerUIConfig {
            url: "/openapi.json".to_owned(),
            ..Default::default()
        },
    )
    .into();

    // Voice handler
    let voice_client = VoiceClient::new(config.api.livekit.nodes.clone());
    // Configure Rabbit

    let amqp = AMQP::new_auto().await;

    // Launch background task workers
    revolt_database::tasks::start_workers(db.clone(), amqp.clone());

    // DJ playback engine (single instance, no durable state)
    let dj_engine = dj::DjEngine::new(config.hosts.api.clone());
    dj_engine.wipe_all().await;
    dj_engine.spawn_sweeper();
    // Membership is not heartbeat based: reconcile against live sockets so a
    // socket which dies uncleanly does not leave a phantom member behind.
    dj::spawn_reconciler(dj_engine.clone(), db.clone());

    if !dj::transcode::check_available().await {
        log::error!("DJ channel uploads are unavailable until ffmpeg is installed.");
    }

    // YouTube links need yt-dlp on top of ffmpeg. Not fatal: uploads and
    // playback keep working, only link queueing is refused.
    if !dj::youtube::check_available().await {
        log::error!("DJ YouTube links are unavailable until yt-dlp is installed.");
    }

    // Configure Rocket
    let rocket = rocket::build();
    let prometheus = PrometheusMetrics::new();

    // Ratelimits
    let ratelimits = ratelimiter::RatelimitStorage::new(util::ratelimits::DeltaRatelimits);

    routes::mount(config, rocket)
        .attach(prometheus.clone())
        .mount("/metrics", prometheus)
        .mount("/", rocket_cors::catch_all_options_routes())
        .mount("/", ratelimiter::routes())
        .mount("/swagger/", swagger)
        .manage(db)
        .manage(amqp)
        .manage(cors.clone())
        .manage(voice_client)
        .manage(dj_engine)
        .manage(ratelimits)
        .attach(ratelimiter::RatelimitFairing)
        .attach(cors)
        .configure(rocket::Config {
            limits: rocket::data::Limits::default()
                .limit("string", 5.megabytes())
                // DJ uploads are capped per channel in the handler; this is
                // the hard ceiling for a single multipart body
                .limit("file", 128.megabytes())
                .limit("data-form", 128.megabytes()),
            address: Ipv4Addr::new(0, 0, 0, 0).into(),
            port: 14702,
            ip_header: Some("X-Forwarded-For".into()),
            ..Default::default()
        })
}

#[launch]
async fn rocket() -> _ {
    // Configure logging and environment
    revolt_config::configure!(api);

    // Start web server
    web().await
}
