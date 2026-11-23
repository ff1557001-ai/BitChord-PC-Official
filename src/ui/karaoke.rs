//! Synced lyrics: the word motion, wrapping and scroll ported from binimum/am-lyrics
//! v1.9 (Mozilla Public License 2.0) and the clock offsets from Better Lyrics' renderer
//! (`@braccato/core` 1.16: engine.js, inject.js, instrumental.js, lyrics.css, variables.css),
//! to egui.
//!
//! What is the same as the extension, by construction:
//! - Timing: the clock is shown 150 ms ahead for word-synced lyrics and 115 ms for
//!   line-synced ones, and the view scrolls ahead of that by the gap to the next line,
//!   between 350 and 500 ms.
//! - Words: every provider part is split on whitespace with its time pro-rated by
//!   characters; words between spaces form groups that never wrap unless 16 characters
//!   or more.
//! - Karaoke: each word has a lit copy over the dim one. The lit copy is revealed by the
//!   am-lyrics wipe: a linear gradient whose feather is 0.75 em (a little wider for a long
//!   word, 0.3 em for a script that can't split) and whose edge runs straight across
//!   `width + feather` over the word's duration. Where two words run together (under 180 ms
//!   apart) the next word's feather creeps in before its timestamp, so the highlight hands
//!   off without a gap.
//! - Letters: a word rises whole on the extension's 2 s spring — `.normal-word-motion` gives
//!   every glyph of it the same lift, so nothing on screen moves letter by letter. Only the
//!   scripts the extension marks `char-rise` (Han, kana, Hangul) are left out of that class
//!   and rise glyph by glyph, fanning and swelling while they're held.
//! - Glow: `text-shadow 0 0 0.3em` on the lit glyphs, breathing 0 → 0.8 → 1 → 0.3 → 0 across
//!   each glyph's animation and only on held words, never on CJK: a Gaussian blur of the lit
//!   glyph pixels (never a box).
//! - Lines: scale 0.98 → 1 when they start, back when they end, and no blur on the line about
//!   to be sung (`.pre-active`); lit words fade back to dim over 0.5 s once the next line starts.
//! - Scrolling: the view jumps to the new position and every line eases from where it was
//!   in 400 ms on `cubic-bezier(0.2, 0.8, 0.2, 1)`, a line further from the sung one
//!   waiting 50 ms for each step, four at most.

use std::collections::HashMap;
use std::sync::Arc;

use egui::epaint::Vertex;
use egui::epaint::text::FontsView;
use egui::layers::ShapeIdx;
use egui::{Color32, ColorImage, FontId, Galley, Mesh, Pos2, Rect, Shape, TextureHandle, TextureId, TextureOptions, Vec2, pos2, vec2};

use crate::lyrics::{Line, Lyrics, Part, Sync};
use crate::preset::{BlurScope, Fill, LyricStyle, ThemeCss, Weight};
use crate::theme;

// ------------------------------------------------------------------ easing

/// A CSS `cubic-bezier()` timing function.
#[derive(Clone, Copy, Debug)]
pub struct Bezier(pub f32, pub f32, pub f32, pub f32);

impl Bezier {
    pub fn at(self, x: f32) -> f32 {
        if x <= 0.0 {
            return 0.0;
        }
        if x >= 1.0 {
            return 1.0;
        }
        let Bezier(x1, y1, x2, y2) = self;
        let curve = |t: f32, a: f32, b: f32| {
            let u = 1.0 - t;
            3.0 * u * u * t * a + 3.0 * u * t * t * b + t * t * t
        };
        let slope = |t: f32, a: f32, b: f32| {
            let u = 1.0 - t;
            3.0 * u * u * a + 6.0 * u * t * (b - a) + 3.0 * t * t * (1.0 - b)
        };
        let mut t = x;
        for _ in 0..8 {
            let e = curve(t, x1, x2) - x;
            if e.abs() < 1e-6 {
                return curve(t, y1, y2);
            }
            let d = slope(t, x1, x2);
            if d.abs() < 1e-6 {
                break;
            }
            t = (t - e / d).clamp(0.0, 1.0);
        }
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        t = x;
        for _ in 0..40 {
            let v = curve(t, x1, x2);
            if (v - x).abs() < 1e-6 {
                break;
            }
            if v < x {
                lo = t;
            } else {
                hi = t;
            }
            t = (lo + hi) * 0.5;
        }
        curve(t, y1, y2)
    }
}

pub const EASE: Bezier = Bezier(0.25, 0.1, 0.25, 1.0);
pub const EASE_IN: Bezier = Bezier(0.42, 0.0, 1.0, 1.0);
pub const EASE_IN_OUT: Bezier = Bezier(0.42, 0.0, 0.58, 1.0);
/// `.main-vocal-container`: the curve the view moves along while it scrolls.
pub const SCROLL_EASE: Bezier = Bezier(0.2, 0.8, 0.2, 1.0);
/// `blyrics-decoration-in`.
const DECO_EASE: Bezier = Bezier(0.25, 1.0, 0.5, 1.0);

// ------------------------------------------------------------------ theme values

/// `--blyrics-richsync-timing-offset` / `--blyrics-timing-offset`.
pub const RICHSYNC_OFFSET: f64 = 0.150;
pub const LINE_OFFSET: f64 = 0.115;
/// `scrollLookAheadMs`: the view runs ahead of the singing by the gap between the line
/// being sung and the next one's start, held between these two.
const SCROLL_AHEAD_MIN: f64 = 0.35;
const SCROLL_AHEAD_MAX: f64 = 0.5;
/// `AmLyrics.ts`: `frequency = 10`, and `LyricMotion` damps at `0.9 * frequency`. The
/// frames run for `1.2 s` and are read linearly, so the view glides rather than snaps.
const SCROLL_SPRING: (f64, f64, f64) = (10.0, 0.9, 1.2);
/// `--scroll-duration`, and the cascade that lets each line further from the sung one
/// start a little later, up to four of them.
const SCROLL_DUR: f64 = SCROLL_SPRING.2;
const SCROLL_CASCADE: f64 = 0.05;
/// `blyrics-early-scroll-consider-s`.
const EARLY_SCROLL: f64 = 0.54;
/// `blyrics-lyric-ending-threshold-s`.
const ENDING_THRESHOLD: f64 = 0.5;
const TIME_JUMP: f64 = 0.5;
/// `BASE_WIPE_GRADIENT_EM`, `LONG_WORD_WIPE_EXTRA_EM/_RATIO` and `CJK_WIPE_GRADIENT_EM`:
/// the wipe's soft edge, in em of the word's own font.
const FEATHER_EM: f32 = 0.75;
const FEATHER_EXTRA_EM: f32 = 0.45;
const FEATHER_LONG_FROM: f32 = 10.0;
const FEATHER_CJK_EM: f32 = 0.3;
/// `LONG_WORD_WIPE_EXTRA_RATIO`: how much longer a long word's wipe runs.
const LONG_WIPE_RATIO: f64 = 0.35;
/// `NEXT_WORD_PRE_WIPE_MAX_GAP_MS` / `_HANDOFF_LEAD_MS` / `_MIN` / `_MAX`: how early the
/// next word's feather starts creeping in.
const PRE_WIPE_GAP: f64 = 0.180;
const PRE_WIPE_HANDOFF: f64 = 0.100;
const PRE_WIPE_MIN: f64 = 0.080;
const PRE_WIPE_MAX: f64 = 0.240;
/// `text-shadow: 0 0 0.3em` and the `--char-glow-max` ceiling.
const GLOW_EM: f32 = 0.3;
const GLOW_MAX: f64 = 0.45;
/// The glow's decay for the fills that don't wipe a word glyph by glyph.
const GLOW_TAIL: f64 = 1.2;
/// `--char-rise-y` is -3 px at the extension's 34 px: the fan's extra lift (2.25 px) and
/// its sideways spread, in em.
const FAN_LIFT_EM: f32 = 2.25 / 34.0;
const FAN_SPREAD_EM: f32 = 0.05;
/// `getCharacterFrames`: a glyph's motion is run for its word's response, no longer than
/// three seconds.
const LETTER_RESPONSE_MAX: f64 = 3.0;
const FADE_IN: f64 = 0.33;
const FADE_OUT: f64 = 0.5;
/// `.lyrics-line-container`: `transform 0.5 s ease` into the sung line and
/// `0.4 s cubic-bezier(.41,0,.12,.99)` back out of it.
const SCALE_IN: f64 = 0.5;
const SCALE_OUT: f64 = 0.4;
const SETTLE: Bezier = Bezier(0.41, 0.0, 0.12, 0.99);
/// `blyrics-line-synced-animation-delay`.
const LINE_SYNC_DELAY: f64 = 0.05;
/// Word groups of `LONG_GROUP_CHARS` characters or more may break (`.allow-break`).
const LONG_GROUP_CHARS: usize = 16;
/// `USER_SCROLL_RESUME_DELAY_MS`: how long reading the words by hand holds the view
/// before it goes back to the singing.
const USER_SCROLL_RESUME: f64 = 5.0;
const DECO_ENTRY: f64 = 0.35;

/// Colours for the lyric surface. Only the light sidebar needs a dark foreground;
/// the player always sits over artwork and the dark sidebar keeps its original look.
#[derive(Clone, Copy, Debug, PartialEq)]
struct LyricColors {
    foreground: Color32,
    active: Color32,
    glow: Color32,
    light_sidebar: bool,
}

impl LyricColors {
    fn new(surface: Surface, light: bool, text: Color32, background: Color32, active: Color32, glow: Color32) -> Self {
        let light_sidebar = surface == Surface::Sidebar && light;
        if !light_sidebar {
            return Self { foreground: Color32::WHITE, active, glow, light_sidebar };
        }
        // White is the preset default (including #ffffff). Keep other selected colours
        // when they contrast, and darken only those that would disappear on the panel.
        let active = if active == Color32::WHITE { text } else { contrast_color(active, text, background) };
        Self { foreground: text, active, glow, light_sidebar }
    }

    // Make faint lines legible on light surfaces without making intentionally hidden
    // lines visible, or changing their transition clocks and endpoints.
    fn dim_alpha(self, alpha: f32) -> f32 {
        if self.light_sidebar {
            let alpha = alpha.clamp(0.0, 1.0);
            alpha * (2.0 - alpha)
        } else {
            alpha
        }
    }
}

/// Darken a low-contrast selected colour towards the theme's text, retaining its tint.
fn contrast_color(color: Color32, text: Color32, background: Color32) -> Color32 {
    const MIN_CONTRAST: f32 = 4.5;
    if contrast_ratio(color, background) >= MIN_CONTRAST {
        return color;
    }
    // Once a colour needs correction, leave enough contrast to distinguish the lit
    // layer from the dim foreground as well as from the background.
    const ADJUSTED_CONTRAST: f32 = 6.0;
    let (mut low, mut high) = (0.0, 1.0);
    for _ in 0..12 {
        let mid = (low + high) * 0.5;
        if contrast_ratio(theme::lerp_color(color, text, mid), background) >= ADJUSTED_CONTRAST {
            high = mid;
        } else {
            low = mid;
        }
    }
    theme::lerp_color(color, text, high)
}

fn contrast_ratio(a: Color32, b: Color32) -> f32 {
    let luminance = |c: Color32| {
        let linear = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * linear(c.r()) + 0.7152 * linear(c.g()) + 0.0722 * linear(c.b())
    };
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// Resolved look for one frame: the extension's defaults, adjusted by the preset.
#[derive(Clone)]
struct Cfg {
    font: String,
    weight: Weight,
    size: f32,
    colors: LyricColors,
    /// Alpha of dim text on the line being sung.
    inactive_alpha: f32,
    scale: f32,
    active_scale: f32,
    padding: f32,
    target_ratio: f32,
    centered: bool,
    fill: Fill,
    /// `--char-rise-y`: how far a lit glyph rises, in em.
    rise: f32,
    upcoming_alpha: f32,
    falloff: f32,
    min_alpha: f32,
    past_alpha: Option<f32>,
    blur: f32,
    blur_scope: BlurScope,
    /// One CSS rem in points (YouTube Music's 10 px when the theme sizes in rem).
    rem: f32,
    /// `.blyrics-background-lyric` font size (em).
    bg_em: f32,
    /// `.blyrics-container` inline padding (start, end).
    pad: (f32, f32),
    /// `--lyplus-lyrics-palette` alpha: how bright a glyph's glow is at its peak.
    glow_alpha: f32,
    css: Option<ThemeCss>,
}

/// `GLOW_INVISIBLE_ALPHA`: a glow colour this faint renders nothing and is skipped.
const GLOW_INVISIBLE: f32 = 0.5 / 255.0;

impl Cfg {
    /// `width`: the lyrics panel; `viewport`: the window (the theme's media queries).
    /// The most a line is scaled up: while sung, or under a theme's hover scale.
    fn max_scale(&self) -> f32 {
        self.active_scale.max(self.css.as_ref().and_then(|c| c.hover_scale).unwrap_or(1.0)).max(1.0)
    }

    fn new(s: &LyricStyle, accent: Color32, width: f32, viewport: f32, centered: bool, scale: f32, surface: Surface) -> Self {
        let css = s.css.clone();
        // Font size and rem: a theme's own `--blyrics-font-size` steps in YouTube Music's
        // 10 px rem; otherwise 48 pt scaled by the preset with the extension's proportions.
        let (mut size, mut rem) = match css.as_ref().filter(|c| !c.font_rem.is_empty()) {
            Some(c) => {
                let r = c.font_rem.iter().filter(|st| viewport >= st[0]).map(|st| st[1]).next_back().unwrap_or(c.font_rem[0][1]);
                (r * 10.0, 10.0)
            }
            None => {
                let size = 48.0 * s.size.clamp(0.6, 1.6);
                (size, size / 3.0)
            }
        };
        // Kept to about a dozen CJK characters per row on narrow windows.
        let cap = (width / 10.5).max(22.0);
        if size > cap {
            rem *= cap / size;
            size = cap;
        }
        // The listener's text size goes on top, everything in rem along with it
        // (romanization, translation, spacing), so the theme keeps its proportions.
        // Whole points: every new size fills the font atlas again (it never shrinks),
        // and resizing the window would otherwise make one per pixel of width.
        let rounded = (size * scale).round().max(1.0);
        rem *= rounded / size;
        size = rounded;
        let glow = s.glow.clamp(0.0, 1.0);
        let weight = match css.as_ref().and_then(|c| c.font_weight) {
            Some(w) if w >= 700 => Weight::Bold,
            Some(w) if w >= 550 => Weight::Semibold,
            Some(_) => Weight::Regular,
            None => s.weight,
        };
        // A theme's own `--char-rise-y`; the app's look takes it from the preset's lift,
        // which is what the listener's slider moves.
        let rise = css.as_ref().and_then(|c| c.letter_wave).map_or(s.lift.clamp(0.0, 8.0) / size, |w| w[0]);
        // A look without a glow setting of its own glows only as much as its preset asks:
        // the default look has none.
        let glow_alpha = css.as_ref().and_then(|c| c.glow_alpha).unwrap_or(glow);
        let pad = css.as_ref().and_then(|c| c.padding_inline).map_or((0.0, 0.0), |p| (p[0] * rem / 10.0, p[1] * rem / 10.0));
        Self {
            font: theme::effective_font(&s.font),
            weight,
            size,
            colors: LyricColors::new(surface, theme::light(), theme::text(), theme::bg(), crate::preset::paint(&s.sung_color, accent), crate::preset::paint(&s.glow_color, accent)),
            inactive_alpha: s.active_alpha,
            // `--am-lyrics-inactive-scale`.
            scale: 0.98,
            active_scale: s.active_scale.max(1.0),
            padding: rem * 2.0 * (s.line_gap / 0.6).clamp(0.2, 3.0),
            target_ratio: surface.target_ratio(s),
            centered,
            fill: s.fill,
            rise,
            upcoming_alpha: s.inactive_alpha,
            falloff: s.falloff,
            min_alpha: s.min_alpha,
            past_alpha: s.past_alpha,
            blur: s.blur,
            blur_scope: s.blur_scope,
            rem,
            bg_em: css.as_ref().and_then(|c| c.background_em).unwrap_or(0.75),
            pad,
            glow_alpha,
            css,
        }
    }
    fn rem(&self) -> f32 {
        self.rem
    }
    // Each takes the text it sets, so a line in a script the font lacks is all Inter.
    fn main_font(&self, text: &str) -> FontId {
        theme::lyric_font_for(&self.font, self.weight, self.size, text)
    }
    fn bg_font(&self, text: &str) -> FontId {
        theme::lyric_font_for(&self.font, Weight::Semibold, self.size * self.bg_em, text)
    }
    fn roman_font(&self, text: &str) -> FontId {
        theme::lyric_font_for(&self.font, Weight::Semibold, self.rem() * 2.0 / 1.25, text)
    }
    fn trans_font(&self, text: &str) -> FontId {
        theme::lyric_font_for(&self.font, Weight::Semibold, self.rem() * 2.0, text)
    }
}

// ------------------------------------------------------------------ layout

/// One animated word (`.lyrics-syllable`), relative to its line's content box.
#[derive(Clone)]
struct Word {
    text: String,
    font: FontId,
    start: f64,
    dur: f64,
    /// Zero-duration (line-synced) word: fades in instead of sweeping.
    line_synced: bool,
    bg: bool,
    /// `--wipe-gradient-width` in em: the wipe's soft edge.
    feather: f32,
    /// A script whose characters stand alone and mean something apart (Han, kana, Hangul).
    cjk: bool,
    /// The pre-wipe: when its feather starts creeping in, and for how long (0 = none).
    pre_start: f64,
    pre_dur: f64,
    pos: Pos2,
    w: f32,
    h: f32,
    group: usize,
    /// Right-to-left letters: lit from the right.
    rtl: bool,
}

/// A word (`.lyrics-word`): the timed pieces that share one glyph clock.
#[derive(Clone)]
struct Group {
    first: usize,
    words: std::ops::Range<usize>,
}

#[derive(Clone)]
enum DecoBody {
    Timed { words: Vec<Word>, groups: Vec<Group> },
    Text(String, FontId),
}

#[derive(Clone)]
struct Deco {
    body: DecoBody,
    /// Box relative to the line's content origin (the pill, or the translation's text box).
    rect: Rect,
    pill: bool,
}

#[derive(Clone)]
struct Block {
    /// Index into the lyrics' lines (`None` for the credits).
    line: Option<usize>,
    start: f64,
    dur: f64,
    instrumental: bool,
    /// `v2`/`v3` sing on the right, `v1000` (everyone) in the middle.
    align: Align,
    top: f32,
    height: f32,
    words: Vec<Word>,
    groups: Vec<Group>,
    decos: Vec<Deco>,
    /// Songwriter credits text.
    credits: Option<(String, FontId)>,
}

#[derive(Clone, Copy, PartialEq)]
enum Align {
    Left,
    Right,
    Center,
}

struct Layout {
    key: u64,
    width: f32,
    blocks: Vec<Block>,
    pad_top: f32,
    content_h: f32,
    richsync: bool,
}

/// A whitespace run or a piece of a part (`splitPartIntoTokens`).
struct Tok {
    text: String,
    start: f64,
    dur: f64,
    bg: bool,
    space: bool,
}

fn split_part(p: &Part) -> Vec<Tok> {
    let mut chunks: Vec<(String, bool)> = Vec::new();
    for c in p.text.chars() {
        let ws = c.is_whitespace();
        match chunks.last_mut() {
            Some((s, w)) if *w == ws => s.push(c),
            _ => chunks.push((c.to_string(), ws)),
        }
    }
    let total: usize = chunks.iter().filter(|(_, w)| !*w).map(|(s, _)| s.chars().count()).sum();
    let mut cursor = 0usize;
    chunks
        .into_iter()
        .map(|(text, space)| {
            if space {
                return Tok { text, start: 0.0, dur: 0.0, bg: p.bg, space: true };
            }
            let n = text.chars().count();
            let (start_ms, end_ms) = if total == 0 {
                (p.start as f64, p.start as f64)
            } else {
                (
                    p.start as f64 + (p.dur as f64 * cursor as f64 / total as f64).round(),
                    p.start as f64 + (p.dur as f64 * (cursor + n) as f64 / total as f64).round(),
                )
            };
            cursor += n;
            Tok { text, start: start_ms / 1000.0, dur: (end_ms - start_ms) / 1000.0, bg: p.bg, space: false }
        })
        .collect()
}

/// `buildLineSyncedParts`: a line without timed parts becomes zero-duration words, each
/// 50 ms after the one before.
fn line_synced_parts(l: &Line) -> Vec<Part> {
    let mut out = Vec::new();
    let mut word = 0;
    let mut chunks: Vec<(String, bool)> = Vec::new();
    for c in l.text.chars() {
        let ws = c.is_whitespace();
        match chunks.last_mut() {
            Some((s, w)) if *w == ws => s.push(c),
            _ => chunks.push((c.to_string(), ws)),
        }
    }
    for (text, ws) in chunks {
        let start = l.start + (word as f64 * LINE_SYNC_DELAY * 1000.0) as i32;
        out.push(Part { start, dur: 0, text, bg: false });
        if !ws {
            word += 1;
        }
    }
    out
}

/// Joining scripts (Arabic and friends) keep whole-word sweeps, as in the extension.
fn joining_script(s: &str) -> bool {
    s.chars().any(|c| matches!(c as u32, 0x0600..=0x06FF | 0x0700..=0x074F | 0x0750..=0x077F | 0x0780..=0x07BF | 0x0840..=0x085F | 0x08A0..=0x08FF | 0x1800..=0x18AF | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF))
}

/// The extension's `char-rise` class: scripts whose every character is a word-sized unit,
/// which is what picks the tighter wipe, the spring rise and the missing glow.
fn cjk_script(s: &str) -> bool {
    s.chars().any(|c| matches!(c as u32, 0x2E80..=0x303F | 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F))
}

struct Flow {
    words: Vec<Word>,
    groups: Vec<Group>,
    height: f32,
    width: f32,
}

/// Lay out tokens the way inline formatting does it in the extension: groups between
/// spaces don't break (unless long), spaces do.
fn text_size(fonts: &mut FontsView<'_>, s: &str, font: &FontId) -> Vec2 {
    fonts.layout_no_wrap(s.to_owned(), font.clone(), Color32::WHITE).size()
}

/// `rtl`: the line reads right to left, so each row is laid out from the right.
fn flow(fonts: &mut FontsView<'_>, toks: Vec<Tok>, font: &FontId, max_w: f32, line_h: f32, align: Align, rtl: bool) -> Flow {
    // `groupTokensByWord`
    enum Item {
        Space,
        Group(Vec<Tok>),
    }
    let mut items: Vec<Item> = Vec::new();
    // A single piece wider than the line (a line-synced CJK phrase arrives as one, with no
    // spaces to break at) is split into its characters, sharing its time, so it can wrap.
    let toks = toks.into_iter().flat_map(|t| {
        // Not right-to-left words either: their letters, laid out one by one, would read backwards.
        if t.space || joining_script(&t.text) || crate::bidi::has_rtl(&t.text) || t.text.chars().count() < 2 || text_size(fonts, &t.text, font).x <= max_w {
            return vec![t];
        }
        let n = t.text.chars().count();
        t.text
            .chars()
            .enumerate()
            .map(|(i, c)| Tok { text: c.to_string(), start: t.start + t.dur * i as f64 / n as f64, dur: t.dur / n as f64, bg: t.bg, space: false })
            .collect()
    });
    for t in toks {
        if t.space {
            items.push(Item::Space);
            continue;
        }
        match items.last_mut() {
            Some(Item::Group(g)) if g[0].bg == t.bg => g.push(t),
            _ => items.push(Item::Group(vec![t])),
        }
    }
    let space_w = text_size(fonts, " ", font).x.max(font.size * 0.25);
    let mut words: Vec<Word> = Vec::new();
    let mut groups: Vec<Group> = Vec::new();
    let mut rows: Vec<(usize, f32)> = vec![(0, 0.0)]; // (first word index, width)
    let (mut x, mut row) = (0.0f32, 0usize);
    let mut pending_space = false;
    for item in items {
        match item {
            Item::Space => pending_space = !words.is_empty(),
            Item::Group(toks) => {
                let text: String = toks.iter().map(|t| t.text.as_str()).collect();
                let long = text.chars().count() >= LONG_GROUP_CHARS;
                let cjk = cjk_script(&text);
                let sizes: Vec<Vec2> = toks.iter().map(|t| text_size(fonts, &t.text, font)).collect();
                let w: f32 = sizes.iter().map(|s| s.x).sum();
                let sp = if pending_space { space_w } else { 0.0 };
                pending_space = false;
                // A group that can't fit even on a row of its own breaks between its parts
                // rather than running off the side (the extension lets it overflow).
                let breakable = long || w > max_w;
                if x > 0.0 && x + sp + w > max_w && !breakable {
                    row += 1;
                    x = 0.0;
                    rows.push((words.len(), 0.0));
                } else {
                    x += sp;
                }
                let gi = groups.len();
                let first = words.len();
                for (t, size) in toks.iter().zip(sizes) {
                    if breakable && x > 0.0 && x + size.x > max_w {
                        row += 1;
                        x = 0.0;
                        rows.push((words.len(), 0.0));
                    }
                    // `applyWipeShape`: the edge widens with the word, up to a fifth of an em.
                    let glyphs = t.text.chars().filter(|c| !c.is_whitespace()).count() as f32;
                    let feather = if cjk {
                        FEATHER_CJK_EM
                    } else {
                        FEATHER_EM + ((glyphs - 6.0) / FEATHER_LONG_FROM).clamp(0.0, 1.0) * FEATHER_EXTRA_EM
                    };
                    words.push(Word {
                        text: t.text.clone(),
                        font: font.clone(),
                        start: t.start,
                        dur: t.dur,
                        line_synced: t.dur <= 0.0,
                        bg: t.bg,
                        feather,
                        cjk,
                        pre_start: 0.0,
                        pre_dur: 0.0,
                        pos: pos2(x, row as f32 * line_h + (line_h - size.y) * 0.5),
                        w: size.x,
                        h: size.y,
                        group: gi,
                        rtl: crate::bidi::has_rtl(&t.text),
                    });
                    x += size.x;
                    rows[row].1 = x;
                }
                groups.push(Group { first, words: first..words.len() });
            }
        }
    }
    // Right to left: words go from the right of each row, except runs of left-to-right
    // ones (Latin, numbers), which keep their order; in a left-to-right line, the other
    // way round.
    for (r, &(first, rw)) in rows.iter().enumerate() {
        let end = rows.get(r + 1).map_or(words.len(), |n| n.0);
        let row = &mut words[first..end];
        if rtl {
            for w in row.iter_mut() {
                w.pos.x = rw - w.pos.x - w.w;
            }
        }
        let mut i = 0;
        while i < row.len() {
            let mut j = i;
            while j < row.len() && row[j].rtl != rtl {
                j += 1;
            }
            if j > i + 1 {
                let lo = row[i..j].iter().map(|w| w.pos.x).fold(f32::INFINITY, f32::min);
                let hi = row[i..j].iter().map(|w| w.pos.x + w.w).fold(f32::NEG_INFINITY, f32::max);
                for w in &mut row[i..j] {
                    w.pos.x = lo + hi - w.pos.x - w.w;
                }
            }
            i = j.max(i + 1);
        }
    }
    // Align rows to the right or centre.
    if align != Align::Left {
        for (r, &(first, rw)) in rows.iter().enumerate() {
            let end = rows.get(r + 1).map_or(words.len(), |n| n.0);
            let shift = if align == Align::Right { max_w - rw } else { (max_w - rw) * 0.5 };
            for w in &mut words[first..end] {
                w.pos.x += shift.max(0.0);
            }
        }
    }
    // `maybePreWipeNextWord`: when a word follows another with no breath between them, its
    // feather starts creeping in early, so the highlight hands over without a gap.
    for i in 1..words.len() {
        let (prev, next) = (&words[i - 1], &words[i]);
        if next.group == prev.group || next.line_synced || prev.line_synced || next.bg || prev.bg || next.dur <= 0.0 {
            continue;
        }
        let prev_end = prev.start + prev.dur.max(0.0);
        let gap = next.start - prev_end;
        if !(0.0..=PRE_WIPE_GAP).contains(&gap) {
            continue;
        }
        let n = next.text.chars().filter(|c| !c.is_whitespace()).count() as f64;
        // `getWordPreWipeDuration`: 64 ms plus 9 ms a glyph, and the handoff is never
        // more than 100 ms behind the previous word.
        let pre_dur = (0.064 + 0.009 * n).clamp(PRE_WIPE_MIN, PRE_WIPE_MAX);
        let (pre_dur, pre_start) = (pre_dur, next.start - pre_dur);
        let pre_start = pre_start.max(prev_end - PRE_WIPE_HANDOFF);
        // The write comes last, after every read of the pair.
        let w = &mut words[i];
        w.pre_dur = pre_dur;
        w.pre_start = pre_start;
    }
    let width = rows.iter().map(|r| r.1).fold(0.0, f32::max);
    let height = if words.is_empty() { 0.0 } else { (row + 1) as f32 * line_h };
    Flow { words, groups, height, width }
}

/// Romanization for a line: timed (from the lyrics) or plain text.
pub enum Roman<'a> {
    Timed(&'a [Part], &'a str),
    Text(&'a str),
}

fn hash<T: std::hash::Hash>(v: T) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

#[allow(clippy::too_many_arguments)]
fn build_block(fonts: &mut FontsView<'_>, cfg: &Cfg, l: &Line, index: usize, width: f32, roman: Option<Roman>, trans: Option<&str>, agent: Option<&str>) -> Block {
    let em = cfg.size;
    let rtl = crate::bidi::rtl_base(&l.text);
    let align = match agent {
        Some("v2" | "v3") => Align::Right,
        Some("v1000") => Align::Center,
        _ if cfg.centered => Align::Center,
        _ => Align::Left,
    };
    // Right-to-left lines start on the right (and the second singer's on the left).
    let align = match align {
        Align::Left if rtl => Align::Right,
        Align::Right if rtl => Align::Left,
        a => a,
    };
    // Wrapped to fit at the line's largest scale (sung, or a theme's hover), which grows
    // it about its aligned edge: wrapped at full width, a sung line ran off the side.
    let full_w = (width - em * 0.5).max(40.0);
    let content_w = (full_w / cfg.max_scale()).max(40.0);
    let line_h = em * 1.333;
    let mut y = cfg.padding;
    let mut block = Block {
        line: Some(index),
        start: l.start as f64 / 1000.0,
        dur: l.dur.max(0) as f64 / 1000.0,
        instrumental: l.instrumental,
        align,
        top: 0.0,
        height: 0.0,
        words: vec![],
        groups: vec![],
        decos: vec![],
        credits: None,
    };
    if l.instrumental {
        block.height = cfg.padding * 2.0 + line_h;
        return block;
    }
    let parts = if l.parts.is_empty() { line_synced_parts(l) } else { l.parts.clone() };
    let toks: Vec<Tok> = parts.iter().flat_map(split_part).collect();
    let (main, bg): (Vec<Tok>, Vec<Tok>) = toks.into_iter().partition(|t| !t.bg);
    let joined = |toks: &[Tok]| toks.iter().map(|t| t.text.as_str()).collect::<String>();
    let font = cfg.main_font(&joined(&main));
    let f = flow(fonts, main, &font, content_w, line_h, align, rtl);
    for w in f.words {
        block.words.push(Word { pos: w.pos + vec2(0.0, y), ..w });
    }
    block.groups = f.groups;
    y += f.height;
    if bg.iter().any(|t| !t.space) {
        y += em * 0.1;
        let base = block.words.len();
        let g0 = block.groups.len();
        let font = cfg.bg_font(&joined(&bg));
        let f = flow(fonts, bg, &font, content_w, line_h, align, rtl);
        for w in f.words {
            block.words.push(Word { pos: w.pos + vec2(0.0, y), group: w.group + g0, ..w });
        }
        block.groups.extend(f.groups.into_iter().map(|g| Group { first: g.first + base, words: g.words.start + base..g.words.end + base, ..g }));
        y += f.height;
    }
    let gap = em / 6.0; // margin-top: 8px at 48 px
    if let Some(r) = roman {
        y += gap;
        let pad = vec2(cfg.rem() * 0.75, cfg.rem() * 0.25);
        let inner_w = content_w - pad.x * 2.0 - 2.0;
        let (body, inner) = match r {
            Roman::Timed(parts, text) => {
                let toks: Vec<Tok> = parts.iter().flat_map(split_part).collect();
                let font = cfg.roman_font(text);
                let lh = fonts.row_height(&font);
                let f = flow(fonts, toks, &font, inner_w, lh, Align::Left, false);
                (DecoBody::Timed { words: f.words, groups: f.groups }, vec2(f.width, f.height))
            }
            Roman::Text(t) => {
                let font = cfg.roman_font(t);
                let g = fonts.layout(t.to_owned(), font.clone(), Color32::WHITE, inner_w);
                (DecoBody::Text(t.to_owned(), font), g.size())
            }
        };
        let size = inner + pad * 2.0 + vec2(2.0, 2.0);
        let x = match align {
            Align::Left => 0.0,
            Align::Right => content_w - size.x,
            Align::Center => (content_w - size.x) * 0.5,
        };
        let rect = Rect::from_min_size(pos2(x, y), size);
        // Timed words sit inside the pill's padding.
        let body = match body {
            DecoBody::Timed { words, groups } => DecoBody::Timed { words: words.into_iter().map(|w| Word { pos: w.pos + rect.min.to_vec2() + pad + vec2(1.0, 1.0), ..w }).collect(), groups },
            b => b,
        };
        block.decos.push(Deco { body, rect, pill: true });
        y += size.y;
    }
    if let Some(t) = trans {
        y += gap;
        let font = cfg.trans_font(t);
        let g = fonts.layout(t.to_owned(), font.clone(), Color32::WHITE, content_w);
        let x = match align {
            Align::Left => 0.0,
            Align::Right => content_w - g.size().x,
            Align::Center => (content_w - g.size().x) * 0.5,
        };
        block.decos.push(Deco { body: DecoBody::Text(t.to_owned(), font), rect: Rect::from_min_size(pos2(x, y), g.size()), pill: false });
        y += g.size().y;
    }
    block.height = y + cfg.padding;
    // Back to the column's edge or centre, which the narrower wrap moved.
    let inset = match align {
        Align::Left => 0.0,
        Align::Right => full_w - content_w,
        Align::Center => (full_w - content_w) * 0.5,
    };
    if inset > 0.0 {
        let dx = vec2(inset, 0.0);
        for w in &mut block.words {
            w.pos += dx;
        }
        for d in &mut block.decos {
            d.rect = d.rect.translate(dx);
            if let DecoBody::Timed { words, .. } = &mut d.body {
                for w in words {
                    w.pos += dx;
                }
            }
        }
    }
    block
}

// ------------------------------------------------------------------ state

/// One scroll: every line eases from `delta` above/below its new place.
#[derive(Clone, Copy)]
struct ScrollAnim {
    start: f64,
    delta: f32,
    active: usize,
}

/// A line shifted by a relayout (a translation or romanization arriving).
#[derive(Clone, Copy)]
struct Shift {
    start: f64,
    delta: f32,
}

/// A CSS transition on one value: eases from where it was to the new target, after a delay.
#[derive(Clone, Copy, Default)]
struct Trans {
    from: f32,
    to: f32,
    start: f64,
    dur: f64,
    delay: f64,
    init: bool,
}

impl Trans {
    fn value(&self, now: f64) -> f32 {
        if !self.init {
            return self.to;
        }
        let p = ((now - self.start - self.delay) / self.dur.max(1e-3)).clamp(0.0, 1.0) as f32;
        lerp(self.from, self.to, EASE.at(p))
    }

    fn set(&mut self, to: f32, now: f64, dur: f64, delay: f64) {
        if !self.init {
            *self = Trans { from: to, to, start: now, dur, delay, init: true };
        } else if (to - self.to).abs() > 1e-4 {
            let from = self.value(now);
            *self = Trans { from, to, start: now, dur, delay, init: true };
        }
    }

    fn moving(&self, now: f64) -> bool {
        self.init && (self.from - self.to).abs() > 1e-4 && now < self.start + self.delay + self.dur
    }
}

#[derive(Clone, Copy, Default)]
struct LineState {
    /// Whole-line opacity and blur (the theme's line-state rules).
    opacity: Trans,
    blur: Trans,
    selected: bool,
    /// Wall time the exit animations started (line scale back, lit words fading).
    exit: Option<f64>,
    /// Song time at exit: which words had started.
    exit_t: f64,
}

/// A lyrics surface has its own view state, independent of horizontal alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    Player,
    Sidebar,
}

impl Surface {
    fn target_ratio(self, style: &LyricStyle) -> f32 {
        match self {
            Self::Player => style.anchor.clamp(0.1, 0.7),
            // Leave room for the edge fade, but keep the current line near the top.
            Self::Sidebar => 0.12,
        }
    }
}

/// The lyrics view's state between frames.
#[derive(Default)]
pub struct View {
    layout: Option<Layout>,
    states: Vec<LineState>,
    scroll_top: f32,
    scroll_set: bool,
    scrolls: Vec<ScrollAnim>,
    shifts: Vec<Vec<Shift>>,
    last_scroll: Vec<usize>,
    /// The line taller than the view being sung, and its row: the view pages down with it.
    page: Option<(usize, i32)>,
    last_t: f64,
    last_wall: f64,
    user_until: f64,
    /// Reader scrolling was on last frame: when it ends, scroll back to the current line.
    was_user: bool,
    glow: Glow,
    credits_focused: bool,
    /// The frame (`cumulative_pass_nr`) the view was last drawn in; 0 once its textures
    /// are freed.
    shown_pass: u64,
}

pub struct Frame<'a> {
    pub lyrics: &'a Lyrics,
    /// Identifies the song, source, decorations and settings the layout depends on.
    pub key: u64,
    pub roman: Vec<Option<Roman<'a>>>,
    pub trans: Vec<Option<&'a str>>,
    /// Audible song position (s), already corrected for output latency and the
    /// listener's offsets.
    pub t: f64,
    pub playing: bool,
    pub now: f64,
    pub rect: Rect,
    pub style: &'a LyricStyle,
    pub accent: Color32,
    pub centered: bool,
    pub surface: Surface,
    /// The listener's lyrics text size, a multiple of the theme's.
    pub scale: f32,
}

pub struct Out {
    pub seek: Option<f64>,
    /// Something is moving: keep drawing frames.
    pub animating: bool,
    /// The reader scrolled away; a "resume" control can bring the view back.
    pub user_scrolling: bool,
}

impl View {
    pub fn reset(&mut self) {
        *self = Self { glow: std::mem::take(&mut self.glow), shown_pass: self.shown_pass, ..Default::default() };
    }

    /// Called every frame: once the view isn't drawn (another tab or page), its glows,
    /// blurred lines and atlas copy go; they're rebuilt as it comes back.
    pub fn release_hidden(&mut self, pass: u64) {
        if self.shown_pass != 0 && self.shown_pass + 1 < pass {
            self.glow = Glow::default();
            self.shown_pass = 0;
        }
    }

    pub fn resume(&mut self) {
        self.user_until = 0.0;
    }

    /// CSS `text-shadow: 0 0 <blur>` for a laid-out text at `at`: the same Gaussian the
    /// lyric glow uses (σ = blur / 2), as a white mask to tint when painting.
    pub fn text_glow(&mut self, ctx: &egui::Context, g: Arc<Galley>, at: Pos2, blur: f32) -> Option<(TextureId, Rect, Rect)> {
        let atlas = ctx.fonts(|fv| fv.font_image_size());
        let ppp = ctx.pixels_per_point();
        // Cached per position, so a moved or resized view re-renders it.
        let key = (hash(("text-glow", at.x.round() as i32, at.y.round() as i32, g.job.text.as_str())), usize::MAX);
        self.glow.blurred_line(ctx, key, blur / 2.0, &[], &[(g, at, 1.0)], &[], 1.0, atlas, ppp)
    }

    pub fn show(&mut self, ui: &mut egui::Ui, f: Frame) -> Out {
        let rect = f.rect;
        let ctx = ui.ctx().clone();
        self.shown_pass = ctx.cumulative_pass_nr();
        let mut cfg = Cfg::new(f.style, f.accent, rect.width(), ctx.content_rect().width(), f.centered, f.scale, f.surface);
        if f.surface == Surface::Sidebar {
            // On short windows keep the active text below the renderer's top-edge fade.
            let fade = (cfg.size * 1.6).min(rect.height() * 0.15).max(1.0);
            let visible_center = (fade + cfg.size * 0.5) / rect.height().max(1.0);
            cfg.target_ratio = cfg.target_ratio.max(visible_center.min(0.7));
        }
        // Padding depends on the anchor and, in the drawer, the viewport height too.
        // Keep the player's existing resize behavior while updating the drawer on resize.
        let height = if f.surface == Surface::Sidebar { rect.height().round() as i32 } else { 0 };
        let layout_key = hash((f.key, rect.width().round() as i32, height, cfg.size.to_bits(), cfg.padding.to_bits(), cfg.max_scale().to_bits(), cfg.target_ratio.to_bits(), cfg.centered, &cfg.font, cfg.weight as u8));
        let layout_changed = self.layout.as_ref().is_none_or(|l| l.key != layout_key);
        if layout_changed {
            self.relayout(ui, &cfg, &f, layout_key, rect);
        }
        let layout = self.layout.as_ref().unwrap();
        let richsync = layout.richsync;
        // Lines that aren't credits.
        let lines_n = layout.blocks.iter().filter(|b| b.line.is_some()).count();

        // Clock: the extension shows lyrics this far ahead of the media time.
        let t = f.t + if richsync { RICHSYNC_OFFSET } else { LINE_OFFSET };
        let jumped = (t - self.last_t - (f.now - self.last_wall)).abs() > TIME_JUMP || self.last_wall == 0.0;
        self.last_t = t;
        self.last_wall = f.now;

        // ---- selection and exits (tickView's per-line loop)
        let early = if f.playing { 2.0 } else { 0.0 };
        let mut animating = false;
        for i in 0..lines_n {
            let b = &layout.blocks[i];
            let next = layout.blocks.get(i + 1).filter(|n| n.line.is_some()).map_or(f64::INFINITY, |n| n.start);
            let end = next.max(b.start + b.dur + 0.05);
            let st = &mut self.states[i];
            let selected = t + early >= b.start && t < end;
            if selected && !st.selected {
                st.selected = true;
                st.exit = None;
            } else if !selected && st.selected {
                st.selected = false;
                let stale = end + FADE_OUT + 0.05;
                st.exit = if (f.playing || jumped) && t <= stale { Some(f.now) } else { None };
                st.exit_t = t;
            } else if !selected && jumped {
                st.exit = None;
            }
            if st.exit.is_some_and(|e| f.now - e > FADE_OUT.max(SCALE_OUT)) {
                st.exit = None;
            }
            animating |= st.exit.is_some();
        }

        // ---- scrolling
        let view_h = rect.height();
        // The view runs ahead of the singing by the gap the line being sung leaves before
        // the next one starts, held between 350 and 500 ms.
        let sung = (0..lines_n).rev().find(|&i| t >= layout.blocks[i].start && t < layout.blocks[i].start + layout.blocks[i].dur.max(0.001));
        let ahead = sung
            .map_or(SCROLL_AHEAD_MAX, |i| layout.blocks.get(i + 1).map_or(SCROLL_AHEAD_MAX, |n| n.start - (layout.blocks[i].start + layout.blocks[i].dur)));
        let scroll_time = t + ahead.clamp(SCROLL_AHEAD_MIN, SCROLL_AHEAD_MAX);
        let user = f.now < self.user_until;
        let mut active: Vec<usize> = Vec::new();
        // Leaving reader scrolling (the button, or the timeout) returns to the current line now.
        let mut new_lyric = jumped || (self.was_user && !user) || (f.surface == Surface::Sidebar && layout_changed);
        self.was_user = user;
        for i in 0..lines_n {
            let b = &layout.blocks[i];
            let next = layout.blocks.get(i + 1).filter(|n| n.line.is_some()).map_or(f64::INFINITY, |n| n.start);
            if scroll_time >= b.start - EARLY_SCROLL && (scroll_time < next || scroll_time < b.start + b.dur) {
                active.push(i);
                if !self.last_scroll.contains(&i) && scroll_time >= b.start {
                    new_lyric = true;
                }
            }
        }
        let active_real = !active.is_empty();
        let last_sung = (0..lines_n).rev().find(|&i| !layout.blocks[i].instrumental);
        let credits = layout.blocks.iter().position(|b| b.credits.is_some());
        let credits_focused = credits.is_some() && last_sung.is_some_and(|i| scroll_time >= layout.blocks[i].start + layout.blocks[i].dur);
        if credits_focused != self.credits_focused {
            self.credits_focused = credits_focused;
            new_lyric = true;
        }
        // A line taller than the view would run off the bottom while it's sung: how far
        // down it the singing is (the latest word's row, or for a line-synced line, as far
        // as its time has gone), so the view pages down with it.
        let line_h = cfg.size * 1.333;
        let sung_y = |i: usize| -> Option<f32> {
            let b = &layout.blocks[i];
            if b.height <= view_h || scroll_time < b.start {
                return None;
            }
            let mut timed = b.words.iter().filter(|w| !w.line_synced && !w.bg).peekable();
            Some(if timed.peek().is_none() {
                b.height * ((scroll_time - b.start) / b.dur.max(0.001)).clamp(0.0, 1.0) as f32
            } else {
                timed.filter(|w| w.start <= scroll_time).map(|w| w.pos.y).fold(0.0, f32::max)
            })
        };
        let page = active.last().and_then(|&i| sung_y(i).map(|y| (i, (y / line_h) as i32)));
        if page != self.page {
            self.page = page;
            new_lyric |= page.is_some();
        }
        let pos = |i: usize| layout.pad_top + layout.blocks[i].top;
        let max_scroll = (layout.content_h - view_h).max(0.0);
        if f.surface == Surface::Sidebar && layout_changed {
            self.scroll_top = self.scroll_top.clamp(0.0, max_scroll);
        }
        if !user && (new_lyric || !self.scroll_set) && lines_n > 0 {
            if active.is_empty() {
                active.push(0);
            }
            let offset = view_h * cfg.target_ratio;
            let last = *active.last().unwrap();
            let sung: Vec<usize> = active.iter().copied().enumerate().filter(|&(k, i)| scroll_time < layout.blocks[i].start + layout.blocks[i].dur - ENDING_THRESHOLD || k == active.len() - 1).map(|(_, i)| i).collect();
            let anchor = sung
                .iter()
                .rev()
                .find(|&&i| layout.blocks[i].start <= scroll_time && scroll_time < layout.blocks[i].start + layout.blocks[i].dur)
                .or_else(|| sung.iter().rev().find(|&&i| layout.blocks[i].start <= scroll_time))
                .copied()
                .unwrap_or(sung[0]);
            // `computeActiveLinesScrollTop`
            let (first, lastl) = (sung[0], *sung.last().unwrap());
            let centre = sung.iter().map(|&i| pos(i) + layout.blocks[i].height / 2.0).sum::<f32>() / sung.len() as f32;
            let h = |i: usize| layout.blocks[i].height;
            let mut top = centre - offset;
            top = top.max(pos(lastl) + h(lastl) - view_h);
            top = top.min(pos(lastl)).min(pos(first));
            top = top.max(pos(anchor) + h(anchor) - view_h);
            top = top.min(pos(anchor));
            if f.surface == Surface::Sidebar && h(anchor) <= view_h - line_h {
                let fade = (cfg.size * 1.6).min(view_h * 0.15).max(1.0);
                top = top.min(pos(anchor) - fade);
            }
            if let Some(y) = sung_y(anchor) {
                // Keep the row being sung, and the one after it, in view.
                top = top.max(pos(anchor) + y + line_h * 2.0 - view_h);
            }
            if credits_focused && let (Some(c), Some(ls)) = (credits, last_sung) {
                top = (pos(c) + h(c) / 2.0 - offset).min(pos(ls));
            }
            let top = top.clamp(0.0, max_scroll);
            if !self.scroll_set || (f.surface == Surface::Sidebar && layout_changed) {
                self.scrolls.clear();
                self.scroll_top = top;
                self.scroll_set = true;
            } else if (top - self.scroll_top).abs() > 2.0 {
                self.scrolls.push(ScrollAnim { start: f.now, delta: top - self.scroll_top, active: last });
                self.scroll_top = top;
            }
            self.last_scroll = active.clone();
        }

        // Reader scrolling.
        let hovered = ui.rect_contains_pointer(rect);
        let wheel = if hovered { ui.input(|i| i.smooth_scroll_delta.y) } else { 0.0 };
        if wheel != 0.0 {
            self.scroll_top = (self.scroll_top - wheel).clamp(0.0, max_scroll);
            self.user_until = f.now + USER_SCROLL_RESUME;
            self.scrolls.clear();
        }
        let user = f.now < self.user_until;

        // Scroll animations: per line, eased from where it was.
        self.scrolls.retain(|a| f.now - a.start < 2.0);
        animating |= !self.scrolls.is_empty();
        for s in &mut self.shifts {
            s.retain(|a| f.now - a.start < DECO_ENTRY);
            animating |= !s.is_empty();
        }

        // ---- paint
        let painter = ui.painter_at(rect);
        let first_shape = ctx.graphics(|g| g.get(painter.layer_id()).map_or(ShapeIdx(0), |l| l.next_idx()));
        let origin_y = rect.top() - self.scroll_top + layout.pad_top;
        let x0 = rect.left() + cfg.pad.0;
        let line_w = rect.width() - cfg.pad.0 - cfg.pad.1;
        let atlas = ctx.fonts(|fv| fv.font_image_size());
        let ppp = ctx.pixels_per_point();
        let mut seek = None;
        let active_line = active.last().copied();
        let first_active = active.first().copied().filter(|_| active_real);
        let mut glow_work: Vec<GlowJob> = Vec::new();
        let pointer = ui.input(|i| i.pointer.hover_pos()).filter(|p| rect.contains(*p));

        for (i, b) in layout.blocks.iter().enumerate() {
            let mut dy = 0.0;
            for a in &self.scrolls {
                let rel = i as isize - a.active as isize;
                // `--scroll-duration` for every line, with the cascade as a wait rather than
                // a slower move: a line four away starts a fifth of a second into the run.
                let wait = scroll_cascade(rel);
                let p = ((f.now - a.start - wait) / SCROLL_DUR).clamp(0.0, 1.0);
                dy += a.delta * (1.0 - spring_step(p * SCROLL_DUR, SCROLL_SPRING.0, SCROLL_SPRING.1) as f32);
            }
            if let Some(shifts) = self.shifts.get(i) {
                for s in shifts {
                    let p = ((f.now - s.start) / DECO_ENTRY).clamp(0.0, 1.0) as f32;
                    dy += s.delta * (1.0 - DECO_EASE.at(p));
                }
            }
            let top = origin_y + b.top + dy;
            if top > rect.bottom() + 40.0 || top + b.height < rect.top() - 40.0 {
                continue;
            }
            let block_rect = Rect::from_min_size(pos2(x0, top), vec2(line_w, b.height));
            let content = pos2(x0 + cfg.size * 0.25, top);

            if let Some((text, font)) = &b.credits {
                let a = if credits_focused { 0.85 } else { 0.2 };
                let a = theme::anim(&ctx, ("lyr-credits", f.key), a, 0.35);
                let o = transform_origin(block_rect, b.align, content.x);
                let g = ctx.fonts_mut(|fv| fv.layout(text.clone(), font.clone(), Color32::WHITE, line_w - cfg.size * 0.5));
                let xf = Xf { origin: o, scale: cfg.scale, content };
                paint_galley(&painter, &g, pos2(0.0, cfg.padding), &xf, theme::with_alpha(cfg.colors.foreground, cfg.colors.dim_alpha(a)), atlas);
                continue;
            }
            let li = b.line.unwrap();
            let hovered = pointer.is_some_and(|p| block_rect.contains(p));
            let st = self.states[i];
            let sung_now = st.selected && t >= b.start;

            // Whole-line opacity and blur. A theme that styles lines by state (its CSS
            // `.blyrics--active` rules) gets those values and their transitions; the
            // others get the preset's approximations.
            let (dim, opacity, blur) = if let Some(css) = &cfg.css {
                let on = active.contains(&i) && active_real || st.selected;
                let target = if hovered || on {
                    (1.0, 0.0)
                } else {
                    match first_active {
                        Some(a) if i < a && !user => css.past.map_or((1.0, 0.0), |p| (p[0], p[1])),
                        Some(a) if i > a => css.upcoming.map_or((1.0, 0.0), |p| (p[0], p[1])),
                        _ => (1.0, 0.0),
                    }
                };
                let line_ms = b.dur;
                let (op_t, blur_t) = if hovered {
                    // `:hover { transition: all 0.3s ease }`
                    ((0.3, 0.0), (0.3, 0.0))
                } else {
                    let o = css.opacity_transition.unwrap_or([0.0, 0.0]);
                    let fl = css.filter_transition.unwrap_or([0.0, 0.0]);
                    ((line_ms * o[0] as f64, o[1] as f64), (line_ms * fl[0] as f64, fl[1] as f64))
                };
                let s = &mut self.states[i];
                s.opacity.set(target.0, f.now, op_t.0, op_t.1);
                s.blur.set(target.1, f.now, blur_t.0, blur_t.1);
                animating |= s.opacity.moving(f.now) || s.blur.moving(f.now);
                (cfg.inactive_alpha, s.opacity.value(f.now), s.blur.value(f.now))
            } else {
                let rel = active_line.map_or(1, |a| i as isize - a as isize);
                let past = rel < 0;
                let dist = rel.unsigned_abs() as f32;
                // The row beside the one being sung is as bright as that one; the steps
                // begin after it, and stop at the preset's floor.
                let upcoming = (cfg.upcoming_alpha - (dist - 1.0).max(0.0).min(6.0) * cfg.falloff).max(cfg.min_alpha.min(cfg.upcoming_alpha));
                let reveal_id = if f.surface == Surface::Sidebar { "sidebar-lyr-reveal" } else { "lyr-reveal" };
                let reveal = theme::anim_bool(&ctx, reveal_id, user, 0.3);
                let dim = if st.selected || rel == 0 {
                    cfg.inactive_alpha
                } else if past {
                    // The lines already sung stay above the current one, dimmed, unless the
                    // listener turned them off in Settings.
                    let shown = ctx.data(|d| d.get_temp::<bool>(egui::Id::unique("lyrics-past"))).unwrap_or(true);
                    lerp(if shown { cfg.past_alpha.unwrap_or(upcoming) } else { 0.0 }, upcoming, reveal)
                } else {
                    upcoming
                };
                // `transition: opacity 0.7 s ease, filter 0.7 s ease` on a line's row.
                let dim = theme::anim(&ctx, ("lyr-dim", f.key, i), dim, 0.7);
                let blur_on = match cfg.blur_scope {
                    BlurScope::All => true,
                    BlurScope::Past => past,
                    BlurScope::Upcoming => !past || reveal > 0.0,
                };
                // `.pre-active`: the line about to be sung is sharp and unblurred already.
                let target = if cfg.blur > 0.0 && blur_on && !st.selected && rel != 0 { (cfg.blur * (1.0 + 0.35 * (dist - 1.0).clamp(0.0, 3.0))).min(9.0) } else { 0.0 };
                let s = &mut self.states[i];
                s.blur.set(target, f.now, 0.7, 0.0);
                animating |= s.blur.moving(f.now);
                (dim, 1.0, s.blur.value(f.now))
            };
            let dim = cfg.colors.dim_alpha(dim);

            // Line scale: `--am-lyrics-inactive-scale` at rest, 1 while sung (enter over
            // 0.5 s at its start, settle back over 0.4 s when it ends); a theme's hover
            // scale on lines not being sung.
            let mut scale = cfg.scale;
            if st.selected && t >= b.start {
                scale = lerp(cfg.scale, cfg.active_scale, EASE.at(((t - b.start) / SCALE_IN) as f32));
            } else if let Some(e) = st.exit {
                scale = lerp(cfg.active_scale, cfg.scale, SETTLE.at(((f.now - e) / SCALE_OUT) as f32));
            } else if let Some(h) = cfg.css.as_ref().and_then(|c| c.hover_scale) {
                let k = theme::anim_bool(&ctx, ("lyr-hover", f.key, i), hovered, 0.3);
                scale = lerp(cfg.scale, h, EASE.at(k));
            }
            if st.selected && t >= b.start && t < b.start + SCALE_IN {
                animating = true;
            }
            let origin = transform_origin(block_rect, b.align, content.x);
            let xf = Xf { origin, scale, content };

            let text_lit = if let Some(e) = st.exit {
                1.0 - EASE.at(((f.now - e) / FADE_OUT) as f32)
            } else if sung_now {
                EASE.at(((t - b.start) / FADE_IN) as f32)
            } else {
                0.0
            };
            let text_alpha = (if cfg.colors.light_sidebar { 0.8 } else { 0.6 }) * lerp(dim, 1.0, text_lit);
            // The pill belongs to its line: it fades out with the line's text.
            let fade = lerp(dim, 1.0, text_lit);
            if b.instrumental {
                animating |= paint_instrumental(&painter, &cfg, b, &xf, t, line_w - cfg.size * 0.5);
            } else if blur > 0.05 {
                // `filter: blur()` on the whole line: its text, romanization and translation.
                let r = (blur * 4.0).round() / 4.0;
                let deco_words: Vec<(&Word, f32)> = b
                    .decos
                    .iter()
                    .flat_map(|d| match &d.body {
                        DecoBody::Timed { words, .. } => words.iter().map(|w| (w, dim)).collect::<Vec<_>>(),
                        DecoBody::Text(..) => vec![],
                    })
                    .collect();
                let texts: Vec<(Arc<Galley>, Pos2, f32)> = b
                    .decos
                    .iter()
                    .filter_map(|d| match &d.body {
                        DecoBody::Text(text, font) => {
                            let g = ctx.fonts_mut(|fv| fv.layout(text.clone(), font.clone(), Color32::WHITE, d.rect.width() + 1.0));
                            let pad = if d.pill { vec2(cfg.rem() * 0.75 + 1.0, cfg.rem() * 0.25 + 1.0) } else { Vec2::ZERO };
                            Some((g, d.rect.min + pad, text_alpha))
                        }
                        DecoBody::Timed { .. } => None,
                    })
                    .collect();
                let words: Vec<(&Word, f32)> = b.words.iter().map(|w| (w, if w.bg { dim * 0.78 } else { dim })).chain(deco_words).collect();
                // The romanization pill is part of the line, so it's blurred with it.
                let pills: Vec<(Rect, f32, f32)> = b.decos.iter().filter(|d| d.pill).map(|d| (d.rect, cfg.rem(), fade)).collect();
                if let Some((tex, rr, uv)) = self.glow.blurred_line(&ctx, (f.key, i), r, &words, &texts, &pills, scale, atlas, ppp) {
                    // Rendered at the line's scale; snapped so it lands 1:1 on screen pixels.
                    let to = xf.rect(rr);
                    let snapped = (to.min.to_vec2() * ppp).round() / ppp;
                    painter.image(tex, to.translate(snapped - to.min.to_vec2()), uv, theme::with_alpha(cfg.colors.foreground, opacity));
                }
                animating |= st.exit.is_some();
            } else {
                let lit = LitState::new(&cfg, st, t, f.now, sung_now || st.selected);
                paint_words(&painter, &ctx, &cfg, &b.words, &b.groups, &xf, &lit, dim, opacity, atlas, &mut glow_work, (f.key, i, 0));
                for (k, d) in b.decos.iter().enumerate() {
                    let dxf = xf;
                    let a = opacity;
                    if d.pill {
                        let r = dxf.rect(d.rect);
                        let radius = (cfg.rem() * scale).min(255.0) as u8;
                        painter.rect(r, radius, theme::with_alpha(cfg.colors.foreground, 0.05 * a * fade), egui::Stroke::new(1.0, theme::with_alpha(cfg.colors.foreground, 0.1 * a * fade)), egui::StrokeKind::Inside);
                    }
                    match &d.body {
                        DecoBody::Timed { words, groups } => {
                            paint_words(&painter, &ctx, &cfg, words, groups, &dxf, &lit, dim, a, atlas, &mut glow_work, (f.key, i, k as u32 + 1));
                        }
                        DecoBody::Text(text, font) => {
                            let wrap = d.rect.width() + 1.0;
                            let g = ctx.fonts_mut(|fv| fv.layout(text.clone(), font.clone(), Color32::WHITE, wrap));
                            let pad = if d.pill { vec2(cfg.rem() * 0.75 + 1.0, cfg.rem() * 0.25 + 1.0) } else { Vec2::ZERO };
                            paint_galley(&painter, &g, d.rect.min + pad, &dxf, theme::with_alpha(cfg.colors.foreground, text_alpha * a), atlas);
                        }
                    }
                }
                animating |= lit.animating(b, t);
            }

            // Click a line to seek to it (the extension seeks to the line's time).
            let hit = block_rect.intersect(rect);
            if hit.is_positive() {
                let resp = ui.interact(hit, ui.scope_id().with(("lyr-line", f.key, i)), egui::Sense::click());
                if resp.clicked() && f.lyrics.sync != Sync::None {
                    seek = Some(f.lyrics.lines[li].start as f64 / 1000.0);
                }
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
            }
        }
        // Glows (drop-shadows of the lit text) were queued while painting; draw them under
        // the lit text they belong to by inserting at the recorded positions.
        self.glow.finish(&ctx, &painter, glow_work, atlas, ppp);
        fade_edges(&ctx, painter.layer_id(), first_shape, rect, cfg.size);
        if f.playing {
            animating = true;
        }
        Out { seek, animating, user_scrolling: user }
    }

    fn relayout(&mut self, ui: &mut egui::Ui, cfg: &Cfg, f: &Frame, key: u64, rect: Rect) {
        let width = rect.width() - cfg.pad.0 - cfg.pad.1;
        let lyrics = f.lyrics;
        let mut blocks: Vec<Block> = ui.ctx().fonts_mut(|fonts| {
            lyrics
                .lines
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    let agent = if l.instrumental { nearest_agent(lyrics, i) } else { l.agent.clone() };
                    let roman = f.roman.get(i).and_then(|r| r.as_ref()).map(|r| match r {
                        Roman::Timed(p, t) => Roman::Timed(p, t),
                        Roman::Text(t) => Roman::Text(t),
                    });
                    let trans = f.trans.get(i).copied().flatten();
                    build_block(fonts, cfg, l, i, width, roman, trans, agent.as_deref())
                })
                .collect()
        });
        if !lyrics.songwriters.is_empty() {
            let names = &lyrics.songwriters;
            let text = if names.len() > 1 { format!("Written by {} & {}", names[..names.len() - 1].join(", "), names[names.len() - 1]) } else { format!("Written by {}", names[0]) };
            let font = theme::regular((cfg.size * 0.4).max(12.0));
            let h = ui.ctx().fonts_mut(|fv| fv.layout(text.clone(), font.clone(), Color32::WHITE, width - cfg.size * 0.5).size().y);
            blocks.push(Block {
                line: None,
                start: 0.0,
                dur: 0.0,
                instrumental: false,
                align: if cfg.centered { Align::Center } else { Align::Left },
                top: 0.0,
                height: h + cfg.padding * 2.0,
                words: vec![],
                groups: vec![],
                decos: vec![],
                credits: Some((text, font)),
            });
        }
        let mut y = 0.0;
        for b in &mut blocks {
            b.top = y;
            y += b.height;
        }
        let view_h = rect.height();
        // `computeScrollPadding`
        let first_h = blocks.first().map_or(0.0, |b| b.height);
        let edge = if f.surface == Surface::Sidebar { (cfg.size * 1.6).min(view_h * 0.15).max(1.0) } else { 0.0 };
        let pad_top = (view_h * cfg.target_ratio - first_h / 2.0).max(edge);
        let last_centre = blocks.iter().rev().find(|b| b.line.is_some()).map(|b| b.top + b.height / 2.0);
        let last_h = blocks.iter().rev().find(|b| b.line.is_some()).map_or(0.0, |b| b.height);
        let content = y;
        let trailing = blocks.last().filter(|b| b.credits.is_some()).map_or(0.0, |b| b.height) + last_h / 2.0;
        let want = last_centre.map_or(view_h, |c| c + view_h * (1.0 - cfg.target_ratio));
        let pad_bottom = (want - content).max(view_h - content).max(view_h * (1.0 - cfg.target_ratio) - trailing).max(0.0);
        let richsync = lyrics.sync == Sync::Word;
        let same_song = self.layout.as_ref().is_some_and(|l| l.blocks.len() == blocks.len() && self.states.len() == blocks.len() && l.richsync == richsync && same_lines(l, &blocks));
        if same_song {
            // Decorations arrived (or the window resized): lines slide to their new places.
            let old = self.layout.as_ref().unwrap();
            let now = ui.input(|i| i.time);
            self.shifts.resize_with(blocks.len(), Vec::new);
            for (i, b) in blocks.iter().enumerate() {
                let delta = (old.pad_top + old.blocks[i].top) - (pad_top + b.top);
                if delta.abs() > 0.5 && (old.width - width).abs() < 1.0 {
                    self.shifts[i].push(Shift { start: now, delta });
                }
            }
        } else {
            let glow = std::mem::take(&mut self.glow);
            *self = Self { glow, shown_pass: self.shown_pass, ..Default::default() };
            self.states = vec![LineState::default(); blocks.len()];
            self.shifts = vec![Vec::new(); blocks.len()];
        }
        self.layout = Some(Layout { key, width, blocks, pad_top, content_h: pad_top + content + pad_bottom, richsync });
    }
}

fn same_lines(l: &Layout, blocks: &[Block]) -> bool {
    l.blocks.iter().zip(blocks).all(|(a, b)| a.line == b.line && a.start == b.start && a.words.len() == b.words.len())
}

/// `findNearestAgent`: an instrumental break takes the side of the singer nearest it.
fn nearest_agent(l: &Lyrics, i: usize) -> Option<String> {
    l.lines[i + 1..].iter().chain(l.lines[..i].iter().rev()).find(|x| !x.instrumental && x.agent.is_some()).and_then(|x| x.agent.clone())
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// How long a line waits before it moves in a scroll. The move itself takes the same
/// `--scroll-duration` whatever the distance; the cascade holds each line further from
/// the active one a little longer, up to four of them.
fn scroll_cascade(rel: isize) -> f64 {
    rel.unsigned_abs().min(4) as f64 * SCROLL_CASCADE
}

/// `transform-origin: left bottom`: a line grows from its baseline, so scaling it never
/// lifts the text out of the row it sits in.
fn transform_origin(block: Rect, align: Align, content_left: f32) -> Pos2 {
    match align {
        Align::Left => pos2(block.left(), block.bottom()),
        Align::Right => pos2(block.right(), block.bottom()),
        Align::Center => pos2(block.center().x.max(content_left), block.bottom()),
    }
}

/// The line's transform: content-relative positions → screen, scaled about `origin`.
#[derive(Clone, Copy)]
struct Xf {
    origin: Pos2,
    scale: f32,
    content: Pos2,
}

impl Xf {
    fn point(&self, p: Pos2) -> Pos2 {
        let s = self.content + p.to_vec2();
        self.origin + (s - self.origin) * self.scale
    }
    fn rect(&self, r: Rect) -> Rect {
        Rect::from_min_max(self.point(r.min), self.point(r.max))
    }
}

/// Lines fade out over the top and bottom edges of the view instead of being cut off
/// there. Geometry crossing an edge zone is sliced into thin horizontal bands, each
/// dimmed by its own height, so the fade is smooth and reaches zero exactly at the edge
/// (per-vertex alpha alone interpolates across a glyph and leaves a visible cut).
fn fade_edges(ctx: &egui::Context, layer: egui::LayerId, from: ShapeIdx, rect: Rect, size: f32) {
    let zone = (size * 1.6).min(rect.height() * 0.15).max(1.0);
    let edge = EdgeFade { rect, zone };
    ctx.graphics_mut(|g| {
        let list = g.entry(layer);
        for i in from.0..list.next_idx().0 {
            list.mutate_shape(ShapeIdx(i), |c| edge.shape(&mut c.shape));
        }
    });
}

struct EdgeFade {
    rect: Rect,
    zone: f32,
}

impl EdgeFade {
    fn k(&self, y: f32) -> f32 {
        let s = |d: f32| {
            let x = (d / self.zone).clamp(0.0, 1.0);
            x * x * (3.0 - 2.0 * x)
        };
        s(y - self.rect.top()) * s(self.rect.bottom() - y)
    }

    fn shape(&self, shape: &mut Shape) {
        match shape {
            Shape::Mesh(m) => {
                let (top, bottom) = (self.rect.top() + self.zone, self.rect.bottom() - self.zone);
                if m.vertices.iter().any(|v| v.pos.y < top || v.pos.y > bottom) {
                    *m = Arc::new(self.mesh(m));
                }
            }
            Shape::Rect(r) => {
                let f = self.k(r.rect.center().y);
                r.fill = r.fill.gamma_multiply(f);
                r.stroke.color = r.stroke.color.gamma_multiply(f);
            }
            Shape::Text(t) => t.opacity_factor *= self.k(t.pos.y + t.galley.size().y / 2.0),
            Shape::Vec(v) => v.iter_mut().for_each(|s| self.shape(s)),
            _ => {}
        }
    }

    fn mesh(&self, m: &Mesh) -> Mesh {
        use egui::epaint::Vertex;
        const BAND: f32 = 2.0;
        let (r, z) = (self.rect, self.zone);
        let mut cuts = Vec::new();
        for (a, b) in [(r.top(), r.top() + z), (r.bottom() - z, r.bottom())] {
            let n = ((b - a) / BAND).ceil().max(1.0) as usize;
            cuts.extend((0..=n).map(|i| a + (b - a) * i as f32 / n as f32));
        }
        let lerp = |a: &Vertex, b: &Vertex, t: f32| Vertex {
            pos: a.pos + (b.pos - a.pos) * t,
            uv: a.uv + (b.uv - a.uv) * t,
            color: a.color.lerp_to_gamma(b.color, t),
        };
        // The part of a polygon on one side of a horizontal line.
        let clip = |poly: &[Vertex], y: f32, keep_below: bool| -> Vec<Vertex> {
            let inside = |v: &Vertex| if keep_below { v.pos.y >= y } else { v.pos.y <= y };
            let mut out = Vec::with_capacity(poly.len() + 2);
            for i in 0..poly.len() {
                let (a, b) = (&poly[i], &poly[(i + 1) % poly.len()]);
                if inside(a) {
                    out.push(*a);
                }
                if inside(a) != inside(b) {
                    out.push(lerp(a, b, (y - a.pos.y) / (b.pos.y - a.pos.y)));
                }
            }
            out
        };
        let mut out = Mesh::with_texture(m.texture_id);
        for tri in m.indices.as_chunks::<3>().0 {
            let poly = [m.vertices[tri[0] as usize], m.vertices[tri[1] as usize], m.vertices[tri[2] as usize]];
            let lo = poly.iter().map(|v| v.pos.y).fold(f32::INFINITY, f32::min);
            let hi = poly.iter().map(|v| v.pos.y).fold(f32::NEG_INFINITY, f32::max);
            if hi <= r.top() || lo >= r.bottom() {
                continue;
            }
            let mut ys: Vec<f32> = cuts.iter().copied().filter(|&y| y > lo && y < hi).collect();
            ys.insert(0, lo.max(r.top()));
            ys.push(hi.min(r.bottom()));
            for w in ys.windows(2) {
                let piece = clip(&clip(&poly, w[0], true), w[1], false);
                if piece.len() < 3 {
                    continue;
                }
                let base = out.vertices.len() as u32;
                out.vertices.extend(piece.iter().map(|v| Vertex { color: v.color.gamma_multiply(self.k(v.pos.y)), ..*v }));
                for i in 1..piece.len() as u32 - 1 {
                    out.indices.extend([base, base + i, base + i + 1]);
                }
            }
        }
        out
    }
}

/// A laid-out galley drawn through a line transform, as glyph quads on the font texture.
fn paint_galley(painter: &egui::Painter, g: &Galley, at: Pos2, xf: &Xf, color: Color32, atlas: [usize; 2]) {
    let mut mesh = Mesh::with_texture(TextureId::default());
    let none: [f32; 0] = [];
    for row in &g.rows {
        for gl in &row.row.glyphs {
            let uv = gl.uv_rect;
            if uv.is_nothing() {
                continue;
            }
            let r = Rect::from_min_size(at + row.pos.to_vec2() + gl.pos.to_vec2() + uv.offset, uv.size);
            let uvr = Rect::from_min_max(
                pos2(uv.min[0] as f32 / atlas[0] as f32, uv.min[1] as f32 / atlas[1] as f32),
                pos2(uv.max[0] as f32 / atlas[0] as f32, uv.max[1] as f32 / atlas[1] as f32),
            );
            push_quad(&mut mesh, xf.rect(r), uvr, color, &|_| 1.0, &none);
        }
    }
    painter.add(Shape::mesh(mesh));
}

// ------------------------------------------------------------------ words

/// Where a line's highlight animations are this frame.
struct LitState {
    /// Line still running its animations (song time drives them).
    live: bool,
    /// Exit: wall time since it started, and the song time it happened at.
    exit: Option<(f32, f64)>,
    t: f64,
    fill: Fill,
}

impl LitState {
    fn new(cfg: &Cfg, st: LineState, t: f64, now: f64, live: bool) -> Self {
        Self { live: live && st.exit.is_none(), exit: st.exit.map(|e| ((now - e) as f32, st.exit_t)), t, fill: cfg.fill }
    }

    fn animating(&self, b: &Block, t: f64) -> bool {
        if self.exit.is_some() {
            return true;
        }
        // The pre-wipe leads, the wipe runs across the word, and the glyphs are still
        // breathing (and glowing) for their own span afterwards.
        self.live && b.words.iter().enumerate().any(|(wi, w)| {
            let from = if w.pre_dur > 0.0 { w.pre_start } else { w.start };
            let to = w.start + w.dur.max(0.0) + clock(&b.words, &b.groups, wi).plan().span + GLOW_TAIL;
            t >= from && t < to
        })
    }
}

/// What a word looks like this frame.
struct WordLook {
    /// Lit layer opacity.
    opacity: f32,
    /// The wipe's gradient, as fractions of the word: fully lit at `start`, gone at `end`.
    start: f32,
    end: f32,
    /// Glow radius in points (0 = none).
    glow: f32,
    /// Alpha of the glow this frame.
    glow_alpha: f32,
}

/// A word the wipe has run through: its gradient sits past the right edge.
const LIT: (f32, f32) = (1.0, 2.0);

/// `springProgress`: the extension's critically damped step response.
fn spring_progress(t: f64, response: f64) -> f64 {
    if t <= 0.0 {
        return 0.0;
    }
    let phase = std::f64::consts::TAU * t / response.max(0.001);
    1.0 - (1.0 + phase) * (-phase).exp()
}

/// `sampleSpring`: a mass on a spring, from rest to 1 — `w0` its angular frequency and
/// `zeta` the share of critical damping it carries. A hair under 1 arrives without a bob,
/// which is what the view's scroll and a word's rise both use.
fn spring_step(t: f64, w0: f64, zeta: f64) -> f64 {
    if t <= 0.0 {
        return 0.0;
    }
    let wd = w0 * (1.0 - zeta * zeta).max(1e-6).sqrt();
    1.0 - (-zeta * w0 * t).exp() * ((wd * t).cos() + (zeta * w0 / wd) * (wd * t).sin())
}

/// `@keyframes char-glow` over one glyph's span: 0, 0.8 at 15 %, 1 at 30 %, 0.3 at 60 %, 0.
fn glow_env(t: f64, span: f64) -> f32 {
    if t <= 0.0 || span <= 0.0 {
        return 0.0;
    }
    let p = (t / span).min(1.0) as f32;
    let at = |a: f32, b: f32, from: f32, to: f32| from + (to - from) * ((p - a) / (b - a));
    if p <= 0.15 {
        at(0.0, 0.15, 0.0, 0.8)
    } else if p <= 0.30 {
        at(0.15, 0.30, 0.8, 1.0)
    } else if p <= 0.60 {
        at(0.30, 0.60, 1.0, 0.3)
    } else {
        at(0.60, 1.0, 0.3, 0.0)
    }
}

/// am-lyrics' motion clock for one word: the whole of it where the script splits into
/// glyphs (`getCachedVirtualWordCharSpans`), each timed piece where it doesn't.
#[derive(Clone, Copy)]
struct Clock {
    start: f64,
    dur: f64,
    chars: usize,
    /// Where this piece's first glyph sits in the shared clock.
    first: usize,
    cjk: bool,
}

/// `updateCharacterMotion`'s schedule for one word.
#[derive(Clone, Copy)]
struct CharPlan {
    /// Between the starts of successive glyphs.
    delay: f64,
    /// Each glyph's own animation.
    span: f64,
    /// How long a glyph stays at the top of its fan.
    hold: f64,
    response: f64,
    /// How far a word fans and swells: nothing under a second, all of it over two.
    emphasis: f64,
    /// `--char-glow-max`: the shadow's own ceiling, which reaches it twice as fast as
    /// the fan does — nothing under a second, 0.45 by a second and a half.
    glow: f64,
}

impl Clock {
    fn plan(&self) -> CharPlan {
        let n = self.chars.max(1) as f64;
        let d = self.dur.max(0.001);
        let stagger = (d / n) * if self.cjk { 0.4 } else { 0.35 };
        let delay = if self.cjk { stagger.min(0.05).min(0.2 / n) } else { stagger.min(0.35) };
        let hold = 2.0 * d / n;
        let response = d.min(3.0);
        CharPlan {
            delay,
            span: (hold + response * 2.0).max(if self.cjk { 3.0 } else { 0.0 }),
            hold,
            response,
            // `getCharacterFrames`: `emphasis = cjk ? 0 : clamp(duration - 1, 0, 1)`; am-lyrics
            // has no length exception.
            emphasis: if self.cjk { 0.0 } else { (d - 1.0).clamp(0.0, 1.0) },
            glow: if self.cjk { 0.0 } else { GLOW_MAX * ((d - 1.0) / 0.5).clamp(0.0, 1.0) },
        }
    }
}

/// The clock one piece of a laid-out line moves to.
fn clock(words: &[Word], groups: &[Group], wi: usize) -> Clock {
    let glyphs = |w: &Word| w.text.chars().filter(|c| !c.is_whitespace()).count();
    let w = &words[wi];
    // CJK has no reliable word boundary, so each piece keeps its own clock.
    if w.cjk {
        return Clock { start: w.start, dur: w.dur, chars: glyphs(w), first: 0, cjk: true };
    }
    let gr = &groups[w.group];
    let run = &words[gr.words.clone()];
    let start = run.iter().map(|x| x.start).fold(f64::INFINITY, f64::min);
    let end = run.iter().map(|x| x.start + x.dur.max(0.0)).fold(f64::NEG_INFINITY, f64::max);
    Clock {
        start,
        dur: (end - start).max(0.001),
        chars: run.iter().map(glyphs).sum(),
        first: run[..wi - gr.first].iter().map(glyphs).sum(),
        cjk: false,
    }
}

/// Where one glyph of the word is this frame: (rise in em, sideways in em, scale).
fn char_pose(p: &CharPlan, i: usize, count: usize, t: f64, rise: f32) -> (f32, f32, f32) {
    let tl = t - (i as f64 + 1.0) * p.delay;
    if tl <= 0.0 {
        return (0.0, 0.0, 1.0);
    }
    // `getCharacterFrames`: the glyph rises on `sampleSpring(-3px, ..., response)`, and
    // rides the fan's worth of that same lift.
    let r = 0.92 * rise as f64 * spring_progress(tl, p.response.min(LETTER_RESPONSE_MAX));
    let e = p.emphasis * spring_progress(tl, p.response) * (1.0 - spring_progress(tl - p.hold, p.response));
    let mid = (count.saturating_sub(1)) as f64 * 0.5;
    ((r + FAN_LIFT_EM as f64 * e) as f32, ((i as f64 - mid) * FAN_SPREAD_EM as f64 * e) as f32, (1.0 + 0.1 * e) as f32)
}

/// Where one glyph of the word is this frame: (rise in em, sideways in em, scale). Every
/// glyph has a clock of its own, the way am-lyrics' `.character` spans do.
fn glyph_pose(_cjk: bool, plan: &CharPlan, i: usize, count: usize, t: f64, rise: f32) -> (f32, f32, f32) {
    char_pose(plan, i, count, t, rise)
}

fn word_look(w: &Word, lit: &LitState, cfg: &Cfg, c: &Clock) -> WordLook {
    let none = WordLook { opacity: 0.0, start: LIT.0, end: LIT.1, glow: 0.0, glow_alpha: 0.0 };
    if let Some((since, exit_t)) = lit.exit {
        // Words already sung fade back over 0.5 s; the rest were reset.
        return if exit_t < w.start {
            none
        } else {
            WordLook { opacity: 1.0 - EASE.at(since / FADE_OUT as f32), ..none }
        };
    }
    if !lit.live {
        return none;
    }
    let e = lit.t - w.start;
    // `resolveLineGlowSuppression`: a glow colour too faint to see isn't animated at all.
    let base = if cfg.glow_alpha < GLOW_INVISIBLE { 0.0 } else { cfg.glow_alpha };
    if lit.fill != Fill::Sweep || w.line_synced {
        // Word: a colour flip (`data-word-state`); Line and the line-synced fade: 0.33 s.
        let d = if lit.fill == Fill::Word { 0.18 } else { FADE_IN };
        let fade = (1.0 - (e / GLOW_TAIL).clamp(0.0, 1.0)) as f32;
        return WordLook { opacity: EASE.at(e.max(0.0) as f32 / d as f32), start: LIT.0, end: LIT.1, glow: GLOW_EM * w.font.size, glow_alpha: base * fade };
    }
    // `.char-motion[data-glow]::after` shadows with `var(--lyplus-text-primary)` at full
    // strength: the wipe's glow is the lit colour itself, not the preset's palette alpha,
    // so only the colour's own faintness can suppress it.
    let text_glow = if cfg.colors.glow.a() < 4 { 0.0 } else { 1.0 };
    // `wipe`: the edge runs straight across the word and its feather, over its duration.
    let feather = (w.feather * w.font.size) as f64;
    let wide = w.w.max(1.0) as f64;
    let s = if e < 0.0 {
        if w.pre_dur <= 0.0 {
            return none;
        }
        // `pre-wipe-universal`: the feather creeping in ahead of the timestamp.
        ((lit.t - w.pre_start) / w.pre_dur).clamp(0.0, 1.0) * feather
    } else {
        // `getLongWordWipeScale`: a long word's edge runs slower, so it takes the word's
        // extra length to cross rather than racing through it in the held first syllable.
        let chars = w.text.chars().filter(|c| !c.is_whitespace()).count() as f64;
        let scale = 1.0 + ((chars - 6.0) / 10.0).clamp(0.0, 1.0) * LONG_WIPE_RATIO;
        feather + (e / (w.dur * scale).max(0.001)).clamp(0.0, 1.0) * wide
    };
    // `char-glow`: every glyph of a held word breathes once, over its own span.
    let plan = c.plan();
    let mut breath = 0.0f32;
    if text_glow > 0.0 && plan.glow > 0.0 {
        let n = c.chars.max(1);
        for i in 0..n {
            breath += glow_env(lit.t - c.start - (i as f64 + 1.0) * plan.delay, plan.span);
        }
        breath /= n as f32;
    }
    WordLook {
        opacity: 1.0,
        start: ((s - feather) / wide) as f32,
        end: (s / wide) as f32,
        glow: GLOW_EM * w.font.size,
        glow_alpha: text_glow * (plan.glow as f32) * breath,
    }
}

/// One glyph of a word, positioned for this frame.
struct GlyphQuad {
    rect: Rect,
    uv: Rect,
}

/// A word's glyphs this frame, in content coordinates (before the line transform): each
/// glyph's `.char-motion` transform applied, so a `char-rise` word fans and swells as it
/// rises and every other word rises whole on its unsplit clock. Laid out once, at the
/// font's size, and scaled with the line: a second size would put a second copy of every
/// glyph in the shared font atlas, which only ever grows.
fn word_glyphs(ctx: &egui::Context, w: &Word, lit: &LitState, cfg: &Cfg, c: &Clock, atlas: [usize; 2]) -> Vec<GlyphQuad> {
    let g = ctx.fonts_mut(|fv| fv.layout_no_wrap(w.text.clone(), w.font.clone(), Color32::WHITE));
    let n: usize = g.rows.iter().map(|r| r.row.glyphs.len()).sum();
    let running = lit.live && lit.exit.is_none();
    let plan = c.plan();
    let e = lit.t - c.start;
    let em = w.font.size;
    let mut out = Vec::with_capacity(n);
    let mut k = 0;
    for row in &g.rows {
        for gl in &row.row.glyphs {
            let idx = k;
            k += 1;
            let uv = gl.uv_rect;
            if uv.is_nothing() {
                continue;
            }
            let mut r = Rect::from_min_size(w.pos + row.pos.to_vec2() + gl.pos.to_vec2() + uv.offset, uv.size);
            if running {
                let (lift, dx, s) = glyph_pose(w.cjk, &plan, c.first + idx, c.chars, e, cfg.rise);
                // `.char-motion`: scale about the glyph's own bottom centre, then rise.
                let o = pos2(r.center().x, r.bottom());
                r = Rect::from_min_max(o + (r.min - o) * s, o + (r.max - o) * s).translate(vec2(dx * em, -lift * em));
            }
            let uvr = Rect::from_min_max(
                pos2(uv.min[0] as f32 / atlas[0] as f32, uv.min[1] as f32 / atlas[1] as f32),
                pos2(uv.max[0] as f32 / atlas[0] as f32, uv.max[1] as f32 / atlas[1] as f32),
            );
            out.push(GlyphQuad { rect: r, uv: uvr });
        }
    }
    out
}

/// Draw a glyph quad, alpha following the lit gradient: 1 left of `a`, 0 right of `b`
/// (screen x), linear between, so the soft edge is exact and never a box.
fn push_quad(mesh: &mut Mesh, r: Rect, uv: Rect, color: Color32, alpha: &dyn Fn(f32) -> f32, cuts: &[f32]) {
    let mut xs = vec![r.left()];
    xs.extend(cuts.iter().copied().filter(|&c| c > r.left() && c < r.right()));
    xs.push(r.right());
    for s in xs.windows(2) {
        let (x0, x1) = (s[0], s[1]);
        if x1 - x0 <= 0.0 {
            continue;
        }
        let (a0, a1) = (alpha(x0), alpha(x1));
        if a0 <= 0.0 && a1 <= 0.0 {
            continue;
        }
        let u = |x: f32| uv.left() + (x - r.left()) / r.width().max(1e-6) * uv.width();
        let base = mesh.vertices.len() as u32;
        let c = |a: f32| theme::with_alpha(color, a);
        mesh.vertices.push(Vertex { pos: pos2(x0, r.top()), uv: pos2(u(x0), uv.top()), color: c(a0) });
        mesh.vertices.push(Vertex { pos: pos2(x1, r.top()), uv: pos2(u(x1), uv.top()), color: c(a1) });
        mesh.vertices.push(Vertex { pos: pos2(x1, r.bottom()), uv: pos2(u(x1), uv.bottom()), color: c(a1) });
        mesh.vertices.push(Vertex { pos: pos2(x0, r.bottom()), uv: pos2(u(x0), uv.bottom()), color: c(a0) });
        mesh.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

/// The lit gradient over the word box, in screen x: (alpha function, cut points).
/// The edge runs from `start` to `end` of the word's width, so the wipe's feather is
/// exact and never a box. `rtl`: the word reads right to left, so it's lit from the right.
fn lit_gradient(word: Rect, look: &WordLook, rtl: bool) -> (Box<dyn Fn(f32) -> f32>, Vec<f32>) {
    let (s, e) = (look.start, look.end);
    let w = word.width().max(1e-6);
    if rtl {
        // Lit at the right edge, gone one feather to its left.
        let xs = word.right() - word.width() * s;
        let xe = word.right() - word.width() * e - 1.0;
        let f = move |x: f32| if x >= xs { 1.0 } else if x <= xe { 0.0 } else { 1.0 - (xs - x) / (xs - xe) };
        (Box::new(f), vec![xe, xs])
    } else {
        let xs = word.left() + w * s;
        let xe = word.left() + w * e + 1.0;
        let f = move |x: f32| if x <= xs { 1.0 } else if x >= xe { 0.0 } else { 1.0 - (x - xs) / (xe - xs) };
        (Box::new(f), vec![xs, xe])
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_words(
    painter: &egui::Painter,
    ctx: &egui::Context,
    cfg: &Cfg,
    words: &[Word],
    groups: &[Group],
    xf: &Xf,
    lit: &LitState,
    dim: f32,
    opacity: f32,
    atlas: [usize; 2],
    glow: &mut Vec<GlowJob>,
    key: (u64, usize, u32),
) {
    let no_cut: [f32; 0] = [];
    let mut base = Mesh::with_texture(TextureId::default());
    let mut per_word = Vec::with_capacity(words.len());
    for (wi, w) in words.iter().enumerate() {
        // The clock this piece of the line moves to: shared with the rest of its word
        // when the script splits into glyphs, its own when it doesn't.
        let c = clock(words, groups, wi);
        let quads = word_glyphs(ctx, w, lit, cfg, &c, atlas);
        let dim_c = if w.bg { dim * 0.78 } else { dim } * opacity;
        for q in &quads {
            push_quad(&mut base, xf.rect(q.rect), q.uv, theme::with_alpha(cfg.colors.foreground, dim_c), &|_| 1.0, &no_cut);
        }
        per_word.push((quads, c));
    }
    // The dim run, then the highlight run over it (`.blyrics-highlight-run`, z-index 1):
    // each word's drop-shadow, then its lit text.
    painter.add(Shape::mesh(base));
    for (wi, (w, (quads, c))) in words.iter().zip(per_word).enumerate() {
        let look = word_look(w, lit, cfg, &c);
        if look.opacity <= 0.0 || look.end <= 0.0 {
            continue;
        }
        let word_box = xf.rect(Rect::from_min_size(w.pos, vec2(w.w, w.h)));
        let mut m = Mesh::with_texture(TextureId::default());
        let color = theme::with_alpha(cfg.colors.active, look.opacity * opacity);
        let mut screen_quads = Vec::with_capacity(quads.len());
        for q in &quads {
            let r = xf.rect(q.rect);
            let (alpha, cuts) = lit_gradient(word_box, &look, w.rtl);
            push_quad(&mut m, r, q.uv, color, alpha.as_ref(), &cuts);
            screen_quads.push((r, q.uv, 1.0));
        }
        if look.glow > 0.05 && look.glow_alpha >= GLOW_INVISIBLE && !dev_off("glow") {
            glow.push(GlowJob {
                key: (key.0, key.1, key.2, wi as u32),
                quads: screen_quads,
                word: word_box,
                rtl: w.rtl,
                radius: look.glow * xf.scale,
                pills: Vec::new(),
                color: theme::with_alpha(cfg.colors.glow, look.glow_alpha * look.opacity * opacity),
                look,
                layer: painter.add(Shape::Noop),
            });
        }
        if !m.is_empty() {
            painter.add(Shape::mesh(m));
        }
    }
}

// ------------------------------------------------------------------ glow

/// A word's drop-shadow to rasterize once this frame's glyphs are known.
pub struct GlowJob {
    key: (u64, usize, u32, u32),
    /// This frame's glyph quads: content box, atlas uv, and the alpha the gradient gives.
    quads: Vec<(Rect, Rect, f32)>,
    word: Rect,
    look: WordLook,
    rtl: bool,
    /// Blur radius in points (CSS: std deviation = radius / 2).
    radius: f32,
    /// Romanization pills under the text (box, corner radius, alpha): `rgba(255,255,255,.05)`
    /// with a 1px `rgba(255,255,255,.1)` border.
    pills: Vec<(Rect, f32, f32)>,
    color: Color32,
    /// Placeholder in the paint list, under the word's lit text.
    layer: ShapeIdx,
}

/// A GL texture sized in 64 px steps. A new image that fits is written into it in place
/// (`glTexSubImage2D`); a new texture or a new size makes the driver allocate, and at 144
/// frames a second it kept over 100 MB of those allocations (a new texture per blur step of
/// every line was the bulk of it).
struct Slot {
    tex: TextureHandle,
    size: [usize; 2],
    /// The part the current image covers.
    used: [usize; 2],
    /// Of the current image: an unchanged one (a glow that has stopped moving) isn't sent again.
    hash: u64,
}

const SLOT_STEP: usize = 64;
/// Unused slots kept for the next glow or line to write into.
const SPARE_SLOTS: usize = 8;

impl Slot {
    fn uv(&self) -> Rect {
        Rect::from_min_max(pos2(0.0, 0.0), pos2(self.used[0] as f32 / self.size[0] as f32, self.used[1] as f32 / self.size[1] as f32))
    }
}

/// A blurred line's radius in 1/4 px and scale in 1/1000.
type LineStep = (i32, i32, u64);

#[derive(Default)]
struct Glow {
    /// A copy of the font atlas, refreshed when glyphs we need aren't in it yet.
    atlas: Option<Arc<Coverage>>,
    textures: HashMap<(u64, usize, u32, u32), (Slot, f64)>,
    /// Per line: its blurred image, the radius (in 1/4 px) and scale (in 1/1000) it was
    /// drawn at, where it goes.
    lines: HashMap<(u64, usize), (Slot, LineStep, Rect, f64)>,
    spare: Vec<Slot>,
    scratch: Vec<f32>,
}

impl Glow {
    fn atlas(&mut self, ctx: &egui::Context, size: [usize; 2], need: &[Rect]) -> Arc<Coverage> {
        let stale = match &self.atlas {
            None => true,
            Some(a) => a.size != size || need.iter().any(|uv| glyph_blank(a, *uv)),
        };
        if stale {
            // Only the coverage (alpha) is kept: a quarter of the RGBA image egui hands out.
            let img = ctx.fonts(|f| f.image());
            self.atlas = Some(Arc::new(Coverage { size: img.size, alpha: img.pixels.iter().map(|p| p.a()).collect() }));
        }
        self.atlas.clone().unwrap()
    }

    /// Puts `img` in a texture: `old` if it fits, else a spare one that does, else a new one.
    fn upload(&mut self, ctx: &egui::Context, old: Option<Slot>, img: ColorImage, name: &str) -> Slot {
        let used = img.size;
        let image_hash = hash((used, &img.pixels));
        if let Some(s) = &old
            && s.used == used
            && s.hash == image_hash
        {
            return old.unwrap();
        }
        // The image plus a transparent column and row, so filtering at its edge reads what it
        // did on a texture of its own; whatever an earlier, bigger image left past them is
        // never sampled.
        let (w, h) = (used[0] + 1, used[1] + 1);
        let bucket = [w.next_multiple_of(SLOT_STEP), h.next_multiple_of(SLOT_STEP)];
        let fits = |s: &Slot| s.size[0] >= w && s.size[1] >= h && s.size[0] * s.size[1] <= 2 * bucket[0] * bucket[1];
        let slot = match old {
            Some(s) if fits(&s) => Some(s),
            Some(s) => {
                self.recycle(s);
                None
            }
            None => None,
        };
        let slot = slot.or_else(|| self.spare.iter().position(fits).map(|k| self.spare.swap_remove(k)));
        let size = slot.as_ref().map_or(bucket, |s| s.size);
        // A new texture gets its full size; an old one only the part being drawn.
        let (pw, ph) = if slot.is_some() { (w, h) } else { (size[0], size[1]) };
        let mut pixels = vec![Color32::TRANSPARENT; pw * ph];
        for (y, row) in img.pixels.chunks_exact(used[0]).enumerate() {
            pixels[y * pw..y * pw + used[0]].copy_from_slice(row);
        }
        let img = ColorImage { size: [pw, ph], source_size: vec2(pw as f32, ph as f32), pixels };
        match slot {
            Some(mut s) => {
                s.tex.set_partial([0, 0], img, TextureOptions::LINEAR);
                s.used = used;
                s.hash = image_hash;
                s
            }
            None => Slot { tex: ctx.load_texture(name, img, TextureOptions::LINEAR), size, used, hash: image_hash },
        }
    }

    fn recycle(&mut self, s: Slot) {
        if self.spare.len() >= SPARE_SLOTS {
            self.spare.remove(0);
        }
        self.spare.push(s);
    }

    fn finish(&mut self, ctx: &egui::Context, painter: &egui::Painter, jobs: Vec<GlowJob>, atlas_size: [usize; 2], ppp: f32) {
        let now = ctx.input(|i| i.time);
        if !jobs.is_empty() {
            let uvs: Vec<Rect> = jobs.iter().flat_map(|j| j.quads.iter().map(|q| q.1)).collect();
            let atlas = self.atlas(ctx, atlas_size, &uvs);
            for j in jobs {
                let Some((img, rect)) = rasterize_glow(&atlas, &j, ppp, &mut self.scratch) else { continue };
                let old = self.textures.remove(&j.key).map(|(s, _)| s);
                let slot = self.upload(ctx, old, img, "lyric-glow");
                let mut mesh = Mesh::with_texture(slot.tex.id());
                mesh.add_rect_with_uv(rect, slot.uv(), j.color);
                self.textures.insert(j.key, (slot, now));
                painter.set(j.layer, Shape::mesh(mesh));
            }
        }
        let gone: Vec<Slot> = (self.textures.extract_if(|_, (_, used)| now - *used >= 1.0).map(|(_, (s, _))| s))
            .chain(self.lines.extract_if(|_, (.., used)| now - *used >= 4.0).map(|(_, (s, ..))| s))
            .collect();
        for s in gone {
            self.recycle(s);
        }
    }

    /// A whole line blurred (`filter: blur()`): its words, each at its own alpha, its text
    /// decorations and pills, in content coordinates, rendered at the line's `scale` so the
    /// image isn't resampled on screen. Cached per line at its latest radius and scale.
    #[allow(clippy::too_many_arguments)]
    fn blurred_line(
        &mut self,
        ctx: &egui::Context,
        key: (u64, usize),
        radius: f32,
        words: &[(&Word, f32)],
        texts: &[(Arc<Galley>, Pos2, f32)],
        pills: &[(Rect, f32, f32)],
        scale: f32,
        atlas_size: [usize; 2],
        ppp: f32,
    ) -> Option<(TextureId, Rect, Rect)> {
        if dev_off("blur") {
            return None;
        }
        let now = ctx.input(|i| i.time);
        let weights: Vec<u8> = words.iter().map(|(_, a)| (a.clamp(0.0, 1.0) * 255.0).round() as u8)
            .chain(texts.iter().map(|(_, _, a)| (a.clamp(0.0, 1.0) * 255.0).round() as u8))
            .chain(pills.iter().map(|(_, _, a)| (a.clamp(0.0, 1.0) * 255.0).round() as u8))
            .collect();
        let step = ((radius * 4.0) as i32, (scale * 1000.0).round() as i32, hash(&weights));
        if let Some((s, r, rect, used)) = self.lines.get_mut(&key)
            && *r == step
        {
            *used = now;
            return Some((s.tex.id(), *rect, s.uv()));
        }
        let uv_of = |min: [u16; 2], max: [u16; 2]| {
            Rect::from_min_max(
                pos2(min[0] as f32 / atlas_size[0] as f32, min[1] as f32 / atlas_size[1] as f32),
                pos2(max[0] as f32 / atlas_size[0] as f32, max[1] as f32 / atlas_size[1] as f32),
            )
        };
        let mut quads = Vec::new();
        let push_galley = |g: &Galley, origin: Pos2, weight: f32, quads: &mut Vec<(Rect, Rect, f32)>| {
            for row in &g.rows {
                for gl in &row.row.glyphs {
                    let uv = gl.uv_rect;
                    if uv.is_nothing() {
                        continue;
                    }
                    let r = Rect::from_min_size(origin + row.pos.to_vec2() + gl.pos.to_vec2() + uv.offset, uv.size);
                    quads.push((r, uv_of(uv.min, uv.max), weight));
                }
            }
        };
        for (w, weight) in words {
            let g = ctx.fonts_mut(|fv| fv.layout_no_wrap(w.text.clone(), w.font.clone(), Color32::WHITE));
            push_galley(&g, w.pos, *weight, &mut quads);
        }
        for (g, at, weight) in texts {
            push_galley(g, *at, *weight, &mut quads);
        }
        if quads.is_empty() {
            return None;
        }
        let uvs: Vec<Rect> = quads.iter().map(|q| q.1).collect();
        let atlas = self.atlas(ctx, atlas_size, &uvs);
        let bounds = quads.iter().fold(Rect::NOTHING, |a, q| a.union(q.0));
        let job = GlowJob {
            key: (0, 0, 0, 0),
            quads,
            word: bounds,
            look: WordLook { opacity: 1.0, start: 2.0, end: 3.0, glow: 0.0, glow_alpha: 0.0 },
            rtl: false,
            // CSS `blur(r)` is a Gaussian with σ = r.
            radius: radius * 2.0,
            pills: pills.to_vec(),
            color: Color32::WHITE,
            layer: ShapeIdx(0),
        };
        let (img, content_rect) = rasterize_glow(&atlas, &job, ppp * scale, &mut self.scratch)?;
        let old = self.lines.remove(&key).map(|(s, ..)| s);
        let slot = self.upload(ctx, old, img, "lyric-blur");
        let out = (slot.tex.id(), content_rect, slot.uv());
        self.lines.insert(key, (slot, step, content_rect, now));
        Some(out)
    }
}

/// Whether a glyph's texels are all empty in our atlas copy (it was added since).
/// The font atlas's coverage, one byte per texel.
struct Coverage {
    size: [usize; 2],
    alpha: Vec<u8>,
}

fn glyph_blank(a: &Coverage, uv: Rect) -> bool {
    let (w, h) = (a.size[0] as f32, a.size[1] as f32);
    let (x0, y0, x1, y1) = ((uv.left() * w) as usize, (uv.top() * h) as usize, (uv.right() * w) as usize, (uv.bottom() * h) as usize);
    if x1 <= x0 || y1 <= y0 || x1 > a.size[0] || y1 > a.size[1] {
        return true;
    }
    for y in y0..y1 {
        for x in x0..x1 {
            if a.alpha[y * a.size[0] + x] > 0 {
                return false;
            }
        }
    }
    true
}

/// The lit glyph pixels (coverage × gradient) blurred: CSS `drop-shadow(0 0 r color)`
/// is a Gaussian with σ = r / 2 of the element's alpha, drawn under it. `ppp`: bitmap
/// pixels per unit of the quads' coordinates.
fn rasterize_glow(atlas: &Coverage, j: &GlowJob, ppp: f32, scratch: &mut Vec<f32>) -> Option<(ColorImage, Rect)> {
    let sigma = (j.radius * 0.5 * ppp).max(0.3);
    let pad = (sigma * 3.0).ceil() + 2.0;
    let bounds = j.quads.iter().map(|q| q.0).chain(j.pills.iter().map(|p| p.0)).fold(Rect::NOTHING, |a, r| a.union(r));
    if !bounds.is_positive() {
        return None;
    }
    let origin = bounds.min - Vec2::splat(pad / ppp);
    let w = (bounds.width() * ppp + pad * 2.0).ceil() as usize;
    let h = (bounds.height() * ppp + pad * 2.0).ceil() as usize;
    if w == 0 || h == 0 || w * h > 4_000_000 {
        return None;
    }
    scratch.clear();
    scratch.resize(w * h, 0.0);
    for &(r, radius, alpha) in &j.pills {
        paint_pill(scratch, w, h, (r.min - origin) * ppp, r.size() * ppp, radius * ppp, ppp, alpha);
    }
    let (aw, ah) = (atlas.size[0] as f32, atlas.size[1] as f32);
    for &(r, uv, weight) in &j.quads {
        let (alpha, _) = lit_gradient(j.word, &j.look, j.rtl);
        let tx0 = (uv.left() * aw).round() as isize;
        let ty0 = (uv.top() * ah).round() as isize;
        let tw = ((uv.right() - uv.left()) * aw).round() as isize;
        let th = ((uv.bottom() - uv.top()) * ah).round() as isize;
        if tw <= 0 || th <= 0 {
            continue;
        }
        // Destination box in bitmap pixels (glyphs may be scaled a little by the line).
        let dx0 = (r.left() - origin.x) * ppp;
        let dy0 = (r.top() - origin.y) * ppp;
        let dw = r.width() * ppp;
        let dh = r.height() * ppp;
        let (ix0, iy0) = (dx0.floor().max(0.0) as usize, dy0.floor().max(0.0) as usize);
        let (ix1, iy1) = (((dx0 + dw).ceil() as usize).min(w), ((dy0 + dh).ceil() as usize).min(h));
        // Bilinear, with nothing outside the glyph's own texels: at another scale than the
        // atlas's, nearest-texel picks drop or double rows and the edges go jagged.
        let texel = |x: isize, y: isize| -> f32 {
            if x < 0 || y < 0 || x >= tw || y >= th {
                return 0.0;
            }
            atlas.alpha.get((ty0 + y) as usize * atlas.size[0] + (tx0 + x) as usize).map_or(0.0, |&p| p as f32 / 255.0)
        };
        for y in iy0..iy1 {
            let fy = (y as f32 + 0.5 - dy0) / dh * th as f32 - 0.5;
            let (sy, ky) = (fy.floor() as isize, fy - fy.floor());
            for x in ix0..ix1 {
                let fx = (x as f32 + 0.5 - dx0) / dw * tw as f32 - 0.5;
                let (sx, kx) = (fx.floor() as isize, fx - fx.floor());
                let top = texel(sx, sy) * (1.0 - kx) + texel(sx + 1, sy) * kx;
                let bottom = texel(sx, sy + 1) * (1.0 - kx) + texel(sx + 1, sy + 1) * kx;
                let cov = top * (1.0 - ky) + bottom * ky;
                if cov <= 0.0 {
                    continue;
                }
                let screen_x = origin.x + (x as f32 + 0.5) / ppp;
                let a = cov * alpha(screen_x) * weight;
                let d = &mut scratch[y * w + x];
                *d = (*d + a - *d * a).min(1.0);
            }
        }
    }
    gaussian(scratch, w, h, sigma);
    let pixels = scratch
        .iter()
        .map(|v| {
            let a = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            Color32::from_rgba_premultiplied(a, a, a, a)
        })
        .collect();
    let img = ColorImage { size: [w, h], source_size: vec2(w as f32, h as f32), pixels };
    Some((img, Rect::from_min_size(origin, vec2(w as f32, h as f32) / ppp)))
}

/// Gaussian blur: a true kernel for small σ (Skia's cut-off is 2), else three box passes
/// per axis (what browsers use for large blurs). Boxes for a σ under 1 come out as one
/// 3-tap box, which looks blocky instead of soft.
fn gaussian(buf: &mut [f32], w: usize, h: usize, sigma: f32) {
    if sigma < 2.0 {
        let r = (sigma * 3.0).ceil() as usize;
        let mut k: Vec<f32> = (0..=2 * r).map(|i| (-(i as f32 - r as f32).powi(2) / (2.0 * sigma * sigma)).exp()).collect();
        let sum: f32 = k.iter().sum();
        k.iter_mut().for_each(|v| *v /= sum);
        let mut tmp = vec![0.0f32; buf.len()];
        convolve(buf, &mut tmp, w, h, &k, true);
        convolve(&tmp, buf, w, h, &k, false);
        return;
    }
    // Box sizes for n = 3 (W3C filter effects / "boxes for gauss").
    let n = 3.0f32;
    let ideal = (12.0 * sigma * sigma / n + 1.0).sqrt();
    let mut wl = ideal.floor() as i32;
    if wl % 2 == 0 {
        wl -= 1;
    }
    let wu = wl + 2;
    let m_ideal = (12.0 * sigma * sigma - n * (wl * wl) as f32 - 4.0 * n * wl as f32 - 3.0 * n) / (-4.0 * wl as f32 - 4.0);
    let m = m_ideal.round() as i32;
    let mut tmp = vec![0.0f32; buf.len()];
    for i in 0..3 {
        let size = if i < m { wl } else { wu };
        let r = ((size - 1) / 2).max(0) as usize;
        if r == 0 {
            continue;
        }
        box_pass(buf, &mut tmp, w, h, r, true);
        box_pass(&tmp, buf, w, h, r, false);
    }
}

/// One axis of a separable kernel (odd length, centred), zero past the edges.
fn convolve(src: &[f32], dst: &mut [f32], w: usize, h: usize, k: &[f32], horizontal: bool) {
    let r = (k.len() / 2) as isize;
    let (n, lines) = if horizontal { (w, h) } else { (h, w) };
    let at = |line: usize, i: usize| if horizontal { line * w + i } else { i * w + line };
    for line in 0..lines {
        for i in 0..n {
            let mut acc = 0.0;
            for (j, kv) in k.iter().enumerate() {
                let s = i as isize + j as isize - r;
                if s >= 0 && (s as usize) < n {
                    acc += src[at(line, s as usize)] * kv;
                }
            }
            dst[at(line, i)] = acc;
        }
    }
}

/// A romanization pill into an alpha bitmap, anti-aliased like egui's rect: the 5% fill
/// with its 1-point 10% border inside (`min`/`size`/`radius` in bitmap pixels).
fn paint_pill(buf: &mut [f32], w: usize, h: usize, min: Vec2, size: Vec2, radius: f32, ppp: f32, alpha: f32) {
    if alpha <= 0.0 {
        return;
    }
    let radius = radius.min(size.x * 0.5).min(size.y * 0.5);
    let half = size * 0.5;
    let centre = min + half;
    let (x0, y0) = (min.x.floor().max(0.0) as usize, min.y.floor().max(0.0) as usize);
    let (x1, y1) = (((min.x + size.x).ceil() as usize).min(w), ((min.y + size.y).ceil() as usize).min(h));
    let over = |d: &mut f32, a: f32| *d = (*d + a - *d * a).min(1.0);
    for y in y0..y1 {
        for x in x0..x1 {
            // Signed distance to the rounded box, negative inside.
            let p = vec2(x as f32 + 0.5, y as f32 + 0.5) - centre;
            let q = vec2(p.x.abs(), p.y.abs()) - half + Vec2::splat(radius);
            let d = vec2(q.x.max(0.0), q.y.max(0.0)).length() + q.x.max(q.y).min(0.0) - radius;
            let inside = (0.5 - d).clamp(0.0, 1.0);
            let border = inside - (0.5 - (d + ppp)).clamp(0.0, 1.0);
            let px = &mut buf[y * w + x];
            over(px, 0.05 * alpha * inside);
            over(px, 0.1 * alpha * border);
        }
    }
}

fn box_pass(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize, horizontal: bool) {
    let (n, lines) = if horizontal { (w, h) } else { (h, w) };
    let idx = |line: usize, i: usize| if horizontal { line * w + i } else { i * w + line };
    let norm = 1.0 / (2 * r + 1) as f32;
    for line in 0..lines {
        let mut acc = 0.0;
        for i in 0..=r.min(n - 1) {
            acc += src[idx(line, i)];
        }
        for i in 0..n {
            dst[idx(line, i)] = acc * norm;
            if i + r + 1 < n {
                acc += src[idx(line, i + r + 1)];
            }
            if i >= r {
                acc -= src[idx(line, i - r)];
            }
        }
    }
}

// ------------------------------------------------------------------ instrumental

/// Bits of a break, its beads' alpha at rest, and its beats.
const GAP_DOTS: usize = 3;
const GAP_DOT_REST: f32 = 0.55;
const GAP_REST_SCALE: f32 = 0.76;
const GAP_BREATH: f32 = 0.20;
const GAP_PUFF: f32 = 0.35;
/// The break's shape in seconds: the beads settle in, fill one per beat, and leave.
const GAP_ENTER: f64 = 0.620;
const GAP_EXIT: f64 = 0.350;
const GAP_SPLIT: f32 = 0.35;
const GAP_COLLAPSE: f64 = 0.500;

/// Where a break's gesture sits in its own span. The exit is cut out of the end of the
/// silence and runs backwards from the next line, and the beads spend everything ahead
/// of it: `(exit start, exit length, one bead's beat)`.
fn gap_times(start: f64, dur: f64) -> (f64, f64, f64) {
    let until = start + dur.max(0.001);
    let exit_start = start + GAP_ENTER.max(until - start - GAP_COLLAPSE - GAP_EXIT);
    let exit_len = (until.min(exit_start + GAP_EXIT) - exit_start).max(0.001);
    let step = ((exit_start - start) / GAP_DOTS as f64).max(0.001);
    (exit_start, exit_len, step)
}

/// An intermission: three beads that light one after another as the silence runs out,
/// breathe once per bead, then puff out and vanish before the next line. `true` while
/// it is moving.
fn paint_instrumental(painter: &egui::Painter, cfg: &Cfg, b: &Block, xf: &Xf, t: f64, content_w: f32) -> bool {
    let em = cfg.size;
    // The beads' proportions are the BitChord player's: 13 dp across with 5 dp between
    // them at a 34 sp lyric size.
    let dot = em * 13.0 / 34.0;
    let gap = em * 5.0 / 34.0;
    let total = dot * GAP_DOTS as f32 + gap * (GAP_DOTS - 1) as f32;
    let x0 = match b.align {
        Align::Left => 0.0,
        Align::Right => content_w - total,
        Align::Center => (content_w - total) * 0.5,
    };
    let cy = cfg.padding + em * 1.333 * 0.5;
    let mid = x0 + total * 0.5;
    let into = t - b.start;
    let (exit_start, exit_len, step) = gap_times(b.start, b.dur);
    let until = b.start + b.dur.max(0.001);
    // Opening: the row settles from just under its size rather than appearing.
    let swell = EASE.at(((into / GAP_ENTER) as f32).clamp(0.0, 1.0));
    // Closing: a push over the first [GAP_SPLIT], then a pull into the point the beads
    // grew from and out to nothing.
    let e = ((t - exit_start) / exit_len).clamp(0.0, 1.0) as f32;
    let pull = e > GAP_SPLIT;
    let half = if pull { (e - GAP_SPLIT) / (1.0 - GAP_SPLIT) } else { e / GAP_SPLIT };
    let bend = half * half * (3.0 - 2.0 * half);
    let grow = if e <= 0.0 {
        GAP_REST_SCALE + (1.0 - GAP_REST_SCALE) * swell
    } else if !pull {
        1.0 + GAP_PUFF * bend
    } else {
        (1.0 + GAP_PUFF) * (1.0 - bend)
    };
    let alpha = if e <= 0.0 { swell } else if !pull { 1.0 } else { 1.0 - bend };
    if alpha <= 0.001 || grow <= 0.001 {
        return false;
    }
    // One breath to each bead it lights, all three on the same phase; once the third is
    // lit the row holds still and waits for the exit. Raised cosine, so every turn round
    // is the same movement as the last.
    let beat = if into < step * GAP_DOTS as f64 { ((into % step) / step) as f32 } else { 1.0 };
    let pulse = 1.0 - GAP_BREATH * 0.5 * (1.0 - (std::f32::consts::TAU * beat).cos());
    for i in 0..GAP_DOTS {
        // Beads and the space between them scale together about the middle of the row,
        // which is what makes the outer dots drift apart as they fill.
        let x = x0 + dot * 0.5 + i as f32 * (dot + gap);
        let cx = mid + (x - mid) * grow * pulse;
        let c = xf.point(pos2(cx, cy));
        let r = dot * 0.5 * grow * pulse * xf.scale;
        // Brightness alone is per bead: the count has to read as three arrivals, and
        // only one of them can be happening at a time.
        let k = ((t - b.start - i as f64 * step) / step).clamp(0.0, 1.0) as f32;
        let lit = k * k * (3.0 - 2.0 * k);
        painter.circle_filled(c, r, theme::with_alpha(cfg.colors.foreground, GAP_DOT_REST * alpha));
        if lit > 0.0 {
            painter.circle_filled(c, r, theme::with_alpha(cfg.colors.active, lit * alpha));
        }
    }
    (into >= 0.0 && into <= GAP_ENTER) || t < b.start + step * GAP_DOTS as f64 || (t >= exit_start && t < until)
}

/// Dev hook: YTM_LYR_OFF=glow,blur,view leaves those parts out (memory breakdowns).
pub fn dev_off(what: &str) -> bool {
    static OFF: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    OFF.get_or_init(|| std::env::var("YTM_LYR_OFF").unwrap_or_default()).split(',').any(|x| x == what)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_sidebar_uses_contrasting_lyric_colors() {
        let text = Color32::from_rgb(22, 20, 26);
        let background = Color32::from_rgb(246, 244, 248);
        let custom = Color32::from_rgb(90, 20, 32);
        let custom_glow = Color32::from_rgb(255, 106, 0);
        let colors = |surface, light, active, glow| LyricColors::new(surface, light, text, background, active, glow);

        let sidebar = colors(Surface::Sidebar, true, Color32::WHITE, Color32::WHITE);
        assert_eq!((sidebar.foreground, sidebar.active, sidebar.glow), (text, text, Color32::WHITE));
        assert_eq!(sidebar.dim_alpha(0.0), 0.0);
        assert_eq!(sidebar.dim_alpha(1.0), 1.0);
        assert!((sidebar.dim_alpha(0.3) - 0.51).abs() < 1e-6);
        let dim = theme::lerp_color(background, text, sidebar.dim_alpha(0.3));
        assert!(contrast_ratio(dim, background) >= 3.0);

        let preserved = colors(Surface::Sidebar, true, custom, custom_glow);
        assert_eq!((preserved.active, preserved.glow), (custom, custom_glow));
        for bright in [Color32::from_rgb(250, 88, 106), Color32::from_gray(222)] {
            let adjusted = colors(Surface::Sidebar, true, bright, custom_glow);
            assert_ne!(adjusted.active, bright);
            assert!(contrast_ratio(adjusted.active, background) >= 6.0);
            assert_eq!(adjusted.glow, custom_glow);
        }
        for (surface, light) in [(Surface::Player, false), (Surface::Player, true), (Surface::Sidebar, false)] {
            let unchanged = colors(surface, light, Color32::WHITE, custom_glow);
            assert_eq!((unchanged.foreground, unchanged.active, unchanged.glow), (Color32::WHITE, Color32::WHITE, custom_glow));
            assert_eq!(unchanged.dim_alpha(0.3), 0.3);
        }
    }

    fn draw_test_view(ctx: &egui::Context, view: &mut View, lyrics: &Lyrics, style: &LyricStyle, surface: Surface, t: f64, now: f64, height: f32) {
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(350.0, height));
        // The glow textures the draw allocates get no painter here, so the pass's delta
        // is dropped on purpose.
        ctx.run_ui(Default::default(), |ui| {
            let key = if surface == Surface::Player { 1 } else { 2 };
            view.show(ui, Frame { lyrics, key, roman: vec![], trans: vec![], t, playing: false, now, rect, style, accent: Color32::WHITE, centered: false, surface, scale: 1.0 });
        })
        .textures_delta
        .clear();
    }

    #[test]
    fn sidebar_anchor_and_scroll_state_are_independent() {
        let ctx = shot_ctx();
        let mut style = LyricStyle::default();
        style.blur = 0.0;
        for sync in [Sync::Line, Sync::Word] {
            let lyrics = Lyrics {
                lines: (0..12).map(|i| {
                    let start = 1000 + i * 5000;
                    Line {
                        start,
                        dur: 4000,
                        text: "hello world".into(),
                        parts: if sync == Sync::Word { vec![Part { start, dur: 4000, text: "hello world".into(), bg: false }] } else { vec![] },
                        ..Default::default()
                    }
                }).collect(),
                sync,
                provider: crate::lyrics::Provider::LrclibSynced,
                language: None,
                songwriters: vec![],
            };
            let (mut player, mut sidebar) = (View::default(), View::default());
            draw_test_view(&ctx, &mut player, &lyrics, &style, Surface::Player, 0.0, 100.0, 700.0);
            draw_test_view(&ctx, &mut sidebar, &lyrics, &style, Surface::Sidebar, 0.0, 100.0, 700.0);
            let center = |v: &View, i: usize| {
                let l = v.layout.as_ref().unwrap();
                l.pad_top + l.blocks[i].top + l.blocks[i].height / 2.0 - v.scroll_top
            };
            let check_sidebar = |v: &View, i: usize, height: f32| {
                let l = v.layout.as_ref().unwrap();
                let top = l.pad_top + l.blocks[i].top - v.scroll_top;
                let size = l.blocks[i].words[0].font.size;
                let fade = (size * 1.6).min(height * 0.15).max(1.0);
                assert!(top >= fade - 0.1 && top <= (height * 0.12).max(fade) + 0.1);
            };
            assert!((center(&player, 0) - 700.0 * style.anchor).abs() < 0.1);
            check_sidebar(&sidebar, 0, 700.0);
            assert_ne!(sidebar.shown_pass, 0, "first relayout must retain the draw pass");
            let mut short = View::default();
            draw_test_view(&ctx, &mut short, &lyrics, &style, Surface::Sidebar, 0.0, 100.0, 350.0);
            let size = short.layout.as_ref().unwrap().blocks[0].words[0].font.size;
            let fade = (size * 1.6).min(350.0 * 0.15).max(1.0);
            assert!(center(&short, 0) >= fade + size * 0.5 - 0.1);

            draw_test_view(&ctx, &mut player, &lyrics, &style, Surface::Player, 16.0, 100.1, 700.0);
            draw_test_view(&ctx, &mut sidebar, &lyrics, &style, Surface::Sidebar, 16.0, 100.1, 700.0);
            assert!(!player.scrolls.is_empty() && !sidebar.scrolls.is_empty());
            check_sidebar(&sidebar, 3, 700.0);
            let player_scroll = player.scroll_top;
            sidebar.user_until = 200.0;
            sidebar.was_user = true;
            sidebar.scroll_top = 0.0;
            draw_test_view(&ctx, &mut sidebar, &lyrics, &style, Surface::Sidebar, 16.0, 100.2, 700.0);
            assert_eq!(sidebar.scroll_top, 0.0);
            assert_eq!(player.scroll_top, player_scroll);
            sidebar.resume();
            draw_test_view(&ctx, &mut sidebar, &lyrics, &style, Surface::Sidebar, 16.0, 100.3, 700.0);
            check_sidebar(&sidebar, 3, 700.0);

            let old_key = sidebar.layout.as_ref().unwrap().key;
            draw_test_view(&ctx, &mut sidebar, &lyrics, &style, Surface::Sidebar, 16.0, 100.4, 900.0);
            assert_ne!(sidebar.layout.as_ref().unwrap().key, old_key);
            check_sidebar(&sidebar, 3, 900.0);
            // A backward seek follows the same scroll animation, without moving the player view.
            draw_test_view(&ctx, &mut sidebar, &lyrics, &style, Surface::Sidebar, 6.0, 100.5, 900.0);
            check_sidebar(&sidebar, 1, 900.0);
            assert_eq!(player.scroll_top, player_scroll);
            sidebar.reset();
            assert!(sidebar.layout.is_none() && player.layout.is_some());
            draw_test_view(&ctx, &mut sidebar, &lyrics, &style, Surface::Sidebar, 0.0, 101.0, 700.0);
            check_sidebar(&sidebar, 0, 700.0);
        }
    }

    #[test]
    fn anchor_changes_invalidate_layout_and_hidden_textures_are_released() {
        let ctx = shot_ctx();
        let lyrics = Lyrics {
            lines: vec![Line { start: 1000, dur: 4000, text: "hello".into(), ..Default::default() }],
            sync: Sync::Line,
            provider: crate::lyrics::Provider::LrclibSynced,
            language: None,
            songwriters: vec![],
        };
        let mut style = LyricStyle::default();
        style.blur = 0.0;
        let mut view = View::default();
        draw_test_view(&ctx, &mut view, &lyrics, &style, Surface::Player, 0.0, 100.0, 700.0);
        let old_key = view.layout.as_ref().unwrap().key;
        let old_pad = view.layout.as_ref().unwrap().pad_top;
        style.anchor = 0.2;
        draw_test_view(&ctx, &mut view, &lyrics, &style, Surface::Player, 0.0, 100.1, 700.0);
        assert_ne!(view.layout.as_ref().unwrap().key, old_key);
        assert!(view.layout.as_ref().unwrap().pad_top < old_pad);

        let slot = view.glow.upload(&ctx, None, ColorImage::filled([10, 10], Color32::WHITE), "test-hidden");
        view.glow.spare.push(slot);
        view.shown_pass = 10;
        view.reset();
        assert_eq!(view.shown_pass, 10);
        view.release_hidden(11);
        assert_eq!(view.glow.spare.len(), 1);
        view.release_hidden(12);
        assert!(view.glow.spare.is_empty());
        assert_eq!(view.shown_pass, 0);
    }

    #[test]
    fn slots_are_rewritten_in_place() {
        let ctx = egui::Context::default();
        let img = |w: usize, h: usize| ColorImage::filled([w, h], Color32::WHITE);
        let mut g = Glow::default();
        let a = g.upload(&ctx, None, img(100, 40), "t");
        assert_eq!(a.size, [128, 64]);
        let id = a.tex.id();
        // Smaller, and a little bigger within the slot: the same texture, drawn in part.
        let b = g.upload(&ctx, Some(a), img(60, 30), "t");
        assert_eq!((b.tex.id(), b.used), (id, [60, 30]));
        assert_eq!(b.uv().max, pos2(60.0 / 128.0, 30.0 / 64.0));
        let c = g.upload(&ctx, Some(b), img(120, 60), "t");
        assert_eq!(c.tex.id(), id);
        // Outgrown: a new texture; the old one is kept for the next image that fits it.
        let d = g.upload(&ctx, Some(c), img(300, 40), "t");
        assert_ne!(d.tex.id(), id);
        assert_eq!(g.spare.len(), 1);
        let e = g.upload(&ctx, None, img(90, 50), "t");
        assert_eq!(e.tex.id(), id);
        // Far too big for a small image: not reused.
        let f = g.upload(&ctx, Some(d), img(10, 10), "t");
        assert_eq!(f.size, [64, 64]);
    }

    #[test]
    fn bezier_matches_css_ease() {
        // Reference values of CSS `ease` (cubic-bezier(0.25, 0.1, 0.25, 1)).
        assert!((EASE.at(0.5) - 0.8024).abs() < 0.002);
        assert!((EASE.at(0.25) - 0.4094).abs() < 0.002);
        assert!((EASE_IN.at(0.5) - 0.3153).abs() < 0.002);
        assert_eq!(EASE.at(0.0), 0.0);
        assert_eq!(EASE.at(1.0), 1.0);
    }

    #[test]
    fn parts_split_on_spaces_with_prorated_time() {
        let p = Part { start: 1000, dur: 1000, text: "ab cd".into(), bg: false };
        let t = split_part(&p);
        assert_eq!(t.len(), 3);
        assert!((t[0].start - 1.0).abs() < 1e-9 && (t[0].dur - 0.5).abs() < 1e-9);
        assert!(t[1].space);
        assert!((t[2].start - 1.5).abs() < 1e-9);
    }

    #[test]
    fn rtl_lines_run_from_the_right() {
        let mut fonts = egui::epaint::text::Fonts::new(Default::default(), egui::FontDefinitions::default());
        let mut fv = fonts.with_pixels_per_point(1.0);
        let font = FontId::proportional(20.0);
        let line = |text: &str| line_synced_parts(&Line { start: 0, dur: 1000, text: text.into(), ..Default::default() }).iter().flat_map(split_part).collect::<Vec<_>>();
        let x = |f: &Flow, word: &str| f.words.iter().find(|w| w.text == word).unwrap().pos.x;
        // Hebrew reads from the right; the English inside it keeps its order.
        let f = flow(&mut fv, line("שלום עולם hello world"), &font, 1000.0, 27.0, Align::Right, true);
        assert!(x(&f, "שלום") > x(&f, "עולם"));
        assert!(x(&f, "עולם") > x(&f, "world"));
        assert!(x(&f, "world") > x(&f, "hello"));
        // An English line with a Hebrew phrase: only the phrase turns around.
        let f = flow(&mut fv, line("my song שלום עולם remix"), &font, 1000.0, 27.0, Align::Left, false);
        assert!(x(&f, "song") < x(&f, "עולם") && x(&f, "עולם") < x(&f, "שלום") && x(&f, "שלום") < x(&f, "remix"));
        // A long line wraps instead of running past the edge.
        let f = flow(&mut fv, line(&"word ".repeat(40)), &font, 300.0, 27.0, Align::Left, false);
        assert!(f.height > 27.0 && f.words.iter().all(|w| w.pos.x + w.w <= 300.0 + 0.5));
    }

    /// Where each word of a two-word RTL phrase lands, laid out as before and as now, with
    /// the app's fonts: the first word belongs on the right (the old layout put it left).
    /// Needs the system fonts the app falls back to for Hebrew and Arabic.
    /// `cargo test --release -- --ignored rtl_word_positions --nocapture`
    #[test]
    #[ignore]
    fn rtl_word_positions() {
        let ctx = shot_ctx();
        let font = crate::theme::semibold(26.0);
        for (a, b) in [("אבג", "דהו"), ("عمر", "دياب")] {
            let text = format!("{a} {b}");
            let mean = |g: &Galley, w: &str| {
                let xs: Vec<f32> = g.rows.iter().flat_map(|r| r.row.glyphs.iter()).filter(|gl| w.contains(gl.chr)).map(|gl| gl.pos.x).collect();
                xs.iter().sum::<f32>() / xs.len().max(1) as f32
            };
            let (before, after) = ctx.fonts_mut(|f| (f.layout_no_wrap(text.clone(), font.clone(), Color32::WHITE), f.layout_job(crate::bidi::job(&text, font.clone(), Color32::WHITE))));
            println!("{text}: before first word x {:.0}, second {:.0} | after first {:.0}, second {:.0}", mean(&before, a), mean(&before, b), mean(&after, a), mean(&after, b));
            assert!(mean(&after, a) > mean(&after, b), "{text}: the first word should be on the right");
        }
    }

    /// The app's fonts in a headless egui, for the pictures below.
    fn shot_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx, "Inter");
        for _ in 0..2 {
            // The first pass uploads the font atlas. Nothing draws here, so there's no
            // painter to hand the delta to; epaint only complains about the drop in debug.
            ctx.run_ui(Default::default(), |_| {}).textures_delta.clear();
        }
        ctx
    }

    /// Draw a galley's glyphs from the font atlas into `img`, only between `clip` (y).
    fn blit(img: &mut image::RgbaImage, atlas: &ColorImage, at: Pos2, g: &Galley, rgb: [u8; 3], clip: (f32, f32)) {
        let (w, h) = (img.width() as i64, img.height() as i64);
        for row in &g.rows {
            for gl in &row.row.glyphs {
                let uv = gl.uv_rect;
                if uv.is_nothing() {
                    continue;
                }
                let o = at + row.pos.to_vec2() + gl.pos.to_vec2() + uv.offset;
                for (dy, ty) in (uv.min[1]..uv.max[1]).enumerate() {
                    for (dx, tx) in (uv.min[0]..uv.max[0]).enumerate() {
                        let a = atlas.pixels[ty as usize * atlas.size[0] + tx as usize].a() as f32 / 255.0;
                        let (x, y) = ((o.x + dx as f32).round() as i64, (o.y + dy as f32).round() as i64);
                        if a <= 0.0 || x < 0 || y < 0 || x >= w || y >= h || (y as f32) < clip.0 || (y as f32) >= clip.1 {
                            continue;
                        }
                        let p = img.get_pixel_mut(x as u32, y as u32);
                        for c in 0..3 {
                            p[c] = (p[c] as f32 * (1.0 - a) + rgb[c] as f32 * a) as u8;
                        }
                    }
                }
            }
        }
    }

    /// Before/after picture of a lyric line taller than the view, while its 6th row is
    /// sung: the old view stayed at the line's top; the new one pages down with it. The
    /// view positions are the scroll code's (the line's top, then the paging rule).
    /// `YTM_SHOT=<file.png> cargo test --release -- --ignored paging_before_after_png`
    #[test]
    #[ignore]
    fn paging_before_after_png() {
        let ctx = shot_ctx();
        let lyric = "And I keep on running down the same old road that takes me back to you every single night, \
            through the rain and the lights of a city that never learned your name, and I still hear you calling out to me";
        let (width, view_h, font) = (470.0, 230.0, crate::theme::lyric_font_for("Inter", crate::preset::Weight::Bold, 30.0, lyric));
        let lh = 30.0 * 1.333;
        let toks = line_synced_parts(&Line { start: 0, dur: 1000, text: lyric.into(), ..Default::default() }).iter().flat_map(split_part).collect();
        let (words, galleys) = ctx.fonts_mut(|f| {
            let fl = flow(f, toks, &font, width, lh, Align::Left, false);
            let gs: Vec<Arc<Galley>> = fl.words.iter().map(|w| f.layout_no_wrap(w.text.clone(), font.clone(), Color32::WHITE)).collect();
            (fl.words, gs)
        });
        let labels = ctx.fonts_mut(|f| {
            ["Before (0.2.28): the 6th row is being sung, off the bottom", "After: the view pages down with it"].map(|t| f.layout_no_wrap(t.into(), crate::theme::semibold(16.0), Color32::WHITE))
        });
        let atlas = ctx.fonts(|f| f.image());
        let sung_row = 5.0;
        let y_row = sung_row * lh;
        // Old: the line's top. New: the sung row and the one after it in view.
        let tops = [0.0f32, (y_row + lh * 2.0 - view_h).max(0.0)];
        let (col, pad) = (width + 70.0, 30.0);
        let (iw, ih) = ((col * 2.0) as u32, (view_h + 110.0) as u32);
        let mut img = image::RgbaImage::from_pixel(iw, ih, image::Rgba([24, 22, 30, 255]));
        for (k, top) in tops.iter().enumerate() {
            let x0 = pad + col * k as f32;
            let (v0, v1) = (60.0, 60.0 + view_h);
            blit(&mut img, &atlas, pos2(x0, 20.0), &labels[k], [200, 196, 210], (0.0, ih as f32));
            // The view's frame.
            for x in (x0 - 10.0) as u32..(x0 + width + 10.0) as u32 {
                for y in [v0 as u32 - 1, v1 as u32] {
                    img.put_pixel(x, y, image::Rgba([90, 84, 104, 255]));
                }
            }
            for (w, g) in words.iter().zip(&galleys) {
                let row = (w.pos.y / lh).floor();
                let rgb = if row < sung_row { [242, 240, 245] } else if row == sung_row { [196, 160, 255] } else { [110, 106, 120] };
                blit(&mut img, &atlas, pos2(x0 + w.pos.x, v0 + w.pos.y - top), g, rgb, (v0, v1));
            }
        }
        let out = std::env::var("YTM_SHOT").unwrap_or_else(|_| "paging-before-after.png".into());
        img.save(&out).unwrap();
        println!("wrote {out}");
    }

    /// Before/after picture of right-to-left text with the app's fonts (#29), to a PNG.
    /// `YTM_SHOT=<file.png> cargo test --release -- --ignored rtl_before_after_png`
    #[test]
    #[ignore]
    fn rtl_before_after_png() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx, "Inter");
        for _ in 0..2 {
            let _ = ctx.run_ui(Default::default(), |_| {});
        }
        let titles = ["عمرو دياب - تملي معاك", "עומר אדם - תגידו לה", "Fairuz - نسم علينا الهوى (Live)"];
        let lyric = "يا ليل الصب متى غده أقيام الساعة موعده رقد السمار فأرقه أسف للبين يردده";
        let (col, width) = (560.0, 470.0);
        let title_font = crate::theme::semibold(26.0);
        let lyric_font = crate::theme::lyric_font_for("Inter", crate::preset::Weight::Bold, 30.0, lyric);
        // (position, galley) for everything drawn.
        let mut draw: Vec<(Pos2, Arc<Galley>)> = Vec::new();
        ctx.fonts_mut(|f| {
            for (i, label) in ["Before (0.2.28)", "After"].into_iter().enumerate() {
                draw.push((pos2(30.0 + col * i as f32, 20.0), f.layout_no_wrap(label.into(), crate::theme::bold(20.0), Color32::WHITE)));
            }
            for (k, t) in titles.iter().enumerate() {
                let y = 70.0 + k as f32 * 50.0;
                draw.push((pos2(30.0, y), f.layout_no_wrap(t.to_string(), title_font.clone(), Color32::WHITE)));
                draw.push((pos2(30.0 + col, y), f.layout_job(crate::bidi::job(t, title_font.clone(), Color32::WHITE))));
            }
            // Lyrics: the old view put the words left to right, wrapping at the width; the
            // new one from the right.
            let y = 240.0;
            let (lh, space) = (30.0 * 1.333, text_size(f, " ", &lyric_font).x);
            let mut at = vec2(0.0, 0.0);
            for word in lyric.split(' ') {
                let g = f.layout_no_wrap(word.into(), lyric_font.clone(), Color32::WHITE);
                if at.x > 0.0 && at.x + g.size().x > width {
                    at = vec2(0.0, at.y + lh);
                }
                let w = g.size().x;
                draw.push((pos2(30.0, y) + at, g));
                at.x += w + space;
            }
            let toks = line_synced_parts(&Line { start: 0, dur: 1000, text: lyric.into(), ..Default::default() }).iter().flat_map(split_part).collect();
            let fl = flow(f, toks, &lyric_font, width, 30.0 * 1.333, Align::Right, true);
            for w in fl.words {
                draw.push((pos2(30.0 + col, y) + w.pos.to_vec2(), f.layout_no_wrap(w.text.clone(), lyric_font.clone(), Color32::WHITE)));
            }
        });
        let atlas = ctx.fonts(|f| f.image());
        let (w, h) = ((col * 2.0) as u32, 470u32);
        let mut img = image::RgbaImage::from_pixel(w, h, image::Rgba([24, 22, 30, 255]));
        // The column divider.
        for y in 0..h {
            img.put_pixel(col as u32 - 15, y, image::Rgba([70, 66, 80, 255]));
        }
        for (at, g) in &draw {
            blit(&mut img, &atlas, *at, g, [242, 240, 245], (0.0, h as f32));
        }
        let out = std::env::var("YTM_SHOT").unwrap_or_else(|_| "rtl-before-after.png".into());
        img.save(&out).unwrap();
        println!("wrote {out}");
    }

    #[test]
    fn line_synced_words_are_staggered() {
        let l = Line { start: 2000, dur: 3000, text: "one two  three".into(), ..Default::default() };
        let p = line_synced_parts(&l);
        let words: Vec<_> = p.iter().filter(|p| !p.text.trim().is_empty()).collect();
        assert_eq!(words[1].start, 2050);
        assert_eq!(words[2].start, 2100);
    }

    #[test]
    fn springs_settle_without_oscillating() {
        // `springProgress`: monotone, from rest to 1 by its own duration.
        let mut prev = -1.0;
        for k in 0..=20 {
            let v = spring_progress(k as f64 / 10.0, 2.0);
            assert!(v >= prev, "spring_progress must not go backwards");
            assert!((0.0..=1.0000001).contains(&v), "stays between 0 and 1, got {v}");
            prev = v;
        }
        assert!((spring_progress(2.0, 2.0) - 1.0).abs() < 0.02);
        // `sampleSpring`: the underdamped spring a word's rise and the view's scroll use,
        // from rest to nearly 1 within its settling time.
        assert_eq!(spring_step(0.0, 14.0f64.sqrt(), 3.5 / 14.0f64.sqrt()), 0.0);
        assert!(spring_step(2.0, 14.0f64.sqrt(), 3.5 / 14.0f64.sqrt()) > 0.99);
    }

    #[test]
    fn glyph_glow_breathes_to_the_keyframes() {
        let span = 4.0;
        assert_eq!(glow_env(0.0, span), 0.0);
        assert!((glow_env(0.15 * span, span) - 0.8).abs() < 1e-6);
        assert!((glow_env(0.30 * span, span) - 1.0).abs() < 1e-6);
        assert!((glow_env(0.45 * span, span) - 0.65).abs() < 1e-6);
        assert!((glow_env(0.60 * span, span) - 0.3).abs() < 1e-6);
        assert!((glow_env(span, span) - 0.0).abs() < 1e-6);
        // Before it starts, or with no span to run over: nothing.
        assert_eq!(glow_env(-1.0, span), 0.0);
        assert_eq!(glow_env(1.0, 0.0), 0.0);
    }

    #[test]
    fn only_a_held_non_cjk_word_glows() {
        // Past a beat: the fan opens over a second, the shadow over half one, and the
        // shadow stops at `--char-glow-max` 0.45 rather than reaching full strength.
        let held = Clock { start: 0.0, dur: 3.0, chars: 6, first: 0, cjk: false }.plan();
        assert!((held.emphasis - 1.0).abs() < 1e-9);
        assert!((held.glow - 0.45).abs() < 1e-9);
        let rising = Clock { start: 0.0, dur: 1.25, chars: 6, first: 0, cjk: false }.plan();
        assert!((rising.emphasis - 0.25).abs() < 1e-9);
        assert!((rising.glow - 0.225).abs() < 1e-9);
        let short = Clock { start: 0.0, dur: 0.5, chars: 6, first: 0, cjk: false }.plan();
        assert_eq!(short.emphasis, 0.0);
        assert_eq!(short.glow, 0.0);
        let cjk = Clock { start: 0.0, dur: 3.0, chars: 6, first: 0, cjk: true }.plan();
        assert_eq!(cjk.emphasis, 0.0);
        assert_eq!(cjk.glow, 0.0);
        // CJK staggers its glyphs tighter (never more than 50 ms apart, nor a fifth of a
        // second split between them) and never finishes before 3 s.
        assert!((cjk.delay - 0.2 / 6.0).abs() < 1e-9);
        assert!(cjk.span >= 3.0);
        assert!((held.delay - (3.0 / 6.0) * 0.35).abs() < 1e-9);
        assert_eq!(held.hold, 2.0 * 3.0 / 6.0);
    }

    #[test]
    fn glyphs_rise_one_after_another_and_spread_while_held() {
        let p = Clock { start: 0.0, dur: 1.5, chars: 4, first: 0, cjk: false }.plan();
        // Nothing has moved before the first glyph is due.
        assert_eq!(char_pose(&p, 0, 4, 0.0, 3.0), (0.0, 0.0, 1.0));
        assert_eq!(char_pose(&p, 3, 4, 0.3, 3.0).0, 0.0, "the last glyph is not due yet");
        // `getCharacterFrames`: the rise runs for the word's response.
        let risen = |i: usize, t: f64| char_pose(&p, i, 4, t, 3.0).0 - char_pose(&p, i, 4, t, 0.0).0;
        let early = risen(0, 0.8);
        assert!(early > 1.5 && early < 3.0, "still on its way at {early}");
        assert!((risen(0, 3.0) - 3.0 * 0.92).abs() < 0.01);
        let (a_lift, b_lift) = (risen(0, 1.2), risen(3, 1.2));
        assert!(a_lift > b_lift + 0.1, "the glyph sung later is behind: {a_lift} {b_lift}");
        assert!((risen(0, 4.0) - risen(3, 4.0)).abs() < 1e-3, "both have finished rising");
        let a_dx = char_pose(&p, 0, 4, 1.2, 3.0).1;
        let b_dx = char_pose(&p, 3, 4, 1.2, 3.0).1;
        // The fan is symmetric about the middle and swells as it spreads.
        assert!(a_dx < 0.0 && b_dx > 0.0, "spread from the centre: {a_dx} {b_dx}");
        assert!(char_pose(&p, 1, 4, 1.2, 3.0).2 > 1.0);
        // Once the breath is over the spread and swell are gone, and the rise holds.
        let (lift, dx, s) = char_pose(&p, 0, 4, 12.0, 3.0);
        assert!(dx.abs() < 1e-6 && (s - 1.0).abs() < 1e-6, "settles back: {dx} {s}");
        assert!((lift - 3.0 * 0.92).abs() < 0.01, "holds its rise at {lift}");
    }

    #[test]
    fn every_word_moves_letter_by_letter() {
        let plan = Clock { start: 0.0, dur: 3.0, chars: 4, first: 0, cjk: false }.plan();
        // Each glyph has a clock of its own, as am-lyrics' `.character` spans do.
        assert!(
            (0..300).any(|k| {
                let t = k as f64 * 0.01;
                glyph_pose(false, &plan, 0, 4, t, 3.0) != glyph_pose(false, &plan, 3, 4, t, 3.0)
            }),
            "a word moves letter by letter"
        );
        // Held, it swells and fans from its middle.
        assert!(glyph_pose(false, &plan, 1, 4, 1.6, 3.0).2 > 1.0, "swells while held");
        // Once the breath is over the glyphs settle back into an upright row.
        let (_, dx, s) = glyph_pose(false, &plan, 0, 4, 20.0, 3.0);
        assert!(dx.abs() < 1e-6 && (s - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_standalone_script_keeps_its_own_clock() {
        assert!(cjk_script("青花瓷"));
        assert!(cjk_script("さくら"));
        assert!(cjk_script("안녕"));
        assert!(!cjk_script("hello"));
    }

    #[test]
    fn scroll_cascade_delays_but_never_slows() {
        assert_eq!(scroll_cascade(0), 0.0);
        assert!((scroll_cascade(-3) - 0.15).abs() < 1e-9);
        // Upstream stops the cascade at four lines, so the far end of a long view lands
        // with the rest rather than trailing behind it.
        assert!((scroll_cascade(9) - 0.2).abs() < 1e-9);
    }

    #[test]
    fn gradient_is_soft_and_never_a_box() {
        let look = WordLook { opacity: 1.0, start: 0.5, end: 0.6, glow: 0.0, glow_alpha: 0.5 };
        let word = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 40.0));
        let (f, cuts) = lit_gradient(word, &look, false);
        assert_eq!(f(10.0), 1.0);
        assert!(f(55.0) > 0.0 && f(55.0) < 1.0);
        assert_eq!(f(70.0), 0.0);
        assert_eq!(cuts.len(), 2);
        // A right-to-left word is lit from the right edge instead.
        let (g, _) = lit_gradient(word, &look, true);
        assert_eq!(g(99.0), 1.0);
        assert_eq!(g(20.0), 0.0);
    }

    #[test]
    fn a_break_finishes_before_the_next_line() {
        // A 6 s break: the exit starts 0.85 s before the next line and lasts 0.35 s.
        let (exit, len, step) = gap_times(10.0, 6.0);
        assert!((exit - 15.15).abs() < 1e-9);
        assert!((len - 0.35).abs() < 1e-9);
        assert!((step - 5.15 / 3.0).abs() < 1e-9);
        // A break too short to count still opens, then uses its whole exit.
        let (exit, len, _) = gap_times(10.0, 1.0);
        assert!((exit - 10.62).abs() < 1e-9);
        assert!((len - 0.35).abs() < 1e-9);
    }
}
