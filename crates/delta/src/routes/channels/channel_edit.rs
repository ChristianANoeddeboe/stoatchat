use revolt_database::{
    util::{permissions::DatabasePermissionQuery, reference::Reference},
    voice::{delete_voice_channel, UserVoiceChannel, VoiceClient},
    AuditLogEntryAction, Channel, Database, FieldsChannel, File, PartialChannel, SystemMessage,
    User, AMQP, AUTO_ARCHIVE_MINUTES,
};
use iso8601_timestamp::Timestamp;
use revolt_models::v0;
use revolt_permissions::{calculate_channel_permissions, ChannelPermission};
use revolt_result::{create_error, Result};
use rocket::{serde::json::Json, State};
use std::collections::HashSet;
use ulid::Ulid;
use validator::Validate;

use crate::util::audit_log_reason::AuditLogReason;

/// # Edit Channel
///
/// Edit a channel object by its id.
#[openapi(tag = "Channel Information")]
#[patch("/<target>", data = "<data>")]
pub async fn edit(
    db: &State<Database>,
    voice_client: &State<VoiceClient>,
    amqp: &State<AMQP>,
    user: User,
    reason: AuditLogReason,
    target: Reference<'_>,
    data: Json<v0::DataEditChannel>,
) -> Result<Json<v0::Channel>> {
    let data = data.into_inner();
    data.validate().map_err(|error| {
        create_error!(FailedValidation {
            error: error.to_string()
        })
    })?;

    let mut channel = target.as_channel(db).await?;
    let mut query = DatabasePermissionQuery::new(db, &user).channel(&channel);
    let permissions = calculate_channel_permissions(&mut query).await;

    // Thread owners may edit some properties of their thread, checked below
    if channel.is_thread() {
        permissions.throw_if_lacking_channel_permission(ChannelPermission::ViewChannel)?;
    } else {
        permissions.throw_if_lacking_channel_permission(ChannelPermission::ManageChannel)?;

        if data.name.as_ref().is_some_and(|name| name.chars().count() > 32) {
            return Err(create_error!(FailedValidation {
                error: "name: length must be between 1 and 32".to_string()
            }));
        }
    }

    if data.name.is_none()
        && data.description.is_none()
        && data.icon.is_none()
        && data.nsfw.is_none()
        && data.owner.is_none()
        && data.voice.is_none()
        && data.slowmode.is_none()
        && data.archived.is_none()
        && data.locked.is_none()
        && data.pinned.is_none()
        && data.invitable.is_none()
        && data.auto_archive_minutes.is_none()
        && data.applied_tags.is_none()
        && data.available_tags.is_none()
        && data.require_tag.is_none()
        && data.default_reaction_emoji.is_none()
        && data.default_sort_order.is_none()
        && data.default_layout.is_none()
        && data.default_thread_slowmode.is_none()
        && data.default_auto_archive_minutes.is_none()
        && data.remove.is_empty()
    {
        return Ok(Json(channel.into()));
    }

    let thread_fields = has_thread_fields(&data);
    let forum_fields = has_forum_fields(&data);

    for minutes in [data.auto_archive_minutes, data.default_auto_archive_minutes]
        .iter()
        .flatten()
    {
        if !AUTO_ARCHIVE_MINUTES.contains(minutes) {
            return Err(create_error!(InvalidProperty));
        }
    }

    let mut partial: PartialChannel = Default::default();

    // Transfer group ownership
    if let Some(new_owner) = data.owner {
        if let Channel::Group {
            owner, recipients, ..
        } = &mut channel
        {
            // Make sure we are the owner of this group
            if owner != &user.id {
                return Err(create_error!(NotOwner));
            }

            // Ensure user is part of group
            if !recipients.contains(&new_owner) {
                return Err(create_error!(NotInGroup));
            }

            // Transfer ownership
            partial.owner = Some(new_owner.to_string());
            let old_owner = std::mem::replace(owner, new_owner.to_string());

            // Notify clients
            SystemMessage::ChannelOwnershipChanged {
                from: old_owner,
                to: new_owner,
            }
        } else {
            return Err(create_error!(InvalidOperation));
        }
        .into_message(channel.id().to_string())
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

    let before_channel = channel.clone();

    match &mut channel {
        Channel::Group {
            id,
            name,
            description,
            icon,
            nsfw,
            ..
        } => {
            if thread_fields || forum_fields {
                return Err(create_error!(InvalidOperation));
            }

            if data.remove.contains(&v0::FieldsChannel::Icon) {
                if let Some(icon) = &icon {
                    db.mark_attachment_as_deleted(&icon.id).await?;
                }
            }

            for field in &data.remove {
                match field {
                    v0::FieldsChannel::Description => {
                        description.take();
                    }
                    v0::FieldsChannel::Icon => {
                        icon.take();
                    }
                    _ => {}
                }
            }

            if let Some(icon_id) = data.icon {
                partial.icon = Some(File::use_channel_icon(db, &icon_id, id, &user.id).await?);
                *icon = partial.icon.clone();
            }

            if let Some(new_name) = data.name {
                *name = new_name.clone();
                partial.name = Some(new_name);
            }

            if let Some(new_description) = data.description {
                partial.description = Some(new_description);
                *description = partial.description.clone();
            }

            if let Some(new_nsfw) = data.nsfw {
                *nsfw = new_nsfw;
                partial.nsfw = Some(new_nsfw);
            }

            // Send out mutation system messages.
            if let Some(name) = &partial.name {
                SystemMessage::ChannelRenamed {
                    name: name.to_string(),
                    by: user.id.clone(),
                }
                .into_message(channel.id().to_string())
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

            if partial.description.is_some() {
                SystemMessage::ChannelDescriptionChanged {
                    by: user.id.clone(),
                }
                .into_message(channel.id().to_string())
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

            if partial.icon.is_some() {
                SystemMessage::ChannelIconChanged {
                    by: user.id.clone(),
                }
                .into_message(channel.id().to_string())
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
        }
        Channel::TextChannel {
            id,
            name,
            description,
            icon,
            nsfw,
            voice,
            slowmode,
            default_auto_archive_minutes,
            ..
        } => {
            if thread_fields || forum_fields {
                return Err(create_error!(InvalidOperation));
            }

            if let Some(minutes) = data.default_auto_archive_minutes {
                *default_auto_archive_minutes = Some(minutes);
                partial.default_auto_archive_minutes = Some(minutes);
            }

            if data.remove.contains(&v0::FieldsChannel::Icon) {
                if let Some(icon) = &icon {
                    db.mark_attachment_as_deleted(&icon.id).await?;
                }
            }

            for field in &data.remove {
                match field {
                    v0::FieldsChannel::Description => {
                        description.take();
                    }
                    v0::FieldsChannel::Icon => {
                        icon.take();
                    }
                    v0::FieldsChannel::Voice => {
                        voice.take();
                    }
                    v0::FieldsChannel::Slowmode => {
                        slowmode.take();
                    }
                    v0::FieldsChannel::DefaultAutoArchiveMinutes => {
                        default_auto_archive_minutes.take();
                    }
                    _ => {}
                }
            }

            if let Some(icon_id) = data.icon {
                partial.icon = Some(File::use_channel_icon(db, &icon_id, id, &user.id).await?);
                *icon = partial.icon.clone();
            }

            if let Some(new_name) = data.name {
                *name = new_name.clone();
                partial.name = Some(new_name);
            }

            if let Some(new_description) = data.description {
                partial.description = Some(new_description);
                *description = partial.description.clone();
            }

            if let Some(new_nsfw) = data.nsfw {
                *nsfw = new_nsfw;
                partial.nsfw = Some(new_nsfw);
            }

            if let Some(new_voice) = data.voice {
                *voice = Some(new_voice.clone().into());
                partial.voice = Some(new_voice.into());
            }

            if let Some(new_slowmode) = data.slowmode {
                *slowmode = Some(new_slowmode);
                partial.slowmode = Some(new_slowmode);
            }
        }
        Channel::ForumChannel { id, icon, .. } => {
            if thread_fields || data.voice.is_some() {
                return Err(create_error!(InvalidOperation));
            }

            if data.remove.contains(&v0::FieldsChannel::Icon) {
                if let Some(icon) = &icon {
                    db.mark_attachment_as_deleted(&icon.id).await?;
                }
            }

            if let Some(icon_id) = data.icon {
                partial.icon = Some(File::use_channel_icon(db, &icon_id, id, &user.id).await?);
            }

            if let Some(tags) = data.available_tags {
                partial.available_tags = Some(validate_forum_tags(tags)?);
            }

            partial.name = data.name;
            partial.description = data.description;
            partial.nsfw = data.nsfw;
            partial.slowmode = data.slowmode;
            partial.require_tag = data.require_tag;
            partial.default_reaction_emoji = data.default_reaction_emoji;
            partial.default_sort_order = data.default_sort_order;
            partial.default_layout = data.default_layout;
            partial.default_thread_slowmode = data.default_thread_slowmode;
            partial.default_auto_archive_minutes = data.default_auto_archive_minutes;
        }
        Channel::Thread {
            parent,
            owner,
            private,
            locked,
            archived,
            pinned,
            applied_tags,
            ..
        } => {
            if forum_fields
                || data.description.is_some()
                || data.icon.is_some()
                || data.nsfw.is_some()
                || data.voice.is_some()
                || data
                    .remove
                    .iter()
                    .any(|field| field != &v0::FieldsChannel::Slowmode)
            {
                return Err(create_error!(InvalidOperation));
            }

            let manage = permissions.has_channel_permission(ChannelPermission::ManageChannel);
            let is_owner = owner == &user.id;

            // Locked threads can only be changed by moderators
            if *locked && !manage {
                return Err(create_error!(MissingPermission {
                    permission: ChannelPermission::ManageThreads.to_string()
                }));
            }

            // Anyone who can talk in an unlocked thread may unarchive it
            let unarchive_only = data.archived == Some(false)
                && data.name.is_none()
                && data.locked.is_none()
                && data.pinned.is_none()
                && data.invitable.is_none()
                && data.auto_archive_minutes.is_none()
                && data.applied_tags.is_none()
                && data.slowmode.is_none()
                && data.remove.is_empty();

            let allowed = manage
                || (unarchive_only
                    && permissions.has_channel_permission(ChannelPermission::SendMessage))
                || (is_owner
                    && data.locked.is_none()
                    && data.pinned.is_none()
                    && data.slowmode.is_none()
                    && data.remove.is_empty());

            if !allowed {
                return Err(create_error!(MissingPermission {
                    permission: ChannelPermission::ManageThreads.to_string()
                }));
            }

            if data.invitable.is_some() && !*private {
                return Err(create_error!(InvalidOperation));
            }

            let parent_channel = db.fetch_channel(parent).await?;

            if let Some(tags) = data.applied_tags {
                let Channel::ForumChannel {
                    available_tags,
                    require_tag,
                    ..
                } = &parent_channel
                else {
                    return Err(create_error!(InvalidOperation));
                };

                let tags = validate_applied_tags(tags, available_tags, *require_tag)?;

                // Moderated tags may only be added or removed by moderators
                let changed_moderated = available_tags.iter().any(|tag| {
                    tag.moderated && (tags.contains(&tag.id) != applied_tags.contains(&tag.id))
                });

                if changed_moderated && !manage {
                    return Err(create_error!(MissingPermission {
                        permission: ChannelPermission::ManageThreads.to_string()
                    }));
                }

                partial.applied_tags = Some(tags);
            }

            if let Some(pin) = data.pinned {
                // Only forum posts can be pinned, one at a time
                if !matches!(parent_channel, Channel::ForumChannel { .. }) {
                    return Err(create_error!(InvalidOperation));
                }

                if pin && !*pinned {
                    for mut other in db.fetch_threads(parent, None).await? {
                        if matches!(other, Channel::Thread { pinned: true, .. }) {
                            other
                                .update(
                                    db,
                                    PartialChannel {
                                        pinned: Some(false),
                                        ..Default::default()
                                    },
                                    vec![],
                                )
                                .await?;
                        }
                    }
                }

                partial.pinned = Some(pin);
            }

            if let Some(archive) = data.archived {
                if archive != *archived {
                    partial.archived = Some(archive);
                    partial.archived_at = Some(Timestamp::now_utc());
                }
            }

            // Locking a thread also archives it
            if data.locked == Some(true) && !*archived && data.archived.is_none() {
                partial.archived = Some(true);
                partial.archived_at = Some(Timestamp::now_utc());
            }

            partial.name = data.name;
            partial.locked = data.locked;
            partial.invitable = data.invitable;
            partial.auto_archive_minutes = data.auto_archive_minutes;
            partial.slowmode = data.slowmode;
        }
        _ => return Err(create_error!(InvalidOperation)),
    };

    let remove = data
        .remove
        .into_iter()
        .map(|f| f.into())
        .collect::<Vec<FieldsChannel>>();

    let before = if before_channel.server().is_some() {
        Some(before_channel.generate_diff(&partial, &remove))
    } else {
        None
    };

    channel.update(db, partial.clone(), remove).await?;

    if channel.voice().is_none() {
        delete_voice_channel(voice_client, &UserVoiceChannel::from_channel(&channel)).await?;
    }

    if let Some(before) = before {
        AuditLogEntryAction::ChannelEdit {
            channel: channel.id().to_string(),
            before,
            after: partial,
        }
        .insert(db, channel.server().unwrap().to_string(), reason, user.id, None)
        .await;
    };

    Ok(Json(channel.into()))
}

/// Whether any thread-only properties are being edited
fn has_thread_fields(data: &v0::DataEditChannel) -> bool {
    data.archived.is_some()
        || data.locked.is_some()
        || data.pinned.is_some()
        || data.invitable.is_some()
        || data.auto_archive_minutes.is_some()
        || data.applied_tags.is_some()
}

/// Whether any forum-only properties are being edited
fn has_forum_fields(data: &v0::DataEditChannel) -> bool {
    data.available_tags.is_some()
        || data.require_tag.is_some()
        || data.default_reaction_emoji.is_some()
        || data.default_sort_order.is_some()
        || data.default_layout.is_some()
        || data.default_thread_slowmode.is_some()
        || data.remove.iter().any(|field| {
            matches!(
                field,
                v0::FieldsChannel::DefaultReactionEmoji | v0::FieldsChannel::DefaultThreadSlowmode
            )
        })
}

/// Validate the tags of a forum, assigning ids to new tags
fn validate_forum_tags(tags: Vec<v0::ForumTag>) -> Result<Vec<v0::ForumTag>> {
    let mut ids = HashSet::new();
    let mut result = Vec::with_capacity(tags.len());

    for mut tag in tags {
        tag.name = tag.name.trim().to_string();
        if tag.name.is_empty() || tag.name.chars().count() > 20 {
            return Err(create_error!(FailedValidation {
                error: "available_tags: name length must be between 1 and 20".to_string()
            }));
        }

        if tag.emoji.as_ref().is_some_and(|emoji| emoji.is_empty() || emoji.len() > 64) {
            return Err(create_error!(FailedValidation {
                error: "available_tags: invalid emoji".to_string()
            }));
        }

        if tag.id.is_empty() {
            tag.id = Ulid::new().to_string();
        }

        if !ids.insert(tag.id.clone()) {
            return Err(create_error!(InvalidProperty));
        }

        result.push(tag);
    }

    Ok(result)
}

/// Validate tags applied to a forum post
pub fn validate_applied_tags(
    tags: Vec<String>,
    available_tags: &[v0::ForumTag],
    require_tag: bool,
) -> Result<Vec<String>> {
    let mut result: Vec<String> = Vec::with_capacity(tags.len());
    for tag in tags {
        if !available_tags.iter().any(|available| available.id == tag) {
            return Err(create_error!(InvalidProperty));
        }

        if !result.contains(&tag) {
            result.push(tag);
        }
    }

    if require_tag && result.is_empty() && !available_tags.is_empty() {
        return Err(create_error!(InvalidProperty));
    }

    Ok(result)
}
