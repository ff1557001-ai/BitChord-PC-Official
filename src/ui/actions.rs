//! What clicks and keys ask for: the actions pages queue up, and keyboard shortcuts.

use std::time::Duration;

use crate::backend::Req;
use crate::model::{Target, Track};

use super::lists::LIKED_MUSIC;
use super::queue::QueueFill;
use super::{Action, App, Dialog, LibTab, NpTab, Page, jam, keys, nowplaying, shell, transfer};

impl App {
    /// What a list play means for the page it was pressed on: which collection the queue
    /// came from, and — for a playlist still arriving — the loader its rest comes from.
    fn play_context(&mut self, tracks: &[Track]) -> Option<QueueFill> {
        // Play buttons and rows live on the page they play from.
        self.queue_source = match &self.page {
            Page::Album(id) => Some(Target::Album(id.clone())),
            Page::Playlist(id) => Some(Target::Playlist(id.clone())),
            Page::Library(LibTab::Songs) => Some(Target::Playlist(LIKED_MUSIC.into())),
            Page::Artist(id) => Some(Target::Artist(id.clone())),
            _ => None,
        };
        // All of a playlist that's still coming in: its rest follows onto the queue.
        self.page_list().filter(|id| self.receiving(id)).and_then(|id| {
            let o = self.opened(id).filter(|o| o.c.tracks.len() == tracks.len())?;
            Some(QueueFill { id: id.to_owned(), load: o.load, ran_out: false })
        })
    }

    pub(super) fn apply(&mut self, a: Action) {
        // In a party only the host drives, the relay turns these down for us: say so
        // instead of clicking into nothing. Playing the current song stays ours (a pause).
        if self.jam.locked() && jam::refused(self, &a) {
            self.toast("Only the host can change the party");
            return;
        }
        // The first result opened after a search is what Recently Searched remembers.
        self.remember_search_click(&a);
        match a {
            // Clicking the song that's already playing pauses/resumes it, not restart it.
            Action::Play(tracks, i) if tracks.get(i).is_some_and(|t| self.current_id() == Some(t.id.as_str())) && self.resume_at.is_none() => {
                self.toggle()
            }
            Action::Play(tracks, i) => {
                let fill = self.play_context(&tracks);
                self.play_list_filling(tracks, i, fill, None);
            }
            Action::PlayShuffled(tracks) => {
                let fill = self.play_context(&tracks);
                let start = super::shuffled_start(tracks.len());
                self.play_list_filling(tracks, start, fill, Some(true));
            }
            Action::TogglePlay => self.toggle(),
            Action::TransferPick(t) => transfer::use_pick(self, t),
            Action::BitPerfect(on) => self.set_bit_perfect(on),
            // Kept already: at once. A playlist that isn't starts with its first page.
            Action::PlayCollection(Target::Album(id) | Target::Playlist(id), shuffle) if self.opened(&id).is_some() => self.play_opened(id, shuffle),
            Action::PlayCollection(target, shuffle) => {
                self.toast("Starting playback…");
                self.play_pending = Some((target.clone(), shuffle));
                match target {
                    Target::Playlist(id) => self.load_list(&id, false, false),
                    target => self.backend.request(Req::PlayCollection { target }),
                }
            }
            Action::Open(Target::Song(t)) | Action::Radio(t) => {
                self.queue_source = None;
                self.play_list(vec![t.clone()], 0);
                if !self.radio_pending {
                    self.radio_pending = true;
                    self.backend.request(Req::Radio { song_of: self.song_of(&t), seed: t.id });
                }
            }
            Action::Open(Target::Album(id)) => self.go(Page::Album(id)),
            Action::Open(Target::Playlist(id)) => self.go(Page::Playlist(id)),
            Action::Open(Target::Artist(id)) => self.go(Page::Artist(id)),
            // Apple's tiles name nothing YouTube Music knows. A release opens as Apple's
            // own page; a clip or song plays as soon as it's found; a live station is
            // Apple's own stream and plays nowhere else.
            Action::Open(t @ Target::Apple { kind, .. }) => match kind {
                crate::model::AppleKind::Radio => self.toast("Apple's live radio is Apple's own stream, so it can't play here."),
                crate::model::AppleKind::Album | crate::model::AppleKind::Playlist => {
                    let Target::Apple { url, .. } = &t else { return };
                    if url.is_empty() {
                        self.toast("Finding that on YouTube Music…");
                        self.backend.request(Req::AppleLookup(t));
                        return;
                    }
                    self.go(Page::AppleCollection(url.clone()));
                }
                crate::model::AppleKind::Song | crate::model::AppleKind::Video => {
                    self.toast("Finding that on YouTube Music…");
                    self.play_pending = Some((t.clone(), false));
                    self.backend.request(Req::PlayCollection { target: t });
                }
            },
            // The whole of an Apple collection, each song found on the source that has it.
            Action::ApplePlay { target, songs, shuffle } => {
                if songs.is_empty() {
                    return;
                }
                self.toast("Finding those on YouTube Music…");
                self.play_pending = Some((target.clone(), shuffle));
                self.backend.request(Req::AppleSongs { target, songs });
            }
            Action::PlayNext(t) => {
                let start = self.queue.play_next(t);
                self.toast("Playing next");
                if start {
                    self.play_current();
                } else {
                    self.prefetch_next();
                }
            }
            Action::Enqueue(t) => {
                let start = self.queue.enqueue(t);
                self.toast("Added to queue");
                if start {
                    self.play_current();
                }
            }
            Action::Go(p) => self.go(p),
            Action::SeeAll(shelf) => {
                self.see_all = Some(shelf);
                self.go(Page::SeeAll);
            }
            Action::QueueJump(i) => {
                self.play_pending = None;
                self.queue.index = Some(i);
                self.play_current();
            }
            Action::QueueRemove(i) => self.queue.remove(i),
            Action::QueueMove(i, dir) => {
                self.queue.shift(i, dir);
                self.prefetch_next();
            }
            Action::QueuePlayNext(i) => {
                if self.queue.play_next_at(i) {
                    self.toast("Playing next");
                    self.prefetch_next();
                }
            }
            Action::ViewQueue => {
                if self.np_open {
                    self.np_tab = NpTab::UpNext;
                } else {
                    self.lyrics_open = false;
                    self.queue_open = true;
                }
            }
            Action::Seek(secs) => {
                self.seek_to(Duration::from_secs_f32(secs.max(0.0)));
                self.lyric_view.resume();
                self.sidebar_lyric_view.resume();
            }
            Action::Like(t, like) => {
                if !self.logged_in {
                    self.toast("Sign in to like songs");
                    return;
                }
                // Optimistic: update the like state and the cached Liked songs list now.
                self.set_liked(std::slice::from_ref(&t), like);
                self.backend.request(Req::Like { video_id: t.id, like });
            }
            Action::LikeMany(tracks, like) => {
                if !self.logged_in {
                    self.toast("Sign in to like songs");
                    return;
                }
                let tracks: Vec<Track> = tracks.into_iter().filter(|t| self.liked.contains(&t.id) != like).collect();
                if tracks.is_empty() { return; }
                if like {
                    self.set_liked(&tracks, true);
                } else {
                    // Unlikes go a few a second (YouTube Music drops faster ones): each song
                    // leaves the list when its unlike goes through, not all at once.
                    self.unliking.extend(tracks.iter().map(|t| (t.id.clone(), t.clone())));
                }
                if tracks.len() > 10 {
                    // One at a time, then checked: a few hundred take a few minutes.
                    self.toast(format!("{} {} songs…", if like { "Liking" } else { "Removing likes from" }, tracks.len()));
                }
                self.backend.request(Req::LikeMany { video_ids: tracks.into_iter().map(|t| t.id).collect(), like });
            }
            Action::Dislike(t) => {
                if !self.logged_in { self.toast("Sign in to rate songs"); return; }
                self.set_liked(std::slice::from_ref(&t), false);
                self.backend.request(Req::Dislike(t.id));
            }
            Action::Share(target) => {
                self.share_pending = Some(share_url(&target));
                self.toast("Link copied");
            }
            Action::ShareLyrics { video_id, generation, provider } => {
                if self.lyrics_generation != generation || self.shown().is_none_or(|track| track.id != video_id) {
                    self.toast("Song or lyrics changed. Try sharing again.");
                    return;
                }
                let card = self.shown().and_then(|track| {
                    let (id, set) = self.lyrics.as_ref().filter(|(id, _)| *id == video_id)?;
                    let selected = self.lyric_prefs.choose(id, set);
                    let lyrics = set.get(provider.filter(|p| Some(*p) == selected)?)?;
                    let target = &self.lyric_prefs.target;
                    let extras = self.lyrics_extras.as_ref()
                        .filter(|(video, source, lang, _)| video == id && source == lyrics.provider.key() && lang == target)
                        .map(|(_, _, _, extras)| extras);
                    Some(super::lyrics_share::open(track, lyrics, extras, self.show_roman, self.show_trans, target, None))
                });
                if card.is_none() { self.toast("No lyrics available to share, or the source changed. Try again."); }
                self.lyrics_share = card;
            }
            Action::AboutSong(track) => {
                self.backend.request(Req::GeniusAbout(track.clone()));
                self.dialog = Some(Dialog::GeniusAbout { track, description: None });
            }
            Action::AboutAlbum { id, title, artist } => {
                self.backend.request(Req::AppleAlbumAbout { id: id.clone(), title: title.clone(), artist: artist.clone() });
                self.dialog = Some(Dialog::AppleAlbumAbout { id, title, artist, description: None });
            }
            Action::Download(track) => {
                self.toast(format!("Downloading \u{201c}{}\u{201d}", track.title));
                // The song version playing for a music video: that video plays the file too.
                let for_video = if self.playing_as().is_some_and(|s| s.id == track.id) { self.current_id().map(str::to_owned) } else { None };
                self.backend.request(Req::Download { track, for_video });
            }
            Action::AddToPlaylist(tracks) => {
                if self.logged_in { self.dialog = Some(Dialog::PickPlaylist(tracks)); }
                else { self.toast("Sign in to edit playlists"); }
            }
            Action::RemoveFromPlaylist { playlist, items } => {
                if items.is_empty() {
                    self.toast("YouTube Music didn't say where these songs are in the playlist. Reload it and try again.");
                    return;
                }
                let Some(c) = self.current_collection().filter(|c| c.id == playlist && c.owned && !c.is_album) else { return };
                let rows = super::removal_rows(c, &items);
                if rows.is_empty() {
                    self.toast("Those songs are no longer available in the playlist. Reload it and try again.");
                    return;
                }
                let items = super::removal_items(c, rows.iter().copied());
                c.remove_rows(&rows);
                log::debug!("list {playlist}: removing {} entries", items.len());
                self.lists.edits.remove(&playlist, items.iter().map(|(_, set)| set.clone()));
                self.selection.clear();
                self.backend.request(Req::RemoveFromPlaylist { playlist_id: playlist, items });
            }
            Action::NewPlaylist(tracks) => {
                if self.logged_in { self.dialog = Some(Dialog::NewPlaylist { title: String::new(), tracks }); }
                else { self.toast("Sign in to create playlists"); }
            }
            Action::EditPlaylist => {
                if let Some(c) = self.current_collection().filter(|c| !c.is_album) {
                    self.dialog = Some(Dialog::EditPlaylist {
                        id: c.id.clone(), title: c.title.clone(), description: c.description.clone().unwrap_or_default(), privacy: None,
                    });
                }
            }
            Action::Collaborate => {
                if let Some(id) = self.current_collection().filter(|c| !c.is_album && c.owned).map(|c| c.id.clone()) {
                    self.backend.request(Req::Collaboration(id.clone()));
                    self.dialog = Some(Dialog::Collaborate { id, info: None, busy: true, confirm_off: false });
                }
            }
            Action::DeletePlaylist => {
                if let Some(c) = self.current_collection().filter(|c| !c.is_album) {
                    self.dialog = Some(Dialog::ConfirmDelete { id: c.id.clone(), title: c.title.clone() });
                }
            }
            Action::SaveCollection(id, save) => {
                if self.logged_in { self.backend.request(Req::SaveCollection { id, save }); }
                else { self.toast("Sign in to save to library"); }
            }
            Action::HomeChip(params) => {
                if self.home_loading {
                    return;
                }
                self.home_loading = true;
                if self.page != Page::Home {
                    self.go(Page::Home);
                }
                if let Some(h) = &mut self.home {
                    h.loading_more = None;
                    for c in &mut h.chips {
                        c.selected = params.is_some() && c.params == params;
                    }
                }
                self.backend.request(Req::Home(params));
            }
            Action::MoodStation(mood) => {
                if self.mood_loading {
                    return;
                }
                self.mood_loading = true;
                self.toast(format!("Loading {}…", mood.name));
                self.backend.request(Req::MoodStation(mood));
            }
            Action::OpenNowPlaying(open) => {
                if self.current().is_some() {
                    self.np_open = open;
                }
            }
            Action::Mini(on) => self.mini.request(on),
            Action::RevertQuality => {
                // Mark the playing stream's id (a song version differs from the listed id)
                // so resolve skips hi-res for it, then replay at the same position.
                let listed = self.current_id().map(str::to_owned);
                let playing = self.playing_track().map(|t| t.id.clone()).or(listed.clone());
                let Some(id) = playing else { return };
                let at = self.audio.status.position();
                self.backend.revert_quality(&id);
                if let Some(listed) = listed.filter(|l| l != &id) {
                    self.backend.revert_quality(&listed);
                }
                self.play_current_at(at, false);
            }
            Action::UpgradeQuality => {
                let listed = self.current_id().map(str::to_owned);
                let playing = self.playing_track().map(|t| t.id.clone()).or(listed.clone());
                let Some(id) = playing else { return };
                self.backend.upgrade_quality(&id);
                if let Some(listed) = listed.filter(|l| l != &id) {
                    self.backend.upgrade_quality(&listed);
                }
                if self.hires_sources.is_empty() {
                    self.toast("No hi-res source is set. Add one first in Settings › Playback.");
                } else if self.audio_info.is_none() && self.audio.status.wanted_gen.load(std::sync::atomic::Ordering::Relaxed) == self.generation {
                    // Ask for the stream that is playing, in place. The backend reports the
                    // attempt either way, so nothing here claims success on its own.
                    self.upgrade_asked = true;
                    self.backend.try_upgrade_active(self.generation, self.playing_track().cloned());
                } else {
                    let at = self.audio.status.position();
                    self.play_current_at(at, false);
                }
            }
        }
    }

    /// The keyboard shortcuts, from the bindings in Settings › Keyboard shortcuts. A
    /// binding is a key plus the exact modifiers held with it, so `M` and `⌘/Ctrl M` are
    /// two different things.
    pub(super) fn shortcuts(&mut self, ctx: &egui::Context) {
        // A shortcut row is waiting for a key: the settings page takes this press, not the app.
        if self.keys_edit.is_some() {
            return;
        }
        let hits = keys::pressed(ctx, &self.keys);
        // The mouse's back and forward buttons: no keyboard binding covers them.
        let (mb, mf) = ctx.input(|i| (i.pointer.button_pressed(egui::PointerButton::Extra1), i.pointer.button_pressed(egui::PointerButton::Extra2)));
        // The mini player has no pages or now playing to go to, nor does a window on
        // its way back until it's there.
        let strip = self.mini.busy();
        for cmd in hits.into_iter().chain(mb.then_some(keys::Cmd::Back)).chain(mf.then_some(keys::Cmd::Forward)) {
            match cmd {
                keys::Cmd::PlayPause => self.toggle(),
                keys::Cmd::Next => self.next(),
                keys::Cmd::Previous => self.prev(),
                keys::Cmd::VolumeUp | keys::Cmd::VolumeDown => {
                    let v = (self.audio.volume() + if cmd == keys::Cmd::VolumeUp { 0.05 } else { -0.05 }).clamp(0.0, 1.0);
                    self.audio.set_volume(v);
                }
                keys::Cmd::Mute => shell::toggle_mute(self, ctx),
                // The search box keeps its own focus, and asks for it through keys::hit.
                keys::Cmd::Search => {}
                keys::Cmd::Lyrics if !strip && self.current().is_some() => {
                    if nowplaying::lyrics_shown(self, ctx) {
                        self.np_tab = NpTab::Art;
                    } else if self.np_open {
                        nowplaying::ask_lyrics(self, ctx);
                    } else if self.lyrics_open {
                        self.lyrics_open = false;
                    } else {
                        // The words open beside the page, not over it.
                        self.queue_open = false;
                        self.lyrics_open = true;
                        nowplaying::force_lyrics(self, ctx);
                    }
                }
                keys::Cmd::Queue if !strip && self.current().is_some() => {
                    // The player is open, its Up next tab is the queue; otherwise the
                    // bar's drawer, so the shortcut opens a queue wherever you are.
                    if self.np_open {
                        self.np_tab = NpTab::UpNext;
                    } else {
                        self.queue_open = true;
                    }
                }
                keys::Cmd::Video if !strip && self.current().is_some() => {
                    self.video_on = !self.video_on;
                    if self.video_on {
                        // The song's own video, in the player as it stands: full screen
                        // is the listener's to ask for, not the video's to take.
                        self.np_open = true;
                        self.np_tab = NpTab::Lyrics;
                    }
                }
                keys::Cmd::Fullscreen if !strip && self.np_open => self.set_fullscreen(ctx, !self.fullscreen),
                keys::Cmd::Back if !strip => self.nav_back(),
                keys::Cmd::Forward if !strip => self.nav_forward(),
                keys::Cmd::Close if !strip => {
                    // Each layer steps back on its own: full screen first, the window
                    // keeping the video, then the video, then the player itself.
                    if self.fullscreen {
                        self.set_fullscreen(ctx, false);
                    } else if self.video_on {
                        self.video_on = false;
                    } else {
                        self.np_open = false;
                    }
                }
                keys::Cmd::MiniPlayer => self.actions.push(Action::Mini(!self.mini.target())),
                // Held here but nowhere to go: the mini player has no pages, and a song
                // with nothing playing has no lyrics or queue to show.
                _ => {}
            }
        }
    }

    /// Into the mini player or back. Full screen ends first, a frame ahead.
    pub(super) fn set_mini(&mut self, ctx: &egui::Context, on: bool, now: f64) {
        if on == self.mini.active() {
            return;
        }
        if on && self.fullscreen {
            self.set_fullscreen(ctx, false);
            self.mini.request(true);
            return;
        }
        self.mini.toggle(ctx, now);
        // The card reads the screen under it, so it steps out of that copy: a layered window
        // is left behind by a plain screen grab. The full window has nothing to hide from.
        crate::grab::hide_from_grab(on);
        if !on {
            self.backdrop.forget();
        }
    }

    pub(super) fn set_fullscreen(&mut self, ctx: &egui::Context, on: bool) {
        self.fullscreen = on;
        // Windows: in one step, without winit's unthemed-frame flash.
        if !crate::fullscreen::set(on) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
        }
        ctx.request_repaint();
    }
}

fn share_url(target: &Target) -> String {
    match target {
        Target::Song(t) => format!("https://music.youtube.com/watch?v={}", t.id),
        Target::Playlist(id) => format!("https://music.youtube.com/playlist?list={id}"),
        Target::Album(id) => format!("https://music.youtube.com/browse/{id}"),
        Target::Artist(id) => format!("https://music.youtube.com/channel/{id}"),
        // Apple's own page: there isn't a YouTube link until the tile is found there.
        Target::Apple { url, .. } => url.clone(),
    }
}

#[cfg(test)]
mod share_tests {
    use super::*;

    #[test]
    fn shares_youtube_music_urls() {
        let song = Track { id: "abc123".into(), title: String::new(), artists: vec![], album: None, duration: None, thumbs: vec![], track_nr: None, plays: None, music_video: false, episode: false };
        assert_eq!(share_url(&Target::Song(song)), "https://music.youtube.com/watch?v=abc123");
        assert_eq!(share_url(&Target::Playlist("PL123".into())), "https://music.youtube.com/playlist?list=PL123");
        assert_eq!(share_url(&Target::Album("MPRE123".into())), "https://music.youtube.com/browse/MPRE123");
        assert_eq!(share_url(&Target::Artist("UC123".into())), "https://music.youtube.com/channel/UC123");
    }
}
