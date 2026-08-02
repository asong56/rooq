pub mod image;
pub mod pdf;
pub mod text;

// onas_bridge 是下一阶段的工作（webp/avif 图片 + mkv/webm 视频），
// 本次交付范围不包含，模块先不声明，避免引入还没有实现的空壳代码。
// 等 onas 的具体调用接口确认后，在这里加入：
// pub mod onas_bridge;
