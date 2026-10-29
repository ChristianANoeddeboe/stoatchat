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

/// Whether the given binary answers `-version` successfully
async fn binary_available(binary: &str) -> Option<String> {
    let output = Command::new(binary)
        .arg("-version")
        .stdin(Stdio::null())
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let version = String::from_utf8_lossy(&output.stdout);
    Some(version.lines().next().unwrap_or("unknown version").to_string())
}

/// Check that ffmpeg **and** ffprobe are available, logging loudly if not
///
/// Both are required: ffmpeg transcodes and ffprobe supplies the authoritative
/// duration, so an instance with only ffmpeg would accept uploads and then fail
/// every one of them. A missing binary is not fatal (deployments which do not
/// use DJ channels should still boot) but every upload will fail.
pub async fn check_available() -> bool {
    let ffmpeg = binary_available(&ffmpeg_bin()).await;
    let ffprobe = binary_available(&ffprobe_bin()).await;

    match (ffmpeg, ffprobe) {
        (Some(ffmpeg), Some(ffprobe)) => {
            log::info!("DJ transcoding available: {ffmpeg} / {ffprobe}");
            true
        }
        (ffmpeg, ffprobe) => {
            if ffmpeg.is_none() {
                log::error!(
                    "############################################################\n\
                     # ffmpeg is NOT available on this instance!                #\n\
                     # DJ channel uploads will fail with DjUnavailable.         #\n\
                     # Install ffmpeg in the delta image (see Dockerfile).      #\n\
                     ############################################################"
                );
            }

            if ffprobe.is_none() {
                log::error!(
                    "############################################################\n\
                     # ffprobe is NOT available on this instance!               #\n\
                     # DJ uploads cannot be measured and will fail.             #\n\
                     # Install ffmpeg (it ships ffprobe) in the delta image.    #\n\
                     ############################################################"
                );
            }

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
        // The caller stages to `<id>.mp3.part`, so the extension alone cannot
        // tell ffmpeg which muxer to use. Pin it explicitly.
        .arg("-f")
        .arg("mp3")
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

    #[test]
    fn tolerates_surrounding_whitespace() {
        assert_eq!(parse_probe_duration(" 12.5 \n").unwrap(), 12_500);
        assert_eq!(parse_probe_duration("\t3.001\r\n").unwrap(), 3_001);
    }

    #[test]
    fn rounds_milliseconds() {
        assert_eq!(parse_probe_duration("1.0004").unwrap(), 1_000);
        assert_eq!(parse_probe_duration("1.0006").unwrap(), 1_001);
        assert_eq!(parse_probe_duration("0.5").unwrap(), 500);
    }

    #[test]
    fn rejects_non_finite_and_negative_durations() {
        for value in ["nan", "NaN", "inf", "-inf", "-1", "-0.5"] {
            let error =
                parse_probe_duration(value).expect_err(&format!("{value:?} must be rejected"));

            assert!(
                matches!(error.error_type, ErrorType::DjTranscodeFailed),
                "{:?} produced {:?}",
                value,
                error
            );
        }
    }

    #[test]
    fn accepts_scientific_notation() {
        assert_eq!(parse_probe_duration("1e3").unwrap(), 1_000_000);
        assert_eq!(parse_probe_duration("1.5E-3").unwrap(), 2);
    }

    #[test]
    fn absurdly_long_durations_saturate_instead_of_wrapping() {
        // f64 -> u64 casts saturate in Rust; a hostile probe value must not
        // wrap around into a small, acceptable duration.
        assert_eq!(parse_probe_duration("1e300").unwrap(), u64::MAX);
    }

    #[test]
    fn sub_millisecond_durations_round_to_zero() {
        // Documents the lower bound: ffprobe reporting 0.4 ms yields 0 ms,
        // which then passes the duration cap check.
        assert_eq!(parse_probe_duration("0.0004").unwrap(), 0);
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dj-transcode-{tag}-{}",
            ulid::Ulid::new().to_string().to_lowercase()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    /// Whether the configured ffmpeg binary can actually run
    async fn ffmpeg_works() -> bool {
        match Command::new(ffmpeg_bin())
            .arg("-version")
            .stdin(Stdio::null())
            .output()
            .await
        {
            Ok(output) if output.status.success() => true,
            _ => {
                eprintln!("skipping DJ transcode test: ffmpeg is not available");
                false
            }
        }
    }

    /// Render `seconds` of a sine wave to a WAV file
    async fn render_sine(path: &Path, seconds: u32) {
        let input = format!("sine=frequency=440:duration={seconds}");

        let status = Command::new(ffmpeg_bin())
            .args([
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
            ])
            .arg(input)
            .args(["-ar", "44100"])
            .arg(path)
            .stdin(Stdio::null())
            .status()
            .await
            .expect("spawn ffmpeg");

        assert!(status.success(), "failed to render test audio");
    }

    #[tokio::test]
    async fn real_ffmpeg_transcodes_and_caps_duration() {
        if !ffmpeg_works().await {
            return;
        }

        let dir = temp_dir("cap");
        let source = dir.join("sine.wav");

        render_sine(&source, 3).await;

        // A 1 s cap truncates the 3 s source
        let capped = dir.join("capped.mp3");
        to_mp3(&source, &capped, 1).await.expect("transcode");
        let duration = probe_duration_ms(&capped).await.expect("probe capped");

        assert!(
            (900..=1150).contains(&duration),
            "expected the 1 s cap to apply, probed {} ms",
            duration
        );

        // A 10 s cap keeps the whole track
        let full = dir.join("full.mp3");
        to_mp3(&source, &full, 10).await.expect("transcode");
        let duration = probe_duration_ms(&full).await.expect("probe full");

        assert!(
            (2_850..=3_150).contains(&duration),
            "expected the full 3 s track, probed {} ms",
            duration
        );

        // The output really is an MP3, so `NamedFile`/media players can demux it
        let header = std::fs::read(&full).expect("read output");
        assert!(header.len() > 128, "output looks empty");
        assert!(
            header.starts_with(b"ID3") || header[0] == 0xff,
            "output is not an MP3 (first byte {:#x})",
            header[0]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn real_ffmpeg_accepts_a_part_staging_path() {
        if !ffmpeg_works().await {
            return;
        }

        let dir = temp_dir("part");
        let source = dir.join("sine.wav");
        render_sine(&source, 1).await;

        // The upload handler stages to `<id>.mp3.part`. Without an explicit
        // `-f mp3`, ffmpeg cannot infer a muxer from the `.part` extension and
        // fails with "Unable to find a suitable output format".
        let output = dir.join("track.mp3.part");
        to_mp3(&source, &output, 10)
            .await
            .expect("transcode to .mp3.part");

        let header = std::fs::read(&output).expect("read staged output");
        assert!(header.len() > 128, "output looks empty");
        assert!(
            header.starts_with(b"ID3") || header[0] == 0xff,
            "output is not an MP3 (first byte {:#x})",
            header[0]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn real_ffmpeg_rejects_non_audio_input() {
        if !ffmpeg_works().await {
            return;
        }

        let dir = temp_dir("bad");
        let source = dir.join("not-audio.txt");
        let output = dir.join("out.mp3");

        std::fs::write(&source, b"this is definitely not audio").expect("write");

        let error = to_mp3(&source, &output, 10)
            .await
            .expect_err("non-audio input must fail");

        assert!(matches!(
            error.error_type,
            revolt_result::ErrorType::DjTranscodeFailed
        ));
        assert!(!output.exists(), "no file should be left behind");

        // Probing a non-audio file fails rather than reporting a bogus duration
        assert!(probe_duration_ms(&source).await.is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_missing_binary_is_reported_as_unavailable() {
        // `check_available` must look at ffprobe too: an instance with only
        // ffmpeg accepts uploads and then fails every single one.
        assert!(binary_available("dj-test-definitely-not-a-binary")
            .await
            .is_none());
    }

    #[tokio::test]
    async fn ffmpeg_availability_check_matches_the_binary() {
        if !ffmpeg_works().await {
            return;
        }

        assert!(check_available().await, "check_available should see ffmpeg");
    }
}
