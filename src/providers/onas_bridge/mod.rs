//! Bridge to the `onas` subprocess, used for two format gaps this project
//! doesn't decode itself: webp/avif images and mkv/webm video thumbnails.
//! Both go through `onas` converting to a temporary PNG, which is then read
//! back through the existing zune-png path.

mod subprocess;

pub use subprocess::OnasBridgeError;

use std::path::{Path, PathBuf};

/// RAII wrapper for a one-shot temp file: deletes itself on drop.
pub struct TempPngFile(PathBuf);

impl TempPngFile {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempPngFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Converts a webp/avif image to a temporary PNG (`onas image <input> <o>`).
/// PNG is lossless, so no additional quality loss on top of the source.
pub fn convert_image_to_png(input: &Path) -> Result<TempPngFile, OnasBridgeError> {
    let output = subprocess::temp_output_path("png");
    subprocess::run_onas_image_convert(input, &output)?;
    Ok(TempPngFile(output))
}

/// Extracts one frame from an mkv/webm video as a temporary PNG
/// (`onas frame <input> <o>`).
pub fn extract_video_frame(input: &Path) -> Result<TempPngFile, OnasBridgeError> {
    let output = subprocess::temp_output_path("png");
    subprocess::run_onas_frame_extract(input, &output)?;
    Ok(TempPngFile(output))
}
