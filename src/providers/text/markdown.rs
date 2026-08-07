//! Markdown rendering via `egui_commonmark`, a mature crate purpose-built
//! for CommonMark + GitHub extensions (tables, strikethrough, task lists,
//! footnotes) in egui.
//!
//! Known inconsistency: `egui_commonmark`'s optional code-block highlighting
//! uses syntect, not the tree-sitter/syntastica engine used elsewhere in
//! this project. That feature isn't enabled (see Cargo.toml), so it doesn't
//! currently matter, but it's worth knowing if it's turned on later.

use egui_commonmark::{CommonMarkCache, CommonMarkViewer};

/// Holds a `CommonMarkCache` across frames (it caches image handles and
/// other resources) rather than rebuilding it per frame, which would mean
/// re-parsing and re-loading everything every frame in this immediate-mode
/// GUI.
pub struct MarkdownProvider {
    cache: CommonMarkCache,
}

impl Default for MarkdownProvider {
    fn default() -> Self {
        Self {
            cache: CommonMarkCache::default(),
        }
    }
}

impl MarkdownProvider {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn render(&mut self, ui: &mut egui::Ui, content: &str) {
        CommonMarkViewer::new().show(ui, &mut self.cache, content);
    }
}

#[cfg(test)]
mod tests {
    // Markdown rendering needs a live egui::Ui context, so real rendering
    // is checked manually by running the app; this just confirms
    // construction doesn't panic.
    use super::*;

    #[test]
    fn provider_constructs_without_panicking() {
        let _provider = MarkdownProvider::new();
    }
}
