//! App-side data types, parsed from YouTube Music's responses in `ytm.rs`.

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Thumb {
    pub url: String,
    pub width: u32,
    pub height: u32,
}

impl Thumb {
    /// UV rect that shows a centred square of this image, trimming the letterboxing of
    /// 4:3 video thumbnails and the sides of 16:9 ones.
    pub fn square_uv(&self) -> egui::Rect {
        use egui::{pos2, Rect};
        // Google's crop hands back a square of the picture's centre, so the whole
        // texture already is the picture: nothing to trim off it.
        if google_image(&self.url) {
            return Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        }
        let ratio = if self.height > 0 { self.width as f32 / self.height as f32 } else { 1.0 };
        if ratio > 1.6 {
            let w = 1.0 / ratio;
            Rect::from_min_max(pos2(0.5 - w / 2.0, 0.0), pos2(0.5 + w / 2.0, 1.0))
        } else if ratio > 1.2 {
            // 16:9 picture inside a 4:3 frame: content is the middle 75% vertically.
            let w = 0.75 / ratio;
            Rect::from_min_max(pos2(0.5 - w / 2.0, 0.125), pos2(0.5 + w / 2.0, 0.875))
        } else {
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))
        }
    }
}

/// Whether Google's image CDN serves this URL.
pub fn google_image(url: &str) -> bool {
    url.contains("googleusercontent.com") || url.contains("ggpht.com")
}

/// YouTube's thumbnail renditions, smallest to largest. Every video has `hqdefault`.
const RENDITIONS: [&str; 6] = ["default", "mqdefault", "hqdefault", "sddefault", "hq720", "maxresdefault"];
/// The sizes an old or low-resolution upload doesn't have: YouTube answers 404 for them.
const UPGRADED: [&str; 3] = ["sddefault", "hq720", "maxresdefault"];

/// Whether YouTube's own thumbnail hosts serve this URL.
fn youtube_image(url: &str) -> bool {
    let host = url.split("://").nth(1).or_else(|| url.strip_prefix("//")).unwrap_or_default().split(['/', '?', ':']).next().unwrap_or_default().to_lowercase();
    host == "i.ytimg.com" || host.ends_with(".ytimg.com") || host == "img.youtube.com"
}

/// The rendition of a YouTube thumbnail: where its name starts in `url`, and which one
/// it is. A URL whose file isn't one of the named sizes isn't a thumbnail.
fn rendition(url: &str) -> Option<(usize, &'static str)> {
    let i = url.rfind('/')? + 1;
    let (file, _) = url[i..].split_once('?').unwrap_or((&url[i..], ""));
    let (name, _) = file.split_once('.')?;
    let name = RENDITIONS.iter().find(|r| **r == name).copied()?;
    Some((i, name))
}

/// `url` naming `to` instead, with the query dropped: it signs the rendition it came
/// with, and the bare path serves any of them.
fn with_rendition(url: &str, from: usize, name: &str, to: &str) -> String {
    // Everything past the old name: its extension, then whatever query signed it.
    let rest = &url[from + name.len()..];
    let tail = rest.split_once('?').map_or(rest, |(path, _)| path);
    format!("{}{}{}", &url[..from], to, tail)
}

/// The thumbnail for a slot `px` wide: the biggest rendition that's worth it, leaving
/// the URL alone when it isn't a YouTube thumbnail or already the size asked for.
///
/// Uploading age decides what answers: `sddefault` and `maxresdefault` are 404 for the
/// oldest clips, so a loader has to fall back — see [thumbnail_fallback].
fn upgrade_thumbnail(url: &str, px: u32) -> Option<String> {
    let (from, name) = rendition(url).filter(|_| youtube_image(url))?;
    let to = match px {
        _ if px >= 480 => "maxresdefault",
        _ if px >= 240 => "sddefault",
        // A row keeps the small copy it came with, but an upgraded URL — one read back
        // off a saved queue, say — comes back down to the size every video has.
        _ if UPGRADED.contains(&name) => "hqdefault",
        _ => return Some(url.to_owned()),
    };
    Some(if to == name { url.to_owned() } else { with_rendition(url, from, name, to) })
}

/// The copy every YouTube video has, for an upgraded rendition [upgrade_thumbnail]
/// asked a clip for that turns out not to have one. `None` for any other URL.
pub fn thumbnail_fallback(url: &str) -> Option<String> {
    let (from, name) = rendition(url).filter(|_| youtube_image(url))?;
    UPGRADED.contains(&name).then(|| with_rendition(url, from, name, "hqdefault"))
}

/// Fisher-Yates with a clock-seeded xorshift; shuffling doesn't need a real RNG crate.
pub fn shuffle<T>(v: &mut [T]) {
    let mut x = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0x9e37_79b9_7f4a_7c15, |d| d.as_nanos() as u64)
        | 1;
    for i in (1..v.len()).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.swap(i, (x % (i as u64 + 1)) as usize);
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Link {
    pub name: String,
    /// Browse id (artist channel / album) when the name is clickable.
    pub id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artists: Vec<Link>,
    pub album: Option<Link>,
    /// Seconds; unknown for some home-feed items until playback reports it.
    pub duration: Option<u32>,
    pub thumbs: Vec<Thumb>,
    pub track_nr: Option<u16>,
    /// Play count, as YouTube Music shows it on artist pages and in search.
    pub plays: Option<u64>,
    /// An official music video (not the song), which plays its song version instead
    /// when one is found and that's switched on.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub music_video: bool,
    /// A podcast episode: it plays like a song and sits among them in search, but no one
    /// asked for it when they clicked a song and let the radio take over.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub episode: bool,
}

impl Track {
    pub fn artist_line(&self) -> String {
        self.artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")
    }

    pub fn first_artist_id(&self) -> Option<&str> {
        self.artists.iter().find_map(|a| a.id.as_deref())
    }

    /// A song rather than a video or an episode — what a radio may put in the queue when
    /// the listener only wants songs.
    pub fn is_song(&self) -> bool {
        !self.music_video && !self.episode
    }

    /// Whether the upload says AI made it. YouTube Music's own badge is drawn from
    /// metadata that isn't in anything the API this app talks to returns — searched in
    /// the song page, the queue, the album, the artist page and the search rows, and no
    /// response carries it — so the words the upload itself wears are all there is.
    pub fn ai_labelled(&self) -> bool {
        says_ai(&self.title) || says_ai(&self.artist_line()) || self.album.as_ref().is_some_and(|a| says_ai(&a.name)) || titled_ai(&self.title)
    }
}

/// A name that opens with the label: "AI Cover of …", "AI Generated 90s Hit". Only a
/// title, because an artist called AI is a person and not a confession.
fn titled_ai(text: &str) -> bool {
    let low = text.to_lowercase();
    let words: Vec<&str> = low.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let ai = matches!(words.first(), Some(&"ai")) || (matches!(words.first(), Some(&"a")) && words.get(1) == Some(&"i"));
    ai && words.len() > if words.get(1) == Some(&"i") { 2 } else { 1 }
}

/// The words an upload uses to say AI made the music. "AI" has to stand as its own word
/// beside them: the letters end names ("Nai") and sit inside places ("Miami"), and those
/// aren't labels.
pub fn says_ai(text: &str) -> bool {
    let low = text.to_lowercase();
    // The bare label in brackets, as uploads write it: "[AI]", "(AI)".
    if low.contains("[ai]") || low.contains("(ai)") {
        return true;
    }
    // Hyphens and brackets fall away, so "AI-Generated" reads as "AI" then "Generated".
    let words: Vec<&str> = low.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    // The tools named on their own said who made the music as plainly as any label.
    if words.iter().any(|w| matches!(*w, "suno" | "udio" | "riffusion" | "mureka" | "mubert" | "soundraw" | "trendsound")) {
        return true;
    }
    words.iter().enumerate().any(|(i, w)| {
        // "A.I." falls apart into a's and i's, so the label is the second of the pair.
        let last = match (*w, words.get(i + 1)) {
            ("ai", _) => i,
            ("a", Some(&"i")) => i + 1,
            _ => return false,
        };
        let prev = last.checked_sub(1).map_or("", |j| words[j]);
        let prev2 = last.checked_sub(2).map_or("", |j| words[j]);
        let next = words.get(last + 1).copied().unwrap_or_default();
        // The tool named before it: "Suno AI", "Udio AI".
        matches!(prev, "suno" | "udio" | "riffusion" | "mureka")
            || matches!(next, "generated" | "generate" | "generation" | "cover" | "covers" | "version" | "versions" | "remix" | "remixes" | "mashup" | "remake" | "song" | "songs" | "music" | "album" | "vocals" | "voice" | "voiced" | "singer" | "artist" | "made" | "created")
            // "Generated by AI", "made with AI", "sung using an AI".
            || (matches!(prev, "by" | "with" | "using" | "an") && matches!(prev2, "generated" | "made" | "written" | "composed" | "created" | "sung" | "voiced" | "produced" | "synthesized" | "synthesised" | "entirely" | "fully"))
    })
}

/// Smallest thumbnail at least `min_px` wide (falls back to the largest).
pub fn pick_thumb(thumbs: &[Thumb], min_px: u32) -> Option<&Thumb> {
    thumbs.iter().filter(|t| t.width >= min_px).min_by_key(|t| t.width).or_else(|| thumbs.iter().max_by_key(|t| t.width))
}

pub fn pick(thumbs: &[Thumb], min_px: u32) -> Option<&str> {
    pick_thumb(thumbs, min_px).map(|t| t.url.as_str())
}

/// Ask Google's image CDN for a specific size (`=w226-h226…` / `=s120` suffixes), so
/// thumbnails are sharp at the size drawn without downloading huge originals.
pub fn sized(url: &str, px: u32) -> String {
    // YouTube's own thumbnails have no size suffix to rewrite: they come in fixed
    // renditions, and the biggest one worth the slot is picked instead.
    if youtube_image(url) {
        return upgrade_thumbnail(url, px).unwrap_or_else(|| url.to_owned());
    }
    if (url.contains("googleusercontent.com") || url.contains("ggpht.com"))
        && let Some(i) = url.rfind('=')
    {
        // A square one that asked to be cropped to fill (`-p`, `-c`; artist photos are
        // often portrait) keeps that, or it comes back its own shape and is drawn
        // stretched into the square.
        let crop = square_crop(&url[i + 1..]);
        return format!("{}=w{px}-h{px}{}-l90-rj", &url[..i], if crop { "-p" } else { "" });
    }
    url.to_owned()
}

/// Whether image options like `w120-h120-p-l90-rj` or `s120-c-k-no` ask for a square crop.
fn square_crop(opts: &str) -> bool {
    let parts: Vec<&str> = opts.split('-').collect();
    if !parts.iter().any(|p| *p == "p" || *p == "c") {
        return false;
    }
    let num = |prefix: char| parts.iter().find_map(|p| p.strip_prefix(prefix)?.parse::<u32>().ok());
    parts.iter().any(|p| p.strip_prefix('s').is_some_and(|n| n.parse::<u32>().is_ok())) || matches!((num('w'), num('h')), (Some(w), Some(h)) if w == h)
}

/// The cover URL for a square slot. Google's box resize only fits the picture inside
/// the box, so a portrait sleeve comes back tall and is drawn squashed across the
/// square; asking for the crop hands back a square of the picture's own centre.
pub fn sized_square(url: &str, px: u32) -> String {
    if youtube_image(url) {
        return upgrade_thumbnail(url, px).unwrap_or_else(|| url.to_owned());
    }
    if google_image(url)
        && let Some(i) = url.rfind('=')
    {
        return format!("{}=w{px}-h{px}-c-l90-rj", &url[..i]);
    }
    url.to_owned()
}

#[cfg(test)]
mod sized_tests {
    use super::{sized, sized_square, thumbnail_fallback};

    #[test]
    fn square_crops_stay_cropped() {
        let base = "https://yt3.ggpht.com/abc";
        assert_eq!(sized(&format!("{base}=w120-h120-p-l90-rj"), 544), format!("{base}=w544-h544-p-l90-rj"));
        assert_eq!(sized(&format!("{base}=s88-c-k-c0x00ffffff-no-rj"), 544), format!("{base}=w544-h544-p-l90-rj"));
        // Album art doesn't ask, and a wide banner isn't squared.
        assert_eq!(sized("https://lh3.googleusercontent.com/x=w120-h120-l90-rj", 544), "https://lh3.googleusercontent.com/x=w544-h544-l90-rj");
        assert_eq!(sized("https://lh3.googleusercontent.com/x=w1440-h600-p-l90-rj", 544), "https://lh3.googleusercontent.com/x=w544-h544-l90-rj");
    }

    #[test]
    fn a_square_slot_asks_google_for_the_crop() {
        // Google's box resize only fits the picture inside the box, so a portrait
        // sleeve comes back tall and is drawn squashed across the square.
        let base = "https://lh3.googleusercontent.com/x";
        assert_eq!(sized_square(&format!("{base}=w544-h544-l90-rj"), 120), format!("{base}=w120-h120-c-l90-rj"));
        // Another CDN's picture is left as it is.
        let apple = "https://is1-ssl.mzstatic.com/image/thumb/x/1200x1200bb.jpg";
        assert_eq!(sized_square(apple, 120), apple);
    }

    #[test]
    fn youtube_thumbnails_are_asked_at_their_biggest_size() {
        let clip = "https://i.ytimg.com/vi/x8tfvLUkIAs/mqdefault.jpg";
        // A card wants the full-resolution frame; a row keeps the small copy it came with.
        assert_eq!(sized(clip, 480), "https://i.ytimg.com/vi/x8tfvLUkIAs/maxresdefault.jpg");
        assert_eq!(sized(clip, 300), "https://i.ytimg.com/vi/x8tfvLUkIAs/sddefault.jpg");
        assert_eq!(sized(clip, 120), clip);
        // An upgraded URL read back at row size comes down to the copy every video has.
        let big = "https://i.ytimg.com/vi/x8tfvLUkIAs/maxresdefault.jpg?sqp=-oaymwEmCNAFEPAFMAqqAQq4AhY&rs=AOQU";
        assert_eq!(sized(big, 120), "https://i.ytimg.com/vi/x8tfvLUkIAs/hqdefault.jpg");
        // The signed query goes with the rendition, and the bare path serves any of them.
        assert_eq!(sized(big, 480), big);
        assert_eq!(sized_square("https://img.youtube.com/vi/abc/hq720.jpg", 512), "https://img.youtube.com/vi/abc/maxresdefault.jpg");
        // A clip that has no such size is loaded as the one every video does.
        assert_eq!(thumbnail_fallback("https://i.ytimg.com/vi/abc/sddefault.jpg"), Some("https://i.ytimg.com/vi/abc/hqdefault.jpg".into()));
        assert_eq!(thumbnail_fallback(clip), None);
        assert_eq!(thumbnail_fallback("https://yt3.ggpht.com/abc=s100"), None);
    }
}

/// What a card or shelf item opens.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Target {
    Song(Track),
    Album(String),
    Playlist(String),
    Artist(String),
    /// Something on Apple's own pages: a clip, a song, an album, a playlist or a station.
    /// Apple's ids mean nothing to YouTube Music, so it carries what to look it up with
    /// and Apple's page for sharing.
    Apple { kind: AppleKind, id: String, url: String, title: String, artist: String },
}

/// What an Apple tile is, which decides how it's found on YouTube Music.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AppleKind {
    Album,
    Playlist,
    Song,
    Radio,
    Video,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Card {
    pub title: String,
    pub subtitle: String,
    pub thumbs: Vec<Thumb>,
    pub target: Target,
    /// Artists are shown as circles.
    pub round: bool,
    /// Artists named in the subtitle; clicking one opens their page.
    pub links: Vec<Link>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ShelfKind {
    /// Square/round cards in one horizontal row.
    Cards(Vec<Card>),
    /// Song rows laid out in columns of `rows` (Quick picks).
    Songs { tracks: Vec<Track>, rows: usize },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Shelf {
    pub title: String,
    /// Small line above the title ("GRAPE JUICE" on Listen again).
    pub strapline: Option<String>,
    pub strap_thumb: Option<String>,
    pub kind: ShelfKind,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Chip {
    pub title: String,
    /// Browse params selecting this mood; `None` clears the filter.
    pub params: Option<String>,
    pub selected: bool,
}

#[cfg(test)]
mod tests {
    use super::{Link, Track, says_ai};

    fn track(title: &str, artists: &[&str], album: Option<&str>) -> Track {
        Track {
            id: "x".into(),
            title: title.into(),
            artists: artists.iter().map(|a| Link { name: (*a).into(), id: None }).collect(),
            album: album.map(|a| Link { name: a.into(), id: None }),
            duration: None,
            thumbs: vec![],
            track_nr: None,
            plays: None,
            music_video: false,
            episode: false,
        }
    }

    #[test]
    fn an_ai_label_is_a_word_of_its_own() {
        // How uploads say it, as YouTube Music's search returns them.
        for t in [
            "Starkim1999 [AI Generated Song]",
            "Blue Eyes (AI Cover)",
            "AI-Generated Christmas Song",
            "Made with AI",
            "Summer Heat - Suno AI Remix",
            "AI Music Playlist 2026",
            "A.I. Generated Anthem",
            "Song made by AI",
            "Entirely generated with AI",
            "Nothing Else Matters (Udio Version)",
            "Lovesex — Suno",
        ] {
            assert!(says_ai(t), "{t}");
        }
        // The same letters inside a name or a place say nothing, and a bare "AI" on its own
        // is a person's name as often as a confession.
        for t in ["Miami", "Nai Covers", "Rain Again", "Daisy, Daisy", "Air Supply", "AI", "Main Idea"] {
            assert!(!says_ai(t), "{t}");
        }
    }

    #[test]
    fn a_song_is_ai_when_any_line_of_its_says_so() {
        assert!(!track("Get Lucky", &["Daft Punk"], Some("Random Access Memories")).ai_labelled());
        assert!(track("Get Lucky", &["Daft Punk", "AI Vocals"], None).ai_labelled());
        assert!(track("Get Lucky", &["Daft Punk"], Some("AI Covers Vol. 2")).ai_labelled());
        // A name that opens with the label admits it; an artist called AI doesn't.
        assert!(track("AI Cover of Nothing Else Matters", &["Someone"], None).ai_labelled());
        assert!(!track("Dreamin", &["AI", "Kanye West"], None).ai_labelled());
    }
}
