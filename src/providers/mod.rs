pub mod image;
pub mod onas_bridge;
pub mod pdf;
pub mod text;

// TODO(video-thumbnail): onas_bridge 目前只实现了 webp/avif 图片分支。
// mkv/webm 视频分支仍未接入——不是没做，是核实过 onas 源码后确认它现有
// CLI 拿不出"单帧图像"，详细原因见 providers/onas_bridge/mod.rs 顶部注释
// 和 quicklook-rust-plan-v3.md 第6节。
