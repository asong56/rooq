use crate::providers::subprocess::{self, SearchLocation, SubprocessError};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

// Still bounded even though window.rs now calls this from a background
// thread (not the UI thread): a stuck subprocess would otherwise hang that
// thread indefinitely and the request would never resolve.
const FRAME_CALL_TIMEOUT: Duration = Duration::from_secs(30);

const EXE_NAME: &str = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };

const SEARCH_ORDER: &[SearchLocation] = &[
    SearchLocation::EnvVar("ROOQ_FFMPEG"),
    SearchLocation::Path,
    SearchLocation::NextToExe,
];

const NOT_FOUND_HINT: &str = "Install ffmpeg and put it on PATH, place ffmpeg.exe next to \
     rooq.exe, or set the ROOQ_FFMPEG environment variable to point to it.";

pub(super) fn run_ffmpeg_frame_extract(input: &Path, output: &Path) -> Result<(), SubprocessError> {
    let ffmpeg = subprocess::locate_executable("ffmpeg", EXE_NAME, NOT_FOUND_HINT, SEARCH_ORDER)?;

    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-y", "-nostdin", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(input)
        .args(["-frames:v", "1"])
        .arg(output);

    subprocess::run("ffmpeg", cmd, FRAME_CALL_TIMEOUT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_ffmpeg_binary_reports_all_tried_locations() {
        std::env::remove_var("ROOQ_FFMPEG");
        match subprocess::locate_executable("ffmpeg", EXE_NAME, NOT_FOUND_HINT, SEARCH_ORDER) {
            Err(SubprocessError::BinaryNotFound { tried, .. }) => assert!(!tried.is_empty()),
            other => {
                let _ = other;
            }
        }
    }
}
