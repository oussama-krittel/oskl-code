//! Renders packaging/icon.svg to PNGs for the macOS app icon: `cargo run --example render_icon -- <out_dir>`.

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "target/icon".into());
    std::fs::create_dir_all(&out).unwrap();
    let svg = std::fs::read("packaging/icon.svg").expect("packaging/icon.svg");
    let tree = resvg::usvg::Tree::from_data(&svg, &resvg::usvg::Options::default()).expect("valid svg");
    for size in [16u32, 32, 64, 128, 256, 512, 1024] {
        let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size).unwrap();
        let scale = size as f32 / tree.size().width();
        resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
        pixmap.save_png(format!("{out}/icon_{size}.png")).unwrap();
    }
    println!("rendered to {out}");
}
