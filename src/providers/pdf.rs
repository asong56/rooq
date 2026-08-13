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

    /// Cache-only lookup: never renders. Safe to call from the UI thread
    /// unconditionally — a miss returns None immediately instead of
    /// blocking on mupdf.
    pub fn cached_pages(&mut self, path: &Path) -> Option<&[DecodedPage]> {
        let key = CacheKey::from_path(path).ok()?;
        if self.cache.entries.contains_key(&key) {
            self.cache.touch(&key);
            self.cache.entries.get(&key).map(Vec::as_slice)
        } else {
            None
        }
    }

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

    /// Records a render that already happened elsewhere (the background
    /// loader thread calls `render_first_pages` directly, off the UI
    /// thread, since rendering blocks on file + CPU work). Call this on
    /// receipt so later opens of the same PDF still hit the cache.
    pub fn record_external_render(&mut self, path: &Path, pages: Vec<DecodedPage>) {
        if let Ok(key) = CacheKey::from_path(path) {
            self.cache.insert(key, pages);
        }
    }

    pub(crate) fn render_first_pages(path: &Path) -> Result<Vec<DecodedPage>, PdfProviderError> {
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
                crate::providers::pixels::rgb_to_rgba(samples)
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

    pub fn available_page_count(&self, path: &Path) -> Option<usize> {
        let key = CacheKey::from_path(path).ok()?;
        self.cache.entries.get(&key).map(|pages| pages.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
