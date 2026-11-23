//! The play queue: its songs, the one playing, repeat, and the songs of a playlist
//! still coming in. Only the state: the app starts the songs these pick, prefetches,
//! and asks for radio.

use crate::model::Track;

#[derive(Clone, Copy, PartialEq)]
pub enum Repeat {
    Off,
    All,
    One,
}

/// A playlist's load whose next pages also go to the queue. `ran_out`: playback reached
/// the end of what had come, and carries on when more does (or the load ends).
pub struct QueueFill {
    pub id: String,
    pub load: u64,
    pub ran_out: bool,
}

/// More of a playlist joined the queue.
pub struct Filled {
    /// Playback was waiting for it: go on to the next song.
    pub ran_out: bool,
    /// The song playing was the last one before.
    pub was_at_end: bool,
}

pub struct Queue {
    pub tracks: Vec<Track>,
    /// The song playing (or paused, or loading).
    pub index: Option<usize>,
    pub repeat: Repeat,
    /// The queue is a playlist played while its songs were still coming in: the rest
    /// join the queue as they arrive.
    pub fill: Option<QueueFill>,
    /// Shuffle on: what's still to come is in a random order, and the mark is lit. Off
    /// puts the same songs back in the order their list has.
    pub shuffled: bool,
    /// The songs in their list's own order, kept while shuffle is on so switching it off
    /// can restore it. Empty when nothing was ever shuffled.
    order: Vec<String>,
}

/// How many songs before and after the current one a saved session keeps: liked-songs
/// queues can run to 1000+ songs.
const SAVED_BEFORE: usize = 50;
const SAVED_AFTER: usize = 150;

impl Queue {
    pub fn new(repeat: Repeat) -> Self {
        Self { tracks: Vec::new(), index: None, repeat, fill: None, shuffled: false, order: Vec::new() }
    }

    pub fn current(&self) -> Option<&Track> {
        self.index.and_then(|i| self.tracks.get(i))
    }

    /// The song playing is the last one.
    pub fn at_end(&self) -> bool {
        self.index.is_some_and(|i| i + 1 >= self.tracks.len())
    }

    /// Play `tracks` from `start`; `fill`: a playlist still coming in, whose other songs
    /// follow. `shuffled`: play on in a random order, which the queue's mark shows as lit
    /// and switching off puts back. False (and nothing changes) when there are none.
    pub fn replace(&mut self, tracks: Vec<Track>, start: usize, fill: Option<QueueFill>, shuffled: bool) -> bool {
        if tracks.is_empty() {
            return false;
        }
        self.fill = fill;
        self.shuffled = shuffled;
        // The shuffle is the queue's to do, so the order it shuffles back to is on record.
        self.order = if shuffled { tracks.iter().map(|t| t.id.clone()).collect() } else { Vec::new() };
        self.tracks = tracks;
        let i = start.min(self.tracks.len() - 1);
        if shuffled {
            let first = self.tracks.remove(i);
            self.tracks.insert(0, first);
            super::shuffle(&mut self.tracks[1..]);
            self.index = Some(0);
        } else {
            self.index = Some(i);
        }
        true
    }

    /// Take up a queue from elsewhere (last session, another device), at `index`, which
    /// the caller has checked is one of `tracks`. A saved session brings its shuffle and
    /// the order that shuffle came from, so the mark still means what it says.
    pub fn adopt(&mut self, tracks: Vec<Track>, index: usize, shuffled: bool, order: Vec<String>) {
        self.tracks = tracks;
        self.index = Some(index);
        self.fill = None;
        self.shuffled = shuffled;
        self.order = order;
    }

    /// Next, or the end of a song: true when there's a song to play. At the end of a
    /// playlist still coming in, playback waits for its next songs. (Repeating one song
    /// is the caller's: a song's end replays it rather than coming here.)
    pub fn advance(&mut self) -> bool {
        let Some(i) = self.index else { return false };
        if i + 1 < self.tracks.len() {
            self.index = Some(i + 1);
        } else if let Some(fill) = &mut self.fill {
            fill.ran_out = true;
            return false;
        } else if self.repeat == Repeat::All && !self.tracks.is_empty() {
            self.index = Some(0);
        } else {
            return false;
        }
        true
    }

    /// Back to the song before: false at the first.
    pub fn back(&mut self) -> bool {
        match self.index.filter(|&i| i > 0) {
            Some(i) => {
                self.index = Some(i - 1);
                true
            }
            None => false,
        }
    }

    /// What plays after the current song, to fetch ahead.
    pub fn upcoming(&self) -> Option<&Track> {
        let i = self.index?;
        match self.repeat {
            Repeat::One => None,
            Repeat::All => self.tracks.get(i + 1).or(self.tracks.first()),
            Repeat::Off => self.tracks.get(i + 1),
        }
    }

    /// A song started: it no longer waits for a playlist's next songs.
    pub fn started(&mut self) {
        if let Some(fill) = &mut self.fill {
            fill.ran_out = false;
        }
    }

    /// Shuffle on or off, the transport's mark. On puts what's still to come in a random
    /// order; off returns those same songs to the order their list has. The song playing
    /// and the ones already heard stay where they are.
    pub fn set_shuffle(&mut self, on: bool) {
        if on == self.shuffled {
            return;
        }
        self.shuffled = on;
        let start = self.index.map_or(0, |i| i + 1);
        if start >= self.tracks.len() {
            return;
        }
        if on {
            if self.order.is_empty() {
                self.order = self.tracks.iter().map(|t| t.id.clone()).collect();
            }
            super::shuffle(&mut self.tracks[start..]);
        } else if !self.order.is_empty() {
            // Songs that joined since — a radio seed, one added by hand — were never in
            // the list's order, so they keep their place at the end.
            let order = std::mem::take(&mut self.order);
            let rank: std::collections::HashMap<&str, usize> = order.iter().enumerate().map(|(n, id)| (id.as_str(), n)).collect();
            let part = &mut self.tracks[start..];
            part.sort_by_key(|t| rank.get(t.id.as_str()).copied().unwrap_or(usize::MAX));
            self.order = order;
        }
    }

    /// Play `t` after the current song: true when nothing was playing, so it starts now.
    pub fn play_next(&mut self, t: Track) -> bool {
        let at = self.index.map_or(0, |i| i + 1).min(self.tracks.len());
        self.tracks.insert(at, t);
        if self.index.is_some() {
            return false;
        }
        self.index = Some(0);
        true
    }

    /// Move queue row `i` to play just after the song playing: the queue's own
    /// "Play next", which reorders what's already here instead of queueing a second copy.
    /// False when that's the song playing, or there's no song playing to put it after.
    pub fn play_next_at(&mut self, i: usize) -> bool {
        let Some(cur) = self.index else { return false };
        if i == cur || i >= self.tracks.len() {
            return false;
        }
        let t = self.tracks.remove(i);
        if i < cur {
            self.index = Some(cur - 1);
        }
        let at = (self.index.unwrap_or(0) + 1).min(self.tracks.len());
        self.tracks.insert(at, t);
        true
    }

    /// Add `t` at the end: true when nothing was playing, so it starts now.
    pub fn enqueue(&mut self, t: Track) -> bool {
        self.tracks.push(t);
        if self.index.is_some() {
            return false;
        }
        self.index = Some(self.tracks.len() - 1);
        true
    }

    /// Take song `i` out, unless it's the one playing.
    pub fn remove(&mut self, i: usize) {
        if i < self.tracks.len() && Some(i) != self.index {
            self.tracks.remove(i);
            if let Some(cur) = self.index.filter(|&c| c > i) {
                self.index = Some(cur - 1);
            }
        }
    }

    /// Slide queue row `i` one place — `dir` negative moves it up. Only what's still to
    /// come moves, and only past another song still to come: false otherwise.
    pub fn shift(&mut self, i: usize, dir: isize) -> bool {
        let first = self.index.map_or(0, |c| c + 1);
        let Some(j) = i.checked_add_signed(dir) else { return false };
        if i < first || j < first || j >= self.tracks.len() {
            return false;
        }
        self.tracks.swap(i, j);
        true
    }

    /// Radio songs for `seed` join the end, without the seed or songs already queued.
    /// Returns whether the song playing was the last one.
    pub fn add_radio(&mut self, seed: &str, tracks: Vec<Track>) -> bool {
        let have: std::collections::HashSet<String> = self.tracks.iter().map(|t| t.id.clone()).collect();
        let was_at_end = self.at_end();
        self.tracks.extend(tracks.into_iter().filter(|t| t.id != seed && !have.contains(&t.id)));
        was_at_end
    }

    /// The next page of the playlist the queue fills from (`id`, `load`) joins it; `more`:
    /// pages still follow. `playing_it`: the queue still plays that playlist. `None` when
    /// the page doesn't apply (though the last one still ends the fill); an empty last
    /// page that does still says whether playback was waiting for it.
    pub fn fill_from(&mut self, id: &str, load: u64, tracks: Vec<Track>, more: bool, playing_it: bool) -> Option<Filled> {
        let fill = self.fill.as_mut().filter(|f| f.id == id && f.load == load)?;
        let ran_out = std::mem::take(&mut fill.ran_out);
        if !more {
            self.fill = None;
        }
        if !playing_it {
            return None;
        }
        let was_at_end = self.at_end();
        let shuffled = self.shuffled;
        self.order.extend(tracks.iter().map(|t| t.id.clone()));
        self.tracks.extend(tracks);
        if shuffled {
            // Mixed into what's still to come, past the next song (it may be loading already).
            let from = self.index.map_or(0, |i| i + if was_at_end { 1 } else { 2 }).min(self.tracks.len());
            super::shuffle(&mut self.tracks[from..]);
        }
        Some(Filled { ran_out, was_at_end })
    }

    /// The list's own order, kept while a shuffle is on: a saved session takes it up, so
    /// the mark means the same thing after a restart.
    pub fn list_order(&self) -> &[String] {
        &self.order
    }

    /// The part of the queue a saved session keeps, and the current song's place in it.
    pub fn saved(&self) -> Option<(&[Track], usize)> {
        let i = self.index?;
        let from = i.saturating_sub(SAVED_BEFORE);
        let to = (i + SAVED_AFTER).min(self.tracks.len());
        Some((&self.tracks[from..to], i - from))
    }
}

#[cfg(test)]
mod tests {
    use super::{Queue, QueueFill, Repeat, Track};

    fn track(id: &str) -> Track {
        Track { id: id.into(), title: id.into(), artists: vec![], album: None, duration: None, thumbs: vec![], track_nr: None, plays: None, music_video: false, episode: false }
    }

    fn queue(songs: &[&str], index: usize, repeat: Repeat) -> Queue {
        let mut q = Queue::new(repeat);
        assert!(q.replace(songs.iter().map(|s| track(s)).collect(), index, None, false));
        q
    }

    fn ids(q: &Queue) -> Vec<&str> {
        q.tracks.iter().map(|t| t.id.as_str()).collect()
    }

    fn filling(id: &str, load: u64) -> Option<QueueFill> {
        Some(QueueFill { id: id.into(), load, ran_out: false })
    }

    #[test]
    fn next_and_back_follow_repeat() {
        let mut q = queue(&["a", "b"], 0, Repeat::Off);
        assert!(q.advance());
        assert_eq!(q.index, Some(1));
        // At the end: stays there, unless repeating all.
        assert!(!q.advance());
        assert_eq!(q.index, Some(1));
        q.repeat = Repeat::All;
        assert!(q.advance());
        assert_eq!(q.index, Some(0));
        // Next moves on even when repeating one song (the song's own end replays it).
        q.repeat = Repeat::One;
        assert!(q.advance());
        assert_eq!(q.index, Some(1));
        assert!(q.back());
        assert!(!q.back());
        assert_eq!(q.index, Some(0));
        // Nothing queued: nothing to do.
        assert!(!Queue::new(Repeat::All).advance());
    }

    #[test]
    fn what_plays_next_is_fetched_ahead() {
        let q = queue(&["a", "b"], 1, Repeat::Off);
        assert!(q.upcoming().is_none());
        let q = queue(&["a", "b"], 1, Repeat::All);
        assert_eq!(q.upcoming().unwrap().id, "a");
        let q = queue(&["a", "b"], 0, Repeat::One);
        assert!(q.upcoming().is_none());
        let q = queue(&["a", "b"], 0, Repeat::Off);
        assert_eq!(q.upcoming().unwrap().id, "b");
    }

    #[test]
    fn playing_nothing_leaves_the_queue_as_it_was() {
        let mut q = Queue::new(Repeat::Off);
        q.replace(vec![track("a")], 0, filling("PL1", 1), false);
        assert!(!q.replace(vec![], 0, None, false));
        assert_eq!(ids(&q), ["a"]);
        assert_eq!(q.fill.as_ref().unwrap().id, "PL1");
        // A start past the end plays the last song.
        assert!(q.replace(vec![track("x"), track("y")], 9, None, false));
        assert_eq!(q.current().unwrap().id, "y");
    }

    #[test]
    fn a_shuffled_start_is_a_shuffle_the_mark_can_turn_off() {
        let songs: Vec<Track> = (0..12).map(|i| track(&i.to_string())).collect();
        let mut q = Queue::new(Repeat::Off);
        assert!(q.replace(songs, 0, None, true));
        assert!(q.shuffled);
        // Three songs heard, then the mark switched off: what was heard stays put and what
        // still comes takes its list's own order again, with those three missing from it.
        q.index = Some(2);
        let heard: Vec<String> = q.tracks[..3].iter().map(|t| t.id.clone()).collect();
        q.set_shuffle(false);
        assert!(!q.shuffled);
        assert_eq!(q.tracks[..3].iter().map(|t| t.id.clone()).collect::<Vec<_>>(), heard);
        let want: Vec<String> = (0..12).map(|i| i.to_string()).filter(|id| !heard.contains(id)).collect();
        assert_eq!(q.tracks[3..].iter().map(|t| t.id.clone()).collect::<Vec<_>>(), want);
    }

    #[test]
    fn shuffled_start_keeps_all_songs_and_restores_their_order() {
        let mut q = Queue::new(Repeat::Off);
        assert!(q.replace(vec![track("a"), track("a"), track("b"), track("c")], 3, None, true));
        assert_eq!(q.index, Some(0));
        assert_eq!(q.current().unwrap().id, "c");
        let mut all = ids(&q);
        all.sort();
        assert_eq!(all, ["a", "a", "b", "c"]);
        q.set_shuffle(false);
        assert_eq!(ids(&q), ["c", "a", "a", "b"]);
    }

    #[test]
    fn songs_added_by_hand() {
        let mut q = Queue::new(Repeat::Off);
        // Into an empty queue: it starts.
        assert!(q.enqueue(track("a")));
        assert_eq!(q.index, Some(0));
        assert!(!q.enqueue(track("c")));
        assert!(!q.play_next(track("b")));
        assert_eq!(ids(&q), ["a", "b", "c"]);
        let mut empty = Queue::new(Repeat::Off);
        assert!(empty.play_next(track("x")));
        assert_eq!(empty.current().unwrap().id, "x");

        // Taking one out keeps the song playing where it is; the one playing stays.
        q.index = Some(2);
        q.remove(0);
        assert_eq!(ids(&q), ["b", "c"]);
        assert_eq!(q.current().unwrap().id, "c");
        q.remove(1);
        q.remove(5);
        assert_eq!(ids(&q), ["b", "c"]);
    }

    #[test]
    fn playing_next_a_queued_song_moves_that_row() {
        // The queue plays "a", with "now" heard before it and "b"/"c" still to come.
        let mut q = queue(&["now", "a", "b", "c"], 1, Repeat::Off);
        // A row further down comes up to play next, and is not copied.
        assert!(q.play_next_at(3));
        assert_eq!(ids(&q), ["now", "a", "c", "b"]);
        // A row already heard goes just after the song playing, which keeps its place.
        assert!(q.play_next_at(0));
        assert_eq!(ids(&q), ["a", "now", "c", "b"]);
        assert_eq!(q.current().unwrap().id, "a");
        // The song playing itself, and a row past the end: nothing to move.
        assert!(!q.play_next_at(0));
        assert!(!q.play_next_at(9));
        // Nothing playing: there's no song to put anything after.
        q.index = None;
        assert!(!q.play_next_at(2));
    }

    #[test]
    fn a_queued_song_can_move_one_place() {
        let mut q = queue(&["now", "a", "b", "c"], 0, Repeat::Off);
        // The song playing and the one before it stay where they are.
        assert!(!q.shift(0, 1));
        assert!(!q.shift(1, -1));
        assert!(q.shift(1, 1));
        assert_eq!(ids(&q), ["now", "b", "a", "c"]);
        // The last row has nowhere to go down to.
        assert!(!q.shift(3, 1));
        assert!(q.shift(3, -1));
        assert_eq!(ids(&q), ["now", "b", "c", "a"]);
        // Nothing playing: every row is coming up, so the first may move down.
        let mut q = Queue::new(Repeat::Off);
        q.replace(vec![track("a"), track("b")], 0, None, false);
        q.index = None;
        assert!(q.shift(0, 1));
        assert_eq!(ids(&q), ["b", "a"]);
    }

    #[test]
    fn radio_adds_only_new_songs() {
        let mut q = queue(&["seed", "a"], 1, Repeat::Off);
        assert!(q.add_radio("seed", vec![track("seed"), track("a"), track("b"), track("c")]));
        assert_eq!(ids(&q), ["seed", "a", "b", "c"]);
        assert!(!q.add_radio("seed", vec![track("d")]));
    }

    #[test]
    fn a_playlist_still_coming_in_fills_the_queue() {
        let mut q = Queue::new(Repeat::Off);
        q.replace(vec![track("a"), track("b")], 0, filling("PL1", 3), false);
        // Another playlist's page, or another load's, doesn't join.
        assert!(q.fill_from("PL2", 3, vec![track("x")], true, true).is_none());
        assert!(q.fill_from("PL1", 2, vec![track("x")], true, true).is_none());
        let f = q.fill_from("PL1", 3, vec![track("c")], true, true).unwrap();
        assert!(!f.ran_out && !f.was_at_end);
        assert_eq!(ids(&q), ["a", "b", "c"]);

        // Playback reached the end before the next page: it waits, then carries on.
        q.index = Some(2);
        assert!(!q.advance());
        assert!(q.fill.as_ref().unwrap().ran_out);
        let f = q.fill_from("PL1", 3, vec![track("d")], false, true).unwrap();
        assert!(f.ran_out && f.was_at_end);
        assert!(q.fill.is_none());
        assert!(q.advance());
        assert_eq!(q.current().unwrap().id, "d");
    }

    #[test]
    fn waiting_for_a_playlist_comes_before_repeating_it() {
        let mut q = Queue::new(Repeat::All);
        q.replace(vec![track("a"), track("b")], 1, filling("PL1", 1), false);
        // At the end of what came: it waits rather than starting over.
        assert!(!q.advance());
        assert_eq!(q.index, Some(1));
        // The load ends with nothing more: playback goes on, and now starts over.
        let f = q.fill_from("PL1", 1, vec![], false, true).unwrap();
        assert!(f.ran_out && f.was_at_end);
        assert!(q.fill.is_none());
        assert!(q.advance());
        assert_eq!(q.index, Some(0));
    }

    #[test]
    fn filling_stops_when_something_else_plays() {
        let mut q = Queue::new(Repeat::Off);
        q.replace(vec![track("a")], 0, filling("PL1", 1), false);
        // The queue plays something else now: the page doesn't join, and the last one ends the fill.
        assert!(q.fill_from("PL1", 1, vec![track("b")], true, false).is_none());
        assert!(q.fill.is_some());
        assert!(q.fill_from("PL1", 1, vec![track("c")], false, false).is_none());
        assert!(q.fill.is_none());
        assert_eq!(ids(&q), ["a"]);
        // A song starting means it no longer waits.
        q.replace(vec![track("a")], 0, filling("PL1", 2), false);
        q.advance();
        q.started();
        assert!(!q.fill.as_ref().unwrap().ran_out);
    }

    #[test]
    fn shuffling_up_next_leaves_what_played() {
        let songs: Vec<String> = (0..40).map(|i| i.to_string()).collect();
        let refs: Vec<&str> = songs.iter().map(String::as_str).collect();
        let mut q = queue(&refs, 5, Repeat::Off);
        q.fill = filling("PL1", 1);
        q.set_shuffle(true);
        assert!(q.shuffled);
        assert_eq!(ids(&q)[..6], refs[..6]);
        let mut rest: Vec<&str> = ids(&q)[6..].to_vec();
        // 34 songs coming back in their order by chance is too unlikely to happen.
        assert_ne!(rest, refs[6..]);
        rest.sort_by_key(|s| s.parse::<u32>().unwrap());
        assert_eq!(rest, refs[6..]);
        // Switching it off puts them back the way the list has them.
        q.set_shuffle(false);
        assert!(!q.shuffled);
        assert_eq!(ids(&q), refs);
        // At the last song there's nothing to move, but the mark still changes.
        let mut q = queue(&["a", "b"], 1, Repeat::Off);
        q.set_shuffle(true);
        assert!(q.shuffled);
        assert_eq!(ids(&q), ["a", "b"]);
    }

    #[test]
    fn a_shuffled_playlists_next_pages_mix_in_after_the_next_song() {
        let songs: Vec<String> = (0..40).map(|i| i.to_string()).collect();
        let refs: Vec<&str> = songs.iter().map(String::as_str).collect();
        // The song after the current one stays: it may be loading already.
        let mut q = queue(&refs[..10], 2, Repeat::Off);
        q.fill = filling("PL1", 1);
        q.set_shuffle(true);
        let head: Vec<String> = ids(&q)[..4].iter().map(|s| s.to_string()).collect();
        let more: Vec<Track> = (10..40).map(|i| track(&i.to_string())).collect();
        q.fill_from("PL1", 1, more, true, true).unwrap();
        assert_eq!(ids(&q)[..4], head);
        let mut rest: Vec<&str> = ids(&q)[4..].to_vec();
        assert_ne!(rest, refs[4..]);
        // Every song that's still to come is here once; switching shuffle off hands the
        // list's own order back for all of them, the next song included.
        rest.sort_by_key(|s| s.parse::<u32>().unwrap());
        let mut want: Vec<&str> = refs[3..].to_vec();
        want.retain(|id| *id != head[3]);
        assert_eq!(rest, want);
        q.set_shuffle(false);
        assert_eq!(ids(&q)[3..], refs[3..]);
    }

    #[test]
    fn a_saved_session_keeps_the_songs_around_the_current_one() {
        let songs: Vec<String> = (0..300).map(|i| i.to_string()).collect();
        let refs: Vec<&str> = songs.iter().map(String::as_str).collect();
        let q = queue(&refs, 120, Repeat::Off);
        let (saved, i) = q.saved().unwrap();
        assert_eq!((saved.len(), i), (200, 50));
        assert_eq!(saved[i].id, "120");
        let q = queue(&refs[..10], 3, Repeat::Off);
        let (saved, i) = q.saved().unwrap();
        assert_eq!((saved.len(), i), (10, 3));
        assert!(Queue::new(Repeat::Off).saved().is_none());
    }
}
