use egui_commonmark::{CommonMarkCache, CommonMarkViewer};

// Cache kept across frames: this is immediate-mode, so rebuilding it per frame would re-parse everything.
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
    use super::*;

    #[test]
    fn provider_constructs_without_panicking() {
        let _provider = MarkdownProvider::new();
    }
}
