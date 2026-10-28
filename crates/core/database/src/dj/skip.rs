//! Skip vote arithmetic and queue cap checks for DJ channels.
//!
//! These are pure functions so the edge cases (nobody in the channel, a
//! threshold of zero, a threshold of one) are unit tested independently of
//! Redis.

use revolt_models::v0::DjInformation;
use revolt_result::Result;

/// Maximum amount of tracks a single member may have queued (including the one
/// currently playing)
pub const MAX_TRACKS_PER_MEMBER: u32 = 3;

/// Amount of votes required to skip the current track.
///
/// `needed = max(1, ceil(present_voters * threshold))`, so a threshold of zero
/// still requires one vote and an empty channel never skips on arrival.
pub fn skip_needed(present_voters: u32, threshold: f32) -> u32 {
    let threshold = if threshold.is_finite() {
        threshold.clamp(0.0, 1.0)
    } else {
        0.0
    };

    ((present_voters as f32) * threshold).ceil().max(1.0) as u32
}

/// Tally the votes for the current track against the present members.
///
/// Votes cast by users which are no longer members are ignored.
///
/// Returns `(votes, needed, should_skip)`.
pub fn tally(voters: &[String], members: &[String], threshold: f32) -> (u32, u32, bool) {
    let votes = voters
        .iter()
        .filter(|voter| members.contains(voter))
        .count() as u32;

    let needed = skip_needed(members.len() as u32, threshold);

    (votes, needed, votes >= needed)
}

/// Result of checking the queue caps for a new upload
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueCaps {
    /// Amount of tracks the uploader already has queued or playing
    pub member_tracks: u32,
    /// Amount of tracks currently in the queue
    pub queued: u32,
    /// Amount of bytes currently stored for the channel
    pub total_bytes: u64,
    /// Amount of bytes the new track occupies
    pub new_track_bytes: u64,
}

/// Check the upload against the per-channel and per-member caps
pub fn check_queue_caps(settings: &DjInformation, caps: QueueCaps) -> Result<()> {
    if caps.member_tracks >= MAX_TRACKS_PER_MEMBER {
        return Err(create_error!(DjUploadLimit {
            max: MAX_TRACKS_PER_MEMBER as usize
        }));
    }

    if caps.queued >= settings.max_tracks {
        return Err(create_error!(DjQueueFull {
            max: settings.max_tracks as usize
        }));
    }

    if caps.total_bytes.saturating_add(caps.new_track_bytes) > settings.max_total_bytes {
        return Err(create_error!(DjStorageFull {
            max_bytes: settings.max_total_bytes
        }));
    }

    Ok(())
}

/// Check that a probed track duration fits within the channel's limit
pub fn check_duration(settings: &DjInformation, duration_ms: u64) -> Result<()> {
    if duration_ms > settings.max_track_seconds as u64 * 1000 {
        return Err(create_error!(DjTrackTooLong {
            max_seconds: settings.max_track_seconds
        }));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use revolt_result::ErrorType;

    fn members(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    #[test]
    fn skip_needed_threshold_zero_is_one_vote() {
        assert_eq!(skip_needed(0, 0.0), 1);
        assert_eq!(skip_needed(1, 0.0), 1);
        assert_eq!(skip_needed(50, 0.0), 1);
    }

    #[test]
    fn skip_needed_half() {
        assert_eq!(skip_needed(1, 0.5), 1);
        assert_eq!(skip_needed(2, 0.5), 1);
        assert_eq!(skip_needed(3, 0.5), 2);
        assert_eq!(skip_needed(4, 0.5), 2);
        assert_eq!(skip_needed(5, 0.5), 3);
    }

    #[test]
    fn skip_needed_unanimous() {
        assert_eq!(skip_needed(1, 1.0), 1);
        assert_eq!(skip_needed(4, 1.0), 4);
        assert_eq!(skip_needed(0, 1.0), 1);
    }

    #[test]
    fn skip_needed_clamps_out_of_range_thresholds() {
        assert_eq!(skip_needed(4, -1.0), 1);
        assert_eq!(skip_needed(4, 2.0), 4);
        assert_eq!(skip_needed(4, f32::NAN), 1);
    }

    #[test]
    fn tally_ignores_departed_voters() {
        let voters = members(&["a", "b", "c"]);
        let present = members(&["a"]);

        // Only "a" still counts, and one of one members is enough at 0.5
        assert_eq!(tally(&voters, &present, 0.5), (1, 1, true));
    }

    #[test]
    fn tally_does_not_skip_empty_channel() {
        assert_eq!(tally(&[], &[], 0.0), (0, 1, false));
        assert_eq!(tally(&[], &[], 1.0), (0, 1, false));
    }

    #[test]
    fn tally_reaches_threshold() {
        let voters = members(&["a", "b"]);
        let present = members(&["a", "b", "c", "d"]);

        assert_eq!(tally(&voters, &present, 0.5), (2, 2, true));
        assert_eq!(tally(&voters, &present, 1.0), (2, 4, false));
    }

    #[test]
    fn queue_caps_per_member_limit() {
        let settings = DjInformation::default();

        let error = check_queue_caps(
            &settings,
            QueueCaps {
                member_tracks: MAX_TRACKS_PER_MEMBER,
                queued: 0,
                total_bytes: 0,
                new_track_bytes: 1,
            },
        )
        .expect_err("should reject a fourth track from the same member");

        assert!(matches!(
            error.error_type,
            ErrorType::DjUploadLimit { max } if max == MAX_TRACKS_PER_MEMBER as usize
        ));
    }

    #[test]
    fn queue_caps_queue_depth() {
        let settings = DjInformation {
            max_tracks: 2,
            ..Default::default()
        };

        check_queue_caps(
            &settings,
            QueueCaps {
                member_tracks: 0,
                queued: 1,
                total_bytes: 0,
                new_track_bytes: 1,
            },
        )
        .expect("should accept a track while there is room");

        let error = check_queue_caps(
            &settings,
            QueueCaps {
                member_tracks: 0,
                queued: 2,
                total_bytes: 0,
                new_track_bytes: 1,
            },
        )
        .expect_err("should reject a track when the queue is full");

        assert!(matches!(
            error.error_type,
            ErrorType::DjQueueFull { max } if max == 2
        ));
    }

    #[test]
    fn queue_caps_total_bytes() {
        let settings = DjInformation {
            max_total_bytes: 1024,
            ..Default::default()
        };

        check_queue_caps(
            &settings,
            QueueCaps {
                member_tracks: 0,
                queued: 0,
                total_bytes: 512,
                new_track_bytes: 512,
            },
        )
        .expect("should accept a track which exactly fills the channel");

        let error = check_queue_caps(
            &settings,
            QueueCaps {
                member_tracks: 0,
                queued: 0,
                total_bytes: 1024,
                new_track_bytes: 1,
            },
        )
        .expect_err("should reject a track which exceeds the channel size");

        assert!(matches!(
            error.error_type,
            ErrorType::DjStorageFull { max_bytes } if max_bytes == 1024
        ));
    }

    #[test]
    fn track_duration_limit() {
        let settings = DjInformation {
            max_track_seconds: 10,
            ..Default::default()
        };

        check_duration(&settings, 10_000).expect("duration at the limit is allowed");

        let error = check_duration(&settings, 10_001).expect_err("duration over the limit fails");
        assert!(matches!(
            error.error_type,
            ErrorType::DjTrackTooLong { max_seconds } if max_seconds == 10
        ));
    }
}
