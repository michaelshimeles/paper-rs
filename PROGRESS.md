# Progress

Last updated: 2026-10-17.

## Phase 3: local-first design and whiteboard tool

Goal: grow paper-rs into a Paper / Figma / Excalidraw-class tool. Figma and Paper are studied
clean-room (behavior only, `analysis/FIGMA_NOTES.md`, `analysis/PAPER_ARCHITECTURE.md`).
Excalidraw is MIT and studied from source (`analysis/EXCALIDRAW_NOTES.md`); ported algorithms
are credited in `THIRD_PARTY.md`.

| Milestone | Status | Evidence |
|---|---|---|
| Analysis notes | ✅ done | `analysis/FIGMA_NOTES.md`, `analysis/EXCALIDRAW_NOTES.md`, fixtures in `tests/fixtures/excalidraw/` |
| P1: local projects, home screen, autosave, atomic writes, versioned format | ✅ done | `src/project/`, `src/session.rs`, `src/io/atomic.rs`, `tests/projects.rs::m1_*`, `milestone1_home.png`, `milestone1_trash.png` |
| P2: multiple pages, infinite canvas per page | ✅ done | `src/editor/pages.rs`, `src/ui/pages.rs`, `tests/projects.rs::m2_*`, `milestone2_pages.png` |
| P3: Figma core | ✅ done | `src/layout.rs`, `src/editor/{structure,vector_ops,text_edit}.rs`, `src/render/{draw,tess}.rs`, `src/ui/inspector.rs`, `tests/figma.rs`, `tests/perf.rs`, `milestone3_figma_core.png`, `milestone3_canvas.png` |
| P4: Excalidraw core | ✅ done | `src/render/rough.rs`, `src/connector.rs`, `src/editor/whiteboard.rs`, `src/library.rs`, `src/ui/library.rs`, `tests/excalidraw.rs`, `milestone4_whiteboard.png`, `THIRD_PARTY.md` |
| P5: inspector, layers drag-reorder, command palette, shortcuts | ✅ done | `src/ui/{inspector,palette}.rs`, `src/ui/mod.rs` (layers drag, help), `src/editor/{commands,shortcuts}.rs`, `src/macos.rs`, `tests/workflow.rs`, `milestone5_palette.png`, `milestone5_shortcuts.png`, `milestone5_inspector.png` |
| P6: import/export (PNG/SVG/PDF, .excalidraw, SVG), cross-project paste | ✅ done | `src/io/{pdf,export,excalidraw,svg_import,clipboard}.rs`, inspector Export section, CLI `--export*`/`--import`, `tests/io.rs`, `milestone6_*.png` |
| P7: MCP server for AI agents (Paper-compatible tools, HTTP + stdio bridge) | ✅ done on Linux and Windows; ⏳ Mac run pending (steps below) | `analysis/paper-mcp.md`, `src/mcp/`, `src/bin/paper-rs-mcp.rs`, `tests/mcp.rs`, `milestone7_mcp.png`, `milestone7_mcp_live_xvfb.png`, `milestone7_mcp_live_windows_arm64.png` |

### P7 notes: MCP server

* **Study** (`analysis/paper-mcp.md`): Paper's desktop app serves MCP over streamable HTTP on
  `127.0.0.1:29979/mcp` (hono + MCP SDK), forwards calls into the renderer's editor, guards
  against browsers and DNS rebinding, and ships a Go `paper mcp` stdio relay. 29 tools; the
  write path is `write_html` (HTML + inline CSS → layers) and `update_styles` (CSS maps).
  Studied with REA (behaviour only; raw dumps in gitignored `analysis/raw/`).
* **Server** (`src/mcp/server.rs`, rmcp 3.5): streamable HTTP on `127.0.0.1:29980/mcp`
  (`PAPER_RS_MCP_PORT`, `PAPER_RS_MCP=0` to disable), on its own Tokio thread. Calls are sent
  to the winit loop (`UserEvent::Mcp`) and run on the UI thread via `mcp::exec`, each as one
  `Editor::edit` transaction (one undo step, live repaint, autosave). Edits to other pages go
  to that page's own document and history (`Editor::with_page`) without switching the view.
* **Bridge** (`paper-rs-mcp`): rmcp stdio server that lists tools offline and relays calls to
  the app over HTTP, reconnecting after app restarts.
* **Tools** (`src/mcp/tools.rs`): Paper's names and argument shapes where there is an
  equivalent (see README), plus `create_shape`, `select_nodes`, `undo`, `redo`.
  `write_html` (`src/mcp/{html,css}.rs`, `exec.rs` `Builder`) maps flex/block layout onto auto
  layout with CSS stretch semantics, text styles and rich-text runs, backgrounds, gradients,
  borders, images, inline SVG and clones.
* **Verified**: `tests/mcp.rs` (12 tests: every tool on a headless session, rmcp client over
  HTTP and through the bridge, browser/Origin rejection, a raw 2026-07-28 request through the
  bridge, screenshots). End to end on Linux: the real app under Xvfb, driven by **Claude Code**
  (`claude -p` with the server added over HTTP, then over the stdio bridge) drew a frame with
  a heading, paragraph, button and sticky note that appeared live in the window
  (`docs/screenshots/milestone7_mcp_live_xvfb.png`). That run found a bug the rmcp-only tests
  missed (bridge results lacked `resultType` for 2026-07-28 clients), fixed in `01e0836`.
  The macOS target type-checks (`cargo check --target aarch64-apple-darwin --all-targets`).
* **Mac verification steps** (not run here: no access to the Mac from this machine). In
  `/Users/Shared/projects/paper-rs` on the Mac:

  ```sh
  export CARGO_TARGET_DIR=~/.cache/paper-rs-target   # keep build output out of the synced tree
  cargo build --release
  $CARGO_TARGET_DIR/release/paper-rs &               # app; log says "MCP server listening on http://127.0.0.1:29980/mcp"
  curl -s http://127.0.0.1:29980/x                   # → {"status":"not_found",...The MCP endpoint is /mcp."}

  # HTTP client
  claude mcp add --transport http --scope user paper-rs http://127.0.0.1:29980/mcp
  claude -p "Use the paper-rs MCP tools: create_file 'Mac check', open_file it, create a 480x320 \
    artboard 'Hello', and write_html a heading 'Hello from the Mac' into it." \
    --allowedTools "mcp__paper-rs__*"
  # stdio client (through the bridge)
  claude mcp remove paper-rs --scope user
  claude mcp add --scope user paper-rs -- $CARGO_TARGET_DIR/release/paper-rs-mcp
  claude -p "In paper-rs, add a yellow sticky note saying 'stdio works' next to the Hello artboard." \
    --allowedTools "mcp__paper-rs__*"
  # Codex works the same way:
  #   codex mcp add paper-rs --url http://127.0.0.1:29980/mcp
  #   codex mcp add paper-rs -- $CARGO_TARGET_DIR/release/paper-rs-mcp
  ```

  Done when the "Mac check" project is open in the window with the "Hello" frame and its
  heading (and the sticky note) visible, and ⌘Z removes each agent edit in one step.
* **Not carried over**: comments, design tokens, AI image generation, video export, teams;
  moving layers between pages; remote image URLs (see README Known gaps).

### P6 notes

* **Export** (`io/export.rs`): scope = selection, each frame, page or all pages; format =
  PNG (1–4x, transparent or white), SVG or PDF. Several PNG/SVG outputs go into a chosen
  folder (named after the frames/pages); PDF is always one file with one page per item.
  Inspector Export section, ⌘⇧E / ⌘⌥⇧E / ⌘⌥P, copy as PNG (⌘⇧C) / SVG, and the CLI.
* **PDF** (`io/pdf.rs`): hand-written PDF 1.7 writer over the shared draw ops — paths
  (quadratics elevated), solid fills, axial/radial shadings (stitched functions for 3+
  stops), images as Flate-compressed RGB XObjects with an alpha soft mask, strokes with
  caps/joins/dashes, ExtGState opacity and all 16 blend modes, frame clipping. Verified by
  structure tests and by rasterizing with pdftoppm.
* **Excalidraw import** (`io/excalidraw.rs`): scenes and clipboard JSON (rectangles,
  ellipses, diamonds, text, lines, arrows with bindings/elbows/heads, freedraw with
  pressures, images from `files`, frames, sticky notes with their text, groups → group
  layers, rotation baked into paths, hand-drawn style kept with the element's seed and fill
  style); `.excalidrawlib` v1/v2 into the library.
* **SVG import** (`io/svg_import.rs`, usvg): paths with transforms, fills (solid, linear,
  radial), strokes, opacity, embedded raster images, nested SVG and text outlined with Inter.
* **Clipboard** (`io/clipboard.rs`): copy writes a `paper-rs/clipboard` JSON payload with
  image bytes to the system clipboard (arboard), so layers paste between pages, projects,
  windows and restarts; paste also takes Excalidraw JSON, SVG markup, images and plain text.
  Our own last copy pastes from the internal clipboard (offset on the same page).
* Drag-and-drop of `.svg`, `.excalidraw`, `.excalidrawlib` and images onto the canvas, and
  ⌘O imports into the current page.

### P5 notes

* **Inspector**: grouped macOS-style sections (header actions, alignment icons, position &
  size with per-corner radii, constraints / sizing, auto layout, text, fills with gradients
  and images, stroke, layer blend/opacity, hand-drawn, arrow, sticky). Number fields scrub
  and coalesce into one undo step; edits inside instances become overrides.
* **Layers tree**: drag rows to reorder — above / below a row, or into a frame or group (the
  middle of the row); the canvas position is kept and the move is one undo step.
* **Command palette** (⌘K, also ⌘⇧/): fuzzy search over every bound action, all tools, pages,
  library items and layers (select + zoom). Arrow keys, Enter, Esc.
* **Shortcuts**: one binding table drives dispatch, the grouped help panel (⌘/ or ?), the egui
  menu bar and the native macOS menu (⌘ chords become key equivalents; single keys are
  shown in titles). Added Tab/⇧Tab sibling selection, ⇧Enter parent, ⇧H/⇧V flip,
  ⌘⌥C/⌘⌥V copy/paste properties, ⌥-drag duplicate (one undo step), PageUp/PageDown pages,
  ⌘R rename, ⌘\ hide UI, ⇧0 zoom 100%, Excalidraw number keys. 100+ bindings, checked for
  conflicts by a test.
* Deferred: Figma's number-key opacity (the digits pick tools, as in Excalidraw), drag of
  multiple layers at once in the tree, customizable shortcuts.

### P4 notes

* **rough.js 4.6.4 port** (`render/rough.rs`): RNG, lines, curves, ellipses, SVG paths, solid,
  hachure, cross-hatch, zigzag and dots fills, plus Excalidraw's element → options mapping
  (`adjustRoughness`, dash patterns, fill weight, hachure gap). The test compares ten
  generator calls against real rough.js output (`tests/fixtures/rough/`, produced by
  running the MIT library under Node) to 1e-6, so the same seed draws the same wobble.
  Per-node toggle (⌘⌥H or the inspector), sloppiness 0/1/2, fill style, re-roll seed; with
  nothing selected the toggle sets the default for new shapes.
* **Arrows** (`connector.rs`): a `Connector` node with start/end bindings (`node`, normalized
  fixed point, inside/orbit). Orbit ends stop at the target outline + `5 + stroke/2`, aimed
  at the other end's focus, and re-route in the fixup pass whenever a bound layer (or an
  ancestor) moves, resizes or is deleted. Straight, curved (Catmull-Rom) and elbow routes;
  elbow uses a grid + A* with Excalidraw's bend penalty and segment-count heuristic.
  Heads: arrow, triangle, dot, bar, diamond. Endpoint handles rebind; the midpoint handle
  bends. Labels on shapes defer to the shape when binding.
* **Quick diagram shapes**: diamond tool (D), polygon/star, and quick-connect "+" knobs on a
  selected shape that duplicate it 80 px away and connect the two with a bound arrow.
* **Sticky notes** (S): colored note with padding 16, text wraps and the font shrinks
  (to 12) to fit, six colors.
* **Laser pointer** (K): a trail that fades over 1 s and shows the last 50 points, eased like
  Excalidraw; the event loop animates only while points are visible, then idles.
* **Library**: a shared `library.json` in the projects folder (starter flowchart items on
  first use), a Library tab with tessellated previews, add-selection, rename, remove, insert
  (fresh ids, arrow bindings and instance links remapped).
* Number keys 1–0 select tools like Excalidraw (1 select … 0 eraser).
* Deferred depth: Excalidraw's dynamic-AABB "side hack" in elbow routing, arrow labels and
  text bound inside shapes, multi-point arrows by clicking, freedraw with perfect-freehand
  (paper-rs keeps its own ink), rough rendering of text/ink, collaboration.

### P3 notes

* **Renderer rework.** Nodes become backend-neutral draw ops (fills with solid / linear /
  radial / image paints, strokes, glyph runs) in `render/draw.rs`. The GPU path tessellates
  them in world space and caches per node (keyed by content, world rect, inherited opacity and
  zoom octave). Unchanged frames reuse the item list; the world vertex buffer is only
  re-uploaded when the item list changes, so panning costs one uniform write. Gradients use a
  ramp atlas; images are mipmapped textures; multiply/screen/darken/lighten have their own
  blend pipelines (premultiplied output). The SVG exporter consumes the same ops.
* **Model v3**: fill layers, blend, stroke align/dash/caps/joins, per-corner radii, rich text
  spans, auto layout, layout-child sizing and constraints, component flag and instance links,
  groups, vector paths, polygons, stars. Page format v3 with a v2→v3 migration (`fill` →
  `fills`).
* **Fixup pass** (`Editor::fixup`) runs inside every transaction: normalizes group bounds,
  syncs instances of edited components (respecting per-property overrides recorded by the
  inspector, text edits and drags), and re-runs auto layout from the outermost auto-layout
  ancestor. It undoes together with the edit.
* **Resize** restores the drag-start snapshot each move and re-applies constraints/group
  scaling from it, so repeated resizes never drift.
* **10k elements**: `tests/perf.rs` builds 10 000 nodes (rects, ellipses, stars, text), all in
  view. Release: first frame ~140 ms (CPU tessellation of everything), cached frame ~3 ms,
  editing one node re-tessellates one node, panning re-uploads nothing.
* Deferred depth: rounded-corner frame clipping, live (non-destructive) booleans, on-canvas
  gradient handles, rotation, variants, layout grids, min/max sizes, baseline alignment, and
  exact GPU compositing for the remaining 11 blend modes and group opacity.

### P2 notes

* Each page is its own infinite canvas with its own undo history, selection and camera. The
  active page lives in the editor; the others are parked in their slot (`PageStash`).
* Pages panel at the top of the left sidebar: click to switch, + to add, double-click to rename,
  right-click for Rename / Duplicate / Delete, drag to reorder. The last page can't be deleted.
* Paste on another page (or in another project) keeps the original position; paste on the same
  page offsets by 16 px.
* Deleting a page isn't undoable (the page file is removed at the next save).

### P1 notes

* Projects live in `~/Documents/paper-rs/<name>/` (`PAPER_RS_HOME` overrides): `project.json`
  (manifest: id, name, created/modified, ordered pages, active page), `pages/<id>.json`, and
  `thumbnail.png`. Deleting moves the folder to `.trash/` with a `trashinfo.json`, restorable from
  the Trash view.
* Every write is temp file + fsync + rename + directory fsync (`io::atomic`). Stale temp files
  from a crash are removed when the project opens.
* Formats carry `format` + `version`; loading runs a chain of `Value -> Value` migrations.
  paper-rs 0.1 `.paper-rs` files are page format v1 and migrate to v2 (tested).
* Autosave: changes are written 1 s after the first unsaved edit (bounded latency even while
  editing, never during a drag), on page switch, when leaving the project and on quit. The event
  loop sleeps until the autosave deadline, so idle CPU stays at zero. Thumbnails refresh at most
  every 20 s and when leaving the project.
* Known gap: typing inside an open text edit is saved when the edit ends (it is one undo step).

### Phase 3 verification log (2026-10-11)

* `cargo build --release`: no warnings. `cargo clippy --all-targets`: clean. `cargo fmt`.
* `cargo test`: 99 unit tests + 42 integration tests (projects, figma, excalidraw, workflow,
  io, perf, headless), all passing; GPU tests on Mesa lavapipe.
* `cargo check --target aarch64-apple-darwin` and `--target x86_64-pc-windows-gnu` pass
  (the macOS menu is generated from the shared menu table and type-checks).
* Real window under Xvfb: idle CPU measured from `/proc/<pid>/stat` over 5 s after startup —
  0 ticks on the home screen and 0 ticks in the editor. Autosave, tooltips and the laser
  trail wake the loop only while they need to.
* 10k elements (`tests/perf.rs`, release): cached frame ~3 ms, first frame at a new zoom
  octave ~140 ms, one edited node re-tessellates one node, panning uploads nothing.
* Not yet run on Mac hardware (no Mac available to this session); see README Known gaps.

### Windows verification log (2026-10-17)

* Machine: Windows 11 Pro ARM64, Snapdragon X Elite (X1E80100), Adreno X1-85 (driver
  31.0.133.1), 150% display scaling. Rust 1.98.1 `aarch64-pc-windows-msvc`, Visual Studio
  Build Tools 2022 with the ARM64 tools.
* `cargo build --release`: native ARM64 binaries (PE machine `0xAA64`), no warnings.
* `PAPER_RS_REQUIRE_GPU=1 cargo test`: 162 tests (108 unit + 54 integration), all passing;
  the GPU tests ran on the Adreno. pdftoppm is not installed, so the PDF test checked
  structure only.
* Rendering matches the Linux (lavapipe) references. Of the 26 PNGs the tests write, one
  (`m6_export_svg_via_resvg.png`) is byte-identical and the others have at most 0.0031% of
  pixels differing by more than 16/255 (anti-aliasing), except the home and trash screens,
  which differ only in the printed projects-folder path. `m6_export.svg` matches the
  committed file; `m6_launch_plan.paper-rs` differs in the last digit of three ink pressures
  (platform libm rounding). The regenerated files were not committed.
* `cargo clippy --all-targets`: one warning, `clippy::drop_non_drop` at `tests/perf.rs:105`
  (clippy 1.98; the code has no platform `cfg`), left as is.
* x64: `cargo build --release --target x86_64-pc-windows-msvc` builds (PE machine `0x8664`)
  and `--screenshot --demo` renders on the Adreno under x64 emulation. Not run on x64
  hardware.
* Real window: `paper-rs --demo` opens and draws at 150% scaling; wgpu picks the Adreno
  Vulkan driver. The MCP server listens on `127.0.0.1:29980`. With `WGPU_BACKEND=dx12` the
  window runs on DirectX 12, and headless renders on DX12 and GL (via D3D12) match the
  Vulkan one to within 1/255 per channel.
* MCP end to end: Claude Code (`claude -p` with `--mcp-config`) created the "Windows check"
  project, a "Hello" artboard and its heading over HTTP, then a sticky note through
  `paper-rs-mcp.exe`. Both appeared live in the window and the project autosaved
  (`docs/screenshots/milestone7_mcp_live_windows_arm64.png`).
* Both binaries are console programs (PE subsystem 3), so starting `paper-rs.exe` from
  Explorer also opens a console window (README Known gaps).

## Phase 2 (paper-rs 0.1) record

| Phase / milestone | Status | Evidence |
|---|---|---|
| Phase 1: reverse engineering | ✅ done | `analysis/PAPER_ARCHITECTURE.md`. REA scoped run is summarized in the appendix; raw output in `analysis/raw/` (gitignored). |
| Phase 2: design | ✅ done | `analysis/DESIGN.md` |
| M1: window + infinite canvas, smooth pan/zoom | ✅ done | `tests/headless.rs::m1_*`, editor scenario tests, `m1_*.png`, `x11_window_xvfb.png` |
| M2: freehand sketch with smoothing | ✅ done | `ink.rs` tests, `m2_freehand_sketch_with_smoothing`, `m2_sketch.png` |
| M3: shapes, frames, text | ✅ done | scenario tests, `m3_shapes_frames_and_text`, `m3_*.png` |
| M4: select/move/resize, multi-select, snapping | ✅ done | scenario tests, `m4_select_move_resize_snap`, `m4_*.png` |
| M5: layers panel, undo/redo | ✅ done | `history.rs` tests, `undo_redo_round_trips_a_whole_session`, `m5_layers_and_undo_redo`, `m5_*.png` |
| M6: save/load `.paper-rs`, PNG/SVG export | ✅ done | `io/*` tests, `m6_save_load_and_export`, `m6_svg_export_matches_gpu_render`, `m6_*` |
| M7: keyboard shortcuts | ✅ done | `editor/shortcuts.rs` (conflict test), README table, in-app `?` help |
| README (build/run, parity table, gaps) | ✅ done | `README.md` |

## Verification log

* `cargo build --release` passes with no warnings. `cargo clippy --all-targets` is clean.
* `cargo test` runs 51 unit/scenario tests and 8 headless GPU tests, all passing. The GPU tests
  ran on Mesa lavapipe (llvmpipe, Vulkan) on the headless VPS.
* The real windowed app was launched under Xvfb (X11) with the release binary and screenshotted.
  It needed `libxkbcommon-x11` and `libxcb-xkb` placed in `~/.local/lib`, since the VPS has no
  root access.
* `cargo check --release --target aarch64-apple-darwin` and `--target x86_64-pc-windows-gnu`
  both pass.
* The SVG export was rasterized with resvg and compared with the GPU PNG export: 0.004% of
  pixels differ.

## Notes

* Commit history: the core model, the editor and the app/renderer/I/O layer landed as three
  feature commits. Each one builds in release mode and has its own tests. Milestones 1–7 are
  spread across them because the layers depend on each other: the app needs I/O, and the
  overlay needs the editor.
* REA's full-asar analysis stalled on `node_modules/typescript/lib/_tsc.js` after 20+ minutes
  at 2.6 GB RSS. A scoped run on Paper's own sources finished in 65 s.

## Next steps (not started)

1. Image nodes (textured quads) and drag-and-drop of image files, to match `paper-asset:`.
2. Per-node mesh cache keyed by revision and zoom bucket.
3. Rotation, plus a bezier pen tool.
4. Multiple pages, and system clipboard interop (SVG/PNG/text).
5. Run and tune on macOS hardware, especially trackpad scroll feel and tablet pressure.
