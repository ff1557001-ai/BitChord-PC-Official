//! The application UI: state, playback logic and the per-frame layout.
//!
//! Layout: animated album-art background (shader.rs) → sidebar, top bar and page
//! content → player bar → now-playing overlay (slides up) → toasts.

mod account;
mod actions;
mod backdrop;
mod collections;
mod dev;
mod dialogs;
mod glyphs;
mod integrations;
mod jam;
mod karaoke;
mod keys;
mod lists;
mod lyric_prefs;
mod lyrics_share;
mod mini;
mod notes;
mod nowplaying;
mod pages;
mod playback;
mod queue;
mod responses;
mod shell;
mod titlebar;
pub use titlebar::{custom as custom_titlebar, set_hwnd as set_titlebar_hwnd};
mod transfer;
mod widgets;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crossbeam_channel::Receiver;
use egui::{Color32, Ui};
use souvlaki::MediaControlEvent;

use crate::audio::{AudioEvent, AudioHandle};
use crate::backend::{ArtistPage, Backend, Explore, Library, Req, Resp, SearchResults};
use crate::lyrics::LyricsSet;
use crate::media::Media;
use crate::model::{Card, Chip, Shelf, Target, Track};
use crate::shader::Background;
use crate::theme;
use lists::{LIKED_MUSIC, Lists};
use queue::Queue;
use dev::{dev_pace, dev_screenshot, dev_var};
use dialogs::text_field;
pub use queue::Repeat;

/// How many motion sleeves are decoded at once. A view's frames and texture are
/// sized by the drawn tile, so this caps what the feature can hold.
const MOTION_VIEWS: usize = 4;

#[derive(Clone, PartialEq, Hash, Debug)]
pub enum Page {
    Home,
    New,
    Library(LibTab),
    Search(String),
    Downloads,
    Album(String),
    Playlist(String),
    Artist(String),
    /// Apple's own collection page, by its address.
    AppleCollection(String),
    /// The whole of one home or New section, opened by clicking its name.
    SeeAll,
    Settings,
    /// Transfer from Spotify.
    Transfer,
}

#[derive(Clone, Copy, PartialEq, Hash, Debug)]
pub enum LibTab {
    Playlists,
    Songs,
    Albums,
    Artists,
    History,
    Uploads,
}

impl LibTab {
    /// The name the page wears — Apple's own, which for playlists is the whole of them.
    pub fn title(self) -> &'static str {
        match self {
            LibTab::Playlists => "All Playlists",
            LibTab::Songs => "Songs",
            LibTab::Albums => "Albums",
            LibTab::Artists => "Artists",
            LibTab::History => "Recently Played",
            LibTab::Uploads => "Uploads",
        }
    }
}

/// How a library tab orders what it holds — Apple's Sort popup, which every library page
/// carries and which the pages remember between visits and launches.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LibSort {
    /// The order YouTube Music returns: newest first.
    Added,
    Title,
    Artist,
}

impl LibSort {
    /// The orders this page offers. Apple's own set differs per tab, and every one of
    /// them is a name YouTube's data can answer for.
    pub fn options(tab: LibTab) -> &'static [LibSort] {
        match tab {
            // A playlist's subtitle says what it is, not who made it, so there's no
            // artist to order by.
            LibTab::Playlists | LibTab::Artists => &[LibSort::Added, LibSort::Title],
            _ => &[LibSort::Added, LibSort::Title, LibSort::Artist],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LibSort::Added => "Recently Added",
            LibSort::Title => "Name",
            LibSort::Artist => "Artist",
        }
    }

    /// The saved choice, or the newest-first default.
    pub fn read(stored: Option<&str>) -> Self {
        match stored {
            Some("title") => LibSort::Title,
            Some("artist") => LibSort::Artist,
            _ => LibSort::Added,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            LibSort::Added => "added",
            LibSort::Title => "title",
            LibSort::Artist => "artist",
        }
    }

    /// The cards of a library tab, in this order. Added keeps what YouTube sent.
    pub fn sort(self, cards: &mut [crate::model::Card]) {
        match self {
            LibSort::Added => {}
            LibSort::Title => cards.sort_by(|a, b| sort_key(&a.title).cmp(&sort_key(&b.title))),
            // A library album's subtitle is its artist line; ties break on the release.
            LibSort::Artist => cards.sort_by(|a, b| sort_key(&a.subtitle).cmp(&sort_key(&b.subtitle)).then_with(|| sort_key(&a.title).cmp(&sort_key(&b.title)))),
        }
    }
}

/// A name as a sort key: lower-case and under its own first word, so "Bad" sorts
/// under B and not under T for "The Bad".
pub(crate) fn sort_key(s: &str) -> String {
    let s = s.trim().to_lowercase();
    match s.strip_prefix("the ") {
        Some(rest) => rest.to_owned(),
        None => s,
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum NpTab {
    Lyrics,
    UpNext,
    /// Neither panel: just the artwork.
    Art,
}

/// User intents collected while drawing; applied after the frame's UI pass.
pub enum Action {
    Play(Vec<Track>, usize),
    /// A list's Shuffle control: play these songs with the queue's shuffle mark on, so
    /// what follows is random and switching the mark off puts them back in this order.
    PlayShuffled(Vec<Track>),
    PlayCollection(Target, bool),
    /// Click on a card: songs start a radio, everything else opens its page.
    Open(Target),
    /// Play an Apple collection's own songs, each found on the source that has it.
    ApplePlay { target: Target, songs: Vec<crate::apple::AppleSong>, shuffle: bool },
    PlayNext(Track),
    Enqueue(Track),
    Radio(Track),
    Go(Page),
    /// A section's name clicked: put all it holds on a page of its own.
    SeeAll(Shelf),
    QueueJump(usize),
    QueueRemove(usize),
    /// A queue row one place up (`dir` negative) or down.
    QueueMove(usize, isize),
    /// A queue row put just after the song playing — the queue's own "Play next", which
    /// moves the row instead of queueing a second copy of the song.
    QueuePlayNext(usize),
    /// Open up next (the player's) or the queue drawer (the bar's).
    ViewQueue,
    Seek(f32),
    Like(Track, bool),
    /// Like (or unlike) a selection of songs.
    LikeMany(Vec<Track>, bool),
    Dislike(Track),
    Share(Target),
    ShareLyrics { video_id: String, generation: u64, provider: Option<crate::lyrics::Provider> },
    AboutSong(Track),
    AboutAlbum { id: String, title: String, artist: String },
    AddToPlaylist(Vec<Track>),
    RemoveFromPlaylist { playlist: String, items: Vec<(String, String)> },
    Download(Track),
    NewPlaylist(Vec<Track>),
    EditPlaylist,
    /// The open playlist's collaboration settings.
    Collaborate,
    DeletePlaylist,
    SaveCollection(String, bool),
    HomeChip(Option<String>),
    /// A mood tile clicked: play its station rather than filter the feed.
    MoodStation(crate::ytm::Mood),
    OpenNowPlaying(bool),
    /// Into the mini player (true) or back to the full window.
    Mini(bool),
    /// Play/pause what's already loaded (a card's button on the playing item).
    TogglePlay,
    /// Send lossless tracks straight to the DAC in WASAPI exclusive mode.
    BitPerfect(bool),
    /// Drop the hi-res stream and replay the current song from YouTube at the same spot.
    RevertQuality,
    /// Clear a quality revert and replay, asking hi-res sources again.
    UpgradeQuality,
    /// A song chosen on the search page as the Spotify transfer's match.
    TransferPick(Track),
}

fn removal_items(c: &crate::backend::Collection, rows: impl IntoIterator<Item = usize>) -> Vec<(String, String)> {
    rows.into_iter().filter_map(|i| Some((c.tracks.get(i)?.id.clone(), c.set_ids.get(i)?.as_ref()?.clone()))).collect()
}

fn removal_rows(c: &crate::backend::Collection, items: &[(String, String)]) -> Vec<usize> {
    let entries: HashSet<_> = items.iter().map(|(id, set)| (id.as_str(), set.as_str())).collect();
    c.tracks.iter().enumerate().filter_map(|(i, t)| {
        let set = c.set_ids.get(i)?.as_deref()?;
        entries.contains(&(t.id.as_str(), set)).then_some(i)
    }).collect()
}

enum Dialog {
    GeniusAbout { track: Track, description: Option<Option<String>> },
    AppleAlbumAbout { id: String, title: String, artist: String, description: Option<Option<String>> },
    PickPlaylist(Vec<Track>),
    NewPlaylist { title: String, tracks: Vec<Track> },
    EditPlaylist { id: String, title: String, description: String, privacy: Option<String> },
    ConfirmDelete { id: String, title: String },
    /// `info` until it loads; `busy` while a change is on its way; `confirm_off` asks
    /// before turning collaboration off (that removes the collaborators).
    Collaborate { id: String, info: Option<crate::ytm::Collaboration>, busy: bool, confirm_off: bool },
}

/// Songs picked in a list for a batch action: Ctrl-click toggles one, Shift-click a range,
/// or the Select button makes every click pick.
#[derive(Default)]
pub struct Selection {
    /// The page the picks belong to; on any other page there are none.
    pub page: Option<Page>,
    /// Row indices into that page's list.
    pub rows: std::collections::BTreeSet<usize>,
    pub anchor: Option<usize>,
    /// Select mode: a plain click picks instead of playing.
    pub active: bool,
}

impl Selection {
    pub fn on(&self, page: &Page) -> bool {
        self.page.as_ref() == Some(page) && (self.active || !self.rows.is_empty())
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Start select mode on `page` with nothing picked yet.
    pub fn start(&mut self, page: &Page) {
        *self = Self { page: Some(page.clone()), active: true, ..Self::default() };
    }

    /// A replacement or edit can change the meaning of collection row indices.
    fn collection_changed(&mut self, id: &str) {
        let matches = match self.page.as_ref() {
            Some(Page::Album(page_id) | Page::Playlist(page_id)) => page_id == id,
            Some(Page::Library(LibTab::Songs)) => id == LIKED_MUSIC,
            _ => false,
        };
        if matches {
            self.clear();
        }
    }

    /// Toggle row `i`, or with `range` pick every row from the last one toggled to it.
    pub fn pick(&mut self, page: &Page, i: usize, range: bool) {
        if self.page.as_ref() != Some(page) {
            *self = Self { page: Some(page.clone()), ..Self::default() };
        }
        match self.anchor.filter(|_| range) {
            Some(a) => self.rows.extend(a.min(i)..=a.max(i)),
            None => {
                if !self.rows.remove(&i) {
                    self.rows.insert(i);
                }
                self.anchor = Some(i);
            }
        }
    }

    /// A filtered range contains only visible rows, even when source indices have gaps.
    fn pick_visible(&mut self, page: &Page, i: usize, range: bool, visible: &[usize]) {
        if range && self.page.as_ref() == Some(page)
            && let Some(a) = self.anchor.and_then(|a| visible.iter().position(|&row| row == a))
            && let Some(b) = visible.iter().position(|&row| row == i)
        {
            self.rows.extend(visible[a.min(b)..=a.max(b)].iter().copied());
        } else if range {
            if self.page.as_ref() != Some(page) {
                *self = Self { page: Some(page.clone()), ..Self::default() };
            }
            self.rows.insert(i);
            self.anchor = Some(i);
        } else {
            self.pick(page, i, false);
        }
    }
}

pub struct HomeState {
    pub chips: Vec<Chip>,
    pub shelves: Vec<Shelf>,
    pub continuation: Option<String>,
    /// When and which continuation was requested while more shelves load.
    pub loading_more: Option<(f64, String)>,
    pub picks_loading: bool,
    pub picks_expanded: bool,
}

pub struct Channels {
    pub resp_rx: Receiver<Resp>,
    pub audio_rx: Receiver<AudioEvent>,
    pub media_rx: Receiver<MediaControlEvent>,
}

/// Settings › Network's connection test: not run (`None`), running (`Some(None)`), or
/// its answer (works, message).
type ProxyTest = Option<Option<(bool, String)>>;

/// A thing opened from a search, shown on the search landing with its artwork.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct RecentSearch {
    /// The search it was clicked from.
    pub query: String,
    /// What was opened; `None` for entries saved by an earlier version, which only kept
    /// the query and open the search again.
    #[serde(default)]
    pub target: Option<Target>,
    pub title: String,
    pub subtitle: String,
    pub thumb: Option<String>,
    /// An artist, whose picture is round.
    pub round: bool,
}

/// A decoded motion sleeve, uploaded to a texture as frames arrive.
pub struct MotionView {
    motion: std::sync::Arc<crate::motion::Motion>,
    tex: Option<egui::TextureHandle>,
    /// The same frame taken down to a few pixels wide, for a backdrop out of focus.
    blur: Option<egui::TextureHandle>,
    seen: u64,
    blur_seen: u64,
    /// The width this decoder was started at: a larger view replaces it once.
    width: u32,
    /// When a view last asked for a frame; idle ones are dropped, decoder and all.
    last_used: std::time::Instant,
}

impl MotionView {
    fn new(motion: std::sync::Arc<crate::motion::Motion>, width: u32) -> Self {
        Self { motion, tex: None, blur: None, seen: 0, blur_seen: 0, width, last_used: std::time::Instant::now() }
    }

    /// The newest frame as a texture, once one has been decoded.
    fn texture(&mut self, ctx: &egui::Context) -> Option<&egui::TextureHandle> {
        if let Some((version, frame)) = self.motion.latest()
            && version != self.seen
        {
            self.seen = version;
            let image = egui::ColorImage::from_rgba_unmultiplied([frame.w, frame.h], &frame.rgba);
            match &mut self.tex {
                Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
                None => self.tex = Some(ctx.load_texture("apple-motion", image, egui::TextureOptions::LINEAR)),
            }
        }
        self.tex.as_ref()
    }

    /// The newest frame taken down to forty pixels across: stretched back up it reads as
    /// the video out of focus, which is what a column of lyrics over a video wants.
    /// A grid of samples, not a filter — the whole frame averaged would cost more per
    /// frame than the picture is worth.
    fn blurred(&mut self, ctx: &egui::Context) -> Option<&egui::TextureHandle> {
        if let Some((version, frame)) = self.motion.latest()
            && version != self.blur_seen
        {
            self.blur_seen = version;
            let (w, h) = (frame.w.max(1), frame.h.max(1));
            let cols = 40usize;
            let rows = (cols * h / w).max(1);
            let mut pixels = Vec::with_capacity(cols * rows);
            for ry in 0..rows {
                for rx in 0..cols {
                    let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
                    for dy in 0..4 {
                        for dx in 0..4 {
                            let x = ((rx * 4 + dx) * w / (cols * 4)).min(w - 1);
                            let y = ((ry * 4 + dy) * h / (rows * 4)).min(h - 1);
                            let i = (y * w + x) * 4;
                            r += frame.rgba[i] as u32;
                            g += frame.rgba[i + 1] as u32;
                            b += frame.rgba[i + 2] as u32;
                        }
                    }
                    pixels.push(Color32::from_rgb((r / 16) as u8, (g / 16) as u8, (b / 16) as u8));
                }
            }
            let image = egui::ColorImage::new([cols, rows], pixels);
            match &mut self.blur {
                Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
                None => self.blur = Some(ctx.load_texture("motion-blur", image, egui::TextureOptions::LINEAR)),
            }
        }
        self.blur.as_ref()
    }
}

impl Drop for MotionView {
    fn drop(&mut self) {
        // Off screen: the decoder stops at its next frame instead of looping on.
        self.motion.stop();
    }
}

/// Apple's moving artwork, kept in one place: which releases have a sleeve and where it
/// is, what's asked and what's cooling, and the decoders on screen. A page borrows just
/// this to ask its tiles for sleeves, which leaves the rest of the app — the feed it's
/// drawing from — borrowed at the same time.
#[derive(Default)]
pub struct AppleArt {
    /// Motion sleeve master playlist per release key (`None`: Apple has none).
    covers: std::collections::HashMap<String, Option<String>>,
    /// Releases Apple had nothing for when last asked, and when: asked again after a
    /// while, since a lookup can fail on a network hiccup rather than for good.
    missed: std::collections::HashMap<String, std::time::Instant>,
    /// Apple lookups already in flight.
    waiting: std::collections::HashSet<String>,
    /// Decoders for the motion sleeves on screen.
    views: std::collections::HashMap<String, MotionView>,
}

impl AppleArt {
    /// Ask Apple, once, for the motion sleeve of a release. `title` names the song whose
    /// sleeve is wanted and `None` asks for the release `album` itself; `apple` is
    /// Apple's own record id, which is the only way to a playlist's clip.
    fn cover(&mut self, backend: &Backend, on: bool, album: &str, artist: &str, title: Option<&str>, apple: Option<&str>) {
        if !on || apple.is_none() && (album.is_empty() || artist.is_empty()) {
            return;
        }
        let key = match apple {
            Some(id) => format!("id:{id}"),
            None => apple_key(album, artist, title),
        };
        if self.covers.contains_key(&key) {
            return;
        }
        if self.missed.get(&key).is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(300)) {
            return;
        }
        if !self.waiting.insert(key.clone()) {
            return;
        }
        backend.request(Req::AppleCover {
            key,
            album: album.to_owned(),
            artist: artist.to_owned(),
            title: title.map(str::to_owned),
            record: apple.map(str::to_owned),
        });
    }

    /// The newest frame of that sleeve, started the first time it's seen.
    /// `None` until Apple has answered (or if it has no clip for the release).
    fn sprite(&mut self, ctx: &egui::Context, on: bool, album: &str, artist: &str, title: Option<&str>, apple: Option<&str>, max_width: u32) -> Option<widgets::MotionSprite> {
        if !on {
            return None;
        }
        let key = match apple {
            Some(id) => format!("id:{id}"),
            None => apple_key(album, artist, title),
        };
        let url = self.covers.get(&key)?.clone()?;
        self.clip(ctx, &url, max_width)
    }

    /// The newest frame of a motion clip, started the first time it's seen. The decoders
    /// on screen are capped so old pages don't leave threads behind.
    fn clip(&mut self, ctx: &egui::Context, url: &str, max_width: u32) -> Option<widgets::MotionSprite> {
        // Never decode wider than the player's sleeve needs; frames and textures are
        // sized by this, so it bounds the whole feature's memory.
        let max_width = max_width.clamp(64, 640);
        // A tile that scrolled away stops being asked for; its decoder goes soon after.
        self.views.retain(|_, v| v.last_used.elapsed() < Duration::from_secs(6));
        // One decoder per clip, keyed by its address: the songs of an album share its
        // sleeve, and a view that restarted for each of them would never show a frame.
        // The strip and the player ask for different sizes, and replacing it per caller
        // would restart the clip every frame: a bigger view takes over only when it
        // needs a wider decode.
        let key = url.to_owned();
        let stale = self.views.get(&key).map_or(true, |v| v.width < max_width);
        if stale {
            // At the cap, the least recently used view makes way for the new one.
            if self.views.len() >= MOTION_VIEWS
                && let Some(oldest) = self.views.iter().min_by_key(|(_, v)| v.last_used).map(|(k, _)| k.clone())
            {
                self.views.remove(&oldest);
            }
            self.views.insert(key.clone(), MotionView::new(crate::motion::play(ctx, url, max_width), max_width));
        }
        let view = self.views.get_mut(&key)?;
        view.last_used = std::time::Instant::now();
        let tex = view.texture(ctx)?;
        Some(widgets::MotionSprite { id: tex.id(), size: tex.size_vec2() })
    }

    /// Put the sleeves down: decoders stopped, their frames and textures released.
    fn clear(&mut self) {
        self.views.clear();
    }
}

/// What the music-video player has of the current song. Dropping the `MotionView` stops its
/// decoder, so the picture ends with the mode.
#[derive(Default)]
pub struct Video {
    /// The song a stream was asked for; an answer about a different one is dropped.
    asked: Option<String>,
    /// YouTube Music's answer for `asked`, waiting for the player to take it up.
    stream: Option<crate::innertube::Video>,
    /// Asked, and YouTube Music has nothing for this song: the artwork carries the player.
    none: bool,
    /// The picture running, and the song it belongs to.
    view: Option<MotionView>,
    id: Option<String>,
    /// Where in the song the current picture was started: until the clip runs past it,
    /// it is still catching up with the music and the two aren't compared.
    anchor: Duration,
    /// Times the clip has been restarted to catch the song up. A video shorter than its
    /// song can be chased once or twice, and no further than that.
    anchors: u8,
    /// Where the song was on the last frame, so the listener's own seeking can be told
    /// apart from the clip falling behind.
    song: Duration,
    /// When the player last said "no music video" — the note is shown for three seconds
    /// of a song, then stops, so a song that has nothing doesn't keep saying it.
    missed: Option<std::time::Instant>,
}

/// What the music-video player does about its picture on a given frame.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// Nothing is running for this song: take up the answer and start it.
    Start,
    /// Leave it running exactly as it is.
    Stay,
    /// It has come apart from the song: run it from where the music is now.
    Again,
    /// Restarting hasn't helped either: the artwork carries the player from here.
    End,
}

impl Video {
    /// The rule the picture is held to, given one that is (`running`) or is not yet in
    /// place. `clip` and `song` are how far each has got, and `anchor` is the point in
    /// the song the clip was started at: until the clip runs past it, it is still inside
    /// the download and the skip, so the two aren't compared. A paused song holds the
    /// picture, so it can't be out of step either. Past a second and a half apart the
    /// clip is restarted, and three restarts is as far as that is chased — a video
    /// shorter than its song, or one with a lead-in of its own, would otherwise blink.
    /// `dead` is a clip whose thread has finished without ever giving a frame: it isn't
    /// catching up, it never arrived, so it is chased whatever its clock says.
    fn step(&self, running: bool, playing: bool, clip: Duration, song: Duration, dead: bool) -> Step {
        if !running {
            return Step::Start;
        }
        if !playing || (!dead && (clip <= self.anchor || (clip.as_secs_f64() - song.as_secs_f64()).abs() < 1.5)) {
            return Step::Stay;
        }
        if self.anchors >= 3 {
            Step::End
        } else {
            Step::Again
        }
    }

    /// Whether the song has just been moved by its listener: further than a second and a
    /// half from where the last frame left it. Their own seeking is not the picture
    /// falling behind, so it costs the chase nothing, and a clip that gave up on the song
    /// gets its try back from the new place in it.
    fn seeked(&mut self, song: Duration) -> bool {
        let moved = (song.as_secs_f64() - self.song.as_secs_f64()).abs() > 1.5;
        self.song = song;
        moved
    }

    /// The mode is on and the picture is still on its way: YouTube Music has a clip of the
    /// song, and the decoder has given no frame of it yet.
    fn loading(&self) -> bool {
        !self.none && self.view.as_ref().is_none_or(|v| v.motion.latest().is_none())
    }

    /// The picture on screen, once the decoder has given a frame.
    fn frame(&mut self, ctx: &egui::Context) -> Option<widgets::MotionSprite> {
        let tex = self.view.as_mut()?.texture(ctx)?;
        Some(widgets::MotionSprite { id: tex.id(), size: tex.size_vec2() })
    }

    /// The same frame out of focus, for the column of words to sit over.
    fn blurred(&mut self, ctx: &egui::Context) -> Option<widgets::MotionSprite> {
        let tex = self.view.as_mut()?.blurred(ctx)?;
        Some(widgets::MotionSprite { id: tex.id(), size: tex.size_vec2() })
    }

    /// The mode is off: the decoder stops, but the answer and the file it downloaded stay, so
    /// pressing the key again on the same song is instant. A picture that gave up trying gets
    /// its try back; one YouTube Music never had stays missing.
    fn park(&mut self) {
        (self.view, self.id, self.anchors) = (None, None, 0);
        if self.stream.is_some() {
            (self.none, self.missed) = (false, None);
        }
    }

    /// Say once that there's nothing to show, and when. Being asked again on the same song
    /// doesn't move the mark, so the note doesn't stay lit while the player keeps looking.
    fn miss(&mut self) {
        if self.missed.is_none() {
            self.missed = Some(std::time::Instant::now());
        }
    }

    /// The "no music video" note's own light: a full three seconds, then out with it. The
    /// artwork is left to carry the player in silence, rather than a reason under it.
    fn missing(&self) -> f32 {
        match self.missed {
            None => 0.0,
            Some(t) => ((3.0 - t.elapsed().as_secs_f64()) * 2.0).clamp(0.0, 1.0) as f32,
        }
    }
}

/// Spotify's looping canvas for the song playing: asked for once per song, decoded like a
/// music video and drawn over the sleeve. Session only — the clip is on disk by name.
#[derive(Default)]
pub struct Canvas {
    /// The song the clip was asked for; an answer about another one is dropped.
    asked: Option<String>,
    /// The clip's file name and address, once Spotify has answered.
    url: Option<(String, String)>,
    view: Option<MotionView>,
}

impl Canvas {
    /// The canvas on screen, once the decoder has given a frame.
    fn frame(&mut self, ctx: &egui::Context) -> Option<widgets::MotionSprite> {
        let tex = self.view.as_mut()?.texture(ctx)?;
        Some(widgets::MotionSprite { id: tex.id(), size: tex.size_vec2() })
    }

    /// A new song: the old clip ends, and nothing is asked until asked for.
    fn reset(&mut self) {
        self.view = None;
        self.url = None;
    }
}

/// What can fill the sleeve, in the order the listener set: the first one the song has
/// wins.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Picture {
    Video,
    Canvas,
    Art,
}

impl Picture {
    pub fn key(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Canvas => "canvas",
            Self::Art => "art",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Video => "Music video",
            Self::Canvas => "Spotify canvas",
            Self::Art => "Animated artwork",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Some(match key.trim() {
            "video" => Self::Video,
            "canvas" => Self::Canvas,
            "art" => Self::Art,
            _ => return None,
        })
    }
}

pub struct App {
    backend: Backend,
    audio: AudioHandle,
    ch: Channels,
    media: Option<Media>,
    discord: Option<crate::discord::Discord>,
    discord_enabled: bool,
    /// Last state sent to Discord, to send only real changes.
    discord_sent: Option<crate::discord::Presence>,
    /// How the Discord card is worded and what it carries.
    discord_prefs: crate::discord::Prefs,
    /// The Last.fm account scrobbles go to; a sign-in in progress and its outcome.
    lastfm_user: Option<String>,
    lastfm_busy: Arc<AtomicBool>,
    lastfm_result: Arc<std::sync::Mutex<Option<Result<String, String>>>>,
    /// The play generation Last.fm last heard about, and whether it was scrobbled.
    scrobble: Option<(u64, bool)>,
    /// Last session's song, restored paused: Play starts it from here.
    resume_at: Option<Duration>,
    /// That song came from another device's queue, so playing it here is a new play.
    resume_counts: bool,
    /// When something last played here (unix seconds), to tell whether another
    /// device has played since. When the cross-device queue was last asked for (`now`).
    listened_at: u64,
    queue_checked: Option<f64>,
    was_focused: bool,
    /// Album / playlist / artist the queue was started from, so its card shows pause.
    queue_source: Option<Target>,
    /// Asked to play (shuffled?) before its songs are here: a playlist's first page, or
    /// an album's or artist's songs (`Req::PlayCollection`).
    play_pending: Option<(Target, bool)>,
    bg: Option<Background>,

    // navigation
    page: Page,
    back: Vec<Page>,
    forward: Vec<Page>,
    page_changed: f64,
    loading: bool,
    /// The current page's request failed with nothing cached: show a retry, not a skeleton.
    load_failed: bool,
    search_text: String,
    /// The search page's scope: false searches Apple Music online, true searches only
    /// what's already here (library, uploads and history).
    search_scope: bool,
    /// The route query the box was last filled from, so clearing it isn't undone.
    search_synced: String,
    /// Past searches, newest first, for the search landing.
    recent_searches: Vec<RecentSearch>,
    /// A search whose results are on screen, waiting for the first result to be opened.
    search_pending: bool,
    suggestions: (String, Vec<crate::ytm::Suggestion>),
    /// Up to 32 query results, oldest first, scoped to the current suggestion round.
    suggest_cache: VecDeque<(String, Vec<crate::ytm::Suggestion>)>,
    /// The suggestion picked with the arrow keys, which Enter searches.
    suggest_sel: Option<usize>,
    /// Bumped when a past search is removed or the account changes: suggestions
    /// requested before that are dropped when they arrive, rather than bring it back.
    suggest_round: u64,
    /// Removals sent and not yet confirmed: the list is asked for again once they're done.
    forgets_pending: u32,
    suggest_due: Option<f64>,

    // page data
    home: Option<HomeState>,
    home_loading: bool,
    /// Explore's moods, which Home's mood tile plays as a station.
    moods: Vec<crate::ytm::Mood>,
    moods_loading: bool,
    /// The mood Home offers now: one of `moods`, chosen when they arrive and kept for
    /// the run, so the tile doesn't change under the pointer.
    mood: Option<crate::ytm::Mood>,
    /// A mood station is being built: its tile doesn't answer twice while it loads.
    mood_loading: bool,
    explore: Option<Explore>,
    /// The section whose name was clicked: everything it holds, on its own page.
    see_all: Option<Shelf>,
    search: Option<SearchResults>,
    /// Albums and playlists opened lately, so going back to one shows it at once (and
    /// refreshes it quietly), and their loads under way.
    lists: Lists,
    artist: Option<ArtistPage>,
    library: Option<Library>,
    /// When the library was last requested; the sidebar spinner gives up after a while.
    library_requested: std::time::Instant,
    history: Option<Vec<Track>>,
    uploads: Option<Vec<Track>>,
    /// Files chosen in the upload dialog, handed over from its thread.
    picked_uploads: Arc<std::sync::Mutex<Option<Vec<std::path::PathBuf>>>>,
    liked: HashSet<String>,
    /// Songs whose unlike is queued and hasn't gone through yet.
    unliking: HashMap<String, Track>,
    selection: Selection,
    account_name: Option<String>,
    account_avatar: Option<String>,
    /// The signed-in account's channels and which one is used, once Settings asked.
    channels: Option<crate::backend::Channels>,
    /// When they were last asked for (`ui.input` time).
    channels_asked: Option<f64>,
    /// The channel being switched to, named for the toast once it's done.
    switching_to: Option<String>,
    /// Last launch's playlists, shown in the sidebar until the library arrives.
    sidebar_cache: Option<Vec<Card>>,

    // playback
    queue: Queue,
    generation: u64,
    buffering: bool,
    /// Unplayable songs skipped in a row; a run of them stops playback instead of
    /// racing through the queue.
    skip_streak: u32,
    /// Times the current song was reopened after its download failed.
    recoveries: u32,
    autoplay: bool,
    /// Turn loud tracks down to YouTube's loudness target.
    normalize: bool,
    log_volume: bool,
    high_quality: bool,
    /// Send lossless tracks straight to the selected DAC in WASAPI exclusive mode.
    bit_perfect: bool,
    /// Play an official music video as its song version when one is found.
    play_songs: bool,
    /// A song whose own name says AI made it is skipped rather than played.
    skip_ai: bool,
    /// Ask YouTube Music for the music video at up to 1080p rather than 720p.
    video_hd: bool,
    /// The song's music video stands behind the player as it opens, rather than only
    /// when the V key is pressed.
    video_bg: bool,
    /// The order each library tab puts its rows in.
    lib_sort: LibSort,
    /// The Songs tab's rows as indices into the liked list, kept so a few thousand
    /// songs aren't sorted again on every frame. Tagged with the sort and the list it
    /// was made from; anything else and it's rebuilt.
    lib_rows: Option<(String, Vec<usize>)>,
    /// The song version playing in place of the current music video.
    playing_as: Option<PlayingAs>,
    /// The output device picked in settings (cpal's id and its name, for when it's not
    /// connected), and the devices last listed there.
    output_device: Option<(String, String)>,
    output_devices: Vec<crate::audio::OutputDevice>,
    radio_pending: bool,
    seek_drag: Option<f32>,

    // visuals
    art_key: Option<String>,
    art_changed: f64,
    /// The playing song's blurred sleeve, kept for the frosted bars (`shell::frost`).
    frost_art: Option<(String, Vec<u8>)>,
    /// The opened page's own blurred sleeve, keyed by the page it belongs to: an album
    /// or playlist washes its background with the cover it shows.
    page_art: Option<(String, Vec<u8>)>,
    /// Which page asked for its sleeve and when, so a silent failure is asked again.
    page_art_asked: Option<(String, f64)>,
    /// The desktop as it looks behind the mini card, blurred: the card's ground while
    /// mini mode is on.
    backdrop: backdrop::Backdrop,
    /// Apple's page for each artist visited (their display name and hero clips).
    apple_artists: std::collections::HashMap<String, Option<crate::apple::Artwork>>,
    /// Apple's moving artwork: which releases have a sleeve, which lookups are in flight
    /// or cooling, and the decoders running. All one field, so a page can ask its tiles
    /// for sleeves while its own feed — a different field — stays borrowed as it draws.
    apple: AppleArt,
    /// Apple's browse shelves, by the page path they came from (`new`, `radio`).
    apple_shelves: std::collections::HashMap<String, Vec<crate::apple::Shelf>>,
    /// Apple browse pages asked for and not yet answered.
    apple_feeds: std::collections::HashSet<String>,
    /// Apple's own collection pages, by the address they came from.
    apple_collections: std::collections::HashMap<String, crate::apple::Collection>,
    /// Apple collection pages asked for and not yet answered.
    apple_collection_waiting: std::collections::HashSet<String>,
    /// Settings › Artist pages: the listener's own signature service, or empty.
    signature_api: String,
    /// Signatures already uploaded, by name key (`artist_signature::key`).
    signature_tex: std::collections::HashMap<String, egui::TextureHandle>,
    /// Signatures fetched but not yet uploaded, keyed the same way.
    signature_images: std::collections::HashMap<String, (usize, usize, Vec<u8>)>,
    /// Names the service had nothing for, and when: asked again after a minute.
    signature_missed: std::collections::HashMap<String, std::time::Instant>,
    /// Signature lookups already in flight.
    signature_waiting: std::collections::HashSet<String>,
    /// Settings › Playback: the listener's hi-res addon links, in the order given.
    hires_sources: Vec<String>,
    /// The "+ Add a source" row is open.
    hires_adding: bool,
    /// What's typed in that row, before it's added.
    hires_draft: String,
    /// Focus the draft row as soon as it opens.
    hires_focus: bool,
    /// What each link was found to serve (or why it can't be used), keyed by the link.
    hires_checks: std::collections::HashMap<String, Result<String, String>>,
    /// Links whose test is still running.
    hires_checking: std::collections::HashSet<String>,
    /// Ask the addons for Dolby Atmos renditions.
    prefer_atmos: bool,
    /// What the playing stream is (codec, rate, depth, size), when a hi-res source
    /// supplied it, with the generation it belongs to.
    audio_info: Option<(u64, crate::hires::Info)>,
    show_nerd_stats: bool,
    stream_stats: Option<(u64, crate::stream::Stats)>,
    upgrading_quality: bool,
    /// The player's "Upgrade quality" was pressed and no hi-res stream has answered yet:
    /// the difference between a search that ran and found nothing and one never started.
    upgrade_asked: bool,
    disable_direct_stream: bool,
    accent: Color32,
    /// Selected theme preset (see `preset.rs`).
    theme_id: String,
    /// Preset whose chrome (glass, corner radius) is installed in the egui style.
    chrome_applied: Option<String>,
    /// Light or dark pages (the ytm-rs theme's; see `light_pages`).
    appearance: theme::Appearance,
    anim_bg: bool,
    page_blobs: bool,
    reactive_bg: bool,
    /// Power saver: the shader blurs behind pages and the player stand down for a flat
    /// ambient tint, which costs an integrated GPU almost nothing.
    pub disable_blur: bool,
    /// Apple's moving album art. Off unless asked for: a sleeve needs a decoder, a
    /// frame and a texture each, and they add up while the music plays.
    animated_artwork: bool,
    /// The Best Part button on the artwork: one touch to where people replay most.
    show_best_part: bool,
    /// The volume slider, in the bar and under the transport. Some keyboards and small
    /// screens want the room back.
    show_volume: bool,
    /// Sing along: tune the centre vocal out of the mix.
    sing: bool,
    /// How much of it to take out, 0..1.
    sing_level: f32,
    /// Whether the marks take the song's colour or keep the app's own.
    icon_tint: bool,
    /// Skip the parts of music videos SponsorBlock marks as not music.
    sponsorblock: bool,
    /// Crossfade between songs, and over how many seconds.
    crossfade: bool,
    /// The equalizer: whether it's on, its bands (dB) and whether it keeps clear of clipping.
    eq_on: bool,
    eq_gains: crate::audio::eq::Gains,
    eq_no_clip: bool,
    /// DJ-style handoff: the next song eases in under the last one (see `audio::Ramp`).
    automix: bool,
    crossfade_secs: u32,
    /// The next song starts because this one is ending: crossfade into it.
    handoff: bool,
    /// The handoff just begun, for the play that starts the next song to pick up.
    pending_mix: Option<MixFrom>,
    /// The song an Automix handoff came from, shown until the next one is louder.
    mix_from: Option<MixFrom>,
    /// SponsorBlock segments for a video, and when one was last skipped.
    segments: Option<(String, Vec<crate::sponsorblock::Segment>)>,
    /// youtube.com's "Most replayed" curve for each song asked about (`None`: the video
    /// has none, which is an answer worth remembering for a week).
    curves: HashMap<String, Option<crate::bestpart::Heatmap>>,
    /// Songs whose curve is already on its way, so a redraw doesn't ask twice.
    curves_asked: std::collections::HashSet<String>,
    /// Where the playing song's best part starts, and which song that is: a late answer
    /// about the last one must not mark this one.
    best_part: Option<(String, crate::bestpart::Start)>,
    /// The playing song's best part has gone past: heard once, the button is put away.
    /// Reset with the song, so playing it again offers the jump again.
    best_part_heard: bool,
    /// YouTube's own word that AI made a song, by video id: the watch page's "Made with
    /// AI" block, asked for when the skip is on and kept for the session.
    ai_flags: HashMap<String, bool>,
    segment_skipped: f64,
    /// Background animation clock: advances only while music plays, so pausing
    /// freezes the motion instead of jumping when resumed.
    bg_time: f64,
    last_frame: f64,
    search_rect: Option<egui::Rect>,
    search_focused: bool,
    /// Give the search box its focus back (removing a past search takes it away).
    search_refocus: bool,
    /// A past search was removed during the current press: the rows below slide up
    /// under the held pointer, and must not go too.
    forget_held: bool,
    /// The past search being removed (its token) and when: it folds away first.
    forgetting: Option<(String, f64)>,
    /// Mouse-wheel distance still to be scrolled (smooth scrolling).
    wheel_pending: egui::Vec2,
    wheel_mods: egui::Modifiers,

    // now playing
    np_open: bool,
    np_tab: NpTab,
    queue_open: bool,
    /// The lyrics drawer beside the page: the words without leaving what you were
    /// looking at, the way the reference's own lyric pane works.
    lyrics_open: bool,
    /// The queue panel has scrolled to the playing row for this panel-and-song pair:
    /// what came before stands above it, to be scrolled back to.
    queue_seed: Option<String>,
    fullscreen: bool,
    /// The music-video player: the song's music video, full screen, with the words beside it.
    video_on: bool,
    video: Video,
    /// Spotify's Canvas setting, and the clip it found for the song playing.
    show_canvas: bool,
    /// The two lines already sung stay above the one being sung (am-lyrics' own sheet).
    pub show_past_lines: bool,
    canvas: Canvas,
    /// The order the sleeve tries its pictures in.
    pub picture_order: [Picture; 3],
    /// The mini player: the window shrunk to a strip.
    mini: mini::Mini,
    /// Every source's lyrics for the current song (by video ID).
    lyrics: Option<(String, LyricsSet)>,
    /// Distinguishes repeated requests for the same song.
    lyrics_generation: u64,
    /// Romanization/translation for one source's lines: (video ID, source key, language, extras).
    lyrics_extras: Option<(String, String, String, crate::translate::Extras)>,
    lyrics_share: Option<lyrics_share::LyricsShare>,
    /// The extras last asked for, so each is requested once.
    extras_asked: Option<(String, String, String)>,
    show_roman: bool,
    show_trans: bool,
    lyric_prefs: lyric_prefs::Prefs,
    lyric_view: karaoke::View,
    /// The drawer has its own scroll, timing and texture state.
    sidebar_lyric_view: karaoke::View,
    lyric_clock: lyric_prefs::Clock,
    /// Rotation of a "record" cover (radians); advances only while playing.
    record_angle: f64,

    // account / misc
    logged_in: bool,
    /// The Spotify transfer, running on the network runtime.
    transfer: Arc<crate::transfer::Transfer>,
    xfer: transfer::Setup,
    /// Listen together: the room we're in, if any.
    jam: jam::Session,
    signing_in: Arc<AtomicBool>,
    toast: Option<(String, f64)>,

    // updates
    updater: Option<crate::update::Updater>,
    /// When the last automatic update check ran (app time, s).
    update_checked: Option<f64>,
    update_announced: bool,
    /// Release notes dialog (update ready, or "See all notes").
    notes_dialog: Option<notes::NotesDialog>,
    /// Version whose "Updated to" card is showing, on the first launch after an update.
    whats_new: Option<String>,
    /// The user picked "Restart to update": install on exit, then relaunch.
    restart_for_update: bool,
    /// Settings › Network: the saved proxy setting (`proxy::active()` is this run's),
    /// the mode picked on the page, the custom address being typed, its error, and
    /// the last connection test (`Some(None)` while one runs).
    proxy_saved: crate::proxy::Setting,
    proxy_mode: crate::proxy::Setting,
    proxy_url: String,
    proxy_error: Option<String>,
    proxy_test: Arc<std::sync::Mutex<ProxyTest>>,
    /// Close and start again (the proxy changed).
    restart_app: bool,
    /// The media controls' cover as a local file (video id, `file://` path), fetched
    /// through the proxy: Windows would otherwise download the URL itself, directly.
    media_cover: Arc<std::sync::Mutex<Option<(String, String)>>>,

    // window
    /// Clicking × hides the window to the tray instead of quitting.
    close_to_tray: bool,
    /// Settings › Window: draw with wgpu (Vulkan) from the next start (renderer.rs).
    pub(super) wgpu_renderer: bool,
    /// The app icon (`appicon::ALL` id) shown on the window and in the tray.
    app_icon: &'static str,
    /// The tray icon, while the window is hidden.
    tray: Option<crate::tray::Tray>,
    /// Closing for real (Quit from the tray, restart to update): don't hide.
    exiting: bool,
    /// Minimized, or hidden in the tray: the window's geometry isn't worth saving.
    window_hidden: bool,
    /// eframe's saved window geometry ("window") from the last time it was visible.
    good_window: Option<String>,
    /// The sidebar is a narrow icon rail.
    sidebar_collapsed: bool,
    /// Which sidebar groups are unfolded.
    library_open: bool,
    playlists_open: bool,
    /// The sidebar playlists' order (by id), and the ones hidden from it.
    playlist_order: Vec<String>,
    hidden_playlists: HashSet<String>,
    /// Artists starred from their page's hero.
    fav_artists: HashSet<String>,
    /// The collection page whose inline search is open, and what's typed in it.
    coll_search: Option<(String, String)>,
    /// What's typed in the box above Settings; the page keeps only what it matches.
    settings_search: String,
    /// A Spotify `sp_dc` cookie typed in but not yet saved. Never written to storage, and
    /// cleared once it reaches the vault.
    sp_token: String,
    /// The keyboard shortcuts, with what the user changed over the defaults.
    keys: keys::Map,
    /// The shortcut row waiting for a key, if one is being recorded.
    keys_edit: Option<keys::Cmd>,
    /// Listen together: the relay to use; empty for the app's own.
    jam_server: String,
    /// This device's ids on the relay. They are derived from the data directory the first
    /// time and kept, so a reconnect joins as the same member rather than a stranger.
    jam_user: String,
    jam_device: String,
    /// Settings shows the Listen together section rather than its list of sections.
    jam_open: bool,
    actions: Vec<Action>,
    dialog: Option<Dialog>,
    share_pending: Option<String>,
    smoke: Option<String>,
}

/// Longest crossfade offered, in seconds.
pub(super) const CROSSFADE_MAX: u32 = 12;

/// The equalizer's bands as saved: ten comma-separated dB values.
fn parse_gains(s: &str) -> Option<crate::audio::eq::Gains> {
    let mut out = [0.0; 10];
    for (o, part) in out.iter_mut().zip(s.split(',')) {
        *o = part.trim().parse().ok()?;
    }
    Some(out)
}

/// The song an Automix handoff came from. Its last stretch is still what you hear
/// while the next one eases in, so the player keeps showing it (with its art and
/// lyrics) until the next song is the louder of the two rather than switching at
/// the first beat of the blend.
pub(super) struct MixFrom {
    pub(super) track: Track,
    /// Its length and the handoff's length, in seconds.
    pub(super) total: f32,
    pub(super) mix: f32,
    /// The audio thread has taken the handoff: the position reads the next song's again.
    pub(super) started: bool,
    /// The player has handed the screen to the next song. The mix keeps running after
    /// that, with this song's tail under it, so the "Mixing" caption stays until its end.
    pub(super) released: bool,
}

impl App {
    pub fn new(
        backend: Backend,
        audio: AudioHandle,
        ch: Channels,
        media: Option<Media>,
        bg: Option<Background>,
        storage: Option<&dyn eframe::Storage>,
    ) -> Self {
        let get = |k: &str| storage.and_then(|s| s.get_string(k));
        // Dev hook: YTM_FONT=<font name> tries a font without saving it.
        theme::set_font_choice(&dev_var("YTM_FONT").map(str::to_owned).or_else(|| get("font")).unwrap_or_default());
        let app_icon = crate::appicon::load(backend.data_dir());
        let wgpu_renderer = crate::renderer::load(backend.data_dir()) == crate::renderer::Renderer::Wgpu;
        // Isolated dev profiles must not rewrite the user's Start menu shortcut.
        if std::env::var_os("YTM_DATA_DIR").is_none() && std::env::var_os("YTM_NO_SHORTCUT_SYNC").is_none() {
            crate::appicon::sync_shortcuts(app_icon, backend.data_dir());
        }
        crate::downloads::init(backend.data_dir(), &get("download_dir").unwrap_or_default());
        crate::lastfm::init(backend.data_dir());
        let logged_in = backend.is_logged_in();
        // Before any request: a song started at launch (dev hooks) must see this setting.
        backend.set_high_quality(get("high_quality").is_none_or(|v| v == "1"));
        // Before any request: a song started at launch must see the hi-res source too.
        let hires_source = get("hires_source").unwrap_or_default();
        let hires_sources: Vec<String> = dedupe_hires(hires_source.lines().map(str::trim).filter(|l| !l.is_empty()));
        backend.set_hires_source(&hires_sources.join("\n"));
        let prefer_atmos = get("prefer_atmos").is_some_and(|v| v == "1");
        backend.set_prefer_atmos(prefer_atmos);
        let show_nerd_stats = get("show_nerd_stats").is_some_and(|v| v == "1");
        let disable_direct_stream = get("disable_direct_stream").is_some_and(|v| v == "1");
        backend.set_disable_direct_stream(disable_direct_stream);
        backend.request(Req::Home(None));
        if logged_in {
            backend.request(Req::Library);
            backend.request(Req::Account);
            backend.warm_signed_in();
        }
        let transfer_handle = {
            let b = backend.clone();
            crate::transfer::Transfer::new(backend.data_dir(), Arc::new(move || b.wake()))
        };
        let discord_prefs = crate::discord::Prefs::load(&get);
        let discord_enabled = get("discord_enabled").is_none_or(|v| v == "1");
        let discord = discord_enabled.then(crate::discord::Discord::spawn).flatten();
        if let Some(d) = &discord {
            d.set_prefs(discord_prefs.clone());
        }
        let mut app = Self {
            backend,
            audio,
            ch,
            media,
            discord,
            discord_enabled,
            discord_sent: None,
            discord_prefs,
            lastfm_user: crate::lastfm::user(),
            lastfm_busy: Default::default(),
            lastfm_result: Default::default(),
            scrobble: None,
            resume_at: None,
            resume_counts: false,
            listened_at: 0,
            queue_checked: None,
            was_focused: false,
            queue_source: None,
            play_pending: None,
            bg,
            page: Page::Home,
            back: Vec::new(),
            forward: Vec::new(),
            page_changed: 0.0,
            loading: false,
            load_failed: false,
            search_text: String::new(),
            search_scope: false,
            search_synced: String::new(),
            recent_searches: get("recent_searches").and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
            search_pending: false,
            suggestions: Default::default(),
            suggest_cache: VecDeque::new(),
            suggest_sel: None,
            suggest_round: 0,
            forgets_pending: 0,
            suggest_due: None,
            home: None,
            home_loading: true,
            moods: Vec::new(),
            moods_loading: false,
            mood: None,
            mood_loading: false,
            explore: None,
            see_all: None,
            search: None,
            lists: Lists::default(),
            artist: None,
            library: None,
            library_requested: std::time::Instant::now(),
            history: None,
            uploads: None,
            picked_uploads: Default::default(),
            liked: HashSet::new(),
            unliking: HashMap::new(),
            selection: Selection::default(),
            account_name: None,
            account_avatar: None,
            channels: None,
            channels_asked: None,
            switching_to: None,
            sidebar_cache: None,
            queue: Queue::new(match get("repeat").as_deref() {
                Some("all") => Repeat::All,
                Some("one") => Repeat::One,
                _ => Repeat::Off,
            }),
            generation: 0,
            buffering: false,
            skip_streak: 0,
            recoveries: 0,
            autoplay: get("autoplay").is_none_or(|v| v == "1"),
            normalize: get("normalize").is_none_or(|v| v == "1"),
            log_volume: get("log_volume").is_none_or(|v| v == "1"),
            high_quality: get("high_quality").is_none_or(|v| v == "1"),
            bit_perfect: get("bit_perfect").is_some_and(|v| v == "1"),
            play_songs: get("play_songs").is_none_or(|v| v == "1"),
            skip_ai: get("skip_ai").is_some_and(|v| v == "1"),
            // 720p by default: a clip is downloaded whole before its first frame, and the
            // 1080p file is three times the bytes for a picture the sleeve scales anyway.
            video_hd: get("video_hd").is_some_and(|v| v == "1"),
            video_bg: get("video_bg").is_some_and(|v| v == "1"),
            lib_sort: LibSort::read(get("lib_sort").as_deref()),
            lib_rows: None,
            playing_as: None,
            output_device: get("output_device").filter(|d| !d.is_empty()).map(|id| (id, get("output_device_name").unwrap_or_default())),
            output_devices: Vec::new(),
            updater: None,
            close_to_tray: get("close_to_tray").is_some_and(|v| v == "1"),
            wgpu_renderer,
            app_icon,
            tray: None,
            exiting: false,
            window_hidden: false,
            good_window: get("window"),
            sidebar_collapsed: get("sidebar_collapsed").is_some_and(|v| v == "1"),
            library_open: true,
            playlists_open: true,
            playlist_order: get("playlist_order").map(|v| v.lines().filter(|l| !l.is_empty()).map(str::to_owned).collect()).unwrap_or_default(),
            hidden_playlists: get("hidden_playlists").map(|v| v.lines().filter(|l| !l.is_empty()).map(str::to_owned).collect()).unwrap_or_default(),
            fav_artists: get("fav_artists").map(|v| v.lines().filter(|l| !l.is_empty()).map(str::to_owned).collect()).unwrap_or_default(),
            coll_search: None,
            settings_search: String::new(),
            sp_token: String::new(),
            keys: keys::Map::default(),
            keys_edit: None,
            jam_server: String::new(),
            jam_user: String::new(),
            jam_device: String::new(),
            jam_open: false,
            update_checked: None,
            update_announced: false,
            notes_dialog: None,
            // First launch of a newer version than last time: show what changed.
            // YTM_WHATS_NEW=1 shows the card for this version regardless.
            whats_new: {
                let current = crate::update::current_version();
                let last = get("last_version");
                (std::env::var_os("YTM_WHATS_NEW").is_some() || last.is_some_and(|v| crate::update::newer(&current, &v))).then_some(current)
            },
            restart_for_update: false,
            proxy_saved: crate::proxy::active().clone(),
            proxy_mode: crate::proxy::active().clone(),
            proxy_url: match crate::proxy::active() {
                crate::proxy::Setting::Custom(u) => u.clone(),
                _ => String::new(),
            },
            proxy_error: None,
            proxy_test: Default::default(),
            restart_app: false,
            media_cover: Default::default(),
            radio_pending: false,
            seek_drag: None,
            art_key: None,
            frost_art: None,
            page_art: None,
            page_art_asked: None,
            backdrop: Default::default(),
            apple_artists: Default::default(),
            apple: Default::default(),
            apple_shelves: Default::default(),
            apple_feeds: Default::default(),
            apple_collections: Default::default(),
            apple_collection_waiting: Default::default(),
            signature_api: get("signature_api").unwrap_or_default(),
            signature_tex: Default::default(),
            signature_images: Default::default(),
            signature_missed: Default::default(),
            signature_waiting: Default::default(),
            hires_sources,
            hires_adding: false,
            hires_draft: String::new(),
            hires_focus: false,
            hires_checks: Default::default(),
            hires_checking: Default::default(),
            prefer_atmos,
            audio_info: None,
            show_nerd_stats,
            stream_stats: None,
            upgrading_quality: false,
            upgrade_asked: false,
            disable_direct_stream,
            art_changed: -10.0,
            accent: theme::RED,
            theme_id: crate::preset::DEFAULT_ID.to_owned(),
            chrome_applied: None,
            // Dev hook: YTM_APPEARANCE=light|dark|system tries a mode without saving it.
            appearance: theme::Appearance::from_key(&dev_var("YTM_APPEARANCE").map(str::to_owned).or_else(|| get("appearance")).unwrap_or_default()),
            anim_bg: get("anim_bg").is_none_or(|v| v == "1"),
            page_blobs: get("page_blobs").is_some_and(|v| v == "1"),
            reactive_bg: get("reactive_bg").is_none_or(|v| v == "1"),
            disable_blur: get("disable_blur").is_some_and(|v| v == "1"),
            animated_artwork: get("animated_artwork").is_none_or(|v| v == "1"),
            show_best_part: get("show_best_part").is_none_or(|v| v == "1"),
            show_volume: get("show_volume").is_none_or(|v| v == "1"),
            // Never restored: singing along is something the listener asks for, so the
            // karaoke cut starts off every launch even if it was left on when last quit.
            sing: false,
            sing_level: 1.0,
            icon_tint: get("icon_tint").is_none_or(|v| v == "1"),
            sponsorblock: get("sponsorblock").is_some_and(|v| v == "1"),
            crossfade: get("crossfade").is_some_and(|v| v == "1"),
            eq_on: get("eq").is_some_and(|v| v == "1"),
            eq_gains: get("eq_gains").and_then(|v| parse_gains(&v)).unwrap_or_default(),
            eq_no_clip: get("eq_no_clip").is_none_or(|v| v == "1"),
            automix: get("automix").is_some_and(|v| v == "1"),
            handoff: false,
            pending_mix: None,
            mix_from: None,
            crossfade_secs: get("crossfade_secs").and_then(|v| v.parse().ok()).unwrap_or(5).clamp(1, CROSSFADE_MAX),
            segments: None,
            curves: Default::default(),
            curves_asked: Default::default(),
            best_part: None,
            best_part_heard: false,
            ai_flags: HashMap::new(),
            segment_skipped: f64::NEG_INFINITY,
            bg_time: 0.0,
            last_frame: 0.0,
            search_rect: None,
            search_focused: false,
            search_refocus: false,
            forget_held: false,
            forgetting: None,
            wheel_pending: egui::Vec2::ZERO,
            wheel_mods: egui::Modifiers::NONE,
            np_open: false,
            np_tab: if get("np_art").is_some_and(|v| v == "1") { NpTab::Art } else { NpTab::Lyrics },
            queue_open: false,
            lyrics_open: false,
            queue_seed: None,
            fullscreen: false,
            video_on: false,
            video: Default::default(),
            // Off unless asked for: a clip comes from Spotify, so the listener opts in.
            show_canvas: get("show_canvas").is_some_and(|v| v == "1"),
            show_past_lines: get("show_past_lines").is_none_or(|v| v == "1"),
            canvas: Default::default(),
            picture_order: {
                let mut order: Vec<Picture> = get("picture_order")
                    .unwrap_or_else(|| "video,canvas,art".to_owned())
                    .split(',')
                    .filter_map(Picture::from_key)
                    .collect();
                for p in [Picture::Video, Picture::Canvas, Picture::Art] {
                    if !order.contains(&p) {
                        order.push(p);
                    }
                }
                [order[0], order[1], order[2]]
            },
            mini: mini::Mini::load(get),
            lyrics: None,
            lyrics_generation: 0,
            lyrics_extras: None,
            lyrics_share: None,
            extras_asked: None,
            show_roman: get("show_roman").is_none_or(|v| v == "1"),
            show_trans: get("show_trans").is_none_or(|v| v == "1"),
            lyric_prefs: lyric_prefs::Prefs::load(&get),
            lyric_view: Default::default(),
            sidebar_lyric_view: Default::default(),
            lyric_clock: Default::default(),
            record_angle: 0.0,
            logged_in,
            signing_in: Default::default(),
            transfer: transfer_handle,
            xfer: Default::default(),
            jam: Default::default(),
            toast: None,
            actions: Vec::new(),
            dialog: None,
            share_pending: None,
            smoke: None,
        };
        app.request_liked(false);
        app.audio.send(crate::audio::AudioCmd::SetBitPerfect(app.bit_perfect));
        if app.bit_perfect {
            // Bit-perfect output carries stereo PCM; ask the sources for the stereo copy.
            app.backend.set_prefer_atmos(false);
        }
        // Dev hooks: YTM_SMOKE="query" searches and plays the first song ("id:<videoId>[,<videoId>…]"
        // plays that video); YTM_SEARCH
        // only searches; YTM_PAGE opens a page; YTM_NOWPLAYING opens the overlay.
        if let Some(s) = get(SESSION_KEY).filter(|_| std::env::var_os("YTM_SMOKE").is_none()) {
            app.restore_session(&s);
        }
        let smoke = std::env::var("YTM_SMOKE").ok().filter(|q| !q.is_empty());
        if let Some(ids) = smoke.as_deref().and_then(|q| q.strip_prefix("id:")) {
            // "id:a,b,c" queues several.
            let track = |id: &str| Track { id: id.to_owned(), title: id.to_owned(), artists: vec![], album: None, duration: None, thumbs: vec![], track_nr: None, plays: None, music_video: false, episode: false };
            app.play_list(ids.split(',').map(track).collect(), 0);
        } else if let Some(q) = smoke {
            app.go(Page::Search(q.clone()));
            app.smoke = Some(q);
        } else if let Some(q) = std::env::var("YTM_SEARCH").ok().filter(|q| !q.is_empty()) {
            app.go(Page::Search(q));
        } else if let Some(q) = std::env::var("YTM_SUGGESTIONS").ok().filter(|q| !q.is_empty()) {
            // The dropdown, on screen: the box holds the query and keeps its focus, so
            // the suggestions it earns are there to screenshot.
            app.go(Page::Search(String::new()));
            app.search_text = q;
            app.search_refocus = true;
            // Without the OS's focus there's no gained_focus to arm the ask, so arm it here.
            app.suggest_due = Some(0.0);
        }
        match std::env::var("YTM_PAGE").as_deref() {
            Ok("home") => app.go(Page::Home),
            Ok("new") | Ok("explore") => app.go(Page::New),
            Ok("artists") => app.go(Page::Library(LibTab::Artists)),
            Ok("albums") => app.go(Page::Library(LibTab::Albums)),
            Ok("library") => app.go(Page::Library(LibTab::Playlists)),
            Ok("uploads") => app.go(Page::Library(LibTab::Uploads)),
            Ok("liked") => app.go(Page::Library(LibTab::Songs)),
            Ok("history") => app.go(Page::Library(LibTab::History)),
            Ok("settings") => app.go(Page::Settings),
            Ok("transfer") => app.go(Page::Transfer),
            Ok("jam") => app.go(Page::Settings),
            Ok(p) if p.starts_with("album:") => app.go(Page::Album(p[6..].to_owned())),
            Ok(p) if p.starts_with("artist:") => app.go(Page::Artist(p[7..].to_owned())),
            Ok(p) if p.starts_with("playlist:") => app.go(Page::Playlist(p[9..].to_owned())),
            Ok(p) if p.starts_with("apple:") => app.go(Page::AppleCollection(p[6..].to_owned())),
            _ => {}
        }
        // YTM_UPLOAD=<file> uploads that file as if it had been picked.
        if let Some(f) = std::env::var_os("YTM_UPLOAD") {
            *app.picked_uploads.lock().unwrap() = Some(vec![f.into()]);
        }
        // Dev hook: YTM_PROXY_INPUT=<address> opens Settings › Network on Custom with it typed in.
        if let Some(v) = dev_var("YTM_PROXY_INPUT") {
            app.proxy_mode = crate::proxy::Setting::Custom(String::new());
            app.proxy_url = v.to_owned();
        }
        if std::env::var("YTM_NOWPLAYING").is_ok() {
            app.np_open = true;
        }
        app.jam.name = get("jam_name").unwrap_or_default();
        app.jam_server = get("jam_server").unwrap_or_default();
        app.jam_user = get("jam_user").unwrap_or_default();
        app.jam_device = get("jam_device").unwrap_or_default();
        app.keys = keys::Map::load(get("shortcuts"));
        // Last launch's name, picture and playlists, so they show at once; the account
        // menu and the library replace them when they arrive.
        if app.logged_in
            && let Some(c) = get("account_cache").and_then(|j| serde_json::from_str::<AccountCache>(&j).ok())
        {
            app.account_name = c.name;
            app.account_avatar = c.avatar;
            app.sidebar_cache = Some(c.playlists).filter(|p| !p.is_empty());
        }
        // Dev hook: YTM_QUEUE opens the queue drawer.
        if std::env::var_os("YTM_QUEUE").is_some() {
            app.queue_open = true;
        }
        // Dev hook: YTM_SING engages Sing along with the bar out.
        if std::env::var_os("YTM_SING").is_some() {
            app.sing = true;
            app.np_tab = NpTab::Lyrics;
        }
        jam::dev_hooks(&mut app);
        app.audio.set_normalize(app.normalize);
        app.set_eq();
        app.set_crossfade();
        app.set_sing();
        app
    }

    // ---------------------------------------------------------------- navigation

    fn go(&mut self, page: Page) {
        self.np_open = false;
        if page == self.page {
            // Clicking the page you're on refreshes it.
            self.refresh();
            return;
        }
        // Leaving a page deloads its motion clips: no decoder keeps running unseen.
        self.apple.clear();
        let prev = std::mem::replace(&mut self.page, page);
        self.back.push(prev);
        self.forward.clear();
        self.page_loaded();
    }

    fn nav_back(&mut self) {
        if self.np_open {
            self.np_open = false;
        } else if let Some(p) = self.back.pop() {
            self.apple.clear();
            self.forward.push(std::mem::replace(&mut self.page, p));
            self.page_loaded();
        }
    }

    fn nav_forward(&mut self) {
        if let Some(p) = self.forward.pop() {
            self.apple.clear();
            self.back.push(std::mem::replace(&mut self.page, p));
            self.page_loaded();
        }
    }

    /// Reload the current page's data in place: no page transition, the page stays as
    /// it is until the new data arrives.
    fn refresh(&mut self) {
        let changed = self.page_changed;
        self.page_loaded();
        self.page_changed = changed;
    }

    /// Start the page transition and (re)load the page's data. Every page refreshes on
    /// each visit (things change outside the app: likes, other devices, new releases),
    /// showing the cached copy meanwhile.
    fn page_loaded(&mut self) {
        self.page_changed = -1.0; // stamped with the frame time on the next pass
        // The room opened from Listen is left behind with the page; the rail keeps the
        // category you last chose.
        self.jam_open = false;
        self.sp_token.clear();
        // The search box holds the query of the search page you're on, and is empty elsewhere.
        self.search_synced = match &self.page {
            Page::Search(q) => q.clone(),
            _ => String::new(),
        };
        self.search_text = self.search_synced.clone();
        self.suggestions = Default::default();
        self.suggest_sel = None;
        // Choosing a transfer match lasts while searching; any other page ends it.
        if !matches!(self.page, Page::Search(_)) {
            self.xfer.picking = None;
        }
        if let Some(id) = self.page_list().map(str::to_owned) {
            self.lists.move_to_front(&id);
        }
        // The landing is the empty search: Apple's category bricks, and nothing else.
        if matches!(&self.page, Page::Search(q) if q.trim().is_empty()) {
            self.ask_apple_feed("gb/search");
        }
        let req = match &self.page.clone() {
            Page::Home if !self.home_loading => {
                self.home_loading = true;
                // The mood tile needs Explore's mood list, which comes once per run.
                if self.moods.is_empty() && !self.moods_loading {
                    self.moods_loading = true;
                    self.backend.request(Req::Moods);
                }
                Some(Req::Home(None))
            }
            Page::New => Some(Req::Explore(self.preferred_artist_ids())),
            // The empty search is the page's landing state: nothing to ask for.
            Page::Search(q) if !q.trim().is_empty() && !self.search_scope && self.search.as_ref().is_none_or(|s| &s.query != q) => Some(Req::Search(q.clone())),
            Page::Album(id) | Page::Playlist(id) => {
                if id != LIKED_MUSIC || self.logged_in {
                    self.visit_list(id, matches!(self.page, Page::Album(_)));
                }
                None
            }
            Page::Artist(id) => Some(Req::Artist(id.clone())),
            Page::AppleCollection(url) => {
                self.ask_apple_collection(url);
                None
            }
            Page::Library(LibTab::History) if self.logged_in => Some(Req::History),
            Page::Library(LibTab::Uploads) if self.logged_in => Some(Req::Uploads),
            Page::Library(LibTab::Songs) => {
                if self.logged_in {
                    self.visit_list(LIKED_MUSIC, false);
                }
                None
            }
            Page::Library(_) if self.logged_in => Some(Req::Library),
            _ => None,
        };
        // The spinner is only for a page with nothing to show yet.
        let cached = match &self.page {
            Page::Home => self.home.is_some(),
            Page::New => self.explore.is_some(),
            Page::Album(id) | Page::Playlist(id) => self.opened(id).is_some(),
            Page::Artist(id) => self.artist.as_ref().is_some_and(|a| &a.id == id),
            Page::AppleCollection(url) => self.apple_collections.contains_key(url),
            Page::Library(LibTab::History) => self.history.is_some(),
            Page::Library(LibTab::Uploads) => self.uploads.is_some(),
            Page::Library(LibTab::Songs) => self.opened(LIKED_MUSIC).is_some(),
            Page::Library(_) => self.library.is_some(),
            _ => false,
        };
        let coming = self.page_list().is_some_and(|id| self.lists.loading(id));
        self.loading = (req.is_some() || coming) && !cached;
        self.load_failed = false;
        match req {
            Some(Req::Library) => self.request_library(),
            Some(r) => self.backend.request(r),
            None => {}
        }
    }

    /// Apple's shelves for one of its browse pages, asked once per visit: `path` is
    /// Apple's own (`gb/new`, `gb/search`), so each page keeps its storefront.
    fn ask_apple_feed(&mut self, path: &str) {
        if self.apple_shelves.contains_key(path) || !self.apple_feeds.insert(path.to_owned()) {
            return;
        }
        self.backend.request(Req::AppleFeed(path.to_owned()));
    }

    /// Apple's own collection page, asked once per address.
    fn ask_apple_collection(&mut self, url: &str) {
        if self.apple_collections.contains_key(url) || !self.apple_collection_waiting.insert(url.to_owned()) {
            return;
        }
        self.backend.request(Req::AppleCollection { url: url.to_owned() });
    }

    /// The album or playlist the page shows (the Library's liked songs are Liked Music).
    fn page_list(&self) -> Option<&str> {
        match &self.page {
            Page::Album(id) | Page::Playlist(id) => Some(id),
            Page::Library(LibTab::Songs) => Some(LIKED_MUSIC),
            _ => None,
        }
    }

    /// Ask Apple, once, for the motion sleeve of a release. `title` names the song whose
    /// sleeve is wanted and `None` asks for the release `album` itself; `apple` is
    /// Apple's own record id, which is the only way to a playlist's clip.
    fn apple_cover(&mut self, album: &str, artist: &str, title: Option<&str>, apple: Option<&str>) {
        let on = self.animated_artwork;
        self.apple.cover(&self.backend, on, album, artist, title, apple);
    }

    /// The newest frame of that sleeve, started the first time it's seen.
    /// `None` until Apple has answered (or if it has no clip for the release).
    fn apple_sprite(&mut self, ctx: &egui::Context, album: &str, artist: &str, title: Option<&str>, apple: Option<&str>, max_width: u32) -> Option<widgets::MotionSprite> {
        let on = self.animated_artwork;
        self.apple.sprite(ctx, on, album, artist, title, apple, max_width)
    }

    /// Put the motion sleeves down: decoders stopped, their frames and textures released.
    /// What a song costs while it plays is worth handing back once it has ended, or as
    /// soon as the setting says the art shouldn't move.
    fn deload_motion(&mut self) {
        self.apple.clear();
    }

    /// The artist's handwritten signature, from the service in Settings: asked for once
    /// per name, uploaded the frame it arrives. `None` while it's on its way or when
    /// the service has none for them.
    fn signature_texture(&mut self, ctx: &egui::Context, name: &str) -> Option<egui::TextureHandle> {
        let endpoint = crate::artist_signature::endpoint(&self.signature_api);
        let name = crate::artist_signature::canonical(name);
        let canon = crate::artist_signature::key(&name);
        if endpoint.is_empty() || canon.is_empty() {
            return None;
        }
        // The link is part of the key: an answer from a service the field no longer
        // holds is not this artist's signature.
        let key = format!("{endpoint}\u{1}{canon}");
        if let Some(tex) = self.signature_tex.get(&key) {
            return Some(tex.clone());
        }
        if let Some((w, h, rgba)) = self.signature_images.remove(&key) {
            let image = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
            let tex = ctx.load_texture("artist-signature", image, egui::TextureOptions::LINEAR);
            // A few artists' marks at most: the header never shows two at once.
            if self.signature_tex.len() >= 8 {
                self.signature_tex.clear();
            }
            self.signature_tex.insert(key, tex.clone());
            return Some(tex);
        }
        if self.signature_missed.get(&key).is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(60)) {
            return None;
        }
        if self.signature_waiting.insert(key.clone()) {
            self.backend.request(Req::ArtistSignature { key, endpoint, name });
        }
        None
    }

    /// A new link is a different service: what was read from the old one is no longer
    /// evidence.
    pub(super) fn signature_reset(&mut self) {
        self.signature_tex.clear();
        self.signature_images.clear();
        self.signature_missed.clear();
        self.signature_waiting.clear();
    }

    /// What a pasted hi-res link actually serves, asked once and then remembered. `None`
    /// while the test runs. The link is the key, so a retyped link is tested afresh.
    pub(super) fn hires_check(&mut self, link: &str) -> Option<&Result<String, String>> {
        if let Some(verdict) = self.hires_checks.get(link) {
            return Some(verdict);
        }
        if !self.hires_checking.contains(link) {
            self.hires_checking.insert(link.to_owned());
            self.backend.request(Req::CheckHiResSource { key: link.to_owned(), link: link.to_owned() });
        }
        None
    }

    /// The newest frame of a motion clip, started the first time it's seen. The decoders
    /// on screen are capped so old pages don't leave threads behind.
    fn motion_sprite(&mut self, ctx: &egui::Context, url: &str, max_width: u32) -> Option<widgets::MotionSprite> {
        self.apple.clip(ctx, url, max_width)
    }

    fn accent_color(&self) -> Color32 {
        // With the icon tint off, a preset that borrows the cover's colour keeps the app's
        // own red instead; a preset that names its own colour is untouched by the switch.
        let untinted = !self.icon_tint && self.look().accent == "cover";
        let c = if untinted { theme::RED } else { self.look().accent(self.accent) };
        if theme::light() {
            // A pale cover colour would barely show on light pages: deepen it.
            let luma = 0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32;
            if luma > 150.0 {
                return theme::lerp_color(c, Color32::BLACK, ((luma - 150.0) / 105.0 * 0.45).min(0.45));
            }
        }
        c
    }

    /// Whether pages draw light: only the ytm-rs theme has a light mode, and it follows
    /// Settings › Appearance › Mode.
    fn light_pages(&self, ctx: &egui::Context) -> bool {
        self.look().id == crate::preset::DEFAULT_ID && self.appearance.is_light(ctx)
    }

    /// The active theme preset. Synced once per frame (`sync_look`), so this is a plain
    /// index, not a search.
    fn look(&self) -> &'static crate::preset::Preset {
        crate::preset::active()
    }

    fn sync_look(&self) {
        let id = self.theme_id.as_str();
        if crate::preset::active().id != id {
            crate::preset::set_active(id);
        }
    }

    /// Height of the bottom player area: the 52-point pill, the 20 it floats above the
    /// edge on, and a little more for its shadow.
    fn bar_height(&self) -> f32 {
        78.0
    }

    fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), -1.0));
    }


}



impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        dev_pace();
        self.lyric_view.release_hidden(ctx.cumulative_pass_nr());
        self.sidebar_lyric_view.release_hidden(ctx.cumulative_pass_nr());
        let now = ctx.input(|i| i.time);
        self.sync_look();
        self.pump(now);
        self.skip_segments(ctx, now);
        // System media controls and taskbar buttons follow the player however it changed.
        let playing = self.playing() || self.buffering;
        if let Some(m) = &mut self.media {
            m.set_playing(playing);
        }
        self.request_lyric_extras();
        // Upload: files from the picker, or dropped onto the window.
        let mut files = self.picked_uploads.lock().unwrap().take().unwrap_or_default();
        files.extend(ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_owned()).filter(|p| !p.as_os_str().is_empty()).collect::<Vec<_>>()));
        if !files.is_empty() {
            self.upload_files(files);
        }
        transfer::poll(self, ctx, now);
        if crate::deeplink::take_show() {
            self.show_window(ctx);
        }
        for link in crate::deeplink::take() {
            self.show_window(ctx);
            jam::open_link(self, link.code, link.server);
        }
        for a in std::mem::take(&mut self.actions) {
            self.apply(a);
        }
        jam::tick(self, ctx);
        jam::dev_script(self, ctx, now);
        if self.page_changed < 0.0 {
            self.page_changed = now;
        }
        // Dev hook: YTM_NP_TOGGLE_AT=<secs>[,<secs>…] clicks the Lyrics chip at those times.
        if let Some(v) = dev_var("YTM_NP_TOGGLE_AT") {
            let times: Vec<f64> = v.split(',').filter_map(|t| t.parse().ok()).collect();
            let id = egui::Id::unique("dev-np-toggles");
            let done: usize = ctx.data(|d| d.get_temp(id)).unwrap_or(0);
            if let Some(&t) = times.get(done) {
                if now >= t {
                    ctx.data_mut(|d| d.insert_temp(id, done + 1));
                    self.np_tab = if self.np_tab == NpTab::Lyrics { NpTab::Art } else { NpTab::Lyrics };
                } else {
                    ctx.request_repaint_after(Duration::from_secs_f64(t - now));
                }
            }
        }
        fit_window_to_monitor(ctx);
        // Dev hook: YTM_MINI=<secs>[,<secs>…] switches between the mini player and the
        // full window at those times (YTM_MINI=1: the mini player after a second).
        if let Some(v) = dev_var("YTM_MINI") {
            let times: Vec<f64> = v.split(',').filter_map(|t| t.parse().ok()).collect();
            let id = egui::Id::unique("dev-mini");
            let done: usize = ctx.data(|d| d.get_temp(id)).unwrap_or(0);
            if let Some(&t) = times.get(done) {
                if now >= t {
                    ctx.data_mut(|d| d.insert_temp(id, done + 1));
                    self.mini.request(done.is_multiple_of(2));
                } else {
                    ctx.request_repaint_after(Duration::from_secs_f64((t - now).min(0.2)));
                }
            }
        }
        if let Some(on) = self.mini.take_request() {
            self.set_mini(ctx, on, now);
        }
        self.mini.tick(ctx, now);
        self.tray_window(ctx);
        self.window_hidden = self.tray.is_some() || ctx.input(|i| i.viewport().minimized == Some(true));
        self.sync_discord();
        self.sync_lastfm();
        self.sync_server_queue(ctx, now);
        self.tick_updates(ctx, now);
        let np = widgets::NowPlaying {
            song: self.current_id().map(str::to_owned),
            source: self.queue_source.clone(),
            playing: self.playing() || self.buffering,
        };
        ctx.data_mut(|d| d.insert_temp(widgets::NowPlaying::id(), np));
        if let Some((_, t)) = &mut self.toast {
            if *t < 0.0 {
                *t = now;
            } else if now - *t > 4.5 {
                self.toast = None;
            }
        }
        // Frame pacing: idle costs nothing. The background drifts slowly, so ~15 fps
        // reads as smooth on pages; the now-playing view runs at 30 fps (25 on a page
        // showing the playing row's bars), and the synced
        // lyrics ask for every frame while they animate (the karaoke sweep must be
        // smooth). Each egui frame re-lays-out the UI, so frame rate is the main CPU cost.
        let playing = self.playing();
        if self.window_hidden {
            // In the tray or minimized: nothing to draw, only the player bookkeeping.
            if playing || self.buffering {
                ctx.request_repaint_after(Duration::from_millis(250));
            }
        } else if self.np_open && playing && !self.mini.busy() {
            ctx.request_repaint_after(Duration::from_millis(33));
        } else if playing && widgets::equalizer_shown(ctx) {
            // The playing row's bars need a steady, full rate to read as smooth.
            ctx.request_repaint_after(Duration::from_millis(16));
        } else if playing && self.page_blobs && self.anim_bg && !self.disable_blur && shell::pages_bg_moves(self.look()) {
            ctx.request_repaint_after(Duration::from_millis(66));
        } else if playing || self.buffering {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.shortcuts(&ctx);
        let now = ctx.input(|i| i.time);
        let screen = ctx.content_rect();

        self.sync_look();
        let look = self.look();
        // Now playing is dark; once it's most of the way up, so is the player bar under it.
        let np = theme::anim_bool(&ctx, "np-open", self.np_open && self.current().is_some(), 0.42);
        // The picture belongs to the player: it ends when the player goes down, and the
        // decoder stops with it rather than running a clip nothing is showing. What it
        // returns to is the setting, so the V key is a hand on the same light.
        if np <= 0.001 {
            self.video_on = self.video_bg;
        }
        if !self.video_on {
            self.video.park();
        }
        // Dev hook: YTM_VIDEO=<secs> turns the music-video player on from that moment, so
        // a capture shows the picture without a keyboard in the room. Once, so a later
        // hook can still put the words away.
        if dev_var("YTM_VIDEO").and_then(|v| v.parse::<f64>().ok()).is_some_and(|t| now >= t) {
            let done = egui::Id::unique("dev-video-on");
            if !ctx.data(|d| d.get_temp::<bool>(done).unwrap_or(false)) {
                ctx.data_mut(|d| d.insert_temp(done, true));
                self.video_on = true;
                self.np_open = true;
                self.np_tab = NpTab::Lyrics;
            }
        }
        // Dev hook: YTM_FULLSCREEN=<secs> takes the window full screen from that moment,
        // the way the key does, so a capture of the full-screen layout is the real thing.
        if dev_var("YTM_FULLSCREEN").and_then(|v| v.parse::<f64>().ok()).is_some_and(|t| now >= t) && !self.fullscreen {
            self.set_fullscreen(&ctx, true);
        }
        let light = self.light_pages(&ctx) && np < 0.5 && !self.fullscreen;
        theme::set_mode(&ctx, light);
        if self.chrome_applied.as_deref() != Some(look.id.as_str()) {
            if self.chrome_applied.is_none() {
                ctx.add_texture_loader(Arc::new(crate::images::Textures::default()));
            }
            theme::apply_chrome(&ctx, look);
            self.chrome_applied = Some(look.id.clone());
        }
        let accent = self.accent_color();
        ctx.data_mut(|d| d.insert_temp(egui::Id::unique("accent"), accent));
        // The full-screen player always keeps the sung lines: its sheet is the whole
        // picture, and the setting only trims the panels.
        ctx.data_mut(|d| d.insert_temp(egui::Id::unique("lyrics-past"), self.show_past_lines || self.fullscreen));
        let app_style = &look.app;
        // Once now playing has slid all the way up it hides everything but the player
        // bar, so the pages and the full-screen background under it aren't drawn.
        let covered = !self.fullscreen && np >= 1.0;
        let bar_strip = egui::Rect::from_min_max(egui::pos2(screen.left(), screen.bottom() - self.bar_height()), screen.max);
        titlebar::set_fullscreen(&ctx, self.fullscreen);
        // The mini player, or nothing but the background while the window changes size.
        let layout = self.mini.layout(&ctx);
        // The strip's window is cut to the same corner as the artwork it frames; the
        // full window keeps its corners.
        crate::fullscreen::set_rounded((layout == mini::Layout::Mini).then(|| (theme::thumb_radius() + 1.0) * ctx.pixels_per_point()));
        if layout != mini::Layout::Full {
            shell::background(self, ui, screen, now, None);
            if layout == mini::Layout::Mini {
                mini::strip(self, ui, now);
                titlebar::resize_sides(&ctx);
                // "Sign in to like songs" and such: shown here too, not timed out unseen.
                shell::toast(self, &ctx, now);
                self.mini.fade_in(&ctx, now, shell::base_color(self));
            }
            dev_screenshot(&ctx, now);
            return;
        }
        shell::background(self, ui, screen, now, covered.then_some(bar_strip));
        if !self.fullscreen && !covered {
            // The top of the window, down to the search box's row: what's on it comes after.
            titlebar::drag_area(ui, egui::Rect::from_min_size(screen.min, egui::vec2(screen.width(), titlebar::HEIGHT)), "main");
        }
        if !self.fullscreen && covered {
            // Now playing hides the pages: the player bar keeps its full-width place.
            egui::Panel::bottom("player").resizable(false)
                .exact_size(self.bar_height())
                .frame(egui::Frame::new())
                .show_separator_line(false)
                .show(ui, |ui| shell::player_bar(self, ui, now, false));
        }
        if !self.fullscreen && !covered {
            use crate::preset::SidebarStyle;
            let side_alpha = app_style.sidebar_alpha.clamp(0.0, 1.0);
            // The logo's row is 32 points tall.
            let side_margin = egui::Margin { left: 16, right: 16, top: titlebar::top_margin(16, 32.0), bottom: 16 };
            let frost = shell::sidebar_material(light, side_alpha);
            let side_frame = match app_style.sidebar {
                SidebarStyle::Glass => egui::Frame::new().fill(frost).inner_margin(side_margin),
                SidebarStyle::Transparent => egui::Frame::new().inner_margin(side_margin),
                SidebarStyle::Solid => egui::Frame::new().fill(look.solid()).inner_margin(side_margin),
                SidebarStyle::Floating => egui::Frame::new()
                    .fill(frost)
                    .stroke(egui::Stroke::new(1.0, theme::glass(0.08)))
                    .corner_radius(egui::CornerRadius::same(theme::panel_radius().max(12)))
                    .outer_margin(egui::Margin { left: 10, right: 4, top: 10, bottom: 10 })
                    .inner_margin(egui::Margin::symmetric(12, 14)),
            };
            // The sidebar first, so it runs the window's full height: the player floats
            // over the main section only, and the page content scrolls under it.
            egui::Panel::left("nav").resizable(false)
                .exact_size(shell::SIDEBAR_W - (shell::SIDEBAR_W - shell::RAIL_W) * shell::sidebar_collapse(self, &ctx))
                .frame(side_frame)
                .show_separator_line(false)
                .show(ui, |ui| shell::sidebar(self, ui));
            let section = ui.available_rect_before_wrap();
            let q = theme::anim_bool(&ctx, "queue-drawer", self.queue_open && !self.np_open, 0.25);
            if q > 0.01 {
                egui::Panel::right("queue").resizable(false)
                    .exact_size(380.0 * q)
                    .frame(egui::Frame::new().fill(theme::panel(0.42)).inner_margin(egui::Margin::symmetric(14, 16)))
                    .show_separator_line(false)
                    .show(ui, |ui| shell::queue_drawer(self, ui));
            }
            // The words of the song being played, beside the page: opening the lyrics
            // leaves what you were looking at where it was.
            let l = theme::anim_bool(&ctx, "lyrics-drawer", self.lyrics_open && !self.np_open, 0.25);
            if l > 0.01 {
                egui::Panel::right("lyrics").resizable(false)
                    .exact_size(380.0 * l)
                    .frame(egui::Frame::new().fill(theme::panel(0.42)).inner_margin(egui::Margin::symmetric(14, 16)))
                    .show_separator_line(false)
                    .show(ui, |ui| shell::lyrics_drawer(self, ui, now));
            }
            let main_right = ui.available_rect_before_wrap().right();
            egui::CentralPanel::default()
                .frame(egui::Frame::new().inner_margin(egui::Margin {
                    left: 34,
                    // The page's floating scrollbar rides the panel's right edge: the panel
                    // reaches the window side so the bar sits flush with it.
                    right: 0,
                    top: titlebar::top_margin(14, 42.0),
                    bottom: 0,
                }))
                .show(ui, |ui| {
                    shell::top_bar(self, ui);
                    ui.add_space(2.0);
                    pages::page(self, ui, now);
                });
            // The player: a frosted strip over the bottom of the main section, above the
            // pages, so their last rows fade under it instead of stopping at a seam.
            let strip = egui::Rect::from_min_max(egui::pos2(section.left(), screen.bottom() - self.bar_height()), egui::pos2(main_right, screen.bottom()));
            egui::Area::new(egui::Id::new("player-bar"))
                .fixed_pos(strip.min)
                .order(egui::Order::Middle)
                .show(&ctx, |ui| {
                    ui.scope_builder(egui::UiBuilder::new().max_rect(strip).layout(egui::Layout::top_down(egui::Align::Min)), |ui| {
                        shell::player_bar(self, ui, now, true);
                    });
                });
        }
        if np > 0.001 {
            let pages = theme::set_mode(&ctx, false);
            nowplaying::overlay(self, &ctx, now);
            theme::set_mode(&ctx, pages);
        }
        self.draw_dialog(&ctx);
        lyrics_share::show(&ctx, &mut self.lyrics_share);
        notes::card(self, &ctx);
        notes::dialog(self, &ctx);
        if let Some(url) = self.share_pending.take() { ctx.copy_text(url); }
        shell::suggestions(self, &ctx);
        shell::toast(self, &ctx, now);

        // The player keeps the window buttons too: the top right stays the window's.
        titlebar::window_buttons(&ctx);
        titlebar::resize_edges(&ctx);
        // Back from the mini player.
        self.mini.fade_in(&ctx, now, shell::base_color(self));
        // Menus carry the same blurred sleeve as the player bar, under their labels.
        shell::frost_menus(self, &ctx);

        if dev_var("YTM_THEME_TOUR").is_some() {
            self.theme_tour(&ctx, now);
        } else {
            dev_screenshot(&ctx, now);
        }
    }

    /// Dev hook: `YTM_FAKE_POINTER=x,y` parks a synthetic pointer there, for testing
    /// hover behaviour without touching the real mouse.
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        // Dev hook: YTM_FAKE_WHEEL="secs,notches" injects one wheel event, for testing.
        if let Some((at, n)) = dev_var("YTM_FAKE_WHEEL").and_then(|v| {
            let (a, n) = v.split_once(',')?;
            Some((a.parse::<f64>().ok()?, n.parse::<f32>().ok()?))
        }) {
            let fired = egui::Id::unique("dev-wheel-fired");
            let t = raw.time.unwrap_or(0.0);
            if t >= at && !ctx.data(|d| d.get_temp::<bool>(fired).unwrap_or(false)) {
                ctx.data_mut(|d| d.insert_temp(fired, true));
                raw.events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: egui::vec2(0.0, -n),
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::NONE,
                    source: egui::MouseWheelSource::Wheel,
                });
            } else if t < at {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }
        smooth_wheel(self, ctx, raw);
        if let Some((x, y)) = dev_var("YTM_FAKE_POINTER").and_then(|v| {
            let (x, y) = v.split_once(',')?;
            Some((x.trim().parse::<f32>().ok()?, y.trim().parse::<f32>().ok()?))
        }) {
            raw.events.retain(|e| !matches!(e, egui::Event::PointerMoved(_) | egui::Event::PointerGone));
            raw.events.push(egui::Event::PointerMoved(egui::pos2(x, y)));
        }
        // Dev hook: YTM_FAKE_CLICK="secs,x,y" sends one primary click at that point, so a
        // test run can open a menu and screenshot it.
        if let Some((at, x, y)) = dev_var("YTM_FAKE_CLICK").and_then(|v| {
            let mut f = v.split(',');
            Some((f.next()?.trim().parse::<f64>().ok()?, f.next()?.trim().parse::<f32>().ok()?, f.next()?.trim().parse::<f32>().ok()?))
        }) {
            let state = egui::Id::unique("dev-click-state");
            let sent = ctx.data(|d| d.get_temp::<u8>(state)).unwrap_or(0);
            let t = raw.time.unwrap_or(0.0);
            let click = |pressed| egui::Event::PointerButton {
                pos: egui::pos2(x, y),
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            if t < at {
                ctx.request_repaint_after(Duration::from_millis(100));
            } else if sent == 0 {
                ctx.data_mut(|d| d.insert_temp(state, 1));
                raw.events.push(click(true));
                ctx.request_repaint();
            } else if sent == 1 {
                ctx.data_mut(|d| d.insert_temp(state, 2));
                raw.events.push(click(false));
                ctx.request_repaint();
            }
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if std::env::var_os("YTM_MUTE").is_none() {
            storage.set_string("volume", self.audio.volume().to_string());
        }
        let repeat = match self.queue.repeat {
            Repeat::Off => "off",
            Repeat::All => "all",
            Repeat::One => "one",
        };
        storage.set_string("repeat", repeat.into());
        let b = |v: bool| if v { "1" } else { "0" }.to_owned();
        storage.set_string("autoplay", b(self.autoplay));
        storage.set_string("recent_searches", serde_json::to_string(&self.recent_searches).unwrap_or_default());
        storage.set_string("jam_name", self.jam.name.clone());
        storage.set_string("jam_server", self.jam_server.clone());
        storage.set_string("jam_user", self.jam_user.clone());
        storage.set_string("jam_device", self.jam_device.clone());
        storage.set_string("shortcuts", self.keys.json());
        let cache = self.logged_in.then(|| AccountCache {
            name: self.account_name.clone(),
            avatar: self.account_avatar.clone(),
            playlists: self.library.as_ref().map(|l| l.playlists.clone()).or_else(|| self.sidebar_cache.clone()).unwrap_or_default(),
        });
        storage.set_string("account_cache", cache.and_then(|c| serde_json::to_string(&c).ok()).unwrap_or_default());
        storage.set_string("normalize", b(self.normalize));
        storage.set_string("log_volume", b(self.log_volume));
        storage.set_string("high_quality", b(self.high_quality));
        storage.set_string("bit_perfect", b(self.bit_perfect));
        storage.set_string("play_songs", b(self.play_songs));
        storage.set_string("skip_ai", b(self.skip_ai));
        storage.set_string("video_hd", b(self.video_hd));
        storage.set_string("show_canvas", b(self.show_canvas));
        storage.set_string("show_past_lines", b(self.show_past_lines));
        storage.set_string("picture_order", self.picture_order.iter().map(|p| p.key()).collect::<Vec<_>>().join(","));
        storage.set_string("video_bg", b(self.video_bg));
        storage.set_string("lib_sort", self.lib_sort.key().to_owned());
        let folder = crate::downloads::folder();
        storage.set_string("download_dir", if folder == crate::downloads::default_folder() { String::new() } else { folder.display().to_string() });
        let (device, device_name) = self.output_device.clone().unzip();
        storage.set_string("output_device", device.unwrap_or_default());
        storage.set_string("output_device_name", device_name.unwrap_or_default());
        // eframe has just saved the window's geometry. Minimized or in the tray that's
        // 0×0 at -32000, which would reopen as a speck: keep the last real one instead.
        if self.window_hidden {
            if let Some(w) = &self.good_window {
                storage.set_string("window", w.clone());
            }
        } else {
            self.good_window = storage.get_string("window");
        }
        // In the mini player, the full window's place is the one to reopen at.
        self.mini.save(storage);
        if let Some(w) = self.mini.full_window() {
            storage.set_string("window", w.clone());
            self.good_window = Some(w);
        }
        storage.set_string("close_to_tray", b(self.close_to_tray));
        storage.set_string("sidebar_collapsed", b(self.sidebar_collapsed));
        // The sidebar's playlist order, completed with any playlist that appeared since
        // it was last changed (so a drag can place them all).
        if let Some(l) = &self.library {
            for c in &l.playlists {
                if let Target::Playlist(id) = &c.target
                    && !self.playlist_order.contains(id)
                {
                    self.playlist_order.push(id.clone());
                }
            }
        }
        storage.set_string("playlist_order", self.playlist_order.join("\n"));
        storage.set_string("hidden_playlists", self.hidden_playlists.iter().cloned().collect::<Vec<_>>().join("\n"));
        storage.set_string("fav_artists", self.fav_artists.iter().cloned().collect::<Vec<_>>().join("\n"));
        // Pretend versions (dev hook) mustn't decide the real "what's new".
        if std::env::var_os("YTM_UPDATE_AS").is_none() {
            storage.set_string("last_version", crate::update::current_version());
        }
        storage.set_string("discord_enabled", b(self.discord_enabled));
        self.discord_prefs.save(storage);
        if dev_var("YTM_APPEARANCE").is_none() {
            storage.set_string("appearance", self.appearance.key().to_owned());
        }
        if std::env::var_os("YTM_THEME").is_none() {
            storage.set_string("theme", self.theme_id.clone());
        }
        if dev_var("YTM_FONT").is_none() {
            storage.set_string("font", theme::font_choice());
        }
        storage.set_string("anim_bg", b(self.anim_bg));
        storage.set_string("page_blobs", b(self.page_blobs));
        storage.set_string("reactive_bg", b(self.reactive_bg));
        storage.set_string("disable_blur", b(self.disable_blur));
        storage.set_string("animated_artwork", b(self.animated_artwork));
        storage.set_string("show_best_part", b(self.show_best_part));
        storage.set_string("show_volume", b(self.show_volume));
        storage.set_string("sing_level", self.sing_level.to_string());
        storage.set_string("icon_tint", b(self.icon_tint));
        storage.set_string("signature_api", self.signature_api.clone());
        storage.set_string("hires_source", self.hires_sources.join("\n"));
        storage.set_string("prefer_atmos", b(self.prefer_atmos));
        storage.set_string("show_nerd_stats", b(self.show_nerd_stats));
        storage.set_string("disable_direct_stream", b(self.disable_direct_stream));
        storage.set_string("sponsorblock", b(self.sponsorblock));
        storage.set_string("crossfade", b(self.crossfade));
        storage.set_string("eq", b(self.eq_on));
        storage.set_string("eq_gains", self.eq_gains.map(|g| g.to_string()).join(","));
        storage.set_string("eq_no_clip", b(self.eq_no_clip));
        storage.set_string("automix", b(self.automix));
        storage.set_string("crossfade_secs", self.crossfade_secs.to_string());
        storage.set_string("show_roman", b(self.show_roman));
        storage.set_string("show_trans", b(self.show_trans));
        // Background test runs mustn't change the listener's sources or per-song picks.
        if std::env::var_os("YTM_BACKGROUND").is_none() || std::env::var_os("YTM_SAVE_SESSION").is_some() {
            self.lyric_prefs.save(storage);
        }
        storage.set_string("np_art", b(self.np_tab == NpTab::Art));
        // Background test runs leave the user's real "last song" alone unless asked.
        let test_run = std::env::var_os("YTM_BACKGROUND").is_some() && std::env::var_os("YTM_SAVE_SESSION").is_none();
        if let Some(s) = self.session_json().filter(|_| !test_run) {
            storage.set_string(SESSION_KEY, s);
        }
    }

    fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
        if let (Some(bg), Some(gl)) = (&self.bg, gl) {
            bg.destroy(gl);
        }
        // A downloaded update installs only when the user restarted for it; nothing is
        // installed behind their back as the app closes.
        let update_ready = self.updater.as_ref().is_some_and(|u| matches!(u.state(), crate::update::State::Ready { .. }));
        if update_ready {
            // eframe has saved and flushed app.ron by now (its save runs before
            // on_exit), so the backup goes into the update carrying this session's
            // every setting — the launch-time copy could be a whole session old,
            // and a restore from it read as "my addons are gone".
            crate::guard_settings(&self.backend.data_dir());
        }
        if let Some(u) = &self.updater
            && self.restart_for_update
        {
            u.install(true);
        }
        if self.restart_app && !update_ready {
            relaunch();
        }
    }
}

const SESSION_KEY: &str = "session";

/// The cache slot of a release's motion sleeve: a song's sleeve is kept apart from its
/// album's, since Apple answers one and not the other.
fn apple_key(album: &str, artist: &str, title: Option<&str>) -> String {
    match title {
        Some(t) => format!("{album}\u{1}{artist}\u{1}{t}"),
        None => format!("{album}\u{1}{artist}"),
    }
}

/// Start this exe again once this process has saved and exited.
/// One entry per distinct link. Only an exact repeat is dropped: a second link on the
/// same addon host (a refreshed token, or another addon served from there) is a source
/// the user asked for and stays. The backend still talks to each exact link once, so a
/// link pasted twice can't fire duplicate searches and streams at one server.
fn dedupe_hires<'a>(links: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for link in links {
        let link = link.trim();
        if !link.is_empty() && seen.insert(link.to_owned()) {
            out.push(link.to_owned());
        }
    }
    out
}

fn relaunch() {
    let Ok(exe) = std::env::current_exe() else { return };
    log::info!("restarting");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        let script = format!("ping -n 3 127.0.0.1 >NUL & start \"\" \"{}\"", exe.display());
        let _ = std::process::Command::new("cmd").arg("/C").raw_arg(&script).creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new(exe).spawn();
    }
}

/// What the sidebar and the account button show, kept for the next launch.
#[derive(serde::Serialize, serde::Deserialize)]
struct AccountCache {
    name: Option<String>,
    avatar: Option<String>,
    playlists: Vec<Card>,
}

/// What's saved between launches so the player picks up where it left off.
#[derive(serde::Serialize, serde::Deserialize)]
struct Session {
    queue: Vec<Track>,
    index: usize,
    position_ms: u64,
    duration_ms: u64,
    #[serde(default)]
    listened_at: u64,
    /// The song version that was playing for the current music video: the position is its.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    playing_as: Option<Track>,
    /// The shuffle mark as it stood, and the order its shuffle came from, so switching it
    /// off after a restart still hands the list's own order back.
    #[serde(default)]
    shuffled: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    order: Vec<String>,
}

/// The song version playing in place of the music video `listed`, in play `generation`.
struct PlayingAs {
    generation: u64,
    listed: String,
    song: Track,
}


/// A restored window size can exceed the monitor (e.g. saved after maximizing),
/// leaving the player bar partly off-screen. Once at startup, maximize instead.
fn fit_window_to_monitor(ctx: &egui::Context) {
    let done = egui::Id::unique("fit-window-checked");
    if ctx.data(|d| d.get_temp::<bool>(done).unwrap_or(false)) {
        return;
    }
    if std::env::var_os("YTM_BACKGROUND").is_some() {
        // eframe re-applies the saved (often maximized) window state regardless of the
        // builder, so push the test window off-screen here, before it's ever shown.
        ctx.data_mut(|d| d.insert_temp(done, true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(-4000.0, 0.0)));
        // YTM_SIZE=WxH tests other window sizes (e.g. the narrow layout).
        let size = std::env::var("YTM_SIZE").ok().and_then(|v| {
            let (w, h) = v.split_once('x')?;
            Some(egui::vec2(w.parse().ok()?, h.parse().ok()?))
        });
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size.unwrap_or(egui::vec2(1920.0, 1057.0))));
        return;
    }
    let (outer, monitor, maximized) = ctx.input(|i| (i.viewport().outer_rect, i.viewport().monitor_size, i.viewport().maximized));
    let (Some(outer), Some(monitor)) = (outer, monitor) else { return };
    ctx.data_mut(|d| d.insert_temp(done, true));
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, monitor);
    // A saved size below the minimum (closed while minimized saves a tiny one, which
    // eframe restores as-is) would open a window too small to see: start over centred.
    let min = egui::Vec2::from(crate::MIN_SIZE);
    if maximized != Some(true) && (outer.width() < min.x || outer.height() < min.y) {
        let size = egui::Vec2::from(crate::DEFAULT_SIZE).min(monitor * 0.9).max(min);
        log::info!("window {outer:?} is below the minimum size; resetting to {size:?}");
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(((monitor - size) / 2.0).max(egui::Vec2::ZERO).to_pos2()));
        return;
    }
    // A little slack for the invisible resize borders Windows adds around the frame.
    if maximized != Some(true) && !screen.expand(12.0).contains_rect(outer) {
        log::info!("window {outer:?} exceeds monitor {monitor:?}; maximizing");
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
    }
}

/// Smooth scrolling: mouse-wheel notches (line units) are collected and replayed as
/// small pixel steps with an exponential ease-out (~0.3 s glide), like browsers do.
/// Touchpads already report smooth pixel deltas and pass through untouched, as does
/// Ctrl+wheel zoom.
fn smooth_wheel(app: &mut App, ctx: &egui::Context, raw: &mut egui::RawInput) {
    const NOTCH_PX: f32 = 70.0;
    const GLIDE: f32 = 0.06; // time constant, seconds (settles in ~0.18 s)
    let viewport_h = raw.screen_rect.map_or(800.0, |r| r.height());
    raw.events.retain(|e| match e {
        egui::Event::MouseWheel { unit, delta, modifiers, .. } if !modifiers.command && !modifiers.ctrl => {
            let px = match unit {
                egui::MouseWheelUnit::Line => *delta * NOTCH_PX,
                egui::MouseWheelUnit::Page => *delta * viewport_h * 0.8,
                egui::MouseWheelUnit::Point => return true,
            };
            // Reversing direction cancels the remaining glide instead of fighting it.
            if app.wheel_pending.y * px.y < 0.0 || app.wheel_pending.x * px.x < 0.0 {
                app.wheel_pending = egui::Vec2::ZERO;
            }
            app.wheel_pending += px;
            app.wheel_mods = *modifiers;
            false
        }
        _ => true,
    });
    if app.wheel_pending.length() < 0.5 {
        app.wheel_pending = egui::Vec2::ZERO;
        return;
    }
    let dt = raw.predicted_dt.clamp(0.001, 0.05);
    let step = app.wheel_pending * (1.0 - (-dt / GLIDE).exp());
    app.wheel_pending -= step;
    raw.events.push(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: step,
        phase: egui::TouchPhase::Move,
        modifiers: app.wheel_mods,
        source: egui::MouseWheelSource::Wheel,
    });
    ctx.request_repaint();
}

fn shuffled_start(len: usize) -> usize {
    let mut indices: Vec<usize> = (0..len).collect();
    shuffle(&mut indices);
    indices.first().copied().unwrap_or(0)
}

pub use crate::model::shuffle;

/// Whether this is the installed or the portable app (updates only apply there). It
/// can't change while running, and the check touches the disk, so it runs once.
fn updatable() -> bool {
    static UPDATABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *UPDATABLE.get_or_init(|| crate::update::installed() || crate::update::portable_dir().is_some())
}

pub fn fmt_time(secs: u32) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

#[cfg(test)]
mod session_tests {
    use super::*;

    /// The video's picture: where the clip and the song have each got to decides what the player
    /// does next. A clip is started at the song's own place, so until it runs past that
    /// point it is still inside the download and the skip; a held song means a held
    /// picture; and a chase that hasn't worked by the third try ends the picture rather
    /// than blink.
    #[test]
    fn the_picture_holds_its_place() {
        let at = |secs: u64| Duration::from_secs(secs);
        let clip = Video { anchor: at(20), ..Default::default() };
        assert_eq!(clip.step(false, true, at(0), at(20), false), Step::Start, "nothing running yet");
        assert_eq!(clip.step(true, true, at(20), at(240), false), Step::Stay, "still catching up");
        assert_eq!(clip.step(true, true, at(46), at(46), false), Step::Stay, "in step");
        assert_eq!(clip.step(true, true, at(47), at(46), false), Step::Stay, "a second adrift is the picture's own pace");
        assert_eq!(clip.step(true, true, at(60), at(46), false), Step::Again, "a clip behind its song");
        assert_eq!(clip.step(true, false, at(60), at(46), false), Step::Stay, "paused with the song");
        assert_eq!(clip.step(true, true, at(20), at(20), true), Step::Again, "a clip that never gave a frame is chased");
        let tried = Video { anchors: 3, ..clip };
        assert_eq!(tried.step(true, true, at(60), at(46), false), Step::End, "chased, and given up");
        assert_eq!(tried.step(true, true, at(10), at(46), false), Step::Stay, "given up, still catching up");
    }

    /// The listener moving the song is told apart from the clip falling behind, so their
    /// own seeking costs the chase nothing.
    #[test]
    fn a_seek_is_the_listeners_own() {
        let at = |secs: u64| Duration::from_secs(secs);
        let mut clip = Video::default();
        assert!(!clip.seeked(at(1)), "a second of the song playing is not a seek");
        assert!(clip.seeked(at(120)), "the song was moved");
        assert!(!clip.seeked(at(121)), "and it plays on from there");
        assert!(clip.seeked(at(30)), "back is a seek too");
    }

    /// The player says what it is doing where the quality mark stands: only while a clip
    /// is really on its way, and never for a song YouTube Music has none of.
    #[test]
    fn only_a_picture_on_its_way_is_loading() {
        assert!(Video::default().loading(), "asked, and not answered yet");
        assert!(!Video { none: true, ..Default::default() }.loading(), "a song with no clip of its own");
    }

    #[test]
    fn sessions_saved_before_song_versions_still_load() {
        let old = r#"{"queue":[{"id":"a","title":"A","artists":[],"album":null,"duration":null,"thumbs":[],"track_nr":null,"plays":null}],"index":0,"position_ms":5000,"duration_ms":0}"#;
        let s: Session = serde_json::from_str(old).unwrap();
        assert!(!s.queue[0].music_video && s.playing_as.is_none());
        // A song (or a session without a song version) saves as before, so an older
        // version, or a jam guest on one, reads it too.
        let saved = serde_json::to_string(&s).unwrap();
        assert!(!saved.contains("music_video") && !saved.contains("playing_as"), "{saved}");
        let video = Track { music_video: true, ..s.queue[0].clone() };
        let back: Track = serde_json::from_str(&serde_json::to_string(&video).unwrap()).unwrap();
        assert!(back.music_video);
    }

    #[test]
    fn the_settings_box_wants_every_word_it_was_given() {
        let words = |q: &str| q.split_whitespace().map(str::to_lowercase).collect::<Vec<_>>();
        let row = "Crossfade length Blend the end of one song into the start of the next";
        assert!(pages::settings_hit(row, &words("crossfade")));
        assert!(pages::settings_hit(row, &words("length crossfade")));
        assert!(pages::settings_hit(row, &words("CROSSFADE")));
        assert!(!pages::settings_hit(row, &words("crossfade atmos")));
        assert!(pages::settings_hit(row, &words("")));
    }
}
