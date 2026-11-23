//! "Best Part": where a song is worth jumping to, worked out from what other people do
//! with it and from its words, never from its audio.
//!
//! The answer comes from YouTube's "Most replayed" curve — the heat markers its watch
//! page draws on the scrubber, which say where listeners keep dragging back, rather than
//! where the mix is loudest. A song too few have watched to be charted falls back to the
//! first time its chorus comes round, read off the synced lyrics the player already has.
//! Nothing is decoded, so the answer costs one request and no battery.
//!
//! Ported from BitChord's `BestPart.kt` (v1.10); keep the two in step.

use std::collections::HashMap;

use serde_json::Value;

/// Nothing in a song's opening seconds counts: everyone "replays" the start.
const SKIP_HEAD_MS: i64 = 5_000;
/// A best part needs some song left after it to be worth jumping to.
const SKIP_TAIL_MS: i64 = 30_000;
/// Interludes and skits have no best part to skip to.
const MIN_SONG_MS: i64 = 60_000;
/// Fewer markers than this is a video YouTube has too few views on to chart.
const MIN_MARKERS: usize = 10;
/// How far, as a share, the peak has to stand above the trend around it.
const MIN_RISE: f32 = 0.08;
/// And at least this much in absolute terms, since the curves run 0..1.
const MIN_LIFT: f32 = 0.03;
/// The smoothing either side of a moment: one hot second shouldn't beat a hot chorus.
const SMOOTH_HALF_MS: f64 = 4_500.0;
/// The trend around a moment is the median of the curve this far either side.
const TREND_HALF_MS: f64 = 45_000.0;
/// A trend this low is treated as this, so a near-silent stretch can't make any bump look huge.
const TREND_FLOOR: f32 = 0.05;

/// YouTube's "Most replayed" curve: how much each `segment_ms` stretch of a video gets
/// replayed, 0..1, start to end.
#[derive(Clone, Debug, PartialEq)]
pub struct Heatmap {
    pub segment_ms: i64,
    pub intensity: Vec<f32>,
}

impl Heatmap {
    pub fn duration_ms(&self) -> i64 {
        self.segment_ms * self.intensity.len() as i64
    }
}

/// What said where a song's best part starts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Source {
    /// The bump in YouTube's "Most replayed" curve.
    Heatmap,
    /// The first time the chorus comes round in the synced lyrics.
    Lyrics,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Start {
    pub at_ms: i64,
    pub source: Source,
}

/// The heat markers in a youtube.com `next` answer, or `None` when the video has none.
///
/// They sit in a `macroMarkersListEntity` under the answer's framework updates, which
/// have moved around more than once, so this looks for the entity anywhere in the answer
/// rather than down a fixed path. A chapters entity is the same shape and is skipped.
pub fn parse_heatmap(answer: &Value) -> Option<Heatmap> {
    let mut found: Vec<&Value> = Vec::new();
    walk(answer, "macroMarkersListEntity", &mut found);
    let entity = found.into_iter().find(|e| e["markersList"]["markerType"].as_str() == Some("MARKER_TYPE_HEATMAP"))?;
    let mut markers: Vec<(i64, i64, f32)> = arr(&entity["markersList"]["markers"])
        .iter()
        .filter_map(|m| {
            let heat = num(m.get("intensityScoreNormalized")?)?;
            Some((num_i(&m["startMillis"])?, num_i(&m["durationMillis"])?, heat.clamp(0.0, 1.0)))
        })
        .collect();
    if markers.len() < MIN_MARKERS {
        return None;
    }
    markers.sort_by_key(|m| m.0);
    // The segments are equal (a hundredth of the video each). The median is taken rather
    // than the first in case the last one is cut short.
    let durations: Vec<i64> = markers.iter().map(|m| m.1).collect();
    let segment = durations[durations.len() / 2];
    if segment <= 0 {
        return None;
    }
    Some(Heatmap { segment_ms: segment, intensity: markers.iter().map(|m| m.2).collect() })
}

fn walk<'a>(v: &'a Value, key: &str, out: &mut Vec<&'a Value>) {
    match v {
        Value::Array(list) => {
            for item in list {
                walk(item, key, out);
            }
        }
        Value::Object(map) => {
            for (k, val) in map {
                if k == key {
                    out.push(val);
                } else {
                    walk(val, key, out);
                }
            }
        }
        _ => {}
    }
}

fn arr(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or_default()
}

/// YouTube writes these as strings and, in newer answers, as numbers; either counts.
fn num_i(v: &Value) -> Option<i64> {
    v.as_str().and_then(|s| s.parse().ok()).or_else(|| v.as_i64())
}

fn num(v: &Value) -> Option<f32> {
    v.as_f64().map(|x| x as f32).or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// Where the most replayed stretch begins, in ms, for a song `duration_ms` long (the
/// curve's own length when 0). `None` when nothing stands out or the song is too short.
///
/// The hottest moment is no use on its own. A curve mostly falls from start to end,
/// because people drift off as a song goes on, so its maximum is nearly always the first
/// few seconds. What marks the best part is a bump above that falling trend, so every
/// moment is measured against the median of the curve around it and the biggest rise wins.
pub fn peak_ms(heat: &Heatmap, duration_ms: i64) -> Option<i64> {
    let duration = if duration_ms > 0 { duration_ms } else { heat.duration_ms() };
    let n = heat.intensity.len();
    if duration < MIN_SONG_MS || n < MIN_MARKERS {
        return None;
    }
    let seg = heat.segment_ms;
    let half = (SMOOTH_HALF_MS / seg as f64).round().max(1.0) as usize;
    let smooth: Vec<f32> = (0..n)
        .map(|i| {
            let (from, to) = (i.saturating_sub(half), (i + half).min(n - 1));
            heat.intensity[from..=to].iter().sum::<f32>() / (to - from + 1) as f32
        })
        .collect();
    let wide = (TREND_HALF_MS / seg as f64).round().max(3.0) as usize;
    let trend: Vec<f32> = (0..n)
        .map(|i| {
            // The opening segment is left out: its heat is everyone pressing play.
            let from = (i as i64 - wide as i64).max(1) as usize;
            let to = (i + wide).min(n - 1);
            let mut window: Vec<f32> = smooth[from..=to].to_vec();
            window.sort_by(|a, b| a.total_cmp(b));
            window[window.len() / 2]
        })
        .collect();
    let rise = |i: usize| smooth[i] / trend[i].max(TREND_FLOOR) - 1.0;

    let head = SKIP_HEAD_MS.max(2 * seg);
    let ok = |i: usize| (i as i64) * seg >= head && (i as i64) * seg <= duration - SKIP_TAIL_MS;
    let peak = (0..n).filter(|i| ok(*i)).max_by(|a, b| rise(*a).total_cmp(&rise(*b)))?;
    if rise(peak) < MIN_RISE || smooth[peak] - trend[peak] < MIN_LIFT {
        return None;
    }
    // Back from the peak to where the heat starts climbing towards it: half the peak's
    // rise. That is where the chorus starts, not where it's hottest.
    let floor = rise(peak) * 0.5;
    let mut start = peak;
    while start > 0 && ok(start - 1) && rise(start - 1) >= floor {
        start -= 1;
    }
    // The smoothed curve climbs gradually; the raw segments say where it jumps.
    for i in start..=peak {
        if heat.intensity[i] / trend[i].max(TREND_FLOOR) - 1.0 >= floor {
            return Some((i as i64) * seg);
        }
    }
    Some((start as i64) * seg)
}

/// The first time the chorus comes round, from time-synced `lines`: the earliest line of
/// the most repeated lyric, walked back over the lines of its block that come before it.
/// `None` when nothing repeats.
pub fn chorus_ms(lines: &[crate::lyrics::Line], duration_ms: i64) -> Option<i64> {
    if duration_ms >= 1 && duration_ms < MIN_SONG_MS {
        return None;
    }
    let timed: Vec<&crate::lyrics::Line> =
        lines.iter().filter(|l| l.start >= 0 && !l.instrumental && !l.text.trim().is_empty()).collect();
    if timed.len() < 4 {
        return None;
    }
    let keys: Vec<String> = timed.iter().map(|l| normalize(&l.text)).collect();
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for k in &keys {
        if k.chars().count() >= 4 {
            *counts.entry(k.as_str()).or_default() += 1;
        }
    }
    let most = *counts.values().max()?;
    if most < 2 {
        return None;
    }
    let count = |k: &str| counts.get(k).copied().unwrap_or(0);
    let limit = if duration_ms > 0 { duration_ms - SKIP_TAIL_MS } else { i64::MAX };
    let at = |i: usize| timed[i].start as i64;
    let first = (0..keys.len()).find(|&i| count(&keys[i]) == most && at(i) >= SKIP_HEAD_MS && at(i) <= limit)?;
    // Walk back over lines that repeat about as often: they are the same block, and a
    // chorus is heard from its first line, not its hook line.
    let mut start = first;
    while start > 0 {
        let c = count(&keys[start - 1]);
        if c < 2 || c < most - 1 || at(start - 1) < SKIP_HEAD_MS || first - start >= 4 {
            break;
        }
        start -= 1;
    }
    Some(at(start))
}

fn normalize(text: &str) -> String {
    text.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect()
}

/// Pulls `target_ms` back to a natural start: a lyric line beginning near it (just
/// before, or a moment after), else a second before it, so a clip doesn't open mid-word.
pub fn snap(target_ms: i64, line_starts: &[i64]) -> i64 {
    match line_starts
        .iter()
        .filter(|s| **s >= target_ms - 4_000 && **s <= target_ms + 1_500)
        .min_by_key(|s| (**s - target_ms).abs() + if **s > target_ms { 1_500 } else { 0 })
    {
        Some(s) => (s - 250).max(0),
        None => (target_ms - 1_000).max(0),
    }
}

/// Where the best part of a song `duration_ms` long starts: its "Most replayed" curve if
/// that has a bump, else the chorus in its synced words. `None` when neither has anything
/// to say — the player offers the jump rather than guessing at a point.
pub fn pick(heat: Option<&Heatmap>, lines: &[crate::lyrics::Line], duration_ms: i64) -> Option<Start> {
    let starts: Vec<i64> = lines.iter().filter(|l| l.start >= 0 && !l.instrumental).map(|l| l.start as i64).collect();
    if let Some(heat) = heat {
        if let Some(peak) = peak_ms(heat, duration_ms) {
            return Some(Start { at_ms: snap(peak, &starts), source: Source::Heatmap });
        }
    }
    let chorus = chorus_ms(lines, duration_ms)?;
    Some(Start { at_ms: snap(chorus, &starts), source: Source::Lyrics })
}

// ─────────────────────────────────────────────────────── the curves on disk

/// How long a curve stays worth using: YouTube's own replays don't move much in a month.
const MONTH_MS: i64 = 30 * 24 * 3_600_000;
/// And how long "this video has no curve" stays worth using, since a song that is new
/// now has the views for one within a week.
const WEEK_MS: i64 = 7 * 24 * 3_600_000;
/// Curves kept, newest first: the file is a listener's listening, not a library.
const MAX_KEPT: usize = 500;
const FILE_NAME: &str = "replay_heatmaps.json";

/// A curve as it was fetched. Empty `intensity` means the video had none.
#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
struct StoredHeat {
    segment_ms: i64,
    intensity: Vec<f32>,
    fetched_at: i64,
}

impl StoredHeat {
    fn heat(&self) -> Option<Heatmap> {
        (self.segment_ms > 0 && !self.intensity.is_empty()).then(|| Heatmap { segment_ms: self.segment_ms, intensity: self.intensity.clone() })
    }
}

/// The curves fetched so far, in one JSON file in the app's own folder. Only curves are
/// kept, never the starts: a start depends on the lyrics as well, and those are not
/// stored anywhere, so it is worked out again each time from the curve plus whatever
/// lyrics come back.
#[derive(Default)]
pub struct Curves {
    file: std::path::PathBuf,
    heats: HashMap<String, StoredHeat>,
}

impl Curves {
    /// Read the cache, if there is one. An unreadable file is thrown away rather than
    /// trusted: a broken one would keep every song on its fallback.
    pub fn open(dir: &std::path::Path) -> Self {
        let file = dir.join(FILE_NAME);
        let mut out = Self { file, heats: Default::default() };
        let Some(text) = std::fs::read_to_string(&out.file).ok() else { return out };
        match serde_json::from_str::<HashMap<String, StoredHeat>>(&text) {
            Ok(heats) => out.heats = heats,
            Err(_) => {
                let _ = std::fs::remove_file(&out.file);
            }
        }
        out
    }

    /// The answer kept about `id`, when it is still in date. `Some(None)` is a kept
    /// "this video has no curve"; `None` says to ask.
    pub fn fresh(&self, id: &str, now: i64) -> Option<Option<Heatmap>> {
        let kept = self.heats.get(id)?;
        let heat = kept.heat();
        let life = if heat.is_some() { MONTH_MS } else { WEEK_MS };
        (now - kept.fetched_at < life).then_some(heat)
    }

    /// The last answer about `id`, however old. A stale curve beats nothing when the
    /// network won't answer.
    pub fn stale(&self, id: &str) -> Option<Heatmap> {
        self.heats.get(id).and_then(StoredHeat::heat)
    }

    /// Remember what youtube.com said. A request that failed is kept nowhere: offline or
    /// a timeout is not an answer about the video, and keeping it as "none" would leave
    /// the song on its fallback for a week after the network came back.
    pub fn put(&mut self, id: &str, heat: Option<&Heatmap>, now: i64) {
        self.heats.insert(
            id.to_owned(),
            heat.map(|h| StoredHeat { segment_ms: h.segment_ms, intensity: h.intensity.clone(), fetched_at: now })
                .unwrap_or(StoredHeat { fetched_at: now, ..Default::default() }),
        );
        let mut keep: Vec<(String, StoredHeat)> = self.heats.drain().collect();
        keep.sort_by_key(|(_, h)| std::cmp::Reverse(h.fetched_at));
        keep.truncate(MAX_KEPT);
        self.heats = keep.into_iter().collect();
        if let Ok(text) = serde_json::to_string(&self.heats) {
            let _ = std::fs::write(&self.file, text);
        }
    }
}

/// The wall clock, in milliseconds: the cache ages across runs, so it can't be a
/// stopwatch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// Whether this is a YouTube video id rather than a file's path or an addon's key: only
/// a video youtube.com knows has a replay curve.
pub fn is_video_id(id: &str) -> bool {
    id.len() == 11 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::{Heatmap, Source, Start, chorus_ms, parse_heatmap, peak_ms, pick, snap};
    use serde_json::{Value, json};

    /// A four-minute song in YouTube's hundred equal segments.
    const SEGMENT: i64 = 2_400;

    /// How a real curve looks: hot at the very start, sagging as people drift off, with
    /// the chorus standing up out of the sag at `bump_from..bump_to`.
    fn decaying(bump: Option<(usize, usize)>) -> Heatmap {
        Heatmap {
            segment_ms: SEGMENT,
            intensity: (0..100)
                .map(|i| {
                    let base = if i == 0 { 1.0 } else { 0.6 - 0.003 * i as f32 };
                    base + if bump.is_some_and(|(f, t)| i >= f && i <= t) { 0.25 } else { 0.0 }
                })
                .collect(),
        }
    }

    fn next_answer(markers: &[f32], kind: &str) -> Value {
        let list: Vec<Value> = markers
            .iter()
            .enumerate()
            .map(|(i, heat)| json!({ "startMillis": (i as i64 * SEGMENT).to_string(), "durationMillis": SEGMENT.to_string(), "intensityScoreNormalized": heat }))
            .collect();
        json!({ "frameworkUpdates": { "entityBatchUpdate": { "mutations": [
            // A chapters entity first: it is the same shape and must be skipped.
            { "payload": { "macroMarkersListEntity": { "markersList": { "markerType": "MARKER_TYPE_CHAPTERS", "markers": [] } } } },
            { "payload": { "macroMarkersListEntity": { "markersList": { "markerType": kind, "markers": list } } } }
        ] } } })
    }

    #[test]
    fn the_heat_markers_are_read_out_of_the_next_answer() {
        let heat = parse_heatmap(&next_answer(&decaying(Some((40, 50))).intensity, "MARKER_TYPE_HEATMAP")).unwrap();
        assert_eq!(heat.segment_ms, SEGMENT);
        assert_eq!(heat.intensity.len(), 100);
        assert_eq!(heat.intensity[0], 1.0);
    }

    #[test]
    fn an_answer_without_heat_markers_has_no_curve() {
        assert_eq!(parse_heatmap(&next_answer(&decaying(None).intensity, "MARKER_TYPE_CHAPTERS")), None);
        assert_eq!(parse_heatmap(&json!({ "contents": "nothing" })), None);
        // Fewer than the chart's own hundred is a video too few have watched.
        assert_eq!(parse_heatmap(&next_answer(&[0.5; 6], "MARKER_TYPE_HEATMAP")), None);
    }

    #[test]
    fn the_chorus_bump_wins_over_the_hot_opening() {
        let start = peak_ms(&decaying(Some((40, 50))), 240_000).unwrap();
        // Where the bump starts climbing, not its middle and not the opening.
        assert!((38 * SEGMENT..=41 * SEGMENT).contains(&start), "started at {start}");
    }

    #[test]
    fn a_flat_curve_and_a_short_song_have_no_best_part() {
        assert_eq!(peak_ms(&Heatmap { segment_ms: SEGMENT, intensity: vec![0.4; 100] }, 240_000), None);
        assert_eq!(peak_ms(&decaying(Some((40, 50))), 50_000), None);
        // 97..99 is the final seven seconds of a four-minute song.
        assert_eq!(peak_ms(&decaying(Some((97, 99))), 240_000), None);
    }

    fn line(start: i32, text: &str) -> crate::lyrics::Line {
        crate::lyrics::Line {
            start,
            dur: 4_000,
            text: text.into(),
            parts: Vec::new(),
            agent: None,
            instrumental: false,
            translations: Vec::new(),
            romanization: None,
            timed_roman: Vec::new(),
        }
    }

    fn lyrics() -> Vec<crate::lyrics::Line> {
        [
            (10_000, "Woke up early in the morning"),
            (14_000, "Coffee on the window sill"),
            (18_000, "Every road is getting longer"),
            (22_000, ""),
            (30_000, "Hold on to me"),
            (34_000, "Never let go"),
            (50_000, "Streetlights humming out of tune"),
            (54_000, "Somebody singing on the bus"),
            (70_000, "Hold on to me"),
            (74_000, "Never let go"),
            (110_000, "Hold on, to me!"),
            (114_000, "Never let go"),
        ]
        .into_iter()
        .map(|(ms, t)| line(ms, t))
        .collect()
    }

    #[test]
    fn the_chorus_is_where_the_most_repeated_lines_first_come() {
        assert_eq!(chorus_ms(&lyrics(), 180_000), Some(30_000));
        let verses: Vec<_> = lyrics().into_iter().filter(|l| !l.text.contains("Hold") && !l.text.contains("Never")).collect();
        assert_eq!(chorus_ms(&verses, 180_000), None);
    }

    #[test]
    fn the_start_is_pulled_back_to_a_line_beginning() {
        // The earlier line is preferred over a slightly nearer later one.
        assert_eq!(snap(96_000, &[95_200, 96_500]), 94_950);
        // No line nearby: a second early.
        assert_eq!(snap(96_000, &[10_000, 200_000]), 95_000);
    }

    #[test]
    fn the_curve_is_trusted_before_the_words() {
        assert_eq!(pick(Some(&decaying(Some((40, 50)))), &lyrics(), 240_000).unwrap().source, Source::Heatmap);
        assert_eq!(pick(None, &lyrics(), 240_000).unwrap().source, Source::Lyrics);
        assert_eq!(pick(Some(&Heatmap { segment_ms: SEGMENT, intensity: vec![0.4; 100] }), &lyrics(), 240_000).unwrap().source, Source::Lyrics);
        // Nothing to go on is no answer at all, rather than a guess.
        assert_eq!(pick(None, &[], 200_000), None);
        assert_eq!(pick(None, &lyrics(), 240_000), Some(Start { at_ms: 29_750, source: Source::Lyrics }));
    }

    /// Where the button would land on real videos: the anonymous youtube.com `next`
    /// answer, its curve parsed, its peak. Prints ids and numbers only, no address.
    /// Needs the network:
    /// `cargo test --release -- --ignored replay_curves_live --nocapture`
    #[test]
    #[ignore]
    fn replay_curves_live() {
        let ids: Vec<String> = std::env::var("YTM_VIDEO")
            .map(|s| s.split(',').map(|i| i.trim().to_owned()).filter(|i| !i.is_empty()).collect())
            .unwrap_or_else(|_| ["x8tfvLUkIAs", "dQw4w9WgXcQ"].iter().map(|s| (*s).to_string()).collect());
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        for id in ids {
            let answer = match rt.block_on(crate::innertube::web_next(&crate::proxy::http(), &id)) {
                Ok(a) => a,
                Err(e) => {
                    println!("{id}: no answer ({e})");
                    continue;
                }
            };
            match parse_heatmap(&answer) {
                Some(heat) => println!("{id}: {} segments of {} ms, peak {:?}", heat.intensity.len(), heat.segment_ms, peak_ms(&heat, heat.duration_ms())),
                None => println!("{id}: no curve"),
            }
        }
    }
}
