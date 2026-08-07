# Rooq

A lightweight, single-binary file previewer built with egui/eframe. Not a
full reader for any format — it's for quickly confirming "is this the file
I'm looking for" without launching a heavier app.

## Supported formats

- **Images**: jpg, png, gif (decoded in-process via zune-jpeg/zune-png/image);
  webp, avif (via the external `onas` tool, see below)
- **PDF**: first 6 pages only, rendered via mupdf and cached in memory for
  the life of the process
- **Text/code**: encoding auto-detected (BOM, then `chardetng`); syntax
  highlighting via tree-sitter (`syntastica`) for common languages
- **Markdown**: rendered via `egui_commonmark`
- **Video**: first-frame thumbnail for mkv/webm (via `onas`)

## Building

Requires Rust 1.76+ (egui/egui_commonmark's minimum) and a C/C++ toolchain
with libclang for `mupdf-sys` to build MuPDF from source (see
[mupdf-sys's README](https://github.com/messense/mupdf-rs) for
platform-specific setup, e.g. MSVC Build Tools + LLVM on Windows).

```bash
rustup update stable
cargo build --release
```

## Usage

```bash
rooq path/to/file
```

Or run `rooq` with no arguments and open a file through the UI.

### The `onas` companion tool

webp/avif images and mkv/webm video thumbnails are not decoded by Rooq
directly. Instead Rooq shells out to `onas`, a separate executable, which
converts the input to a temporary PNG that Rooq then reads through its
normal PNG path. `onas` is not a Cargo dependency — it's located at
runtime via, in order: the `ROOQ_ONAS` environment variable, a file next
to the running `rooq` executable, or `onas` on `PATH`. If it can't be
found, or the conversion fails or times out, Rooq shows an error in the
preview area rather than crashing.

## Project layout

```
src/
├── main.rs                    Entry point; takes an optional file path argument
├── core/
│   ├── dispatcher.rs           File type detection (magic bytes, extension fallback) and routing
│   ├── request_gen.rs          Generation counter to discard stale preview results on quick switching
│   └── window.rs               eframe App: owns preview state, wires providers to the UI
└── providers/
    ├── image.rs                 In-memory jpg/png/gif decoding
    ├── pdf.rs                   First-6-pages rendering + in-memory LRU cache, via mupdf
    ├── onas_bridge/             Subprocess bridge to `onas` for webp/avif and video thumbnails
    └── text/
        ├── mod.rs               Encoding detection
        ├── highlight.rs         tree-sitter highlighting -> egui LayoutJob
        └── markdown.rs          egui_commonmark integration
```

## PDF licensing

The PDF engine is [mupdf-rs](https://github.com/messense/mupdf-rs)
(MuPDF), licensed AGPL-3.0 (or a commercial Artifex license as an
alternative). Distributing this program without a commercial license means
the whole project needs to be open-sourced under AGPL-3.0. `mupdf-sys`
compiles and statically links MuPDF at build time, so no separate dynamic
library needs to be shipped — but the license obligation applies
regardless.

## Known limitations

- **PDF caps at 6 pages.** This is a hard limit built into the API, not a
  setting — reading beyond page 6 means opening the file in an actual PDF
  reader.
- **PDF cache is in-memory only**, cleared on process exit. Re-opening the
  same PDF after a restart re-renders it.
- **CJK text rendering depends on a system font being present.** Rooq looks
  for `msyh.ttc` or `simsun.ttc` on Windows as a fallback for glyphs egui's
  default fonts don't cover (see `load_cjk_fallback_font` in
  `core/window.rs`). On a minimal English Windows install without East
  Asian language support, neither may exist, and CJK text will render as
  tofu boxes rather than crashing.
- **16-bit PNGs are rejected**, not downsampled. If that turns out to
  matter in practice, `providers/image.rs` needs a proper 16-to-8-bit
  scaling path.
- **Markdown code blocks have no syntax highlighting.** Enabling
  `egui_commonmark`'s `better_syntax_highlighting` feature would pull in
  syntect, which the rest of the project avoids in favor of tree-sitter;
  the trade-off was made in favor of consistency over that one feature.

## TODO

- **Large text/log files aren't virtualized.** `core/window.rs` currently
  highlights and lays out the whole file synchronously on open. Fine for
  typical source files; a viewport-based incremental highlighter (parse
  only visible lines, refresh asynchronously) would be needed for very
  large files (tens of MB) to stay responsive.
- **mupdf vs. a pure-Rust, permissively-licensed PDF renderer** is an open
  question, not a decision made here. Candidates worth evaluating if the
  AGPL obligation is unwanted: `pdf_oxide` (MIT/Apache-2.0, strong text
  extraction, but its page-rendering path looked less battle-tested as of
  early 2026) and `fop-pdf-renderer` (pure Rust, but built for validating
  generator output rather than handling arbitrary real-world PDFs).
