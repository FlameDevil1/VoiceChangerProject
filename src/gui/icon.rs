//! The app icon, drawn in code (no image files to ship): a rounded square with a microphone.

/// RGBA pixels of a `size` x `size` icon.
pub fn rgba(size: u32) -> Vec<u8> {
    let s = size as f32;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            // Sample at the pixel centre, in units of the icon size (0..1).
            let (px, py) = ((x as f32 + 0.5) / s, (y as f32 + 0.5) / s);
            let aa = 1.0 / s;
            // Background: rounded square, purple-to-blue gradient.
            let bg = coverage(rounded_rect(px, py, 0.5, 0.5, 0.46, 0.46, 0.18), aa);
            let t = py;
            let (r, g, b) = (lerp(124.0, 70.0, t), lerp(92.0, 110.0, t), lerp(240.0, 230.0, t));
            // Microphone: capsule head, U-shaped holder, stem and base.
            let head = rounded_rect(px, py, 0.5, 0.38, 0.11, 0.19, 0.11);
            // Lower half of a ring around the head.
            let holder = if py >= 0.42 { circle(px, py, 0.5, 0.42, 0.21).abs() - 0.035 } else { f32::MAX };
            let stem = rounded_rect(px, py, 0.5, 0.71, 0.03, 0.08, 0.0);
            let base = rounded_rect(px, py, 0.5, 0.79, 0.13, 0.03, 0.03);
            let mic = coverage(head.min(holder).min(stem).min(base), aa);
            let (r, g, b) = (lerp(r, 255.0, mic), lerp(g, 255.0, mic), lerp(b, 255.0, mic));
            out.extend_from_slice(&[r as u8, g as u8, b as u8, (bg * 255.0) as u8]);
        }
    }
    out
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

/// Signed distance to a rounded rectangle centred at (cx, cy) with half-size (hw, hh).
fn rounded_rect(x: f32, y: f32, cx: f32, cy: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let qx = (x - cx).abs() - (hw - r);
    let qy = (y - cy).abs() - (hh - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - r
}

fn circle(x: f32, y: f32, cx: f32, cy: f32, r: f32) -> f32 {
    ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - r
}

/// Anti-aliased coverage from a signed distance.
fn coverage(d: f32, aa: f32) -> f32 {
    (0.5 - d / aa).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn icon_has_opaque_middle_and_transparent_corner() {
        let px = super::rgba(32);
        assert_eq!(px.len(), 32 * 32 * 4);
        assert_eq!(px[3], 0, "corner is transparent");
        let mid = ((16 * 32 + 16) * 4) as usize;
        assert_eq!(px[mid + 3], 255, "centre is opaque");
    }
}
