// Magic bytes are trusted over extension: users rename files often.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileCategory {
    Image(ImageRoute),
    Pdf,
    Text(TextKind),
    RequiresOnas(OnasReason),
    RequiresFfmpeg(FfmpegReason),
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageRoute {
    InMemory(InMemoryImageKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InMemoryImageKind {
    Jpeg,
    Png,
    Gif,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnasReason {
    ImageWebpOrAvif,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfmpegReason {
    VideoMkvOrWebm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKind {
    Markdown,
    PlainOrCode,
}

pub fn detect(path: &Path) -> FileCategory {
    let sniffed = infer::get_from_path(path).ok().flatten();

    if let Some(kind) = sniffed {
        if let Some(category) = category_from_mime(kind.mime_type()) {
            return category;
        }
    }

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
        "image/webp" | "image/avif" => Some(FileCategory::RequiresOnas(OnasReason::ImageWebpOrAvif)),
        "application/pdf" => Some(FileCategory::Pdf),
        "video/x-matroska" | "video/webm" => {
            Some(FileCategory::RequiresFfmpeg(FfmpegReason::VideoMkvOrWebm))
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
        return FileCategory::Text(TextKind::PlainOrCode);
    };

    match ext.as_str() {
        "jpg" | "jpeg" => FileCategory::Image(ImageRoute::InMemory(InMemoryImageKind::Jpeg)),
        "png" => FileCategory::Image(ImageRoute::InMemory(InMemoryImageKind::Png)),
        "gif" => FileCategory::Image(ImageRoute::InMemory(InMemoryImageKind::Gif)),
        "webp" | "avif" => FileCategory::RequiresOnas(OnasReason::ImageWebpOrAvif),
        "mkv" | "webm" => FileCategory::RequiresFfmpeg(FfmpegReason::VideoMkvOrWebm),
        "pdf" => FileCategory::Pdf,
        "md" | "markdown" => FileCategory::Text(TextKind::Markdown),
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

    #[test]
    fn mkv_routes_to_ffmpeg() {
        let path = write_temp("ql_test.mkv", b"not real mkv bytes but ext matches");
        let cat = detect(&path);
        assert_eq!(
            cat,
            FileCategory::RequiresFfmpeg(FfmpegReason::VideoMkvOrWebm)
        );
        std::fs::remove_file(path).ok();
    }
}
