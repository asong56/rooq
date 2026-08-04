//! 图片 InMemory 路径：jpg/png/gif 的进程内解码。
//!
//! 选型（本次调整）：
//! - **jpg**: `zune-jpeg`，不是 `image` crate。SIMD 加速，依赖树小，
//!   速度接近 libjpeg-turbo；`image` crate 自己的维护者甚至在讨论
//!   把 zune 的快速路径吸收进主线，这不是一个边缘选择。
//! - **png**: `zune-png`，同一个 zune-image 项目下的姊妹 crate，
//!   同样是 SIMD 加速、依赖树小。
//! - **gif**: 仍然用 `image` crate。zune 生态目前没有成熟的 gif 动图支持，
//!   `image` crate 的 `AnimationDecoder` 已经很完善（帧+delay的抽象很干净），
//!   这里没有必要为了"整体统一用一个库"而牺牲 gif 这块的成熟度——
//!   "各用各的强项"比"图省事整体套用一个通用库"更贴近
//!   "刚好覆盖需要的功能、不多不少"这个目标。
//!
//! webp/avif 不在本文件处理范围内：走 onas_bridge 子进程转出临时 PNG，
//! 再回到这里的 PNG 分支解码（见 core/window.rs 的 load_onas_image）。

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
    #[error("无法打开文件: {0}")]
    Io(#[from] std::io::Error),
    #[error("JPEG 解码失败: {0}")]
    JpegDecode(String),
    #[error("PNG 解码失败: {0}")]
    PngDecode(String),
    #[error("GIF 解码失败: {0}")]
    GifDecode(#[from] ImageError),
    #[error("文件没有帧数据（可能是空文件或已损坏）")]
    NoFrames,
    #[error("非预期的PNG位深（当前仅处理8位深度，遇到其他位深需要额外的缩放逻辑）")]
    UnsupportedBitDepth,
}

/// 单帧图片数据，尺寸 + 紧凑排列的 RGBA8 像素。
/// 统一转成 RGBA8 是为了让上层 UI 渲染代码（egui 的 ColorImage）
/// 不需要关心图片原始格式/色彩空间的差异——多种 decoder 共用同一套上屏管线。
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub rgba8: Vec<u8>,
    /// 该帧在动图序列里应当停留的时长；静态图为 None。
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

    // 直接要求解码器输出 RGBA，省掉自己再做一次色彩空间转换的工作——
    // zune-jpeg 原生支持输出目标色彩空间，这一步在解码内部完成，
    // 比"解码到RGB再手动扩展alpha"更省一次遍历。
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(&bytes), options);

    let pixels = decoder
        .decode()
        .map_err(|e| ImageProviderError::JpegDecode(e.to_string()))?;
    let info = decoder
        .info()
        .ok_or_else(|| ImageProviderError::JpegDecode("无法读取图像尺寸信息".into()))?;

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

    // 不依赖 `png_set_add_alpha_channel`——文档明确警告这个选项对 Luma
    // 输入只会转成 LumaA（灰度+alpha），不会转成 RGBA。为了保证统一拿到
    // RGBA8（不管原图是 Luma/LumaA/RGB/RGBA 哪种），这里自己读原始
    // colorspace 后手动扩展，比依赖库内选项更可控。
    let mut decoder = PngDecoder::new(ZCursor::new(&bytes));
    let result = decoder
        .decode()
        .map_err(|e| ImageProviderError::PngDecode(e.to_string()))?;

    let colorspace = decoder
        .get_colorspace()
        .ok_or_else(|| ImageProviderError::PngDecode("无法读取色彩空间信息".into()))?;
    let info = decoder
        .get_info()
        .ok_or_else(|| ImageProviderError::PngDecode("无法读取图像尺寸信息".into()))?;
    let (width, height) = (info.width as u32, info.height as u32);

    // 16位深度图暂不处理精细缩放（简单截断到8位在预览场景下不是不可接受，
    // 但为了不悄悄引入精度损失且不声明，这里选择明确报错而不是静默降级——
    // 如果实际使用中遇到16位PNG较多，需要回来加上到8位的正确缩放逻辑）。
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

/// 把 zune-png 解出的原始分量数据（Luma/LumaA/RGB/RGBA其中之一）
/// 统一扩展成紧凑排列的 RGBA8。
fn expand_to_rgba8(
    samples: &[u8],
    colorspace: ColorSpace,
) -> Result<Vec<u8>, ImageProviderError> {
    let n = colorspace.num_components();
    match n {
        1 => {
            // Luma: 每个分量复制三次做灰度到RGB，alpha固定不透明
            let mut out = Vec::with_capacity(samples.len() * 4);
            for &l in samples {
                out.extend_from_slice(&[l, l, l, 255]);
            }
            Ok(out)
        }
        2 => {
            // LumaA: (L, A) 交替
            let mut out = Vec::with_capacity(samples.len() * 2);
            for chunk in samples.chunks_exact(2) {
                let (l, a) = (chunk[0], chunk[1]);
                out.extend_from_slice(&[l, l, l, a]);
            }
            Ok(out)
        }
        3 => {
            // RGB: 补一个不透明alpha
            let mut out = Vec::with_capacity(samples.len() / 3 * 4);
            for chunk in samples.chunks_exact(3) {
                out.extend_from_slice(chunk);
                out.push(255);
            }
            Ok(out)
        }
        4 => Ok(samples.to_vec()), // 已经是RGBA，原样返回
        _ => Err(ImageProviderError::PngDecode(format!(
            "非预期的分量数: {n}"
        ))),
    }
}

fn decode_gif(path: &Path) -> Result<DecodedImage, ImageProviderError> {
    // gif 保留用 image crate（理由见文件头注释）。
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
        // 用 image crate 只是为了生成测试素材文件本身，
        // 不影响本模块实际解码路径用的是 zune-png——
        // 测试素材的生成方式和被测代码的实现路径是两回事。
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
        // 直接测试 expand_to_rgba8 的转换算法本身，不依赖真实PNG文件，
        // 验证 Luma(1分量) -> RGBA(4分量) 的扩展逻辑正确。
        let luma_samples: [u8; 2] = [10, 200]; // 两个灰度像素
        let out = expand_to_rgba8(&luma_samples, ColorSpace::Luma).unwrap();
        assert_eq!(out, vec![10, 10, 10, 255, 200, 200, 200, 255]);
    }

    #[test]
    fn luma_alpha_expansion_preserves_alpha() {
        let luma_alpha_samples: [u8; 4] = [10, 128, 200, 64]; // (L,A) x2
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
