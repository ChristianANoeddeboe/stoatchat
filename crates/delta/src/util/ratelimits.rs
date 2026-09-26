use revolt_ratelimits::ratelimiter::RatelimitResolver;
use rocket::{http::Method, Request};

pub struct DeltaRatelimits;

impl<'a> RatelimitResolver<Request<'a>> for DeltaRatelimits {
    fn resolve_bucket<'r>(&self, request: &'r Request<'_>) -> (&'r str, Option<&'r str>) {
        let (segment, resource, extra) = if request.routed_segment(0) == Some("0.8") {
            (
                request.routed_segment(1),
                request.routed_segment(2),
                request.routed_segment(3),
            )
        } else {
            (
                request.routed_segment(0),
                request.routed_segment(1),
                request.routed_segment(2),
            )
        };

        if let Some(segment) = segment {
            #[allow(clippy::redundant_locals)]
            let resource = resource;

            let method = request.method();
            match (segment, resource, method) {
                ("users", target, Method::Patch) => ("user_edit", target),
                ("users", _, _) => {
                    if let Some("default_avatar") = extra {
                        return ("default_avatar", None);
                    }

                    ("users", None)
                }
                ("bots", _, _) => ("bots", None),
                ("channels", Some(id), _) => {
                    if request.method() == Method::Post {
                        if let Some("messages") = extra {
                            return ("messaging", Some(id));
                        }
                    }

                    ("channels", Some(id))
                }
                ("servers", Some(id), _) => ("servers", Some(id)),
                ("auth", _, _) => {
                    if request.method() == Method::Delete {
                        ("auth_delete", None)
                    } else {
                        ("auth", None)
                    }
                }
                ("swagger", _, _) => ("swagger", None),
                ("safety", Some("report"), _) => ("safety_report", Some("report")),
                ("safety", _, _) => ("safety", None),
                _ => ("any", None),
            }
        } else {
            ("any", None)
        }
    }

    fn resolve_bucket_limit(&self, bucket: &str) -> u32 {
        match bucket {
            "user_edit" => 2,
            "users" => 20,
            "bots" => 10,
            "messaging" => 10,
            "channels" => 15,
            "servers" => 5,
            "auth" => 15,
            "auth_delete" => 255,
            "default_avatar" => 255,
            "swagger" => 100,
            "safety" => 15,
            "safety_report" => 3,
            _ => 20,
        }
    }
}

#[cfg(test)]
mod test {
    use crate::util::test::TestHarness;
    use revolt_database::{Bot, BotInformation, User};
    use rocket::http::{Header, Status};

    /// Matches `api.ratelimits.boosted_bots` in Revolt.test.toml
    const BOOSTED_BOT: &str = "01J00000000000000000B00STD";

    /// Send requests until ratelimited, returns how many succeeded
    async fn requests_until_limited(harness: &TestHarness, token: &str) -> usize {
        for i in 0..1000 {
            let response = harness
                .client
                .get("/users/@me")
                .header(Header::new("x-bot-token", token.to_string()))
                .dispatch()
                .await;

            if response.status() == Status::TooManyRequests {
                return i;
            }

            assert_eq!(response.status(), Status::Ok);
        }

        1000
    }

    #[rocket::async_test]
    async fn bots_have_own_buckets() {
        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;

        let (first, _) = Bot::create(&harness.db, TestHarness::rand_string(), &owner, None)
            .await
            .expect("`Bot`");
        let (second, _) = Bot::create(&harness.db, TestHarness::rand_string(), &owner, None)
            .await
            .expect("`Bot`");

        // Previously every bot on the same IP shared one bucket
        assert_eq!(requests_until_limited(&harness, &first.token).await, 20);
        assert_eq!(requests_until_limited(&harness, &second.token).await, 20);
    }

    #[rocket::async_test]
    async fn boosted_bot_gets_multiplied_limit() {
        let harness = TestHarness::new().await;
        let (_, _, owner) = harness.new_user().await;

        let token = format!("boosted-{}", TestHarness::rand_string());
        harness
            .db
            .insert_user(&User {
                id: BOOSTED_BOT.to_string(),
                username: TestHarness::rand_string(),
                discriminator: "0001".to_string(),
                bot: Some(BotInformation {
                    owner: owner.id.clone(),
                }),
                ..Default::default()
            })
            .await
            .ok();
        harness.db.delete_bot(BOOSTED_BOT).await.ok();
        harness
            .db
            .insert_bot(&Bot {
                id: BOOSTED_BOT.to_string(),
                owner: owner.id,
                token: token.clone(),
                ..Default::default()
            })
            .await
            .expect("`Bot`");

        assert_eq!(requests_until_limited(&harness, &token).await, 20 * 20);
    }
}
