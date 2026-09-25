use std::collections::HashSet;

use revolt_database::util::permissions::DatabasePermissionQuery;
use revolt_database::{
    iso8601_timestamp::Timestamp, util::reference::Reference, Channel, Database, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::serde::json::Json;
use rocket::State;

/// Fetch the parent channel and check that we can see its threads,
/// returns whether we can see every private thread
async fn fetch_parent(db: &Database, user: &User, target: &Reference<'_>) -> Result<(Channel, bool)> {
    let channel = target.as_channel(db).await?;
    if !matches!(
        channel,
        Channel::TextChannel { .. } | Channel::ForumChannel { .. }
    ) {
        return Err(create_error!(InvalidOperation));
    }

    let mut query = DatabasePermissionQuery::new(db, user).channel(&channel);
    let permissions = calculate_channel_permissions(&mut query).await;
    permissions.throw_if_lacking_channel_permission(ChannelPermission::ViewChannel)?;
    permissions.throw_if_lacking_channel_permission(ChannelPermission::ReadMessageHistory)?;

    let manage = permissions.has_channel_permission(ChannelPermission::ManageThreads);
    Ok((channel, manage))
}

/// Drop the private threads we can't see and collect our memberships of the rest
async fn into_thread_list(
    db: &Database,
    user: &User,
    manage: bool,
    threads: Vec<Channel>,
    has_more: bool,
) -> Result<v0::ThreadList> {
    let ids: HashSet<&str> = threads.iter().map(|thread| thread.id()).collect();
    let members: Vec<v0::ThreadMember> = db
        .fetch_thread_memberships(&user.id)
        .await?
        .into_iter()
        .filter(|member| ids.contains(member.id.thread.as_str()))
        .map(Into::into)
        .collect();

    let joined: HashSet<&str> = members.iter().map(|member| member.id.thread.as_str()).collect();
    let threads = threads
        .into_iter()
        .filter(|thread| match thread {
            Channel::Thread {
                id, private, owner, ..
            } => !private || manage || owner == &user.id || joined.contains(id.as_str()),
            _ => false,
        })
        .map(Into::into)
        .collect();

    Ok(v0::ThreadList {
        threads,
        members,
        has_more,
        messages: vec![],
        users: vec![],
    })
}

/// Clamp a page size, defaulting to `default`
fn page_limit(limit: Option<i64>, default: i64) -> Result<i64> {
    match limit {
        None => Ok(default),
        Some(limit) if (1..=100).contains(&limit) => Ok(limit),
        _ => Err(create_error!(InvalidProperty)),
    }
}

/// # List Active Threads
///
/// Lists the unarchived threads of a text or forum channel that we can see.
#[openapi(tag = "Threads")]
#[get("/<target>/threads/active")]
pub async fn list_active_threads(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<v0::ThreadList>> {
    let (channel, manage) = fetch_parent(db, &user, &target).await?;
    let threads = db.fetch_threads(channel.id(), Some(false)).await?;
    into_thread_list(db, &user, manage, threads, false)
        .await
        .map(Json)
}

/// # List Archived Threads
///
/// Lists the archived threads of a text or forum channel, most recently archived first.
///
/// Private threads are only listed if we are a member or have ManageThreads.
#[openapi(tag = "Threads")]
#[get("/<target>/threads/archived?<options..>")]
pub async fn list_archived_threads(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    options: v0::OptionsArchivedThreads,
) -> Result<Json<v0::ThreadList>> {
    let (channel, manage) = fetch_parent(db, &user, &target).await?;
    let limit = page_limit(options.limit, 50)?;
    let before = match &options.before {
        Some(before) => Some(Timestamp::parse(before).ok_or_else(|| create_error!(InvalidProperty))?),
        None => None,
    };

    let mut threads = db
        .fetch_archived_threads(
            channel.id(),
            options.private.unwrap_or_default(),
            before,
            limit + 1,
        )
        .await?;

    let has_more = threads.len() as i64 > limit;
    threads.truncate(limit as usize);

    into_thread_list(db, &user, manage, threads, has_more)
        .await
        .map(Json)
}

/// # Search Forum Posts
///
/// Lists the posts of a forum channel, pinned posts first,
/// together with their first messages and authors.
#[openapi(tag = "Threads")]
#[get("/<target>/threads/search?<options..>")]
pub async fn search_threads(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    options: v0::OptionsThreadSearch,
) -> Result<Json<v0::ThreadList>> {
    let (channel, manage) = fetch_parent(db, &user, &target).await?;
    let Channel::ForumChannel {
        default_sort_order, ..
    } = &channel
    else {
        return Err(create_error!(InvalidOperation));
    };

    let limit = page_limit(options.limit, 25)?;
    let sort = options.sort.unwrap_or_else(|| default_sort_order.clone());
    let tags = options.tags.unwrap_or_default();

    let mut threads = db
        .search_threads(
            channel.id(),
            &tags,
            &sort,
            options.archived.unwrap_or_default(),
            options.before.as_deref(),
            limit + 1,
        )
        .await?;

    let has_more = threads.len() as i64 > limit;
    threads.truncate(limit as usize);

    let mut list = into_thread_list(db, &user, manage, threads, has_more).await?;

    // The first message of a post has the post's id
    let ids: Vec<String> = list.threads.iter().map(|thread| thread.id().to_string()).collect();
    let messages: Vec<v0::Message> = db
        .fetch_messages_by_id(&ids)
        .await?
        .into_iter()
        .filter(|message| message.id == message.channel)
        .map(|message| message.into_model(None, None))
        .collect();

    let mut user_ids: HashSet<String> = messages
        .iter()
        .map(|message| message.author.clone())
        .collect();

    for thread in &list.threads {
        if let v0::Channel::Thread { owner, .. } = thread {
            user_ids.insert(owner.clone());
        }
    }

    let user_ids: Vec<String> = user_ids.into_iter().collect();
    list.users = User::fetch_many_ids_as_mutuals(db, &user, &user_ids).await?;
    list.messages = messages;

    Ok(Json(list))
}
