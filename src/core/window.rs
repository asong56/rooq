use crate::core::dispatcher::{self, FileCategory, ImageRoute, InMemoryImageKind, TextKind};
use crate::core::request_gen::RequestGenerator;
use crate::providers::image as image_provider;
use crate::providers::onas_bridge;
use crate::providers::pdf::PdfProvider;
use crate::providers::text::{self, highlight, markdown::MarkdownProvider};
use eframe::egui;
use std::path::PathBuf;
use std::time::Instant;

enum PreviewState {
    Empty,
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

/// Registers a system CJK font as a fallback so Chinese/Japanese/Korean text
/// doesn't render as tofu boxes: egui's default fonts have no CJK glyphs and
/// egui has no built-in fallback-to-system-font behavior (upstream issue
/// #5233). Appended (not prepended) to the fallback chain so Latin glyphs
/// still come from egui's default fonts. Reads from disk at startup rather
/// than bundling a font, since a CJK font is several MB and the target
/// (Windows) almost always has one installed already.
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

pub struct RooqApp {
    request_gen: RequestGenerator,
    pdf_provider: PdfProvider,
    markdown_provider: MarkdownProvider,
    state: PreviewState,
    current_path: Option<PathBuf>,
    code_theme: syntastica::theme::ResolvedTheme,
}

impl RooqApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        load_cjk_fallback_font(&cc.egui_ctx);

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

    pub fn open_path(&mut self, ctx: &egui::Context, path: PathBuf) {
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
            FileCategory::RequiresOnas(dispatcher::OnasReason::ImageWebpOrAvif) => {
                self.load_onas_image(ctx, path)
            }
            FileCategory::RequiresOnas(dispatcher::OnasReason::VideoMkvOrWebm) => {
                self.load_onas_video_frame(ctx, path)
            }
            FileCategory::Unsupported => PreviewState::Error("Unrecognized file type".to_string()),
        }
    }

    fn load_image(
        &mut self,
        ctx: &egui::Context,
        path: &PathBuf,
        kind: InMemoryImageKind,
    ) -> PreviewState {
        match image_provider::decode(path, kind) {
            Ok(decoded) => Self::decoded_image_to_preview_state(ctx, decoded),
            Err(e) => PreviewState::Error(format!("Image decode failed: {e}")),
        }
    }

    /// webp/avif: onas converts to a temporary PNG, which is then decoded by
    /// the same zune-png path used for local PNGs. `temp_png` is an RAII
    /// guard that deletes the temp file when dropped.
    fn load_onas_image(&mut self, ctx: &egui::Context, path: &PathBuf) -> PreviewState {
        let temp_png = match onas_bridge::convert_image_to_png(path) {
            Ok(guard) => guard,
            Err(e) => return PreviewState::Error(format!("onas conversion failed: {e}")),
        };

        match image_provider::decode(temp_png.path(), InMemoryImageKind::Png) {
            Ok(decoded) => Self::decoded_image_to_preview_state(ctx, decoded),
            Err(e) => PreviewState::Error(format!("onas succeeded but reading its output failed: {e}")),
        }
    }

    /// mkv/webm: onas extracts one frame to a temporary PNG (`onas frame`),
    /// decoded via the same path as `load_onas_image`.
    fn load_onas_video_frame(&mut self, ctx: &egui::Context, path: &PathBuf) -> PreviewState {
        let temp_png = match onas_bridge::extract_video_frame(path) {
            Ok(guard) => guard,
            Err(e) => return PreviewState::Error(format!("onas frame extraction failed: {e}")),
        };

        match image_provider::decode(temp_png.path(), InMemoryImageKind::Png) {
            Ok(decoded) => Self::decoded_image_to_preview_state(ctx, decoded),
            Err(e) => PreviewState::Error(format!("onas succeeded but reading its output failed: {e}")),
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
            let texture = ctx.load_texture("static_image", color_image, egui::TextureOptions::LINEAR);
            PreviewState::Image {
                texture,
                anim: None,
            }
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
            Err(e) => PreviewState::Error(format!("PDF render failed: {e}")),
        }
    }

    fn load_markdown(&mut self, path: &PathBuf) -> PreviewState {
        match text::read_as_text(path) {
            Ok(content) => PreviewState::Markdown { content },
            Err(e) => PreviewState::Error(format!("File read failed: {e}")),
        }
    }

    fn load_text(&mut self, path: &PathBuf) -> PreviewState {
        let content = match text::read_as_text(path) {
            Ok(c) => c,
            Err(e) => return PreviewState::Error(format!("File read failed: {e}")),
        };

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

impl eframe::App for RooqApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
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

impl RooqApp {
    fn draw_preview(&mut self, ui: &mut egui::Ui) {
        match &mut self.state {
            PreviewState::Empty => {
                ui.centered_and_justified(|ui| {
                    ui.label("Drop or open a file to preview");
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
                    let img_size = display_texture.size_vec2();
                    let scale = (available.x / img_size.x)
                        .min(available.y / img_size.y)
                        .min(1.0);
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
                            let available_width = ui.available_width();
                            let img_size = texture.size_vec2();
                            let scale = (available_width / img_size.x).min(1.0);
                            ui.image((texture.id(), img_size * scale));
                        }
                    });
                });
            }
            PreviewState::PlainText { content } => {
                // Read-only Label, not TextEdit: TextEdit requires a &mut
                // String even when non-interactive, which invites cloning
                // the whole buffer every frame.
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
