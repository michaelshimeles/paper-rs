//! Headless GPU tests: render real scenes through wgpu (lavapipe on CI/VPS), save screenshots to
//! `docs/screenshots/`, and assert pixels at known positions.

mod common;

use common::*;

use sketch_rs::demo;
use sketch_rs::editor::{Button, Editor, InputEvent, Key, Modifiers, Tool};
use sketch_rs::geom::{Rect, V2, v2};
use sketch_rs::io::{file, png, svg};
use sketch_rs::model::{Color, Node, NodeId, NodeKind};
use sketch_rs::render::frame;
use sketch_rs::render::scene::{self, ACCENT, CANVAS_BG_LIGHT, GUIDE};
use sketch_rs::ui::UiState;

#[test]
fn m1_infinite_canvas_pan_and_zoom() {
    let Some(g) = gpu() else { return };
    let mut e = editor(800.0, 500.0);
    let mut n = Node::new(
        NodeId(0),
        NodeKind::Rect,
        Rect::new(100.0, 100.0, 120.0, 80.0),
    );
    n.style.set_fill(Some(ACCENT));
    let id = e.add_node(n, None);
    let far = Node::new(
        NodeId(0),
        NodeKind::Ellipse,
        Rect::new(-5000.0, 9000.0, 60.0, 60.0),
    );
    let far_id = e.add_node(far, None);

    let a = render_canvas(g, &e);
    a.save(shot("m1_canvas.png")).unwrap();
    assert!(close(px(&a, v2(160.0, 140.0)), ACCENT, 2));
    assert!(
        close(px(&a, v2(400.0, 400.0)), CANVAS_BG_LIGHT, 2)
            || close(px(&a, v2(400.0, 400.0)), scene::GRID_DOT_LIGHT, 4)
    );

    // Pan with the wheel, then zoom 3x about a cursor point using Ctrl+wheel.
    e.handle(InputEvent::Scroll {
        delta: v2(-200.0, -100.0),
        pos: v2(400.0, 250.0),
    });
    let anchor = v2(460.0, 290.0);
    let world_under_anchor = e.screen_to_world(anchor);
    e.handle(InputEvent::Modifiers(Modifiers {
        ctrl: true,
        ..Modifiers::NONE
    }));
    e.handle(InputEvent::Scroll {
        delta: v2(0.0, -(3.0f64.ln() * 100.0)),
        pos: anchor,
    });
    e.handle(InputEvent::Modifiers(Modifiers::NONE));
    assert!((e.camera.zoom - 3.0).abs() < 1e-9);
    assert!(
        e.screen_to_world(anchor).dist(world_under_anchor) < 1e-9,
        "zoom keeps the cursor point fixed"
    );
    let b = render_canvas(g, &e);
    b.save(shot("m1_zoomed.png")).unwrap();
    // The rect's center moved exactly where the camera says.
    let c = e.camera.world_to_screen(v2(160.0, 140.0));
    assert!(close(px(&b, c), ACCENT, 2));
    // Zoomed edges stay crisp: just inside vs just outside the (visible) right edge.
    let edge = e.camera.world_to_screen(v2(220.0, 140.0));
    assert!(
        edge.x > 4.0 && edge.x < 796.0,
        "right edge on screen: {edge:?}"
    );
    assert!(close(px(&b, edge - v2(1.0, 0.0)), ACCENT, 2));
    assert!(!close(px(&b, edge + v2(1.0, 0.0)), ACCENT, 40));

    // Far-away content is reachable: zoom to fit shows both.
    key(&mut e, Key::Char('1'), Modifiers::SHIFT);
    let fit = render_canvas(g, &e);
    fit.save(shot("m1_zoom_to_fit.png")).unwrap();
    for id in [id, far_id] {
        let r = e
            .camera
            .world_rect_to_screen(&e.doc.world_rect(id).unwrap());
        assert!(Rect::new(0.0, 0.0, 800.0, 500.0).contains_rect(&r), "{r:?}");
    }
}

#[test]
fn m2_freehand_sketch_with_smoothing() {
    let Some(g) = gpu() else { return };
    let mut e = editor(900.0, 420.0);
    // Same jittery zig-zag input drawn with smoothing 0, 0.5 and 0.9.
    let mut seed = 99u64;
    let mut jitter = || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((seed >> 33) as f64 / (1u64 << 31) as f64 - 0.5) * 6.0
    };
    let base: Vec<V2> = (0..=80)
        .map(|i| v2(i as f64 * 3.0, (i as f64 * 0.15).sin() * 50.0))
        .collect();
    let noisy: Vec<V2> = base.iter().map(|p| *p + v2(jitter(), jitter())).collect();
    for (k, s) in [0.0, 0.5, 0.9].iter().enumerate() {
        e.ink_smoothing = *s;
        let off = v2(40.0 + k as f64 * 290.0, 140.0);
        let pts: Vec<V2> = noisy.iter().map(|p| *p + off).collect();
        demo::sketch(&mut e, &pts, 4.0, Color::INK);
    }
    // A fast flick (thin, simulated pressure) vs a slow line (thicker).
    e.ink_smoothing = 0.5;
    e.tool = Tool::Pencil;
    let t0 = 10_000.0;
    e.handle(InputEvent::PointerDown {
        pos: v2(40.0, 300.0),
        button: Button::Primary,
        pressure: None,
        time_ms: t0,
    });
    for i in 1..=40 {
        e.handle(InputEvent::PointerMove {
            pos: v2(40.0 + i as f64 * 9.0, 300.0),
            pressure: None,
            time_ms: t0 + i as f64 * 2.0,
        });
    }
    e.handle(InputEvent::PointerUp {
        pos: v2(400.0, 300.0),
        button: Button::Primary,
        time_ms: t0 + 90.0,
    });
    e.handle(InputEvent::PointerDown {
        pos: v2(480.0, 300.0),
        button: Button::Primary,
        pressure: None,
        time_ms: t0 + 1000.0,
    });
    for i in 1..=120 {
        e.handle(InputEvent::PointerMove {
            pos: v2(480.0 + i as f64 * 3.0, 300.0),
            pressure: None,
            time_ms: t0 + 1000.0 + i as f64 * 30.0,
        });
    }
    e.handle(InputEvent::PointerUp {
        pos: v2(840.0, 300.0),
        button: Button::Primary,
        time_ms: t0 + 5000.0,
    });
    // Real pen pressure ramps from light to heavy.
    e.handle(InputEvent::PointerDown {
        pos: v2(40.0, 380.0),
        button: Button::Primary,
        pressure: Some(0.05),
        time_ms: t0 + 9000.0,
    });
    for i in 1..=80 {
        let p = i as f64 / 80.0;
        e.handle(InputEvent::PointerMove {
            pos: v2(40.0 + i as f64 * 10.0, 380.0),
            pressure: Some(p),
            time_ms: t0 + 9000.0 + i as f64 * 8.0,
        });
    }
    e.handle(InputEvent::PointerUp {
        pos: v2(840.0, 380.0),
        button: Button::Primary,
        time_ms: t0 + 9700.0,
    });

    assert_eq!(e.doc.nodes.len(), 6);
    assert_eq!(e.history.undo_len(), 6);
    let img = render_canvas(g, &e);
    img.save(shot("m2_sketch.png")).unwrap();

    // Ink is actually on screen for each smoothing level.
    for k in 0..3 {
        let r = Rect::new(40.0 + k as f64 * 290.0, 80.0, 250.0, 120.0);
        assert!(count(&img, r, is_dark) > 600, "stroke {k} missing");
    }
    // Speed-simulated pressure: the fast flick is thinner than the slow line.
    let thickness = |x: f64| count(&img, Rect::new(x, 285.0, 1.0, 30.0), is_dark);
    assert!(
        thickness(220.0) < thickness(660.0),
        "fast {} vs slow {}",
        thickness(220.0),
        thickness(660.0)
    );
    // Pen pressure: heavier end is thicker.
    let t_light = count(&img, Rect::new(120.0, 365.0, 1.0, 30.0), is_dark);
    let t_heavy = count(&img, Rect::new(760.0, 365.0, 1.0, 30.0), is_dark);
    assert!(t_light < t_heavy, "light {t_light} vs heavy {t_heavy}");
    // Smoothing makes the polyline shorter (less zig-zag).
    let length = |id: NodeId| match &e.doc.get(id).unwrap().kind {
        NodeKind::Stroke { points, .. } => points
            .windows(2)
            .map(|w| w[0].pos().dist(w[1].pos()))
            .sum::<f64>(),
        _ => 0.0,
    };
    let ids = e.doc.root.clone();
    assert!(length(ids[0]) > length(ids[1]) && length(ids[1]) > length(ids[2]));
}

#[test]
fn m3_shapes_frames_and_text() {
    let Some(g) = gpu() else { return };
    let mut e = editor(900.0, 560.0);
    e.handle(InputEvent::KeyDown {
        key: Key::Char('f'),
        repeat: false,
    });
    ev_drag(&mut e, v2(40.0, 60.0), v2(560.0, 520.0), 0.0, true);
    let frame_id = e.selection[0];
    e.handle(InputEvent::KeyDown {
        key: Key::Char('r'),
        repeat: false,
    });
    ev_drag(&mut e, v2(80.0, 100.0), v2(240.0, 220.0), 1000.0, true);
    let rect_id = e.selection[0];
    e.handle(InputEvent::KeyDown {
        key: Key::Char('o'),
        repeat: false,
    });
    ev_drag(&mut e, v2(300.0, 100.0), v2(460.0, 220.0), 2000.0, true);
    let ell_id = e.selection[0];
    e.handle(InputEvent::KeyDown {
        key: Key::Char('l'),
        repeat: false,
    });
    ev_drag(&mut e, v2(80.0, 260.0), v2(520.0, 300.0), 3000.0, true);
    // A rect that overflows the frame is clipped.
    e.handle(InputEvent::KeyDown {
        key: Key::Char('r'),
        repeat: false,
    });
    ev_drag(&mut e, v2(480.0, 400.0), v2(640.0, 480.0), 4000.0, true);
    let overflow = e.selection[0];
    e.edit("color", |tx| {
        tx.node_mut(rect_id)
            .unwrap()
            .style
            .set_fill(Some(Color::rgb(0xff, 0xe0, 0x82)));
        tx.node_mut(rect_id).unwrap().style.corner_radius = 16.0;
        tx.node_mut(ell_id).unwrap().style.set_fill(Some(ACCENT));
        tx.node_mut(overflow)
            .unwrap()
            .style
            .set_fill(Some(Color::rgb(0xe0, 0x4f, 0x5f)));
    });
    // Text typed through the text tool.
    e.handle(InputEvent::KeyDown {
        key: Key::Char('t'),
        repeat: false,
    });
    e.handle(InputEvent::PointerDown {
        pos: v2(80.0, 340.0),
        button: Button::Primary,
        pressure: None,
        time_ms: 5000.0,
    });
    e.handle(InputEvent::PointerUp {
        pos: v2(80.0, 340.0),
        button: Button::Primary,
        time_ms: 5010.0,
    });
    e.handle(InputEvent::Text("Plan · Work · Sketch".into()));
    let text_id = e.text_edit.as_ref().unwrap().id;
    e.handle(InputEvent::KeyDown {
        key: Key::Escape,
        repeat: false,
    });
    e.edit("size", |tx| {
        if let NodeKind::Text { font_size, .. } = &mut tx.node_mut(text_id).unwrap().kind {
            *font_size = 36.0;
        }
    });
    e.fit_text(text_id);
    e.commit();
    // A loose text on the canvas, outside the frame.
    let loose = demo::text_node(
        "Outside the frame",
        20.0,
        v2(600.0, 80.0),
        Color::rgb(0x55, 0x55, 0x60),
    );
    e.add_node(loose, None);
    e.selection.clear();

    for id in [rect_id, ell_id, text_id, overflow] {
        assert_eq!(
            e.doc.get(id).unwrap().parent,
            Some(frame_id),
            "drawn inside the frame"
        );
    }
    let img = render_canvas(g, &e);
    img.save(shot("m3_shapes_frames_text.png")).unwrap();

    assert!(
        close(px(&img, v2(160.0, 160.0)), Color::rgb(0xff, 0xe0, 0x82), 2),
        "rect fill"
    );
    assert!(
        !close(px(&img, v2(81.5, 101.5)), Color::rgb(0xff, 0xe0, 0x82), 30),
        "rounded corner is cut"
    );
    assert!(
        close(px(&img, v2(380.0, 160.0)), ACCENT, 2),
        "ellipse center"
    );
    assert!(
        !close(px(&img, v2(302.0, 102.0)), ACCENT, 30),
        "outside the ellipse"
    );
    assert!(
        close(px(&img, v2(520.0, 440.0)), Color::rgb(0xe0, 0x4f, 0x5f), 2),
        "overflow rect drawn inside the frame"
    );
    assert!(
        close(px(&img, v2(600.0, 440.0)), CANVAS_BG_LIGHT, 4)
            || close(px(&img, v2(600.0, 440.0)), scene::GRID_DOT_LIGHT, 4),
        "clipped by frame"
    );
    assert!(
        close(px(&img, v2(50.0, 500.0)), Color::WHITE, 1),
        "frame background"
    );
    let tr = e.doc.world_rect(text_id).unwrap();
    assert!(tr.w > 250.0, "36px text width {}", tr.w);
    assert!(count(&img, tr, is_dark) > 2000, "text glyphs rendered");
    assert!(
        count(&img, Rect::new(80.0, 270.0, 440.0, 20.0), is_dark) > 300,
        "line rendered"
    );
    // Frame label above the frame.
    assert!(
        count(&img, Rect::new(40.0, 40.0, 60.0, 18.0), |p| p[0] < 200) > 20,
        "frame name label"
    );
}

#[test]
fn m4_select_move_resize_snap() {
    let Some(g) = gpu() else { return };
    let mut e = editor(900.0, 520.0);
    let mk = |e: &mut Editor, r: Rect, c: Color| {
        let mut n = Node::new(NodeId(0), NodeKind::Rect, r);
        n.style.set_fill(Some(c));
        n.style.corner_radius = 8.0;
        e.add_node(n, None)
    };
    let a = mk(
        &mut e,
        Rect::new(80.0, 80.0, 200.0, 140.0),
        Color::rgb(0xff, 0xc2, 0xd1),
    );
    let b = mk(
        &mut e,
        Rect::new(420.0, 300.0, 160.0, 100.0),
        Color::rgb(0xb8, 0xf0, 0xd2),
    );
    let c = mk(
        &mut e,
        Rect::new(620.0, 60.0, 120.0, 120.0),
        Color::rgb(0xff, 0xe0, 0x82),
    );

    // Marquee-select b and c, then shift-click a: three selected.
    ev_drag(&mut e, v2(400.0, 40.0), v2(780.0, 440.0), 0.0, true);
    assert_eq!(e.selection, vec![b, c]);
    e.handle(InputEvent::Modifiers(Modifiers::SHIFT));
    e.handle(InputEvent::PointerDown {
        pos: v2(150.0, 150.0),
        button: Button::Primary,
        pressure: None,
        time_ms: 1000.0,
    });
    e.handle(InputEvent::PointerUp {
        pos: v2(150.0, 150.0),
        button: Button::Primary,
        time_ms: 1010.0,
    });
    e.handle(InputEvent::Modifiers(Modifiers::NONE));
    assert_eq!(e.selection.len(), 3);
    let multi = render_canvas(g, &e);
    multi.save(shot("m4_multi_select.png")).unwrap();
    let bounds = e.selection_bounds().unwrap();
    assert_eq!(bounds, Rect::new(80.0, 60.0, 660.0, 340.0));
    // Handle squares at the bounds corners are white with an accent border.
    assert!(close(px(&multi, v2(740.0, 400.0)), Color::WHITE, 2));
    assert!(close(px(&multi, v2(740.0 - 4.0, 400.0)), ACCENT, 60));

    // Drag b so its left edge approaches a's right edge (280) and its top a's top (80): snap.
    e.select(vec![b]);
    e.handle(InputEvent::PointerDown {
        pos: v2(500.0, 350.0),
        button: Button::Primary,
        pressure: None,
        time_ms: 2000.0,
    });
    e.handle(InputEvent::PointerMove {
        pos: v2(450.0, 300.0),
        pressure: None,
        time_ms: 2016.0,
    });
    e.handle(InputEvent::PointerMove {
        pos: v2(363.5, 133.2),
        pressure: None,
        time_ms: 2032.0,
    });
    let snapping = render_canvas(g, &e);
    snapping.save(shot("m4_snapping.png")).unwrap();
    let rb = e.doc.world_rect(b).unwrap();
    assert_eq!(
        (rb.x, rb.y),
        (280.0, 80.0),
        "snapped to a's right and top edges"
    );
    assert!(!e.guides.is_empty());
    assert!(
        count(&snapping, Rect::new(279.0, 20.0, 2.0, 400.0), |p| close(
            p, GUIDE, 40
        )) > 20,
        "vertical guide drawn"
    );
    e.handle(InputEvent::PointerUp {
        pos: v2(363.5, 133.2),
        button: Button::Primary,
        time_ms: 2048.0,
    });

    // Resize c from its bottom-left handle with Shift (aspect locked).
    e.select(vec![c]);
    e.handle(InputEvent::Modifiers(Modifiers::SHIFT));
    ev_drag(&mut e, v2(620.0, 180.0), v2(560.0, 230.0), 3000.0, true);
    e.handle(InputEvent::Modifiers(Modifiers::NONE));
    let rc = e.doc.world_rect(c).unwrap();
    assert!((rc.w - rc.h).abs() < 1e-9 && rc.w > 150.0, "{rc:?}");
    assert_eq!(rc.right(), 740.0, "anchored at the opposite edge");
    let resized = render_canvas(g, &e);
    resized.save(shot("m4_resized.png")).unwrap();
    assert!(close(
        px(&resized, rc.center()),
        Color::rgb(0xff, 0xe0, 0x82),
        2
    ));
    assert_eq!(e.history.undo_len(), 5);
    let _ = a;
}

#[test]
fn m5_layers_panel_and_undo_redo() {
    let Some(g) = gpu() else { return };
    let mut e = demo::showcase();
    let mut st = UiState::default();
    let week = e.doc.root[0];
    // Hide the sketches frame and delete a note, via the same actions the panel uses.
    let sketches = e.doc.root[1];
    e.select(vec![sketches]);
    key(&mut e, Key::Char('h'), Modifiers::cmd_shift());
    let note = e.doc.get(week).unwrap().children[2];
    e.select(vec![note]);
    key(&mut e, Key::Delete, Modifiers::NONE);
    let before_undo = e.doc.clone();
    let img = frame::capture(g, &mut e, &mut st, [1280.0, 760.0], 2.0, |e| {
        e.zoom_to_fit()
    });
    img.save(shot("m5_layers_edited.png")).unwrap();
    assert!(!e.doc.get(sketches).unwrap().visible);

    key(&mut e, Key::Char('z'), Modifiers::cmd());
    key(&mut e, Key::Char('z'), Modifiers::cmd());
    assert!(e.doc.get(sketches).unwrap().visible);
    assert!(e.doc.get(note).is_some());
    e.select(vec![note]);
    let img2 = frame::capture(g, &mut e, &mut st, [1280.0, 760.0], 2.0, |e| {
        e.zoom_to_fit()
    });
    img2.save(shot("m5_layers_after_undo.png")).unwrap();
    // The two frames differ (note + sketches back).
    let diff = img
        .pixels()
        .zip(img2.pixels())
        .filter(|(a, b)| a != b)
        .count();
    assert!(
        diff > 10_000,
        "undo visibly restored content ({diff} px changed)"
    );
    key(&mut e, Key::Char('z'), Modifiers::cmd_shift());
    key(&mut e, Key::Char('z'), Modifiers::cmd_shift());
    assert_eq!(e.doc, before_undo);
}

#[test]
fn m6_save_load_and_export() {
    let Some(g) = gpu() else { return };
    let e = demo::showcase();
    let dir = std::env::temp_dir().join(format!("sketch-rs-m6-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("launch.sketch-rs");
    file::save(&path, &e.doc, Some(&e.camera)).unwrap();
    let (doc, cam) = file::load(&path).unwrap();
    assert_eq!(doc, e.doc);
    assert_eq!(cam, Some(e.camera));
    std::fs::copy(&path, shot("m6_launch_plan.sketch-rs")).unwrap();

    // PNG export of the first frame at 2x, white background.
    let week = doc.root[0];
    let img = png::render(g, &doc, Some(&[week]), 2.0, Some(Color::WHITE)).unwrap();
    img.save(shot("m6_export_frame@2x.png")).unwrap();
    assert_eq!(img.dimensions(), (1280, 880));
    // First note (yellow) at frame-local (120, 180) -> (240, 360) in 2x pixels.
    assert!(close(
        img.get_pixel(240, 360).0,
        Color::rgb(0xff, 0xe0, 0x82),
        2
    ));
    // Everything export with transparency.
    let all = png::render(g, &doc, None, 1.0, None).unwrap();
    all.save(shot("m6_export_all_transparent.png")).unwrap();
    assert_eq!(
        all.get_pixel(all.width() - 1, all.height() - 1).0[3],
        0,
        "transparent outside content"
    );
    assert!(close(all.get_pixel(10, 10).0, Color::WHITE, 1));

    // The loaded document renders identically to the original.
    let a = png::render(g, &e.doc, None, 1.0, Some(Color::WHITE)).unwrap();
    let b = png::render(g, &doc, None, 1.0, Some(Color::WHITE)).unwrap();
    assert!(a == b, "round-tripped document renders identically");

    let s = svg::export(&doc, None, Some(Color::WHITE)).unwrap();
    std::fs::write(shot("m6_export.svg"), &s).unwrap();
    assert!(s.starts_with("<svg") && s.contains("clip-path") && s.contains("Week plan"));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn app_full_ui_screenshot() {
    let Some(g) = gpu() else { return };
    let mut e = demo::showcase();
    let sel = e.doc.get(e.doc.root[0]).unwrap().children[2];
    let mut st = UiState::default();
    let img = frame::capture(g, &mut e, &mut st, [1440.0, 900.0], 2.0, |e| {
        e.zoom_to_fit();
        e.select(vec![sel]);
    });
    img.save(shot("app_demo.png")).unwrap();
    // Chrome present: left panel background and canvas background both visible.
    assert!(
        close(
            img.get_pixel(20, 600).0,
            {
                let c = sketch_rs::theme::palette(true).sidebar;
                Color::rgb(c.r(), c.g(), c.b())
            },
            3
        ),
        "layers panel (dark sidebar)"
    );
    let bg = img.get_pixel(1300, 1600).0;
    assert!(
        close(bg, scene::CANVAS_BG_DARK, 3) || close(bg, scene::GRID_DOT_DARK, 4),
        "canvas background {bg:?}"
    );
}

#[test]
fn ui_theme_choice_switches_chrome_and_canvas() {
    let Some(g) = gpu() else { return };
    use sketch_rs::options::ThemeChoice;
    let mut e = demo::showcase();
    let mut st = UiState::default();
    let sidebar = |dark: bool| {
        let c = sketch_rs::theme::palette(dark).sidebar;
        Color::rgb(c.r(), c.g(), c.b())
    };

    st.options.theme = ThemeChoice::Light;
    let light = frame::capture(g, &mut e, &mut st, [800.0, 500.0], 2.0, |_| {});
    st.options.theme = ThemeChoice::Dark;
    let dark = frame::capture(g, &mut e, &mut st, [800.0, 500.0], 2.0, |_| {});

    assert!(
        close(light.get_pixel(20, 600).0, sidebar(false), 3),
        "light sidebar"
    );
    assert!(
        close(dark.get_pixel(20, 600).0, sidebar(true), 3),
        "dark sidebar"
    );
    let canvas = |img: &image::RgbaImage, dark: bool| {
        let (bg, dot) = if dark {
            (scene::CANVAS_BG_DARK, scene::GRID_DOT_DARK)
        } else {
            (scene::CANVAS_BG_LIGHT, scene::GRID_DOT_LIGHT)
        };
        let p = img.get_pixel(1000, 600).0;
        close(p, bg, 3) || close(p, dot, 4)
    };
    assert!(canvas(&light, false), "light canvas");
    assert!(canvas(&dark, true), "dark canvas");
}

#[test]
fn m6_svg_export_matches_gpu_render() {
    let Some(g) = gpu() else { return };
    let e = demo::showcase();
    let gpu_img = png::render(g, &e.doc, None, 1.0, Some(Color::WHITE)).unwrap();
    let s = svg::export(&e.doc, None, Some(Color::WHITE)).unwrap();
    let tree =
        resvg::usvg::Tree::from_str(&s, &resvg::usvg::Options::default()).expect("valid SVG");
    let (w, h) = gpu_img.dimensions();
    assert_eq!(
        (tree.size().width() as u32, tree.size().height() as u32),
        (w, h)
    );
    let mut pm = resvg::tiny_skia::Pixmap::new(w, h).unwrap();
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::default(),
        &mut pm.as_mut(),
    );
    let svg_img = image::RgbaImage::from_raw(w, h, pm.data().to_vec()).unwrap();
    svg_img.save(shot("m6_export_svg_via_resvg.png")).unwrap();
    // Allow anti-aliasing differences along edges, but no missing or miscolored content.
    let differing = gpu_img
        .pixels()
        .zip(svg_img.pixels())
        .filter(|(a, b)| (0..3).any(|c| (a.0[c] as i32 - b.0[c] as i32).abs() > 64))
        .count();
    let pct = differing as f64 / (w * h) as f64 * 100.0;
    eprintln!("SVG vs GPU: {differing} px differ ({pct:.3}%)");
    assert!(
        pct < 0.5,
        "SVG export diverges from the GPU render: {pct:.3}%"
    );
}

/// Regression: on macOS the first surface frame is often `Occluded`. The textures delta from that
/// skipped frame (the initial font atlas) must still be uploaded, or the next frame's partial
/// atlas update panics with "Tried to update a texture that has not been allocated yet".
#[test]
fn skipped_first_frame_keeps_egui_textures_in_sync() {
    let Some(g) = gpu() else { return };
    let ctx = egui::Context::default();
    sketch_rs::ui::setup_style(&ctx);
    let mut e = demo::showcase();
    let mut st = UiState::default();
    let (pts, ppp) = ([800.0f32, 600.0f32], 2.0f32);
    let size = [(pts[0] * ppp) as u32, (pts[1] * ppp) as u32];
    let input = || egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(pts[0], pts[1]),
        )),
        viewports: std::iter::once((
            egui::ViewportId::ROOT,
            egui::ViewportInfo {
                native_pixels_per_point: Some(ppp),
                ..Default::default()
            },
        ))
        .collect(),
        ..Default::default()
    };
    let mut painter = frame::Painter::new(&g.device, sketch_rs::render::gpu::OFFSCREEN_FORMAT);
    let tex = g.target(size);
    let view = tex.create_view(&Default::default());
    // Frame 1: surface unavailable -> skipped.
    let mut l = frame::layout(&ctx, input(), &mut e, &mut st);
    assert!(
        !l.output.textures_delta.set.is_empty(),
        "first frame should allocate the font atlas"
    );
    painter.skip_frame(&g.device, &g.queue, &mut l.output.textures_delta);
    // Following frames render normally; force new glyphs so egui emits partial atlas updates.
    for i in 0..4 {
        let l = frame::layout(&ctx, input(), &mut e, &mut st);
        ctx.fonts_mut(|f| {
            let _ = f.layout_no_wrap(
                format!("ÀÉÎÕÜ ŁŃŚŹ ΩΔΣ жфц {i} ✓✗"),
                egui::FontId::proportional(13.0 + i as f32 * 7.0),
                egui::Color32::BLACK,
            );
        });
        painter.paint(&g.device, &g.queue, &view, size, &ctx, l, &e, true);
    }
}
