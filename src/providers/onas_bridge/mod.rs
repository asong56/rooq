//! onas_bridge：Rooq 与 onas 子进程之间的桥接层。
//!
//! **当前范围：只接了图片的 webp/avif 分支**
//! （对应 `dispatcher::OnasReason::ImageWebpOrAvif`）。
//!
//! 视频（`OnasReason::VideoMkvOrWebm`）没有接进来——不是图省事没做，
//! 是核实过 onas 源码（v0.2.0）之后确认它现有 CLI 压根没有"从视频文件里
//! 拿出一帧图像"的能力：
//!
//! - `onas video <input> <output.mkv>` 只有"整个文件从头到尾解码、
//!   重新编码（或至少完整解复用）、再写出一个完整 mkv"这一条路径，
//!   没有按时间戳/帧号取单帧的参数，也没有任何输出图片格式的选项。
//! - `onas image` 的格式探测（`Fmt::from_path`）只认
//!   jpg/png/webp/avif/jxl 五种扩展名，传一个 .mkv/.webm 进去在真正开始
//!   处理前就会直接报错退出。
//!
//! 也就是说，即使愿意为了一张缩略图付出"整个视频转码一遍"的代价，
//! 转码完拿到的还是另一个 .mkv 文件，Rooq 自己没有视频帧解码器，
//! 依然拿不到能上屏的像素——这不是"慢一点但能用"的取舍题，是现有接口
//! 下根本走不通。要打通这个分支，需要先给 onas 加一个新的子命令
//! （复用它已有的 H.264/H.265/VP9/AV1 解码器，解出一帧就停、编码成图片，
//! 跳过整段重新编码+封装 mkv 的流程），这是 onas 那边的改造，
//! 不是这一层 subprocess.rs 能绕过去的问题。在那之前，
//! `core/window.rs` 里 `VideoMkvOrWebm` 分支继续走占位提示。

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
