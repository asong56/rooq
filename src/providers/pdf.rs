//! PDF viewer: renders only the first 6 pages (or fewer if the document is
//! shorter). This is a hard limit, not a configurable option — beyond
//! confirming "is this the file I'm looking for," full reading belongs in a
//! real PDF reader.
//!
//! Engine: mupdf-rs (MuPDF), AGPL-3.0 or Artifex commercial license.
//! mupdf-sys compiles and statically links MuPDF at build time, so no
//! external dynamic library needs to be shipped. Building requires a C/C++
//! toolchain and libclang (for bindgen) — see mupdf-sys's README for
//! platform setup.

use mupdf::{Colorspace, Document, Matrix};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use thiserror::Error;

pub struct DecodedPage {
    pub width: u32,
    pub height: u32,
    pub rgba8: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum PdfProviderError {
    #[error("failed to open PDF file: {0}")]
    OpenFailed(String),
    #[error("page render failed: {0}")]
    RenderFailed(String),
}

/// Cache key: path + mtime + size, cheaper than hashing full file content
/// and sufficient for the common case of "did the file change".
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    path: PathBuf,
    mtime_nanos: u128,
    size: u64,
}

impl CacheKey {
    fn from_path(path: &Path) -> std::io::Result<Self> {
        let meta = std::fs::metadata(path)?;
        let mtime = meta.modified()?;
        let mtime_nanos = mtime
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Ok(Self {
            path: path.to_path_buf(),
            mtime_nanos,
            size: meta.len(),
        })
    }
}

/// In-memory cache only: cleared on process exit, nothing written to disk.
const MAX_CACHED_DOCUMENTS: usize = 20;

pub struct PdfCache {
    entries: HashMap<CacheKey, Vec<DecodedPage>>,
    access_order: Vec<CacheKey>,
}

impl Default for PdfCache {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            access_order: Vec::new(),
        }
    }
}

impl PdfCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn touch(&mut self, key: &CacheKey) {
        if let Some(pos) = self.access_order.iter().position(|k| k == key) {
            let k = self.access_order.remove(pos);
            self.access_order.push(k);
        }
    }

    fn insert(&mut self, key: CacheKey, pages: Vec<DecodedPage>) {
        if self.entries.len() >= MAX_CACHED_DOCUMENTS && !self.entries.contains_key(&key) {
            if !self.access_order.is_empty() {
                let oldest = self.access_order.remove(0);
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(key.clone(), pages);
        self.access_order.retain(|k| k != &key);
        self.access_order.push(key);
    }
}

const MAX_PAGES: usize = 6;

/// MuPDF's `to_pixmap` takes a scale matrix rather than a target width.
/// 2.0 renders standard 72dpi PDF coordinates at roughly 144dpi, sharp
/// enough for preview without slowing down the first render.
const RENDER_SCALE: f32 = 2.0;

pub struct PdfProvider {
    cache: PdfCache,
}

impl PdfProvider {
    pub fn new() -> Self {
        Self {
            cache: PdfCache::new(),
        }
    }

    /// Returns (rendering if necessary) the first N pages, N = min(6, total
    /// pages). No API for rendering beyond page 6 — the limit is enforced
    /// at the interface, not just in policy.
    pub fn get_or_render_first_pages(
        &mut self,
        path: &Path,
    ) -> Result<&[DecodedPage], PdfProviderError> {
        let key = CacheKey::from_path(path)
            .map_err(|e| PdfProviderError::OpenFailed(e.to_string()))?;

        if self.cache.entries.contains_key(&key) {
            self.cache.touch(&key);
            return Ok(self.cache.entries.get(&key).unwrap());
        }

        let pages = Self::render_first_pages(path)?;
        self.cache.insert(key.clone(), pages);
        Ok(self.cache.entries.get(&key).unwrap())
    }

    fn render_first_pages(path: &Path) -> Result<Vec<DecodedPage>, PdfProviderError> {
        let path_str = path
            .to_str()
            .ok_or_else(|| PdfProviderError::OpenFailed("path contains non-UTF-8 characters".into()))?;

        let document = Document::open(path_str)
            .map_err(|e| PdfProviderError::OpenFailed(e.to_string()))?;

        let total_pages = document
            .page_count()
            .map_err(|e| PdfProviderError::OpenFailed(e.to_string()))?
            as usize;
        let pages_to_render = total_pages.min(MAX_PAGES);

        let matrix = Matrix::new_scale(RENDER_SCALE, RENDER_SCALE);
        let colorspace = Colorspace::device_rgb();

        let mut result = Vec::with_capacity(pages_to_render);
        for i in 0..pages_to_render {
            let page = document
                .load_page(i as i32)
                .map_err(|e| PdfProviderError::RenderFailed(e.to_string()))?;

            let pixmap = page
                .to_pixmap(&matrix, &colorspace, false, true)
                .map_err(|e| PdfProviderError::RenderFailed(e.to_string()))?;

            let width = pixmap.width();
            let height = pixmap.height();
            let samples = pixmap.samples();
            let n = pixmap.n();

            let rgba8 = if n == 4 {
                samples.to_vec()
            } else if n == 3 {
                let mut out = Vec::with_capacity(samples.len() / 3 * 4);
                for chunk in samples.chunks_exact(3) {
                    out.extend_from_slice(chunk);
                    out.push(255);
                }
                out
            } else {
                return Err(PdfProviderError::RenderFailed(format!(
                    "unexpected pixel component count: {n} (expected 3 or 4)"
                )));
            };

            result.push(DecodedPage {
                width,
                height,
                rgba8,
            });
        }

        Ok(result)
    }

    /// Number of pages actually rendered/cached, not the document's real
    /// total (which the UI never needs beyond page 6).
    pub fn available_page_count(&self, path: &Path) -> Option<usize> {
        let key = CacheKey::from_path(path).ok()?;
        self.cache.entries.get(&key).map(|pages| pages.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Integration tests that actually open/render a PDF via mupdf need a
    // real C/C++ toolchain and sample files; only pure-logic tests
    // (cache key, LRU eviction, pixel conversion) run here.

    #[test]
    fn cache_key_changes_when_mtime_changes() {
        let mut path = std::env::temp_dir();
        path.push("ql_test_pdf_cachekey_mupdf.txt");
        std::fs::write(&path, b"v1").unwrap();
        let key1 = CacheKey::from_path(&path).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&path, b"v2-longer-content").unwrap();
        let key2 = CacheKey::from_path(&path).unwrap();

        assert_ne!(key1, key2);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn lru_cache_evicts_oldest_when_over_capacity() {
        let mut cache = PdfCache::new();
        for i in 0..(MAX_CACHED_DOCUMENTS + 5) {
            let key = CacheKey {
                path: PathBuf::from(format!("fake_{i}.pdf")),
                mtime_nanos: i as u128,
                size: 100,
            };
            cache.insert(key, vec![]);
        }
        assert_eq!(cache.entries.len(), MAX_CACHED_DOCUMENTS);
        assert!(!cache.entries.contains_key(&CacheKey {
            path: PathBuf::from("fake_0.pdf"),
            mtime_nanos: 0,
            size: 100,
        }));
    }

    #[test]
    fn rgb_to_rgba_conversion_adds_opaque_alpha() {
        let rgb_samples: [u8; 6] = [255, 0, 0, 0, 255, 0];
        let mut out = Vec::with_capacity(8);
        for chunk in rgb_samples.chunks_exact(3) {
            out.extend_from_slice(chunk);
            out.push(255);
        }
        assert_eq!(out, vec![255, 0, 0, 255, 0, 255, 0, 255]);
    }
}
