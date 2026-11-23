//! A frost of the desktop behind the mini player window.
//!
//! Its ground is what the screen looks like under the window, blurred: the pixels are
//! copied out of the screen by `crate::grab` — which keeps this window out of the copy —
//! then squeezed to a small sheet and smoothed, and the sheet is stretched over the window.
//! A bright window behind the card's left edge comes through its left edge; move the card
//! and the frost follows it.
//!
//! Where there is nothing to read (no window handle yet, another platform, a driver that
//! refuses the copy) there is no sheet and the caller paints its own ground.

use std::time::{Duration, Instant};

use egui::{Context, TextureHandle, TextureOptions};

/// The frost's own footprint, in pixels across its longer side. Shrinking the grab this
/// far is most of the blur and keeps the cost off the window's size; stretching it back
/// with linear filtering is the rest.
const SHEET: usize = 64;

/// How often the frost is re-read while the card sits still: a window dragged behind it
/// shows up within this.
const EVERY: Duration = Duration::from_millis(200);

/// Reads stop after this many failures in a row (a lock screen, a driver that won't
/// answer), until the next successful grab.
const MISSES: u32 = 12;

pub struct Backdrop {
    /// The sheet, and the screen rectangle it was read from (physical pixels).
    tex: Option<TextureHandle>,
    at: Option<(i32, i32, u32, u32)>,
    read: Option<Instant>,
    misses: u32,
}

impl Default for Backdrop {
    fn default() -> Self {
        Self { tex: None, at: None, read: None, misses: 0 }
    }
}

impl Backdrop {
    /// The window's frosted ground, re-read when the window has moved or the sheet has
    /// aged. None while there is nothing to read.
    pub fn frame(&mut self, ctx: &Context) -> Option<TextureHandle> {
        let Some((x, y, w, h)) = crate::grab::client_rect() else {
            note("no window to read behind");
            return None;
        };
        let moved = self.at != Some((x, y, w, h));
        if !moved && (self.misses > MISSES || self.read.is_some_and(|t| t.elapsed() < EVERY)) {
            return self.tex.clone();
        }
        self.read = Some(Instant::now());
        match crate::grab::grab(x, y, w, h) {
            Some((px, gw, gh)) => {
                if let Some(path) = super::dev::dev_var("YTM_GRAB_DUMP") {
                    note(&format!("grabbed {gw}x{gh} at ({x},{y}) on_screen={}", crate::grab::on_screen(x, y, gw, gh)));
                    // A copy of an off-desktop window is black by definition; only a
                    // rectangle that is actually on a screen can say anything about the frost.
                    if crate::grab::on_screen(x, y, gw, gh) {
                        dump(path, &px, gw, gh);
                    }
                }
                match sheet(&px, gw, gh) {
                    Some((pixels, size)) => {
                        let img = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
                        match &mut self.tex {
                            Some(tex) => tex.set(img, TextureOptions::LINEAR),
                            None => self.tex = Some(ctx.load_texture("mini-backdrop", img, TextureOptions::LINEAR)),
                        }
                        self.misses = 0;
                    }
                    None => {
                        note("blur refused the pixels");
                        self.misses += 1;
                    }
                }
            }
            None => {
                note(&format!("grab failed at ({x},{y}) {w}x{h}"));
                self.misses += 1;
            }
        }
        self.at = Some((x, y, w, h));
        // The frost is live: it asks for the next frame whether or not anything else does.
        ctx.request_repaint_after(EVERY);
        self.tex.clone()
    }

    /// The card is gone: forget where the sheet came from, so the next one is read fresh.
    pub fn forget(&mut self) {
        self.at = None;
        self.read = None;
        self.misses = 0;
    }
}

/// The diagnostic's one-shot guards: `YTM_GRAB_DUMP=<path>` says once what the copy read,
/// and the first refusal is the one worth hearing.
static DUMPED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static NOTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn note(msg: &str) {
    if !NOTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        log::info!("backdrop: {msg}");
    }
}

/// The raw copy, straight off the screen, kept as a PNG for the eye to read.
fn dump(path: &str, px: &[u8], w: u32, h: u32) {
    if DUMPED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    // GDI leaves the alpha byte at zero, which would save as an invisible image; the copy
    // is opaque, and the spread of its luma is the answer to "is there anything there?".
    let mut rgba = px.to_vec();
    let (mut lo, mut hi) = (255u32, 0u32);
    for c in rgba.chunks_exact_mut(4) {
        c.swap(0, 2);
        c[3] = 255;
        let l = (c[0] as u32 + c[1] as u32 + c[2] as u32) / 3;
        lo = lo.min(l);
        hi = hi.max(l);
    }
    if let Some(img) = image::RgbaImage::from_raw(w, h, rgba) {
        log::info!("backdrop: grab dumped {} ({w}x{h}) luma {lo}..{hi}", img.save(path).is_ok());
    }
}

/// The grab averaged down to `SHEET` across, then smoothed. RGBA bytes and its size.
///
/// One pass of box averaging over `f` × `f` source pixels does the heavy blurring; three
/// [1,2,1] passes take the blockiness out of it, which together reads as frosted glass.
fn sheet(px: &[u8], w: u32, h: u32) -> Option<(Vec<u8>, [usize; 2])> {
    let (w, h) = (w as usize, h as usize);
    if w == 0 || h == 0 || px.len() < w * h * 4 {
        return None;
    }
    let f = (w.max(h).div_ceil(SHEET)).max(1);
    let (ow, oh) = ((w / f).max(1), (h / f).max(1));
    let mut out = vec![0u16; ow * oh * 3];
    for j in 0..oh {
        for i in 0..ow {
            let (x0, y0) = (i * f, j * f);
            let (x1, y1) = ((x0 + f).min(w), (y0 + f).min(h));
            let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = (y * w + x) * 4;
                    b += px[p] as u32;
                    g += px[p + 1] as u32;
                    r += px[p + 2] as u32;
                    n += 1;
                }
            }
            let o = (j * ow + i) * 3;
            out[o] = (r / n) as u16;
            out[o + 1] = (g / n) as u16;
            out[o + 2] = (b / n) as u16;
        }
    }
    for _ in 0..3 {
        smooth(&mut out, ow, oh);
    }
    let mut rgba = Vec::with_capacity(ow * oh * 4);
    for c in out.chunks_exact(3) {
        rgba.extend_from_slice(&[c[0] as u8, c[1] as u8, c[2] as u8, 255]);
    }
    Some((rgba, [ow, oh]))
}

/// One [1,2,1] pass across the rows, then down the columns, clamped at the edges.
fn smooth(px: &mut [u16], w: usize, h: usize) {
    let at = |p: &[u16], x: usize, y: usize, c: usize| p[(y * w + x) * 3 + c] as u32;
    let mut t = vec![0u16; px.len()];
    let src = &*px;
    for y in 0..h {
        for x in 0..w {
            for c in 0..3 {
                let v = at(src, x.saturating_sub(1), y, c) + 2 * at(src, x, y, c) + at(src, (x + 1).min(w - 1), y, c);
                t[(y * w + x) * 3 + c] = (v / 4) as u16;
            }
        }
    }
    for y in 0..h {
        for x in 0..w {
            for c in 0..3 {
                let v = at(&t, x, y.saturating_sub(1), c) + 2 * at(&t, x, y, c) + at(&t, x, (y + 1).min(h - 1), c);
                px[(y * w + x) * 3 + c] = (v / 4) as u16;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{sheet, smooth};

    #[test]
    fn a_flat_grab_stays_flat() {
        let (w, h) = (400u32, 300u32);
        let px = vec![0x40u8; (w * h * 4) as usize];
        let (rgba, size) = sheet(&px, w, h).unwrap();
        assert_eq!(size, [w as usize / 7, h as usize / 7]);
        assert_eq!(rgba.len(), size[0] * size[1] * 4);
        assert!(rgba.chunks_exact(4).all(|c| c[0] == 0x40 && c[3] == 255));
    }

    #[test]
    fn the_frost_keeps_the_light_on_its_own_side() {
        // Half the screen black, half white, at the smallest sheet the code will make.
        let (w, h) = (64u32, 8u32);
        let mut px = vec![0u8; (w * h * 4) as usize];
        for y in 0..h as usize {
            for x in 32..w as usize {
                let p = (y * w as usize + x) * 4;
                px[p..p + 3].copy_from_slice(&[255, 255, 255]);
            }
        }
        let (rgba, size) = sheet(&px, w, h).unwrap();
        let lum = |i: usize| rgba[i * 4] as usize;
        assert_eq!(size, [64, 8]);
        assert!(lum(0) < 8, "the dark half stays dark: {}", lum(0));
        assert!(lum(size[0] - 1) > 240, "the bright half stays bright: {}", lum(size[0] - 1));
        // And the seam between them is a run, not a step.
        let mid = size[0] / 2;
        assert!(lum(mid - 1) > 40 && lum(mid) < 215, "{}) {} vs {}", mid - 1, lum(mid - 1), lum(mid));
    }

    #[test]
    fn smoothing_spreads_a_single_bright_dot() {
        let (w, h) = (5, 5);
        let mut px = vec![0u16; w * h * 3];
        px[(2 * w + 2) * 3 + 1] = 400;
        smooth(&mut px, w, h);
        assert!(px[((2 * w + 1) * 3) + 1] > 0, "its neighbour took some of it");
        assert!(px[((2 * w + 2) * 3) + 1] < 400, "and the dot itself came down");
    }
}
