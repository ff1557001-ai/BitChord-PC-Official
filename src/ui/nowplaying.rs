//! Now-playing overlay: big art + synced lyrics / up next, over the full-strength
//! animated background. The lyrics themselves are drawn by `karaoke` (a port of the
//! better-lyrics renderer); this adds their toolbar and the plain-text fallback.

use egui::{Color32, Rect, Sense, Ui, Vec2, pos2, vec2};

use super::widgets::{self, named, text_at};
use std::time::Duration;

use super::{Action, App, MotionView, NpTab, Repeat, Step, Target, glyphs, karaoke};
use crate::lyrics::{Lyrics, Sync, SyncKind};
use crate::shader::Params;
use crate::theme::{self, icon};

/// Where the player remembers the last song whose lyrics were asked for by name.
fn forced_id() -> egui::Id {
    egui::Id::new("np-lyrics-forced")
}

/// Was this song's panel asked for, even though it has nothing to show? The mark is the
/// song's id, so a skip drops it.
fn asked(app: &App, ctx: &egui::Context) -> bool {
    app.current_id().is_some_and(|id| ctx.data(|d| d.get_temp::<String>(forced_id())).is_some_and(|saved| saved == id))
}

/// The panel actually on screen: a song without lyrics keeps the lyrics tab out of it, so
/// the artwork shows instead, until the panel is asked for anyway.
fn view(app: &App, ctx: &egui::Context) -> NpTab {
    if app.np_tab == NpTab::Lyrics && app.no_lyrics() && !asked(app, ctx) { NpTab::Art } else { app.np_tab }
}

/// Open the lyrics panel and keep it there, even for a song with nothing to show: its
/// "no lyrics" note is the answer. The next song forgets the ask, so a song that does have
/// lyrics opens the panel by itself.
pub(super) fn ask_lyrics(app: &mut App, ctx: &egui::Context) {
    force_lyrics(app, ctx);
    app.np_tab = NpTab::Lyrics;
    app.np_open = true;
}

/// Ask for the current song's words without opening the player over the page: the
/// drawer beside it shows them instead.
pub(super) fn force_lyrics(app: &mut App, ctx: &egui::Context) {
    if let Some(id) = app.current_id().map(str::to_owned) {
        ctx.data_mut(|d| d.insert_temp(forced_id(), id));
    }
}

/// Is the lyrics panel the one showing?
pub(super) fn lyrics_shown(app: &App, ctx: &egui::Context) -> bool {
    app.np_open && view(app, ctx) == NpTab::Lyrics
}

pub fn overlay(app: &mut App, ctx: &egui::Context, now: f64) {
    use crate::preset::BgMode;
    let t = theme::anim_bool(ctx, "np-open", app.np_open && app.current().is_some(), 0.42);
    if t <= 0.001 {
        return;
    }
    let screen = ctx.content_rect();
    // The player brings its own transport, so it takes the whole window: the capsule
    // under it belongs to the pages it covers.
    let full = screen;
    let rect = full.translate(vec2(0.0, full.height() * (1.0 - t)));
    if app.video_on {
        picture(app, ctx, rect);
    }
    // Once the clip has given a frame it is the player's whole backdrop; until then, and
    // for a song with no video, the preset's own background carries it.
    let picture = if app.video_on { app.video.frame(ctx) } else { None };
    let look = app.look();
    let b = &look.background;
    egui::Area::new(egui::Id::unique("now-playing"))
        .order(egui::Order::Middle)
        .fixed_pos(rect.min)
        .show(ctx, |ui| {
            // Swallow input so the page underneath doesn't react.
            ui.allocate_rect(rect, Sense::click_and_drag());
            // It covers the window's top too: that still moves the window.
            if !app.fullscreen {
                super::titlebar::drag_area(ui, Rect::from_min_size(rect.min, vec2(rect.width(), super::titlebar::HEIGHT)), "now-playing");
            }
            let painter = ui.painter_at(rect);
            let art = matches!(b.mode, BgMode::Kawarp | BgMode::CoverStatic);
            if let Some(clip) = picture.as_ref() {
                fill(ui, rect, clip, Color32::WHITE);
            } else if b.mode == BgMode::CoverFullbleed {
                // The cover itself fills the screen behind a scrim. With the lyrics-only
                // layout on a wide window it fills the height on the left instead and
                // fades out toward the lyrics (reCover).
                let solid = look.solid();
                painter.rect_filled(rect, 0.0, solid);
                if let Some(track) = app.shown() {
                    let side_by_side = look.cover.layout == crate::preset::Layout::LyricsOnly && rect.width() >= 900.0;
                    let area = if side_by_side { Rect::from_min_size(rect.min, Vec2::splat(rect.height())) } else { rect };
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area));
                    child.set_clip_rect(area);
                    widgets::paint_cover_fill(&child, area, &track.thumbs, Color32::from_gray((b.np_dim.clamp(0.0, 1.0) * 255.0) as u8));
                    if side_by_side {
                        let fade = Rect::from_min_max(pos2(area.center().x - area.width() * 0.1, area.top()), area.max);
                        super::shell::hgradient(&painter, fade, Color32::TRANSPARENT, solid);
                    }
                }
            } else if let Some(bg) = app.bg.as_ref().filter(|_| art && !app.disable_blur) {
                let blend = theme::ease_in_out(((now - app.art_changed) / 1.4) as f32);
                let level = if app.reactive_bg && app.playing() { app.audio.status.level() * b.reactive } else { 0.0 };
                bg.paint(
                    &painter,
                    rect,
                    Params {
                        time: app.bg_time as f32,
                        blend,
                        intensity: if b.mode == BgMode::CoverStatic { 0.0 } else { b.intensity },
                        saturation: b.saturation,
                        dim: b.np_dim,
                        scale: b.scale + level.min(0.5) * 0.12,
                    },
                );
            } else if b.mode == BgMode::Solid {
                // Full screen wears the page's colour rather than covering it: a flat
                // fill here would hide the theme the window is already showing.
                let solid = if app.fullscreen { theme::with_alpha(look.solid(), 0.55) } else { look.solid() };
                painter.rect_filled(rect, 0.0, solid);
            } else {
                // The same in full screen for a preset with no artwork background.
                let ground = theme::lerp_color(theme::bg(), app.accent_color(), 0.25);
                painter.rect_filled(rect, 0.0, if app.fullscreen { theme::with_alpha(ground, 0.55) } else { ground });
            }
            let k = b.scrim / 0.3;
            super::shell::vgradient(ui.painter(), rect, theme::shade((0.18 * k).min(0.95)), theme::shade((0.42 * k).min(0.95)));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(32.0, 20.0))));
            body(app, &mut child, now, picture.is_some());
        });
}

/// The video's picture, kept with the song: YouTube Music is asked for its video, the answer is
/// taken up, and the clip starts where the music already is and is held there. This runs
/// every frame the player is up, whether or not the picture has arrived yet: a whole clip
/// is downloaded before it gives a frame, and the preset's background carries the player
/// until it does.
fn picture(app: &mut App, ctx: &egui::Context, rect: Rect) {
    let Some(track) = app.current().cloned() else { app.video = Default::default(); return };
    if app.video.asked.as_deref() != Some(track.id.as_str()) {
        // A new song: the last one's picture ends here and this one's is asked for.
        app.video = Default::default();
        app.video.asked = Some(track.id.clone());
        // Physical width, capped: the file comes down whole before a frame shows, so a
        // bigger picture is mostly a longer wait for one. The HD setting lifts the cap
        // to 1080p for those who would rather wait.
        let cap = if app.video_hd { 1920.0 } else { 1280.0 };
        let width = (rect.width() * ctx.pixels_per_point()).clamp(640.0, cap) as u32;
        app.backend.request(crate::backend::Req::MusicVideo { key: track.id.clone(), track, width, skip: app.sponsorblock });
        return;
    }
    let at = app.audio.status.position_smooth();
    // The listener moving the song is not the picture falling behind: the chase starts
    // over, and a clip that had given up is asked for the song from its new place. A song
    // YouTube Music has no clip of stays without one — there is nothing to try again.
    if app.video.seeked(at) && app.video.stream.is_some() {
        (app.video.none, app.video.anchors, app.video.missed) = (false, 0, None);
    }
    if app.video.none {
        // Either YouTube Music has nothing for this song, or its clip was chased three
        // times and still wouldn't sit with the music. The artwork carries the player.
        return;
    }
    let Some(stream) = app.video.stream.clone() else { return };
    // Where the song's place in the music is inside the clip. YouTube Music's video of a
    // song usually opens with the intro the song itself doesn't have, so the two clocks
    // only agree once the marks between them are added back.
    let clip_at = Duration::from_secs_f64(crate::sponsorblock::video_time(&stream.skip, at.as_secs_f64()));
    let (playing, running) = (app.playing(), app.video.view.is_some() && app.video.id.as_deref() == Some(track.id.as_str()));
    // The picture waits with the song, and the clip's own clock is what the two are
    // compared on.
    if let Some(view) = app.video.view.as_ref() {
        view.motion.set_pause(!playing);
    }
    let clip = app.video.view.as_ref().map_or(Duration::ZERO, |v| v.motion.position());
    let dead = app.video.view.as_ref().is_some_and(|v| v.motion.done() && v.motion.latest().is_none());
    match app.video.step(running, playing, clip, clip_at, dead) {
        Step::Stay => return,
        Step::End => {
            (app.video.view, app.video.id, app.video.none) = (None, None, true);
            app.video.miss();
            return;
        }
        Step::Again => app.video.anchors += 1,
        Step::Start => {}
    }
    // The file is already on disk by now or comes down again under the same name, so a
    // restart costs the skip through what the song has already played.
    // The decoder is told the size it is drawn at, not the size the stream was picked at:
    // a 1080p source is sharp where a 720p one had to be stretched, while the frames it
    // turns out and the pixels swizzled each one stay the size of the picture on screen.
    let decode_w = (rect.width() * ctx.pixels_per_point()).clamp(640.0, 1920.0) as u32;
    app.video.view = Some(MotionView::new(
        crate::motion::play_video(ctx, &stream.id, &stream.url, stream.hls.as_deref(), decode_w, clip_at),
        stream.width,
    ));
    app.video.id = Some(track.id.clone());
    app.video.anchor = clip_at;
    ctx.request_repaint();
}

/// A texture stretched to cover `rect`, its own aspect kept and the overflow cropped: the
/// video behind the player, and its out-of-focus twin behind the words.
fn fill(ui: &Ui, rect: Rect, clip: &widgets::MotionSprite, tint: Color32) {
    fill_part(ui, rect, rect, clip, tint);
}

/// The part of the picture that sits behind `rect`, when the whole of it covers `whole`
/// the way `fill` stretches it: the column of words wears the blur of the video behind
/// it, not of the frame's centre.
fn fill_part(ui: &Ui, rect: Rect, whole: Rect, clip: &widgets::MotionSprite, tint: Color32) {
    if !ui.is_rect_visible(rect) || rect.width() <= 0.0 || rect.height() <= 0.0 || clip.size.y <= 0.0 || whole.width() <= 0.0 || whole.height() <= 0.0 {
        return;
    }
    let (a, b) = (clip.size.x / clip.size.y, whole.width() / whole.height());
    let uv = if b > a {
        Rect::from_center_size(pos2(0.5, 0.5), vec2(1.0, a / b))
    } else {
        Rect::from_center_size(pos2(0.5, 0.5), vec2(b / a, 1.0))
    };
    let f = |v: f32, lo: f32, hi: f32| ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
    let (x0, x1) = (f(rect.left(), whole.left(), whole.right()), f(rect.right(), whole.left(), whole.right()));
    let (y0, y1) = (f(rect.top(), whole.top(), whole.bottom()), f(rect.bottom(), whole.top(), whole.bottom()));
    let sub = Rect::from_min_max(
        pos2(uv.min.x + (uv.max.x - uv.min.x) * x0, uv.min.y + (uv.max.y - uv.min.y) * y0),
        pos2(uv.min.x + (uv.max.x - uv.min.x) * x1, uv.min.y + (uv.max.y - uv.min.y) * y1),
    );
    ui.painter_at(rect).add(egui::epaint::RectShape::filled(rect, 0.0, tint).with_texture(clip.id, sub));
}

/// The video player's column of words: a touch wider than the sidebar's lyric pane, and
/// like that pane it runs the window's full height rather than starting under the header.
const VIDEO_PANEL_W: f32 = 420.0;

/// The player's own body. `video` is whether the music video's picture is on screen: the
/// layout is the video's while the clip is playing, and the player's own otherwise.
fn body(app: &mut App, ui: &mut Ui, now: f64, video: bool) {
    use crate::preset::{Layout, Side};
    // An Automix handoff keeps the outgoing song here until the next one is the louder
    // of the two (see `MixFrom`): its track, seek line and lyrics lag the queue's switch.
    let hold = app.mix_hold().map(|h| (h.total, h.mix, app.mix_hold_pos(h)));
    let Some(track) = app.current().cloned() else { return };
    let track = match &hold {
        Some(_) => app.shown().cloned().unwrap_or(track),
        None => track,
    };
    let heard = if hold.is_some() { track.clone() } else { app.playing_track().cloned().unwrap_or_else(|| track.clone()) };
    let look = app.look();
    let cs = &look.cover;
    let area = ui.max_rect();
    let shown_tab = view(app, ui.ctx());
    // The content is this area less its margins: a cover column beside the panel needs
    // 860 points, and below that the cover gives way to the panel.
    let narrow = area.width() - 64.0 < 860.0;
    // Video mode: the words take a column of their own out at the window's right edge,
    // running its full height the way the sidebar's lyric pane does, so the header's right
    // end, the transport and the picture all stop short of it.
    let full = area.expand2(vec2(32.0, 20.0));
    let vcol = Rect::from_min_max(pos2(full.right() - VIDEO_PANEL_W.min((full.width() * 0.45).max(240.0)), full.top()), full.max);
    // The side panel (lyrics / up next) slides in and out when its chip is toggled. A
    // song with no lyrics has no lyrics panel to show: it's the artwork instead, unless
    // the panel was asked for and its "no lyrics" note is what's wanted.
    let last_id = egui::Id::unique("np-last-panel");
    let panel_tab = if shown_tab == NpTab::Art {
        ui.ctx().data(|d| d.get_temp(last_id)).unwrap_or(NpTab::Lyrics)
    } else {
        ui.ctx().data_mut(|d| d.insert_temp(last_id, shown_tab));
        shown_tab
    };
    // Linear 0..1 over the transition; each part eases its own slice of it.
    let shown = ui.ctx().animate_bool_with_time(egui::Id::unique("np-panel"), shown_tab != NpTab::Art, 0.6 * theme::SPEED);
    if shown > 0.0 && shown < 1.0 {
        // Every frame while sliding, not the now-playing view's usual 30 fps.
        ui.ctx().request_repaint();
    }
    let ppp = ui.ctx().pixels_per_point();
    let p = shown;
    // Whichever is leaving slides and fades out over the first 45%, then the other
    // comes in over the last 70%, both easing out, in either direction.
    let (panel_vis, art_vis) = if shown_tab != NpTab::Art {
        (theme::ease_out((p - 0.3) / 0.7), 1.0 - theme::ease_out(p / 0.45))
    } else {
        let q = 1.0 - p;
        (1.0 - theme::ease_out(q / 0.45), theme::ease_out((q - 0.3) / 0.7))
    };
    // The cover gliding between its two places (wide layouts): a gentle S-curve.
    let glide = p * p * (3.0 - 2.0 * p);
    // Video mode: the player's own right edge is the words column's, and only while it
    // stands — with the words away the transport and the header controls keep the whole
    // window, the way they do without the video.
    let vedge = if video { egui::lerp(area.right()..=vcol.left() - 28.0, glide) } else { area.right() };
    // Header: on the window buttons' line, when the app draws them.
    let lift = 20.0 - f32::from(super::titlebar::top_margin(20, 44.0));
    let header = Rect::from_min_size(area.min - vec2(0.0, if app.fullscreen { 0.0 } else { lift }), vec2(area.width(), 44.0));
    let mut hui = ui.new_child(egui::UiBuilder::new().max_rect(header).layout(egui::Layout::left_to_right(egui::Align::Center)));
    // The left capsule closes the sheet or opens the mini player; volume and Sing sit
    // at the right, above every panel.
    let left = Rect::from_min_size(pos2(header.left(), header.center().y - 16.0), vec2(64.0, 32.0));
    capsule(&hui, left);
    let close = cap_slot(&mut hui, left, 0, "close", "Close now playing");
    let ch = theme::anim_bool(ui.ctx(), close.id.with("h"), close.hovered(), 0.12);
    glyphs::draw(&hui, close.rect.center(), 16.0, glyphs::CLOSE, theme::lerp_color(theme::text_dim(), theme::text(), ch));
    if close.clicked() {
        app.np_open = false;
        if app.fullscreen {
            app.set_fullscreen(ui.ctx(), false);
        }
    }
    let mini = cap_slot(&mut hui, left, 1, "mini", "Mini player");
    let mh = theme::anim_bool(ui.ctx(), mini.id.with("h"), mini.hovered(), 0.12);
    pip_mark(&hui, mini.rect.center(), 20.0, theme::lerp_color(theme::text_dim(), theme::text(), mh));
    if mini.clicked() {
        app.actions.push(Action::Mini(true));
    }
    let mut edge = vedge - super::titlebar::right_gap(ui.ctx(), ui.max_rect(), 0.0);
    if app.show_volume {
        let c = Rect::from_min_size(pos2(edge - 168.0, header.center().y - 18.0), vec2(168.0, 36.0));
        volume_pill(app, &mut hui, c);
        edge = c.left() - 10.0;
    }
    let sing = Rect::from_center_size(pos2(edge - 19.0, header.center().y), Vec2::splat(38.0));
    sing_button(app, &mut hui, sing);
    edge = sing.left() - 10.0;
    // The song's actions stand in up here only when the cover column is hidden — a
    // narrow window showing the panel alone; otherwise they live under the sleeve.
    let cover_hidden = !video && (narrow || cs.layout == Layout::LyricsOnly) && shown_tab != NpTab::Art;
    if cover_hidden {
        options_menu(app, &mut hui, Rect::from_center_size(pos2(edge - 17.0, header.center().y), Vec2::splat(34.0)), &heard);
    }

    let content = Rect::from_min_max(pos2(area.left(), header.bottom() + 12.0), pos2(vedge, area.bottom()));
    // Video mode: the video is the picture, so the columns keep their places whatever the preset
    // says — the sleeve's art is gone but its words and transport aren't, and the panel
    // takes the right.
    let lyrics_only = !video && (narrow || cs.layout == Layout::LyricsOnly);
    // reCover: the backdrop cover sits on the left, so the lyrics take the right side.
    let beside_cover = !video && !narrow && cs.layout == Layout::LyricsOnly && look.background.mode == crate::preset::BgMode::CoverFullbleed;
    let centered = lyrics_only && !beside_cover;
    // Artwork alone: centered.
    let col_art = Rect::from_center_size(content.center(), vec2(content.width().min(content.height() * 0.9), content.height()));
    let slide = 72.0;
    let right = if lyrics_only {
        // No cover column next to the panel: the artwork fades in from the left as the panel leaves.
        let side = art_side(cs, col_art);
        let px = (side * ppp) as u32;
        if art_vis > 0.001 {
            let mut cui = ui.new_child(egui::UiBuilder::new().max_rect(content));
            cui.multiply_opacity(art_vis);
            cover_column(app, &mut cui, col_art.translate(vec2(-slide * (1.0 - art_vis), 0.0)), &track, now, px, side, true);
        } else {
            // Hidden behind the lyrics: have it loaded for when they're turned off.
            widgets::preload_cover(ui.ctx(), &track.thumbs, px);
        }
        if beside_cover { Rect::from_min_max(pos2(content.left() + content.width() * 0.46, content.top()), content.max) } else { content }
    } else {
        // Cover column, on the preset's side; it glides to the center as the panel leaves.
        // Video mode: the clip is the picture and the words have a column of their own already cut
        // out of the right, so the transport's column takes all that is left.
        let panel_right = !video && cs.side == Side::Right;
        let frac = if video { 1.0 } else if cs.layout == Layout::CoverHero { 0.5 } else { 0.42 };
        let col_w = content.width() * frac;
        let col_panel = if panel_right {
            Rect::from_min_size(pos2(content.right() - col_w, content.top()), vec2(col_w, content.height()))
        } else {
            Rect::from_min_size(content.min, vec2(col_w, content.height()))
        };
        let (col, side) = if video {
            (col_panel, art_side(cs, col_art).min(art_side(cs, col_panel)))
        } else {
            (Rect::from_min_max(col_art.min.lerp(col_panel.min, glide), col_art.max.lerp(col_panel.max, glide)), art_side(cs, col_art).min(art_side(cs, col_panel)))
        };
        // One image size for the whole glide: the sleeve keeps its size when the panel
        // comes and goes, so it is measured from the narrower of the two columns.
        let px = (side * ppp) as u32;
        // The panel's own rectangle, known before anything stands in it: the backdrop and
        // the song's block both belong in front of the words, not under them.
        let panel_rect = if video {
            vcol
        } else if panel_right {
            Rect::from_min_max(content.min, pos2(col_panel.left() - 28.0, content.bottom()))
        } else {
            Rect::from_min_max(pos2(col_panel.right() + 28.0, content.top()), content.max)
        };
        if video && panel_vis > 0.001 && let Some(blur) = app.video.blurred(ui.ctx()) {
            // The words stand on the video itself, out of focus, so the column reads as
            // part of the picture rather than a card laid over it.
            let brect = panel_rect.translate(vec2(-slide * (1.0 - panel_vis), 0.0));
            fill_part(ui, brect, full, &blur, Color32::from_white_alpha((255.0 * panel_vis) as u8));
            // The scrim eases in from the picture rather than starting at an edge: the
            // canvas and the words blur into one another.
            let fade_w = 160.0f32.min(brect.width());
            let fade = Rect::from_min_max(brect.min, pos2(brect.left() + fade_w, brect.bottom()));
            super::shell::hgradient(ui.painter(), fade, Color32::TRANSPARENT, theme::shade(0.42 * panel_vis));
            let rest = Rect::from_min_max(pos2(brect.left() + fade_w, brect.top()), brect.max);
            if rest.width() > 0.0 {
                ui.painter().rect_filled(rest, 0.0, theme::shade(0.42 * panel_vis));
            }
        }
        if video {
            // The clip is the picture: the sleeve stays out and the words and transport
            // keep the place they have in the player itself.
            let mut cui = ui.new_child(egui::UiBuilder::new().max_rect(col));
            cover_column(app, &mut cui, col, &track, now, px, side, false);
        } else {
            cover_column(app, ui, col, &track, now, px, side, true);
        }
        panel_rect
    };
    if panel_vis > 0.001 {
        let toward = if !lyrics_only && (video || cs.side == Side::Right) { -slide } else { slide };
        let rect = right.translate(vec2(toward * (1.0 - panel_vis), 0.0));
        // Switching between Lyrics and Up next: the old one fades out over the first 55%,
        // the new one fades in and rises into place from a third of the way.
        for (tab, key) in [(NpTab::Lyrics, "np-tab-lyrics"), (NpTab::UpNext, "np-tab-upnext")] {
            let w = ui.ctx().animate_bool_with_time(egui::Id::unique(key), panel_tab == tab, 0.45 * theme::SPEED);
            if w > 0.0 && w < 1.0 {
                ui.ctx().request_repaint();
            }
            let vis = if panel_tab == tab {
                theme::ease_out((w - 0.35) / 0.65)
            } else {
                let x = ((w - 0.45) / 0.55).clamp(0.0, 1.0);
                x * x * (3.0 - 2.0 * x)
            };
            if vis <= 0.001 {
                continue;
            }
            let rise = if panel_tab == tab { 18.0 * (1.0 - vis) } else { 0.0 };
            let rect = rect.translate(vec2(0.0, rise));
            // Video mode: the column runs out to the window's own edge, so the words keep
            // the player's margin inside it and stop short of the corner toggles.
            let rect = if video { Rect::from_min_max(pos2(rect.left() + 28.0, rect.top()), pos2(rect.right() - 32.0, area.bottom() - 48.0)) } else { rect };
            // Up next stops short of the player's bottom edge: the corner toggles are
            // painted after it and would otherwise land on its last row's buttons.
            let r = if tab == NpTab::UpNext { Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - 44.0)) } else { rect };
            let mut rui = ui.new_child(egui::UiBuilder::new().max_rect(r));
            rui.set_clip_rect((if video { right } else { content }).intersect(ui.clip_rect()));
            rui.multiply_opacity(panel_vis * vis);
            match tab {
                NpTab::Lyrics => lyrics_panel(app, &mut rui, r, now, centered, karaoke::Surface::Player),
                _ => super::shell::queue_list(app, &mut rui, "np", false),
            }
        }
    }

    // The mode is on and YouTube Music has no clip for this song: the artwork carries
    // the player, and the reason the picture isn't coming stands under it for three
    // seconds before it fades out and leaves the artwork alone.
    let miss = if app.video_on && !video { app.video.missing() } else { 0.0 };
    if miss > 0.0 {
        let ink = theme::with_alpha(theme::text_dim(), miss);
        let g = widgets::galley(ui, "No music video for this song on YouTube Music", theme::semibold(15.0), ink, 460.0);
        ui.painter().galley(pos2(content.center().x - g.size().x / 2.0, area.bottom() - 72.0), g, ink);
        ui.ctx().request_repaint_after(Duration::from_millis(200));
    }

    // Bottom right: ONE seamless capsule carrying the lyrics and the queue, as the
    // reference sets them. The panel that's showing wears a light disc which fills the
    // pill's height; the song's colour never enters either. Clicking the lit one returns
    // to the artwork alone.
    let ink = |h: f32| theme::lerp_color(theme::text_dim(), theme::text(), h);
    // Video mode: the capsule sits at the window's own bottom-right corner, under the
    // column of words it belongs to.
    let cap_x = if video { area.right() - 80.0 } else { vedge - 80.0 };
    let cap = Rect::from_min_size(pos2(cap_x, area.bottom() - 40.0), vec2(72.0, 36.0));
    capsule(ui, cap);
    let corner = |ui: &mut Ui, i: usize, key: &str, name: &str| -> egui::Response {
        let r = Rect::from_min_size(pos2(cap.left() + 36.0 * i as f32, cap.top()), vec2(36.0, 36.0));
        let resp = ui.interact(r, ui.scope_id().with(("np-corner", key)), Sense::click());
        named(resp, name).on_hover_text(name).on_hover_cursor(egui::CursorIcon::PointingHand)
    };
    // The lit disc and the hover wash share one radius, so the mark never shifts.
    let disc = |ui: &Ui, center: egui::Pos2, on: bool, h: f32| {
        if on {
            ui.painter().circle_filled(center, 18.0, Color32::from_white_alpha(112));
        } else if h > 0.0 {
            ui.painter().circle_filled(center, 18.0, theme::glass(0.11 * h));
        }
    };
    let r = corner(ui, 0, "lyrics", "Lyrics");
    let h = theme::anim_bool(ui.ctx(), r.id.with("h"), r.hovered(), 0.15);
    let on = shown_tab == NpTab::Lyrics;
    disc(ui, r.rect.center(), on, h);
    glyphs::draw(ui, r.rect.center(), 18.0, if on { glyphs::LYRICS } else { glyphs::LYRICS_OFF }, if on { app.accent_color() } else { ink(h) });
    if r.clicked() {
        if on {
            app.np_tab = NpTab::Art;
        } else {
            ask_lyrics(app, ui.ctx());
        }
    }
    let r = corner(ui, 1, "queue", "Up next");
    let h = theme::anim_bool(ui.ctx(), r.id.with("h"), r.hovered(), 0.15);
    let on = app.np_tab == NpTab::UpNext;
    disc(ui, r.rect.center(), on, h);
    glyphs::draw(ui, r.rect.center(), 18.0, glyphs::QUEUE, if on { app.accent_color() } else { ink(h) });
    if r.clicked() {
        app.np_tab = if on { NpTab::Art } else { NpTab::UpNext };
    }
}

/// A frosted capsule in the player's chrome: one seamless pill its slots sit inside.
fn capsule(ui: &Ui, r: Rect) {
    ui.painter().rect_filled(r, r.height() / 2.0, theme::glass(0.14));
}

/// One of a header capsule's two equal cells.
fn cap_slot(ui: &mut Ui, cap: Rect, i: usize, key: &str, name: &str) -> egui::Response {
    let w = cap.width() / 2.0;
    let r = Rect::from_min_size(pos2(cap.left() + i as f32 * w, cap.top()), vec2(w, cap.height()));
    let resp = ui.interact(r, ui.scope_id().with(("np-cap", key)), Sense::click());
    named(resp, name).on_hover_text(name).on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Apple's picture-in-picture mark: a centered screen with a smaller inset at its lower
/// right. Drawn rather than carried as an asset — the icon font has no PiP glyph.
fn pip_mark(ui: &Ui, c: egui::Pos2, s: f32, tint: Color32) {
    let pen = (s * 0.075).max(1.1);
    let frame = Rect::from_center_size(c, vec2(s * 0.88, s * 0.68));
    ui.painter().rect_stroke(frame, s * 0.08, egui::Stroke::new(pen, tint), egui::StrokeKind::Inside);
    let card = Rect::from_min_max(c + vec2(-s * 0.03, -s * 0.04), c + vec2(s * 0.29, s * 0.19));
    ui.painter().rect_filled(card, s * 0.05, tint);
}

/// Apple's Sing mark toggles full vocal reduction from the player header. Drawn rather
/// than carried as an asset — the icon set has no mic-with-sparkles glyph.
fn sing_button(app: &mut App, ui: &mut Ui, rect: Rect) {
    let ctx = ui.ctx();
    let c = rect.center();
    let disc = named(ui.interact(rect, ui.scope_id().with("np-sing"), Sense::click()), "Sing")
        .on_hover_text(if app.sing { "Stop singing along" } else { "Sing along" })
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ctx, disc.id.with("h"), disc.hovered(), 0.15);
    // Idle wears the frosted disc the player's other chrome uses; engaged is the lit
    // one the corner toggles take.
    if app.sing {
        ui.painter().circle_filled(c, 19.0, Color32::from_white_alpha(112));
    } else {
        ui.painter().circle_filled(c, 19.0, theme::glass(0.16 + 0.12 * h));
    }
    sing_mark(ui, c, 23.0, theme::lerp_color(Color32::from_white_alpha(190), Color32::WHITE, h));
    if disc.clicked() {
        app.sing = !app.sing;
        app.sing_level = 1.0;
        app.set_sing();
    }
}

/// The mark: a hand-held mic tilted down to the left, four sparks about its head.
fn sing_mark(ui: &Ui, c: egui::Pos2, s: f32, tint: Color32) {
    let head = c + vec2(s * 0.20, -s * 0.24);
    let tail = c + vec2(-s * 0.20, s * 0.20);
    let pen = s * 0.10;
    ui.painter().line_segment([head, tail], egui::Stroke::new(pen, tint));
    // Rounded at both ends, as the reference's rod is.
    ui.painter().circle_filled(head, pen / 2.0, tint);
    ui.painter().circle_filled(tail, pen / 2.0, tint);
    ui.painter().circle_filled(head, s * 0.13, tint);
    for (dx, dy, r) in [(-0.10, -0.36, 0.10), (-0.36, -0.02, 0.13), (0.30, 0.17, 0.155), (0.02, 0.40, 0.09)] {
        sparkle(ui, c + vec2(s * dx, s * dy), s * r, tint);
    }
}

/// A four-pointed spark: straight arms, a deeply pinched waist. egui fans the polygon
/// from its centre, so the concave sides survive the triangulation.
fn sparkle(ui: &Ui, c: egui::Pos2, r: f32, tint: Color32) {
    let mut pts = Vec::with_capacity(8);
    for i in 0..8 {
        let a = i as f32 * std::f32::consts::FRAC_PI_4 - std::f32::consts::FRAC_PI_2;
        let rad = if i % 2 == 0 { r } else { r * 0.24 };
        pts.push(pos2(c.x + rad * a.cos(), c.y + rad * a.sin()));
    }
    ui.painter().add(egui::Shape::convex_polygon(pts, tint, egui::Stroke::NONE));
}

/// The player's volume, top right: a pill carrying the track, its knob and the speaker
/// at its end. The speaker mutes and unmutes; the wheel over the pill nudges the level.
fn volume_pill(app: &mut App, ui: &mut Ui, rect: Rect) {
    let ctx = ui.ctx();
    capsule(ui, rect);
    let vol = app.audio.volume();
    // A slim track, solid up to the knob and faint for what's left. Its larger hit
    // area keeps the knob and track easy to drag.
    let track = Rect::from_min_max(pos2(rect.left() + 18.0, rect.center().y - 2.0), pos2(rect.right() - 58.0, rect.center().y + 2.0));
    let resp = named(ui.interact(track.expand(8.0), ui.scope_id().with("np-vol"), Sense::click_and_drag()), "Volume").on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ctx, resp.id.with("h"), resp.hovered() || resp.dragged(), 0.15);
    ui.painter().rect_filled(track, 2.0, Color32::from_white_alpha(58));
    let fill = Rect::from_min_size(track.min, vec2(track.width() * vol, 4.0));
    ui.painter().rect_filled(fill, 2.0, theme::lerp_color(Color32::from_white_alpha(206), Color32::WHITE, h));
    let knob = Rect::from_center_size(pos2(track.left() + track.width() * vol, rect.center().y), vec2(16.0, 12.0));
    ui.painter().rect_filled(knob, 6.0, theme::lerp_color(Color32::from_white_alpha(235), Color32::WHITE, h));
    if let Some(p) = resp.interact_pointer_pos()
        && (resp.dragged() || resp.clicked())
    {
        app.audio.set_volume(((p.x - track.left()) / track.width()).clamp(0.0, 1.0));
    }
    let speaker = Rect::from_center_size(pos2(rect.right() - 26.0, rect.center().y), Vec2::splat(30.0));
    let sr = named(ui.interact(speaker, ui.scope_id().with("np-mute"), Sense::click()), "Mute").on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.hovered() || sr.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            app.audio.set_volume((vol + scroll / 600.0).clamp(0.0, 1.0));
        }
    }
    let sh = theme::anim_bool(ctx, sr.id.with("h"), sr.hovered(), 0.12);
    // The speaker's arcs light one at a time, as the strip's do: same mark, same material.
    super::glyphs::draw(ui, speaker.center(), 21.0, super::glyphs::VOLUME[super::glyphs::volume_step(vol)], theme::lerp_color(Color32::from_white_alpha(190), Color32::WHITE, 0.5 + 0.5 * sh));
    if sr.clicked() {
        super::shell::toggle_mute(app, ctx);
    }
}

/// An engaged transport mark: the bare glyph, inked in the accent. Nothing sits behind it —
/// off it is tertiary white, on it is themed, and the change is the whole announcement.
pub(super) fn lit_mark(app: &App, on: f32, off: Color32) -> Color32 {
    theme::lerp_color(off, app.accent_color(), on)
}

/// Splits Apple's explicit mark off a title: the suffix goes, the badge comes.
fn split_explicit(title: &str) -> (String, bool) {
    let lower = title.to_ascii_lowercase();
    for tail in ["(explicit)", "[explicit]"] {
        if let Some(at) = lower.rfind(tail)
            && at > 0
            && at + tail.len() == lower.len()
        {
            return (title[..at].trim_end().to_owned(), true);
        }
    }
    (title.to_owned(), false)
}

/// Size of the artwork laid out in `col`: what is left once the sleeve's information,
/// its bar and the transport underneath it have taken their room.
fn art_side(cs: &crate::preset::CoverStyle, col: Rect) -> f32 {
    (col.width() * 0.82 * cs.size.clamp(0.5, 1.35)).min(col.height() - 250.0).max(160.0)
}

/// The sleeve: art, title and artists, the seek line with its two times, then the
/// transport — the order the reference keeps them in. `px`: the size to fetch the image
/// at (fixed while the column glides). `side`: the sleeve's own size, measured once so
/// that showing or hiding the panel never resizes it. `sleeve` off leaves the art out but
/// keeps the layout, which is what the video player wants: the video is the picture, the words and the
/// transport stand where they always did.
/// The canvas for the song playing: asked for once per song while the setting is on, and
/// decoded over the sleeve once Spotify's answer has landed. The album art carries the
/// sleeve until the first frame is here, and for every song with no clip.
fn canvas(app: &mut App, ctx: &egui::Context, track: &crate::model::Track, px: u32) -> Option<widgets::MotionSprite> {
    let rank = |p: super::Picture| app.picture_order.iter().position(|q| *q == p).unwrap_or(9);
    // The music video is the picture whenever it is on and has a frame, unless the canvas
    // is ranked above it: the canvas then holds still underneath rather than hiding it.
    let watched = app.video_on || app.video_bg;
    let video_has_a_frame = app.video.view.as_ref().is_some_and(|v| v.motion.latest().is_some());
    if watched && video_has_a_frame && rank(super::Picture::Video) < rank(super::Picture::Canvas) {
        if let Some(v) = &app.canvas.view {
            v.motion.set_pause(true);
        }
        return None;
    }
    // The artwork can be ranked above the canvas; then the canvas isn't the sleeve's.
    if !app.show_canvas || rank(super::Picture::Canvas) > rank(super::Picture::Art) {
        if app.canvas.view.is_some() || app.canvas.url.is_some() {
            app.canvas.reset();
        }
        return None;
    }
    if app.canvas.asked.as_deref() != Some(track.id.as_str()) {
        app.canvas.reset();
        app.canvas.asked = Some(track.id.clone());
        let artist = track.artists.first().map(|a| a.name.clone()).unwrap_or_else(|| track.artist_line());
        let ms = track.duration.unwrap_or(0) as u64 * 1000;
        app.backend.request(crate::backend::Req::Canvas { key: track.id.clone(), artist, title: track.title.clone(), ms });
        return None;
    }
    let (name, url) = app.canvas.url.clone()?;
    if app.canvas.view.is_none() {
        app.canvas.view = Some(super::MotionView::new(crate::motion::play_canvas(ctx, &name, &url, px), px));
    }
    // The clip waits with the song, but only once it has a frame to hold: pausing before
    // the first one would leave the decoder asleep and the sleeve on its artwork.
    if let Some(v) = &app.canvas.view
        && v.motion.latest().is_some()
    {
        v.motion.set_pause(!app.playing());
    }
    app.canvas.frame(ctx)
}

/// `fill`, drawn past the player's own margins: Spotify's canvas covers the side of the
/// window edge to edge, so it clips to the screen rather than to the column's layout.
fn fill_edge(ui: &Ui, rect: Rect, clip: &widgets::MotionSprite, tint: Color32) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 || clip.size.y <= 0.0 {
        return;
    }
    let (a, b) = (clip.size.x / clip.size.y, rect.width() / rect.height());
    let uv = if b > a {
        Rect::from_center_size(pos2(0.5, 0.5), vec2(1.0, a / b))
    } else {
        Rect::from_center_size(pos2(0.5, 0.5), vec2(b / a, 1.0))
    };
    let screen = ui.ctx().content_rect();
    ui.ctx()
        .layer_painter(ui.layer_id())
        .with_clip_rect(screen)
        .add(egui::epaint::RectShape::filled(rect, 0.0, tint).with_texture(clip.id, uv));
}

#[allow(clippy::too_many_arguments)]
fn cover_column(app: &mut App, ui: &mut Ui, col: Rect, track: &crate::model::Track, now: f64, px: u32, side: f32, sleeve: bool) {
    use crate::preset::Shape;
    // A handoff's outgoing song is still what's heard: its star, menu and seek are its own.
    let hold = app.mix_hold().map(|h| (h.total, h.mix, app.mix_hold_pos(h)));
    let heard = if hold.is_some() { track.clone() } else { app.playing_track().cloned().unwrap_or_else(|| track.clone()) };
    let look = app.look();
    let cs = &look.cover;
    let art = Rect::from_center_size(pos2(col.center().x, col.top() + side / 2.0 + (col.height() - side - 250.0).max(0.0) * 0.42), Vec2::splat(side));
    let round = cs.shape != Shape::Rounded;
    let radius = if round { side / 2.0 } else { cs.radius.clamp(0.0, side / 2.0) };
    // Apple draws the sleeve smaller while the song is held: the layout stays put, only the
    // art breathes in and out, so the words never jump when the transport is paused.
    let breath = theme::anim_bool(ui.ctx(), "np-art-play", app.playing(), 0.36);
    let draw = Rect::from_center_size(art.center(), Vec2::splat(side * (0.90 + 0.10 * breath)));
    let dr = draw.width() / 2.0;
    let draw_cr = egui::CornerRadius::same((radius * (0.90 + 0.10 * breath)).min(255.0) as u8);
    // Best Part: the second the song's replay curve peaks, while the playhead has not
    // reached it. Hearing the part is what puts the button away — the playhead reaching it,
    // not merely nearing it, so a best part twenty seconds out doesn't take the offer off
    // the sleeve just as the listener notices it. A handoff hides the offer too, because
    // what lies ahead during a blend is the next song rather than this one's best part.
    let heard_ms = hold.as_ref().map_or_else(|| app.audio.status.position_smooth().as_secs_f32(), |(_, _, p)| *p) as f64 * 1000.0;
    let offer = app
        .best_part
        .as_ref()
        .filter(|(id, _)| app.playing_id() == Some(id.as_str()))
        .filter(|_| hold.is_none() && !app.best_part_heard)
        .map(|(_, start)| start.at_ms);
    if let Some(ms) = offer
        && heard_ms >= ms as f64 - 400.0
    {
        app.best_part_heard = true;
    }
    let ahead = app.show_best_part && offer.is_some_and(|ms| heard_ms < ms as f64 - 400.0);
    let bp = theme::anim_bool(ui.ctx(), "np-best-part", ahead, 0.28);
    // Spotify's canvas is parked for now: no lookup, no decoder, no picture. Everything
    // under it (the state, the backend ask, the loop, the cache) stays ready to switch
    // back on.
    let canvas: Option<widgets::MotionSprite> = None;
    // The pill that used to stand on the sleeve's foot has moved up into the song's own
    // row, so nothing over the artwork has to make room for it.
    let lift = 0.0;
    // Spotify's canvas covers the whole column — the way the music video does — rather
    // than the sleeve's square: the words, the star and the transport stand over it.
    if let Some(c) = &canvas {
        let screen = ui.ctx().content_rect();
        // Below the header line: the player's own way out stands there, and a picture
        // painted over it makes the X unclickable.
        let full = Rect::from_min_max(pos2(screen.left(), col.top()), pos2(col.right(), screen.bottom()));
        fill_edge(ui, full, c, Color32::WHITE);
        // A floor for the words and the transport: a canvas can be any brightness.
        super::shell::vgradient(ui.painter(), Rect::from_min_max(pos2(full.left(), full.bottom() - 300.0), full.max), Color32::TRANSPARENT, theme::shade(0.55));
    }
    if sleeve {
        if canvas.is_none() {
            if cs.glow > 0.0 {
                let s = egui::epaint::Shadow { offset: [0, 16], blur: 90, spread: 6, color: theme::with_alpha(app.accent_color(), cs.glow.min(1.0) * 0.7) };
                ui.painter().add(s.as_shape(draw, draw_cr));
            }
            if cs.shadow > 0.0 {
                widgets::soft_shadow(ui, draw, (radius * (0.90 + 0.10 * breath)).min(255.0), 60.0, cs.shadow.min(1.0));
            }
        }
        let tint = Color32::from_gray((cs.brightness.clamp(0.1, 1.0) * 255.0) as u8);
        if canvas.is_none() {
            if round {
                if cs.shape == Shape::Record && app.playing() {
                    let dt = ui.input(|i| i.stable_dt).min(0.1) as f64;
                    app.record_angle = (app.record_angle + dt * std::f64::consts::TAU / cs.spin_secs.max(4.0) as f64) % std::f64::consts::TAU;
                }
                let angle = if cs.shape == Shape::Record { app.record_angle as f32 } else { 0.0 };
                widgets::paint_disc(ui, egui::Id::unique("np-art"), draw.center(), dr, &track.thumbs, angle, tint, px);
                if cs.shape == Shape::Record {
                    // Vinyl: faint grooves and a spindle hole.
                    let p = ui.painter();
                    for i in 1..6 {
                        p.circle_stroke(draw.center(), dr * (0.42 + i as f32 * 0.1), egui::Stroke::new(1.0, theme::shade(0.18)));
                    }
                    p.circle_filled(draw.center(), dr * 0.18, theme::shade(0.35));
                    p.circle_filled(draw.center(), dr * 0.044, Color32::from_rgb(10, 10, 12));
                }
            } else {
                widgets::paint_cover_xfade_px(ui, egui::Id::unique("np-art"), draw, &track.thumbs, radius * (0.90 + 0.10 * breath), 1.0, px);
                // Apple's motion sleeve, over the still until its first frame arrives.
                if let Some(album) = track.album.as_ref().map(|a| a.name.clone()).filter(|a| !a.is_empty()) {
                    let artist = track.artists.first().map(|a| a.name.clone()).unwrap_or_else(|| track.artist_line());
                    app.apple_cover(&album, &artist, Some(&track.title), None);
                    if let Some(s) = app.apple_sprite(ui.ctx(), &album, &artist, Some(&track.title), None, px) {
                        widgets::paint_motion(ui, draw, radius * (0.90 + 0.10 * breath), &s);
                    }
                }
                if cs.brightness < 0.99 {
                    ui.painter().rect_filled(draw, draw_cr, theme::shade(1.0 - cs.brightness));
                }
            }
        }
        // A handoff still shows the outgoing song; the stats are the incoming one's.
        if hold.is_none() {
            nerd_stats(app, ui, draw, round, lift);
        }
    }
    // Title and artists: 17 semibold over 15 secondary, left-aligned to the art.
    let w = side;
    let x0 = art.left();
    let info_y = art.bottom() + 22.0;
    // The Best Part disc, the star and the ⋯ menu share the information line's right end.
    let text_w = w - 112.0;
    // Apple keeps "(Explicit)" out of the name and wears it as a small colour-synced square
    // after the title instead.
    let (name, explicit) = split_explicit(&track.title);
    let tr = text_at(ui, pos2(x0, info_y), &name, theme::bold(17.0), Color32::WHITE, text_w - if explicit { 21.0 } else { 0.0 });
    // The name is the way into the song's own sheet — the ⋯ menu no longer carries it.
    let about = ui.interact(tr.expand2(vec2(0.0, 3.0)), ui.scope_id().with("np-about"), Sense::click());
    let about = named(about, "About song").on_hover_cursor(egui::CursorIcon::PointingHand);
    if about.clicked() {
        app.actions.push(Action::AboutSong(track.clone()));
    }
    if explicit {
        let e = Rect::from_min_size(pos2(tr.right() + 7.0, tr.center().y - 6.5), vec2(13.0, 13.0));
        ui.painter().rect_filled(e, 3.5, theme::lerp_color(app.accent_color(), Color32::WHITE, 0.28));
        let g = widgets::galley(ui, "E", theme::semibold(9.5), Color32::from_rgb(14, 16, 22), 20.0);
        ui.painter().galley(e.center() - g.size() / 2.0, g, Color32::from_rgb(14, 16, 22));
    }
    // Each artist opens their own page (features included); the album follows the
    // artist behind an em dash, as the reference writes the line.
    let mut line = track.artist_line();
    if let Some(album) = &track.album
        && !album.name.is_empty()
    {
        line = format!("{line} — {}", album.name);
    }
    let line_w = text_w;
    let line_rect = widgets::linked_text(ui, pos2(x0, info_y + 21.0), &line, &track.artists, theme::semibold(15.0), theme::text_dim(), line_w, &mut app.actions);
    // The album after the em dash opens its page too, underlined like the artists.
    if let Some(album) = track.album.as_ref().filter(|a| !a.name.is_empty())
        && let Some(id) = album.id.clone()
    {
        let g = widgets::galley(ui, &line, theme::semibold(15.0), theme::text_dim(), line_w);
        if let Some(at) = g.job.text.rfind(album.name.as_str()) {
            let c0 = g.job.text[..at].chars().count();
            let c1 = c0 + album.name.chars().count();
            let a0 = g.pos_from_cursor(egui::text::CCursor::new(c0)).min.x;
            let a1 = g.pos_from_cursor(egui::text::CCursor::new(c1)).min.x;
            if a1 - a0 >= 2.0 {
                let r = Rect::from_min_max(pos2(line_rect.left() + a0, line_rect.top()), pos2(line_rect.left() + a1, line_rect.bottom()));
                let resp = ui.interact(r, ui.scope_id().with(("album-link", &album.name)), Sense::click());
                let resp = named(resp, &album.name).on_hover_cursor(egui::CursorIcon::PointingHand);
                if resp.hovered() {
                    ui.painter().hline(r.x_range(), r.bottom() - 1.0, egui::Stroke::new(1.0, theme::text_dim()));
                }
                if resp.clicked() {
                    app.actions.push(Action::Open(super::Target::Album(id)));
                }
            }
        }
    }
    // The Best Part disc: in front of the star, on the song's own line, wearing the same
    // transparent disc — a music note with the skip mark beside it. It arrives with the
    // replay curve and leaves once the part has been reached or played.
    if bp > 0.02
        && let Some(ms) = offer
    {
        let disc_r = Rect::from_center_size(pos2(art.right() - 90.0, info_y + 14.0), Vec2::splat(30.0));
        let bresp = ui.interact(disc_r, ui.scope_id().with("np-best-part"), Sense::click());
        let bresp = named(bresp, "Best Part").on_hover_text("Skip to the part of the song people replay most").on_hover_cursor(egui::CursorIcon::PointingHand);
        let bh = theme::anim_bool(ui.ctx(), bresp.id.with("h"), bresp.hovered(), 0.1) * bp;
        ui.painter().circle_filled(disc_r.center(), 15.0, theme::glass(0.10 + 0.05 * bh));
        let ink = theme::with_alpha(theme::lerp_color(theme::text(), app.accent_color(), bh), bp);
        best_part_mark(ui, disc_r.center(), ink);
        if bresp.clicked() {
            app.best_part_heard = true;
            app.actions.push(Action::Seek(ms as f32 / 1000.0));
        }
    }
    // The star: a 30 px disc that washes under the pointer and fills in the accent
    // once liked. The ⋯ menu sits at the line's end, as on music.youtube.com.
    let liked = app.liked.contains(&heard.id);
    let star = Rect::from_center_size(pos2(art.right() - 55.0, info_y + 14.0), Vec2::splat(30.0));
    let sr = ui.interact(star, ui.scope_id().with(("np-like", &heard.id)), Sense::click());
    let sr = named(sr, if liked { "Remove from favourites" } else { "Add to favourites" }).on_hover_cursor(egui::CursorIcon::PointingHand);
    let sh = theme::anim_bool(ui.ctx(), sr.id.with("h"), sr.hovered(), 0.1);
    let pop = theme::anim_bool(ui.ctx(), ("liked", &heard.id), liked, 0.3);
    // Apple's star stands on a transparent disc of its own, like the ⋯ beside it: the wash
    // deepens under the pointer and the disc takes the accent once the song is liked.
    let disc = theme::lerp_color(theme::glass(0.10 + 0.05 * sh), theme::with_alpha(app.accent_color(), 0.18), pop);
    ui.painter().circle_filled(star.center(), 15.0, disc);
    let dim = 0.85 + 0.15 * sh;
    let tint = theme::lerp_color(theme::with_alpha(Color32::WHITE, dim), app.accent_color(), pop);
    super::glyphs::draw(ui, star.center(), 19.0, super::glyphs::STAR, tint);
    if pop > 0.0 {
        super::glyphs::draw(ui, star.center(), 19.0, super::glyphs::STAR_FILLED, theme::with_alpha(app.accent_color(), pop));
    }
    if sr.secondary_clicked() {
        // The other half of the star: a right click tells YouTube the song isn't wanted.
        app.actions.push(Action::Dislike(heard.clone()));
    } else if sr.clicked() {
        app.actions.push(Action::Like(heard.clone(), !liked));
    }
    // The song's options, beside the star.
    let options = Rect::from_center_size(pos2(art.right() - 15.0, info_y + 14.0), Vec2::splat(34.0));
    options_menu(app, ui, options, &heard);

    // Seek: a 4 px line that grows under the pointer, its times below.
    let status = app.audio.status.clone();
    let held = hold.is_some();
    let (total, mix) = match &hold {
        Some((total, mix, _)) => (*total, *mix),
        None => {
            let total = status
                .duration()
                .map(|d| d.as_secs_f32())
                .or(track.duration.map(|d| d as f32))
                .unwrap_or(0.0);
            let next = app.queue.upcoming().is_some() || app.autoplay;
            let mix = match (total > 0.0, next, app.queue.repeat) {
                (true, true, repeat) if repeat != Repeat::One => {
                    status.mix_secs(status.duration().or_else(|| track.duration.map(|s| std::time::Duration::from_secs(s as u64)))).min(total * 0.5)
                }
                _ => 0.0,
            };
            (total, mix)
        }
    };
    let smooth = match &hold {
        Some((_, _, pos)) => *pos,
        None => status.position_smooth().as_secs_f32(),
    };
    let pos = app.seek_drag.unwrap_or(smooth).min(total.max(0.0));
    // The bar and its row of marks come first, directly under the song's name: elapsed
    // at the left, the stream's name centred, the remaining time at the right — all three
    // on one line, as the reference sets them. The transport a step below, the step being
    // the play button's own height, and it gives way when the column runs out of room.
    // Transport first, because its measures set the step under the times line: the row is
    // spread over the sleeve's own width — the outer pair a hair inside its edges, the
    // skips a little under a quarter of the side out from the play button.
    let icons = [0.038f32, 0.055, 0.075, 0.055, 0.038].map(|f| side * f);
    let sizes = icons.map(|s| s * 1.42);
    let hit = Rect::from_min_size(pos2(x0, info_y + 46.0), vec2(w, 16.0));
    let sy = hit.center().y;
    // Far enough under the bar for the quality mark's box to clear it: the mark stands on
    // this line, and the box that holds it reaches above the words.
    let times_y = sy + 18.0;
    let ty = (times_y + icons[2]).min(col.bottom() - 26.0 - icons[2] * 1.5);
    // Held for the handoff's first half: its last stretch is where the pointer is,
    // and seeking the queue's song from here would cut the mix.
    let sresp = ui.interact(hit, ui.scope_id().with("np-seek"), if held { Sense::hover() } else { Sense::click_and_drag() });
    let sresp = if total > 0.0 && !held { sresp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { sresp };
    sresp.widget_info(|| egui::WidgetInfo::slider(total > 0.0 && !held, pos as f64, "Seek"));
    let focus = theme::anim_bool(ui.ctx(), "np-seek-h", total > 0.0 && !held && (sresp.hovered() || sresp.dragged()), 0.18);
    let thick = side * (0.008 + 0.0055 * focus);
    let bar = Rect::from_min_max(pos2(x0, hit.center().y - thick / 2.0), pos2(x0 + w, hit.center().y + thick / 2.0));
    ui.painter().rect_filled(bar, thick / 2.0, Color32::from_white_alpha(38));
    // Automix: the handoff window tints to the song's colour with its start ticked, so
    // the blend has a place it visibly begins at.
    if mix > 0.5 {
        let edge = x0 + w * ((total - mix) / total);
        let band = Rect::from_min_max(pos2(edge, bar.top()), bar.max);
        ui.painter().rect_filled(band, thick / 2.0, theme::with_alpha(app.accent_color(), 0.32));
        let tick = Rect::from_min_size(pos2(edge - 1.0, bar.top()), vec2(2.0, thick));
        ui.painter().rect_filled(tick, 1.0, theme::with_alpha(app.accent_color(), 0.9));
    }
    let frac = if total > 0.0 { (pos / total).clamp(0.0, 1.0) } else { 0.0 };
    if frac > 0.0 {
        let fill = theme::lerp_color(Color32::from_white_alpha(163), Color32::from_white_alpha(240), focus);
        ui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * frac, thick)), thick / 2.0, fill);
    }
    let at = |p: egui::Pos2| (((p.x - x0) / w.max(1.0)).clamp(0.0, 1.0) * total).max(0.0);
    let tc = theme::with_alpha(Color32::WHITE, 0.62);
    let g = widgets::galley(ui, &super::fmt_time(pos as u32), theme::semibold(11.0), tc, 80.0);
    let lh = g.size().y.max(13.0);
    ui.painter().galley(pos2(x0, times_y - lh / 2.0), g, tc);
    // Remaining time counts down to zero, with the stream's mark between the times.
    let remaining = (total - pos).max(0.0) as u32;
    let g = widgets::galley(ui, &format!("-{}", super::fmt_time(remaining)), theme::semibold(11.0), tc, 80.0);
    ui.painter().galley(pos2(x0 + w - g.size().x, times_y - lh / 2.0), g, tc);
    let badge_w = audio_badge_width(ui, app, false);
    if badge_w > 0.0 {
        audio_badge(app, ui, pos2(x0 + (w - badge_w) / 2.0, times_y - lh / 2.0), false);
    }
    // While the two songs blend, its name sits there instead.
    let mixed = theme::anim_bool(ui.ctx(), "np-automix-on", app.mixing(), 0.3);
    if mixed > 0.01 && badge_w == 0.0 {
        let accent = theme::with_alpha(app.accent_color(), mixed);
        let g = widgets::galley(ui, "Mixing", theme::semibold(10.5), accent, 80.0);
        ui.painter().galley(pos2(x0 + (w - g.size().x) / 2.0, times_y - g.size().y / 2.0), g, accent);
    }
    if total > 0.0
        && let Some(p) = sresp.interact_pointer_pos()
    {
        if sresp.dragged() {
            app.seek_drag = Some(at(p));
        }
        if sresp.drag_stopped() || sresp.clicked() {
            app.seek_drag = None;
            app.actions.push(Action::Seek(at(p)));
        }
    }

    // Transport: the reference's row — shuffle, previous, play, next, repeat, spread over
    // the sleeve's own width.
    let xs = [-0.466f32, -0.206, 0.0, 0.206, 0.466].map(|f| art.center().x + side * f);
    // The marks are primary white, an off shuffle or repeat tertiary; a pressed button
    // shrinks to 90%, as the reference's does.
    let ink = |h: f32, base: f32, on: f32| theme::with_alpha(theme::lerp_color(theme::with_alpha(Color32::WHITE, base), Color32::WHITE, h), on);
    let enabled = app.current().is_some();
    let dim = if enabled { 1.0 } else { 0.4 };
    let btn = |ui: &mut Ui, i: usize, key: &str, name: &str, live: bool| -> (egui::Response, f32, f32) {
        let r = Rect::from_center_size(pos2(xs[i], ty), Vec2::splat(sizes[i]));
        let resp = ui.interact(r, ui.scope_id().with(("np-t", key)), if live { Sense::click() } else { Sense::hover() });
        let resp = named(resp, name);
        let resp = if live { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
        let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), live && resp.hovered(), 0.1);
        let p = theme::anim_bool(ui.ctx(), resp.id.with("p"), live && resp.is_pointer_button_down_on(), 0.1);
        if h > 0.0 {
            ui.painter().circle_filled(pos2(xs[i], ty), sizes[i] / 2.0 * (1.0 - 0.1 * p), Color32::from_white_alpha((0.1 * h * 255.0) as u8));
        }
        (resp, h, p)
    };
    let (r, h, p) = btn(ui, 0, "shuffle", "Shuffle", true);
    let shuffled = app.queue.shuffled;
    let son = theme::anim_bool(ui.ctx(), r.id.with("on"), shuffled, 0.25);
    let mark = lit_mark(app, son, ink(h, 0.55, 1.0));
    super::glyphs::draw(ui, r.rect.center(), icons[0] * (1.0 - 0.1 * p), super::glyphs::SHUFFLE, mark);
    if r.clicked() {
        app.toggle_shuffle();
    }
    let (r, h, p) = btn(ui, 1, "prev", "Previous", enabled);
    super::glyphs::draw(ui, r.rect.center(), icons[1] * (1.0 - 0.1 * p), super::glyphs::PREVIOUS, ink(h, 1.0, dim));
    if r.clicked() && enabled {
        app.prev();
    }
    let (r, h, p) = btn(ui, 2, "play", if app.playing() || app.buffering { "Pause" } else { "Play" }, enabled);
    let c = r.rect.center();
    let size = icons[2] * (1.0 - 0.1 * p);
    let mark = ink(h, 1.0, dim);
    let morph = theme::anim_bool(ui.ctx(), "np-play-morph", app.playing() || app.buffering, 0.18);
    if app.buffering {
        let a = now as f32 * 6.0;
        let pts: Vec<egui::Pos2> = (0..16).map(|i| c + vec2((a + i as f32 * 0.24).cos(), (a + i as f32 * 0.24).sin()) * icons[2] * 0.19).collect();
        ui.painter().add(egui::Shape::Path(egui::epaint::PathShape::line(pts, egui::epaint::PathStroke::new(icons[2] * 0.042, mark))));
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
    } else {
        // The two marks cross-fade: the reference keeps its pause up through a buffer,
        // and a hard cut between them flickers.
        if morph < 1.0 {
            super::glyphs::draw(ui, c, size, super::glyphs::PLAY, theme::with_alpha(mark, 1.0 - morph));
        }
        if morph > 0.0 {
            super::glyphs::draw(ui, c, size, super::glyphs::PAUSE, theme::with_alpha(mark, morph));
        }
    }
    if r.clicked() && enabled {
        app.toggle();
    }
    let (r, h, p) = btn(ui, 3, "next", "Next", enabled);
    super::glyphs::draw(ui, r.rect.center(), icons[3] * (1.0 - 0.1 * p), super::glyphs::NEXT, ink(h, 1.0, dim));
    if r.clicked() && enabled {
        app.next();
    }
    let (repeat_glyph, repeat_name) = match app.queue.repeat {
        Repeat::Off => (super::glyphs::REPEAT, "Repeat off"),
        Repeat::All => (super::glyphs::REPEAT, "Repeat all"),
        Repeat::One => (super::glyphs::REPEAT_ONE, "Repeat one"),
    };
    let looping = app.queue.repeat != Repeat::Off;
    let (r, h, p) = btn(ui, 4, "repeat", repeat_name, true);
    let ron = theme::anim_bool(ui.ctx(), r.id.with("on"), looping, 0.25);
    let mark = lit_mark(app, ron, ink(h, 0.55, 1.0));
    super::glyphs::draw(ui, r.rect.center(), icons[4] * (1.0 - 0.1 * p), repeat_glyph, mark);
    if r.clicked() {
        app.queue.repeat = match app.queue.repeat {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        };
    }
}

/// The Best Part mark: the music note the button has always carried, with the skip mark
/// beside it — the part it jumps to, and the jump.
fn best_part_mark(ui: &Ui, c: egui::Pos2, tint: Color32) {
    super::glyphs::draw(ui, c + vec2(-3.5, -1.0), 15.0, super::glyphs::MUSIC_NOTE, tint);
    let at = c + vec2(7.5, 4.0);
    let tri = vec![pos2(at.x - 3.2, at.y - 3.4), pos2(at.x - 3.2, at.y + 3.4), pos2(at.x + 2.0, at.y)];
    ui.painter().add(egui::Shape::convex_polygon(tri, tint, egui::Stroke::NONE));
    ui.painter().line_segment([pos2(at.x + 2.8, at.y - 3.4), pos2(at.x + 2.8, at.y + 3.4)], egui::Stroke::new(1.4, tint));
}

// ------------------------------------------------------------------ lyrics

pub(super) fn lyrics_panel(app: &mut App, ui: &mut Ui, rect: Rect, now: f64, centered: bool, surface: karaoke::Surface) {
    // Lent out for the frame rather than cloned (the drawing needs `app` mutably).
    let Some((id, set)) = app.lyrics.take() else {
        ui.add_space(rect.height() * 0.35);
        ui.vertical_centered(|ui| {
            ui.spinner();
            ui.label(egui::RichText::new("Loading lyrics…").color(theme::text_dim()));
        });
        return;
    };
    if let Some(lyrics) = app.lyric_prefs.choose(&id, &set).and_then(|p| set.get(p)) {
        let panel = Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - 4.0));
        if lyrics.sync == Sync::None {
            plain_lyrics(app, ui, panel, lyrics, surface);
        } else {
            synced(app, ui, panel, &id, lyrics, now, centered, surface);
        }
    } else {
        ui.add_space(rect.height() * 0.4);
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new("No lyrics available").font(theme::semibold(20.0)).color(theme::text_dim()));
            ui.add_space(6.0);
            ui.label(egui::RichText::new("None of the lyric providers has this song.").font(theme::regular(14.0)).color(theme::text_faint()));
        });
    }
    app.lyrics = Some((id, set));
}

/// Romanization and translation for each line, the way the extension picks them: what
/// the lyrics ship (Apple-style TTML) first, then Unison/Google.
fn decorations<'a>(
    show_roman: bool,
    show_trans: bool,
    target: &'a str,
    extras: Option<&'a (String, String, String, crate::translate::Extras)>,
    id: &str,
    lyrics: &'a Lyrics,
) -> (Vec<Option<karaoke::Roman<'a>>>, Vec<Option<&'a str>>) {
    let extras = extras.filter(|(v, s, t, _)| v == id && s == lyrics.provider.key() && t == target).map(|(_, _, _, e)| e);
    let same = |a: &str, b: &str| a.trim().eq_ignore_ascii_case(b.trim());
    // A blank romanization would draw an empty pill box with nothing inside it.
    let worth = |r: &str, l: &crate::lyrics::Line| !r.trim().is_empty() && !same(r, &l.text);
    let roman = lyrics
        .lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if !show_roman || l.instrumental {
                return None;
            }
            if let Some(r) = l.romanization.as_deref().filter(|r| worth(r, l)) {
                return Some(if l.timed_roman.is_empty() { karaoke::Roman::Text(r) } else { karaoke::Roman::Timed(&l.timed_roman, r) });
            }
            let r = extras?.roman.get(i)?.as_deref()?;
            worth(r, l).then_some(karaoke::Roman::Text(r))
        })
        .collect();
    let trans = lyrics
        .lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if !show_trans || l.instrumental {
                return None;
            }
            let t = super::lyric_prefs::shipped_translation(l, target).or_else(|| extras?.trans.get(i)?.as_deref())?;
            (!same(t, &l.text)).then_some(t)
        })
        .collect();
    (roman, trans)
}

fn synced(app: &mut App, ui: &mut Ui, rect: Rect, id: &str, lyrics: &Lyrics, now: f64, narrow: bool, surface: karaoke::Surface) {
    let style = &app.look().lyrics;
    let accent = app.accent_color();
    let centered = narrow || style.align == crate::preset::Align::Center;
    let playing = if app.mix_hold().is_some() { app.audio.status.outgoing_playing() } else { app.playing() };
    // Audible position: the device plays what the player hands it a buffer later. A
    // handoff's first half is still the outgoing song's tail, which lines up with it.
    let pos = match app.mix_hold() {
        Some(h) => app.mix_hold_pos(h) as f64 - app.audio.status.latency().as_secs_f64(),
        None => app.audio.status.position_smooth().as_secs_f64() - app.audio.status.latency().as_secs_f64(),
    };
    let pos = app.lyric_clock.tick(pos, playing, now);
    let t = pos - app.lyric_prefs.offset(id, lyrics.provider, lyrics.sync == Sync::Word);
    let (roman, trans) = decorations(app.show_roman, app.show_trans, &app.lyric_prefs.target, app.lyrics_extras.as_ref(), id, lyrics);
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        id.hash(&mut h);
        lyrics.provider.key().hash(&mut h);
        app.theme_id.hash(&mut h);
        for r in &roman {
            match r {
                Some(karaoke::Roman::Timed(_, s)) | Some(karaoke::Roman::Text(s)) => s.hash(&mut h),
                None => 0u8.hash(&mut h),
            }
        }
        trans.hash(&mut h);
        // Keep the player's animation IDs unchanged; the drawer needs its own transitions.
        if surface == karaoke::Surface::Sidebar {
            "sidebar".hash(&mut h);
        }
        h.finish()
    };
    let scale = app.lyric_prefs.text_scale;
    let frame = karaoke::Frame { lyrics, key, roman, trans, t, playing, now, rect, style, accent, centered, surface, scale };
    // The frame borrows decorations from `app`; lend out the selected view until done.
    let mut view = match surface {
        karaoke::Surface::Player => std::mem::take(&mut app.lyric_view),
        karaoke::Surface::Sidebar => std::mem::take(&mut app.sidebar_lyric_view),
    };
    let out = if karaoke::dev_off("view") { karaoke::Out { seek: None, animating: true, user_scrolling: false } } else { view.show(ui, frame) };
    if let Some(s) = out.seek {
        app.actions.push(Action::Seek(s as f32));
    }
    if out.animating {
        ui.ctx().request_repaint();
    }
    // The extension's `.autoscroll-resume-button` (YouTube Music's rem is 10 px): a frosted
    // pill under the top edge in the lyrics font at 600, 1.75rem; padding .5rem 1rem;
    // white 5% fill, 1px white 10% border, radius 1rem, over a 5px backdrop blur (which
    // reads as about twice those on screen). It slides down from -3em to 2em as it fades
    // in, and its text glows on hover.
    let (resume_id, resume_hover_id) = match surface {
        karaoke::Surface::Player => ("lyr-resume", "lyr-resume-h"),
        karaoke::Surface::Sidebar => ("sidebar-lyr-resume", "sidebar-lyr-resume-h"),
    };
    let shown = theme::anim_bool(ui.ctx(), resume_id, out.user_scrolling, 0.2);
    if shown > 0.0 {
        let ink = if surface == karaoke::Surface::Sidebar && theme::light() { theme::text() } else { Color32::WHITE };
        let font = theme::lyric_font(&style.font, crate::preset::Weight::Semibold, 17.5);
        let g = widgets::galley(ui, "Resume Autoscroll", font, ink, rect.width() - 40.0);
        let size = g.size() + vec2(20.0, 10.0) + Vec2::splat(2.0);
        let em = 17.5;
        let y = rect.top() + 8.0 + egui::lerp(-3.0 * em..=0.5 * em, theme::ease_out(shown));
        let r = Rect::from_min_size(pos2(rect.center().x - size.x / 2.0, y), size);
        let resp = ui.interact(r, ui.scope_id().with("lyr-resume-btn"), if out.user_scrolling { Sense::click() } else { Sense::hover() });
        let resp = named(resp, "Resume Autoscroll").on_hover_cursor(egui::CursorIcon::PointingHand);
        let hover = theme::anim_bool(ui.ctx(), resume_hover_id, resp.hovered() && out.user_scrolling, 0.25);
        let p = ui.painter().with_clip_rect(rect.expand2(vec2(0.0, 4.0)));
        p.rect_filled(r, 10.0, theme::with_alpha(ink, 0.10 * shown));
        p.rect_stroke(r, 10.0, egui::Stroke::new(1.0, theme::with_alpha(ink, 0.2 * shown)), egui::StrokeKind::Inside);
        let at = r.center() - g.size() / 2.0;
        if hover > 0.0
            && let Some((tex, glow_rect, uv)) = view.text_glow(ui.ctx(), g.clone(), at, 15.0)
        {
            // `text-shadow: 0 0 1.5rem var(--blyrics-glow-color)`: a real blur of the letters.
            let glow = crate::preset::paint(&style.glow_color, accent);
            p.image(tex, glow_rect, uv, theme::with_alpha(glow, hover * shown));
        }
        p.galley_with_override_text_color(at, g, theme::with_alpha(ink, shown));
        if shown < 1.0 {
            ui.ctx().request_repaint();
        }
        if resp.clicked() {
            view.resume();
        }
    }
    match surface {
        karaoke::Surface::Player => app.lyric_view = view,
        karaoke::Surface::Sidebar => app.sidebar_lyric_view = view,
    }
}

fn plain_lyrics(app: &mut App, ui: &mut Ui, rect: Rect, lyrics: &Lyrics, surface: karaoke::Surface) {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    let scroll_id = match surface { karaoke::Surface::Player => "plain-lyrics", karaoke::Surface::Sidebar => "sidebar-plain-lyrics" };
    egui::ScrollArea::vertical().id_salt(scroll_id).auto_shrink(false).show(&mut child, |ui| {
        ui.label(egui::RichText::new("These lyrics aren't synced").font(theme::regular(13.0)).color(theme::text_faint()));
        ui.add_space(14.0);
        let style = &app.look().lyrics;
        let size = (32.0 * style.size.clamp(0.6, 1.6)).min(rect.width() / 12.5).max(18.0);
        for l in &lyrics.lines {
            let font = theme::lyric_font_for(&style.font, style.weight, size, &l.text);
            let ink = if surface == karaoke::Surface::Sidebar && theme::light() { theme::text() } else { Color32::WHITE };
            // Unsynced words are the reference's plain text: full strength, no dimming.
            let color = ink;
            if crate::bidi::has_rtl(&l.text) {
                // Wrapped in reading order, each row in display order; RTL lines start on the right.
                let width = ui.available_width();
                let g = super::widgets::galley_wrapped(ui, &l.text, font, color, width, usize::MAX);
                let (r, _) = ui.allocate_exact_size(vec2(width, g.size().y), Sense::hover());
                let x = if crate::bidi::rtl_base(&l.text) { r.right() - g.size().x } else { r.left() };
                ui.painter().galley(pos2(x, r.top()), g, color);
            } else {
                ui.label(egui::RichText::new(&l.text).font(font).color(color));
            }
            ui.add_space(size * 0.25);
        }
        ui.add_space(80.0);
    });
}

/// The ⋯ menu: quality, song actions, and shortcuts to the player's panels.
fn options_menu(app: &mut App, ui: &mut Ui, rect: Rect, track: &crate::model::Track) {
    let resp = ui.interact(rect, ui.scope_id().with("np-options"), Sense::click());
    let resp = named(resp, "Options").on_hover_cursor(egui::CursorIcon::PointingHand);
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&resp));
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered() || open, 0.12);
    // Apple wears the dots on a transparent disc, there whether or not the pointer is on it.
    ui.painter().circle_filled(rect.center(), 17.0, theme::glass(0.10 + 0.05 * h));
    for dx in [-6.0, 0.0, 6.0] {
        ui.painter().circle_filled(rect.center() + vec2(dx, 0.0), 1.9, theme::with_alpha(Color32::WHITE, 0.75 + 0.25 * h));
    }
    // YTM_MENU holds this open for a screenshot, the way the pill's own ⋯ does.
    let popup = egui::Popup::menu(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
    let popup = if super::dev_var("YTM_MENU").is_some() && ui.input(|i| i.time) >= 12.0 { popup.open(true) } else { popup };
    popup.show(|ui| {
        ui.set_width(270.0);
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        // Leave room for the popup frame and viewport edges, even in a short window — and
        // never more than the space under the ⋯ itself: a menu that fits its own slot
        // scrolls instead of being shifted up as the lyric options unfold.
        let frame_height = egui::Frame::popup(ui.style()).total_margin().sum().y;
        let below = (ui.ctx().content_rect().bottom() - resp.rect.bottom() - 20.0).max(160.0);
        let max_height = below.min((ui.ctx().content_rect().height() - frame_height - 16.0).max(1.0));
        ui.set_max_height(max_height);
        egui::ScrollArea::vertical().max_height(max_height).min_scrolled_height(1.0).auto_shrink([false, true]).show(ui, |ui| {
            ui.add_space(4.0);
            if lyrics_shown(app, ui.ctx()) && app.lyrics.is_some() {
                // First in the menu, so unfolding it only ever adds rows below the ⋯:
                // the button and the menu's top edge stay where they were.
                let open_id = ui.scope_id().with("lyric-options-open");
                let mut open = ui.ctx().data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);
                if expand_row(ui, "Lyrics options", open) {
                    open = !open;
                    ui.ctx().data_mut(|d| d.insert_temp(open_id, open));
                }
                if open {
                    lyric_options(app, ui);
                }
                menu_divider(ui);
            }
            // Revert when hi-res is playing, upgrade when another source is available.
            let hires_playing = app.audio_info.as_ref().is_some_and(|(_, i)| i.lossless() || i.atmos);
            if hires_playing || !app.hires_sources.is_empty() {
                if hires_playing {
                    if widgets::menu_action_apple(ui, glyphs::MUSIC_NOTE, "Revert quality") {
                        app.actions.push(Action::RevertQuality);
                    }
                } else if widgets::menu_action_apple(ui, glyphs::MUSIC_NOTE, "Upgrade quality") {
                    app.actions.push(Action::UpgradeQuality);
                }
                menu_divider(ui);
            }
            let liked = app.liked.contains(&track.id);
            widgets::song_actions(ui, track, liked, None, false, false, &mut app.actions);
            menu_divider(ui);
            if widgets::menu_action_apple(ui, glyphs::RADIO, "Start radio") {
                app.actions.push(Action::Radio(track.clone()));
            }
            // The star's other half, where the star is: a thumbs-down tells YouTube the
            // song isn't wanted, and the listening stops leaning on it.
            if widgets::menu_action(ui, Some(icon::THUMBS_DOWN), "Dislike") {
                app.actions.push(Action::Dislike(track.clone()));
            }
            // Both shares answer for the same song — its own address and a card of its
            // lyrics — so they sit in one section.
            menu_divider(ui);
            if widgets::menu_action_share(ui, "Share song") {
                app.actions.push(Action::Share(Target::Song(track.clone())));
            }
            if widgets::menu_action_share(ui, "Share lyrics") {
                let video_id = app.shown().map(|t| t.id.clone()).unwrap_or_else(|| track.id.clone());
                let provider = app.lyrics.as_ref().filter(|(id, _)| *id == video_id)
                    .and_then(|(id, set)| app.lyric_prefs.choose(id, set));
                app.actions.push(Action::ShareLyrics { video_id, generation: app.lyrics_generation, provider });
            }
            ui.add_space(4.0);
        });
    });
}

fn lyric_options(app: &mut App, ui: &mut Ui) {
    let Some((id, set)) = app.lyrics.as_ref() else { return };
    let id = id.clone();
    let available = app.lyric_prefs.available(set);
    let Some(shown) = app.lyric_prefs.choose(&id, set) else { return };
    let mut pick = None;
    menu_caption(ui, if available.len() > 1 { "LYRICS SOURCE" } else { "LYRICS SOURCE · ONLY ONE FOUND" });
    for p in &available {
        if source_row(ui, p.name(), p.sync(), *p == shown) {
            pick = Some(*p);
        }
    }
    menu_divider(ui);
    let tip = format!("Translation · {}", lang_name(&app.lyric_prefs.target));
    switch_row(ui, &tip, &mut app.show_trans);
    switch_row(ui, "Romanization", &mut app.show_roman);
    let off = app.lyric_prefs.song_offset(&id, shown);
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
    ui.painter().text(pos2(row.left() + 12.0, row.center().y), egui::Align2::LEFT_CENTER, "Timing", theme::regular(14.0), theme::text());
    let mut sub = ui.new_child(egui::UiBuilder::new().max_rect(row.shrink2(vec2(8.0, 0.0))).layout(egui::Layout::right_to_left(egui::Align::Center)));
    sub.spacing_mut().item_spacing.x = 2.0;
    if mini_icon(&mut sub, icon::CHEVRON_RIGHT, "Lyrics later (+0.1 s)").clicked() {
        app.lyric_prefs.set_song_offset(&id, shown, off + 0.1);
    }
    let label = format!("{off:+.1}s").replace("+0.0", "0.0");
    if mini_text(&mut sub, &label, "This song's lyrics timing. Double-click to reset.").double_clicked() {
        app.lyric_prefs.set_song_offset(&id, shown, 0.0);
    }
    if mini_icon(&mut sub, icon::CHEVRON_LEFT, "Lyrics earlier (−0.1 s)").clicked() {
        app.lyric_prefs.set_song_offset(&id, shown, off - 0.1);
    }
    if let Some(p) = pick.filter(|p| *p != shown) {
        app.lyric_prefs.pick(&id, set, p);
        app.lyric_view.reset();
        app.sidebar_lyric_view.reset();
    }
}

fn source_row(ui: &mut Ui, name: &str, sync: SyncKind, current: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
    let resp = named(resp, &format!("Lyrics source {name}")).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.12);
    if h > 0.0 || current {
        ui.painter().rect_filled(rect, 8.0, theme::glass(0.05 * current as u8 as f32 + 0.08 * h));
    }
    let (color, label) = match sync {
        SyncKind::Syllable => (Color32::from_rgb(196, 160, 255), "Syllable synced"),
        SyncKind::Word => (Color32::from_rgb(130, 200, 255), "Word synced"),
        SyncKind::Line => (Color32::from_rgb(255, 205, 110), "Line synced"),
        SyncKind::Unsynced => (Color32::from_rgb(170, 170, 170), "Not synced"),
    };
    ui.painter().circle_filled(pos2(rect.left() + 19.0, rect.center().y), 3.5, color);
    let x = rect.left() + 40.0;
    text_at(ui, pos2(x, rect.top() + 6.0), name, theme::semibold(13.5), theme::text(), rect.right() - x - 34.0);
    text_at(ui, pos2(x, rect.top() + 24.0), label, theme::regular(12.0), theme::text_dim(), rect.right() - x - 34.0);
    if current {
        widgets::icon_at(ui, pos2(rect.right() - 18.0, rect.center().y), icon::CHECK, 16.0, theme::text());
    }
    resp.clicked()
}

fn switch_row(ui: &mut Ui, label: &str, on: &mut bool) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
    let resp = named(resp, label).on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.clicked() { *on = !*on; }
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.12);
    if h > 0.0 { ui.painter().rect_filled(rect, 8.0, theme::glass(0.08 * h)); }
    ui.painter().text(pos2(rect.left() + 12.0, rect.center().y), egui::Align2::LEFT_CENTER, label, theme::regular(14.0), theme::text());
    let t = theme::anim_bool(ui.ctx(), resp.id.with("t"), *on, 0.16);
    let track = Rect::from_center_size(pos2(rect.right() - 30.0, rect.center().y), vec2(36.0, 20.0));
    let accent = ui.ctx().data(|d| d.get_temp::<Color32>(egui::Id::unique("accent"))).unwrap_or(theme::RED);
    ui.painter().rect_filled(track, 10.0, theme::lerp_color(theme::glass(0.16 + 0.06 * h), accent, t));
    let knob = pos2(egui::lerp(track.left() + 10.0..=track.right() - 10.0, t), track.center().y);
    ui.painter().circle_filled(knob, 8.0, Color32::WHITE);
}

/// A menu row that folds a block open and shut. Unlike [`widgets::menu_action`] it keeps
/// the menu up when clicked, because what it reveals is read from the same menu.
fn expand_row(ui: &mut Ui, label: &str, open: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
    let resp = named(resp, label).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.12);
    if h > 0.0 {
        ui.painter().rect_filled(rect, 7.0, theme::glass(0.12 * h));
    }
    ui.painter().text(pos2(rect.left() + 12.0, rect.center().y), egui::Align2::LEFT_CENTER, label, theme::regular(14.0), theme::text());
    // Up while closed and down while open: the rows unfold beneath the row, and the mark
    // says so.
    let chevron = if open { icon::CHEVRON_DOWN } else { icon::CHEVRON_UP };
    let ink = if h > 0.0 { theme::text() } else { theme::text_dim() };
    widgets::icon_at(ui, pos2(rect.right() - 19.0, rect.center().y), chevron, 16.0, ink);
    resp.clicked()
}

fn mini_icon(ui: &mut Ui, glyph: char, tip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(22.0, 24.0), Sense::click());
    let resp = named(resp, tip).on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.12);
    if h > 0.0 { ui.painter().rect_filled(rect, 7.0, theme::glass(0.12 * h)); }
    widgets::icon_at(ui, rect.center(), glyph, 14.0, theme::with_alpha(Color32::WHITE, 0.6 + 0.35 * h));
    resp
}

fn mini_text(ui: &mut Ui, text: &str, tip: &str) -> egui::Response {
    let g = ui.fonts_mut(|f| f.layout_no_wrap(text.to_owned(), theme::semibold(12.0), Color32::WHITE));
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 12.0, 24.0), Sense::click());
    let resp = named(resp, tip).on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.12);
    if h > 0.0 { ui.painter().rect_filled(rect, 7.0, theme::glass(0.12 * h)); }
    ui.painter().galley_with_override_text_color(rect.center() - g.size() / 2.0, g, theme::with_alpha(Color32::WHITE, 0.6 + 0.35 * h));
    resp
}

fn lang_name(code: &str) -> &str {
    super::lyric_prefs::LANGUAGES.iter().find(|(c, _)| *c == code).map_or(code, |(_, n)| n)
}

fn menu_caption(ui: &mut Ui, text: &str) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
    ui.painter().text(pos2(r.left() + 12.0, r.center().y + 2.0), egui::Align2::LEFT_CENTER, text, theme::semibold(11.0), theme::text_faint());
}

fn menu_divider(ui: &mut Ui) {
    ui.add_space(5.0);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(r.shrink2(vec2(8.0, 0.0)), 0.0, theme::glass(0.08));
    ui.add_space(5.0);
}

// ------------------------------------------------------------------ audio quality

fn badge_font(small: bool) -> egui::FontId {
    theme::semibold(if small { 9.0 } else { 11.0 })
}

fn glyph_side(small: bool) -> f32 {
    if small { 11.0 } else { 13.0 }
}

/// Whether the mark leads with an icon: Apple's lossless wave, or Dolby's double-D.
fn has_quality_glyph(info: &crate::hires::Info) -> bool {
    info.atmos || info.lossless()
}

/// Apple's lossless wave, and Dolby's own Atmos lockup from its newsroom asset page.
fn quality_icon(atmos: bool) -> &'static [u8] {
    if atmos {
        include_bytes!("../../assets/quality/atmos.png")
    } else {
        include_bytes!("../../assets/quality/lossless.png")
    }
}

/// The whole mark: Dolby's lockup carries its own words, so it stands alone.
fn quality_icon_aspect(atmos: bool) -> f32 {
    if atmos { 665.0 / 95.0 } else { 86.0 / 60.0 }
}

/// Just the glyph: the double-D cropped off the lockup's left, for the mini player and
/// the mixing badge where the words come from elsewhere.
fn quality_mark_aspect(atmos: bool) -> f32 {
    if atmos { 106.0 / 95.0 } else { 86.0 / 60.0 }
}

/// The icon as white pixels with its own alpha, so it takes the badge's tint.
fn quality_icon_texture(ui: &Ui, atmos: bool, mark: bool) -> egui::TextureHandle {
    let ctx = ui.ctx();
    let id = egui::Id::new(("quality-icon", atmos, mark));
    if let Some(tex) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(id)) {
        return tex;
    }
    let rgba = image::load_from_memory(quality_icon(atmos)).expect("quality icon").to_rgba8();
    let rgba = if mark && atmos {
        image::imageops::crop_imm(&rgba, 0, 0, 106, rgba.height()).to_image()
    } else {
        rgba
    };
    // The marks are drawn a dozen points tall: a 665-pixel-wide lockup sampled straight
    // down to that shimmers and reads as blur. Halving it a few times on the way in keeps
    // the edge in the texture rather than in the sampler.
    let rgba = if rgba.height() > 128 {
        let h = 128u32;
        let w = (rgba.width() as f32 * h as f32 / rgba.height() as f32).round().max(1.0) as u32;
        image::imageops::resize(&rgba, w, h, image::imageops::FilterType::Lanczos3)
    } else {
        rgba
    };
    let mut img = egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw());
    for p in &mut img.pixels {
        *p = Color32::from_white_alpha(p.a());
    }
    // Mipmaps: the lockup is drawn a dozen points tall, and a straight minification of a
    // 665-pixel bitmap shimmers. The mip chain is what keeps its edge.
    let options = egui::TextureOptions {
        magnification: egui::TextureFilter::Linear,
        minification: egui::TextureFilter::Linear,
        wrap_mode: egui::TextureWrapMode::ClampToEdge,
        mipmap_mode: Some(egui::TextureFilter::Linear),
    };
    let tex = ctx.load_texture(if atmos { "dolby-atmos" } else { "apple-lossless" }, img, options);
    ctx.data_mut(|d| d.insert_temp(id, tex.clone()));
    tex
}

fn paint_quality_icon(ui: &Ui, center: egui::Pos2, height: f32, atmos: bool, tint: Color32, mark: bool) {
    let w = (height * if mark { quality_mark_aspect(atmos) } else { quality_icon_aspect(atmos) }).round();
    let rect = Rect::from_center_size(center, vec2(w, height));
    let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    ui.painter().image(quality_icon_texture(ui, atmos, mark).id(), rect, uv, tint);
}

/// What the quality mark speaks for. The hi-res deck states its own `Info`, but an
/// ordinary YouTube stream sends only its stats — and a mark that vanished on every
/// song the addons don't serve is worse than one read off what is actually playing.
fn shown_info(app: &App) -> Option<crate::hires::Info> {
    if let Some((_, info)) = app.audio_info.as_ref() {
        return Some(info.clone());
    }
    let (_, s) = app.stream_stats.as_ref()?;
    Some(crate::hires::Info {
        source: String::new(),
        host: String::new(),
        codec: s.codec.clone(),
        quality: String::new(),
        bitrate_kbps: s.bitrate_kbps,
        sample_rate: Some(s.sample_rate).filter(|r| *r > 0),
        bit_depth: s.bit_depth,
        channels: Some(s.channels as u8),
        atmos: false,
        size: 0,
        duration_ms: 0,
    })
}

/// The music-video player is on and its picture hasn't arrived yet: the song is still
/// playing over the artwork, so the player says what it is doing where the quality mark
/// usually stands.
const VIDEO_BADGE: &str = "Playing video";

/// How wide the quality mark is, so a line can leave room for it.
pub(super) fn audio_badge_width(ui: &Ui, app: &App, small: bool) -> f32 {
    if app.video_on && app.video.loading() {
        let text = widgets::galley(ui, VIDEO_BADGE, badge_font(small), Color32::WHITE, 200.0).size().x;
        return text + glyph_side(small) + 5.0;
    }
    if app.audio_info.is_none() && app.upgrading_quality {
        let label = "Upgrading quality";
        let text = widgets::galley(ui, label, badge_font(small), Color32::WHITE, 200.0).size().x;
        return text + glyph_side(small) + 5.0;
    }
    let Some(info) = shown_info(app) else { return 0.0 };
    let gh = glyph_side(small);
    if info.atmos && !app.mixing() {
        return gh * quality_icon_aspect(true);
    }
    let label = if app.mixing() { "Mixing" } else { info.badge() };
    let text = widgets::galley(ui, label, badge_font(small), Color32::WHITE, 200.0).size().x;
    if has_quality_glyph(&info) { text + gh * quality_mark_aspect(info.atmos) + 5.0 } else { text }
}

/// The quality mark — a glyph and the stream's name, tinted with the theme — drawn at
/// `at`; a click opens its details. Returns how much room it took.
pub(super) fn audio_badge(app: &App, ui: &mut Ui, at: egui::Pos2, small: bool) -> f32 {
    if app.video_on && app.video.loading() {
        // The clip is coming down or being decoded: it pulses in the song's colour the way
        // the quality mark does while it upgrades, so a wait reads as work and not as a
        // player that has given up on the video.
        let font = badge_font(small);
        let gh = glyph_side(small);
        let now = ui.input(|i| i.time);
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
        let alpha = 0.35 + 0.65 * ((now * 3.5).sin() * 0.5 + 0.5) as f32;
        let accent = app.accent_color();
        let ink = theme::with_alpha(theme::lerp_color(accent, Color32::WHITE, 0.20), alpha);
        let g = widgets::galley(ui, VIDEO_BADGE, font, ink, 200.0);
        let w = audio_badge_width(ui, app, small);
        let h = g.size().y.max(gh);
        let rect = Rect::from_min_size(at, vec2(w, h));
        let resp = ui.interact(rect, ui.scope_id().with("audio-badge-video"), Sense::hover());
        named(resp, "Music video").on_hover_text("Loading the song's own music video…");
        pip_mark(ui, pos2(at.x + gh / 2.0, at.y + h / 2.0), gh, theme::with_alpha(accent, alpha));
        ui.painter().galley(pos2(at.x + gh + 5.0, at.y + (h - g.size().y) / 2.0), g, ink);
        return w;
    }
    if app.audio_info.is_none() && app.upgrading_quality {
        let font = badge_font(small);
        let gh = glyph_side(small);
        let now = ui.input(|i| i.time);
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
        let pulse = ((now * 3.5).sin() * 0.5 + 0.5) as f32;
        let alpha = 0.35 + 0.65 * pulse;
        let accent = app.accent_color();
        let ink = theme::with_alpha(theme::lerp_color(accent, Color32::WHITE, 0.20), alpha);
        let mark = theme::with_alpha(accent, alpha);
        let g = widgets::galley(ui, "Upgrading quality", font, ink, 200.0);
        let w = audio_badge_width(ui, app, small);
        let h = g.size().y.max(gh);
        let rect = Rect::from_min_size(at, vec2(w, h));
        let resp = ui.interact(rect, ui.scope_id().with("audio-badge-upgrading"), Sense::hover());
        named(resp, "Audio quality").on_hover_text("Checking your hi-res sources and upgrading audio quality…");
        let mut x = at.x;
        super::glyphs::draw(ui, pos2(x + gh / 2.0, at.y + h / 2.0), gh, super::glyphs::WAVE, mark);
        x += gh + 5.0;
        ui.painter().galley(pos2(x, at.y + (h - g.size().y) / 2.0), g, ink);
        return w;
    }
    let Some(info) = shown_info(app) else { return 0.0 };
    let font = badge_font(small);
    // A mix in progress wears its own name over the quality mark; the details keep the truth.
    let g = widgets::galley(ui, if app.mixing() { "Mixing" } else { info.badge() }, font, Color32::WHITE, 200.0);
    let gh = glyph_side(small);
    // Dolby's lockup carries its own words, so it stands alone instead of beside them.
    let lockup = info.atmos && !app.mixing();
    let glyph = has_quality_glyph(&info) && !lockup;
    let w = audio_badge_width(ui, app, small);
    let h = if lockup { gh } else { g.size().y.max(if glyph { gh } else { 0.0 }) };
    let rect = Rect::from_min_size(at, vec2(w, h));
    // The reference sets the full player's mark in a small clear box: no border, a faint
    // wash, only slightly rounded — a rounded rectangle, not a pill. The box sits lower
    // than the words it holds: level padding put its top edge against the seek line.
    let br = if small { rect } else { Rect::from_min_max(rect.min - vec2(9.0, 3.0), rect.max + vec2(9.0, 9.0)) };
    // The box is centred on its contents, not the contents on the box: the top edge stays
    // where the seek line lets it sit, and the mark and its words sit in the middle of the
    // shape rather than riding high in it.
    let mid = br.center().y;
    if !small {
        ui.painter().rect_filled(br, 7.0, Color32::from_white_alpha(13));
    }
    let resp = ui.interact(br, ui.scope_id().with("audio-badge"), Sense::click());
    let resp = named(resp, "Audio quality").on_hover_cursor(egui::CursorIcon::PointingHand);
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&resp));
    let hover = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered() || open, 0.12);
    // Lossless and Atmos wear the song's colour; anything lossy stays quiet.
    let (ink, tint) = if info.lossless() || info.atmos {
        let accent = app.accent_color();
        (theme::lerp_color(accent, Color32::WHITE, 0.10 + 0.30 * hover), theme::with_alpha(accent, 0.85 + 0.15 * hover))
    } else {
        (theme::with_alpha(theme::text_dim(), 0.85 + 0.15 * hover), theme::text_dim())
    };
    if lockup {
        paint_quality_icon(ui, pos2(rect.center().x, mid), gh, true, tint, false);
    } else {
        let mut x = at.x;
        if glyph {
            let icon_w = gh * quality_mark_aspect(info.atmos);
            paint_quality_icon(ui, pos2(x + icon_w / 2.0, mid), gh, info.atmos, tint, true);
            x += icon_w + 5.0;
        }
        ui.painter().galley(pos2(x, mid - g.size().y / 2.0), g, ink);
    }
    egui::Popup::menu(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| audio_info_menu(ui, &info));
    w
}

/// Just the quality glyph, centred on `center`, for the mini player's artist line.
pub(super) fn audio_badge_icon_width(app: &App, small: bool) -> f32 {
    if app.audio_info.is_none() && app.upgrading_quality {
        return glyph_side(small);
    }
    let Some(info) = shown_info(app).filter(|i| has_quality_glyph(i)) else { return 0.0 };
    glyph_side(small) * quality_mark_aspect(info.atmos)
}

pub(super) fn audio_badge_icon(app: &App, ui: &mut Ui, center: egui::Pos2, small: bool) {
    if app.audio_info.is_none() && app.upgrading_quality {
        let gh = glyph_side(small);
        let now = ui.input(|i| i.time);
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
        let pulse = ((now * 3.5).sin() * 0.5 + 0.5) as f32;
        let alpha = 0.35 + 0.65 * pulse;
        let accent = app.accent_color();
        let mark = theme::with_alpha(accent, alpha);
        super::glyphs::draw(ui, center, gh, super::glyphs::WAVE, mark);
        return;
    }
    let Some(info) = shown_info(app).filter(|i| has_quality_glyph(i)) else { return };
    let gh = glyph_side(small);
    let rect = Rect::from_center_size(center, vec2(gh * quality_mark_aspect(info.atmos), gh));
    let resp = ui.interact(rect, ui.scope_id().with("audio-badge"), Sense::click());
    let resp = named(resp, "Audio quality").on_hover_cursor(egui::CursorIcon::PointingHand);
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&resp));
    let hover = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered() || open, 0.12);
    let accent = app.accent_color();
    let tint = theme::with_alpha(accent, 0.85 + 0.15 * hover);
    paint_quality_icon(ui, center, gh, info.atmos, tint, true);
    egui::Popup::menu(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| audio_info_menu(ui, &info));
}

/// What the stream is: its source, format and the numbers under those.
fn audio_info_menu(ui: &mut Ui, info: &crate::hires::Info) {
    ui.set_width(300.0);
    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
    ui.add_space(4.0);
    menu_caption(ui, "AUDIO QUALITY");
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
    let g = widgets::galley(ui, info.badge(), theme::semibold(17.0), theme::text(), 200.0);
    let mut x = r.left() + 12.0;
    if info.atmos {
        // The lockup says "Dolby Atmos" itself.
        let ih = 15.0;
        let icon_w = ih * quality_icon_aspect(true);
        paint_quality_icon(ui, pos2(x + icon_w / 2.0, r.center().y), ih, true, theme::text(), false);
    } else {
        if has_quality_glyph(info) {
            let ih = 17.0;
            let icon_w = ih * quality_mark_aspect(false);
            paint_quality_icon(ui, pos2(x + icon_w / 2.0, r.center().y), ih, false, theme::text(), true);
            x += icon_w + 8.0;
        }
        ui.painter().galley(pos2(x, r.center().y - g.size().y / 2.0), g.clone(), theme::text());
    }
    menu_divider(ui);
    if !info.source.is_empty() || !info.host.is_empty() {
        info_row(ui, "Source", if info.source.is_empty() { &info.host } else { &info.source });
    }
    if !info.codec.is_empty() {
        info_row(ui, "Codec", &info.codec.to_ascii_uppercase());
    }
    if let Some(kbps) = bitrate(info) {
        info_row(ui, "Bitrate", &format!("{} kbps", thousands(kbps)));
    }
    if let Some(hz) = info.sample_rate {
        info_row(ui, "Sample rate", &rate_text(hz));
    }
    if let Some(bits) = info.bit_depth {
        info_row(ui, "Bit depth", &format!("{bits}-bit"));
    }
    if let Some(n) = info.channels {
        info_row(ui, "Channels", &channels_text(n.into()));
    }
    if info.size > 0 {
        info_row(ui, "Size", &size_text(info.size));
    }
    if info.duration_ms > 0 {
        info_row(ui, "Duration", &super::fmt_time((info.duration_ms / 1000) as u32));
    }
    ui.add_space(6.0);
}

/// Stats for nerds, on the foot of the sleeve: what the decoder is being fed, from
/// whichever source. Nothing while the setting is off.
fn nerd_stats(app: &App, ui: &Ui, art: Rect, round: bool, lift: f32) {
    let Some((_, stats)) = app.stream_stats.as_ref().filter(|_| app.show_nerd_stats) else { return };
    let ink = theme::with_alpha(Color32::WHITE, 0.65);
    let g = widgets::galley(ui, &stats_line(stats), theme::semibold(11.0), ink, art.width() - 20.0);
    // A disc narrows toward its foot: the line rises until it fits across.
    let foot = if round {
        let r = art.width() / 2.0;
        let half = (g.size().x / 2.0 + 10.0).min(r);
        art.center().y + (r * r - half * half).sqrt() - 8.0
    } else {
        art.bottom() - 8.0
    };
    let at = pos2(art.center().x - g.size().x / 2.0, foot - lift - g.size().y);
    // Plain white washes out over a pale cover; the shadow under it keeps it legible.
    ui.painter().galley_with_override_text_color(at + vec2(0.0, 1.0), g.clone(), Color32::from_black_alpha(140));
    ui.painter().galley(at, g, ink);
}

/// The stats as one line: `Opus · 48 kHz · 133 kbps · Stereo`.
fn stats_line(s: &crate::stream::Stats) -> String {
    let mut parts = vec![s.codec.clone()];
    if let Some(bits) = s.bit_depth {
        parts.push(format!("{bits}-bit"));
    }
    parts.push(rate_text(s.sample_rate));
    if let Some(kbps) = s.bitrate_kbps {
        parts.push(format!("{} kbps", thousands(kbps)));
    }
    parts.push(channels_text(s.channels));
    parts.join(" · ")
}

fn channels_text(n: u16) -> String {
    match n {
        1 => "Mono".to_owned(),
        2 => "Stereo".to_owned(),
        _ => format!("{n} channels"),
    }
}

fn info_row(ui: &mut Ui, label: &str, value: &str) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    ui.painter().text(pos2(r.left() + 12.0, r.center().y), egui::Align2::LEFT_CENTER, label, theme::regular(13.0), theme::text_dim());
    let g = widgets::galley(ui, value, theme::semibold(13.0), theme::text(), 190.0);
    ui.painter().galley(pos2(r.right() - 12.0 - g.size().x, r.center().y - g.size().y / 2.0), g, theme::text());
}

/// The stated bitrate, or the one the file's size and length imply.
fn bitrate(info: &crate::hires::Info) -> Option<u32> {
    info.bitrate_kbps
        .or_else(|| (info.duration_ms > 0 && info.size > 0).then(|| (info.size * 8 / info.duration_ms) as u32))
        .filter(|k| *k > 0)
}

fn thousands(n: u32) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn rate_text(hz: u32) -> String {
    if hz % 1000 == 0 { format!("{} kHz", hz / 1000) } else { format!("{:.1} kHz", hz as f64 / 1000.0) }
}

fn size_text(bytes: u64) -> String {
    if bytes >= 1024 * 1024 { format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0)) } else { format!("{:.0} KB", bytes as f64 / 1024.0) }
}
