//! 文件类型探测与分发。
//!
//! 设计原则：优先信任文件内容的 magic bytes（用 `infer` 探测），
//! 扩展名只作为 magic bytes 探测失败时的兜底（用户重命名后缀是常态，
//! 不能只信任扩展名）。
//!
//! 本文件只负责"这个文件该交给谁处理"，不涉及具体解码逻辑。
//! 具体解码逻辑在 providers/ 下各自的模块里。

use std::path::Path;

/// 顶层的文件类别，dispatcher 探测后落到这几类之一。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileCategory {
    Image(ImageRoute),
    Pdf,
    Text(TextKind),
    /// 需要 onas 子进程协助的类别：webp/avif 图片转换、mkv/webm 视频
    /// 首帧缩略图。两者都已经在 `onas_bridge` 里接好（见该模块文档），
    /// `RequiresOnas` 这个名字保留，只是不再意味着"占位未实现"。
    RequiresOnas(OnasReason),
    Unsupported,
}

/// 图片类别再往下分流：走进程内解码，还是需要外部工具（onas）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageRoute {
    /// jpg/png/gif：`image` crate 纯 Rust 解码，进程内完成，无子进程开销。
    InMemory(InMemoryImageKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InMemoryImageKind {
    Jpeg,
    Png,
    /// gif 可能是动图，也可能是静态图，由 image.rs 里进一步判断帧数。
    Gif,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnasReason {
    ImageWebpOrAvif,
    VideoMkvOrWebm,
}

/// 文本类文件的子分类，决定 providers/text 用哪条渲染路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKind {
    /// Markdown 走 egui_commonmark 渲染。
    Markdown,
    /// 其他文本/代码走 tree-sitter 高亮；具体语言由 providers/text/highlight.rs
    /// 再根据扩展名/内容做语言识别，dispatcher 这一层不需要知道语言细节。
    PlainOrCode,
}

/// 探测文件类别。
///
/// `path` 用于兜底的扩展名判断；实际内容通过读取文件前若干字节做 magic bytes 探测。
/// 调用方需要保证 path 指向的文件存在且可读，本函数内部会做一次小范围的文件读取
/// （只读文件头部，不会加载整个文件，即使是超大文件也很快）。
pub fn detect(path: &Path) -> FileCategory {
    // infer 只需要文件头部若干字节，内部做的是 sniff，不会读全文件。
    let sniffed = infer::get_from_path(path).ok().flatten();

    if let Some(kind) = sniffed {
        if let Some(category) = category_from_mime(kind.mime_type()) {
            return category;
        }
    }

    // magic bytes 没能识别（常见于纯文本文件——文本没有统一的 magic bytes 特征），
    // 回退到扩展名判断。
    category_from_extension(path)
}

fn category_from_mime(mime: &str) -> Option<FileCategory> {
    match mime {
        "image/jpeg" => Some(FileCategory::Image(ImageRoute::InMemory(
            InMemoryImageKind::Jpeg,
        ))),
        "image/png" => Some(FileCategory::Image(ImageRoute::InMemory(
            InMemoryImageKind::Png,
        ))),
        "image/gif" => Some(FileCategory::Image(ImageRoute::InMemory(
            InMemoryImageKind::Gif,
        ))),
        "image/webp" => Some(FileCategory::RequiresOnas(OnasReason::ImageWebpOrAvif)),
        // infer 对 avif 的 mime 探测在不同版本里可能是 "image/avif"，
        // 也可能因为 avif 容器和 heic 共享 ISOBMFF 结构而需要更精细的 brand 判断；
        // 这里先按最常见的 mime 字符串处理，若后续发现漏判，
        // 需要用 infer 的底层 matcher 或手动读 ftyp box 的 brand 字段兜底。
        "image/avif" => Some(FileCategory::RequiresOnas(OnasReason::ImageWebpOrAvif)),
        "application/pdf" => Some(FileCategory::Pdf),
        "video/x-matroska" | "video/webm" => {
            Some(FileCategory::RequiresOnas(OnasReason::VideoMkvOrWebm))
        }
        _ => None,
    }
}

fn category_from_extension(path: &Path) -> FileCategory {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());

    let Some(ext) = ext else {
        // 无扩展名、magic bytes 也没识别出来：按纯文本兜底尝试，
        // 是否真的是文本（而非二进制垃圾）留给 text provider 用
        // encoding_rs/chardetng 探测时再做判断，dispatcher 这层不做二进制内容嗅探。
        return FileCategory::Text(TextKind::PlainOrCode);
    };

    match ext.as_str() {
        "jpg" | "jpeg" => FileCategory::Image(ImageRoute::InMemory(InMemoryImageKind::Jpeg)),
        "png" => FileCategory::Image(ImageRoute::InMemory(InMemoryImageKind::Png)),
        "gif" => FileCategory::Image(ImageRoute::InMemory(InMemoryImageKind::Gif)),
        "webp" | "avif" => FileCategory::RequiresOnas(OnasReason::ImageWebpOrAvif),
        "mkv" | "webm" => FileCategory::RequiresOnas(OnasReason::VideoMkvOrWebm),
        "pdf" => FileCategory::Pdf,
        "md" | "markdown" => FileCategory::Text(TextKind::Markdown),
        // 常见文本/代码后缀，覆盖面没必要在 dispatcher 里穷举——
        // 任何没被上面分支命中、且不是已知二进制格式的文件，
        // 一律先按 PlainOrCode 处理，交给 text provider 做进一步的
        // "是否真的是可读文本"判断和语言识别。这样新增语言支持
        // 不需要改 dispatcher，只需要在 highlight.rs 里注册新的语言映射。
        _ => FileCategory::Text(TextKind::PlainOrCode),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(bytes).unwrap();
        path
    }

    #[test]
    fn detects_png_by_magic_bytes_even_with_wrong_extension() {
        // PNG 文件签名，故意用 .txt 后缀，验证 magic bytes 优先于扩展名
        let png_sig: &[u8] = &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        let path = write_temp("ql_test_fake.txt", png_sig);
        let cat = detect(&path);
        assert_eq!(
            cat,
            FileCategory::Image(ImageRoute::InMemory(InMemoryImageKind::Png))
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn falls_back_to_extension_for_markdown() {
        let path = write_temp("ql_test.md", b"# hello");
        let cat = detect(&path);
        assert_eq!(cat, FileCategory::Text(TextKind::Markdown));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn unknown_extension_defaults_to_plain_text() {
        let path = write_temp("ql_test.myweirdlang", b"fn main() {}");
        let cat = detect(&path);
        assert_eq!(cat, FileCategory::Text(TextKind::PlainOrCode));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn webp_routes_to_onas() {
        let path = write_temp("ql_test.webp", b"not real webp bytes but ext matches");
        let cat = detect(&path);
        assert_eq!(
            cat,
            FileCategory::RequiresOnas(OnasReason::ImageWebpOrAvif)
        );
        std::fs::remove_file(path).ok();
    }
}
