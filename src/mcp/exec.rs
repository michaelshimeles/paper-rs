//! Runs MCP tool calls against the editor, on the thread that owns it.
//!
//! Node ids are strings `"<page>:<n>"`: the first six hex digits of the page id and the
//! node's number in that page's document (numbers are only unique per page). A bare number
//! means the page from `pageId`, else the page the user is viewing. Writes run as one
//! transaction each (`Editor::edit`), so they undo in one step and refresh the window;
//! writes to other pages go to that page's own document and history (`Editor::with_page`).

use super::css::{self, Context, Styles};
use super::html::{self, Element, Html};
use super::{Out, ToolResult, tools};
use crate::editor::Editor;
use crate::geom::{Rect, V2, v2};
use crate::history::Tx;
use crate::model::{
    AutoLayout, Binding, Color, Direction, Document, Head, ImageMode, Node, NodeId, NodeKind,
    Paint, PathPoint, Route, Sizing, SubPath, TextAlign,
};
use crate::project::{Project, Store};
use crate::render::gpu::Headless;
use crate::session::Session;
use serde_json::{Map, Value, json};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// What a tool call needs from its surroundings.
pub trait Host {
    fn editor(&mut self) -> &mut Editor;
    fn store(&self) -> &Store;
    /// The project shown in the app (None for a scratch document or the home screen).
    fn project(&self) -> Option<&Project>;
    /// A canvas is showing (false on the home screen).
    fn canvas_open(&self) -> bool;
    /// Open a project folder (saving the current one).
    fn open_project(&mut self, dir: &Path) -> Result<(), String>;
    /// Save the open project now.
    fn save(&mut self) -> Result<(), String>;
    /// An offscreen renderer for screenshots and PNG export.
    fn gpu(&mut self) -> Option<Headless>;
}

/// A host without a window: an editor plus a project session. Used by tests and anything
/// that wants to drive Sketch headlessly.
pub struct SessionHost {
    pub ed: Editor,
    pub session: Session,
    gpu: Option<Headless>,
    gpu_tried: bool,
}

impl SessionHost {
    pub fn new(store: Store) -> SessionHost {
        SessionHost {
            ed: Editor::default(),
            session: Session::new(store),
            gpu: None,
            gpu_tried: false,
        }
    }
}

impl Host for SessionHost {
    fn editor(&mut self) -> &mut Editor {
        &mut self.ed
    }
    fn store(&self) -> &Store {
        &self.session.store
    }
    fn project(&self) -> Option<&Project> {
        self.session.project.as_ref()
    }
    fn canvas_open(&self) -> bool {
        true
    }
    fn open_project(&mut self, dir: &Path) -> Result<(), String> {
        if self.session.project.is_some() {
            self.session
                .save_all(&mut self.ed)
                .map_err(|e| e.to_string())?;
        }
        self.session
            .open(&mut self.ed, dir)
            .map_err(|e| format!("{}: {e}", dir.display()))
    }
    fn save(&mut self) -> Result<(), String> {
        if self.session.project.is_some() {
            self.session
                .save_all(&mut self.ed)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    fn gpu(&mut self) -> Option<Headless> {
        if !self.gpu_tried {
            self.gpu_tried = true;
            self.gpu = Headless::new()
                .map_err(|e| log::warn!("mcp: no offscreen renderer: {e}"))
                .ok();
        }
        self.gpu.as_ref().map(|g| Headless {
            device: g.device.clone(),
            queue: g.queue.clone(),
            adapter_info: g.adapter_info.clone(),
        })
    }
}

// ------------------------------------------------------------------------------ arguments

struct Args<'a>(&'a Map<String, Value>);

impl<'a> Args<'a> {
    fn get(&self, k: &str) -> Option<&'a Value> {
        self.0.get(k).filter(|v| !v.is_null())
    }
    fn str(&self, k: &str) -> Option<&'a str> {
        self.get(k).and_then(Value::as_str).map(str::trim)
    }
    fn req_str(&self, k: &str) -> Result<&'a str, String> {
        self.str(k)
            .ok_or_else(|| format!("missing required string argument `{k}`"))
    }
    fn num(&self, k: &str) -> Option<f64> {
        self.get(k).and_then(|v| match v {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.trim().trim_end_matches("px").parse().ok(),
            _ => None,
        })
    }
    fn arr(&self, k: &str) -> Result<&'a Vec<Value>, String> {
        self.get(k)
            .and_then(Value::as_array)
            .ok_or_else(|| format!("missing required array argument `{k}`"))
    }
    fn strs(&self, k: &str) -> Result<Vec<String>, String> {
        Ok(self
            .arr(k)?
            .iter()
            .filter_map(|v| match v {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .collect())
    }
}

fn text(v: Value) -> ToolResult {
    Ok(vec![Out::Text(
        serde_json::to_string_pretty(&v).unwrap_or_default(),
    )])
}

fn r2(v: f64) -> Value {
    json!((v * 100.0).round() / 100.0)
}

// ------------------------------------------------------------------------------ ids

const ROOT: &str = "root";

fn page_prefix(ed: &Editor, page: usize) -> &str {
    let id = &ed.pages[page].id;
    &id[..id.len().min(6)]
}

pub fn node_key(ed: &Editor, page: usize, id: NodeId) -> String {
    format!("{}:{}", page_prefix(ed, page), id.0)
}

fn find_page(ed: &Editor, key: &str) -> Result<usize, String> {
    let key = key.trim();
    ed.pages
        .iter()
        .position(|p| p.id == key)
        .or_else(|| {
            ed.pages
                .iter()
                .position(|p| p.id.starts_with(key) && key.len() >= 4)
        })
        .or_else(|| {
            ed.pages
                .iter()
                .position(|p| p.name.eq_ignore_ascii_case(key))
        })
        .ok_or_else(|| {
            format!(
                "page `{key}` not found; pages are {}",
                ed.pages
                    .iter()
                    .map(|p| format!("{} ({})", p.id, p.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

fn page_arg(ed: &Editor, a: &Args) -> Result<usize, String> {
    match a.str("pageId") {
        Some(p) if !p.is_empty() => find_page(ed, p),
        _ => Ok(ed.active_page),
    }
}

/// Resolve a node id string to (page index, node id).
fn resolve(ed: &Editor, key: &str, default_page: usize) -> Result<(usize, NodeId), String> {
    let key = key.trim().trim_start_matches('#');
    let (page, num) = match key.split_once(':') {
        Some((p, n)) => (find_page(ed, p)?, n),
        None => (default_page, key),
    };
    let id = NodeId(
        num.parse::<u64>()
            .map_err(|_| format!("`{key}` is not a node id (expected e.g. \"3fa9c1:14\")"))?,
    );
    if ed.page_doc(page).get(id).is_none() {
        return Err(format!("node `{key}` does not exist"));
    }
    Ok((page, id))
}

/// Resolve several ids that must all live on one page.
fn resolve_all(
    ed: &Editor,
    keys: &[String],
    default_page: usize,
) -> Result<(usize, Vec<NodeId>), String> {
    let mut page = None;
    let mut out = Vec::new();
    for k in keys {
        let (p, id) = resolve(ed, k, default_page)?;
        if page.is_some_and(|q| q != p) {
            return Err("all nodes in one call must be on the same page".into());
        }
        page = Some(p);
        out.push(id);
    }
    Ok((page.unwrap_or(default_page), out))
}

// ------------------------------------------------------------------------------ dispatch

/// Run one tool. Errors are returned as messages for the agent.
pub fn call(host: &mut dyn Host, name: &str, args: &Value) -> ToolResult {
    let empty = Map::new();
    let a = Args(args.as_object().unwrap_or(&empty));
    match name {
        "list_resources" => list_resources(host, &a),
        "open_file" => open_file(host, &a),
        "create_file" => create_file(host, &a),
        "get_font_family_info" => font_info(&a),
        "get_guide" => guide(&a),
        _ => {
            check_file(host, &a)?;
            let ed = host.editor();
            ed.commit_pending();
            // The first content on the page the user is looking at is brought into view.
            let was_empty = ed.doc.root.is_empty();
            let r = match name {
                "create_page" => create_page(ed, &a),
                "rename_pages" => rename_pages(ed, &a),
                "get_basic_info" => basic_info(host, &a),
                "get_selection" => selection(ed),
                "get_node_info" => node_info(ed, &a),
                "get_children" => children(ed, &a),
                "get_tree_summary" => tree_summary(ed, &a),
                "get_screenshot" => screenshot(host, &a),
                "get_jsx" => jsx(host, &a),
                "get_computed_styles" => computed_styles(ed, &a),
                "get_fill_image" => fill_image(ed, &a),
                "find_nodes" => find_nodes(ed, &a),
                "export" => export(host, &a),
                "export_combined_pdf" => export_pdf(host, &a),
                "create_artboard" => create_artboard(ed, &a),
                "write_html" => write_html(ed, &a),
                "create_shape" => create_shape(ed, &a),
                "update_styles" => update_styles(ed, &a),
                "set_text_content" => set_text_content(ed, &a),
                "rename_nodes" => rename_nodes(ed, &a),
                "delete_nodes" => delete_nodes(ed, &a),
                "duplicate_nodes" => duplicate_nodes(ed, &a),
                "move_nodes" => move_nodes(ed, &a),
                "select_nodes" => select_nodes(ed, &a),
                "undo" | "redo" => undo_redo(ed, &a, name == "undo"),
                "finish_working_on_nodes" => {
                    host.save()?;
                    text(json!({"ok": true, "saved": host.project().is_some()}))
                }
                _ => Err(format!("unknown tool `{name}`")),
            };
            let ed = host.editor();
            if r.is_ok() && was_empty && !ed.doc.root.is_empty() {
                ed.zoom_to_fit();
            }
            r
        }
    }
}

/// Projects match by manifest id, name or folder path.
fn find_project(store: &Store, key: &str) -> Result<PathBuf, String> {
    let key = key.trim().trim_start_matches("/file/");
    let p = Path::new(key);
    if p.is_absolute() && p.join(crate::project::MANIFEST).is_file() {
        return Ok(p.to_path_buf());
    }
    let list = store.list();
    list.iter()
        .find(|e| e.manifest.id == key)
        .or_else(|| list.iter().find(|e| e.manifest.name == key))
        .or_else(|| {
            list.iter()
                .find(|e| e.manifest.name.eq_ignore_ascii_case(key))
        })
        .or_else(|| {
            list.iter()
                .find(|e| e.dir.file_name().is_some_and(|f| f == key))
        })
        .map(|e| e.dir.clone())
        .ok_or_else(|| format!("project `{key}` not found (see list_resources)"))
}

fn check_file(host: &mut dyn Host, a: &Args) -> Result<(), String> {
    if !host.canvas_open() {
        return Err(
            "No project is open in Sketch (the home screen is showing). Call list_resources \
             and open_file, or create_file then open_file."
                .into(),
        );
    }
    let Some(key) = a.str("fileId").filter(|k| !k.is_empty()) else {
        return Ok(());
    };
    let open = host
        .project()
        .map(|p| (p.manifest.id.clone(), p.dir.clone()));
    let want = find_project(host.store(), key)?;
    match open {
        Some((_, dir)) if same_dir(&dir, &want) => Ok(()),
        Some((id, _)) => Err(format!(
            "`{key}` is not the project open in the app ({id}). Call open_file first."
        )),
        None => Err(format!(
            "`{key}` is not open (a scratch document is showing). Call open_file first."
        )),
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    a == b || a.canonicalize().ok() == b.canonicalize().ok()
}

// ------------------------------------------------------------------------------ projects

fn list_resources(host: &mut dyn Host, a: &Args) -> ToolResult {
    let limit = a.num("limit").unwrap_or(50.0).max(1.0) as usize;
    let open = host.project().map(|p| p.dir.clone());
    let list = host.store().list();
    let total = list.len();
    let items: Vec<Value> = list
        .into_iter()
        .take(limit)
        .map(|e| {
            let is_open = open.as_ref().is_some_and(|d| same_dir(d, &e.dir));
            json!({
                "id": e.manifest.id,
                "type": "file",
                "name": e.manifest.name,
                "path": e.dir,
                "pages": e.manifest.pages.len(),
                "updatedAt": e.manifest.modified,
                "open": is_open,
                "active": is_open && host.canvas_open(),
            })
        })
        .collect();
    let mut v = json!({"items": items, "root": host.store().root});
    if total > limit {
        v["truncated"] = json!(true);
    }
    text(v)
}

fn open_file(host: &mut dyn Host, a: &Args) -> ToolResult {
    let dir = find_project(host.store(), a.req_str("fileId")?)?;
    let already = host.canvas_open() && host.project().is_some_and(|p| same_dir(&p.dir, &dir));
    if !already {
        host.open_project(&dir)?;
    }
    if let Some(p) = a.str("pageId") {
        let ed = host.editor();
        let i = find_page(ed, p)?;
        ed.switch_page(i);
    }
    basic_info(host, &Args(&Map::new()))
}

fn create_file(host: &mut dyn Host, a: &Args) -> ToolResult {
    let name = a
        .str("name")
        .filter(|n| !n.is_empty())
        .unwrap_or("Untitled");
    let (p, page) = host.store().create(name).map_err(|e| e.to_string())?;
    text(json!({
        "fileId": p.manifest.id,
        "name": p.manifest.name,
        "path": p.dir,
        "pageId": page.id,
        "next": "call open_file with this fileId to work in it",
    }))
}

fn create_page(ed: &mut Editor, a: &Args) -> ToolResult {
    let i = ed.add_page_in_background(a.str("name"));
    text(json!({"pageId": ed.pages[i].id, "name": ed.pages[i].name}))
}

fn rename_pages(ed: &mut Editor, a: &Args) -> ToolResult {
    let mut done = Vec::new();
    for u in a.arr("updates")? {
        let u = Args(u.as_object().ok_or("each update must be an object")?);
        let i = find_page(ed, u.req_str("pageId")?)?;
        ed.rename_page(i, u.req_str("name")?);
        done.push(json!({"pageId": ed.pages[i].id, "name": ed.pages[i].name}));
    }
    text(json!({"renamed": done}))
}

// ------------------------------------------------------------------------------ reading

/// Node summary with parent-relative and world positions.
fn node_brief(ed: &Editor, page: usize, id: NodeId) -> Value {
    let doc = ed.page_doc(page);
    let n = doc.get(id).unwrap();
    let w = doc.world_rect(id).unwrap_or(n.rect);
    json!({
        "id": node_key(ed, page, id),
        "name": n.name,
        "type": n.kind.type_name(),
        "x": r2(n.rect.x), "y": r2(n.rect.y),
        "worldX": r2(w.x), "worldY": r2(w.y),
        "width": r2(n.rect.w), "height": r2(n.rect.h),
        "childCount": n.children.len(),
    })
}

/// The top-level frame a node belongs to.
fn artboard_of(doc: &Document, id: NodeId) -> Option<NodeId> {
    let top = doc.ancestors(id).last().copied().unwrap_or(id);
    doc.get(top).filter(|n| n.is_frame()).map(|n| n.id)
}

fn basic_info(host: &mut dyn Host, a: &Args) -> ToolResult {
    let project = host.project().map(|p| {
        (
            p.manifest.id.clone(),
            p.manifest.name.clone(),
            p.dir.clone(),
        )
    });
    let ed = host.editor();
    let page = page_arg(ed, a)?;
    let doc = ed.page_doc(page);
    let mut artboards = Vec::new();
    let mut others: HashMap<&str, usize> = HashMap::new();
    for id in &doc.root {
        let n = doc.get(*id).unwrap();
        if n.is_frame() {
            artboards.push(node_brief(ed, page, *id));
        } else {
            *others.entry(n.kind.type_name()).or_default() += 1;
        }
    }
    let pages: Vec<Value> = ed
        .pages
        .iter()
        .enumerate()
        .map(|(i, p)| {
            json!({
                "id": p.id, "name": p.name, "isActive": i == ed.active_page,
                "nodeCount": ed.page_doc(i).nodes.len(),
            })
        })
        .collect();
    let (file_id, file_name, path) = match project {
        Some((id, name, dir)) => (json!(id), json!(name), json!(dir)),
        None => (Value::Null, json!(doc.name), Value::Null),
    };
    text(json!({
        "fileId": file_id,
        "fileName": file_name,
        "path": path,
        "page": {"id": ed.pages[page].id, "name": ed.pages[page].name, "isActive": page == ed.active_page},
        "pages": pages,
        "nodeCount": doc.nodes.len(),
        "artboards": artboards,
        "otherTopLevelLayers": others,
        "fontFamilies": ["Inter"],
        "selectionCount": if page == ed.active_page { ed.selection.len() } else { 0 },
        "nodeIdFormat": "\"<page>:<n>\", e.g. \"".to_string() + &node_key(ed, page, NodeId(1)) + "\"",
    }))
}

fn selection(ed: &mut Editor) -> ToolResult {
    let page = ed.active_page;
    let items: Vec<Value> = ed
        .selection
        .iter()
        .filter(|id| ed.doc.get(**id).is_some())
        .map(|id| {
            let mut v = node_brief(ed, page, *id);
            if let Some(ab) = artboard_of(&ed.doc, *id) {
                v["artboardId"] = json!(node_key(ed, page, ab));
                v["artboardName"] = json!(ed.doc.get(ab).unwrap().name);
            }
            if let Some(t) = ed.doc.get(*id).unwrap().kind.text() {
                v["text"] = json!(t);
            }
            v
        })
        .collect();
    text(json!({"pageId": ed.pages[page].id, "selection": items}))
}

fn node_info(ed: &mut Editor, a: &Args) -> ToolResult {
    let (page, id) = resolve(ed, a.req_str("nodeId")?, page_arg(ed, a)?)?;
    let doc = ed.page_doc(page);
    let n = doc.get(id).unwrap();
    let mut v = node_brief(ed, page, id);
    v["pageId"] = json!(ed.pages[page].id);
    v["visible"] = json!(n.visible);
    v["locked"] = json!(n.locked);
    v["parentId"] = json!(n.parent.map_or(ROOT.to_string(), |p| node_key(ed, page, p)));
    v["childIds"] = json!(
        n.children
            .iter()
            .map(|c| node_key(ed, page, *c))
            .collect::<Vec<_>>()
    );
    if let Some(t) = n.kind.text() {
        v["text"] = json!(t);
    }
    if let Some(ab) = artboard_of(doc, id) {
        v["artboardId"] = json!(node_key(ed, page, ab));
    }
    if n.component {
        v["component"] = json!(true);
    }
    if n.instance.is_some() {
        v["instance"] = json!(true);
    }
    v["autoLayout"] = json!(n.layout.is_some());
    text(v)
}

fn children(ed: &mut Editor, a: &Args) -> ToolResult {
    let def = page_arg(ed, a)?;
    let key = a.req_str("nodeId")?;
    let (page, parent) = if key == ROOT {
        (def, None)
    } else {
        let (p, id) = resolve(ed, key, def)?;
        (p, Some(id))
    };
    let kids: Vec<Value> = ed
        .page_doc(page)
        .children(parent)
        .iter()
        .map(|c| node_brief(ed, page, *c))
        .collect();
    text(json!({"children": kids}))
}

fn tree_summary(ed: &mut Editor, a: &Args) -> ToolResult {
    let def = page_arg(ed, a)?;
    let key = a.req_str("nodeId")?;
    let depth = a.num("depth").unwrap_or(3.0).clamp(1.0, 10.0) as usize;
    let (page, roots) = if key == ROOT {
        (def, ed.page_doc(def).root.clone())
    } else {
        let (p, id) = resolve(ed, key, def)?;
        (p, vec![id])
    };
    fn walk(ed: &Editor, page: usize, id: NodeId, d: usize, max: usize, out: &mut String) {
        let doc = ed.page_doc(page);
        let n = doc.get(id).unwrap();
        let size = if matches!(n.kind, NodeKind::Text { .. }) {
            format!("{}×{}", n.rect.w.round(), n.rect.h.round())
        } else {
            let s = |ax: usize, v: f64| match n.layout_child.sizing[ax] {
                Sizing::Hug => format!("hug({})", v.round()),
                Sizing::Fill => format!("fill({})", v.round()),
                Sizing::Fixed => format!("{}", v.round()),
            };
            format!("{}×{}", s(0, n.rect.w), s(1, n.rect.h))
        };
        let mut line = format!(
            "{}{} \"{}\" [{}] {}",
            "  ".repeat(d),
            n.kind.type_name(),
            n.name,
            node_key(ed, page, id),
            size
        );
        if let Some(t) = n.kind.text() {
            let short: String = t.chars().take(40).collect();
            line += &format!(" {:?}", short);
        }
        if !n.visible {
            line += " (hidden)";
        }
        out.push_str(&line);
        out.push('\n');
        if d + 1 >= max && !n.children.is_empty() {
            out.push_str(&format!(
                "{}… {} children\n",
                "  ".repeat(d + 1),
                n.children.len()
            ));
            return;
        }
        for c in &n.children {
            walk(ed, page, *c, d + 1, max, out);
        }
    }
    let mut out = String::new();
    for r in roots {
        walk(ed, page, r, 0, depth, &mut out);
    }
    if out.is_empty() {
        out = "(empty page)".into();
    }
    Ok(vec![Out::Text(out)])
}

/// Largest screenshot edge, in pixels.
const SHOT_MAX_EDGE: f64 = 2048.0;

fn screenshot(host: &mut dyn Host, a: &Args) -> ToolResult {
    let gpu = host
        .gpu()
        .ok_or("screenshots need a GPU and none is available")?;
    let ed = host.editor();
    let def = page_arg(ed, a)?;
    let (page, roots) = match a.str("nodeId").filter(|k| !k.is_empty() && *k != ROOT) {
        Some(k) => {
            let (p, id) = resolve(ed, k, def)?;
            (p, vec![id])
        }
        None => (def, ed.page_doc(def).root.clone()),
    };
    let doc = ed.page_doc(page);
    let b = crate::io::svg::export_bounds(doc, &roots).ok_or("nothing to capture (empty)")?;
    let want = a.num("scale").unwrap_or(1.0).clamp(0.1, 4.0);
    let mut scale = want.min(SHOT_MAX_EDGE / b.w.max(b.h).max(1.0));
    loop {
        let img = crate::io::png::render(&gpu, doc, Some(&roots), scale, Some(Color::WHITE))?;
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        let png = buf.into_inner();
        // Keep the base64 payload comfortably under API image limits (~5 MB).
        if png.len() > 3_500_000 && scale > 0.2 {
            scale *= 0.7;
            continue;
        }
        let (w, h) = img.dimensions();
        return Ok(vec![
            Out::Image {
                data: png,
                mime: "image/png".into(),
            },
            Out::Text(
                json!({"width": w, "height": h, "scale": (scale * 1000.0).round() / 1000.0})
                    .to_string(),
            ),
        ]);
    }
}

fn parent_layout(doc: &Document, n: &Node) -> Option<AutoLayout> {
    n.parent.and_then(|p| doc.get(p)).and_then(|p| p.layout)
}

fn computed_styles(ed: &mut Editor, a: &Args) -> ToolResult {
    let def = page_arg(ed, a)?;
    let mut out = Map::new();
    for k in a.strs("nodeIds")? {
        let (page, id) = resolve(ed, &k, def)?;
        let doc = ed.page_doc(page);
        let n = doc.get(id).unwrap();
        out.insert(
            node_key(ed, page, id),
            Value::Object(css::computed(
                n,
                parent_layout(doc, n).as_ref(),
                n.parent.is_none(),
            )),
        );
    }
    text(Value::Object(out))
}

fn fill_image(ed: &mut Editor, a: &Args) -> ToolResult {
    let (page, id) = resolve(ed, a.req_str("nodeId")?, page_arg(ed, a)?)?;
    let n = ed.page_doc(page).get(id).unwrap();
    let asset = n
        .style
        .fills
        .iter()
        .rev()
        .find_map(|p| match p {
            Paint::Image { asset, .. } => Some(asset.clone()),
            _ => None,
        })
        .ok_or("the node has no image fill")?;
    let img = crate::assets::get(&asset).ok_or("the image data is not loaded")?;
    Ok(vec![
        Out::Image {
            data: img.bytes.clone(),
            mime: img.mime().into(),
        },
        Out::Text(json!({"asset": asset, "width": img.width, "height": img.height}).to_string()),
    ])
}

/// Case-insensitive glob with `*`, anchored at both ends.
pub fn glob(pattern: &str, s: &str) -> bool {
    let (p, s) = (pattern.to_lowercase(), s.to_lowercase());
    let parts: Vec<&str> = p.split('*').collect();
    if parts.len() == 1 {
        return p == s;
    }
    let mut rest = s.as_str();
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            match rest.strip_prefix(part) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else {
            match rest.find(part) {
                Some(j) => rest = &rest[j + part.len()..],
                None => return false,
            }
        }
    }
    true
}

fn value_matches(want: &str, got: &str) -> Option<String> {
    if glob(want, got) {
        return Some(got.to_string());
    }
    let c = css::color(want)?;
    css::split_ws(&got.replace(',', " , "))
        .into_iter()
        .chain(std::iter::once(got.to_string()))
        .find(|part| css::color(part) == Some(c))
}

fn find_nodes(ed: &mut Editor, a: &Args) -> ToolResult {
    let mut scope: Vec<(usize, Vec<NodeId>)> = Vec::new();
    if let Some(k) = a.str("nodeId").filter(|k| !k.is_empty()) {
        let (p, id) = resolve(ed, k, page_arg(ed, a)?)?;
        scope.push((p, ed.page_doc(p).subtree(id)));
    } else if a.str("pageId").is_some() {
        let p = page_arg(ed, a)?;
        scope.push((p, ed.page_doc(p).paint_order()));
    } else {
        for p in 0..ed.pages.len() {
            scope.push((p, ed.page_doc(p).paint_order()));
        }
    }
    let text_q = a.str("textValue");
    let filters: Vec<(String, String)> = a
        .get("filters")
        .and_then(Value::as_array)
        .map(|v| {
            v.iter()
                .filter_map(Value::as_object)
                .map(|o| {
                    let s = |k: &str| o.get(k).and_then(Value::as_str).unwrap_or("*").to_string();
                    (css::camel(&s("styleName")), s("styleValue"))
                })
                .collect()
        })
        .unwrap_or_default();
    if text_q.is_none() && filters.is_empty() {
        return Err("pass textValue and/or filters".into());
    }
    let mut results = Vec::new();
    for (page, ids) in scope {
        let doc = ed.page_doc(page);
        'node: for id in ids {
            let n = doc.get(id).unwrap();
            let mut matched = Vec::new();
            if let Some(q) = text_q {
                match n.kind.text() {
                    Some(t) if glob(q, t) => matched.push(json!({"textValue": t})),
                    _ => continue,
                }
            }
            if !filters.is_empty() {
                let st = css::computed(n, parent_layout(doc, n).as_ref(), n.parent.is_none());
                for (name, want) in &filters {
                    let hit = st.iter().find_map(|(k, v)| {
                        let v = v.as_str()?;
                        (glob(name, k))
                            .then(|| value_matches(want, v))
                            .flatten()
                            .map(|m| (k, m))
                    });
                    match hit {
                        Some((k, m)) => matched.push(json!({"styleName": k, "styleValue": m})),
                        None => continue 'node,
                    }
                }
            }
            results.push(json!({
                "id": node_key(ed, page, id),
                "name": n.name,
                "type": n.kind.type_name(),
                "pageId": ed.pages[page].id,
                "matched": matched,
            }));
            if results.len() >= 500 {
                break;
            }
        }
    }
    text(json!({"count": results.len(), "results": results}))
}

fn font_info(a: &Args) -> ToolResult {
    let fams: Vec<Value> = a
        .strs("familyNames")?
        .iter()
        .map(|f| {
            let inter = f.trim().eq_ignore_ascii_case("inter");
            json!({
                "family": f,
                "available": inter,
                "styles": if inter { json!([
                    {"weight": 400, "style": "normal"}, {"weight": 700, "style": "normal"},
                    {"weight": 400, "style": "italic"}, {"weight": 700, "style": "italic"}]) } else { json!([]) },
                "note": if inter { "" } else { "not available; text renders in Inter" },
            })
        })
        .collect();
    text(
        json!({"families": fams, "weights": "fontWeight ≥ 600 renders bold, below renders regular"}),
    )
}

fn guide(a: &Args) -> ToolResult {
    let t = a.req_str("topic")?;
    let s = match t {
        "sketch-rs-mcp-instructions"
        | "paper-rs-mcp-instructions"
        | "paper-mcp-instructions"
        | "instructions" => tools::GUIDE_INSTRUCTIONS,
        "html" | "write_html" => tools::GUIDE_HTML,
        "whiteboard" | "excalidraw" => tools::GUIDE_WHITEBOARD,
        _ => {
            return Err(format!(
                "unknown topic `{t}`; topics: sketch-rs-mcp-instructions, html, whiteboard"
            ));
        }
    };
    Ok(vec![Out::Text(s.into())])
}

// ------------------------------------------------------------------------------ jsx

fn jsx(host: &mut dyn Host, a: &Args) -> ToolResult {
    let assets_dir = host
        .project()
        .map(|p| p.dir.join(crate::project::ASSETS_DIR));
    let ed = host.editor();
    let (page, id) = resolve(ed, a.req_str("nodeId")?, page_arg(ed, a)?)?;
    let doc = ed.page_doc(page);
    let mut out = String::new();
    jsx_node(doc, id, 0, true, assets_dir.as_deref(), &mut out);
    Ok(vec![Out::Text(out)])
}

fn style_obj(m: &Map<String, Value>) -> String {
    let parts: Vec<String> = m
        .iter()
        .map(|(k, v)| format!("{k}: {}", serde_json::to_string(v).unwrap_or_default()))
        .collect();
    format!("{{{{ {} }}}}", parts.join(", "))
}

fn jsx_text(t: &str) -> String {
    if t.chars()
        .all(|c| !matches!(c, '{' | '}' | '<' | '>' | '\n' | '"' | '\\'))
    {
        t.to_string()
    } else {
        format!("{{{}}}", serde_json::to_string(t).unwrap_or_default())
    }
}

fn jsx_node(
    doc: &Document,
    id: NodeId,
    depth: usize,
    root: bool,
    assets: Option<&Path>,
    out: &mut String,
) {
    let n = doc.get(id).unwrap();
    let ind = "  ".repeat(depth);
    let mut st = css::computed(n, parent_layout(doc, n).as_ref(), n.parent.is_none());
    if root {
        for k in ["position", "left", "top"] {
            st.remove(k);
        }
    }
    let name = n.name.replace('"', "'");
    match &n.kind {
        NodeKind::Frame { .. } | NodeKind::Group => {
            if n.children.is_empty() {
                out.push_str(&format!(
                    "{ind}<div data-name=\"{name}\" style={} />\n",
                    style_obj(&st)
                ));
            } else {
                out.push_str(&format!(
                    "{ind}<div data-name=\"{name}\" style={}>\n",
                    style_obj(&st)
                ));
                for c in &n.children {
                    jsx_node(doc, *c, depth + 1, false, assets, out);
                }
                out.push_str(&format!("{ind}</div>\n"));
            }
        }
        NodeKind::Text { text, .. } => {
            out.push_str(&format!(
                "{ind}<span style={}>{}</span>\n",
                style_obj(&st),
                jsx_text(text)
            ));
        }
        NodeKind::Rect | NodeKind::Ellipse => {
            if matches!(n.kind, NodeKind::Ellipse) {
                st.insert("borderRadius".into(), json!("50%"));
            }
            let img = n.style.fills.iter().rev().find_map(|p| match p {
                Paint::Image { asset, .. } => Some(asset.clone()),
                _ => None,
            });
            match img {
                Some(asset) => {
                    let ext = crate::assets::get(&asset).map_or("png", |a| a.ext);
                    let src = match assets {
                        Some(d) => d.join(format!("{asset}.{ext}")).display().to_string(),
                        None => format!("paper-asset://{asset}"),
                    };
                    st.remove("background");
                    st.insert("objectFit".into(), json!("cover"));
                    out.push_str(&format!(
                        "{ind}<img src=\"{src}\" alt=\"{name}\" style={} />\n",
                        style_obj(&st)
                    ));
                }
                None => out.push_str(&format!("{ind}<div style={} />\n", style_obj(&st))),
            }
        }
        NodeKind::Sticky { text, .. } => {
            st.insert("padding".into(), json!("16px"));
            out.push_str(&format!(
                "{ind}<div style={}>{}</div>\n",
                style_obj(&st),
                jsx_text(text)
            ));
        }
        _ => {
            // Vectors, lines, arrows, ink: inline SVG of just this node.
            let svg = crate::io::svg::export(doc, Some(&[id]), None).unwrap_or_default();
            let svg = svg
                .lines()
                .filter(|l| !l.starts_with("<?xml"))
                .collect::<Vec<_>>()
                .join("");
            let mut pos = Map::new();
            for k in ["position", "left", "top", "width", "height", "flexShrink"] {
                if let Some(v) = st.get(k) {
                    pos.insert(k.into(), v.clone());
                }
            }
            out.push_str(&format!(
                "{ind}<div data-name=\"{name}\" style={}>{}</div>\n",
                style_obj(&pos),
                svg
            ));
        }
    }
}

// ------------------------------------------------------------------------------ export

fn parse_scale(s: Option<&str>, b: Rect) -> Result<f64, String> {
    let Some(s) = s.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(2.0);
    };
    let (num, unit) = s.split_at(s.len() - 1);
    let v: f64 = num
        .parse()
        .map_err(|_| format!("bad scale `{s}` (use e.g. 2x, 512w, 512h, 720p)"))?;
    Ok(match unit {
        "x" => v,
        "w" => v / b.w.max(1.0),
        "h" => v / b.h.max(1.0),
        "p" => v / b.w.min(b.h).max(1.0),
        _ => s.parse().map_err(|_| format!("bad scale `{s}`"))?,
    })
}

fn out_dir(a: &Args) -> PathBuf {
    a.str("directory")
        .map(PathBuf::from)
        .or_else(dirs::download_dir)
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let stem = crate::project::folder_name(stem);
    let mut p = dir.join(format!("{stem}.{ext}"));
    let mut i = 2;
    while p.exists() {
        p = dir.join(format!("{stem} {i}.{ext}"));
        i += 1;
    }
    p
}

fn export(host: &mut dyn Host, a: &Args) -> ToolResult {
    use crate::io::export::{Format, Item, Scope, Spec};
    let gpu = host.gpu();
    let ed = host.editor();
    let def = page_arg(ed, a)?;
    let nodes = a
        .get("nodes")
        .and_then(Value::as_object)
        .ok_or("`nodes` must be an object of nodeId → export settings")?;
    let dir = out_dir(a);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut written = Vec::new();
    for (key, settings) in nodes {
        let (page, id) = resolve(ed, key, def)?;
        let doc = ed.page_doc(page);
        let b = crate::io::svg::export_bounds(doc, &[id]).ok_or("nothing to export")?;
        let name = doc.get(id).unwrap().name.clone();
        let list: Vec<Value> = match settings.as_array() {
            Some(v) if !v.is_empty() => v.clone(),
            _ => vec![json!({"format": "png", "scale": "2x"})],
        };
        for s in list {
            let s = Args(
                s.as_object()
                    .ok_or("each export setting must be an object")?,
            );
            let format = match s.str("format").unwrap_or("png") {
                "png" => Format::Png,
                "svg" => Format::Svg,
                "pdf" => Format::Pdf,
                f => return Err(format!("format `{f}` is not supported (png, svg, pdf)")),
            };
            let scale = parse_scale(s.str("scale"), b)?;
            let stem = if format == Format::Png && scale != 1.0 {
                format!("{name}@{}x", (scale * 100.0).round() / 100.0)
            } else {
                name.clone()
            };
            let path = unique_path(&dir, &stem, format.ext());
            let spec = Spec {
                format,
                scope: Scope::Selection,
                scale,
                transparent: false,
            };
            let item = Item {
                name: name.clone(),
                doc,
                roots: vec![id],
            };
            written.extend(crate::io::export::write(
                &spec,
                &[item],
                &path,
                gpu.as_ref(),
            )?);
        }
    }
    text(json!({"files": written}))
}

fn export_pdf(host: &mut dyn Host, a: &Args) -> ToolResult {
    use crate::io::export::{Format, Item, Scope, Spec};
    let project_name = host.project().map(|p| p.manifest.name.clone());
    let ed = host.editor();
    let (page, ids) = resolve_all(ed, &a.strs("nodeIds")?, page_arg(ed, a)?)?;
    let doc = ed.page_doc(page);
    let mut ids: Vec<(Rect, NodeId)> = ids
        .into_iter()
        .map(|id| (doc.world_rect(id).unwrap(), id))
        .collect();
    ids.sort_by(|(a, _), (b, _)| a.y.total_cmp(&b.y).then(a.x.total_cmp(&b.x)));
    let items: Vec<Item> = ids
        .iter()
        .map(|(_, id)| Item {
            name: doc.get(*id).unwrap().name.clone(),
            doc,
            roots: vec![*id],
        })
        .collect();
    let path = match a.str("path") {
        Some(p) => PathBuf::from(p),
        None => {
            let dir = out_dir(a);
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            unique_path(&dir, project_name.as_deref().unwrap_or("Sketch"), "pdf")
        }
    };
    let spec = Spec {
        format: Format::Pdf,
        scope: Scope::Selection,
        scale: 1.0,
        transparent: false,
    };
    let files = crate::io::export::write(&spec, &items, &path, None)?;
    text(json!({"file": files.first(), "pages": items.len()}))
}

// ------------------------------------------------------------------------------ writing

/// Containers whose `align-items` was explicitly not `stretch` (CSS's default). Our auto
/// layout has no stretch alignment, so this remembers which `Start`-aligned frames should not
/// stretch new children. Keyed by page id and node.
fn no_stretch() -> &'static Mutex<HashSet<(String, NodeId)>> {
    static S: OnceLock<Mutex<HashSet<(String, NodeId)>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

fn stretches(page_id: &str, n: &Node) -> bool {
    n.layout
        .is_some_and(|l| l.align_cross == crate::model::Align::Start)
        && !no_stretch()
            .lock()
            .unwrap()
            .contains(&(page_id.to_string(), n.id))
}

fn remember_stretch(page_id: &str, id: NodeId, styles: &Styles) {
    if let Some((_, v)) = styles.iter().rev().find(|(k, _)| k == "alignItems") {
        let key = (page_id.to_string(), id);
        let mut s = no_stretch().lock().unwrap();
        if matches!(v.trim(), "stretch" | "normal") {
            s.remove(&key);
        } else {
            s.insert(key);
        }
    }
}

/// Recompute a text node's box from its content (inside a transaction).
fn fit_text(tx: &mut Tx, id: NodeId, keep_height: bool) {
    let Some(n) = tx.doc().get(id) else { return };
    let NodeKind::Text { auto_width, .. } = n.kind else {
        return;
    };
    let Some(l) = crate::text::layout_node(n) else {
        return;
    };
    let w = if auto_width {
        l.width.max(1.0)
    } else {
        n.rect.w
    };
    let h = if keep_height { n.rect.h } else { l.height };
    if (n.rect.w - w).abs() > 1e-9 || (n.rect.h - h).abs() > 1e-9 {
        let n = tx.node_mut(id).unwrap();
        n.rect.w = w;
        n.rect.h = h;
    }
}

/// Lay out the outermost auto-layout frame around `id` until it settles (hug sizes depend on
/// wrapped text heights, which depend on fill widths).
fn relayout(tx: &mut Tx, id: Option<NodeId>) {
    let Some(mut top) = id.filter(|i| tx.doc().get(*i).is_some()) else {
        return;
    };
    let mut cur = Some(top);
    while let Some(c) = cur {
        let n = tx.doc().get(c).unwrap();
        if n.layout.is_some() {
            top = c;
        }
        cur = n.parent;
    }
    if tx.doc().get(top).is_some_and(|n| n.layout.is_some()) {
        for _ in 0..6 {
            if !crate::layout::layout_frame(tx, top) {
                break;
            }
        }
    }
}

/// Where new content goes and how it sizes there.
#[derive(Clone)]
struct Parent {
    id: Option<NodeId>,
    layout: Option<AutoLayout>,
    size: Option<(f64, f64)>,
    /// Size along x / y is known (not hugging its content).
    definite: [bool; 2],
    /// Children without an explicit cross size stretch (CSS `align-items: normal`).
    stretch: bool,
}

impl Parent {
    fn of(doc: &Document, page_id: &str, id: Option<NodeId>) -> Parent {
        match id.and_then(|i| doc.get(i)) {
            None => Parent {
                id: None,
                layout: None,
                size: None,
                definite: [false; 2],
                stretch: false,
            },
            Some(n) => Parent {
                id: Some(n.id),
                layout: n.layout,
                size: Some((n.rect.w, n.rect.h)),
                definite: [
                    n.layout_child.sizing[0] != Sizing::Hug,
                    n.layout_child.sizing[1] != Sizing::Hug,
                ],
                stretch: stretches(page_id, n),
            },
        }
    }
    fn ctx(&self) -> Context {
        Context {
            parent_layout: self.layout,
            parent_size: self.size,
        }
    }
    /// The cross axis, when children are laid out.
    fn cross(&self) -> Option<usize> {
        self.layout.map(|l| {
            if l.direction == Direction::Horizontal {
                1
            } else {
                0
            }
        })
    }
}

/// Resolve an image reference (paper-asset://, file://, absolute path, data: URI) to a
/// registered asset id.
fn image_asset(src: &str) -> Result<String, String> {
    let src = src.trim();
    let bytes = if let Some(data) = src.strip_prefix("data:") {
        let (meta, payload) = data.split_once(',').ok_or("malformed data: URI")?;
        if meta.ends_with(";base64") {
            crate::assets::base64_decode(payload.trim()).ok_or("bad base64 in data: URI")?
        } else {
            payload.as_bytes().to_vec()
        }
    } else if src.starts_with("http://") || src.starts_with("https://") {
        return Err(format!(
            "{src}: remote images aren't fetched; download it and use paper-asset:///absolute/path"
        ));
    } else {
        let path = src
            .strip_prefix("paper-asset://")
            .or_else(|| src.strip_prefix("file://"))
            .unwrap_or(src);
        // An asset id already registered (e.g. from get_jsx output).
        if crate::assets::get(path).is_some() {
            return Ok(path.to_string());
        }
        let path = percent_decode(path);
        std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?
    };
    if bytes.starts_with(b"<svg") || bytes.starts_with(b"<?xml") {
        return Err("SVG images: put the <svg> markup inline instead".into());
    }
    crate::assets::register(&bytes)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) =
                u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// User-agent defaults for an element (before its inline style).
fn ua_styles(tag: &str) -> Styles {
    let s = |v: &[(&str, &str)]| {
        v.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    };
    match tag {
        "h1" => s(&[("fontSize", "32px"), ("fontWeight", "700")]),
        "h2" => s(&[("fontSize", "24px"), ("fontWeight", "700")]),
        "h3" => s(&[("fontSize", "20px"), ("fontWeight", "700")]),
        "h4" | "h5" | "h6" | "th" => s(&[("fontWeight", "700")]),
        "b" | "strong" => s(&[("fontWeight", "700")]),
        "i" | "em" | "cite" => s(&[("fontStyle", "italic")]),
        "u" | "ins" => s(&[("textDecoration", "underline")]),
        "s" | "del" => s(&[("textDecoration", "line-through")]),
        "small" => s(&[("fontSize", "13px")]),
        "code" | "kbd" => s(&[("fontSize", "14px")]),
        "pre" => s(&[("fontSize", "14px"), ("whiteSpace", "pre")]),
        "button" | "nav" | "header" | "footer" | "section" | "main" | "article" | "aside" => {
            Vec::new()
        }
        _ => Vec::new(),
    }
}

const INHERITED: &[&str] = &[
    "color",
    "fontSize",
    "fontWeight",
    "fontStyle",
    "lineHeight",
    "letterSpacing",
    "textAlign",
    "fontFamily",
    "whiteSpace",
    "textDecoration",
];

fn get<'s>(st: &'s Styles, k: &str) -> Option<&'s str> {
    st.iter()
        .rev()
        .find(|(kk, _)| kk == k)
        .map(|(_, v)| v.as_str())
}

/// Inherited text properties for children, with font-size resolved to px.
fn inherit(parent: &Styles, own: &Styles) -> Styles {
    let pfs = get(parent, "fontSize")
        .and_then(|v| css::length(v, 16.0))
        .unwrap_or(16.0);
    let mut out: Styles = parent.clone();
    for (k, v) in own {
        if INHERITED.contains(&k.as_str()) {
            let v = if k == "fontSize" {
                css::length(v, pfs).map_or(v.clone(), |px| format!("{px}px"))
            } else {
                v.clone()
            };
            out.push((k.clone(), v));
        }
    }
    out
}

/// Inline content as a styled character stream.
#[derive(Clone, Copy, Default, PartialEq)]
struct Run {
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    strike: Option<bool>,
    color: Option<Color>,
    size: Option<f64>,
}

fn text_only(el: &Element) -> bool {
    el.children.iter().all(|c| match c {
        Html::Text(_) => true,
        Html::Element(e) => html::is_inline(&e.tag) && text_only(e),
    })
}

fn is_flex(st: &Styles) -> bool {
    get(st, "display").is_some_and(|d| d.contains("flex"))
}

fn has_element_children(el: &Element) -> bool {
    el.children
        .iter()
        .any(|c| matches!(c, Html::Element(e) if e.tag != "br"))
}

fn has_text(el: &Element) -> bool {
    el.children.iter().any(|c| match c {
        Html::Text(t) => !t.trim().is_empty(),
        Html::Element(e) => e.tag == "br" || has_text(e),
    })
}

fn collect_runs(nodes: &[Html], run: Run, base_fs: f64, out: &mut Vec<(char, Run)>) {
    for c in nodes {
        match c {
            Html::Text(t) => out.extend(t.chars().map(|ch| (ch, run))),
            Html::Element(e) if e.tag == "br" => out.push(('\u{2028}', run)),
            Html::Element(e) => {
                let mut st = ua_styles(&e.tag);
                st.extend(css::parse_inline(e.attr("style").unwrap_or("")));
                let mut r = run;
                for (k, v) in &st {
                    let lower = v.to_ascii_lowercase();
                    match k.as_str() {
                        "fontWeight" => {
                            r.bold = Some(
                                lower == "bold" || lower.parse::<f64>().is_ok_and(|w| w >= 600.0),
                            )
                        }
                        "fontStyle" => r.italic = Some(lower == "italic"),
                        "textDecoration" | "textDecorationLine" => {
                            r.underline = Some(lower.contains("underline"));
                            r.strike = Some(lower.contains("line-through"));
                        }
                        "color" => r.color = css::color(v).or(r.color),
                        "fontSize" => {
                            r.size = css::length(v, run.size.unwrap_or(base_fs)).or(r.size)
                        }
                        _ => {}
                    }
                }
                collect_runs(&e.children, r, base_fs, out);
            }
        }
    }
}

/// Collapse whitespace like CSS `white-space: normal` (hard breaks from `<br>` survive).
fn collapse(chars: Vec<(char, Run)>, pre: bool) -> Vec<(char, Run)> {
    if pre {
        return chars
            .into_iter()
            .map(|(c, r)| (if c == '\u{2028}' { '\n' } else { c }, r))
            .collect();
    }
    let mut out: Vec<(char, Run)> = Vec::new();
    for (c, r) in chars {
        if c == '\u{2028}' {
            while out.last().is_some_and(|(c, _)| *c == ' ') {
                out.pop();
            }
            out.push(('\n', r));
        } else if c.is_whitespace() && c != '\u{a0}' {
            if !out.is_empty() && !matches!(out.last(), Some((' ' | '\n', _))) {
                out.push((' ', r));
            }
        } else {
            out.push((if c == '\u{a0}' { ' ' } else { c }, r));
        }
    }
    while out.last().is_some_and(|(c, _)| *c == ' ') {
        out.pop();
    }
    out
}

struct Builder<'t, 'a> {
    tx: &'t mut Tx<'a>,
    page_id: String,
    ignored: BTreeSet<String>,
    warnings: Vec<String>,
}

impl Builder<'_, '_> {
    fn insert(&mut self, n: Node, parent: &Parent, index: &mut Option<usize>) -> NodeId {
        let id = self.tx.insert(n, parent.id, *index);
        if let Some(i) = index {
            *i += 1;
        }
        id
    }

    fn note(&mut self, a: &css::Applied) {
        self.ignored.extend(a.ignored.iter().cloned());
    }

    /// Positioning that needs the final size (right/bottom).
    fn place(&mut self, id: NodeId, a: &css::Applied, parent: &Parent) {
        if let (Some(ps), true) = (parent.size, a.right.is_some() || a.bottom.is_some()) {
            let n = self.tx.node_mut(id).unwrap();
            if let Some(r) = a.right {
                n.rect.x = ps.0 - n.rect.w - r;
            }
            if let Some(b) = a.bottom {
                n.rect.y = ps.1 - n.rect.h - b;
            }
        }
    }

    /// Default sizing before explicit styles: stretch on the cross axis when the parent's
    /// size there is definite.
    fn stretch_default(parent: &Parent, n: &mut Node, styles: &Styles) {
        let Some(ca) = parent.cross() else { return };
        let explicit = get(styles, if ca == 0 { "width" } else { "height" }).is_some()
            || get(styles, "alignSelf").is_some_and(|v| v != "stretch")
            || matches!(get(styles, "position"), Some("absolute" | "fixed"));
        if parent.stretch && parent.definite[ca] && !explicit {
            n.layout_child.sizing[ca] = Sizing::Fill;
            if ca == 0
                && let NodeKind::Text { auto_width, .. } = &mut n.kind
            {
                *auto_width = false;
            }
            if let Some(ps) = parent.size {
                let pad = parent.layout.map_or([0.0; 4], |l| l.padding);
                if ca == 0 {
                    n.rect.w = (ps.0 - pad[1] - pad[3]).max(1.0);
                } else {
                    n.rect.h = (ps.1 - pad[0] - pad[2]).max(1.0);
                }
            }
        }
    }

    /// Lay out a list of children. In a flex container (`flex_items`) every child element and
    /// every text run is its own item (CSS blockification); elsewhere consecutive inline
    /// content flows into one text layer.
    fn nodes(
        &mut self,
        list: &[Html],
        inherited: &Styles,
        parent: &Parent,
        index: &mut Option<usize>,
        flex_items: bool,
    ) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut inline: Vec<Html> = Vec::new();
        let flush = |b: &mut Self,
                     inline: &mut Vec<Html>,
                     out: &mut Vec<NodeId>,
                     index: &mut Option<usize>| {
            if inline.is_empty() {
                return;
            }
            let el = Element {
                tag: "span".into(),
                children: std::mem::take(inline),
                ..Default::default()
            };
            if has_text(&el) {
                out.extend(b.text(&el, &Vec::new(), inherited, parent, index));
            }
        };
        for h in list {
            match h {
                Html::Element(e) if flex_items || !html::is_inline(&e.tag) || !text_only(e) => {
                    flush(self, &mut inline, &mut out, index);
                    out.extend(self.element(e, inherited, parent, index));
                }
                other => {
                    inline.push(other.clone());
                    if flex_items {
                        flush(self, &mut inline, &mut out, index);
                    }
                }
            }
        }
        flush(self, &mut inline, &mut out, index);
        out
    }

    fn element(
        &mut self,
        el: &Element,
        inherited: &Styles,
        parent: &Parent,
        index: &mut Option<usize>,
    ) -> Option<NodeId> {
        let mut own = ua_styles(&el.tag);
        own.extend(css::parse_inline(el.attr("style").unwrap_or("")));
        match el.tag.as_str() {
            "head" | "meta" | "link" | "title" | "script" | "style" => None,
            "html" | "body" => {
                // Treat as transparent wrappers.
                let inh = inherit(inherited, &own);
                let ids = self.nodes(&el.children, &inh, parent, index, false);
                ids.first().copied()
            }
            "img" => self.image(el, &own, parent, index),
            "svg" => self.svg(el, &own, inherited, parent, index),
            "x-sketch-clone" | "x-paper-clone" | "paper-clone" => {
                self.clone_el(el, &own, parent, index)
            }
            "hr" => {
                let mut st: Styles = vec![
                    ("height".into(), "1px".into()),
                    ("backgroundColor".into(), "#e0e0e0".into()),
                ];
                st.extend(own);
                self.frame(el, &st, inherited, parent, index, false)
            }
            _ if text_only(el) && has_text(el) && !(is_flex(&own) && has_element_children(el)) => {
                let boxy = own.iter().any(|(k, v)| {
                    k.starts_with("background")
                        || k.starts_with("border") && v != "none" && v != "0"
                        || k.starts_with("padding")
                        || k == "display" && v.contains("flex")
                }) || matches!(el.tag.as_str(), "button" | "li" | "a")
                    && own.iter().any(|(k, _)| k.starts_with("padding"));
                if boxy {
                    self.frame(el, &own, inherited, parent, index, true)
                } else {
                    self.text(el, &own, inherited, parent, index)
                        .into_iter()
                        .next()
                }
            }
            _ => self.frame(el, &own, inherited, parent, index, false),
        }
    }

    fn layer_name(el: &Element) -> Option<String> {
        el.attr("layer-name")
            .or_else(|| el.attr("data-name"))
            .or_else(|| el.attr("aria-label"))
            .or_else(|| el.attr("id"))
            .map(|s| s.chars().take(50).collect())
    }

    /// A text layer for an element whose content is inline only.
    fn text(
        &mut self,
        el: &Element,
        own: &Styles,
        inherited: &Styles,
        parent: &Parent,
        index: &mut Option<usize>,
    ) -> Option<NodeId> {
        let mut styles = inherited.clone();
        styles.extend(own.iter().cloned());
        let fs = get(&styles, "fontSize")
            .and_then(|v| css::length(v, 16.0))
            .unwrap_or(16.0);
        let pre = get(&styles, "whiteSpace").is_some_and(|v| v.starts_with("pre"));
        let mut chars = Vec::new();
        collect_runs(&el.children, Run::default(), fs, &mut chars);
        let chars = collapse(chars, pre);
        if chars.is_empty() {
            return None;
        }
        let content: String = chars.iter().map(|(c, _)| *c).collect();
        let mut n = Node::new(
            NodeId(0),
            NodeKind::Text {
                text: content.clone(),
                font_size: fs,
                auto_width: true,
                align: TextAlign::Left,
            },
            Rect::new(0.0, 0.0, 1.0, 1.0),
        );
        n.id = self.tx.alloc_id();
        n.style.set_fill(Some(Color::BLACK));
        n.name = Self::layer_name(el).unwrap_or_else(|| {
            content
                .lines()
                .next()
                .unwrap_or("Text")
                .chars()
                .take(30)
                .collect()
        });
        Self::stretch_default(parent, &mut n, &styles);
        let a = css::apply(&mut n, &styles, &parent.ctx());
        self.note(&a);
        // Rich text runs.
        let mut i = 0;
        while i < chars.len() {
            let r = chars[i].1;
            let mut j = i + 1;
            while j < chars.len() && chars[j].1 == r {
                j += 1;
            }
            if r != Run::default() {
                n.text_style.apply_range(i, j, |s| {
                    if r.bold.is_some() {
                        s.bold = r.bold;
                    }
                    if r.italic.is_some() {
                        s.italic = r.italic;
                    }
                    if r.underline.is_some() {
                        s.underline = r.underline;
                    }
                    if r.strike.is_some() {
                        s.strike = r.strike;
                    }
                    if r.color.is_some() {
                        s.color = r.color;
                    }
                    if r.size.is_some() {
                        s.size = r.size;
                    }
                });
            }
            i = j;
        }
        n.text_style.normalize();
        let keep_h = get(own, "height").is_some_and(|v| css::length(v, fs).is_some());
        let id = self.insert(n, parent, index);
        fit_text(self.tx, id, keep_h);
        self.place(id, &a, parent);
        Some(id)
    }

    /// A frame for a container element (`wrap_text`: its inline content becomes one text child).
    fn frame(
        &mut self,
        el: &Element,
        own: &Styles,
        inherited: &Styles,
        parent: &Parent,
        index: &mut Option<usize>,
        wrap_text: bool,
    ) -> Option<NodeId> {
        let mut n = Node::new(
            NodeId(0),
            NodeKind::Frame { clip: false },
            Rect::new(0.0, 0.0, 0.0, 0.0),
        );
        n.id = self.tx.alloc_id();
        n.style.fills.clear();
        n.name = Self::layer_name(el).unwrap_or_else(|| match el.tag.as_str() {
            "div" | "span" => "Frame".into(),
            t => {
                let mut c = t.chars();
                c.next()
                    .map_or("Frame".into(), |f| f.to_uppercase().chain(c).collect())
            }
        });
        let flex = get(own, "display").is_some_and(|d| d.contains("flex"));
        n.layout = Some(AutoLayout {
            direction: if flex {
                Direction::Horizontal
            } else {
                Direction::Vertical
            },
            gap: 0.0,
            cross_gap: 0.0,
            padding: [0.0; 4],
            ..AutoLayout::default()
        });
        n.layout_child.sizing = [Sizing::Hug, Sizing::Hug];
        Self::stretch_default(parent, &mut n, own);
        let a = css::apply(&mut n, own, &parent.ctx());
        self.note(&a);
        if let Some((src, mode)) = &a.image {
            match image_asset(src) {
                Ok(asset) => n.style.fills.push(Paint::Image {
                    asset,
                    mode: *mode,
                    opacity: 1.0,
                }),
                Err(e) => self.warnings.push(e),
            }
        }
        let id = self.insert(n, parent, index);
        remember_stretch(&self.page_id, id, own);
        let me = Parent::of(self.tx.doc(), &self.page_id, Some(id));
        let inh = inherit(inherited, own);
        let mut child_index = None;
        if wrap_text {
            let inner = Element {
                tag: "span".into(),
                children: el.children.clone(),
                ..Default::default()
            };
            self.text(&inner, &Vec::new(), &inh, &me, &mut child_index);
        } else {
            self.nodes(&el.children, &inh, &me, &mut child_index, is_flex(own));
        }
        relayout(self.tx, Some(id));
        self.place(id, &a, parent);
        Some(id)
    }

    fn image(
        &mut self,
        el: &Element,
        own: &Styles,
        parent: &Parent,
        index: &mut Option<usize>,
    ) -> Option<NodeId> {
        let src = el.attr("src").unwrap_or("");
        let asset = match image_asset(src) {
            Ok(a) => Some(a),
            Err(e) => {
                self.warnings.push(e);
                None
            }
        };
        let natural = asset
            .as_deref()
            .and_then(crate::assets::get)
            .map(|a| (a.width as f64, a.height as f64))
            .unwrap_or((100.0, 100.0));
        let attr_px = |k: &str| el.attr(k).and_then(|v| css::length(v, 16.0));
        let (mut w, mut h) = (attr_px("width"), attr_px("height"));
        if w.is_none() && h.is_none() {
            let s = (1000.0 / natural.0.max(natural.1)).min(1.0);
            (w, h) = (Some(natural.0 * s), Some(natural.1 * s));
        }
        let w2 = w.unwrap_or_else(|| h.unwrap() * natural.0 / natural.1);
        let h2 = h.unwrap_or_else(|| w2 * natural.1 / natural.0);
        let mut n = Node::new(NodeId(0), NodeKind::Rect, Rect::new(0.0, 0.0, w2, h2));
        n.id = self.tx.alloc_id();
        n.name = Self::layer_name(el)
            .or_else(|| el.attr("alt").map(String::from))
            .unwrap_or_else(|| "Image".into());
        n.style.fills = match &asset {
            Some(a) => vec![Paint::Image {
                asset: a.clone(),
                mode: ImageMode::Fill,
                opacity: 1.0,
            }],
            None => vec![Paint::solid(Color::rgb(0xd9, 0xd9, 0xd9))],
        };
        Self::stretch_default(parent, &mut n, own);
        let has_w = get(own, "width").is_some();
        let a = css::apply(&mut n, own, &parent.ctx());
        // Keep the aspect ratio when only the width was styled.
        if has_w && get(own, "height").is_none() && n.layout_child.sizing[0] != Sizing::Fill {
            n.rect.h = n.rect.w * natural.1 / natural.0;
        }
        self.note(&a);
        let id = self.insert(n, parent, index);
        self.place(id, &a, parent);
        Some(id)
    }

    fn svg(
        &mut self,
        el: &Element,
        own: &Styles,
        inherited: &Styles,
        parent: &Parent,
        index: &mut Option<usize>,
    ) -> Option<NodeId> {
        let raw = el.raw.clone().unwrap_or_default();
        let color = get(own, "color")
            .or_else(|| get(inherited, "color"))
            .and_then(css::color)
            .unwrap_or(Color::BLACK);
        let fs = 16.0;
        let size_of = |k: &str| {
            get(own, k)
                .and_then(|v| css::length(v, fs))
                .or_else(|| el.attr(k).and_then(|v| css::length(v, fs)))
        };
        let (w, h) = (size_of("width"), size_of("height"));
        // Rebuild the opening tag with the final size so the importer scales the content.
        let body = raw.find('>').map_or("", |i| &raw[i + 1..]);
        let self_closing = raw[..raw.find('>').unwrap_or(0)].ends_with('/');
        let mut open = String::from("<svg");
        for (k, v) in &el.attrs {
            if matches!(k.as_str(), "width" | "height" | "style") {
                continue;
            }
            open += &format!(" {}=\"{}\"", svg_attr_name(k), v.replace('"', "&quot;"));
        }
        if !el.attrs.iter().any(|(k, _)| k == "xmlns") {
            open += " xmlns=\"http://www.w3.org/2000/svg\"";
        }
        if let Some(w) = w {
            open += &format!(" width=\"{w}\"");
        }
        if let Some(h) = h {
            open += &format!(" height=\"{h}\"");
        }
        let src = format!("{open}>{}", if self_closing { "</svg>" } else { body })
            .replace("currentColor", &color.to_hex());
        let shapes = match crate::io::svg_import::parse(&src) {
            Ok(s) => s,
            Err(e) => {
                self.warnings.push(format!("<svg>: {e}"));
                return None;
            }
        };
        let b =
            Rect::union_all(&shapes.iter().map(|n| n.rect).collect::<Vec<_>>()).unwrap_or_default();
        let fw = w.unwrap_or(b.x.max(0.0) + b.w);
        let fh = h.unwrap_or(b.y.max(0.0) + b.h);
        let mut f = Node::new(
            NodeId(0),
            NodeKind::Frame { clip: false },
            Rect::new(0.0, 0.0, fw, fh),
        );
        f.id = self.tx.alloc_id();
        f.style.fills.clear();
        f.name = Self::layer_name(el).unwrap_or_else(|| "Icon".into());
        let st: Styles = own
            .iter()
            .filter(|(k, _)| k != "width" && k != "height")
            .cloned()
            .collect();
        let a = css::apply(&mut f, &st, &parent.ctx());
        self.note(&a);
        let fid = self.insert(f, parent, index);
        for mut s in shapes {
            s.id = self.tx.alloc_id();
            s.children.clear();
            self.tx.insert(s, Some(fid), None);
        }
        self.place(fid, &a, parent);
        Some(fid)
    }

    fn clone_el(
        &mut self,
        el: &Element,
        own: &Styles,
        parent: &Parent,
        index: &mut Option<usize>,
    ) -> Option<NodeId> {
        let key = el.attr("node-id").unwrap_or("");
        let num = key.rsplit(':').next().and_then(|n| n.parse::<u64>().ok());
        let Some(src) = num
            .map(NodeId)
            .filter(|id| self.tx.doc().get(*id).is_some())
        else {
            self.warnings.push(format!(
                "x-sketch-clone: node `{key}` not found on this page"
            ));
            return None;
        };
        let (id, _) = clone_subtree(self.tx, src, parent.id, *index, V2::ZERO);
        if let Some(i) = index {
            *i += 1;
        }
        let ctx = parent.ctx();
        let n = self.tx.node_mut(id).unwrap();
        if parent.layout.is_some() {
            n.layout_child.absolute = false;
        }
        let a = css::apply(n, own, &ctx);
        self.note(&a);
        if let Some(name) = Self::layer_name(el) {
            self.tx.node_mut(id).unwrap().name = name;
        }
        Some(id)
    }
}

/// Deep-copy a subtree under `parent` (keeping its world position plus `offset`). Returns the
/// new root and the (old → new) id pairs.
fn clone_subtree(
    tx: &mut Tx,
    src: NodeId,
    parent: Option<NodeId>,
    index: Option<usize>,
    offset: V2,
) -> (NodeId, Vec<(NodeId, NodeId)>) {
    let ids = tx.doc().subtree(src);
    let nodes: Vec<Node> = ids
        .iter()
        .map(|i| tx.doc().get(*i).unwrap().clone())
        .collect();
    let mut map = HashMap::new();
    for n in &nodes {
        map.insert(n.id, tx.alloc_id());
    }
    let world = tx.doc().world_rect(src).unwrap();
    let origin = tx.doc().parent_origin(parent);
    for n in &nodes {
        let mut c = n.clone();
        c.id = map[&n.id];
        c.children = n.children.iter().map(|x| map[x]).collect();
        if let NodeKind::Connector { start, end, .. } = &mut c.kind {
            for b in [start, end].into_iter().flatten() {
                if let Some(nn) = map.get(&b.node) {
                    b.node = *nn;
                }
            }
        }
        if n.id == src {
            c.rect = world.translate(offset - origin);
            tx.insert(c, parent, index);
        } else {
            c.parent = n.parent.map(|p| map[&p]);
            tx.put(c);
        }
    }
    for id in map.values() {
        crate::editor::whiteboard::update_connector(tx, *id);
    }
    let pairs = ids.iter().map(|i| (*i, map[i])).collect();
    (map[&src], pairs)
}

/// A free spot for a new top-level frame: right of the existing artboards, 80 px apart.
fn free_spot(doc: &Document, w: f64) -> V2 {
    let rects: Vec<Rect> = doc
        .root
        .iter()
        .filter_map(|id| doc.world_rect(*id))
        .collect();
    match Rect::union_all(&rects) {
        None => V2::ZERO,
        Some(b) => {
            let _ = w;
            v2((b.x + b.w + 80.0).round(), b.y.round())
        }
    }
}

fn create_artboard(ed: &mut Editor, a: &Args) -> ToolResult {
    let page = page_arg(ed, a)?;
    let name = a.str("name").unwrap_or("Artboard").to_string();
    let styles = css::from_json(
        a.get("styles")
            .ok_or("missing `styles` (width and height are required)")?,
    )?;
    if get(&styles, "width").is_none() || get(&styles, "height").is_none() {
        return Err("styles.width and styles.height are required (e.g. \"1440px\")".into());
    }
    let page_id = ed.pages[page].id.clone();
    let (id, ignored) = ed.with_page(page, |ed| {
        ed.edit("Create artboard", |tx| {
            let mut n = Node::new(
                NodeId(0),
                NodeKind::Frame { clip: true },
                Rect::new(0.0, 0.0, 100.0, 100.0),
            );
            n.id = tx.alloc_id();
            n.name = name.chars().take(50).collect();
            n.layout = Some(AutoLayout {
                direction: Direction::Vertical,
                gap: 0.0,
                cross_gap: 0.0,
                padding: [0.0; 4],
                ..AutoLayout::default()
            });
            let a = css::apply(&mut n, &styles, &Context::default());
            let mut ignored = a.ignored.clone();
            if let Some((src, mode)) = &a.image {
                match image_asset(src) {
                    Ok(asset) => n.style.fills.push(Paint::Image {
                        asset,
                        mode: *mode,
                        opacity: 1.0,
                    }),
                    Err(e) => ignored.push(e),
                }
            }
            if get(&styles, "left").is_none() && get(&styles, "top").is_none() {
                let p = free_spot(tx.doc(), n.rect.w);
                n.rect.x = p.x;
                n.rect.y = p.y;
            }
            let id = tx.insert(n, None, None);
            remember_stretch(&page_id, id, &styles);
            relayout(tx, Some(id));
            (id, ignored)
        })
    });
    let mut v = json!({"id": node_key(ed, page, id), "name": name, "pageId": ed.pages[page].id});
    if !ignored.is_empty() {
        v["ignoredStyles"] = json!(ignored);
    }
    text(v)
}

fn write_html(ed: &mut Editor, a: &Args) -> ToolResult {
    let def = page_arg(ed, a)?;
    let src = a.req_str("html")?;
    let target = a.req_str("targetNodeId")?;
    let mode = a.str("mode").unwrap_or("insert-children");
    let (page, target) = if target == ROOT {
        (def, None)
    } else {
        let (p, id) = resolve(ed, target, def)?;
        (p, Some(id))
    };
    let replace = match mode {
        "insert-children" | "insert" | "append" => false,
        "replace" => true,
        m => {
            return Err(format!(
                "mode must be \"insert-children\" or \"replace\", not `{m}`"
            ));
        }
    };
    if replace && target.is_none() {
        return Err("replace needs a targetNodeId (not \"root\")".into());
    }
    if let Some(t) = target
        && !replace
        && !ed.page_doc(page).get(t).unwrap().is_container()
    {
        return Err("insert-children needs a frame or group target; use mode \"replace\" or target its parent".into());
    }
    let tree = html::parse(src);
    let page_id = ed.pages[page].id.clone();
    let (created, ignored, warnings) = ed.with_page(page, |ed| {
        ed.edit("Write HTML", |tx| {
            // Where the new content goes.
            let (parent_id, mut index, old) = match (replace, target) {
                (true, Some(t)) => {
                    let n = tx.doc().get(t).unwrap();
                    let p = n.parent;
                    let i = tx.doc().children(p).iter().position(|c| *c == t);
                    (p, i, Some(n.clone()))
                }
                (_, t) => (t, None, None),
            };
            if let Some(t) = target.filter(|_| replace) {
                tx.remove(t);
            }
            let parent = Parent::of(tx.doc(), &page_id, parent_id);
            let mut inherited: Styles = Vec::new();
            // Text inherits the target's text color when inserting into a text-less frame.
            if let Some(o) = &old
                && let Some(c) = o.style.fill().filter(|_| o.kind.text().is_some())
            {
                inherited.push(("color".into(), c.to_hex()));
            }
            let mut b = Builder {
                tx,
                page_id: page_id.clone(),
                ignored: BTreeSet::new(),
                warnings: Vec::new(),
            };
            let flex = parent.layout.is_some();
            let made = b.nodes(&tree, &inherited, &parent, &mut index, flex);
            // Root-level content without a position goes where the replaced node was, or
            // into free space.
            if parent_id.is_none() {
                for (k, id) in made.iter().enumerate() {
                    let n = b.tx.doc().get(*id).unwrap();
                    if let Some(o) = &old
                        && k == 0
                    {
                        let (x, y) = (o.rect.x, o.rect.y);
                        let n = b.tx.node_mut(*id).unwrap();
                        n.rect.x = x;
                        n.rect.y = y;
                    } else if n.rect.x == 0.0 && n.rect.y == 0.0 {
                        let w = n.rect.w;
                        let p = free_spot(&doc_without(b.tx.doc(), *id), w);
                        let n = b.tx.node_mut(*id).unwrap();
                        n.rect.x = p.x;
                        n.rect.y = p.y;
                    }
                }
            }
            relayout(b.tx, parent_id.or(made.first().copied()));
            let (ig, w) = (b.ignored, b.warnings);
            (made, ig, w)
        })
    });
    if created.is_empty() && !warnings.is_empty() {
        return Err(warnings.join("; "));
    }
    let mut v = json!({
        "createdNodeIds": created.iter().map(|id| node_key(ed, page, *id)).collect::<Vec<_>>(),
        "pageId": ed.pages[page].id,
    });
    if let Some(t) = target {
        let parent = ed
            .page_doc(page)
            .get(created.first().copied().unwrap_or(t))
            .and_then(|n| n.parent);
        v["parentId"] = json!(parent.map_or(ROOT.into(), |p| node_key(ed, page, p)));
    }
    if !ignored.is_empty() {
        v["ignoredStyles"] = json!(ignored);
    }
    if !warnings.is_empty() {
        v["warnings"] = json!(warnings);
    }
    text(v)
}

/// The page without one root node (for placing that node in free space).
fn doc_without(doc: &Document, id: NodeId) -> Document {
    Document {
        root: doc.root.iter().copied().filter(|r| *r != id).collect(),
        ..doc.clone()
    }
}

fn head(s: Option<&str>, default: Head) -> Result<Head, String> {
    Ok(match s.map(str::to_ascii_lowercase).as_deref() {
        None => default,
        Some("none") => Head::None,
        Some("arrow") => Head::Arrow,
        Some("triangle") => Head::Triangle,
        Some("dot" | "circle") => Head::Dot,
        Some("bar") => Head::Bar,
        Some("diamond") => Head::Diamond,
        Some(o) => return Err(format!("unknown arrowhead `{o}`")),
    })
}

fn create_shape(ed: &mut Editor, a: &Args) -> ToolResult {
    let def = page_arg(ed, a)?;
    let ty = a.req_str("type")?.to_ascii_lowercase();
    let (mut page, mut parent) = (def, None);
    if let Some(p) = a.str("parentId").filter(|p| !p.is_empty() && *p != ROOT) {
        let (pg, id) = resolve(ed, p, def)?;
        if !ed.page_doc(pg).get(id).unwrap().is_container() {
            return Err("parentId must be a frame or group".into());
        }
        (page, parent) = (pg, Some(id));
    }
    let bind = |k: &str| -> Result<Option<NodeId>, String> {
        match a.str(k).filter(|s| !s.is_empty()) {
            None => Ok(None),
            Some(s) => {
                let (pg, id) = resolve(ed, s, page)?;
                if pg != page {
                    return Err(format!("{k} is on another page"));
                }
                Ok(Some(id))
            }
        }
    };
    let (start_node, end_node) = (bind("startNodeId")?, bind("endNodeId")?);
    let points: Vec<V2> = a
        .get("points")
        .and_then(Value::as_array)
        .map(|v| {
            v.iter()
                .filter_map(|p| {
                    let p = p.as_array()?;
                    Some(v2(p.first()?.as_f64()?, p.get(1)?.as_f64()?))
                })
                .collect()
        })
        .unwrap_or_default();
    let styles = match a.get("styles") {
        Some(s) => css::from_json(s)?,
        None => Vec::new(),
    };
    let sides = a.num("sides").map(|s| s.clamp(3.0, 64.0) as u32);
    let txt = a.str("text").unwrap_or("").to_string();
    let (x, y) = (a.num("x").unwrap_or(0.0), a.num("y").unwrap_or(0.0));
    let (w, h) = (a.num("width"), a.num("height"));
    let route = match a.str("route") {
        Some("curved") => Route::Curved,
        Some("elbow") => Route::Elbow,
        _ => Route::Straight,
    };
    let (start_head, end_head) = (
        head(a.str("startHead"), Head::None)?,
        head(a.str("endHead"), Head::Arrow)?,
    );
    let name = a.str("name").map(String::from);
    let sketch = ed.sketch;
    let page_id = ed.pages[page].id.clone();
    let (id, ignored) = ed.with_page(page, |ed| {
        ed.edit("Add shape", |tx| -> Result<(NodeId, Vec<String>), String> {
            let o = tx.doc().parent_origin(parent);
            let rect = Rect::new(x, y, w.unwrap_or(160.0), h.unwrap_or(100.0));
            let kind = match ty.as_str() {
                "rectangle" | "rect" => NodeKind::Rect,
                "ellipse" | "circle" => NodeKind::Ellipse,
                "diamond" => NodeKind::Polygon { sides: 4 },
                "triangle" => NodeKind::Polygon { sides: 3 },
                "polygon" => NodeKind::Polygon { sides: sides.unwrap_or(6) },
                "star" => NodeKind::Star { points: sides.unwrap_or(5), inner: 0.38 },
                "sticky" | "note" => NodeKind::Sticky { text: txt.clone(), font_size: 24.0 },
                "text" => NodeKind::Text { text: txt.clone(), font_size: 20.0, auto_width: w.is_none(), align: TextAlign::Left },
                "line" | "arrow" => NodeKind::Line { a: V2::ZERO, b: V2::ZERO },
                t => return Err(format!("unknown shape type `{t}`")),
            };
            let mut n = Node::new(NodeId(0), kind, rect);
            n.id = tx.alloc_id();
            if ty == "sticky" || ty == "note" {
                n.rect.w = w.unwrap_or(200.0);
                n.rect.h = h.unwrap_or(200.0);
            }
            if let Some(nm) = &name {
                n.name = nm.chars().take(50).collect();
            } else if !txt.is_empty() {
                n.name = txt.chars().take(30).collect();
            }
            if ty == "line" || ty == "arrow" {
                // World points (parent-relative when a parent is given), else between the bound shapes.
                let mut pts: Vec<V2> = points.iter().map(|p| *p + o).collect();
                let center = |id: Option<NodeId>| id.and_then(|i| tx.doc().world_rect(i)).map(|r| r.center());
                if pts.len() < 2 {
                    match (center(start_node), center(end_node)) {
                        (Some(s), Some(e)) => pts = vec![s, e],
                        _ => return Err("lines and arrows need `points` ([[x1,y1],[x2,y2]]) or both startNodeId and endNodeId".into()),
                    }
                }
                let b = Rect::union_all(&pts.iter().map(|p| Rect::new(p.x, p.y, 0.0, 0.0)).collect::<Vec<_>>()).unwrap();
                n.rect = Rect::new(b.x - o.x, b.y - o.y, b.w.max(1.0), b.h.max(1.0));
                if ty == "arrow" || start_node.is_some() || end_node.is_some() {
                    let local: Vec<V2> = pts.iter().map(|p| *p - b.min()).collect();
                    let binding = |id: Option<NodeId>| id.map(|node| Binding { node, fixed: v2(0.5, 0.5), inside: false });
                    let style = n.style.clone();
                    n.kind = NodeKind::Connector {
                        points: local,
                        start: binding(start_node),
                        end: binding(end_node),
                        route,
                        start_head,
                        end_head: if ty == "line" { head(None, Head::None)? } else { end_head },
                    };
                    n.style = style;
                    n.name = name.clone().unwrap_or_else(|| "Arrow".into());
                } else if pts.len() == 2 {
                    n.kind = NodeKind::Line { a: pts[0] - b.min(), b: pts[1] - b.min() };
                } else {
                    n.kind = NodeKind::Path {
                        subpaths: vec![SubPath { points: pts.iter().map(|p| PathPoint::corner(*p - b.min())).collect(), closed: false }],
                        fill_rule: Default::default(),
                    };
                    n.style.fills.clear();
                    n.style.stroke = Some(Color::INK);
                    n.style.stroke_width = 2.0;
                }
            }
            if let Some(r) = sketch
                && !matches!(n.kind, NodeKind::Text { .. } | NodeKind::Sticky { .. })
            {
                n.style.rough = Some(crate::model::rough::Rough { seed: (n.id.0 as u32).wrapping_mul(2_654_435_761).max(1), ..r });
            }
            let ctx = Parent::of(tx.doc(), &page_id, parent).ctx();
            if parent.is_some() {
                n.layout_child.absolute = ctx.parent_layout.is_some();
            }
            let applied = css::apply(&mut n, &styles, &ctx);
            let mut ignored = applied.ignored.clone();
            if let Some((src, mode)) = &applied.image {
                match image_asset(src) {
                    Ok(asset) => n.style.fills.push(Paint::Image { asset, mode: *mode, opacity: 1.0 }),
                    Err(e) => ignored.push(e),
                }
            }
            let id = tx.insert(n, parent, None);
            fit_text(tx, id, false);
            crate::editor::whiteboard::update_connector(tx, id);
            Ok((id, ignored))
        })
    })?;
    let mut v = json!({"id": node_key(ed, page, id), "pageId": ed.pages[page].id});
    if !ignored.is_empty() {
        v["ignoredStyles"] = json!(ignored);
    }
    text(v)
}

fn update_styles(ed: &mut Editor, a: &Args) -> ToolResult {
    let def = page_arg(ed, a)?;
    let mut jobs: Vec<(usize, Vec<NodeId>, Styles)> = Vec::new();
    for u in a.arr("updates")? {
        let u = Args(u.as_object().ok_or("each update must be an object")?);
        let (page, ids) = resolve_all(ed, &u.strs("nodeIds")?, def)?;
        let styles = css::from_json(u.get("styles").ok_or("each update needs `styles`")?)?;
        jobs.push((page, ids, styles));
    }
    let mut ignored: Map<String, Value> = Map::new();
    let mut warnings = Vec::new();
    let mut count = 0;
    for (page, ids, styles) in jobs {
        let page_id = ed.pages[page].id.clone();
        let res = ed.with_page(page, |ed| {
            ed.edit("Update styles", |tx| {
                let mut out = Vec::new();
                for id in &ids {
                    let parent =
                        Parent::of(tx.doc(), &page_id, tx.doc().get(*id).and_then(|n| n.parent));
                    let old = tx.doc().get(*id).unwrap().rect;
                    let mut n = tx.doc().get(*id).unwrap().clone();
                    let applied = css::apply(&mut n, &styles, &parent.ctx());
                    let mut warn = None;
                    if let Some((src, mode)) = &applied.image {
                        match image_asset(src) {
                            Ok(asset) => {
                                n.style.fills.retain(|p| !matches!(p, Paint::Image { .. }));
                                n.style.fills.push(Paint::Image {
                                    asset,
                                    mode: *mode,
                                    opacity: 1.0,
                                });
                            }
                            Err(e) => warn = Some(e),
                        }
                    }
                    if let Some(ps) = parent.size {
                        if let Some(r) = applied.right {
                            n.rect.x = ps.0 - n.rect.w - r;
                        }
                        if let Some(b) = applied.bottom {
                            n.rect.y = ps.1 - n.rect.h - b;
                        }
                    }
                    let new = n.rect;
                    let keep_h =
                        get(&styles, "height").is_some_and(|v| css::length(v, 16.0).is_some());
                    *tx.node_mut(*id).unwrap() = n;
                    remember_stretch(&page_id, *id, &styles);
                    if (old.w - new.w).abs() > 1e-9 || (old.h - new.h).abs() > 1e-9 {
                        crate::layout::resize_children(tx, *id, old, new);
                    }
                    fit_text(tx, *id, keep_h);
                    relayout(tx, Some(*id));
                    out.push((*id, applied.ignored, warn));
                }
                out
            })
        });
        for (id, ig, warn) in res {
            count += 1;
            if !ig.is_empty() {
                ignored.insert(node_key(ed, page, id), json!(ig));
            }
            warnings.extend(warn);
        }
    }
    let mut v = json!({"updated": count});
    if !ignored.is_empty() {
        v["ignoredStyles"] = Value::Object(ignored);
    }
    if !warnings.is_empty() {
        v["warnings"] = json!(warnings);
    }
    text(v)
}

/// Group per-node updates by page and run each page's batch as one edit.
fn batch<T: Clone>(
    ed: &mut Editor,
    a: &Args,
    key: &str,
    label: &str,
    parse: impl Fn(&Args) -> Result<T, String>,
    mut f: impl FnMut(&mut Tx, NodeId, &T) -> Result<(), String>,
) -> Result<usize, String> {
    let def = page_arg(ed, a)?;
    let mut by_page: Vec<(usize, Vec<(NodeId, T)>)> = Vec::new();
    for u in a.arr(key)? {
        let u = Args(u.as_object().ok_or("each update must be an object")?);
        let (page, id) = resolve(ed, u.req_str("nodeId")?, def)?;
        let v = parse(&u)?;
        match by_page.iter_mut().find(|(p, _)| *p == page) {
            Some((_, list)) => list.push((id, v)),
            None => by_page.push((page, vec![(id, v)])),
        }
    }
    let mut n = 0;
    for (page, list) in by_page {
        ed.with_page(page, |ed| {
            ed.edit(label, |tx| {
                for (id, v) in &list {
                    f(tx, *id, v)?;
                    n += 1;
                }
                Ok::<(), String>(())
            })
        })?;
    }
    Ok(n)
}

fn set_text_content(ed: &mut Editor, a: &Args) -> ToolResult {
    let n = batch(
        ed,
        a,
        "updates",
        "Set text",
        |u| {
            Ok(u.get("textContent")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string())
        },
        |tx, id, t| {
            let node = tx.node_mut(id).unwrap();
            let old_len = node.kind.text().map(|s| s.chars().count()).ok_or_else(|| {
                format!(
                    "{} is a {}, not a text layer",
                    node.name,
                    node.kind.type_name()
                )
            })?;
            *node.kind.text_mut().unwrap() = t.clone();
            let len = t.chars().count();
            for s in &mut node.text_style.spans {
                if s.start == 0 && s.end >= old_len {
                    s.end = len;
                }
                s.start = s.start.min(len);
                s.end = s.end.min(len);
            }
            node.text_style.spans.retain(|s| s.end > s.start);
            fit_text(tx, id, false);
            relayout(tx, Some(id));
            Ok(())
        },
    )?;
    text(json!({"updated": n}))
}

fn rename_nodes(ed: &mut Editor, a: &Args) -> ToolResult {
    let n = batch(
        ed,
        a,
        "updates",
        "Rename",
        |u| Ok(u.req_str("name")?.chars().take(50).collect::<String>()),
        |tx, id, name| {
            tx.node_mut(id).unwrap().name = name.clone();
            Ok(())
        },
    )?;
    text(json!({"renamed": n}))
}

fn delete_nodes(ed: &mut Editor, a: &Args) -> ToolResult {
    let def = page_arg(ed, a)?;
    let mut missing = Vec::new();
    let mut by_page: Vec<(usize, Vec<NodeId>)> = Vec::new();
    for k in a.strs("nodeIds")? {
        match resolve(ed, &k, def) {
            Ok((p, id)) => match by_page.iter_mut().find(|(q, _)| *q == p) {
                Some((_, l)) => l.push(id),
                None => by_page.push((p, vec![id])),
            },
            Err(_) => missing.push(k),
        }
    }
    let mut deleted = 0;
    for (page, ids) in by_page {
        deleted += ed.with_page(page, |ed| {
            let n = ed.edit("Delete", |tx| {
                let mut n = 0;
                for id in &ids {
                    if tx.doc().get(*id).is_some() {
                        tx.remove(*id);
                        n += 1;
                    }
                }
                n
            });
            ed.selection.retain(|s| ed.doc.get(*s).is_some());
            n
        });
    }
    let mut v = json!({"deleted": deleted});
    if !missing.is_empty() {
        v["notFound"] = json!(missing);
    }
    text(v)
}

fn duplicate_nodes(ed: &mut Editor, a: &Args) -> ToolResult {
    let def = page_arg(ed, a)?;
    let mut jobs = Vec::new();
    for item in a.arr("nodes")? {
        let item = Args(item.as_object().ok_or("each item must be an object")?);
        let (page, id) = resolve(ed, item.req_str("id")?, def)?;
        let parent = match item.str("parentId").filter(|p| !p.is_empty()) {
            None => None,
            Some(ROOT) => Some(None),
            Some(p) => {
                let (pp, pid) = resolve(ed, p, page)?;
                if pp != page {
                    return Err("duplicating to another page isn't supported".into());
                }
                if !ed.page_doc(pp).get(pid).unwrap().is_container() {
                    return Err("parentId must be a frame or group".into());
                }
                Some(Some(pid))
            }
        };
        jobs.push((page, id, parent));
    }
    let mut out = Vec::new();
    for (page, id, parent) in jobs {
        let (new, pairs) = ed.with_page(page, |ed| {
            ed.edit("Duplicate", |tx| {
                let n = tx.doc().get(id).unwrap();
                let same_parent = parent.is_none_or(|p| p == n.parent);
                let target = parent.unwrap_or(n.parent);
                let index = if same_parent {
                    tx.doc()
                        .children(target)
                        .iter()
                        .position(|c| *c == id)
                        .map(|i| i + 1)
                } else {
                    None
                };
                let offset = if target.is_none() && n.is_frame() {
                    let w = tx.doc().world_rect(id).unwrap();
                    free_spot(tx.doc(), w.w) - w.min()
                } else {
                    V2::ZERO
                };
                let r = clone_subtree(tx, id, target, index, offset);
                relayout(tx, Some(r.0));
                r
            })
        });
        let map: Map<String, Value> = pairs
            .iter()
            .filter(|(o, _)| *o != id)
            .map(|(o, n)| (node_key(ed, page, *o), json!(node_key(ed, page, *n))))
            .collect();
        out.push(json!({
            "sourceId": node_key(ed, page, id),
            "newId": node_key(ed, page, new),
            "descendantIdMap": map,
        }));
    }
    text(json!({"duplicates": out}))
}

fn move_nodes(ed: &mut Editor, a: &Args) -> ToolResult {
    let def = page_arg(ed, a)?;
    let moves = a.arr("moves")?;
    let mut done = 0;
    for m in moves {
        let m = Args(m.as_object().ok_or("each move must be an object")?);
        let (page, id) = resolve(ed, m.req_str("nodeId")?, def)?;
        let sib = |k: &str| -> Result<Option<NodeId>, String> {
            match m.str(k) {
                None => Ok(None),
                Some(s) => {
                    let (p, sid) = resolve(ed, s, page)?;
                    if p != page {
                        return Err(
                            "moving between pages isn't supported; duplicate and delete instead"
                                .into(),
                        );
                    }
                    Ok(Some(sid))
                }
            }
        };
        let (before, after) = (sib("before")?, sib("after")?);
        let parent: Option<Option<NodeId>> = match m.str("parentId") {
            None => None,
            Some(ROOT) => Some(None),
            Some(p) => {
                let (pp, pid) = resolve(ed, p, page)?;
                if pp != page {
                    return Err(
                        "moving between pages isn't supported; duplicate and delete instead".into(),
                    );
                }
                Some(Some(pid))
            }
        };
        let index = m.num("index").map(|i| i.max(0.0) as usize);
        ed.with_page(page, |ed| {
            ed.edit("Move", |tx| -> Result<(), String> {
                let doc = tx.doc();
                let (target, sibling) = if let Some(s) = before.or(after) {
                    (doc.get(s).unwrap().parent, Some(s))
                } else if let Some(p) = parent {
                    (p, None)
                } else {
                    return Err("each move needs before, after or parentId".into());
                };
                if sibling == Some(id) {
                    return Err("a node can't be moved next to itself".into());
                }
                if let Some(t) = target {
                    if t == id || doc.is_ancestor(id, t) {
                        return Err("can't move a node into itself or its descendants".into());
                    }
                    if !doc.get(t).unwrap().is_container() {
                        return Err("the destination can't have children".into());
                    }
                }
                tx.reparent(id, target, None);
                let list: Vec<NodeId> = tx
                    .doc()
                    .children(target)
                    .iter()
                    .copied()
                    .filter(|c| *c != id)
                    .collect();
                let i = match sibling {
                    Some(s) => {
                        list.iter().position(|c| *c == s).unwrap() + usize::from(after.is_some())
                    }
                    None => index.unwrap_or(list.len()).min(list.len()),
                };
                tx.move_to_index(id, i);
                Ok(())
            })
        })?;
        done += 1;
    }
    text(json!({"moved": done}))
}

fn select_nodes(ed: &mut Editor, a: &Args) -> ToolResult {
    let page = ed.active_page;
    let (p, ids) = resolve_all(ed, &a.strs("nodeIds")?, page)?;
    if p != page {
        return Err("only nodes on the page the user is viewing can be selected".into());
    }
    ed.select(ids.clone());
    if a.get("zoom").and_then(Value::as_bool).unwrap_or(false)
        && let Some(b) = Rect::union_all(
            &ids.iter()
                .filter_map(|i| ed.doc.world_rect(*i))
                .collect::<Vec<_>>(),
        )
    {
        ed.camera.fit(&b, ed.viewport, 64.0, 1.0);
    }
    ed.revision += 1;
    text(json!({"selected": ids.len()}))
}

fn undo_redo(ed: &mut Editor, a: &Args, undo: bool) -> ToolResult {
    let page = page_arg(ed, a)?;
    let label = ed.with_page(page, |ed| {
        let before = ed.history.generation();
        if undo {
            ed.undo();
        } else {
            ed.redo();
        }
        (ed.history.generation() != before).then(|| ed.status.clone())
    });
    text(json!({"done": label.is_some(), "what": label}))
}

/// The HTML parser lowercases attribute names. Put the `<svg>` attributes that SVG spells in
/// camelCase back in their case before the markup goes to usvg, like the HTML spec's "adjust SVG
/// attributes" step. Without this, `viewbox` is ignored and the content is not scaled.
fn svg_attr_name(k: &str) -> &str {
    match k {
        "viewbox" => "viewBox",
        "preserveaspectratio" => "preserveAspectRatio",
        "baseprofile" => "baseProfile",
        "zoomandpan" => "zoomAndPan",
        _ => k,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globbing() {
        assert!(glob("Get *", "get started"));
        assert!(glob("*start*", "Get started"));
        assert!(glob("foo*bar", "foo-x-bar"));
        assert!(!glob("foo", "foobar"));
        assert!(!glob("a*c", "abd"));
    }

    #[test]
    fn text_collapses_whitespace_and_keeps_breaks() {
        let tree = html::parse("<p>  Hello\n   <b>bold</b> world<br> next </p>");
        let Html::Element(p) = &tree[0] else { panic!() };
        let mut chars = Vec::new();
        collect_runs(&p.children, Run::default(), 16.0, &mut chars);
        let s: String = collapse(chars.clone(), false)
            .iter()
            .map(|(c, _)| *c)
            .collect();
        assert_eq!(s, "Hello bold world\nnext");
        let bold: String = collapse(chars, false)
            .iter()
            .filter(|(_, r)| r.bold == Some(true))
            .map(|(c, _)| *c)
            .collect();
        assert_eq!(bold, "bold");
    }
}
