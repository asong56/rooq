# Rooq 精简重写技术方案 v3（Rust / Single Binary，复用 onas）

**范围**：图片（jpg/png/webp/avif/gif）+ 文本/代码/Markdown 查看器 + PDF + 视频（mkv/webm，首帧缩略图，见第6节——最初被 onas 接口挡住，onas v0.2.0 加了 `frame` 子命令后已打通）。
GUI 框架：egui + eframe（纯 Rust 静态链接，理由见 v2 方案，此处不再重复论证）。
onas：你自建的 Rust 编译产物（独立 app，非 crate），用于处理图像（webp/avif）与视频（mkv/webm）解码。**onas 的 CLI 接口已核实源码确认（v0.2.0）**，图片 webp/avif 分支和视频首帧分支均已打通，详见第6节。

---

## 1. 图片模块：自动分流策略

按你的决定，`dispatcher.rs` 在探测到文件类型后自动决定路径，用户无感知：

```rust
enum ImagePath {
    InMemory,   // jpg/png/gif
    ViaOnas,    // webp/avif
}

fn route_image(kind: infer::Type) -> ImagePath {
    match kind.extension() {
        "jpg" | "jpeg" | "png" | "gif" => ImagePath::InMemory,
        "webp" | "avif" => ImagePath::ViaOnas,
        _ => ImagePath::InMemory, // 保底兜底，尝试用image crate硬解一次
    }
}
```

- **InMemory 路径**：`image` crate 直接在进程内解码，jpg/png 是纯 Rust 实现，无外部依赖；gif 走 `image` crate 自带的帧迭代器支持动图。零子进程开销，几毫秒级完成。
- **ViaOnas 路径**：webp/avif 这两种格式的解码复杂度/生态成熟度都不如 jpg/png（尤其 avif 依赖 AV1 解码器，是相对新且重的技术栈），交给 onas 处理，Rust 侧不需要为这两种冷门格式引入额外的解码器依赖，减少体积和维护面。

这个分流点本身就是 `dispatcher.rs` 里的一个简单函数，后续如果 onas 新增支持其他格式（比如 heic），只需要在 match 分支里加一行。

---

## 2. PDF 模块：仅前 6 页转 jpg 缓存，硬性上限、不提供后续页面

**核心思路**：这不是"完整 PDF 阅读器"，是"快速确认文件内容"的预览工具。绝大多数预览场景用户只是想知道"这是不是我要找的那份文件"，看前几页就够了——**第7页及以后的内容本方案不提供渲染，也不打算作为"低频兜底路径"支持**。这是与上一版最大的区别：上一版还保留了"超过6页现场单页渲染"的退化路径，本版把这条路径整个砍掉，PDF 查看器的能力边界就是"前6页，仅此而已"。

### 流程设计

```
用户按空格预览 xxx.pdf
        │
        ▼
检查缓存目录是否已有该文件(按路径+mtime+size做key)的预渲染jpg
        │
   ┌────┴────┐
  命中        未命中
   │           │
   ▼           ▼
直接显示    用pdfium-render打开文档
              │
              ▼
         仅渲染 min(6, total_pages) 页
         (文档不足6页时，实际渲染页数=文档总页数)
              │
              ▼
         每页转成jpg，存入缓存目录
         （文件名如 <hash>_page01.jpg ... pageNN.jpg，NN<=6）
              │
              ▼
         显示第1页，UI仅提供"下一页/上一页"翻页，
         范围限定在已缓存的1~min(6,total_pages)页之间
```

### 关键设计决策

1. **为什么是 jpg 而不是保留 PDFium 渲染的位图直接展示**：
   转成 jpg 文件、写入磁盘缓存，好处是**同一份文件重复预览时可以完全跳过 PDFium 渲染**，直接读缓存图。PDFium 的页面渲染（尤其含大量矢量图形、复杂字体嵌入的PDF）不是免费的，缓存能让"反复预览同一份文件"（用户在文件管理器里来回切换看几眼）这个高频场景几乎零延迟。

2. **为什么硬性限定前6页、不提供后续页面**：
   这是产品边界的主动收窄，不是技术限制。预览工具的核心价值在于"确认这是不是我要的文件"，超过6页的深入阅读需求本身就该交给专门的PDF阅读器去做，不应该让这个精简工具承担"完整阅读体验"的职责——一旦支持"翻到第7页时现场渲染"，就意味着要处理任意页码的按需渲染、大文档的内存/性能边界（比如几百页PDF、含超大分辨率扫描图的PDF）等一整套复杂度，而这些复杂度对应的使用场景本身就超出了"预览"的范畴。明确不做，把复杂度锁死在"最多6页"这个可预测、可测试的范围内，是让这个模块保持"轻量"的关键决定。

3. **页数不足6页的文档**：
   自然处理——PDFium 报告文档总页数，取 `min(6, total_pages)`，不需要特殊分支，UI上翻页按钮到最后一页自动禁用，不存在"用户以为还有下一页但其实没有"的困惑。

4. **用户尝试查看更多内容时的体验**：
   翻到已缓存的最后一页后，"下一页"按钮直接置灰/禁用，不做任何"现场渲染"的兜底逻辑。如果需要，可以在UI上给一行小字提示（比如"仅预览前N页，完整阅读请用PDF阅读器打开"），但这只是体验上的诚实告知，不涉及任何额外渲染能力。

5. **缓存失效策略**：
   用"文件路径 + 修改时间(mtime) + 文件大小"组合做 key（比读取整个文件内容做hash更快，对于"预览工具"这个场景，用户几乎不会遇到"内容变了但mtime和size都没变"的边界情况，用完整内容hash是不必要的性能开销）。文件被修改后自然重新触发渲染并覆盖缓存。

6. **缓存清理**：
   需要一个简单的 LRU 或"总大小上限"策略清理缓存目录（比如设定缓存目录不超过 500MB，超过时删除最久未访问的条目），避免用户预览过大量PDF后缓存无限增长。这块工作量不大，用一个 sqlite 或者干脆一个 json 索引文件记录"文件hash -> 最后访问时间"就够，不需要引入完整的数据库依赖。

### 交互体验

- 首次预览稍有延迟（渲染最多6页，取决于PDF复杂度，通常在几十到几百毫秒量级）。
- 之后预览同一文件秒开（读缓存jpg）。
- 到达已缓存的最后一页即止步，没有"现场单页渲染"这个退化路径——这也意味着 `pdf.rs` 模块完全不需要处理"任意页码按需渲染"这类更复杂的场景，代码复杂度和测试范围都显著收窄。

---

## 3. Markdown 在 egui 路线下的方案

**结论：用 `egui_commonmark`**，这是 egui 生态里专门做这件事的成熟第三方 crate，不需要你自己写 Markdown AST 到 egui widget 的映射层。

关键特性：
- 支持 CommonMark 完整语法，外加 GitHub 风格扩展：表格、删除线、任务列表（checkbox）、脚注。
- 有 `better_syntax_highlighting` feature，可以给代码块做语法高亮（这块下面第4节会详细说明高亮引擎选型，因为你要求"不要照搬"原方案的 syntect）。
- 有 `svg` feature，支持 Markdown 中内嵌 SVG 图片显示。
- API 极简：维护一个 `CommonMarkCache`（做解析结果缓存，避免每帧重新解析同一份 Markdown 文本——这点对 immediate-mode GUI 很重要，因为 egui 每帧都会重新执行 UI 构建代码，如果不缓存解析结果会造成不必要的重复计算），然后 `CommonMarkViewer::new().show(ui, &mut cache, markdown_text)` 一行代码显示。

**集成方式**：
```rust
struct MarkdownProvider {
    cache: egui_commonmark::CommonMarkCache,
}

impl MarkdownProvider {
    fn render(&mut self, ui: &mut egui::Ui, content: &str) {
        egui_commonmark::CommonMarkViewer::new().show(ui, &mut self.cache, content);
    }
}
```

这一块是整个项目里工作量最小的模块之一，不需要额外设计。

---

## 4. 文本/代码查看器：抛弃 syntect，采用 tree-sitter 路线

你要求"不要照搬原版 TextViewer"，且要"最先进、最强大、最轻量、极速"。这四个形容词其实指向同一个技术方向的迁移：**从正则匹配的高亮引擎换成基于真实语法解析的高亮引擎**。

### 为什么 syntect 不是最优选择

`syntect` 是移植自 Sublime Text 的方案，核心是**基于正则表达式**做逐行状态机匹配（`.sublime-syntax` 语法定义本质是复杂的正则规则集合）。这个方案成熟、覆盖语言广，但有两个跟你的四个形容词相悖的地方：

1. **不是真正理解代码结构**：正则匹配容易在复杂嵌套场景出错（比如字符串内嵌表达式、多行注释边界），且无法做到"语义级"的高亮（比如区分"函数名定义"和"函数调用"）。
2. **性能上限受限于正则引擎**：虽然 syntect 官方给出的性能数据已经不慢（大文件重新高亮控制在100ms内），但正则状态机的本质决定了它在超大文件或者极端复杂语法上的表现不如真正的增量解析器。

### 推荐方案：tree-sitter 路线

**tree-sitter** 是当前（2026年）业界公认最先进的通用增量解析框架，被 Neovim、Helix、Zed、GitHub 的代码高亮后端等主流现代工具采用，核心优势：

1. **真正的语法解析**：为每种语言生成实际的语法树（AST），而不是正则匹配的"伪装理解"，高亮更准确（能正确区分变量/函数/类型/关键字等语义角色）。
2. **增量解析**：文件编辑后只需要重新解析变更的部分，不需要整个文件重新扫描——虽然你这个场景是"只读预览"不是编辑器，用不上增量编辑的优势，但这说明其底层解析性能设计目标就是极致的"快"。
3. **生态活跃**：几乎所有主流语言都有社区维护的 tree-sitter 语法包（`tree-sitter-rust`, `tree-sitter-python`, `tree-sitter-javascript` 等），且持续被 Neovim/Helix 这些大用户量项目验证和维护，比 Sublime 语法定义的更新频率更高。

**具体 Rust 集成方式**，两个可选 crate：

| crate | 说明 |
|---|---|
| **`tree-sitter-highlight`**（tree-sitter 官方子 crate） | 官方原生方案，直接消费 tree-sitter 的解析结果做高亮标注（"highlight query"机制），最底层、最灵活，但需要你自己写颜色主题映射和逐语言的语法包依赖管理。 |
| **`syntastica`** | 社区在 `tree-sitter-highlight` 基础上做的更高层封装，明确定位为"syntect 的替代品"，自带多种语言包集合（按需选择 `some`/`most`/`all` 三档，控制编译体积）、内置多套配色主题、API 更简洁。 |

**建议：用 `syntastica`**。理由：
- 定位就是"syntect 替代品"，减少你自己拼装底层 `tree-sitter-highlight` API 的工作量。
- 支持按需选择语言包档位（`syntastica-parsers-some`只含最常用的十几种语言，体积可控；`-most`/`-all` 覆盖更广但体积更大），你可以按预览场景需要覆盖的语言范围（常见的 rs/py/js/ts/go/c/cpp/json/yaml/toml/md/sh 等）精确控制体积，不需要打包不会用到的冷门语言语法包。
- 输出可以直接映射到 egui 的 `LayoutJob`（egui 的富文本布局机制，支持给文本片段分别指定颜色/字重），不需要额外的HTML中间层，渲染链路干净。

### 文本查看器的其他"极速"优化点

1. **大文件不整体加载**：对于体积很大的日志文件/代码文件，只解析和渲染当前视口可见的行范围（egui 的 `ScrollArea` 支持"虚拟滚动"式的按需渲染，只需要在滚动回调里判断可见区间，避免一次性对几十万行文件做完整tree-sitter解析）。这是真正决定"极速"体验的关键点——语言解析引擎再快，如果对一个100MB的日志文件做全量解析也会有明显延迟，"只解析可见部分"才是应对超大文件的正确策略。
2. **编码探测**：`chardetng` 或 `encoding_rs` 处理非 UTF-8 编码（比如老旧 GBK/GB2312 编码的中文文本文件），避免打开非UTF-8文件时显示乱码。
3. **异步解析**：文件打开后先立即显示原始文本（无高亮），高亮解析在后台线程完成后再刷新颜色。这样即使遇到超大文件或者冷门语言解析稍慢，用户也不会感觉预览"卡住不显示"，只是先看到无色文本、几十毫秒后颜色补上，体验上比"等解析完再一次性显示"更好。

---

## 5. 修订后的模块架构图

```
rooq/  (egui + eframe, 纯Rust静态链接)
├── core/
│   ├── dispatcher.rs       # magic-byte探测 -> 分发；图片自动分流InMemory/ViaOnas
│   ├── window.rs           # egui主循环、快捷键、多屏定位
│   ├── request_gen.rs      # 请求代次管理，处理"快速切换预览文件"时结果丢弃
│   └── cache_store.rs      # PDF页面jpg缓存 + LRU清理策略，索引用简单json/sqlite
├── providers/
│   ├── image.rs            # jpg/png/gif走image crate；webp/avif走onas_bridge（已实现）
│   ├── pdf.rs              # pdfium-render，仅渲染前6页转jpg存入cache_store，硬上限，不支持第7页及以后
│   ├── onas_bridge/
│   │   ├── mod.rs          # 公开API：convert_image_to_png、extract_video_frame
│   │   │                   # 没有做 FrameDecoder trait 抽象——onas是唯一实现路径
│   │   │                   # （独立app，非crate，没有"直接link为库"这个选项），
│   │   │                   # 一条路径不需要trait来对付"未来可能换实现"这种不存在的需求
│   │   └── subprocess.rs   # 子进程路线：定位可执行文件/超时/stderr处理，已实现（图片+视频两个分支）
│   └── text/
│       ├── mod.rs          # 编码探测(chardetng) + 大文件视口按需解析调度
│       ├── highlight.rs    # syntastica集成，tree-sitter驱动的高亮 -> egui LayoutJob
│       └── markdown.rs     # egui_commonmark集成
└── main.rs
```

注：视频首帧没有单独的 `providers/video.rs`——`onas_bridge::extract_video_frame`
转出临时 PNG 后，直接复用 `image.rs` 现成的 PNG 解码路径，调用方是
`core/window.rs` 里的 `load_onas_video_frame`，和 webp/avif 分支
（`load_onas_image`）是完全对称的两个函数，没有必要为视频单独开一个 provider 文件。

---

## 6. onas 接口确认结果（已核实源码，v0.2.0）

onas 已确认是**独立 Rust 编译的 app（非crate）**，`onas_bridge` 只有 `subprocess.rs` 这一条实现路径（没有"直接link为库"这个选项，除非未来 onas 额外导出 `cdylib`，那是 onas 那边的改造，不在本项目范围）。

拿到 onas 源码后核实了 `cli.rs`/`image.rs`/`video.rs`/`meta.rs`，原来列的5个问题现在都有确定答案：

1. **有没有"提取单帧"子命令？** 有——`onas frame <input> <output> [--at SECONDS]`，从视频里解出单帧编码成 PNG/JPEG。这是 onas v0.2.0 新增的子命令，写这份方案最初的版本时确认过还没有，后来 onas 更新后已经打通，详见下方"视频首帧缩略图"一节。
2. **输出到 stdout 还是文件？** 只支持文件。`image`/`frame` 两个子命令都是老老实实往 `<output>` 参数指定的路径写一个完整文件，没有 stdout 这个选项。
3. **输出文件名/路径谁定？** 调用方定——`<output>` 是普通的位置参数，onas 只是照着写，这部分和预期一致，`subprocess.rs` 指到临时目录里去。
4. **mkv/webm 有没有按时间戳/百分比取一帧的参数？** 有，`--at SECONDS`（时间戳）和 `--at-frame N`（帧号）二选一，互斥参数。不传时 onas 默认取第一帧（`pipeline::extract_frame` 里 `(None, None) => true // default: first frame`），这正好符合"随便一帧当缩略图"的需要，`subprocess.rs` 里两个都不传。
5. **错误处理/退出码约定？** `main()` 返回 `anyhow::Result<()>`，失败时进程以非零 exit code 退出（v0.2.0 起区分了具体错误类型，见 onas 的 `exitcode` 模块），完整错误链（"Error: ...\nCaused by:..."）打到 stderr。`subprocess.rs` 目前仍只把 stderr 整体当人类可读文本展示，不解析具体退出码——Rooq 只关心"成功还是失败"，暂不需要更细的区分。

图片 webp/avif 分支和视频 mkv/webm 首帧分支，实现思路完全一致：onas 转出临时 PNG（无损、不引入二次有损），复用 `providers/image.rs` 现成的 zune-png 解码路径，临时文件用 RAII guard 包一层，读完就删，不留盘——和第2节 PDF 缓存"不落盘"的原则保持一致（这里落盘本身没法避免，onas 只支持写文件，但生命周期收紧到"这一次调用期间"）。

### 视频首帧缩略图（已打通，不再是 TODO）

最初写这份方案时确认过"整文件转码换取一帧缩略图"这条退路走不通：`onas video` 无论怎么调用，输出都只会是另一个 `.mkv` 文件，从来不是图片；Rooq 自己没有视频帧解码器，转码完依然拿不到能上屏的像素。当时的结论是要等 onas 加一个新子命令才能打通。

onas v0.2.0 加上了 `onas frame` 子命令后，这个缺口已经堵上：复用 onas 已有的 H.264/H.265/VP9/AV1 解码器，解出第一帧就停、直接编码成 PNG/JPEG，不再需要整段重新编码+封装 mkv。Rooq 这边对应加了 `onas_bridge::extract_video_frame` 和 `core/window.rs` 的 `load_onas_video_frame`，用法和图片分支（`convert_image_to_png` / `load_onas_image`）完全对称，详见这两个函数旁的注释。

---

## 7. 建议的开发顺序（更新）

1. **文本/代码/Markdown 查看器**：`syntastica` + `egui_commonmark` + 大文件视口按需渲染，零外部工具依赖，最快跑通，验证 egui 主框架。
2. **图片 InMemory 路径**（jpg/png/gif）：验证 `dispatcher.rs` 分流逻辑的一半。
3. **PDF 前6页缓存方案**：验证 `pdfium-render` 静态链接 + `cache_store.rs` 的设计，明确不做超过6页的任何渲染路径，这是本版方案里除 onas 对接外最大的独立工作量。
4. **onas_bridge 打通（已完成，图片分支）**：`onas image` 子进程调用/超时/临时文件清理机制，接到了图片的 webp/avif 分支。
5. **视频首帧缩略图（已完成）**：`onas frame` 子命令上线后，复用第4步打通的子进程调用机制，加了 `extract_video_frame`/`load_onas_video_frame` 两个对称的函数接入视频分支，见第6节。
