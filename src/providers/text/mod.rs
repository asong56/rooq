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

// BOM checked first (explicit), then chardetng heuristics for BOM-less encodings like GBK.
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
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
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
