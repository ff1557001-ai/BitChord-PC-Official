//! Page bodies. Every page fades and slides in when navigated to.

use std::collections::HashSet;
use std::sync::atomic::Ordering;

use egui::{Align2, Color32, Rect, RichText, Sense, Ui, Vec2, pos2, vec2};

use super::glyphs;
use super::widgets::{self, RowOpts, Shape, card_shape, h_scroll, named, paint_cover, pill, shelf_header, track_row};
use super::{Action, App, AppleArt, LibSort, LibTab, Page, Selection};
use crate::backend::Backend;
use crate::model::{Card, Chip, Shelf, ShelfKind, Target, Thumb, Track};
use crate::theme::{self, icon};

const CARD: f32 = 180.0;
const SONGS_SEARCH: &str = "library:songs";
/// The album and playlist page's sleeve, measured from Apple Music's iPad page.
const COVER: f32 = 300.0;

/// How many tiles Apple fits across a feed row at this content width. The site
/// measures from its own viewport, which includes the navigation column this page
/// already sits past.
fn feed_columns(width: f32, shape: Shape) -> usize {
    shape.columns(width + 260.0)
}

/// One tile of an Apple feed row: a share of the content on wide windows, the site's
/// own narrow width below its breakpoint.
fn feed_tile(ui: &Ui, shape: Shape) -> f32 {
    let width = ui.available_width();
    let min = shape.min_width();
    if width < 1024.0 {
        return min;
    }
    let cols = feed_columns(width, shape);
    ((width - 16.0 * (cols - 1) as f32) / cols as f32).max(min)
}

/// The shape a shelf's tiles take: videos are 16:9, a shelf of singles is portrait,
/// everything else is the square sleeve. Mixed rows stay square, as on the site.
fn shelf_shape(cards: &[Card]) -> Shape {
    if cards.is_empty() {
        return Shape::Square;
    }
    if cards.iter().all(|c| matches!(&c.target, Target::Song(t) if t.music_video)) {
        return Shape::Wide;
    }
    let single = |c: &Card| match &c.target {
        Target::Album(_) | Target::Playlist(_) => {
            let kind = c.subtitle.split(" • ").next().unwrap_or("");
            matches!(kind, "Single" | "EP") || c.title.rsplit(" - ").next().is_some_and(|t| t.eq_ignore_ascii_case("single"))
        }
        _ => false,
    };
    if cards.iter().all(single) {
        return Shape::Tall;
    }
    Shape::Square
}

pub fn page(app: &mut App, ui: &mut Ui, now: f64) {
    let t = theme::ease_out(((now - app.page_changed) / 0.32) as f32);
    if t < 1.0 {
        // The fade-in finishes even when nothing else is drawing frames.
        ui.ctx().request_repaint();
    }
    ui.set_opacity(0.15 + 0.85 * t);
    let page = app.page.clone();
    // Each page (each album, each search) keeps its own scroll position.
    let mut area = egui::ScrollArea::vertical().id_salt(("page", &page)).auto_shrink(false);
    // Dev hook: YTM_SCROLL=<px> pins the page scroll, for screenshots of lower sections.
    if let Some(y) = super::dev_var("YTM_SCROLL").and_then(|v| v.parse::<f32>().ok()) {
        area = area.vertical_scroll_offset(y);
    }
    let out = area.show(ui, |ui| {
        // The panel reaches the window's side so egui's own bar sits flush with it; the
        // content keeps its measured right edge instead of following the panel.
        ui.set_max_width((ui.available_width() - 48.0).max(0.0));
        ui.add_space(18.0 * (1.0 - t));
        match &page {
            Page::Home => home(app, ui, now),
            Page::New => new(app, ui, now),
            Page::Search(q) => search(app, ui, q, now),
            Page::Album(id) | Page::Playlist(id) => collection(app, ui, id, now),
            Page::AppleCollection(url) => apple_collection(app, ui, url, now),
            Page::Artist(id) => artist(app, ui, id, now),
            Page::Library(tab) => library(app, ui, *tab, now),
            Page::SeeAll => see_all(app, ui, now),
            Page::Downloads => downloads(app, ui, now),
            Page::Settings => settings(app, ui),
            Page::Transfer => super::transfer::page(app, ui, now),
        }
        // Room to scroll the last rows clear of the player's frosted strip.
        ui.add_space(96.0);
    });
    selection_bar(app, ui.ctx(), &page, out.inner_rect);
    // Infinite scroll on home: fetch the next shelves when nearing the bottom.
    if page == Page::Home {
        let remaining = out.content_size.y - (out.state.clamped_offset().y + out.inner_rect.height());
        if let Some(h) = &mut app.home {
            // A failed fetch reports nothing (it's a quiet request): allow a retry later.
            if h.loading_more.as_ref().is_some_and(|(t, _)| now - t > 20.0) {
                h.loading_more = None;
            }
            if remaining < 700.0 && !app.home_loading && h.loading_more.is_none()
                && let Some(token) = h.continuation.clone()
            {
                h.loading_more = Some((now, token.clone()));
                app.backend.request(crate::backend::Req::HomeMore(token));
            }
        }
    }
}

/// What every song row needs from the app, gathered once per frame.
pub(super) struct Rows {
    current: Option<String>,
    playing: bool,
    accent: Color32,
    now: f64,
}

impl Rows {
    pub(super) fn new(app: &App, now: f64) -> Self {
        Self {
            current: app.current_id().map(str::to_owned),
            playing: app.audio.status.playing.load(Ordering::Relaxed),
            accent: app.accent_color(),
            now,
        }
    }

    pub(super) fn opts(&self, liked: &HashSet<String>, t: &Track, number: Option<usize>, show_album: bool) -> RowOpts {
        RowOpts {
            number,
            show_album,
            current: self.current.as_deref() == Some(t.id.as_str()),
            playing: self.playing,
            accent: self.accent,
            time: self.now,
            liked: liked.contains(&t.id),
            added_by: None,
            pick: false,
            selected: None,
            remove: None,
            queue_remove: None,
            queue_move: None,
            artist_col: false,
            sub_album: false,
        }
    }
}

/// A page's data hasn't arrived: shimmering placeholders while it loads, a retry if the
/// request failed.
fn placeholder(app: &mut App, ui: &mut Ui, now: f64, rows: usize, card: f32) {
    if app.load_failed {
        ui.add_space(40.0);
        ui.label(RichText::new("Couldn't load this page").color(theme::text_dim()));
        ui.add_space(12.0);
        if pill(ui, "Try again", None, false).clicked() {
            app.refresh();
        }
    } else {
        widgets::skeleton(ui, now, rows, card);
    }
}

/// What a collection header's round buttons draw: one of the app's Apple silhouettes,
/// or the three dots.
enum CircleGlyph {
    Mark(&'static [u8]),
    More,
}

/// A round glass action on a collection header, at a chosen diameter.
fn circle_action_sized(ui: &mut Ui, glyph: CircleGlyph, name: &str, active: bool, size: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    let resp = named(resp, name).on_hover_text(name).on_hover_cursor(egui::CursorIcon::PointingHand);
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&resp));
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered() || open, 0.15);
    let press = if resp.is_pointer_button_down_on() { size / 2.0 - 0.7 } else { size / 2.0 };
    ui.painter().circle_filled(rect.center(), press, theme::glass(0.10 + 0.09 * h));
    let color = theme::lerp_color(theme::text_dim(), theme::text(), h.max(active as u8 as f32));
    // Sized by ink, not by box: Apple's marks carry different amounts of padding, so
    // one box number would make them weigh differently inside the same ring.
    match glyph {
        CircleGlyph::Mark(g) => glyphs::draw(ui, rect.center(), glyphs::box_for(g, size * 0.40), g, color),
        CircleGlyph::More => glyphs::draw(ui, rect.center(), glyphs::box_for(glyphs::ELLIPSIS, size * 0.37), glyphs::ELLIPSIS, color),
    }
    resp
}

/// A glass disc painted at a fixed point, for the artist hero's centered row of three.
fn disc_at(ui: &mut Ui, center: egui::Pos2, size: f32, glyph: Option<&'static [u8]>, name: &str) -> egui::Response {
    let rect = Rect::from_center_size(center, Vec2::splat(size));
    let id = ui.make_persistent_id(("disc", name));
    let resp = named(ui.interact(rect, id, Sense::click()), name).on_hover_text(name).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), id.with("h"), resp.hovered(), 0.15);
    let press = if resp.is_pointer_button_down_on() { size / 2.0 - 0.7 } else { size / 2.0 };
    ui.painter().circle_filled(rect.center(), press, theme::glass(0.26 + 0.12 * h));
    if let Some(g) = glyph {
        glyphs::draw(ui, rect.center(), glyphs::box_for(g, size * 0.36), g, theme::lerp_color(theme::text_dim(), theme::text(), h));
    }
    resp
}

/// The hero's play control on Apple's white disc: Pause while the artist is loaded.
fn play_disc(ui: &mut Ui, center: egui::Pos2, size: f32, paused: bool) -> egui::Response {
    let rect = Rect::from_center_size(center, Vec2::splat(size));
    let id = ui.make_persistent_id("artist-play-disc");
    let resp = named(ui.interact(rect, id, Sense::click()), if paused { "Pause" } else { "Play" }).on_hover_cursor(egui::CursorIcon::PointingHand);
    let press = if resp.is_pointer_button_down_on() { size / 2.0 - 1.4 } else { size / 2.0 };
    ui.painter().circle_filled(rect.center(), press, Color32::WHITE);
    let glyph = if paused { glyphs::PAUSE } else { glyphs::PLAY };
    glyphs::draw(ui, rect.center(), glyphs::box_for(glyph, size * 0.30), glyph, Color32::BLACK);
    resp
}

/// The artist page's "Latest Release": the sleeve, its date and name, then its options.
fn latest_release(ui: &mut Ui, card: &Card, acts: &mut Vec<Action>) {
    ui.label(RichText::new("Latest Release").font(theme::bold(19.0)).color(theme::text()));
    ui.add_space(14.0);
    ui.horizontal_top(|ui| {
        let (img, _) = ui.allocate_exact_size(Vec2::splat(157.0), Sense::hover());
        widgets::soft_shadow(ui, img, 8.0, 26.0, 0.45);
        paint_cover(ui, img, &card.thumbs, theme::card_radius(), 1.0);
        ui.add_space(16.0);
        let open = named(ui.interact(img, ui.id().with("rel"), Sense::click()), &card.title).on_hover_cursor(egui::CursorIcon::PointingHand);
        ui.vertical(|ui| {
            ui.set_max_width(ui.available_width());
            if !card.subtitle.is_empty() {
                ui.label(RichText::new(&card.subtitle).font(theme::regular(13.5)).color(theme::text_dim()));
            }
            ui.label(RichText::new(&card.title).font(theme::semibold(17.0)).color(theme::text()));
            if !card.subtitle.is_empty() {
                ui.add_space(2.0);
            }
            // Apple's small "+" disc: the release's own menu.
            let (rect, more) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
            let more = named(more, "Release options").on_hover_text("Release options").on_hover_cursor(egui::CursorIcon::PointingHand);
            let open_menu = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&more));
            let h = theme::anim_bool(ui.ctx(), more.id.with("h"), more.hovered() || open_menu, 0.15);
            ui.painter().circle_filled(rect.center(), 12.0, theme::glass(0.10 + 0.09 * h));
            let c = rect.center();
            let col = theme::lerp_color(theme::text_dim(), theme::text(), h);
            ui.painter().line_segment([pos2(c.x - 4.5, c.y), pos2(c.x + 4.5, c.y)], egui::Stroke::new(1.6, col));
            ui.painter().line_segment([pos2(c.x, c.y - 4.5), pos2(c.x, c.y + 4.5)], egui::Stroke::new(1.6, col));
            widgets::menu_popup(egui::Popup::menu(&more), widgets::MENU_W, |ui| widgets::card_menu_contents(ui, card, false, acts));
            if open.clicked() {
                acts.push(Action::Open(card.target.clone()));
            }
        });
    });
}

/// A page's main Play button. Shows Pause while this page's album / playlist / artist
/// is playing, and toggles instead of restarting when it's already loaded.
fn play_pill_h(ui: &mut Ui, acts: &mut Vec<Action>, target: &Target, tracks: &[Track], height: f32) {
    let active = widgets::active_state(ui, target);
    let (label, glyph) = if active == Some(true) { ("Pause", glyphs::PAUSE) } else { ("Play", glyphs::PLAY) };
    if widgets::pill_h_apple(ui, label, Some(glyph), true, height).clicked() {
        if active.is_some() {
            acts.push(Action::TogglePlay);
        } else if !tracks.is_empty() {
            acts.push(Action::Play(tracks.to_vec(), 0));
        }
    }
}

/// A section's name clicked: hand the whole shelf to the page that lays it out in full.
fn see_all_run(acts: &mut Vec<Action>, title: &str, strap: Option<&str>, kind: ShelfKind) {
    acts.push(Action::SeeAll(Shelf { title: title.to_owned(), strapline: strap.map(str::to_owned), strap_thumb: None, kind }));
}

fn card_row(app: &mut App, ui: &mut Ui, id: &str, title: &str, cards: &[Card]) {
    if cards.is_empty() {
        return;
    }
    // Apple's hero run has no heading: its tiles sit straight under the page title.
    let (l, r, all) = if title.is_empty() { (false, false, false) } else { shelf_header(ui, title, None, None, true, true) };
    if all {
        see_all_run(&mut app.actions, title, None, ShelfKind::Cards(cards.to_vec()));
    }
    let nudge = r as i32 - l as i32;
    let shape = shelf_shape(cards);
    let tile = feed_tile(ui, shape);
    h_scroll(ui, id, nudge, cards.len(), tile, 16.0, |ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        for c in cards {
            // Moving artwork is the player's alone: the shelves keep their stills.
            card_shape(ui, c, tile, shape, false, &mut app.actions, None);
        }
    });
}

/// The motion sleeve of a hovered shelf tile.
fn card_motion(app: &mut App, ui: &Ui, c: &Card, tile: f32) -> Option<widgets::MotionSprite> {
    let animated = app.animated_artwork;
    tile_motion(&mut app.apple, &app.backend, animated, ui, c, tile, Shape::Square)
}

/// The motion sleeve a tile wears, asked of Apple by the names the card carries. Only a
/// square tile gets one: Apple cuts its clip from square art, and a square clip laid
/// into a poster slot shows the still cover in bands above and below.
fn tile_motion(apple: &mut AppleArt, backend: &Backend, animated: bool, ui: &Ui, c: &Card, tile: f32, shape: Shape) -> Option<widgets::MotionSprite> {
    if shape != Shape::Square {
        return None;
    }
    let (album, artist, title) = motion_names(c)?;
    apple.cover(backend, animated, &album, &artist, title, None);
    let px = (tile * ui.ctx().pixels_per_point()).round().max(128.0) as u32;
    apple.sprite(ui.ctx(), animated, &album, &artist, title, None, px)
}

/// Whose sleeve a card would wear, if it wears one at all: the release an album card
/// names, and a song's own album only when the tile already shows that song's cover —
/// a row that swapped picture as it scrolled would read as a glitch.
fn motion_names(c: &Card) -> Option<(String, String, Option<&str>)> {
    match &c.target {
        Target::Song(t) => {
            if !c.thumbs.first().is_some_and(|a| t.thumbs.first().is_some_and(|b| a.url == b.url)) {
                return None;
            }
            let artist = t.artists.first()?.name.clone();
            let album = t.album.as_ref().map(|a| a.name.clone()).unwrap_or_else(|| t.title.clone());
            Some((album, artist, Some(t.title.as_str())))
        }
        Target::Album(_) => {
            // The subtitle carries a kind prefix ("EP • …"); the artist link is the plain name.
            let artist = c
                .links
                .first()
                .map(|l| l.name.clone())
                .or_else(|| c.subtitle.rsplit('•').next().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned))?;
            Some((c.title.clone(), artist, None))
        }
        _ => None,
    }
}

/// A shelf of still tiles, for pages whose results are borrowed while they draw.
fn card_row_plain(ui: &mut Ui, id: &str, title: &str, cards: &[Card], acts: &mut Vec<Action>) {
    if cards.is_empty() {
        return;
    }
    let shape = shelf_shape(cards);
    card_row_shape(ui, id, title, cards, acts, shape);
}

/// The same shelf at a shape the page picks itself (Apple's videos are 16:9 whatever
/// their titles say).
fn card_row_shape(ui: &mut Ui, id: &str, title: &str, cards: &[Card], acts: &mut Vec<Action>, shape: Shape) {
    if cards.is_empty() {
        return;
    }
    // Apple's hero run has no heading: its tiles sit straight under the page title.
    let (l, r, all) = if title.is_empty() { (false, false, false) } else { shelf_header(ui, title, None, None, true, true) };
    if all {
        see_all_run(acts, title, None, ShelfKind::Cards(cards.to_vec()));
    }
    let nudge = r as i32 - l as i32;
    let tile = feed_tile(ui, shape);
    h_scroll(ui, id, nudge, cards.len(), tile, 16.0, |ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        for c in cards {
            card_shape(ui, c, tile, shape, false, acts, None);
        }
    });
}

/// Songs laid out in columns of `rows`, scrolling sideways (Quick picks). Clicking a
/// song starts its radio. Returns whether the arrows reached the end.
#[allow(clippy::too_many_arguments)]
fn song_grid(ui: &mut Ui, rows_ctx: &Rows, liked: &HashSet<String>, acts: &mut Vec<Action>, id: &str, title: &str, strap: Option<&str>, tracks: &[Track], rows: usize) -> bool {
    let (l, r, all) = shelf_header(ui, title, strap, None, true, true);
    if all {
        see_all_run(acts, title, strap, ShelfKind::Songs { tracks: tracks.to_vec(), rows });
    }
    let nudge = r as i32 - l as i32;
    let col_w = feed_tile(ui, Shape::Wide);
    let mut picked = None;
    let (_, reached_end) = h_scroll(ui, id, nudge, tracks.len().div_ceil(rows.max(1)), col_w, 16.0, |ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        for (c, col) in tracks.chunks(rows.max(1)).enumerate() {
            ui.allocate_ui(vec2(col_w, rows as f32 * 60.0), |ui| {
                ui.set_width(col_w);
                ui.vertical(|ui| {
                    for (i, t) in col.iter().enumerate() {
                        let o = rows_ctx.opts(liked, t, None, false);
                        if track_row(ui, t, &o, acts).clicked() {
                            picked = Some(c * rows + i);
                        }
                    }
                });
            });
        }
    });
    if let Some(i) = picked {
        acts.push(Action::Radio(tracks[i].clone()));
    }
    reached_end
}

/// Where a song list lives: the page its selection belongs to, and whether it's your own
/// playlist (so songs can be taken out of it).
struct ListCtx<'a> {
    sel: &'a mut Selection,
    page: &'a Page,
    collection: Option<&'a crate::backend::Collection>,
    /// For filtered rows, their indices in the original collection, not the play queue.
    indices: Option<&'a [usize]>,
}

/// A long song list; only the rows on screen are built. Clicking one plays `play` from it,
/// or picks it while selecting: Ctrl-click picks one song, Shift-click a run of them.
///
/// `tracks` is the run drawn here and `base` its first index in the play queue. Filtered
/// rows additionally carry their original collection indices for selection and removal.
#[allow(clippy::too_many_arguments)]
fn list(ui: &mut Ui, rows: &Rows, liked: &HashSet<String>, tracks: &[Track], base: usize, play: &[Track], show_album: bool, artist_col: bool, number: impl Fn(usize, &Track) -> Option<usize>, cx: ListCtx, acts: &mut Vec<Action>) {
    let ListCtx { sel, page, collection, indices } = cx;
    let selecting = sel.on(page);
    let mods = ui.input(|i| i.modifiers);
    widgets::visible_rows(ui, tracks.len(), |ui, i| {
        let t = &tracks[i];
        let row = indices.map_or(base + i, |indices| indices[i]);
        let set_id = collection.and_then(|c| c.set_ids.get(row)).and_then(Option::as_ref);
        let o = RowOpts {
            selected: selecting.then(|| sel.rows.contains(&row)),
            remove: collection.map(|c| (c.id.clone(), set_id.cloned())),
            artist_col,
            ..rows.opts(liked, t, number(i, t), show_album)
        };
        let resp = ui.push_id((&t.id, set_id), |ui| track_row(ui, t, &o, acts)).inner;
        // Apple rules the song table with a hairline under every row; the rows themselves
        // keep the page's ground. A playlist's hairline starts at the song, not the sleeve.
        let left = if artist_col { resp.rect.left() + 64.0 } else { resp.rect.left() };
        ui.painter().hline(egui::Rangef::new(left, resp.rect.right()), resp.rect.bottom(), egui::Stroke::new(1.0, theme::glass(0.07)));
        if resp.clicked() {
            if selecting || mods.command || mods.shift {
                if let Some(indices) = indices {
                    sel.pick_visible(page, row, mods.shift, indices);
                } else {
                    sel.pick(page, row, mods.shift);
                }
            } else {
                acts.push(Action::Play(play.to_vec(), base + i));
            }
        }
    });
}

/// A list page's "Select" button: turns select mode on and off.
fn select_pill(ui: &mut Ui, sel: &mut Selection, page: &Page) {
    let on = sel.on(page);
    if pill(ui, if on { "Done" } else { "Select" }, Some(icon::LIST_CHECKS), false).clicked() {
        if on { sel.clear() } else { sel.start(page) }
    }
}

/// The songs a page lists, and whether they're your own editable playlist's.
fn selectable<'a>(app: &'a App, page: &Page) -> Option<(&'a [Track], bool)> {
    match page {
        Page::Album(id) | Page::Playlist(id) => {
            app.opened(id).map(|o| &o.c).map(|c| (c.tracks.as_slice(), removable_collection(c, app.logged_in)))
        }
        Page::Library(LibTab::Songs) => app.opened(super::LIKED_MUSIC).map(|o| (o.c.tracks.as_slice(), false)),
        Page::Library(LibTab::History) => app.history.as_deref().map(|h| (h, false)),
        Page::Library(LibTab::Uploads) => app.uploads.as_deref().map(|u| (u, false)),
        _ => None,
    }
}

fn removable_collection(c: &crate::backend::Collection, logged_in: bool) -> bool {
    logged_in && c.owned && !c.is_album && !matches!(c.id.as_str(), "LM" | "SE")
}

/// Indices into the original list, preserving separate occurrences of the same song.
/// `None` means no filter, so callers can use the original slice without copying it.
fn collection_filter(tracks: &[Track], query: &str) -> Option<Vec<usize>> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return None;
    }
    Some(tracks.iter().enumerate().filter_map(|(i, t)| {
        (t.title.to_lowercase().contains(&needle)
            || t.artist_line().to_lowercase().contains(&needle)
            || t.album.as_ref().is_some_and(|a| a.name.to_lowercase().contains(&needle))).then_some(i)
    }).collect())
}

fn all_visible_selected(sel: &Selection, n: usize, visible: Option<&[usize]>) -> bool {
    match visible {
        Some(indices) => !indices.is_empty() && indices.iter().all(|i| sel.rows.contains(i)),
        None => n > 0 && (0..n).all(|i| sel.rows.contains(&i)),
    }
}

fn toggle_visible(sel: &mut Selection, n: usize, visible: Option<&[usize]>, all: bool) {
    sel.active = true;
    sel.anchor = None;
    match visible {
        Some(indices) if all => { for i in indices { sel.rows.remove(i); } }
        Some(indices) => sel.rows.extend(indices.iter().copied()),
        None if all => sel.rows.clear(),
        None => sel.rows = (0..n).collect(),
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    use crate::model::Link;

    fn track(id: &str, title: &str) -> Track {
        Track { id: id.into(), title: title.into(), artists: vec![], album: None, duration: None, thumbs: vec![], track_nr: None, plays: None, music_video: false, episode: false }
    }

    fn collection(id: &str) -> crate::backend::Collection {
        crate::backend::Collection {
            id: id.into(), is_album: false, save_id: None, title: String::new(), subtitle: String::new(),
            artists: vec![], description: None, thumbs: vec![], tracks: vec![], set_ids: vec![], count: None, owned: true,
        }
    }

    #[test]
    fn filtered_selection_keeps_original_duplicate_rows_and_visible_ranges() {
        let duplicate = track("same-id", "Match");
        let tracks = vec![track("a", "Other"), duplicate.clone(), track("b", "Hidden"), duplicate, track("c", "Match")];
        let set_ids = ["set-a", "set-first", "set-b", "set-second", "set-c"];
        let visible = collection_filter(&tracks, " mAtCh ").unwrap();
        assert_eq!(visible, [1, 3, 4]);
        let page = Page::Playlist("PL1".into());
        let mut sel = Selection::default();
        sel.start(&page);
        sel.pick_visible(&page, visible[1], false, &visible);
        assert_eq!(sel.rows.iter().copied().collect::<Vec<_>>(), [3]);
        assert_eq!(tracks[*sel.rows.first().unwrap()].id, "same-id");
        assert_eq!(set_ids[*sel.rows.first().unwrap()], "set-second");
        // A shift range includes matching duplicates, not the hidden row between them.
        sel.pick_visible(&page, visible[0], true, &visible);
        assert_eq!(sel.rows.iter().copied().collect::<Vec<_>>(), [1, 3]);
        assert_eq!(sel.rows.iter().map(|&i| set_ids[i]).collect::<Vec<_>>(), ["set-first", "set-second"]);

        // Changing the query keeps the selected source rows, including hidden ones.
        let narrowed = collection_filter(&tracks, "Hidden").unwrap();
        sel.pick_visible(&page, narrowed[0], true, &narrowed);
        assert_eq!(sel.rows.iter().copied().collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(sel.anchor, Some(2)); // the old anchor was hidden
        sel.pick_visible(&page, 1, true, &[1]);
        assert!(sel.rows.contains(&1));
        assert_eq!(sel.anchor, Some(1));
        assert!(collection_filter(&tracks, "  ").is_none());
        assert!(collection_filter(&tracks, "nothing").unwrap().is_empty());
    }

    #[test]
    fn filtered_list_clicks_select_source_rows_but_play_visible_rows() {
        let duplicate = track("same-id", "Match");
        let tracks = vec![track("a", "Other"), duplicate.clone(), track("b", "Hidden"), duplicate, track("c", "Match")];
        let indices = collection_filter(&tracks, "match").unwrap();
        let shown: Vec<_> = indices.iter().map(|&i| tracks[i].clone()).collect();
        let page = Page::Playlist("PL1".into());
        let rows = Rows { current: None, playing: false, accent: Color32::WHITE, now: 0.0 };
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        let proportional = fonts.families[&egui::FontFamily::Proportional].clone();
        for name in ["semibold", "bold", "icons"] {
            fonts.families.insert(egui::FontFamily::Name(name.into()), proportional.clone());
        }
        ctx.set_fonts(fonts);
        let mut sel = Selection::default();
        sel.start(&page);
        let mut actions = Vec::new();
        let mut frame = |events| {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(800.0, 400.0))),
                events, ..Default::default()
            };
            ctx.run_ui(input, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                list(ui, &rows, &HashSet::new(), &shown, 0, &shown, true, false, |_, _| None,
                    ListCtx { sel: &mut sel, page: &page, collection: None, indices: Some(&indices) }, &mut actions);
            }).drop_without_applying_deltas();
        };
        let pos = pos2(120.0, 84.0); // second visible row, the second duplicate
        let button = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        frame(vec![]);
        frame(vec![egui::Event::PointerMoved(pos)]);
        frame(vec![button(true)]);
        frame(vec![button(false)]);
        drop(frame);
        assert_eq!(sel.rows.iter().copied().collect::<Vec<_>>(), [3]);
        assert!(actions.is_empty());

        // A normal click still starts the filtered queue at its displayed index.
        sel.clear();
        let mut frame = |events| {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(800.0, 400.0))),
                events, ..Default::default()
            };
            ctx.run_ui(input, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                list(ui, &rows, &HashSet::new(), &shown, 0, &shown, true, false, |_, _| None,
                    ListCtx { sel: &mut sel, page: &page, collection: None, indices: Some(&indices) }, &mut actions);
            }).drop_without_applying_deltas();
        };
        frame(vec![button(true)]);
        frame(vec![button(false)]);
        drop(frame);
        assert!(matches!(actions.as_slice(), [Action::Play(queue, 1)] if queue == &shown));
    }

    #[test]
    fn select_all_only_toggles_visible_rows_and_keeps_hidden_picks() {
        let page = Page::Playlist("PL1".into());
        let mut sel = Selection::default();
        sel.start(&page);
        sel.pick(&page, 0, false);
        let visible = [1, 3];
        assert!(!all_visible_selected(&sel, 5, Some(&visible)));
        toggle_visible(&mut sel, 5, Some(&visible), false);
        assert_eq!(sel.rows.iter().copied().collect::<Vec<_>>(), [0, 1, 3]);
        assert!(all_visible_selected(&sel, 5, Some(&visible)));
        toggle_visible(&mut sel, 5, Some(&visible), true);
        assert_eq!(sel.rows.iter().copied().collect::<Vec<_>>(), [0]);
        assert!(!all_visible_selected(&sel, 5, Some(&[])));
        toggle_visible(&mut sel, 5, None, false);
        assert!(all_visible_selected(&sel, 5, None));
        toggle_visible(&mut sel, 5, None, true);
        assert!(sel.rows.is_empty());
    }

    #[test]
    fn collection_search_matches_artist_and_album() {
        let mut t = track("a", "Title");
        t.artists.push(Link { name: "Artist Name".into(), id: None });
        t.album = Some(Link { name: "Album Name".into(), id: None });
        assert_eq!(collection_filter(std::slice::from_ref(&t), "artist"), Some(vec![0]));
        assert_eq!(collection_filter(&[t], "ALBUM"), Some(vec![0]));
    }

    #[test]
    fn removal_keeps_entry_identity_across_reload_and_duplicates() {
        let mut c = collection("PL1");
        c.tracks = vec![track("same", "First"), track("other", "Other"), track("same", "Second")];
        c.set_ids = vec![Some("first".into()), Some("other".into()), Some("second".into())];
        let items = super::super::removal_items(&c, [2]);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].0, "same");
        assert_eq!(items[0].1, "second");
        c.tracks.swap(0, 2);
        c.set_ids.swap(0, 2);
        assert_eq!(super::super::removal_rows(&c, &items), [0]);
        c.remove_rows(&[0]);
        assert!(super::super::removal_rows(&c, &items).is_empty());
        assert_eq!(c.set_ids[1].as_deref(), Some("first"));
        c.set_ids[0] = None;
        assert!(super::super::removal_items(&c, [0]).is_empty());
    }

    #[test]
    fn automatic_collections_cannot_be_removed_from() {
        assert!(removable_collection(&collection("PL1"), true));
        assert!(!removable_collection(&collection("PL1"), false));
        assert!(!removable_collection(&collection("LM"), true));
        assert!(!removable_collection(&collection("SE"), true));
    }

    #[test]
    fn collection_changes_clear_only_the_matching_selection() {
        let page = Page::Playlist("PL1".into());
        let mut sel = Selection::default();
        sel.start(&page);
        sel.pick(&page, 2, false);
        sel.collection_changed("PL2");
        assert_eq!(sel.rows.iter().copied().collect::<Vec<_>>(), [2]);
        sel.collection_changed("PL1");
        assert!(!sel.on(&page));
        let favourites = Page::Library(LibTab::Songs);
        sel.start(&favourites);
        sel.collection_changed(super::super::LIKED_MUSIC);
        assert!(!sel.on(&favourites));
    }
}

/// While selecting: a bar floating over the bottom of the page with what to do with the picks.
fn selection_bar(app: &mut App, ctx: &egui::Context, page: &Page, area: Rect) {
    if !app.selection.on(page) {
        return;
    }
    let Some((tracks, own)) = selectable(app, page) else { return };
    let n = tracks.len();
    let collection_id = match page {
        Page::Album(id) | Page::Playlist(id) => Some(id.as_str()),
        Page::Library(LibTab::Songs) => Some(super::LIKED_MUSIC),
        _ => None,
    };
    let search_id = if matches!(page, Page::Library(LibTab::Songs)) { Some(SONGS_SEARCH) } else { collection_id };
    let visible = search_id.and_then(|id| app.coll_search.as_ref().filter(|(fid, _)| fid == id))
        .and_then(|(_, query)| collection_filter(tracks, query));
    let picked: Vec<Track> = app.selection.rows.iter().filter_map(|&i| tracks.get(i).cloned()).collect();
    let removal = collection_id.and_then(|id| app.opened(id)).filter(|_| own).map(|o| {
        (o.c.id.clone(), super::removal_items(&o.c, app.selection.rows.iter().copied()))
    });
    let all = all_visible_selected(&app.selection, n, visible.as_deref());
    let all_liked = !picked.is_empty() && picked.iter().all(|t| app.liked.contains(&t.id));
    // Labelled buttons when there's room, icons with tooltips when the window is narrow.
    let wide = area.width() > 820.0;
    let button = |ui: &mut Ui, label: &str, glyph: char, primary: bool| {
        if wide { pill(ui, label, Some(glyph), primary) } else { widgets::icon_button(ui, glyph, 18.0, label, false) }
    };
    let mut acts = Vec::new();
    let mut toggle_all = false;
    let mut done = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    egui::Area::new(egui::Id::unique("selection-bar"))
        .order(egui::Order::Foreground)
        .pivot(Align2::CENTER_BOTTOM)
        .fixed_pos(pos2(area.center().x, area.bottom() - 18.0))
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(theme::menu_fill())
                .corner_radius(14)
                .inner_margin(egui::Margin { left: 20, right: 10, top: 8, bottom: 8 })
                .shadow(egui::epaint::Shadow { offset: [0, 8], blur: 24, spread: 0, color: theme::shade(0.45) })
                .show(ui, |ui| {
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let count = if picked.is_empty() { "Select songs".to_owned() } else { format!("{} selected", picked.len()) };
                        ui.label(RichText::new(count).font(theme::semibold(14.0)).color(theme::text()));
                        ui.add_space(10.0);
                        if !picked.is_empty() {
                            if button(ui, "Add to playlist", icon::LIST_PLUS, true).clicked() {
                                acts.push(Action::AddToPlaylist(picked.clone()));
                            }
                            if let Some((playlist, items)) = &removal
                                && button(ui, "Remove from playlist", icon::LIST_MINUS, false).clicked()
                            {
                                acts.push(Action::RemoveFromPlaylist { playlist: playlist.clone(), items: items.clone() });
                            }
                            if button(ui, if all_liked { "Remove like" } else { "Like" }, icon::HEART, false).clicked() {
                                acts.push(Action::LikeMany(picked.clone(), !all_liked));
                            }
                            if button(ui, "Add to queue", icon::LIST_MUSIC, false).clicked() {
                                acts.extend(picked.iter().cloned().map(Action::Enqueue));
                            }
                        }
                        let select_all_label = match (visible.is_some(), all) {
                            (true, true) => "Deselect visible",
                            (true, false) => "Select visible",
                            (false, true) => "Select none",
                            (false, false) => "Select all",
                        };
                        toggle_all = button(ui, select_all_label, icon::LIST_CHECKS, false).clicked();
                        done |= widgets::icon_button(ui, icon::X, 18.0, "Done selecting", false).clicked();
                    });
                });
        });
    if toggle_all {
        toggle_visible(&mut app.selection, n, visible.as_deref(), all);
    }
    if done || !acts.is_empty() {
        app.selection.clear();
    }
    app.actions.extend(acts);
}

// ------------------------------------------------------------------ home

fn home(app: &mut App, ui: &mut Ui, now: f64) {
    page_title(ui, "Home");
    // A refresh keeps showing the current feed; the skeleton is only for an empty one.
    if app.home.as_ref().is_none_or(|h| app.home_loading && h.shelves.is_empty()) {
        placeholder(app, ui, now, 3, CARD);
        return;
    }
    // Apple's Home has no banner: its lead shelf is a run of portrait tiles, and every
    // shelf after it takes the shape its own cards ask for.
    let rows = Rows::new(app, now);
    // The picks row is out for now; the feed's own shelves follow directly.
    let Some(h) = &app.home else { return };
    if h.shelves.is_empty() {
        ui.add_space(40.0);
        ui.label(RichText::new("Nothing here yet").color(theme::text_dim()));
    }
    let mut more_picks = None;
    // The feed's first shelf keeps the poster shape: nothing leads the page now.
    let mut lead = true;
    // Two sleeves move on the whole page rather than two per row: that cap is what stops
    // a scrolling Home from restarting decoders as fast as rows come into view.
    let mut moving = 0usize;
    // A tile asks Apple for its sleeve while the feed is borrowed to draw it. The two
    // are fields beside each other, so they're borrowed one at a time, not the whole app.
    let (acts, liked, apple, backend) = (&mut app.actions, &app.liked, &mut app.apple, &app.backend);
    let animated = app.animated_artwork;
    let (picks_loading, picks_expanded) = (h.picks_loading, h.picks_expanded);
    for (i, s) in h.shelves.iter().enumerate() {
        // Apple's Home never shows a shelf of the listener's own library; that lives on
        // the Library page.
        if s.title == "From your library" {
            continue;
        }
        let id = format!("home-{i}-{}", s.title);
        match &s.kind {
            ShelfKind::Cards(cards) => {
                let (strap, thumb) = if s.title == "Listen again" { (None, None) } else { (s.strapline.as_deref(), s.strap_thumb.as_deref()) };
                let (l, r, all) = shelf_header(ui, &s.title, strap, thumb, true, true);
                if all {
                    acts.push(Action::SeeAll(s.clone()));
                }
                let nudge = r as i32 - l as i32;
                let shape = if lead { Shape::Poster } else { shelf_shape(cards) };
                lead = false;
                let tile = feed_tile(ui, shape);
                // Apple moves the sleeves of a whole run, not only the one under the
                // pointer, so a row animates on its own.
                h_scroll(ui, &id, nudge, cards.len(), tile, 16.0, |ui| {
                    ui.spacing_mut().item_spacing.x = 16.0;
                    for c in cards {
                        // Moving artwork is the player's alone: the feed keeps its stills.
                        let sprite: Option<widgets::MotionSprite> = None;
                        let is_liked = matches!(&c.target, Target::Song(t) if liked.contains(&t.id));
                        card_shape(ui, c, tile, shape, is_liked, acts, sprite.as_ref());
                    }
                });
            }
            ShelfKind::Songs { tracks, rows: n } => {
                let reached_end = song_grid(ui, &rows, liked, acts, &id, &s.title, s.strapline.as_deref(), tracks, *n);
                if s.title == "Quick picks" && reached_end && !picks_loading && !picks_expanded {
                    more_picks = tracks.last().map(|t| t.id.clone());
                }
            }
        }
    }
    let loading_more = app.home.as_ref().is_some_and(|h| h.loading_more.is_some());
    if let Some(seed) = more_picks
        && let Some(h) = &mut app.home
    {
        h.picks_loading = true;
        app.backend.request(crate::backend::Req::MoreQuickPicks(seed));
    }
    if loading_more {
        widgets::skeleton(ui, now, 1, CARD);
    }
}

// ------------------------------------------------------------------ home picks

/// One tile of Apple's Home lead row.
enum Pick {
    /// Something in the library, drawn as the site's tall poster tile.
    Item(Card),
    /// A mood, which re-filters the feed instead of opening a page of its own.
    Mood { name: String, params: Option<String>, on: bool },
    /// A mood played as a station: its playlists' songs, queued.
    Station(crate::ytm::Mood),
}

/// The first song a shelf offers, whether it runs as rows or as tiles.
fn first_track(s: &Shelf) -> Option<Track> {
    match &s.kind {
        ShelfKind::Songs { tracks, .. } => tracks.first().cloned(),
        ShelfKind::Cards(cards) => cards.iter().find_map(|c| match &c.target {
            Target::Song(t) => Some(t.clone()),
            _ => None,
        }),
    }
}

/// The first song a shelf offers that the lead row has not already stood on.
fn unseeded_track(s: &Shelf, seeded: &HashSet<String>) -> Option<Track> {
    match &s.kind {
        ShelfKind::Songs { tracks, .. } => tracks.iter().find(|t| !seeded.contains(&t.id)).cloned(),
        ShelfKind::Cards(cards) => cards.iter().find_map(|c| match &c.target {
            Target::Song(t) if !seeded.contains(&t.id) => Some(t.clone()),
            _ => None,
        }),
    }
}

/// The first tile of a shelf that answers to `pred`.
fn first_card(s: &Shelf, pred: impl Fn(&Card) -> bool) -> Option<Card> {
    match &s.kind {
        ShelfKind::Cards(cards) => cards.iter().find(|c| pred(c)).cloned(),
        ShelfKind::Songs { .. } => None,
    }
}

/// Apple's Home lead row, cut from the feed YouTube Music gives the account: its own
/// station, an artist to feature, something to discover, the newest release, what's
/// trending and a mood.
///
/// YouTube Music names these rows differently per account and language, so each tile
/// takes the first shelf carrying the idea rather than a shelf with a fixed title, and
/// simply leaves the row short when the feed has nothing of that kind today.
fn top_picks(app: &App) -> Vec<Pick> {
    let Some(h) = &app.home else { return Vec::new() };
    let shelf = |needles: &[&str]| h.shelves.iter().find(|s| needles.iter().any(|n| s.title.to_lowercase().contains(n)));
    let mut out: Vec<Pick> = Vec::new();
    let mut seeded = HashSet::new();

    // Made for You: the account's station, cut from what it keeps replaying. Clicking it
    // plays that song and asks for its radio, which is what a station is here.
    if let Some(t) = shelf(&["listen again", "made for you", "for you", "quick picks"]).and_then(first_track) {
        seeded.insert(t.id.clone());
        let who = app
            .account_name
            .clone()
            .or_else(|| t.artists.first().map(|a| a.name.clone()))
            .unwrap_or_else(|| "You".to_owned());
        let thumbs = match &app.account_avatar {
            Some(a) => vec![Thumb { url: a.clone(), width: 512, height: 512 }],
            None => t.thumbs.clone(),
        };
        out.push(Pick::Item(Card { title: format!("{who}'s Station"), subtitle: "Made for You".into(), thumbs, target: Target::Song(t), round: false, links: Vec::new() }));
    }
    // Featuring: an artist off the feed, opening as their page, which carries their
    // similar artists the way Apple's Featuring card does. A feed that hands over no
    // artist card today lends one from a song the listener keeps playing.
    let mut featured = h.shelves.iter().find_map(|s| first_card(s, |c| c.round));
    if featured.is_none()
        && let Some(t) = h.shelves.iter().find_map(|s| unseeded_track(s, &seeded))
        && let Some((id, name)) = t.artists.iter().find_map(|a| a.id.clone().map(|id| (id, a.name.clone())))
    {
        seeded.insert(t.id.clone());
        featured = Some(Card { title: name, subtitle: String::new(), thumbs: t.thumbs.clone(), target: Target::Artist(id), round: false, links: Vec::new() });
    }
    if let Some(c) = featured {
        // A poster tile, not a circle: Apple's Featuring card wears the artist's photo
        // square like the rest of the row.
        out.push(Pick::Item(Card { subtitle: format!("Featuring {}", c.title), title: format!("{} & Similar Artists", c.title), round: false, ..c }));
    }
    // Discovery: what the feed is pushing new, played as its own radio rather than opened.
    // A shelf is only a source when it actually carries a song, so each is asked in turn,
    // and a feed that pushes nothing new today cuts the station from the listener's own
    // rotation, on a song other than the ones the tiles above already stand on.
    let discovery = [&["discover", "recommended", "trending"][..], &["new release", "new album"], &["quick picks", "listen again", "for you"]]
        .into_iter()
        .find_map(|needles| shelf(needles).and_then(|s| unseeded_track(s, &seeded)));
    if let Some(t) = discovery {
        out.push(Pick::Item(Card { title: "Discovery Station".into(), subtitle: "Discovery".into(), thumbs: t.thumbs.clone(), target: Target::Song(t), round: false, links: Vec::new() }));
    }
    // The feed's own releases, wearing Apple's kind line in place of their credits.
    for (needles, kind) in [(&["new release", "new album"][..], "New Release"), (&["trending", "charts", "top"][..], "Trending")] {
        if let Some(card) = shelf(needles).and_then(|s| first_card(s, |c| !c.round)) {
            out.push(Pick::Item(Card { subtitle: kind.into(), ..card }));
        }
    }
    // Mood for You: a mood played as a station of its own — its playlists' songs
    // shuffled — chosen by the hour the way v1.10 chooses it. Until Explore's mood
    // list arrives, the feed's own chip stands in, which re-filters the page.
    if let Some(mood) = &app.mood {
        out.push(Pick::Station(mood.clone()));
    } else if let Some(chip) = h.chips.iter().find(|c| !c.selected && feels(c)).or_else(|| h.chips.iter().find(|c| feels(c))).or(h.chips.first()) {
        out.push(Pick::Mood { name: chip.title.clone(), params: chip.params.clone(), on: chip.selected });
    }
    out.truncate(6);
    out
}

/// The lead row: Apple's tall tiles under one heading, scrolling sideways.
fn picks_row(app: &mut App, ui: &mut Ui, picks: &[Pick]) {
    // The lead row mixes moods in with the releases, so there's no full list behind it.
    let (l, r, _) = shelf_header(ui, "Top Picks For You", None, None, true, false);
    let nudge = r as i32 - l as i32;
    let tile = feed_tile(ui, Shape::Poster);
    h_scroll(ui, "home-picks", nudge, picks.len(), tile, 16.0, |ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        for p in picks {
            match p {
                // A poster tile stays still: Apple cuts its sleeves from square art.
                Pick::Item(c) => card_shape(ui, c, tile, Shape::Poster, false, &mut app.actions, None),
                Pick::Mood { name, params, on } => mood_tile(ui, name, tile, Shape::Poster.aspect(), *on, Action::HomeChip(params.clone()), &mut app.actions),
                Pick::Station(mood) => mood_tile(ui, &mood.name, tile, Shape::Poster.aspect(), false, Action::MoodStation(mood.clone()), &mut app.actions),
            }
        }
    });
}

/// A mood tile. YouTube Music sends its moods as plain chips with no artwork, so the
/// tile is a flat coloured ground with its name sunk into it, as Apple's are. The ground
/// comes from a fixed set so the same mood keeps its colour across visits.
fn mood_tile(ui: &mut Ui, name: &str, width: f32, aspect: f32, on: bool, act: Action, acts: &mut Vec<Action>) {
    const MOOD_GROUNDS: [Color32; 8] = [
        Color32::from_rgb(0x2E, 0x7D, 0xD1),
        Color32::from_rgb(0xD1, 0x5B, 0x2E),
        Color32::from_rgb(0x2E, 0xA8, 0x8F),
        Color32::from_rgb(0x7B, 0x4E, 0xB0),
        Color32::from_rgb(0xC2, 0x3E, 0x6B),
        Color32::from_rgb(0x4E, 0x9A, 0x3E),
        Color32::from_rgb(0xB0, 0x84, 0x2E),
        Color32::from_rgb(0x3E, 0x5A, 0x9E),
    ];
    let (rect, resp) = ui.allocate_exact_size(vec2(width, width * aspect), Sense::click());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let resp = named(resp, name).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.contains_pointer(), 0.22);
    let ground = MOOD_GROUNDS[(name.bytes().fold(0u32, |a, b| a.rotate_left(5) ^ b as u32) % MOOD_GROUNDS.len() as u32) as usize];
    let radius = theme::card_radius();
    ui.painter().rect_filled(rect, radius, ground);
    super::shell::vgradient(ui.painter(), rect, Color32::TRANSPARENT, theme::shade(0.42 + 0.2 * h));
    ui.painter().text(rect.left_bottom() + vec2(15.0, -19.0), Align2::LEFT_BOTTOM, name, theme::semibold(17.0), Color32::WHITE);
    if on {
        ui.painter().rect_stroke(rect, radius, egui::Stroke::new(2.0, Color32::WHITE), egui::StrokeKind::Outside);
    }
    if resp.clicked() {
        acts.push(act);
    }
}

/// YouTube's chip run sorts the feed by mood, but it also carries subjects that are not
/// one. A tile for those reads as a blank brick where Apple would name a feeling.
fn feels(c: &Chip) -> bool {
    (c.params.is_some() || c.selected) && !["podcast", "radio", "live", "video"].iter().any(|n| c.title.to_lowercase().contains(n))
}

/// The mood Home offers now, as v1.10's `moodFor` chooses it: mostly one that suits the
/// hour (focus mid-morning, love late in the evening), sometimes any of them. YouTube
/// Music's grid names its own moods, and the names differ by language, so the choice is
/// made by what they say rather than by ids fixed here.
pub(super) fn mood_for_hour(moods: &[crate::ytm::Mood]) -> Option<crate::ytm::Mood> {
    let suited: &[&[&str]] = match crate::ytmapi::local_hour() {
        5..=8 => &[&["workout", "energy", "gaming"], &["feel good", "motivation", "party"], &["focus", "chill", "calm"]],
        9..=11 => &[&["focus", "study", "work"], &["feel good", "motivation"], &["workout", "energy", "gaming"]],
        12..=16 => &[&["feel good", "motivation", "party"], &["focus", "study", "work"], &["workout", "energy", "gaming"], &["chill", "relax", "calm"]],
        17..=19 => &[&["workout", "energy", "gaming"], &["feel good", "motivation", "party"], &["chill", "relax", "calm"]],
        20..=22 => &[&["romance", "love"], &["chill", "relax", "calm"], &["sad", "heartbreak"], &["feel good", "motivation", "party"]],
        _ => &[&["chill", "relax", "calm", "sleep"], &["sad", "heartbreak"], &["romance", "love"]],
    };
    let by_word = |needles: &[&str]| -> Vec<crate::ytm::Mood> { moods.iter().filter(|m| needles.iter().any(|n| m.name.to_lowercase().contains(n))).cloned().collect() };
    let mut pool = suited.iter().find_map(|n| {
        let hits = by_word(n);
        (!hits.is_empty()).then_some(hits)
    });
    // Three times in ten, and whenever the hour's moods aren't on this grid, any mood
    // at all: the tile is a way out of a rut, not a schedule.
    if pool.is_none() || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos()) % 10 < 3 {
        pool = Some(moods.to_vec());
    }
    let mut pool = pool.filter(|p| !p.is_empty())?;
    crate::model::shuffle(&mut pool);
    pool.into_iter().next()
}

// ------------------------------------------------------------------ new

fn new(app: &mut App, ui: &mut Ui, now: f64) {
    page_title(ui, "New");
    if app.explore.is_none() {
        placeholder(app, ui, now, 2, CARD);
        return;
    }
    let rows = Rows::new(app, now);
    // The card rows need the whole app (motion lookups), so take the shelves out first.
    let (releases, charts, top) = {
        let Some(e) = &app.explore else { return };
        (e.new_albums.clone(), e.charts.clone(), e.top.clone())
    };
    // Apple's New is a long stack, not a lead row: an untitled run of the week's chart
    // shows, the hot list as a numbered run, then sleeves with their names under them —
    // albums, then singles, then the charts themselves — and the moods last.
    let clip = |c: &Card| {
        let t = c.title.to_lowercase();
        t.contains("video") || t.contains("live")
    };
    let (lead, charts): (Vec<Card>, Vec<Card>) = charts.into_iter().partition(|c| clip(c));
    let kind = |c: &Card| c.subtitle.split(" • ").next().unwrap_or("").to_owned();
    let (albums, singles): (Vec<Card>, Vec<Card>) = releases.into_iter().partition(|c| kind(c) == "Album");
    card_row(app, ui, "ex-lead", "", &lead);
    if !top.is_empty() {
        song_grid(ui, &rows, &app.liked, &mut app.actions, "ex-top", "Top songs", None, &top, 4);
    }
    let heading = if app.logged_in { "New albums for you" } else { "New albums" };
    card_row(app, ui, "ex-albums", heading, &albums);
    card_row(app, ui, "ex-singles", "New singles", &singles);
    card_row(app, ui, "ex-charts", "Charts", &charts);
}

// ------------------------------------------------------------------ see all

/// The whole of one section on a page of its own, which the chevron on its name opens:
/// what a run shows a few at a time now fills the page in as many columns as fit.
fn see_all(app: &mut App, ui: &mut Ui, now: f64) {
    let Some(s) = &app.see_all else { return };
    page_title(ui, &s.title);
    ui.add_space(10.0);
    match &s.kind {
        ShelfKind::Cards(cards) => grid_shape(ui, cards, &mut app.actions, shelf_shape(cards)),
        ShelfKind::Songs { tracks, .. } => {
            let rows = Rows::new(app, now);
            let liked = &app.liked;
            let acts = &mut app.actions;
            for t in tracks {
                let o = rows.opts(liked, t, None, false);
                if track_row(ui, t, &o, acts).clicked() {
                    acts.push(Action::Radio(t.clone()));
                }
            }
        }
    }
}

// ------------------------------------------------------------------ apple shelves

/// One of Apple's browse shelves as cards. Its layout is Apple's: a square run of
/// releases, a 16:9 run of clips, or the search page's grid of coloured bricks.
/// A brick names a subject rather than a release, so clicking one searches for it.
fn apple_shelf(app: &mut App, ui: &mut Ui, id: &str, shelf: &crate::apple::Shelf) {
    use crate::apple::Layout;
    if shelf.layout == Layout::Brick {
        browse_grid(app, ui, id, &shelf.title, &shelf.tiles);
        return;
    }
    // A shelf of songs is a run of rows, three to a column: Apple's own charts read
    // that way, not as cover tiles.
    if shelf.layout == Layout::Songs {
        apple_song_run(app, ui, id, shelf);
        return;
    }
    let shape = match shelf.layout {
        Layout::Square => Shape::Square,
        Layout::Wide => Shape::Wide,
        Layout::Brick | Layout::Songs => unreachable!(),
    };
    let cards: Vec<Card> = shelf.tiles.iter().map(|t| apple_card(t, shelf.layout)).collect();
    card_row_shape(ui, id, &shelf.title, &cards, &mut app.actions, shape);
}

/// Apple's song shelf: rows of a name, an artist and a small sleeve, three to a
/// column. Clicking a row plays that song from the source that has it.
fn apple_song_run(app: &mut App, ui: &mut Ui, id: &str, shelf: &crate::apple::Shelf) {
    if shelf.tiles.is_empty() {
        return;
    }
    // Apple's song tiles aren't YouTube Music tracks, so the run has no list page behind it.
    let (l, r, _) = shelf_header(ui, &shelf.title, None, None, true, false);
    let nudge = r as i32 - l as i32;
    let col_w = 340.0;
    let acts = &mut app.actions;
    let tiles = &shelf.tiles;
    h_scroll(ui, id, nudge, tiles.len().div_ceil(3), col_w, 16.0, |ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        for col in tiles.chunks(3) {
            ui.allocate_ui(vec2(col_w, 3.0 * 54.0), |ui| {
                ui.set_width(col_w);
                ui.vertical(|ui| {
                    for t in col {
                        apple_song_row(ui, t, acts);
                    }
                });
            });
        }
    });
}

/// One of Apple's song rows: its small sleeve, the name and the artist.
fn apple_song_row(ui: &mut Ui, t: &crate::apple::Tile, acts: &mut Vec<Action>) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 54.0), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let resp = named(resp, &t.title).on_hover_cursor(egui::CursorIcon::PointingHand);
    let img = Rect::from_center_size(pos2(rect.left() + 26.0, rect.center().y), Vec2::splat(40.0));
    let art = if t.art.is_empty() { Vec::new() } else { vec![Thumb { url: t.art.clone(), width: 200, height: 200 }] };
    paint_cover(ui, img, &art, theme::thumb_radius(), 1.0);
    let x = rect.left() + 58.0;
    let width = (rect.right() - x - 16.0).max(80.0);
    let title = widgets::text_at(ui, pos2(x, rect.center().y - 17.0), &t.title, theme::semibold(14.0), theme::text(), width);
    // Only the name underlines; the row keeps the page's ground.
    if resp.hovered() {
        ui.painter().hline(egui::Rangef::new(title.left(), title.right()), title.bottom() + 2.0, egui::Stroke::new(1.0, theme::text_dim()));
    }
    if !t.subtitle.is_empty() {
        widgets::text_at(ui, pos2(x, rect.center().y + 5.0), &t.subtitle, theme::regular(13.0), theme::text_dim(), width);
    }
    if resp.clicked() {
        acts.push(Action::Open(Target::Apple {
            kind: crate::model::AppleKind::Song,
            id: t.id.clone(),
            url: t.url.clone(),
            title: t.title.clone(),
            artist: t.subtitle.clone(),
        }));
    }
}

/// Apple's tile as a card. Its sleeve is baked at the shelf's shape, and the kind is
/// what decides what opening it does.
fn apple_card(t: &crate::apple::Tile, layout: crate::apple::Layout) -> Card {
    let (w, h) = match layout {
        crate::apple::Layout::Square => (600, 600),
        crate::apple::Layout::Wide => (800, 450),
        crate::apple::Layout::Brick => (1000, 250),
        crate::apple::Layout::Songs => (200, 200),
    };
    Card {
        title: t.title.clone(),
        subtitle: t.subtitle.clone(),
        thumbs: vec![Thumb { url: t.art.clone(), width: w, height: h }],
        target: Target::Apple { kind: t.kind, id: t.id.clone(), url: t.url.clone(), title: t.title.clone(), artist: t.subtitle.clone() },
        round: false,
        links: vec![],
    }
}

/// Apple's category bricks, wrapped across the page like its search landing's grid:
/// the site's own column count, a 10dp gutter, and each brick its art's 4:1 strip.
/// A brick names a subject rather than a release, so clicking one searches for it.
fn browse_grid(app: &mut App, ui: &mut Ui, id: &str, title: &str, tiles: &[crate::apple::Tile]) {
    if tiles.is_empty() {
        return;
    }
    if !title.is_empty() {
        ui.add_space(26.0);
        ui.label(RichText::new(title).font(theme::bold(24.0)).color(theme::text()));
        ui.add_space(12.0);
    }
    let width = ui.available_width();
    let cols = feed_columns(width, Shape::Wide).clamp(2, 5);
    let w = ((width - 10.0 * (cols - 1) as f32) / cols as f32).max(160.0);
    let h = (w / 4.0).max(76.0);
    let acts = &mut app.actions;
    for (i, chunk) in tiles.chunks(cols).enumerate() {
        let row = ui.id().with((id, i));
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            for t in chunk {
                brick(ui, row, t, w, h, acts);
            }
        });
        ui.add_space(10.0);
    }
}

/// One brick: Apple's own background under the strip it serves, its name in the
/// corner, and the site's flat shade while the pointer is on it.
fn brick(ui: &mut Ui, row: egui::Id, t: &crate::apple::Tile, w: f32, h: f32, acts: &mut Vec<Action>) {
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let resp = named(resp, &t.title).on_hover_cursor(egui::CursorIcon::PointingHand);
    let radius = 10.0;
    if t.art.is_empty() {
        // No strip: the colour Apple writes behind it keeps the brick legible.
        let c = Color32::from_rgb((t.tint >> 16) as u8, (t.tint >> 8) as u8, t.tint as u8);
        ui.painter().rect_filled(rect, radius, c);
    } else {
        widgets::paint_cover_crop(ui, rect, &[Thumb { url: t.art.clone(), width: 1000, height: 250 }], radius, 1.0);
    }
    let k = theme::anim_bool(ui.ctx(), row.with("h").with(&t.title), resp.contains_pointer(), 0.1);
    if k > 0.0 {
        ui.painter().rect_filled(rect, radius, theme::shade(0.30 * k));
    }
    let title = widgets::galley_wrapped(ui, &t.title, theme::semibold(15.0), Color32::WHITE, w - 24.0, 2);
    let th = title.size().y;
    ui.painter().galley(pos2(rect.left() + 12.0, rect.bottom() - 9.0 - th), title, Color32::WHITE);
    if resp.clicked() {
        acts.push(Action::Go(Page::Search(t.title.clone())));
    }
}

pub(super) fn page_title(ui: &mut Ui, title: &str) {
    // Measured at 2x: the title's ink starts on the tiles' left edge, not inset from it.
    ui.add_space(18.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 42.0), Sense::hover());
    ui.painter().text(pos2(rect.left(), rect.center().y), Align2::LEFT_CENTER, title, theme::bold(34.0), theme::text());
}

// ------------------------------------------------------------------ search

fn search(app: &mut App, ui: &mut Ui, q: &str, now: f64) {
    let pick = super::transfer::pick_banner(app, ui);
    // Coming from a suggestion, the sidebar or a link, the field says what's shown.
    // Only when the route changed: clearing the box leaves the route alone until Enter.
    if app.search_synced != q {
        app.search_synced = q.to_owned();
        app.search_text = q.trim().to_owned();
    }
    // The box stands in for this page's title, exactly where Apple puts it; what sits
    // under it is what was opened lately, then the coloured category bricks.
    ui.add_space(18.0);
    let w = ui.available_width();
    super::shell::search_field(app, ui, w);
    if q.trim().is_empty() {
        ui.add_space(22.0);
        recent_searches(ui, app);
        browse_categories(app, ui);
        return;
    }
    if app.search.as_ref().is_none_or(|r| r.query != q) {
        ui.add_space(14.0);
        placeholder(app, ui, now, 2, CARD);
        return;
    }
    let rows = Rows::new(app, now);
    let Some(r) = &app.search else { return };
    let acts = &mut app.actions;
    let liked = &app.liked;
    if r.tracks.is_empty() && r.artists.is_empty() && r.albums.is_empty() && r.playlists.is_empty() {
        ui.add_space(18.0);
        ui.label(RichText::new("No results").font(theme::bold(20.0)).color(theme::text()));
        return;
    }
    ui.add_space(18.0);
    // Apple's order: the best of each kind the query names, then everything else by kind.
    let song = |t: &Track| Card {
        title: t.title.clone(),
        subtitle: format!("{} • {}", if t.music_video { "Video" } else { "Song" }, t.artist_line()),
        thumbs: t.thumbs.clone(),
        target: Target::Song(t.clone()),
        round: false,
        links: t.artists.clone(),
    };
    let songs: Vec<&Track> = r.tracks.iter().filter(|t| !t.music_video).collect();
    let videos: Vec<Card> = r.tracks.iter().filter(|t| t.music_video).map(song).collect();
    // The top artist only leads when the query is that name; otherwise the song is.
    let tops: Vec<Card> = [
        r.artists.first().filter(|a| a.title.eq_ignore_ascii_case(q.trim())).cloned(),
        r.albums.first().cloned(),
        songs.first().map(|t| song(t)),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !tops.is_empty() {
        ui.label(RichText::new("Top Results").font(theme::bold(24.0)));
        ui.add_space(12.0);
        let tile = ((ui.available_width() - 24.0 * (tops.len() - 1) as f32) / tops.len() as f32).clamp(260.0, 460.0);
        ui.horizontal_top(|ui| {
            for (n, c) in tops.iter().enumerate() {
                if n > 0 {
                    ui.add_space(24.0);
                }
                let liked_it = match &c.target {
                    Target::Song(t) => liked.contains(&t.id),
                    _ => false,
                };
                top_tile(ui, c, tile, pick, liked_it, acts);
            }
        });
    }
    card_row_plain(ui, "s-artists", "Artists", &r.artists, acts);
    card_row_plain(ui, "s-albums", "Albums", &r.albums, acts);
    if !songs.is_empty() {
        ui.add_space(26.0);
        ui.label(RichText::new("Songs").font(theme::bold(24.0)));
        ui.add_space(10.0);
        for t in songs {
            let o = RowOpts { pick, ..rows.opts(liked, t, None, true) };
            if track_row(ui, t, &o, acts).clicked() {
                acts.push(if pick { Action::TransferPick(t.clone()) } else { Action::Radio(t.clone()) });
            }
        }
    }
    card_row_plain(ui, "s-pl", "Playlists", &r.playlists, acts);
    card_row_plain(ui, "s-videos", "Music Videos", &videos, acts);
}

/// Apple's search landing: what was opened from the last few searches, with its artwork.
fn recent_searches(ui: &mut Ui, app: &mut App) {
    if app.recent_searches.is_empty() {
        ui.label(RichText::new("Search for artists, songs, albums and playlists.").font(theme::regular(15.0)).color(theme::text_dim()));
        return;
    }
    ui.label(RichText::new("Recently Searched").font(theme::bold(22.0)).color(theme::text()));
    ui.add_space(12.0);
    let w = ((ui.available_width() - 24.0) / 4.0).max(1.0);
    let recents = app.recent_searches.clone();
    let mut remove = None;
    for (row, chunk) in recents.chunks(4).take(2).enumerate() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
            for (col, r) in chunk.iter().enumerate() {
                let (rect, resp) = ui.allocate_exact_size(vec2(w, 64.0), Sense::click());
                let resp = named(resp, &r.title).on_hover_cursor(egui::CursorIcon::PointingHand);
                let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.contains_pointer(), 0.12);
                ui.painter().rect_filled(rect, 10.0, theme::glass(0.05 + 0.06 * h));
                let side = (w - 16.0).clamp(0.0, 48.0);
                let art = Rect::from_min_size(rect.left_top() + vec2(8.0, (64.0 - side) / 2.0), Vec2::splat(side));
                let radius = if r.round { side / 2.0 } else { 6.0 };
                match r.thumb.as_deref().filter(|u| !u.is_empty()) {
                    Some(url) => {
                        let thumbs = [crate::model::Thumb { url: url.to_owned(), width: 300, height: 300 }];
                        paint_cover(ui, art, &thumbs, radius, 1.0);
                    }
                    None => {
                        ui.painter().rect_filled(art, radius, theme::glass(0.1));
                    }
                }
                let x = art.right() + 10.0;
                let text_width = (rect.right() - x - 8.0).max(0.0);
                widgets::text_at(ui, pos2(x, rect.center().y - 17.0), &r.title, theme::semibold(15.0), theme::text(), text_width);
                widgets::text_at(ui, pos2(x, rect.center().y + 4.0), &r.subtitle, theme::regular(13.0), theme::text_dim(), text_width);
                egui::Popup::context_menu(&resp).show(|ui| {
                    ui.set_min_width(200.0);
                    if ui.button("Remove from Recently Searched").clicked() {
                        remove = Some(row * 4 + col);
                        ui.close();
                    }
                });
                if resp.clicked() {
                    app.actions.push(match &r.target {
                        Some(t) => Action::Open(t.clone()),
                        None => Action::Go(Page::Search(r.query.clone())),
                    });
                }
            }
        });
        ui.add_space(6.0);
    }
    if let Some(i) = remove {
        app.recent_searches.remove(i);
    }
}

/// Apple's search landing below its recents: the storefront's own category bricks,
/// read from the same page the site draws them on.
fn browse_categories(app: &mut App, ui: &mut Ui) {
    let bricks = app
        .apple_shelves
        .get("gb/search")
        .and_then(|s| s.iter().find(|s| s.layout == crate::apple::Layout::Brick).cloned());
    let Some(bricks) = bricks else { return };
    browse_grid(app, ui, "browse", &bricks.title, &bricks.tiles);
}

/// One of the query's best matches: the tile Apple puts in its "Top Results" row —
/// artwork at the left, the name and what it is beside it, and a play button in the
/// corner. `pick`: choosing a transfer match, so clicking a song picks it.
fn top_tile(ui: &mut Ui, c: &Card, width: f32, pick: bool, liked: bool, acts: &mut Vec<Action>) {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 108.0), Sense::click());
    let resp = named(resp, &c.title).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.contains_pointer(), 0.2);
    ui.painter().rect_filled(rect, 14.0, theme::glass(0.07 + 0.05 * h));
    let img = Rect::from_min_size(rect.min + vec2(16.0, 16.0), Vec2::splat(76.0));
    paint_cover(ui, img, &c.thumbs, if c.round { 38.0 } else { theme::card_radius() }, 1.0);
    let x = img.right() + 14.0;
    let text_w = (rect.right() - x - 52.0).max(0.0);
    widgets::text_at(ui, pos2(x, img.top() + 4.0), &c.title, theme::bold(18.0), theme::text(), text_w);
    widgets::linked_text(ui, pos2(x, img.top() + 32.0), &c.subtitle, &c.links, theme::regular(13.0), theme::text_dim(), text_w, acts);
    // Floating play button
    let pc = rect.right_bottom() + vec2(-36.0, -36.0 + 5.0 * (1.0 - h));
    let pr = Rect::from_center_size(pc, Vec2::splat(46.0));
    let active = widgets::active_state(ui, &c.target);
    let playing = active == Some(true);
    let label = format!("{} {}", if playing { "Pause" } else { "Play" }, c.title);
    let p = named(ui.interact(pr, resp.id.with("play"), Sense::click()), &label).on_hover_cursor(egui::CursorIcon::PointingHand);
    ui.painter().circle_filled(pc, 23.0, theme::with_alpha(Color32::WHITE, 0.4 + 0.6 * h));
    let nudge = if playing { 0.0 } else { 1.4 };
    widgets::play_pause_glyph(ui, pc + vec2(nudge, 0.0), 9.5, Color32::from_gray(15), playing);
    if p.clicked() {
        acts.push(match &c.target {
            _ if active.is_some() => Action::TogglePlay,
            Target::Song(t) => Action::Radio(t.clone()),
            t => Action::PlayCollection(t.clone(), false),
        });
    } else if resp.clicked() {
        acts.push(match &c.target {
            Target::Song(t) if pick => Action::TransferPick(t.clone()),
            t => Action::Open(t.clone()),
        });
    }
    // Right-click the box for the same menu the tile's ⋮ carries.
    widgets::menu_popup(egui::Popup::context_menu(&resp), widgets::MENU_W, |ui| widgets::card_menu_contents(ui, c, liked, acts));
}

// ------------------------------------------------------------------ album / playlist

fn collection(app: &mut App, ui: &mut Ui, id: &str, now: f64) {
    if id == super::LIKED_MUSIC && !app.logged_in {
        library(app, ui, LibTab::Songs, now);
        return;
    }
    if app.opened(id).is_none() {
        placeholder(app, ui, now, 1, COVER);
        return;
    }
    let rows = Rows::new(app, now);
    // The page wears its own cover: asked once per release, and painted behind everything
    // below. A quiet fetch that never answers is asked again once the page has sat a while.
    let cover = app.opened(id).and_then(|o| o.c.thumbs.first()).map(|t| t.url.clone()).unwrap_or_default();
    let held = app.page_art.as_ref().is_some_and(|(k, _)| k == id);
    let stale = app.page_art_asked.as_ref().is_none_or(|(k, t)| k != id || now - t > 20.0);
    if !held && !cover.is_empty() && stale {
        app.page_art_asked = Some((id.to_owned(), now));
        app.backend.request(crate::backend::Req::PageArt { key: id.to_owned(), url: cover });
    }
    if held {
        let top = ui.cursor().min - vec2(48.0, 18.0);
        let w = ui.available_width() + 96.0;
        // Long enough to run past the fold: the fade spends itself across the whole top of
        // the window instead of ending on a line halfway down a short page.
        let h = (COVER + 250.0).max(ui.ctx().content_rect().height());
        super::shell::cover_wash(app, ui, Rect::from_min_size(top, vec2(w, h)));
    }
    // The header's motion sleeve, asked for before the release is borrowed below.
    // Albums only: playlist covers stay still.
    let (release, artist) = match app.opened(id) {
        Some(o) => (o.c.title.clone(), o.c.artists.first().map(|a| a.name.clone()).unwrap_or_else(|| o.c.byline())),
        None => (String::new(), String::new()),
    };
    // The header keeps its still sleeve: moving artwork belongs to the player.
    let sprite: Option<widgets::MotionSprite> = None;
    let Some(o) = app.lists.get(id) else { return };
    let (c, more) = (&o.c, app.receiving(id));
    let acts = &mut app.actions;
    let tracks = &c.tracks;
    let is_album = c.is_album;
    let filter_on = app.coll_search.as_ref().is_some_and(|(fid, _)| fid == id);
    let selecting = app.selection.on(&app.page);
    let mut toggle_search = false;
    let mut start_select = false;
    let mut stop_select = false;
    ui.add_space(18.0);
    ui.horizontal_top(|ui| {
        let (img, _) = ui.allocate_exact_size(Vec2::splat(COVER), Sense::hover());
        widgets::soft_shadow(ui, img, 10.0, 40.0, 0.5);
        if id == super::LIKED_MUSIC {
            ui.painter().rect_filled(img, theme::card_radius() + 2.0, Color32::from_rgb(245, 245, 247));
            glyphs::draw(ui, img.center(), COVER * 0.58, glyphs::STAR_FILLED, Color32::from_rgb(227, 62, 75));
        } else {
            paint_cover(ui, img, &c.thumbs, theme::card_radius() + 2.0, 1.0);
        }
        if let Some(s) = &sprite {
            widgets::paint_motion(ui, img, theme::card_radius() + 2.0, s);
        }
        ui.add_space(32.0);
        ui.vertical(|ui| {
            ui.set_max_width(ui.available_width().min(760.0));
            // Apple sets the name over the cover's lower half with no kind eyebrow above it.
            ui.add_space(if is_album { 30.0 } else { 72.0 });
            ui.label(RichText::new(if id == super::LIKED_MUSIC { "Favourite Songs" } else { &c.title }).font(theme::bold(42.0)).color(theme::text()));
            ui.add_space(4.0);
            if id != super::LIKED_MUSIC {
                let (byline_size, byline_color) = if is_album { (15.0, theme::text_dim()) } else { (21.0, theme::text()) };
                let owner = if c.subtitle.is_empty() { c.byline() } else { c.subtitle.clone() };
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), byline_size + 5.0), Sense::hover());
                widgets::linked_text(ui, r.min, &owner, &c.artists, theme::regular(byline_size), byline_color, r.width(), acts);
            }
            let total: u32 = tracks.iter().filter_map(|t| t.duration).sum();
            if total > 0 {
                ui.label(
                    RichText::new(format!("{} songs • {} min", tracks.len(), total / 60)).font(theme::regular(13.5)).color(theme::text_faint()),
                );
            }
            if let Some(d) = c.description.as_deref().filter(|d| id != super::LIKED_MUSIC && !d.trim().is_empty()) {
                ui.add_space(6.0);
                let g = widgets::galley_wrapped(ui, d.trim(), theme::regular(13.5), theme::text_dim(), ui.available_width(), 2);
                ui.label(g);
            }
            ui.add_space(18.0);
            let target = if is_album { Target::Album(id.to_owned()) } else { Target::Playlist(id.to_owned()) };
            // Liked Music and Episodes for later are the account's own lists: they can't be
            // saved, removed, edited or deleted.
            let auto = matches!(id, "LM" | "SE");
            let saved = app.library.as_ref().is_some_and(|l| {
                let cards = if is_album { &l.albums } else { &l.playlists };
                cards.iter().any(|card| card.target == target)
            });
            // Saving only applies to someone else's playlist or an album.
            let save_id = if is_album { c.save_id.clone() } else { (!c.owned).then(|| id.to_owned()) };
            let owned = !auto && !is_album && c.owned;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = vec2(12.0, 0.0);
                // Apple leads with the shuffle circle and sets the white pill beside it.
                if circle_action_sized(ui, CircleGlyph::Mark(glyphs::SHUFFLE), "Shuffle", false, 44.0).clicked() && !tracks.is_empty() {
                    acts.push(Action::PlayShuffled(tracks.clone()));
                }
                play_pill_h(ui, acts, &target, tracks, 44.0);
                if circle_action_sized(ui, CircleGlyph::Mark(glyphs::SEARCH), "Search in this list", filter_on, 44.0).clicked() {
                    toggle_search = true;
                }
                let more = circle_action_sized(ui, CircleGlyph::More, "More", false, 44.0);
                egui::Popup::menu(&more).show(|ui| {
                    ui.set_width(226.0);
                    ui.add_space(3.0);
                    if let Some(first) = tracks.first()
                        && widgets::menu_action(ui, Some(icon::RADIO), "Start radio")
                    {
                        acts.push(Action::Radio(first.clone()));
                    }
                    if is_album && widgets::menu_action(ui, None, "About album") {
                        acts.push(Action::AboutAlbum { id: id.to_owned(), title: release.clone(), artist: artist.clone() });
                    }
                    if let Some(save_id) = save_id.clone()
                        && widgets::menu_action(ui, Some(icon::LIBRARY), if saved { "Remove from library" } else { "Save to library" })
                    {
                        acts.push(Action::SaveCollection(save_id, !saved));
                    }
                    if app.logged_in && (!auto || id == super::LIKED_MUSIC) && !tracks.is_empty() {
                        if widgets::menu_action(ui, Some(icon::LIST_CHECKS), if selecting { "Done selecting" } else { "Select" }) {
                            if selecting { stop_select = true } else { start_select = true }
                        }
                    }
                    if app.logged_in && !auto && (save_id.is_some() || owned) {
                        ui.separator();
                    }
                    if owned {
                        if widgets::menu_action(ui, Some(icon::MUSIC), "Edit playlist") {
                            acts.push(Action::EditPlaylist);
                        }
                        if widgets::menu_action(ui, Some(icon::USERS), "Collaborate") {
                            acts.push(Action::Collaborate);
                        }
                        if widgets::menu_action(ui, Some(icon::TRASH_2), "Delete playlist") {
                            acts.push(Action::DeletePlaylist);
                        }
                    }
                    if app.logged_in && !auto && !owned && save_id.is_none() {
                        ui.separator();
                    }
                    if widgets::menu_action(ui, Some(icon::LINK), "Share") {
                        acts.push(Action::Share(target.clone()));
                    }
                    ui.add_space(3.0);
                });
            });
            if toggle_search {
                let closing = app.coll_search.as_ref().is_some_and(|(fid, _)| fid == id);
                app.coll_search = (!closing).then(|| (id.to_owned(), String::new()));
            }
            if start_select {
                app.selection.start(&app.page);
            }
            if stop_select {
                app.selection.clear();
            }

        });
    });
    list_search(ui, &mut app.coll_search, id, toggle_search);
    let query = app.coll_search.as_ref().filter(|(fid, _)| fid == id).map_or("", |(_, q)| q.as_str());
    let indices = collection_filter(tracks, query);
    let filtered: Vec<Track> = indices.as_ref().map_or_else(Vec::new, |indices| indices.iter().map(|&i| tracks[i].clone()).collect());
    let shown: &[Track] = if indices.is_some() { &filtered } else { tracks };
    let number = |i: usize, t: &Track| is_album.then(|| t.track_nr.map_or(i + 1, |n| n as usize));
    let own = removable_collection(c, app.logged_in);
    // Apple keeps the artist under the song's name in a playlist and gives the release its
    // own column; an album's rows are numbered and name no album at all.
    list(ui, &rows, &app.liked, shown, 0, shown, !is_album, false, number, ListCtx { sel: &mut app.selection, page: &app.page, collection: own.then_some(c), indices: indices.as_deref() }, acts);
    if more {
        widgets::skeleton_rows(ui, now, 3);
    }
}

/// The field that filters a list's rows, and the space that stands in for it when the
/// list isn't being searched. `opening` is the frame it was just asked for, when it
/// takes the caret.
fn list_search(ui: &mut Ui, search: &mut Option<(String, String)>, id: &str, opening: bool) {
    let mut dismiss = false;
    if let Some((fid, query)) = search
        && fid == id
    {
        ui.add_space(14.0);
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width().min(420.0), 40.0), Sense::hover());
        ui.painter().rect_filled(rect, 20.0, theme::glass(0.09));
        glyphs::draw(ui, pos2(rect.left() + 20.0, rect.center().y), glyphs::box_for(glyphs::SEARCH, 14.0), glyphs::SEARCH, theme::text_dim());
        let clearable = !query.is_empty();
        let inner = Rect::from_min_max(pos2(rect.left() + 40.0, rect.top() + 4.0), pos2(rect.right() - if clearable { 40.0 } else { 16.0 }, rect.bottom() - 4.0));
        let edit = ui.put(
            inner,
            egui::TextEdit::singleline(query)
                .hint_text(egui::RichText::new("Search in this list").color(theme::text_faint()))
                .font(theme::regular(14.0))
                .vertical_align(egui::Align::Center)
                .frame(egui::Frame::NONE)
                .desired_width(inner.width()),
        );
        if opening {
            edit.request_focus();
        } else if !edit.has_focus() && query.is_empty() {
            // Left alone empty, the field goes away instead of sitting over the list.
            dismiss = true;
        }
        if clearable {
            let x = Rect::from_center_size(pos2(rect.right() - 20.0, rect.center().y), Vec2::splat(26.0));
            let resp = named(ui.interact(x, ui.scope_id().with("coll-clear"), Sense::click()), "Clear search").on_hover_cursor(egui::CursorIcon::PointingHand);
            glyphs::draw(ui, x.center(), glyphs::box_for(glyphs::CLOSE, 11.0), glyphs::CLOSE, theme::text_dim());
            if resp.clicked() {
                query.clear();
            }
        }
        ui.add_space(18.0);
    } else {
        ui.add_space(26.0);
    }
    if dismiss {
        *search = None;
    }
}

// ------------------------------------------------------------------ apple collection

/// Apple's own collection page, drawn as Apple draws it: the header it serves and the
/// songs it lists. The songs keep Apple's names; each one plays from the source that
/// has it.
fn apple_collection(app: &mut App, ui: &mut Ui, url: &str, now: f64) {
    let Some(c) = app.apple_collections.get(url).cloned() else {
        placeholder(app, ui, now, 1, COVER);
        return;
    };
    let is_album = c.kind == crate::model::AppleKind::Album;
    // Apple's address carries the record's own id, and that is the only way to ask for
    // a playlist's motion sleeve: Apple's album index has no playlists in it.
    let record = crate::apple::id_from_url(url);
    let record = record.as_deref();
    // Still artwork here; the player is the only place that moves.
    let sprite: Option<widgets::MotionSprite> = None;
    let target = Target::Apple { kind: c.kind, id: String::new(), url: url.to_owned(), title: c.title.clone(), artist: c.author.clone() };
    ui.add_space(18.0);
    ui.horizontal_top(|ui| {
        let (img, _) = ui.allocate_exact_size(Vec2::splat(COVER), Sense::hover());
        widgets::soft_shadow(ui, img, 10.0, 40.0, 0.5);
        let art = c.art.clone().map(|u| vec![Thumb { url: u, width: 800, height: 800 }]).unwrap_or_default();
        paint_cover(ui, img, &art, theme::card_radius() + 2.0, 1.0);
        if let Some(s) = &sprite {
            widgets::paint_motion(ui, img, theme::card_radius() + 2.0, s);
        }
        ui.add_space(32.0);
        ui.vertical(|ui| {
            ui.set_max_width(ui.available_width().min(760.0));
            ui.add_space(18.0);
            ui.label(RichText::new(&c.title).font(theme::bold(42.0)).color(theme::text()));
            if !c.author.is_empty() {
                ui.add_space(6.0);
                ui.label(RichText::new(&c.author).font(theme::regular(17.0)).color(theme::text_dim()));
            }
            let total: u32 = c.songs.iter().filter_map(|s| s.duration).sum();
            if total > 0 {
                ui.label(
                    RichText::new(format!("{} songs • {} min", c.songs.len(), total / 60)).font(theme::regular(13.5)).color(theme::text_faint()),
                );
            }
            if let Some(d) = c.description.as_deref().filter(|d| !d.trim().is_empty()) {
                ui.add_space(8.0);
                let g = widgets::galley_wrapped(ui, d.trim(), theme::regular(13.5), theme::text_dim(), ui.available_width(), 3);
                ui.label(g);
            }
            ui.add_space(18.0);
            let acts = &mut app.actions;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing = vec2(12.0, 0.0);
                if circle_action_sized(ui, CircleGlyph::Mark(glyphs::SHUFFLE), "Shuffle", false, 44.0).clicked() && !c.songs.is_empty() {
                    acts.push(Action::ApplePlay { target: target.clone(), songs: c.songs.clone(), shuffle: true });
                }
                if widgets::pill_h_apple(ui, "Play", Some(glyphs::PLAY), true, 44.0).clicked() && !c.songs.is_empty() {
                    acts.push(Action::ApplePlay { target: target.clone(), songs: c.songs.clone(), shuffle: false });
                }
            });
        });
    });
    ui.add_space(26.0);
    apple_songs(ui, &c.songs, is_album, &target, &mut app.actions);
}

/// Apple's song table: the sleeve, the name, the artist and the time. Clicking a row
/// plays that song from wherever it can be found.
fn apple_songs(ui: &mut Ui, songs: &[crate::apple::AppleSong], numbered: bool, target: &Target, acts: &mut Vec<Action>) {
    if songs.is_empty() {
        return;
    }
    widgets::visible_rows(ui, songs.len(), |ui, i| {
        let s = &songs[i];
        let (rect, base) = ui.allocate_exact_size(vec2(ui.available_width(), 56.0), Sense::hover());
        if !ui.is_rect_visible(rect) {
            return;
        }
        // The options at the right keep the row's own click to itself.
        let hit = Rect::from_min_max(pos2(rect.left(), rect.top()), pos2(rect.right() - 42.0, rect.bottom()));
        let resp = named(ui.interact(hit, base.id.with("play"), Sense::click()), &s.title).on_hover_cursor(egui::CursorIcon::PointingHand);
        let cy = rect.center().y;
        let mut x = rect.left() + 10.0;
        if numbered {
            let slot = Rect::from_center_size(pos2(x + 12.0, cy), vec2(24.0, 24.0));
            ui.painter().text(slot.center(), Align2::CENTER_CENTER, (i + 1).to_string(), theme::regular(14.0), theme::text_dim());
            x += 34.0;
        } else if let Some(art) = s.art.as_deref() {
            // The song's own sleeve, as every other list of songs shows it.
            let r = Rect::from_min_size(pos2(x, cy - 20.0), Vec2::splat(40.0));
            widgets::paint_remote(ui, art, r, theme::thumb_radius() + 1.0, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            x += 54.0;
        }
        let width = (rect.right() - x - 60.0).max(80.0);
        let title = widgets::text_at(ui, pos2(x, cy - 16.0), &s.title, theme::semibold(14.5), theme::text(), width);
        // Only the name underlines; the row keeps the page's ground.
        if resp.hovered() {
            ui.painter().hline(egui::Rangef::new(title.left(), title.right()), title.bottom() + 2.0, egui::Stroke::new(1.0, theme::text_dim()));
        }
        // The artist under the name, and — in a playlist, where a song can come from
        // anywhere — the release it sits on after that.
        let mut who = if !s.artist.is_empty() { s.artist.clone() } else { s.album.clone().unwrap_or_default() };
        if !numbered {
            if let Some(a) = s.album.as_deref().filter(|a| !a.trim().is_empty()) {
                if !who.is_empty() {
                    who.push_str(" \u{2022} ");
                }
                who.push_str(a);
            }
        }
        if !who.is_empty() {
            widgets::text_at(ui, pos2(x, cy + 6.0), &who, theme::regular(13.0), theme::text_dim(), width);
        }
        if let Some(d) = s.duration {
            ui.painter().text(pos2(rect.right() - 51.0, cy), Align2::RIGHT_CENTER, clock(d), theme::regular(13.0), theme::text_faint());
        }
        let more_rect = Rect::from_center_size(pos2(rect.right() - 20.0, cy), Vec2::splat(30.0));
        let more = widgets::more_button(ui, more_rect, base.id.with("more"), "Song options");
        widgets::menu_popup(egui::Popup::menu(&more), widgets::MENU_W, |ui| apple_song_menu(ui, s, i, songs, target, acts));
        widgets::menu_popup(egui::Popup::context_menu(&resp), widgets::MENU_W, |ui| apple_song_menu(ui, s, i, songs, target, acts));
        if resp.clicked() {
            acts.push(Action::Open(Target::Apple {
                kind: crate::model::AppleKind::Song,
                id: s.id.clone(),
                url: s.url.clone(),
                title: s.title.clone(),
                artist: s.artist.clone(),
            }));
        }
    });
}

/// What an Apple song offers: play it, play the rest of the list from here, shuffle it
/// from here, or take Apple's own link for it.
fn apple_song_menu(ui: &mut Ui, s: &crate::apple::AppleSong, i: usize, songs: &[crate::apple::AppleSong], target: &Target, acts: &mut Vec<Action>) {
    let song = Target::Apple {
        kind: crate::model::AppleKind::Song,
        id: s.id.clone(),
        url: s.url.clone(),
        title: s.title.clone(),
        artist: s.artist.clone(),
    };
    ui.set_width(226.0);
    ui.add_space(3.0);
    if widgets::menu_action(ui, Some(icon::PLAY), "Play") {
        acts.push(Action::Open(song.clone()));
    }
    if widgets::menu_action(ui, Some(icon::PLAY), "Play from here") {
        acts.push(Action::ApplePlay { target: target.clone(), songs: songs[i..].to_vec(), shuffle: false });
    }
    if widgets::menu_action(ui, Some(icon::SHUFFLE), "Shuffle from here") {
        acts.push(Action::ApplePlay { target: target.clone(), songs: songs[i..].to_vec(), shuffle: true });
    }
    ui.separator();
    if widgets::menu_action(ui, None, "Copy link") {
        acts.push(Action::Share(song));
    }
    ui.add_space(3.0);
}

/// A length of time the way Apple writes it: minutes, then seconds.
fn clock(secs: u32) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

// ------------------------------------------------------------------ artist

/// Apple's wordmark for the artist, bottom-left at `base` (or centered on `center`), no
/// wider than `max_w`.
/// False until the image is ready, so the caller can draw the name instead.
fn wordmark(ui: &Ui, base: egui::Pos2, logo: &crate::apple::LogoSrc, max_w: f32, center: Option<f32>) -> bool {
    let aspect = logo.w.max(1) as f32 / logo.h.max(1) as f32;
    let mut size = vec2(72.0 * aspect, 72.0);
    if size.x > max_w {
        size = vec2(max_w, max_w / aspect);
    }
    let url = logo.at_height((size.y * 2.0) as u32);
    let ready = matches!(egui::Image::new(url.as_str()).load_for_size(ui.ctx(), size), Ok(egui::load::TexturePoll::Ready { .. }));
    if ready {
        let left = center.map_or(base.x, |cx| cx - size.x / 2.0);
        let rect = Rect::from_min_size(pos2(left, base.y - 40.0 - size.y), size);
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        widgets::paint_remote(ui, &url, rect, 0.0, uv, Color32::WHITE);
    }
    ready
}

fn artist(app: &mut App, ui: &mut Ui, id: &str, now: f64) {
    if app.artist.as_ref().is_none_or(|a| a.id != id) {
        placeholder(app, ui, now, 2, CARD);
        return;
    }
    let rows = Rows::new(app, now);
    let accent = app.accent_color();
    // Apple's own page for this artist: their display name and the hero clip. Artists
    // Apple has no clip for (Charli xcx) keep the still banner.
    let apple = app.apple_artists.get(id).and_then(|o| o.clone());
    let name = apple.as_ref().map(|x| x.name.clone()).unwrap_or_else(|| app.artist.as_ref().map(|a| a.name.clone()).unwrap_or_default());
    // Apple's search misses once now and then. While the page is open, ask again after
    // the cooldown rather than waiting for the listener to come back to it.
    if apple.is_none() {
        let key = format!("artist:{id}");
        let cooling = app.apple.missed.get(&key).is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(120));
        if !cooling && app.apple.waiting.insert(key) {
            app.backend.request(crate::backend::Req::AppleArtist { id: id.to_owned(), name: name.clone() });
        }
    }
    // Apple's wordmark wins the hero; failing one, the listener's signature service;
    // failing that, the name in type. A lettered artist still has their signature
    // asked for, because it signs the About section below instead of the hero.
    let signature = app.signature_texture(ui.ctx(), &name);
    // Apple's own hero clip on the artist's page: the one moving picture outside the
    // player. Everything else stays still.
    let hero_sprite = if let Some(a) = &app.artist {
        let _ = a;
        apple.as_ref().and_then(|x| x.hero_video.clone()).and_then(|url| {
            let px = (ui.available_width() * ui.ctx().pixels_per_point()).round().clamp(256.0, 640.0) as u32;
            app.motion_sprite(ui.ctx(), &url, px)
        })
    } else {
        None
    };
    let Some(a) = &app.artist else { return };
    let acts = &mut app.actions;
    let top = &a.top;
    // Apple's hero: the banner bleeds edge to edge and fills most of the window, the name
    // stands centered over its lower part, and the three discs float under it in the fade.
    let w = ui.available_width();
    // The hero is a band, not a poster: Apple keeps it to a little over a third of the
    // window's height, so the artist's name and discs sit under it without the page
    // feeling top-heavy.
    let img_h = (ui.ctx().viewport_rect().height() * 0.40).clamp(240.0, 460.0);
    let side = (img_h * 0.105).clamp(36.0, 48.0);
    let mid = (img_h * 0.165).clamp(56.0, 72.0);
    let gap = mid * 1.1;
    let (band, _) = ui.allocate_exact_size(vec2(w, img_h), Sense::hover());
    let hero = band;
    // Apple's own header art first: their wide still, then the YouTube banner, then a
    // wash of the accent. The motion clip paints over whichever still is standing.
    if let Some(url) = apple.as_ref().and_then(|x| x.hero_still.as_deref()) {
        // The still is Apple's 16:9 crop; cover-fit it to the band's own shape.
        let dst = w / img_h;
        let src = 16.0 / 9.0;
        let uv = if src > dst {
            let vis_w = dst / src;
            Rect::from_min_max(pos2((1.0 - vis_w) / 2.0, 0.0), pos2((1.0 + vis_w) / 2.0, 1.0))
        } else {
            let vis_h = src / dst;
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, vis_h))
        };
        ui.painter().rect_filled(hero, 0.0, theme::glass(0.07));
        widgets::paint_remote(ui, url, hero, 0.0, uv, Color32::WHITE);
    } else if let Some(t) = a.banner.iter().max_by_key(|t| t.width) {
        let img_ratio = if t.height > 0 { t.width as f32 / t.height as f32 } else { 16.0 / 9.0 };
        // Cover-fit: crop the image's top band to the hero's aspect.
        let vis_h = (img_ratio / (w / img_h)).min(1.0);
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, vis_h));
        ui.painter().rect_filled(hero, 0.0, theme::glass(0.07));
        widgets::paint_remote(ui, &t.url, hero, 0.0, uv, Color32::WHITE);
    } else {
        ui.painter().rect_filled(hero, 0.0, theme::with_alpha(accent, 0.25));
    }
    if let Some(s) = &hero_sprite {
        widgets::paint_motion(ui, hero, 0.0, s);
    }
    // The fade runs past the picture and under the discs, into the page's own ground.
    super::shell::vgradient(ui.painter(), Rect::from_min_max(pos2(band.left(), band.top() + img_h * 0.42), band.max), Color32::TRANSPARENT, theme::shade(0.85));
    // A second wash over the lower half: the name, signature and discs sit on dark
    // ground like the reference, not on the clip's own floor.
    super::shell::vgradient(ui.painter(), Rect::from_min_max(pos2(band.left(), band.top() + img_h * 0.5), band.max), Color32::TRANSPARENT, theme::shade(0.85));
    let cx = band.center().x;
    let name_base = pos2(cx, band.top() + img_h * 0.75);
    let name_size = (img_h * 0.088).clamp(30.0, 44.0);
    // Apple letters some artists a wordmark image instead of setting their name in type.
    // Until it downloads (or when there's none) the typed name stands in.
    let drawn = apple.as_ref().and_then(|x| x.logo.as_ref()).is_some_and(|logo| wordmark(ui, name_base, logo, w * 0.55, Some(cx)));
    if !drawn {
        match &signature {
            // The artist's own hand, credited above it the way a page signs itself off.
            Some(tex) => {
                let tex_size = tex.size_vec2();
                let aspect = tex_size.x / tex_size.y.max(1.0);
                let sig_h = (img_h * 0.13).clamp(60.0, 110.0);
                let mut size = vec2(sig_h * aspect, sig_h);
                let max_w = (w * 0.42).min(420.0);
                if size.x > max_w {
                    size = vec2(max_w, max_w / aspect);
                }
                let rect = Rect::from_min_size(pos2(cx - size.x / 2.0, name_base.y - 14.0 - size.y), size);
                ui.painter().text(pos2(rect.center().x, rect.top() - 8.0), Align2::CENTER_BOTTOM, format!("Signed by {name}"), theme::regular(12.0), Color32::from_white_alpha(199));
                let tex = egui::load::SizedTexture::from_handle(tex);
                egui::Image::from_texture(tex).uv(Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))).paint_at(ui, rect);
            }
            None => {
                ui.painter().text(name_base, Align2::CENTER_BOTTOM, &name, theme::bold(name_size), Color32::WHITE);
            }
        }
    }
    // The three-circle row: info and star in quiet glass, play on white in the middle,
    // the way Apple's header carries them.
    let target = Target::Artist(id.to_owned());
    let active = widgets::active_state(ui, &target);
    let discs_y = band.top() + img_h * 0.86;
    let info = disc_at(ui, pos2(cx - gap, discs_y), side, None, "About this artist");
    ui.painter().text(pos2(cx - gap, discs_y), Align2::CENTER_CENTER, "i", theme::semibold(side * 0.44), theme::text());
    if let Some(d) = a.description.as_deref().filter(|d| !d.trim().is_empty()) {
        egui::Popup::menu(&info).show(|ui| {
            ui.set_max_width(360.0);
            let g = widgets::galley_wrapped(ui, d.trim(), theme::regular(13.5), theme::text_dim(), 340.0, 8);
            ui.label(g);
        });
    }
    if play_disc(ui, pos2(cx, discs_y), mid, active == Some(true)).clicked() {
        if active.is_some() {
            acts.push(Action::TogglePlay);
        } else if let Some(seed) = top.first() {
            // Play on an artist starts their radio rather than just their top songs:
            // YouTube builds it around the artist of the seed, which here is theirs.
            acts.push(Action::Radio(seed.clone()));
        }
    }
    let fav = app.fav_artists.contains(id);
    let star = disc_at(ui, pos2(cx + gap, discs_y), side, None, if fav { "Remove from favourites" } else { "Add to favourites" });
    let g = if fav { glyphs::STAR_FILLED } else { glyphs::STAR };
    glyphs::draw(ui, pos2(cx + gap, discs_y), glyphs::box_for(g, side * 0.40), g, if fav { accent } else { theme::text() });
    if star.clicked() {
        if fav {
            app.fav_artists.remove(id);
        } else {
            app.fav_artists.insert(id.to_owned());
        }
    }
    // Apple puts the newest release and the top songs side by side under the hero.
    let full = ui.available_width();
    let two = !a.albums.is_empty() && top.len() > 1;
    let col = if two { ((full - 34.0) / 2.0).max(300.0) } else { full };
    ui.add_space(if two { 30.0 } else { 46.0 });
    ui.horizontal_top(|ui| {
        if two {
            ui.vertical(|ui| {
                ui.set_max_width(col);
                latest_release(ui, a.albums.first().unwrap(), acts);
            });
            ui.add_space(34.0);
        }
        if !top.is_empty() {
            ui.vertical(|ui| {
                ui.set_max_width(col);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Top songs").font(theme::bold(19.0)).color(theme::text()));
                    if let Some(pl) = &a.top_playlist {
                        let (rect, more) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
                        let more = named(more, "Show all").on_hover_cursor(egui::CursorIcon::PointingHand);
                        glyphs::draw(ui, rect.center(), glyphs::box_for(glyphs::CHEVRON_RIGHT, 9.0), glyphs::CHEVRON_RIGHT, theme::text_dim());
                        if more.clicked() {
                            acts.push(Action::Open(Target::Playlist(pl.clone())));
                        }
                    }
                });
                ui.add_space(10.0);
                for t in top.iter().take(3) {
                    let mut o = rows.opts(&app.liked, t, None, false);
                    o.sub_album = true;
                    if track_row(ui, t, &o, acts).clicked() {
                        acts.push(Action::Radio(t.clone()));
                    }
                }
            });
        }
    });
    card_row_plain(ui, "a-albums", "Albums & singles", &a.albums, acts);
    card_row_plain(ui, "a-pl", "Featured on", &a.playlists, acts);
    card_row_plain(ui, "a-sim", "Fans might also like", &a.similar, acts);
    if let Some(d) = a.description.as_deref().filter(|d| !d.trim().is_empty()) {
        shelf_header(ui, "About", None, None, false, false);
        let full = ui.available_width().min(820.0);
        // A lettered artist's signature can't be the hero, so it signs the About
        // section beside the prose the way a page signs itself off.
        let beside = drawn.then_some(signature.as_ref()).flatten();
        if let Some(tex) = beside {
            ui.horizontal_top(|ui| {
                let sig_w = (full * 0.28).clamp(120.0, 210.0);
                let text_w = (full - sig_w - 32.0).max(220.0);
                let g = widgets::galley_wrapped(ui, d.trim(), theme::regular(14.5), theme::text_dim(), text_w, 8);
                ui.label(g);
                ui.add_space(32.0);
                let tex_size = tex.size_vec2();
                let aspect = tex_size.x / tex_size.y.max(1.0);
                let mut size = vec2(sig_w, sig_w / aspect);
                if size.y > 140.0 {
                    size = vec2(140.0 * aspect, 140.0);
                }
                let (rect, _) = ui.allocate_exact_size(vec2(size.x, size.y + 22.0), Sense::hover());
                ui.painter().text(rect.left_top(), Align2::LEFT_TOP, format!("Signed by {name}"), theme::regular(12.0), Color32::from_white_alpha(199));
                let img = Rect::from_min_size(pos2(rect.left(), rect.top() + 22.0), size);
                let tex = egui::load::SizedTexture::from_handle(tex);
                egui::Image::from_texture(tex).uv(Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))).paint_at(ui, img);
            });
        } else {
            let g = widgets::galley_wrapped(ui, d.trim(), theme::regular(14.5), theme::text_dim(), full, 8);
            ui.label(g);
        }
    }
    if let Some(bio) = apple.as_ref().and_then(|a| a.bio.as_deref()).filter(|bio| !bio.trim().is_empty()) {
        shelf_header(ui, &format!("About {name}"), Some("Apple Music"), None, false, false);
        let g = widgets::galley_wrapped(ui, bio.trim(), theme::regular(14.5), theme::text_dim(), ui.available_width().min(820.0), usize::MAX);
        ui.label(g);
    }
}

// ------------------------------------------------------------------ library

fn library(app: &mut App, ui: &mut Ui, tab: LibTab, now: f64) {
    if !app.logged_in {
        ui.add_space(10.0);
        ui.label(RichText::new("Sign in to see your playlists, liked songs and history.").color(theme::text_dim()));
        ui.add_space(12.0);
        if pill(ui, "Sign in with Google", Some(icon::CIRCLE_USER), true).clicked() {
            app.start_sign_in(ui.ctx());
        }
        return;
    }
    ui.add_space(12.0);
    let rows = Rows::new(app, now);
    let plain = |_: usize, _: &Track| None;
    match tab {
        LibTab::History => {
            // The sidebar's Songs row opens what has been heard lately; it says so at the
            // top of the page the way every other library tab does.
            page_title(ui, "Songs");
            ui.add_space(10.0);
            if app.history.is_none() {
                placeholder(app, ui, now, 1, 48.0);
                return;
            }
            let Some(h) = &app.history else { return };
            list(ui, &rows, &app.liked, h, 0, h, true, false, plain, ListCtx { sel: &mut app.selection, page: &app.page, collection: None, indices: None }, &mut app.actions);
        }
        LibTab::Uploads => {
            ui.horizontal(|ui| {
                if pill(ui, "Upload music", Some(icon::UPLOAD), true).clicked() {
                    app.pick_uploads(ui.ctx());
                }
                if app.uploads.as_ref().is_some_and(|u| !u.is_empty()) && pill(ui, "Shuffle", Some(icon::SHUFFLE), false).clicked() {
                    let t = app.uploads.clone().unwrap_or_default();
                    app.actions.push(Action::PlayShuffled(t));
                }
            });
            ui.add_space(6.0);
            ui.label(RichText::new("Or drop audio files anywhere on the window. MP3, M4A, FLAC, OGG and WMA.").font(theme::regular(13.0)).color(theme::text_dim()));
            ui.add_space(12.0);
            if app.uploads.is_none() {
                placeholder(app, ui, now, 1, 48.0);
                return;
            }
            let Some(up) = &app.uploads else { return };
            if up.is_empty() {
                ui.add_space(20.0);
                ui.label(RichText::new("No uploads yet").color(theme::text_dim()));
            }
            list(ui, &rows, &app.liked, up, 0, up, true, false, plain, ListCtx { sel: &mut app.selection, page: &app.page, collection: None, indices: None }, &mut app.actions);
        }
        // Apple keeps its favourites in a table of their own, not as a playlist page: the
        // same columns the site's Songs page writes, under its Sort.
        LibTab::Songs => songs(app, ui, now),
        LibTab::Playlists | LibTab::Albums | LibTab::Artists => {
            let new = lib_head(app, ui, tab.title(), tab);
            if app.library.is_none() {
                placeholder(app, ui, now, 2, CARD);
                return;
            }
            let Some(lib) = &app.library else { return };
            let mut cards: Vec<Card> = match tab {
                // Liked Music rides in the Songs tab, so it has no card here.
                LibTab::Playlists => lib.playlists.iter().filter(|c| !matches!(&c.target, Target::Playlist(id) if matches!(id.as_str(), "LM" | "VLLM"))).cloned().collect(),
                LibTab::Albums => lib.albums.iter().map(|c| Card { subtitle: without_type(&c.subtitle), ..c.clone() }).collect(),
                // Everyone behind a song the listener has hearted, once each, and never
                // the listener's own channel.
                _ => {
                    let me = app.account_name.clone().unwrap_or_default();
                    let mut seen: HashSet<String> = HashSet::new();
                    let liked = app.lists.get(super::lists::LIKED_MUSIC).map_or(&[][..], |o| o.c.tracks.as_slice());
                    liked
                        .iter()
                        .flat_map(|t| t.artists.iter().map(move |a| (a, &t.thumbs)))
                        .filter_map(|(a, thumbs)| {
                            let id = a.id.clone()?;
                            if seen.contains(&id) || (!me.is_empty() && a.name.eq_ignore_ascii_case(&me)) {
                                return None;
                            }
                            seen.insert(id.clone());
                            Some(Card { title: a.name.clone(), subtitle: "Artist".to_owned(), thumbs: thumbs.clone(), target: Target::Artist(id), round: true, links: vec![] })
                        })
                        .collect()
                }
            };
            // The artists come from the hearted songs, so their list is asked for when the
            // tab opens without it.
            if tab == LibTab::Artists && app.lists.get(super::lists::LIKED_MUSIC).is_none() {
                app.request_liked(false);
            }
            app.lib_sort.sort(&mut cards);
            if new {
                app.actions.push(Action::NewPlaylist(Vec::new()));
            }
            if tab == LibTab::Artists {
                artists(ui, &cards, &mut app.actions);
            } else {
                grid(ui, &cards, &mut app.actions);
            }
        }
    }
}

/// The line every Apple library page opens with: its name, and under it the controls
/// that page has — a way to make a playlist, and the Sort that orders the rows. The
/// answer says whether the new-playlist button was pressed.
fn lib_head(app: &mut App, ui: &mut Ui, title: &str, tab: LibTab) -> bool {
    page_title(ui, title);
    ui.add_space(10.0);
    let mut new = false;
    let mut sort: Option<LibSort> = None;
    ui.horizontal(|ui| {
        if tab == LibTab::Playlists && pill(ui, "New playlist", None, false).clicked() {
            new = true;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let btn = sort_trigger(ui, app.lib_sort.label());
            egui::Popup::menu(&btn).show(|ui| {
                ui.set_width(210.0);
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                ui.add_space(4.0);
                for o in LibSort::options(tab) {
                    if option_row(ui, o.label(), *o == app.lib_sort).clicked() {
                        sort = Some(*o);
                        ui.close();
                    }
                }
                ui.add_space(4.0);
            });
        });
    });
    if let Some(s) = sort {
        app.lib_sort = s;
    }
    new
}

/// The Sort trigger: the order it has picked, in dim text with a chevron, opening the
/// menu of orders this page offers.
fn sort_trigger(ui: &mut Ui, label: &str) -> egui::Response {
    let g = ui.painter().layout_no_wrap(label.to_owned(), theme::regular(13.5), Color32::PLACEHOLDER);
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 24.0, 28.0), Sense::click());
    let resp = named(resp, "Sort").on_hover_cursor(egui::CursorIcon::PointingHand);
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&resp));
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered() || open, 0.12);
    let fg = theme::lerp_color(theme::text_dim(), theme::text(), h);
    ui.painter().galley(pos2(rect.left(), rect.center().y - g.size().y / 2.0), g, fg);
    widgets::icon_at(ui, pos2(rect.right() - 9.0, rect.center().y), if open { icon::CHEVRON_UP } else { icon::CHEVRON_DOWN }, 13.0, fg);
    resp
}

/// Apple's library grid: the shelves' own lockups, wrapped into as many equal columns
/// as the page fits instead of run off to the right.
fn grid(ui: &mut Ui, cards: &[Card], acts: &mut Vec<Action>) {
    grid_shape(ui, cards, acts, Shape::Square);
}

/// The same grid at a tile shape the page picks, for a section whose run wasn't square.
fn grid_shape(ui: &mut Ui, cards: &[Card], acts: &mut Vec<Action>, shape: Shape) {
    if cards.is_empty() {
        ui.add_space(20.0);
        ui.label(RichText::new("Nothing here yet").color(theme::text_dim()));
        return;
    }
    let tile = feed_tile(ui, shape);
    let cols = (((ui.available_width() + 20.0) / (tile + 20.0)).floor() as usize).clamp(1, 8);
    for chunk in cards.chunks(cols) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing = vec2(20.0, 0.0);
            for c in chunk {
                card_shape(ui, c, tile, shape, false, acts, None);
            }
        });
        ui.add_space(20.0);
    }
}

/// Apple's Artists page: one row each, a 36 px circle and the name over a keyline. Not
/// the cover grid — the site keeps its artists in a plain list, and plays them from
/// their own page.
fn artists(ui: &mut Ui, cards: &[Card], acts: &mut Vec<Action>) {
    if cards.is_empty() {
        ui.add_space(20.0);
        ui.label(RichText::new("No artists in your library yet").color(theme::text_dim()));
        return;
    }
    widgets::visible_rows_with(ui, cards.len(), 54.0, |ui, i| {
        let c = &cards[i];
        let (rect, base) = ui.allocate_exact_size(vec2(ui.available_width(), 54.0), Sense::hover());
        if !ui.is_rect_visible(rect) {
            return;
        }
        let hit = Rect::from_min_max(pos2(rect.left(), rect.top()), pos2(rect.right() - 44.0, rect.bottom()));
        let resp = named(ui.interact(hit, base.id.with("open"), Sense::click()), &c.title).on_hover_cursor(egui::CursorIcon::PointingHand);
        let cy = rect.center().y;
        let img = Rect::from_min_size(pos2(rect.left() + 10.0, cy - 18.0), Vec2::splat(36.0));
        paint_cover(ui, img, &c.thumbs, 18.0, 1.0);
        let x = img.right() + 16.0;
        let name = widgets::text_at(ui, pos2(x, cy - 9.0), &c.title, theme::semibold(14.5), theme::text(), (rect.right() - 56.0 - x).max(60.0));
        if resp.hovered() {
            ui.painter().hline(egui::Rangef::new(name.left(), name.right()), name.bottom() + 2.0, egui::Stroke::new(1.0, theme::text_dim()));
        }
        let more = widgets::more_button(ui, Rect::from_center_size(pos2(rect.right() - 20.0, cy), Vec2::splat(30.0)), base.id.with("more"), "Artist options");
        widgets::menu_popup(egui::Popup::menu(&more), widgets::MENU_W, |ui| widgets::card_menu_contents(ui, c, false, acts));
        widgets::menu_popup(egui::Popup::context_menu(&resp), widgets::MENU_W, |ui| widgets::card_menu_contents(ui, c, false, acts));
        if resp.clicked() {
            acts.push(Action::Open(c.target.clone()));
        }
        ui.painter().hline(rect.x_range(), rect.bottom(), egui::Stroke::new(1.0, theme::glass(0.07)));
    });
}

/// Apple's Songs table: the column names over the account's favourites, each row 44 px
/// of artwork and name, artist, album and length.
fn songs(app: &mut App, ui: &mut Ui, now: f64) {
    lib_head(app, ui, "Songs", LibTab::Songs);
    let id = super::LIKED_MUSIC;
    if app.opened(id).is_none() {
        placeholder(app, ui, now, 1, 44.0);
        return;
    }
    let query = app.coll_search.as_ref().filter(|(fid, _)| fid == SONGS_SEARCH).map(|(_, q)| q.clone()).unwrap_or_default();
    // The rows are positions in the list YouTube sent, and they say which sort and which
    // search they answer for. Sorting a few thousand songs is not something to do on
    // every frame; this does it when one of the three changes.
    let tag = app
        .opened(id)
        .map(|o| {
            let t = o.c.tracks.as_slice();
            format!("{}|{}|{}|{}|{}", app.lib_sort.key(), t.len(), t.first().map(|t| t.id.as_str()).unwrap_or_default(), t.last().map(|t| t.id.as_str()).unwrap_or_default(), query)
        })
        .unwrap_or_default();
    if app.lib_rows.as_ref().is_none_or(|(k, _)| *k != tag) {
        let order = app.opened(id).map_or_else(Vec::new, |o| order(o.c.tracks.as_slice(), app.lib_sort, &query));
        app.lib_rows = Some((tag, order));
    }

    // The controls above the table, in a block that ends before the search field is
    // drawn: the field writes the app, and the rows below only read it.
    let mut opening = false;
    {
        let Some(o) = app.lists.get(id) else { return };
        let tracks = &o.c.tracks;
        let acts = &mut app.actions;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = vec2(10.0, 0.0);
            if !tracks.is_empty() {
                let target = Target::Playlist(id.to_owned());
                if circle_action_sized(ui, CircleGlyph::Mark(glyphs::SHUFFLE), "Shuffle", false, 34.0).clicked() {
                    acts.push(Action::PlayShuffled(tracks.to_vec()));
                }
                play_pill_h(ui, acts, &target, tracks, 34.0);
            }
            if app.logged_in {
                select_pill(ui, &mut app.selection, &app.page);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if circle_action_sized(ui, CircleGlyph::Mark(glyphs::SEARCH), "Search in these songs", !query.is_empty(), 34.0).clicked() {
                    app.coll_search = if query.is_empty() { Some((SONGS_SEARCH.to_owned(), String::new())) } else { None };
                    opening = query.is_empty();
                }
            });
        });
    }
    list_search(ui, &mut app.coll_search, SONGS_SEARCH, opening);
    // Apple's header: 11 px semibold names, dim, over the keyline the rows keep.
    let rows = Rows::new(app, now);
    let more = app.receiving(id);
    let Some(o) = app.lists.get(id) else { return };
    let tracks = &o.c.tracks;
    let Some((_, order)) = &app.lib_rows else { return };
    let acts = &mut app.actions;
    let (hrect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 27.0), Sense::hover());
    let cols = SongCols::new(hrect);
    let head = |ui: &Ui, x: f32, w: f32, label: &str, right: bool| {
        let g = widgets::galley(ui, label, theme::semibold(11.5), theme::text_dim(), w);
        let at = if right { pos2(x + w - g.size().x, hrect.center().y - g.size().y / 2.0) } else { pos2(x, hrect.center().y - g.size().y / 2.0) };
        ui.painter().galley(at, g, theme::text_dim());
    };
    head(ui, cols.name.0, cols.name.1, "Name", false);
    if let Some((x, w)) = cols.artist {
        head(ui, x, w, "Artist", false);
    }
    if let Some((x, w)) = cols.album {
        head(ui, x, w, "Album", false);
    }
    head(ui, cols.time - 52.0, 52.0, "Time", true);
    ui.painter().hline(hrect.x_range(), hrect.bottom(), egui::Stroke::new(1.0, theme::glass(0.10)));

    let selecting = app.selection.on(&app.page);
    let page = &app.page;
    let sel = &mut app.selection;
    let liked = &app.liked;
    let mods = ui.input(|i| i.modifiers);
    widgets::visible_rows_with(ui, order.len(), 44.0, |ui, pos| {
        let row = order[pos];
        let t = &tracks[row];
        let (rect, base) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::hover());
        if !ui.is_rect_visible(rect) {
            return;
        }
        let hit = Rect::from_min_max(pos2(rect.left(), rect.top()), pos2(rect.right() - 42.0, rect.bottom()));
        let resp = named(ui.interact(hit, base.id.with("play"), Sense::click()), &t.title).on_hover_cursor(egui::CursorIcon::PointingHand);
        let h = theme::anim_bool(ui.ctx(), base.id.with("h"), base.contains_pointer(), 0.15);
        let current = rows.current.as_deref() == Some(t.id.as_str());
        let picked = selecting && sel.rows.contains(&row);
        if current || h > 0.0 || picked {
            let a = if current { 0.07 + 0.05 * h } else if picked { 0.11 } else { 0.06 * h };
            ui.painter().rect_filled(rect, 8.0, if picked && !current { theme::with_alpha(rows.accent, 0.14) } else { theme::glass(a) });
        }
        let cy = rect.center().y;
        let mut x = rect.left() + 10.0;
        if selecting {
            widgets::checkbox(ui, pos2(x + 9.0, cy), sel.rows.contains(&row));
            x += 30.0;
        }
        let art = Rect::from_min_size(pos2(x, cy - 18.0), Vec2::splat(36.0));
        widgets::paint_cover(ui, art, &t.thumbs, theme::thumb_radius(), 1.0);
        if current {
            ui.painter().rect_filled(art, theme::thumb_radius(), theme::shade(0.45));
            widgets::equalizer(ui, art.shrink(9.0), now, rows.playing, Color32::WHITE);
        } else if h > 0.0 {
            ui.painter().rect_filled(art, theme::thumb_radius(), theme::shade(0.5 * h));
            widgets::play_triangle(ui, art.center(), 6.5, theme::with_alpha(Color32::WHITE, h));
        }
        // The name, the people and the release, each in its own column; a song's artists
        // and album are links, the way the site makes them.
        let cols = SongCols::new(rect);
        let who = t.artist_line();
        widgets::linked_text(ui, pos2(cols.name.0 + 44.0, cy - 9.0), &t.title, &[], theme::semibold(14.5), if current { rows.accent } else { theme::text() }, cols.name.1, acts);
        if let Some((x, w)) = cols.artist {
            widgets::linked_text(ui, pos2(x, cy - 9.0), &who, &t.artists, theme::regular(13.5), theme::text_dim(), w, acts);
        }
        if let Some((x, w)) = cols.album
            && let Some(a) = &t.album
        {
            widgets::linked_text(ui, pos2(x, cy - 9.0), &a.name, std::slice::from_ref(a), theme::regular(13.5), theme::text_dim(), w, acts);
        }
        if let Some(d) = t.duration {
            ui.painter().text(pos2(cols.time, cy), Align2::RIGHT_CENTER, super::fmt_time(d), theme::regular(13.0), theme::text_dim());
        }
        let more = widgets::more_button(ui, Rect::from_center_size(pos2(rect.right() - 20.0, cy), Vec2::splat(30.0)), base.id.with("more"), "Song options");
        widgets::menu_popup(egui::Popup::menu(&more), widgets::MENU_W, |ui| widgets::song_menu(ui, t, liked.contains(&t.id), None, None, acts));
        // Right-click anywhere on the row opens the same menu.
        widgets::menu_popup(egui::Popup::context_menu(&resp), widgets::MENU_W, |ui| widgets::song_menu(ui, t, liked.contains(&t.id), None, None, acts));
        if resp.clicked() {
            if selecting || mods.command || mods.shift {
                sel.pick_visible(page, row, mods.shift, order);
            } else {
                let queue: Vec<Track> = order.iter().map(|&i| tracks[i].clone()).collect();
                acts.push(Action::Play(queue, pos));
            }
        }
        ui.painter().hline(rect.x_range(), rect.bottom(), egui::Stroke::new(1.0, theme::glass(0.07)));
    });
    if more {
        widgets::skeleton_rows(ui, now, 3);
    }
}

/// The Songs table's column edges, shared by its header and its rows so the names sit
/// over what they name. Apple narrows the table down to the song's own name.
struct SongCols {
    /// Where the name writes, and how wide it may get: the artwork is inside it.
    name: (f32, f32),
    artist: Option<(f32, f32)>,
    album: Option<(f32, f32)>,
    /// The right edge of the Time column.
    time: f32,
}

impl SongCols {
    fn new(rect: Rect) -> Self {
        let name_x = rect.left() + 10.0 + 44.0;
        let time = rect.right() - 44.0;
        let w = (time - name_x).max(120.0);
        let (artist, album) = match w {
            w if w >= 700.0 => (Some(name_x + w * 0.40), Some(name_x + w * 0.72)),
            w if w >= 460.0 => (Some(name_x + w * 0.45), None),
            _ => (None, None),
        };
        let next = artist.unwrap_or(album.unwrap_or(time - 52.0));
        SongCols {
            name: (name_x, next - name_x - 14.0),
            artist: artist.map(|x| (x, album.unwrap_or(time - 52.0) - x - 14.0)),
            album: album.map(|x| (x, time - 52.0 - x - 14.0)),
            time,
        }
    }
}

/// The rows of a library list, as positions in the order YouTube sent them: sorting
/// moves the rows, never the songs, so picking and removing keep meaning the same row.
fn order(tracks: &[Track], sort: LibSort, query: &str) -> Vec<usize> {
    let mut order: Vec<usize> = collection_filter(tracks, query).unwrap_or_else(|| (0..tracks.len()).collect());
    // The names are compared lower-cased and without a leading "The", and only once per
    // row — the cached sort keeps what YouTube sent behind an equal pair.
    match sort {
        LibSort::Added => {}
        LibSort::Title => order.sort_by_cached_key(|&i| super::sort_key(&tracks[i].title)),
        LibSort::Artist => order.sort_by_cached_key(|&i| (super::sort_key(&tracks[i].artist_line()), super::sort_key(&tracks[i].title))),
    }
    order
}

/// What kind of release a card is, when its subtitle opens with the word. YouTube leads
/// library albums with "Album • Royal Blood • 2026"; on a page already headed Albums,
/// that word is the heading's, and Apple's caption under the title is just the artist.
fn without_type(sub: &str) -> String {
    match sub.split_once(" • ") {
        Some((kind, rest)) if matches!(kind, "Album" | "EP" | "Single") => rest.to_owned(),
        _ => sub.to_owned(),
    }
}

// ------------------------------------------------------------------ downloads

fn downloads(app: &mut App, ui: &mut Ui, now: f64) {
    page_title(ui, "Downloads");
    let songs = crate::downloads::list();
    ui.add_space(4.0);
    ui.label(
        RichText::new(format!("{} songs \u{2022} {}", songs.len(), crate::downloads::folder().display()))
            .font(theme::regular(13.0))
            .color(theme::text_dim()),
    );
    ui.add_space(12.0);
    if songs.is_empty() {
        ui.label(RichText::new("No downloads yet").font(theme::semibold(16.0)).color(theme::text()));
        ui.add_space(4.0);
        ui.label(
            RichText::new("Right-click a song and choose Download to keep it on this computer, playable offline.")
                .font(theme::regular(13.0))
                .color(theme::text_dim()),
        );
        ui.add_space(10.0);
        if pill(ui, "Open folder", Some(icon::FOLDER_OPEN), false).clicked() {
            crate::downloads::open_folder();
        }
        return;
    }
    ui.horizontal(|ui| {
        if pill(ui, "Play", Some(icon::PLAY), true).clicked() {
            app.actions.push(Action::Play(songs.clone(), 0));
        }
        if pill(ui, "Shuffle", Some(icon::SHUFFLE), false).clicked() {
            app.actions.push(Action::PlayShuffled(songs.clone()));
        }
        if pill(ui, "Open folder", Some(icon::FOLDER_OPEN), false).clicked() {
            crate::downloads::open_folder();
        }
    });
    ui.add_space(12.0);
    let rows = Rows::new(app, now);
    list(ui, &rows, &app.liked, &songs, 0, &songs, true, false, |_, _| None, ListCtx { sel: &mut app.selection, page: &app.page, collection: None, indices: None }, &mut app.actions);
}

// ------------------------------------------------------------------ settings

/// What the box above Settings is looking for: the words typed in it, the group being
/// drawn and whether any of the page was left standing.
#[derive(Clone, Default)]
struct SettingsSearch {
    /// Lowercased words; a row is in when every one of them is in it.
    words: Vec<String>,
    /// The heading of the group whose rows are being drawn.
    section: String,
    /// Draw the group's heading and card at all.
    shown: bool,
    /// Draw every row of the group: nothing is being looked for, or its heading matched.
    all: bool,
    /// Some group survived, so the page isn't empty.
    matched: bool,
}

/// Every row Settings drew while the box was empty — the list the box searches, since a
/// filtered page no longer draws the rows it hides.
#[derive(Clone, Default)]
struct SettingsRows(Vec<(String, String, String)>);

fn search_key() -> egui::Id {
    egui::Id::new("bitchord-settings-search")
}

fn rows_key() -> egui::Id {
    egui::Id::new("bitchord-settings-rows")
}

fn search_of(ui: &Ui) -> SettingsSearch {
    ui.data(|d| d.get_temp::<SettingsSearch>(search_key())).unwrap_or_default()
}

fn put_search(ui: &Ui, s: &SettingsSearch) {
    ui.data_mut(|d| d.insert_temp(search_key(), s.clone()));
}

/// Every word has to be in the text, in any order.
pub(super) fn settings_hit(text: &str, words: &[String]) -> bool {
    let text = text.to_lowercase();
    words.iter().all(|w| text.contains(w))
}

/// Whether the group being drawn survives the box at all — for the few notes Settings
/// draws that aren't rows.
fn settings_group_shown(ui: &Ui) -> bool {
    let s = search_of(ui);
    s.words.is_empty() || s.shown
}

/// Called by every row of Settings, `setting_row` and the hand-drawn ones alike: whether
/// to draw it, and what the box has to search through while the page is drawn whole.
pub(super) fn settings_row_shown(ui: &mut Ui, title: &str, desc: &str) -> bool {
    let s = search_of(ui);
    if s.words.is_empty() {
        let section = s.section.clone();
        ui.data_mut(|d| {
            let mut rows = d.get_temp::<SettingsRows>(rows_key()).unwrap_or_default();
            rows.0.push((section, title.to_owned(), desc.to_owned()));
            d.insert_temp(rows_key(), rows);
        });
        return true;
    }
    s.all || settings_hit(&format!("{title} {desc}"), &s.words)
}

fn settings(app: &mut App, ui: &mut Ui) {
    let words: Vec<String> = app.settings_search.split_whitespace().map(str::to_lowercase).collect();
    // The page drawn whole is what the box searches; a filtered one leaves that list alone.
    if words.is_empty() {
        ui.data_mut(|d| d.insert_temp(rows_key(), SettingsRows::default()));
    }
    put_search(ui, &SettingsSearch { words, ..Default::default() });
    // One centred column; every section shares its width.
    let avail = ui.available_width();
    let w = avail.min(920.0);
    ui.horizontal(|ui| {
        ui.add_space(((avail - w) / 2.0).max(0.0));
        ui.vertical(|ui| {
            ui.set_width(w);
            settings_body(app, ui);
            let s = search_of(ui);
            if !s.words.is_empty() && !s.matched {
                ui.add_space(14.0);
                ui.label(egui::RichText::new(format!("Nothing in Settings matches “{}”.", app.settings_search.trim())).color(theme::text_dim()));
            }
            ui.add_space(40.0);
        });
    });
    // Only Settings is filtered; the rest of the app draws every row it has.
    put_search(ui, &SettingsSearch::default());
}

fn settings_body(app: &mut App, ui: &mut Ui) {
    // A room opened from Listen together takes the column to itself, the way the site's
    // settings drill in.
    if app.jam_open {
        if super::jam::subpage(app, ui) {
            app.jam_open = false;
        }
        ui.add_space(40.0);
        return;
    }
    page_title(ui, "Settings");
    settings_search_bar(ui, &mut app.settings_search);
    settings_account(app, ui);
    settings_playback(app, ui);
    settings_hires(app, ui);
    settings_listen(app, ui);
    settings_downloads(ui);
    settings_lyrics(app, ui);
    settings_artists(app, ui);
    settings_appearance(app, ui);
    settings_player(app, ui);
    settings_window(app, ui);
    settings_connections(app, ui);
    section_label(ui, "Network", None);
    settings_card(ui, |ui| network(app, ui));
    super::transfer::settings_section(app, ui);
    settings_shortcuts(app, ui);
}

/// The box above Settings: what's typed in it leaves only the sections and rows that say
/// it, and the page keeps its shape while it does.
fn settings_search_bar(ui: &mut Ui, query: &mut String) {
    ui.add_space(10.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width().min(420.0), 40.0), Sense::hover());
    ui.painter().rect_filled(rect, 20.0, theme::glass(0.09));
    widgets::icon_at(ui, pos2(rect.left() + 20.0, rect.center().y), icon::SEARCH, 16.0, theme::text_dim());
    let clearable = !query.is_empty();
    let inner = Rect::from_min_max(pos2(rect.left() + 40.0, rect.top() + 4.0), pos2(rect.right() - if clearable { 40.0 } else { 16.0 }, rect.bottom() - 4.0));
    ui.put(
        inner,
        egui::TextEdit::singleline(query)
            .hint_text(egui::RichText::new("Search settings").color(theme::text_faint()))
            .font(theme::regular(14.0))
            .vertical_align(egui::Align::Center)
            .frame(egui::Frame::NONE)
            .desired_width(inner.width()),
    );
    if clearable {
        let x = Rect::from_center_size(pos2(rect.right() - 20.0, rect.center().y), Vec2::splat(26.0));
        let resp = ui.interact(x, ui.id().with("settings-search-clear"), Sense::click());
        widgets::icon_at(ui, x.center(), icon::X, 14.0, theme::text_dim());
        if resp.clicked() {
            query.clear();
        }
    }
    ui.add_space(4.0);
}
/// Settings › Listen together: the room's door, as its own section.
fn settings_listen(app: &mut App, ui: &mut Ui) {
    section_label(ui, "Listen together", None);
    settings_card(ui, |ui| {
        if open_row(ui, "Listen together", &format!("Play the same music at the same moment, on up to {} devices.", crate::jam::DEFAULT_MEMBERS)) {
            app.jam_open = true;
        }
    });
}

/// Settings › Account: signing in and out, and which channel to use.
fn settings_account(app: &mut App, ui: &mut Ui) {
    section_label(ui, "Account", None);
    let busy = app.signing_in.load(Ordering::Relaxed);
    let (mut sign_in, mut sign_out) = (false, false);
    // Asked again now and then while Settings shows: a failed ask, or a channel made since.
    let now = ui.input(|i| i.time);
    if app.logged_in && app.channels_asked.is_none_or(|at| now - at > 60.0) {
        app.channels_asked = Some(now);
        app.backend.request(crate::backend::Req::Channels);
    }
    settings_card(ui, |ui| {
        if app.logged_in {
            let name = app.account_name.clone().unwrap_or_else(|| "YouTube Music".to_owned());
            setting_row(ui, &name, Some("Signed in with Google. Your home feed, library and likes come from this account. Saved encrypted in Windows Credential Manager."), |ui| {
                sign_out = pill(ui, "Sign out", Some(icon::LOG_OUT), false).clicked();
            });
            // Also with one, when the one in use isn't it (a brand channel since deleted).
            if app.channels.as_ref().is_some_and(|c| c.list.len() > 1 || !c.list.iter().any(|l| l.page_id == c.current)) {
                divider(ui);
                setting_row(ui, "Channel", Some("Your Google account and each YouTube channel it manages have their own library, likes, history and recommendations."), |ui| {
                    channel_picker(app, ui);
                });
            }
        } else {
            setting_row(ui, "Not signed in", Some("Sign in to get your home feed, library and likes."), |ui| {
                if busy {
                    ui.spinner();
                } else {
                    sign_in = pill(ui, "Sign in with Google", Some(icon::CIRCLE_USER), true).clicked();
                }
            });
        }
    });
    if busy {
        ui.add_space(6.0);
        ui.label(RichText::new("Finish signing in through the window that opened.").font(theme::regular(12.5)).color(theme::text_dim()));
    }
    if sign_in {
        app.start_sign_in(ui.ctx());
    }
    if sign_out {
        app.backend.request(crate::backend::Req::Logout);
    }
}

fn channel_picker(app: &mut App, ui: &mut Ui) {
    let Some(crate::backend::Channels { list, current, epoch }) = &app.channels else { return };
    // A brand channel often has the account's name: the handle, or "No channel" (as
    // YouTube's switcher says), tells them apart.
    let label = |c: &crate::ytm::Channel| match (&c.handle, c.has_channel) {
        (Some(h), _) => format!("{} ({h})", c.name),
        (None, false) => format!("{} (No channel)", c.name),
        (None, true) => c.name.clone(),
    };
    // The button is narrower: a handle alone is enough there.
    let shown = match list.iter().find(|c| c.page_id == *current) {
        Some(c) => c.handle.clone().unwrap_or_else(|| label(c)),
        None => "Another channel".to_owned(),
    };
    // A transfer would go on as the new channel.
    if app.transfer.running() {
        ui.label(RichText::new("Not while a transfer runs").font(theme::regular(13.0)).color(theme::text_dim()));
        return;
    }
    let switching = app.switching_to.is_some();
    let btn = dropdown_button(ui, if switching { "Switching…" } else { &shown }, 220.0, "Channel");
    let mut pick = None;
    egui::Popup::menu(&btn).show(|ui| {
        ui.set_width(300.0);
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            ui.add_space(4.0);
            for c in list {
                if option_row(ui, &label(c), c.page_id == *current).clicked() {
                    pick = Some(c.clone());
                    ui.close();
                }
            }
            ui.add_space(4.0);
        });
    });
    if let Some(c) = pick.filter(|c| c.page_id != *current && !switching) {
        app.switching_to = Some(label(&c));
        app.backend.request(crate::backend::Req::SwitchChannel { page_id: c.page_id, epoch: *epoch });
    }
}

/// Settings › Playback.
fn settings_playback(app: &mut App, ui: &mut Ui) {
    section_label(ui, "Playback", None);
    settings_card(ui, |ui| {
        setting_row(ui, "Autoplay", Some("Keep playing similar songs when the queue ends."), |ui| {
            toggle(ui, &mut app.autoplay, "Autoplay");
        });
        divider(ui);
        setting_row(ui, "Volume normalization", Some("Turn loud songs down to the same level, like YouTube Music does."), |ui| {
            if toggle(ui, &mut app.normalize, "Volume normalization").changed() {
                app.audio.set_normalize(app.normalize);
            }
        });
        divider(ui);
        setting_row(ui, "Logarithmic volume", Some("Space the volume slider out the way ears hear loudness, so quiet levels aren't squeezed into its first few pixels."), |ui| {
            if toggle(ui, &mut app.log_volume, "Logarithmic volume").changed() {
                app.audio.set_log_volume(app.log_volume);
            }
        });
        divider(ui);
        setting_row(ui, "Play songs, not music videos", Some("An official music video plays as its song, the album recording, when YouTube Music has one: no intros, skits or video-only edits, and the radio keeps videos and podcast episodes out of the queue it fills for you. Lists still show and play what they hold."), |ui| {
            toggle(ui, &mut app.play_songs, "Play songs, not music videos");
        });
        divider(ui);
        setting_row(ui, "No AI songs", Some("A song whose own name says AI made it — an AI cover, an AI-generated release — is skipped rather than played, and the queue moves on to the next one. The label is read from the words of the song, its artists or its release, and from YouTube's own \"Made with AI\" note on the video, which a clean title can still carry."), |ui| {
            // Switching it on mid-song moves on at once: a listener who just asked for
            // no AI songs shouldn't have to sit through the one that's playing.
            if toggle(ui, &mut app.skip_ai, "No AI songs").changed() && app.skip_ai && app.shown().is_some_and(|t| t.ai_labelled() || app.ai_made(&t.id)) {
                app.next();
            }
        });
        divider(ui);
        setting_row(ui, "Output device", Some("Where the music plays. If the chosen device isn't connected, BitChord uses the Windows default until the device is back."), |ui| {
            output_device_picker(app, ui);
        });
        divider(ui);
        setting_row(ui, "Automix", Some("Slide from one song into the next the way a DJ would: both hold their level for a moment, then swap in one smooth move, instead of a straight fade. The mix picks its own length, in phrases of 8 or 16 bars (16 on long songs), following the beat it hears."), |ui| {
            if toggle(ui, &mut app.automix, "Automix").changed() {
                app.set_crossfade();
            }
        });
        if !app.automix {
            divider(ui);
            setting_row(ui, "Crossfade songs", Some("Blend the end of each song into the start of the next."), |ui| {
                if toggle(ui, &mut app.crossfade, "Crossfade songs").changed() {
                    app.set_crossfade();
                }
            });
            if app.crossfade {
                divider(ui);
                setting_row(ui, "Crossfade length", None, |ui| {
                    if seconds_slider(ui, &mut app.crossfade_secs, 1..=super::CROSSFADE_MAX, "Crossfade length").changed() {
                        app.set_crossfade();
                    }
                });
            }
        }
        divider(ui);
        setting_row(ui, "Skip non-music parts", Some("Jump over talking, skits and sponsor breaks in music videos, as marked by the SponsorBlock community. Album songs don't have any."), |ui| {
            if toggle(ui, &mut app.sponsorblock, "Skip non-music parts").changed() {
                app.request_segments();
            }
        });
    });
    settings_equalizer(app, ui);
}

/// Settings › Playback › Equalizer: ten bands, presets, and the curve they make drawn
/// behind the sliders. Shapes only what you hear: not downloads, or what others in a jam
/// hear.
fn settings_equalizer(app: &mut App, ui: &mut Ui) {
    use crate::audio::eq;
    ui.add_space(6.0);
    section_label(ui, "Equalizer", None);
    ui.add_space(4.0);
    settings_card(ui, |ui| {
        setting_row(ui, "Equalizer", Some("Shape the sound from deep bass to treble, on everything BitChord plays. Only what you hear changes: not downloads, or what others in a jam hear."), |ui| {
            if toggle(ui, &mut app.eq_on, "Equalizer").changed() {
                app.set_eq();
            }
        });
        ui.add_enabled_ui(app.eq_on, |ui| {
            divider(ui);
            setting_row(ui, "Preset", None, |ui| {
                let btn = dropdown_button(ui, eq::preset_name(&app.eq_gains).unwrap_or("Custom"), 180.0, "Equalizer preset");
                let mut pick = None;
                egui::Popup::menu(&btn).show(|ui| {
                    ui.set_width(200.0);
                    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                    ui.add_space(4.0);
                    for (name, gains) in &eq::PRESETS {
                        if option_row(ui, name, app.eq_gains == *gains).clicked() {
                            pick = Some(*gains);
                            ui.close();
                        }
                    }
                    ui.add_space(4.0);
                });
                if let Some(g) = pick {
                    app.eq_gains = g;
                    app.set_eq();
                }
            });
            divider(ui);
            if eq_bands(ui, &mut app.eq_gains) {
                app.set_eq();
            }
            divider(ui);
            setting_row(ui, "Prevent clipping", Some("Turn everything down by as much as the bands boost, so loud songs are less likely to crackle."), |ui| {
                if toggle(ui, &mut app.eq_no_clip, "Prevent clipping").changed() {
                    app.set_eq();
                }
            });
        });
    });
    ui.add_space(6.0);
}

/// The equalizer's bands as vertical sliders, over the curve they make together.
/// Drag or click to set one, or double-click for 0 dB. True if one changed.
fn eq_bands(ui: &mut Ui, gains: &mut crate::audio::eq::Gains) -> bool {
    use crate::audio::eq::{BANDS, MAX_DB, response};
    const LABELS: [&str; 10] = ["31", "62", "125", "250", "500", "1k", "2k", "4k", "8k", "16k"];
    const NAMES: [&str; 10] = ["31 Hz", "62 Hz", "125 Hz", "250 Hz", "500 Hz", "1 kHz", "2 kHz", "4 kHz", "8 kHz", "16 kHz"];
    const GUTTER: f32 = 34.0;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 210.0), Sense::hover());
    let fade = if ui.is_enabled() { 1.0 } else { 0.45 };
    let accent = ui.ctx().data(|d| d.get_temp::<Color32>(egui::Id::unique("accent"))).unwrap_or(theme::RED);
    let p = ui.painter();
    let top = rect.top() + 26.0;
    let bottom = rect.bottom() - 30.0;
    let mid = (top + bottom) / 2.0;
    let y_of = |db: f32| mid - db / MAX_DB * (bottom - top) / 2.0;
    let col_w = (rect.width() - GUTTER) / BANDS.len() as f32;
    let x_of = |i: f32| rect.left() + GUTTER + col_w * (i + 0.5);
    for (db, text) in [(MAX_DB, "+12"), (0.0, "0"), (-MAX_DB, "\u{2212}12")] {
        p.text(pos2(rect.left(), y_of(db)), Align2::LEFT_CENTER, text, theme::regular(11.5), theme::with_alpha(theme::text_dim(), fade));
    }
    p.line_segment([pos2(x_of(-0.5), mid), pos2(x_of(9.5), mid)], egui::Stroke::new(1.0, theme::glass(0.10 * fade)));
    // Bands are an octave apart, so evenly spaced columns sit on a log frequency axis.
    // The curve can go a little past ±12 between bands; it runs off the top rather than
    // flattening there and hiding it. It's drawn from the first band to the last: past
    // them it only shows the filters' tails, which the sliders don't set.
    let db_at = response(gains);
    let curve: Vec<egui::Pos2> = (0..=180)
        .map(|k| {
            let i = 9.0 * k as f32 / 180.0;
            pos2(x_of(i), y_of(db_at(BANDS[0] * 2f32.powf(i))))
        })
        .collect();
    ui.painter_at(rect).add(egui::Shape::line(curve, egui::Stroke::new(2.0, theme::with_alpha(accent, 0.55 * fade))));
    let mut changed = false;
    for (i, g) in gains.iter_mut().enumerate() {
        let x = x_of(i as f32);
        // Just the track and a knob's reach past its ends, so clicking a label doesn't
        // send the band to the bottom.
        let col = Rect::from_min_max(pos2(x - col_w / 2.0, top - 8.0), pos2(x + col_w / 2.0, bottom + 8.0));
        let mut resp = ui.interact(col, ui.scope_id().with(("eq-band", i)), Sense::click_and_drag());
        let name = NAMES[i];
        let old = *g;
        if resp.double_clicked() {
            *g = 0.0;
        } else if let Some(pos) = resp.interact_pointer_pos()
            && (resp.dragged() || resp.clicked())
        {
            *g = (((mid - pos.y) / ((bottom - top) / 2.0) * MAX_DB * 2.0).round() / 2.0).clamp(-MAX_DB, MAX_DB);
        }
        if *g != old {
            changed = true;
            resp.mark_changed();
        }
        let value = *g;
        resp.widget_info(|| egui::WidgetInfo::slider(ui.is_enabled(), value as f64, name));
        let resp = resp.on_hover_cursor(egui::CursorIcon::ResizeVertical);
        let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered() || resp.dragged() || resp.has_focus(), 0.12);
        let track = Rect::from_center_size(pos2(x, mid), vec2(4.0, bottom - top));
        p.rect_filled(track, 2.0, theme::glass(0.15 * fade));
        let knob = pos2(x, y_of(value));
        p.rect_filled(Rect::from_two_pos(pos2(x - 2.0, mid), pos2(x + 2.0, knob.y)), 2.0, theme::with_alpha(accent, fade));
        p.circle_filled(knob + vec2(0.0, 1.0), 7.0 + h, theme::shade(0.25 * fade));
        p.circle_filled(knob, 7.0 + h, theme::with_alpha(Color32::WHITE, fade));
        if h > 0.0 {
            let text = match value {
                0.0 => "0 dB".to_owned(),
                v if v > 0.0 => format!("+{v} dB"),
                v => format!("\u{2212}{} dB", -v),
            };
            p.text(pos2(x, rect.top() + 9.0), Align2::CENTER_CENTER, text, theme::semibold(12.0), theme::with_alpha(theme::text(), h * fade));
        }
        p.text(pos2(x, rect.bottom() - 12.0), Align2::CENTER_CENTER, LABELS[i], theme::regular(12.0), theme::with_alpha(theme::text_dim(), fade));
    }
    changed
}

/// Settings › Hi-res audio: the addon sources that carry lossless copies, and how the
/// player waits for them.
fn settings_hires(app: &mut App, ui: &mut Ui) {
    section_label(ui, "Hi-res audio", None);
    settings_card(ui, |ui| {
        ui.add_space(12.0);
        hires_sources_editor(app, ui);
        ui.add_space(10.0);
        setting_row(ui, "Prefer Dolby Atmos", Some("Ask the hi-res sources for a Dolby Atmos copy when they offer one. It falls back to the best stereo copy when the stream's codec can't be decoded. Bit-perfect output always takes the stereo copy."), |ui| {
            if toggle(ui, &mut app.prefer_atmos, "Prefer Dolby Atmos").changed() {
                app.backend.set_prefer_atmos(app.prefer_atmos && !app.bit_perfect);
            }
        });
        divider(ui);
        setting_row(ui, "Disable direct source stream", Some("Play YouTube audio immediately and upgrade to hi-res in the background, instead of waiting for the addon to answer first."), |ui| {
            if toggle(ui, &mut app.disable_direct_stream, "Disable direct source stream").changed() {
                app.backend.set_disable_direct_stream(app.disable_direct_stream);
            }
        });
        if cfg!(windows) {
            divider(ui);
            setting_row(ui, "Bit-perfect output", Some("Send lossless tracks straight to the selected DAC in WASAPI exclusive mode: no Windows mixer, volume, EQ, normalization, crossfade or Automix. YouTube's Opus/AAC stay on the normal path, and volume belongs to the DAC."), |ui| {
                let mut on = app.bit_perfect;
                if toggle(ui, &mut on, "Bit-perfect output").changed() {
                    app.actions.push(Action::BitPerfect(on));
                }
            });
            if app.bit_perfect {
                let active = app.audio.status.bit_perfect.load(Ordering::Relaxed);
                let rate = app.audio.status.bit_perfect_rate.load(Ordering::Relaxed);
                let bits = app.audio.status.bit_perfect_bits.load(Ordering::Relaxed);
                let status = if active { format!("Active \u{2014} {rate} Hz, {bits}-bit") } else { "Waiting for a lossless track".to_owned() };
                ui.add_space(2.0);
                ui.label(RichText::new(status).size(12.0).color(theme::text_dim()));
                ui.add_space(6.0);
            }
        }
    });
}

/// The hi-res source list: one line per addon, and "+ Add a source" below it that
/// opens a fresh line for the next one. Each settled link is tested against the addon
/// and its verdict shown underneath, the way the old BitChord did.
fn hires_sources_editor(app: &mut App, ui: &mut Ui) {
    let mut edited = false;
    let mut remove = None;
    for i in 0..app.hires_sources.len() {
        let link = app.hires_sources[i].trim().to_owned();
        // Asked once the field is settled: testing on every keystroke would fire a
        // search and a stream at the addon for each character typed.
        let verdict = if link.is_empty() { None } else { app.hires_check(&link).cloned() };
        // Fixed-height row so the × sits at the same height on every line, whatever
        // verdict text appears; the verdict goes below the row, not inside it.
        ui.allocate_ui_with_layout(vec2(ui.available_width(), 42.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::icon_button(ui, icon::X, 14.0, "Remove source", false).clicked() {
                remove = Some(i);
            }
            ui.add_space(8.0);
            if super::text_field(ui, "", &mut app.hires_sources[i], false, false, "https://your-addon.example").changed() {
                edited = true;
            }
        });
        let text = match &verdict {
            Some(Ok(summary)) => Some((summary.clone(), theme::text_dim())),
            Some(Err(reason)) => Some((reason.clone(), theme::RED)),
            None if !link.is_empty() => Some(("Testing the link…".to_owned(), theme::text_faint())),
            None => None,
        };
        if let Some((text, colour)) = text {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(text).font(theme::regular(11.5)).color(colour));
        }
        ui.add_space(6.0);
    }
    if let Some(i) = remove {
        app.hires_sources.remove(i);
        edited = true;
    }
    if edited {
        app.hires_sources = super::dedupe_hires(app.hires_sources.iter().map(String::as_str));
        app.backend.set_hires_source(&live_sources(app));
    }
    if app.hires_adding {
        let resp = super::text_field(ui, "", &mut app.hires_draft, false, false, "https://your-addon.example");
        if app.hires_focus {
            resp.request_focus();
            app.hires_focus = false;
        }
        // Enter commits without needing to click away; clicking elsewhere does too.
        let enter = resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if resp.lost_focus() || enter {
            let link = app.hires_draft.trim().to_owned();
            if !link.is_empty() {
                app.hires_sources.push(link);
                app.hires_sources = super::dedupe_hires(app.hires_sources.iter().map(String::as_str));
                app.backend.set_hires_source(&live_sources(app));
            }
            app.hires_draft.clear();
            app.hires_adding = false;
        }
        ui.add_space(6.0);
    } else if pill(ui, "+  Add a source", None, false).clicked() {
        app.hires_adding = true;
        app.hires_focus = true;
        app.hires_draft.clear();
    }
}

/// The non-empty links as one newline-separated list, the way the backend reads them.
fn live_sources(app: &App) -> String {
    app.hires_sources.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n")
}

/// Settings › Downloads.
fn settings_downloads(ui: &mut Ui) {
    section_label(ui, "Downloads", None);
    if settings_group_shown(ui) {
        ui.label(
            RichText::new("Download songs from their menu. They're saved as the best copy your sources have, tagged with their cover art when that's an .m4a, and play from the file, even offline.")
                .font(theme::regular(13.0))
                .color(theme::text_dim()),
        );
        ui.add_space(12.0);
    }
    settings_card(ui, |ui| {
        let folder = crate::downloads::folder().display().to_string();
        setting_row(ui, "Download folder", Some(&folder), |ui| {
            if pill(ui, "Change", None, false).clicked() {
                crate::downloads::pick_folder(ui.ctx());
            }
            ui.add_space(6.0);
            if pill(ui, "Open", Some(icon::FOLDER_OPEN), false).clicked() {
                crate::downloads::open_folder();
            }
        });
    });
}

/// Settings › Lyrics: the better-lyrics extension's options.
fn settings_lyrics(app: &mut App, ui: &mut Ui) {
    section_label(ui, "Lyrics", None);
    settings_card(ui, |ui| {
        setting_row(ui, "Romanization", Some("Japanese, Korean, Chinese, Cyrillic and other scripts in Latin letters. Timed with the lyrics when the source has it."), |ui| {
            toggle(ui, &mut app.show_roman, "Romanization");
        });
        divider(ui);
        setting_row(ui, "Translation", Some("Translated by Unison, or Google when it can't. Lyrics that ship their own translation use it."), |ui| {
            toggle(ui, &mut app.show_trans, "Translation");
        });
        divider(ui);
        setting_row(ui, "Translate into", None, |ui| {
            let current = super::lyric_prefs::LANGUAGES.iter().find(|(c, _)| *c == app.lyric_prefs.target).map_or(app.lyric_prefs.target.as_str(), |(_, n)| n);
            let btn = dropdown_button(ui, current, 200.0, "Translate into");
            let just_opened = btn.clicked();
            egui::Popup::menu(&btn).show(|ui| {
                ui.set_width(200.0);
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    ui.add_space(4.0);
                    for (code, name) in super::lyric_prefs::LANGUAGES {
                        let on = app.lyric_prefs.target == *code;
                        let row = option_row(ui, name, on);
                        if on && just_opened {
                            row.scroll_to_me(Some(egui::Align::Center));
                        }
                        if row.clicked() {
                            app.lyric_prefs.target = (*code).to_owned();
                            ui.close();
                        }
                    }
                    ui.add_space(4.0);
                });
            });
        });
        divider(ui);
        setting_row(ui, "Text size", Some("Make the lyrics larger or smaller. Works on top of the theme's own size."), |ui| {
            let (min, max, step) = super::lyric_prefs::TEXT_SCALE;
            let v = &mut app.lyric_prefs.text_scale;
            if ui.add_enabled(*v < max - 0.01, egui::Button::new("+").min_size(vec2(28.0, 26.0))).on_hover_text("Larger").clicked() {
                *v = ((*v + step) * 10.0).round() / 10.0;
            }
            ui.label(egui::RichText::new(format!("{:.0}%", *v * 100.0)).font(theme::semibold(13.0)));
            if ui.add_enabled(*v > min + 0.01, egui::Button::new("−").min_size(vec2(28.0, 26.0))).on_hover_text("Smaller").clicked() {
                *v = ((*v - step) * 10.0).round() / 10.0;
            }
        });
        divider(ui);
        // Offsets: seconds taken off the lyrics clock, as the extension's dock sets them.
        for (title, desc, which) in [
            ("Timing offset", "Shift all lyrics later (+) or earlier (−).", 0),
            ("Word-synced trim", "Extra shift for syllable and word-synced lyrics.", 1),
            ("Line-synced trim", "Extra shift for line-synced lyrics.", 2),
        ] {
            setting_row(ui, title, Some(desc), |ui| {
                let v = match which {
                    0 => &mut app.lyric_prefs.global_offset,
                    1 => &mut app.lyric_prefs.word_trim,
                    _ => &mut app.lyric_prefs.line_trim,
                };
                if ui.add(egui::Button::new("+").min_size(vec2(28.0, 26.0))).on_hover_text("Later").clicked() {
                    *v = ((*v + 0.1) * 10.0).round() / 10.0;
                }
                ui.label(egui::RichText::new(format!("{:+.1} s", *v).replace("+0.0", "0.0")).font(theme::semibold(13.0)));
                if ui.add(egui::Button::new("−").min_size(vec2(28.0, 26.0))).on_hover_text("Earlier").clicked() {
                    *v = ((*v - 0.1) * 10.0).round() / 10.0;
                }
            });
            divider(ui);
        }
        // Source order: the first enabled source with lyrics for a song is the one shown.
        setting_row(ui, "Lyrics sources", Some("Enabled sources are shown in this order."), |_| {});
        let n = app.lyric_prefs.order.len();
        let mut swap: Option<(usize, usize)> = None;
        for i in 0..n {
            let (p, on) = app.lyric_prefs.order[i];
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
            let mut row = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
            let mut enabled = on;
            if toggle(&mut row, &mut enabled, p.name()).changed() {
                app.lyric_prefs.order[i].1 = enabled;
                if p == crate::lyrics::Provider::GeniusPlain && let Some(track) = app.shown().cloned() {
                    app.lyrics = None;
                    app.lyrics_extras = None;
                    app.extras_asked = None;
                    app.lyric_view.reset();
                    app.sidebar_lyric_view.reset();
                    app.request_lyrics(track);
                }
            }
            row.add_space(10.0);
            let kind = match p.sync() {
                crate::lyrics::SyncKind::Syllable => "syllable",
                crate::lyrics::SyncKind::Word => "word",
                crate::lyrics::SyncKind::Line => "line",
                crate::lyrics::SyncKind::Unsynced => "plain",
            };
            row.label(egui::RichText::new(p.name()).font(theme::semibold(13.5)).color(if on { theme::text() } else { theme::text_dim() }));
            row.label(egui::RichText::new(kind).font(theme::regular(12.0)).color(theme::text_dim()));
            row.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled(i + 1 < n, egui::Button::new("↓").min_size(vec2(28.0, 24.0))).on_hover_text("Lower").clicked() {
                    swap = Some((i, i + 1));
                }
                if ui.add_enabled(i > 0, egui::Button::new("↑").min_size(vec2(28.0, 24.0))).on_hover_text("Higher").clicked() {
                    swap = Some((i, i - 1));
                }
            });
        }
        if let Some((a, b)) = swap {
            app.lyric_prefs.order.swap(a, b);
        }
        ui.add_space(6.0);
    });
}

/// Settings › Appearance › Signature: the listener's own signature service.
fn settings_artists(app: &mut App, ui: &mut Ui) {
    section_label(ui, "Signature", None);
    settings_card(ui, |ui| {
        setting_row(ui, "Signature service", Some("A link to your own signature API. Artists get their handwritten signature, with “Signed by …” above it — beside About when Apple also letters them a wordmark; without a signature, their name is set in type."), |_| {});
        divider(ui);
        ui.add_space(12.0);
        if super::text_field(ui, "Link", &mut app.signature_api, false, false, "https://your-signature-service.example").changed() {
            app.signature_reset();
        }
        ui.add_space(12.0);
    });
}

/// Settings › Appearance.
fn settings_appearance(app: &mut App, ui: &mut Ui) {
    section_label(ui, "Appearance", None);
    settings_card(ui, |ui| {
        let desc = if cfg!(windows) {
            "Light or dark pages. System follows Windows' app mode. Now playing stays dark, over the album art."
        } else {
            "Light or dark pages. System follows your system's setting. Now playing stays dark, over the album art."
        };
        let mut pick = None;
        setting_row(ui, "Mode", Some(desc), |ui| {
            let btn = dropdown_button(ui, app.appearance.label(), 160.0, "Mode");
            egui::Popup::menu(&btn).show(|ui| {
                ui.set_width(160.0);
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                ui.add_space(4.0);
                for a in theme::Appearance::ALL {
                    if option_row(ui, a.label(), a == app.appearance).clicked() {
                        pick = Some(a);
                        ui.close();
                    }
                }
                ui.add_space(4.0);
            });
        });
        if let Some(a) = pick {
            app.appearance = a;
        }
        divider(ui);
        setting_row(ui, "Blobs on pages", Some("Show the animated album-art background behind browse pages."), |ui| {
            toggle(ui, &mut app.page_blobs, "Blobs on pages");
        });
        divider(ui);
        setting_row(ui, "Colour icons to the song", Some("The marks — play, shuffle, repeat, the progress bar — take the sleeve's colour. Off keeps the app's own."), |ui| {
            toggle(ui, &mut app.icon_tint, "Colour icons to the song");
        });
        divider(ui);
        setting_row(ui, "Power saver / Disable background blur", Some("Replaces the GPU shader blur behind pages with a flat ambient tint. Much lighter on an integrated GPU: less heat, less battery, a lower fan."), |ui| {
            toggle(ui, &mut app.disable_blur, "Power saver / Disable background blur");
        });
        divider(ui);
        setting_row(ui, "Font", Some("For the whole app and the lyrics. Any font installed on this PC works, e.g. one made for your language."), |ui| {
            if let Some(name) = font_picker(ui) {
                theme::set_font_choice(&name);
                app.chrome_applied = None;
            }
        });
        divider(ui);
        let hidden = app.hidden_playlists.len();
        let desc = match hidden {
            0 => "Nothing hidden. Right-click a playlist in the sidebar to hide it from there.".to_owned(),
            1 => "1 playlist is hidden from the sidebar. Right-click a playlist there to hide or show it.".to_owned(),
            n => format!("{} playlists are hidden from the sidebar. Right-click a playlist there to hide or show it.", thousands(n as u64)),
        };
        setting_row(ui, "Hidden playlists", Some(&desc), |ui| {
            if hidden > 0 && pill(ui, "Show all", None, false).clicked() {
                app.hidden_playlists.clear();
            }
        });
    });
}

/// Settings › Player appearance: what the sleeve does, how loud the bar is, and the
/// lyric sheet. The rows that change the player rather than the app's pages.
fn settings_player(app: &mut App, ui: &mut Ui) {
    section_label(ui, "Player appearance", None);
    settings_card(ui, |ui| {
        setting_row(ui, "HD music videos", Some("Ask YouTube Music for the music video at up to 1080p rather than 720p. The clip comes down whole before the first picture shows, so the higher picture is a longer wait and a bigger file in the cache."), |ui| {
            toggle(ui, &mut app.video_hd, "HD music videos");
        });
        divider(ui);
        setting_row(ui, "Music video backdrop", Some("The song's video stands behind the player as soon as it opens, rather than only when the V key is pressed. The key still works either way."), |ui| {
            if toggle(ui, &mut app.video_bg, "Music video backdrop").changed() {
                app.video_on = app.video_bg;
            }
        });
        divider(ui);
        setting_row(ui, "Volume bar", Some("Show the volume slider under the transport and beside the player bar. Off gives the room back on small screens."), |ui| {
            toggle(ui, &mut app.show_volume, "Volume bar");
        });
        divider(ui);
        setting_row(ui, "Animated artwork", Some("Apple's moving album covers in Now Playing, the player and the tiles on Home. Costs memory while a song plays, and gives it back when the song ends."), |ui| {
            if toggle(ui, &mut app.animated_artwork, "Animated artwork").changed() && !app.animated_artwork {
                app.deload_motion();
            }
        });
        divider(ui);
        setting_row(ui, "Show past lyric lines", Some("Keep the lines already sung above the one being sung, the way am-lyrics shows them. Off, only the current line and what's next stay on the sheet."), |ui| {
            toggle(ui, &mut app.show_past_lines, "Show past lyric lines");
        });
        divider(ui);
        setting_row(ui, "Best Part button", Some("A button on the artwork that skips to the part of the song people replay most. It stays on the sleeve until that part plays, and this switch is the way to have it at all. Asks YouTube for each song's replay curve once and keeps the answer."), |ui| {
            if toggle(ui, &mut app.show_best_part, "Best Part button").changed() && app.show_best_part {
                app.request_best_part();
                app.update_best_part();
            }
        });
        divider(ui);
        setting_row(ui, "Animated background", Some("The album art slowly drifts behind everything."), |ui| {
            toggle(ui, &mut app.anim_bg, "Animated background");
        });
        divider(ui);
        setting_row(ui, "Pulse with the music", Some("The background swells with the beat."), |ui| {
            toggle(ui, &mut app.reactive_bg, "Pulse with the music");
        });
        divider(ui);
        setting_row(ui, "Stats for nerds", Some("Show the codec, sample rate and bitrate of what's playing at the foot of the cover in Now Playing, whatever the source."), |ui| {
            toggle(ui, &mut app.show_nerd_stats, "Stats for nerds");
        });
    });
    settings_lyrics(app, ui);
}

/// Settings › Window.
fn settings_window(app: &mut App, ui: &mut Ui) {
    section_label(ui, "Window", None);
    settings_card(ui, |ui| {
        setting_row(ui, "Close to tray", Some("Clicking × hides the window and keeps the music playing. Click the tray icon to bring it back, or right-click it to quit."), |ui| {
            toggle(ui, &mut app.close_to_tray, "Close to tray");
        });
        divider(ui);
        setting_row(ui, "Collapse sidebar", Some("Show only icons in the sidebar, for narrow windows. The menu button at its top does the same."), |ui| {
            toggle(ui, &mut app.sidebar_collapsed, "Collapse sidebar");
        });
        divider(ui);
        let api = crate::renderer::api_name();
        let title = format!("Draw with {api}");
        let desc = format!(
            "Use {api} (wgpu) instead of OpenGL. It isn't faster for this app and takes about three times the memory; try it if OpenGL draws wrong on this PC. If {api} won't start, the app goes back to OpenGL."
        );
        setting_row(ui, &title, Some(&desc), |ui| {
            if toggle(ui, &mut app.wgpu_renderer, &title).changed() {
                use crate::renderer::Renderer;
                let r = if app.wgpu_renderer { Renderer::Wgpu } else { Renderer::OpenGl };
                if let Err(e) = crate::renderer::save(app.backend.data_dir(), r) {
                    log::warn!("renderer: couldn't save the setting: {e}");
                }
            }
        });
        if app.wgpu_renderer != (crate::renderer::active() == crate::renderer::Renderer::Wgpu) {
            divider(ui);
            let mut restart = false;
            setting_row(ui, "Restart to apply", Some("The app draws with the new renderer from the next start."), |ui| {
                restart = pill(ui, "Restart now", None, true).clicked();
            });
            if restart {
                app.restart(ui.ctx());
            }
        }
    });
}

/// Settings › Connections.
fn settings_connections(app: &mut App, ui: &mut Ui) {
    let mut discord_enabled = app.discord_enabled;
    let prefs_before = app.discord_prefs.clone();
    section_label(ui, "Discord", None);
    settings_card(ui, |ui| {
        setting_row(ui, "Discord", Some("Show what you're playing on your Discord profile."), |ui| {
            toggle(ui, &mut discord_enabled, "Discord");
        });
    });
    if discord_enabled != app.discord_enabled {
        app.set_discord_enabled(discord_enabled);
    }
    // The card's wording, as the desktop app's Discord settings: a few switches, then
    // the activity and its name behind "Customize the card".
    if app.discord_enabled {
        section_label(ui, "Discord card", None);
        settings_card(ui, |ui| {
            setting_row(ui, "Lead with the song", Some("Discord repeats the song, not the artist, beside your name in a channel."), |ui| {
                toggle(ui, &mut app.discord_prefs.lead_with_song, "Lead with the song");
            });
            divider(ui);
            setting_row(ui, "Show audio quality", Some("Hi-Res, lossless and Dolby specs on the card's last line."), |ui| {
                toggle(ui, &mut app.discord_prefs.show_quality, "Show audio quality");
            });
            divider(ui);
            setting_row(ui, "Customize the card", Some("The activity and its name."), |ui| {
                // Off means the plain card, so what was typed behind it is reset rather
                // than merely hidden.
                if toggle(ui, &mut app.discord_prefs.advanced, "Customize the card").changed() && !app.discord_prefs.advanced {
                    app.discord_prefs.reset_card();
                }
            });
            if app.discord_prefs.advanced {
                divider(ui);
                setting_row(ui, "Activity", Some("The verb shown above the card."), |ui| {
                    let btn = dropdown_button(ui, app.discord_prefs.activity.label(), 220.0, "Activity");
                    egui::Popup::menu(&btn).show(|ui| {
                        ui.set_width(220.0);
                        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                        ui.add_space(4.0);
                        for k in [crate::discord::ActivityKind::Listening, crate::discord::ActivityKind::Playing, crate::discord::ActivityKind::Watching, crate::discord::ActivityKind::Competing] {
                            if option_row(ui, k.label(), k == app.discord_prefs.activity).clicked() {
                                app.discord_prefs.activity = k;
                                ui.close();
                            }
                        }
                        ui.add_space(4.0);
                    });
                });
                divider(ui);
                setting_row(ui, "Name", Some("What follows the verb. Empty shows BitChord PC Continued."), |ui| {
                    settings_field(ui, &mut app.discord_prefs.name, "BitChord PC Continued", 128);
                });
            }
        });
    }
    if app.discord_prefs != prefs_before
        && let Some(d) = &app.discord
    {
        d.set_prefs(app.discord_prefs.clone());
    }
    section_label(ui, "Last.fm", None);
    settings_card(ui, |ui| {
        let busy = app.lastfm_busy.load(Ordering::Relaxed);
        let desc = match &app.lastfm_user {
            Some(name) => format!("Scrobbling what you play to {name}'s profile."),
            None if busy => "Allow BitChord on the Last.fm page that opened in your browser.".to_owned(),
            None => "Scrobble what you play to your Last.fm profile.".to_owned(),
        };
        let (mut connect, mut disconnect) = (false, false);
        setting_row(ui, "Last.fm", Some(&desc), |ui| {
            if app.lastfm_user.is_some() {
                disconnect = pill(ui, "Disconnect", None, false).clicked();
            } else if busy {
                ui.spinner();
            } else {
                connect = pill(ui, "Connect", None, false).clicked();
            }
        });
        if connect {
            app.lastfm_sign_in();
        }
        if disconnect {
            app.lastfm_sign_out();
        }
    });
}

/// A one-line text field in a settings row's control slot.
pub(super) fn settings_field(ui: &mut Ui, text: &mut String, hint: &str, limit: usize) {
    let (rect, _) = ui.allocate_exact_size(vec2(200.0, 36.0), Sense::hover());
    ui.painter().rect_filled(rect, 10.0, theme::glass(0.08));
    let inner = rect.shrink2(vec2(12.0, 4.0));
    ui.put(
        inner,
        egui::TextEdit::singleline(text)
            .hint_text(egui::RichText::new(hint).color(theme::text_faint()))
            .font(theme::regular(14.0))
            .char_limit(limit)
            .frame(egui::Frame::NONE)
            .align(egui::Align2::LEFT_CENTER)
            .desired_width(inner.width()),
    );
}

/// Settings › Keyboard shortcuts. The bindings the app answers to, and the way to change
/// them: click a row and press a key. `⌘` is the Mac's command key and `Ctrl` everyone
/// else's, so the same row means the right thing on each machine.
fn settings_shortcuts(app: &mut App, ui: &mut Ui) {
    section_label(ui, "Keyboard shortcuts", None);
    let armed = app.keys_edit;
    // The recorder listens only while a row waits for a key.
    let typed = if armed.is_some() { super::keys::typed(ui.ctx()) } else { super::keys::Typed::Nothing };
    settings_card(ui, |ui| {
        for (i, info) in super::keys::INFOS.iter().enumerate() {
            let caps = app.keys.get(info.cmd).caps();
            if !settings_row_shown(ui, info.label, &caps.join(" ")) {
                continue;
            }
            if i > 0 {
                divider(ui);
            }
            let waiting = armed == Some(info.cmd);
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
            let resp = named(resp, info.label)
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(if app.keys.is_default(info.cmd) { "Click it, then press the keys" } else { "Click it, then press the keys · right-click for the default" });
            ui.painter().text(pos2(rect.left(), rect.center().y), Align2::LEFT_CENTER, info.label, theme::regular(14.0), theme::text());
            if waiting {
                let accent = app.accent_color();
                ui.painter().rect_stroke(rect.shrink(2.0), 8.0, egui::Stroke::new(1.5, accent), egui::StrokeKind::Inside);
                ui.painter().text(pos2(rect.right(), rect.center().y), Align2::RIGHT_CENTER, "Press a key…", theme::regular(13.5), accent);
            } else {
                let mut x = rect.right();
                for (j, k) in caps.iter().enumerate().rev() {
                    x = keycap(ui, pos2(x, rect.center().y), k) - 6.0;
                    if j > 0 {
                        let plus = ui.painter().text(pos2(x, rect.center().y), Align2::RIGHT_CENTER, "+", theme::regular(13.0), theme::text_dim());
                        x = plus.left() - 6.0;
                    }
                }
            }
            if resp.clicked() {
                app.keys_edit = if waiting { None } else { Some(info.cmd) };
                if !waiting {
                    // The keys are for the row now, not for a text box that still has them.
                    if let Some(id) = ui.ctx().memory(|m| m.focused()) {
                        ui.ctx().memory_mut(|m| m.surrender_focus(id));
                    }
                }
            }
            if resp.secondary_clicked() && !app.keys.is_default(info.cmd) {
                app.keys.reset(info.cmd);
            }
        }
    });
    let Some(cmd) = armed else { return };
    match typed {
        super::keys::Typed::Nothing => {}
        super::keys::Typed::Cancel => app.keys_edit = None,
        super::keys::Typed::Combo(combo) => {
            match app.keys.clash(cmd, combo) {
                Some(other) => app.toast(format!("{} already has those keys", other.info().label)),
                None => {
                    app.toast(format!("{} now uses {}", cmd.info().label, combo.caps().join(" + ")));
                    app.keys.set(cmd, combo);
                }
            }
            app.keys_edit = None;
        }
    }
}

/// Settings › Network: the proxy everything goes through, a connection test, and a
/// restart when the setting changed.
fn network(app: &mut App, ui: &mut Ui) {
    use crate::proxy::{self, Setting};
    let same = |a: &Setting, b: &Setting| std::mem::discriminant(a) == std::mem::discriminant(b);
    let mut pick = None;
    setting_row(ui, "Proxy", Some("Send everything BitChord fetches through a proxy: songs, lyrics, sign-in and Listen together."), |ui| {
        let btn = dropdown_button(ui, app.proxy_mode.label(), 220.0, "Proxy");
        egui::Popup::menu(&btn).show(|ui| {
            ui.set_width(220.0);
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            ui.add_space(4.0);
            for s in [Setting::System, Setting::Off, Setting::Custom(String::new())] {
                if option_row(ui, s.label(), same(&s, &app.proxy_mode)).clicked() {
                    pick = Some(s);
                    ui.close();
                }
            }
            ui.add_space(4.0);
        });
    });
    if let Some(s) = pick {
        app.proxy_error = None;
        *app.proxy_test.lock().unwrap() = None;
        app.proxy_mode = s.clone();
        // System and Off take effect as picked; a custom address once it's saved.
        if !matches!(s, Setting::Custom(_)) {
            match proxy::save(app.backend.data_dir(), &s) {
                Ok(()) => app.proxy_saved = s,
                Err(e) => app.proxy_error = Some(format!("{e:#}")),
            }
        }
    }
    match &app.proxy_mode {
        Setting::System => {
            divider(ui);
            let desc = match proxy::system() {
                Some(p) => format!("Windows is set to use {}.", proxy::redact(&p)),
                None => "Windows isn't set to use a proxy, so BitChord connects directly.".to_owned(),
            };
            setting_row(ui, "Windows proxy", Some(&desc), |_| {});
        }
        Setting::Custom(_) => {
            divider(ui);
            ui.add_space(12.0);
            super::text_field(ui, "Address", &mut app.proxy_url, false, false, "http://127.0.0.1:8080 or socks5://user:pass@host:1080");
            ui.add_space(10.0);
            let custom = Setting::Custom(app.proxy_url.trim().to_owned());
            let saved = app.proxy_saved == custom;
            let mut save = false;
            ui.horizontal(|ui| {
                save = pill(ui, if saved { "Saved" } else { "Save" }, None, !saved).clicked() && !saved;
                ui.add_space(8.0);
                ui.label(RichText::new("A password in the address is kept in Windows Credential Manager.").font(theme::regular(12.5)).color(theme::text_dim()));
            });
            if save {
                match proxy::save(app.backend.data_dir(), &custom) {
                    Ok(()) => {
                        app.proxy_saved = custom;
                        app.proxy_error = None;
                    }
                    Err(e) => app.proxy_error = Some(format!("{e:#}")),
                }
            }
            ui.add_space(12.0);
        }
        Setting::Off => {}
    }
    if let Some(e) = &app.proxy_error {
        ui.label(RichText::new(e).font(theme::regular(12.5)).color(theme::RED));
        ui.add_space(8.0);
    }
    divider(ui);
    let result = app.proxy_test.lock().unwrap().clone();
    let desc = match &result {
        Some(Some((_, msg))) => msg.clone(),
        Some(None) => "Checking…".to_owned(),
        None => "Checks that YouTube Music answers through this setting.".to_owned(),
    };
    setting_row(ui, "Test connection", Some(&desc), |ui| {
        if matches!(result, Some(None)) {
            ui.spinner();
        } else if pill(ui, "Test", None, false).clicked() {
            let s = match &app.proxy_mode {
                Setting::Custom(_) => Setting::Custom(app.proxy_url.trim().to_owned()),
                other => other.clone(),
            };
            let (slot, b) = (app.proxy_test.clone(), app.backend.clone());
            *slot.lock().unwrap() = Some(None);
            app.backend.spawn(async move {
                let out = match proxy::test(&s).await {
                    Ok(t) => (true, format!("Working: YouTube Music answered in {} ms.", t.as_millis())),
                    Err(e) => (false, format!("Didn't connect: {e:#}")),
                };
                *slot.lock().unwrap() = Some(Some(out));
                b.wake();
            });
        }
    });
    if app.proxy_saved != *proxy::active() {
        divider(ui);
        let mut restart = false;
        setting_row(ui, "Restart to apply", Some("The new proxy setting is used from the next start."), |ui| {
            restart = pill(ui, "Restart now", None, true).clicked();
        });
        if restart {
            app.restart(ui.ctx());
        }
    }
}

/// Paint `v` into `rect`, from the ICO entry that fits its size in physical pixels.
pub(super) fn paint_app_icon(ui: &Ui, v: &crate::appicon::Variant, rect: Rect) {
    let px = (rect.width() * ui.ctx().pixels_per_point()).ceil() as u32;
    let png = v.png(px);
    egui::Image::from_bytes(format!("bytes://app-icon/{}-{}.png", v.id, png.len()), png).paint_at(ui, rect);
}

/// The font setting: the theme's own, a bundled face, or one installed on this PC.
/// Returns the new choice ("" for the theme's).
fn font_picker(ui: &mut Ui) -> Option<String> {
    let current = theme::font_choice();
    let btn = dropdown_button(ui, if current.is_empty() { "Theme's font" } else { &current }, 220.0, "Font");
    let just_opened = btn.clicked();
    let mut picked = None;
    egui::Popup::menu(&btn).show(|ui| {
        ui.set_width(220.0);
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
            ui.add_space(4.0);
            let mut row = |ui: &mut Ui, label: &str, value: &str| {
                let on = current.eq_ignore_ascii_case(value);
                let r = option_row(ui, label, on);
                if on && just_opened {
                    r.scroll_to_me(Some(egui::Align::Center));
                }
                if r.clicked() {
                    picked = Some(value.to_owned());
                    ui.close();
                }
            };
            row(ui, "Theme's font", "");
            for name in theme::BUNDLED_FONTS {
                row(ui, name, name);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(RichText::new("Installed on this PC").font(theme::semibold(12.0)).color(theme::text_faint()));
            });
            ui.add_space(4.0);
            let ctx = ui.ctx().clone();
            match theme::installed_fonts(move || ctx.request_repaint()) {
                Some(fonts) => {
                    for f in fonts {
                        row(ui, &f.name, &f.name);
                    }
                }
                None => {
                    ui.horizontal(|ui| {
                        ui.add_space(12.0);
                        ui.label(RichText::new("Looking for fonts…").font(theme::regular(13.0)).color(theme::text_dim()));
                    });
                }
            }
            ui.add_space(4.0);
        });
    });
    picked.filter(|p| *p != current)
}

const SYSTEM_DEFAULT: &str = "Windows default";

fn output_device_picker(app: &mut App, ui: &mut Ui) {
    let current = match &app.output_device {
        None => SYSTEM_DEFAULT.to_owned(),
        Some((id, name)) => match app.output_devices.iter().find(|d| d.id == *id) {
            Some(d) => d.name.clone(),
            None if app.output_devices.is_empty() => name.clone(),
            None => format!("{name} (not connected)"),
        },
    };
    let btn = dropdown_button(ui, &current, 210.0, "Output device");
    // Listed when opened, so a device that just connected shows up.
    if btn.clicked() || app.output_devices.is_empty() {
        app.output_devices = crate::audio::output_devices();
    }
    let mut pick: Option<Option<(String, String)>> = None;
    egui::Popup::menu(&btn).show(|ui| {
        ui.set_width(340.0);
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            ui.add_space(4.0);
            if option_row(ui, SYSTEM_DEFAULT, app.output_device.is_none()).clicked() {
                pick = Some(None);
                ui.close();
            }
            for d in &app.output_devices {
                if option_row(ui, &d.name, app.output_device.as_ref().is_some_and(|(id, _)| *id == d.id)).clicked() {
                    pick = Some(Some((d.id.clone(), d.name.clone())));
                    ui.close();
                }
            }
            ui.add_space(4.0);
        });
    });
    if let Some(pick) = pick {
        app.audio.send(crate::audio::AudioCmd::SetDevice(pick.as_ref().map(|(id, _)| id.clone())));
        app.output_device = pick;
    }
}

/// A dropdown's closed state: the chosen value and a chevron on a glass pill.
pub(super) fn dropdown_button(ui: &mut Ui, value: &str, width: f32, name: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 36.0), Sense::click());
    let resp = widgets::named(resp, name).on_hover_cursor(egui::CursorIcon::PointingHand);
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&resp));
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered() || open, 0.12);
    ui.painter().rect_filled(rect, 10.0, theme::glass(0.08 + 0.05 * h));
    widgets::text_at(ui, pos2(rect.left() + 14.0, rect.center().y - 9.0), value, theme::regular(14.0), theme::text(), rect.width() - 44.0);
    let glyph = if open { theme::icon::CHEVRON_UP } else { theme::icon::CHEVRON_DOWN };
    widgets::icon_at(ui, pos2(rect.right() - 18.0, rect.center().y), glyph, 15.0, theme::lerp_color(theme::text_dim(), theme::text(), h));
    resp
}

/// A dropdown option: hover glass, and a check on the chosen one.
pub(super) fn option_row(ui: &mut Ui, label: &str, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
    let resp = widgets::named(resp, label).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.12);
    if h > 0.0 || selected {
        ui.painter().rect_filled(rect, 8.0, theme::glass(0.05 * selected as u8 as f32 + 0.08 * h));
    }
    let font = if selected { theme::semibold(14.0) } else { theme::regular(14.0) };
    widgets::text_at(ui, pos2(rect.left() + 12.0, rect.center().y - 9.0), label, font, theme::text(), rect.width() - 44.0);
    if selected {
        widgets::icon_at(ui, pos2(rect.right() - 16.0, rect.center().y), theme::icon::CHECK, 15.0, theme::text());
    }
    resp
}

/// 12345 -> "12,345".
pub(super) fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Small heading above a settings card, with an optional value on the right.
pub(super) fn section_label(ui: &mut Ui, title: &str, value: Option<&str>) {
    let mut s = search_of(ui);
    if !s.words.is_empty() {
        let on_name = settings_hit(title, &s.words);
        let any_row = ui
            .data(|d| d.get_temp::<SettingsRows>(rows_key()))
            .unwrap_or_default()
            .0
            .iter()
            .any(|(sec, row, desc)| sec == title && (on_name || settings_hit(&format!("{row} {desc}"), &s.words)));
        s.shown = on_name || any_row;
        s.all = on_name;
        s.matched |= s.shown;
    } else {
        s.shown = true;
        s.all = true;
    }
    s.section = title.to_owned();
    put_search(ui, &s);
    if !s.shown {
        return;
    }
    ui.add_space(30.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    ui.painter().text(pos2(rect.left() + 2.0, rect.center().y), Align2::LEFT_CENTER, title, theme::bold(17.0), theme::text());
    if let Some(v) = value {
        ui.painter().text(pos2(rect.right() - 2.0, rect.center().y), Align2::RIGHT_CENTER, v, theme::semibold(13.0), theme::text_dim());
    }
    ui.add_space(10.0);
}

pub(super) fn settings_card(ui: &mut Ui, body: impl FnOnce(&mut Ui)) {
    if !settings_group_shown(ui) {
        return;
    }
    egui::Frame::new()
        .fill(theme::glass(0.06))
        .stroke(egui::Stroke::new(1.0, theme::glass(0.05)))
        .corner_radius(theme::panel_radius())
        .inner_margin(egui::Margin::symmetric(20, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            body(ui);
        });
}

/// A settings row: title and description on the left, the control on the right.
pub(super) fn setting_row(ui: &mut Ui, title: &str, desc: Option<&str>, control: impl FnOnce(&mut Ui)) {
    if !settings_row_shown(ui, title, desc.unwrap_or("")) {
        return;
    }
    let w = ui.available_width();
    let text_w = (w - 230.0).max(200.0);
    let title_g = widgets::galley(ui, title, theme::semibold(14.5), theme::text(), text_w);
    let desc_g = desc.map(|d| widgets::galley_wrapped(ui, d, theme::regular(12.5), theme::text_dim(), text_w, 3));
    let text_h = title_g.size().y + desc_g.as_ref().map_or(0.0, |g| g.size().y + 3.0);
    let h = (text_h + 26.0).max(58.0);
    let (rect, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
    let fade = if ui.is_enabled() { 1.0 } else { 0.45 };
    let top = rect.center().y - text_h / 2.0;
    ui.painter().galley(pos2(rect.left(), top), title_g.clone(), theme::with_alpha(theme::text(), fade));
    if let Some(g) = desc_g {
        ui.painter().galley(pos2(rect.left(), top + title_g.size().y + 3.0), g, theme::with_alpha(theme::text_dim(), fade));
    }
    let ctl = Rect::from_min_max(pos2(rect.left() + text_w + 20.0, rect.top()), rect.max);
    let mut cui = ui.new_child(egui::UiBuilder::new().max_rect(ctl).layout(egui::Layout::right_to_left(egui::Align::Center)));
    control(&mut cui);
}

/// A settings row that opens a section of its own: the same text as `setting_row`, but
/// the whole row is clickable and a chevron stands where the control would.
pub(super) fn open_row(ui: &mut Ui, title: &str, desc: &str) -> bool {
    if !settings_row_shown(ui, title, desc) {
        return false;
    }
    let w = ui.available_width();
    let text_w = (w - 230.0).max(200.0);
    let title_g = widgets::galley(ui, title, theme::semibold(14.5), theme::text(), text_w);
    let desc_g = widgets::galley_wrapped(ui, desc, theme::regular(12.5), theme::text_dim(), text_w, 3);
    let text_h = title_g.size().y + desc_g.size().y + 3.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, (text_h + 26.0).max(58.0)), Sense::click());
    let resp = named(resp, title).on_hover_cursor(egui::CursorIcon::PointingHand);
    let top = rect.center().y - text_h / 2.0;
    let title_h = title_g.size().y;
    ui.painter().galley(pos2(rect.left(), top), title_g, theme::text());
    ui.painter().galley(pos2(rect.left(), top + title_h + 3.0), desc_g, theme::text_dim());
    widgets::icon_at(ui, pos2(rect.right() - 8.0, rect.center().y), icon::CHEVRON_RIGHT, 16.0, theme::text_faint());
    resp.clicked()
}

pub(super) fn divider(ui: &mut Ui) {
    // Rows the box kept read as one block, so a filtered group draws no lines between them.
    let s = search_of(ui);
    if !s.words.is_empty() && !s.all {
        return;
    }
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme::glass(0.07));
}

/// On/off switch tinted with the accent.
pub(super) fn toggle(ui: &mut Ui, on: &mut bool, name: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(44.0, 26.0), Sense::click());
    let mut r = toggle_at(ui, rect, *on, resp.id, name);
    if r.clicked() {
        *on = !*on;
        r.mark_changed();
    }
    r
}

/// The switch itself, at a place the caller has laid out (the queue drawer's header
/// draws its row by hand). It only reports the click; turning `on` is the caller's.
pub(super) fn toggle_at(ui: &mut Ui, rect: Rect, on: bool, id: egui::Id, name: &str) -> egui::Response {
    let resp = ui.interact(rect, id, Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, ui.is_enabled(), on, name));
    let resp = resp.on_hover_text(name).on_hover_cursor(egui::CursorIcon::PointingHand);
    let t = theme::anim_bool(ui.ctx(), resp.id.with("t"), on, 0.16);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.12);
    let accent = ui.ctx().data(|d| d.get_temp::<Color32>(egui::Id::unique("accent"))).unwrap_or(theme::RED);
    let fade = if ui.is_enabled() { 1.0 } else { 0.4 };
    let off = theme::glass(0.16 + 0.06 * h);
    ui.painter().rect_filled(rect, 13.0, theme::with_alpha(theme::lerp_color(off, accent, t), fade));
    let knob = pos2(egui::lerp(rect.left() + 13.0..=rect.right() - 13.0, t), rect.center().y);
    ui.painter().circle_filled(knob + vec2(0.0, 1.0), 10.0, theme::shade(0.25 * fade));
    ui.painter().circle_filled(knob, 10.0, theme::with_alpha(Color32::WHITE, fade));
    resp
}

fn seconds_slider(ui: &mut Ui, v: &mut u32, range: std::ops::RangeInclusive<u32>, name: &str) -> egui::Response {
    let (lo, hi) = (*range.start(), *range.end());
    let label = |ui: &mut Ui, text: String, w: f32, font: egui::FontId, color: Color32| {
        let (rect, _) = ui.allocate_exact_size(vec2(w, 26.0), Sense::hover());
        let g = widgets::galley(ui, &text, font, color, w);
        ui.painter().galley(pos2(rect.right() - g.size().x, rect.center().y - g.size().y / 2.0), g, color);
    };
    label(ui, format!("{v} s"), 34.0, theme::semibold(13.0), theme::text());
    ui.add_space(6.0);
    label(ui, format!("{hi} s"), 26.0, theme::regular(12.0), theme::text_dim());
    let (rect, mut resp) = ui.allocate_exact_size(vec2(170.0, 26.0), Sense::click_and_drag());
    label(ui, format!("{lo} s"), 22.0, theme::regular(12.0), theme::text_dim());
    let track = Rect::from_center_size(rect.center(), vec2(rect.width() - 16.0, 4.0));
    let old = *v;
    if let Some(p) = resp.interact_pointer_pos()
        && (resp.dragged() || resp.clicked())
    {
        let t = ((p.x - track.left()) / track.width()).clamp(0.0, 1.0);
        *v = (lo as f32 + t * (hi - lo) as f32).round() as u32;
    }
    if *v != old {
        resp.mark_changed();
    }
    let value = *v;
    resp.widget_info(|| egui::WidgetInfo::slider(ui.is_enabled(), value as f64, name));
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered() || resp.dragged(), 0.12);
    let accent = ui.ctx().data(|d| d.get_temp::<Color32>(egui::Id::unique("accent"))).unwrap_or(theme::RED);
    let t = (value - lo) as f32 / (hi - lo) as f32;
    let knob = pos2(egui::lerp(track.left()..=track.right(), t), track.center().y);
    ui.painter().rect_filled(track, 2.0, theme::glass(0.15));
    ui.painter().rect_filled(Rect::from_min_max(track.min, pos2(knob.x, track.max.y)), 2.0, accent);
    ui.painter().circle_filled(knob + vec2(0.0, 1.0), 7.0 + h, theme::shade(0.25));
    ui.painter().circle_filled(knob, 7.0 + h, Color32::WHITE);
    resp
}

/// Draw a key cap whose right edge is at `right`; returns its left edge.
fn keycap(ui: &Ui, right: egui::Pos2, key: &str) -> f32 {
    let g = widgets::galley(ui, key, theme::semibold(12.5), theme::text(), 200.0);
    let size = vec2((g.size().x + 18.0).max(30.0), 26.0);
    let rect = Rect::from_min_size(pos2(right.x - size.x, right.y - size.y / 2.0), size);
    ui.painter().rect_filled(rect, 7.0, theme::glass(0.08));
    ui.painter().rect_stroke(rect, 7.0, egui::Stroke::new(1.0, theme::glass(0.12)), egui::StrokeKind::Inside);
    ui.painter().rect_filled(Rect::from_min_max(pos2(rect.left() + 2.0, rect.bottom() - 2.0), pos2(rect.right() - 2.0, rect.bottom())), 2.0, theme::shade(0.25));
    ui.painter().galley(rect.center() - g.size() / 2.0, g, theme::text());
    rect.left()
}


#[cfg(test)]
mod library_tests {
    use super::*;
    use crate::model::Link;

    fn song(id: &str, title: &str, artist: &str, album: &str) -> Track {
        Track {
            id: id.into(),
            title: title.into(),
            artists: vec![Link { name: artist.into(), id: None }],
            album: Some(Link { name: album.into(), id: None }),
            duration: Some(61),
            thumbs: vec![],
            track_nr: None,
            plays: None,
            music_video: false,
            episode: false,
        }
    }

    #[test]
    fn the_songs_sort_moves_rows_and_not_the_songs() {
        let songs = [song("b", "Bad Boy", "Cher", "Believe"), song("a", "Yellow Submarine", "The Beatles", "Abbey Road")];
        // Added keeps the order YouTube sent; a name ignores a leading "The" wherever
        // it starts, and an artist's is the line the row shows.
        assert_eq!(order(&songs, LibSort::Added, ""), vec![0, 1]);
        assert_eq!(order(&songs, LibSort::Title, ""), vec![0, 1]);
        assert_eq!(order(&songs, LibSort::Artist, ""), vec![1, 0]);
        // A search takes rows out of the sorted list; the rest keep their places, and
        // what's left still says which song it is.
        assert_eq!(order(&songs, LibSort::Title, "yellow"), vec![1]);
        assert_eq!(order(&songs, LibSort::Title, "believe"), vec![0]);
        assert_eq!(order(&songs, LibSort::Title, "nothing"), vec![] as Vec<usize>);
    }

    #[test]
    fn library_columns_widen_with_the_page() {
        // A narrow window keeps the song's own name and nothing else; the release only
        // gets a column when there's room for the artist to keep its own.
        let narrow = SongCols::new(Rect::from_min_max(pos2(0.0, 0.0), pos2(360.0, 44.0)));
        assert_eq!((narrow.artist, narrow.album), (None, None));
        let middle = SongCols::new(Rect::from_min_max(pos2(0.0, 0.0), pos2(720.0, 44.0)));
        assert!(middle.artist.is_some() && middle.album.is_none());
        let wide = SongCols::new(Rect::from_min_max(pos2(0.0, 0.0), pos2(1400.0, 44.0)));
        let (ax, aw) = wide.artist.unwrap();
        let (bx, bw) = wide.album.unwrap();
        assert!(bx > ax && aw > 60.0 && bw > 60.0);
        assert!(wide.name.1 > 60.0 && bx + bw < wide.time);
    }

    #[test]
    fn the_albums_page_drops_its_own_word() {
        assert_eq!(without_type("Album • Royal Blood • 2026"), "Royal Blood • 2026");
        assert_eq!(without_type("EP • King Gizzard • 2022"), "King Gizzard • 2022");
        // A playlist's subtitle has no type to lose, and neither has a line that only
        // happens to start with a word.
        assert_eq!(without_type("jackonyoutube • 341 tracks"), "jackonyoutube • 341 tracks");
        assert_eq!(without_type(""), "");
    }
}
