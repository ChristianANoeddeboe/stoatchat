//! Paths for DJ audio files.
//!
//! Audio is stored unencrypted on the local disk of the delta instance which
//! owns playback, under `DJ_DIR` (default `/var/lib/revolt/dj`), one directory
//! per channel. Files never enter the file service and are deleted after they
//! have been played.

use std::path::PathBuf;

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

/// Route of the streaming endpoint, as stored in the queue entry
///
/// This is deliberately the route (not a file path): the audio can only be
/// fetched from `/dj/{channel}/stream` with a signed token, which the client
/// receives in `DjNowPlaying`/`DjTrackStarted`.
pub fn track_url_path(channel: &str) -> String {
    format!("/dj/{}/stream", sanitise(channel))
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
///
/// The directory walk runs on the blocking pool: `remove_dir_all` of a channel
/// with hundreds of tracks is not something to do on a runtime worker.
///
/// Only ever removes a directory which is a direct child of `dj_dir()`.
pub async fn remove_channel_files(channel: &str) {
    let dir = channel_dir(channel);

    if dir.parent() != Some(dj_dir().as_path()) {
        return;
    }

    let removed = dir.clone();

    let result = tokio::task::spawn_blocking(move || {
        std::fs::remove_dir_all(&removed).map_err(|error| error.kind())
    })
    .await;

    match result {
        Ok(Ok(())) | Ok(Err(std::io::ErrorKind::NotFound)) => {}
        Ok(Err(kind)) => log::warn!("Failed to remove DJ files at {}: {kind:?}", dir.display()),
        Err(error) => log::warn!("Failed to remove DJ files at {}: {error}", dir.display()),
    }
}

/// Ensure the directory for a channel exists
pub async fn create_channel_dir(channel: &str) -> std::io::Result<PathBuf> {
    let dir = channel_dir(channel);
    let created = dir.clone();

    tokio::task::spawn_blocking(move || std::fs::create_dir_all(&created))
        .await
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error))??;

    Ok(dir)
}

/// Remove every file under `DJ_DIR` and recreate the root
pub async fn wipe_dir() {
    let dir = dj_dir();
    let wiped = dir.clone();

    let result = tokio::task::spawn_blocking(move || {
        // Clear the *contents* rather than removing the directory itself:
        // DJ_DIR is owned by the runtime user, but its parent (/var/lib/revolt)
        // belongs to root, so `remove_dir_all(DJ_DIR)` fails with EACCES in the
        // deployed container even though DJ_DIR is writable.
        match std::fs::read_dir(&wiped) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry?;
                    let path = entry.path();

                    if path.is_dir() {
                        std::fs::remove_dir_all(&path)?;
                    } else {
                        std::fs::remove_file(&path)?;
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }

        std::fs::create_dir_all(&wiped)
    })
    .await;

    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => log::error!(
            "Failed to reset DJ storage at {}: {error}",
            dir.display()
        ),
        Err(error) => log::error!("Failed to reset DJ storage at {}: {error}", dir.display()),
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
