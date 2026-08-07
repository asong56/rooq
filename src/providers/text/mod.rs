//! Text/code viewer. `highlight.rs` does tree-sitter/syntastica
//! highlighting; `markdown.rs` wraps egui_commonmark; this file handles
//! encoding detection.

pub mod highlight;
pub mod markdown;

use encoding_rs::Encoding;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TextProviderError {
    #[error("failed to read file: {0}")]
    Io(#[from] std::io::Error),
    #[error("file is not valid text (likely a misdetected binary file)")]
    NotText,
}

/// Reads a file and decodes it to a UTF-8 `String`.
///
/// Detection order: BOM first (explicit, more reliable than statistical
/// guessing), then `chardetng` heuristics for BOM-less encodings like GBK.
/// If even that fails, falls back to lossy UTF-8 (garbled beats unreadable).
pub fn read_as_text(path: &Path) -> Result<String, TextProviderError> {
    let bytes = std::fs::read(path)?;

    if let Some((encoding, bom_len)) = Encoding::for_bom(&bytes) {
        let (decoded, _, _) = encoding.decode(&bytes[bom_len..]);
        return Ok(decoded.into_owned());
    }

    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(&bytes, true);
    let encoding = detector.guess(None, true);
    let (decoded, _, had_errors) = encoding.decode(&bytes);

    // The dispatcher already routes known binary formats elsewhere, so this
    // is mostly a defensive fallback for misdetected files.
    if had_errors && decoded.trim().is_empty() {
        return Err(TextProviderError::NotText);
    }

    Ok(decoded.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_utf8_bom_file() {
        let mut path = std::env::temp_dir();
        path.push("ql_test_bom.txt");
        let mut bytes = vec![0xEF, 0xBB, 0xBF]; // UTF-8 BOM
        bytes.extend_from_slice("hello world".as_bytes());
        std::fs::write(&path, &bytes).unwrap();

        let result = read_as_text(&path).unwrap();
        assert_eq!(result, "hello world");
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
