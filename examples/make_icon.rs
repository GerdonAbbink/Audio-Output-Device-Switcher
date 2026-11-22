//! Generates `assets/app.ico`, the icon embedded in the exe.
//!
//! Run with `cargo run --example make_icon` after changing the design.
//! A blue rounded tile with a white headset, at every size Windows uses.

use std::f32::consts::PI;
use tiny_skia::{Color, FillRule, LineCap, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

const SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

fn rounded_rect(pb: &mut PathBuilder, x: f32, y: f32, w: f32, h: f32, r: f32) {
    let k = 0.5523 * r; // cubic approximation of a quarter circle
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
}

/// Same headset glyph as the tray icon, in a 16x16 unit grid.
fn headset(px: &mut Pixmap, paint: &Paint, t: Transform) {
    let mut band = PathBuilder::new();
    for i in 0..=32 {
        let a = (180.0 + 180.0 * i as f32 / 32.0) * PI / 180.0;
        let (x, y) = (8.0 + 6.0 * a.cos(), 9.0 + 6.0 * a.sin());
        if i == 0 { band.move_to(x, y) } else { band.line_to(x, y) }
    }
    if let Some(p) = band.finish() {
        let s = Stroke { width: 1.7, line_cap: LineCap::Round, ..Default::default() };
        px.stroke_path(&p, paint, &s, t, None);
    }
    let mut cups = PathBuilder::new();
    cups.push_rect(Rect::from_ltrb(1.0, 8.5, 5.0, 15.0).unwrap());
    cups.push_rect(Rect::from_ltrb(11.0, 8.5, 15.0, 15.0).unwrap());
    if let Some(p) = cups.finish() {
        px.fill_path(&p, paint, FillRule::Winding, t, None);
    }
}

fn draw(size: u32) -> Pixmap {
    let mut px = Pixmap::new(size, size).unwrap();
    let s = size as f32 / 16.0;

    let mut tile = Paint::default();
    tile.set_color(Color::from_rgba8(0, 120, 212, 255)); // Windows blue
    tile.anti_alias = true;
    let mut pb = PathBuilder::new();
    rounded_rect(&mut pb, 0.5, 0.5, 15.0, 15.0, 3.5);
    if let Some(p) = pb.finish() {
        px.fill_path(&p, &tile, FillRule::Winding, Transform::from_scale(s, s), None);
    }

    let mut glyph = Paint::default();
    glyph.set_color(Color::from_rgba8(255, 255, 255, 255));
    glyph.anti_alias = true;
    let k = 0.62; // glyph scale inside the tile
    let off = 8.0 * (1.0 - k);
    headset(&mut px, &glyph, Transform::from_scale(s * k, s * k).post_translate(s * off, s * (off - 0.3)));
    px
}

fn main() {
    // ICO with PNG-compressed entries (supported since Windows Vista).
    let images: Vec<(u32, Vec<u8>)> = SIZES.iter().map(|&n| (n, draw(n).encode_png().unwrap())).collect();
    let mut ico = Vec::new();
    ico.extend_from_slice(&[0, 0, 1, 0]);
    ico.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (n, png) in &images {
        let dim = if *n >= 256 { 0 } else { *n as u8 };
        ico.extend_from_slice(&[dim, dim, 0, 0]);
        ico.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        ico.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in &images {
        ico.extend_from_slice(png);
    }
    std::fs::create_dir_all("assets").unwrap();
    std::fs::write("assets/app.ico", &ico).unwrap();
    draw(256).save_png("assets/app-256.png").unwrap();
    println!("wrote assets/app.ico ({} sizes, {} bytes)", images.len(), ico.len());
}
