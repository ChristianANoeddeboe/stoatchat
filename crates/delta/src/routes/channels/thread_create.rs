use revolt_database::util::permissions::DatabasePermissionQuery;
use revolt_database::{
    util::{idempotency::IdempotencyKey, reference::Reference},
    Channel, Database, Message, MessageFlagsValue, PartialMessage, SystemMessage, User,
    AMQP, AUTO_ARCHIVE_MINUTES,
};
use revolt_models::v0::{self, MessageFlags};
use revolt_permissions::{
    calculate_channel_permissions, ChannelPermission, PermissionQuery, PermissionValue,
};
use revolt_result::{create_error, Result};
use rocket::serde::json::Json;
use rocket::State;
use ulid::Ulid;
use validator::Validate;

use super::channel_edit::validate_applied_tags;

/// Validate the fields shared by every kind of thread, returns the trimmed name
fn validate_thread(data: &v0::DataCreateThread) -> Result<String> {
    data.validate().map_err(|error| {
        create_error!(FailedValidation {
            error: error.to_string()
        })
    })?;

    let name = data.name.trim().to_string();
    if name.is_empty() {
        return Err(create_error!(FailedValidation {
            error: "name: must not be empty".to_string()
        }));
    }

    if let Some(minutes) = data.auto_archive_minutes {
        if !AUTO_ARCHIVE_MINUTES.contains(&minutes) {
            return Err(create_error!(InvalidProperty));
        }
    }

    Ok(name)
}

/// # Start Thread from Message
///
/// Start a public thread from a message in a text channel.
///
/// The thread has the same id as the message.
#[openapi(tag = "Threads")]
#[post("/<target>/messages/<msg>/threads", data = "<data>")]
pub async fn create_thread_from_message(
    db: &State<Database>,
    user: User,
    target: Reference<'_>,
    msg: Reference<'_>,
    data: Json<v0::DataCreateThread>,
) -> Result<Json<v0::Channel>> {
    let data = data.into_inner();
    let name = validate_thread(&data)?;
    if data.private || data.message.is_some() || !data.applied_tags.is_empty() {
        return Err(create_error!(InvalidOperation));
    }

    let channel = target.as_channel(db).await?;
    if !matches!(channel, Channel::TextChannel { .. }) {
        return Err(create_error!(InvalidOperation));
    }

    let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
    let permissions = calculate_channel_permissions(&mut query).await;
    permissions.throw_if_lacking_channel_permission(ChannelPermission::ViewChannel)?;
    permissions.throw_if_lacking_channel_permission(ChannelPermission::ReadMessageHistory)?;
    permissions.throw_if_lacking_channel_permission(ChannelPermission::CreatePublicThreads)?;

    let mut message = msg.as_message_in_channel(db, channel.id()).await?;
    let mut flags = MessageFlagsValue(message.flags.unwrap_or_default());
    if flags.has(MessageFlags::HasThread) {
        return Err(create_error!(InvalidOperation));
    }

    let thread = Channel::create_thread(
        db,
        &channel,
        &user.id,
        message.id.clone(),
        name,
        false,
        false,
        data.auto_archive_minutes,
        data.slowmode,
        vec![],
    )
    .await?;

    flags.set(MessageFlags::HasThread, true);
    message
        .update(
            db,
            PartialMessage {
                flags: Some(flags.0),
                ..Default::default()
            },
            vec![],
        )
        .await?;

    Ok(Json(thread.into()))
}

/// # Start Thread
///
/// Start a thread in a text channel, or create a post in a forum channel.
///
/// Public threads in text channels are announced with a system message which has the thread's id.
/// Forum posts are created together with their first message, which has the post's id.
#[openapi(tag = "Threads")]
#[post("/<target>/threads", data = "<data>")]
pub async fn create_thread(
    db: &State<Database>,
    amqp: &State<AMQP>,
    user: User,
    target: Reference<'_>,
    data: Json<v0::DataCreateThread>,
    idempotency: IdempotencyKey,
) -> Result<Json<v0::Channel>> {
    let data = data.into_inner();
    let name = validate_thread(&data)?;

    let channel = target.as_channel(db).await?;
    let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
    let permissions = calculate_channel_permissions(&mut query).await;
    permissions.throw_if_lacking_channel_permission(ChannelPermission::ViewChannel)?;

    match &channel {
        Channel::TextChannel { .. } => {
            if data.message.is_some() || !data.applied_tags.is_empty() {
                return Err(create_error!(InvalidOperation));
            }

            permissions.throw_if_lacking_channel_permission(if data.private {
                ChannelPermission::CreatePrivateThreads
            } else {
                ChannelPermission::CreatePublicThreads
            })?;

            let thread = Channel::create_thread(
                db,
                &channel,
                &user.id,
                Ulid::new().to_string(),
                name.clone(),
                data.private,
                data.private && data.invitable,
                data.auto_archive_minutes,
                data.slowmode,
                vec![],
            )
            .await?;

            // Announce public threads in the parent channel
            if !data.private {
                let mut flags = MessageFlagsValue(0);
                flags.set(MessageFlags::HasThread, true);

                let mut message = SystemMessage::ThreadCreated {
                    name,
                    by: user.id.clone(),
                }
                .into_message(channel.id().to_string());
                message.id = thread.id().to_string();
                message.flags = Some(flags.0);

                message
                    .send(
                        db,
                        Some(amqp),
                        user.as_author_for_system(),
                        None,
                        None,
                        &channel,
                        false,
                    )
                    .await
                    .ok();
            }

            Ok(Json(thread.into()))
        }
        Channel::ForumChannel {
            available_tags,
            require_tag,
            ..
        } => {
            // Creating a post is "sending a message" in the forum
            permissions.throw_if_lacking_channel_permission(ChannelPermission::SendMessage)?;

            if data.private {
                return Err(create_error!(InvalidOperation));
            }

            let Some(message) = data.message else {
                return Err(create_error!(InvalidOperation));
            };

            message.validate().map_err(|error| {
                create_error!(FailedValidation {
                    error: error.to_string()
                })
            })?;

            check_message_permissions(&permissions, &message)?;

            let applied_tags =
                validate_applied_tags(data.applied_tags, available_tags, *require_tag)?;

            if !permissions.has_channel_permission(ChannelPermission::ManageThreads)
                && available_tags
                    .iter()
                    .any(|tag| tag.moderated && applied_tags.contains(&tag.id))
            {
                return Err(create_error!(MissingPermission {
                    permission: ChannelPermission::ManageThreads.to_string()
                }));
            }

            let author: v0::User = user.clone().into(db, Some(&user)).await;
            query.are_we_a_member().await;

            let model_user = user
                .clone()
                .into_known_static(revolt_presence::is_online(&user.id).await)
                .await;

            let model_member: Option<v0::Member> = query
                .member_ref()
                .as_ref()
                .map(|member| member.clone().into_owned().into());

            let thread = Channel::create_thread(
                db,
                &channel,
                &user.id,
                Ulid::new().to_string(),
                name,
                false,
                false,
                data.auto_archive_minutes,
                data.slowmode,
                applied_tags,
            )
            .await?;

            // A post without its first message is useless, so undo it on failure
            if let Err(error) = Message::create_from_api_with_id(
                db,
                Some(amqp),
                thread.clone(),
                message,
                v0::MessageAuthor::User(&author),
                Some(model_user),
                model_member,
                user.limits().await,
                idempotency,
                permissions.has_channel_permission(ChannelPermission::SendEmbeds),
                true,
                Some(thread.id().to_string()),
            )
            .await
            {
                thread.delete(db).await.ok();
                return Err(error);
            }

            // The post's first message is counted as a reply by `send`, undo that
            db.add_to_thread_counts(thread.id(), -1, 0).await?;

            Ok(Json(db.fetch_channel(thread.id()).await?.into()))
        }
        _ => Err(create_error!(InvalidOperation)),
    }
}

/// Check the permissions needed for the contents of a message
fn check_message_permissions(
    permissions: &PermissionValue,
    message: &v0::DataMessageSend,
) -> Result<()> {
    if let Some(masq) = &message.masquerade {
        permissions.throw_if_lacking_channel_permission(ChannelPermission::Masquerade)?;

        if masq.colour.is_some() {
            permissions.throw_if_lacking_channel_permission(ChannelPermission::ManageRole)?;
        }
    }

    if message.embeds.as_ref().is_some_and(|v| !v.is_empty()) {
        permissions.throw_if_lacking_channel_permission(ChannelPermission::SendEmbeds)?;
    }

    if message.attachments.as_ref().is_some_and(|v| !v.is_empty()) {
        permissions.throw_if_lacking_channel_permission(ChannelPermission::UploadFiles)?;
    }

    Ok(())
}
