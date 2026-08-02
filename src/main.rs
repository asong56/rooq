//! quicklook_rs 程序入口。
//!
//! 本次交付范围：图片(jpg/png/gif，InMemory路径) + PDF(前6页) +
//! 文本/代码(tree-sitter高亮) + Markdown。
//! webp/avif/mkv/webm 需要 onas，本次不实现，dispatcher 会分流到
//! 一个"暂不支持"的占位提示，不会崩溃。
//!
//! 用法：`quicklook_rs <文件路径>`，不传参数则显示空白等待界面
//! （对应"以后接入文件管理器右键预览调用"这个使用场景的最小占位）。

mod core;
mod providers;

use core::window::QuickLookApp;
use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let initial_path = args.get(1).map(PathBuf::from);

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 700.0])
            .with_min_inner_size([300.0, 200.0]),
        ..Default::default()
    };

    eframe::run_native(
        "QuickLook (Rust)",
        native_options,
        Box::new(move |cc| {
            let mut app = QuickLookApp::new(cc);
            if let Some(path) = initial_path {
                if path.exists() {
                    app.open_path(&cc.egui_ctx, path);
                } else {
                    eprintln!("警告：命令行传入的路径不存在: {}", path.display());
                }
            }
            Ok(Box::new(app))
        }),
    )
}
