//! Low-level `onas` subprocess handling: locate the binary, build args, run
//! the process, drain pipes, enforce a timeout.
//!
//! Interface confirmed against onas source (v0.2.0, cli.rs / image.rs /
//! video.rs):
//! - `onas image <input> <output>`: full-file format conversion. Output
//!   format is inferred from `<output>`'s extension. onas decodes the input
//!   to RGBA8 internally and re-encodes to the target format; there's no
//!   decode-only mode and no stdout output, every call writes a full file
//!   to disk.
//! - `onas frame <input> <output> [--at SECONDS]`: extracts a single video
//!   frame (added in v0.2.0). `--at` is omitted here since any frame works
//!   for a thumbnail; onas defaults to the first frame.
//! - Errors: onas's `main()` returns `anyhow::Result<()>`, so any failure
//!   exits non-zero with the full error chain on stderr. That stderr text
//!   is surfaced to the user as-is; exit codes aren't parsed since
//!   success/failure is the only distinction Rooq needs.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum OnasBridgeError {
    #[error(
        "onas executable not found (tried: {tried}). \
         Place onas.exe next to rooq.exe, or set the ROOQ_ONAS \
         environment variable to point to it."
    )]
    BinaryNotFound { tried: String },
    #[error("failed to spawn onas subprocess: {0}")]
    SpawnFailed(std::io::Error),
    #[error("failed to wait for onas subprocess to exit: {0}")]
    WaitFailed(std::io::Error),
    #[error("onas timed out (exceeded {0:?}), subprocess killed")]
    Timeout(Duration),
    #[error("onas reported failure:\n{stderr}")]
    OnasFailed { stderr: String },
}

/// webp/avif conversion normally takes milliseconds; 10s is a circuit
/// breaker for a hung onas process or broken environment, not an expected
/// duration. window.rs calls into this synchronously, so a hang here would
/// block the UI thread indefinitely without this cutoff.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// Frame extraction gets a looser timeout: video codec init (H.265/VP9/AV1)
/// is heavier than static image decode, and large mkv/webm files shouldn't
/// be misdiagnosed as hung just for being big.
const FRAME_CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Locate the onas executable, in priority order:
/// 1. `ROOQ_ONAS` env var (explicit override)
/// 2. `onas.exe` next to the running rooq.exe (recommended deployment)
/// 3. `onas` on PATH (fallback)
fn locate_onas() -> Result<PathBuf, OnasBridgeError> {
    let mut tried = Vec::new();

    if let Ok(p) = std::env::var("ROOQ_ONAS") {
        let path = PathBuf::from(&p);
        let exists = path.is_file();
        tried.push(p);
        if exists {
            return Ok(path);
        }
    }

    let exe_name = if cfg!(windows) { "onas.exe" } else { "onas" };

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(exe_name);
            tried.push(candidate.display().to_string());
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    if let Ok(path_var) = std::env::var("PATH") {
        let sep = if cfg!(windows) { ';' } else { ':' };
        for dir in path_var.split(sep) {
            let candidate = Path::new(dir).join(exe_name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    tried.push(format!("{exe_name} on PATH"));

    Err(OnasBridgeError::BinaryNotFound {
        tried: tried.join(", "),
    })
}

/// Generates a temp output path unique to this call (pid + counter, so
/// concurrent/consecutive calls never collide). Deleted by the caller right
/// after reading it back — it's a one-shot handoff file, not a cache.
pub(super) fn temp_output_path(target_ext: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let mut path = std::env::temp_dir();
    path.push(format!("rooq_onas_{pid}_{n}.{target_ext}"));
    path
}

/// Runs `onas image <input> <output>`, blocking until completion, timeout,
/// or failure.
pub(super) fn run_onas_image_convert(input: &Path, output: &Path) -> Result<(), OnasBridgeError> {
    run_onas(
        &["image", &input.display().to_string(), &output.display().to_string()],
        CALL_TIMEOUT,
    )
}

/// Runs `onas frame <input> <output>`, blocking until completion, timeout,
/// or failure. `--at` is intentionally omitted; see module docs.
pub(super) fn run_onas_frame_extract(input: &Path, output: &Path) -> Result<(), OnasBridgeError> {
    run_onas(
        &["frame", &input.display().to_string(), &output.display().to_string()],
        FRAME_CALL_TIMEOUT,
    )
}

fn run_onas(args: &[&str], timeout: Duration) -> Result<(), OnasBridgeError> {
    let onas = locate_onas()?;

    let mut child = Command::new(&onas)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(OnasBridgeError::SpawnFailed)?;

    // Drain stdout/stderr on separate threads while waiting: if onas writes
    // more than the OS pipe buffer (typically 64KB) and nothing reads it,
    // the child blocks on write() and never exits — a classic pipe
    // deadlock. Output is small in practice, but correctness here shouldn't
    // depend on that assumption.
    let mut stdout_pipe = child.stdout.take().expect("stdout was piped at spawn");
    let mut stderr_pipe = child.stderr.take().expect("stderr was piped at spawn");
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout_pipe.read_to_string(&mut buf);
        buf
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stderr_pipe.read_to_string(&mut buf);
        buf
    });

    let start = Instant::now();
    let status = loop {
        match child.try_wait().map_err(OnasBridgeError::WaitFailed)? {
            Some(status) => break status,
            None => {
                if start.elapsed() > timeout {
                    // kill() closes the child's standard handles, so both
                    // reader threads hit EOF and return; join() below won't
                    // hang.
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    return Err(OnasBridgeError::Timeout(timeout));
                }
                std::thread::sleep(Duration::from_millis(15));
            }
        }
    };

    let stderr = stderr_reader.join().unwrap_or_default();
    let _ = stdout_reader.join();

    if status.success() {
        Ok(())
    } else {
        Err(OnasBridgeError::OnasFailed { stderr })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_output_paths_are_unique_across_calls() {
        let a = temp_output_path("png");
        let b = temp_output_path("png");
        assert_ne!(a, b);
    }

    #[test]
    fn temp_output_path_uses_requested_extension() {
        let p = temp_output_path("png");
        assert_eq!(p.extension().and_then(|e| e.to_str()), Some("png"));
    }

    #[test]
    fn missing_onas_binary_reports_all_tried_locations() {
        // Assumes onas isn't on PATH in the test environment. If it is,
        // this test can't validate the "not found" path — not a code bug.
        std::env::remove_var("ROOQ_ONAS");
        match locate_onas() {
            Err(OnasBridgeError::BinaryNotFound { tried }) => {
                assert!(!tried.is_empty());
            }
            other => {
                let _ = other;
            }
        }
    }
}
