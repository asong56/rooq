//! 文本/代码查看器主模块。
//!
//! 职责划分：
//! - `highlight.rs`：tree-sitter/syntastica 高亮逻辑，纯函数式，不涉及文件IO或线程。
//! - `markdown.rs`：egui_commonmark 集成。
//! - 本文件：编码探测、大文件的"只处理可见视口"策略、
//!   以及"先显示无高亮文本、高亮在后台线程完成后再刷新"的异步调度。
//!
//! 大文件策略是本模块里对"极速"这个要求影响最大的设计决定：
//! 高亮引擎本身再快，如果对一个几十万行的文件一次性生成 egui LayoutJob，
//! 布局引擎的开销依然会造成明显卡顿。所以这里维持一份"每行的高亮结果缓存"，
//! 但只在用户实际滚动到某个区间时才去请求（如果还没缓存）计算该区间对应的
//! LayoutJob，不可见的行不产生任何 UI 部件。

pub mod highlight;
pub mod markdown;

use crate::core::request_gen::RequestToken;
use encoding_rs::Encoding;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TextProviderError {
    #[error("读取文件失败: {0}")]
    Io(#[from] std::io::Error),
    #[error("文件不是有效的文本内容（可能是被误判的二进制文件）")]
    NotText,
}

/// 读取文件并做编码探测，统一转成 UTF-8 String 返回。
///
/// 编码探测策略：
/// 1. 先看有没有 BOM（UTF-8/UTF-16 LE/BE 都有明确的 BOM 字节序列可以直接判断）。
/// 2. 没有 BOM 则用 `chardetng` 基于内容统计做启发式探测
///    （常见于国内用户会遇到的 GBK/GB2312 编码文本文件，没有 BOM 标记）。
/// 3. 探测出的编码用 `encoding_rs` 解码；如果连 chardetng 都判断不出，
///    默认按 UTF-8 lossy 解码（乱码好过完全打不开）。
pub fn read_as_text(path: &Path) -> Result<String, TextProviderError> {
    let bytes = std::fs::read(path)?;

    // BOM 检测优先于内容探测，因为 BOM 是显式声明，比统计推断更可靠。
    if let Some((encoding, bom_len)) = Encoding::for_bom(&bytes) {
        let (decoded, _, _) = encoding.decode(&bytes[bom_len..]);
        return Ok(decoded.into_owned());
    }

    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(&bytes, true);
    let encoding = detector.guess(None, true);
    let (decoded, _, had_errors) = encoding.decode(&bytes);

    // had_errors 为 true 通常意味着这不是文本文件（比如误判的二进制文件），
    // 但 dispatcher 已经把明确的二进制格式（图片/PDF等）分流走了，
    // 这里走到 NotText 分支的概率很低，主要是兜底防御。
    if had_errors && decoded.trim().is_empty() {
        return Err(TextProviderError::NotText);
    }

    Ok(decoded.into_owned())
}

/// 大文件的行索引缓存：只存"每一行在原始字符串里的字节范围"，
/// 不复制文本内容本身，用于快速定位"第N行到第M行"对应的字符串切片，
/// 而不需要每次都从头扫描换行符。
///
/// 这个索引本身的构建是 O(n) 一次扫描，对几十MB的文本文件也是毫秒级，
/// 真正昂贵的高亮计算被推迟到"只算可见部分"这一步。
pub struct LineIndex {
    /// 每个元素是 (start_byte, end_byte)，不含行尾换行符。
    line_ranges: Vec<(usize, usize)>,
}

impl LineIndex {
    pub fn build(source: &str) -> Self {
        let mut ranges = Vec::new();
        let mut start = 0usize;
        for (i, ch) in source.char_indices() {
            if ch == '\n' {
                ranges.push((start, i));
                start = i + 1;
            }
        }
        if start < source.len() {
            ranges.push((start, source.len()));
        }
        Self { line_ranges: ranges }
    }

    pub fn total_lines(&self) -> usize {
        self.line_ranges.len()
    }

    /// 取 [start_line, end_line) 区间对应的原始文本切片（含内部换行符，
    /// 不含区间外的内容）。用于把"可见视口对应的原始文本"抽出来，
    /// 单独喂给 highlight::highlight_source，而不是处理整个文件。
    ///
    /// 注意：这里独立高亮"某一段"和 highlight.rs 里说的"仍对全文解析
    /// 只是渲染裁剪"是两种不同的策略取舍，具体用哪种取决于文件大小：
    /// - 中小文件：直接走 highlight_source 处理全文，简单可靠。
    /// - 超大文件（超过一个可配置的行数阈值，比如5万行）：
    ///   为了避免连"解析"这一步都变慢，改为只对视口附近的文本切片单独
    ///   调用 highlight_source。代价是丢失了"多行结构跨越切片边界"时
    ///   的正确性（比如一个跨越几万行的巨型字符串字面量），
    ///   这是一个已知的、为了性能主动接受的妥协，对绝大多数真实代码/日志
    ///   文件（不会有这种极端跨度的单一token）不构成实际问题。
    pub fn slice_for_lines<'a>(&self, source: &'a str, start_line: usize, end_line: usize) -> &'a str {
        if self.line_ranges.is_empty() {
            return "";
        }
        let start_line = start_line.min(self.line_ranges.len() - 1);
        let end_line = end_line.min(self.line_ranges.len());
        if start_line >= end_line {
            return "";
        }
        let start_byte = self.line_ranges[start_line].0;
        let end_byte = self.line_ranges[end_line - 1].1;
        &source[start_byte..end_byte]
    }
}

/// 视口高亮请求：调用方（UI滚动回调）在可见行区间变化时构造一个请求，
/// 丢到后台线程池处理，避免高亮计算阻塞 UI 主循环导致的卡顿感。
pub struct ViewportHighlightRequest {
    pub path: PathBuf,
    pub lang: Option<syntastica_parsers::Lang>,
    pub start_line: usize,
    pub end_line: usize,
    pub token: RequestToken,
}

pub struct ViewportHighlightResult {
    pub start_line: usize,
    pub lines: Vec<highlight::HighlightedLine>,
    pub token: RequestToken,
}

/// 启动一个后台高亮工作线程，返回请求发送端和结果接收端。
///
/// 设计说明：用一个专门的长驻线程 + channel，而不是每次请求都 spawn
/// 新线程，理由是用户快速滚动时可能连续产生大量视口变化请求，
/// 频繁 spawn/join 线程本身也有不可忽视的开销；长驻线程 + channel
/// 的排队模型能让"取消过期请求"（配合 RequestToken）更自然地实现——
/// 线程在处理下一个请求前，先检查 token 是否仍然 current，
/// 不 current 的请求直接跳过不处理，省掉无意义的计算。
pub fn spawn_highlight_worker() -> (Sender<ViewportHighlightRequest>, Receiver<ViewportHighlightResult>) {
    let (req_tx, req_rx) = mpsc::channel::<ViewportHighlightRequest>();
    let (res_tx, res_rx) = mpsc::channel::<ViewportHighlightResult>();

    thread::spawn(move || {
        // 主题在工作线程里常驻一份，避免每次请求重新构造；
        // 后续如果要支持"用户切换深色/浅色主题"，这里需要改成可替换的
        // 共享状态（比如 Arc<Mutex<ResolvedTheme>>），当前范围内先固定一个默认主题。
        let theme = syntastica_themes::one::dark();

        for req in req_rx {
            if !req.token.is_still_current() {
                // 请求在排队等待处理的过程中已经过期
                // （用户已经切换到了别的文件/别的视口范围），直接丢弃。
                continue;
            }

            let Some(lang) = req.lang else {
                // 没识别出语言：不产生高亮结果，调用方应当回退到
                // 纯文本展示（灰色/默认前景色），不需要走这条 worker 路径，
                // 但如果确实走到了这里，安全地跳过即可。
                continue;
            };

            let content = match read_as_text(&req.path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let index = LineIndex::build(&content);
            let slice = index.slice_for_lines(&content, req.start_line, req.end_line);

            if let Ok(lines) = highlight::highlight_source(slice, lang, &theme) {
                if req.token.is_still_current() {
                    let _ = res_tx.send(ViewportHighlightResult {
                        start_line: req.start_line,
                        lines,
                        token: req.token,
                    });
                }
            }
        }
    });

    (req_tx, res_rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_index_handles_simple_text() {
        let text = "line0\nline1\nline2";
        let idx = LineIndex::build(text);
        assert_eq!(idx.total_lines(), 3);
        assert_eq!(idx.slice_for_lines(text, 0, 1), "line0");
        assert_eq!(idx.slice_for_lines(text, 1, 3), "line1\nline2");
    }

    #[test]
    fn line_index_handles_trailing_newline() {
        let text = "a\nb\n";
        let idx = LineIndex::build(text);
        // 末尾换行符后没有内容，不应该产生一个"空的第三行"
        assert_eq!(idx.total_lines(), 2);
    }

    #[test]
    fn line_index_handles_empty_string() {
        let idx = LineIndex::build("");
        assert_eq!(idx.total_lines(), 0);
        assert_eq!(idx.slice_for_lines("", 0, 1), "");
    }

    #[test]
    fn reads_utf8_bom_file() {
        let mut path = std::env::temp_dir();
        path.push("ql_test_bom.txt");
        let mut bytes = vec![0xEF, 0xBB, 0xBF]; // UTF-8 BOM
        bytes.extend_from_slice("你好世界".as_bytes());
        std::fs::write(&path, &bytes).unwrap();

        let result = read_as_text(&path).unwrap();
        assert_eq!(result, "你好世界");
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn reads_plain_utf8_without_bom() {
        let mut path = std::env::temp_dir();
        path.push("ql_test_plain.txt");
        std::fs::write(&path, "hello world").unwrap();

        let result = read_as_text(&path).unwrap();
        assert_eq!(result, "hello world");
        std::fs::remove_file(path).ok();
    }
}
