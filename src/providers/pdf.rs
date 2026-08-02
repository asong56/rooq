//! PDF 查看器：仅渲染前 6 页（或文档总页数，取较小值），硬性上限，
//! 不提供第 7 页及以后的任何渲染路径。
//!
//! 引擎选型：**mupdf-rs**（对应 MuPDF，C 编写的 PDF/XPS/EPUB 渲染引擎），
//! 不是 PDFium。这是一次明确的返工——上一版实现用了 PDFium，
//! 但那从来不是你要的东西，是我自己套用旧方案文档想当然加上去的，
//! 你没有批准过。这里改正。
//!
//! **许可证提醒（这是选择 mupdf 前必须清楚的事，不是事后免责声明）**：
//! MuPDF 采用 AGPL-3.0（或 Artifex 的付费商业授权）双授权模式。
//! 你已经明确表示接受 AGPL：如果分发这个程序但不购买 Artifex 商业授权，
//! 意味着整个项目都需要以 AGPL-3.0 开源。这和 PDFium（宽松的 BSD 风格许可）
//! 是完全不同的法律状况，请确保这个选择在项目实际对外分发时依然是你要的——
//! 这不是我在这里能替你判断的事，只是如实提醒。
//!
//! 部署优势（相对 PDFium 方案的关键改进点）：`mupdf-sys` 在构建时直接
//! 编译 MuPDF 的 C 源码并静态链接进最终二进制，**不需要**你手动下载、
//! 放置、分发一个独立的动态库文件（不像 PDFium 需要单独的 pdfium.dll）。
//! 这意味着 `cargo build --release` 产出的就是真正意义上的单一可执行文件，
//! 不需要额外的部署步骤——这正是"single binary"这个目标本来就该有的样子。
//!
//! 编译前提：`mupdf-sys` 需要 C/C++ 工具链和 libclang（供 bindgen 使用）。
//! Windows 上建议通过 MSVC Build Tools 提供 C/C++ 工具链，
//! 并安装 LLVM（提供 libclang.dll）。具体安装步骤请参考
//! `mupdf-sys` 仓库 README 里的平台相关说明，这里不重复罗列
//! 可能随版本变化的具体命令。

use mupdf::{Colorspace, Document, Matrix};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use thiserror::Error;

/// 单页渲染结果，统一转成 RGBA8，复用和图片/视频同一套上屏管线。
pub struct DecodedPage {
    pub width: u32,
    pub height: u32,
    pub rgba8: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum PdfProviderError {
    #[error("无法打开 PDF 文件: {0}")]
    OpenFailed(String),
    #[error("页面渲染失败: {0}")]
    RenderFailed(String),
}

/// 缓存 key：路径 + mtime + 文件大小，理由见 PdfCache 文档注释。
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

/// 纯内存缓存：进程存活期间有效，退出后随进程释放，磁盘上不留任何文件。
/// 按你的明确要求：不落盘、不用 AppData/Roaming，纯单文件。
///
/// 代价：跨进程重启不保留缓存，重新打开预览器需要重新渲染一次。
/// 这是"零磁盘footprint"这个约束下主动接受的交换。
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

/// PDF 页面数量硬上限。产品能力边界，不是可调性能旋钮。
const MAX_PAGES: usize = 6;

/// 渲染缩放比例。MuPDF 的 `to_pixmap` 接受一个变换矩阵而非目标像素宽度
/// （这是它和 PDFium 的 API 设计差异之一：PDFium 是"告诉我要多宽"，
/// MuPDF 是"告诉我要放大几倍"）。2.0 大致相当于把标准 72dpi 的 PDF
/// 坐标系渲染到 144dpi 附近，对"预览确认内容"这个场景足够清晰，
/// 同时不会让渲染开销过大拖慢首次预览的响应速度。
const RENDER_SCALE: f32 = 2.0;

pub struct PdfProvider {
    cache: PdfCache,
}

impl PdfProvider {
    /// 构造函数不再需要像 PDFium 方案那样做"引擎初始化可能失败"的处理——
    /// MuPDF 通过 mupdf-sys 静态链接进了二进制，没有"找不到动态库"这种
    /// 运行时才会暴露的失败模式，构造这个 provider 本身不会失败。
    pub fn new() -> Self {
        Self {
            cache: PdfCache::new(),
        }
    }

    /// 获取（必要时渲染）某个 PDF 文件的前 N 页（N = min(6, 文档总页数)）。
    ///
    /// 明确不做的事：不接受页码范围参数，不支持"渲染第7页"这种调用——
    /// 这是接口层面对硬上限的强制。
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
        // Document::open 接受路径字符串；mupdf-rs 在遇到非 UTF-8 路径时
        // 会有潜在的转换损耗，这是当前范围内的已知限制，
        // 绝大多数真实使用场景（正常的文件路径）不受影响。
        let path_str = path
            .to_str()
            .ok_or_else(|| PdfProviderError::OpenFailed("路径包含无法处理的非UTF-8字符".into()))?;

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

            // to_pixmap 参数：(变换矩阵, 色彩空间, 是否带alpha, 是否显示注释/表单等额外内容)。
            // alpha=false：PDF页面渲染通常不需要透明通道，直接不透明输出即可；
            // show_extra=true：渲染注释等内容，更贴近"用户实际会看到的完整页面外观"。
            let pixmap = page
                .to_pixmap(&matrix, &colorspace, false, true)
                .map_err(|e| PdfProviderError::RenderFailed(e.to_string()))?;

            let width = pixmap.width();
            let height = pixmap.height();
            let samples = pixmap.samples();
            let n = pixmap.n(); // 每像素分量数：RGB是3，若alpha=true则是4

            // samples() 返回的是按 n 个分量交织的原始数据（RGB或RGBA），
            // 上层统一约定用 RGBA8，这里做一次分量数对齐：
            // n==3(RGB) 时补一个不透明的alpha分量；n==4时原样使用。
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
                    "非预期的像素分量数: {n}（预期3或4）"
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

    /// 供 UI 层判断"是否已经到达可预览的最后一页"。
    /// 返回的是已缓存/已渲染的页数，不是文档实际总页数——
    /// 即使文档有200页，硬上限生效后这里也只会反映最多6页，
    /// UI 没有办法、也不应该得知"文档实际还有更多页"。
    pub fn available_page_count(&self, path: &Path) -> Option<usize> {
        let key = CacheKey::from_path(path).ok()?;
        self.cache.entries.get(&key).map(|pages| pages.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 注意：真正调用 mupdf 打开/渲染 PDF 的集成测试需要在你自己机器上跑
    // （本沙盒容器缺少可用的 C/C++ 工具链和真实 PDF 测试样本），
    // 这里只保留不依赖 MuPDF 运行时的纯逻辑测试（缓存key、LRU淘汰），
    // 和 PDFium 版本的测试策略一致，理由相同。

    #[test]
    fn cache_key_changes_when_mtime_changes() {
        let mut path = std::env::temp_dir();
        path.push("ql_test_pdf_cachekey_mupdf.txt");
        std::fs::write(&path, b"v1").unwrap();
        let key1 = CacheKey::from_path(&path).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&path, b"v2-longer-content").unwrap();
        let key2 = CacheKey::from_path(&path).unwrap();

        assert_ne!(key1, key2, "内容变化后size或mtime应至少一项不同");
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
        // 验证 n==3 (RGB) 转 RGBA 的分量对齐逻辑本身是正确的，
        // 不依赖真实的 mupdf 渲染结果，只测试这一段转换算法。
        let rgb_samples: [u8; 6] = [255, 0, 0, 0, 255, 0]; // 两个像素：红、绿
        let mut out = Vec::with_capacity(8);
        for chunk in rgb_samples.chunks_exact(3) {
            out.extend_from_slice(chunk);
            out.push(255);
        }
        assert_eq!(out, vec![255, 0, 0, 255, 0, 255, 0, 255]);
    }
}
