# paper-rs

A native Rust infinite canvas for planning, working and sketching. It is modeled on the
behavior of [Paper](https://paper.design) and built clean-room on `wgpu` + `winit` + `egui`.

![paper-rs main window](docs/screenshots/app_demo.png)

A local-first design and whiteboard tool. Projects are folders of pages; each page is an
infinite canvas with frames, groups, components and instances, auto layout and constraints,
vector paths and booleans, gradients and image fills, rich text, smart guides, and
pressure-sensitive ink — plus a whiteboard mode: hand-drawn (rough.js-style) shapes, arrows that
bind to shapes and re-route, elbow connectors, sticky notes, a laser pointer and a shape
library. Everything autosaves, and exports to PNG, SVG and PDF; Excalidraw files and SVG import.

![Figma-core features](docs/screenshots/milestone3_figma_core.png)

> **Clean-room note.** `analysis/PAPER_ARCHITECTURE.md` documents how Paper Desktop 0.5.15 is
> put together, from a licensed install. The Electron package turned out to be a thin shell
> around the hosted web editor. paper-rs re-implements *behavior* only. No Paper code,
> shaders, icons, fonts or assets are used or committed. The UI and canvas text use
> [Inter](https://rsms.me/inter/) 4.1 (SIL OFL 1.1), bundled in `assets/fonts/`.

## Reverse-engineered origin

paper-rs is an independent, clean-room-style reimplementation. It was reverse engineered from
[Paper](https://paper.design) by studying the behavior of the Paper desktop app with
[REA](https://github.com/morluto/rea) and Claude Code. It is not affiliated with or endorsed by
Paper. No original Paper code or assets are included in this repository.

## Projects

paper-rs opens on a **home screen** of your projects (recents grid with thumbnails, New, Import,
and Rename / Duplicate / Move to Trash from each card's ⋯ menu or right-click; restore from
Trash). Each project is a plain folder in `~/Documents/paper-rs/<name>/` holding `project.json`
and one JSON file per page in `pages/`. Work is autosaved about a second after each change with
crash-safe atomic writes; files are versioned and older versions migrate on load. Set
`PAPER_RS_HOME` to use another folder.

A project has any number of **pages** (Pages list at the top of the left sidebar: + to add,
double-click to rename, right-click to duplicate or delete, drag to reorder). Each page is its
own infinite canvas with its own undo history and view. Copy and paste works between pages and
between projects.

## Build and run

You need Rust 1.95 or newer (`rust-version` in `Cargo.toml`; the latest stable is recommended). Install it with [rustup](https://rustup.rs).

```sh
cargo build --release
./target/release/paper-rs                  # projects home screen
./target/release/paper-rs --demo           # sample planning board (not saved)
./target/release/paper-rs ~/Documents/paper-rs/Plan   # open a project folder
./target/release/paper-rs board.paper-rs   # open a paper-rs 0.1 document
```

### macOS

```sh
xcode-select --install          # command line tools (linker)
curl https://sh.rustup.rs -sSf | sh
cargo build --release && ./target/release/paper-rs --demo
```

It uses Metal through wgpu. Shortcuts use ⌘, and trackpad pinch zooms. The macOS target is
type-checked in CI-style (`cargo check --target aarch64-apple-darwin`) but has not been run on
Mac hardware yet (see Known gaps).

### Linux

```sh
# Debian/Ubuntu runtime libraries (usually already present on desktops)
sudo apt install libxkbcommon-x11-0 libxcb-xkb1 libvulkan1 mesa-vulkan-drivers
cargo build --release && ./target/release/paper-rs --demo
```

It runs on X11 and Wayland through Vulkan, or GL as a fallback. Shortcuts use Ctrl. File
dialogs use the XDG desktop portal.

### Windows

Install Rust with [rustup](https://rustup.rs) and the
[Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) with the
"Desktop development with C++" workload (the linker). On Windows on ARM, use the ARM64 rustup
(`rustc -vV` shows `host: aarch64-pc-windows-msvc`) and also select the MSVC ARM64 build tools,
which that toolchain needs to link. An x64 toolchain also works, but it builds an x64 binary
that runs under emulation.

```powershell
cargo build --release
.\target\release\paper-rs.exe --demo
```

It uses Vulkan or DirectX 12 through wgpu. Shortcuts use Ctrl, and file dialogs are the native
Windows ones. Projects go in your Documents folder (OneDrive's, if Documents is redirected
there). It has been run natively on Windows 11 ARM64; the x64 build has been run only under
x64 emulation on that machine (see Known gaps).

### Headless (servers, CI)

No display is needed. wgpu renders offscreen, through Mesa lavapipe when there is no GPU:

```sh
paper-rs --screenshot ui.png --demo --size 1440x900 --scale 2      # full UI
paper-rs --screenshot home.png --home                              # projects home screen
paper-rs --export-png out.png PROJECT_DIR --page 0 --scale 2 [--transparent]
paper-rs --export-svg out.svg PROJECT_DIR
paper-rs --export-pdf out.pdf PROJECT_DIR --frames                 # one PDF page per frame
paper-rs --export out_dir PROJECT_DIR --format png --scope frames  # a folder of PNGs
paper-rs --import board.excalidraw                                 # new project, prints its folder
```

## AI agents (MCP)

![A sign-in screen written by an agent through write_html](docs/screenshots/milestone7_mcp.png)

paper-rs serves the [Model Context Protocol](https://modelcontextprotocol.io) while it runs, so
agents such as Claude Code and Codex can read and design on the open canvas, the way Paper's
own MCP server works (behaviour notes: `analysis/paper-mcp.md`). Every tool call runs through
the editor like a user edit: one undo step per call, and the window repaints immediately.

* **Streamable HTTP** (built into the app): `http://127.0.0.1:29980/mcp`. Paper uses 29979, so
  both can run. Loopback only; requests from browsers (with an `Origin` header) and foreign
  `Host` headers are rejected. `PAPER_RS_MCP_PORT=NNNN` changes the port; `PAPER_RS_MCP=0`
  turns the server off.
* **stdio**: `paper-rs-mcp` (built next to `paper-rs`; `paper-rs-mcp.exe` on Windows) relays
  stdio to the running app. It lists the tools even when the app is closed, and tells the
  agent to start paper-rs if a call can't reach it.

### Claude Code

```sh
# HTTP (the app must be running)
claude mcp add --transport http --scope user paper-rs http://127.0.0.1:29980/mcp
# or stdio through the bridge
claude mcp add --scope user paper-rs -- /path/to/paper-rs/target/release/paper-rs-mcp
```

### Codex

```sh
# HTTP
codex mcp add paper-rs --url http://127.0.0.1:29980/mcp
# or stdio
codex mcp add paper-rs -- /path/to/paper-rs/target/release/paper-rs-mcp
```

which writes this to `~/.codex/config.toml` (either form works):

```toml
[mcp_servers.paper-rs]
url = "http://127.0.0.1:29980/mcp"
# command = "/path/to/paper-rs/target/release/paper-rs-mcp"
```

Then ask the agent something like *"In paper-rs, create a 390×844 mobile artboard and design a
sign-in screen"*, or *"draw a flowchart of our deploy process on a new page, hand-drawn
style"*.

### Tools

Names and arguments follow Paper's MCP server where paper-rs has an equivalent, so prompts
written for Paper work here too.

| Group | Tools |
|---|---|
| Projects & pages | `list_resources`, `open_file`, `create_file`, `create_page`, `rename_pages` |
| Read | `get_basic_info`, `get_selection`, `get_node_info`, `get_children`, `get_tree_summary`, `get_computed_styles`, `get_jsx`, `find_nodes`, `get_fill_image`, `get_font_family_info`, `get_guide` |
| See | `get_screenshot` (PNG image content of a node or the page), `export` (PNG/SVG/PDF files), `export_combined_pdf` |
| Write | `create_artboard`, `write_html`, `update_styles`, `set_text_content`, `rename_nodes`, `duplicate_nodes`, `move_nodes`, `delete_nodes`, `finish_working_on_nodes` (saves) |
| paper-rs extras | `create_shape` (rectangles, ellipses, diamonds, stars, stickies, text, lines, arrows bound to shapes), `select_nodes`, `undo`, `redo` |

`write_html` takes HTML with inline styles: flexbox (`display:flex`, direction, gap, padding,
alignment, `flex-grow`, wrap) and block stacking map onto auto layout with CSS's stretch rules;
text gets size, weight, italic, color, line height, letter spacing, alignment and rich-text
runs (`<b>`, `<i>`, `<span style>`); boxes get backgrounds (colors, gradients, images),
borders, radii, opacity and blend modes; `<img>` takes local paths or `data:` URIs and `<svg>`
is imported as vectors. `update_styles` takes the same CSS as JSON, plus the hand-drawn keys
`sketch`, `roughness`, `fillStyle`, `strokeStyle`. Node ids look like `3fa9c1:14` (page and
node); `pageId` lets an agent work on another page without switching the user's view.

## Tests

```sh
cargo test                                # ~140 tests: unit, scenario, headless GPU, CLI
PAPER_RS_REQUIRE_GPU=1 cargo test         # fail (instead of skip) when no GPU adapter exists
cargo test --release --test perf -- --nocapture   # 10k-element frame timings
```

* **Unit tests** cover geometry, vector paths, the rough.js port (compared with real
  rough.js 4.6.4 output), auto layout and constraints, booleans, rich text spans, the elbow
  router, formats and migrations, atomic writes, the project store, PDF structure, Excalidraw
  and SVG import, the palette's fuzzy matching and the shortcut table.
* **Scenario tests** drive the editor with synthetic pointer and keyboard events:
  `src/editor/tests.rs` (core editing), `tests/projects.rs` (projects, autosave, pages),
  `tests/figma.rs` (groups, components, auto layout, constraints, pen, booleans, rich text,
  guides), `tests/excalidraw.rs` (hand-drawn style, arrows, quick-connect, stickies, laser,
  library), `tests/workflow.rs` (layers drag, palette, shortcuts), `tests/io.rs` (export,
  import, clipboard, CLI), `tests/mcp.rs` (every MCP tool on a headless session, then an rmcp
  client over HTTP and through the `paper-rs-mcp` stdio bridge).
* **Headless GPU tests** render through wgpu, assert pixels and write the screenshots below.
  One rasterizes the SVG export with resvg and compares it with the GPU render; one renders
  the PDF export with pdftoppm when installed.

| Milestone | Screenshot(s) in `docs/screenshots/` |
|---|---|
| 1. Local projects | `milestone1_home.png`, `milestone1_trash.png` |
| 2. Pages | `milestone2_pages.png` |
| 3. Figma core | `milestone3_figma_core.png`, `milestone3_canvas.png` |
| 4. Excalidraw core | `milestone4_whiteboard.png` |
| 5. Inspector, palette, shortcuts | `milestone5_inspector.png`, `milestone5_palette.png`, `milestone5_shortcuts.png` |
| 6. Import / export | `milestone6_pdf_page.png` (PDF rasterized by pdftoppm), `milestone6_excalidraw_import.png`, `milestone6_svg_import.png` |
| 7. MCP server | `milestone7_mcp.png` (a screen written through `write_html`, captured with `get_screenshot`), `milestone7_mcp_live_xvfb.png` (the live app after Claude Code drew into it over the stdio bridge), `milestone7_mcp_live_windows_arm64.png` (the same on Windows 11 ARM64, over HTTP and the bridge) |
| paper-rs 0.1 | `m1_*` … `m6_*`, `app_demo.png`, `x11_window_xvfb.png` |

## Using it

![Whiteboard features](docs/screenshots/milestone4_whiteboard.png)

The tools sit in the floating bar at the bottom of the canvas. Select a layer to edit it in
the **Design** panel on the right. The **Layers** panel on the left shows the tree: click to
select, Shift-click to add, double-click to rename, and use 👁 / 🔒 to hide or lock. The
**Up/Down** buttons reorder.

* Draw a frame around existing shapes and it adopts them. Shapes drawn inside a frame become
  its children, and dragging a layer into or out of a frame re-parents it.
* Top-level frames that contain content act as canvas regions. Drag from their body to
  marquee-select inside them, and drag the frame by its name label.
* The pencil keeps a constant on-screen width at any zoom. With nothing selected, set width,
  smoothing and color in the Design panel.
* Hold ⌘/Ctrl while dragging to suspend snapping.

### Keyboard shortcuts

⌘ on macOS, Ctrl on Linux and Windows. Press `?` or ⌘/ in the app for the full grouped list
(100+ bindings), or ⌘K to search every command.

| Keys | Action | Keys | Action |
|---|---|---|---|
| V / 1 | Select | ⌘K | Command palette |
| H, or hold Space | Hand (pan) | ⌘Z / ⌘⇧Z | Undo / redo |
| F or A | Frame | ⌘G / ⌘⇧G | Group / ungroup |
| R / 2, O / 4, D / 3 | Rectangle, ellipse, diamond | ⌘⌥G | Frame selection |
| ⇧Y, ⇧S | Polygon, star | ⌘⌥K / ⌘⌥B | Create component / detach |
| L / 6, ⇧L / 5 | Line, arrow | ⇧A / ⌥⇧A | Add / remove auto layout |
| P, ⇧P / 7 | Pen, pencil | ⌥A ⌥H ⌥D / ⌥W ⌥V ⌥S | Align left/center/right, top/middle/bottom |
| T / 8, S | Text, sticky note | ⌥⇧H / ⌥⇧V | Distribute spacing |
| K | Laser pointer | ⌥⇧U S I E | Union, subtract, intersect, exclude |
| E / 0 | Eraser | ⌘E | Flatten |
| 9, ⌘⇧K | Place image | ⌘B / ⌘I / ⌘U / ⌘⇧X | Bold, italic, underline, strikethrough |
| Enter / ⇧Enter | Edit (text, path) / select parent | ⌘⌥H | Hand-drawn style |
| Tab / ⇧Tab | Next / previous sibling | ⇧H / ⇧V | Flip |
| ⌥-drag | Duplicate while dragging | ⌘⌥C / ⌘⌥V | Copy / paste properties |
| ⌥ hover | Measure distances | ⌘R | Rename |
| ⌘= / ⌘- / ⇧0 | Zoom in / out / 100% | ⇧1 / ⇧2 | Zoom to fit / selection |
| PageUp / PageDown | Previous / next page | ⌘\\ | Show / hide UI |

## Feature parity: Paper / Figma / Excalidraw vs paper-rs

Columns 2–4 say whether each product has the feature (✓) and where the behavior was learned:
Paper from its desktop shell and public docs (`analysis/PAPER_ARCHITECTURE.md`), Figma from
public docs (`analysis/FIGMA_NOTES.md`), Excalidraw from its MIT source
(`analysis/EXCALIDRAW_NOTES.md`). The last column is paper-rs: ✅ done, ⚠️ partial (see
Known gaps), ⏳ planned in a later milestone, ❌ not planned.

| Feature | Paper | Figma | Excalidraw | paper-rs |
|---|---|---|---|---|
| Local projects, recents grid, trash | (cloud dashboard) | ✓ (cloud) | (single file) | ✅ Folders in `~/Documents/paper-rs`, thumbnails, rename/duplicate/trash/restore |
| Autosave, crash-safe, versioned files | ✓ (cloud) | ✓ (cloud) | ✓ (browser storage) | ✅ 1 s autosave, atomic writes, migration chain (page v1→v3) |
| Pages, infinite canvas per page | ✓ | ✓ | – | ✅ Per-page history and camera, reorder/duplicate/delete |
| Frames with clipping | ✓ | ✓ | ✓ | ✅ ⚠️ clip is rectangular (corner radius not clipped) |
| Groups | ✓ | ✓ | ✓ | ✅ Bounds follow children, click selects group, double-click enters, scale on resize |
| Components, instances, overrides | – | ✓ | (library only) | ✅ Live sync, per-property overrides, reset, detach, go to main |
| Auto layout (direction, gap, padding, hug/fill/fixed, wrap) | ✓ (flexbox) | ✓ | – | ✅ Drag-reorder in flow, space-between, absolute children |
| Constraints | – | ✓ | – | ✅ Left/right/stretch/center/scale per axis, ⌘ to ignore |
| Pen tool, vector point editing | ✓ | ✓ | – | ✅ Corner/smooth points, handles (⌥ breaks), close, delete, toggle |
| Boolean ops (union/subtract/intersect/exclude), flatten | (roadmap) | ✓ | – | ✅ ⚠️ destructive (result is a path, curves flattened) |
| Gradients (linear, radial) | ✓ | ✓ | – | ✅ Multi-stop, angle, radius; ⚠️ no on-canvas gradient handles |
| Image fills | ✓ | ✓ | ✓ (image element) | ✅ Fill/fit/stretch/tile, place image (⌘⇧K), drag-and-drop, stored in project |
| Strokes: width, align, dash, caps, joins | ✓ | ✓ | ✓ | ✅ |
| Corner radius (per corner), polygons, stars | ✓ | ✓ | – | ✅ |
| Opacity, blend modes | ✓ | ✓ | ✓ (opacity) | ✅ ⚠️ canvas renders 5 modes exactly, all 16 exact in SVG |
| Rich text (per-range bold/italic/size/colour) | ✓ | ✓ | – | ✅ Plus underline, strikethrough, line height, letter spacing |
| Align / distribute | ✓ | ✓ | ✓ | ✅ ⌥A/D/W/S/H/V, ⌥⇧H/V |
| Smart guides, spacing, measurement | ✓ | ✓ | – | ✅ Edge/center snap, equal spacing, ⌥-hover distances |
| Hand-drawn (rough) style per element | – | – | ✓ | ✅ rough.js 4.6.4 port, verified against rough.js output; sloppiness, fill styles |
| Bound arrows, elbow arrows, quick shapes, sticky notes | – | (FigJam) | ✓ | ✅ Orbit binding that follows shapes, straight/curved/elbow, 5 heads, diamond, quick-connect, stickies |
| Laser pointer, shape library | – | (FigJam) | ✓ | ✅ Fading laser trail; shared library with starter items |
| Inspector, layers drag-reorder, command palette (⌘K) | ✓ | ✓ | ✓ | ✅ Sectioned inspector, tree drag above/below/into, fuzzy palette over actions, pages, layers, library |
| Export PNG / SVG / PDF of selection, frame, page | ✓ | ✓ | ✓ (PNG/SVG) | ✅ Selection, each frame, page or all pages; vector PDF with gradients, images, blend modes; copy as PNG/SVG |
| Import .excalidraw / SVG, copy-paste across projects | – | ✓ (SVG) | ✓ | ✅ .excalidraw scenes, .excalidrawlib libraries, SVG (paths, gradients, images, text), system clipboard between pages, projects and apps |
| Freehand ink with pressure | – | (pencil) | ✓ | ✅ Smoothing, pressure, eraser |
| MCP server for AI agents | ✓ | ✓ (Dev Mode) | ✓ (community) | ✅ Streamable HTTP in the app + stdio bridge; Paper-compatible tool names, `write_html`, screenshots, whiteboard shapes |
| Multiplayer, cloud sync, plugins | ✓ | ✓ | ✓ | ❌ local-first by design |

## Known gaps

* **Paper's document model is HTML/CSS.** paper-rs uses geometric nodes with flexbox-like auto
  layout on frames; there are no variables, CSS export or code generation.
* **Rendering approximations.** Frame clipping is a rectangle (scissor), so rounded frames
  don't clip their corners. Group opacity multiplies into children instead of compositing the
  group. Overlay, color dodge/burn, hard/soft light, difference, exclusion, hue, saturation,
  color and luminosity blend as Normal on the canvas (they are exact in SVG export).
* **Booleans are destructive**: the result is a new vector path with flattened curves.
  Gradients are edited in the inspector only.
* **Not run on macOS hardware yet.** The macOS build type-checks and uses only portable APIs
  (Metal via wgpu, winit, rfd), but it has not been launched on a Mac. On Linux, the real
  window was exercised under Xvfb; the screenshot is `x11_window_xvfb.png`.
* **Windows** has been run on one machine: Windows 11 ARM64 (Snapdragon X Elite, Adreno
  X1-85), natively. The x64 build was run there under x64 emulation, not on x64 hardware.
  `paper-rs.exe` is a console program, so starting it from Explorer also opens a console
  window (which carries the log and the CLI output).
* **Pen pressure.** winit 0.30 does not expose tablet pressure for pen events that arrive as
  mouse input, which is common with macOS and Wacom tablets. Pressure comes from touch/pen
  `force` when the platform reports it. Otherwise it is simulated from stroke velocity.
* **Text** uses Inter (regular, bold, italic, bold italic) with no shaping, so there are no
  ligatures, complex scripts or RTL, and no font picker. IME preedit is not displayed
  (committed text works).
* **No rotation.** Nodes are axis-aligned boxes; rotated imports are baked into paths.
* **Undo granularity.** Document renames from the Design panel are not undoable, and the
  selection is not part of history.
* **Performance.** Nodes are tessellated once per zoom octave and cached; unchanged frames
  reuse the previous item list and GPU buffers, so panning uploads nothing. With 10k visible
  nodes a cached frame takes ~3 ms (release, `tests/perf.rs`); the first frame at a new zoom
  octave takes ~140 ms on a CPU. Not tuned for 100k+.
* **PDF**: text is outlined (not selectable); gradient stop transparency uses the first
  stop's alpha; tiled image fills are drawn as repeated images.
* **Import**: Excalidraw rotation is baked into paths (text rotation is dropped), bound text
  inside shapes stays a separate text layer (except sticky notes), and fonts map to Inter.
  SVG patterns, filters, masks and clip paths are not imported.
* **MCP**: one project is open at a time, so `fileId` must name the open project (call
  `open_file` to switch; the user's window switches too). No design tokens, comments or AI
  image generation (Paper has them). Moving or duplicating layers between pages isn't
  supported. Remote `http(s)` images aren't fetched; pass a local path or a `data:` URI.
  CSS margin, grid, shadows and transforms are reported back as ignored styles.
* **Linux file dialogs** need an XDG desktop portal. Without one, Save/Open dialogs return
  nothing. The headless CLI is unaffected.

## Project layout

```
src/geom.rs        vectors, rects, camera
src/vector.rs      bezier paths: flatten, hit test, SVG data, shapes, dashes
src/model/         Node / NodeKind / paints / Document arena
src/history.rs     transactions, undo/redo
src/layout.rs      auto layout, constraints, group bounds
src/ink.rs         stroke smoothing, pressure, outline
src/text.rs        rich text layout and glyph outlines
src/assets.rs      image registry (content-hashed)
src/project/       project store, manifest/page formats, migrations
src/session.rs     open project, autosave, thumbnails
src/editor/        tools, structure (groups/components), vector ops, pages, text editing
src/render/        draw ops → cached meshes → wgpu; headless capture
src/io/            PNG, SVG, PDF, export scopes, Excalidraw + SVG import, clipboard,
                   atomic writes
src/connector.rs   arrow bindings, elbow router, arrowheads
src/library.rs     shared shape library
src/ui/            home screen, pages, layers, inspector
src/app.rs         winit application
src/mcp/           MCP server (rmcp): tools, HTML/CSS mapping, HTTP service
src/bin/paper-rs-mcp.rs  stdio bridge to the running app
analysis/          reverse-engineering findings and design (raw dumps are gitignored)
docs/screenshots/  headless renders produced by `cargo test`
```

## License

MIT OR Apache-2.0 for the code in this repository. Inter (Regular, Medium, SemiBold) is
© The Inter Project Authors under the SIL Open Font License 1.1; see `assets/fonts/OFL.txt`.
