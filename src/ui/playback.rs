//! Playing the queue: starting songs, Next and Back, pausing and seeking, picking up
//! after a dropped download, SponsorBlock, the art and lyrics that follow a song, and
//! the session kept across launches and devices.

use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::audio::AudioCmd;
use crate::backend::{self, Req};
use crate::lyrics::Lyrics;
use crate::model::Track;

use super::queue::QueueFill;
use super::{App, MixFrom, PlayingAs, Repeat, Session, lyric_prefs};

/// Where in the blend the two songs trade places: the outgoing one holds its level for
/// the first 30% (see `audio::mix_handoff`) and the curves are mirrors, so the next song
/// becomes the louder of the two at 65% of the mix. That's when the player follows it.
const MIX_SWITCH: f32 = 0.65;

impl App {
    pub fn current(&self) -> Option<&Track> {
        self.queue.current()
    }

    /// The lyrics shown for the current song: the song's pick, then the listener's order.
    fn lyric_choice(&self) -> Option<&Lyrics> {
        let (id, set) = self.lyrics.as_ref()?;
        set.get(self.lyric_prefs.choose(id, set)?)
    }

    /// Lyrics have loaded, and no enabled source has any.
    pub(super) fn no_lyrics(&self) -> bool {
        self.lyrics.is_some() && self.lyric_choice().is_none()
    }

    /// Ask for romanization/translation of the shown source once, when they're switched on
    /// and some line needs one the lyrics don't ship.
    pub(super) fn request_lyric_extras(&mut self) {
        if !(self.show_roman || self.show_trans) {
            return;
        }
        let Some(l) = self.lyric_choice() else { return };
        let Some((id, _)) = &self.lyrics else { return };
        let target = self.lyric_prefs.target.clone();
        let want = (id.clone(), l.provider.key().to_owned(), target.clone());
        if self.extras_asked.as_ref() == Some(&want) {
            return;
        }
        let needs = l.lines.iter().any(|x| {
            !x.instrumental
                && !x.text.trim().is_empty()
                && ((self.show_roman && x.romanization.is_none() && crate::translate::non_latin(&x.text))
                    || (self.show_trans && lyric_prefs::shipped_translation(x, &target).is_none()))
        });
        let req = needs.then(|| Req::LyricsExtras {
            video_id: id.clone(),
            source: l.provider.key().to_owned(),
            lines: l.lines.iter().map(|x| if x.instrumental { String::new() } else { x.text.clone() }).collect(),
            lang: l.language.clone(),
            target,
        });
        self.extras_asked = Some(want);
        if let Some(r) = req {
            self.backend.request(r);
        }
    }

    pub(super) fn current_id(&self) -> Option<&str> {
        self.current().map(|t| t.id.as_str())
    }

    /// The song version standing in for the current music video. Only for the play it
    /// came with: a new play, or a queue that changed under it, leaves it behind.
    pub(super) fn playing_as(&self) -> Option<&Track> {
        self.playing_as.as_ref().filter(|p| p.generation == self.generation && self.current_id() == Some(p.listed.as_str())).map(|p| &p.song)
    }

    /// What actually plays: the current track, or the song version standing in for it.
    /// Lyrics, SponsorBlock, Last.fm, Discord, media controls and Now playing's actions go
    /// by this (timing, length and title are the upload's own); lists and the queue keep
    /// the track as listed.
    pub(super) fn playing_track(&self) -> Option<&Track> {
        self.playing_as().or(self.current())
    }

    pub(super) fn playing_id(&self) -> Option<&str> {
        self.playing_track().map(|t| t.id.as_str())
    }

    /// Whether YouTube's own description says AI made this song: the watch page's
    /// "Made with AI" block, which a clean title and artist never repeat.
    pub(super) fn ai_made(&self, id: &str) -> bool {
        self.ai_flags.get(id).copied().unwrap_or(false)
    }

    /// The outgoing song of an Automix handoff, while it is still the louder of the
    /// two: its tail keeps the player until the next song has taken over.
    pub(super) fn mix_hold(&self) -> Option<&MixFrom> {
        self.mix_from.as_ref().filter(|h| !h.released)
    }

    /// The blend is under way: the handoff exists until both of its halves have played.
    pub(super) fn mixing(&self) -> bool {
        self.mix_from.as_ref().is_some_and(|h| h.started && self.audio.status.mixing())
    }

    /// That song's progress, in seconds: its tail runs out as the mix goes on.
    pub(super) fn mix_hold_pos(&self, h: &MixFrom) -> f32 {
        self.audio.status.outgoing_position().as_secs_f32().min(h.total)
    }

    /// Note where the handoff has got to; true on the frame the next song becomes the
    /// louder one and takes the screen over (see `MIX_SWITCH`).
    pub(super) fn watch_mix(&mut self) -> bool {
        let pos = self.audio.status.position().as_secs_f32();
        let Some(h) = &mut self.mix_from else { return false };
        if let Some(duration) = self.audio.status.mix_duration(self.generation) {
            h.started = true;
            h.mix = duration;
        }
        let switch = h.started && !h.released && pos >= h.mix * MIX_SWITCH;
        if switch {
            h.released = true;
        }
        switch
    }

    /// The handoff's whole blend has played: the transition is over.
    pub(super) fn mix_over(&self) -> bool {
        self.mix_from.as_ref().is_some_and(|h| h.started && !self.audio.status.mixing())
    }

    /// The song the player shows: a handoff's outgoing one while it's still the louder,
    /// then whatever actually plays.
    pub(super) fn shown(&self) -> Option<&Track> {
        self.mix_hold().map(|h| &h.track).or_else(|| self.playing_track())
    }

    /// The next song has taken the blend over: its art, lyrics and media info follow.
    pub(super) fn release_mix(&mut self) {
        if let Some(track) = self.playing_track().cloned() {
            self.show_in_media(&track);
            self.request_art();
            self.refresh_lyrics(track);
        }
    }

    /// `t`, when it's a music video to play as its song version. A party shares song ids,
    /// not streams: each device plays the best copy of the song it has.
    pub(super) fn song_of(&self, t: &Track) -> Option<Track> {
        (self.play_songs && t.music_video).then(|| t.clone())
    }

    pub(super) fn playing(&self) -> bool {
        self.audio.status.playing.load(Ordering::Relaxed)
    }

    /// Fetch the current song's art for the background and accent.
    pub(super) fn request_art(&mut self) {
        let Some(track) = self.current() else { return };
        if self.art_key.as_deref() == Some(track.id.as_str()) {
            return;
        }
        if let Some(url) = backend::art_url(track) {
            let key = track.id.clone();
            self.backend.request(Req::Art { key, url });
        }
    }

    pub(super) fn play_list(&mut self, tracks: Vec<Track>, start: usize) {
        self.play_list_filling(tracks, start, None, None);
    }

    /// `fill`: the queue is a playlist still coming in, and its other songs follow.
    /// `shuffle`: what the queue's shuffle mark does for these songs — `None` keeps it as
    /// it stands, so a song clicked mid-shuffle still plays from there and shuffles on.
    pub(super) fn play_list_filling(&mut self, tracks: Vec<Track>, start: usize, fill: Option<QueueFill>, shuffle: Option<bool>) {
        let shuffled = shuffle.unwrap_or(self.queue.shuffled);
        if !self.queue.replace(tracks, start, fill, shuffled) {
            return;
        }
        // Whatever was about to play doesn't.
        self.play_pending = None;
        self.play_current();
    }

    /// Put last session's queue back, paused at the saved position, without loading audio.
    pub(super) fn restore_session(&mut self, json: &str) {
        let Ok(s) = serde_json::from_str::<Session>(json) else { return };
        let Some(track) = s.queue.get(s.index).cloned() else { return };
        self.queue.adopt(s.queue, s.index, s.shuffled, s.order);
        let shown = s.playing_as.clone().unwrap_or_else(|| track.clone());
        self.playing_as = s.playing_as.map(|song| PlayingAs { generation: self.generation, listed: track.id.clone(), song });
        let dur = shown.duration.map_or(s.duration_ms, |d| d as u64 * 1000);
        self.audio.status.duration_ms.store(dur, Ordering::Relaxed);
        self.audio.status.position_ms.store(s.position_ms, Ordering::Relaxed);
        self.resume_at = Some(Duration::from_millis(s.position_ms));
        self.listened_at = s.listened_at;
        self.request_art();
        self.request_lyrics(shown);
    }

    /// Ask for the queue YouTube Music keeps across devices when the app starts and
    /// whenever its window comes back into focus with nothing playing, so a song
    /// started on the phone is here to carry on with.
    pub(super) fn sync_server_queue(&mut self, ctx: &egui::Context, now: f64) {
        if self.playing() {
            self.listened_at = unix_now();
        }
        let focused = ctx.input(|i| i.focused);
        let refocused = focused && !std::mem::replace(&mut self.was_focused, focused);
        let idle = !self.playing() && !self.buffering && !self.jam.active() && self.smoke.is_none();
        let due = self.queue_checked.is_none_or(|t| refocused && now - t > 60.0);
        if self.logged_in && idle && due && std::env::var_os("YTM_SMOKE").is_none() {
            self.queue_checked = Some(now);
            self.backend.request(Req::ServerQueue);
        }
    }

    /// Take up what another device was playing, paused at its start like a restored
    /// session, when it played there after anything played here.
    pub(super) fn adopt_server_queue(&mut self, q: crate::ytm::ServerQueue) {
        let Some(track) = q.tracks.get(q.index).cloned() else { return };
        let idle = !self.playing() && !self.buffering && !self.jam.active();
        // A minute's slack: this app's own plays reach that queue too.
        if !idle || q.watched_at <= self.listened_at + 60 || self.current_id() == Some(track.id.as_str()) {
            return;
        }
        log::info!("picking up {} from another device ({} in its queue)", track.id, q.tracks.len());
        self.queue.adopt(q.tracks, q.index, false, Vec::new());
        // Not a new play: an old song version mustn't come back with its video.
        self.playing_as = None;
        self.queue_source = None;
        self.listened_at = q.watched_at;
        self.audio.send(AudioCmd::Stop);
        self.audio.status.duration_ms.store(track.duration.map_or(0, |d| d as u64 * 1000), Ordering::Relaxed);
        self.audio.status.position_ms.store(0, Ordering::Relaxed);
        self.resume_at = Some(Duration::ZERO);
        self.resume_counts = true;
        self.request_art();
        self.refresh_lyrics(track.clone());
        self.toast(format!("Picked up \"{}\" from your other device. Press play to carry on.", track.title));
    }

    pub(super) fn session_json(&self) -> Option<String> {
        let (queue, index) = self.queue.saved()?;
        serde_json::to_string(&Session {
            queue: queue.to_vec(),
            index,
            position_ms: self.audio.status.position().as_millis() as u64,
            duration_ms: self.audio.status.duration_ms.load(Ordering::Relaxed),
            listened_at: if self.playing() { unix_now() } else { self.listened_at },
            playing_as: self.playing_as().cloned(),
            shuffled: self.queue.shuffled,
            order: self.queue.list_order().to_vec(),
        })
        .ok()
    }

    /// The song's download failed partway: pick it up where it stopped from a fresh
    /// stream, rather than letting it end early and skip to the next one. If that keeps
    /// failing, stay on the song, ready to resume from there.
    pub(super) fn recover(&mut self, at: Duration, reason: &str) {
        const MAX_RECOVERIES: u32 = 3;
        if self.recoveries < MAX_RECOVERIES {
            self.recoveries += 1;
            log::warn!("playback interrupted at {at:?} ({reason}); reopening (try {})", self.recoveries);
            self.play_current_at(at, false);
            return;
        }
        log::warn!("playback interrupted at {at:?} ({reason}); giving up");
        // The player has already run dry; Stop would also reset the position to 0:00.
        self.audio.status.position_ms.store(at.as_millis() as u64, Ordering::Relaxed);
        self.resume_at = Some(at);
        self.buffering = false;
        let title = self.current().map(|t| t.title.clone()).unwrap_or_default();
        self.toast(format!("Lost the connection while playing \"{title}\". Press play to pick up where it stopped."));
    }

    pub(super) fn play_current(&mut self) {
        // A song whose own name says AI made it doesn't play while that's switched off:
        // on to the next the queue holds, and stop rather than run round the list.
        if self.skip_ai {
            let mut skipped = 0;
            while self.current().is_some_and(|t| t.ai_labelled() || self.ai_made(&t.id)) {
                if !self.queue.advance() || skipped + 1 >= self.queue.tracks.len() {
                    self.toast("Everything left is labelled AI, so nothing plays.");
                    return;
                }
                skipped += 1;
            }
            if skipped > 0 {
                self.toast(format!("Skipped {skipped} song{} labelled AI.", if skipped == 1 { "" } else { "s" }));
            }
        }
        self.recoveries = 0;
        super::jam::hand_picked(self);
        self.play_current_at(Duration::ZERO, true);
    }

    /// Start the current track, opened directly at `at` so nothing plays from 0:00 first.
    /// `report`: a new play for the account's history (not picking up last session's song).
    pub(super) fn play_current_at(&mut self, at: Duration, report: bool) {
        let Some(track) = self.current().cloned() else { return };
        // A natural Automix handoff keeps the outgoing song on screen until the next one
        // is louder (see `MixFrom`); any other play switches at once.
        let mixing = self.pending_mix.is_some();
        if !mixing {
            self.mix_from = None;
        }
        // Picking up mid-song (a resume, a download that broke off): the position is the
        // song version's when one was playing, so that goes on; a music video stays one.
        let carried = if at.is_zero() { None } else { self.playing_as().cloned() };
        self.queue.started();
        self.resume_at = None;
        self.resume_counts = false;
        self.generation += 1;
        self.audio.status.wanted_gen.store(self.generation, Ordering::Relaxed);
        self.audio_info = None;
        self.stream_stats = None;
        self.upgrading_quality = false;
        self.upgrade_asked = false;
        let shown = carried.clone().unwrap_or_else(|| track.clone());
        self.audio.status.duration_ms.store(shown.duration.map_or(0, |d| d as u64 * 1000), Ordering::Relaxed);
        // A song running into the next crossfades (so does Next, when that's switched
        // on); going back or picking a song cuts over. Bit-perfect output never mixes
        // two songs, so even Next is a clean cut there.
        self.audio.send(if self.handoff && at.is_zero() && !self.bit_perfect { AudioCmd::Handoff } else { AudioCmd::Stop });
        self.buffering = true;
        self.seek_drag = None;
        // A carried song version goes with its music video, which plays if the song won't.
        let song_of = if carried.is_none() && at.is_zero() { self.song_of(&track) } else { None };
        self.playing_as = carried.clone().map(|song| PlayingAs { generation: self.generation, listed: track.id.clone(), song });
        self.backend.request(Req::Play { video_id: track.id.clone(), generation: self.generation, at, report, song_of, song: carried, track: Some(track.clone()) });
        if mixing {
            // The outgoing song keeps the art, lyrics and media info it still deserves.
        } else {
            self.show_in_media(&shown);
            // Background art follows the track; lyrics and segments, what plays.
            self.request_art();
            // YouTube's own label is read off the video's description: a song whose
            // name keeps it secret is still owed the skip once the answer lands.
            if self.skip_ai {
                self.backend.request(Req::AiCheck { video_id: shown.id.clone() });
            }
            self.refresh_lyrics(shown);
        }
        if let Some(m) = &mut self.media {
            m.set_playing(true);
        }
        self.top_up();
    }

    /// The playing track's title, artist and length for the system's media controls,
    /// with the listed track's art (what the app shows too).
    pub(super) fn show_in_media(&mut self, shown: &Track) {
        let Some(track) = self.current().cloned() else { return };
        let Some(m) = &mut self.media else { return };
        // Windows fetches a cover URL on its own, through its own proxy setting only.
        let by_url = matches!(crate::proxy::active(), crate::proxy::Setting::System);
        let art = backend::art_url(&track);
        m.set_track(&shown.title, &shown.artist_line(), shown.album.as_ref().map(|a| a.name.as_str()), art.as_deref().filter(|_| by_url), shown.duration);
        if !by_url && let Some(url) = art {
            self.fetch_media_cover(track.id.clone(), url);
        }
    }

    /// Lyrics for a song.
    pub(super) fn request_lyrics(&mut self, track: Track) {
        self.lyrics_generation += 1;
        let genius_enabled = self.lyric_prefs.order.iter().any(|(p, enabled)| *p == crate::lyrics::Provider::GeniusPlain && *enabled);
        self.backend.request(Req::Lyrics { track, generation: self.lyrics_generation, genius_enabled });
    }

    /// Lyrics and SponsorBlock segments for `playing`, which timing is its own.
    pub(super) fn refresh_lyrics(&mut self, playing: Track) {
        self.lyrics = None;
        self.lyrics_extras = None;
        self.extras_asked = None;
        self.lyric_view.reset();
        self.sidebar_lyric_view.reset();
        self.request_lyrics(playing);
        self.segments = None;
        self.request_segments();
        // A new song starts with its best part unreached and unheard: the button comes
        // back for a song played again.
        self.best_part = None;
        self.best_part_heard = false;
        self.request_best_part();
        self.update_best_part();
    }

    /// Ask youtube.com where the playing song's best part is, when the button is on.
    pub(super) fn request_best_part(&mut self) {
        if !self.show_best_part {
            return;
        }
        // What plays: a song version's curve is its own.
        let Some(video_id) = self.playing_id().map(str::to_owned) else { return };
        if self.curves.contains_key(&video_id) || !self.curves_asked.insert(video_id.clone()) {
            return;
        }
        self.backend.request(Req::BestPart(video_id));
    }

    /// Where the playing song's best part starts: the bump in its replay curve where
    /// youtube.com charts one, and the first chorus its synced words show otherwise.
    /// Nothing while the curve is still on its way, since which of the two applies is
    /// the curve's to say; the words are the fallback, not an addition to it.
    pub(super) fn update_best_part(&mut self) {
        let Some(id) = self.playing_id().map(str::to_owned) else { return };
        // A late answer about the song before this one is dropped, as is an ask about a
        // song the listener has already moved on from.
        let Some(curve) = self.curves.get(&id) else { return };
        let lines = self
            .lyrics
            .as_ref()
            .filter(|(lyrics_id, _)| *lyrics_id == id)
            .and_then(|(_, set)| set.sources.iter().find(|l| l.sync != crate::lyrics::Sync::None))
            .map(|l| l.lines.as_slice())
            .unwrap_or_default();
        let duration_ms = self.playing_track().and_then(|t| t.duration).map_or(0, |d| d as i64 * 1000);
        self.best_part = crate::bestpart::pick(curve.as_ref(), lines, duration_ms).map(|start| (id, start));
    }

    /// Keep the queue topped up so autoplay never stalls at the end; a playlist still
    /// coming in goes first.
    pub(super) fn top_up(&mut self) {
        if self.autoplay && !self.radio_pending && !self.jam.follows() && self.queue.fill.is_none() && self.queue.at_end()
            && let Some(t) = self.current().cloned()
        {
            self.radio_pending = true;
            self.backend.request(Req::Radio { song_of: self.song_of(&t), seed: t.id });
        }
    }

    pub(crate) fn set_crossfade(&self) {
        // Automix is a crossfade with its own shape, so it needs the length too.
        self.audio.set_automix(self.automix);
        self.audio.set_crossfade(if self.crossfade || self.automix { self.crossfade_secs } else { 0 });
    }

    /// The equalizer's settings, to the audio thread. Takes effect at once, gliding.
    pub(crate) fn set_eq(&self) {
        self.audio.set_eq(self.eq_on, &self.eq_gains, self.eq_no_clip);
    }

    /// How much of the centre vocal the engine takes out while singing along.
    pub(crate) fn set_sing(&self) {
        self.audio.set_vocal_cut(if self.sing { self.sing_level } else { 0.0 });
    }

    /// Ask SponsorBlock about the playing song, when that's switched on.
    pub(super) fn request_segments(&mut self) {
        if !self.sponsorblock || self.segments.is_some() {
            return;
        }
        // What plays: a song version's segments are its own.
        let Some(video_id) = self.playing_id().map(str::to_owned) else { return };
        self.backend.request(Req::Segments { video_id });
    }

    /// Jump over a SponsorBlock segment once playback is inside it; one that runs to the
    /// end finishes the song. A member who doesn't drive the party follows it, and the one
    /// who does skips for everyone.
    pub(super) fn skip_segments(&mut self, ctx: &egui::Context, now: f64) {
        if !self.sponsorblock || self.jam.follows() || self.resume_at.is_some() || self.buffering || !self.playing() {
            return;
        }
        let Some((id, segs)) = &self.segments else { return };
        // A seek takes a moment to show in the position: don't send it twice.
        if self.playing_id() != Some(id.as_str()) || now - self.segment_skipped < 1.5 {
            return;
        }
        // The stream's own length: the song row's can be another cut's (the song's,
        // where the video plays), and a segment marked on another cut skips wrong.
        let Some(duration) = self.audio.status.duration().map(|d| d.as_secs_f64()) else { return };
        let pos = self.audio.status.position().as_secs_f64();
        let mut segs = segs.iter().filter(|s| s.fits(duration));
        let Some(seg) = segs.clone().find(|s| pos >= s.start && pos < s.end - 0.3).copied() else {
            // Wake up for the next one even when nothing else repaints.
            if let Some(next) = segs.find(|s| s.start > pos) {
                ctx.request_repaint_after(Duration::from_secs_f64(next.start - pos + 0.05));
            }
            return;
        };
        self.segment_skipped = now;
        log::info!("sponsorblock: skipping {:.1}–{:.1} s", seg.start, seg.end);
        if seg.end >= duration - 1.0 {
            if self.queue.repeat == Repeat::One {
                self.play_current();
            } else {
                self.next();
            }
        } else {
            self.audio.send(AudioCmd::Seek(Duration::from_secs_f64(seg.end)));
        }
    }

    /// Fetch whatever plays after the current track so skipping is instant.
    pub(super) fn prefetch_next(&self) {
        if let Some(t) = self.queue.upcoming() {
            self.backend.request(Req::Prefetch { video_id: t.id.clone(), song_of: self.song_of(t), track: Some(t.clone()) });
        }
    }

    pub(super) fn next(&mut self) {
        if self.jam.locked() {
            return self.toast("Only the host can change the party");
        }
        if !self.queue.advance() {
            // The end of what's queued: with autoplay on the songs go on, so ask for
            // more here too — an ask made when this song started may have failed or
            // never been made at all, and playback would otherwise just park.
            self.top_up();
            return;
        }
        self.play_current();
    }

    pub(super) fn prev(&mut self) {
        if self.jam.locked() {
            return self.toast("Only the host can change the party");
        }
        if self.audio.status.position() > Duration::from_secs(3) {
            self.seek_to(Duration::ZERO);
            return;
        }
        if self.queue.back() {
            self.play_current();
        }
    }

    pub(super) fn seek_to(&mut self, at: Duration) {
        // A needle this device moved is the room's to hear about; `publish` keeps telling
        // it until the room agrees. Only hand seeks come through here — the sync's own
        // corrections seek the player directly, so drift never gets reported as a move.
        if self.jam.active() {
            self.jam.pending_seek = Some(at.as_millis() as u64);
        }
        if self.resume_at.is_some() {
            // Restored but not loaded yet (nothing for the player to seek): move the start point instead.
            self.resume_at = Some(at);
            self.audio.status.position_ms.store(at.as_millis() as u64, Ordering::Relaxed);
        } else {
            self.audio.send(AudioCmd::Seek(at));
        }
    }

    pub(super) fn toggle(&mut self) {
        if self.queue.index.is_none() {
            return;
        }
        if self.jam.active() && !(self.jam.locked() && !self.playing()) {
            // Play and pause are the room's to know, so say so here first: that keeps the
            // sync from walking us straight back to the room's still-unmoved state while
            // our own ask is on its way. In a room only the host drives, a play stays the
            // room's alone, and a pause is ours.
            self.jam.intent = Some(!self.playing());
        }
        if let Some(at) = self.resume_at {
            // Last session's song, already counted when it started then (unless it was
            // another device's).
            let report = std::mem::take(&mut self.resume_counts);
            self.play_current_at(at, report);
            return;
        }
        let playing = self.playing();
        // A bit-perfect track that ran out or lost its exclusive stream has nothing
        // loaded to resume: start it again. A paused one keeps its DAC open.
        if !playing
            && !self.buffering
            && self.bit_perfect
            && self.audio.status.bit_perfect_current.load(Ordering::Relaxed)
            && !self.audio.status.bit_perfect.load(Ordering::Relaxed)
        {
            self.play_current();
            return;
        }
        // The media controls catch up in `logic` once the player has switched.
        self.audio.send(if playing { AudioCmd::Pause } else { AudioCmd::Resume });
    }

    /// The transport's shuffle mark: on plays what's still to come in a random order, off
    /// hands the list's own order back.
    pub(super) fn toggle_shuffle(&mut self) {
        if self.jam.locked() {
            return self.toast("Only the host can shuffle the party");
        }
        let on = !self.queue.shuffled;
        self.queue.set_shuffle(on);
        self.toast(if on { "Shuffling up next" } else { "Playing in order" });
        self.prefetch_next();
    }

    /// Switch lossless playback between the normal mixer and WASAPI exclusive output.
    pub(super) fn set_bit_perfect(&mut self, on: bool) {
        if self.bit_perfect == on {
            return;
        }
        self.bit_perfect = on;
        self.audio.send(AudioCmd::SetBitPerfect(on));
        // Bit-perfect carries stereo PCM, so do not ask for an Atmos rendition while it
        // is on. The listener's own Atmos choice comes back when it is switched off.
        self.backend.set_prefer_atmos(self.prefer_atmos && !on);
        self.toast(if on {
            "Bit-perfect output on — lossless tracks go straight to the DAC"
        } else {
            "Bit-perfect output off"
        });
        if self.current().is_some() {
            let at = self.audio.status.position();
            self.play_current_at(at, false);
        }
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}
