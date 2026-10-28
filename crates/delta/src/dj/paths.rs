//! Paths for DJ audio files.
//!
//! Audio is stored unencrypted on the local disk of the delta instance which
//! owns playback, under `DJ_DIR` (default `/var/lib/revolt/dj`), one directory
//! per channel. Files never enter the file service and are deleted after they
//! have been played.

use std::path::{Path, PathBuf};

/// Environment variable overriding the storage directory for DJ audio
pub const DJ_DIR_ENV: &str = "DJ_DIR";

/// Default directory used to store DJ audio
pub const DEFAULT_DJ_DIR: &str = "/var/lib/revolt/dj";

/// Directory in which DJ audio is stored
pub fn dj_dir() -> PathBuf {
    match std::env::var(DJ_DIR_ENV) {
        Ok(dir) if !dir.trim().is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(DEFAULT_DJ_DIR),
    }
}

/// Directory holding the audio of a single channel
pub fn channel_dir(channel: &str) -> PathBuf {
    dj_dir().join(sanitise(channel))
}

/// Path of a single track's audio file
pub fn track_path(channel: &str, track: &str) -> PathBuf {
    channel_dir(channel).join(format!("{}.mp3", sanitise(track)))
}

/// Public (informational) path of a track, as stored in the queue entry
pub fn track_url_path(channel: &str, track: &str) -> String {
    format!("/dj/{}/{}.mp3", sanitise(channel), sanitise(track))
}

/// Whether the given id is safe to use as a path component
pub fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Strip anything which is not safe for a path component
///
/// Ids are validated before they reach this function, so it is a defence in
/// depth measure rather than the primary check.
fn sanitise(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(128)
        .collect()
}

/// Remove every file stored for the given channel
pub fn remove_channel_files(channel: &str) {
    let dir = channel_dir(channel);

    // Only ever remove a directory which is a direct child of `dj_dir()`
    if dir.parent() != Some(dj_dir().as_path()) {
        return;
    }

    if let Err(error) = std::fs::remove_dir_all(&dir) {
        if error.kind() != std::io::ErrorKind::NotFound {
            log::warn!("Failed to remove DJ files at {}: {error}", dir.display());
        }
    }
}

/// Ensure the directory for a channel exists
pub fn create_channel_dir(channel: &str) -> std::io::Result<PathBuf> {
    let dir = channel_dir(channel);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Remove every file under `DJ_DIR`
pub fn wipe_dir() {
    let dir: &Path = &dj_dir();

    if let Err(error) = std::fs::remove_dir_all(dir) {
        if error.kind() != std::io::ErrorKind::NotFound {
            log::warn!("Failed to clear DJ storage at {}: {error}", dir.display());
        }
    }

    if let Err(error) = std::fs::create_dir_all(dir) {
        log::error!(
            "Failed to create DJ storage directory at {}: {error}",
            dir.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_traversal_ids() {
        assert!(!is_safe_id("../etc/passwd"));
        assert!(!is_safe_id("a/b"));
        assert!(!is_safe_id(""));
        assert!(!is_safe_id("with space"));
        assert!(is_safe_id("01J8Z-9abcDEF"));
    }

    #[test]
    fn track_paths_stay_inside_the_channel_directory() {
        let path = track_path("chan", "../escape");
        assert_eq!(path.parent(), Some(channel_dir("chan").as_path()));
        assert!(path.to_string_lossy().ends_with("escape.mp3"));
    }
}
