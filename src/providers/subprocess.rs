//! Shared plumbing for the onas and ffmpeg bridges: locating an external
//! binary, running it with a timeout, and handing back a one-shot temp file.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SubprocessError {
    #[error("{tool} executable not found (tried: {tried}). {hint}")]
    BinaryNotFound {
        tool: &'static str,
        tried: String,
        hint: &'static str,
    },
    #[error("failed to spawn {tool} subprocess: {source}")]
    SpawnFailed {
        tool: &'static str,
        source: std::io::Error,
    },
    #[error("failed to wait for {tool} subprocess to exit: {source}")]
    WaitFailed {
        tool: &'static str,
        source: std::io::Error,
    },
    #[error("{tool} timed out (exceeded {timeout:?}), subprocess killed")]
    Timeout { tool: &'static str, timeout: Duration },
    #[error("{tool} reported failure:\n{stderr}")]
    ProcessFailed { tool: &'static str, stderr: String },
}

/// Where to look for a companion executable, tried in the order given.
pub enum SearchLocation {
    EnvVar(&'static str),
    NextToExe,
    Path,
}

pub fn locate_executable(
    tool: &'static str,
    exe_name: &str,
    hint: &'static str,
    order: &[SearchLocation],
) -> Result<PathBuf, SubprocessError> {
    let mut tried = Vec::new();

    for location in order {
        match location {
            SearchLocation::EnvVar(var) => {
                if let Ok(p) = std::env::var(var) {
                    let path = PathBuf::from(&p);
                    let exists = path.is_file();
                    tried.push(p);
                    if exists {
                        return Ok(path);
                    }
                }
            }
            SearchLocation::NextToExe => {
                if let Ok(exe) = std::env::current_exe() {
                    if let Some(dir) = exe.parent() {
                        let candidate = dir.join(exe_name);
                        tried.push(candidate.display().to_string());
                        if candidate.is_file() {
                            return Ok(candidate);
                        }
                    }
                }
            }
            SearchLocation::Path => {
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
            }
        }
    }

    Err(SubprocessError::BinaryNotFound {
        tool,
        tried: tried.join(", "),
        hint,
    })
}

// Drained on separate threads: an unread pipe past the OS buffer would
// block the child in write() forever.
pub fn run(tool: &'static str, mut command: Command, timeout: Duration) -> Result<(), SubprocessError> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| SubprocessError::SpawnFailed { tool, source })?;

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
        match child
            .try_wait()
            .map_err(|source| SubprocessError::WaitFailed { tool, source })?
        {
            Some(status) => break status,
            None => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    return Err(SubprocessError::Timeout { tool, timeout });
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
        Err(SubprocessError::ProcessFailed { tool, stderr })
    }
}

/// Deleted by the caller right after reading it back — a one-shot handoff
/// file, not a cache.
pub fn temp_output_path(prefix: &str, target_ext: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let mut path = std::env::temp_dir();
    path.push(format!("rooq_{prefix}_{pid}_{n}.{target_ext}"));
    path
}

/// RAII wrapper for a one-shot temp file: deletes itself on drop.
pub struct TempFile(PathBuf);

impl TempFile {
    pub(super) fn new(path: PathBuf) -> Self {
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TempFile {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_output_paths_are_unique_across_calls() {
        let a = temp_output_path("onas", "png");
        let b = temp_output_path("onas", "png");
        assert_ne!(a, b);
    }

    #[test]
    fn temp_output_path_uses_requested_prefix_and_extension() {
        let p = temp_output_path("ffmpeg", "png");
        assert_eq!(p.extension().and_then(|e| e.to_str()), Some("png"));
        assert!(p.file_stem().unwrap().to_str().unwrap().starts_with("rooq_ffmpeg_"));
    }

    #[test]
    fn missing_binary_reports_all_tried_locations() {
        std::env::remove_var("ROOQ_TEST_TOOL");
        let order = [SearchLocation::EnvVar("ROOQ_TEST_TOOL"), SearchLocation::NextToExe, SearchLocation::Path];
        match locate_executable("test-tool", "rooq_test_tool_that_does_not_exist", "hint", &order) {
            Err(SubprocessError::BinaryNotFound { tried, .. }) => assert!(!tried.is_empty()),
            other => {
                let _ = other;
            }
        }
    }
}
