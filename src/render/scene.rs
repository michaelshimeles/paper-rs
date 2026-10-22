//! Document + camera → world-space cached meshes, plus the screen-space overlay.

use super::draw;
use super::mesh::{Batch, MeshBuilder};
use super::tess::{Piece, Tess};
use crate::editor::{Editor, hit, snap::Guide};
use crate::geom::{Camera, Rect, V2, v2};
use crate::model::{BlendMode, Node};
use crate::model::{Color, Document, NodeId, NodeKind, TextAlign};
use crate::text;
use lyon::tessellation::{FillRule, LineCap};
use std::collections::HashMap;
use std::sync::Arc;

pub const CANVAS_BG_LIGHT: Color = Color::rgb(0xee, 0xee, 0xf0);
pub const CANVAS_BG_DARK: Color = Color::rgb(0x1e, 0x1e, 0x20);
pub const GRID_DOT_LIGHT: Color = Color::rgb(0xcf, 0xcf, 0xd6);
pub const GRID_DOT_DARK: Color = Color::rgb(0x3a, 0x3a, 0x3c);
pub const ACCENT: Color = Color::rgb(0x2f, 0x6f, 0xf3);
pub const GUIDE: Color = Color::rgb(0xf2, 0x2f, 0x7a);

pub fn canvas_bg(dark: bool) -> Color {
    if dark {
        CANVAS_BG_DARK
    } else {
        CANVAS_BG_LIGHT
    }
}

pub fn grid_dot(dark: bool) -> Color {
    if dark { GRID_DOT_DARK } else { GRID_DOT_LIGHT }
}

pub fn rgba(c: Color) -> [f32; 4] {
    c.to_f32(1.0)
}

/// What to draw and where.
pub struct View<'a> {
    pub doc: &'a Document,
    pub camera: Camera,
    /// Canvas size in logical px.
    pub viewport: V2,
    /// Canvas top-left in the render target, logical px.
    pub offset: V2,
}

impl View<'_> {
    fn to_screen(&self, p: V2) -> V2 {
        self.camera.world_to_screen(p) + self.offset
    }
    fn rect_to_screen(&self, r: &Rect) -> Rect {
        self.camera.world_rect_to_screen(r).translate(self.offset)
    }
    fn canvas_rect(&self) -> Rect {
        Rect::new(
            self.offset.x,
            self.offset.y,
            self.viewport.x,
            self.viewport.y,
        )
    }
}

/// Dot grid whose spacing adapts to the zoom so it never gets too dense or sparse.
pub fn grid(mb: &mut MeshBuilder, v: &View, dark: bool) {
    let z = v.camera.zoom;
    let mut spacing = 20.0;
    while spacing * z < 14.0 {
        spacing *= 5.0;
    }
    while spacing * z > 70.0 {
        spacing /= 5.0;
    }
    let vis = v.camera.visible_world(v.viewport);
    let x0 = (vis.x / spacing).floor() as i64;
    let y0 = (vis.y / spacing).floor() as i64;
    let x1 = (vis.right() / spacing).ceil() as i64;
    let y1 = (vis.bottom() / spacing).ceil() as i64;
    let color = rgba(grid_dot(dark));
    let r = 0.9;
    for yi in y0..=y1 {
        for xi in x0..=x1 {
            let s = v.to_screen(v2(xi as f64 * spacing, yi as f64 * spacing));
            mb.rect(Rect::new(s.x - r, s.y - r, 2.0 * r, 2.0 * r), color);
        }
    }
}

// ---------------------------------------------------------------------------------- world scene

/// One node's cached triangles plus where they are clipped and how they blend.
#[derive(Clone)]
pub struct Item {
    pub id: NodeId,
    pub pieces: Arc<Vec<Piece>>,
    /// World-space clip rectangle (from clipping frames).
    pub clip: Option<Rect>,
    pub blend: BlendMode,
}

struct Entry {
    node: Node,
    wr: Rect,
    opacity: f64,
    bucket: i32,
    pieces: Arc<Vec<Piece>>,
    used: bool,
}

/// Per-node tessellation cache. A node is re-tessellated only when it (or its world position,
/// inherited opacity or the zoom bucket) changes, so panning and idle frames do no work.
#[derive(Default)]
pub struct SceneCache {
    entries: HashMap<NodeId, Entry>,
    /// Item list of the previous frame and what it was built for (doc stamp, zoom bucket,
    /// culled world rect): reused while the view stays inside that rect.
    last: Option<(u64, i32, Rect, Vec<Item>)>,
    /// Tessellations performed during the last `items` call (for tests and profiling).
    pub last_misses: usize,
}

/// Zoom buckets are powers of two: geometry built for 2^b is accurate up to 2^(b+1).
pub fn zoom_bucket(zoom: f64) -> i32 {
    zoom.max(1e-6).log2().floor() as i32
}

impl SceneCache {
    pub fn clear(&mut self) {
        self.entries.clear();
        self.last = None;
    }

    /// Items for `roots` (or the whole page) in paint order. `cull` skips subtrees outside a
    /// world rect.
    pub fn items(
        &mut self,
        doc: &Document,
        roots: Option<&[NodeId]>,
        zoom: f64,
        cull: Option<Rect>,
    ) -> Vec<Item> {
        let bucket = zoom_bucket(zoom);
        let tz = 2f64.powi(bucket);
        let mut tess = Tess::new(tz);
        let mut out = Vec::new();
        self.last_misses = 0;
        for e in self.entries.values_mut() {
            e.used = false;
        }
        let all = doc.root.clone();
        for id in roots.unwrap_or(&all) {
            let op = draw::inherited_opacity(doc, *id);
            let origin = doc.parent_origin(doc.get(*id).and_then(|n| n.parent));
            self.walk(
                doc, *id, origin, None, op, bucket, &mut tess, cull, &mut out,
            );
        }
        // Evict entries for deleted or long-unseen nodes when the cache grows large.
        let used = self.entries.values().filter(|e| e.used).count();
        if self.entries.len() > used * 4 + 2000 {
            self.entries.retain(|_, e| e.used);
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(
        &mut self,
        doc: &Document,
        id: NodeId,
        origin: V2,
        clip: Option<Rect>,
        parent_opacity: f64,
        bucket: i32,
        tess: &mut Tess,
        cull: Option<Rect>,
        out: &mut Vec<Item>,
    ) {
        let Some(n) = doc.get(id) else { return };
        if !n.visible {
            return;
        }
        let wr = n.rect.translate(origin);
        let opacity = parent_opacity * n.style.opacity;
        let bounds = draw::draw_bounds(n, wr);
        let visible = cull.is_none_or(|c| c.intersects(&bounds))
            && clip.is_none_or(|c| c.intersects(&bounds));
        if visible && !n.is_group() {
            let hit = self.entries.get(&id).is_some_and(|e| {
                e.bucket == bucket && e.wr == wr && e.opacity == opacity && e.node == *n
            });
            if !hit {
                self.last_misses += 1;
                let d = draw::node_draw(n, wr, opacity);
                let pieces = Arc::new(tess.ops(&d.ops));
                self.entries.insert(
                    id,
                    Entry {
                        node: n.clone(),
                        wr,
                        opacity,
                        bucket,
                        pieces,
                        used: true,
                    },
                );
            }
            let e = self.entries.get_mut(&id).unwrap();
            e.used = true;
            if !e.pieces.is_empty() {
                out.push(Item {
                    id,
                    pieces: e.pieces.clone(),
                    clip,
                    blend: n.style.blend,
                });
            }
        }
        if n.children.is_empty() {
            return;
        }
        let child_clip = match n.kind {
            NodeKind::Frame { clip: true } => {
                match clip.map_or(Some(wr), |c| c.intersection(&wr)) {
                    Some(c) => Some(c),
                    None => return,
                }
            }
            _ => clip,
        };
        if matches!(n.kind, NodeKind::Frame { clip: true })
            && cull.is_some_and(|c| !c.intersects(&wr))
        {
            return;
        }
        let o = wr.min();
        for c in &n.children {
            self.walk(doc, *c, o, child_clip, opacity, bucket, tess, cull, out);
        }
    }
}

/// Everything the GPU draws for the canvas: screen-space underlay (background, grid), the
/// world-space document, and the screen-space overlay (selection, guides, labels).
pub struct CanvasFrame {
    pub under: Vec<Batch>,
    pub items: Vec<Item>,
    pub over: Vec<Batch>,
    pub camera: Camera,
    /// Canvas top-left in the render target, logical px.
    pub offset: V2,
    /// Canvas rect in logical px (world items are scissored to it).
    pub viewport: Rect,
}

/// Draw text with its box's top-left at `origin` (screen px) at screen scale `z`.
#[allow(clippy::too_many_arguments)]
pub fn draw_text(
    mb: &mut MeshBuilder,
    t: &str,
    font_size: f64,
    wrap: Option<f64>,
    align: TextAlign,
    origin: V2,
    z: f64,
    color: [f32; 4],
) {
    let layout = text::layout(t, font_size, wrap, align);
    let px = font_size * z;
    if px < 2.5 {
        // Too small to read: draw "greeked" bars instead of glyphs.
        for l in &layout.lines {
            let lh = l.height * z;
            if l.width > 0.0 {
                let x = origin.x + l.carets[0] * z;
                mb.rect(
                    Rect::new(x, origin.y + l.top * z + lh * 0.3, l.width * z, lh * 0.4),
                    [color[0], color[1], color[2], color[3] * 0.35],
                );
            }
        }
        return;
    }
    for g in &layout.glyphs {
        let s = (g.size / text::face_of(g.face).units_per_em() as f64 * z) as f32;
        let m = text::glyph_mesh(g.face, g.gid);
        if m.indices.is_empty() {
            continue;
        }
        let o = origin + v2(g.x, g.baseline) * z;
        let (ox, oy) = (o.x as f32, o.y as f32);
        mb.triangles(
            m.vertices.iter().map(|v| [ox + v[0] * s, oy - v[1] * s]),
            &m.indices,
            color,
        );
    }
}

/// Interactive overlay: hover, selection, handles, marquee, guides, labels, caret.
pub fn overlay(mb: &mut MeshBuilder, ed: &Editor, offset: V2) {
    let v = View {
        doc: &ed.doc,
        camera: ed.camera,
        viewport: ed.viewport,
        offset,
    };
    let accent = rgba(ACCENT);
    let z = ed.camera.zoom;
    mb.set_clip(Some(v.canvas_rect()));

    // Frame name labels.
    for (id, r) in ed.frame_labels() {
        let n = ed.doc.get(id).unwrap();
        let selected = ed.selection.contains(&id);
        let color = if selected {
            accent
        } else {
            [0.42, 0.42, 0.46, 1.0]
        };
        let r = r.translate(offset);
        let mut name = n.name.clone();
        let max_w = r.w;
        while !name.is_empty() && text::layout(&name, 12.0, None, TextAlign::Left).width > max_w {
            name.pop();
        }
        draw_text(
            mb,
            &name,
            12.0,
            None,
            TextAlign::Left,
            v2(r.x, r.y + 2.0),
            1.0,
            color,
        );
    }

    if let Some(h) = ed.hover.filter(|h| !ed.selection.contains(h))
        && let Some(wr) = ed.doc.world_rect(h)
    {
        outline_node(mb, &v, h, v.rect_to_screen(&wr), accent, 1.5);
    }

    for id in &ed.selection {
        if let Some(wr) = ed.doc.world_rect(*id) {
            outline_node(mb, &v, *id, v.rect_to_screen(&wr), accent, 1.0);
        }
    }

    if let Some(te) = &ed.text_edit {
        if let (Some(n), Some(wr)) = (ed.doc.get(te.id), ed.doc.world_rect(te.id))
            && let Some(l) = text::layout_node(n)
        {
            let o = v.to_screen(wr.min() + text::text_offset(n));
            if let Some(a) = te.anchor.filter(|a| *a != te.caret) {
                for (x, top, w, h) in l.selection_rects(a, te.caret) {
                    mb.rect(
                        Rect::new(o.x + x * z, o.y + top * z, w * z, h * z),
                        [accent[0], accent[1], accent[2], 0.25],
                    );
                }
            }
            let (x, top, h) = l.caret(te.caret);
            mb.rect(
                Rect::new(o.x + x * z - 0.75, o.y + top * z, 1.5, h * z),
                accent,
            );
        }
    } else if let Some(b) = ed.selection_bounds() {
        let sb = v.rect_to_screen(&b);
        if ed.selection.len() > 1 {
            mb.rect_outline(sb, 1.0, accent);
        }
        let tiny = sb.w.min(sb.h) < 24.0;
        for (hx, hy) in hit::HANDLES {
            if tiny && (hx == 0.5 || hy == 0.5) {
                continue;
            }
            let c = v2(sb.x + sb.w * hx, sb.y + sb.h * hy);
            let r = Rect::new(c.x - 4.0, c.y - 4.0, 8.0, 8.0);
            mb.rect(r, [1.0, 1.0, 1.0, 1.0]);
            mb.rect_outline(r, 1.0, accent);
        }
        // Size badge below the selection.
        let label = format!("{} × {}", fmt_num(b.w), fmt_num(b.h));
        let w = text::layout(&label, 11.0, None, TextAlign::Left).width;
        let pill = Rect::new(
            sb.center().x - w * 0.5 - 6.0,
            sb.bottom() + 8.0,
            w + 12.0,
            18.0,
        );
        mb.fill_path(
            &MeshBuilder::rounded_rect_path(pill, 4.0),
            accent,
            FillRule::NonZero,
        );
        draw_text(
            mb,
            &label,
            11.0,
            None,
            TextAlign::Left,
            v2(pill.x + 6.0, pill.y + 2.0),
            1.0,
            [1.0; 4],
        );
    }

    if let Some(m) = ed.marquee {
        let sm = v.rect_to_screen(&m);
        mb.rect(sm, [accent[0], accent[1], accent[2], 0.08]);
        mb.rect_outline(sm, 1.0, accent);
    }

    let guide = rgba(GUIDE);
    for g in ed.guides.iter().chain(&ed.measures) {
        match *g {
            Guide::V { x, y0, y1 } => {
                mb.line(v.to_screen(v2(x, y0)), v.to_screen(v2(x, y1)), 1.0, guide)
            }
            Guide::H { y, x0, x1 } => {
                mb.line(v.to_screen(v2(x0, y)), v.to_screen(v2(x1, y)), 1.0, guide)
            }
            Guide::Gap { a, b } => gap_guide(mb, &v, a, b, guide),
        }
    }
    vector_edit_overlay(mb, ed, &v, accent);
    whiteboard_overlay(mb, ed, &v, accent);
    mb.set_clip(None);
}

/// Arrow handles, bind targets, quick-connect knobs and the laser trail.
fn whiteboard_overlay(mb: &mut MeshBuilder, ed: &Editor, v: &View, accent: [f32; 4]) {
    let off = v.offset;
    if let Some(t) = ed.bind_hover
        && let Some(wr) = ed.doc.world_rect(t)
    {
        let n = ed.doc.get(t).unwrap();
        let path = draw::shape_path(n, wr.inflate(4.0 / v.camera.zoom))
            .unwrap_or_else(|| crate::vector::rounded_rect(wr, [0.0; 4]));
        let sp = path.map(|p| v.to_screen(p));
        mb.stroke_path(
            &sp.to_lyon(),
            3.0,
            [accent[0], accent[1], accent[2], 0.45],
            LineCap::Round,
        );
    }
    for (end, p) in ed.connector_handles() {
        let p = p + off;
        if end == 2 {
            mb.circle(p, 4.0, [accent[0], accent[1], accent[2], 0.5]);
        } else {
            mb.circle(p, 5.5, accent);
            mb.circle(p, 4.0, [1.0; 4]);
        }
    }
    for (_, p) in ed.quick_knobs() {
        let p = p + off;
        mb.circle(p, 8.0, [accent[0], accent[1], accent[2], 0.9]);
        mb.line(p - v2(4.0, 0.0), p + v2(4.0, 0.0), 1.6, [1.0; 4]);
        mb.line(p - v2(0.0, 4.0), p + v2(0.0, 4.0), 1.6, [1.0; 4]);
    }
    let trail = ed.laser_trail(ed.now_ms);
    if trail.len() >= 2 {
        let red = [0.98, 0.18, 0.2, 1.0];
        for w in trail.windows(2) {
            let (a, ka) = w[0];
            let (b, kb) = w[1];
            let width = 4.0 * (ka + kb) * 0.5;
            mb.line(v.to_screen(a), v.to_screen(b), width, red);
            mb.circle(v.to_screen(b), width * 0.5, red);
        }
        let (head, _) = *trail.last().unwrap();
        mb.circle(v.to_screen(head), 3.5, [1.0, 0.6, 0.6, 1.0]);
    } else if let Some((p, _)) = trail.first() {
        mb.circle(v.to_screen(*p), 3.0, [0.98, 0.18, 0.2, 1.0]);
    }
}

/// A measured distance: line with end ticks and a pill showing the length.
fn gap_guide(mb: &mut MeshBuilder, v: &View, a: V2, b: V2, color: [f32; 4]) {
    let (sa, sb) = (v.to_screen(a), v.to_screen(b));
    mb.line(sa, sb, 1.0, color);
    let n = (sb - sa).normalized().perp() * 4.0;
    mb.line(sa - n, sa + n, 1.0, color);
    mb.line(sb - n, sb + n, 1.0, color);
    let label = fmt_num(a.dist(b));
    let w = text::layout(&label, 11.0, None, TextAlign::Left).width;
    let c = (sa + sb) * 0.5;
    let horizontal = (sb.x - sa.x).abs() >= (sb.y - sa.y).abs();
    let pill = if horizontal {
        Rect::new(c.x - w * 0.5 - 4.0, c.y + 4.0, w + 8.0, 16.0)
    } else {
        Rect::new(c.x + 6.0, c.y - 8.0, w + 8.0, 16.0)
    };
    mb.fill_path(
        &MeshBuilder::rounded_rect_path(pill, 3.0),
        color,
        FillRule::NonZero,
    );
    draw_text(
        mb,
        &label,
        11.0,
        None,
        TextAlign::Left,
        v2(pill.x + 4.0, pill.y + 1.0),
        1.0,
        [1.0; 4],
    );
}

/// Anchors and handles while drawing with the pen or editing a path.
fn vector_edit_overlay(mb: &mut MeshBuilder, ed: &Editor, v: &View, accent: [f32; 4]) {
    let (id, selected, pen) = match (&ed.pen, &ed.path_edit) {
        (Some(p), _) => (p.id, None, true),
        (None, Some(pe)) => (pe.id, pe.selected, false),
        _ => return,
    };
    let (Some(n), Some(wr)) = (ed.doc.get(id), ed.doc.world_rect(id)) else {
        return;
    };
    let NodeKind::Path { subpaths, .. } = &n.kind else {
        return;
    };
    let s = |p: V2| v.to_screen(wr.min() + p);
    // Path outline in accent so it stays visible over any fill.
    let path = crate::vector::from_subpaths(wr.min(), subpaths).map(|p| v.to_screen(p));
    mb.stroke_path(&path.to_lyon(), 1.0, accent, LineCap::Round);
    for (si, sp) in subpaths.iter().enumerate() {
        let last = sp.points.len().saturating_sub(1);
        for (pi, pt) in sp.points.iter().enumerate() {
            let show_handles = selected == Some((si, pi)) || (pen && pi == last);
            if show_handles {
                for h in [pt.h_in, pt.h_out].into_iter().flatten() {
                    mb.line(s(pt.p), s(h), 1.0, accent);
                    mb.circle(s(h), 3.5, accent);
                    mb.circle(s(h), 2.0, [1.0; 4]);
                }
            }
            let c = s(pt.p);
            let r = Rect::new(c.x - 3.5, c.y - 3.5, 7.0, 7.0);
            let sel = selected == Some((si, pi));
            mb.rect(r, if sel { accent } else { [1.0; 4] });
            mb.rect_outline(r, 1.0, accent);
        }
    }
    // Rubber band from the last pen point to the pointer.
    if pen
        && !ed.is_busy()
        && let Some(last) = subpaths.first().and_then(|sp| sp.points.last())
    {
        mb.line(
            s(last.p),
            ed.pointer + v.offset,
            1.0,
            [accent[0], accent[1], accent[2], 0.6],
        );
    }
}

fn outline_node(mb: &mut MeshBuilder, v: &View, id: NodeId, sr: Rect, color: [f32; 4], w: f64) {
    let n = v.doc.get(id).unwrap();
    let z = v.camera.zoom;
    match &n.kind {
        NodeKind::Frame { .. } | NodeKind::Rect | NodeKind::Text { .. } | NodeKind::Group => {
            mb.rect_outline(sr, w, color)
        }
        NodeKind::Stroke { points, .. } if points.len() > 1 => {
            let pts: Vec<V2> = points.iter().map(|q| sr.min() + q.pos() * z).collect();
            mb.stroke_path(
                &MeshBuilder::polygon_path(&pts, false),
                w as f32,
                color,
                LineCap::Round,
            );
            mb.rect_outline(sr, 1.0, [color[0], color[1], color[2], 0.5]);
        }
        _ => {
            let wr = v.doc.world_rect(id).unwrap();
            if let Some(path) = draw::shape_path(n, wr) {
                let sp = path.map(|p| v.to_screen(p));
                mb.stroke_path(&sp.to_lyon(), w as f32, color, LineCap::Round);
            }
        }
    }
}

fn fmt_num(x: f64) -> String {
    let r = (x * 10.0).round() / 10.0;
    if r.fract() == 0.0 {
        format!("{}", r as i64)
    } else {
        format!("{r:.1}")
    }
}

/// Full interactive canvas: background grid, document, overlay.
pub fn canvas(
    ed: &Editor,
    cache: &mut SceneCache,
    offset: V2,
    show_grid: bool,
    dark: bool,
) -> CanvasFrame {
    let v = View {
        doc: &ed.doc,
        camera: ed.camera,
        viewport: ed.viewport,
        offset,
    };
    let mut under = MeshBuilder::default();
    under.set_clip(Some(v.canvas_rect()));
    under.rect(v.canvas_rect(), rgba(canvas_bg(dark)));
    if show_grid {
        grid(&mut under, &v, dark);
    }
    let vis = ed.camera.visible_world(ed.viewport);
    let bucket = zoom_bucket(ed.camera.zoom);
    let items = match &cache.last {
        Some((stamp, b, cull, items))
            if *stamp == ed.doc_stamp && *b == bucket && cull.contains_rect(&vis) =>
        {
            cache.last_misses = 0;
            items.clone()
        }
        _ => {
            let cull = vis.inflate(vis.w.max(vis.h) * 0.5);
            let items = cache.items(&ed.doc, None, ed.camera.zoom, Some(cull));
            cache.last = Some((ed.doc_stamp, bucket, cull, items.clone()));
            items
        }
    };
    let mut over = MeshBuilder::default();
    overlay(&mut over, ed, offset);
    CanvasFrame {
        under: under.finish(),
        items,
        over: over.finish(),
        camera: ed.camera,
        offset,
        viewport: v.canvas_rect(),
    }
}

/// A frame that draws only `roots` (or the page) with no chrome: exports and thumbnails.
pub fn export_frame(
    doc: &Document,
    roots: Option<&[NodeId]>,
    camera: Camera,
    size_px: V2,
    background: Option<Color>,
) -> CanvasFrame {
    let mut under = MeshBuilder::default();
    if let Some(bg) = background {
        under.rect(Rect::new(0.0, 0.0, size_px.x, size_px.y), rgba(bg));
    }
    let items = SceneCache::default().items(doc, roots, camera.zoom, None);
    CanvasFrame {
        under: under.finish(),
        items,
        over: Vec::new(),
        camera,
        offset: V2::ZERO,
        viewport: Rect::new(0.0, 0.0, size_px.x, size_px.y),
    }
}
