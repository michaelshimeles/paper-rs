//! Helpers shared by the integration tests.
#![allow(dead_code)]

use sketch_rs::editor::{Button, Editor, InputEvent, Key, Modifiers};
use sketch_rs::geom::{Camera, Rect, V2, v2};
use sketch_rs::model::Color;
use sketch_rs::render::gpu::Headless;
use sketch_rs::render::scene;
use std::path::PathBuf;
use std::sync::OnceLock;

pub const SCALE: f64 = 2.0;

pub fn gpu() -> Option<&'static Headless> {
    static GPU: OnceLock<Option<Headless>> = OnceLock::new();
    GPU.get_or_init(|| match Headless::new() {
        Ok(g) => {
            eprintln!(
                "GPU adapter: {} ({:?})",
                g.adapter_info.name, g.adapter_info.backend
            );
            Some(g)
        }
        Err(e) => {
            assert!(
                std::env::var_os("SKETCH_RS_REQUIRE_GPU").is_none(),
                "GPU required: {e}"
            );
            eprintln!("skipping GPU test: {e}");
            None
        }
    })
    .as_ref()
}

pub fn shot(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/screenshots");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

/// Render the interactive canvas (no chrome) at 2x.
pub fn render_canvas(g: &Headless, ed: &Editor) -> image::RgbaImage {
    let frame = scene::canvas(ed, &mut scene::SceneCache::default(), V2::ZERO, true, false);
    let size = [
        (ed.viewport.x * SCALE) as u32,
        (ed.viewport.y * SCALE) as u32,
    ];
    g.render_frame(&frame, size, SCALE as f32, [1.0; 4])
}

/// Pixel at a logical (canvas) position.
pub fn px(img: &image::RgbaImage, p: V2) -> [u8; 4] {
    img.get_pixel((p.x * SCALE) as u32, (p.y * SCALE) as u32).0
}

pub fn close(a: [u8; 4], c: Color, tol: i32) -> bool {
    (a[0] as i32 - c.r as i32).abs() <= tol
        && (a[1] as i32 - c.g as i32).abs() <= tol
        && (a[2] as i32 - c.b as i32).abs() <= tol
}

pub fn is_dark(a: [u8; 4]) -> bool {
    (a[0] as u32 + a[1] as u32 + a[2] as u32) < 300
}

pub fn count(img: &image::RgbaImage, r: Rect, pred: impl Fn([u8; 4]) -> bool) -> usize {
    let mut n = 0;
    for y in (r.y * SCALE) as u32..(r.bottom() * SCALE) as u32 {
        for x in (r.x * SCALE) as u32..(r.right() * SCALE) as u32 {
            if x < img.width() && y < img.height() && pred(img.get_pixel(x, y).0) {
                n += 1;
            }
        }
    }
    n
}

pub fn editor(w: f64, h: f64) -> Editor {
    let mut e = Editor::default();
    e.viewport = v2(w, h);
    e.camera = Camera {
        pan: V2::ZERO,
        zoom: 1.0,
    };
    e
}

pub fn ev_drag(e: &mut Editor, a: V2, b: V2, t: f64, release: bool) {
    e.handle(InputEvent::PointerDown {
        pos: a,
        button: Button::Primary,
        pressure: None,
        time_ms: t,
    });
    for i in 1..=12 {
        e.handle(InputEvent::PointerMove {
            pos: a.lerp(b, i as f64 / 12.0),
            pressure: None,
            time_ms: t + i as f64 * 16.0,
        });
    }
    if release {
        e.handle(InputEvent::PointerUp {
            pos: b,
            button: Button::Primary,
            time_ms: t + 220.0,
        });
    }
}

pub fn key(e: &mut Editor, k: Key, m: Modifiers) {
    e.handle(InputEvent::Modifiers(m));
    e.handle(InputEvent::KeyDown {
        key: k,
        repeat: false,
    });
    e.handle(InputEvent::Modifiers(Modifiers::NONE));
}
