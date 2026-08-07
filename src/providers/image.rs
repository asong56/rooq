//! In-memory image decoding for jpg/png/gif.
//!
//! jpg/png use zune-jpeg/zune-png (SIMD, smaller dependency tree than the
//! `image` crate's decoders). gif uses the `image` crate, since zune has no
//! mature animated-gif support.
//!
//! webp/avif and video thumbnails aren't handled here; they go through
//! onas_bridge, which converts to a temporary PNG and hands it back to the
//! PNG path below.

use crate::core::dispatcher::InMemoryImageKind;
use image::{AnimationDecoder, ImageError};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::time::Duration;
use thiserror::Error;
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_core::result::DecodingResult;
use zune_jpeg::JpegDecoder;
use zune_png::PngDecoder;

#[derive(Debug, Error)]
pub enum ImageProviderError {
    #[error("failed to open file: {0}")]
    Io(#[from] std::io::Error),
    #[error("JPEG decode failed: {0}")]
    JpegDecode(String),
    #[error("PNG decode failed: {0}")]
    PngDecode(String),
    #[error("GIF decode failed: {0}")]
    GifDecode(#[from] ImageError),
    #[error("file has no frame data (empty or corrupted)")]
    NoFrames,
    #[error("unsupported PNG bit depth (only 8-bit is currently handled)")]
    UnsupportedBitDepth,
}

/// One decoded frame, dimensions plus packed RGBA8 pixels. Every decoder
/// normalizes to RGBA8 so the UI layer doesn't need to care about source
/// format or color space.
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub rgba8: Vec<u8>,
    pub delay: Option<Duration>,
}

pub struct DecodedImage {
    pub frames: Vec<DecodedFrame>,
}

impl DecodedImage {
    pub fn is_animated(&self) -> bool {
        self.frames.len() > 1
    }
}

pub fn decode(path: &Path, kind: InMemoryImageKind) -> Result<DecodedImage, ImageProviderError> {
    match kind {
        InMemoryImageKind::Jpeg => decode_jpeg(path),
        InMemoryImageKind::Png => decode_png(path),
        InMemoryImageKind::Gif => decode_gif(path),
    }
}

fn decode_jpeg(path: &Path) -> Result<DecodedImage, ImageProviderError> {
    let bytes = std::fs::read(path)?;

    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(&bytes), options);

    let pixels = decoder
        .decode()
        .map_err(|e| ImageProviderError::JpegDecode(e.to_string()))?;
    let info = decoder
        .info()
        .ok_or_else(|| ImageProviderError::JpegDecode("could not read image dimensions".into()))?;

    Ok(DecodedImage {
        frames: vec![DecodedFrame {
            width: info.width as u32,
            height: info.height as u32,
            rgba8: pixels,
            delay: None,
        }],
    })
}

fn decode_png(path: &Path) -> Result<DecodedImage, ImageProviderError> {
    let bytes = std::fs::read(path)?;

    // zune-png's own `png_set_add_alpha_channel` only converts Luma to
    // LumaA, not RGBA, so alpha is expanded manually below to guarantee
    // RGBA8 regardless of source color space.
    let mut decoder = PngDecoder::new(ZCursor::new(&bytes));
    let result = decoder
        .decode()
        .map_err(|e| ImageProviderError::PngDecode(e.to_string()))?;

    let colorspace = decoder
        .get_colorspace()
        .ok_or_else(|| ImageProviderError::PngDecode("could not read color space".into()))?;
    let info = decoder
        .get_info()
        .ok_or_else(|| ImageProviderError::PngDecode("could not read image dimensions".into()))?;
    let (width, height) = (info.width as u32, info.height as u32);

    let samples_u8 = match result {
        DecodingResult::U8(px) => px,
        DecodingResult::U16(_) => return Err(ImageProviderError::UnsupportedBitDepth),
        _ => return Err(ImageProviderError::UnsupportedBitDepth),
    };

    let rgba8 = expand_to_rgba8(&samples_u8, colorspace)?;

    Ok(DecodedImage {
        frames: vec![DecodedFrame {
            width,
            height,
            rgba8,
            delay: None,
        }],
    })
}

fn expand_to_rgba8(
    samples: &[u8],
    colorspace: ColorSpace,
) -> Result<Vec<u8>, ImageProviderError> {
    let n = colorspace.num_components();
    match n {
        1 => {
            let mut out = Vec::with_capacity(samples.len() * 4);
            for &l in samples {
                out.extend_from_slice(&[l, l, l, 255]);
            }
            Ok(out)
        }
        2 => {
            let mut out = Vec::with_capacity(samples.len() * 2);
            for chunk in samples.chunks_exact(2) {
                let (l, a) = (chunk[0], chunk[1]);
                out.extend_from_slice(&[l, l, l, a]);
            }
            Ok(out)
        }
        3 => {
            let mut out = Vec::with_capacity(samples.len() / 3 * 4);
            for chunk in samples.chunks_exact(3) {
                out.extend_from_slice(chunk);
                out.push(255);
            }
            Ok(out)
        }
        4 => Ok(samples.to_vec()),
        _ => Err(ImageProviderError::PngDecode(format!(
            "unexpected component count: {n}"
        ))),
    }
}

fn decode_gif(path: &Path) -> Result<DecodedImage, ImageProviderError> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let gif_decoder = image::codecs::gif::GifDecoder::new(reader)?;

    let frames_iter = gif_decoder.into_frames();

    let mut frames = Vec::new();
    for frame_result in frames_iter {
        let frame = frame_result?;
        let delay: Duration = frame.delay().into();
        let buffer = frame.into_buffer();
        let (width, height) = (buffer.width(), buffer.height());
        frames.push(DecodedFrame {
            width,
            height,
            rgba8: buffer.into_raw(),
            delay: Some(delay),
        });
    }

    if frames.is_empty() {
        return Err(ImageProviderError::NoFrames);
    }

    if frames.len() == 1 {
        frames[0].delay = None;
    }

    Ok(DecodedImage { frames })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_1x1_png_rgb(path: &Path) {
        let img = image::RgbImage::from_pixel(1, 1, image::Rgb([255, 0, 0]));
        img.save(path).unwrap();
    }

    #[test]
    fn decodes_minimal_rgb_png_to_rgba() {
        let mut path = std::env::temp_dir();
        path.push("ql_test_zune_1x1.png");
        write_1x1_png_rgb(&path);

        let result = decode(&path, InMemoryImageKind::Png).unwrap();
        assert_eq!(result.frames.len(), 1);
        assert!(!result.is_animated());
        assert_eq!(result.frames[0].width, 1);
        assert_eq!(result.frames[0].height, 1);
        assert_eq!(result.frames[0].rgba8, vec![255, 0, 0, 255]);

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn missing_file_returns_io_error() {
        let path = Path::new("/nonexistent/path/does_not_exist.png");
        let result = decode(path, InMemoryImageKind::Png);
        assert!(matches!(result, Err(ImageProviderError::Io(_))));
    }

    #[test]
    fn luma_expansion_produces_correct_rgba() {
        let luma_samples: [u8; 2] = [10, 200];
        let out = expand_to_rgba8(&luma_samples, ColorSpace::Luma).unwrap();
        assert_eq!(out, vec![10, 10, 10, 255, 200, 200, 200, 255]);
    }

    #[test]
    fn luma_alpha_expansion_preserves_alpha() {
        let luma_alpha_samples: [u8; 4] = [10, 128, 200, 64];
        let out = expand_to_rgba8(&luma_alpha_samples, ColorSpace::LumaA).unwrap();
        assert_eq!(out, vec![10, 10, 10, 128, 200, 200, 200, 64]);
    }

    #[test]
    fn rgb_expansion_adds_opaque_alpha() {
        let rgb_samples: [u8; 6] = [255, 0, 0, 0, 255, 0];
        let out = expand_to_rgba8(&rgb_samples, ColorSpace::RGB).unwrap();
        assert_eq!(out, vec![255, 0, 0, 255, 0, 255, 0, 255]);
    }
}
