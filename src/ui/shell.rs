//! App chrome: background, sidebar, top bar, player bar, queue drawer, toasts.

use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, Ui, Vec2, pos2, vec2};

use super::widgets::{self, icon_at, named, paint_cover, text_at};
use super::{Action, App, LibTab, NpTab, Page, Repeat, fmt_time, glyphs};
use crate::model::Target;
use crate::shader::Params;
use crate::theme::{self, icon};

// ------------------------------------------------------------------ background

/// Vertical gradient (top colour → bottom colour) over `rect`.
pub fn vgradient(painter: &egui::Painter, rect: Rect, top: Color32, bottom: Color32) {
    gradient(painter, rect, [top, top, bottom, bottom]);
}

/// Horizontal gradient (left colour → right colour) over `rect`.
pub fn hgradient(painter: &egui::Painter, rect: Rect, left: Color32, right: Color32) {
    gradient(painter, rect, [left, right, left, right]);
}

/// Corner colours: left top, right top, left bottom, right bottom.
fn gradient(painter: &egui::Painter, rect: Rect, c: [Color32; 4]) {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), c[0]);
    mesh.colored_vertex(rect.right_top(), c[1]);
    mesh.colored_vertex(rect.left_bottom(), c[2]);
    mesh.colored_vertex(rect.right_bottom(), c[3]);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 3, 2);
    painter.add(egui::Shape::mesh(mesh));
}

/// The background mode browse pages use: the theme's own choice, else whatever now
/// playing uses.
pub(crate) fn page_mode(look: &crate::preset::Preset) -> crate::preset::BgMode {
    use crate::preset::{BgMode, PageBg};
    match look.app.page_bg {
        PageBg::Same => look.background.mode,
        PageBg::Static => BgMode::CoverStatic,
        PageBg::Accent => BgMode::AccentGradient,
        PageBg::Solid => BgMode::Solid,
    }
}

/// Whether the pages' background moves while music plays (static themes need no frames).
pub fn pages_bg_moves(look: &crate::preset::Preset) -> bool {
    use crate::preset::BgMode;
    let b = &look.background;
    matches!(page_mode(look), BgMode::Kawarp | BgMode::CoverFullbleed) && b.speed > 0.0 && b.intensity > 0.0
}

/// `only`: paint just this part (the rest is covered by now playing).
/// The background's flat colour, under its art and gradients.
pub fn base_color(app: &App) -> Color32 {
    app.look().solid()
}

/// Whatever the pages are actually sitting on right now: `background` lays this down
/// before any page paints over it, so a wash that means to vanish into the page has to
/// end on this, not on the theme's own background colour.
pub fn page_ground(app: &App) -> Color32 {
    if app.page_blobs { base_color(app) } else if theme::light() { theme::bg() } else { Color32::BLACK }
}

pub fn background(app: &mut App, ui: &mut Ui, screen: Rect, now: f64, only: Option<Rect>) {
    use crate::preset::BgMode;
    let dt = (now - app.last_frame).clamp(0.0, 0.1);
    app.last_frame = now;
    let look = app.look();
    let b = &look.background;
    let mode = page_mode(look);
    // Light pages are the art at full strength under a white wash, a pale tint of the cover.
    let light = theme::light();
    let dim = if light { 1.0 } else { look.app.page_dim.unwrap_or(b.page_dim).clamp(0.02, 1.0) };
    // Now playing animates the shader itself, so keep the clock running for it too.
    let animated = matches!(b.mode, BgMode::Kawarp | BgMode::CoverFullbleed) && b.speed > 0.0;
    let moving = app.anim_bg && animated && app.playing();
    if moving {
        app.bg_time += dt * b.speed as f64;
    }
    let painter = match only {
        Some(r) => ui.painter().with_clip_rect(r),
        None => ui.painter().clone(),
    };
    painter.rect_filled(screen, 0.0, base_color(app));
    if !app.page_blobs {
        painter.rect_filled(screen, 0.0, if light { theme::bg() } else { Color32::BLACK });
        return;
    }
    let art = matches!(mode, BgMode::Kawarp | BgMode::CoverStatic | BgMode::CoverFullbleed);
    if let Some(bg) = app.bg.as_ref().filter(|_| art && !app.disable_blur) {
        let blend = theme::ease_in_out(((now - app.art_changed) / 1.4) as f32);
        if blend < 1.0 {
            // The crossfade to new art finishes even while paused.
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
        }
        let level = if app.reactive_bg && app.playing() { app.audio.status.level() * b.reactive } else { 0.0 };
        let still = mode == BgMode::CoverStatic;
        bg.paint(
            &painter,
            screen,
            Params {
                time: app.bg_time as f32,
                blend,
                intensity: if still { 0.0 } else { b.intensity * 0.9 },
                saturation: b.saturation * 0.9,
                dim,
                scale: (b.scale - 0.08).max(1.0) + if still { 0.0 } else { level.min(0.5) * 0.08 },
            },
        );
    } else if mode == BgMode::Solid {
        // Plain colour: nothing over it.
    } else {
        vgradient(&painter, screen, theme::with_alpha(app.accent_color(), 0.18 * dim / 0.5), look.solid());
    }
    if light {
        painter.rect_filled(screen, 0.0, Color32::from_white_alpha(212));
    } else if mode != BgMode::Solid {
        // Keep text readable: darken toward the bottom and under the sidebar.
        vgradient(&painter, screen, theme::shade(0.12), theme::shade(0.55));
    }
}

// ------------------------------------------------------------------ sidebar

/// Sidebar width expanded and collapsed to an icon rail (points). The reference panel
/// measures 280 pt.
pub const SIDEBAR_W: f32 = 280.0;
pub const RAIL_W: f32 = 76.0;

/// The sidebar's material: a hair lighter than whatever page it sits on, so the
/// artwork's colour still shows through on a collection but the panel reads as frosted
/// glass on the flat black browse pages.
pub fn sidebar_material(light: bool, a: f32) -> Color32 {
    if light {
        theme::panel(a)
    } else {
        Color32::from_rgba_unmultiplied(255, 246, 240, (28.0 + 18.0 * a.clamp(0.0, 1.0)).min(255.0) as u8)
    }
}

/// How collapsed the sidebar is, 0 (full) to 1 (icon rail), eased over the toggle.
pub fn sidebar_collapse(app: &App, ctx: &egui::Context) -> f32 {
    let t = ctx.animate_bool_with_time(egui::Id::unique("sidebar-collapse"), app.sidebar_collapsed, 0.28 * theme::SPEED);
    // Ease in-out: starts and lands gently in both directions.
    t * t * (3.0 - 2.0 * t)
}

/// The small chevron that walks the page history, centred on `at`.
fn history_button(ui: &mut Ui, at: Pos2, glyph: char, name: &str) -> bool {
    let rect = Rect::from_center_size(at, Vec2::splat(22.0));
    let resp = named(ui.interact(rect, ui.id().with(name), Sense::click()), name).on_hover_text(name).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
    if h > 0.0 {
        ui.painter().circle_filled(rect.center(), 11.0, theme::glass(0.10 * h));
    }
    icon_at(ui, rect.center(), glyph, 16.0, theme::lerp_color(theme::text_dim(), theme::text(), h));
    resp.clicked()
}

/// One layout for both states, so collapsing animates: the panel narrows while icons
/// and covers keep their column and the labels fade and clip.
pub fn sidebar(app: &mut App, ui: &mut Ui) {
    // Measured off the reference panel at 2x: rows walk on a 44 pt pitch and the mark
    // sits 28 in from the content edge, the label 56. The pill wears most of that pitch.
    const ICON_X: f32 = 28.0;
    const LABEL_X: f32 = 56.0;
    const ROW_H: f32 = 44.0;
    const PILL_H: f32 = 38.0;
    // Rows carry their own pitch, so the theme's 6 pt between items would push it to 50.
    ui.spacing_mut().item_spacing.y = 0.0;
    // The box at the head of this panel used to anchor the suggestions; the field now
    // lives on the search page, so nothing to reset but the stale rect.
    app.search_rect = None;
    let t = sidebar_collapse(app, ui.ctx());
    let collapsed = app.sidebar_collapsed;
    // Labels fade out over the first two thirds of collapsing (and back in late).
    let a = ((1.0 - t) * 1.5 - 0.5).clamp(0.0, 1.0);
    // Content width when expanded: text keeps its truncation while the panel narrows.
    let full_w = ui.available_width() + (SIDEBAR_W - RAIL_W) * t;
    // The icon column slides to the rail's centre as the panel narrows.
    let ix = ICON_X - (ICON_X - 22.0) * t;
    let radius = PILL_H / 2.0;
    let accent = app.accent_color();
    let mut go_to: Option<Page> = None;

    // The panel's head: the BitChord mark at the left, the history chevrons at the right
    // edge. On the rail the mark keeps the top and the chevrons drop onto a line under it.
    {
        let (hr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 14.0 + 43.0 + 40.0 * t), Sense::hover());
        let top = hr.bottom() - 43.0 - 40.0 * t;
        if t < 0.5 {
            ui.multiply_opacity(1.0 - 2.0 * t);
            let logo = Rect::from_center_size(pos2(hr.left() + 24.0, top + 19.0), vec2(30.0, 20.0));
            mark(ui, logo, theme::text());
            let resp = named(ui.interact(logo.expand(6.0), ui.id().with("sidebar-home"), Sense::click()), "Home").on_hover_cursor(egui::CursorIcon::PointingHand);
            if resp.clicked() {
                app.actions.push(Action::Go(Page::Home));
            }
            if history_button(ui, pos2(hr.right() - 42.0, top + 19.0), icon::CHEVRON_LEFT, "Back") {
                app.nav_back();
            }
            if history_button(ui, pos2(hr.right() - 16.0, top + 19.0), icon::CHEVRON_RIGHT, "Forward") {
                app.nav_forward();
            }
            ui.multiply_opacity(1.0);
        } else {
            ui.multiply_opacity(2.0 * t - 1.0);
            let logo = Rect::from_center_size(pos2(hr.left() + 22.0, hr.top() + 18.0), vec2(30.0, 20.0));
            mark(ui, logo, theme::text());
            let resp = named(ui.interact(logo.expand(6.0), ui.id().with("sidebar-home"), Sense::click()), "Home");
            if resp.clicked() {
                app.actions.push(Action::Go(Page::Home));
            }
            let yc = hr.top() + 52.0;
            if history_button(ui, pos2(hr.left() + 12.0, yc), icon::CHEVRON_LEFT, "Back") {
                app.nav_back();
            }
            if history_button(ui, pos2(hr.left() + 34.0, yc), icon::CHEVRON_RIGHT, "Forward") {
                app.nav_forward();
            }
            ui.multiply_opacity(1.0);
        }
    }
    ui.add_space(20.0);

    // A group's heading: Apple's grey title at the left, the chevron that folds it at
    // the right. It walks the same 44 pitch as the rows under it.
    let head = |ui: &mut Ui, label: &str, open: &mut bool| {
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
        let resp = named(resp, label).on_hover_cursor(egui::CursorIcon::PointingHand);
        if a > 0.0 {
            ui.multiply_opacity(a);
            ui.painter().text(pos2(rect.left() + 17.0, rect.center().y), Align2::LEFT_CENTER, label, theme::semibold(17.0), theme::with_alpha(theme::text(), 0.55));
            icon_at(ui, pos2(rect.right() - 22.0, rect.center().y), if *open { icon::CHEVRON_DOWN } else { icon::CHEVRON_RIGHT }, 15.0, theme::with_alpha(theme::text(), 0.55));
            ui.multiply_opacity(1.0);
        }
        if resp.clicked() {
            *open = !*open;
        }
    };
    let nav = |ui: &mut Ui, glyph: &'static [u8], size: f32, label: &str, page: Page, selected: bool, go_to: &mut Option<Page>| {
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
        let mut resp = named(resp, label).on_hover_cursor(egui::CursorIcon::PointingHand);
        if collapsed {
            resp = resp.on_hover_text(label);
        }
        let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
        let s = theme::anim_bool(ui.ctx(), resp.id.with("s"), selected, 0.2);
        let pill = Rect::from_center_size(rect.center(), vec2(rect.width(), PILL_H));
        ui.painter().rect_filled(pill, radius, theme::glass(0.05 * h + 0.10 * s));
        glyphs::draw(ui, pos2(rect.left() + ix, rect.center().y), size, glyph, accent);
        if a > 0.0 {
            ui.painter().text(pos2(rect.left() + LABEL_X, rect.center().y), Align2::LEFT_CENTER, label, theme::regular(17.0), theme::with_alpha(theme::text(), a));
        }
        if resp.clicked() {
            *go_to = Some(page);
        }
    };
    let p = app.page.clone();
    const APPLE: f32 = glyphs::APPLE_GLYPH;
    const LUCIDE: f32 = glyphs::LUCIDE_GLYPH;
    // Apple's "Favourite Songs" mark: not a line icon but a white tile wearing a red
    // star, the only coloured badge in the sidebar.
    let star = |ui: &mut Ui, label: &str, page: Page, selected: bool, go_to: &mut Option<Page>| {
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
        let mut resp = named(resp, label).on_hover_cursor(egui::CursorIcon::PointingHand);
        if collapsed {
            resp = resp.on_hover_text(label);
        }
        let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
        let s = theme::anim_bool(ui.ctx(), resp.id.with("s"), selected, 0.2);
        let pill = Rect::from_center_size(rect.center(), vec2(rect.width(), PILL_H));
        ui.painter().rect_filled(pill, radius, theme::glass(0.05 * h + 0.10 * s));
        let tile = Rect::from_center_size(pos2(rect.left() + ix, rect.center().y), vec2(22.0, 22.0));
        ui.painter().rect_filled(tile, 5.0, Color32::from_rgb(245, 245, 247));
        glyphs::draw(ui, tile.center(), 15.0, glyphs::STAR_FILLED, Color32::from_rgb(227, 62, 75));
        if a > 0.0 {
            ui.painter().text(pos2(rect.left() + LABEL_X, rect.center().y), Align2::LEFT_CENTER, label, theme::regular(17.0), theme::with_alpha(theme::text(), a));
        }
        if resp.clicked() {
            *go_to = Some(page);
        }
    };
    // The reference's sidebar opens with a Search row, not a field: the box itself
    // lives on the search page.
    nav(ui, glyphs::SEARCH, APPLE, "Search", Page::Search(String::new()), matches!(p, Page::Search(_)), &mut go_to);
    nav(ui, glyphs::HOME, APPLE, "Home", Page::Home, p == Page::Home, &mut go_to);
    nav(ui, glyphs::EXPLORE, APPLE, "New", Page::New, p == Page::New, &mut go_to);
    ui.add_space(14.0);
    head(ui, "Library", &mut app.library_open);
    if app.library_open {
        nav(ui, glyphs::MIC, APPLE, "Artists", Page::Library(LibTab::Artists), p == Page::Library(LibTab::Artists), &mut go_to);
        nav(ui, glyphs::ALBUM, APPLE, "Albums", Page::Library(LibTab::Albums), p == Page::Library(LibTab::Albums), &mut go_to);
        // Songs is the list of what has been heard lately, the way the reference's
        // sidebar has it; there is no separate Recently Played row.
        nav(ui, glyphs::MUSIC_NOTE, APPLE, "Songs", Page::Library(LibTab::History), p == Page::Library(LibTab::History), &mut go_to);
        nav(ui, glyphs::DOWNLOAD, APPLE, "Downloaded", Page::Downloads, p == Page::Downloads, &mut go_to);
    }

    // Playlists that appeared since the order was last changed join it at the end, so a
    // drag can place any of them.
    if let Some(l) = &app.library {
        for c in &l.playlists {
            if let Target::Playlist(id) = &c.target
                && !app.playlist_order.contains(id)
            {
                app.playlist_order.push(id.clone());
            }
        }
    }
    ui.add_space(14.0);
    head(ui, "Playlists", &mut app.playlists_open);
    nav(ui, glyphs::LIBRARY, APPLE, "All Playlists", Page::Library(LibTab::Playlists), p == Page::Library(LibTab::Playlists), &mut go_to);
    star(ui, "Favourite Songs", Page::Playlist(super::LIKED_MUSIC.to_owned()), matches!(&p, Page::Playlist(id) if id == super::LIKED_MUSIC), &mut go_to);

    // Drag-to-reorder and Edit-mode removal, applied once the rows are done.
    let mut reorder: Option<(String, String)> = None;
    let mut hide: Option<String> = None;

    let transferring = super::transfer::sidebar_visible(app, ui.input(|i| i.time));
    // Room under the list: the account row, and the transfer card.
    let bottom_h = if transferring { 148.0 } else { 48.0 };
    let list_h = (ui.available_height() - bottom_h).max(0.0);
    ui.allocate_ui(vec2(ui.available_width(), list_h), |ui| {
        if !app.logged_in {
            // Too wide to squeeze: cross-fade the message and the rail's icon.
            if t < 0.5 {
                ui.multiply_opacity(1.0 - 2.0 * t);
                ui.label(egui::RichText::new("Sign in to see your playlists").color(theme::text_dim()));
                ui.add_space(8.0);
                if widgets::pill(ui, "Sign in", None, false).clicked() {
                    app.start_sign_in(ui.ctx());
                }
            } else {
                ui.multiply_opacity(2.0 * t - 1.0);
                nav(ui, glyphs::PERSON_CIRCLE, LUCIDE, "Sign in", Page::Settings, false, &mut go_to);
            }
            return;
        }
        if !app.playlists_open {
            return;
        }
        let bars = if t > 0.0 { egui::scroll_area::ScrollBarVisibility::AlwaysHidden } else { egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded };
        egui::ScrollArea::vertical().id_salt("sidebar-pl").auto_shrink(false).scroll_bar_visibility(bars).show(ui, |ui| {
            // Last launch's list until the library arrives.
            let Some(playlists) = app.library.as_ref().map(|l| &l.playlists).or(app.sidebar_cache.as_ref()) else {
                // The spinner animates every frame: give up on a request that never answered.
                if app.library_requested.elapsed() < std::time::Duration::from_secs(20) {
                    ui.horizontal(|ui| {
                        ui.add_space(ICON_X - 8.0);
                        ui.spinner();
                    });
                } else if t < 0.5 {
                    ui.multiply_opacity(1.0 - 2.0 * t);
                    ui.label(egui::RichText::new("Couldn't load your playlists").color(theme::text_dim()));
                    ui.add_space(8.0);
                    if widgets::pill(ui, "Try again", None, false).clicked() {
                        app.request_library();
                    }
                }
                return;
            };
            let mut visible: Vec<&crate::model::Card> = playlists
                .iter()
                .filter(|c| !matches!(&c.target, Target::Playlist(id) if matches!(id.as_str(), "LM" | "VLLM") || app.hidden_playlists.contains(id)))
                .collect();
            visible.sort_by_key(|c| match &c.target {
                Target::Playlist(id) => app.playlist_order.iter().position(|x| x == id).unwrap_or(usize::MAX),
                _ => usize::MAX,
            });
            let drag_key = egui::Id::new("sidebar-pl-drag");
            let dragging: Option<String> = ui.data(|d| d.get_temp(drag_key));
            let down = ui.input(|i| i.pointer.any_down());
            let pointer = ui.input(|i| i.pointer.interact_pos());
            for c in &visible {
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click_and_drag());
                let id = match &c.target {
                    Target::Playlist(id) => id.clone(),
                    _ => continue,
                };
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                let is_dragged = dragging.as_deref() == Some(id.as_str());
                let mut resp = named(resp, &c.title).on_hover_cursor(if is_dragged { egui::CursorIcon::Grabbing } else { egui::CursorIcon::PointingHand });
                if collapsed {
                    resp = resp.on_hover_text(&c.title);
                }
                if resp.drag_started() {
                    ui.data_mut(|d| d.insert_temp(drag_key, id.clone()));
                }
                if let Some(did) = &dragging
                    && did != &id
                    && down
                    && pointer.is_some_and(|p| rect.contains(p))
                {
                    reorder = Some((did.clone(), id.clone()));
                }
                let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
                let selected = matches!(&c.target, Target::Playlist(id) if app.page == Page::Playlist(id.clone()));
                let s = theme::anim_bool(ui.ctx(), resp.id.with("s"), selected, 0.2);
                let lift = if is_dragged { 0.10 } else { 0.0 };
                let pill = Rect::from_center_size(rect.center(), vec2(rect.width(), PILL_H));
                ui.painter().rect_filled(pill, radius, theme::glass(0.05 * h + 0.10 * s + lift));
                let img = Rect::from_center_size(pos2(rect.left() + ix, rect.center().y), Vec2::splat(22.0));
                paint_cover(ui, img, &c.thumbs, 4.0, 1.0);
                if a > 0.0 {
                    let w = full_w - LABEL_X - 12.0;
                    text_at(ui, pos2(rect.left() + LABEL_X, rect.center().y - 10.5), &c.title, theme::regular(17.0), theme::with_alpha(theme::text(), a), w);
                }
                egui::Popup::context_menu(&resp).show(|ui| {
                    ui.set_min_width(190.0);
                    if ui.button("Hide from sidebar").clicked() {
                        hide = Some(id.clone());
                        ui.close();
                    }
                });
                if resp.clicked() && !is_dragged {
                    app.actions.push(Action::Open(c.target.clone()));
                }
            }
            if ui.input(|i| i.pointer.any_released()) {
                ui.data_mut(|d| d.remove_temp::<String>(drag_key));
            }
        });
    });
    if let Some(id) = hide {
        app.hidden_playlists.insert(id);
    }
    if let Some((dragged, target)) = reorder {
        if let (Some(from), Some(to)) = (app.playlist_order.iter().position(|x| x == &dragged), app.playlist_order.iter().position(|x| x == &target)) {
            let id = app.playlist_order.remove(from);
            app.playlist_order.insert(to.min(app.playlist_order.len()), id);
        }
    }
    let ready = app.updater.as_ref().and_then(|u| match u.state() {
        crate::update::State::Ready { version, .. } => Some(version),
        _ => None,
    });
    let mut restart = false;
    ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
        if let Some(v) = &ready {
            ui.add_space(6.0);
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::click());
            let mut resp = named(resp, "Restart to update").on_hover_cursor(egui::CursorIcon::PointingHand);
            if collapsed {
                resp = resp.on_hover_text(format!("Restart to update to BitChord {v}"));
            }
            let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
            ui.painter().rect_filled(rect, 12.0, theme::with_alpha(accent, 0.22 + 0.12 * h));
            glyphs::draw(ui, pos2(rect.left() + ix, rect.center().y), LUCIDE, glyphs::SPARKLES, theme::text());
            if a > 0.0 {
                ui.painter().text(pos2(rect.left() + 46.0, rect.top() + 17.0), Align2::LEFT_CENTER, "Restart to update", theme::semibold(13.5), theme::with_alpha(theme::text(), a));
                ui.painter().text(pos2(rect.left() + 46.0, rect.top() + 35.0), Align2::LEFT_CENTER, format!("BitChord {v}"), theme::regular(12.0), theme::with_alpha(theme::text(), 0.75 * a));
            }
            restart = resp.clicked();
        }
        if transferring {
            super::transfer::sidebar_card(app, ui, collapsed, a, ICON_X);
        }
        // The panel closes the way the reference does: the account's picture and name on
        // a hairline. It is also the way into Settings.
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
        let resp = named(resp, "Account").on_hover_cursor(egui::CursorIcon::PointingHand);
        let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
        ui.painter().line_segment([pos2(rect.left(), rect.min.y), pos2(rect.right(), rect.min.y)], Stroke::new(1.0, theme::with_alpha(theme::text(), 0.07)));
        let pill = Rect::from_center_size(rect.center(), vec2(rect.width(), PILL_H));
        ui.painter().rect_filled(pill, radius, theme::glass(0.05 * h));
        let img = Rect::from_center_size(pos2(rect.left() + ix, rect.center().y), Vec2::splat(24.0));
        let initial = app.account_name.as_deref().and_then(|n| n.chars().next()).unwrap_or('?');
        if app.logged_in {
            ui.painter().circle_filled(img.center(), 12.0, theme::lerp_color(accent, Color32::BLACK, 0.35));
            if let Some(avatar) = &app.account_avatar {
                widgets::paint_remote(ui, avatar, img, 12.0, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            } else {
                ui.painter().text(img.center(), Align2::CENTER_CENTER, initial, theme::bold(11.0), Color32::WHITE);
            }
        } else {
            ui.painter().circle_filled(img.center(), 12.0, theme::glass(0.12));
            icon_at(ui, img.center(), icon::USER, 13.0, theme::text_dim());
        }
        if a > 0.0 {
            let name = app.account_name.as_deref().unwrap_or("Sign in");
            widgets::text_at(ui, pos2(rect.left() + LABEL_X, rect.center().y - 10.5), name, theme::regular(17.0), theme::with_alpha(theme::text(), a), full_w - LABEL_X - 14.0);
        }
        if resp.clicked() {
            go_to = Some(Page::Settings);
        }
    });
    if restart {
        app.restart_to_update(ui.ctx());
    }
    if let Some(page) = go_to {
        if matches!(page, Page::Search(_)) {
            // Search lands on the search page with the caret in the box; already there,
            // it just brings the box back.
            app.search_refocus = true;
            if !matches!(app.page, Page::Search(_)) {
                app.actions.push(Action::Go(Page::Search(String::new())));
            }
        } else {
            app.actions.push(Action::Go(page));
        }
    }
}

// ------------------------------------------------------------------ top bar

pub fn top_bar(app: &mut App, ui: &mut Ui) {
    // This corner carries nothing but the loading spinner: the round ⋯ and its menu are
    // gone, and Settings and Account stay where the sidebar already puts them.
    if !app.loading {
        return;
    }
    let panel = ui.max_rect();
    let row = Rect::from_min_size(panel.min, vec2(panel.width(), 44.0));
    let right = panel.right() - super::titlebar::right_gap(ui.ctx(), row, 20.0);
    let spin = Rect::from_min_size(pos2(right - 78.0, panel.top() + 22.0), Vec2::splat(20.0));
    // Its own layer, above the pages: a page's header art scrolls up into this corner.
    egui::Area::new(ui.scope_id().with("more")).order(egui::Order::Middle).fixed_pos(spin.min).show(ui.ctx(), |ui| {
        let mut cui = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(pos2(0.0, 0.0), spin.size())));
        cui.spinner();
    });
}

/// The search pill: typeahead, suggestions and Enter to search. The Search page draws
/// it; `app.search_rect` anchors the suggestions under it.
pub fn search_field(app: &mut App, ui: &mut Ui, width: f32) {
    // Anywhere on the box focuses the field, not just on the text area inside it.
    let (rect, field) = ui.allocate_exact_size(vec2(width, 44.0), Sense::click());
    let field = field.on_hover_cursor(egui::CursorIcon::Text);
    let focus = theme::anim_bool(ui.ctx(), "search-focus", app.search_focused, 0.2);
    let accent = app.accent_color();
    ui.painter().rect_filled(rect, 22.0, theme::glass(0.12 + 0.04 * focus));
    if focus > 0.0 {
        ui.painter().rect_stroke(rect, 22.0, egui::Stroke::new(1.0, theme::with_alpha(accent, 0.55 * focus)), egui::StrokeKind::Inside);
    }
    icon_at(ui, pos2(rect.left() + 22.0, rect.center().y), icon::SEARCH, 18.0, theme::lerp_color(theme::text_dim(), theme::text(), 0.4));
    let clearable = !app.search_text.is_empty();
    // Apple's box wears a microphone at the right. There is no dictation here, so it is
    // decoration and says so on hover.
    if !clearable {
        let mic = Rect::from_center_size(pos2(rect.right() - 26.0, rect.center().y), Vec2::splat(26.0));
        let resp = named(ui.interact(mic, ui.scope_id().with("search-mic"), Sense::hover()), "Voice search");
        glyphs::draw(ui, mic.center(), 20.0, glyphs::MIC, theme::text_dim());
        resp.on_hover_text("Voice search isn't available");
    }
    let inner = Rect::from_min_max(pos2(rect.left() + 44.0, rect.top() + 4.0), pos2(rect.right() - if clearable { 40.0 } else { 52.0 }, rect.bottom() - 4.0));
    // Up and Down walk the suggestions (past either end back to what was typed). Taken
    // before the box sees them, where they'd move the caret. Only bare arrows (Shift
    // selects text), and only suggestions for what's typed now, not ones still being replaced.
    let fresh = app.suggestions.0 == app.search_text.trim();
    let n = if fresh { app.suggestions.1.len() } else { 0 };
    if app.search_focused && n > 0 && ui.input(|i| i.modifiers.is_none()) {
        ui.input_mut(|i| {
            if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                app.suggest_sel = match app.suggest_sel {
                    None => Some(0),
                    Some(s) if s + 1 < n => Some(s + 1),
                    Some(_) => None,
                };
            }
            if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                app.suggest_sel = match app.suggest_sel {
                    None => Some(n - 1),
                    Some(0) => None,
                    Some(s) => Some(s - 1),
                };
            }
        });
    }
    let edit = ui.put(
        inner,
        egui::TextEdit::singleline(&mut app.search_text)
            .hint_text(egui::RichText::new("Artists, Songs, Lyrics and More").color(theme::text_faint()))
            .font(theme::regular(17.0))
            .frame(egui::Frame::NONE)
            .align(egui::Align2::LEFT_CENTER)
            .desired_width(inner.width()),
    );
    app.search_rect = Some(rect);
    if std::mem::take(&mut app.search_refocus) {
        edit.request_focus();
    }
    app.search_focused = edit.has_focus();
    let mut cleared = false;
    if clearable {
        let x = Rect::from_center_size(pos2(rect.right() - 26.0, rect.center().y), Vec2::splat(26.0));
        let resp = named(ui.interact(x, ui.scope_id().with("search-clear"), Sense::click()), "Clear search").on_hover_cursor(egui::CursorIcon::PointingHand);
        let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.15);
        if h > 0.0 {
            ui.painter().circle_filled(x.center(), 13.0, theme::glass(0.10 * h));
        }
        icon_at(ui, x.center(), icon::X, 16.0, theme::lerp_color(theme::text_dim(), theme::text(), h));
        if resp.clicked() {
            // Back to the empty search page: the old results go with the text.
            app.search_text.clear();
            app.search_synced = String::new();
            app.suggestions = Default::default();
            app.suggest_sel = None;
            cleared = true;
            app.actions.push(Action::Go(Page::Search(String::new())));
            edit.request_focus();
        }
    }
    // The search shortcut from Settings › Keyboard shortcuts; the box keeps its own focus.
    let find = super::keys::hit(ui.ctx(), &app.keys, super::keys::Cmd::Search);
    if field.clicked() || find {
        edit.request_focus();
    }
    if edit.changed() || edit.gained_focus() || cleared {
        app.suggest_sel = None;
        let now = ui.input(|i| i.time);
        // Show cached results immediately, then refresh after a short typing pause.
        let first = app.suggestions.1.is_empty();
        let query = app.search_text.trim();
        app.suggestions = app.suggest_cache.iter()
            .find(|(q, _)| q == query)
            .cloned()
            .unwrap_or_default();
        let delay = if first || cleared || edit.gained_focus() { 0.0 } else { 0.06 };
        app.suggest_due = Some(now + delay);
    }
    // The event loop can otherwise sleep past the debounce deadline.
    if let Some(due) = app.suggest_due {
        let now = ui.input(|i| i.time);
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64((due - now).max(0.0)));
    }
    if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        let picked = app.suggest_sel.filter(|_| fresh).and_then(|s| app.suggestions.1.get(s));
        let target = picked.and_then(|s| s.thing.as_ref().map(|t| t.target.clone()));
        let q = picked.map(|s| s.text.clone()).unwrap_or_else(|| app.search_text.trim().to_owned());
        if !q.is_empty() {
            app.suggestions = Default::default();
            // Enter on a row naming a thing opens that thing; on a term it searches.
            app.actions.push(match target {
                Some(t) => Action::Open(t),
                None => Action::Go(Page::Search(q)),
            });
        }
    }
}

/// Search suggestions dropdown under the search box.
pub fn suggestions(app: &mut App, ctx: &egui::Context) {
    let Some(rect) = app.search_rect else { return };
    // Through a press on a remove button too: its release briefly takes the box's focus.
    // An off-screen test window never gets the OS's focus, so YTM_SUGGESTIONS holds the
    // list open the way a click would.
    let focused = app.search_focused || app.forget_held || super::dev_var("YTM_SUGGESTIONS").is_some();
    let show = focused && !app.suggestions.1.is_empty() && !app.np_open;
    let t = theme::anim_bool(ctx, "suggest-open", show, 0.15);
    if t <= 0.0 {
        return;
    }
    egui::Area::new(egui::Id::unique("suggestions"))
        .order(egui::Order::Foreground)
        .fixed_pos(rect.left_bottom() + vec2(0.0, 6.0 - 6.0 * (1.0 - t)))
        .show(ctx, |ui| {
            ui.set_opacity(t);
            egui::Frame::new()
                .fill(theme::menu_fill())
                .corner_radius(14)
                .inner_margin(6)
                .shadow(egui::epaint::Shadow { offset: [0, 10], blur: 30, spread: 0, color: theme::shade(0.5) })
                .show(ui, |ui| {
                    ui.set_width(rect.width() - 12.0);
                    if !ui.input(|i| i.pointer.any_down() || i.pointer.any_released()) {
                        app.forget_held = false;
                    }
                    // Releasing that press is a click outside the box, which takes its focus
                    // (and this list) away: give it back until the press is over.
                    if app.forget_held {
                        app.search_refocus = true;
                    }
                    let now = ui.input(|i| i.time);
                    for (i, s) in app.suggestions.1.clone().into_iter().enumerate() {
                        let term = s.text;
                        // The removed one folds away, and the rows below slide up into its place.
                        if let Some((_, at)) = app.forgetting.as_ref().filter(|(t, _)| s.forget.as_deref() == Some(t.as_str())) {
                            let p = ((now - at) / 0.18).clamp(0.0, 1.0) as f32;
                            let e = 1.0 - (1.0 - p).powi(3);
                            let gap = ui.spacing().item_spacing.y;
                            // Its spacing folds away with it, so nothing jumps at the end.
                            ui.spacing_mut().item_spacing.y = 0.0;
                            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), (40.0 + gap) * (1.0 - e)), Sense::hover());
                            ui.spacing_mut().item_spacing.y = gap;
                            ui.scope(|ui| {
                                ui.set_clip_rect(r.intersect(ui.clip_rect()));
                                ui.set_opacity(1.0 - e);
                                let cy = r.top() + 20.0;
                                icon_at(ui, pos2(r.left() + 20.0, cy), icon::HISTORY, 15.0, theme::text_dim());
                                suggestion_text(ui, pos2(r.left() + 42.0, cy - 9.0), &term, &app.suggestions.0, r.width() - 88.0);
                            });
                            ui.ctx().request_repaint();
                            continue;
                        }
                        let thing = s.thing.clone();
                        let target = thing.as_ref().map(|t| t.target.clone());
                        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), if thing.is_some() { 56.0 } else { 40.0 }), Sense::click());
                        let resp = named(resp, &term).on_hover_cursor(egui::CursorIcon::PointingHand);
                        let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.contains_pointer() || app.suggest_sel == Some(i), 0.1);
                        ui.painter().rect_filled(r, 8.0, theme::glass(0.08 * h));
                        match thing {
                            // A thing the query has already found: its artwork, its name and
                            // what it is, as the reference lists them under the terms.
                            Some(t) => {
                                ui.painter().line_segment(
                                    [pos2(r.left() + 12.0, r.top()), pos2(r.right() - 12.0, r.top())],
                                    Stroke::new(1.0, theme::glass(0.07)),
                                );
                                let art = Rect::from_center_size(pos2(r.left() + 36.0, r.center().y), Vec2::splat(40.0));
                                // Artists are discs, releases and clips are sleeves.
                                let circle = matches!(t.target, Target::Artist(_));
                                paint_cover(ui, art, &t.thumbs, if circle { 20.0 } else { 6.0 }, 1.0);
                                let room = if t.explicit { 96.0 } else { 72.0 };
                                let name = text_at(ui, pos2(r.left() + 68.0, r.center().y - 10.0), &term, theme::regular(15.0), theme::text(), r.width() - room);
                                if t.explicit {
                                    let b = Rect::from_min_size(pos2(name.right() + 6.0, name.top() + 2.0), Vec2::splat(14.0));
                                    ui.painter().rect_filled(b, 3.0, theme::text_faint());
                                    text_at(ui, pos2(b.center().x, b.center().y - 5.0), "E", theme::semibold(10.0), theme::menu_fill(), 12.0);
                                }
                                let by = if t.by.is_empty() { t.kind } else { format!("{} · {}", t.kind, t.by) };
                                text_at(ui, pos2(r.left() + 68.0, r.center().y + 8.0), &by, theme::regular(13.0), theme::text_dim(), r.width() - 84.0);
                            }
                            None => {
                                let glyph = if s.forget.is_some() { icon::HISTORY } else { icon::SEARCH };
                                icon_at(ui, pos2(r.left() + 20.0, r.center().y), glyph, 15.0, theme::text_dim());
                                let room = if s.forget.is_some() { 88.0 } else { 52.0 };
                                suggestion_text(ui, pos2(r.left() + 42.0, r.center().y - 9.0), &term, &app.suggestions.0, r.width() - room);
                            }
                        }
                        // A past search can be removed from the account's history.
                        if let Some(token) = s.forget {
                            let x = Rect::from_center_size(pos2(r.right() - 22.0, r.center().y), Vec2::splat(30.0));
                            let xr = named(ui.interact(x, resp.id.with("forget"), Sense::click()), "Remove from recent searches")
                                .on_hover_text("Remove")
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            let xh = theme::anim_bool(ui.ctx(), xr.id.with("h"), xr.hovered(), 0.1);
                            if xh > 0.0 {
                                ui.painter().circle_filled(x.center(), 15.0, theme::glass(0.12 * xh));
                            }
                            icon_at(ui, x.center(), icon::TRASH_2, 15.0, theme::lerp_color(theme::text_dim(), theme::text(), xh));
                            // On the press, and one per press: the next row's button slides
                            // under the held pointer.
                            if (xr.is_pointer_button_down_on() || xr.clicked()) && !app.forget_held {
                                app.forget_held = true;
                                // One still folding away is done.
                                if let Some((t, _)) = app.forgetting.take() {
                                    app.suggestions.1.retain(|s| s.forget.as_deref() != Some(t.as_str()));
                                }
                                app.forgetting = Some((token.clone(), now));
                                app.suggest_sel = None;
                                app.suggest_round += 1;
                                app.suggest_cache.clear();
                                app.forgets_pending += 1;
                                app.search_refocus = true;
                                app.backend.request(crate::backend::Req::ForgetSearch(token));
                            }
                        }
                        // Pointer-down, not click: the text box loses focus (hiding this) first.
                        if resp.is_pointer_button_down_on() || resp.clicked() {
                            app.suggestions = Default::default();
                            app.search_focused = false;
                            // A row naming a thing goes to that thing; a term searches for it.
                            app.actions.push(match target {
                                Some(t) => Action::Open(t),
                                None => Action::Go(Page::Search(term)),
                            });
                            break;
                        }
                    }
                    if let Some((t, _)) = app.forgetting.take_if(|(_, at)| now - *at >= 0.18) {
                        app.suggestions.1.retain(|s| s.forget.as_deref() != Some(t.as_str()));
                    }
                });
        });
}

/// A suggestion with what's typed in bold and the rest dimmed, as on music.youtube.com.
fn suggestion_text(ui: &Ui, pos: Pos2, text: &str, typed: &str, width: f32) {
    let n = text.chars().zip(typed.chars()).take_while(|(a, b)| a.to_lowercase().eq(b.to_lowercase())).count();
    let split = if n == typed.chars().count() { text.char_indices().nth(n).map_or(text.len(), |(i, _)| i) } else { 0 };
    let (bold, rest) = text.split_at(split);
    let mut job = egui::text::LayoutJob::default();
    job.append(bold, 0.0, egui::TextFormat { font_id: theme::semibold(15.0), color: theme::text(), ..Default::default() });
    job.append(rest, 0.0, egui::TextFormat { font_id: theme::regular(15.0), color: theme::text_dim(), ..Default::default() });
    job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(10.0));
    let g = ui.fonts_mut(|f| f.layout_job(job));
    ui.painter().galley(pos, g, theme::text());
}

// ------------------------------------------------------------------ player bar

/// The song's blurred sleeve as a frosted sheet over `rect`: the same 64 px art the
/// background shader paints, mapped to the window so the capsule carries what's behind
/// it, dimmed under a heavy tint. `false` when no song's art has arrived yet — the caller
/// falls back to a plain panel.
pub fn frost(app: &App, ui: &Ui, rect: Rect, radius: f32, dark: f32) -> bool {
    let Some((tex, _, _)) = frost_textures(app, ui.ctx()) else { return false };
    frost_slice(ui, rect, radius, &tex);
    let a = (dark.clamp(0.0, 1.0) * 255.0) as u8;
    let tint = if theme::light() { Color32::from_white_alpha(a) } else { Color32::from_black_alpha(a) };
    ui.painter().rect_filled(rect, radius, tint);
    true
}

/// The same softness in grey: the sleeve's light and shade, none of its colour. The
/// player pill wants something moving behind its frost without wearing the song.
fn frost_grey(app: &App, ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let (key, rgba) = app.frost_art.as_ref()?;
    let id = egui::Id::unique("frost-grey-tex");
    if let Some((k, t)) = ctx.data(|d| d.get_temp::<(String, egui::TextureHandle)>(id))
        && &k == key
    {
        return Some(t);
    }
    let px: Vec<u8> = rgba
        .chunks_exact(4)
        .flat_map(|c| {
            let g = (0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32).round() as u8;
            [g, g, g, 255]
        })
        .collect();
    let n = crate::art::SIZE as usize;
    let t = ctx.load_texture("frost-grey", egui::ColorImage::from_rgba_unmultiplied([n, n], &px), egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(id, (key.clone(), t.clone())));
    Some(t)
}

/// A blurred sleeve stretched over `rect`, cropped to where the rect sits on the window,
/// so the sheet keeps the shape of what is behind it.
fn frost_slice(ui: &Ui, rect: Rect, radius: f32, tex: &egui::TextureHandle) {
    frost_slice_tinted(ui, rect, radius, tex, Color32::WHITE);
}

/// The same sheet with `tint` over it, so a small piece of frost can fade in and away
/// rather than blinking.
fn frost_slice_tinted(ui: &Ui, rect: Rect, radius: f32, tex: &egui::TextureHandle, tint: Color32) {
    let screen = ui.ctx().content_rect();
    let slice = |v: f32| 0.12 + 0.76 * v;
    let uv = Rect::from_min_max(
        pos2(slice((rect.left() - screen.left()) / screen.width()), slice((rect.top() - screen.top()) / screen.height())),
        pos2(slice((rect.right() - screen.left()) / screen.width()), slice((rect.bottom() - screen.top()) / screen.height())),
    );
    let sized = egui::load::SizedTexture::new(tex.id(), rect.size());
    egui::Image::from_texture(sized).uv(uv).corner_radius(egui::CornerRadius::same(radius as u8)).tint(tint).paint_at(ui, rect);
}

/// The player pill's grey frost at any size: the song's light and shade moving behind the
/// shape, none of its colour, then a thin grey slab and a rim that reads as glass. Chrome
/// smaller than the pill — Best Part's button on the artwork — wears the same material so
/// the player is one substance in three places. Flat grey where the screen won't answer.
/// `alpha` fades the whole thing, frost included, for chrome that comes and goes.
pub fn grey_frost(app: &App, ui: &Ui, rect: Rect, radius: f32, alpha: f32) {
    let grey = if theme::light() { (246, 246, 248) } else { (56, 56, 60) };
    let a = (alpha.clamp(0.0, 1.0) * 255.0) as u8;
    let slab = if theme::light() { 26 } else { 18 };
    match frost_grey(app, ui.ctx()) {
        Some(soft) => {
            frost_slice_tinted(ui, rect, radius, &soft, Color32::from_white_alpha(a));
            ui.painter().rect_filled(rect, radius, Color32::from_rgba_unmultiplied(grey.0, grey.1, grey.2, (100 * a as u16 / 255) as u8));
            ui.painter().rect_filled(rect, radius, Color32::from_white_alpha((slab * a as u16 / 255) as u8));
        }
        None => {
            ui.painter().rect_filled(rect, radius, Color32::from_rgba_unmultiplied(grey.0, grey.1, grey.2, (184 * a as u16 / 255) as u8));
        }
    }
    ui.painter().rect_stroke(rect, radius, egui::Stroke::new(1.0, theme::with_alpha(theme::glass(0.16), alpha)), egui::StrokeKind::Inside);
}

/// An opened release's own sleeve blown up behind its page. The blur is the cover's, so
/// the page wears its colours; it dims and fades into the page ground before the first
/// row, so the words on top never have to fight the picture.
pub fn cover_wash(app: &App, ui: &mut Ui, rect: Rect) {
    let Some((key, rgba)) = app.page_art.clone() else { return };
    if rect.width() < 2.0 || rect.height() < 2.0 {
        return;
    }
    let id = egui::Id::unique("page-wash-tex");
    let tex = match ui.ctx().data(|d| d.get_temp::<(String, egui::TextureHandle)>(id)) {
        Some((k, t)) if k == key => t,
        _ => {
            let n = crate::art::SIZE as usize;
            let img = egui::ColorImage::from_rgba_unmultiplied([n, n], &rgba);
            let t = ui.ctx().load_texture("page-wash", img, egui::TextureOptions::LINEAR);
            ui.ctx().data_mut(|d| d.insert_temp(id, (key, t.clone())));
            t
        }
    };
    let sized = egui::load::SizedTexture::new(tex.id(), rect.size());
    egui::Image::from_texture(sized).paint_at(ui, rect);
    let a = (if theme::light() { 0.18 } else { 0.42 } * 255.0) as u8;
    ui.painter().rect_filled(rect, 0.0, Color32::from_black_alpha(a));
    // Down to the colour the page is really sitting on: ending on the theme's background
    // instead left a line where the wash ran out, however faint the two colours differ.
    vgradient(ui.painter(), rect, Color32::TRANSPARENT, page_ground(app));
}

/// The blurred sleeve in three finishes: raw for the bars, which lay their own tint over
/// it, and pre-dimmed for menus — a menu's frame is one shape, so its wash has to ride
/// inside the texture instead of above it.
fn frost_textures(app: &App, ctx: &egui::Context) -> Option<(egui::TextureHandle, egui::TextureHandle, egui::TextureHandle)> {
    let (key, rgba) = app.frost_art.as_ref()?;
    let id = egui::Id::unique("frost-tex");
    if let Some((k, plain, dark, light)) = ctx.data(|d| d.get_temp::<(String, egui::TextureHandle, egui::TextureHandle, egui::TextureHandle)>(id)) {
        if &k == key {
            return Some((plain, dark, light));
        }
    }
    let n = crate::art::SIZE as usize;
    let plain = ctx.load_texture("frost-art", egui::ColorImage::from_rgba_unmultiplied([n, n], rgba), egui::TextureOptions::LINEAR);
    let dim = |a: f32, white: bool| {
        let px: Vec<u8> = rgba
            .chunks_exact(4)
            .flat_map(|c| {
                let over = |v: u8| {
                    let v = v as f32 * (1.0 - a);
                    (if white { v + 255.0 * a } else { v }).round() as u8
                };
                [over(c[0]), over(c[1]), over(c[2]), 255]
            })
            .collect();
        ctx.load_texture("frost-art-dim", egui::ColorImage::from_rgba_unmultiplied([n, n], &px), egui::TextureOptions::LINEAR)
    };
    let dark = dim(0.72, false);
    let light = dim(0.72, true);
    let t = (plain, dark, light);
    ctx.data_mut(|d| d.insert_temp(id, (key.clone(), t.clone())));
    Some(t)
}

/// Menus wear the player bar's frost: every frame painted with the menu fill swaps that
/// flat fill for the blurred sleeve, pre-dimmed, keeping the frame's rounding and hairline.
pub fn frost_menus(app: &App, ctx: &egui::Context) {
    let Some((_, dark, light)) = frost_textures(app, ctx) else {
        return;
    };
    let tex = if theme::light() { light } else { dark };
    let fill = theme::menu_fill();
    let ids: Vec<egui::LayerId> = ctx.memory(|m| m.areas().visible_layer_ids().into_iter().collect());
    ctx.graphics_mut(|g| {
        for id in ids {
            let Some(list) = g.get_mut(id) else { continue };
            let hits: Vec<(egui::layers::ShapeIdx, egui::epaint::RectShape)> = list
                .all_entries()
                .enumerate()
                .filter_map(|(i, cs)| match &cs.shape {
                    egui::Shape::Rect(r) if r.fill == fill && r.fill_texture_id() == egui::TextureId::default() => {
                        Some((egui::layers::ShapeIdx(i), r.clone()))
                    }
                    _ => None,
                })
                .collect();
            for (idx, mut r) in hits {
                // The whole blurred sleeve fills the frame: a crop of it would read as a
                // flat wash instead of the soft colour the reference's menus carry.
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                r.fill = Color32::WHITE;
                list.set(idx, r.rect, egui::Shape::Rect(r.with_texture(tex.id(), uv)));
            }
        }
    });
}

/// The player: one frosted capsule floating over the bottom of the window, as in the
/// BitChord desktop app. The transport hugs its left, the more menu / lyrics / queue its
/// right, and the sleeve with its titles takes the middle.
pub fn player_bar(app: &mut App, ui: &mut Ui, now: f64, frosted: bool) {
    let full = ui.max_rect();
    if frosted {
        // The page scrolls on under this strip: fade it out rather than end it at the
        // player's edge. Clicks in the fade stay with the player, not the rows beneath.
        vgradient(ui.painter(), full, theme::panel(0.0), theme::panel(0.66));
        ui.interact(full, ui.id().with("player-strip"), Sense::hover());
    }
    // Apple's floating player, measured off its own page: a 52 pt pill of its own width,
    // centred and hung 20 pt above the bottom, with 16 pt of padding inside it and 11 pt
    // between its groups. It is not a bar across the window.
    const H: f32 = 52.0;
    const PAD: f32 = 16.0;
    const GAP: f32 = 11.0;
    const ART: f32 = 32.0;
    let ctx = ui.ctx().clone();
    // The groups' own widths, so the pill closes around them rather than the window.
    let tpitch = 38.0;
    let t_w = 4.0 * tpitch + 34.0;
    let upitch = 33.0;
    let u_w = if app.show_volume { 3.0 * upitch + 30.0 } else { 2.0 * upitch + 30.0 };
    // Titles take what the window leaves; Apple's own column is about 200 pt.
    let text_w = (full.width() - 40.0 - (PAD * 2.0 + ART + 8.0 + GAP + t_w + GAP + u_w)).clamp(60.0, 200.0);
    let w = (PAD * 2.0 + ART + 8.0 + text_w + GAP + t_w + GAP + u_w).min(full.width() - 24.0);
    let cap = Rect::from_center_size(pos2(full.center().x, full.bottom() - 20.0 - H / 2.0), vec2(w, H));
    // Apple's shadow: 0 10px 40px at 20% black.
    widgets::soft_shadow(ui, cap, H / 2.0, 40.0, 0.2);
    // Frosted glass, and grey glass at that: what moves under the pill is the song's
    // light and shade, never its colours, and the pane over it is flat grey. A sheen and
    // a hairline finish the frost; until a cover arrives it is the same grey over the
    // page ground.
    let grey = if theme::light() { (246, 246, 248) } else { (56, 56, 60) };
    // Transparent glass over the blur of what's behind: the frost slice is the picture,
    // and only a light wash sits on it so the controls read.
    match frost_grey(app, ui.ctx()) {
        Some(soft) => {
            frost_slice(ui, cap, H / 2.0, &soft);
            let wash: u8 = if theme::light() { 30 } else { 46 };
            ui.painter().rect_filled(cap, H / 2.0, Color32::from_black_alpha(wash));
            ui.painter().rect_filled(cap, H / 2.0, Color32::from_white_alpha(if theme::light() { 18 } else { 10 }));
        }
        None => {
            vgradient(ui.painter(), cap, Color32::from_rgba_unmultiplied(grey.0, grey.1, grey.2, 150), Color32::from_rgba_unmultiplied(grey.0.saturating_sub(14), grey.1.saturating_sub(14), grey.2.saturating_sub(12), 150));
        }
    }
    ui.painter().rect_stroke(cap, H / 2.0, egui::Stroke::new(1.0, theme::glass(0.10)), egui::StrokeKind::Inside);

    let status = app.audio.status.clone();
    // An Automix handoff keeps the outgoing song on this strip until the next one is
    // the louder of the two.
    let hold = app.mix_hold().map(|h| (h.total, h.mix, app.mix_hold_pos(h)));
    let track = match &hold {
        Some(_) => app.shown().cloned(),
        None => app.current().cloned(),
    };
    let enabled = track.is_some();
    let playing = app.playing();
    let accent = app.accent_color();
    let cy = cap.center().y;
    // Left to right, as the reference reads: the transport hugging the pill's left, the
    // sleeve with its two lines of text in what lies between, and the marks that open
    // things at its right. Each group takes its own width and the pill closes around the
    // groups, so no button floats in leftover space.
    // Apple keeps the volume at the very end of the group. Switched off, the marks that
    // remain slide into its place, so the group still closes against the capsule.
    let tail = cap.right() - PAD - 15.0;
    let (vo_x, q_x, ly_x, mo_x) = if app.show_volume {
        (tail, tail - upitch, tail - 2.0 * upitch, tail - 3.0 * upitch)
    } else {
        (tail, tail, tail - upitch, tail - 2.0 * upitch)
    };
    let t0 = cap.left() + PAD + 17.0;
    let (sh_x, pv_x, pl_x, nx_x, rp_x) = (t0, t0 + tpitch, t0 + 2.0 * tpitch, t0 + 3.0 * tpitch, t0 + 4.0 * tpitch);
    let cover = Rect::from_min_size(pos2(cap.left() + PAD + t_w + GAP, cy - ART / 2.0), Vec2::splat(ART));
    let text_l = cover.right() + 8.0;
    let text_r = text_l + text_w;
    let ink = |h: f32| {
        let c = theme::lerp_color(theme::text_dim(), theme::text(), h);
        if enabled { c } else { c.gamma_multiply(0.45) }
    };

    // Transport: shuffle | previous · play · next | repeat. The glyphs alone carry the
    // state; no button fills.
    let tbtn = |ui: &mut Ui, x: f32, key: &str, name: &str| -> egui::Response {
        let r = Rect::from_center_size(pos2(x, cy), Vec2::splat(34.0));
        let resp = ui.interact(r, ui.scope_id().with(("bar-t", key)), Sense::click());
        named(resp, name).on_hover_text(name).on_hover_cursor(egui::CursorIcon::PointingHand)
    };
    let r = tbtn(ui, sh_x, "shuffle", "Shuffle");
    let h = theme::anim_bool(&ctx, r.id.with("h"), r.hovered(), 0.12);
    glyphs::draw(ui, r.rect.center(), 22.5, glyphs::SHUFFLE, if app.queue.shuffled { accent } else { ink(h) });
    if r.clicked() {
        app.toggle_shuffle();
    }
    let r = tbtn(ui, pv_x, "prev", "Previous");
    let h = theme::anim_bool(&ctx, r.id.with("h"), r.hovered(), 0.12);
    glyphs::draw(ui, r.rect.center(), 25.0, glyphs::PREVIOUS, ink(h));
    if r.clicked() && enabled {
        app.prev();
    }
    let r = tbtn(ui, pl_x, "play", if playing { "Pause" } else { "Play" });
    let h = theme::anim_bool(&ctx, r.id.with("h"), r.hovered(), 0.12);
    let c = r.rect.center();
    let morph = theme::anim_bool(&ctx, "bar-play-morph", playing || app.buffering, 0.18);
    if app.buffering {
        let a = now as f32 * 6.0;
        let pts: Vec<Pos2> = (0..16).map(|i| c + vec2((a + i as f32 * 0.24).cos(), (a + i as f32 * 0.24).sin()) * 9.0).collect();
        ui.painter().add(egui::Shape::Path(egui::epaint::PathShape::line(pts, egui::epaint::PathStroke::new(2.2, ink(0.6)))));
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    } else {
        // The glyphs cross-fade rather than swap: the site keeps its pause mark up
        // through a buffer, and a hard cut between the two weights flickers.
        let fg = ink(0.55 + 0.45 * h);
        if morph < 1.0 {
            glyphs::draw(ui, c, 32.0, glyphs::PLAY, theme::with_alpha(fg, 1.0 - morph));
        }
        if morph > 0.0 {
            glyphs::draw(ui, c, 32.0, glyphs::PAUSE, theme::with_alpha(fg, morph));
        }
    }
    if r.clicked() && enabled {
        app.toggle();
    }
    let r = tbtn(ui, nx_x, "next", "Next");
    let h = theme::anim_bool(&ctx, r.id.with("h"), r.hovered(), 0.12);
    glyphs::draw(ui, r.rect.center(), 25.0, glyphs::NEXT, ink(h));
    if r.clicked() && enabled {
        app.next();
    }
    let (glyph, on) = match app.queue.repeat {
        Repeat::Off => (glyphs::REPEAT, false),
        Repeat::All => (glyphs::REPEAT, true),
        Repeat::One => (glyphs::REPEAT_ONE, true),
    };
    let r = tbtn(ui, rp_x, "repeat", "Repeat");
    let h = theme::anim_bool(&ctx, r.id.with("h"), r.hovered(), 0.12);
    glyphs::draw(ui, r.rect.center(), 22.5, glyph, if on { accent } else { ink(h) });
    if r.clicked() {
        app.queue.repeat = match app.queue.repeat {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        };
    }

    // Seek: a thin line under the sleeve and titles, thicker while it has the pointer.
    let held = hold.is_some();
    let (total, mix) = match &hold {
        Some((total, mix, _)) => (*total, *mix),
        None => {
            let total = status
                .duration()
                .map(|d| d.as_secs_f32())
                .or(track.as_ref().and_then(|t| t.duration).map(|d| d as f32))
                .unwrap_or(0.0);
            let next = app.queue.upcoming().is_some() || app.autoplay;
            let mix = match (total > 0.0, next, app.queue.repeat) {
                (true, true, repeat) if repeat != Repeat::One => {
                    status.mix_secs(status.duration().or_else(|| track.as_ref().and_then(|t| t.duration).map(|s| std::time::Duration::from_secs(s as u64)))).min(total * 0.5)
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
    let seek_hit = Rect::from_min_max(pos2(text_l, cap.bottom() - 12.0), pos2(text_r, cap.bottom()));
    let sresp = ui.interact(seek_hit, ui.scope_id().with("bar-seek"), if held { Sense::hover() } else { Sense::click_and_drag() });
    let sresp = if total > 0.0 && !held { sresp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { sresp };
    sresp.widget_info(|| egui::WidgetInfo::slider(total > 0.0 && !held, pos as f64, "Seek"));
    let focus = theme::anim_bool(&ctx, "bar-seek-h", total > 0.0 && !held && (sresp.hovered() || sresp.dragged()), 0.15);
    let (sx0, sx1) = (text_l, text_r);
    // Apple's capsule carries no progress line at all; this one stays a hairline glued to
    // the bottom rim, so it reads as the pill's own edge and not an underline under the
    // artist names, while seeking survives without breaking the reference's silence.
    let sy = cap.bottom() - 2.0;
    let thick = 1.5 + 2.0 * focus;
    let line_r = Rect::from_min_max(pos2(sx0, sy - thick / 2.0), pos2(sx1, sy + thick / 2.0));
    ui.painter().rect_filled(line_r, thick / 2.0, theme::glass(0.18));
    // Automix's handoff window, tinted on the part still to play.
    if mix > 0.5 {
        let edge = sx0 + (sx1 - sx0) * ((total - mix) / total);
        let band = Rect::from_min_max(pos2(edge, line_r.top()), line_r.max);
        ui.painter().rect_filled(band, thick / 2.0, theme::with_alpha(accent, 0.30));
        let tick = Rect::from_min_size(pos2(edge - 0.75, line_r.top()), vec2(1.5, thick));
        ui.painter().rect_filled(tick, 0.75, theme::with_alpha(accent, 0.9));
    }
    let frac = if total > 0.0 { (pos / total).clamp(0.0, 1.0) } else { 0.0 };
    if frac > 0.0 {
        ui.painter().rect_filled(Rect::from_min_size(line_r.min, vec2(line_r.width() * frac, thick)), thick / 2.0, accent);
    }
    let at = |p: Pos2| (((p.x - sx0) / (sx1 - sx0)).clamp(0.0, 1.0) * total).max(0.0);

    // Sleeve: the mark when nothing is playing; a click goes up into the player.
    match &track {
        Some(t) => {
            widgets::paint_cover_xfade(ui, egui::Id::unique("player-bar-art"), cover, &t.thumbs, 6.0, 1.0);
            let ar = ui.interact(cover, ui.scope_id().with("bar-art"), Sense::click());
            let ar = named(ar, "Open now playing").on_hover_cursor(egui::CursorIcon::PointingHand);
            let ah = theme::anim_bool(&ctx, ar.id.with("h"), ar.hovered(), 0.15);
            if ah > 0.0 {
                // egui has no blur of what's already drawn: frost the sleeve with a milky
                // pane and a scrim, so the art reads soft under the expand mark.
                ui.painter().rect_filled(cover, 6.0, theme::glass(0.16 * ah));
                ui.painter().rect_filled(cover, 6.0, theme::shade(0.3 * ah));
                glyphs::draw(ui, cover.center(), 14.0, glyphs::MAXIMIZE_2, theme::with_alpha(Color32::WHITE, ah));
            }
            if ar.clicked() {
                app.actions.push(Action::OpenNowPlaying(true));
            }
        }
        None => {
            ui.painter().rect_filled(cover, 6.0, theme::glass(0.08));
            paint_mark(ui, cover.shrink(9.0));
        }
    }

    // Titles, dimmed while the seek line has the pointer; the artist line gives way to
    // the times, which take the same row so nothing overlaps.
    let (title, _) = match &track {
        Some(t) => (t.title.clone(), ()),
        None => ("BitChord".to_owned(), ()),
    };
    let fade = 1.0 - 0.7 * focus;
    let afade = 1.0 - focus;
    let tr = text_at(ui, pos2(text_l, cy - 15.0), &title, theme::semibold(13.0), theme::with_alpha(theme::text(), fade), text_w);
    if let Some(t) = &track {
        let tre = ui.interact(tr, ui.scope_id().with("bar-title"), Sense::click());
        if named(tre, &t.title).on_hover_text(&t.title).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            app.actions.push(Action::OpenNowPlaying(true));
        }
    }
    let mut ax = text_l;
    if afade > 0.01 && let Some(t) = &track {
        for (i, a) in t.artists.iter().enumerate() {
            let s = if i + 1 < t.artists.len() { format!("{}, ", a.name) } else { a.name.clone() };
            let r = text_at(ui, pos2(ax, cy + 1.0), &s, theme::regular(12.0), theme::with_alpha(theme::text_dim(), afade), (text_l + text_w - ax).max(10.0));
            if let Some(id) = &a.id {
                let resp = ui.interact(r, ui.scope_id().with(("bar-artist", i)), Sense::click());
                let resp = named(resp, &a.name).on_hover_cursor(egui::CursorIcon::PointingHand);
                if resp.hovered() {
                    ui.painter().hline(r.x_range(), r.bottom(), egui::Stroke::new(1.0, theme::text_dim()));
                }
                if resp.clicked() {
                    app.actions.push(Action::Open(Target::Artist(id.clone())));
                }
            }
            ax = r.right();
            if ax > text_l + text_w {
                break;
            }
        }
    }
    if focus > 0.0 && total > 0.0 {
        let col = theme::with_alpha(theme::text(), focus);
        let g = widgets::galley(ui, &fmt_time(pos as u32), theme::semibold(12.0), theme::text(), 80.0);
        ui.painter().galley(pos2(text_l, cy + 1.0), g, col);
        let left = fmt_time((total - pos).max(0.0) as u32);
        let g = widgets::galley(ui, &format!("-{left}"), theme::semibold(12.0), theme::text(), 80.0);
        ui.painter().galley(pos2(text_r - g.size().x, cy + 1.0), g, col);
    }

    // Right: the more menu, lyrics, up next and volume. Hovering the volume slides its
    // slider over the other three, as the strip's own reference does.
    let vol = app.audio.volume();
    let slider = Rect::from_min_max(pos2(mo_x - 15.0, cy - 10.0), pos2(q_x + 15.0, cy + 10.0));
    let band = Rect::from_min_max(slider.min - vec2(3.0, 4.0), pos2(vo_x + 15.0, cy + 14.0));
    // Pointer position, not `rect_contains_pointer`: the window's own title-bar layers
    // sit above this bar, so egui would report the pointer as being over them.
    let hover = |r: Rect| ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| r.contains(p));
    let open_id = ui.scope_id().with("bar-vol");
    let was = ui.data(|d| d.get_temp::<bool>(open_id).unwrap_or(false));
    let menu_id = ui.scope_id().with(("bar-r", "more")).with("popup");
    let menu_open = egui::Popup::is_id_open(&ctx, menu_id);
    let open = app.show_volume
        && !menu_open
        && (hover(Rect::from_center_size(pos2(vo_x, cy), Vec2::splat(30.0)))
            || (was && (hover(band) || ui.input(|i| i.pointer.any_down()))));
    ui.data_mut(|d| d.insert_temp(open_id, open));
    let reveal = theme::anim_bool(&ctx, "bar-vol-reveal", open, 0.15);
    let rbtn = |ui: &mut Ui, x: f32, key: &str, name: &str| -> egui::Response {
        let r = Rect::from_center_size(pos2(x, cy), Vec2::splat(30.0));
        let resp = ui.interact(r, ui.scope_id().with(("bar-r", key)), Sense::click());
        named(resp, name).on_hover_text(name).on_hover_cursor(egui::CursorIcon::PointingHand)
    };
    if reveal < 0.5 {
        let queue_open = if app.np_open { app.np_tab == NpTab::UpNext } else { app.queue_open };
        let r = rbtn(ui, q_x, "queue", "Up next");
        let h = theme::anim_bool(&ctx, r.id.with("h"), r.hovered(), 0.15);
        glyphs::draw(ui, r.rect.center(), 20.0, glyphs::QUEUE, if queue_open { accent } else { ink(h) });
        if r.clicked() {
            if app.np_open {
                app.np_tab = if queue_open { NpTab::Art } else { NpTab::UpNext };
            } else {
                app.queue_open = !app.queue_open;
                if app.queue_open {
                    app.lyrics_open = false;
                }
            }
        }
        let lyrics_on = super::nowplaying::lyrics_shown(app, &ctx) || app.lyrics_open;
        let r = rbtn(ui, ly_x, "lyrics", "Lyrics");
        let h = theme::anim_bool(&ctx, r.id.with("h"), r.hovered(), 0.15);
        glyphs::draw(ui, r.rect.center(), 20.0, if lyrics_on { glyphs::LYRICS } else { glyphs::LYRICS_OFF }, if lyrics_on { accent } else { ink(h) });
        if r.clicked() {
            if lyrics_on {
                app.np_open = false;
                app.lyrics_open = false;
            } else {
                // The words open beside the page, not over it.
                app.queue_open = false;
                app.lyrics_open = true;
                super::nowplaying::force_lyrics(app, &ctx);
            }
        }
        // What's heard, like Now playing's menu: a song version standing in for the video.
        let heard = if held { track.clone() } else { app.playing_track().cloned().or_else(|| track.clone()) };
        let liked = heard.as_ref().is_some_and(|t| app.liked.contains(&t.id));
        let r = rbtn(ui, mo_x, "more", "More");
        let h = theme::anim_bool(&ctx, r.id.with("h"), r.hovered(), 0.15);
        glyphs::draw(ui, r.rect.center(), 20.0, glyphs::ELLIPSIS, ink(h));
        let popup = egui::Popup::menu(&r);
        let popup = if super::dev_var("YTM_MENU").is_some() { popup.open(now >= 12.0) } else { popup };
        popup
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_min_width(220.0);
                ui.set_max_width(260.0);
                if widgets::menu_action_star(ui, liked, if liked { "Remove from favourites" } else { "Add to favourites" })
                    && let Some(t) = heard.clone()
                {
                    app.actions.push(Action::Like(t, !liked));
                }
            });
    } else {
        volume_slider(app, ui, slider, "bar");
    }
    if app.show_volume {
        let r = rbtn(ui, vo_x, "vol", if vol > 0.0 { "Mute" } else { "Unmute" });
        let h = theme::anim_bool(&ctx, r.id.with("h"), r.hovered(), 0.15);
        // Three arcs, not two: the desktop app lights them a third at a time.
        let step = glyphs::volume_step(vol);
        glyphs::draw(ui, r.rect.center(), 20.0, glyphs::VOLUME[step], ink(h));
        if r.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                app.audio.set_volume((vol + scroll / 600.0).clamp(0.0, 1.0));
            }
        }
        if r.clicked() {
            toggle_mute(app, ui.ctx());
        }
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
}

/// The BitChord mark, white, fitted inside `r` (the sleeve slot when nothing plays).
fn paint_mark(ui: &Ui, r: Rect) {
    mark(ui, r, Color32::from_white_alpha(170));
}

/// The BitChord mark, fitted inside `r`, tinted `tint`.
fn mark(ui: &Ui, r: Rect, tint: Color32) {
    const MARK: &[u8] = include_bytes!("../../assets/icon/mark.png");
    const W: f32 = 256.0;
    const H: f32 = 171.0;
    let key = egui::Id::unique("bitchord-mark");
    let tex = match ui.ctx().data(|d| d.get_temp::<egui::TextureHandle>(key)) {
        Some(t) => t,
        None => {
            let Ok(img) = image::load_from_memory(MARK) else { return };
            let img = img.into_rgba8();
            let color = egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw());
            // The mark is drawn far smaller than the bitmap: mipmaps keep its edges.
            let options = egui::TextureOptions {
                magnification: egui::TextureFilter::Linear,
                minification: egui::TextureFilter::Linear,
                wrap_mode: egui::TextureWrapMode::ClampToEdge,
                mipmap_mode: Some(egui::TextureFilter::Linear),
            };
            let t = ui.ctx().load_texture("bitchord-mark", color, options);
            ui.ctx().data_mut(|d| d.insert_temp(key, t.clone()));
            t
        }
    };
    let k = (r.width() / W).min(r.height() / H);
    let dst = Rect::from_center_size(r.center(), vec2((W * k).round(), (H * k).round()));
    ui.painter().image(tex.id(), dst, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), tint);
}

/// Where Mute keeps the volume to go back to (the player bar's and the mini player's).
pub fn unmute_id() -> egui::Id {
    egui::Id::new("unmute")
}

/// Mute and back, from the player bar, the mini strip or the keyboard. What the volume
/// was before the mute is remembered, so unmuting returns to it rather than to a guess.
pub fn toggle_mute(app: &mut App, ctx: &egui::Context) {
    let vol = app.audio.volume();
    if vol > 0.0 {
        ctx.data_mut(|d| d.insert_temp(unmute_id(), vol));
        app.audio.set_volume(0.0);
    } else {
        app.audio.set_volume(ctx.data(|d| d.get_temp::<f32>(unmute_id())).unwrap_or(0.6));
    }
}

/// The volume track: click or drag to set, scroll over it to nudge.
fn volume_slider(app: &mut App, ui: &mut Ui, slider: Rect, salt: &str) {
    let vol = app.audio.volume();
    let vresp = ui.interact(slider, ui.scope_id().with(salt), Sense::click_and_drag()).on_hover_cursor(egui::CursorIcon::PointingHand);
    vresp.widget_info(|| egui::WidgetInfo::slider(true, vol as f64, "Volume"));
    let vh = theme::anim_bool(ui.ctx(), ui.scope_id().with((salt, "h")), vresp.hovered() || vresp.dragged(), 0.15);
    let track_r = Rect::from_center_size(slider.center(), vec2(slider.width(), 4.0));
    ui.painter().rect_filled(track_r, 2.0, theme::glass(0.15));
    let fill = Rect::from_min_size(track_r.min, vec2(track_r.width() * vol, 4.0));
    ui.painter().rect_filled(fill, 2.0, theme::lerp_color(theme::text_dim(), theme::text(), vh));
    if let Some(p) = vresp.interact_pointer_pos()
        && (vresp.dragged() || vresp.clicked())
    {
        app.audio.set_volume(((p.x - track_r.left()) / track_r.width()).clamp(0.0, 1.0));
    }
    if vresp.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            app.audio.set_volume((vol + scroll / 600.0).clamp(0.0, 1.0));
        }
    }
}

// ------------------------------------------------------------------ queue drawer

pub fn queue_drawer(app: &mut App, ui: &mut Ui) {
    queue_list(app, ui, "drawer", true);
}

/// The lyrics drawer: the words of the song being played, beside the page rather than
/// over it. Its own heading carries the song and a way out, the way the reference's
/// lyric pane does.
pub fn lyrics_drawer(app: &mut App, ui: &mut Ui, now: f64) {
    let title = app.shown().map(|t| t.title.clone()).unwrap_or_default();
    let artist = app.shown().map(|t| t.artist_line()).unwrap_or_default();
    // The window's own buttons sit at the top right: the way out of this pane stands
    // clear of them, or it can never be clicked.
    let gap = super::titlebar::right_gap(ui.ctx(), ui.max_rect(), 0.0);
    let (hr, _) = ui.allocate_exact_size(vec2((ui.available_width() - gap).max(260.0), 44.0), Sense::hover());
    let cy = hr.center().y;
    let c = close_centre(ui, hr, cy);
    let r = Rect::from_center_size(c, Vec2::splat(30.0));
    let resp = named(ui.interact(r, ui.scope_id().with("l-close"), Sense::click()), "Close lyrics").on_hover_cursor(egui::CursorIcon::PointingHand);
    icon_at(ui, c, icon::X, 16.0, theme::text_dim());
    if resp.clicked() {
        app.lyrics_open = false;
    }
    let w = (c.x - 26.0 - hr.left()).max(80.0);
    // The two lines are stacked by their own heights. Fixed offsets put the artist's
    // ascenders into the title's descenders, which is the clipping on the row.
    let th = widgets::galley(ui, &title, theme::bold(19.0), theme::text(), w).size().y;
    let ah = widgets::galley(ui, &artist, theme::regular(13.0), theme::text_dim(), w).size().y;
    let top = cy - (th + 2.0 + ah) / 2.0;
    text_at(ui, pos2(hr.left(), top), &title, theme::bold(19.0), theme::text(), w);
    if !artist.is_empty() {
        text_at(ui, pos2(hr.left(), top + th + 2.0), &artist, theme::regular(13.0), theme::text_dim(), w);
    }
    ui.add_space(6.0);
    let rest = Rect::from_min_max(pos2(ui.max_rect().left(), hr.bottom() + 6.0), ui.max_rect().max);
    super::nowplaying::lyrics_panel(app, ui, rest, now, false, super::karaoke::Surface::Sidebar);
}

/// The reference's queue row: a 52 pt sleeve, the name over the artist, and a three-line
/// handle at the edge carrying the song's options.
const QUEUE_ROW_H: f32 = 68.0;

/// Where a drawer's own way out stands. The window's buttons hold the last 138 pt of the
/// screen's top edge, and a 380 pt drawer is narrow enough that its header row's own floor
/// can put that row's right edge under them — the mark then draws over the minimise button,
/// which takes the click. So it stands in front of the buttons rather than in the row.
fn close_centre(ui: &Ui, hr: Rect, cy: f32) -> Pos2 {
    let edge = super::titlebar::buttons_rect(ui.ctx()).map_or(hr.right(), |b| (b.left() - 24.0).min(hr.right()));
    pos2(edge - 14.0, cy)
}

/// The heading and its endless-play switch: "Up Next" at the left, with the switch named
/// beneath it and standing at the right. `close` adds the drawer's own mark; in the
/// player the corner disc is the way out.
fn queue_header(app: &mut App, ui: &mut Ui, _close: bool) {
    let gap = if _close { super::titlebar::right_gap(ui.ctx(), ui.max_rect(), 0.0) } else { 0.0 };
    let (hr, _) = ui.allocate_exact_size(vec2((ui.available_width() - gap).max(260.0), 44.0), Sense::hover());
    let cy = hr.center().y;
    // The reading row stops in front of the window's own buttons: the player's queue
    // column runs into them, and nothing here is drawn under a control that takes clicks.
    let safe = super::titlebar::buttons_rect(ui.ctx()).map_or(hr.right(), |b| (b.left() - 12.0).min(hr.right()));
    // Just the heading: the drawer closes from the player bar's queue mark, so no ✕ of
    // its own.
    let title_w = (safe - 16.0 - hr.left()).max(72.0);
    let th = widgets::galley(ui, "Up Next", theme::bold(21.0), theme::text(), title_w).size().y;
    text_at(ui, pos2(hr.left(), cy - th / 2.0), "Up Next", theme::bold(21.0), theme::text(), title_w);
    let _ = app;
}

/// The queue: the playing row with what comes after it, and the songs already heard
/// above it for anyone who scrolls back. Shared by the drawer and now playing.
pub fn queue_list(app: &mut App, ui: &mut Ui, salt: &str, close: bool) {
    let rows = super::pages::Rows::new(app, ui.input(|i| i.time));
    queue_header(app, ui, close);
    ui.add_space(6.0);
    let cur = app.queue.index.unwrap_or(0);
    let has_current = app.queue.index.is_some();
    // Once to a panel and a song: it opens with the playing row at the top of the run,
    // the history above it for whoever wants to scroll back. The offset is the row's
    // place in a list whose rows are all the same height.
    let seed_key = format!("{salt} {}", app.queue.current().map_or("", |t| t.id.as_str()));
    let seed = app.queue_seed.as_deref() != Some(seed_key.as_str());
    let h = QUEUE_ROW_H + ui.spacing().item_spacing.y;
    let want = cur as f32 * h;
    let mut area = egui::ScrollArea::vertical().id_salt(("queue", salt)).auto_shrink(false);
    if seed {
        area = area.vertical_scroll_offset(want);
    }
    let out = area.show(ui, |ui| {
        if app.queue.tracks.is_empty() {
            ui.label(egui::RichText::new("Nothing queued yet").color(theme::text_dim()));
            return;
        }
        let (queue, liked, acts) = (&app.queue.tracks, &app.liked, &mut app.actions);
        widgets::visible_rows_with(ui, queue.len(), QUEUE_ROW_H, |ui, i| {
            let t = &queue[i];
            let (current, past) = (has_current && i == cur, has_current && i < cur);
            let mut o = rows.opts(liked, t, None, false);
            // A song already heard is history: it plays again if clicked, but taking it
            // out or moving it would rewrite what happened.
            o.queue_remove = (!current && !past).then_some(i);
            // Every row still to come can play next, the last one included: the menu
            // moves it rather than queueing a second copy of a song already here.
            o.queue_move = (!current && !past).then(|| (i, i > cur + 1, i + 1 < queue.len()));
            let resp = queue_row(ui, t, &o, past, acts);
            // The playing row pauses/resumes instead of restarting itself.
            if resp.clicked() {
                acts.push(if current { Action::TogglePlay } else { Action::QueueJump(i) });
            }
        });
    });
    // The offset is held to the list's own height, which a fresh area only measures as it
    // lays out: until it has, the ask goes back for the next frame, so the row being
    // heard is where the drawer opens and the songs already played sit above it.
    if seed {
        let room = (out.content_size.y - out.inner_rect.height()).max(0.0);
        if out.state.clamped_offset().y >= want.min(room) - 1.0 {
            app.queue_seed = Some(seed_key);
        } else {
            ui.ctx().request_repaint();
        }
    }
}

fn queue_row(ui: &mut Ui, t: &crate::model::Track, o: &widgets::RowOpts, past: bool, acts: &mut Vec<Action>) -> egui::Response {
    let (rect, base) = ui.allocate_exact_size(vec2(ui.available_width(), QUEUE_ROW_H), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return base;
    }
    let h = theme::anim_bool(ui.ctx(), base.id.with("h"), base.contains_pointer(), 0.15);
    // The remove mark stands in front of the sleeve and stays lit: a queue is there to be
    // edited, so the control is part of the row rather than something the pointer uncovers.
    let remove = o.queue_remove;
    let slot = Rect::from_center_size(pos2(rect.left() + 18.0, rect.center().y), Vec2::splat(24.0));
    let art = Rect::from_min_size(pos2(rect.left() + 34.0, rect.center().y - 26.0), Vec2::splat(52.0));
    let tr = theme::thumb_radius();
    paint_cover(ui, art, &t.thumbs, tr, 1.0);
    if past {
        // Already heard: the sleeve sits back from the rows still to come.
        ui.painter().rect_filled(art, tr, theme::shade(0.35));
    }
    if o.current {
        ui.painter().rect_filled(art, tr, theme::shade(0.45));
        widgets::equalizer(ui, art.shrink(13.0), o.time, o.playing, Color32::WHITE);
    } else if h > 0.0 {
        ui.painter().rect_filled(art, tr, theme::shade(0.5 * h));
        widgets::play_triangle(ui, art.center(), 9.0, theme::with_alpha(Color32::WHITE, h));
    }
    // The row's own hairline, from the sleeve's edge to the window's: the reference
    // separates its rows with nothing but this.
    ui.painter().hline(art.right() + 14.0..=rect.right() - 6.0, rect.bottom() - 0.5, Stroke::new(1.0, Color32::from_white_alpha(18)));
    let handle = Rect::from_center_size(pos2(rect.right() - 20.0, rect.center().y), Vec2::splat(30.0));
    let text_r = handle.left() - 14.0;
    let x = art.right() + 16.0;
    let w = (text_r - x).max(40.0);
    let ink = if o.current { o.accent } else if past { theme::text_faint() } else { theme::text() };
    let line = t.artist_line();
    // The name over the artist, the pair centred in the row and spaced by their own
    // heights: a fixed offset put the artist's ascenders into the descenders of a name
    // like "Stairway To Heaven", which is the clipping on these rows.
    let th = widgets::galley(ui, &t.title, theme::semibold(18.0), ink, w).size().y;
    let ah = widgets::galley(ui, &line, theme::regular(13.5), theme::text_dim(), w).size().y;
    let top = rect.center().y - (th + 2.0 + ah) / 2.0;
    text_at(ui, pos2(x, top), &t.title, theme::semibold(18.0), ink, w);
    widgets::linked_text(ui, pos2(x, top + th + 2.0), &line, &t.artists, theme::regular(13.5), theme::text_dim(), w, acts);
    let play_l = if remove.is_some() { slot.right() } else { rect.left() };
    let resp = named(ui.interact(Rect::from_min_max(pos2(play_l, rect.top()), pos2(text_r, rect.bottom())), base.id.with("play"), Sense::click()), &t.title)
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let hr = named(ui.interact(handle, base.id.with("menu"), Sense::click()), "Song options").on_hover_cursor(egui::CursorIcon::PointingHand);
    glyphs::draw(ui, handle.center(), 18.0, glyphs::MENU, theme::with_alpha(Color32::WHITE, 0.28 + 0.45 * h));
    if let Some(i) = remove {
        let xr = named(ui.interact(slot, base.id.with("rm"), Sense::click()), "Remove from queue").on_hover_cursor(egui::CursorIcon::PointingHand);
        icon_at(ui, slot.center(), icon::X, 12.0, theme::with_alpha(Color32::WHITE, 0.55 + 0.45 * h));
        if xr.clicked() {
            acts.push(Action::QueueRemove(i));
        }
    }
    let (liked, queue_move) = (o.liked, o.queue_move);
    widgets::menu_popup(egui::Popup::menu(&hr), widgets::MENU_W, |ui| widgets::song_menu(ui, t, liked, None, queue_move, acts));
    // Right-click anywhere on the row opens the same menu.
    widgets::menu_popup(egui::Popup::context_menu(&resp), widgets::MENU_W, |ui| widgets::song_menu(ui, t, liked, None, queue_move, acts));
    resp
}

// ------------------------------------------------------------------ toast

pub fn toast(app: &mut App, ctx: &egui::Context, now: f64) {
    let Some((msg, shown)) = &app.toast else { return };
    if *shown < 0.0 {
        return;
    }
    let age = (now - shown) as f32;
    let t = theme::ease_out(age / 0.25) * (1.0 - ((age - 4.1) / 0.4).clamp(0.0, 1.0));
    let screen = ctx.content_rect();
    egui::Area::new(egui::Id::unique("toast"))
        .order(egui::Order::Tooltip)
        .pivot(Align2::CENTER_BOTTOM)
        // Above the player bar; in the mini player, over the strip's middle.
        .fixed_pos(Pos2::new(screen.center().x, if screen.height() < 200.0 { screen.center().y + 22.0 } else { screen.bottom() - 104.0 } + 14.0 * (1.0 - t)))
        .show(ctx, |ui| {
            ui.set_opacity(t);
            egui::Frame::new()
                .fill(theme::menu_fill())
                .corner_radius(12)
                .inner_margin(egui::Margin::symmetric(18, 12))
                .shadow(egui::epaint::Shadow { offset: [0, 8], blur: 24, spread: 0, color: theme::shade(0.45) })
                .show(ui, |ui| {
                    ui.set_max_width(520.0);
                    ui.label(egui::RichText::new(msg.as_str()).font(theme::regular(14.0)).color(theme::text()));
                });
        });
    // Frames only while it fades in or out; in between it just waits.
    if (0.25..4.1).contains(&age) {
        ctx.request_repaint_after(std::time::Duration::from_secs_f32(4.1 - age));
    } else {
        ctx.request_repaint();
    }
}
