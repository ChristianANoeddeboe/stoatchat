//! Shared permission checks for DJ channel routes.
//!
//! Membership (`dj_members`) is a Redis set which is only cleaned up when a
//! socket closes cleanly or the reconciliation sweep runs. It therefore cannot
//! be trusted as an authorisation source: a member who was kicked, banned or had
//! their channel permissions revoked may still be in the set. Every route which
//! acts on behalf of a member re-checks the channel permissions first.

pub mod join;
pub mod leave;
pub mod queue;
pub mod settings;
pub mod skip;
pub mod state;


use revolt_permissions::{ChannelPermission, PermissionValue};
use revolt_result::Result;

/// Permissions needed to vote to skip
pub(crate) fn check_vote_permissions(permissions: &PermissionValue) -> Result<()> {
    permissions.throw_if_lacking_channel_permission(ChannelPermission::ViewChannel)
}

/// Permissions needed to add a track to the queue
///
/// Uploading audio is an attachment upload, so both the ability to see the
/// channel and the ability to send attachments are required.
pub(crate) fn check_queue_permissions(permissions: &PermissionValue) -> Result<()> {
    permissions.throw_if_lacking_channel_permission(ChannelPermission::ViewChannel)?;
    permissions.throw_if_lacking_channel_permission(ChannelPermission::UploadFiles)
}

/// Permissions needed to join
pub(crate) fn check_join_permissions(permissions: &PermissionValue) -> Result<()> {
    permissions.throw_if_lacking_channel_permission(ChannelPermission::Connect)
}

#[cfg(test)]
mod tests {
    use super::*;
    use revolt_result::ErrorType;

    fn permissions(allow: &[ChannelPermission]) -> PermissionValue {
        let mut value = PermissionValue::from_raw(0);

        for permission in allow {
            value.allow(*permission as u64);
        }

        value
    }

    #[test]
    fn a_member_without_view_channel_cannot_vote() {
        let error = check_vote_permissions(&permissions(&[])).expect_err("no permissions");

        assert!(matches!(
            error.error_type,
            ErrorType::MissingPermission { .. }
        ));
    }

    #[test]
    fn view_channel_is_enough_to_vote() {
        check_vote_permissions(&permissions(&[ChannelPermission::ViewChannel]))
            .expect("viewing is enough to vote");
    }

    #[test]
    fn queueing_needs_both_view_and_upload() {
        // Viewing alone is not enough to upload
        let error = check_queue_permissions(&permissions(&[ChannelPermission::ViewChannel]))
            .expect_err("upload permission is required");

        assert!(matches!(
            error.error_type,
            ErrorType::MissingPermission { .. }
        ));

        // Uploading alone is not enough either: the channel must be visible
        assert!(check_queue_permissions(&permissions(&[ChannelPermission::UploadFiles])).is_err());

        check_queue_permissions(&permissions(&[
            ChannelPermission::ViewChannel,
            ChannelPermission::UploadFiles,
        ]))
        .expect("both permissions are present");
    }

    #[test]
    fn revoked_permissions_are_not_restored_by_membership() {
        // This is the shape of the bug: the user is still in `dj_members` but
        // their channel permissions were revoked.
        let mut revoked = permissions(&[
            ChannelPermission::ViewChannel,
            ChannelPermission::UploadFiles,
        ]);
        revoked.revoke(ChannelPermission::UploadFiles as u64);

        assert!(check_queue_permissions(&revoked).is_err());
    }

    #[test]
    fn joining_needs_connect() {
        assert!(check_join_permissions(&permissions(&[])).is_err());
        check_join_permissions(&permissions(&[ChannelPermission::Connect]))
            .expect("connect is enough to join");
    }
}
