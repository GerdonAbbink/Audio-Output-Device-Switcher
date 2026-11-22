//! Tray glyphs drawn at runtime (crisp at any DPI), in a 16x16 unit grid.

use std::f32::consts::PI;
use tiny_skia::{Color, FillRule, LineCap, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

use crate::model::Status;

fn arc(pb: &mut PathBuilder, cx: f32, cy: f32, r: f32, a0: f32, a1: f32) {
    const N: usize = 24;
    for i in 0..=N {
        let a = (a0 + (a1 - a0) * i as f32 / N as f32) * PI / 180.0;
        let (x, y) = (cx + r * a.cos(), cy + r * a.sin());
        if i == 0 { pb.move_to(x, y) } else { pb.line_to(x, y) }
    }
}

fn stroke(px: &mut Pixmap, pb: PathBuilder, paint: &Paint, width: f32, t: Transform) {
    if let Some(path) = pb.finish() {
        let s = Stroke { width, line_cap: LineCap::Round, ..Default::default() };
        px.stroke_path(&path, paint, &s, t, None);
    }
}

fn fill(px: &mut Pixmap, pb: PathBuilder, paint: &Paint, t: Transform) {
    if let Some(path) = pb.finish() {
        px.fill_path(&path, paint, FillRule::Winding, t, None);
    }
}

fn speaker(px: &mut Pixmap, paint: &Paint, t: Transform) {
    let mut body = PathBuilder::new();
    body.push_rect(Rect::from_ltrb(1.0, 5.5, 4.5, 10.5).unwrap());
    body.move_to(4.0, 5.5);
    body.line_to(8.5, 1.8);
    body.line_to(8.5, 14.2);
    body.line_to(4.0, 10.5);
    body.close();
    fill(px, body, paint, t);

    let mut waves = PathBuilder::new();
    arc(&mut waves, 8.8, 8.0, 2.8, -50.0, 50.0);
    arc(&mut waves, 8.8, 8.0, 5.6, -55.0, 55.0);
    stroke(px, waves, paint, 1.5, t);
}

fn headset(px: &mut Pixmap, paint: &Paint, t: Transform) {
    let mut band = PathBuilder::new();
    arc(&mut band, 8.0, 9.0, 6.0, 180.0, 360.0);
    stroke(px, band, paint, 1.7, t);

    let mut cups = PathBuilder::new();
    cups.push_rect(Rect::from_ltrb(1.0, 8.5, 5.0, 15.0).unwrap());
    cups.push_rect(Rect::from_ltrb(11.0, 8.5, 15.0, 15.0).unwrap());
    fill(px, cups, paint, t);
}

/// Render the tray icon as straight (non-premultiplied) RGBA.
pub fn render(status: Status, light_taskbar: bool, size: u32) -> tray_icon::Icon {
    let px = draw(status, light_taskbar, size);
    let rgba: Vec<u8> = px
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    tray_icon::Icon::from_rgba(rgba, size, size).expect("valid icon")
}

/// Debug helper: `audio-output-device-switcher --dump-icons <dir>` writes every glyph as PNG.
pub fn dump(dir: &str) {
    for (st, n) in [(Status::Speakers, "speakers"), (Status::Headset, "headset"), (Status::Mixed, "mixed"), (Status::Unknown, "unknown")] {
        for light in [false, true] {
            let p = draw(st, light, 64);
            let _ = p.save_png(format!("{dir}/{n}-{}.png", if light { "light" } else { "dark" }));
        }
    }
}

fn draw(status: Status, light_taskbar: bool, size: u32) -> Pixmap {
    let mut px = Pixmap::new(size, size).expect("icon size");
    let (r, g, b) = if light_taskbar { (25, 25, 25) } else { (255, 255, 255) };
    let alpha = if status == Status::Unknown { 110 } else { 255 };
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(r, g, b, alpha));
    paint.anti_alias = true;

    let s = size as f32 / 16.0;
    let full = Transform::from_scale(s, s);
    match status {
        Status::Speakers | Status::Unknown => speaker(&mut px, &paint, full),
        Status::Headset => headset(&mut px, &paint, full),
        Status::Mixed => {
            // Small speaker top-left, small headset bottom-right.
            let k = 0.58;
            speaker(&mut px, &paint, Transform::from_scale(s * k, s * k));
            headset(&mut px, &paint, Transform::from_scale(s * k, s * k).post_translate(s * 6.7, s * 6.7));
        }
    }
    px
}
