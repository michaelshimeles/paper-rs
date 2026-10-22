//! Milestone 3: Figma-core features, driven through the editor's input path.

mod common;

use common::*;
use sketch_rs::editor::vector_ops::BoolOp;
use sketch_rs::editor::{Button, Editor, InputEvent, Key, Modifiers, Tool};
use sketch_rs::geom::{Rect, V2, v2};
use sketch_rs::model::{
    Align, AutoLayout, BlendMode, Color, Constraint, Direction, ImageMode, Node, NodeId, NodeKind,
    Paint, Sizing,
};
use sketch_rs::render::frame;
use sketch_rs::ui::UiState;

fn rect(ed: &mut Editor, r: Rect, c: Color, parent: Option<NodeId>) -> NodeId {
    let mut n = Node::new(NodeId(0), NodeKind::Rect, r);
    n.style.set_fill(Some(c));
    ed.add_node(n, parent)
}

fn frame_node(ed: &mut Editor, r: Rect, parent: Option<NodeId>) -> NodeId {
    ed.add_node(
        Node::new(NodeId(0), NodeKind::Frame { clip: true }, r),
        parent,
    )
}

fn click(e: &mut Editor, p: V2, t: f64) {
    e.handle(InputEvent::PointerDown {
        pos: p,
        button: Button::Primary,
        pressure: None,
        time_ms: t,
    });
    e.handle(InputEvent::PointerUp {
        pos: p,
        button: Button::Primary,
        time_ms: t + 30.0,
    });
}

fn wr(e: &Editor, id: NodeId) -> Rect {
    e.doc.world_rect(id).unwrap()
}

const RED: Color = Color::rgb(220, 40, 40);
const BLUE: Color = Color::rgb(40, 80, 220);

#[test]
fn groups_follow_children_and_capture_clicks() {
    let mut e = editor(1000.0, 800.0);
    let a = rect(&mut e, Rect::new(100.0, 100.0, 50.0, 50.0), RED, None);
    let b = rect(&mut e, Rect::new(200.0, 120.0, 50.0, 50.0), BLUE, None);
    e.selection = vec![a, b];
    key(&mut e, Key::Char('g'), Modifiers::cmd());
    let g = e.selection[0];
    assert!(e.doc.get(g).unwrap().is_group());
    assert_eq!(wr(&e, g), Rect::new(100.0, 100.0, 150.0, 70.0));
    assert_eq!(wr(&e, a).min(), v2(100.0, 100.0), "world positions kept");
    // A click on a child selects the group; a double-click enters it.
    e.selection.clear();
    click(&mut e, v2(120.0, 120.0), 0.0);
    assert_eq!(e.selection, vec![g]);
    click(&mut e, v2(120.0, 120.0), 1000.0);
    click(&mut e, v2(120.0, 120.0), 1100.0);
    assert_eq!(e.selection, vec![a]);
    // Dragging the child grows the group live.
    ev_drag(&mut e, v2(120.0, 120.0), v2(120.0, 320.0), 3000.0, true);
    assert_eq!(wr(&e, g).bottom(), 320.0 + 30.0);
    // Resizing the group scales its children.
    e.selection = vec![g];
    let gr = wr(&e, g);
    ev_drag(&mut e, gr.max(), gr.max() + v2(150.0, 0.0), 5000.0, true);
    let gr2 = wr(&e, g);
    assert!((gr2.w - (gr.w + 150.0)).abs() < 1.0, "{gr2:?}");
    assert!((wr(&e, b).right() - gr2.right()).abs() < 1e-6);
    key(&mut e, Key::Char('g'), Modifiers::cmd_shift());
    assert!(e.doc.get(g).is_none());
    assert_eq!(e.doc.root.len(), 2);
    e.undo();
    e.undo();
    assert!(e.doc.get(g).is_some());
    e.doc.validate().unwrap();
}

#[test]
fn auto_layout_hugs_reorders_and_fills() {
    let mut e = editor(1200.0, 800.0);
    let f = frame_node(&mut e, Rect::new(0.0, 0.0, 600.0, 200.0), None);
    let k: Vec<NodeId> = [(20.0, 40.0), (120.0, 80.0), (240.0, 60.0)]
        .iter()
        .map(|(x, w)| rect(&mut e, Rect::new(*x, 30.0, *w, 40.0), RED, Some(f)))
        .collect();
    e.selection = vec![f];
    key(&mut e, Key::Char('a'), Modifiers::SHIFT);
    let l = e.doc.get(f).unwrap().layout.expect("auto layout added");
    assert_eq!(l.direction, Direction::Horizontal);
    // Hug: width = padding + children + gaps.
    let fr = e.doc.get(f).unwrap().rect;
    let total: f64 = 40.0 + 80.0 + 60.0 + 2.0 * l.gap + l.padding[1] + l.padding[3];
    assert!((fr.w - total).abs() < 1e-6, "{fr:?} vs {total}");
    // Growing a child relayouts its siblings.
    e.selection = vec![k[0]];
    sketch_rs::ui::inspector::apply(&mut e, "width", &[k[0]], |n| n.rect.w = 100.0);
    assert_eq!(
        e.doc.get(k[1]).unwrap().rect.x,
        l.padding[3] + 100.0 + l.gap
    );
    // Drag the first child past the last one: it moves to the end of the flow.
    let start = wr(&e, k[0]).center();
    let end = wr(&e, k[2]).center() + v2(60.0, 0.0);
    e.selection = vec![k[0]];
    ev_drag(&mut e, start, end, 0.0, true);
    assert_eq!(e.doc.get(f).unwrap().children, vec![k[1], k[2], k[0]]);
    assert!(e.doc.get(k[0]).unwrap().rect.x > e.doc.get(k[2]).unwrap().rect.x);
    // Fill: one child takes the remaining width of a fixed frame.
    sketch_rs::ui::inspector::apply(&mut e, "layout", &[f], |n| {
        n.layout_child.sizing = [Sizing::Fixed, Sizing::Fixed];
        n.rect.w = 800.0;
    });
    sketch_rs::ui::inspector::apply(&mut e, "layout", &[k[1]], |n| {
        n.layout_child.sizing[0] = Sizing::Fill
    });
    let r = |i: usize| e.doc.get(k[i]).unwrap().rect;
    assert!(
        (r(0).right() + l.padding[1] - 800.0).abs() < 1e-6,
        "fill pushes the last child to the end"
    );
    assert!(r(1).w > 80.0);
}

#[test]
fn constraints_follow_frame_resize_and_cmd_ignores_them() {
    let mut e = editor(1200.0, 900.0);
    let f = frame_node(&mut e, Rect::new(100.0, 100.0, 400.0, 300.0), None);
    let right = rect(&mut e, Rect::new(340.0, 20.0, 40.0, 40.0), RED, Some(f));
    let stretch = rect(&mut e, Rect::new(20.0, 240.0, 360.0, 40.0), BLUE, Some(f));
    sketch_rs::ui::inspector::apply(&mut e, "layout", &[right], |n| {
        n.layout_child.constraints.h = Constraint::Max
    });
    sketch_rs::ui::inspector::apply(&mut e, "layout", &[stretch], |n| {
        n.layout_child.constraints.h = Constraint::Stretch;
        n.layout_child.constraints.v = Constraint::Max;
    });
    e.selection = vec![f];
    ev_drag(&mut e, v2(500.0, 400.0), v2(700.0, 500.0), 0.0, true);
    assert_eq!(e.doc.get(f).unwrap().rect.w, 600.0);
    assert_eq!(e.doc.get(right).unwrap().rect.x, 540.0);
    assert_eq!(
        e.doc.get(stretch).unwrap().rect,
        Rect::new(20.0, 340.0, 560.0, 40.0)
    );
    // Resizing again recomputes from the drag-start state: going back restores exactly.
    ev_drag(&mut e, v2(700.0, 500.0), v2(500.0, 400.0), 1000.0, true);
    assert_eq!(
        e.doc.get(stretch).unwrap().rect,
        Rect::new(20.0, 240.0, 360.0, 40.0)
    );
    // Cmd held: children stay put.
    e.handle(InputEvent::Modifiers(Modifiers::cmd()));
    ev_drag(&mut e, v2(500.0, 400.0), v2(600.0, 400.0), 2000.0, true);
    e.handle(InputEvent::Modifiers(Modifiers::NONE));
    assert_eq!(e.doc.get(right).unwrap().rect.x, 340.0);
}

#[test]
fn components_sync_instances_and_respect_overrides() {
    let mut e = editor(1200.0, 900.0);
    let f = frame_node(&mut e, Rect::new(0.0, 0.0, 200.0, 60.0), None);
    let label = e.add_node(
        sketch_rs::demo::text_node("Button", 16.0, v2(20.0, 20.0), Color::WHITE),
        Some(f),
    );
    let bg = rect(&mut e, Rect::new(0.0, 0.0, 200.0, 60.0), BLUE, Some(f));
    e.selection = vec![f];
    key(
        &mut e,
        Key::Char('k'),
        Modifiers {
            alt: true,
            ..Modifiers::cmd()
        },
    );
    assert!(e.doc.get(f).unwrap().component);
    let i1 = e.create_instance(f, v2(0.0, 200.0)).unwrap();
    let i2 = e.create_instance(f, v2(300.0, 200.0)).unwrap();
    assert_eq!(e.doc.subtree(i1).len(), 3);
    let inst_bg = |e: &Editor, i: NodeId| {
        e.doc
            .subtree(i)
            .into_iter()
            .find(|id| {
                e.doc
                    .get(*id)
                    .unwrap()
                    .instance
                    .as_ref()
                    .is_some_and(|l| l.main == bg)
            })
            .unwrap()
    };
    let inst_label = |e: &Editor, i: NodeId| {
        e.doc
            .subtree(i)
            .into_iter()
            .find(|id| {
                e.doc
                    .get(*id)
                    .unwrap()
                    .instance
                    .as_ref()
                    .is_some_and(|l| l.main == label)
            })
            .unwrap()
    };
    // Override the fill of instance 2's background.
    let b2 = inst_bg(&e, i2);
    sketch_rs::ui::inspector::apply(&mut e, "fill", &[b2], |n| n.style.set_fill(Some(RED)));
    // Edit the main component: fill and corner radius.
    sketch_rs::ui::inspector::apply(&mut e, "fill", &[bg], |n| {
        n.style.set_fill(Some(Color::rgb(0, 160, 90)))
    });
    sketch_rs::ui::inspector::apply(&mut e, "radius", &[bg], |n| n.style.corner_radius = 12.0);
    let n1 = e.doc.get(inst_bg(&e, i1)).unwrap();
    assert_eq!(
        n1.style.fill(),
        Some(Color::rgb(0, 160, 90)),
        "instance follows main"
    );
    let n2 = e.doc.get(inst_bg(&e, i2)).unwrap();
    assert_eq!(n2.style.fill(), Some(RED), "override kept");
    assert_eq!(
        n2.style.corner_radius, 12.0,
        "non-overridden props still sync"
    );
    // Text override on one instance.
    let l1 = inst_label(&e, i1);
    sketch_rs::ui::inspector::apply(&mut e, "text", &[l1], |n| {
        if let NodeKind::Text { text, .. } = &mut n.kind {
            *text = "Buy now".into();
        }
    });
    // A new child in the main appears in every instance.
    let dot = rect(
        &mut e,
        Rect::new(170.0, 20.0, 20.0, 20.0),
        Color::WHITE,
        Some(f),
    );
    for i in [i1, i2] {
        assert!(e.doc.subtree(i).iter().any(|id| {
            e.doc
                .get(*id)
                .unwrap()
                .instance
                .as_ref()
                .is_some_and(|l| l.main == dot)
        }));
    }
    let NodeKind::Text { text, .. } = &e.doc.get(l1).unwrap().kind else {
        unreachable!()
    };
    assert_eq!(text, "Buy now");
    // Reset overrides on instance 2, then detach instance 1.
    e.selection = vec![i2];
    e.reset_overrides();
    assert_eq!(
        e.doc.get(inst_bg(&e, i2)).unwrap().style.fill(),
        Some(Color::rgb(0, 160, 90))
    );
    e.selection = vec![i1];
    e.detach_instance();
    assert!(!e.is_instance_root(i1));
    sketch_rs::ui::inspector::apply(&mut e, "fill", &[bg], |n| n.style.set_fill(Some(BLUE)));
    assert_ne!(
        e.doc
            .get(e.doc.get(i1).unwrap().children[1])
            .unwrap()
            .style
            .fill(),
        Some(BLUE)
    );
    e.doc.validate().unwrap();
}

#[test]
fn pen_draws_curves_and_points_can_be_edited() {
    let mut e = editor(1000.0, 800.0);
    e.run(sketch_rs::editor::shortcuts::Action::Tool(Tool::Pen));
    click(&mut e, v2(100.0, 100.0), 0.0);
    // Click-drag: a smooth point.
    ev_drag(&mut e, v2(300.0, 100.0), v2(350.0, 160.0), 500.0, true);
    click(&mut e, v2(300.0, 300.0), 1200.0);
    // Clicking the first point closes the path.
    click(&mut e, v2(101.0, 101.0), 1600.0);
    assert_eq!(e.tool, Tool::Select);
    let id = e.selection[0];
    let n = e.doc.get(id).unwrap();
    let NodeKind::Path { subpaths, .. } = &n.kind else {
        panic!("not a path")
    };
    assert!(subpaths[0].closed);
    assert_eq!(subpaths[0].points.len(), 3);
    assert!(subpaths[0].points[1].h_out.is_some() && subpaths[0].points[1].h_in.is_some());
    assert!(!n.style.fills.is_empty(), "closed paths get a fill");
    assert_eq!(e.history.undo_len(), 1, "drawing a path is one undo step");
    // Edit: drag the third point.
    e.start_path_edit(id);
    ev_drag(&mut e, v2(300.0, 300.0), v2(260.0, 380.0), 3000.0, true);
    let NodeKind::Path { subpaths, .. } = &e.doc.get(id).unwrap().kind else {
        unreachable!()
    };
    let p = wr(&e, id).min() + subpaths[0].points[2].p;
    assert!(p.dist(v2(260.0, 380.0)) < 1e-6, "{p:?}");
    key(&mut e, Key::Delete, Modifiers::NONE);
    let NodeKind::Path { subpaths, .. } = &e.doc.get(id).unwrap().kind else {
        unreachable!()
    };
    assert_eq!(subpaths[0].points.len(), 2);
}

#[test]
fn booleans_and_flatten() {
    let mut e = editor(1000.0, 800.0);
    let a = rect(&mut e, Rect::new(0.0, 0.0, 100.0, 100.0), RED, None);
    let b = e.add_node(
        Node::new(
            NodeId(0),
            NodeKind::Ellipse,
            Rect::new(50.0, 50.0, 100.0, 100.0),
        ),
        None,
    );
    e.selection = vec![a, b];
    e.boolean(BoolOp::Subtract);
    let id = e.selection[0];
    let n = e.doc.get(id).unwrap();
    assert!(matches!(n.kind, NodeKind::Path { .. }));
    assert_eq!(
        n.style.fill(),
        Some(RED),
        "subtract keeps the bottom layer's style"
    );
    assert_eq!(wr(&e, id), Rect::new(0.0, 0.0, 100.0, 100.0));
    assert!(sketch_rs::editor::hit::node_contains(
        &e.doc,
        id,
        v2(10.0, 10.0),
        1.0
    ));
    assert!(!sketch_rs::editor::hit::node_contains(
        &e.doc,
        id,
        v2(90.0, 90.0),
        1.0
    ));
    e.undo();
    assert_eq!(e.doc.nodes.len(), 2);
    e.selection = vec![a, b];
    key(
        &mut e,
        Key::Char('u'),
        Modifiers {
            alt: true,
            shift: true,
            ..Modifiers::NONE
        },
    );
    assert_eq!(wr(&e, e.selection[0]), Rect::new(0.0, 0.0, 150.0, 150.0));
    // Flatten text into outlines.
    let t = e.add_node(
        sketch_rs::demo::text_node("Hi", 40.0, v2(300.0, 300.0), Color::INK),
        None,
    );
    e.selection = vec![t];
    key(&mut e, Key::Char('e'), Modifiers::cmd());
    assert!(matches!(e.doc.get(t).unwrap().kind, NodeKind::Path { .. }));
}

#[test]
fn rich_text_ranges_and_smart_guides() {
    let mut e = editor(1000.0, 800.0);
    let t = e.add_node(
        sketch_rs::demo::text_node("Hello world", 20.0, v2(100.0, 100.0), Color::INK),
        None,
    );
    e.start_text_edit(t, None);
    key(&mut e, Key::End, Modifiers::NONE);
    // Select "world" with the keyboard and make it bold.
    for _ in 0..5 {
        key(&mut e, Key::Left, Modifiers::SHIFT);
    }
    assert_eq!(e.text_selection(), Some((6, 11)));
    let w0 = e.doc.get(t).unwrap().rect.w;
    key(&mut e, Key::Char('b'), Modifiers::cmd());
    let n = e.doc.get(t).unwrap();
    assert_eq!(n.text_style.spans.len(), 1);
    assert!(n.text_style.char_style(7, 20.0, Color::INK).bold);
    assert!(!n.text_style.char_style(2, 20.0, Color::INK).bold);
    assert!(n.rect.w > w0, "bold glyphs are wider");
    // Typing over the selection keeps the bold run.
    e.handle(InputEvent::Text("there".into()));
    let n = e.doc.get(t).unwrap();
    let NodeKind::Text { text, .. } = &n.kind else {
        unreachable!()
    };
    assert_eq!(text, "Hello there");
    assert!(n.text_style.char_style(8, 20.0, Color::INK).bold);
    e.end_text_edit();

    // Equal spacing guides while moving between two neighbors.
    let mut e = editor(1000.0, 800.0);
    rect(&mut e, Rect::new(100.0, 100.0, 50.0, 50.0), RED, None);
    rect(&mut e, Rect::new(400.0, 100.0, 50.0, 50.0), RED, None);
    let m = rect(&mut e, Rect::new(240.0, 300.0, 50.0, 50.0), BLUE, None);
    e.selection = vec![m];
    e.handle(InputEvent::PointerDown {
        pos: v2(260.0, 320.0),
        button: Button::Primary,
        pressure: None,
        time_ms: 0.0,
    });
    for i in 1..=10 {
        e.handle(InputEvent::PointerMove {
            pos: v2(260.0 + i as f64 * 1.3, 320.0 - i as f64 * 20.0),
            pressure: None,
            time_ms: i as f64 * 16.0,
        });
    }
    assert!(
        e.guides
            .iter()
            .any(|g| matches!(g, sketch_rs::editor::snap::Guide::Gap { .. })),
        "{:?}",
        e.guides
    );
    e.handle(InputEvent::PointerUp {
        pos: v2(273.0, 120.0),
        button: Button::Primary,
        time_ms: 300.0,
    });
    assert_eq!(
        wr(&e, m).x,
        250.0,
        "centered between neighbors: gaps 100/100"
    );

    // Alt-hover measures the distance to another layer.
    e.handle(InputEvent::Modifiers(Modifiers {
        alt: true,
        ..Modifiers::NONE
    }));
    e.handle(InputEvent::PointerMove {
        pos: v2(420.0, 120.0),
        pressure: None,
        time_ms: 500.0,
    });
    assert!(
        matches!(e.measures.first(), Some(sketch_rs::editor::snap::Guide::Gap { a, b }) if (b.x - a.x - 100.0).abs() < 1e-9)
    );
}

#[test]
fn align_and_distribute() {
    let mut e = editor(1000.0, 800.0);
    let ids: Vec<NodeId> = [(0.0, 0.0), (130.0, 40.0), (400.0, 10.0)]
        .iter()
        .map(|(x, y)| rect(&mut e, Rect::new(*x, *y, 50.0, 30.0 + y), RED, None))
        .collect();
    e.selection = ids.clone();
    key(
        &mut e,
        Key::Char('w'),
        Modifiers {
            alt: true,
            ..Modifiers::NONE
        },
    );
    assert!(ids.iter().all(|id| wr(&e, *id).y == 0.0));
    key(
        &mut e,
        Key::Char('h'),
        Modifiers {
            alt: true,
            shift: true,
            ..Modifiers::NONE
        },
    );
    assert_eq!(wr(&e, ids[1]).x, 200.0);
    key(
        &mut e,
        Key::Char('d'),
        Modifiers {
            alt: true,
            ..Modifiers::NONE
        },
    );
    assert!(ids.iter().all(|id| wr(&e, *id).right() == 450.0));
}

/// A board that exercises every milestone-3 feature, rendered through the full app UI.
pub fn figma_board() -> Editor {
    let mut e = editor(1440.0, 900.0);
    e.doc.name = "Design system".into();
    // Component: a button with auto layout.
    let btn = frame_node(&mut e, Rect::new(0.0, 0.0, 10.0, 10.0), None);
    let label = e.add_node(
        sketch_rs::demo::text_node("Get started", 16.0, v2(0.0, 0.0), Color::WHITE),
        Some(btn),
    );
    let _ = label;
    sketch_rs::ui::inspector::apply(&mut e, "layout", &[btn], |n| {
        n.name = "Button".into();
        n.component = true;
        n.style.fills = vec![Paint::linear(
            Color::rgb(0x5b, 0x5b, 0xf6),
            Color::rgb(0x2f, 0x9b, 0xf3),
        )];
        n.style.fills[0] = match &n.style.fills[0] {
            Paint::Linear { stops, .. } => Paint::Linear {
                stops: stops.clone(),
                from: v2(0.0, 0.5),
                to: v2(1.0, 0.5),
            },
            p => p.clone(),
        };
        n.style.corner_radius = 12.0;
        n.layout = Some(AutoLayout {
            padding: [12.0, 22.0, 12.0, 22.0],
            align_cross: Align::Center,
            ..AutoLayout::default()
        });
        n.layout_child.sizing = [Sizing::Hug, Sizing::Hug];
        n.rect.x = 60.0;
        n.rect.y = 60.0;
    });
    // Instances with overrides.
    let i1 = e.create_instance(btn, v2(60.0, 140.0)).unwrap();
    let i2 = e.create_instance(btn, v2(240.0, 140.0)).unwrap();
    let i2_label = e.doc.get(i2).unwrap().children[0];
    sketch_rs::ui::inspector::apply(&mut e, "text", &[i2_label], |n| {
        if let NodeKind::Text { text, .. } = &mut n.kind {
            *text = "Upgrade →".into();
        }
    });
    sketch_rs::ui::inspector::apply(&mut e, "fill", &[i2], |n| {
        n.style.set_fill(Some(Color::rgb(0x1c, 0x1c, 0x1e)))
    });
    let _ = i1;
    // Card frame with auto layout (vertical) and constraints.
    let card = frame_node(&mut e, Rect::new(480.0, 40.0, 320.0, 380.0), None);
    sketch_rs::ui::inspector::apply(&mut e, "layout", &[card], |n| {
        n.name = "Card".into();
        n.style.corner_radius = 20.0;
        n.style.stroke = Some(Color::rgb(0xe2, 0xe2, 0xe8));
        n.layout = Some(AutoLayout {
            direction: Direction::Vertical,
            gap: 14.0,
            padding: [20.0; 4],
            ..AutoLayout::default()
        });
        n.layout_child.sizing = [Sizing::Fixed, Sizing::Hug];
    });
    // Image fill (generated pattern registered as an asset).
    let mut img = image::RgbaImage::new(160, 100);
    for (x, y, p) in img.enumerate_pixels_mut() {
        let t = x as f32 / 160.0;
        let w = ((x as f32 * 0.15).sin() + (y as f32 * 0.2).cos()) * 0.25 + 0.5;
        let c = |v: f32| v.clamp(0.0, 255.0) as u8;
        *p = image::Rgba([
            c(215.0 * (1.0 - t) * w + 40.0),
            c(180.0 * w + 30.0),
            c(255.0 * t),
            255,
        ]);
    }
    let asset = sketch_rs::assets::register_rgba(&img);
    let photo = rect(&mut e, Rect::new(0.0, 0.0, 280.0, 150.0), RED, Some(card));
    sketch_rs::ui::inspector::apply(&mut e, "fill", &[photo], |n| {
        n.name = "Photo".into();
        n.style.corner_radius = 12.0;
        n.style.fills = vec![Paint::Image {
            asset: asset.clone(),
            mode: ImageMode::Fill,
            opacity: 1.0,
        }];
        n.layout_child.sizing[0] = Sizing::Fill;
    });
    let title = e.add_node(
        sketch_rs::demo::text_node(
            "Rich text: bold, italic, colour",
            18.0,
            v2(0.0, 0.0),
            Color::INK,
        ),
        Some(card),
    );
    e.start_text_edit(title, None);
    if let Some(te) = &mut e.text_edit {
        te.anchor = Some(11);
        te.caret = 15;
    }
    e.toggle_bold();
    if let Some(te) = &mut e.text_edit {
        te.anchor = Some(17);
        te.caret = 23;
    }
    e.toggle_italic();
    if let Some(te) = &mut e.text_edit {
        te.anchor = Some(25);
        te.caret = 31;
    }
    e.style_text("color", |s| s.color = Some(Color::rgb(0xe8, 0x4a, 0x5f)));
    e.end_text_edit();
    let body = e.add_node(
        sketch_rs::demo::text_node(
            "Auto layout stacks these children vertically with a 14 px gap.",
            14.0,
            v2(0.0, 0.0),
            Color::rgb(0x6b, 0x6b, 0x75),
        ),
        Some(card),
    );
    sketch_rs::ui::inspector::apply(&mut e, "layout", &[body, title], |n| {
        n.layout_child.sizing[0] = Sizing::Fill;
        if let NodeKind::Text { auto_width, .. } = &mut n.kind {
            *auto_width = false;
        }
    });
    let cta = e.create_instance(btn, v2(0.0, 0.0)).unwrap();
    e.edit("Move", |tx| tx.reparent(cta, Some(card), None));
    // Shapes: star, polygon, gradients, dashed stroke, blend.
    let mut star = Node::new(
        NodeId(0),
        NodeKind::Star {
            points: 5,
            inner: 0.45,
        },
        Rect::new(880.0, 60.0, 140.0, 140.0),
    );
    star.style.fills = vec![Paint::radial(
        Color::rgb(0xff, 0xd8, 0x4d),
        Color::rgb(0xff, 0x7a, 0x1a),
    )];
    star.style.corner_radius = 6.0;
    e.add_node(star, None);
    let mut poly = Node::new(
        NodeId(0),
        NodeKind::Polygon { sides: 6 },
        Rect::new(1060.0, 60.0, 130.0, 140.0),
    );
    poly.style.set_fill(Some(Color::rgb(0x34, 0xc7, 0x8a)));
    poly.style.stroke = Some(Color::rgb(0x0f, 0x5e, 0x3f));
    poly.style.stroke_width = 4.0;
    poly.style.dash = vec![10.0, 6.0];
    poly.style.join = sketch_rs::model::Join::Round;
    e.add_node(poly, None);
    let c1 = e.add_node(
        Node::new(
            NodeId(0),
            NodeKind::Ellipse,
            Rect::new(880.0, 260.0, 150.0, 150.0),
        ),
        None,
    );
    let c2 = e.add_node(
        Node::new(
            NodeId(0),
            NodeKind::Ellipse,
            Rect::new(960.0, 260.0, 150.0, 150.0),
        ),
        None,
    );
    sketch_rs::ui::inspector::apply(&mut e, "fill", &[c1], |n| {
        n.style.set_fill(Some(Color::rgb(0x2f, 0x9b, 0xf3)))
    });
    sketch_rs::ui::inspector::apply(&mut e, "fill", &[c2], |n| {
        n.style.set_fill(Some(Color::rgb(0xff, 0x5c, 0x8a)));
        n.style.blend = BlendMode::Multiply;
    });
    // Boolean: a ring (circle minus circle).
    let outer = e.add_node(
        Node::new(
            NodeId(0),
            NodeKind::Ellipse,
            Rect::new(1150.0, 260.0, 150.0, 150.0),
        ),
        None,
    );
    let inner = e.add_node(
        Node::new(
            NodeId(0),
            NodeKind::Ellipse,
            Rect::new(1190.0, 300.0, 70.0, 70.0),
        ),
        None,
    );
    sketch_rs::ui::inspector::apply(&mut e, "fill", &[outer], |n| {
        n.style.set_fill(Some(Color::rgb(0x7c, 0x4d, 0xff)))
    });
    e.selection = vec![outer, inner];
    e.boolean(BoolOp::Subtract);
    // Pen path.
    e.run(sketch_rs::editor::shortcuts::Action::Tool(Tool::Pen));
    let s = |p: V2| e.camera.world_to_screen(p);
    let (p0, p1, p2) = (s(v2(60.0, 330.0)), s(v2(200.0, 300.0)), s(v2(380.0, 380.0)));
    click(&mut e, p0, 0.0);
    ev_drag(&mut e, p1, p1 + v2(60.0, -50.0), 300.0, true);
    click(&mut e, p2, 900.0);
    key(&mut e, Key::Enter, Modifiers::NONE);
    if let Some(&pen) = e.selection.first() {
        sketch_rs::ui::inspector::apply(&mut e, "stroke", &[pen], |n| {
            n.style.stroke = Some(Color::rgb(0xe8, 0x4a, 0x5f));
            n.style.stroke_width = 4.0;
            n.style.cap = sketch_rs::model::Cap::Round;
        });
    }
    e.selection = vec![card];
    e
}

#[test]
fn m3_figma_core_screenshot() {
    let Some(g) = gpu() else { return };
    let mut e = figma_board();
    e.doc.validate().unwrap();
    let mut st = UiState {
        project_name: "Design system".into(),
        save_state: "Saved".into(),
        ..Default::default()
    };
    let img = frame::capture(g, &mut e, &mut st, [1600.0, 1000.0], 2.0, |ed| {
        let b = ed.doc.content_bounds().unwrap();
        ed.camera.fit(&b, ed.viewport, 50.0, 1.0);
    });
    img.save(shot("milestone3_figma_core.png")).unwrap();
    // Canvas-only render with gradient, image and blend checks.
    let mut e = figma_board();
    e.selection.clear();
    e.viewport = v2(1400.0, 520.0);
    e.camera = sketch_rs::geom::Camera {
        pan: v2(20.0, 20.0),
        zoom: 1.0,
    };
    let img = render_canvas(g, &e);
    img.save(shot("milestone3_canvas.png")).unwrap();
    let at = |w: V2| px(&img, e.camera.world_to_screen(w));
    // Multiply blend: overlap of blue and pink is darker than either.
    let overlap = at(v2(995.0, 335.0));
    let blue = at(v2(900.0, 335.0));
    assert!(
        overlap[0] < blue[0] + 10 && overlap[1] < blue[1],
        "{overlap:?} vs {blue:?}"
    );
    // Ring: the hole shows the canvas.
    assert!(close(
        at(v2(1225.0, 335.0)),
        sketch_rs::render::scene::CANVAS_BG_LIGHT,
        6
    ));
    // Radial gradient star: center is yellow, tips are orange.
    let c = at(v2(950.0, 135.0));
    assert!(c[1] > 180, "{c:?}");
}
