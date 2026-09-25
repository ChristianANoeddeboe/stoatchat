use std::borrow::Cow;

use revolt_permissions::{
    calculate_user_permissions, ChannelType, Override, PermissionQuery, PermissionValue,
    RelationshipStatus, ThreadState, DEFAULT_PERMISSION_DIRECT_MESSAGE,
};

use crate::{Channel, Database, Member, Server, User};

/// Permissions calculator
#[derive(Clone)]
pub struct DatabasePermissionQuery<'a> {
    #[allow(dead_code)]
    database: &'a Database,

    perspective: &'a User,
    user: Option<Cow<'a, User>>,
    channel: Option<Cow<'a, Channel>>,
    parent: Option<Cow<'a, Channel>>,
    thread_member: Option<bool>,
    server: Option<Cow<'a, Server>>,
    member: Option<Cow<'a, Member>>,

    // flag_known_relationship: Option<&'a RelationshipStatus>,
    cached_user_permission: Option<PermissionValue>,
    cached_mutual_connection: Option<bool>,
    cached_permission: Option<u64>,
}

#[async_trait]
impl PermissionQuery for DatabasePermissionQuery<'_> {
    // * For calculating user permission

    /// Is our perspective user privileged?
    async fn are_we_privileged(&mut self) -> bool {
        self.perspective.privileged
    }

    /// Is our perspective user a bot?
    async fn are_we_a_bot(&mut self) -> bool {
        self.perspective.bot.is_some()
    }

    /// Is our perspective user and the currently selected user the same?
    async fn are_the_users_same(&mut self) -> bool {
        if let Some(other_user) = &self.user {
            self.perspective.id == other_user.id
        } else {
            false
        }
    }

    /// Get the relationship with have with the currently selected user
    async fn user_relationship(&mut self) -> RelationshipStatus {
        if let Some(other_user) = &self.user {
            if self.perspective.id == other_user.id {
                return RelationshipStatus::User;
            } else if let Some(bot) = &other_user.bot {
                // For the purposes of permissions checks,
                // assume owner is the same as bot
                if self.perspective.id == bot.owner {
                    return RelationshipStatus::User;
                }
            }

            if let Some(relations) = &self.perspective.relations {
                for entry in relations {
                    if entry.id == other_user.id {
                        return match entry.status {
                            crate::RelationshipStatus::None => RelationshipStatus::None,
                            crate::RelationshipStatus::User => RelationshipStatus::User,
                            crate::RelationshipStatus::Friend => RelationshipStatus::Friend,
                            crate::RelationshipStatus::Outgoing => RelationshipStatus::Outgoing,
                            crate::RelationshipStatus::Incoming => RelationshipStatus::Incoming,
                            crate::RelationshipStatus::Blocked => RelationshipStatus::Blocked,
                            crate::RelationshipStatus::BlockedOther => {
                                RelationshipStatus::BlockedOther
                            }
                        };
                    }
                }
            }
        }

        RelationshipStatus::None
    }

    /// Whether the currently selected user is a bot
    async fn user_is_bot(&mut self) -> bool {
        if let Some(other_user) = &self.user {
            other_user.bot.is_some()
        } else {
            false
        }
    }

    /// Do we have a mutual connection with the currently selected user?
    async fn have_mutual_connection(&mut self) -> bool {
        if let Some(value) = self.cached_mutual_connection {
            value
        } else if let Some(user) = &self.user {
            let value = self
                .perspective
                .has_mutual_connection(self.database, &user.id)
                .await
                .unwrap_or_default();

            self.cached_mutual_connection = Some(value);
            value
        } else {
            false
        }
    }

    // * For calculating server permission

    /// Is our perspective user the server's owner?
    async fn are_we_server_owner(&mut self) -> bool {
        if let Some(server) = &self.server {
            server.owner == self.perspective.id
        } else {
            false
        }
    }

    /// Is our perspective user a member of the server?
    async fn are_we_a_member(&mut self) -> bool {
        if let Some(server) = &self.server {
            if self.member.is_some() {
                true
            } else if let Ok(member) = self
                .database
                .fetch_member(&server.id, &self.perspective.id)
                .await
            {
                self.member = Some(Cow::Owned(member));
                true
            } else {
                false
            }
        } else {
            false
        }
    }

    /// Get default server permission
    async fn get_default_server_permissions(&mut self) -> u64 {
        if let Some(server) = &self.server {
            server.default_permissions as u64
        } else {
            0
        }
    }

    /// Get the ordered role overrides (from lowest to highest) for this member in this server
    async fn get_our_server_role_overrides(&mut self) -> Vec<Override> {
        if let Some(server) = &self.server {
            let member_roles = self
                .member
                .as_ref()
                .map(|member| member.roles.clone())
                .unwrap_or_default();

            let mut roles = server
                .roles
                .iter()
                .filter(|(id, _)| member_roles.contains(id))
                .map(|(_, role)| {
                    let v: Override = role.permissions.into();
                    (role.rank, v)
                })
                .collect::<Vec<(i64, Override)>>();

            roles.sort_by(|a, b| b.0.cmp(&a.0));
            roles.into_iter().map(|(_, v)| v).collect()
        } else {
            vec![]
        }
    }

    /// Is our perspective user timed out on this server?
    async fn are_we_timed_out(&mut self) -> bool {
        if let Some(member) = &self.member {
            member.in_timeout()
        } else {
            false
        }
    }

    async fn do_we_have_publish_overwrites(&mut self) -> bool {
        if let Some(member) = &self.member {
            member.can_publish
        } else {
            true
        }
    }

    async fn do_we_have_receive_overwrites(&mut self) -> bool {
        if let Some(member) = &self.member {
            member.can_receive
        } else {
            true
        }
    }

    // * For calculating channel permission

    /// Get the type of the channel
    #[allow(deprecated)]
    async fn get_channel_type(&mut self) -> ChannelType {
        match self.channel.as_deref() {
            Some(Channel::DirectMessage { .. }) => ChannelType::DirectMessage,
            Some(Channel::Group { .. }) => ChannelType::Group,
            Some(Channel::SavedMessages { .. }) => ChannelType::SavedMessages,
            Some(Channel::TextChannel { .. } | Channel::ForumChannel { .. }) => {
                ChannelType::ServerChannel
            }
            Some(Channel::Thread { .. }) => ChannelType::Thread,
            None => ChannelType::Unknown,
        }
    }

    /// Get the state of the thread, if the channel is a thread
    async fn get_thread_state(&mut self) -> ThreadState {
        let Some(Channel::Thread {
            id,
            owner,
            private,
            locked,
            ..
        }) = self.channel.as_deref()
        else {
            unimplemented!()
        };

        let (private, locked) = (*private, *locked);
        let member = if owner == &self.perspective.id {
            true
        } else if let Some(member) = self.thread_member {
            member
        } else if private {
            let id = id.clone();
            let member = self
                .database
                .fetch_thread_member(&id, &self.perspective.id)
                .await
                .is_ok();

            self.thread_member = Some(member);
            member
        } else {
            // membership only matters for private threads
            false
        };

        ThreadState {
            private,
            locked,
            member,
        }
    }

    /// Get the default channel permissions
    /// Group channel defaults should be mapped to an allow-only override
    async fn get_default_channel_permissions(&mut self) -> Override {
        match self.permission_channel().await {
            Some(Channel::Group { permissions, .. }) => Override {
                allow: permissions.unwrap_or(*DEFAULT_PERMISSION_DIRECT_MESSAGE as i64) as u64,
                deny: 0,
            },
            Some(
                Channel::TextChannel {
                    default_permissions,
                    ..
                }
                | Channel::ForumChannel {
                    default_permissions,
                    ..
                },
            ) => default_permissions.unwrap_or_default().into(),
            _ => Default::default(),
        }
    }

    /// Get the ordered role overrides (from lowest to highest) for this member in this channel
    async fn get_our_channel_role_overrides(&mut self) -> Vec<Override> {
        self.permission_channel().await;

        let channel = if let Some(Channel::Thread { .. }) = self.channel.as_deref() {
            self.parent.as_deref()
        } else {
            self.channel.as_deref()
        };

        match channel {
            Some(
                Channel::TextChannel {
                    role_permissions, ..
                }
                | Channel::ForumChannel {
                    role_permissions, ..
                },
            ) => {
                if let Some(server) = &self.server {
                    let member_roles = self
                        .member
                        .as_ref()
                        .map(|member| member.roles.clone())
                        .unwrap_or_default();

                    let mut roles = role_permissions
                        .iter()
                        .filter(|(id, _)| member_roles.contains(id))
                        .filter_map(|(id, permission)| {
                            server.roles.get(id).map(|role| {
                                let v: Override = (*permission).into();
                                (role.rank, v)
                            })
                        })
                        .collect::<Vec<(i64, Override)>>();

                    roles.sort_by(|a, b| b.0.cmp(&a.0));
                    roles.into_iter().map(|(_, v)| v).collect()
                } else {
                    vec![]
                }
            }
            _ => vec![],
        }
    }

    /// Do we own this group or saved messages channel if it is one of those?
    async fn do_we_own_the_channel(&mut self) -> bool {
        match self.channel.as_deref() {
            Some(Channel::Group { owner, .. }) => owner == &self.perspective.id,
            Some(Channel::SavedMessages { user, .. }) => user == &self.perspective.id,
            _ => false,
        }
    }

    /// Are we a recipient of this channel?
    async fn are_we_part_of_the_channel(&mut self) -> bool {
        if let Some(Channel::DirectMessage { recipients, .. } | Channel::Group { recipients, .. }) =
            self.channel.as_deref()
        {
            recipients.contains(&self.perspective.id)
        } else {
            false
        }
    }

    /// Set the current user as the recipient of this channel
    /// (this will only ever be called for DirectMessage channels, use unimplemented!() for other code paths)
    async fn set_recipient_as_user(&mut self) {
        match self.channel.as_deref() {
            Some(Channel::DirectMessage { recipients, .. }) => {
                let recipient_id = recipients
                    .iter()
                    .find(|recipient| recipient != &&self.perspective.id)
                    .expect("Missing recipient for DM")
                    .clone();

                if let Ok(user) = self.database.fetch_user(&recipient_id).await {
                    self.user.replace(Cow::Owned(user));
                }
            }
            Some(_) => unimplemented!(),
            None => {}
        }
    }

    /// Set the current server as the server owning this channel
    /// (this will only ever be called for server channels, use unimplemented!() for other code paths)
    async fn set_server_from_channel(&mut self) {
        let Some(channel) = self.channel.as_deref() else {
            return;
        };

        let Some(server) = channel.server() else {
            unimplemented!()
        };

        if self
            .server
            .as_deref()
            .is_some_and(|known_server| known_server.id == server)
        {
            // Already cached, return early.
            return;
        }

        let server = server.to_string();
        if let Ok(server) = self.database.fetch_server(&server).await {
            self.server.replace(Cow::Owned(server));
        }
    }
}

impl DatabasePermissionQuery<'_> {
    /// Channel whose permission overrides apply,
    /// which is the parent channel for threads (fetched if not known)
    async fn permission_channel(&mut self) -> Option<&Channel> {
        if let Some(Channel::Thread { parent, .. }) = self.channel.as_deref() {
            if self
                .parent
                .as_deref()
                .is_none_or(|known| known.id() != parent)
            {
                let parent = parent.clone();
                self.parent = self
                    .database
                    .fetch_channel(&parent)
                    .await
                    .ok()
                    .map(Cow::Owned);
            }

            self.parent.as_deref()
        } else {
            self.channel.as_deref()
        }
    }
}

impl<'a> DatabasePermissionQuery<'a> {
    /// Create a new permission calculator
    pub fn new(database: &'a Database, perspective: &'a User) -> DatabasePermissionQuery<'a> {
        DatabasePermissionQuery {
            database,
            perspective,
            user: None,
            channel: None,
            parent: None,
            thread_member: None,
            server: None,
            member: None,

            cached_mutual_connection: None,
            cached_user_permission: None,
            cached_permission: None,
        }
    }

    /// Calculate the user permission value
    pub async fn calc_user(mut self) -> DatabasePermissionQuery<'a> {
        if self.cached_user_permission.is_some() {
            return self;
        }

        if self.user.is_none() {
            panic!("Expected `PermissionCalculator.user to exist.");
        }

        DatabasePermissionQuery {
            cached_user_permission: Some(calculate_user_permissions(&mut self).await),
            ..self
        }
    }

    /// Calculate the permission value
    pub async fn calc(self) -> DatabasePermissionQuery<'a> {
        if self.cached_permission.is_some() {
            return self;
        }

        self
    }

    /// Use user
    pub fn user(self, user: &'a User) -> DatabasePermissionQuery<'a> {
        DatabasePermissionQuery {
            user: Some(Cow::Borrowed(user)),
            ..self
        }
    }

    /// Use channel
    pub fn channel(self, channel: &'a Channel) -> DatabasePermissionQuery<'a> {
        DatabasePermissionQuery {
            channel: Some(Cow::Borrowed(channel)),
            ..self
        }
    }

    /// Use parent channel (for threads)
    pub fn parent(self, parent: &'a Channel) -> DatabasePermissionQuery<'a> {
        DatabasePermissionQuery {
            parent: Some(Cow::Borrowed(parent)),
            ..self
        }
    }

    /// Use known thread membership (for threads)
    pub fn thread_member(self, thread_member: bool) -> DatabasePermissionQuery<'a> {
        DatabasePermissionQuery {
            thread_member: Some(thread_member),
            ..self
        }
    }

    /// Use server
    pub fn server(self, server: &'a Server) -> DatabasePermissionQuery<'a> {
        DatabasePermissionQuery {
            server: Some(Cow::Borrowed(server)),
            ..self
        }
    }

    /// Use member
    pub fn member(self, member: &'a Member) -> DatabasePermissionQuery<'a> {
        DatabasePermissionQuery {
            member: Some(Cow::Borrowed(member)),
            ..self
        }
    }

    /// Access the underlying user
    pub fn user_ref(&self) -> &Option<Cow<User>> {
        &self.user
    }

    /// Access the underlying server
    pub fn channel_ref(&self) -> &Option<Cow<Channel>> {
        &self.channel
    }

    /// Access the underlying server
    pub fn server_ref(&self) -> &Option<Cow<Server>> {
        &self.server
    }

    /// Access the underlying member
    pub fn member_ref(&self) -> &Option<Cow<Member>> {
        &self.member
    }

    /// Get the known member's current ranking
    pub fn get_member_rank(&self) -> Option<i64> {
        self.member
            .as_ref()
            .map(|member| member.get_ranking(self.server.as_ref().unwrap()))
    }
}

/// Short-hand for creating a permission calculator
pub fn perms<'a>(database: &'a Database, perspective: &'a User) -> DatabasePermissionQuery<'a> {
    DatabasePermissionQuery::new(database, perspective)
}
