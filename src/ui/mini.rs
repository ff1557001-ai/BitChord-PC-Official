//! The mini player: Apple's picture-in-picture card — the sleeve filling a square window,
//! sharp at its top and losing focus down into the song's name, with the quality mark, the
//! seek line and the transport over its lower half. Ctrl+M or the player bar's button
//! switches; the card's expand button, its × (which closes the card, not the app) or a
//! double-click switch back.
//!
//! Each mode keeps its own place: the full window goes back to exactly where it was
//! (maximized again if it was), and the strip reopens where it was last moved. Places
//! are kept in physical pixels with the monitor's scale, so they survive moving between
//! monitors with different scaling. Launching always opens the full window: closed in
//! the mini player, the app saves the full window's place for next time.
//!
//! On Windows the app draws the strip's window buttons itself (see `titlebar`). Elsewhere
//! the system's frame stays, with its own close button, so the strip leaves its out.
//! Keeping on top is up to the window manager (Wayland has no way to ask).

use std::time::Duration;

use egui::{Color32, Pos2, Rect, Sense, Ui, Vec2, ViewportCommand, WindowLevel, pos2, vec2};
use serde::{Deserialize, Serialize};

use super::widgets::{self, icon_at, named, text_at};
use super::{Action, App, Repeat, fmt_time, glyphs, titlebar};
use crate::theme::{self, icon};

// Decisions that are easy to change.

/// The card: Apple's square, and the only size it comes in. A square is what lets the
/// sleeve be the whole cover — a wider card would crop the top and bottom off it.
pub const SIZE: Vec2 = vec2(256.0, 256.0);
const MIN_W: f32 = SIZE.x;
const MAX_W: f32 = SIZE.x;
/// Kept above other windows until unpinned.
const PINNED_BY_DEFAULT: bool = true;
/// From the work area's corner, the first time.
const MARGIN: f32 = 16.0;

/// The card's rows, measured up from its bottom edge: the transport, the clock, the seek
/// line, and the words the sleeve feathers into.
const TRANSPORT: f32 = 32.0;
const CLOCK: f32 = 58.0;
const SEEK: f32 = 80.0;
const SUB: f32 = 98.0;
const TITLE: f32 = 124.0;
/// Where the sleeve stops being sharp, measured up from the card's bottom.
const SPLIT: f32 = 142.0;
/// A chrome button's centre from the card's edge, a capsule slot, and the star's disc.
/// One scale for all of it: the card is small, and buttons of three different sizes on it
/// read as a mistake rather than as emphasis.
const KEY_X: f32 = 24.0;
const CAP_SLOT: f32 = 25.0;
const MARK: f32 = 24.0;
/// How far the volume's track slides out of the capsule while it has the pointer.
const VOL_TRACK: f32 = 80.0;
/// The card's inset.
const PAD: f32 = 18.0;

/// Where a window was: its outer corner in physical pixels, its inner size in points,
/// and the monitor's scale and the app's zoom then (points are that big).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub scale: f32,
    #[serde(default = "unzoomed")]
    pub zoom: f32,
}

fn unzoomed() -> f32 {
    1.0
}

impl Place {
    /// The window's place now, once the platform has said. Wayland never tells apps
    /// where their windows are (nor lets them move them): there only the size counts.
    fn current(ctx: &egui::Context) -> Option<Place> {
        let ppp = ctx.pixels_per_point();
        let size = ctx.content_rect().size();
        let zoom = ctx.zoom_factor();
        ctx.input(|i| {
            let vp = i.viewport();
            let pos = vp.outer_rect.map_or(Pos2::ZERO, |r| r.min * ppp);
            Some(Place { x: pos.x, y: pos.y, w: size.x, h: size.y, scale: vp.native_pixels_per_point?, zoom })
        })
    }

    fn size(&self) -> Vec2 {
        vec2(self.w, self.h)
    }

    /// Its size in points at the app's zoom now (it may have changed since).
    fn size_at(&self, zoom: f32) -> Vec2 {
        self.size() * (self.zoom / zoom)
    }

    /// The outer rectangle in physical pixels, given the frame around the inside (points).
    fn rect_px(&self, frame: Vec2) -> Rect {
        Rect::from_min_size(pos2(self.x, self.y), (self.size() + frame) * self.scale * self.zoom)
    }

    /// eframe's saved-window format, so a launch after closing in the mini player opens
    /// the full window where it was.
    fn eframe_window(&self, maximized: bool) -> String {
        format!(
            "(normal_geometry:(outer_position_pixels:Some((x:{:?},y:{:?})),scale_factor:{:?},inner_size_points:(x:{:?},y:{:?})),fullscreen:false,maximized:{maximized})",
            self.x, self.y, self.scale, self.w, self.h
        )
    }
}

/// The strip's first place: the bottom right of `area` (physical pixels), clear of its
/// edges, for a window with `frame` (points) around its inside. A point is `scale`
/// times `zoom` pixels.
fn corner_place(area: Rect, frame: Vec2, scale: f32, zoom: f32) -> Place {
    let outer = (SIZE + frame) * scale * zoom;
    let p = area.max - outer - Vec2::splat(MARGIN * scale);
    let p = p.max(area.min);
    Place { x: p.x.round(), y: p.y.round(), w: SIZE.x, h: SIZE.y, scale, zoom }
}

/// A window `size` (points) in the middle of `area` (physical pixels), kept inside it.
fn centered(area: Rect, size: Vec2, frame: Vec2, scale: f32, zoom: f32) -> Place {
    let outer = (size + frame) * scale * zoom;
    let p = (area.center() - outer / 2.0).max(area.min);
    Place { x: p.x.round(), y: p.y.round(), w: size.x, h: size.y, scale, zoom }
}

/// The frame around the window's inside (points): what the platform drew outside the
/// content. Nothing on Windows, where the app draws its own buttons.
fn frame_size(ctx: &egui::Context) -> Vec2 {
    ctx.input(|i| {
        let vp = i.viewport();
        vp.outer_rect.zip(vp.inner_rect).map_or(Vec2::ZERO, |(o, n)| o.size() - n.size())
    })
}

/// What the app can ask about the monitors (the system's; made up in tests).
#[derive(Clone, Copy)]
struct Screens {
    /// Whether some of a rectangle (physical pixels) is on a connected monitor.
    on_a_monitor: fn(Rect) -> bool,
    /// The window's monitor less the taskbar, in physical pixels.
    work_area: fn() -> Option<Rect>,
}

const SYSTEM: Screens = Screens { on_a_monitor: crate::fullscreen::on_a_monitor, work_area: crate::fullscreen::work_area };

/// A remembered strip size, within the limits (they may have changed since). The strip
/// has one height.
fn clamp_size(p: Place) -> Place {
    Place { w: p.w.clamp(MIN_W, MAX_W), h: SIZE.y, ..p }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Step {
    /// Waiting for a maximized full window to come down, so its normal place is known.
    Unmaximize,
    /// Move and resize now.
    Resize,
    /// Moved onto a monitor with another scale: size it once the window has that scale.
    Rescale(Place),
    /// Waiting for the window to reach its new size (None: to be maximized) before the
    /// new layout shows.
    Settle(Option<Vec2>),
}

struct Switch {
    to_mini: bool,
    step: Step,
    since: f64,
    /// The window's size when it began.
    from: Vec2,
}

/// What to draw this frame.
#[derive(Clone, Copy, PartialEq)]
pub enum Layout {
    Full,
    Mini,
    /// Just the background: mid-switch, the window no longer fits the old layout and
    /// doesn't have the new one's size yet.
    Blank,
}


pub struct Mini {
    /// The strip is showing.
    on: bool,
    pub pinned: bool,
    /// The strip's last place.
    place: Option<Place>,
    /// The full window's, from before it shrank, and whether it was maximized.
    full: Option<(Place, bool)>,
    was_maximized: bool,
    switch: Option<Switch>,
    /// A switch asked for, started once the window is ready.
    wanted: Option<bool>,
    /// When the current layout appeared, for its fade-in.
    shown_at: f64,
    /// The scale the size limits were last sent at: winit keeps them in pixels.
    limits_scale: Option<f32>,
    /// The window level last sent, and when: a topmost strip is re-stamped now and
    /// then, since another window's z-order change can knock it down.
    level: Option<WindowLevel>,
    level_at: f64,
    screens: Screens,
    /// The full window's normal place after coming back maximized, until it's restored down.
    restored_max: Option<Place>,
}

impl Mini {
    pub fn load(get: impl Fn(&str) -> Option<String>) -> Self {
        Mini {
            on: false,
            pinned: get("mini_pinned").map_or(PINNED_BY_DEFAULT, |v| v == "1"),
            place: get("mini_window").and_then(|v| serde_json::from_str(&v).ok()).map(clamp_size),
            full: None,
            was_maximized: false,
            switch: None,
            wanted: None,
            shown_at: f64::NEG_INFINITY,
            limits_scale: None,
            level: None,
            level_at: f64::NEG_INFINITY,
            screens: SYSTEM,
            restored_max: None,
        }
    }

    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        storage.set_string("mini_pinned", if self.pinned { "1" } else { "0" }.into());
        if let Some(p) = &self.place {
            storage.set_string("mini_window", serde_json::to_string(p).unwrap_or_default());
        }
    }

    /// Ask for the strip (true) or the full window; started by [`Mini::take_request`].
    pub fn request(&mut self, on: bool) {
        self.wanted = Some(on);
    }

    /// The switch to start now, if any (not while one runs).
    pub fn take_request(&mut self) -> Option<bool> {
        if self.switch.is_some() {
            return None;
        }
        self.wanted.take()
    }

    /// Where it's headed once queued switches are done: what Ctrl+M flips.
    pub fn target(&self) -> bool {
        self.wanted.unwrap_or_else(|| self.active())
    }

    /// The strip is showing or on its way.
    pub fn active(&self) -> bool {
        self.switch.as_ref().map_or(self.on, |s| s.to_mini)
    }

    /// The strip is showing, or the window is switching either way: the full window's
    /// pages and now playing aren't up.
    pub fn busy(&self) -> bool {
        self.on || self.switch.is_some()
    }

    /// The strip is showing (not mid-switch).
    pub fn showing(&self) -> bool {
        self.on && self.switch.is_none()
    }

    /// The old layout stays until the window starts changing size.
    pub fn layout(&self, ctx: &egui::Context) -> Layout {
        match &self.switch {
            Some(sw) if (ctx.content_rect().size() - sw.from).length() > 1.0 => Layout::Blank,
            _ if self.on => Layout::Mini,
            _ => Layout::Full,
        }
    }

    /// The full window's place in eframe's format, while eframe's own would be wrong: with
    /// the strip up or on its way back (eframe's is the strip's), and while the full window
    /// is still maximized after it (eframe took the strip's as its normal size).
    pub fn full_window(&self) -> Option<String> {
        if self.active() || self.switch.is_some() {
            return self.full.map(|(p, max)| p.eframe_window(max));
        }
        self.restored_max.map(|p| p.eframe_window(true))
    }

    /// The new layout fades in from `base` (the background's own colour): drawn over
    /// everything, since pages and popups paint on layers of their own.
    pub fn fade_in(&self, ctx: &egui::Context, now: f64, base: Color32) {
        let t = ((now - self.shown_at) / 0.18).clamp(0.0, 1.0) as f32;
        if t >= 1.0 {
            return;
        }
        ctx.request_repaint();
        let layer = egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("mini-fade"));
        ctx.layer_painter(layer).rect_filled(ctx.content_rect(), 0.0, base.gamma_multiply(1.0 - theme::ease_out(t)));
    }

    /// Switch to the strip or back. The full window leaves full screen first (the caller's).
    pub fn toggle(&mut self, ctx: &egui::Context, now: f64) {
        if self.switch.is_some() {
            return;
        }
        let to_mini = !self.on;
        let mut step = Step::Resize;
        if to_mini {
            self.was_maximized = ctx.input(|i| i.viewport().maximized == Some(true));
            if self.was_maximized {
                ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
                step = Step::Unmaximize;
            }
        }
        self.switch = Some(Switch { to_mini, step, since: now, from: ctx.content_rect().size() });
        ctx.request_repaint();
    }

    pub fn set_pinned(&mut self, ctx: &egui::Context, on: bool) {
        self.pinned = on;
        self.apply_level(ctx);
    }

    /// Push the level the current mode asks for. A pinned strip is re-stamped every
    /// couple of seconds: raising another window can drop it out of the topmost band.
    fn apply_level(&mut self, ctx: &egui::Context) {
        let want = if self.active() && self.pinned { WindowLevel::AlwaysOnTop } else { WindowLevel::Normal };
        let now = ctx.input(|i| i.time);
        let since = now - self.level_at;
        let stale = want == WindowLevel::AlwaysOnTop && since > 2.0;
        if self.level == Some(want) && !stale {
            // Wake for the next re-stamp even when nothing else is animating.
            if want == WindowLevel::AlwaysOnTop {
                ctx.request_repaint_after(Duration::from_secs_f64((2.0 - since).max(0.2)));
            }
            return;
        }
        self.level = Some(want);
        self.level_at = now;
        // Windows stamps the style itself: winit posts its change to the window's thread
        // (`SWP_ASYNCWINDOWPOS`) and can lose it, which is what made the pin look dead.
        // Elsewhere that is the only way.
        if !crate::fullscreen::set_topmost(want == WindowLevel::AlwaysOnTop) {
            ctx.send_viewport_cmd(ViewportCommand::WindowLevel(want));
        }
    }

    /// Once a frame: drive a switch, note the strip's place, keep the size limits right.
    pub fn tick(&mut self, ctx: &egui::Context, now: f64) {
        let scale = ctx.input(|i| i.viewport().native_pixels_per_point);
        // winit keeps the limits in pixels: again after a move to another scale or a zoom.
        if self.limits_scale.is_some_and(|s| s != ctx.pixels_per_point()) && self.switch.is_none() {
            self.limits(ctx, self.on);
        }
        if self.restored_max.is_some() && self.switch.is_none() && ctx.input(|i| i.viewport().maximized == Some(false)) {
            self.restored_max = None;
        }
        // The strip keeps the level its pin asks for; the full window never stays on top.
        self.apply_level(ctx);
        let (minimized, maximized) = ctx.input(|i| (i.viewport().minimized == Some(true), i.viewport().maximized == Some(true)));
        // Win+Up or a drag onto the screen's top edge: the strip has one height.
        if self.showing() && maximized && !minimized {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
        }
        // Minimized, the platform says no place (and the strip's isn't one maximized).
        if self.showing()
            && !minimized
            && !maximized
            && let Some(p) = Place::current(ctx).filter(|p| p.w >= MIN_W - 1.0)
        {
            self.place = Some(clamp_size(p));
        }
        let Some(sw) = &mut self.switch else { return };
        ctx.request_repaint_after(Duration::from_millis(16));
        let waited = now - sw.since;
        let to_mini = sw.to_mini;
        let next = match sw.step {
            Step::Unmaximize => {
                let down = ctx.input(|i| i.viewport().maximized != Some(true));
                // Its normal place, at once: closing before the next step still saves it.
                if down {
                    self.full = Place::current(ctx).map(|p| (p, true));
                }
                (down || waited > 0.5).then_some(Step::Resize)
            }
            Step::Resize => Some(self.resize(ctx, to_mini)),
            Step::Rescale(target) => (scale.is_some_and(|s| (s - target.scale).abs() < 0.01) || waited > 0.6).then(|| {
                self.limits(ctx, to_mini);
                let size = target.size_at(ctx.zoom_factor());
                ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
                Step::Settle(Some(size))
            }),
            Step::Settle(size) => {
                let done = match size {
                    Some(size) => (ctx.content_rect().size() - size).length() < 2.0,
                    None => ctx.input(|i| i.viewport().maximized == Some(true)),
                };
                if done || waited > 0.4 {
                    log::info!("{}: {:?}{}", if to_mini { "mini player" } else { "full window" }, Place::current(ctx), if done { "" } else { " (timed out)" });
                    self.on = to_mini;
                    if !to_mini {
                        self.restored_max = self.full.take().filter(|(_, max)| *max).map(|(p, _)| p);
                    }
                    self.switch = None;
                    self.shown_at = now;
                    return;
                }
                None
            }
        };
        if let (Some(step), Some(sw)) = (next, &mut self.switch) {
            sw.step = step;
            sw.since = now;
        }
    }

    /// Whether a place is (at least partly) on a connected monitor. Test runs live off-screen.
    fn visible(&self, p: Place, frame: Vec2) -> bool {
        // Its top middle, where it can be grabbed: a sliver on screen isn't enough.
        let r = p.rect_px(frame);
        let grip = Rect::from_center_size(pos2(r.center().x, r.top() + 12.0 * p.scale), vec2(r.width().min(120.0 * p.scale), 16.0 * p.scale));
        std::env::var_os("YTM_BACKGROUND").is_some() || (self.screens.on_a_monitor)(grip)
    }

    /// Move and size the window for the new mode; the step to wait in after.
    fn resize(&mut self, ctx: &egui::Context, to_mini: bool) -> Step {
        let now_place = Place::current(ctx);
        let frame = ctx.input(|i| {
            let vp = i.viewport();
            vp.outer_rect.zip(vp.inner_rect).map_or(Vec2::ZERO, |(o, i)| o.size() - i.size())
        });
        if to_mini {
            self.full = now_place.map(|p| (p, self.was_maximized));
            let target = match (self.place, now_place) {
                // Test runs stay off-screen, where they are; YTM_MINI_W tries other widths.
                (_, Some(here)) if std::env::var_os("YTM_BACKGROUND").is_some() => {
                    let w = std::env::var("YTM_MINI_W").ok().and_then(|w| w.parse().ok()).unwrap_or(SIZE.x);
                    clamp_size(Place { w, ..here })
                }
                (Some(p), _) if self.visible(p, frame) => p,
                (_, here) => {
                    let scale = here.map_or(1.0, |h| h.scale);
                    // Not Windows: the bottom right of the full window, where the player bar was.
                    let area = (self.screens.work_area)()
                        .or_else(|| here.map(|h| h.rect_px(frame)))
                        .unwrap_or(Rect::from_min_size(Pos2::ZERO, SIZE * scale));
                    corner_place(area, frame, scale, ctx.zoom_factor())
                }
            };
            self.apply_level(ctx);
            self.go(ctx, target, false, true)
        } else {
            if let Some(p) = now_place {
                self.place = Some(clamp_size(p));
            }
            self.apply_level(ctx);
            // Kept until the window has settled: a save meanwhile still writes it.
            match self.full {
                Some((p, max)) if self.visible(p, frame) => self.go(ctx, p, max, false),
                // Its monitor is gone: its size, in the middle of the strip's monitor.
                full => {
                    let zoom = ctx.zoom_factor();
                    let size = full.map_or(Vec2::from(crate::DEFAULT_SIZE), |(p, _)| p.size_at(zoom));
                    let max = full.is_some_and(|(_, max)| max);
                    match ((self.screens.work_area)(), now_place) {
                        (Some(area), Some(here)) => {
                            // Its new place is the one to save (and restore down to).
                            let target = centered(area, size, frame, here.scale, zoom);
                            self.full = Some((target, max));
                            self.go(ctx, target, max, false)
                        }
                        _ => {
                            self.limits(ctx, false);
                            ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
                            Step::Settle(Some(size))
                        }
                    }
                }
            }
        }
    }

    /// Move first, then the limits (changing them can resize the window at once, which
    /// could carry it onto another monitor before the move), then the size.
    fn go(&mut self, ctx: &egui::Context, target: Place, maximize: bool, mini: bool) -> Step {
        let ppp = ctx.pixels_per_point();
        ctx.send_viewport_cmd(ViewportCommand::OuterPosition(pos2(target.x, target.y) / ppp));
        self.limits(ctx, mini);
        let scale = ctx.input(|i| i.viewport().native_pixels_per_point);
        if scale.is_some_and(|s| (s - target.scale).abs() >= 0.01) && !maximize {
            // Points mean another size there: wait for the move to land.
            return Step::Rescale(target);
        }
        let size = target.size_at(ctx.zoom_factor());
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
        if maximize {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(true));
            return Step::Settle(None);
        }
        Step::Settle(Some(size))
    }

    #[cfg(target_os = "linux")]
    pub(super) fn wayland_viewport(&self, builder: egui::ViewportBuilder) -> egui::ViewportBuilder {
        if self.active() {
            let h = height(self.lyrics);
            builder
                .with_min_inner_size([MIN_W, h])
                .with_max_inner_size([MAX_W, h])
        } else {
            builder
        }
    }

    /// The window's size limits for a mode. Shrinking sets them first, so the full
    /// window's minimum doesn't hold the strip up. The strip's height is fixed at both
    /// and only the width runs.
    fn limits(&mut self, ctx: &egui::Context, mini: bool) {
        if mini {
            let h = SIZE.y;
            ctx.send_viewport_cmd(ViewportCommand::MinInnerSize(vec2(MIN_W, h)));
            ctx.send_viewport_cmd(ViewportCommand::MaxInnerSize(vec2(MAX_W, h)));
        } else {
            ctx.send_viewport_cmd(ViewportCommand::MaxInnerSize(Vec2::INFINITY));
            ctx.send_viewport_cmd(ViewportCommand::MinInnerSize(Vec2::from(crate::MIN_SIZE)));
        }
        self.limits_scale = Some(ctx.pixels_per_point());
    }
}

/// The card: the sleeve fills the window, sharp over its top and feathered into its own
/// blur above the words, with the song's name and album on the sleeve, the star and the
/// song's menu beside them, the seek line over the clock and the quality mark, and the
/// transport along the bottom. The window's own buttons sit top left, a frosted capsule
/// top right, and the pin keeps its corner.
pub fn strip(app: &mut App, ui: &mut Ui, now: f64) {
    let ctx = ui.ctx().clone();
    let r = ctx.content_rect();
    // An Automix handoff keeps the outgoing song here until the next one is louder.
    let hold = app.mix_hold().is_some();
    let track = if hold { app.shown().cloned() } else { app.current().cloned() };
    let col = r;

    // Chrome: the window's buttons top left, a frosted capsule top right. A press in either
    // corner is the button's, never the window's — the card is a drag area, and a strip that
    // slid away under the pointer turns the click into a drag and loses it, which is how the
    // pin came to look like a button that moves the player.
    let keys = if titlebar::custom() { 3.0 } else { 2.0 };
    let cap_r = col.right() - PAD + 6.0;
    let cap_n = if app.show_volume { 1.0 } else { 0.0 };
    let cap_w = CAP_SLOT * cap_n + 8.0;
    let corners = [
        Rect::from_min_max(r.min + vec2(2.0, 2.0), pos2(col.left() + KEY_X + 28.0 * (keys - 1.0) + 16.0, col.top() + 48.0)),
        Rect::from_min_max(pos2(cap_r - cap_w - if app.show_volume { VOL_TRACK } else { 0.0 }, col.top()), pos2(col.right(), col.top() + 48.0)),
    ];
    // Its background moves the window; a double-click goes back to the full window.
    if titlebar::drag_strip(ui, r, "mini", &corners).double_clicked() {
        app.actions.push(Action::Mini(false));
    }

    // The screen behind the card, frosted, is the window's ground: a bright window at its
    // left edge comes through that edge, and the frost follows the card as it is dragged.
    // The sleeve lies over the card's top and loses focus on the way down to the words, as
    // the reference's does. Where the screen can't be read — another platform, a driver that
    // won't answer the copy — the sleeve's own blur stands in for the desktop's.
    match app.backdrop.frame(&ctx) {
        Some(tex) => {
            let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            ui.painter().image(tex.id(), r, full, Color32::from_gray(198));
        }
        None => {
            if !super::shell::frost(app, ui, r, 0.0, 0.0) {
                ui.painter().rect_filled(r, 0.0, theme::panel(0.9));
            }
        }
    }
    let split = col.bottom() - SPLIT;
    if let Some(t) = &track {
        sleeve(ui, col, &t.thumbs, split);
    } else {
        icon_at(ui, pos2(col.center().x, col.top() + 78.0), icon::MUSIC, 34.0, theme::with_alpha(Color32::WHITE, 0.45));
    }
    // A wash for the chrome and a deeper one for the words, so both read on any sleeve.
    wash(ui, Rect::from_min_max(col.min, pos2(col.right(), col.top() + 62.0)), 92, 0);
    wash(ui, Rect::from_min_max(pos2(col.left(), split - 26.0), col.max), 0, 186);

    // The words, over the sleeve's soft end.
    let (title, sub) = match &track {
        Some(t) => {
            let line = t.artist_line();
            match t.album.as_ref().map(|a| a.name.clone()).filter(|a| !a.is_empty()) {
                Some(album) => (t.title.clone(), format!("{line} — {album}")),
                None => (t.title.clone(), line),
            }
        }
        None => ("Nothing playing".to_owned(), String::new()),
    };
    let x0 = col.left() + PAD;
    // The star and the menu stand at the words' right, so the text stops before them.
    let x1 = col.right() - PAD - if track.is_some() { 2.0 * MARK + 10.0 } else { 0.0 };
    let tw = (x1 - x0).max(20.0);
    let tr = text_at(ui, pos2(x0, col.bottom() - TITLE), &title, theme::semibold(13.0), Color32::WHITE, tw);
    if let Some(t) = &track {
        named(ui.interact(tr, ui.scope_id().with("mini-title"), Sense::hover()), &t.title).on_hover_text(&t.title);
    }
    text_at(ui, pos2(x0, col.bottom() - SUB - 8.5), &sub, theme::regular(12.0), theme::with_alpha(Color32::WHITE, 0.78), tw);
    marks(app, ui, col, &track, hold);

    seek_row(app, ui, col, track.as_ref());
    transport(app, ui, col, &track, now);
    chrome(app, ui, r, col, cap_r, cap_w);
}

/// The sleeve cropped to fill the card, sharp over its top and out of focus by the time it
/// reaches the words. Each strip is the same crop, so the layers register.
fn sleeve(ui: &mut Ui, col: Rect, thumbs: &[crate::model::Thumb], split: f32) {
    let mut all = ui.new_child(egui::UiBuilder::new().max_rect(col));
    all.set_clip_rect(col);
    widgets::paint_cover_fill(&all, col, thumbs, Color32::WHITE);
    // egui can't filter a texture it hasn't decoded, so the blur is the same crop drawn a
    // few points apart. Down the card each band takes more of the layer and spreads its
    // samples wider, so the focus goes soft by degrees instead of on a line — and soft
    // enough, by the time the words start, to read as a defocused sleeve and not a shadow.
    const STEPS: usize = 8;
    // The centre plus a ring of eight: a sample every way the lens could have missed.
    const TAPS: [(f32, f32); 9] = [
        (0.0, 0.0),
        (1.0, 0.0),
        (-1.0, 0.0),
        (0.0, 1.0),
        (0.0, -1.0),
        (0.7, 0.7),
        (-0.7, 0.7),
        (0.7, -0.7),
        (-0.7, -0.7),
    ];
    let band = (col.bottom() - split).max(0.0);
    for i in 0..STEPS {
        let f = (i as f32 + 0.5) / STEPS as f32;
        let y0 = split + band * (f - 0.5 / STEPS as f32);
        let y1 = split + band * (f + 0.5 / STEPS as f32);
        let a = (f * f).min(1.0);
        let r = 2.0 + 14.0 * f * f;
        // n layers at this alpha read as one at `a`, since they blend instead of averaging.
        let t = 1.0 - (1.0 - a).powf(1.0 / TAPS.len() as f32);
        let alpha = (255.0 * t).round().clamp(0.0, 255.0) as u8;
        if alpha < 3 {
            continue;
        }
        let clip = Rect::from_min_max(pos2(col.left(), y0.max(col.top())), pos2(col.right(), y1.min(col.bottom())));
        for (dx, dy) in TAPS {
            let mut s = ui.new_child(egui::UiBuilder::new().max_rect(col));
            s.set_clip_rect(clip);
            widgets::paint_cover_fill(&s, col.translate(vec2(dx * r, dy * r)), thumbs, Color32::from_white_alpha(alpha));
        }
    }
}

/// A wash of black down `r`, from `a0` at its top to `a1` at its bottom. One mesh, so the
/// ramp is continuous: in steps it read as stripes over the sleeve.
fn wash(ui: &Ui, r: Rect, a0: u8, a1: u8) {
    super::shell::vgradient(ui.painter(), r, Color32::from_black_alpha(a0), Color32::from_black_alpha(a1));
}

/// The star and the song's menu: Apple's two little discs, standing at the end of the
/// subtitle's line.
fn marks(app: &mut App, ui: &mut Ui, col: Rect, track: &Option<crate::model::Track>, hold: bool) {
    let Some(shown) = track else { return };
    let ctx = ui.ctx().clone();
    let cy = col.bottom() - SUB;
    let right = col.right() - PAD;
    let scope = ui.scope_id();
    let disc = |ui: &mut Ui, c: Pos2, key: &str, name: &str| -> egui::Response {
        let r = Rect::from_center_size(c, Vec2::splat(MARK));
        let resp = ui.interact(r, scope.with(("mini-mark", key)), Sense::click());
        named(resp, name).on_hover_text(name).on_hover_cursor(egui::CursorIcon::PointingHand)
    };
    // What's heard, like Now playing's menu: a song version standing in for the video.
    let t = if hold { shown.clone() } else { app.playing_track().cloned().unwrap_or_else(|| shown.clone()) };
    let liked = app.liked.contains(&t.id);
    let name = if liked { "Remove like" } else { "Like" };
    let s = disc(ui, pos2(right - MARK - 10.0 - MARK / 2.0, cy), "like", name);
    let sh = theme::anim_bool(&ctx, s.id.with("h"), s.hovered(), 0.12);
    let pop = theme::anim_bool(&ctx, ("liked", &t.id), liked, 0.3);
    // Engaged wears the lit disc the player's corners use; idle only washes under the pointer.
    if pop > 0.01 {
        ui.painter().circle_filled(s.rect.center(), MARK / 2.0, theme::with_alpha(Color32::from_white_alpha(112), pop));
    } else if sh > 0.0 {
        ui.painter().circle_filled(s.rect.center(), MARK / 2.0, theme::glass(0.16 * sh));
    }
    let size = 13.0 * (1.0 + 0.22 * (pop * std::f32::consts::PI).sin());
    glyphs::draw(ui, s.rect.center(), size, glyphs::STAR, theme::lerp_color(theme::lerp_color(theme::with_alpha(Color32::WHITE, 0.78), Color32::WHITE, sh), theme::shade(0.86), pop));
    if pop > 0.0 {
        glyphs::draw(ui, s.rect.center(), size, glyphs::STAR_FILLED, theme::with_alpha(app.accent_color(), pop));
    }
    if s.clicked() {
        app.actions.push(Action::Like(t.clone(), !liked));
    }
    let m = disc(ui, pos2(right - MARK / 2.0, cy), "menu", "Song options");
    let mh = theme::anim_bool(&ctx, m.id.with("h"), m.hovered(), 0.12);
    ui.painter().circle_filled(m.rect.center(), MARK / 2.0, theme::glass(0.16 + 0.12 * mh));
    glyphs::draw(ui, m.rect.center(), 13.0, glyphs::ELLIPSIS, theme::lerp_color(theme::with_alpha(Color32::WHITE, 0.78), Color32::WHITE, mh));
    let mut acts: Vec<Action> = Vec::new();
    // YTM_MENU holds this open for a screenshot, the way the pill's own ⋯ does: the card is
    // far too short for the song menu, and its placement is the thing worth looking at.
    let popup = egui::Popup::menu(&m);
    let popup = if super::dev_var("YTM_MENU").is_some() { popup.open(true) } else { popup };
    widgets::menu_popup(popup, widgets::MENU_W, |ui| widgets::song_menu(ui, &t, liked, None, None, &mut acts));
    app.actions.extend(acts);
}

/// The seek line and its clock, over the sleeve's blur: a rounded bar with a knob, the
/// time sung at its left, what the song costs in quality in the middle, and what's left
/// — as a minus — at its right.
fn seek_row(app: &mut App, ui: &mut Ui, col: Rect, track: Option<&crate::model::Track>) {
    let (total, smooth, held) = seek_state(app, track);
    seek_slider(app, ui, col.left() + PAD, col.right() - PAD, col.bottom() - SEEK, total, smooth, held);
    if total > 0.0 {
        clock_row(app, ui, col.left() + PAD, col.right() - PAD, col.bottom() - CLOCK, total, smooth);
    }
}

/// Where the song is and how long it is: an Automix handoff keeps the outgoing song's own
/// clock until the next one is the louder of the two.
fn seek_state(app: &App, track: Option<&crate::model::Track>) -> (f32, f32, bool) {
    let status = app.audio.status.clone();
    let hold = app.mix_hold().map(|h| (h.total, app.mix_hold_pos(h)));
    let total = match &hold {
        Some((total, _)) => *total,
        None => status.duration().map(|d| d.as_secs_f32()).or(track.and_then(|t| t.duration).map(|d| d as f32)).unwrap_or(0.0),
    };
    let smooth = match &hold {
        Some((_, pos)) => *pos,
        None => status.position_smooth().as_secs_f32(),
    };
    (total, smooth, hold.is_some())
}

/// The seek line itself: a glass track with the played part in white and a knob on its end
/// while the song runs. A drag holds the position until the hand lets go, and only then
/// asks the player to move.
fn seek_slider(app: &mut App, ui: &mut Ui, l: f32, r: f32, cy: f32, total: f32, smooth: f32, held: bool) {
    let ctx = ui.ctx().clone();
    let pos = app.seek_drag.unwrap_or(smooth).min(total.max(0.0));
    let hit = Rect::from_min_max(pos2(l, cy - 10.0), pos2(r, cy + 10.0));
    let resp = ui.interact(hit, ui.scope_id().with("mini-seek"), if held { Sense::hover() } else { Sense::click_and_drag() });
    let live = total > 0.0 && !held;
    let resp = if live { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
    resp.widget_info(|| egui::WidgetInfo::slider(live, pos as f64, "Seek"));
    let h = theme::anim_bool(&ctx, "mini-seek-h", live && (resp.hovered() || resp.dragged()), 0.15);
    let thick = 5.0 + 2.0 * h;
    let line = Rect::from_min_max(pos2(l, cy - thick / 2.0), pos2(r, cy + thick / 2.0));
    ui.painter().rect_filled(line, thick / 2.0, theme::glass(0.26));
    let frac = if total > 0.0 { pos / total } else { 0.0 };
    ui.painter().rect_filled(Rect::from_min_max(line.min, pos2(line.left() + line.width() * frac, line.bottom())), thick / 2.0, Color32::WHITE);
    if live {
        ui.painter().circle_filled(pos2(line.left() + line.width() * frac, cy), 6.5 + 1.5 * h, Color32::WHITE);
    }
    let at = |p: Pos2| ((p.x - l) / (r - l).max(1.0)).clamp(0.0, 1.0) * total;
    if live
        && let Some(p) = resp.interact_pointer_pos()
    {
        if resp.dragged() {
            app.seek_drag = Some(at(p));
        }
        if resp.drag_stopped() || resp.clicked() {
            app.seek_drag = None;
            app.actions.push(Action::Seek(at(p)));
        }
    }
}

/// The clock: where the song is at the left, what is left of it as a minus at its right,
/// and the quality mark between the two.
fn clock_row(app: &mut App, ui: &mut Ui, l: f32, r: f32, y: f32, total: f32, smooth: f32) {
    let pos = app.seek_drag.unwrap_or(smooth).min(total.max(0.0));
    let ink = theme::with_alpha(Color32::WHITE, 0.75);
    let font = theme::regular(11.5);
    text_at(ui, pos2(l, y - 7.5), &fmt_time(pos as u32), font.clone(), ink, 90.0);
    let left = format!("-{}", fmt_time((total - pos).max(0.0) as u32));
    let g = widgets::galley(ui, &left, font, ink, 90.0);
    ui.painter().galley(pos2(r - g.size().x, y - 7.5), g, ink);
    let w = super::nowplaying::audio_badge_width(ui, app, true);
    super::nowplaying::audio_badge(app, ui, pos2((l + r) / 2.0 - w / 2.0, y - 6.0), true);
}

/// The transport along the card's foot: shuffle, previous, play, next, repeat, spread
/// across it at Apple's sizes. An engaged shuffle or repeat stands on the same lit disc
/// the player's corner toggles use.
fn transport(app: &mut App, ui: &mut Ui, col: Rect, track: &Option<crate::model::Track>, now: f64) {
    let ctx = ui.ctx().clone();
    let cy = col.bottom() - TRANSPORT;
    let enabled = track.is_some();
    // Apple's row nearly fills the card; on a wide one it stops short of the edges.
    let span = (col.width() - 2.0 * (PAD + 14.0)).min(330.0);
    let x = |f: f32| col.center().x - span / 2.0 + span * f;
    let ink = |h: f32| {
        let c = theme::lerp_color(theme::with_alpha(Color32::WHITE, 0.68), Color32::WHITE, h);
        if enabled { c } else { c.gamma_multiply(0.45) }
    };
    let scope = ui.scope_id();
    let btn = |ui: &mut Ui, cx: f32, s: f32, key: &str, name: &str| -> egui::Response {
        let r = Rect::from_center_size(pos2(cx, cy), Vec2::splat(s * 2.0));
        let resp = ui.interact(r, scope.with(("mini-t", key)), Sense::click());
        named(resp, name).on_hover_text(name).on_hover_cursor(egui::CursorIcon::PointingHand)
    };
    let r = btn(ui, x(0.10), 17.0, "shuffle", "Shuffle");
    let h = theme::anim_bool(&ctx, r.id.with("h"), r.hovered(), 0.12);
    let on = theme::anim_bool(&ctx, "mini-shuffle", app.queue.shuffled, 0.2);
    let tint = super::nowplaying::lit_mark(app, on, ink(h));
    glyphs::draw(ui, r.rect.center(), 18.0, glyphs::SHUFFLE, tint);
    if r.clicked() {
        app.toggle_shuffle();
    }
    let r = btn(ui, x(0.30), 17.0, "prev", "Previous");
    let h = theme::anim_bool(&ctx, r.id.with("h"), r.hovered(), 0.12);
    glyphs::draw(ui, r.rect.center(), 20.0, glyphs::PREVIOUS, ink(h));
    if r.clicked() && enabled {
        app.prev();
    }
    let playing = app.playing();
    let r = btn(ui, x(0.50), 19.0, "play", if playing { "Pause" } else { "Play" });
    let h = theme::anim_bool(&ctx, r.id.with("h"), r.hovered(), 0.15);
    let c = r.rect.center();
    let morph = theme::anim_bool(&ctx, "mini-play-morph", playing || app.buffering, 0.18);
    let fg = ink(0.55 + 0.45 * h);
    if app.buffering {
        let a = now as f32 * 6.0;
        let pts: Vec<Pos2> = (0..20).map(|i| c + vec2((a + i as f32 * 0.22).cos(), (a + i as f32 * 0.22).sin()) * 8.0).collect();
        ui.painter().add(egui::Shape::Path(egui::epaint::PathShape::line(pts, egui::epaint::PathStroke::new(2.0, fg))));
        ctx.request_repaint_after(Duration::from_millis(33));
    } else if morph < 1.0 {
        glyphs::draw(ui, c, 26.0, glyphs::PLAY, theme::with_alpha(fg, 1.0 - morph));
    }
    if morph > 0.0 {
        glyphs::draw(ui, c, 26.0, glyphs::PAUSE, theme::with_alpha(fg, morph));
    }
    if r.clicked() && enabled {
        app.toggle();
    }
    let r = btn(ui, x(0.70), 17.0, "next", "Next");
    let h = theme::anim_bool(&ctx, r.id.with("h"), r.hovered(), 0.12);
    glyphs::draw(ui, r.rect.center(), 20.0, glyphs::NEXT, ink(h));
    if r.clicked() && enabled {
        app.next();
    }
    let (glyph, on) = match app.queue.repeat {
        Repeat::Off => (glyphs::REPEAT, false),
        Repeat::All => (glyphs::REPEAT, true),
        Repeat::One => (glyphs::REPEAT_ONE, true),
    };
    let r = btn(ui, x(0.90), 17.0, "repeat", "Repeat");
    let h = theme::anim_bool(&ctx, r.id.with("h"), r.hovered(), 0.12);
    let ron = theme::anim_bool(&ctx, "mini-repeat", on, 0.2);
    let tint = super::nowplaying::lit_mark(app, ron, ink(h));
    glyphs::draw(ui, r.rect.center(), 18.0, glyph, tint);
    if r.clicked() {
        app.queue.repeat = match app.queue.repeat {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        };
    }
}

/// The card's chrome: the window's own lights top left, and a frosted capsule top right
/// carrying the volume. They fade in while the pointer is on the card (or one of them has
/// the keyboard), so the sleeve gets the whole card to itself.
fn chrome(app: &mut App, ui: &mut Ui, r: Rect, col: Rect, cap_r: f32, cap_w: f32) {
    let ctx = ui.ctx().clone();
    let scope = ui.scope_id();
    // By a key, not the label: the pin's changes, and its focus has to stay.
    let btn_id = move |key: &str| scope.with(("mini-btn", key));
    let focused = ctx.memory(|m| m.focused()).is_some_and(|f| ["close", "full", "pin", "volume"].iter().any(|k| btn_id(k) == f));
    let over = ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| r.contains(p));
    let reveal = theme::anim_bool(&ctx, "mini-btns", over || focused, 0.15);
    let hover = |on: Rect| ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| on.contains(p));
    // The capsule sits top right, and the window's own buttons stay top left.
    let cy = col.top() + 24.0;
    let by = cy;

    // The volume's track slides out of the capsule's end, and holds itself open while the
    // pointer travels along it to the knob.
    let vol = app.audio.volume();
    let open_id = scope.with("mini-vol-open");
    let was_open = ui.data(|d| d.get_temp::<bool>(open_id).unwrap_or(false));
    let speaker = Rect::from_center_size(pos2(cap_r - 4.0 - CAP_SLOT / 2.0, cy), Vec2::splat(CAP_SLOT));
    let band = Rect::from_min_max(pos2(speaker.left() - VOL_TRACK - 8.0, cy - 22.0), pos2(speaker.right(), cy + 22.0));
    let open = app.show_volume && (hover(speaker) || (was_open && (hover(band) || ctx.input(|i| i.pointer.any_down()))));
    ui.data_mut(|d| d.insert_temp(open_id, open));
    if open != was_open {
        ctx.request_repaint();
    }
    let grow = VOL_TRACK * theme::anim_bool(&ctx, "mini-vol-reveal", open, 0.15);
    if app.show_volume {
        let pill = Rect::from_min_max(pos2(cap_r - cap_w - grow, cy - CAP_SLOT / 2.0), pos2(cap_r, cy + CAP_SLOT / 2.0));
        ui.painter().rect_filled(pill, pill.height() / 2.0, theme::glass(0.18).gamma_multiply(reveal));
    }
    if grow > 30.0 {
        let t = Rect::from_min_max(pos2(cap_r - cap_w - grow + 16.0, cy - 2.5), pos2(speaker.left() - 15.0, cy + 2.5));
        let sresp = named(ui.interact(t.expand2(vec2(0.0, 9.0)), scope.with("mini-vol-slider"), Sense::click_and_drag()), "Volume").on_hover_cursor(egui::CursorIcon::PointingHand);
        let sh = theme::anim_bool(&ctx, sresp.id.with("h"), sresp.hovered() || sresp.dragged(), 0.12);
        sresp.widget_info(|| egui::WidgetInfo::slider(true, vol as f64, "Volume"));
        ui.painter().rect_filled(t, 2.5, theme::with_alpha(Color32::WHITE, 0.28));
        let fw = t.width() * vol;
        ui.painter().rect_filled(Rect::from_min_max(t.min, pos2(t.left() + fw, t.bottom())), 2.5, Color32::WHITE.gamma_multiply(reveal));
        ui.painter().circle_filled(pos2(t.left() + fw, cy), 6.5 + 1.5 * sh, Color32::WHITE.gamma_multiply(reveal));
        if let Some(p) = sresp.interact_pointer_pos()
            && (sresp.clicked() || sresp.dragged())
        {
            app.audio.set_volume(((p.x - t.left()) / t.width()).clamp(0.0, 1.0));
        }
    }

    // The speaker mutes on a click, and the wheel over its slot or the track changes it.
    if app.show_volume {
        let v = named(ui.interact(speaker, btn_id("volume"), Sense::click()), "Mute")
            .on_hover_text(format!("Volume {}%", (vol * 100.0).round()))
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let vh = theme::anim_bool(&ctx, v.id.with("h"), v.hovered() || v.dragged(), 0.12);
        glyphs::draw(ui, speaker.center(), 17.0, glyphs::VOLUME[glyphs::volume_step(vol)], theme::with_alpha(Color32::WHITE, 0.75 + 0.25 * vh).gamma_multiply(reveal.max(vh)));
        if v.hovered() {
            let scroll = ctx.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                app.audio.set_volume((vol + scroll / 600.0).clamp(0.0, 1.0));
            }
        }
        if v.clicked() {
            super::shell::toggle_mute(app, &ctx);
        }
        if v.has_focus() {
            ui.painter().circle_stroke(speaker.center(), CAP_SLOT / 2.0 - 2.0, egui::Stroke::new(1.5, Color32::WHITE));
        }
    }

    // One corner button: a disc that only arrives with the pointer, and a glyph with it.
    // Apple's own card puts coloured lights here; these are the same three jobs — close,
    // full window, pin — without the traffic colours.
    let button = |ui: &mut Ui, c: Pos2, glyph: char, key: &str, name: &str, on: bool| -> bool {
        let b = Rect::from_center_size(c, Vec2::splat(24.0));
        let resp = named(ui.interact(b.expand(4.0), btn_id(key), Sense::click()), name).on_hover_text(name).on_hover_cursor(egui::CursorIcon::PointingHand);
        let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered() || resp.is_pointer_button_down_on(), 0.12);
        let lit = if on { 1.0 } else { 0.0 };
        if h > 0.0 || lit > 0.0 {
            ui.painter().circle_filled(b.center(), 12.0, theme::glass((0.16 + 0.12 * h).max(0.10 * lit)).gamma_multiply(reveal.max(h).max(lit)));
        }
        let fg = theme::lerp_color(theme::with_alpha(Color32::WHITE, 0.72), Color32::WHITE, h.max(lit));
        let size = if resp.is_pointer_button_down_on() { 11.0 } else { 12.5 };
        // An engaged button keeps its glyph while the row is faded out: the disc alone,
        // with nothing on it, reads as a smudge rather than as the pin being on.
        icon_at(ui, b.center(), glyph, size, fg.gamma_multiply(reveal.max(h).max(lit)));
        if resp.has_focus() {
            ui.painter().circle_stroke(b.center(), 12.0, egui::Stroke::new(1.5, Color32::WHITE));
        }
        // A click that landed on the button counts even before the reveal finishes; only
        // a press the drag area already claimed stays a window move.
        resp.clicked() && (reveal > 0.5 || resp.has_focus() || resp.hovered())
    };

    // The window's own buttons, top left: the × closes the card, never the app, and the
    // next opens the full window. A system frame brings its own ×, so elsewhere the row
    // starts with that one.
    let custom = titlebar::custom();
    let lx = |i: usize| col.left() + KEY_X + 28.0 * i as f32;
    let mut i = 0;
    if custom && button(ui, pos2(lx(0), by), icon::X, "close", "Close mini player", false) {
        app.actions.push(Action::Mini(false));
    }
    if custom {
        i = 1;
    }
    if button(ui, pos2(lx(i), by), icon::MAXIMIZE_2, "full", "Full window (Ctrl+M)", false) {
        app.actions.push(Action::Mini(false));
    }

    // The pin, last of the row and on a layer of its own: the whole card is a drag area
    // that moves the window, and a press it takes never reaches the button.
    let pinned = app.mini.pinned;
    let at = pos2(lx(i + 1), by);
    let mut pin_clicked = false;
    egui::Area::new(scope.with("mini-pin")).order(egui::Order::Middle).fixed_pos(at).show(&ctx, |ui| {
        let name = if pinned { "Stop keeping on top" } else { "Keep on top" };
        // The glyph is what the click does: pinned, it offers to come off the top.
        let glyph = if pinned { icon::PIN_OFF } else { icon::PIN };
        pin_clicked = button(ui, at, glyph, "pin", name, pinned);
    });
    if pin_clicked {
        app.mini.set_pinned(&ctx, !pinned);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_place_is_the_work_areas_corner() {
        let area = Rect::from_min_max(pos2(0.0, 0.0), pos2(2560.0, 1400.0));
        let p = corner_place(area, Vec2::ZERO, 1.0, 1.0);
        assert_eq!((p.x, p.y, p.w, p.h), (2560.0 - SIZE.x - 16.0, 1400.0 - SIZE.y - 16.0, SIZE.x, SIZE.y));
        // Scaled: pixels grow with the scale, points don't.
        let p = corner_place(area, vec2(0.0, 0.0), 1.5, 1.0);
        assert_eq!((p.x, p.y, p.w), (2560.0 - SIZE.x * 1.5 - 24.0, 1400.0 - SIZE.y * 1.5 - 24.0, SIZE.x));
        // A second monitor left of the main one has negative coordinates.
        let left = Rect::from_min_max(pos2(-1920.0, 100.0), pos2(0.0, 1140.0));
        let p = corner_place(left, vec2(2.0, 2.0), 1.0, 1.0);
        assert_eq!((p.x, p.y), (-SIZE.x - 2.0 - 16.0, 1140.0 - SIZE.y - 2.0 - 16.0));
        // Never past the area's top left, however small it is.
        let tiny = Rect::from_min_max(pos2(10.0, 20.0), pos2(200.0, 50.0));
        let p = corner_place(tiny, Vec2::ZERO, 1.0, 1.0);
        assert_eq!((p.x, p.y), (10.0, 20.0));
        // Zoomed in, a point is bigger: the strip still ends at the margin.
        let p = corner_place(area, Vec2::ZERO, 1.0, 2.0);
        assert_eq!((p.x, p.y, p.zoom), (2560.0 - SIZE.x * 2.0 - 16.0, 1400.0 - SIZE.y * 2.0 - 16.0, 2.0));
    }

    #[test]
    fn a_lost_full_window_is_centred() {
        let area = Rect::from_min_max(pos2(1920.0, 0.0), pos2(3840.0, 1040.0));
        let p = centered(area, vec2(1200.0, 780.0), Vec2::ZERO, 1.0, 1.0);
        assert_eq!((p.x, p.y, p.w, p.h), (1920.0 + 360.0, 130.0, 1200.0, 780.0));
        // Bigger than the area: its top left stays on it.
        let p = centered(area, vec2(1000.0, 800.0), Vec2::ZERO, 2.0, 1.0);
        assert_eq!((p.x, p.y), (1920.0, 0.0));
    }

    #[test]
    fn remembered_sizes_stay_in_limits() {
        let p = Place { x: 5.0, y: 6.0, w: 2000.0, h: 300.0, scale: 1.25, zoom: 1.0 };
        assert_eq!(clamp_size(p), Place { w: MAX_W, h: SIZE.y, ..p });
        assert_eq!(clamp_size(Place { w: 10.0, ..p }).w, MIN_W);
        // The strip has one height, whatever was remembered.
        assert_eq!(clamp_size(Place { w: 10.0, h: 900.0, ..p }), Place { w: MIN_W, h: SIZE.y, ..p });
    }

    #[test]
    fn full_window_is_saved_in_eframes_format() {
        let p = Place { x: 814.0, y: -183.0, w: 1200.0, h: 812.0, scale: 1.0, zoom: 1.0 };
        assert_eq!(
            p.eframe_window(false),
            "(normal_geometry:(outer_position_pixels:Some((x:814.0,y:-183.0)),scale_factor:1.0,inner_size_points:(x:1200.0,y:812.0)),fullscreen:false,maximized:false)"
        );
        let s = p.eframe_window(true);
        assert!(s.ends_with("maximized:true)"));
    }

    #[test]
    fn places_round_trip_through_settings() {
        let p = Place { x: -1500.0, y: 900.0, w: SIZE.x, h: SIZE.y, scale: 1.5, zoom: 1.25 };
        let back: Place = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
        // Saved before places had a zoom: unzoomed.
        let old: Place = serde_json::from_str(r#"{"x":1.0,"y":2.0,"w":400.0,"h":112.0,"scale":1.0}"#).unwrap();
        assert_eq!(old.zoom, 1.0);
        // A full window kept at 100% comes back the same size in pixels at 125%.
        assert_eq!(Place { w: 1250.0, h: 1000.0, zoom: 1.0, ..p }.size_at(1.25), vec2(1000.0, 800.0));
        let m = Mini::load(|k| (k == "mini_window").then(|| serde_json::to_string(&p).unwrap()));
        assert_eq!(m.place, Some(p));
        // A card remembered from when it did resize comes back as the square it is now.
        let wide = Place { w: 420.0, ..p };
        let m = Mini::load(|k| (k == "mini_window").then(|| serde_json::to_string(&wide).unwrap()));
        assert_eq!(m.place.map(|q| (q.w, q.h)), Some((SIZE.x, SIZE.y)));
        assert_eq!(m.pinned, PINNED_BY_DEFAULT);
        let m = Mini::load(|k| (k == "mini_pinned").then(|| "0".to_owned()));
        assert!(!m.pinned && m.place.is_none() && !m.active());
    }
}
