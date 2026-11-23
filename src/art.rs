//! Album-art processing for the animated background and accent colours.
//!
//! Follows Kawarp's pipeline (MIT, Better Lyrics, used by better-lyrics-shaders):
//! tint the dark areas, then blur heavily. Kawarp does this on the GPU every frame;
//! here it runs once per track on a 64×64 image, so the per-frame shader is cheap.

use egui::Color32;

pub const SIZE: u32 = 64;

#[derive(Clone)]
pub struct Art {
    /// SIZE×SIZE RGBA, already tinted and blurred.
    pub rgba: Vec<u8>,
    /// Most vivid colour, lifted for use on dark backgrounds (progress bar, highlights).
    pub accent: Color32,
}

pub fn process(bytes: &[u8]) -> anyhow::Result<Art> {
    let img = image::load_from_memory(bytes)?.to_rgb8();
    let small = image::imageops::resize(&img, SIZE, SIZE, image::imageops::FilterType::Triangle);

    let (accent, tint, share) = palette(&small);
    let mut px: Vec<[f32; 3]> = small.pixels().map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect();

    // Kawarp's tint pass: pull near-black regions toward the cover's own colour so dark
    // sleeves still produce a coloured backdrop instead of a black one. How far it pulls
    // follows how much of the sleeve actually carries that colour — a black cover with a
    // red logo stays black, rather than washing the whole field red.
    let tint = [tint[0], tint[1], tint[2]];
    let strength = 0.45 * (share / 0.15).clamp(0.05, 1.0);
    for p in &mut px {
        let luma = (0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2]) / 255.0;
        let dark = 1.0 - smoothstep(0.0, 0.5, luma);
        for c in 0..3 {
            p[c] += (tint[c] - p[c]) * dark * strength;
        }
    }
    // Three box-blur passes approximate a wide Gaussian (Kawarp runs 8 Kawase passes).
    for _ in 0..3 {
        px = box_blur(&px, SIZE as usize, 6);
    }
    let rgba = px.iter().flat_map(|p| [p[0] as u8, p[1] as u8, p[2] as u8, 255]).collect();
    Ok(Art { rgba, accent })
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn box_blur(src: &[[f32; 3]], n: usize, r: usize) -> Vec<[f32; 3]> {
    let pass = |src: &[[f32; 3]], horizontal: bool| -> Vec<[f32; 3]> {
        let mut out = vec![[0.0; 3]; src.len()];
        for y in 0..n {
            for x in 0..n {
                let mut acc = [0.0; 3];
                for k in 0..=2 * r {
                    let o = k as isize - r as isize;
                    let (sx, sy) = if horizontal {
                        ((x as isize + o).clamp(0, n as isize - 1) as usize, y)
                    } else {
                        (x, (y as isize + o).clamp(0, n as isize - 1) as usize)
                    };
                    let p = src[sy * n + sx];
                    for c in 0..3 {
                        acc[c] += p[c];
                    }
                }
                let d = (2 * r + 1) as f32;
                out[y * n + x] = [acc[0] / d, acc[1] / d, acc[2] / d];
            }
        }
        out
    };
    pass(&pass(src, true), false)
}

/// The cover's colour: the hue its pixels actually cluster in, not its single most vivid
/// point. Returns the accent for the UI, the same hue at the brightness the sleeve really
/// has for the backdrop's tint, and the share of the sleeve that hue holds.
fn palette(img: &image::RgbImage) -> (Color32, [f32; 3], f32) {
    const B: usize = 8;
    let mut bins = [[0.0f32; 4]; B * B * B];
    let mut sum = [0.0f32; 3];
    let mut n = 0.0f32;
    let mut lit = 0.0f32;
    for p in img.pixels() {
        let rgb = [p[0] as f32, p[1] as f32, p[2] as f32];
        for c in 0..3 {
            sum[c] += rgb[c];
        }
        n += 1.0;
        let (max, min) = (rgb[0].max(rgb[1]).max(rgb[2]), rgb[0].min(rgb[1]).min(rgb[2]));
        // Near-black and grey pixels carry no hue worth following.
        if max < 46.0 || max - min < 28.0 {
            continue;
        }
        lit += 1.0;
        let i = (rgb[0] as usize * B / 256) * (B * B) + (rgb[1] as usize * B / 256) * B + (rgb[2] as usize * B / 256);
        let b = &mut bins[i];
        b[0] += rgb[0];
        b[1] += rgb[1];
        b[2] += rgb[2];
        b[3] += 1.0;
    }
    let avg = [sum[0] / n, sum[1] / n, sum[2] / n];
    let share = lit / n;
    let Some(best) = bins
        .iter()
        .filter(|b| b[3] > 0.0)
        .max_by(|a, b| score(a).partial_cmp(&score(b)).unwrap_or(std::cmp::Ordering::Equal))
    else {
        // Nothing coloured anywhere: a black-and-white sleeve keeps its own grey, so the
        // backdrop stays monochrome instead of being tinted a colour it never had.
        let g = ((avg[0] + avg[1] + avg[2]) / 3.0).clamp(28.0, 200.0);
        return (Color32::from_rgb(g as u8, g as u8, g as u8), avg, 0.0);
    };
    let mean = [best[0] / best[3], best[1] / best[3], best[2] / best[3]];
    // A hue only a sliver of the sleeve wears is a logo or a corner of the artwork, not
    // the cover's colour: the mark may take it, the field drifts back to the average.
    let strong = (share / 0.12).clamp(0.0, 1.0);
    let tint = [
        mean[0] * strong + avg[0] * (1.0 - strong),
        mean[1] * strong + avg[1] * (1.0 - strong),
        mean[2] * strong + avg[2] * (1.0 - strong),
    ];
    (lift(mean), tint, share)
}

/// What a colour bin is worth: how many pixels stand in it, and how coloured they are.
fn score(b: &[f32; 4]) -> f32 {
    let (r, g, bl) = (b[0] / b[3], b[1] / b[3], b[2] / b[3]);
    let (max, min) = (r.max(g).max(bl), r.min(g).min(bl));
    let sat = if max > 0.0 { (max - min) / max } else { 0.0 };
    b[3] * (0.35 + sat)
}

/// Raise brightness so the colour reads on a dark UI.
fn lift(c: [f32; 3]) -> Color32 {
    let max = c[0].max(c[1]).max(c[2]).max(1.0);
    let k = (220.0 / max).max(1.0);
    Color32::from_rgb((c[0] * k).min(255.0) as u8, (c[1] * k).min(255.0) as u8, (c[2] * k).min(255.0) as u8)
}

/// A neutral backdrop for when nothing is playing.
pub fn idle() -> Art {
    let n = SIZE as usize;
    let mut rgba = Vec::with_capacity(n * n * 4);
    for y in 0..n {
        for x in 0..n {
            let (fx, fy) = (x as f32 / n as f32, y as f32 / n as f32);
            let r = 40.0 + 70.0 * (1.0 - fy) * fx;
            let g = 18.0 + 10.0 * fy;
            let b = 46.0 + 60.0 * fy * (1.0 - fx);
            rgba.extend_from_slice(&[r as u8, g as u8, b as u8, 255]);
        }
    }
    Art { rgba, accent: Color32::from_rgb(0xff, 0x4e, 0x6a) }
}
