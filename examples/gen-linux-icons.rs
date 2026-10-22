//! Renders the app icon into the sizes the Linux icon theme expects.
//!
//! The brand icon in `site/scripts/brand/icon.svg` is the source, the same one the macOS
//! release script turns into an `.icns` file. Run this after changing it:
//!
//! ```sh
//! cargo run --example gen-linux-icons
//! ```

use resvg::{tiny_skia, usvg};

/// Sizes the freedesktop icon theme spec lists for apps.
const SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = root.join("site/scripts/brand/icon.svg");
    let svg = std::fs::read_to_string(&source).expect("read the brand icon");
    let tree = usvg::Tree::from_str(&svg, &usvg::Options::default()).expect("parse the brand icon");

    for size in SIZES {
        let mut pixmap = tiny_skia::Pixmap::new(size, size).expect("pixmap");
        let scale = size as f32 / tree.size().width();
        resvg::render(
            &tree,
            tiny_skia::Transform::from_scale(scale, scale),
            &mut pixmap.as_mut(),
        );
        let image = image::RgbaImage::from_raw(size, size, pixmap.data().to_vec()).expect("pixels");
        let dir = root.join(format!("packaging/linux/icons/hicolor/{size}x{size}/apps"));
        std::fs::create_dir_all(&dir).expect("create the icon folder");
        image
            .save(dir.join("sketch-rs.png"))
            .expect("write the icon");
        println!("wrote {}", dir.join("sketch-rs.png").display());
    }

    // The scalable copy is the source itself, so the theme can scale it to any size.
    let dir = root.join("packaging/linux/icons/hicolor/scalable/apps");
    std::fs::create_dir_all(&dir).expect("create the icon folder");
    std::fs::copy(&source, dir.join("sketch-rs.svg")).expect("copy the scalable icon");
    println!("wrote {}", dir.join("sketch-rs.svg").display());
}
