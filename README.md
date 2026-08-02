# quicklook_rs — 本次交付说明

本次交付范围：**不需要 onas 的四个模块**——图片(jpg/png/gif，InMemory 路径)、
PDF(仅前6页，纯内存缓存)、文本/代码查看器(tree-sitter/syntastica 高亮)、
Markdown(egui_commonmark)。

webp/avif（图片）与 mkv/webm（视频）需要 onas，本次不实现；
dispatcher 探测到这几类文件时会显示"暂不支持"的占位提示，不会崩溃。

## 在你自己机器上编译前必读

**这份代码没有在编写它的沙盒环境里跑通完整编译**，原因是那个环境只能装到
rustc 1.75（2023年底版本），而 egui/egui_commonmark 现在的版本要求 1.76+
（egui_commonmark 0.23 甚至要求 egui 0.34）。这不影响代码本身的正确性，
但意味着你需要在自己的机器上做第一次真正的 `cargo build` 验证——
沙盒里只做到了：
- 全部依赖关系在 crates.io 上确认真实存在、版本号互相匹配（不是编造的版本号）；
- 逐个 API 用法（`Processor::process`、`resolve_styles`、`CommonMarkViewer::new().show()`、
  `mupdf` 的 `Document::open`/`load_page`/`to_pixmap`/`Pixmap::samples` 等）都对照了
  当前 crates.io/docs.rs 上的真实文档，不是凭训练记忆编造的接口；
- 人工审查了每个文件的括号配对、明显的借用冲突、以及一处已发现并修正的
  反模式写法（`PlainText` 渲染原本用 `TextEdit::multiline(&mut content.clone())`
  每帧克隆全文，已改成只读 `Label`）。

**请在开始使用前跑一次 `cargo check`**，把编译器实际报出的错误反馈给我，
比继续凭空猜测更有效率。

### 环境准备

```bash
# 1. 确保 rustc 是当前 stable，且不低于 1.76（建议直接装最新版）
rustup update stable
rustc --version   # 确认版本号

# 2. 编译
cd quicklook_rs
cargo build --release
```

### PDF 功能：mupdf（AGPL-3.0），无需额外部署动态库

PDF 引擎用的是 **mupdf-rs**（对应 MuPDF），不是 PDFium。这是对上一版交付的
更正——上一版用了 PDFium，但那不是你要的东西，是我自作主张套用旧方案文档
加上去的，你从未批准过。

**许可证提醒（务必确认这依然是你要的）**：MuPDF 是 AGPL-3.0（或购买
Artifex 商业授权二选一）。你已经明确表示接受 AGPL，但这里再强调一次：
如果你分发这个程序却不购买商业授权，意味着整个项目都需要以 AGPL-3.0
对外开源。这和 PDFium 的宽松许可证是完全不同的法律状况，请在实际对外
分发前再确认一次这个选择依然成立。

部署上比 PDFium 方案简单：`mupdf-sys` 在 `cargo build` 时会从源码编译
MuPDF 并静态链接进最终二进制，**不需要**你另外下载、放置、分发一个
独立的动态库文件（不像 PDFium 需要单独的 `pdfium.dll`）。代价是：

- 编译机器需要 C/C++ 工具链 + libclang（供 bindgen 用）。
  Windows 上建议装 MSVC Build Tools + LLVM（提供 `libclang.dll`），
  具体步骤请查阅 `mupdf-sys` 仓库 README 里的平台相关说明。
- 首次编译会比纯 Rust 依赖慢不少（要编译 MuPDF 的 C 源码），
  这是一次性成本，之后的增量编译不受影响。

## 目录结构

```
src/
├── main.rs                    # 程序入口，支持命令行传入文件路径
├── core/
│   ├── mod.rs
│   ├── dispatcher.rs           # 文件类型探测与分流（含单元测试）
│   ├── request_gen.rs          # 请求代次管理，为后续onas异步接入预留（含单元测试）
│   └── window.rs               # eframe主App，串联四个provider的UI渲染
└── providers/
    ├── mod.rs
    ├── image.rs                 # jpg/png走zune-jpeg/zune-png，gif走image crate（含单元测试）
    ├── pdf.rs                   # PDF前6页渲染+内存LRU缓存，引擎为mupdf（含单元测试）
    └── text/
        ├── mod.rs               # 编码探测(BOM/chardetng) + 大文件视口策略框架（含单元测试）
        ├── highlight.rs         # tree-sitter/syntastica高亮 -> egui LayoutJob（含单元测试）
        └── markdown.rs          # egui_commonmark集成（不启用syntect相关feature）
```

## 已知的取舍和尚待你确认的点

1. **PDF 引擎是 mupdf，AGPL-3.0 许可证**：你已明确接受这个许可证含义，
   这里再提醒一次因为它比技术选型重要——分发本程序若不购买 Artifex
   商业授权，整个项目需要以 AGPL-3.0 对外开源。这是和上一版（误用
   PDFium）完全不同的法律状况，请在正式对外分发前再次确认。

2. **PDF 缓存不落盘**：按你的要求做成纯内存 `HashMap` 缓存，进程退出即清空，
   不写任何 AppData/Roaming/自定义缓存目录。代价是重启程序后同一份PDF
   需要重新渲染一次，这是"纯单文件、零磁盘footprint"这个约束下的
   直接后果，已经在 `providers/pdf.rs` 顶部注释里写明。

3. **图片解码引擎换成 zune-jpeg/zune-png（jpg/png），gif 仍用 image crate**：
   这是针对"是否有更轻量、性能更好、只覆盖需要的功能"这个问题的复盘结果——
   `image` crate 的 jpg/png 解码路径不是当前最优解，zune 系列 SIMD 加速、
   依赖树更小。gif 因为 zune 生态目前没有成熟的动图支持，保留用
   `image` crate（这是"各用各的强项"而非"整体套用一个通用库"）。
   PNG 解码这里额外做了 Luma/LumaA/RGB/RGBA 四种色彩空间到统一RGBA8的
   手动展开（而非依赖库内的 `png_set_add_alpha_channel` 选项——那个选项
   对 Luma 输入只会转出 LumaA，不会转出 RGBA，不满足"统一按RGBA8上屏"
   的需要），16位深度PNG当前明确报错而非静默降级精度，如果你的实际
   使用场景有较多16位PNG，需要回来补上到8位的正确缩放逻辑。

4. **Markdown 不再启用 `better_syntax_highlighting`**：
   之前的版本为了给 markdown 里的围栏代码块上色，加了这个会引入 syntect
   的 feature——这和你"主代码查看器要用 tree-sitter、不要 syntect"的
   要求是矛盾的，即使只是次要场景也不该引入。现在去掉了，代价是
   markdown 文件里的代码块不再有语法高亮，只显示等宽字体纯文本。
   如果你后续觉得这块高亮体验值得要，需要走"自己拦截围栏代码块、
   调用 highlight.rs 处理"这条路，是一块独立的额外工作量。

5. **大文件"只渲染可见视口"策略目前只搭了框架，未接入UI**：
   `providers/text/mod.rs` 里的 `LineIndex`、`spawn_highlight_worker`、
   `ViewportHighlightRequest` 这套异步管线已经写好且有单元测试覆盖核心逻辑
   （行索引切片、请求代次过期判断），但 `core/window.rs` 当前为了先把
   四个模块的核心解码/渲染逻辑跑通，走的是同步全文高亮的简单路径。
   把这套异步管线接到 `ScrollArea` 的滚动回调上、真正做到"大文件不卡顿"，
   是下一步需要补上的接线工作，目前的实现对中小文件完全够用，
   只是还没有真正验证过对超大文件（比如几十MB的日志）的表现。

6. **动图(gif)播放机制**：用 `ctx.request_repaint_after` 做帧定时切换，
   已经接入 `eframe::App::update`，逻辑上是完整的，但同样没有在真实
   GUI环境里跑过，建议你测试时专门用一个动图验证播放效果是否流畅。

7. **onas_bridge 完全未实现**：`providers/mod.rs` 里特意留了注释占位，
   等你确认 onas 的调用方式（子进程/输出协议）后再回来补上这部分，
   dispatcher/window.rs 里的 `RequiresOnas` 分支已经预留好了接入点。
