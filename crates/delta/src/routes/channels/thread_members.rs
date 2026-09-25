use revolt_database::util::permissions::DatabasePermissionQuery;
use revolt_database::{
    events::client::EventV1, util::reference::Reference, Channel, Database, User,
};
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission, PermissionValue};
use revolt_result::{create_error, Result};
use rocket::serde::json::Json;
use rocket::State;
use rocket_empty::EmptyResponse;

/// Fetch a thread we can see, with our permissions in it
async fn fetch_thread(
    db: &Database,
    user: &User,
    target: &Reference<'_>,
) -> Result<(Channel, PermissionValue)> {
    let channel = target.as_channel(db).await?;
    if !channel.is_thread() {
        return Err(create_error!(InvalidOperation));
    }

    let mut query = DatabasePermissionQuery::new(db, user).channel(&channel);
    let permissions = calculate_channel_permissions(&mut query).await;
    permissions.throw_if_lacking_channel_permission(ChannelPermission::ViewChannel)?;

    Ok((channel, permissions))
}

/// # Fetch Thread Members
///
/// Lists the members of a thread.
#[openapi(tag = "Threads")]
#[get("/<target>/thread_members")]
pub async fn fetch_thread_members(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
) -> Result<Json<Vec<v0::ThreadMember>>> {
    let (channel, _) = fetch_thread(db, &user, &target).await?;
    Ok(Json(
        db.fetch_thread_members(channel.id())
            .await?
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}

/// # Add Thread Member
///
/// Join a thread (`@me`), or add someone else to it.
///
/// Adding people to a private thread needs ManageThreads unless the thread is invitable.
#[openapi(tag = "Threads")]
#[put("/<target>/thread_members/<member>")]
pub async fn add_thread_member(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    member: Reference<'_>,
) -> Result<EmptyResponse> {
    let (channel, permissions) = fetch_thread(db, &user, &target).await?;
    let Channel::Thread {
        id,
        owner,
        private,
        invitable,
        ..
    } = &channel
    else {
        unreachable!()
    };

    if member.id == "@me" || member.id == user.id {
        channel.add_thread_member(db, &user.id).await?;
        return Ok(EmptyResponse);
    }

    let manage = permissions.has_channel_permission(ChannelPermission::ManageThreads);
    permissions.throw_if_lacking_channel_permission(ChannelPermission::SendMessage)?;

    if !manage {
        // Only members may bring others in
        if owner != &user.id && db.fetch_thread_member(id, &user.id).await.is_err() {
            return Err(create_error!(NotFound));
        }

        if *private && !*invitable {
            return Err(create_error!(MissingPermission {
                permission: ChannelPermission::ManageThreads.to_string()
            }));
        }
    }

    // They must be able to see the parent channel
    let target_user = member.as_user(db).await?;
    let parent = db.fetch_channel(channel.parent().expect("thread")).await?;
    let mut query = DatabasePermissionQuery::new(db, &target_user).channel(&parent);
    if !calculate_channel_permissions(&mut query)
        .await
        .has_channel_permission(ChannelPermission::ViewChannel)
    {
        return Err(create_error!(NotFound));
    }

    channel.add_thread_member(db, &target_user.id).await?;
    Ok(EmptyResponse)
}

/// # Remove Thread Member
///
/// Leave a thread (`@me`), or remove someone else from it.
///
/// Removing others needs ManageThreads, or being the owner of the private thread.
#[openapi(tag = "Threads")]
#[delete("/<target>/thread_members/<member>")]
pub async fn remove_thread_member(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    member: Reference<'_>,
) -> Result<EmptyResponse> {
    let (channel, permissions) = fetch_thread(db, &user, &target).await?;
    let Channel::Thread { owner, private, .. } = &channel else {
        unreachable!()
    };

    let user_id = if member.id == "@me" {
        user.id.as_str()
    } else {
        member.id
    };

    if user_id != user.id
        && !permissions.has_channel_permission(ChannelPermission::ManageThreads)
        && !(*private && owner == &user.id)
    {
        return Err(create_error!(MissingPermission {
            permission: ChannelPermission::ManageThreads.to_string()
        }));
    }

    if !channel.remove_thread_member(db, user_id).await? {
        return Err(create_error!(NotFound));
    }

    Ok(EmptyResponse)
}

/// # Edit Thread Membership
///
/// Change our notification setting for a thread we are a member of.
#[openapi(tag = "Threads")]
#[patch("/<target>/thread_members/@me", data = "<data>")]
pub async fn edit_thread_member(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    data: Json<v0::DataEditThreadMember>,
) -> Result<Json<v0::ThreadMember>> {
    let data = data.into_inner();
    let (channel, _) = fetch_thread(db, &user, &target).await?;

    // Must already be a member
    db.fetch_thread_member(channel.id(), &user.id).await?;

    if let Some(notify) = &data.notify {
        db.update_thread_member_notify(channel.id(), &user.id, notify)
            .await?;
    }

    let member: v0::ThreadMember = db
        .fetch_thread_member(channel.id(), &user.id)
        .await?
        .into();

    EventV1::ThreadMemberUpdate {
        id: channel.id().to_string(),
        member: Some(member.clone()),
    }
    .private(user.id.clone())
    .await;

    Ok(Json(member))
}
