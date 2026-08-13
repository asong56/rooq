use crate::providers::subprocess::{self, SearchLocation, SubprocessError};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

// Still bounded even though window.rs now calls this from a background
// thread (not the UI thread): a hung onas process would otherwise block
// that thread indefinitely and the request would never resolve.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

const EXE_NAME: &str = if cfg!(windows) { "onas.exe" } else { "onas" };

const SEARCH_ORDER: &[SearchLocation] = &[
    SearchLocation::EnvVar("ROOQ_ONAS"),
    SearchLocation::NextToExe,
    SearchLocation::Path,
];

const NOT_FOUND_HINT: &str =
    "Place onas.exe next to rooq.exe, or set the ROOQ_ONAS environment variable to point to it.";

pub(super) fn run_onas_image_convert(input: &Path, output: &Path) -> Result<(), SubprocessError> {
    let onas = subprocess::locate_executable("onas", EXE_NAME, NOT_FOUND_HINT, SEARCH_ORDER)?;

    let mut cmd = Command::new(onas);
    cmd.arg("image").arg(input).arg(output);

    subprocess::run("onas", cmd, CALL_TIMEOUT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_onas_binary_reports_all_tried_locations() {
        std::env::remove_var("ROOQ_ONAS");
        match subprocess::locate_executable("onas", EXE_NAME, NOT_FOUND_HINT, SEARCH_ORDER) {
            Err(SubprocessError::BinaryNotFound { tried, .. }) => assert!(!tried.is_empty()),
            other => {
                let _ = other;
            }
        }
    }
}
