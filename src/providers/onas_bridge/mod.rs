//! onas_bridge：Rooq 与 onas 子进程之间的桥接层。
//!
//! 覆盖图片的 webp/avif 分支（`OnasReason::ImageWebpOrAvif`）和视频的
//! mkv/webm 缩略图分支（`OnasReason::VideoMkvOrWebm`）。
//!
//! 视频分支曾经被现有 onas 接口挡住：旧版 onas 只有 `onas video
//! <input> <output.mkv>`——整个文件从头到尾解码、重新编码、写出另一个
//! 完整 mkv，没有按时间戳/帧号取单帧的参数，也没有任何输出图片格式的
//! 选项。即使愿意付出"整段转码"的代价，拿到手的依然是另一个 mkv，
//! Rooq 自己没有视频帧解码器，还是拿不到能上屏的像素。
//!
//! **现状（onas v0.2.0 新增 `frame` 子命令后已打通）**：onas 现在有
//! `onas frame <input> <output>` ——复用它已有的 H.264/H.265/VP9/AV1
//! 解码器，解出单帧就停、直接编码成 PNG/JPEG 写出，不再需要整段转码。
//! Rooq 这边用法和 webp/avif 图片分支完全一致：子进程转出一张临时 PNG，
//! 再复用现成的 PNG InMemory 解码路径读出 RGBA8。

mod subprocess;

pub use subprocess::OnasBridgeError;

use std::path::{Path, PathBuf};

/// 一次性临时文件的 RAII 包装：作用域结束时（正常返回、提前 return，
/// 或者 panic 展开）自动删除磁盘上的文件。
///
/// 呼应 `providers/pdf.rs` 里"纯内存缓存、不落盘"的既定原则——onas 的
/// CLI 只支持写文件不支持 stdout，这里的落盘无法避免，但生命周期必须
/// 严格收紧到"这一次转换调用期间"：拿到 guard 后立刻读出 RGBA 数据，
/// 读完就可以让 guard 出作用域，不会在磁盘上留下任何持久残留。
pub struct TempPngFile(PathBuf);

impl TempPngFile {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempPngFile {
    fn drop(&mut self) {
        // 删除失败（比如文件已经被外部清理、或者从未成功写出过）不是
        // 需要向上层报告的错误——析构函数里也没有合适的地方报告。
        let _ = std::fs::remove_file(&self.0);
    }
}

/// 把一张 webp/avif 图片转换成一个临时 PNG 文件（子进程调用
/// `onas image <input> <临时路径>.png`）。
///
/// 用 PNG 做中转格式：onas 内部的 PNG 编码器是无损的（image crate 的
/// PNG 编码路径），不会在 webp/avif 原图之上引入二次有损；而 PNG 解码
/// 这一端 Rooq 自己已经有 zune-png 实现
/// （见 `providers::image::decode`），不需要为"读 onas 的输出"
/// 再单独写一条解码路径——拿到这个函数返回的临时文件后，直接调用
/// `providers::image::decode(guard.path(), InMemoryImageKind::Png)`
/// 复用已有管线即可。
///
/// 返回的 `TempPngFile` 是 RAII guard，读完像素数据后让它自然析构，
/// 临时文件会被自动删除，调用方不需要手动清理。
pub fn convert_image_to_png(input: &Path) -> Result<TempPngFile, OnasBridgeError> {
    let output = subprocess::temp_output_path("png");
    subprocess::run_onas_image_convert(input, &output)?;
    Ok(TempPngFile(output))
}

/// 从一个 mkv/webm 视频里提取一帧，输出成临时 PNG 文件（子进程调用
/// `onas frame <input> <临时路径>.png`）。用法和上面的 `convert_image_to_png`
/// 完全对称：同样用 PNG 做中转格式（无损，读取端复用 zune-png），
/// 同样返回 RAII guard，读完像素后自动删除临时文件，不落盘残留。
pub fn extract_video_frame(input: &Path) -> Result<TempPngFile, OnasBridgeError> {
    let output = subprocess::temp_output_path("png");
    subprocess::run_onas_frame_extract(input, &output)?;
    Ok(TempPngFile(output))
}
