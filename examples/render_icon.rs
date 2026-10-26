//! Renders the brush logo (packaging/logo-art.svg) into the app icon PNGs and the
//! start-screen wordmark: `cargo run --example render_icon -- <out_dir>`.

fn render(svg: &str, width: u32, height: u32, path: &str) {
    let tree = resvg::usvg::Tree::from_data(svg.as_bytes(), &resvg::usvg::Options::default()).expect("valid svg");
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height).unwrap();
    let scale = width as f32 / tree.size().width();
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    pixmap.save_png(path).unwrap();
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "target/icon".into());
    std::fs::create_dir_all(&out).unwrap();
    let art = std::fs::read_to_string("packaging/logo-art.svg").expect("packaging/logo-art.svg");
    let art = art.split_once("-->").map(|(_, rest)| rest).unwrap_or(&art);

    // App icon: the painting on a dark macOS-style rounded square.
    let icon = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024">
  <defs><linearGradient id="bg" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#1f232b"/><stop offset="1" stop-color="#0b0d11"/></linearGradient></defs>
  <rect x="100" y="100" width="824" height="824" rx="185" fill="url(#bg)"/>
  <rect x="101.5" y="101.5" width="821" height="821" rx="183.5" fill="none" stroke="#ffffff" stroke-opacity="0.07" stroke-width="3"/>
  <g transform="translate(512 512) scale(0.92) translate(-512 -530)">{art}</g>
</svg>"##
    );
    std::fs::write(format!("{out}/icon.svg"), &icon).unwrap();
    for size in [16u32, 32, 64, 128, 256, 512, 1024] {
        render(&icon, size, size, &format!("{out}/icon_{size}.png"));
    }

    // Wordmark: just the painting, transparent, cropped to the letters.
    let wordmark = format!(r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="165 255 710 540">{art}</svg>"##);
    render(&wordmark, 710, 540, &format!("{out}/wordmark.png"));
    println!("rendered to {out}");
}
