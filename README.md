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

Run `rooq` with no arguments to start it as a background daemon: it adds a
tray icon and waits. Select a file in File Explorer and press Space to open
a preview; press Space again to close it. No window appears until you press
Space, and the process keeps running in the background between previews
(exit it from the tray icon's "Exit" item).

```bash
rooq
```

Running `rooq path/to/file` instead opens that file directly in a normal
window, independent of the daemon — useful for testing or for wiring into
a right-click "Open with Rooq" entry.

```bash
rooq path/to/file
```

### How file selection works

The daemon reads the selected file from the foreground Explorer window via
Windows' Shell Automation COM interfaces (the same mechanism behind
features like "Copy as path") — no Explorer extension or admin rights
needed. The Space key is watched with a low-level keyboard hook rather than
`RegisterHotKey`, and only acts when Explorer is the foreground window;
Space presses everywhere else (typing, games, other apps) pass through
untouched.

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
├── main.rs                    Entry point: daemon mode (no args) or single-file mode (path arg)
├── daemon/
│   ├── mod.rs                  Ties the tray icon and keyboard hook to one Win32 message loop
│   ├── hotkey.rs                WH_KEYBOARD_LL hook: detects Space in the foreground Explorer window
│   └── selection.rs             Reads the selected file from Explorer via Shell Automation COM
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
- **Only the first selected file is previewed** when multiple files are
  selected in Explorer; `daemon/selection.rs` reads item 0 of the
  selection and ignores the rest.
- **The daemon doesn't yet handle the desktop** as a selection source
  (only File Explorer windows) — pressing Space with an icon selected on
  the desktop does nothing, since the desktop isn't one of the windows
  `IShellWindows` enumerates the same way.
- **A second Space press may not register as toggle-off if the preview
  window has taken OS focus** away from Explorer, since the hook only
  treats Space as a toggle when Explorer is the foreground window; see
  the comment on `run_daemon` in `main.rs`. `with_always_on_top()` avoids
  deliberately stealing focus, but this hasn't been confirmed against a
  real Explorer window.

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
- **The `daemon/` module's Shell Automation COM calls haven't been run
  against a real Explorer window** — this environment has no Windows
  machine to test against. The overall call chain (`IShellWindows` ->
  match foreground `HWND` -> `IShellFolderViewDual::SelectedItems`) is
  standard and documented, but the exact `VARIANT` construction for
  `IShellWindows::Item`'s index parameter (flagged inline in
  `selection.rs`) is the one call not verified against a real compile.
- **No "start with Windows" option.** Right now the daemon only runs for
  as long as it's manually launched each session; a real install would
  want a Startup shortcut or registry entry, which doesn't exist yet.
