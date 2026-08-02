//! 主窗口 / eframe App 状态。
//!
//! 本次交付范围：把已经写好的四个 provider（图片InMemory、PDF、文本、Markdown）
//! 接到一个可以实际运行、看到东西的 egui 窗口里。onas 相关的部分
//! （webp/avif、mkv/webm）不在本次范围内——dispatcher 遇到
//! `FileCategory::RequiresOnas` 时，目前只显示一个"暂不支持，等待onas集成"
//! 的占位提示，不会崩溃，也不会假装能处理。

use crate::core::dispatcher::{self, FileCategory, ImageRoute, InMemoryImageKind, TextKind};
use crate::core::request_gen::RequestGenerator;
use crate::providers::image as image_provider;
use crate::providers::pdf::PdfProvider;
use crate::providers::text::{self, highlight, markdown::MarkdownProvider};
use eframe::egui;
use std::path::PathBuf;
use std::time::Instant;

/// 当前预览内容的展示状态。每次用户切换预览文件时重新构造。
enum PreviewState {
    Empty,
    Error(String),
    Image {
        texture: egui::TextureHandle,
        /// 动图播放状态；静态图为 None。
        anim: Option<AnimState>,
    },
    Pdf {
        /// 已渲染页面对应的纹理句柄，索引即页码（0-based）。
        page_textures: Vec<egui::TextureHandle>,
        current_page: usize,
    },
    PlainText {
        content: String,
    },
    CodeText {
        job: egui::text::LayoutJob,
    },
    Markdown {
        content: String,
    },
    /// onas 相关格式的占位提示，本次交付范围不实现具体解码。
    RequiresOnasPlaceholder {
        reason: &'static str,
    },
}

struct AnimState {
    frames: Vec<(egui::TextureHandle, std::time::Duration)>,
    current_frame: usize,
    last_switch: Instant,
}

pub struct QuickLookApp {
    request_gen: RequestGenerator,
    pdf_provider: PdfProvider,
    markdown_provider: MarkdownProvider,
    state: PreviewState,
    current_path: Option<PathBuf>,
    /// 供文本/代码高亮使用的默认主题；固定选择，
    /// 后续如需支持深浅色切换，需要把这个字段改成可变并接入
    /// 主题切换逻辑，当前范围内不做。
    code_theme: syntastica::theme::ResolvedTheme,
}

impl QuickLookApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        // 换成 mupdf 后不再有"引擎初始化可能因缺少动态库而失败"的问题——
        // MuPDF 通过 mupdf-sys 在编译期静态链接进了二进制，构造 PdfProvider
        // 本身不会失败，不需要再像 PDFium 方案那样处理 Option/降级路径。
        let pdf_provider = PdfProvider::new();

        Self {
            request_gen: RequestGenerator::new(),
            pdf_provider,
            markdown_provider: MarkdownProvider::new(),
            state: PreviewState::Empty,
            current_path: None,
            code_theme: syntastica_themes::one::dark(),
        }
    }

    /// 打开一个新文件进行预览。这是外部（比如命令行参数、或者未来的
    /// "文件管理器右键预览"集成）驱动预览器的主入口。
    pub fn open_path(&mut self, ctx: &egui::Context, path: PathBuf) {
        // 切换预览目标时代次前进，任何仍在处理中的旧请求
        // （目前范围内还没有真正的后台异步任务接到这里，
        // 但接口先留好，避免后续接入 onas_bridge 时还要回来改这一层）。
        let _token = self.request_gen.advance();

        let category = dispatcher::detect(&path);
        self.state = self.load_for_category(ctx, &path, category);
        self.current_path = Some(path);
    }

    fn load_for_category(
        &mut self,
        ctx: &egui::Context,
        path: &PathBuf,
        category: FileCategory,
    ) -> PreviewState {
        match category {
            FileCategory::Image(ImageRoute::InMemory(kind)) => self.load_image(ctx, path, kind),
            FileCategory::Pdf => self.load_pdf(ctx, path),
            FileCategory::Text(TextKind::Markdown) => self.load_markdown(path),
            FileCategory::Text(TextKind::PlainOrCode) => self.load_text(path),
            FileCategory::RequiresOnas(reason) => {
                let reason_str = match reason {
                    dispatcher::OnasReason::ImageWebpOrAvif => {
                        "webp/avif 需要 onas 解码，本次交付范围未接入"
                    }
                    dispatcher::OnasReason::VideoMkvOrWebm => {
                        "mkv/webm 需要 onas 解码，本次交付范围未接入"
                    }
                };
                PreviewState::RequiresOnasPlaceholder { reason: reason_str }
            }
            FileCategory::Unsupported => {
                PreviewState::Error("无法识别的文件类型".to_string())
            }
        }
    }

    fn load_image(
        &mut self,
        ctx: &egui::Context,
        path: &PathBuf,
        kind: InMemoryImageKind,
    ) -> PreviewState {
        match image_provider::decode(path, kind) {
            Ok(decoded) => {
                if decoded.is_animated() {
                    let frames = decoded
                        .frames
                        .into_iter()
                        .enumerate()
                        .map(|(i, f)| {
                            let color_image = egui::ColorImage::from_rgba_unmultiplied(
                                [f.width as usize, f.height as usize],
                                &f.rgba8,
                            );
                            let texture = ctx.load_texture(
                                format!("anim_frame_{i}"),
                                color_image,
                                egui::TextureOptions::LINEAR,
                            );
                            (texture, f.delay.unwrap_or(std::time::Duration::from_millis(100)))
                        })
                        .collect::<Vec<_>>();
                    // 用第一帧的纹理句柄作为 PreviewState::Image 的主纹理占位，
                    // 实际渲染时 UI 层会优先检查 anim 字段并播放帧序列。
                    let first_texture = frames[0].0.clone();
                    PreviewState::Image {
                        texture: first_texture,
                        anim: Some(AnimState {
                            frames,
                            current_frame: 0,
                            last_switch: Instant::now(),
                        }),
                    }
                } else {
                    let f = &decoded.frames[0];
                    let color_image = egui::ColorImage::from_rgba_unmultiplied(
                        [f.width as usize, f.height as usize],
                        &f.rgba8,
                    );
                    let texture = ctx.load_texture(
                        "static_image",
                        color_image,
                        egui::TextureOptions::LINEAR,
                    );
                    PreviewState::Image {
                        texture,
                        anim: None,
                    }
                }
            }
            Err(e) => PreviewState::Error(format!("图片解码失败: {e}")),
        }
    }

    fn load_pdf(&mut self, ctx: &egui::Context, path: &PathBuf) -> PreviewState {
        match self.pdf_provider.get_or_render_first_pages(path) {
            Ok(pages) => {
                let page_textures = pages
                    .iter()
                    .enumerate()
                    .map(|(i, p)| {
                        let color_image = egui::ColorImage::from_rgba_unmultiplied(
                            [p.width as usize, p.height as usize],
                            &p.rgba8,
                        );
                        ctx.load_texture(
                            format!("pdf_page_{i}"),
                            color_image,
                            egui::TextureOptions::LINEAR,
                        )
                    })
                    .collect();
                PreviewState::Pdf {
                    page_textures,
                    current_page: 0,
                }
            }
            Err(e) => PreviewState::Error(format!("PDF 渲染失败: {e}")),
        }
    }

    fn load_markdown(&mut self, path: &PathBuf) -> PreviewState {
        match text::read_as_text(path) {
            Ok(content) => PreviewState::Markdown { content },
            Err(e) => PreviewState::Error(format!("文件读取失败: {e}")),
        }
    }

    fn load_text(&mut self, path: &PathBuf) -> PreviewState {
        let content = match text::read_as_text(path) {
            Ok(c) => c,
            Err(e) => return PreviewState::Error(format!("文件读取失败: {e}")),
        };

        // 本次交付先走"同步、全文高亮"的简单路径，把 mod.rs 里设计的
        // "后台线程 + 视口裁剪"异步管线的接线工作留到接入真实大文件场景时再做——
        // 当前范围的重点是把四个 provider 的核心解码/渲染逻辑跑通，
        // 异步调度是在此基础上的性能优化层，两者可以独立验证。
        match highlight::detect_language(path) {
            Some(lang) => match highlight::highlight_source(&content, lang, &self.code_theme) {
                Ok(lines) => {
                    let job = highlight::build_layout_job(
                        &lines,
                        egui::FontId::monospace(14.0),
                    );
                    PreviewState::CodeText { job }
                }
                Err(_) => PreviewState::PlainText { content },
            },
            None => PreviewState::PlainText { content },
        }
    }
}

impl eframe::App for QuickLookApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 动图播放的帧推进逻辑：检查当前帧是否已经显示够了 delay 时长，
        // 是则切到下一帧并请求重绘。这里没有走独立的定时器线程，
        // 而是利用 egui 的 `ctx.request_repaint_after` 机制，
        // 是 immediate-mode GUI 里处理"周期性动画"的标准做法。
        if let PreviewState::Image {
            anim: Some(anim), ..
        } = &mut self.state
        {
            let elapsed = anim.last_switch.elapsed();
            let current_delay = anim.frames[anim.current_frame].1;
            if elapsed >= current_delay {
                anim.current_frame = (anim.current_frame + 1) % anim.frames.len();
                anim.last_switch = Instant::now();
            }
            ctx.request_repaint_after(current_delay.saturating_sub(elapsed));
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            self.draw_preview(ui);
        });
    }
}

impl QuickLookApp {
    fn draw_preview(&mut self, ui: &mut egui::Ui) {
        match &mut self.state {
            PreviewState::Empty => {
                ui.centered_and_justified(|ui| {
                    ui.label("拖入或选择一个文件以预览");
                });
            }
            PreviewState::Error(msg) => {
                ui.centered_and_justified(|ui| {
                    ui.colored_label(egui::Color32::from_rgb(220, 80, 80), msg.as_str());
                });
            }
            PreviewState::RequiresOnasPlaceholder { reason } => {
                ui.centered_and_justified(|ui| {
                    ui.label(format!("暂不支持: {reason}"));
                });
            }
            PreviewState::Image { texture, anim } => {
                egui::ScrollArea::both().show(ui, |ui| {
                    let display_texture = match anim {
                        Some(a) => &a.frames[a.current_frame].0,
                        None => &*texture,
                    };
                    let available = ui.available_size();
                    let img_size = display_texture.size_vec2();
                    let scale = (available.x / img_size.x)
                        .min(available.y / img_size.y)
                        .min(1.0); // 不放大超过原始尺寸，只做缩小以适应窗口
                    ui.image((display_texture.id(), img_size * scale));
                });
            }
            PreviewState::Pdf {
                page_textures,
                current_page,
            } => {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        let at_first = *current_page == 0;
                        let at_last = *current_page + 1 >= page_textures.len();
                        if ui.add_enabled(!at_first, egui::Button::new("上一页")).clicked() {
                            *current_page = current_page.saturating_sub(1);
                        }
                        ui.label(format!(
                            "第 {} / {} 页（预览仅支持前 6 页）",
                            *current_page + 1,
                            page_textures.len()
                        ));
                        if ui.add_enabled(!at_last, egui::Button::new("下一页")).clicked() {
                            *current_page = (*current_page + 1).min(page_textures.len() - 1);
                        }
                    });
                    egui::ScrollArea::both().show(ui, |ui| {
                        if let Some(texture) = page_textures.get(*current_page) {
                            let available_width = ui.available_width();
                            let img_size = texture.size_vec2();
                            let scale = (available_width / img_size.x).min(1.0);
                            ui.image((texture.id(), img_size * scale));
                        }
                    });
                });
            }
            PreviewState::PlainText { content } => {
                // 用只读 Label 而非 TextEdit：TextEdit 是为"可编辑"场景设计的，
                // 需要 &mut String 作为落脚点，即使 .interactive(false) 也要求
                // 一个可写的缓冲区，会诱导写出"每帧clone一次内容"这种反模式
                // （对大文件是明显的性能浪费）。纯展示场景用 Label 更直接、
                // 也不需要每帧复制文本。
                egui::ScrollArea::both().show(ui, |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(content.as_str())
                                .font(egui::FontId::monospace(14.0)),
                        )
                        .wrap(),
                    );
                });
            }
            PreviewState::CodeText { job } => {
                egui::ScrollArea::both().show(ui, |ui| {
                    ui.label(job.clone());
                });
            }
            PreviewState::Markdown { content } => {
                egui::ScrollArea::both().show(ui, |ui| {
                    self.markdown_provider.render(ui, content);
                });
            }
        }
    }
}
