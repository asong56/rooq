//! 代码高亮：基于 tree-sitter 的 syntastica，取代原版 QuickLook TextViewer
//! 用的 syntect（正则匹配）方案。
//!
//! 选型理由（对应方案文档第4节）：syntect 是逐行状态机 + 正则规则集合，
//! 本质上是"模式匹配伪装成理解代码"；tree-sitter 是真正解析出语法树，
//! 是 Neovim/Helix/Zed 等当前主流工具的选择，高亮更准确，
//! 且 syntastica 项目本身就明确定位为"syntect 的现代替代品"。
//!
//! API 设计说明：syntastica 的核心产出类型是
//! `ThemedHighlights<'src> = Vec<Vec<(&'src str, Option<Style>)>>`
//! ——外层 Vec 是行，内层 Vec 是该行内的高亮片段。这个结构和 egui 的
//! `LayoutJob`（也是"整段文本 + 一组 (range, format) 片段"的模型）
//! 几乎是天然契合的，所以这里不需要走 syntastica 提供的
//! TerminalRenderer/HtmlRenderer，而是直接用 `resolve_styles` 拿到
//! `ThemedHighlights`，自己拼一次 egui::text::LayoutJob。

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId};
use syntastica::renderer::resolve_styles;
use syntastica::style::Style as SynStyle;
use syntastica::{Processor, ThemedHighlights};
use syntastica_parsers::{Lang, LanguageSetImpl};

/// 支持的语言集合。dispatcher 不关心具体语言（它只知道"这是文本/代码"），
/// 语言识别放在这一层，理由：新增一种语言支持只需要改这里的映射表，
/// 不需要动 dispatcher 的分类逻辑，两者关注点分开。
///
/// 覆盖范围对应 `syntastica-parsers` 的 "some" feature 档，
/// 是目前体积和覆盖面之间的折中选择（见 Cargo.toml 注释）。
/// 后续如果需要支持更多冷门语言，把 Cargo.toml 里的 feature 换成 "most"
/// 或 "all"，并在下面 match 里补上映射即可，不需要动其他代码。
pub fn detect_language(path: &std::path::Path) -> Option<Lang> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => Lang::Rust,
        "py" | "pyw" => Lang::Python,
        "js" | "mjs" | "cjs" => Lang::Javascript,
        "ts" | "mts" | "cts" => Lang::Typescript,
        "tsx" => Lang::Tsx,
        "go" => Lang::Go,
        "c" | "h" => Lang::C,
        "cpp" | "cc" | "cxx" | "hpp" | "hh" => Lang::Cpp,
        "java" => Lang::Java,
        "json" => Lang::Json,
        "yaml" | "yml" => Lang::Yaml,
        "toml" => Lang::Toml,
        "sh" | "bash" | "zsh" => Lang::Bash,
        "html" | "htm" => Lang::Html,
        "css" => Lang::Css,
        "xml" => Lang::Xml,
        "sql" => Lang::Sql,
        "rb" => Lang::Ruby,
        "php" => Lang::Php,
        "lua" => Lang::Lua,
        // 未识别的扩展名返回 None，调用方（highlight_lines）在这种情况下
        // 应当直接展示无高亮的纯文本，而不是报错——
        // "看不懂的语言就不高亮"是比"报错拒绝显示"更友好的降级路径。
        _ => return None,
    })
}

/// 高亮结果：每一行是一组 (文本切片, 可选颜色/样式) 的片段列表。
/// 这是从 syntastica 的 ThemedHighlights 转换出来的中间表示，
/// 用 owned String 而不是借用原始文本的切片，是因为上层（大文件按视口
/// 渲染场景）经常需要跨线程传递高亮结果（后台线程解析完，主线程UI消费），
/// 生命周期borrow在这种场景下会很麻烦，owned换取的简单性更划算。
pub struct HighlightedLine {
    pub spans: Vec<(String, Option<SpanStyle>)>,
}

#[derive(Clone, Copy)]
pub struct SpanStyle {
    pub color: Color32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
}

impl From<SynStyle> for SpanStyle {
    fn from(s: SynStyle) -> Self {
        let c = s.color();
        SpanStyle {
            color: Color32::from_rgb(c.red, c.green, c.blue),
            bold: s.bold(),
            italic: s.italic(),
            underline: s.underline(),
            strikethrough: s.strikethrough(),
        }
    }
}

/// 对完整源码做一次高亮，返回逐行的高亮片段。
///
/// 这是"小文件/中等文件"的直接路径：一次性处理整个文件。
/// 大文件（见 viewport 模块）不会走这条路，而是只对可见行区间调用
/// 高亮，避免对几十万行的文件做一次性全量 tree-sitter 解析造成的卡顿。
///
/// 注意：tree-sitter 的解析本身是"整棵语法树"的概念，理论上不能只解析
/// 文件的某一段而完全不管上下文（比如一个多行字符串跨越了视口边界，
/// 只解析可见部分会得到错误的语法边界）。这里的处理策略是：
/// 仍然对全文做一次 tree-sitter 解析（这一步其实很快，tree-sitter的
/// 增量解析设计使得即使是大文件，纯解析开销也远小于"每次都重新走
/// 正则状态机扫描全文"的 syntect 方案），但只把"需要渲染成 egui
/// 部件"这一步限制在可见行范围——真正昂贵的不是解析本身，而是
/// "把每一行都变成 egui 的 LayoutJob 片段并丢给布局引擎"这一步，
/// 这一步才是 viewport 裁剪要优化的目标。
pub fn highlight_source(
    source: &str,
    lang: Lang,
    theme: &syntastica::theme::ResolvedTheme,
) -> Result<Vec<HighlightedLine>, HighlightError> {
    let language_set = LanguageSetImpl::new();
    let mut processor = Processor::new(&language_set);
    let highlights: syntastica::Highlights = processor
        .process(source, lang)
        .map_err(|e| HighlightError::Parse(e.to_string()))?;

    let themed: ThemedHighlights = resolve_styles(highlights, theme);

    let lines = themed
        .into_iter()
        .map(|line_spans| HighlightedLine {
            spans: line_spans
                .into_iter()
                .map(|(text, style)| (text.to_string(), style.map(SpanStyle::from)))
                .collect(),
        })
        .collect();

    Ok(lines)
}

#[derive(Debug, thiserror::Error)]
pub enum HighlightError {
    #[error("语法解析失败: {0}")]
    Parse(String),
}

/// 把一组已经算好的高亮行，拼装成 egui 可以直接渲染的 LayoutJob。
///
/// 这个函数故意只接受"已经切好的行区间"（`lines[start..end]`的调用方式），
/// 而不是整个文件——这是 viewport 裁剪生效的地方：调用方（text/mod.rs 里
/// 的滚动回调）只把当前可见的行传进来，不可见的行完全不会走到这一步，
/// 也就不会产生对应的 egui 部件，这是"大文件极速"这个目标真正落地的位置。
pub fn build_layout_job(lines: &[HighlightedLine], base_font: FontId) -> LayoutJob {
    let mut job = LayoutJob::default();

    for line in lines {
        for (text, style) in &line.spans {
            let format = match style {
                Some(s) => {
                    let mut fmt = TextFormat {
                        font_id: base_font.clone(),
                        color: s.color,
                        underline: if s.underline {
                            egui::Stroke::new(1.0, s.color)
                        } else {
                            egui::Stroke::NONE
                        },
                        strikethrough: if s.strikethrough {
                            egui::Stroke::new(1.0, s.color)
                        } else {
                            egui::Stroke::NONE
                        },
                        ..Default::default()
                    };
                    if s.italic {
                        // egui 的斜体需要选用斜体字体变体；这里假设调用方
                        // 已经在 base_font 对应的字体族里注册了斜体样式，
                        // 具体字体注册逻辑属于 core/window.rs 初始化阶段的职责，
                        // 不在本模块处理范围内。
                        fmt.italics = true;
                    }
                    fmt
                }
                None => TextFormat {
                    font_id: base_font.clone(),
                    color: Color32::GRAY,
                    ..Default::default()
                },
            };
            job.append(text, 0.0, format);
        }
        job.append("\n", 0.0, TextFormat::default());
    }

    job
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn detects_rust_from_extension() {
        assert!(matches!(
            detect_language(Path::new("main.rs")),
            Some(Lang::Rust)
        ));
    }

    #[test]
    fn unknown_extension_returns_none() {
        assert!(detect_language(Path::new("file.zzz")).is_none());
    }

    #[test]
    fn highlights_simple_rust_snippet() {
        let theme = syntastica_themes::one::dark();
        let result = highlight_source("fn main() {}", Lang::Rust, &theme);
        assert!(result.is_ok());
        let lines = result.unwrap();
        assert_eq!(lines.len(), 1);
        // 至少应该有非空的高亮片段
        assert!(!lines[0].spans.is_empty());
    }
}
