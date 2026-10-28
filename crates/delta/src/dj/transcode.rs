//! Transcoding of DJ uploads to MP3.
//!
//! Uploads are transcoded synchronously in the request handler, bounded by a
//! semaphore and a hard timeout. The output is probed with `ffprobe` for the
//! authoritative duration, because the container header of the input cannot be
//! trusted after `-t` has truncated it.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use once_cell::sync::OnceCell;
use revolt_result::{create_error, Result};
use tokio::process::Command;
use tokio::sync::Semaphore;

/// Maximum amount of concurrent transcode jobs
pub const MAX_CONCURRENT_JOBS: usize = 2;

/// Hard timeout for a single transcode or probe
pub const TRANSCODE_TIMEOUT: Duration = Duration::from_secs(120);

/// Environment variable overriding the ffmpeg binary
pub const FFMPEG_ENV: &str = "FFMPEG_BIN";
/// Environment variable overriding the ffprobe binary
pub const FFPROBE_ENV: &str = "FFPROBE_BIN";

static SEMAPHORE: OnceCell<Semaphore> = OnceCell::new();

/// Semaphore bounding concurrent transcode jobs
pub fn semaphore() -> &'static Semaphore {
    SEMAPHORE.get_or_init(|| Semaphore::new(MAX_CONCURRENT_JOBS))
}

fn ffmpeg_bin() -> String {
    std::env::var(FFMPEG_ENV).unwrap_or_else(|_| "ffmpeg".to_string())
}

fn ffprobe_bin() -> String {
    std::env::var(FFPROBE_ENV).unwrap_or_else(|_| "ffprobe".to_string())
}

/// Check that ffmpeg is available, logging loudly if it is not
///
/// This is called on start up. A missing binary is not fatal (deployments which
/// do not use DJ channels should still boot) but every upload will fail.
pub async fn check_available() -> bool {
    let output = Command::new(ffmpeg_bin())
        .arg("-version")
        .stdin(Stdio::null())
        .output()
        .await;

    match output {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout);
            let version = version.lines().next().unwrap_or("unknown version");

            log::info!("DJ transcoding available: {version}");
            true
        }
        _ => {
            log::error!(
                "############################################################\n\
                 # ffmpeg is NOT available on this instance!                #\n\
                 # DJ channel uploads will fail with DjUnavailable.         #\n\
                 # Install ffmpeg in the delta image (see Dockerfile).      #\n\
                 ############################################################"
            );
            false
        }
    }
}

/// Transcode the input file to MP3, capping the output at `max_seconds`
///
/// Video streams are dropped (`-vn`) and the first audio stream is required, so
/// non-audio uploads fail here rather than producing an empty file.
pub async fn to_mp3(input: &Path, output: &Path, max_seconds: u32) -> Result<()> {
    let _permit = semaphore()
        .acquire()
        .await
        .map_err(|_| create_error!(DjUnavailable))?;

    let child = Command::new(ffmpeg_bin())
        .arg("-nostdin")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-y")
        .arg("-i")
        .arg(input)
        .arg("-vn")
        .arg("-map")
        .arg("0:a:0")
        .arg("-c:a")
        .arg("libmp3lame")
        .arg("-b:a")
        .arg("192k")
        .arg("-t")
        .arg(max_seconds.to_string())
        .arg(output)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| {
            log::error!("Failed to spawn ffmpeg: {error}");
            create_error!(DjUnavailable)
        })?;

    let result = tokio::time::timeout(TRANSCODE_TIMEOUT, child.wait_with_output()).await;

    match result {
        Ok(Ok(output)) if output.status.success() => Ok(()),
        Ok(Ok(output)) => {
            let stderr = String::from_utf8_lossy(&output.stderr);

            log::warn!(
                "ffmpeg failed to transcode {}: {}",
                input.display(),
                stderr.trim()
            );

            Err(create_error!(DjTranscodeFailed))
        }
        Ok(Err(error)) => {
            log::error!("Failed to run ffmpeg: {error}");
            Err(create_error!(DjTranscodeFailed))
        }
        Err(_) => {
            // `kill_on_drop` reaps the child when it is dropped
            log::warn!(
                "ffmpeg timed out after {}s transcoding {}",
                TRANSCODE_TIMEOUT.as_secs(),
                input.display()
            );

            Err(create_error!(DjTranscodeFailed))
        }
    }
}

/// Probe the duration of an audio file, in milliseconds
pub async fn probe_duration_ms(path: &Path) -> Result<u64> {
    let output = tokio::time::timeout(
        TRANSCODE_TIMEOUT,
        Command::new(ffprobe_bin())
            .arg("-v")
            .arg("error")
            .arg("-show_entries")
            .arg("format=duration")
            .arg("-of")
            .arg("default=noprint_wrappers=1:nokey=1")
            .arg(path)
            .stdin(Stdio::null())
            .output(),
    )
    .await
    .map_err(|_| create_error!(DjTranscodeFailed))?
    .map_err(|error| {
        log::error!("Failed to spawn ffprobe: {error}");
        create_error!(DjUnavailable)
    })?;

    if !output.status.success() {
        log::warn!(
            "ffprobe failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );

        return Err(create_error!(DjTranscodeFailed));
    }

    parse_probe_duration(&String::from_utf8_lossy(&output.stdout))
}

/// Parse the duration (in seconds) printed by ffprobe into milliseconds
fn parse_probe_duration(stdout: &str) -> Result<u64> {
    let seconds = stdout
        .trim()
        .parse::<f64>()
        .map_err(|_| create_error!(DjTranscodeFailed))?;

    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(create_error!(DjTranscodeFailed));
    }

    Ok((seconds * 1000.0).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use revolt_result::ErrorType;

    #[test]
    fn parses_ffprobe_duration() {
        assert_eq!(parse_probe_duration("183.456000\n").unwrap(), 183_456);
        assert_eq!(parse_probe_duration("0.5").unwrap(), 500);
    }

    #[test]
    fn rejects_empty_or_invalid_durations() {
        assert!(matches!(
            parse_probe_duration("").unwrap_err().error_type,
            ErrorType::DjTranscodeFailed
        ));
        assert!(matches!(
            parse_probe_duration("N/A").unwrap_err().error_type,
            ErrorType::DjTranscodeFailed
        ));
        assert!(matches!(
            parse_probe_duration("0").unwrap_err().error_type,
            ErrorType::DjTranscodeFailed
        ));
    }
}
