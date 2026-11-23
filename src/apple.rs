//! Apple Music's public web data: editorial descriptions, motion artwork and artist-page hero clips.
//!
//! Nothing here is authenticated on the user's behalf. The catalog API wants a
//! bearer token even for anonymous reads, and the one the web player mints for
//! visitors is scraped from its own JS bundle, kept in memory and never logged.
//! Searches are scored rather than trusted: a plausible-looking wrong album is
//! worse than none, so a hit has to match the artist and the title to be used.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::{Map, Value};

const AMP: &str = "https://amp-api.music.apple.com/v1/catalog";
const STOREFRONT: &str = "us";
const WEB_PLAYER: &str = "https://music.apple.com/us/browse";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0 Safari/537.36";

/// Motion clips for one release: square for sleeves, wide for hero banners.
#[derive(Clone, Debug, Default)]
pub struct Motion {
    pub square: Option<String>,
    pub wide: Option<String>,
}

/// Apple's own page for an artist: their display name, biography, wordmark and videos.
#[derive(Clone, Debug, Default)]
pub struct Artwork {
    pub name: String,
    /// Apple's biography, when the artist's page provides one.
    pub bio: Option<String>,
    /// The custom wordmark Apple lettered for the artist, when they have one.
    pub logo: Option<LogoSrc>,
    /// The clips on it, which the catalog API won't hand out to an anonymous token.
    pub videos: Vec<Video>,
    /// The page header's background clip (an HLS master), when Apple made one.
    pub hero_video: Option<String>,
    /// The header's own wide still, sized for a hero band.
    pub hero_still: Option<String>,
}

/// A music video on Apple's artist page. It has no YouTube counterpart until asked for.
#[derive(Clone, Debug)]
pub struct Video {
    pub title: String,
    /// The year Apple writes under the title.
    pub year: String,
    /// Apple's page for it, for sharing.
    pub url: String,
    /// Apple's own id, which is also what a shelf's tiles are keyed by.
    pub id: String,
    /// A poster, already sized for a 16:9 tile.
    pub poster: String,
}

/// A tile on one of Apple's browse pages: what it is, what it's called, and its sleeve.
#[derive(Clone, Debug)]
pub struct Tile {
    pub title: String,
    pub subtitle: String,
    /// Decides what plays when the tile is opened, and what it's searched for.
    pub kind: crate::model::AppleKind,
    /// Apple's own id, and Apple's page for sharing.
    pub id: String,
    pub url: String,
    /// Artwork, already sized for the shelf's shape.
    pub art: String,
    /// A category brick's background, packed as `0xRRGGBB`. Zero for every other tile.
    pub tint: u32,
}

/// A shelf as Apple's browse page writes it.
#[derive(Clone, Debug)]
pub struct Shelf {
    /// The hero run has no heading; the rest do.
    pub title: String,
    pub layout: Layout,
    pub tiles: Vec<Tile>,
}

/// How a shelf's tiles are shaped.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Layout {
    Square,
    Wide,
    /// Apple's search page: coloured bricks named for what they hold.
    Brick,
    /// A shelf of songs, laid out as rows in columns.
    Songs,
}

/// One song on an Apple collection page.
#[derive(Clone, Debug)]
pub struct AppleSong {
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    /// Seconds, as Apple counts them.
    pub duration: Option<u32>,
    /// The song's own sleeve, sized square.
    pub art: Option<String>,
    /// Apple's id for the song.
    pub id: String,
    /// Apple's page for it.
    pub url: String,
}

/// An Apple collection page: the header Apple writes and the songs under it.
#[derive(Clone, Debug)]
pub struct Collection {
    /// What it is: an album or a playlist.
    pub kind: crate::model::AppleKind,
    pub title: String,
    /// Who made it: a curator, an artist or Apple itself.
    pub author: String,
    /// The blurb Apple writes under the header.
    pub description: Option<String>,
    /// The sleeve, sized square.
    pub art: Option<String>,
    pub songs: Vec<AppleSong>,
}

/// The artist's wordmark as Apple's CDN template, `{w}x{h}{c}.{f}` and all.
#[derive(Clone, Debug)]
pub struct LogoSrc {
    pub template: String,
    pub w: u32,
    pub h: u32,
}

impl LogoSrc {
    /// The wordmark `height` px tall, keeping Apple's aspect, at the `bb` crop.
    pub fn at_height(&self, height: u32) -> String {
        let height = height.max(1);
        let w = (self.w as u64 * height as u64 / self.h.max(1) as u64).max(1);
        self.template
            .replace("{w}", &w.to_string())
            .replace("{h}", &height.to_string())
            .replace("{c}", "bb")
            .replace("{f}", "png")
    }
}

const TOKEN_TTL_MARGIN: i64 = 300;
const TOKEN_RETRY: Duration = Duration::from_secs(30 * 60);
const MIN_SCORE: i32 = 12;

struct Token {
    jwt: String,
    expires: i64,
}

static TOKEN: LazyLock<Mutex<Option<Token>>> = LazyLock::new(|| Mutex::new(None));
static REJECTED: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);
static RETRY_AFTER: LazyLock<Mutex<Option<Instant>>> = LazyLock::new(|| Mutex::new(None));

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

async fn get(http: &reqwest::Client, url: &str, bearer: Option<&str>) -> anyhow::Result<(u16, String)> {
    let mut r = http.get(url).timeout(Duration::from_secs(12)).header("User-Agent", UA);
    if let Some(b) = bearer {
        r = r.header("Authorization", format!("Bearer {b}")).header("Origin", "https://music.apple.com").header("Referer", "https://music.apple.com/");
    }
    let resp = r.send().await?;
    let code = resp.status().as_u16();
    Ok((code, resp.text().await?))
}

// --------------------------------------------------------------- token

/// The anonymous bearer the web player uses for catalog reads. It isn't
/// published anywhere and rotates, so it's read the way the browser gets it:
/// load the web player, find the JS bundle it pulls in, take the token out of it.
async fn token(http: &reqwest::Client) -> Option<String> {
    let now = now_secs();
    if let Some(t) = TOKEN.lock().ok().and_then(|t| t.as_ref().map(|t| (t.jwt.clone(), t.expires))) {
        if now < t.1 - TOKEN_TTL_MARGIN {
            return Some(t.0);
        }
    }
    if RETRY_AFTER.lock().ok().and_then(|r| *r).is_some_and(|at| Instant::now() < at) {
        return None;
    }

    let mut found = None;
    if let Ok((_, html)) = get(http, WEB_PLAYER, None).await {
        for path in script_paths(&html) {
            let Ok((_, js)) = get(http, &format!("https://music.apple.com{path}"), None).await else { continue };
            let rejected = REJECTED.lock().map(|r| r.clone()).unwrap_or_default();
            let mut best: Option<(String, i64, bool)> = None;
            for jwt in jwt_candidates(&js) {
                if rejected.contains(&jwt) {
                    continue;
                }
                let Some(exp) = jwt_expiry(&jwt) else { continue };
                if exp <= now {
                    continue;
                }
                let web = is_web_player_token(&jwt);
                if best.as_ref().is_none_or(|b| web && !b.2) {
                    best = Some((jwt, exp, web));
                }
            }
            if let Some((jwt, expires, _)) = best {
                found = Some(Token { jwt, expires });
                break;
            }
        }
    }
    match found {
        Some(t) => {
            log::debug!("apple: web player token good for {} min", (t.expires - now).max(0) / 60);
            let jwt = t.jwt.clone();
            if let Ok(mut slot) = TOKEN.lock() {
                *slot = Some(t);
            }
            Some(jwt)
        }
        None => {
            log::debug!("apple: no usable web player token");
            if let Ok(mut r) = RETRY_AFTER.lock() {
                *r = Some(Instant::now() + TOKEN_RETRY);
            }
            None
        }
    }
}

/// `/assets/index~<hash>.js` and its legacy twin, as the page names them.
fn script_paths(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(i) = rest.find("/assets/index") {
        let tail = &rest[i..];
        if let Some(end) = tail.find(".js") {
            let path = &tail[..end + 3];
            if path.chars().all(|c| !matches!(c, '"' | '\'' | '<' | '>' | ' ')) && !out.iter().any(|p| p == path) {
                out.push(path.to_owned());
            }
            rest = &tail[end..];
        } else {
            break;
        }
    }
    out
}

fn jwt_candidates(js: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = js.as_bytes();
    let mut i = 0;
    while i + 2 < bytes.len() {
        if &bytes[i..i + 2] == b"ey" {
            let mut j = i;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || matches!(bytes[j], b'-' | b'_' | b'.')) {
                j += 1;
            }
            let cand = &js[i..j];
            if cand.matches('.').count() == 2 && cand.len() > 40 && !out.iter().any(|c| c == cand) {
                out.push(cand.to_owned());
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

fn jwt_parts(jwt: &str) -> Option<(String, String)> {
    let part = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s).ok().and_then(|b| String::from_utf8(b).ok());
    let mut it = jwt.split('.');
    Some((part(it.next()?)?, part(it.next()?)?))
}

fn jwt_expiry(jwt: &str) -> Option<i64> {
    let (_, payload) = jwt_parts(jwt)?;
    let i = payload.find("\"exp\"")?;
    let rest = &payload[i + 5..];
    let start = rest.find(|c: char| c.is_ascii_digit())?;
    let digits: String = rest[start..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

fn is_web_player_token(jwt: &str) -> bool {
    jwt_parts(jwt).is_some_and(|(header, payload)| header.contains("WebPlayKid") || payload.contains("AMPWebPlay"))
}

fn reject(jwt: &str) {
    if let Ok(mut set) = REJECTED.lock() {
        set.insert(jwt.to_owned());
    }
    if let Ok(mut slot) = TOKEN.lock() {
        if slot.as_ref().is_some_and(|t| t.jwt == jwt) {
            *slot = None;
        }
    }
}

/// An authenticated catalog read. A 401 means the token picked out of the
/// bundle isn't the one this endpoint honours, so it is struck off and the
/// next lookup scrapes a different one.
async fn api(http: &reqwest::Client, path_and_query: &str) -> Option<Value> {
    for _ in 0..2 {
        let jwt = token(http).await?;
        let (code, body) = get(http, &format!("{AMP}/{STOREFRONT}/{path_and_query}"), Some(&jwt)).await.ok()?;
        if code == 200 {
            return serde_json::from_str(&body).ok();
        }
        if code == 401 {
            log::debug!("apple: catalog rejected the token; re-scraping");
            reject(&jwt);
            continue;
        }
        log::debug!("apple: catalog answered {code} for {path_and_query}");
        return None;
    }
    None
}

// ------------------------------------------------------------- matching

/// A name with its case and punctuation off, so Apple's and YouTube's spellings of the
/// same release can be compared.
pub(crate) fn norm(s: &str) -> String {
    s.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect()
}

fn artists_match(want: &str, got: &str) -> bool {
    let g = norm(got);
    let parts: Vec<String> = want.split(',').map(norm).filter(|p| p.len() > 1).collect();
    !parts.is_empty() && parts.iter().all(|p| g.contains(p.as_str()))
}

const EDITION_WORDS: [&str; 8] = ["deluxe", "expanded", "remastered", "remix", "version", "edit", "mix", "bonus"];
const COMPILATION_MARKERS: [&str; 8] = ["playlist", "set list", "essentials", "dj mix", "mixed", "apple music", "today's hits", "session"];

fn is_compilation(name: &str) -> bool {
    let lower = name.to_lowercase();
    COMPILATION_MARKERS.iter().any(|m| lower.contains(m))
}

/// How well a catalog hit lines up with what's asked for, or `None` to reject
/// it outright: the artist is a gate, the title and album decide the score.
fn score(title: &str, artist: &str, album: Option<&str>, hit: &Value, album_is_self: bool) -> Option<i32> {
    let a = hit.get("attributes")?;
    let name = a.get("name")?.as_str().unwrap_or_default();
    let hit_artist = a.get("artistName")?.as_str().unwrap_or_default();
    let hit_album = if album_is_self { name } else { a.get("albumName").and_then(Value::as_str).unwrap_or_default() };
    if is_compilation(name) || is_compilation(hit_album) {
        return None;
    }
    if !artists_match(artist, hit_artist) {
        return None;
    }
    let (nt, nh) = (norm(title), norm(name));
    if nt.is_empty() || nh.is_empty() {
        return None;
    }
    let mut s = 10;
    s += if nh == nt {
        15
    } else if nh.contains(&nt) || nt.contains(&nh) {
        7
    } else {
        -10
    };
    if let Some(al) = album.filter(|a| !a.is_empty())
        && !hit_album.is_empty()
    {
        let (na, nha) = (norm(al), norm(hit_album));
        s += if na == nha {
            20
        } else if !na.is_empty() && (nha.contains(&na) || na.contains(&nha)) {
            10
        } else {
            0
        };
    }
    for w in EDITION_WORDS {
        let in_want = title.to_lowercase().contains(w);
        let in_hit = name.to_lowercase().contains(w);
        if in_want && in_hit {
            s += 4;
        } else if in_hit {
            s -= 3;
        }
    }
    Some(s)
}

/// Every hit worth using, best score first.
///
/// A release can come back twice under different catalogue ids — the clean and explicit
/// copies of a single, say — and only one of them carries Apple's motion sleeve. Taking
/// the single top-scoring hit therefore loses the clip on the twin, so callers that want
/// a clip walk the whole list.
fn ranked<'a>(hits: &'a [Value], title: &str, artist: &str, album: Option<&str>, album_is_self: bool) -> Vec<(i32, &'a Value)> {
    let mut out: Vec<(i32, &Value)> = hits
        .iter()
        .filter_map(|h| score(title, artist, album, h, album_is_self).map(|s| (s, h)))
        .filter(|(s, _)| *s >= MIN_SCORE)
        .collect();
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out
}

/// The square clip Apple letters straight onto a search hit, if it has one.
fn clip(hit: &Value) -> Option<Motion> {
    hit.pointer("/attributes/editorialVideo").map(motion_urls).filter(|m| m.square.is_some())
}

fn motion_urls(video: &Value) -> Motion {
    let link = |key: &str| -> Option<String> {
        let asset = video.get(key)?;
        ["video", "videoUrl", "hlsUrl", "url"]
            .iter()
            .find_map(|k| asset.get(*k).and_then(Value::as_str))
            .filter(|u| !u.is_empty())
            .map(str::to_owned)
    };
    let square = link("motionDetailSquare").or_else(|| link("motionSquareVideo1x1")).or_else(|| link("motionDetailRaw"));
    let wide = link("motionDetailTall").or_else(|| link("motionTallVideo3x4"));
    Motion { square, wide }
}

fn album_id(hit: &Value) -> Option<String> {
    let from_rel = hit
        .get("relationships")?
        .get("albums")?
        .get("data")?
        .as_array()?
        .first()?
        .get("id")?
        .as_str()
        .map(str::to_owned);
    if let Some(id) = from_rel.filter(|id| !id.starts_with("pl.")) {
        return Some(id);
    }
    let url = hit.get("attributes")?.get("url")?.as_str()?;
    let tail = url.split("/album/").nth(1)?.split('?').next()?;
    tail.rsplit('/').next().filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())).map(str::to_owned)
}

// -------------------------------------------------------------- lookups

/// Motion artwork for a song: the sleeve clip of the album it's on.
pub async fn song_motion(http: &reqwest::Client, title: &str, artist: &str, album: Option<&str>) -> Option<Motion> {
    let term = match album.filter(|a| !a.is_empty() && !title.to_lowercase().contains(&a.to_lowercase())) {
        Some(a) => format!("{artist} {title} {a}"),
        None => format!("{artist} {title}"),
    };
    let q = format!(
        "search?term={}&types=songs&limit=10&extend=editorialVideo&include=albums",
        url_escape(&term)
    );
    let v = api(http, &q).await?;
    let hits = v.pointer("/results/songs/data")?.as_array()?;
    let cands = ranked(hits, title, artist, album, false);
    let Some((s, hit)) = cands.first() else {
        log::debug!("apple: song motion '{title}' by '{artist}': {} hits, none matched", hits.len());
        return None;
    };
    log::debug!("apple: song motion '{title}' matched '{}' (score {s})", hit.pointer("/attributes/name").and_then(Value::as_str).unwrap_or("?"));
    if let Some(m) = cands.iter().find_map(|(_, h)| clip(h)) {
        return Some(m);
    }
    // Search hits carry no album relationship: ask by the release's own name.
    let mut ids: Vec<String> = Vec::new();
    for (_, h) in &cands {
        if let Some(id) = album_id(h)
            && !ids.contains(&id)
            && ids.len() < 3
        {
            ids.push(id);
        }
    }
    let mut by_id = None;
    for id in &ids {
        by_id = album_by_id(http, id).await;
        if by_id.is_some() {
            break;
        }
    }
    let mut m = by_id;
    if m.is_none()
        && let Some(a) = album.filter(|a| !a.is_empty())
    {
        m = album_motion(http, a, artist).await;
    }
    log::debug!("apple: song motion '{title}': album fallback clip {}", if m.is_some() { "found" } else { "absent" });
    m
}

/// Motion artwork for a release, by its own name.
pub async fn album_motion(http: &reqwest::Client, album: &str, artist: &str) -> Option<Motion> {
    let term = if album.to_lowercase().contains(&artist.to_lowercase()) { album.to_owned() } else { format!("{artist} {album}") };
    let q = format!("search?term={}&types=albums&limit=10&extend=editorialVideo", url_escape(&term));
    let v = api(http, &q).await?;
    let hits = v.pointer("/results/albums/data")?.as_array()?;
    let cands = ranked(hits, album, artist, Some(album), true);
    let Some((s, hit)) = cands.first() else {
        log::debug!("apple: album motion '{album}' by '{artist}': {} hits, none matched", hits.len());
        return None;
    };
    let m = cands.iter().find_map(|(_, h)| clip(h));
    log::debug!("apple: album motion '{album}' matched '{}' (score {s}), clip {}", hit.pointer("/attributes/name").and_then(Value::as_str).unwrap_or("?"), if m.is_some() { "found" } else { "absent" });
    m
}

/// Apple's editorial description for a release, only when both names match.
/// A missing or ambiguous match is preferable to showing another album's notes.
pub async fn album_description(http: &reqwest::Client, album: &str, artist: &str) -> Option<String> {
    if norm(album).is_empty() || norm(artist).is_empty() {
        return None;
    }
    let term = format!("{artist} {album}");
    let q = format!("search?term={}&types=albums&limit=10", url_escape(&term));
    let search = api(http, &q).await?;
    let hits = search.pointer("/results/albums/data")?.as_array()?;
    // Apple's order is the tie-breaker among exact matches; unlike motion artwork,
    // a substring match or an edition with a different name isn't safe for prose.
    let hits: Vec<&Value> = hits.iter().filter(|hit| description_album_match(hit, album, artist)).collect();
    if let Some(notes) = hits.iter().find_map(|hit| album_editorial_notes(hit)) {
        return Some(notes);
    }

    let mut seen = HashSet::new();
    let mut pages = Vec::new();
    // Clean/explicit twins can have different notes, but don't make unbounded requests.
    for hit in hits {
        let Some(id) = hit.get("id").and_then(Value::as_str).filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())) else { continue };
        if !seen.insert(id.to_owned()) {
            continue;
        }
        if seen.len() > 3 {
            break;
        }
        let mut page = hit.pointer("/attributes/url").and_then(Value::as_str).and_then(|url| safe_album_page_url(url, id));
        if let Some(response) = api(http, &format!("albums/{id}")).await
            && let Some(record) = response.get("data").and_then(Value::as_array).and_then(|data| data.first())
        {
            // Don't trust an inconsistent detail response, even if search matched.
            if record.get("id").and_then(Value::as_str) != Some(id) || !description_album_match(record, album, artist) {
                continue;
            }
            if let Some(notes) = album_editorial_notes(record) {
                return Some(notes);
            }
            page = record.pointer("/attributes/url").and_then(Value::as_str).and_then(|url| safe_album_page_url(url, id)).or(page);
        }
        if let Some(url) = page {
            pages.push((id.to_owned(), url));
        }
    }

    for (id, url) in pages {
        // The supplied client can follow arbitrary redirects. The page client instead
        // respects the configured proxy and permits only redirects to this Apple album.
        let page_http = crate::proxy::apply(reqwest::Client::builder())
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= 5 {
                    attempt.error("too many Apple redirects")
                } else if safe_album_page_url(attempt.url().as_str(), &id).is_some() {
                    attempt.follow()
                } else {
                    attempt.error("unsafe Apple album redirect")
                }
            }))
            .build().ok()?;
        if let Ok(page) = collection(&page_http, url.as_str()).await
            && let Some(description) = collection_album_description(&page, album, artist)
        {
            return Some(description);
        }
    }
    None
}

fn album_names_match(album: &str, artist: &str, name: &str, author: &str) -> bool {
    let (album, artist) = (norm(album), norm(artist));
    !album.is_empty() && !artist.is_empty() && album == norm(name) && artist == norm(author)
}

fn description_album_match(hit: &Value, album: &str, artist: &str) -> bool {
    if hit.get("type").is_some_and(|kind| kind.as_str() != Some("albums")) {
        return false;
    }
    let name = hit.pointer("/attributes/name").and_then(Value::as_str).unwrap_or_default();
    let author = hit.pointer("/attributes/artistName").and_then(Value::as_str).unwrap_or_default();
    album_names_match(album, artist, name, author)
}

/// Only the actual editorial notes qualify, with the full text preferred to the summary.
fn album_editorial_notes(hit: &Value) -> Option<String> {
    ["/attributes/editorialNotes/standard", "/attributes/editorialNotes/short"]
        .iter().filter_map(|path| hit.pointer(path).and_then(Value::as_str)).find_map(editorial_text)
}

fn collection_album_description(page: &Collection, album: &str, artist: &str) -> Option<String> {
    if page.kind != crate::model::AppleKind::Album || !album_names_match(album, artist, &page.title, &page.author) {
        return None;
    }
    page.description.as_deref().map(str::trim).filter(|text| !text.is_empty()).map(str::to_owned)
}

/// Keep the catalog's page on Apple's HTTPS album endpoint and bind it to the hit's id.
fn safe_album_page_url(raw: &str, id: &str) -> Option<url::Url> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut url = url::Url::parse(raw).ok()?;
    if url.scheme() != "https" || url.host_str() != Some("music.apple.com") || url.port().is_some()
        || !url.username().is_empty() || url.password().is_some()
    {
        return None;
    }
    let segments: Vec<_> = url.path_segments()?.collect();
    if !(segments.len() == 3 || segments.len() == 4) || segments[0] != STOREFRONT || segments[1] != "album"
        || segments.last()?.strip_prefix("id").unwrap_or(segments.last()?) != id
        || (segments.len() == 4 && segments[2].is_empty())
    {
        return None;
    }
    // Track and referral parameters aren't part of the album page we verified.
    url.set_query(None);
    url.set_fragment(None);
    Some(url)
}

/// Decode entities and remove markup without losing paragraph breaks or literal '<'.
fn editorial_text(raw: &str) -> Option<String> {
    let doc = dom_query::Document::from(raw);
    let body = doc.select("body");
    let mut text = String::new();
    for root in body.nodes() {
        let mut stack = vec![(*root, false)];
        while let Some((node, closing)) = stack.pop() {
            if closing {
                text.push_str("\n\n");
            } else if node.is_text() {
                text.push_str(&node.text());
            } else {
                let name = node.node_name();
                let name = name.as_deref().unwrap_or_default();
                if matches!(name, "script" | "style" | "noscript") {
                    continue;
                }
                if name == "br" {
                    text.push('\n');
                } else {
                    if matches!(name, "p" | "div" | "section" | "blockquote" | "li") {
                        if !text.is_empty() {
                            text.push_str("\n\n");
                        }
                        stack.push((node, true));
                    }
                    stack.extend(node.children_it(true).map(|child| (child, false)));
                }
            }
        }
    }
    let mut lines = Vec::new();
    for line in text.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if !line.is_empty() || !lines.is_empty() && !lines.last().is_some_and(String::is_empty) {
            lines.push(line);
        }
    }
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// A release's motion sleeve, asked for by Apple's own record id.
///
/// A playlist's clip lives only on the playlist record: Apple's album index has no
/// editorial playlists in it, and a playlist carries a curator rather than an artist,
/// so no name search can find it.
pub async fn motion_by_id(http: &reqwest::Client, id: &str) -> Option<Motion> {
    let kind = if id.starts_with("pl.") || id.starts_with("p.") { "playlists" } else { "albums" };
    let v = api(http, &format!("{kind}/{id}?extend=editorialVideo")).await?;
    let record = v.get("data")?.as_array()?.first()?;
    record.pointer("/attributes/editorialVideo").map(motion_urls).filter(|m| m.square.is_some())
}

/// The record id at the end of one of Apple's page addresses: an album's number, a
/// playlist's `pl.xxxx`, or a library playlist's `p.xxxx`.
pub fn id_from_url(url: &str) -> Option<String> {
    let tail = url.split(['?', '#']).next()?.rsplit('/').next()?;
    let tail = tail.strip_prefix("id").unwrap_or(tail);
    (!tail.is_empty() && (tail.contains('.') || tail.chars().all(|c| c.is_ascii_digit()))).then(|| tail.to_owned())
}

async fn album_by_id(http: &reqwest::Client, id: &str) -> Option<Motion> {
    let v = api(http, &format!("albums/{id}?extend=editorialVideo")).await?;
    let album = v.get("data")?.as_array()?.first()?;
    let name = album.pointer("/attributes/name")?.as_str()?;
    if is_compilation(name) {
        return None;
    }
    album.pointer("/attributes/editorialVideo").map(motion_urls).filter(|m| m.square.is_some())
}

/// Apple's artist page: their display name, biography, wordmark and videos.
pub async fn artist(http: &reqwest::Client, name: &str) -> Option<Artwork> {
    let q = format!("search?term={}&types=artists&limit=10", url_escape(name));
    let v = api(http, &q).await?;
    let hits = v.pointer("/results/artists/data")?.as_array()?;
    let (id, page_name) = artist_hit(hits, name)?;

    let url = format!("https://music.apple.com/{STOREFRONT}/artist/-/{id}");
    let (code, html) = get(http, &url, None).await.ok()?;
    if code != 200 {
        return Some(Artwork { name: page_name, ..Default::default() });
    }
    let shown = page_title(&html).unwrap_or(page_name);
    let logo = artist_logo(&html);
    let page = server_data(&html);
    let bio = page.as_ref().and_then(artist_bio);
    let videos = page.as_ref().map(videos).unwrap_or_default();
    let (hero_video, hero_still) = page.as_ref().map(hero).unwrap_or((None, None));
    Some(Artwork { name: shown, bio, logo, videos, hero_video, hero_still })
}

/// The JSON Apple's server writes into the artist page. Its shelves are what the
/// editorial API refuses an anonymous token, so the page is read instead.
fn server_data(html: &str) -> Option<Value> {
    const OPEN: &str = "<script type=\"application/json\" id=\"serialized-server-data\">";
    let at = html.find(OPEN)? + OPEN.len();
    let rest = &html[at..];
    let end = rest.find("</script>")?;
    serde_json::from_str(rest[..end].trim()).ok()
}

/// Every music video on the page, in Apple's own order.
fn videos(page: &Value) -> Vec<Video> {
    let mut out = Vec::new();
    lockups(page, &mut out);
    out
}

/// The first item in the artist detail header, shared by the hero and biography.
fn artist_header(page: &Value) -> Option<&Value> {
    page.pointer("/data/0/data/sections")?.as_array()?.iter()
        .find(|s| s["itemKind"].as_str() == Some("artistDetailHeader"))?
        .pointer("/items/0")
}

/// Apple's biography, not text from another shelf or an empty header.
fn artist_bio(page: &Value) -> Option<String> {
    let bio = artist_header(page)?.get("bio")?.as_str()?;
    // Plain text can contain intentional paragraph breaks; only strip markup when present.
    let bio = if bio.contains('<') && bio.contains('>') { plain(bio) } else { bio.trim().to_owned() };
    (!bio.trim().is_empty()).then_some(bio)
}

/// The page header's background: the artist's own motion clip (widest first) and a
/// wide still for when motion is off or the clip can't play.
fn hero(page: &Value) -> (Option<String>, Option<String>) {
    let Some(item) = artist_header(page) else { return (None, None) };
    let link = |p: &str| item.pointer(p).and_then(Value::as_str).filter(|u| !u.is_empty()).map(str::to_owned);
    let video = link("/wideVideoArtwork/dictionary/motionArtistWide16x9/video")
        .or_else(|| link("/wideVideoArtwork/dictionary/motionArtistFullscreen16x9/video"))
        .or_else(|| link("/videoArtwork/dictionary/motionArtistSquare1x1/video"));
    let still = link("/wideArtwork/dictionary/url")
        .or_else(|| link("/artwork/dictionary/url"))
        .and_then(|t| fill(&t, 1600, 900));
    (video, still)
}

fn lockups(node: &Value, out: &mut Vec<Video>) {
    match node {
        Value::Object(fields) => {
            if fields.get("itemKind").and_then(Value::as_str) == Some("verticalVideoLockup") {
                lockup(fields, out);
            }
            fields.values().for_each(|child| lockups(child, out));
        }
        Value::Array(items) => items.iter().for_each(|child| lockups(child, out)),
        _ => {}
    }
}

fn lockup(fields: &Map<String, Value>, out: &mut Vec<Video>) {
    let Some(items) = fields.get("items").and_then(Value::as_array) else { return };
    for item in items {
        // The same shelf carries posts and interviews, which aren't videos.
        if item.pointer("/contentDescriptor/kind").and_then(Value::as_str) != Some("musicVideo") {
            continue;
        }
        let (Some(title), Some(art)) = (item.pointer("/titleLinks/0/title").and_then(Value::as_str), item.pointer("/artwork/dictionary/url").and_then(Value::as_str)) else {
            continue;
        };
        let Some(poster) = poster(art) else { continue };
        out.push(Video {
            title: title.to_owned(),
            year: item.pointer("/subtitleLinks/0/title").and_then(Value::as_str).unwrap_or("").to_owned(),
            url: item.pointer("/contentDescriptor/url").and_then(Value::as_str).unwrap_or("").to_owned(),
            id: item.pointer("/contentDescriptor/identifiers/storeAdamID").and_then(Value::as_str).unwrap_or("").to_owned(),
            poster,
        });
    }
}

/// Apple's artwork template as a 16:9 poster. A plain URL is taken as it comes.
fn poster(template: &str) -> Option<String> {
    fill(template, 800, 450)
}

/// Apple's artwork template at `w`x`h`. Its `{c}` is the crop: `sr` fills the box
/// and trims the overflow, so the picture arrives at the shape it was asked for and
/// is never squeezed into it. (`bb` only fits the picture inside the box, which
/// leaves a banner narrower than the tile and stretches it across.) A plain URL is
/// taken as it comes.
fn fill(template: &str, w: u32, h: u32) -> Option<String> {
    if !template.contains("{w}") {
        return template.starts_with("https://").then(|| template.to_owned());
    }
    Some(template.replace("{w}", &w.to_string()).replace("{h}", &h.to_string()).replace("{c}", "sr").replace("{f}", "webp"))
}

/// Apple's browse page, path and all (`gb/new`, `gb/search`), as its server writes it.
/// The artist page's blob again: the editorial API won't hand these shelves to an
/// anonymous token, and the storefront rides in the path so each page is Apple's own.
pub async fn shelves(http: &reqwest::Client, path: &str) -> anyhow::Result<Vec<Shelf>> {
    let (code, html) = get(http, &format!("https://music.apple.com/{path}"), None).await?;
    if code != 200 {
        anyhow::bail!("Apple's '{path}' page answered {code}");
    }
    let page = server_data(&html).ok_or_else(|| anyhow::anyhow!("Apple's '{path}' page carries no data"))?;
    let out: Vec<Shelf> = sections(&page).iter().map(shelf).filter(|s| !s.tiles.is_empty()).collect();
    if out.is_empty() {
        anyhow::bail!("Apple's '{path}' page has no shelves");
    }
    Ok(out)
}

fn sections(page: &Value) -> Vec<Value> {
    page.pointer("/data/0/data/sections").and_then(Value::as_array).map(|s| s.to_vec()).unwrap_or_default()
}

/// Apple's own collection page (an album or a playlist), as its server writes it.
/// The page is the source: its header and its songs are Apple's own, not a
/// YouTube Music list that happens to share the name.
pub async fn collection(http: &reqwest::Client, url: &str) -> anyhow::Result<Collection> {
    let (code, html) = get(http, url, None).await?;
    if code != 200 {
        anyhow::bail!("Apple's page answered {code}");
    }
    let page = server_data(&html).ok_or_else(|| anyhow::anyhow!("Apple's page carries no data"))?;
    let secs = sections(&page);
    let header = secs.iter().find(|s| s["itemKind"].as_str() == Some("containerDetailHeaderLockup")).and_then(|s| s.pointer("/items/0"));
    let title = header.and_then(|i| string_at(i, "/title")).unwrap_or_default();
    let author = header.and_then(|i| string_at(i, "/subtitleLinks/0/title")).unwrap_or_default();
    let description = header.and_then(|i| string_at(i, "/modalPresentationDescriptor/paragraphText")).and_then(|d| {
        if url.contains("/album/") { editorial_text(&d) } else { Some(plain(&d)) }
    });
    let art = header.and_then(|i| string_at(i, "/artwork/dictionary/url")).and_then(|t| fill(&t, 800, 800));
    let mut songs = Vec::new();
    for section in secs.iter().filter(|s| s["itemKind"].as_str() == Some("trackLockup")) {
        for item in section["items"].as_array().into_iter().flatten() {
            let Some(title) = string_at(item, "/title") else { continue };
            let artist = string_at(item, "/subtitleLinks/0/title").or_else(|| string_at(item, "/artistName")).unwrap_or_default();
            songs.push(AppleSong {
                title,
                artist,
                album: string_at(item, "/tertiaryLinks/0/title"),
                duration: item["duration"].as_u64().map(|d| (d / 1000) as u32),
                art: string_at(item, "/artwork/dictionary/url").and_then(|t| fill(&t, 200, 200)),
                id: string_at(item, "/contentDescriptor/identifiers/storeAdamID").unwrap_or_default(),
                url: string_at(item, "/contentDescriptor/url").unwrap_or_default(),
            });
        }
    }
    if title.is_empty() && songs.is_empty() {
        anyhow::bail!("Apple's page carries no collection");
    }
    Ok(Collection {
        kind: if url.contains("/album/") { crate::model::AppleKind::Album } else { crate::model::AppleKind::Playlist },
        title,
        author,
        description,
        art,
        songs,
    })
}

/// Apple's own text without the markup it puts names in.
fn plain(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One shelf: its heading, the shape its tiles have, and its tiles. The lockup kinds
/// Apple's browse pages use are read; link lists and headers are skipped.
fn shelf(section: &Value) -> Shelf {
    let kind = section["itemKind"].as_str().unwrap_or_default();
    let layout = match kind {
        "squareLockup" => Layout::Square,
        "trackLockup" => Layout::Songs,
        "flowcaseLockup" | "horizontalLockup" | "verticalVideoLockup" => Layout::Wide,
        "brickLockup" => Layout::Brick,
        _ => return Shelf { title: String::new(), layout: Layout::Square, tiles: Vec::new() },
    };
    // A video shelf of station promos repeats one station and calls it "Listen Now";
    // only real clips earn their place here.
    let clips_only = kind == "verticalVideoLockup";
    let mut tiles = Vec::new();
    let mut seen = HashSet::new();
    for item in section["items"].as_array().into_iter().flatten() {
        // A hero writes its title itself; a lockup keeps it behind the first link.
        let Some(title) = string_at(item, "/title").or_else(|| string_at(item, "/titleLinks/0/title")) else { continue };
        // A category brick is a name on a colour. It opens no release: it's a search.
        if layout == Layout::Brick {
            let id = string_at(item, "/contentDescriptor/identifiers/storeAdamID").unwrap_or_default();
            // A category strip is 4:1 by its own measure (4320x1080), so it is baked wide.
            let art = string_at(item, "/artwork/dictionary/url").and_then(|t| fill(&t, 1000, 250)).unwrap_or_default();
            if !seen.insert(if id.is_empty() { title.clone() } else { id.clone() }) {
                continue;
            }
            tiles.push(Tile { title, subtitle: String::new(), kind: crate::model::AppleKind::Playlist, id, url: string_at(item, "/contentDescriptor/url").unwrap_or_default(), art, tint: brick_tint(item) });
            continue;
        }
        let Some(kind) = string_at(item, "/contentDescriptor/kind").and_then(|k| apple_kind(&k)) else { continue };
        if clips_only && kind != crate::model::AppleKind::Video {
            continue;
        }
        let (w, h) = match layout {
            Layout::Wide => (800, 450),
            // A song row carries a small sleeve beside the name, not a cover tile.
            Layout::Songs => (200, 200),
            _ => (600, 600),
        };
        let (Some(id), Some(art)) = (string_at(item, "/contentDescriptor/identifiers/storeAdamID"), string_at(item, "/artwork/dictionary/url").and_then(|t| fill(&t, w, h))) else { continue };
        if !seen.insert(id.clone()) {
            continue;
        }
        tiles.push(Tile { title, subtitle: subtitle(item), kind, id, url: string_at(item, "/contentDescriptor/url").unwrap_or_default(), art, tint: 0 });
    }
    Shelf { title: string_at(section, "/header/item/titleLink/title").unwrap_or_default(), layout, tiles }
}

/// A brick's background, which Apple writes as six bare hex digits.
fn brick_tint(item: &Value) -> u32 {
    string_at(item, "/artwork/dictionary/bgColor").and_then(|h| u32::from_str_radix(&h, 16).ok()).unwrap_or(0)
}

/// The line under a tile's title: the artists of a lockup, or what Apple writes itself.
fn subtitle(item: &Value) -> String {
    match item["subtitleLinks"].as_array() {
        Some(links) => links.iter().filter_map(|l| l["title"].as_str()).collect::<Vec<_>>().join(", "),
        None => item["subtitle"].as_str().unwrap_or_default().to_owned(),
    }
}

fn string_at(v: &Value, path: &str) -> Option<String> {
    v.pointer(path).and_then(Value::as_str).map(str::to_owned)
}

/// What an Apple tile is here, or nothing when it isn't something this app can play.
fn apple_kind(name: &str) -> Option<crate::model::AppleKind> {
    use crate::model::AppleKind;
    Some(match name {
        "album" => AppleKind::Album,
        "playlist" => AppleKind::Playlist,
        "song" => AppleKind::Song,
        "radioStation" => AppleKind::Radio,
        "musicVideo" => AppleKind::Video,
        _ => return None,
    })
}

/// The search hit that is this artist: an exact name match first, then one whose
/// name contains the other's. Apple ranks its hits, so on a tie the earliest is the
/// artist people mean — it also lists impostor profiles under famous names, and some
/// of those have no wordmark at all.
fn artist_hit(hits: &[serde_json::Value], name: &str) -> Option<(String, String)> {
    let want = norm(name);
    hits.iter()
        .enumerate()
        .filter_map(|(i, h)| {
            let n = h.pointer("/attributes/name")?.as_str()?;
            let got = norm(n);
            let s = if got == want {
                2
            } else if got.contains(&want) || want.contains(&got) {
                1
            } else {
                0
            };
            let id = h.get("id")?.as_str()?;
            (s > 0).then(|| (s, std::cmp::Reverse(i), id.to_owned(), n.to_owned()))
        })
        .max_by_key(|(s, first, _, _)| (*s, *first))
        .map(|(_, _, id, n)| (id, n))
}

/// The `"artistLogo"` block's CDN template and its natural size.
fn artist_logo(html: &str) -> Option<LogoSrc> {
    // Anchored on the open brace: `"artistLogo":null` must not fall through to some
    // neighbouring artwork's URL and be drawn as the artist's name.
    let at = html.find("\"artistLogo\":{")?;
    // The block is a flat dictionary; a generous window keeps us inside it.
    let block = &html[at..(at + 1400).min(html.len())];
    let template = url_after_key(block, "{", "url")?.replace("\\/", "/");
    let (w, h) = (number_after(block, "\"width\"")?, number_after(block, "\"height\"")?);
    // Only a real template can be resized; a plain URL is taken as-is.
    (w > 0 && h > 0 && template.contains("{w}")).then(|| LogoSrc { template, w, h })
}

/// The number after `"key":` in `s`.
fn number_after(s: &str, key: &str) -> Option<u32> {
    let i = s.find(key)? + key.len();
    let tail = s[i..].trim_start_matches(|c: char| c == ':' || c.is_whitespace());
    let end = tail.find(|c: char| !c.is_ascii_digit()).unwrap_or(tail.len());
    tail[..end].parse().ok()
}

fn page_title(html: &str) -> Option<String> {
    let i = html.find("<title>")? + 7;
    let rest = &html[i..];
    let end = rest.find("</title>")?;
    let t = rest[..end].trim().trim_start_matches('\u{200e}');
    let t = t.strip_suffix(" - Apple Music").unwrap_or(t);
    Some(t.trim().to_owned()).filter(|t| !t.is_empty())
}

/// The quoted string after `"key":"…"` within the JSON blob that follows
/// `anchor` in the page.
fn url_after_key<'a>(html: &'a str, anchor: &str, key: &str) -> Option<&'a str> {
    let at = html.find(anchor)?;
    let rest = &html[at..];
    let pat = format!("\"{key}\":\"");
    let i = rest.find(&pat)? + pat.len();
    let tail = &rest[i..];
    let end = tail.find('"')?;
    Some(&tail[..end])
}

fn url_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(b as char),
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ------------------------------------------------------------ HLS picking

/// The variant to decode for a sleeve shown at `max_width`: the smallest one at
/// least that wide (the decoder scales it down), or the largest when every
/// variant is narrower. A 200 px tile never pulls a 1080p clip.
pub async fn variant(http: &reqwest::Client, master: &str, max_width: u32) -> Option<String> {
    let (code, text) = get(http, master, None).await.ok()?;
    if code != 200 {
        return None;
    }
    let mut over: Option<(u32, String)> = None;
    let mut under: Option<(u32, String)> = None;
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if !line.starts_with("#EXT-X-STREAM-INF:") || line.contains("hvc1") {
            continue;
        }
        let Some(res) = line.split("RESOLUTION=").nth(1) else { continue };
        let w = res.split('x').next()?.trim().parse::<u32>().ok()?;
        let uri = lines.find(|l| !l.trim().is_empty() && !l.starts_with('#'))?;
        if w >= max_width {
            if over.as_ref().is_none_or(|b| w < b.0) {
                over = Some((w, uri.trim().to_owned()));
            }
        } else if under.as_ref().is_none_or(|b| w > b.0) {
            under = Some((w, uri.trim().to_owned()));
        }
    }
    let uri = over.or(under).map(|b| b.1)?;
    Some(resolve(master, &uri))
}

/// One piece of a clip: where it lives, and which bytes of it to take.
pub struct Part {
    pub url: String,
    /// The byte range to take, when the playlist names one.
    pub range: Option<(u64, u64)>,
}

/// The init segment and every media segment of a clip's playlist, in order. The clip
/// only plays once the header and the pictures sit in one file: the header alone has no
/// frames in it. A playlist of transport-stream segments carries its own header in each
/// piece and names no init segment at all.
pub async fn media_parts(http: &reqwest::Client, media: &str) -> Option<(Option<Part>, Vec<Part>)> {
    let (code, text) = get(http, media, None).await.ok()?;
    if code != 200 {
        return None;
    }
    let mut init = None;
    let mut segments = Vec::new();
    let mut range = None;
    // A range without an offset continues where the last one ended.
    let mut end = 0u64;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-MAP:") {
            let uri = rest.split("URI=\"").nth(1)?.split('"').next()?;
            init = Some(Part { url: resolve(media, uri.trim()), range: None });
        } else if let Some(rest) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            let s = rest.trim();
            let (len, start) = match s.split_once('@') {
                Some((l, o)) => (l.parse::<u64>().ok()?, o.parse::<u64>().ok()?),
                None => (s.parse::<u64>().ok()?, end),
            };
            range = Some((start, start + len.saturating_sub(1)));
            end = start + len;
        } else if !line.starts_with('#') {
            segments.push(Part { url: resolve(media, line), range });
            range = None;
        }
    }
    (!segments.is_empty()).then_some((init, segments))
}

fn resolve(base: &str, uri: &str) -> String {
    if uri.starts_with("http") {
        return uri.to_owned();
    }
    match base.rfind('/') {
        Some(i) => format!("{}/{}", &base[..i], uri),
        None => uri.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apple_page_addresses_end_with_their_record_id() {
        // The collection page asks for its motion sleeve by this id, so a playlist
        // opened from Apple's own address finds its clip.
        assert_eq!(id_from_url("https://music.apple.com/us/album/randm/1440561081").as_deref(), Some("1440561081"));
        assert_eq!(id_from_url("https://music.apple.com/us/album/id1440561081?i=1440561433").as_deref(), Some("1440561081"));
        assert_eq!(
            id_from_url("https://music.apple.com/us/playlist/new-in-pop/pl.16ef2c958d6e4b20ac75d783cb787053").as_deref(),
            Some("pl.16ef2c958d6e4b20ac75d783cb787053")
        );
        assert_eq!(id_from_url("https://music.apple.com/library/playlist/p.9BUP8rv39E").as_deref(), Some("p.9BUP8rv39E"));
        // A name is not an id: a slug is no use for a by-record lookup, and neither is
        // a trailing slash.
        assert_eq!(id_from_url("https://music.apple.com/us/search"), None);
        assert_eq!(id_from_url("https://music.apple.com/us/album/random-access-memories/"), None);
    }

    #[test]
    fn the_first_of_two_profiles_with_one_name_is_the_artist() {
        // Apple lists a second "Charli xcx" (an impostor profile) after the real one,
        // and its page has no wordmark: picking the last equal hit loses the lettering.
        let hit = |id: &str, name: &str| serde_json::json!({"id": id, "attributes": {"name": name}});
        let hits = vec![hit("432942256", "Charli xcx"), hit("1845539402", "Charli xcx"), hit("9", "Charli XCX Tribute")];
        assert_eq!(artist_hit(&hits, "Charli XCX").map(|(id, _)| id).as_deref(), Some("432942256"));
        // An exact name still beats one that merely contains it.
        let hits = vec![hit("1", "Taylor Swift Tribute"), hit("2", "Taylor Swift")];
        assert_eq!(artist_hit(&hits, "Taylor Swift").map(|(id, _)| id).as_deref(), Some("2"));
        assert!(artist_hit(&[hit("3", "Someone Else")], "Charli XCX").is_none());
    }

    #[test]
    fn artist_bio_comes_from_the_same_header_as_the_hero() {
        let page = serde_json::json!({"data": [{"data": {"sections": [
            {"itemKind": "other", "items": [{"bio": "Not the artist"}]},
            {"itemKind": "artistDetailHeader", "items": [
                {"bio": "  First paragraph.\n\nSecond paragraph.  ",
                 "wideVideoArtwork": {"dictionary": {"motionArtistWide16x9": {"video": "https://example.com/hero.m3u8"}}}},
                {"bio": "Not the header item"}
            ]}
        ]}}]});
        let html = format!("<script type=\"application/json\" id=\"serialized-server-data\">{page}</script>");
        let parsed = server_data(&html).unwrap();
        assert_eq!(artist_bio(&parsed).as_deref(), Some("First paragraph.\n\nSecond paragraph."));
        assert_eq!(hero(&parsed).0.as_deref(), Some("https://example.com/hero.m3u8"));
    }

    #[test]
    fn artist_bio_strips_markup_and_ignores_missing_or_empty_text() {
        let page = |bio: Value| serde_json::json!({"data": [{"data": {"sections": [
            {"itemKind": "artistDetailHeader", "items": [{"bio": bio}]}
        ]}}]});
        assert_eq!(artist_bio(&page(serde_json::json!(" <p>Made <b>music</b> for all.</p> "))).as_deref(), Some("Made music for all."));
        assert_eq!(artist_bio(&page(serde_json::json!("Fans <3 music"))).as_deref(), Some("Fans <3 music"));
        for bio in [Value::Null, serde_json::json!(""), serde_json::json!(" \n\t "), serde_json::json!("<p>  </p>"), serde_json::json!(42), serde_json::json!({})] {
            assert_eq!(artist_bio(&page(bio)), None);
        }
        let no_bio = serde_json::json!({"data": [{"data": {"sections": [
            {"itemKind": "artistDetailHeader", "items": [{}, {"bio": "Other item"}]},
            {"itemKind": "other", "items": [{"bio": "Other shelf"}]}
        ]}}]});
        assert_eq!(artist_bio(&no_bio), None);
        assert_eq!(artist_bio(&serde_json::json!({"data": []})), None);
        assert_eq!(artist_bio(&serde_json::json!({"data": [{"data": {"sections": [
            {"itemKind": "other", "items": [{"bio": "Other shelf"}]}
        ]}}]})), None);
        assert_eq!(artist_bio(&serde_json::json!({"data": [{"data": {"sections": [
            {"itemKind": "artistDetailHeader", "items": []}
        ]}}]})), None);
    }

    #[test]
    fn the_page_blob_holds_the_videos_the_api_wont() {
        let art = "https://is1-ssl.mzstatic.com/image/thumb/Video221/v4/d5/36/24/x.jpg/{w}x{h}{c}.{f}";
        let clip = |id: &str, title: &str, year: &str, kind: &str| {
            serde_json::json!({
                "id": id,
                "titleLinks": [{"title": title}],
                "subtitleLinks": [{"title": year}],
                "contentDescriptor": {"kind": kind, "url": format!("https://music.apple.com/us/music-video/{id}")},
                "artwork": {"dictionary": {"url": art}},
            })
        };
        let page = serde_json::json!({"data": [{"data": {"sections": [
            {"itemKind": "verticalVideoLockup", "header": {"item": {"titleLink": {"title": "Music Videos"}}},
             "items": [clip("1", "Camera", "2026", "musicVideo"), clip("2", "A chat", "", "artistUploadedVideo")]},
            {"itemKind": "albumLockup", "items": [clip("9", "An album", "", "album")]},
            {"itemKind": "verticalVideoLockup", "items": [clip("3", "Party Girl", "2024", "musicVideo")]},
        ]}}]});
        let got = videos(&page);
        // Apple's interviews share the shelf and aren't videos; nothing else is read.
        assert_eq!(got.iter().map(|v| v.title.as_str()).collect::<Vec<_>>(), ["Camera", "Party Girl"]);
        assert_eq!(got[0].year, "2026");
        assert_eq!(got[0].poster, art.replace("{w}", "800").replace("{h}", "450").replace("{c}", "sr").replace("{f}", "webp"));
    }

    #[test]
    fn the_browse_page_shelves_keep_what_plays_and_drop_the_rest() {
        use crate::model::AppleKind;
        let art = "https://is1-ssl.mzstatic.com/image/thumb/Blah/v4/00/11/22/x.jpg/{w}x{h}{c}.{f}";
        let tile = |id: &str, title: &str, kind: &str, subs: &[&str]| {
            serde_json::json!({
                "titleLinks": [{"title": title}],
                "subtitleLinks": subs.iter().map(|s| serde_json::json!({"title": s})).collect::<Vec<_>>(),
                "contentDescriptor": {"kind": kind, "url": format!("https://music.apple.com/us/{kind}/{id}"),
                                      "identifiers": {"storeAdamID": id}},
                "artwork": {"dictionary": {"url": art}},
            })
        };
        let page = serde_json::json!({"data": [{"data": {"sections": [
            // The hero run is wide and writes no heading of its own.
            {"itemKind": "flowcaseLockup", "items": [tile("1", "Werk", "playlist", &[])]},
            {"itemKind": "squareLockup",
             "header": {"item": {"titleLink": {"title": "New Albums"}}},
             // A repeated id, an episode and a heading-only section earn no tile.
             "items": [tile("2", "Damen", "album", &["Geist"]), tile("3", "Antenna", "album", &["Ms. Mun", "Feadz"]),
                       tile("3", "Dupe", "album", &[]), tile("4", "An interview", "episode", &[]),
                       tile("5", "Newsroom", "radioStation", &[])]},
            {"itemKind": "linkList", "items": [tile("6", "See all", "playlist", &[])]},
        ]}}]});
        let shelves: Vec<Shelf> = sections(&page).iter().map(shelf).filter(|s| !s.tiles.is_empty()).collect();
        assert_eq!(shelves.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(), ["", "New Albums"]);
        assert_eq!(shelves[0].layout, Layout::Wide);
        assert_eq!(shelves[0].tiles[0].art, art.replace("{w}", "800").replace("{h}", "450").replace("{c}", "sr").replace("{f}", "webp"));
        assert_eq!(shelves[1].tiles[0].art, art.replace("{w}", "600").replace("{h}", "600").replace("{c}", "sr").replace("{f}", "webp"));
        assert_eq!(shelves[1].tiles.iter().map(|t| (t.title.as_str(), t.id.as_str(), t.kind)).collect::<Vec<_>>(),
                   [("Damen", "2", AppleKind::Album), ("Antenna", "3", AppleKind::Album), ("Newsroom", "5", AppleKind::Radio)]);
        assert_eq!(shelves[1].tiles[1].subtitle, "Ms. Mun, Feadz");
    }

    /// Live: what the app itself gets back for real artists.
    #[tokio::test]
    #[ignore]
    async fn artist_live() {
        let http = reqwest::Client::builder().build().unwrap();
        for name in ["Charli XCX", "Taylor Swift", "Lady Gaga", "Kendrick Lamar"] {
            match artist(&http, name).await {
                Some(a) => println!("{name}: name={:?} logo={:?} videos={:?}", a.name, a.logo.as_ref().map(|l| (l.w, l.h, l.at_height(144))), a.videos.iter().map(|v| (&v.title, &v.year)).collect::<Vec<_>>()),
                None => println!("{name}: no answer at all"),
            }
        }
    }

    /// Live: the shelves Apple's own browse pages write, and what each one holds.
    #[tokio::test]
    #[ignore]
    async fn feed_live() {
        let http = reqwest::Client::builder().build().unwrap();
        for path in ["gb/new", "gb/radio", "gb/search", "gb/home"] {
            match shelves(&http, path).await {
                Ok(got) => {
                    println!("{path}: {} shelves", got.len());
                    for s in got.iter().filter(|s| !s.tiles.is_empty()) {
                        println!("  {:?} {:>2}  {:?}{}", s.layout, s.tiles.len(), if s.title.is_empty() { "(none)" } else { s.title.as_str() }, s.tiles.first().map(|t| format!("  e.g. {} — {} [{:?}]", t.title, t.subtitle, t.kind)).unwrap_or_default());
                    }
                }
                Err(e) => println!("{path}: {e:#}"),
            }
        }
    }

    #[test]
    fn scripts_and_tokens_are_found_without_a_regex() {
        let html = r#"<script src="/assets/index~abc123.js"></script><script src="/assets/index-legacy~de.js"></script>"#;
        assert_eq!(script_paths(html), vec!["/assets/index~abc123.js", "/assets/index-legacy~de.js"]);
        // Two base64url parts around a payload that names the web player.
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"iss":"AMPWebPlay","exp":1900000000}"#);
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"kid":"WebPlayKid"}"#);
        let sig = "A".repeat(43);
        let js = format!("var t=\"{header}.{payload}.{sig}\";var u=\"ey.short\";");
        let found = jwt_candidates(&js);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(is_web_player_token(&found[0]));
        assert_eq!(jwt_expiry(&found[0]), Some(1_900_000_000));
    }

    #[test]
    fn scoring_rejects_the_wrong_artist_and_ranks_the_right_release() {
        let hit = |name: &str, artist: &str, album: &str| {
            serde_json::json!({"attributes": {"name": name, "artistName": artist, "albumName": album}})
        };
        assert!(score("Birds of a Feather", "Billie Eilish", Some("HIT ME HARD AND SOFT"), &hit("Birds of a Feather", "Billie Eilish", "HIT ME HARD AND SOFT"), false).is_some_and(|s| s >= MIN_SCORE));
        assert!(ranked(&[hit("Birds of a Feather", "Someone Else", "X")], "Birds of a Feather", "Billie Eilish", None, false).is_empty());
        // Same artist, another song: kept out.
        assert!(ranked(&[hit("Lunch", "Billie Eilish", "HIT ME HARD AND SOFT")], "Birds of a Feather", "Billie Eilish", None, false).is_empty());
        // A playlist dressed up as an album is never it.
        assert!(ranked(&[hit("Apple Music Essentials", "Billie Eilish", "")], "HIT ME HARD AND SOFT", "Billie Eilish", None, true).is_empty());
    }

    #[test]
    fn descriptions_require_both_exact_album_and_artist_names() {
        let hit = |name: &str, artist: &str| serde_json::json!({
            "type": "albums", "attributes": {"name": name, "artistName": artist}
        });
        assert!(description_album_match(&hit("Random Access Memories", "Daft Punk"), "random access memories", "DAFT PUNK"));
        assert!(description_album_match(&hit("A/B", "A & B"), "A-B", "A, B"));
        for (name, artist) in [
            ("Random Access Memories (Deluxe Edition)", "Daft Punk"),
            ("Random Access", "Daft Punk"),
            ("Random Access Memories", "Daft Punk Tribute"),
            ("Random Access Memories", "Someone Else"),
            ("", "Daft Punk"),
        ] {
            assert!(!description_album_match(&hit(name, artist), "Random Access Memories", "Daft Punk"));
        }
        assert!(!description_album_match(&hit("X", "Artist"), "", "Artist"));
        assert!(!description_album_match(&hit("X", "Artist"), "X", ""));
        assert!(!description_album_match(&serde_json::json!({"type": "songs", "attributes": {"name": "X", "artistName": "Artist"}}), "X", "Artist"));
        assert!(!description_album_match(&serde_json::json!({"attributes": {"name": "X"}}), "X", "Artist"));
    }

    #[test]
    fn album_notes_are_decoded_and_prefer_full_editorial_text() {
        let hit = serde_json::json!({"attributes": {"editorialNotes": {
            "standard": "<p>First &amp; <i>best</i>.</p><p>Next<br>line &#39;here&#39;.</p>",
            "short": "A short note"
        }, "description": "Not editorial notes"}});
        assert_eq!(album_editorial_notes(&hit).as_deref(), Some("First & best.\n\nNext\nline 'here'."));
        let short = serde_json::json!({"attributes": {"editorialNotes": {"standard": "<p> </p>", "short": " Short &amp; sweet "}}});
        assert_eq!(album_editorial_notes(&short).as_deref(), Some("Short & sweet"));
        assert_eq!(editorial_text(" Fans <3 music\n\nAnother paragraph. ").as_deref(), Some("Fans <3 music\n\nAnother paragraph."));
        assert_eq!(editorial_text("<script>not prose</script><style>nor this</style>"), None);
        assert_eq!(album_editorial_notes(&serde_json::json!({"attributes": {"description": "Not notes", "editorialNotes": {"standard": 42}}})), None);
    }

    #[test]
    fn album_page_fallback_requires_a_safe_matching_apple_album() {
        use crate::model::AppleKind;
        let valid = "https://music.apple.com/us/album/random-access-memories/617154241?i=1#section";
        assert_eq!(safe_album_page_url(valid, "617154241").map(|u| u.to_string()).as_deref(),
                   Some("https://music.apple.com/us/album/random-access-memories/617154241"));
        assert!(safe_album_page_url("https://music.apple.com/us/album/id617154241", "617154241").is_some());
        for bad in [
            "http://music.apple.com/us/album/name/617154241",
            "https://music.apple.com.evil.test/us/album/name/617154241",
            "https://music.apple.com@evil.test/us/album/name/617154241",
            "https://user@music.apple.com/us/album/name/617154241",
            "https://music.apple.com:444/us/album/name/617154241",
            "https://music.apple.com/us/playlist/name/617154241",
            "https://music.apple.com/gb/album/name/617154241",
            "https://music.apple.com/us/album/name/999",
            "https://music.apple.com/us/album/name/617154241/extra",
        ] {
            assert!(safe_album_page_url(bad, "617154241").is_none(), "{bad}");
        }
        assert!(safe_album_page_url(valid, "../617154241").is_none());
        let mut page = Collection {
            kind: AppleKind::Album, title: "Random Access Memories".into(), author: "Daft Punk".into(),
            description: Some(" First paragraph.\n\nSecond paragraph. ".into()), art: None, songs: vec![],
        };
        assert_eq!(collection_album_description(&page, "Random Access Memories", "Daft Punk").as_deref(), Some("First paragraph.\n\nSecond paragraph."));
        page.author = "Daft Punk Tribute".into();
        assert!(collection_album_description(&page, "Random Access Memories", "Daft Punk").is_none());
        page.author = "Daft Punk".into();
        page.title = "Random Access Memories (Deluxe)".into();
        assert!(collection_album_description(&page, "Random Access Memories", "Daft Punk").is_none());
        page.title = "Random Access Memories".into();
        page.kind = AppleKind::Playlist;
        assert!(collection_album_description(&page, "Random Access Memories", "Daft Punk").is_none());
        page.kind = AppleKind::Album;
        page.description = Some("   ".into());
        assert!(collection_album_description(&page, "Random Access Memories", "Daft Punk").is_none());
    }

    #[test]
    fn the_artist_page_title_names_the_artist() {
        let page = "<title>\u{200e}Charli xcx - Apple Music</title>";
        assert_eq!(page_title(page).as_deref(), Some("Charli xcx"));
    }

    #[test]
    fn the_artist_logo_template_is_sized_in_proportion() {
        let html = r#"{"artistLogo":{"dictionary":{"bgColor":"ae211a","height":655,"textColor1":"faf3f3","url":"https://is1-ssl.mzstatic.com/image/thumb/Features/v4/ae/d9/28/aed9.png/{w}x{h}{c}.{f}","width":2880},"cropStyle":"bb"}}"#;
        let logo = artist_logo(html).unwrap();
        assert_eq!((logo.w, logo.h), (2880, 655));
        let url = logo.at_height(1310);
        assert_eq!(url, "https://is1-ssl.mzstatic.com/image/thumb/Features/v4/ae/d9/28/aed9.png/5760x1310bb.png");
        assert!(artist_logo("{\"artistLogo\":null}").is_none());
        // A null logo must not borrow the next image on the page.
        let none = r#"{"artistLogo":null}"#.to_owned()
            + r#"{"editorialVideo":{"dictionary":{"url":"https://img/other.png/{w}x{h}{c}.{f}","width":100,"height":100}}}"#;
        assert!(artist_logo(&none).is_none());
    }

    #[test]
    fn a_media_playlist_resolves_its_one_file() {
        let media = "https://mvod.itunes.apple.com/a/b/c/clip.m3u8";
        assert_eq!(resolve(media, "clip-.mp4"), "https://mvod.itunes.apple.com/a/b/c/clip-.mp4");
        assert_eq!(resolve(media, "https://x/y.m3u8"), "https://x/y.m3u8");
    }
}
