mod subprocess;

pub use crate::providers::subprocess::{SubprocessError, TempFile};

use crate::providers::subprocess::temp_output_path;
use std::path::Path;

pub fn extract_video_frame(input: &Path) -> Result<TempFile, SubprocessError> {
    let output = temp_output_path("ffmpeg", "png");
    subprocess::run_ffmpeg_frame_extract(input, &output)?;
    Ok(TempFile::new(output))
}
