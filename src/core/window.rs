use crate::core::dispatcher::{self, FileCategory, ImageRoute, InMemoryImageKind, TextKind};
use crate::core::request_gen::{RequestGenerator, RequestToken};
use crate::providers::ffmpeg_bridge;
use crate::providers::image as image_provider;
use crate::providers::onas_bridge;
use crate::providers::pdf::{DecodedPage, PdfProvider, PdfProviderError};
use crate::providers::text::{self, highlight, markdown::MarkdownProvider};
use eframe::egui;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Instant;

enum PreviewState {
    Empty,
    Loading,
    Error(String),
    Image {
        texture: egui::TextureHandle,
        anim: Option<AnimState>,
    },
    Pdf {
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
}

struct AnimState {
    frames: Vec<(egui::TextureHandle, std::time::Duration)>,
    current_frame: usize,
    last_switch: Instant,
}

/// Raw decode/read output produced on a background thread. No GPU textures
/// are built here — that must happen on the UI thread that owns the paint
/// context — so this is the payload that crosses the channel back to
/// `update()`. Carries the source path alongside PDF pages so the result
/// can be recorded into `PdfProvider`'s cache on receipt.
enum LoadResult {
    Image(image_provider::DecodedImage),
    Pdf(PathBuf, Vec<DecodedPage>),
    PlainText(String),
    CodeText {
        content: String,
        lines: Vec<highlight::HighlightedLine>,
    },
    Markdown(String),
    Error(String),
}

/// egui has no fallback-to-system-font behavior (upstream issue #5233), so a CJK font is loaded from disk and appended after egui's defaults.
fn load_cjk_fallback_font(ctx: &egui::Context) {
    const CANDIDATE_PATHS: &[&str] = &[
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\simsun.ttc",
    ];

    for path in CANDIDATE_PATHS {
        match std::fs::read(path) {
            Ok(bytes) => {
                let mut fonts = egui::FontDefinitions::default();
                fonts.font_data.insert(
                    "cjk_fallback".to_owned(),
                    std::sync::Arc::new(egui::FontData::from_owned(bytes)),
                );
                fonts
                    .families
                    .entry(egui::FontFamily::Proportional)
                    .or_default()
                    .push("cjk_fallback".to_owned());
                fonts
                    .families
                    .entry(egui::FontFamily::Monospace)
                    .or_default()
                    .push("cjk_fallback".to_owned());
                ctx.set_fonts(fonts);
                return;
            }
            Err(_) => continue,
        }
    }

    eprintln!(
        "warning: no system CJK font found (tried msyh.ttc, simsun.ttc); \
         CJK text may render as tofu boxes."
    );
}

fn load_rgba_texture(
    ctx: &egui::Context,
    name: impl Into<String>,
    width: u32,
    height: u32,
    rgba8: &[u8],
) -> egui::TextureHandle {
    let color_image =
        egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], rgba8);
    ctx.load_texture(name, color_image, egui::TextureOptions::LINEAR)
}

pub struct RooqApp {
    request_gen: RequestGenerator,
    pdf_provider: PdfProvider,
    markdown_provider: MarkdownProvider,
    state: PreviewState,
    current_path: Option<PathBuf>,
    code_theme: Arc<syntastica::theme::ResolvedTheme>,
    close_requested: Option<Arc<AtomicBool>>,
    pending: Option<Receiver<(RequestToken, LoadResult)>>,
}

impl RooqApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self::with_close_signal(cc, None)
    }

    pub fn with_close_signal(
        cc: &eframe::CreationContext<'_>,
        close_requested: Option<Arc<AtomicBool>>,
    ) -> Self {
        load_cjk_fallback_font(&cc.egui_ctx);

        Self {
            request_gen: RequestGenerator::new(),
            pdf_provider: PdfProvider::new(),
            markdown_provider: MarkdownProvider::new(),
            state: PreviewState::Empty,
            current_path: None,
            code_theme: Arc::new(syntastica_themes::one::dark()),
            close_requested,
            pending: None,
        }
    }

    /// Kicks off loading on a background thread and returns immediately —
    /// the result is picked up by `poll_pending` during the normal eframe
    /// update loop. File I/O, subprocess calls (ffmpeg/onas), and PDF
    /// rendering used to run synchronously on the UI thread here, freezing
    /// the window for as long as they took (up to the ffmpeg/onas
    /// timeouts, or however long mupdf took on a large PDF). Only cheap
    /// GPU texture upload now happens on the UI thread, in
    /// `apply_load_result`.
    pub fn open_path(&mut self, ctx: &egui::Context, path: PathBuf) {
        // Fast path: an already-cached PDF render can be applied
        // immediately without a round trip through a background thread.
        // cached_pages() never renders, so this never blocks — a cache miss
        // falls through to the normal async path below.
        if matches!(dispatcher::detect(&path), FileCategory::Pdf) {
            if let Some(pages) = self.pdf_provider.cached_pages(&path) {
                let _token = self.request_gen.advance();
                self.state = Self::pdf_pages_to_preview_state(ctx, pages);
                self.current_path = Some(path);
                self.pending = None;
                return;
            }
        }

        let token = self.request_gen.advance();
        self.current_path = Some(path.clone());
        self.state = PreviewState::Loading;

        let (tx, rx) = std::sync::mpsc::channel();
        self.pending = Some(rx);

        let ctx = ctx.clone();
        let code_theme = Arc::clone(&self.code_theme);
        std::thread::spawn(move || {
            let result = Self::load_for_category(&path, &code_theme);
            // Wake the UI thread even if it's idle — otherwise the result
            // sits in the channel until the next unrelated repaint.
            let _ = tx.send((token, result));
            ctx.request_repaint();
        });
    }

    /// Drains at most one finished background load per frame and applies it
    /// (building GPU textures, which must happen on this thread) if it's
    /// still the most recent request. Stale results — from a file the user
    /// already navigated away from — are silently dropped.
    fn poll_pending(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.pending else { return };
        let Ok((token, result)) = rx.try_recv() else {
            return;
        };
        self.pending = None;

        if !token.is_still_current() {
            return;
        }

        self.state = self.apply_load_result(ctx, result);
    }

    fn load_for_category(
        path: &PathBuf,
        code_theme: &syntastica::theme::ResolvedTheme,
    ) -> LoadResult {
        let category = dispatcher::detect(path);
        match category {
            FileCategory::Image(ImageRoute::InMemory(kind)) => Self::load_image(path, kind),
            FileCategory::Pdf => Self::load_pdf(path),
            FileCategory::Text(TextKind::Markdown) => Self::load_markdown(path),
            FileCategory::Text(TextKind::PlainOrCode) => Self::load_text(path, code_theme),
            FileCategory::RequiresOnas(dispatcher::OnasReason::ImageWebpOrAvif) => {
                Self::load_onas_image(path)
            }
            FileCategory::RequiresFfmpeg(dispatcher::FfmpegReason::VideoMkvOrWebm) => {
                Self::load_ffmpeg_video_frame(path)
            }
            FileCategory::Unsupported => {
                LoadResult::Error("Unrecognized file type".to_string())
            }
        }
    }

    fn load_image(path: &PathBuf, kind: InMemoryImageKind) -> LoadResult {
        match image_provider::decode(path, kind) {
            Ok(decoded) => LoadResult::Image(decoded),
            Err(e) => LoadResult::Error(format!("Image decode failed: {e}")),
        }
    }

    fn load_onas_image(path: &PathBuf) -> LoadResult {
        Self::load_via_temp_png(onas_bridge::convert_image_to_png(path), "onas", "conversion")
    }

    fn load_ffmpeg_video_frame(path: &PathBuf) -> LoadResult {
        Self::load_via_temp_png(
            ffmpeg_bridge::extract_video_frame(path),
            "ffmpeg",
            "frame extraction",
        )
    }

    fn load_via_temp_png(
        result: Result<impl AsRef<std::path::Path>, impl std::fmt::Display>,
        tool: &str,
        action: &str,
    ) -> LoadResult {
        let temp_png = match result {
            Ok(guard) => guard,
            Err(e) => return LoadResult::Error(format!("{tool} {action} failed: {e}")),
        };

        match image_provider::decode(temp_png.as_ref(), InMemoryImageKind::Png) {
            Ok(decoded) => LoadResult::Image(decoded),
            Err(e) => {
                LoadResult::Error(format!("{tool} succeeded but reading its output failed: {e}"))
            }
        }
    }

    fn load_pdf(path: &PathBuf) -> LoadResult {
        match PdfProvider::render_first_pages(path) {
            Ok(pages) => LoadResult::Pdf(path.clone(), pages),
            Err(PdfProviderError::OpenFailed(e)) => {
                LoadResult::Error(format!("PDF render failed: {e}"))
            }
            Err(PdfProviderError::RenderFailed(e)) => {
                LoadResult::Error(format!("PDF render failed: {e}"))
            }
        }
    }

    fn load_markdown(path: &PathBuf) -> LoadResult {
        match text::read_as_text(path) {
            Ok(content) => LoadResult::Markdown(content),
            Err(e) => LoadResult::Error(format!("File read failed: {e}")),
        }
    }

    fn load_text(path: &PathBuf, code_theme: &syntastica::theme::ResolvedTheme) -> LoadResult {
        let content = match text::read_as_text(path) {
            Ok(c) => c,
            Err(e) => return LoadResult::Error(format!("File read failed: {e}")),
        };

        match highlight::detect_language(path) {
            Some(lang) => match highlight::highlight_source(&content, lang, code_theme) {
                Ok(lines) => LoadResult::CodeText { content, lines },
                Err(_) => LoadResult::PlainText(content),
            },
            None => LoadResult::PlainText(content),
        }
    }

    fn apply_load_result(&mut self, ctx: &egui::Context, result: LoadResult) -> PreviewState {
        match result {
            LoadResult::Error(msg) => PreviewState::Error(msg),
            LoadResult::Image(decoded) => Self::decoded_image_to_preview_state(ctx, decoded),
            LoadResult::Pdf(path, pages) => {
                self.pdf_provider.record_external_render(&path, pages);
                match self.pdf_provider.get_or_render_first_pages(&path) {
                    Ok(pages) => Self::pdf_pages_to_preview_state(ctx, pages),
                    Err(e) => PreviewState::Error(format!("PDF render failed: {e}")),
                }
            }
            LoadResult::PlainText(content) => PreviewState::PlainText { content },
            LoadResult::CodeText { lines, .. } => {
                let job = highlight::build_layout_job(&lines, egui::FontId::monospace(14.0));
                PreviewState::CodeText { job }
            }
            LoadResult::Markdown(content) => PreviewState::Markdown { content },
        }
    }

    fn pdf_pages_to_preview_state(ctx: &egui::Context, pages: &[DecodedPage]) -> PreviewState {
        let page_textures = pages
            .iter()
            .enumerate()
            .map(|(i, p)| load_rgba_texture(ctx, format!("pdf_page_{i}"), p.width, p.height, &p.rgba8))
            .collect();
        PreviewState::Pdf {
            page_textures,
            current_page: 0,
        }
    }

    fn decoded_image_to_preview_state(
        ctx: &egui::Context,
        decoded: image_provider::DecodedImage,
    ) -> PreviewState {
        if decoded.is_animated() {
            let frames = decoded
                .frames
                .into_iter()
                .enumerate()
                .map(|(i, f)| {
                    let texture =
                        load_rgba_texture(ctx, format!("anim_frame_{i}"), f.width, f.height, &f.rgba8);
                    (texture, f.delay.unwrap_or(std::time::Duration::from_millis(100)))
                })
                .collect::<Vec<_>>();
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
            let texture = load_rgba_texture(ctx, "static_image", f.width, f.height, &f.rgba8);
            PreviewState::Image {
                texture,
                anim: None,
            }
        }
    }
}

impl eframe::App for RooqApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(flag) = &self.close_requested {
            if flag.load(Ordering::SeqCst) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
        }

        self.poll_pending(ctx);

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

        if self.close_requested.is_some() || self.pending.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
}

fn scale_to_fit(natural: egui::Vec2, max: egui::Vec2) -> f32 {
    (max.x / natural.x).min(max.y / natural.y).min(1.0)
}

fn draw_scaled_image(ui: &mut egui::Ui, texture: &egui::TextureHandle, max: egui::Vec2) {
    let natural = texture.size_vec2();
    let scale = scale_to_fit(natural, max);
    ui.image((texture.id(), natural * scale));
}

impl RooqApp {
    fn draw_preview(&mut self, ui: &mut egui::Ui) {
        match &mut self.state {
            PreviewState::Empty => {
                ui.centered_and_justified(|ui| {
                    ui.label("Drop or open a file to preview");
                });
            }
            PreviewState::Loading => {
                ui.centered_and_justified(|ui| {
                    ui.spinner();
                });
            }
            PreviewState::Error(msg) => {
                ui.centered_and_justified(|ui| {
                    ui.colored_label(egui::Color32::from_rgb(220, 80, 80), msg.as_str());
                });
            }
            PreviewState::Image { texture, anim } => {
                egui::ScrollArea::both().show(ui, |ui| {
                    let display_texture = match anim {
                        Some(a) => &a.frames[a.current_frame].0,
                        None => &*texture,
                    };
                    let available = ui.available_size();
                    draw_scaled_image(ui, display_texture, available);
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
                        if ui.add_enabled(!at_first, egui::Button::new("Previous")).clicked() {
                            *current_page = current_page.saturating_sub(1);
                        }
                        ui.label(format!(
                            "Page {} / {} (preview limited to first 6)",
                            *current_page + 1,
                            page_textures.len()
                        ));
                        if ui.add_enabled(!at_last, egui::Button::new("Next")).clicked() {
                            *current_page = (*current_page + 1).min(page_textures.len() - 1);
                        }
                    });
                    egui::ScrollArea::both().show(ui, |ui| {
                        if let Some(texture) = page_textures.get(*current_page) {
                            let max = egui::vec2(ui.available_width(), f32::INFINITY);
                            draw_scaled_image(ui, texture, max);
                        }
                    });
                });
            }
            PreviewState::PlainText { content } => {
                // Label, not TextEdit: TextEdit needs a &mut String even read-only, cloning the whole buffer every frame.
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
