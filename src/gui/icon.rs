//! The app icon, drawn in code: a rounded square with a microphone. The window and tray draw it
//! at runtime; `assets/voicechanger.ico` (embedded in the .exe, used by the installer) is generated
//! from the same code and a test keeps the two in sync.

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

/// Sizes in the .ico: small icons, Start menu and taskbar at 100–250 % scaling, Explorer views.
#[cfg(test)]
const ICO_SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];

/// A Windows .ico with every size in `ICO_SIZES` (32-bit BGRA bitmaps with alpha).
#[cfg(test)]
fn ico() -> Vec<u8> {
    let images: Vec<Vec<u8>> = ICO_SIZES.iter().map(|&n| ico_bitmap(n)).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]); // reserved, type 1 = icon
    out.extend_from_slice(&(ICO_SIZES.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * ICO_SIZES.len();
    for (&n, img) in ICO_SIZES.iter().zip(&images) {
        let dim = if n >= 256 { 0 } else { n as u8 }; // 0 means 256
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(img.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += img.len();
    }
    images.iter().for_each(|img| out.extend_from_slice(img));
    out
}

/// One .ico entry: BITMAPINFOHEADER, bottom-up BGRA rows, then an all-zero AND mask.
#[cfg(test)]
fn ico_bitmap(n: u32) -> Vec<u8> {
    let px = rgba(n);
    let mask_row = n.div_ceil(32) * 4;
    let mut out = Vec::new();
    for v in [40, n, 2 * n] {
        out.extend_from_slice(&v.to_le_bytes()); // header size, width, height (image + mask)
    }
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]); // no compression, default sizes and palette
    for row in px.chunks_exact(n as usize * 4).rev() {
        for p in row.as_chunks::<4>().0 {
            out.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
    }
    out.resize(out.len() + (mask_row * n) as usize, 0);
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

    /// The .ico shipped in `assets/` matches the drawing. After changing the icon, regenerate it
    /// with `VC_UPDATE_GOLDEN=1 cargo test --bin voicechanger icon`.
    #[test]
    fn ico_file_is_up_to_date() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/voicechanger.ico");
        let ico = super::ico();
        if std::env::var_os("VC_UPDATE_GOLDEN").is_some() {
            std::fs::write(path, &ico).unwrap();
        }
        assert_eq!(&ico[..6], &[0, 0, 1, 0, 8, 0]);
        let shipped = std::fs::read(path).expect("assets/voicechanger.ico is missing");
        assert!(shipped == ico, "assets/voicechanger.ico is out of date with icon.rs (see the test's docs)");
    }
}
