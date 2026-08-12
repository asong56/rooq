// tree-sitter (syntastica) over regex engines: real parsing, same approach editors like Neovim/Helix/Zed use.

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId};
use syntastica::renderer::resolve_styles;
use syntastica::style::Style as SynStyle;
use syntastica::{Processor, ThemedHighlights};
use syntastica_parsers::{Lang, LanguageSetImpl};

pub fn detect_language(path: &std::path::Path) -> Option<Lang> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => Lang::Rust,
        "py" | "pyw" => Lang::Python,
        "js" | "mjs" | "cjs" => Lang::Javascript,
        "ts" | "mts" | "cts" => Lang::Typescript,
        "tsx" => Lang::Tsx,
        "go" => Lang::Go,
        "c" | "h" => Lang::C,
        "cpp" | "cc" | "cxx" | "hpp" | "hh" => Lang::Cpp,
        "java" => Lang::Java,
        "json" => Lang::Json,
        "yaml" | "yml" => Lang::Yaml,
        "sh" | "bash" | "zsh" => Lang::Bash,
        "html" | "htm" => Lang::Html,
        "css" => Lang::Css,
        "lua" => Lang::Lua,
        _ => return None,
    })
}

pub struct HighlightedLine {
    pub spans: Vec<(String, Option<SpanStyle>)>,
}

#[derive(Clone, Copy)]
pub struct SpanStyle {
    pub color: Color32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
}

impl From<SynStyle> for SpanStyle {
    fn from(s: SynStyle) -> Self {
        let c = s.color();
        SpanStyle {
            color: Color32::from_rgb(c.red, c.green, c.blue),
            bold: s.bold(),
            italic: s.italic(),
            underline: s.underline(),
            strikethrough: s.strikethrough(),
        }
    }
}

pub fn highlight_source(
    source: &str,
    lang: Lang,
    theme: &syntastica::theme::ResolvedTheme,
) -> Result<Vec<HighlightedLine>, HighlightError> {
    let language_set = LanguageSetImpl::new();
    let mut processor = Processor::new(&language_set);
    let highlights: syntastica::Highlights = processor
        .process(source, lang)
        .map_err(|e| HighlightError::Parse(e.to_string()))?;

    let themed: ThemedHighlights = resolve_styles(&highlights, theme);

    let lines = themed
        .into_iter()
        .map(|line_spans| HighlightedLine {
            spans: line_spans
                .into_iter()
                .map(|(text, style)| (text.to_string(), style.map(SpanStyle::from)))
                .collect(),
        })
        .collect();

    Ok(lines)
}

#[derive(Debug, thiserror::Error)]
pub enum HighlightError {
    #[error("syntax parsing failed: {0}")]
    Parse(String),
}

pub fn build_layout_job(lines: &[HighlightedLine], base_font: FontId) -> LayoutJob {
    let mut job = LayoutJob::default();

    for line in lines {
        for (text, style) in &line.spans {
            let format = match style {
                Some(s) => {
                    let mut fmt = TextFormat {
                        font_id: base_font.clone(),
                        color: s.color,
                        underline: if s.underline {
                            egui::Stroke::new(1.0_f32, s.color)
                        } else {
                            egui::Stroke::NONE
                        },
                        strikethrough: if s.strikethrough {
                            egui::Stroke::new(1.0_f32, s.color)
                        } else {
                            egui::Stroke::NONE
                        },
                        ..Default::default()
                    };
                    if s.italic {
                        fmt.italics = true;
                    }
                    fmt
                }
                None => TextFormat {
                    font_id: base_font.clone(),
                    color: Color32::GRAY,
                    ..Default::default()
                },
            };
            job.append(text, 0.0, format);
        }
        job.append("\n", 0.0, TextFormat::default());
    }

    job
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn detects_rust_from_extension() {
        assert!(matches!(
            detect_language(Path::new("main.rs")),
            Some(Lang::Rust)
        ));
    }

    #[test]
    fn unknown_extension_returns_none() {
        assert!(detect_language(Path::new("file.zzz")).is_none());
    }

    #[test]
    fn highlights_simple_rust_snippet() {
        let theme = syntastica_themes::one::dark();
        let result = highlight_source("fn main() {}", Lang::Rust, &theme);
        assert!(result.is_ok());
        let lines = result.unwrap();
        assert_eq!(lines.len(), 1);
        assert!(!lines[0].spans.is_empty());
    }
}
