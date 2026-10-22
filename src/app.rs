//! The windowed application: winit event loop, wgpu surface, egui integration, file dialogs.

use crate::editor::{AppRequest, Button, Editor, InputEvent, Key, Modifiers};
use crate::geom::{V2, v2};
use crate::io::{file, png};
use crate::project::Store;
use crate::render::frame::{self, Painter};
use crate::render::gpu::Headless;
use crate::session::Session;
use crate::theme;
use crate::ui::home::HomeAction;
use crate::ui::{self, UiState};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key as WKey, NamedKey};
use winit::window::{Window, WindowId};

struct Gfx {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    painter: Painter,
    egui: egui_winit::State,
    adapter_info: wgpu::AdapterInfo,
}

pub struct App {
    gfx: Option<Gfx>,
    pub ed: Editor,
    session: Session,
    st: UiState,
    ctx: egui::Context,
    start: Instant,
    canvas: egui::Rect,
    captured: bool,
    title: String,
    /// When egui asked for a delayed repaint (tooltips, caret blink, animations).
    next_repaint: Option<Instant>,
    #[cfg(target_os = "macos")]
    menu: Option<crate::macos::NativeMenu>,
    /// Tool calls from the MCP server, and the server itself (stops when dropped).
    mcp: Option<(
        std::sync::mpsc::Receiver<crate::mcp::Call>,
        crate::mcp::server::Running,
    )>,
}

/// Events delivered to the winit loop from other sources.
#[derive(Debug)]
pub enum UserEvent {
    #[cfg(target_os = "macos")]
    Menu(muda::MenuId),
    /// MCP tool calls are waiting in `App::mcp`.
    Mcp,
}

/// What to show at startup.
pub enum Start {
    /// The projects home screen.
    Home,
    /// Open this project folder.
    Project(PathBuf),
    /// A scratch editor that is not backed by a project (demo, tests).
    Scratch(Box<Editor>),
}

impl App {
    pub fn new(start: Start, store: Store) -> App {
        let ctx = egui::Context::default();
        theme::set_accent(theme::system_accent());
        ui::setup_style(&ctx);
        let options = crate::options::Options::load(&store.root);
        theme::set_preference(&ctx, options.theme);
        let library = crate::library::Library::load(&crate::library::Library::path_in(&store.root));
        let mut app = App {
            gfx: None,
            ed: Editor::default(),
            session: Session::new(store),
            st: UiState {
                options,
                ..UiState::default()
            },
            ctx,
            start: Instant::now(),
            canvas: egui::Rect::NOTHING,
            captured: false,
            title: String::new(),
            next_repaint: None,
            #[cfg(target_os = "macos")]
            menu: None,
            mcp: None,
        };
        app.st.library.library = library;
        app.ed.clipboard_io = crate::io::clipboard::System::new()
            .map(|c| Box::new(c) as Box<dyn crate::io::clipboard::Provider>);
        match start {
            Start::Home => app.show_home(),
            Start::Project(dir) => {
                if let Err(e) = app.session.open(&mut app.ed, &dir) {
                    app.error(format!("Could not open {}: {e}", dir.display()));
                    app.show_home();
                } else {
                    app.st.project_name = app.session.project_name();
                }
            }
            Start::Scratch(ed) => app.ed = *ed,
        }
        app
    }

    fn now(&self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1000.0
    }

    fn to_canvas(&self, p: V2) -> V2 {
        p - v2(self.canvas.min.x as f64, self.canvas.min.y as f64)
    }

    fn over_canvas(&self, p: V2) -> bool {
        self.canvas.contains(egui::pos2(p.x as f32, p.y as f32)) && !self.ctx.is_pointer_over_egui()
    }

    /// Before leaving a scratch (project-less) document. Projects autosave, so they never ask.
    fn confirm_discard(&mut self) -> bool {
        if self.session.project.is_some() || self.st.home.is_some() || !self.ed.is_dirty() {
            return true;
        }
        let r = rfd::MessageDialog::new()
            .set_title("Unsaved changes")
            .set_description(format!("Save changes to \"{}\" first?", self.ed.doc.name))
            .set_buttons(rfd::MessageButtons::YesNoCancel)
            .show();
        match r {
            rfd::MessageDialogResult::Yes => self.save(false),
            rfd::MessageDialogResult::No => true,
            _ => false,
        }
    }

    fn error(&mut self, msg: String) {
        log::error!("{msg}");
        self.ed.status = msg.clone();
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("Sketch")
            .set_description(msg)
            .show();
    }

    /// Save a scratch document to a legacy file (only used when no project is open).
    fn save(&mut self, _save_as: bool) -> bool {
        if self.session.project.is_some() {
            return self.save_project(true);
        }
        let Some(p) = rfd::FileDialog::new()
            .add_filter("Sketch document", &[file::EXTENSION])
            .set_file_name(format!("{}.{}", self.ed.doc.name, file::EXTENSION))
            .save_file()
        else {
            return false;
        };
        let path = with_ext(p, file::EXTENSION);
        match file::save(&path, &self.ed.doc, Some(&self.ed.camera)) {
            Ok(()) => {
                self.ed.mark_saved();
                self.ed.status = format!("Saved {}", path.display());
                self.ed.file_path = Some(path);
                true
            }
            Err(e) => {
                self.error(format!("Could not save: {e}"));
                false
            }
        }
    }

    /// A GPU handle for offscreen work (thumbnails, PNG export) on the window's device.
    fn offscreen(&self) -> Option<Headless> {
        let g = self.gfx.as_ref()?;
        Some(Headless {
            device: g.device.clone(),
            queue: g.queue.clone(),
            adapter_info: g.adapter_info.clone(),
        })
    }

    /// Save the open project; `full` also writes the camera and refreshes the thumbnail.
    fn save_project(&mut self, full: bool) -> bool {
        self.st.save_state = String::new();
        let r = if full {
            self.session.save_all(&mut self.ed)
        } else {
            self.session.save(&mut self.ed).map(|_| ())
        };
        if let Err(e) = r {
            self.st.save_state = "Not saved".into();
            self.session.defer();
            self.error(format!("Could not save project: {e}"));
            return false;
        }
        self.st.save_state = "Saved".into();
        if self.session.thumbnail_due(Instant::now(), full)
            && let Some(gpu) = self.offscreen()
            && let Err(e) = self.session.write_thumbnail(&gpu, &self.ed)
        {
            log::warn!("thumbnail: {e}");
        }
        true
    }

    /// Autosave when due; returns whether it tried. `about_to_wait` calls this rather than
    /// the frame code, because an occluded window draws no frames and MCP edits made while it
    /// sits behind other windows still have to reach the disk.
    fn tick_autosave(&mut self) -> bool {
        let due = self.session.autosave_due(&self.ed, Instant::now());
        if due {
            self.save_project(false);
        }
        due
    }

    fn show_home(&mut self) {
        let mut hv = self.st.home.take().unwrap_or_default();
        hv.refresh(&self.session.store, crate::project::now_ms());
        self.st.home = Some(hv);
        self.st.project_name.clear();
        self.st.save_state.clear();
    }

    fn open_project(&mut self, dir: &std::path::Path) {
        if self.session.project.is_some() {
            self.save_project(true);
        }
        match self.session.open(&mut self.ed, dir) {
            Ok(()) => {
                self.st.home = None;
                self.st.project_name = self.session.project_name();
                self.st.save_state.clear();
                self.ed.status.clear();
            }
            Err(e) => self.error(format!("Could not open {}: {e}", dir.display())),
        }
    }

    fn new_project(&mut self) {
        if self.session.project.is_some() {
            self.save_project(true);
        }
        match self.session.create(&mut self.ed, "Untitled") {
            Ok(()) => {
                self.st.home = None;
                self.st.project_name = self.session.project_name();
                self.st.save_state.clear();
            }
            Err(e) => self.error(format!("Could not create a project: {e}")),
        }
    }

    fn go_home(&mut self) {
        if self.session.project.is_some() {
            self.save_project(true);
            if let Err(e) = self.session.close(&mut self.ed) {
                self.error(format!("Could not save project: {e}"));
            }
        } else if !self.confirm_discard() {
            return;
        }
        self.show_home();
    }

    /// Import a file into a new project and open it.
    fn import_dialog(&mut self) {
        let Some(p) = rfd::FileDialog::new()
            .add_filter(
                "Supported files",
                &[file::EXTENSION, file::OLD_EXTENSION, "excalidraw", "svg"],
            )
            .add_filter(
                "Sketch 0.1 document",
                &[file::EXTENSION, file::OLD_EXTENSION],
            )
            .add_filter("Excalidraw scene", &["excalidraw"])
            .add_filter("SVG", &["svg"])
            .pick_file()
        else {
            return;
        };
        self.import_file(&p);
    }

    fn import_file(&mut self, p: &std::path::Path) {
        match crate::io::import::import_as_pages(p) {
            Ok((name, pages)) => match self.session.store.create_with_pages(&name, pages) {
                Ok(proj) => {
                    let dir = proj.dir.clone();
                    self.open_project(&dir);
                    self.ed.status = format!("Imported {}", p.display());
                }
                Err(e) => self.error(format!("Could not create a project: {e}")),
            },
            Err(e) => self.error(format!("Could not import {}: {e}", p.display())),
        }
    }

    fn handle_home_actions(&mut self) {
        let Some(hv) = &mut self.st.home else { return };
        let actions = std::mem::take(&mut hv.actions);
        if actions.is_empty() {
            return;
        }
        let store = self.session.store.clone();
        for a in actions {
            let r: Result<(), String> = match a {
                HomeAction::New => {
                    self.new_project();
                    Ok(())
                }
                HomeAction::Import => {
                    self.import_dialog();
                    Ok(())
                }
                HomeAction::Open(dir) => {
                    self.open_project(&dir);
                    Ok(())
                }
                HomeAction::Rename(dir, name) => store
                    .rename(&dir, &name)
                    .map(|_| ())
                    .map_err(|e| e.to_string()),
                HomeAction::Duplicate(dir) => {
                    store.duplicate(&dir).map(|_| ()).map_err(|e| e.to_string())
                }
                HomeAction::Trash(dir) => store.trash(&dir).map(|_| ()).map_err(|e| e.to_string()),
                HomeAction::Restore(dir) => {
                    store.restore(&dir).map(|_| ()).map_err(|e| e.to_string())
                }
                HomeAction::DeleteForever(dir) => {
                    store.delete_forever(&dir).map_err(|e| e.to_string())
                }
                HomeAction::EmptyTrash => store.empty_trash().map_err(|e| e.to_string()),
            };
            if let Err(e) = r {
                self.error(e);
            }
        }
        if self.st.home.is_some() {
            self.show_home();
        }
    }

    fn place_image_file(&mut self, p: &std::path::Path, at: V2) {
        match std::fs::read(p)
            .map_err(|e| e.to_string())
            .and_then(|b| crate::assets::register(&b))
        {
            Ok(id) => {
                self.ed.insert_image(&id, at);
                self.ed.status = format!("Placed {}", p.display());
            }
            Err(e) => self.error(format!("Could not place {}: {e}", p.display())),
        }
    }

    /// Export through a save (or folder) dialog.
    fn export(&mut self, spec: crate::io::export::Spec) {
        use crate::io::export;
        let names: Vec<String> = self.ed.pages.iter().map(|p| p.name.clone()).collect();
        let pages: Vec<(&str, &crate::model::Document)> = names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.as_str(), self.ed.page_doc(i)))
            .collect();
        let sel = self.ed.top_selection();
        let items = export::items(&spec, &pages, self.ed.active_page, &sel);
        if items.is_empty() {
            self.ed.status = "Nothing to export".into();
            return;
        }
        let ext = spec.format.ext();
        let out = if spec.single_file(items.len()) {
            let base = if items.len() == 1 {
                items[0].name.clone()
            } else {
                self.st.project_name.clone()
            };
            rfd::FileDialog::new()
                .add_filter(ext.to_uppercase(), &[ext])
                .set_file_name(format!("{}.{ext}", crate::project::folder_name(&base)))
                .save_file()
                .map(|p| with_ext(p, ext))
        } else {
            rfd::FileDialog::new()
                .set_title(format!(
                    "Choose a folder for {} {} files",
                    items.len(),
                    ext.to_uppercase()
                ))
                .pick_folder()
        };
        let Some(out) = out else { return };
        let gpu = self.offscreen();
        match export::write(&spec, &items, &out, gpu.as_ref()) {
            Ok(files) => {
                self.ed.status = if files.len() == 1 {
                    format!("Exported {}", files[0].display())
                } else {
                    format!("Exported {} files to {}", files.len(), out.display())
                }
            }
            Err(e) => self.error(format!("Export failed: {e}")),
        }
    }

    /// Bring a file into the current page: SVG, Excalidraw scene/library, image, or a
    /// Sketch page. Without an open project, a new project is created from it.
    fn import_into_page(&mut self, p: &std::path::Path, at: Option<V2>) {
        let ext = p
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let at = at.unwrap_or_else(|| self.ed.camera.screen_to_world(self.ed.viewport * 0.5));
        let text = || std::fs::read_to_string(p).map_err(|e| e.to_string());
        let result: Result<String, String> = match ext.as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "webp" => {
                self.place_image_file(p, at);
                return;
            }
            "excalidrawlib" => text()
                .and_then(|t| crate::io::excalidraw::import_library(&t))
                .map(|items| {
                    let n = items.len();
                    self.st.library.library.items.extend(items);
                    self.st.library.dirty = true;
                    self.st.show_library = true;
                    format!("Added {n} item(s) to the library")
                }),
            "svg" | "excalidraw" | "json" => text().and_then(|t| {
                let nodes: Vec<crate::model::Node> = if ext == "svg" {
                    crate::io::svg_import::parse(&t)?
                } else {
                    let doc = crate::io::excalidraw::import_scene(&t)?;
                    doc.paint_order()
                        .into_iter()
                        .filter_map(|id| doc.get(id).cloned())
                        .collect()
                };
                let (roots, world) = crate::io::clipboard::roots_of(&nodes);
                let off = crate::io::clipboard::center_offset(&world, at);
                let label = format!(
                    "Import {}",
                    p.file_name()
                        .map(|f| f.to_string_lossy().to_string())
                        .unwrap_or_default()
                );
                let ids = if ext == "svg" {
                    // Keep an SVG together as one group.
                    let name = p
                        .file_stem()
                        .map(|f| f.to_string_lossy().to_string())
                        .unwrap_or("SVG".into());
                    let b = crate::io::clipboard::bounds(&world).unwrap_or_default();
                    vec![self.ed.edit(&label, |tx| {
                        crate::io::svg_import::insert_group(tx, nodes, &name, b.min() + off)
                    })]
                } else {
                    self.ed
                        .insert_subtrees(&label, &nodes, &roots, &world, off, false)
                };
                self.ed.selection = ids;
                Ok(format!("Imported {}", p.display()))
            }),
            _ => {
                if self.session.project.is_none() {
                    self.import_file(p);
                    return;
                }
                // A Sketch page or project: add its pages to this project.
                crate::io::import::import_as_pages(p).map(|(_, pages)| {
                    let n = pages.len();
                    for pg in pages {
                        let i = self.ed.add_page();
                        self.ed.rename_page(i, &pg.name);
                        let nodes: Vec<crate::model::Node> = pg
                            .doc
                            .paint_order()
                            .into_iter()
                            .filter_map(|id| pg.doc.get(id).cloned())
                            .collect();
                        let (roots, world) = crate::io::clipboard::roots_of(&nodes);
                        self.ed.insert_subtrees(
                            "Import page",
                            &nodes,
                            &roots,
                            &world,
                            V2::ZERO,
                            false,
                        );
                    }
                    format!("Imported {n} page(s)")
                })
            }
        };
        match result {
            Ok(msg) => self.ed.status = msg,
            Err(e) => self.error(format!("Could not import {}: {e}", p.display())),
        }
    }

    fn export_roots(&self) -> Option<Vec<crate::model::NodeId>> {
        let sel = self.ed.top_selection();
        (!sel.is_empty()).then_some(sel)
    }

    fn handle_requests(&mut self) {
        for req in std::mem::take(&mut self.ed.requests) {
            match req {
                AppRequest::Home => self.go_home(),
                AppRequest::ImageFill(ids, i) => {
                    let Some(p) = rfd::FileDialog::new()
                        .add_filter("Images", &["png", "jpg", "jpeg", "gif", "webp"])
                        .pick_file()
                    else {
                        continue;
                    };
                    match std::fs::read(&p)
                        .map_err(|e| e.to_string())
                        .and_then(|b| crate::assets::register(&b))
                    {
                        Ok(asset) => {
                            ui::inspector::apply(&mut self.ed, "fills", &ids, |n| {
                                let paint = crate::model::Paint::Image {
                                    asset: asset.clone(),
                                    mode: crate::model::ImageMode::Fill,
                                    opacity: 1.0,
                                };
                                match n.style.fills.get_mut(i) {
                                    Some(slot) => *slot = paint,
                                    None => n.style.fills.push(paint),
                                }
                            });
                        }
                        Err(e) => self.error(format!("Could not load {}: {e}", p.display())),
                    }
                }
                AppRequest::PlaceImage => {
                    let Some(p) = rfd::FileDialog::new()
                        .add_filter("Images", &["png", "jpg", "jpeg", "gif", "webp"])
                        .pick_file()
                    else {
                        continue;
                    };
                    let at = self.ed.camera.screen_to_world(self.ed.viewport * 0.5);
                    self.place_image_file(&p, at);
                }
                AppRequest::New => self.new_project(),
                AppRequest::Open => {
                    if self.session.project.is_some() {
                        if let Some(p) = rfd::FileDialog::new()
                            .add_filter(
                                "Supported files",
                                &[
                                    "svg",
                                    "excalidraw",
                                    "excalidrawlib",
                                    "json",
                                    "png",
                                    "jpg",
                                    "jpeg",
                                    "gif",
                                    "webp",
                                    file::EXTENSION,
                                    file::OLD_EXTENSION,
                                ],
                            )
                            .pick_file()
                        {
                            self.import_into_page(&p, None);
                        }
                    } else {
                        self.import_dialog();
                    }
                }
                AppRequest::Save => {
                    self.save(false);
                }
                AppRequest::SaveAs => {
                    if let Some(dir) = self.session.project.as_ref().map(|p| p.dir.clone()) {
                        self.save_project(true);
                        match self.session.store.duplicate(&dir) {
                            Ok(copy) => self.open_project(&copy),
                            Err(e) => self.error(format!("Could not duplicate: {e}")),
                        }
                    } else {
                        self.save(true);
                    }
                }
                AppRequest::ExportFormat(f) => {
                    let spec = crate::io::export::Spec {
                        format: f,
                        ..self.st.export
                    };
                    self.export(spec);
                }
                AppRequest::Export(spec) => self.export(spec),
                AppRequest::CopyAsPng => {
                    let roots = self.export_roots();
                    let Some(gpu) = self.offscreen() else {
                        continue;
                    };
                    match png::render(&gpu, &self.ed.doc, roots.as_deref(), 2.0, None) {
                        Ok(img) => {
                            if let Some(io) = &mut self.ed.clipboard_io {
                                io.write_image(&img);
                                self.ed.status =
                                    format!("Copied {}×{} PNG", img.width(), img.height());
                            }
                        }
                        Err(e) => self.ed.status = e,
                    }
                }
            }
        }
    }

    fn redraw(&mut self) {
        let now = self.now();
        self.ed.tick(now);
        let Some(g) = self.gfx.as_mut() else { return };
        let input = g.egui.take_egui_input(&g.window);
        let mut l = frame::layout(&self.ctx, input, &mut self.ed, &mut self.st);
        self.canvas = l.canvas;
        let repaint_delay = l
            .output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map_or(Duration::MAX, |v| v.repaint_delay);
        let platform = std::mem::take(&mut l.output.platform_output);
        g.egui.handle_platform_output(&g.window, platform);

        let frame = match g.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Occluded => {
                // macOS reports Occluded until the window is on screen (and while it is
                // hidden). Keep egui's texture state in sync, but don't spin:
                // WindowEvent::Occluded(false) triggers a redraw.
                log::debug!("surface occluded; skipping frame");
                g.painter
                    .skip_frame(&g.device, &g.queue, &mut l.output.textures_delta);
                return;
            }
            other => {
                log::warn!("surface unavailable: {other:?}");
                g.surface.configure(&g.device, &g.config);
                g.painter
                    .skip_frame(&g.device, &g.queue, &mut l.output.textures_delta);
                g.window.request_redraw();
                return;
            }
        };
        // AppKit re-lays out the titlebar (first show, fullscreen, resize) and puts the
        // traffic lights back; re-inset them (a no-op when already in place).
        #[cfg(target_os = "macos")]
        {
            let (x, y) = theme::TRAFFIC_LIGHTS;
            crate::macos::inset_traffic_lights(&g.window, x, y);
        }
        let view = frame.texture.create_view(&Default::default());
        let size = [g.config.width, g.config.height];
        g.painter.paint(
            &g.device,
            &g.queue,
            &view,
            size,
            &self.ctx,
            l,
            &self.ed,
            !self.st.show_grid_off,
        );
        g.window.pre_present_notify();
        g.queue.present(frame);

        let title = if self.st.home.is_some() {
            "Sketch".to_string()
        } else if self.session.project.is_some() {
            format!(
                "{} — {} — Sketch",
                self.st.project_name,
                self.ed.page_name()
            )
        } else {
            format!(
                "{}{} — Sketch",
                self.ed.doc.name,
                if self.ed.is_dirty() { " •" } else { "" }
            )
        };
        if title != self.title {
            g.window.set_title(&title);
            self.title = title;
        }
        let pending = !self.ed.requests.is_empty()
            || self.st.home.as_ref().is_some_and(|h| !h.actions.is_empty());
        self.handle_requests();
        self.handle_home_actions();
        if self.st.library.dirty {
            self.st.library.dirty = false;
            if let Err(e) = self.st.library.library.save() {
                self.error(format!("Could not save the library: {e}"));
            }
        }
        if self.st.options_dirty {
            self.st.options_dirty = false;
            if let Err(e) = self.st.options.save(&self.session.store.root) {
                self.error(format!("Could not save options: {e}"));
            }
        }
        self.sync_menu();
        // Repaint only when egui wants it: immediately (an animation is running or state just
        // changed), after a delay (tooltip, caret blink), or never (idle; input wakes us).
        let Some(g) = &self.gfx else { return };
        if log::log_enabled!(log::Level::Trace) {
            log::trace!(
                "frame: repaint in {repaint_delay:?}, causes {:?}",
                self.ctx.repaint_causes()
            );
        }
        // The laser trail fades out frame by frame, then the loop goes idle again.
        let animating = self.ed.animating(now);
        if repaint_delay.is_zero() || pending || animating || !self.ed.requests.is_empty() {
            g.window.request_redraw();
            self.next_repaint = None;
        } else if repaint_delay < Duration::from_secs(3600) {
            self.next_repaint = Some(Instant::now() + repaint_delay);
        } else {
            self.next_repaint = None;
        }
    }

    /// Keep the native menu's check marks in step with the editor.
    fn sync_menu(&mut self) {
        #[cfg(target_os = "macos")]
        if let Some(m) = &self.menu {
            let set = |item: &muda::CheckMenuItem, v: bool| {
                if item.is_checked() != v {
                    item.set_checked(v);
                }
            };
            set(&m.snapping, self.ed.snapping);
            set(&m.grid, !self.st.show_grid_off);
            set(&m.help, self.st.show_help);
        }
    }

    #[cfg(target_os = "macos")]
    fn menu_command(&mut self, event_loop: &ActiveEventLoop, cmd: crate::macos::MenuCmd) {
        use crate::editor::shortcuts::Action as A;
        use crate::macos::MenuCmd;
        match cmd {
            MenuCmd::Quit => {
                if self.confirm_discard() {
                    event_loop.exit();
                }
            }
            MenuCmd::CheckForUpdates => crate::macos::updater::check_for_updates(),
            MenuCmd::ToggleGrid => self.st.show_grid_off = !self.st.show_grid_off,
            MenuCmd::ToggleHelp => self.st.show_help = !self.st.show_help,
            // Menu key equivalents never reach egui, so while a text field is focused, hand
            // the standard editing commands to it instead of the canvas.
            MenuCmd::Run(a @ (A::Undo | A::Redo | A::Cut | A::Copy | A::Paste | A::SelectAll))
                if self.ctx.egui_wants_keyboard_input() =>
            {
                let Some(g) = self.gfx.as_mut() else { return };
                let key = |key, shift: bool| egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: if shift {
                        egui::Modifiers::COMMAND | egui::Modifiers::SHIFT
                    } else {
                        egui::Modifiers::COMMAND
                    },
                };
                let ev = match a {
                    A::Cut => egui::Event::Cut,
                    A::Copy => egui::Event::Copy,
                    A::Paste => match g.egui.clipboard_text() {
                        Some(t) => egui::Event::Paste(t),
                        None => return,
                    },
                    A::Undo => key(egui::Key::Z, false),
                    A::Redo => key(egui::Key::Z, true),
                    _ => key(egui::Key::A, false),
                };
                g.egui.egui_input_mut().events.push(ev);
            }
            MenuCmd::Run(a) => self.ed.run(a),
        }
        if let Some(g) = &self.gfx {
            g.window.request_redraw();
        }
    }
}

fn with_ext(p: PathBuf, ext: &str) -> PathBuf {
    if p.extension().is_some_and(|e| e == ext) {
        p
    } else {
        p.with_extension(ext)
    }
}

fn map_key(k: &WKey) -> Option<Key> {
    Some(match k {
        WKey::Named(NamedKey::Escape) => Key::Escape,
        WKey::Named(NamedKey::Enter) => Key::Enter,
        WKey::Named(NamedKey::Backspace) => Key::Backspace,
        WKey::Named(NamedKey::Delete) => Key::Delete,
        WKey::Named(NamedKey::Tab) => Key::Tab,
        WKey::Named(NamedKey::Space) => Key::Space,
        WKey::Named(NamedKey::ArrowLeft) => Key::Left,
        WKey::Named(NamedKey::ArrowRight) => Key::Right,
        WKey::Named(NamedKey::ArrowUp) => Key::Up,
        WKey::Named(NamedKey::ArrowDown) => Key::Down,
        WKey::Named(NamedKey::Home) => Key::Home,
        WKey::Named(NamedKey::End) => Key::End,
        WKey::Named(NamedKey::PageUp) => Key::PageUp,
        WKey::Named(NamedKey::PageDown) => Key::PageDown,
        WKey::Character(s) => Key::Char(s.chars().next()?),
        _ => return None,
    })
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Sketch")
            .with_inner_size(winit::dpi::LogicalSize::new(1440.0, 900.0))
            .with_min_inner_size(winit::dpi::LogicalSize::new(720.0, 480.0));
        // The app id ties the window to the desktop entry in packaging/linux. On Wayland it is
        // the only way a desktop shell can match the window to that entry, and it is where KDE
        // takes the taskbar and Alt+Tab icon from. On X11 it sets the window class instead.
        #[cfg(target_os = "linux")]
        let attrs = {
            use winit::platform::wayland::WindowAttributesExtWayland;
            attrs.with_name("sketch-rs", "sketch-rs")
        };
        // SKETCH_RS_OPAQUE=1 turns off vibrancy (and window transparency) on macOS.
        #[cfg(target_os = "macos")]
        let translucent = crate::env_var("OPAQUE").is_none();
        #[cfg(target_os = "macos")]
        let attrs = {
            use winit::platform::macos::WindowAttributesExtMacOS;
            // Unified, title-less toolbar look: content runs under a transparent titlebar.
            attrs
                .with_titlebar_transparent(true)
                .with_fullsize_content_view(true)
                .with_title_hidden(true)
                .with_transparent(translucent)
        };
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        window.set_ime_allowed(true);
        #[cfg(target_os = "macos")]
        {
            self.st.translucent_sidebar = translucent && crate::macos::apply_vibrancy(&window);
            self.st.native_chrome = true;
            let (x, y) = theme::TRAFFIC_LIGHTS;
            crate::macos::inset_traffic_lights(&window, x, y);
            self.menu = Some(crate::macos::install_menu());
        }
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let surface = instance
            .create_surface(window.clone())
            .expect("create surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .expect("no compatible GPU adapter");
        let adapter_info = adapter.get_info();
        log::info!("adapter: {adapter_info:?}");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("sketch-rs"),
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .expect("create device");
        let caps = surface.get_capabilities(&adapter);
        log::info!("surface alpha modes: {:?}", caps.alpha_modes);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: pick_alpha_mode(&caps.alpha_modes, self.st.translucent_sidebar),
            view_formats: vec![],
            color_space: Default::default(),
        };
        surface.configure(&device, &config);
        let mut painter = Painter::new(&device, format);
        painter.transparent = self.st.translucent_sidebar;
        let egui = egui_winit::State::new(
            self.ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            // Start in the system appearance; ThemeChanged keeps it in sync afterwards.
            window.theme(),
            Some(device.limits().max_texture_dimension_2d as usize),
        );
        window.request_redraw();
        self.gfx = Some(Gfx {
            window,
            surface,
            device,
            queue,
            config,
            painter,
            egui,
            adapter_info,
        });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(g) = self.gfx.as_mut() else { return };
        let resp = g.egui.on_window_event(&g.window, &event);
        if !matches!(event, WindowEvent::RedrawRequested) {
            log::trace!("event {event:?} -> egui repaint {}", resp.repaint);
        }
        let scale = g.window.scale_factor();
        let mut redraw = resp.repaint;
        let now = self.now();
        match event {
            WindowEvent::CloseRequested => {
                if self.session.project.is_some() {
                    self.save_project(true);
                }
                if self.confirm_discard() {
                    event_loop.exit();
                }
            }
            WindowEvent::Resized(s) => {
                let g = self.gfx.as_mut().unwrap();
                g.config.width = s.width.max(1);
                g.config.height = s.height.max(1);
                g.surface.configure(&g.device, &g.config);
                #[cfg(target_os = "macos")]
                {
                    let (x, y) = theme::TRAFFIC_LIGHTS;
                    crate::macos::inset_traffic_lights(&g.window, x, y);
                }
                redraw = true;
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                // Moved between Retina and non-Retina displays. egui-winit has already picked
                // up the new pixels-per-point; resize the surface to the new physical size
                // (winit follows with a Resized, but don't rely on it).
                let g = self.gfx.as_mut().unwrap();
                let s = g.window.inner_size();
                g.config.width = s.width.max(1);
                g.config.height = s.height.max(1);
                g.surface.configure(&g.device, &g.config);
                redraw = true;
            }
            WindowEvent::ThemeChanged(_) => {
                // Light/dark switch: egui-winit swaps the style; refresh the accent too.
                theme::set_accent(theme::system_accent());
                ui::setup_style(&self.ctx);
                redraw = true;
            }
            WindowEvent::Occluded(false) => redraw = true,
            WindowEvent::RedrawRequested => {
                // egui-winit reports `repaint: true` for RedrawRequested itself; honouring it
                // re-requested a redraw every frame (a 100% CPU busy loop). Whether another
                // frame is needed is decided inside redraw() from egui's repaint delay.
                self.redraw();
                return;
            }
            WindowEvent::ModifiersChanged(m) => {
                let s = m.state();
                self.ed.handle(InputEvent::Modifiers(Modifiers {
                    shift: s.shift_key(),
                    ctrl: s.control_key(),
                    alt: s.alt_key(),
                    logo: s.super_key(),
                }));
            }
            WindowEvent::CursorMoved { position, .. } => {
                let p = v2(position.x / scale, position.y / scale);
                if self.captured || self.over_canvas(p) {
                    self.ed.handle(InputEvent::PointerMove {
                        pos: self.to_canvas(p),
                        pressure: None,
                        time_ms: now,
                    });
                    redraw = true;
                } else if self.ed.hover.take().is_some() {
                    redraw = true;
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let b = match button {
                    MouseButton::Left => Button::Primary,
                    MouseButton::Middle => Button::Middle,
                    MouseButton::Right => Button::Secondary,
                    _ => return,
                };
                let p = self
                    .ctx
                    .input(|i| i.pointer.latest_pos())
                    .map(|p| v2(p.x as f64, p.y as f64));
                let Some(p) = p else { return };
                match state {
                    ElementState::Pressed if self.over_canvas(p) => {
                        // Clicking the canvas takes keyboard focus away from text fields.
                        self.ctx.memory_mut(|m| {
                            if let Some(f) = m.focused() {
                                m.surrender_focus(f)
                            }
                        });
                        self.captured = true;
                        self.ed.handle(InputEvent::PointerDown {
                            pos: self.to_canvas(p),
                            button: b,
                            pressure: None,
                            time_ms: now,
                        });
                    }
                    ElementState::Released if self.captured => {
                        self.captured = false;
                        self.ed.handle(InputEvent::PointerUp {
                            pos: self.to_canvas(p),
                            button: b,
                            time_ms: now,
                        });
                    }
                    _ => {}
                }
                redraw = true;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let p = self
                    .ctx
                    .input(|i| i.pointer.latest_pos())
                    .map(|p| v2(p.x as f64, p.y as f64));
                if let Some(p) = p.filter(|p| self.over_canvas(*p)) {
                    let d = match delta {
                        MouseScrollDelta::LineDelta(x, y) => v2(x as f64 * 40.0, y as f64 * 40.0),
                        MouseScrollDelta::PixelDelta(pp) => v2(pp.x / scale, pp.y / scale),
                    };
                    let d = if self.ed.mods.shift && d.x == 0.0 {
                        v2(d.y, 0.0)
                    } else {
                        d
                    };
                    self.ed.handle(InputEvent::Scroll {
                        delta: -d,
                        pos: self.to_canvas(p),
                    });
                    redraw = true;
                }
            }
            WindowEvent::PinchGesture { delta, .. } => {
                let p = self
                    .ctx
                    .input(|i| i.pointer.latest_pos())
                    .map(|p| v2(p.x as f64, p.y as f64));
                if let Some(p) = p.filter(|p| self.over_canvas(*p)) {
                    // `delta` is the change in magnification; exp() keeps pinch in and out
                    // symmetric, like MKMapView / Preview.
                    self.ed.handle(InputEvent::Zoom {
                        factor: delta.exp(),
                        pos: self.to_canvas(p),
                    });
                    redraw = true;
                }
            }
            WindowEvent::DoubleTapGesture { .. } => {
                // Trackpad smart zoom (two-finger double tap): toggle fit / 100%.
                let a = if (self.ed.camera.zoom - 1.0).abs() < 1e-3 {
                    crate::editor::shortcuts::Action::ZoomFit
                } else {
                    crate::editor::shortcuts::Action::Zoom100
                };
                self.ed.run(a);
                redraw = true;
            }
            WindowEvent::Touch(t) => {
                // Pens and touch screens: forward pressure for ink.
                let p = v2(t.location.x / scale, t.location.y / scale);
                let pressure = t.force.map(|f| f.normalized());
                let pos = self.to_canvas(p);
                match t.phase {
                    TouchPhase::Started if self.over_canvas(p) => {
                        self.captured = true;
                        self.ed.handle(InputEvent::PointerDown {
                            pos,
                            button: Button::Primary,
                            pressure,
                            time_ms: now,
                        });
                    }
                    TouchPhase::Moved if self.captured => self.ed.handle(InputEvent::PointerMove {
                        pos,
                        pressure,
                        time_ms: now,
                    }),
                    TouchPhase::Ended | TouchPhase::Cancelled if self.captured => {
                        self.captured = false;
                        self.ed.handle(InputEvent::PointerUp {
                            pos,
                            button: Button::Primary,
                            time_ms: now,
                        });
                    }
                    _ => {}
                }
                redraw = true;
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if self.ctx.egui_wants_keyboard_input() {
                    self.gfx.as_ref().unwrap().window.request_redraw();
                    return;
                }
                let physical = match &event.physical_key {
                    // Shortcuts should work regardless of the active keyboard layout's shift state.
                    winit::keyboard::PhysicalKey::Code(c) => physical_char(*c).map(Key::Char),
                    _ => None,
                };
                // Alt changes the logical character on macOS (⌥A types "å"), so chords with
                // Alt (and with ⌘/Ctrl) resolve by physical key instead.
                let chord = self.ed.mods.alt || self.ed.mods.command();
                let key = if chord && self.ed.text_edit.is_none() {
                    physical.clone().or_else(|| map_key(&event.logical_key))
                } else {
                    map_key(&event.logical_key).or(physical)
                };
                if let Some(key) = key {
                    match event.state {
                        ElementState::Pressed => {
                            self.ed.handle(InputEvent::KeyDown {
                                key,
                                repeat: event.repeat,
                            });
                            if let Some(t) = &event.text
                                && self.ed.text_edit.is_some()
                                && !self.ed.mods.command()
                            {
                                self.ed.handle(InputEvent::Text(t.to_string()));
                            }
                        }
                        ElementState::Released => self.ed.handle(InputEvent::KeyUp { key }),
                    }
                }
                redraw = true;
            }
            WindowEvent::Ime(winit::event::Ime::Commit(s)) => {
                if !self.ctx.egui_wants_keyboard_input() {
                    self.ed.handle(InputEvent::Text(s));
                }
                redraw = true;
            }
            WindowEvent::DroppedFile(p) => {
                let ext = p
                    .extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                if matches!(
                    ext.as_str(),
                    "png"
                        | "jpg"
                        | "jpeg"
                        | "gif"
                        | "webp"
                        | "svg"
                        | "excalidraw"
                        | "excalidrawlib"
                ) && self.st.home.is_none()
                {
                    let at = self.ed.camera.screen_to_world(self.ed.pointer);
                    self.import_into_page(&p, Some(at));
                } else if file::is_document(&p) && self.confirm_discard() {
                    match file::load(&p) {
                        Ok((doc, cam)) => self.ed.load(doc, cam, Some(p)),
                        Err(e) => self.error(format!("Could not open {}: {e}", p.display())),
                    }
                }
                redraw = true;
            }
            _ => {}
        }
        if redraw && let Some(g) = &self.gfx {
            g.window.request_redraw();
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            #[cfg(target_os = "macos")]
            UserEvent::Menu(id) => {
                if let Some(cmd) = self.menu.as_ref().and_then(|m| m.command(&id)) {
                    self.menu_command(_event_loop, cmd);
                }
            }
            UserEvent::Mcp => self.run_mcp_calls(),
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Quitting from the menu, the Dock or an update install skips CloseRequested, so save
        // here instead of losing the edits that are still waiting for the autosave.
        if self.session.project.is_some() {
            self.save_project(true);
        }
        // Drop the window and the egui state here, while the event loop is still alive. On
        // Wayland, egui's clipboard runs a helper thread that reads the display socket when it
        // shuts down, and the socket is gone once the event loop returns.
        self.gfx = None;
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // winit runs the first redraw before this, so Sparkle loads once the window is up.
        // Later calls do nothing.
        #[cfg(target_os = "macos")]
        if self.gfx.is_some() {
            crate::macos::updater::start();
        }
        // Starts the autosave clock after any edit, and saves once the timer below wakes the
        // loop past the deadline, even while the window sits idle.
        if self.tick_autosave()
            && let Some(g) = &self.gfx
        {
            g.window.request_redraw();
        }
        let wake = match (self.next_repaint, self.session.next_deadline()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        match wake {
            Some(t) if Instant::now() >= t => {
                if self.next_repaint.is_some_and(|r| r <= t) {
                    self.next_repaint = None;
                }
                if let Some(g) = &self.gfx {
                    g.window.request_redraw();
                }
                event_loop.set_control_flow(ControlFlow::Wait);
            }
            Some(t) => event_loop.set_control_flow(ControlFlow::WaitUntil(t)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }
}

/// A non-opaque mode when the window shows vibrancy through transparent pixels.
fn pick_alpha_mode(
    modes: &[wgpu::CompositeAlphaMode],
    translucent: bool,
) -> wgpu::CompositeAlphaMode {
    use wgpu::CompositeAlphaMode as A;
    if translucent {
        for m in [A::PreMultiplied, A::PostMultiplied, A::Inherit] {
            if modes.contains(&m) {
                return m;
            }
        }
        log::warn!("no transparent surface alpha mode in {modes:?}");
    }
    if modes.contains(&A::Opaque) {
        A::Opaque
    } else {
        modes[0]
    }
}

fn physical_char(c: winit::keyboard::KeyCode) -> Option<char> {
    use winit::keyboard::KeyCode::*;
    Some(match c {
        BracketLeft => '[',
        BracketRight => ']',
        Equal => '=',
        Minus => '-',
        Quote => '\'',
        Slash => '/',
        Backslash => '\\',
        Comma => ',',
        Period => '.',
        Digit0 => '0',
        Digit1 => '1',
        Digit2 => '2',
        Digit3 => '3',
        Digit4 => '4',
        Digit5 => '5',
        Digit6 => '6',
        Digit7 => '7',
        Digit8 => '8',
        Digit9 => '9',
        KeyA => 'a',
        KeyB => 'b',
        KeyC => 'c',
        KeyD => 'd',
        KeyE => 'e',
        KeyF => 'f',
        KeyG => 'g',
        KeyH => 'h',
        KeyI => 'i',
        KeyJ => 'j',
        KeyK => 'k',
        KeyL => 'l',
        KeyM => 'm',
        KeyN => 'n',
        KeyO => 'o',
        KeyP => 'p',
        KeyQ => 'q',
        KeyR => 'r',
        KeyS => 's',
        KeyT => 't',
        KeyU => 'u',
        KeyV => 'v',
        KeyW => 'w',
        KeyX => 'x',
        KeyY => 'y',
        KeyZ => 'z',
        _ => return None,
    })
}

impl App {
    /// Run the MCP tool calls that arrived since the last wake-up, then repaint.
    fn run_mcp_calls(&mut self) {
        let calls: Vec<crate::mcp::Call> = match &self.mcp {
            Some((rx, _)) => rx.try_iter().collect(),
            None => return,
        };
        if calls.is_empty() {
            return;
        }
        for c in calls {
            let r = crate::mcp::exec::call(self, &c.name, &c.args);
            if let Err(e) = &r {
                log::info!("mcp {}: {e}", c.name);
            }
            let _ = c.reply.send(r);
        }
        self.st.project_name = self.session.project_name();
        self.ed.revision += 1;
        if let Some(g) = &self.gfx {
            g.window.request_redraw();
        }
    }
}

impl crate::mcp::exec::Host for App {
    fn editor(&mut self) -> &mut Editor {
        &mut self.ed
    }
    fn store(&self) -> &Store {
        &self.session.store
    }
    fn project(&self) -> Option<&crate::project::Project> {
        self.session
            .project
            .as_ref()
            .filter(|_| self.st.home.is_none())
    }
    fn canvas_open(&self) -> bool {
        self.st.home.is_none()
    }
    fn open_project(&mut self, dir: &std::path::Path) -> Result<(), String> {
        // No modal dialogs here: the agent gets the error instead.
        if self.session.project.is_some() {
            self.session
                .save_all(&mut self.ed)
                .map_err(|e| format!("could not save the open project: {e}"))?;
        }
        self.session
            .open(&mut self.ed, dir)
            .map_err(|e| format!("could not open {}: {e}", dir.display()))?;
        self.st.home = None;
        self.st.project_name = self.session.project_name();
        self.st.save_state.clear();
        self.ed.status.clear();
        Ok(())
    }
    fn save(&mut self) -> Result<(), String> {
        if self.session.project.is_some() {
            self.session
                .save_all(&mut self.ed)
                .map_err(|e| e.to_string())?;
            self.st.save_state = "Saved".into();
        }
        Ok(())
    }
    fn gpu(&mut self) -> Option<Headless> {
        self.offscreen().or_else(|| Headless::new().ok())
    }
}

/// Start the MCP server unless `SKETCH_RS_MCP=0`. Calls wake the event loop through `proxy`.
fn start_mcp(
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,
) -> Option<(
    std::sync::mpsc::Receiver<crate::mcp::Call>,
    crate::mcp::server::Running,
)> {
    if crate::env_var("MCP").is_some_and(|v| matches!(v.to_str(), Some("0" | "off" | "false"))) {
        return None;
    }
    let (tx, rx) = std::sync::mpsc::channel::<crate::mcp::Call>();
    let tx = std::sync::Mutex::new(tx);
    let dispatch: crate::mcp::server::Dispatch = std::sync::Arc::new(move |call| {
        tx.lock().unwrap().send(call).is_ok() && proxy.send_event(UserEvent::Mcp).is_ok()
    });
    match crate::mcp::server::spawn(crate::mcp::port(), dispatch) {
        Ok(running) => {
            log::info!("MCP server listening on {}", running.url());
            Some((rx, running))
        }
        Err(e) => {
            log::warn!("MCP server not started: {e}");
            None
        }
    }
}

pub fn run(start: Start) -> Result<(), String> {
    let store = Store::open(Store::default_root())
        .map_err(|e| format!("cannot create the projects folder: {e}"))?;
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .map_err(|e| format!("cannot open a window: {e}"))?;
    event_loop.set_control_flow(ControlFlow::Wait);
    #[cfg(target_os = "macos")]
    {
        let proxy = event_loop.create_proxy();
        muda::MenuEvent::set_event_handler(Some(move |e: muda::MenuEvent| {
            let _ = proxy.send_event(UserEvent::Menu(e.id));
        }));
    }
    let mut app = App::new(start, store);
    app.mcp = start_mcp(event_loop.create_proxy());
    event_loop.run_app(&mut app).map_err(|e| e.to_string())
}
