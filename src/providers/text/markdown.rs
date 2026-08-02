//! Markdown 查看器，基于 `egui_commonmark`。
//!
//! 选型说明：在 egui 路线下，Markdown 渲染没有必要自己搭一套
//! "AST -> egui widget"的映射层——`egui_commonmark` 就是专门做这件事的
//! 成熟三方 crate，支持 CommonMark 完整语法外加 GitHub 风格扩展
//! （表格、删除线、任务列表、脚注）。
//!
//! 已知取舍（详见 Cargo.toml 里的注释）：该 crate 的代码块高亮
//! （`better_syntax_highlighting` feature）内部走的是 syntect，
//! 不是本项目主代码查看器采用的 tree-sitter/syntastica。
//! 这是一处局部的、范围很小的不一致，只影响 markdown 内嵌代码块的高亮引擎，
//! 不影响本项目"文本/代码查看器不照搬syntect"这个决定的核心场景
//! （那个场景对应的是"用户直接预览一个.rs/.py等代码文件"，
//! 与"预览一个.md文件里恰好嵌了一段代码"是两个不同的使用路径）。

use egui_commonmark::{CommonMarkCache, CommonMarkViewer};

/// Markdown 渲染状态。`CommonMarkCache` 内部会缓存图片句柄等跨帧资源，
/// 所以需要在 provider 里持有一个实例并复用，而不是每帧新建
/// ——immediate-mode GUI 里，"重建缓存"等价于"每帧重新解析和重新加载所有资源"，
/// 会造成不必要的重复工作，尤其是文档里包含图片引用时。
pub struct MarkdownProvider {
    cache: CommonMarkCache,
}

impl Default for MarkdownProvider {
    fn default() -> Self {
        Self {
            cache: CommonMarkCache::default(),
        }
    }
}

impl MarkdownProvider {
    pub fn new() -> Self {
        Self::default()
    }

    /// 渲染一份 Markdown 内容。`content` 通常是
    /// `providers::text::read_as_text` 读出来的结果——
    /// Markdown 复用同一套编码探测逻辑（BOM/chardetng），
    /// 不需要单独实现一遍。
    pub fn render(&mut self, ui: &mut egui::Ui, content: &str) {
        CommonMarkViewer::new().show(ui, &mut self.cache, content);
    }
}

#[cfg(test)]
mod tests {
    // Markdown 渲染依赖 egui::Ui 上下文，真正的渲染效果需要在
    // eframe 的事件循环里跑起来才能验证（这类 UI 渲染代码通常靠人工
    // 目视检查而非单元测试）。这里只做最基础的"能否构造 provider"验证，
    // 更完整的验证请在你本机运行主程序后目视检查渲染效果。
    use super::*;

    #[test]
    fn provider_constructs_without_panicking() {
        let _provider = MarkdownProvider::new();
    }
}
