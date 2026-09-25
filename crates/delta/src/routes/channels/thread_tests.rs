use crate::{rocket, util::test::TestHarness};
use revolt_database::{
    Channel, Member, MessageFlagsValue, PartialChannel, Server, Session, User,
};
use revolt_models::v0::{self, MessageFlags};
use rocket::http::{ContentType, Header, Status};
use rocket::local::asynchronous::LocalResponse;

async fn post<'a>(
    harness: &'a TestHarness,
    session: &Session,
    url: String,
    body: serde_json::Value,
) -> LocalResponse<'a> {
    harness
        .client
        .post(url)
        .header(Header::new("x-session-token", session.token.clone()))
        .header(ContentType::JSON)
        .body(body.to_string())
        .dispatch()
        .await
}

async fn get<'a>(harness: &'a TestHarness, session: &Session, url: String) -> LocalResponse<'a> {
    harness
        .client
        .get(url)
        .header(Header::new("x-session-token", session.token.clone()))
        .dispatch()
        .await
}

async fn forum(harness: &TestHarness, server: &mut Server) -> Channel {
    Channel::create_server_channel(
        &harness.db,
        server,
        v0::DataCreateServerChannel {
            channel_type: v0::LegacyServerChannelType::Forum,
            name: "Forum".to_string(),
            description: None,
            nsfw: None,
            voice: None,
        },
        true,
    )
    .await
    .expect("forum")
}

async fn join(harness: &TestHarness, server: &Server, user: &User) {
    Member::create(&harness.db, server, user, None)
        .await
        .expect("member");
}

#[rocket::async_test]
async fn thread_from_message() {
    let harness = TestHarness::new().await;
    let (_, session, user) = harness.new_user().await;
    let (_, channels) = harness.new_server(&user).await;
    let channel = &channels[0];

    let response = post(
        &harness,
        &session,
        format!("/channels/{}/messages", channel.id()),
        serde_json::json!({ "content": "Start here" }),
    )
    .await;
    let message: v0::Message = response.into_json().await.expect("message");

    let response = post(
        &harness,
        &session,
        format!("/channels/{}/messages/{}/threads", channel.id(), message.id),
        serde_json::json!({ "name": "  Thread  " }),
    )
    .await;
    assert_eq!(response.status(), Status::Ok);
    let thread: v0::Channel = response.into_json().await.expect("thread");

    let v0::Channel::Thread {
        id,
        parent,
        name,
        owner,
        member_count,
        ..
    } = thread
    else {
        panic!("not a thread");
    };
    assert_eq!(id, message.id);
    assert_eq!(parent, channel.id());
    assert_eq!(name, "Thread");
    assert_eq!(owner, user.id);
    assert_eq!(member_count, 1);

    let message = harness.db.fetch_message(&message.id).await.unwrap();
    assert!(MessageFlagsValue(message.flags.unwrap_or_default()).has(MessageFlags::HasThread));

    // A message can only have one thread
    let response = post(
        &harness,
        &session,
        format!("/channels/{}/messages/{}/threads", channel.id(), message.id),
        serde_json::json!({ "name": "Again" }),
    )
    .await;
    assert_eq!(response.status(), Status::BadRequest);
    drop(response);

    // No threads in threads
    let response = post(
        &harness,
        &session,
        format!("/channels/{}/threads", id),
        serde_json::json!({ "name": "Nested" }),
    )
    .await;
    assert_eq!(response.status(), Status::BadRequest);
    drop(response);

    // Deleting the thread clears the flag
    let thread = harness.db.fetch_channel(&id).await.unwrap();
    thread.delete(&harness.db).await.unwrap();
    let message = harness.db.fetch_message(&message.id).await.unwrap();
    assert!(!MessageFlagsValue(message.flags.unwrap_or_default()).has(MessageFlags::HasThread));
}

#[rocket::async_test]
async fn public_thread_announced() {
    let harness = TestHarness::new().await;
    let (_, session, user) = harness.new_user().await;
    let (_, channels) = harness.new_server(&user).await;
    let channel = &channels[0];

    let response = post(
        &harness,
        &session,
        format!("/channels/{}/threads", channel.id()),
        serde_json::json!({ "name": "Chat", "auto_archive_minutes": 60 }),
    )
    .await;
    assert_eq!(response.status(), Status::Ok);
    let thread: v0::Channel = response.into_json().await.expect("thread");

    // The system message has the thread's id
    let message = harness.db.fetch_message(thread.id()).await.unwrap();
    assert_eq!(message.channel, channel.id());
    assert!(matches!(
        message.system,
        Some(revolt_database::SystemMessage::ThreadCreated { .. })
    ));
    assert!(MessageFlagsValue(message.flags.unwrap_or_default()).has(MessageFlags::HasThread));

    // Bad archive durations are rejected
    let response = post(
        &harness,
        &session,
        format!("/channels/{}/threads", channel.id()),
        serde_json::json!({ "name": "Chat", "auto_archive_minutes": 61 }),
    )
    .await;
    assert_eq!(response.status(), Status::BadRequest);
}

#[rocket::async_test]
async fn private_thread_visibility() {
    let harness = TestHarness::new().await;
    let (_, session, user) = harness.new_user().await;
    let (_, other_session, other) = harness.new_user().await;
    let (server, channels) = harness.new_server(&user).await;
    let channel = &channels[0];
    join(&harness, &server, &other).await;

    let response = post(
        &harness,
        &other_session,
        format!("/channels/{}/threads", channel.id()),
        serde_json::json!({ "name": "Secret", "private": true }),
    )
    .await;
    assert_eq!(response.status(), Status::Ok);
    let thread: v0::Channel = response.into_json().await.expect("thread");

    // No system message for private threads
    assert!(harness.db.fetch_message(thread.id()).await.is_err());

    // A third member can't see it
    let (_, third_session, third) = harness.new_user().await;
    join(&harness, &server, &third).await;

    let response = get(&harness, &third_session, format!("/channels/{}", thread.id())).await;
    assert_eq!(response.status(), Status::Forbidden);
    drop(response);

    let response = get(
        &harness,
        &third_session,
        format!("/channels/{}/threads/active", channel.id()),
    )
    .await;
    let list: v0::ThreadList = response.into_json().await.expect("list");
    assert!(list.threads.is_empty());

    // The owner of the server has ManageThreads
    let response = get(
        &harness,
        &session,
        format!("/channels/{}/threads/active", channel.id()),
    )
    .await;
    let list: v0::ThreadList = response.into_json().await.expect("list");
    assert_eq!(list.threads.len(), 1);

    // Invited members can see it
    let response = harness
        .client
        .put(format!(
            "/channels/{}/thread_members/{}",
            thread.id(),
            third.id
        ))
        .header(Header::new("x-session-token", other_session.token.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::NoContent);
    drop(response);

    let response = get(&harness, &third_session, format!("/channels/{}", thread.id())).await;
    assert_eq!(response.status(), Status::Ok);
    drop(response);

    let response = get(
        &harness,
        &third_session,
        format!("/channels/{}/thread_members", thread.id()),
    )
    .await;
    let members: Vec<v0::ThreadMember> = response.into_json().await.expect("members");
    assert_eq!(members.len(), 2);

    // Leaving hides it again
    let response = harness
        .client
        .delete(format!("/channels/{}/thread_members/@me", thread.id()))
        .header(Header::new("x-session-token", third_session.token.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::NoContent);
    drop(response);

    let response = get(&harness, &third_session, format!("/channels/{}", thread.id())).await;
    assert_eq!(response.status(), Status::Forbidden);
}

#[rocket::async_test]
async fn forum_posts() {
    let harness = TestHarness::new().await;
    let (_, session, user) = harness.new_user().await;
    let (_, other_session, other) = harness.new_user().await;
    let (mut server, _) = harness.new_server(&user).await;
    join(&harness, &server, &other).await;
    let mut forum = forum(&harness, &mut server).await;

    forum
        .update(
            &harness.db,
            PartialChannel {
                available_tags: Some(vec![
                    v0::ForumTag {
                        id: "TAG1".to_string(),
                        name: "Help".to_string(),
                        emoji: None,
                        moderated: false,
                    },
                    v0::ForumTag {
                        id: "TAG2".to_string(),
                        name: "Staff".to_string(),
                        emoji: None,
                        moderated: true,
                    },
                ]),
                require_tag: Some(true),
                ..Default::default()
            },
            vec![],
        )
        .await
        .unwrap();

    // Messages can't be sent straight into a forum
    let response = post(
        &harness,
        &other_session,
        format!("/channels/{}/messages", forum.id()),
        serde_json::json!({ "content": "hi" }),
    )
    .await;
    assert_eq!(response.status(), Status::BadRequest);
    drop(response);

    // A tag is required
    let response = post(
        &harness,
        &other_session,
        format!("/channels/{}/threads", forum.id()),
        serde_json::json!({ "name": "Post", "message": { "content": "Body" } }),
    )
    .await;
    assert_eq!(response.status(), Status::BadRequest);
    drop(response);

    // Moderated tags need ManageThreads
    let response = post(
        &harness,
        &other_session,
        format!("/channels/{}/threads", forum.id()),
        serde_json::json!({
            "name": "Post",
            "message": { "content": "Body" },
            "applied_tags": ["TAG2"]
        }),
    )
    .await;
    assert_eq!(response.status(), Status::Forbidden);
    drop(response);

    let response = post(
        &harness,
        &other_session,
        format!("/channels/{}/threads", forum.id()),
        serde_json::json!({
            "name": "Post",
            "message": { "content": "Body" },
            "applied_tags": ["TAG1"]
        }),
    )
    .await;
    assert_eq!(response.status(), Status::Ok);
    let post_channel: v0::Channel = response.into_json().await.expect("post");

    let v0::Channel::Thread {
        id,
        applied_tags,
        message_count,
        last_message_id,
        ..
    } = &post_channel
    else {
        panic!("not a thread");
    };
    assert_eq!(applied_tags, &vec!["TAG1".to_string()]);
    assert_eq!(*message_count, 0);
    assert_eq!(last_message_id.as_deref(), Some(id.as_str()));

    // The first message has the post's id and lives in the post
    let message = harness.db.fetch_message(id).await.unwrap();
    assert_eq!(&message.channel, id);

    // The forum tracks its latest post
    let forum = harness.db.fetch_channel(forum.id()).await.unwrap();
    assert!(matches!(
        forum,
        Channel::ForumChannel { last_message_id: Some(ref last), .. } if last == id
    ));

    // Replying counts messages and joins the post
    let response = post(
        &harness,
        &session,
        format!("/channels/{}/messages", id),
        serde_json::json!({ "content": "Reply" }),
    )
    .await;
    assert_eq!(response.status(), Status::Ok);
    drop(response);

    let thread = harness.db.fetch_channel(id).await.unwrap();
    assert!(matches!(
        thread,
        Channel::Thread {
            message_count: 1,
            member_count: 2,
            ..
        }
    ));

    // Search returns the post with its first message and author
    let response = get(
        &harness,
        &session,
        format!("/channels/{}/threads/search", forum.id()),
    )
    .await;
    assert_eq!(response.status(), Status::Ok);
    let list: v0::ThreadList = response.into_json().await.expect("list");
    assert_eq!(list.threads.len(), 1);
    assert_eq!(list.messages.len(), 1);
    assert!(list.users.iter().any(|u| u.id == other.id));
    assert_eq!(list.members.len(), 1);

    // Owners may delete their own forum posts
    let response = harness
        .client
        .delete(format!("/channels/{}", id))
        .header(Header::new("x-session-token", other_session.token.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::NoContent);
}

#[rocket::async_test]
async fn locked_and_archived_threads() {
    let harness = TestHarness::new().await;
    let (_, session, user) = harness.new_user().await;
    let (_, other_session, other) = harness.new_user().await;
    let (server, channels) = harness.new_server(&user).await;
    let channel = &channels[0];
    join(&harness, &server, &other).await;

    let response = post(
        &harness,
        &other_session,
        format!("/channels/{}/threads", channel.id()),
        serde_json::json!({ "name": "Chat" }),
    )
    .await;
    let thread: v0::Channel = response.into_json().await.expect("thread");
    let id = thread.id().to_string();

    // The owner can archive, sending a message brings it back
    let response = harness
        .client
        .patch(format!("/channels/{}", id))
        .header(Header::new("x-session-token", other_session.token.clone()))
        .header(ContentType::JSON)
        .body(serde_json::json!({ "archived": true }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    drop(response);

    let response = get(
        &harness,
        &other_session,
        format!("/channels/{}/threads/archived", channel.id()),
    )
    .await;
    let list: v0::ThreadList = response.into_json().await.expect("list");
    assert_eq!(list.threads.len(), 1);

    let response = post(
        &harness,
        &other_session,
        format!("/channels/{}/messages", id),
        serde_json::json!({ "content": "back" }),
    )
    .await;
    assert_eq!(response.status(), Status::Ok);
    drop(response);
    assert!(matches!(
        harness.db.fetch_channel(&id).await.unwrap(),
        Channel::Thread { archived: false, .. }
    ));

    // The owner can't lock
    let response = harness
        .client
        .patch(format!("/channels/{}", id))
        .header(Header::new("x-session-token", other_session.token.clone()))
        .header(ContentType::JSON)
        .body(serde_json::json!({ "locked": true }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Forbidden);
    drop(response);

    // Moderators can, which also archives
    let response = harness
        .client
        .patch(format!("/channels/{}", id))
        .header(Header::new("x-session-token", session.token.clone()))
        .header(ContentType::JSON)
        .body(serde_json::json!({ "locked": true }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    drop(response);
    assert!(matches!(
        harness.db.fetch_channel(&id).await.unwrap(),
        Channel::Thread {
            archived: true,
            locked: true,
            ..
        }
    ));

    // Nobody but moderators can talk in a locked thread
    let response = post(
        &harness,
        &other_session,
        format!("/channels/{}/messages", id),
        serde_json::json!({ "content": "let me in" }),
    )
    .await;
    assert_eq!(response.status(), Status::Forbidden);
    drop(response);

}

async fn thread_member_ids(harness: &TestHarness, id: &str) -> Vec<String> {
    harness
        .db
        .fetch_thread_members(id)
        .await
        .expect("members")
        .into_iter()
        .map(|m| m.id.user)
        .collect()
}

#[rocket::async_test]
async fn mentions_join_private_threads() {
    let harness = TestHarness::new().await;
    let (_, session, owner) = harness.new_user().await;
    let (_, other_session, other) = harness.new_user().await;
    let (_, _, third) = harness.new_user().await;
    let (_, _, fourth) = harness.new_user().await;
    let (server, channels) = harness.new_server(&owner).await;
    let channel = &channels[0];
    join(&harness, &server, &owner).await;
    join(&harness, &server, &other).await;
    join(&harness, &server, &third).await;
    join(&harness, &server, &fourth).await;

    let create = |invitable: bool| {
        post(
            &harness,
            &other_session,
            format!("/channels/{}/threads", channel.id()),
            serde_json::json!({ "name": "Secret", "private": true, "invitable": invitable }),
        )
    };

    // A member of an invitable thread can bring people in by mentioning them
    let thread: v0::Channel = create(true).await.into_json().await.expect("thread");
    let response = post(
        &harness,
        &other_session,
        format!("/channels/{}/messages", thread.id()),
        serde_json::json!({ "content": format!("hey <@{}>", third.id) }),
    )
    .await;
    assert_eq!(response.status(), Status::Ok);
    let message: v0::Message = response.into_json().await.expect("message");
    assert_eq!(message.mentions, Some(vec![third.id.clone()]));
    assert!(thread_member_ids(&harness, thread.id()).await.contains(&third.id));

    // Without invitable, only people with ManageThreads can
    let thread: v0::Channel = create(false).await.into_json().await.expect("thread");
    let response = post(
        &harness,
        &other_session,
        format!("/channels/{}/messages", thread.id()),
        serde_json::json!({ "content": format!("hey <@{}>", third.id) }),
    )
    .await;
    let message: v0::Message = response.into_json().await.expect("message");
    assert_eq!(message.mentions, None);
    assert!(!thread_member_ids(&harness, thread.id()).await.contains(&third.id));

    let response = post(
        &harness,
        &session,
        format!("/channels/{}/messages", thread.id()),
        serde_json::json!({ "content": format!("hey <@{}>", fourth.id) }),
    )
    .await;
    let message: v0::Message = response.into_json().await.expect("message");
    assert_eq!(message.mentions, Some(vec![fourth.id.clone()]));
    assert!(thread_member_ids(&harness, thread.id()).await.contains(&fourth.id));
}

#[rocket::async_test]
async fn deleting_messages_updates_thread_count() {
    let harness = TestHarness::new().await;
    let (_, session, user) = harness.new_user().await;
    let (_, other_session, other) = harness.new_user().await;
    let (server, channels) = harness.new_server(&user).await;
    let channel = &channels[0];
    join(&harness, &server, &other).await;

    let thread: v0::Channel = post(
        &harness,
        &session,
        format!("/channels/{}/threads", channel.id()),
        serde_json::json!({ "name": "Counting" }),
    )
    .await
    .into_json()
    .await
    .expect("thread");

    let mut ids = vec![];
    for i in 0..4 {
        let message: v0::Message = post(
            &harness,
            &session,
            format!("/channels/{}/messages", thread.id()),
            serde_json::json!({ "content": format!("message {i}") }),
        )
        .await
        .into_json()
        .await
        .expect("message");
        ids.push(message.id);
    }

    let count = || async {
        harness
            .db
            .fetch_channel(thread.id())
            .await
            .expect("thread")
            .thread_message_count()
    };
    assert_eq!(count().await, 4);

    let response = harness
        .client
        .delete(format!("/channels/{}/messages/{}", thread.id(), ids[0]))
        .header(Header::new("x-session-token", session.token.clone()))
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::NoContent);
    drop(response);
    assert_eq!(count().await, 3);

    let response = harness
        .client
        .delete(format!("/channels/{}/messages/bulk", thread.id()))
        .header(Header::new("x-session-token", session.token.clone()))
        .header(ContentType::JSON)
        .body(serde_json::json!({ "ids": [ids[1], ids[2]] }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::NoContent);
    drop(response);
    assert_eq!(count().await, 1);

    // Banning with message deletion sweeps threads too
    let other_message: v0::Message = post(
        &harness,
        &other_session,
        format!("/channels/{}/messages", thread.id()),
        serde_json::json!({ "content": "spam" }),
    )
    .await
    .into_json()
    .await
    .expect("message");
    assert_eq!(count().await, 2);

    let response = harness
        .client
        .put(format!("/servers/{}/bans/{}", server.id, other.id))
        .header(Header::new("x-session-token", session.token.clone()))
        .header(ContentType::JSON)
        .body(serde_json::json!({ "delete_message_seconds": 3600 }).to_string())
        .dispatch()
        .await;
    assert_eq!(response.status(), Status::Ok);
    drop(response);
    assert!(harness.db.fetch_message(&other_message.id).await.is_err());
    assert_eq!(count().await, 1);
}
