//! Bildoperationen auf reinen Byte-Puffern. Unabhängig von Windows, daher unter Linux voll testbar.

use std::collections::HashMap;

pub const FLAT_TOP_SHARE: f64 = 0.92;
pub const FLAT_MAX_DISTINCT: usize = 50;

/// BGRA8 mit premultiplied Alpha -> RGBA8 unpremultiplied. `alpha == 0` ergibt zwingend R=G=B=0.
pub fn unpremultiply_bgra(bgra: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bgra.len());
    for px in bgra.chunks_exact(4) {
        let (b, g, r, a) = (px[0] as u32, px[1] as u32, px[2] as u32, px[3] as u32);
        if a == 0 {
            out.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        let unpremul = |c: u32| -> u8 { ((c * 255 + a / 2) / a).min(255) as u8 };
        out.extend_from_slice(&[unpremul(r), unpremul(g), unpremul(b), a as u8]);
    }
    out
}

/// Blendet jedes Pixel auf eine opake Hintergrundfarbe. Ergebnis hat überall Alpha 255.
pub fn flatten_over(rgba: &[u8], bg: [u8; 3]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len());
    for px in rgba.chunks_exact(4) {
        let a = px[3] as u32;
        for channel in 0..3 {
            let c = px[channel] as u32;
            let b = bg[channel] as u32;
            out.push(((c * a + b * (255 - a) + 127) / 255) as u8);
        }
        out.push(255);
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct Metrics {
    pub visible_pixels: usize,
    pub distinct_colors: usize,
    pub top_color_share: f64,
}

/// Farbmetriken ausschließlich über Pixel mit `alpha > 0`. Transparente Ecken zählen nicht.
pub fn metrics(rgba: &[u8]) -> Metrics {
    let mut counts: HashMap<[u8; 3], usize> = HashMap::new();
    let mut visible = 0usize;
    for px in rgba.chunks_exact(4) {
        if px[3] == 0 {
            continue;
        }
        visible += 1;
        *counts.entry([px[0], px[1], px[2]]).or_insert(0) += 1;
    }
    let top = counts.values().copied().max().unwrap_or(0);
    Metrics {
        visible_pixels: visible,
        distinct_colors: counts.len(),
        top_color_share: if visible == 0 { 0.0 } else { top as f64 / visible as f64 },
    }
}

/// Flach = kein Inhalt sichtbar, oder fast nur eine Farbe mit wenig Farbvielfalt (z. B. schwarzer Frame).
pub fn is_flat(m: &Metrics) -> bool {
    m.visible_pixels == 0 || (m.top_color_share >= FLAT_TOP_SHARE && m.distinct_colors <= FLAT_MAX_DISTINCT)
}

pub fn crop(rgba: &[u8], width: usize, height: usize, x: usize, y: usize, cw: usize, ch: usize) -> Result<Vec<u8>, String> {
    if rgba.len() != width * height * 4 {
        return Err("Pufferlänge passt nicht zur Bildgröße.".into());
    }
    if cw == 0 || ch == 0 || x + cw > width || y + ch > height {
        return Err(format!("Zuschnitt {x},{y} {cw}x{ch} liegt außerhalb des {width}x{height}-Bildes."));
    }
    let mut out = Vec::with_capacity(cw * ch * 4);
    for row in y..y + ch {
        let start = (row * width + x) * 4;
        out.extend_from_slice(&rgba[start..start + cw * 4]);
    }
    Ok(out)
}

/// Setzt mehrere RGBA-Teilbilder auf eine transparente Leinwand. Spätere Teile überschreiben frühere,
/// transparente Pixel des Teilbildes werden übersprungen.
pub fn compose(canvas_w: usize, canvas_h: usize, parts: &[(usize, usize, usize, usize, &[u8])]) -> Result<Vec<u8>, String> {
    let mut canvas = vec![0u8; canvas_w * canvas_h * 4];
    for &(dx, dy, w, h, pixels) in parts {
        if pixels.len() != w * h * 4 {
            return Err("Teilbild hat eine unpassende Pufferlänge.".into());
        }
        if dx + w > canvas_w || dy + h > canvas_h {
            return Err("Teilbild liegt außerhalb der Leinwand.".into());
        }
        for row in 0..h {
            for col in 0..w {
                let src = (row * w + col) * 4;
                if pixels[src + 3] == 0 {
                    continue;
                }
                let dst = ((dy + row) * canvas_w + dx + col) * 4;
                canvas[dst..dst + 4].copy_from_slice(&pixels[src..src + 4]);
            }
        }
    }
    Ok(canvas)
}

pub fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    if rgba.len() != width as usize * height as usize * 4 {
        return Err("PNG-Puffer passt nicht zur Bildgröße.".into());
    }
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| format!("PNG-Kopf fehlgeschlagen: {e}"))?;
        writer.write_image_data(rgba).map_err(|e| format!("PNG-Schreiben fehlgeschlagen: {e}"))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transparent_pixels_become_black_without_division() {
        let out = unpremultiply_bgra(&[200, 100, 50, 0]);
        assert_eq!(out, vec![0, 0, 0, 0]);
    }

    #[test]
    fn premultiplied_color_is_restored() {
        // BGRA: B=64, G=32, R=128, A=128 entspricht unpremultiplied R=255, G=64, B=128.
        let out = unpremultiply_bgra(&[64, 32, 128, 128]);
        assert_eq!(out, vec![255, 64, 128, 128]);
    }

    #[test]
    fn opaque_pixels_are_passed_through_with_channel_order_fixed() {
        assert_eq!(unpremultiply_bgra(&[10, 20, 30, 255]), vec![30, 20, 10, 255]);
    }

    #[test]
    fn flatten_blends_transparent_to_background() {
        let out = flatten_over(&[0, 0, 0, 0, 255, 255, 255, 255], [16, 32, 48]);
        assert_eq!(out, vec![16, 32, 48, 255, 255, 255, 255, 255]);
    }

    #[test]
    fn metrics_ignore_transparent_pixels_completely() {
        // Zwei sichtbare Pixel derselben Farbe, zwei transparente schwarze: Anteil 1.0, distinct 1.
        let rgba = [9, 9, 9, 255, 9, 9, 9, 255, 0, 0, 0, 0, 0, 0, 0, 0];
        let m = metrics(&rgba);
        assert_eq!(m.visible_pixels, 2);
        assert_eq!(m.distinct_colors, 1);
        assert!((m.top_color_share - 1.0).abs() < 1e-9);
        assert!(is_flat(&m));
    }

    #[test]
    fn fully_transparent_image_counts_as_flat() {
        let m = metrics(&[0, 0, 0, 0]);
        assert_eq!(m.visible_pixels, 0);
        assert!(is_flat(&m));
    }

    #[test]
    fn varied_image_is_not_flat() {
        let mut rgba = Vec::new();
        for i in 0..200u32 {
            rgba.extend_from_slice(&[(i % 256) as u8, (i * 7 % 256) as u8, (i * 13 % 256) as u8, 255]);
        }
        let m = metrics(&rgba);
        assert!(m.distinct_colors > FLAT_MAX_DISTINCT);
        assert!(!is_flat(&m));
    }

    #[test]
    fn dominant_single_color_with_few_colors_is_flat() {
        let mut rgba = Vec::new();
        for _ in 0..95 {
            rgba.extend_from_slice(&[30, 30, 30, 255]);
        }
        for i in 0..5u8 {
            rgba.extend_from_slice(&[i, i, i, 255]);
        }
        let m = metrics(&rgba);
        assert!(m.top_color_share >= FLAT_TOP_SHARE);
        assert!(is_flat(&m));
    }

    #[test]
    fn crop_extracts_the_requested_window_and_rejects_out_of_bounds() {
        // 3x2 Bild mit Pixelindex in Rot-Kanal.
        let mut rgba = Vec::new();
        for i in 0..6u8 {
            rgba.extend_from_slice(&[i, 0, 0, 255]);
        }
        let cropped = crop(&rgba, 3, 2, 1, 0, 2, 2).unwrap();
        assert_eq!(cropped.chunks_exact(4).map(|p| p[0]).collect::<Vec<_>>(), vec![1, 2, 4, 5]);
        assert!(crop(&rgba, 3, 2, 2, 0, 2, 1).is_err());
        assert!(crop(&rgba, 3, 2, 0, 0, 0, 1).is_err());
    }

    #[test]
    fn compose_places_parts_and_keeps_gaps_transparent() {
        let red = [255, 0, 0, 255];
        let blue = [0, 0, 255, 255];
        let canvas = compose(3, 1, &[(0, 0, 1, 1, &red), (2, 0, 1, 1, &blue)]).unwrap();
        assert_eq!(&canvas[0..4], &red);
        assert_eq!(&canvas[4..8], &[0, 0, 0, 0], "Lücke bleibt transparent");
        assert_eq!(&canvas[8..12], &blue);
        assert!(compose(1, 1, &[(1, 0, 1, 1, &red)]).is_err());
    }

    #[test]
    fn png_output_has_signature_and_matches_dimensions() {
        let rgba = vec![10u8; 2 * 2 * 4];
        let png_bytes = encode_png(&rgba, 2, 2).unwrap();
        assert_eq!(&png_bytes[0..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        assert!(encode_png(&rgba, 3, 2).is_err());
    }
}
