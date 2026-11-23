//! Time-synced lyrics from every source the Better Lyrics extension uses, parsed the way
//! its parsers (`@braccato/parsers`) parse them, so any of them can be shown and the
//! listener can switch between them.
//!
//! - The Better Lyrics API streams its providers as server-sent events: Better Lyrics
//!   (Apple-style TTML), BiniLyrics (TTML), Musixmatch (word-by-word and line LRC), LRCLib
//!   (LRC and plain), Legato (KuGou LRC) and Portato (QQ QRC).
//! - Unison, the community lyrics service, answers a plain GET.
//! - lrc.red provides TTML; Genius provides unsynced lyrics from its public website.
//!
//! The API needs a JWT, obtained by exchanging a Cloudflare Turnstile token (see
//! turnstile.rs). JWTs last 24 h and are cached on disk.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

const API: &str = "https://lyrics.api.dacubeking.com/";
const UNISON_API: &str = "https://unison.betterlyrics.org/lyrics";
const LRCRED_API: &str = "https://lrc.red/";
const GENIUS_SEARCH: &str = "https://genius.com/api/search/multi";
// A non-browser UA avoids claiming browser TLS/client-hint fingerprints.
const GENIUS_UA: &str = "BitChord";

/// lrc.red's free API asks callers to identify themselves with a link.
const LRCRED_UA: &str = concat!("BitChord/", env!("CARGO_PKG_VERSION"), " (+https://github.com/jackychan4891/BitChord-PC-Continued)");

/// One source of lyrics, in default priority order (the extension's
/// `PROVIDER_CONFIGS`, without YouTube, plus additional providers).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Provider {
    BLyricsRich,
    UnisonRich,
    BinimumRich,
    UnisonWord,
    PortatoRich,
    MusixmatchRich,
    LrcredRich,
    BLyricsSynced,
    UnisonSynced,
    BinimumSynced,
    LrclibSynced,
    LegatoSynced,
    MusixmatchSynced,
    LrcredSynced,
    UnisonPlain,
    LrclibPlain,
    GeniusPlain,
}

/// How finely a provider's lyrics are timed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SyncKind {
    Syllable,
    Word,
    Line,
    Unsynced,
}

impl Provider {
    pub const ALL: [Provider; 17] = [
        Provider::BLyricsRich,
        Provider::UnisonRich,
        Provider::BinimumRich,
        Provider::UnisonWord,
        Provider::PortatoRich,
        Provider::MusixmatchRich,
        Provider::LrcredRich,
        Provider::BLyricsSynced,
        Provider::UnisonSynced,
        Provider::BinimumSynced,
        Provider::LrclibSynced,
        Provider::LegatoSynced,
        Provider::MusixmatchSynced,
        Provider::LrcredSynced,
        Provider::UnisonPlain,
        Provider::LrclibPlain,
        Provider::GeniusPlain,
    ];

    /// The extension's key for it (also how the order is stored).
    pub fn key(self) -> &'static str {
        match self {
            Provider::BLyricsRich => "bLyrics-richsynced",
            Provider::UnisonRich => "unison-richsynced",
            Provider::BinimumRich => "binimum-richsynced",
            Provider::UnisonWord => "unison-wordsynced",
            Provider::PortatoRich => "portato-richsynced",
            Provider::MusixmatchRich => "musixmatch-richsync",
            Provider::LrcredRich => "lrcred-richsynced",
            Provider::BLyricsSynced => "bLyrics-synced",
            Provider::UnisonSynced => "unison-synced",
            Provider::BinimumSynced => "binimum-synced",
            Provider::LrclibSynced => "lrclib-synced",
            Provider::LegatoSynced => "legato-synced",
            Provider::MusixmatchSynced => "musixmatch-synced",
            Provider::LrcredSynced => "lrcred-synced",
            Provider::UnisonPlain => "unison-plain",
            Provider::LrclibPlain => "lrclib-plain",
            Provider::GeniusPlain => "genius-plain",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.key() == key)
    }

    pub fn name(self) -> &'static str {
        match self {
            Provider::BLyricsRich | Provider::BLyricsSynced => "Better Lyrics",
            Provider::UnisonRich | Provider::UnisonWord | Provider::UnisonSynced | Provider::UnisonPlain => "Unison",
            Provider::BinimumRich | Provider::BinimumSynced => "BiniLyrics",
            Provider::PortatoRich => "Better Lyrics Portato",
            Provider::MusixmatchRich | Provider::MusixmatchSynced => "Musixmatch",
            Provider::LrclibSynced | Provider::LrclibPlain => "LRCLib",
            Provider::LrcredRich | Provider::LrcredSynced => "lrc.red",
            Provider::LegatoSynced => "Better Lyrics Legato",
            Provider::GeniusPlain => "Genius",
        }
    }

    pub fn sync(self) -> SyncKind {
        match self {
            Provider::BLyricsRich | Provider::UnisonRich | Provider::BinimumRich => SyncKind::Syllable,
            Provider::UnisonWord | Provider::PortatoRich | Provider::MusixmatchRich | Provider::LrcredRich => SyncKind::Word,
            Provider::BLyricsSynced
            | Provider::UnisonSynced
            | Provider::BinimumSynced
            | Provider::LrclibSynced
            | Provider::LegatoSynced
            | Provider::MusixmatchSynced
            | Provider::LrcredSynced => SyncKind::Line,
            Provider::UnisonPlain | Provider::LrclibPlain | Provider::GeniusPlain => SyncKind::Unsynced,
        }
    }
}

/// How a set of lines is timed, decided from the lines themselves (`deriveSyncType`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Sync {
    None,
    Line,
    Word,
}

/// A timed piece of a line: a syllable, a word or several words.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub start: i32,
    pub dur: i32,
    pub text: String,
    /// Background vocals, shown on their own row under the line.
    pub bg: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Line {
    pub start: i32,
    pub dur: i32,
    pub text: String,
    /// Empty for line-synced and plain lyrics.
    pub parts: Vec<Part>,
    /// Singer: `v1`, `v2`, … or `v1000` for everyone.
    pub agent: Option<String>,
    /// A gap of more than five seconds nobody sings over.
    pub instrumental: bool,
    /// Translations shipped with the lyrics, by language.
    pub translations: Vec<(String, String)>,
    /// Romanization shipped with the lyrics, and its timing when it has some.
    pub romanization: Option<String>,
    pub timed_roman: Vec<Part>,
}

impl Line {
    pub fn end(&self) -> i32 {
        self.start + self.dur
    }
}

#[derive(Clone, Debug)]
pub struct Lyrics {
    pub lines: Vec<Line>,
    pub sync: Sync,
    pub provider: Provider,
    /// Language the source says the lyrics are in.
    pub language: Option<String>,
    pub songwriters: Vec<String>,
}

impl Lyrics {
    fn new(provider: Provider, lines: Vec<Line>, language: Option<String>, songwriters: Vec<String>) -> Option<Self> {
        if lines.is_empty() {
            return None;
        }
        let sync = derive_sync(&lines);
        Some(Self { lines, sync, provider, language, songwriters })
    }
}

/// `deriveSyncType`: richsync if any sung line has a part with a duration.
fn derive_sync(lines: &[Line]) -> Sync {
    if lines.iter().any(|l| !l.instrumental && l.parts.iter().any(|p| p.dur != 0)) {
        Sync::Word
    } else if lines.iter().all(|l| l.start == 0) {
        Sync::None
    } else {
        Sync::Line
    }
}

/// Every source that had lyrics for a song, in default priority order.
#[derive(Clone, Debug, Default)]
pub struct LyricsSet {
    pub sources: Vec<Lyrics>,
}

impl LyricsSet {
    pub fn get(&self, p: Provider) -> Option<&Lyrics> {
        self.sources.iter().find(|l| l.provider == p)
    }
}

// ------------------------------------------------------------------- API client

pub struct Client {
    http: reqwest::Client,
    /// Separate redirect policy for untrusted Genius search results.
    genius_about_http: tokio::sync::OnceCell<reqwest::Client>,
    jwt_file: PathBuf,
    webview_profile: PathBuf,
    jwt: tokio::sync::Mutex<Option<String>>,
    /// When the Turnstile challenge last failed; it isn't retried for a while, so
    /// every new song doesn't start another WebView2.
    challenge_failed: std::sync::Mutex<Option<std::time::Instant>>,
    /// Final results and the newest request for each song still being fetched.
    recent: std::sync::Arc<std::sync::Mutex<RecentLyrics>>,
}

const CHALLENGE_RETRY: Duration = Duration::from_secs(15 * 60);
const RECENT: usize = 8;

#[derive(Default)]
struct RecentLyrics {
    entries: std::collections::VecDeque<(String, bool, LyricsSet)>,
    // Keep the newest generation until every overlapping request has finished, even if
    // its result was empty or was evicted while an older request was still running.
    pending: HashMap<String, (u64, usize)>,
}

/// Registered when the request is issued, before it waits for the stream duration.
pub struct FetchTicket {
    recent: std::sync::Arc<std::sync::Mutex<RecentLyrics>>,
    video_id: String,
    generation: u64,
}

impl FetchTicket {
    fn is_current(&self) -> bool {
        self.recent.lock().unwrap().pending.get(&self.video_id).is_some_and(|(newest, _)| *newest == self.generation)
    }

    fn cached(&self, genius_enabled: bool) -> Option<LyricsSet> {
        self.recent.lock().unwrap().entries.iter().find(|(id, enabled, _)| id == &self.video_id && *enabled == genius_enabled).map(|(_, _, set)| set.clone())
    }

    fn cache_final(&self, genius_enabled: bool, set: &LyricsSet) {
        if set.sources.is_empty() {
            return;
        }
        let mut recent = self.recent.lock().unwrap();
        if !recent.pending.get(&self.video_id).is_some_and(|(newest, _)| *newest == self.generation) {
            return;
        }
        recent.entries.retain(|(id, _, _)| id != &self.video_id);
        while recent.entries.len() >= RECENT {
            recent.entries.pop_front();
        }
        recent.entries.push_back((self.video_id.clone(), genius_enabled, set.clone()));
    }
}

impl Drop for FetchTicket {
    fn drop(&mut self) {
        let mut recent = self.recent.lock().unwrap();
        if let Some((_, count)) = recent.pending.get_mut(&self.video_id) {
            *count -= 1;
            if *count == 0 {
                recent.pending.remove(&self.video_id);
            }
        }
    }
}

pub struct Query<'a> {
    pub video_id: &'a str,
    pub song: &'a str,
    pub artist: &'a str,
    pub album: Option<&'a str>,
    pub duration: Option<u32>,
}

impl Client {
    pub fn new(http: reqwest::Client, data_dir: &Path) -> Self {
        let jwt_file = data_dir.join("lyrics_jwt");
        let cached = std::fs::read_to_string(&jwt_file).ok().filter(|t| jwt_seconds_left(t) > 3600);
        Self {
            http,
            genius_about_http: tokio::sync::OnceCell::new(),
            jwt_file,
            webview_profile: data_dir.join("webview-lyrics"),
            jwt: tokio::sync::Mutex::new(cached),
            challenge_failed: Default::default(),
            recent: Default::default(),
        }
    }

    /// Register synchronously so a request waiting for duration still supersedes older ones.
    pub fn begin_fetch(&self, video_id: &str, generation: u64) -> FetchTicket {
        let mut recent = self.recent.lock().unwrap();
        let pending = recent.pending.entry(video_id.to_owned()).or_insert((generation, 0));
        pending.0 = pending.0.max(generation);
        pending.1 += 1;
        FetchTicket { recent: self.recent.clone(), video_id: video_id.to_owned(), generation }
    }

    pub async fn fetch(&self, q: &Query<'_>, ticket: &FetchTicket, genius_enabled: bool, partial: impl FnMut(LyricsSet) + Send) -> anyhow::Result<LyricsSet> {
        if let Some(set) = ticket.cached(genius_enabled) {
            return Ok(set);
        }
        let genius = async {
            if genius_enabled && ticket.is_current() { self.fetch_genius(q).await } else { Ok(Vec::new()) }
        };
        let (set, genius_ok) = gather_lyrics(self.fetch_stream(q), self.fetch_unison(q), self.fetch_lrcred(q), genius, partial).await;
        if genius_ok {
            ticket.cache_final(genius_enabled, &set);
        }
        Ok(set)
    }

    async fn fetch_stream(&self, q: &Query<'_>) -> anyhow::Result<Vec<Lyrics>> {
        let mut jwt = self.token(false).await?;
        for attempt in 0..2 {
            let mut form = vec![("videoId", q.video_id.to_owned())];
            if !q.song.is_empty() {
                form.push(("song", q.song.to_owned()));
            }
            if !q.artist.is_empty() {
                form.push(("artist", q.artist.to_owned()));
            }
            if let Some(d) = q.duration.filter(|d| *d > 0) {
                form.push(("duration", d.to_string()));
            }
            if let Some(a) = q.album {
                form.push(("album", a.to_owned()));
            }
            form.push(("alwaysFetchMetadata", "false".to_owned()));
            form.push(("token", jwt.clone()));
            let resp = self.http.post(format!("{API}v2/lyrics")).form(&form).timeout(Duration::from_secs(20)).send().await?;
            if resp.status() == reqwest::StatusCode::FORBIDDEN && attempt == 0 {
                jwt = self.token(true).await?;
                continue;
            }
            let body = resp.error_for_status()?.text().await?;
            return Ok(from_stream(&body, q));
        }
        unreachable!()
    }

    async fn fetch_unison(&self, q: &Query<'_>) -> anyhow::Result<Vec<Lyrics>> {
        let mut params = vec![("v", q.video_id.to_owned()), ("song", q.song.to_owned()), ("artist", q.artist.to_owned())];
        params.push(("duration", q.duration.unwrap_or(0).to_string()));
        if let Some(a) = q.album {
            params.push(("album", a.to_owned()));
        }
        let resp = self.http.get(UNISON_API).query(&params).timeout(Duration::from_secs(10)).send().await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(vec![]);
        }
        let v: Value = resp.error_for_status()?.json().await?;
        Ok(from_unison(&v["data"], q.duration.unwrap_or(0) as i32 * 1000))
    }

    /// lrc.red: find the track in its catalogue, then fetch its TTML (word or line timed).
    async fn fetch_lrcred(&self, q: &Query<'_>) -> anyhow::Result<Vec<Lyrics>> {
        if q.song.trim().is_empty() {
            return Ok(vec![]);
        }
        let query = match q.artist.trim().is_empty() {
            true => q.song.to_owned(),
            false => format!("{} {}", q.song, q.artist),
        };
        let hits: Value = self
            .http
            .get(format!("{LRCRED_API}search.json"))
            .query(&[("q", query)])
            .header(reqwest::header::USER_AGENT, LRCRED_UA)
            .timeout(Duration::from_secs(8))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let Some(hit) = best_lrcred_hit(hits["hits"].as_array().map(Vec::as_slice).unwrap_or_default(), q) else {
            return Ok(vec![]);
        };
        let Some(isrc) = hit["isrc"].as_str().filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric())) else {
            return Ok(vec![]);
        };
        let text = self
            .http
            .get(format!("{LRCRED_API}s/{isrc}.ttml"))
            .header(reqwest::header::USER_AGENT, LRCRED_UA)
            .timeout(Duration::from_secs(8))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        let t = parse_ttml(&text, q.duration.unwrap_or(0) as i32 * 1000);
        // lrc.red declares its own timing on the root; fall back to the parsed spans.
        let word = if text.contains("lrc:timing=\"Line\"") {
            false
        } else if text.contains("lrc:timing=\"Word\"") || text.contains("lrc:timing=\"Syllable\"") {
            true
        } else {
            t.word_synced
        };
        let p = if word { Provider::LrcredRich } else { Provider::LrcredSynced };
        Ok(Lyrics::new(p, t.lines, t.language, t.songwriters).into_iter().collect())
    }

    /// Anonymous Genius search and page scrape. Bound the whole fallback, including retries.
    ///
    /// The cause travels with the error, because "it failed" and "the site answered 403"
    /// are different problems to fix; the URL that would have said more is off it already.
    async fn fetch_genius(&self, q: &Query<'_>) -> anyhow::Result<Vec<Lyrics>> {
        tokio::time::timeout(Duration::from_secs(12), self.fetch_genius_inner(q)).await
            .map_err(|_| anyhow::anyhow!("Genius lyrics lookup timed out"))?
    }

    async fn fetch_genius_inner(&self, q: &Query<'_>) -> anyhow::Result<Vec<Lyrics>> {
        for attempt in genius_search_attempts(q.song, q.artist) {
            let Some(body) = self.genius_page(&attempt).await? else { continue };
            let lines = parse_genius_html(&String::from_utf8_lossy(&body));
            return Ok(Lyrics::new(Provider::GeniusPlain, lines, None, vec![]).into_iter().collect());
        }
        Ok(vec![])
    }

    /// Genius's description of the song, not its lyrics or a line annotation.
    /// `Ok(None)` means no suitable match or description; errors carry the site's answer but
    /// never a URL. This runs only when explicitly requested and does not use a JWT.
    pub async fn genius_about(&self, q: &Query<'_>) -> anyhow::Result<Option<String>> {
        if q.song.trim().is_empty() || q.artist.trim().is_empty() {
            return Ok(None);
        }
        tokio::time::timeout(Duration::from_secs(12), self.genius_about_inner(q)).await
            .map_err(|_| anyhow::anyhow!("Genius about lookup timed out"))?
    }

    async fn genius_about_inner(&self, q: &Query<'_>) -> anyhow::Result<Option<String>> {
        // The shared matcher also supports title-only lyric searches. About must never
        // accept one after cleaning away the artist's entire name.
        let attempts = genius_search_attempts(q.song, q.artist).into_iter()
            .filter(|attempt| !genius_fold(&attempt.artist).is_empty() && !genius_fold(&attempt.title).is_empty());
        for attempt in attempts {
            let Some(body) = self.genius_page(&attempt).await? else { continue };
            return Ok(parse_genius_about(&String::from_utf8_lossy(&body)));
        }
        Ok(None)
    }

    /// Both Genius lookups use the proxy-aware client with restricted redirects and bodies.
    ///
    /// The app's one client that isn't the system's TLS: Genius's bot filter scores the
    /// handshake before it looks at the headers, and the Windows stack's signature reads as
    /// a non-browser wearing a costume — rustls's is let through. What a request would have
    /// named in the log (the song, in the search's query) comes off the error first.
    async fn genius_page(&self, attempt: &GeniusAttempt) -> anyhow::Result<Option<Vec<u8>>> {
        let http = self.genius_about_http.get_or_try_init(|| async {
            crate::proxy::apply(reqwest::Client::builder())
                .use_rustls_tls()
                .redirect(genius_redirect_policy())
                .connect_timeout(Duration::from_secs(8))
                .build()
                .map_err(|_| anyhow::anyhow!("Genius HTTP client setup failed"))
        }).await?;
        let response = http.get(GENIUS_SEARCH)
            .query(&[("q", &attempt.query)])
            .header(reqwest::header::USER_AGENT, GENIUS_UA)
            .header(reqwest::header::ACCEPT, "application/json, text/html;q=0.9, */*;q=0.8")
            .header(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
            .timeout(Duration::from_secs(8))
            .send().await.map_err(|e| anyhow::anyhow!("Genius search request failed: {}", reqwest::Error::without_url(e)))?;
        let body = genius_bounded_body(response, 1024 * 1024).await.map_err(|e| anyhow::anyhow!("Genius search response failed: {e:#}"))?;
        let hits: Value = serde_json::from_slice(&body).map_err(|_| anyhow::anyhow!("invalid Genius search response"))?;
        let Some(url) = best_genius_url(&hits, &attempt.title, &attempt.artist) else { return Ok(None) };
        // A song page is identified by its path, not untrusted query parameters.
        let url = genius_page_url(&url)?;
        let response = http.get(url)
            .header(reqwest::header::USER_AGENT, GENIUS_UA)
            .header(reqwest::header::ACCEPT, "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
            .header(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
            .timeout(Duration::from_secs(8))
            .send().await.map_err(|e| anyhow::anyhow!("Genius page request failed: {}", reqwest::Error::without_url(e)))?;
        let body = genius_bounded_body(response, 4 * 1024 * 1024).await.map_err(|e| anyhow::anyhow!("Genius page response failed: {e:#}"))?;
        Ok(Some(body))
    }

    /// A valid JWT, from cache or by running the Turnstile challenge.
    async fn token(&self, force_new: bool) -> anyhow::Result<String> {
        let mut slot = self.jwt.lock().await;
        if let Some(t) = slot.as_ref().filter(|t| !force_new && jwt_seconds_left(t) > 60) {
            return Ok(t.clone());
        }
        if self.challenge_failed.lock().unwrap().is_some_and(|at| at.elapsed() < CHALLENGE_RETRY) {
            anyhow::bail!("lyrics verification failed recently; retrying later");
        }
        let profile = self.webview_profile.clone();
        let turnstile = match tokio::task::spawn_blocking(move || crate::turnstile::obtain(profile)).await? {
            Ok(t) => t,
            Err(e) => {
                *self.challenge_failed.lock().unwrap() = Some(std::time::Instant::now());
                return Err(e);
            }
        };
        let resp: Value = self
            .http
            .post(format!("{API}verify-turnstile"))
            .json(&serde_json::json!({ "token": turnstile }))
            .timeout(Duration::from_secs(15))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let jwt = resp["jwt"].as_str().ok_or_else(|| anyhow::anyhow!("no JWT in response"))?.to_owned();
        let _ = std::fs::write(&self.jwt_file, &jwt);
        log::info!("lyrics: new access token ({}h)", jwt_seconds_left(&jwt) / 3600);
        *slot = Some(jwt.clone());
        Ok(jwt)
    }
}

async fn gather_lyrics<S, U, L, G>(stream: S, unison: U, lrcred: L, genius: G, mut partial: impl FnMut(LyricsSet)) -> (LyricsSet, bool)
where
    S: Future<Output = anyhow::Result<Vec<Lyrics>>>,
    U: Future<Output = anyhow::Result<Vec<Lyrics>>>,
    L: Future<Output = anyhow::Result<Vec<Lyrics>>>,
    G: Future<Output = anyhow::Result<Vec<Lyrics>>>,
{
    tokio::pin!(stream, unison, lrcred, genius);
    let (mut stream_done, mut unison_done, mut lrcred_done, mut genius_done) = (false, false, false, false);
    let (mut completed, mut genius_ok) = (0, true);
    let mut sources = Vec::new();
    while completed < 4 {
        let (provider, result) = tokio::select! {
            result = &mut stream, if !stream_done => { stream_done = true; ("better lyrics", result) },
            result = &mut unison, if !unison_done => { unison_done = true; ("unison", result) },
            result = &mut lrcred, if !lrcred_done => { lrcred_done = true; ("lrc.red", result) },
            result = &mut genius, if !genius_done => { genius_done = true; ("genius", result) },
        };
        completed += 1;
        match result {
            Ok(found) if !found.is_empty() => {
                sources.extend(found);
                sources.sort_by_key(|l| Provider::ALL.iter().position(|p| *p == l.provider));
                if completed < 4 {
                    partial(LyricsSet { sources: sources.clone() });
                }
            }
            Ok(_) => {}
            Err(e) if provider == "genius" => {
                genius_ok = false;
                log::info!("genius: {e:#}");
            }
            Err(e) => log::info!("{provider}: {e:#}"),
        }
    }
    (LyricsSet { sources }, genius_ok)
}

fn jwt_seconds_left(jwt: &str) -> i64 {
    use base64::Engine;
    let Some(payload) = jwt.split('.').nth(1) else { return 0 };
    let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')) else {
        return 0;
    };
    let exp = serde_json::from_slice::<Value>(&bytes).ok().and_then(|v| v["exp"].as_i64()).unwrap_or(0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    exp - now
}

// ------------------------------------------------------------------- provider results

/// Every provider's lyrics out of a complete SSE response (`processStreamData`).
pub fn from_stream(body: &str, q: &Query) -> Vec<Lyrics> {
    let mut out: Vec<Lyrics> = Vec::new();
    let mut dur_ms = q.duration.unwrap_or(0) as i32 * 1000;
    let (mut title, mut artist) = (q.song.to_owned(), q.artist.to_owned());
    for block in body.split("\n\n").flat_map(|b| b.split("\r\n\r\n")) {
        let mut event = "";
        let mut data = String::new();
        for line in block.lines() {
            if let Some(e) = line.strip_prefix("event:") {
                event = e.trim();
            } else if let Some(d) = line.strip_prefix("data:") {
                data.push_str(d.trim());
            }
        }
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&data) else { continue };
        if event == "metadata" {
            if let Some(d) = v["duration"].as_str().and_then(|d| d.parse::<f64>().ok()).or_else(|| v["duration"].as_f64()) {
                dur_ms = (d * 1000.0) as i32;
            }
            if let Some(s) = v["song"].as_str() {
                title = s.to_owned();
            }
            if let Some(a) = v["artist"].as_str() {
                artist = a.to_owned();
            }
            continue;
        }
        let r = &v["results"];
        let s = |k: &str| r[k].as_str().filter(|s| !s.is_empty());
        let mut push = |p: Provider, lines: Vec<Line>, lang: Option<String>, writers: Vec<String>| {
            if let Some(l) = Lyrics::new(p, lines, lang, writers) {
                out.retain(|o| o.provider != p);
                out.push(l);
            }
        };
        match v["provider"].as_str().unwrap_or_default() {
            "musixmatch" => {
                if let Some(t) = s("wordByWord") {
                    let mut lines = parse_lrc(t, dur_ms);
                    lrc_fixers(&mut lines);
                    push(Provider::MusixmatchRich, lines, None, lrc_songwriters(t));
                }
                if let Some(t) = s("synced") {
                    push(Provider::MusixmatchSynced, parse_lrc(t, dur_ms), None, lrc_songwriters(t));
                }
            }
            "lrclib" => {
                if let Some(t) = s("synced") {
                    push(Provider::LrclibSynced, parse_lrc(t, dur_ms), None, lrc_songwriters(t));
                }
                if let Some(t) = s("plain") {
                    push(Provider::LrclibPlain, parse_plain(t), None, vec![]);
                }
            }
            "kugou" => {
                if let Some(t) = s("lyrics").and_then(|t| serde_json::from_str::<Value>(t).ok()).and_then(|j| j["lyrics"].as_str().map(str::to_owned)) {
                    push(Provider::LegatoSynced, parse_lrc(&t, dur_ms), None, lrc_songwriters(&t));
                }
            }
            "qq" => {
                if let Some(t) = s("lyrics").and_then(|t| serde_json::from_str::<Value>(t).ok()).and_then(|j| j["lyrics"].as_str().map(str::to_owned)) {
                    push(Provider::PortatoRich, parse_qrc(&t, dur_ms, Some((&title, &artist))), None, qrc_songwriters(&t));
                }
            }
            "golyrics" => {
                if let Some(raw) = s("lyrics") {
                    let ttml = serde_json::from_str::<Value>(raw).ok().and_then(|j| j["ttml"].as_str().map(str::to_owned)).unwrap_or_else(|| raw.to_owned());
                    let t = parse_ttml(&ttml, dur_ms);
                    let p = if t.word_synced { Provider::BLyricsRich } else { Provider::BLyricsSynced };
                    push(p, t.lines, t.language, t.songwriters);
                }
            }
            "binimum" => {
                if let Some(raw) = s("lyrics") {
                    let t = parse_ttml(raw, dur_ms);
                    let word = match r["timingType"].as_str() {
                        Some("syllable") => true,
                        Some("line") => false,
                        _ => t.word_synced,
                    };
                    let p = if word { Provider::BinimumRich } else { Provider::BinimumSynced };
                    push(p, t.lines, t.language, t.songwriters);
                }
            }
            _ => {}
        }
    }
    out
}

/// A Unison answer (`unison.ts`): TTML, LRC or plain text.
fn from_unison(data: &Value, dur_ms: i32) -> Vec<Lyrics> {
    let (Some(format), Some(text)) = (data["format"].as_str(), data["lyrics"].as_str()) else { return vec![] };
    let lyrics = match format {
        "ttml" => {
            let t = parse_ttml(text, dur_ms);
            let p = if t.word_synced { Provider::UnisonRich } else { Provider::UnisonSynced };
            Lyrics::new(p, t.lines, t.language, t.songwriters)
        }
        "lrc" => {
            let p = if data["syncType"] == "richsync" { Provider::UnisonWord } else { Provider::UnisonSynced };
            Lyrics::new(p, parse_lrc(text, dur_ms), data["language"].as_str().map(str::to_owned), lrc_songwriters(text))
        }
        "plain" => Lyrics::new(Provider::UnisonPlain, parse_plain(text), None, vec![]),
        _ => None,
    };
    lyrics.into_iter().collect()
}

/// The lrc.red search hit closest to the track: same title (a remaster or live
/// suffix aside), a matching artist, and within twenty seconds of the known length.
fn best_lrcred_hit<'a>(hits: &'a [Value], q: &Query<'_>) -> Option<&'a Value> {
    let title = fold_title(q.song);
    let artist = fold(q.artist);
    let dur = q.duration.filter(|d| *d > 0).map(f64::from);
    let mut best: Option<(i32, &Value)> = None;
    for h in hits {
        let ht = fold_title(h["title"].as_str().unwrap_or_default());
        let ha = fold(h["artist"].as_str().unwrap_or_default());
        let title_ok = !title.is_empty() && (ht == title || ht.contains(&title) || title.contains(&ht));
        let artist_ok = artist.is_empty() || ha == artist || ha.contains(&artist) || artist.contains(&ha);
        if !title_ok || !artist_ok {
            continue;
        }
        let diff = match (dur, h["duration"].as_f64()) {
            (Some(a), Some(b)) => (a - b).abs(),
            _ => 0.0,
        };
        if diff > 20.0 {
            continue;
        }
        let base = if ht == title { 100 } else { 60 };
        let score = base + (20.0 - diff) as i32;
        if best.is_none_or(|(s, _)| score > s) {
            best = Some((score, h));
        }
    }
    best.map(|(_, h)| h)
}

/// Letters and digits only, lower case, single-spaced.
fn fold(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { ' ' }).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A title without its bracketed or dashed qualifiers: "(Remastered 2011)", " - Live".
fn fold_title(s: &str) -> String {
    let s = s.split(" (").next().unwrap_or(s);
    let s = s.split(" [").next().unwrap_or(s);
    let s = s.split(" - ").next().unwrap_or(s);
    fold(s)
}

// ------------------------------------------------------------------- Genius

struct GeniusAttempt {
    query: String,
    title: String,
    artist: String,
}

/// Remove common video/credit decorations without discarding meaningful song titles.
fn genius_clean_query(text: &str) -> String {
    let text = text.split(" | ").next().unwrap_or(text);
    let mut cleaned = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if matches!(c, '(' | '[') {
            let close = if c == '(' { ')' } else { ']' };
            let mut bracket = String::new();
            let mut found = false;
            for ch in chars.by_ref() {
                if ch == close {
                    found = true;
                    break;
                }
                bracket.push(ch);
            }
            let lower = bracket.trim().to_lowercase();
            let noise = ["from ", "feat", "ft.", "ft ", "featuring", "with ", "prod", "official", "lyrical", "video", "audio", "music video", "visualizer", "mv", "hd", "4k", "hq", "full song", "remaster"];
            if !found || !noise.iter().any(|n| lower.starts_with(n)) {
                cleaned.push(c);
                cleaned.push_str(&bracket);
                if found {
                    cleaned.push(close);
                }
            } else {
                cleaned.push(' ');
            }
        } else if matches!(c, '♪' | '♫' | '★' | '☆' | '【' | '】' | '《' | '》' | '「' | '」' | '~' | '_') {
            cleaned.push(' ');
        } else {
            cleaned.push(c);
        }
    }
    // These are decorations rather than title qualifiers, so remove them even unbracketed.
    // ASCII-only case folding keeps byte offsets valid in the original Unicode string.
    let lower = cleaned.to_ascii_lowercase();
    let end = [" official music video", " official video", " official audio", " music video", " lyrical video", " full song", " prod.", " produced by "]
        .iter()
        .filter_map(|n| lower.find(n))
        .min()
        .unwrap_or(cleaned.len());
    cleaned[..end].split_whitespace().collect::<Vec<_>>().join(" ")
}

fn genius_without_brackets(text: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in text.chars() {
        match c {
            '(' | '[' => { inside = true; out.push(' '); }
            ')' | ']' if inside => inside = false,
            _ if !inside => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn genius_search_attempts(song: &str, artist: &str) -> Vec<GeniusAttempt> {
    let clean_title = genius_clean_query(song);
    let clean_artist = genius_clean_query(artist);
    if clean_title.is_empty() {
        return vec![];
    }
    let mut title = clean_title.as_str();
    let mut artist = clean_artist.as_str();
    if let Some((left, right)) = [" - ", " – ", " — ", ": ", "–", "—"].iter().find_map(|sep| clean_title.split_once(*sep)) {
        if !artist.is_empty() && genius_fold(left) == genius_fold(artist) {
            title = right.trim();
        } else if !artist.is_empty() && genius_fold(right) == genius_fold(artist) {
            title = left.trim();
        } else if artist.is_empty() && !left.trim().is_empty() && !right.trim().is_empty() {
            artist = left.trim();
            title = right.trim();
        }
    }
    let mut attempts = Vec::new();
    let mut add = |query: String, title: &str, artist: &str| {
        let query = query.trim().to_owned();
        if !query.is_empty() && !title.trim().is_empty() && !attempts.iter().any(|a: &GeniusAttempt| a.query == query) {
            attempts.push(GeniusAttempt { query, title: title.to_owned(), artist: artist.to_owned() });
        }
    };
    add(format!("{artist} {title}"), title, artist);
    let unbracketed = genius_without_brackets(title);
    if unbracketed != title && !unbracketed.is_empty() {
        // Keep the original title for matching; the shorter form is only a search hint.
        add(format!("{artist} {unbracketed}"), title, artist);
    }
    if clean_title != title {
        add(format!("{clean_artist} {clean_title}"), &clean_title, &clean_artist);
    }
    add(unbracketed, title, artist);
    attempts
}

/// Unicode-aware, punctuation-insensitive comparison, with words kept separate.
fn genius_fold(s: &str) -> String {
    let mut folded = String::new();
    for c in s.chars() {
        if c.is_alphanumeric() {
            folded.extend(c.to_lowercase());
        } else {
            folded.push(' ');
        }
    }
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn genius_phrase_contains(haystack: &str, needle: &str) -> bool {
    !needle.is_empty() && format!(" {haystack} ").contains(&format!(" {needle} "))
}

/// Limit both the initial page and redirects to Genius over standard HTTPS.
fn safe_genius_url(url: &url::Url) -> bool {
    url.scheme() == "https" && matches!(url.host_str(), Some("genius.com" | "www.genius.com"))
        && url.port_or_known_default() == Some(443) && url.username().is_empty() && url.password().is_none()
}

fn genius_page_url(raw: &str) -> anyhow::Result<url::Url> {
    let mut url = url::Url::parse(raw).map_err(|_| anyhow::anyhow!("invalid Genius page URL"))?;
    anyhow::ensure!(safe_genius_url(&url), "unsafe Genius page URL");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn genius_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() < 5 && safe_genius_url(attempt.url()) {
            attempt.follow()
        } else {
            // A redirect error can nest the previous URL, including the search query.
            attempt.stop()
        }
    })
}

/// Reject oversized responses, including chunked or compressed bodies with no useful length.
async fn genius_bounded_body(response: reqwest::Response, limit: usize) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(!response.status().is_redirection(), "Genius redirect refused: {}", response.status());
    let mut response = response.error_for_status().map_err(reqwest::Error::without_url)?;
    anyhow::ensure!(response.content_length().is_none_or(|n| n <= limit as u64), "Genius response exceeds {limit} bytes");
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(reqwest::Error::without_url)? {
        anyhow::ensure!(chunk.len() <= limit.saturating_sub(body.len()), "Genius response exceeds {limit} bytes");
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Match both title and known artist; never fall back to an unrelated search hit.
fn best_genius_url(response: &Value, title: &str, artist: &str) -> Option<String> {
    let target_title = genius_fold(&genius_clean_query(title));
    let target_artist = genius_fold(artist);
    if target_title.is_empty() {
        return None;
    }
    let mut best: Option<(i32, String)> = None;
    for section in response["response"]["sections"].as_array().into_iter().flatten().filter(|s| s["type"] == "song") {
        for hit in section["hits"].as_array().into_iter().flatten() {
            let item = &hit["result"];
            let hit_title = genius_fold(&genius_clean_query(item["title"].as_str().unwrap_or_default()));
            let hit_artist = genius_fold(item["artist_names"].as_str().filter(|s| !s.trim().is_empty()).or_else(|| item["primary_artist"]["name"].as_str()).unwrap_or_default());
            if hit_title.is_empty() || (!target_artist.is_empty() && hit_artist.is_empty()) {
                continue;
            }
            let exact_title = hit_title == target_title;
            let shorter = hit_title.chars().count().min(target_title.chars().count());
            let longer = hit_title.chars().count().max(target_title.chars().count());
            let close_title = shorter >= 4 && shorter * 4 >= longer * 3
                && (genius_phrase_contains(&hit_title, &target_title) || genius_phrase_contains(&target_title, &hit_title));
            if !exact_title && !close_title {
                continue;
            }
            let exact_artist = hit_artist == target_artist;
            let hit_contains_artist = genius_phrase_contains(&hit_artist, &target_artist);
            // Don't let a generic fragment like "The" match "The Beatles".
            let artist_contains_hit = hit_artist.chars().count() >= 4
                && hit_artist.chars().count() * 2 >= target_artist.chars().count()
                && genius_phrase_contains(&target_artist, &hit_artist);
            if !target_artist.is_empty() && !exact_artist && !hit_contains_artist && !artist_contains_hit {
                continue;
            }
            let Some(url) = item["url"].as_str().and_then(|u| url::Url::parse(u).ok()) else { continue };
            if !safe_genius_url(&url) {
                continue;
            }
            let path = format!("{} {}", item["path"].as_str().unwrap_or_default(), url.path()).to_lowercase();
            let target = title.to_lowercase();
            if [("translation", "translation"), ("türkçe", "türkçe"), ("turkce", "turkce"), ("polskie-tlumaczenie", "polskie"),
                ("tracklist", "tracklist"), ("album-art", "album art"), ("instrumental", "instrumental"), ("-review", "review")]
                .iter().any(|(noise, wanted)| path.contains(noise) && !target.contains(wanted))
            {
                continue;
            }
            let score = (if exact_title { 50 } else { 25 }) + (if target_artist.is_empty() { 0 } else if exact_artist { 40 } else { 20 });
            if best.as_ref().is_none_or(|(s, _)| score > *s) {
                best = Some((score, url.into()));
            }
        }
    }
    best.map(|(_, url)| url)
}

// ------------------------------------------------------------------- shared helpers

/// Gaps over this long between sung lines become instrumental lines.
const INSTRUMENTAL_GAP_MS: i32 = 5000;

/// `et`: instrumental lines for long gaps before, between and after the sung lines.
fn insert_instrumentals(lines: Vec<Line>, song_ms: i32) -> Vec<Line> {
    if lines.is_empty() {
        return lines;
    }
    let gap = |start: i32, dur: i32| Line { start, dur, instrumental: true, ..Default::default() };
    let mut out = Vec::with_capacity(lines.len() + 4);
    if lines[0].start > INSTRUMENTAL_GAP_MS {
        out.push(gap(0, lines[0].start));
    }
    let n = lines.len();
    let last_end = lines[n - 1].end();
    for (i, l) in lines.iter().enumerate() {
        out.push(l.clone());
        if i + 1 < n {
            let end = l.end();
            let f = lines[i + 1].start - end;
            if f > INSTRUMENTAL_GAP_MS {
                out.push(gap(end, f));
            }
        }
    }
    if song_ms - last_end > INSTRUMENTAL_GAP_MS {
        out.push(gap(last_end, song_ms - last_end));
    }
    out
}

// Credit lines ("作词 : …", "Written by: …") that some providers put in the lyrics.
const WRITER_TAGS: &[&str] = &["词", "詞", "作词", "作詞", "曲", "作曲", "词曲", "詞曲", "作词作曲", "作詞作曲", "writtenby", "lyricsby", "composedby", "lyricist", "composer"];
const CREDIT_TAGS: &[&str] = &[
    "编曲", "編曲", "和声", "和聲", "混音", "吉他", "制作人", "製作人", "演唱", "原唱", "翻唱", "后期", "後期", "和音", "录音", "錄音", "策划", "策劃", "伴奏", "美工", "海报", "海報", "旁白", "producedby",
    "arrangedby", "mixing", "mastering", "vocal", "vocals", "guitar", "bass", "drums", "producer", "arranger",
];
const CREDIT_SUFFIXES: &[char] = &['词', '詞', '曲', '声', '聲', '音'];

fn norm_tag(t: &str) -> String {
    t.to_lowercase().split_whitespace().collect()
}

fn is_credit_tag(t: &str) -> bool {
    WRITER_TAGS.contains(&t) || CREDIT_TAGS.contains(&t) || (t.chars().count() <= 4 && t.ends_with(CREDIT_SUFFIXES))
}

fn tag_pieces(n: &str) -> Vec<String> {
    let no_latin: String = n.chars().filter(|c| !c.is_ascii_lowercase()).collect();
    no_latin.split(['/', '&', '、', ',', '，', '・', '·']).filter(|s| !s.is_empty()).map(str::to_owned).collect()
}

/// `X`: a label that only names credits.
fn is_credit_label(t: &str) -> bool {
    let n = norm_tag(t);
    if WRITER_TAGS.contains(&n.as_str()) || CREDIT_TAGS.contains(&n.as_str()) {
        return true;
    }
    let pieces = tag_pieces(&n);
    !pieces.is_empty() && pieces.iter().all(|p| is_credit_tag(p))
}

fn is_writer_label(t: &str) -> bool {
    let n = norm_tag(t);
    WRITER_TAGS.contains(&n.as_str()) || (is_credit_label(t) && tag_pieces(&n).iter().any(|p| WRITER_TAGS.contains(&p.as_str())))
}

fn split_label(t: &str) -> Option<(&str, &str)> {
    let i = t.find([':', '：'])?;
    let sep = t[i..].chars().next()?.len_utf8();
    let (name, rest) = (&t[..i], t[i + sep..].trim_start());
    (!name.is_empty() && !rest.is_empty()).then_some((name, rest))
}

/// `yt`: a "Label: names" credit line rather than a lyric.
fn is_credit_line(t: &str) -> bool {
    split_label(t.trim()).is_some_and(|(name, _)| is_credit_label(name))
}

fn split_names(t: &str) -> Vec<String> {
    t.split(['/', '、', '，', ',']).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect()
}

/// `tt`: the writers named on a credit line.
fn writers_on(t: &str) -> Vec<String> {
    match split_label(t.trim()) {
        Some((name, rest)) if is_writer_label(name) => split_names(rest),
        _ => vec![],
    }
}

fn dedup(names: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in names {
        let n = n.trim().to_owned();
        if !n.is_empty() && !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

// ------------------------------------------------------------------- LRC

/// `V`: an LRC timestamp (`mm:ss.xx`, `h:mm:ss.xx` or seconds) in ms.
fn lrc_time(t: &str) -> i32 {
    let p: Vec<&str> = t.split(':').collect();
    let s = match p.len() {
        1 => p[0].parse::<f64>().unwrap_or(0.0) * 1000.0,
        2 => p[0].parse::<f64>().unwrap_or(0.0).trunc() * 60_000.0 + p[1].parse::<f64>().unwrap_or(0.0) * 1000.0,
        3 => p[0].parse::<f64>().unwrap_or(0.0).trunc() * 3_600_000.0 + p[1].parse::<f64>().unwrap_or(0.0).trunc() * 60_000.0 + p[2].parse::<f64>().unwrap_or(0.0) * 1000.0,
        _ => 0.0,
    };
    s.round() as i32
}

const LRC_META_TAGS: &[&str] = &["ti", "ar", "al", "au", "lr", "length", "by", "offset", "re", "tool", "ve", "#"];

/// `[mm:ss.xx]` tags: (byte range, time text).
fn line_stamps(s: &str) -> Vec<(std::ops::Range<usize>, &str)> {
    stamps(s, '[', ']')
}

/// `<mm:ss.xx>` word stamps.
fn word_stamps(s: &str) -> Vec<(std::ops::Range<usize>, &str)> {
    stamps(s, '<', '>')
}

fn stamps(s: &str, open: char, close: char) -> Vec<(std::ops::Range<usize>, &str)> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(o) = s[i..].find(open) {
        let o = i + o;
        let Some(c) = s[o + 1..].find(close) else { break };
        let inner = &s[o + 1..o + 1 + c];
        let is_time = inner.split_once(':').is_some_and(|(m, sec)| {
            !m.is_empty() && m.bytes().all(|b| b.is_ascii_digit()) && sec.split_once('.').is_some_and(|(a, b)| !a.is_empty() && !b.is_empty() && a.bytes().chain(b.bytes()).all(|b| b.is_ascii_digit()))
        });
        if is_time {
            out.push((o..o + c + 2, inner));
            i = o + c + 2;
        } else {
            i = o + open.len_utf8();
        }
    }
    out
}

/// Text and stamps alternating, the way JS `split` with a capture group returns them.
fn split_stamps(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut at = 0;
    for (r, t) in word_stamps(s) {
        out.push(&s[at..r.start]);
        out.push(t);
        at = r.end;
    }
    out.push(&s[at..]);
    out
}

fn remove_ranges(s: &str, ranges: &[(std::ops::Range<usize>, &str)]) -> String {
    let mut out = String::with_capacity(s.len());
    let mut at = 0;
    for (r, _) in ranges {
        out.push_str(&s[at..r.start]);
        at = r.end;
    }
    out.push_str(&s[at..]);
    out
}

/// `parseLRC`: line LRC, enhanced (word-by-word) LRC and `[bg: …]` background vocals.
pub fn parse_lrc(text: &str, song_ms: i32) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    let mut meta: HashMap<String, String> = HashMap::new();
    for raw in text.split('\n') {
        let a = raw.trim();
        if let Some(inner) = a.strip_prefix('[').and_then(|r| r.strip_suffix(']'))
            && let Some((tag, val)) = inner.split_once(':')
            && !tag.is_empty()
            && tag.chars().all(|c| c.is_alphanumeric() || c == '_')
            && LRC_META_TAGS.contains(&tag)
        {
            meta.insert(tag.to_owned(), val.to_owned());
            continue;
        }
        let ls = line_stamps(a);
        if ls.is_empty() {
            continue;
        }
        let times: Vec<i32> = ls.iter().map(|(_, t)| lrc_time(t)).collect();
        let (c, u) = (*times.iter().min().unwrap(), *times.iter().max().unwrap());
        let g = remove_ranges(a, &ls).trim().to_owned();
        // `[bg: …]` at the end of the line.
        let (main, bg) = match g.rfind("[bg:") {
            Some(i) if g[i..].trim_end().ends_with(']') => {
                let inner = g[i + 4..].trim_end();
                (g[..i].to_owned(), Some(inner[..inner.len() - 1].to_owned()))
            }
            _ => (g.clone(), None),
        };
        let o = split_stamps(&main);
        // Whitespace-only text between two stamps: the words are spaced by their own segments.
        let spaced = o.iter().enumerate().any(|(e, w)| e % 2 == 0 && e > 0 && e < o.len() - 1 && !w.is_empty() && w.trim().is_empty());
        let mut parts: Vec<Part> = Vec::new();
        let mut prev: Option<i32> = None;
        let mut b = String::new();
        let mut walk = |w: &str, is_bg: bool, parts: &mut Vec<Part>, b: &mut String| {
            for (m, seg) in split_stamps(w).into_iter().enumerate() {
                if m % 2 == 0 {
                    let mut d = seg.to_owned();
                    if spaced {
                        let t = d.trim();
                        d = if t.is_empty() && !d.is_empty() { " ".to_owned() } else { t.to_owned() };
                    }
                    b.push_str(&d);
                    if let Some(last) = parts.last_mut() {
                        last.text.push_str(&d);
                    }
                } else {
                    let d = lrc_time(seg);
                    if let (Some(p), Some(last)) = (prev, parts.last_mut()) {
                        last.dur = d - p;
                    }
                    parts.push(Part { start: d, dur: 0, text: String::new(), bg: is_bg });
                    prev = Some(d);
                }
            }
        };
        walk(&main, false, &mut parts, &mut b);
        if let Some(bg) = &bg {
            walk(bg, true, &mut parts, &mut b);
        }
        let plain: String = remove_ranges(&main, &word_stamps(&main));
        if is_credit_line(&plain) {
            continue;
        }
        lines.push(Line { start: c, dur: u - c, text: b.trim().to_owned(), parts, ..Default::default() });
    }
    let n = lines.len();
    for l in 0..n {
        if l + 1 < n {
            let next_start = lines[l + 1].start;
            let a = &mut lines[l];
            if a.dur == 0 {
                a.dur = (next_start - a.start).max(0);
            }
            if !a.parts.is_empty() {
                let mut c = next_start;
                for f in &a.parts {
                    c = c.max(f.start);
                }
                let last = a.parts.last_mut().unwrap();
                last.dur = (next_start - last.start).max(0);
                a.dur = (c - a.start).max(0);
            }
        } else {
            let a = &mut lines[l];
            let i = song_ms - a.start;
            if a.dur == 0 && i > 0 {
                a.dur = i;
            }
            if let Some(last) = a.parts.last_mut() {
                let u = song_ms - last.start;
                if u > 0 {
                    last.dur = u;
                }
            }
        }
    }
    if let Some(off) = meta.get("offset") {
        let off = (off.trim().parse::<f64>().unwrap_or(0.0) * 1000.0) as i32;
        for a in &mut lines {
            a.start -= off;
            for p in &mut a.parts {
                p.start -= off;
            }
        }
    }
    lines
}

/// `lrcFixers`: Musixmatch word-by-word quirks (spaces carrying a word's tail, and
/// files where most words were given a token 0–100 ms).
pub fn lrc_fixers(lines: &mut [Line]) {
    for e in lines.iter_mut() {
        for r in 1..e.parts.len() {
            let (l, o) = {
                let (a, b) = e.parts.split_at_mut(r);
                (&mut a[r - 1], &mut b[0])
            };
            if o.text == " " && l.text != " " {
                let a = o.dur - l.dur;
                if a.abs() <= 15 || o.dur <= 100 {
                    let i = o.dur;
                    l.dur += i;
                    o.dur -= i;
                    o.start += i;
                }
            }
        }
    }
    let (mut short, mut total) = (0, 0);
    for e in lines.iter() {
        if e.parts.is_empty() {
            continue;
        }
        for o in e.parts.iter().take(e.parts.len().saturating_sub(2)) {
            if o.text != " " {
                if o.dur <= 100 {
                    short += 1;
                }
                total += 1;
            }
        }
    }
    if total > 0 && short as f32 / total as f32 > 0.5 {
        for e in 0..lines.len() {
            for o in 0..lines[e].parts.len() {
                if lines[e].parts[o].text == " " || lines[e].parts[o].dur > 400 {
                    continue;
                }
                let next = if o + 1 < lines[e].parts.len() {
                    Some((e, o + 1))
                } else if e + 1 < lines.len() && !lines[e + 1].parts.is_empty() {
                    Some((e + 1, 0))
                } else {
                    None
                };
                match next {
                    None => lines[e].parts[o].dur = 300,
                    Some((ne, no)) => {
                        let a = lines[ne].parts[no].clone();
                        if a.text == " " {
                            lines[e].parts[o].dur += a.dur;
                            let a = &mut lines[ne].parts[no];
                            a.start += a.dur;
                            a.dur = 0;
                        } else {
                            lines[e].parts[o].dur = a.start - lines[e].parts[o].start;
                        }
                    }
                }
            }
        }
    }
}

fn lrc_songwriters(text: &str) -> Vec<String> {
    let mut n = Vec::new();
    for s in text.split('\n') {
        let e = s.trim();
        if let Some(inner) = e.strip_prefix('[').and_then(|r| r.strip_suffix(']'))
            && let Some((tag, val)) = inner.split_once(':')
            && (tag == "au" || tag == "lr")
        {
            n.extend(split_names(val));
            continue;
        }
        let ls = line_stamps(e);
        if !ls.is_empty() {
            let o = remove_ranges(e, &ls);
            let o = match o.rfind("[bg:") {
                Some(i) => o[..i].to_owned(),
                None => o,
            };
            n.extend(writers_on(&remove_ranges(&o, &word_stamps(&o))));
        }
    }
    dedup(n)
}

pub fn parse_plain(text: &str) -> Vec<Line> {
    text.split('\n').map(|s| Line { text: s.to_owned(), ..Default::default() }).collect()
}

/// Extract only Genius lyric containers, preserving explicit line and paragraph breaks.
fn parse_genius_html(html: &str) -> Vec<Line> {
    let doc = dom_query::Document::from(html);
    let modern = doc.select("div[data-lyrics-container='true']");
    let containers = if modern.is_empty() { doc.select("div.lyrics") } else { modern };
    // Selection and this traversal are both in document order. Track nesting in one
    // pass rather than searching every selected node's ancestors and all matches.
    let mut selected = containers.nodes().iter().peekable();
    let mut scan = vec![(doc.root(), false)];
    let mut raw = String::new();
    while let Some((container, nested)) = scan.pop() {
        let matched = selected.peek().is_some_and(|c| c.id == container.id);
        if matched {
            selected.next();
        }
        scan.extend(container.children_it(true).map(|child| (child, nested || matched)));
        if !matched || nested {
            continue;
        }
        if !raw.is_empty() && !raw.ends_with('\n') {
            raw.push('\n');
        }
        let mut stack = vec![(container, false)];
        while let Some((node, closing)) = stack.pop() {
            if closing {
                if !raw.is_empty() && !raw.ends_with('\n') {
                    raw.push('\n');
                }
                continue;
            }
            if node.is_text() {
                raw.push_str(&node.text());
                continue;
            }
            let name = node.node_name();
            let name = name.as_deref().unwrap_or_default();
            let excluded_class = node.class().is_some_and(|classes| classes.split_ascii_whitespace().any(|c| {
                ["LyricsHeader__Container", "SongBioPreview__Container", "InreadAd__Container"].iter().any(|prefix| c.starts_with(prefix))
            }));
            if matches!(name, "script" | "style" | "button" | "noscript") || excluded_class
                || node.attr("data-exclude-from-selection").is_some_and(|v| v.eq_ignore_ascii_case("true"))
            {
                continue;
            }
            if name == "br" {
                raw.push('\n');
            } else {
                if name == "p" {
                    if !raw.is_empty() && !raw.ends_with('\n') {
                        raw.push('\n');
                    }
                    stack.push((node, true));
                }
                stack.extend(node.children_it(true).map(|child| (child, false)));
            }
        }
    }
    const MARKER: &[u8] = b"you might also like";
    let mut cleaned = String::with_capacity(raw.len());
    for ch in raw.chars() {
        cleaned.push(match ch {
            '\u{00a0}' | '\u{200b}' | '\u{feff}' => ' ',
            _ => ch,
        });
        // The marker cannot overlap itself (its only 'y' is the first byte).
        // Removing a suffix as it arrives also handles markers formed by a removal.
        if cleaned.len() >= MARKER.len() && cleaned.as_bytes()[cleaned.len() - MARKER.len()..].eq_ignore_ascii_case(MARKER) {
            let mut start = cleaned.len() - MARKER.len();
            while start > 0 && cleaned.as_bytes()[start - 1].is_ascii_digit() {
                start -= 1;
            }
            cleaned.truncate(start);
        }
    }
    let mut lines = Vec::new();
    for line in cleaned.lines() {
        let mut line = line.trim();
        if line.to_ascii_lowercase().ends_with("embed") {
            let mut end = line.len() - "embed".len();
            while end > 0 && line.as_bytes()[end - 1].is_ascii_digit() {
                end -= 1;
            }
            line = line[..end].trim_end();
        }
        if line.is_empty() {
            if !lines.is_empty() && !lines.last().is_some_and(|s: &String| s.is_empty()) {
                lines.push(String::new());
            }
        } else {
            lines.push(line.to_owned());
        }
    }
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    if lines.is_empty() { vec![] } else { parse_plain(&lines.join("\n")) }
}

/// Only description/bio containers are eligible; a lyrics container is never a fallback.
fn parse_genius_about(html: &str) -> Option<String> {
    const MAX_CHARS: usize = 900;
    let doc = dom_query::Document::from(html);
    let selections = [
        doc.select("div[data-testid='song-description']"),
        doc.select("[class*='SongBioPreview-desktop__Container'], [class*='SongBioPreview__Container']"),
    ];
    for selection in selections {
        for container in selection.nodes() {
            let mut paragraphs = Vec::new();
            let mut current = String::new();
            let mut stack = vec![(*container, false)];
            while let Some((node, closing)) = stack.pop() {
                if closing {
                    genius_about_flush(&mut current, &mut paragraphs);
                    continue;
                }
                if node.is_text() {
                    current.push_str(&node.text());
                    continue;
                }
                let name = node.node_name();
                let name = name.as_deref().unwrap_or_default();
                let excluded_class = node.class().is_some_and(|classes| classes.split_ascii_whitespace().any(|class| {
                    class == "lyrics" || class.starts_with("Lyrics__") || class.starts_with("LyricsHeader__") || class.starts_with("InreadAd__")
                }));
                if matches!(name, "script" | "style" | "noscript" | "button" | "svg") || excluded_class
                    || node.attr("data-lyrics-container").is_some_and(|v| v.eq_ignore_ascii_case("true"))
                    || node.attr("data-exclude-from-selection").is_some_and(|v| v.eq_ignore_ascii_case("true"))
                    || node.attr("aria-hidden").is_some_and(|v| v.eq_ignore_ascii_case("true"))
                    || (name == "a" && node.text().trim().eq_ignore_ascii_case("read more"))
                {
                    continue;
                }
                if name == "br" {
                    current.push(' ');
                } else {
                    if matches!(name, "p" | "div" | "section" | "blockquote" | "li") {
                        genius_about_flush(&mut current, &mut paragraphs);
                        stack.push((node, true));
                    }
                    stack.extend(node.children_it(true).map(|child| (child, false)));
                }
            }
            genius_about_flush(&mut current, &mut paragraphs);
            let text = paragraphs.join("\n\n");
            if text.is_empty() {
                continue;
            }
            if text.chars().count() <= MAX_CHARS {
                return Some(text);
            }
            // Include the ellipsis in the limit and prefer a nearby word boundary.
            let end = text.char_indices().nth(MAX_CHARS - 1).map_or(text.len(), |(i, _)| i);
            let prefix = &text[..end];
            let end = prefix.rfind(char::is_whitespace).filter(|i| prefix[..*i].chars().count() >= MAX_CHARS / 2).unwrap_or(prefix.len());
            return Some(format!("{}…", prefix[..end].trim_end()));
        }
    }
    None
}

fn genius_about_flush(current: &mut String, paragraphs: &mut Vec<String>) {
    let cleaned = current.replace(['\u{200b}', '\u{feff}'], " ");
    let text = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if !text.is_empty() {
        paragraphs.push(text);
    }
    current.clear();
}

// ------------------------------------------------------------------- QRC

struct QrcLine {
    time: i32,
    duration: i32,
    text: String,
    syllables: Vec<(String, i32, i32)>,
    agent: Option<String>,
}

struct Singers {
    aliases: HashMap<String, String>,
    next: u32,
    current: Option<String>,
}

fn qrc_content(t: &str) -> String {
    let Some(i) = t.find("LyricContent=\"") else { return t.to_owned() };
    let rest = &t[i + 14..];
    // Up to the closing quote that ends the attribute (`"/>`, `">` or `" name=`).
    let mut end = None;
    for (j, _) in rest.match_indices('"') {
        let after = rest[j + 1..].trim_start();
        if after.starts_with("/>") || after.starts_with('>') || after.split_once('=').is_some_and(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphabetic())) {
            end = Some(j);
            break;
        }
    }
    let Some(end) = end else { return t.to_owned() };
    rest[..end].replace("&quot;", "\"").replace("&amp;", "&")
}

/// `[start,duration]` at the start of a QRC line.
fn qrc_header(t: &str) -> Option<(i32, i32, &str)> {
    let inner = t.strip_prefix('[')?;
    let close = inner.find(']')?;
    let (a, b) = inner[..close].split_once(',')?;
    Some((a.parse().ok()?, b.parse().ok()?, &inner[close + 1..]))
}

/// `lt`: `text(start,duration)` syllables.
fn qrc_syllables(t: &str) -> Vec<(String, i32, i32)> {
    let mut out = Vec::new();
    let mut s = 0;
    let mut i = 0;
    while let Some(o) = t[i..].find('(') {
        let o = i + o;
        let parsed = t[o + 1..].find(')').and_then(|c| {
            let (a, b) = t[o + 1..o + 1 + c].split_once(',')?;
            let ok = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
            (ok(a) && ok(b)).then(|| (a.parse::<i32>().ok(), b.parse::<i32>().ok(), o + 2 + c))
        });
        match parsed {
            Some((Some(a), Some(b), end)) => {
                out.push((t[s..o].to_owned(), a, b));
                s = end;
                i = end;
            }
            _ => i = o + 1,
        }
    }
    out
}

fn assign_singer(name: &str, line: &mut QrcLine, st: &mut Singers) {
    if !st.aliases.contains_key(name) {
        let up = name.to_uppercase();
        let group = up == "合" || up == "ALL" || up == "合唱";
        let id = if group {
            "v1000".to_owned()
        } else {
            let id = format!("v{}", st.next);
            st.next += 1;
            id
        };
        st.aliases.insert(name.to_owned(), id);
    }
    line.agent = st.aliases.get(name).cloned();
}

fn retime(line: &mut QrcLine) {
    if let (Some(f), Some(l)) = (line.syllables.first(), line.syllables.last()) {
        line.time = f.1;
        line.duration = l.1 + l.2 - f.1;
    }
}

/// `jt`: Sørensen–Dice similarity of character bigrams.
fn similarity(a: &str, b: &str) -> f32 {
    let (a, b): (Vec<char>, Vec<char>) = (a.to_lowercase().chars().collect(), b.to_lowercase().chars().collect());
    if a.len() < 2 || b.len() < 2 {
        return 0.0;
    }
    let mut map: HashMap<(char, char), i32> = HashMap::new();
    for w in a.windows(2) {
        *map.entry((w[0], w[1])).or_default() += 1;
    }
    let mut hits = 0;
    for w in b.windows(2) {
        if let Some(c) = map.get_mut(&(w[0], w[1]))
            && *c > 0
        {
            *c -= 1;
            hits += 1;
        }
    }
    hits as f32 * 2.0 / (a.len() + b.len() - 2) as f32
}

/// `Bt`: keep a QRC line? Drops title/credit lines and pulls "Singer: …" prefixes into agents.
fn keep_qrc_line(t: &mut QrcLine, st: &mut Singers, early: bool, meta: Option<(&str, &str)>) -> bool {
    if t.syllables.is_empty() {
        if let Some(c) = &st.current {
            t.agent = Some(c.clone());
        }
        return true;
    }
    if early {
        if let Some((title, artist)) = meta
            && (!title.is_empty() || !artist.is_empty())
        {
            let b = |p: &str| p.to_lowercase().chars().filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || ('\u{3000}'..='\u{9fff}').contains(c) || ('\u{ac00}'..='\u{d7af}').contains(c)).collect::<String>();
            let (h, tt, r) = (b(&t.text), b(title), b(artist));
            if (!tt.is_empty() && h.contains(&tt) && !r.is_empty() && h.contains(&r))
                || (!tt.is_empty() && h.contains(&tt) && r.is_empty() && h.chars().count() < tt.chars().count() + 15)
                || (!r.is_empty() && h == r)
            {
                return false;
            }
            if similarity(&t.text, &format!("{title} {artist}")) > 0.5 {
                return false;
            }
        }
        if t.syllables.len() > 2 && t.syllables.iter().all(|s| (s.2 - t.syllables[0].2).abs() < 10) {
            return false;
        }
    }
    if let Some(c) = &st.current {
        t.agent = Some(c.clone());
    }
    let mut r = String::new();
    let mut o = 0;
    for p in &t.syllables {
        r.push_str(&p.0);
        o += 1;
        if r.contains(':') || r.contains('：') {
            break;
        }
        if r.chars().count() > 40 {
            return true;
        }
    }
    let colon = r.find([':', '：']);
    let Some(ci) = colon else { return true };
    let name = r[..ci].trim().to_owned();
    let after = &r[ci + r[ci..].chars().next().unwrap().len_utf8()..];
    if name.is_empty() {
        return true;
    }
    if after.trim().is_empty() {
        // "Singer:" on its own.
        if is_credit_label(&name) {
            return false;
        }
        if name.chars().count() <= 30 {
            t.syllables.drain(..o);
            t.text = t.text.get(r.len()..).unwrap_or_default().to_owned();
            assign_singer(&name, t, st);
            st.current = st.aliases.get(&name).cloned();
            retime(t);
        }
        return true;
    }
    if is_credit_label(&name) {
        return false;
    }
    if name.chars().count() > 20 {
        return true;
    }
    let rest = after.trim_start().to_owned();
    let u = o - 1;
    let tail = t.syllables[u].0.find([':', '：']).map(|i| t.syllables[u].0[i..].chars().skip(1).collect::<String>().trim_start().to_owned()).unwrap_or_default();
    if !tail.is_empty() {
        t.syllables[u].0 = tail;
        t.syllables.drain(..u);
    } else {
        t.syllables.drain(..o);
    }
    assign_singer(&name, t, st);
    st.current = st.aliases.get(&name).cloned();
    t.text = rest;
    retime(t);
    true
}

/// `parseQRC`: QQ Music's syllable format.
pub fn parse_qrc(text: &str, song_ms: i32, meta: Option<(&str, &str)>) -> Vec<Line> {
    if text.is_empty() {
        return vec![];
    }
    let content = qrc_content(text);
    let mut r: Vec<QrcLine> = Vec::new();
    let mut st = Singers { aliases: HashMap::new(), next: 1, current: None };
    for a in content.split('\n') {
        let i = a.trim();
        if i.is_empty() || (i.starts_with('[') && i[1..].find(':').is_some_and(|c| c > 0 && i[1..1 + c].chars().all(|ch| ch.is_ascii_alphabetic()))) {
            continue;
        }
        let Some((time, duration, rest)) = qrc_header(i) else { continue };
        let syllables = qrc_syllables(rest);
        let text = syllables.iter().map(|s| s.0.as_str()).collect::<String>();
        let mut g = QrcLine { time, duration, text, syllables, agent: None };
        let early = r.len() < 5;
        if keep_qrc_line(&mut g, &mut st, early, meta) {
            r.push(g);
        }
    }
    if r.is_empty() {
        return vec![];
    }
    let n = r.len();
    let lines = (0..n)
        .map(|i| {
            let a = &r[i];
            let mut parts: Vec<Part> = a.syllables.iter().map(|(t, s, d)| Part { start: *s, dur: *d, text: t.clone(), bg: false }).collect();
            let mut u = a.duration;
            if u == 0 && i + 1 < n {
                u = (r[i + 1].time - a.time).max(0);
            } else if u == 0 {
                u = song_ms - a.time;
            }
            if let Some(f) = parts.last_mut()
                && f.dur == 0
            {
                f.dur = if i + 1 < n { (r[i + 1].time - f.start).max(0) } else { song_ms - f.start };
            }
            Line { start: a.time, dur: u, text: a.text.clone(), parts, agent: a.agent.clone(), ..Default::default() }
        })
        .collect();
    insert_instrumentals(lines, song_ms)
}

fn qrc_songwriters(text: &str) -> Vec<String> {
    let mut n = Vec::new();
    for s in qrc_content(text).split('\n') {
        if let Some((_, _, rest)) = qrc_header(s.trim()) {
            let t: String = qrc_syllables(rest).into_iter().map(|s| s.0).collect();
            n.extend(writers_on(&t));
        }
    }
    dedup(n)
}

// ------------------------------------------------------------------- TTML

pub struct Ttml {
    pub lines: Vec<Line>,
    pub word_synced: bool,
    pub language: Option<String>,
    pub songwriters: Vec<String>,
}

/// `C` (parseTTMLTime): `1.5s` / `2m` / `1h` / `250ms`, or `h:m:s.fff`.
pub fn ttml_time(t: &str) -> i32 {
    let t = t.trim();
    if t.is_empty() {
        return 0;
    }
    for (suffix, mul) in [("ms", 1.0), ("h", 3_600_000.0), ("m", 60_000.0), ("s", 1000.0)] {
        if let Some(n) = t.strip_suffix(suffix)
            && !n.is_empty()
            && n.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        {
            return (n.parse::<f64>().unwrap_or(0.0) * mul).round() as i32;
        }
    }
    let s: Vec<f64> = t.split(':').map(|p| p.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect::<String>().parse::<f64>().unwrap_or(0.0)).collect();
    let ms = match s.len() {
        1 => s[0] * 1000.0,
        2 => s[0].trunc() * 60_000.0 + s[1] * 1000.0,
        3 => s[0].trunc() * 3_600_000.0 + s[1].trunc() * 60_000.0 + s[2] * 1000.0,
        _ => 0.0,
    };
    ms.round() as i32
}

/// `At`: declare prefixes the file uses without binding (an XML parser refuses them).
fn bind_prefixes(t: &str) -> String {
    let Some(start) = t.find("<tt") else { return t.to_owned() };
    let Some(len) = t[start..].find('>') else { return t.to_owned() };
    let tag = &t[start..start + len + 1];
    let mut declared: Vec<String> = vec!["xml".into(), "xmlns".into()];
    for (i, _) in tag.match_indices("xmlns:") {
        let name: String = tag[i + 6..].chars().take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-')).collect();
        declared.push(name);
    }
    let mut used: Vec<String> = Vec::new();
    let bytes: Vec<char> = t.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        // `<prefix:` / `</prefix:` and ` prefix:attr=`
        let at_tag = bytes[i] == '<' || (bytes[i] == '/' && i > 0 && bytes[i - 1] == '<');
        let at_attr = bytes[i].is_whitespace();
        if at_tag || at_attr {
            let mut j = i + 1;
            let mut name = String::new();
            while j < bytes.len() && (bytes[j].is_alphanumeric() || matches!(bytes[j], '_' | '.' | '-')) {
                name.push(bytes[j]);
                j += 1;
            }
            if !name.is_empty() && name.starts_with(|c: char| c.is_ascii_alphabetic()) && j < bytes.len() && bytes[j] == ':' {
                let ok = if at_attr {
                    let mut k = j + 1;
                    while k < bytes.len() && (bytes[k].is_alphanumeric() || matches!(bytes[k], '_' | '.' | '-')) {
                        k += 1;
                    }
                    while k < bytes.len() && bytes[k].is_whitespace() {
                        k += 1;
                    }
                    k < bytes.len() && bytes[k] == '='
                } else {
                    true
                };
                if ok && !used.contains(&name) {
                    used.push(name);
                }
            }
        }
        i += 1;
    }
    let missing: Vec<&String> = used.iter().filter(|u| !declared.contains(u)).collect();
    if missing.is_empty() {
        return t.to_owned();
    }
    let extra: String = missing.iter().map(|m| format!(" xmlns:{m}=\"urn:braccato:unbound:{m}\"")).collect();
    let new_tag = format!("{}{}>", &tag[..tag.len() - 1], extra);
    t.replacen(tag, &new_tag, 1)
}

fn attr<'a>(n: roxmltree::Node<'a, '_>, name: &str) -> Option<&'a str> {
    n.attributes().find(|a| a.name() == name).map(|a| a.value())
}

/// `D`: text of a node and its spans.
fn span_text(n: roxmltree::Node) -> String {
    let mut s = String::new();
    for c in n.children() {
        if let Some(t) = c.text().filter(|_| c.is_text()) {
            s.push_str(t);
        } else if c.is_element() && c.tag_name().name() == "span" {
            s.push_str(&span_text(c));
        }
    }
    s
}

/// `U`: a paragraph's (or transliteration's) children as parts and text.
fn ttml_parts(node: roxmltree::Node, line_start: i32) -> (Vec<Part>, String, bool) {
    let mut s = String::new();
    let mut e: Vec<Part> = Vec::new();
    let mut timed = false;
    for u in node.children() {
        let bg = u.is_element() && attr(u, "role") == Some("x-bg");
        let group: Vec<roxmltree::Node> = if bg { u.children().collect() } else { vec![u] };
        for m in group {
            if m.is_text() {
                let t = m.text().unwrap_or_default();
                if t.is_empty() {
                    continue;
                }
                s.push_str(t);
                let start = e.last().map_or(line_start, |p| p.start + p.dur);
                e.push(Part { start, dur: 0, text: t.to_owned(), bg });
            } else if m.is_element() && m.tag_name().name() == "span" {
                let p = span_text(m);
                if p.is_empty() {
                    continue;
                }
                let b = ttml_time(attr(m, "begin").unwrap_or_default());
                let h = ttml_time(attr(m, "end").unwrap_or_default());
                e.push(Part { start: b, dur: h - b, text: p.clone(), bg });
                s.push_str(&p);
                timed = true;
            }
        }
    }
    if !timed {
        e.clear();
    }
    (e, s, timed)
}

/// `parseTTMLContent`: Apple-style TTML with agents, background vocals, and the
/// translations and transliterations in its iTunes metadata.
pub fn parse_ttml(text: &str, song_ms: i32) -> Ttml {
    let empty = |language| Ttml { lines: vec![], word_synced: false, language, songwriters: vec![] };
    let src = bind_prefixes(&text.replace("\\\"", "\""));
    let Ok(doc) = roxmltree::Document::parse_with_options(&src, roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() }) else {
        return empty(None);
    };
    let tt = doc.root_element();
    if tt.tag_name().name() != "tt" {
        return empty(None);
    }
    let body = tt.children().find(|n| n.tag_name().name() == "body");
    let language = attr(tt, "lang").or_else(|| body.and_then(|b| attr(b, "lang"))).map(str::to_owned);
    let Some(body) = body else { return empty(language) };
    let head_meta = tt.children().find(|n| n.tag_name().name() == "head").and_then(|h| h.children().find(|n| n.tag_name().name() == "metadata"));
    // Agents: people and characters are v1, v2, …; anything else sings for everyone.
    let mut agents: HashMap<String, String> = HashMap::new();
    if let Some(m) = head_meta {
        let mut k = 0;
        for a in m.children().filter(|n| n.tag_name().name() == "agent") {
            let Some(id) = attr(a, "id") else { continue };
            let kind = attr(a, "type").unwrap_or_default();
            if kind == "person" || kind == "character" {
                k += 1;
                agents.insert(id.to_owned(), format!("v{k}"));
            } else {
                agents.insert(id.to_owned(), "v1000".to_owned());
            }
        }
    }
    let ps: Vec<roxmltree::Node> = body.children().filter(|n| n.tag_name().name() == "div").flat_map(|d| d.children().filter(|n| n.tag_name().name() == "p")).collect();
    if ps.is_empty() {
        return empty(language);
    }
    let mut lines: Vec<Line> = Vec::new();
    let mut keys: HashMap<String, Vec<usize>> = HashMap::new();
    let mut word_synced = false;
    for p in ps {
        let Some(begin) = attr(p, "begin") else { continue };
        let start = ttml_time(begin);
        let end = ttml_time(attr(p, "end").unwrap_or_default());
        let (parts, text, timed) = ttml_parts(p, start);
        word_synced |= timed;
        let agent = attr(p, "agent").map(|a| agents.get(a).cloned().unwrap_or_else(|| a.to_owned()));
        if let Some(k) = attr(p, "key") {
            keys.entry(k.to_owned()).or_default().push(lines.len());
        }
        lines.push(Line { start, dur: end - start, text, parts, agent, ..Default::default() });
    }
    // Translations and transliterations, matched to lines by key.
    let meta_root = head_meta.and_then(|m| m.descendants().find(|n| n.tag_name().name() == "iTunesMetadata")).or(head_meta);
    if let Some(m) = meta_root {
        if let Some(ts) = m.descendants().find(|n| n.tag_name().name() == "translations") {
            let default_lang = attr(ts, "lang");
            for t in ts.children().filter(|n| n.tag_name().name() == "translation") {
                let lang = attr(t, "lang").or(default_lang);
                for item in t.children().filter(|n| n.is_element()) {
                    let Some(key) = attr(item, "for").or_else(|| attr(t, "for")) else { continue };
                    let txt = span_text(item);
                    let (Some(lang), false) = (lang, txt.is_empty()) else { continue };
                    for &i in keys.get(key).map(Vec::as_slice).unwrap_or_default() {
                        lines[i].translations.push((lang.to_owned(), txt.clone()));
                    }
                }
            }
        }
        if let Some(ts) = m.descendants().find(|n| n.tag_name().name() == "transliterations") {
            for t in ts.children().filter(|n| n.tag_name().name() == "transliteration") {
                for item in t.children().filter(|n| n.is_element()) {
                    let Some(key) = attr(item, "for") else { continue };
                    for &i in keys.get(key).map(Vec::as_slice).unwrap_or_default() {
                        let (parts, text, _) = ttml_parts(item, lines[i].start);
                        if text.is_empty() {
                            continue;
                        }
                        lines[i].romanization = Some(text);
                        lines[i].timed_roman = parts;
                    }
                }
            }
        }
    }
    let song = attr(body, "dur").map(ttml_time).unwrap_or(song_ms);
    let songwriters = {
        let mut n = Vec::new();
        if let Some(m) = head_meta {
            if let Some(sw) = m.descendants().find(|n| n.tag_name().name() == "songwriters") {
                for w in sw.children().filter(|n| n.tag_name().name() == "songwriter") {
                    n.push(span_text(w));
                }
            }
            for meta in m.children().filter(|n| n.tag_name().name() == "meta") {
                if matches!(attr(meta, "key"), Some("songwriter" | "songwriters"))
                    && let Some(v) = attr(meta, "value")
                {
                    n.push(v.to_owned());
                }
            }
        }
        dedup(n)
    };
    Ttml { lines: insert_instrumentals(lines, song), word_synced, language, songwriters }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genius_provider_is_last_and_unsynced() {
        assert_eq!(Provider::ALL.last(), Some(&Provider::GeniusPlain));
        assert_eq!(Provider::from_key("genius-plain"), Some(Provider::GeniusPlain));
        assert_eq!(Provider::GeniusPlain.name(), "Genius");
        assert_eq!(Provider::GeniusPlain.sync(), SyncKind::Unsynced);
        let lyrics = Lyrics::new(Provider::GeniusPlain, parse_plain("One\nTwo"), None, vec![]).unwrap();
        assert_eq!(lyrics.sync, Sync::None);
    }

    #[test]
    fn genius_fetch_keeps_the_combined_future_send() {
        fn assert_send<T: Send>(_: T) {}
        let client = Client::new(reqwest::Client::new(), &std::env::temp_dir());
        let q = Query { video_id: "id", song: "Song", artist: "Artist", album: None, duration: None };
        let ticket = client.begin_fetch("id", 1);
        assert_send(client.fetch(&q, &ticket, true, |_| {}));
    }

    fn test_lyrics(provider: Provider) -> Lyrics {
        Lyrics::new(provider, parse_plain("line"), None, vec![]).unwrap()
    }

    #[tokio::test]
    async fn fast_genius_publishes_while_stream_is_pending() {
        let (release, wait) = tokio::sync::oneshot::channel::<()>();
        let (send_partial, mut partial) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            gather_lyrics(
                async move { wait.await.unwrap(); Ok(vec![test_lyrics(Provider::LrclibPlain)]) },
                async { Ok(vec![]) }, async { Ok(vec![]) },
                async { Ok(vec![test_lyrics(Provider::GeniusPlain)]) },
                |set| { let _ = send_partial.send(set); },
            ).await
        });
        let first = tokio::time::timeout(Duration::from_secs(1), partial.recv()).await.unwrap().unwrap();
        assert_eq!(first.sources[0].provider, Provider::GeniusPlain);
        assert!(!task.is_finished());
        release.send(()).unwrap();
        let (final_set, ok) = task.await.unwrap();
        assert!(ok);
        assert_eq!(final_set.sources.iter().map(|l| l.provider).collect::<Vec<_>>(), vec![Provider::LrclibPlain, Provider::GeniusPlain]);
    }

    #[tokio::test]
    async fn fast_other_provider_publishes_while_genius_is_pending() {
        let (release, wait) = tokio::sync::oneshot::channel::<()>();
        let (send_partial, mut partial) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            gather_lyrics(
                async { Ok(vec![test_lyrics(Provider::LrclibPlain)]) },
                async { Ok(vec![]) }, async { Ok(vec![]) },
                async move { wait.await.unwrap(); Err(anyhow::anyhow!("failure")) },
                |set| { let _ = send_partial.send(set); },
            ).await
        });
        let first = tokio::time::timeout(Duration::from_secs(1), partial.recv()).await.unwrap().unwrap();
        assert_eq!(first.sources[0].provider, Provider::LrclibPlain);
        release.send(()).unwrap();
        let (final_set, ok) = task.await.unwrap();
        assert!(!ok);
        assert_eq!(final_set.sources[0].provider, Provider::LrclibPlain);
    }

    #[test]
    fn newer_final_cannot_be_replaced_by_an_older_request() {
        let client = Client::new(reqwest::Client::new(), &std::env::temp_dir());
        let old = client.begin_fetch("same", 1);
        assert!(old.is_current());
        let new = client.begin_fetch("same", 2);
        assert!(!old.is_current());
        assert!(new.is_current());
        let old_set = LyricsSet { sources: vec![test_lyrics(Provider::LrclibPlain)] };
        let new_set = LyricsSet { sources: vec![test_lyrics(Provider::GeniusPlain)] };
        old.cache_final(true, &old_set);
        assert!(old.cached(true).is_none());
        new.cache_final(true, &new_set);
        new.cache_final(true, &new_set);
        drop(new);
        old.cache_final(true, &old_set);
        assert_eq!(old.cached(true).unwrap().sources[0].provider, Provider::GeniusPlain);
        assert_eq!(client.recent.lock().unwrap().entries.len(), 1);
        // Even eviction cannot allow the still-running old request to restore stale data.
        for i in 0..RECENT {
            let ticket = client.begin_fetch(&format!("other{i}"), 3 + i as u64);
            ticket.cache_final(true, &new_set);
        }
        old.cache_final(true, &old_set);
        assert!(old.cached(true).is_none());
        drop(old);
        assert!(client.recent.lock().unwrap().pending.is_empty());
    }

    #[test]
    fn empty_newer_final_still_blocks_older_cache_write() {
        let client = Client::new(reqwest::Client::new(), &std::env::temp_dir());
        let old = client.begin_fetch("same", 1);
        let new = client.begin_fetch("same", 2);
        new.cache_final(true, &LyricsSet::default());
        drop(new);
        old.cache_final(true, &LyricsSet { sources: vec![test_lyrics(Provider::LrclibPlain)] });
        assert!(old.cached(true).is_none());
    }

    #[tokio::test]
    async fn cached_final_skips_interim_and_network() {
        let client = Client::new(reqwest::Client::new(), &std::env::temp_dir());
        let first = client.begin_fetch("id", 1);
        first.cache_final(true, &LyricsSet { sources: vec![test_lyrics(Provider::GeniusPlain)] });
        assert!(first.cached(false).is_none());
        drop(first);
        let ticket = client.begin_fetch("id", 2);
        let q = Query { video_id: "id", song: "Song", artist: "Artist", album: None, duration: None };
        let mut published = false;
        let result = client.fetch(&q, &ticket, true, |_| published = true).await.unwrap();
        assert!(!published);
        assert_eq!(result.sources[0].provider, Provider::GeniusPlain);
    }

    #[test]
    fn genius_page_drops_query_and_fragment() {
        let url = genius_page_url("https://genius.com/a-song?token=secret#fragment").unwrap();
        assert_eq!(url.as_str(), "https://genius.com/a-song");
        assert!(genius_page_url("https://genius.com.evil.example/song?token=secret").is_err());
    }

    #[test]
    fn genius_search_attempts_clean_and_keep_matching_context() {
        let attempts = genius_search_attempts("Björk - Jóga (Official Video) | Live", "Björk");
        assert_eq!(attempts[0].query, "Björk Jóga");
        assert_eq!(attempts[0].title, "Jóga");
        assert_eq!(attempts[0].artist, "Björk");
        assert_eq!(genius_search_attempts("Björk — Jóga", "Björk")[0].query, "Björk Jóga");
        assert_eq!(genius_search_attempts("Jóga: Björk", "Björk")[0].query, "Björk Jóga");
        let attempts = genius_search_attempts("A Song (Part II)", "Artist");
        assert!(attempts.iter().any(|a| a.query == "Artist A Song" && a.title == "A Song (Part II)"));
        assert!(genius_search_attempts("   ", "Artist").is_empty());
        assert_eq!(genius_clean_query("İ song Official Video"), "İ song");
    }

    #[test]
    fn genius_search_requires_relevant_title_artist_and_safe_url() {
        let response = serde_json::json!({"response": {"sections": [
            {"type": "artist", "hits": [{"result": {"title": "Jóga", "artist_names": "Björk", "url": "https://genius.com/wrong"}}]},
            {"type": "song", "hits": [
                {"result": {"title": "Jóga", "artist_names": "Someone Else", "url": "https://genius.com/wrong-artist"}},
                {"result": {"title": "Unrelated", "artist_names": "Björk", "url": "https://genius.com/wrong-title"}},
                {"result": {"title": "Jóga", "artist_names": "Björk", "path": "/joga-translation", "url": "https://genius.com/translation"}},
                {"result": {"title": "Jóga", "artist_names": "Björk", "url": "https://genius.com.evil.example/song"}},
                {"result": {"title": "Jóga", "artist_names": "Björk", "url": "https://genius.com@evil.example/song"}},
                {"result": {"title": "Jóga", "artist_names": "Björk & Guest", "url": "https://genius.com/partial"}},
                {"result": {"title": "Jóga", "artist_names": "BJÖRK", "url": "https://genius.com/bjork-joga-lyrics"}}
            ]}
        ]}});
        assert_eq!(best_genius_url(&response, "Jóga", "Björk").as_deref(), Some("https://genius.com/bjork-joga-lyrics"));
        assert_eq!(best_genius_url(&response, "Jóga", "Another Artist"), None);
        assert_eq!(best_genius_url(&response, "", "Björk"), None);
        assert_eq!(best_genius_url(&serde_json::json!({"response": {"sections": []}}), "Jóga", "Björk"), None);
        let empty_fields = serde_json::json!({"response": {"sections": [{"type": "song", "hits": [
            {"result": {"title": "", "artist_names": "Björk", "url": "https://genius.com/x"}},
            {"result": {"title": "Jóga", "artist_names": "", "url": "https://genius.com/y"}},
            {"result": {"title": "Jóga", "primary_artist": {"name": "Björk"}, "url": "http://genius.com/z"}}
        ]}]}});
        assert_eq!(best_genius_url(&empty_fields, "Jóga", "Björk"), None);
    }

    #[test]
    fn genius_search_avoids_partial_words_and_unrequested_variants() {
        let response = serde_json::json!({"response": {"sections": [{"type": "song", "hits": [
            {"result": {"title": "Cat", "artist_names": "The Artists", "url": "https://genius.com/cat"}},
            {"result": {"title": "Cats", "artist_names": "The Artists", "url": "https://genius.com/cats"}},
            {"result": {"title": "Cats", "artist_names": "The Artists", "path": "/cats-instrumental", "url": "https://genius.com/instrumental"}}
        ]}]}});
        assert_eq!(best_genius_url(&response, "Cats", "Artists").as_deref(), Some("https://genius.com/cats"));
        assert_eq!(best_genius_url(&response, "Cats", "Artist"), None);
        let variant = serde_json::json!({"response": {"sections": [{"type": "song", "hits": [
            {"result": {"title": "Cats", "artist_names": "The Artists", "path": "/cats-instrumental", "url": "https://genius.com/instrumental"}}
        ]}]}});
        assert_eq!(best_genius_url(&variant, "Cats", "The Artists"), None);
        assert_eq!(best_genius_url(&response, "Cats (Remix)", "The Artists"), None);
        let generic_artist = serde_json::json!({"response": {"sections": [{"type": "song", "hits": [
            {"result": {"title": "Cats", "artist_names": "The", "url": "https://genius.com/wrong"}}
        ]}]}});
        assert_eq!(best_genius_url(&generic_artist, "Cats", "The Artists"), None);
    }

    #[test]
    fn genius_about_parses_only_description_paragraphs() {
        let html = r#"<div data-lyrics-container="true">Not a meaning</div>
            <div data-testid="song-description">
              <p> A&nbsp;<a href="/x">song &amp; <b>story</b></a>.<br>More text.</p>
              <p>Second&#39;s <em>paragraph</em>.</p>
              <button>Read More</button><a href="/x">Read More</a>
              <script>junk</script><span aria-hidden="true">hidden</span>
              <div data-lyrics-container="true">[Verse]<br>Not the description</div>
              <div class="lyrics">Not legacy lyrics either</div>
            </div><div class="SongBioPreview-desktop__Container-sc-x">Wrong fallback</div>"#;
        assert_eq!(parse_genius_about(html).as_deref(), Some("A song & story. More text.\n\nSecond's paragraph."));
    }

    #[test]
    fn genius_about_fallback_and_missing_description() {
        let preview = "<div data-testid='song-description'><button>Read more</button></div><div class='SongBioPreview-desktop__Container-sc-1'><div>First</div><div>Second</div></div>";
        assert_eq!(parse_genius_about(preview).as_deref(), Some("First\n\nSecond"));
        assert_eq!(parse_genius_about("<div class='SongBioPreview__Container-ab'>A bio</div>").as_deref(), Some("A bio"));
        assert_eq!(parse_genius_about("<div data-lyrics-container='true'>Lyrics</div><div class='lyrics'>More lyrics</div>"), None);
        assert_eq!(parse_genius_about("<title>Just a moment...</title><body>Checking your browser</body>"), None);
        assert_eq!(parse_genius_about("<div data-testid='song-description' data-lyrics-container='true'>Lyrics</div>"), None);
        assert_eq!(parse_genius_about("<div data-testid='song-description'> &nbsp; <button>Read more</button></div>"), None);
    }

    #[test]
    fn genius_about_is_capped_on_unicode_and_long_words() {
        let text = "界".repeat(1000);
        let description = parse_genius_about(&format!("<div data-testid='song-description'><p>{text}</p></div>")).unwrap();
        assert_eq!(description.chars().count(), 900);
        assert!(description.ends_with('…'));
        let text = format!("{} {}", "words ".repeat(150), "long".repeat(200));
        let description = parse_genius_about(&format!("<div data-testid='song-description'>{text}</div>")).unwrap();
        assert!(description.chars().count() <= 900 && description.ends_with('…'));
    }

    #[test]
    fn genius_about_matching_rejects_wrong_artist_and_unsafe_redirects() {
        let hits = serde_json::json!({"response": {"sections": [{"type": "song", "hits": [
            {"result": {"title": "Jóga", "artist_names": "Someone Else", "url": "https://genius.com/cover"}},
            {"result": {"title": "Jóga", "artist_names": "Björk", "url": "https://genius.com.evil.example/song"}},
            {"result": {"title": "Jóga", "artist_names": "Björk", "url": "https://genius.com/bjork-joga-lyrics"}}
        ]}]}});
        let attempt = &genius_search_attempts("Björk - Jóga (Official Video)", "Björk")[0];
        assert_eq!(best_genius_url(&hits, &attempt.title, &attempt.artist).as_deref(), Some("https://genius.com/bjork-joga-lyrics"));
        assert_eq!(best_genius_url(&hits, &attempt.title, "Unrelated"), None);
        assert_eq!(best_genius_url(&serde_json::json!({}), &attempt.title, &attempt.artist), None);
        for unsafe_url in ["http://genius.com/x", "https://genius.com:8443/x", "https://genius.com@evil.example/x", "https://user:pass@genius.com/x"] {
            assert!(!safe_genius_url(&url::Url::parse(unsafe_url).unwrap()));
        }
        assert!(safe_genius_url(&url::Url::parse("https://www.genius.com/song").unwrap()));
    }

    #[tokio::test]
    async fn genius_about_missing_title_or_artist_needs_no_network() {
        let client = Client::new(reqwest::Client::new(), &std::env::temp_dir());
        let q = Query { video_id: "id", song: "", artist: "Artist", album: None, duration: None };
        assert_eq!(client.genius_about(&q).await.unwrap(), None);
        let q = Query { video_id: "id", song: "Song", artist: " ", album: None, duration: None };
        assert_eq!(client.genius_about(&q).await.unwrap(), None);
        let q = Query { video_id: "id", song: "Song", artist: "(Official Video)", album: None, duration: None };
        assert_eq!(client.genius_about(&q).await.unwrap(), None);
        assert!(client.genius_about_http.get().is_none());
    }

    fn genius_test_server(reply: &'static str) -> String {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(reply.as_bytes());
        });
        format!("http://{address}/?secret=do-not-log")
    }

    #[tokio::test]
    async fn genius_about_rejects_unsafe_redirects_and_oversized_bodies() {
        let http = reqwest::Client::builder().no_proxy().redirect(genius_redirect_policy()).build().unwrap();
        let url = genius_test_server("HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/?secret=redirect-secret\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let response = http.get(url).send().await.unwrap();
        assert!(response.status().is_redirection());
        let error = genius_bounded_body(response, 5).await.unwrap_err();
        assert!(!format!("{error:#}").contains("secret"));

        let url = genius_test_server("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n6\r\nabcdef\r\n0\r\n\r\n");
        let response = http.get(url).send().await.unwrap();
        assert!(genius_bounded_body(response, 5).await.is_err());
        let url = genius_test_server("HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let response = http.get(url).send().await.unwrap();
        let error = genius_bounded_body(response, 5).await.unwrap_err();
        assert!(!format!("{error:#}").contains("secret"));
    }

    /// Live: the plain scrape a song's Genius source runs, from this machine's network and
    /// through the rustls client. It answers 403 to the system's TLS stack, so this is the
    /// only check that the client Genius is willing to talk to is still the one we build.
    /// `cargo test --release -- --ignored genius_plain_live --nocapture`
    #[tokio::test]
    #[ignore]
    async fn genius_plain_live() {
        let client = Client::new(reqwest::Client::new(), &std::env::temp_dir());
        let q = Query { video_id: "id", song: "Jóga", artist: "Björk", album: None, duration: None };
        let found = client.fetch_genius(&q).await.expect("Genius refused the lookup");
        let lyrics = found.first().expect("Genius matched no song");
        assert_eq!(lyrics.provider, Provider::GeniusPlain);
        assert!(lyrics.lines.iter().any(|l| !l.text.trim().is_empty()), "Genius page held no lyrics");
    }

    #[test]
    fn genius_html_preserves_lines_sections_entities_and_excludes_ui() {
        let html = r#"<html><body>Outside
            <div data-lyrics-container="true">[Verse 1]<br>Hello&nbsp;<a>world &amp; <b>friends</b></a><br><br>二&#x754C;<span>12You might also like</span><span data-exclude-from-selection="true">translation</span><button>Share</button><script>script</script><style>style</style><div class="LyricsHeader__Container-sc-abc">heading</div><div data-lyrics-container="true"><p>Next&#39;s line</p></div></div>
            <div data-lyrics-container="true">Last line123Embed</div><div class="lyrics">Legacy must not appear</div>
        </body></html>"#;
        let lines = parse_genius_html(html);
        assert_eq!(lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(),
            vec!["[Verse 1]", "Hello world & friends", "", "二界", "Next's line", "Last line"]);
        assert!(lines.iter().all(|l| l.start == 0 && l.parts.is_empty()));
    }

    #[test]
    fn genius_html_handles_legacy_empty_and_challenge_pages() {
        let legacy = "<div class='lyrics'><p>First<br>second</p><p>Third\u{200b}line</p></div>";
        assert_eq!(parse_genius_html(legacy).iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), vec!["First", "second", "Third line"]);
        assert!(parse_genius_html("<html><title>Just a moment...</title><body>Checking your browser</body></html>").is_empty());
        assert!(parse_genius_html("<div data-lyrics-container='true'>You might also like<br>123Embed</div>").is_empty());
        assert!(parse_genius_html("<div data-lyrics-container='true'></div><div class='lyrics'>Wrong fallback</div>").is_empty());
    }

    #[test]
    fn genius_html_many_containers_and_deep_ancestors() {
        for attr in ["data-lyrics-container='true'", "class='lyrics'"] {
            let mut html = "<section>".repeat(1_200);
            for _ in 0..12_000 {
                html.push_str(&format!("<div {attr}>line</div>"));
            }
            html.push_str(&"</section>".repeat(1_200));
            assert!(html.len() < 4 * 1024 * 1024);
            let lines = parse_genius_html(&html);
            assert_eq!(lines.len(), 12_000);
            assert!(lines.iter().all(|line| line.text == "line"));
        }
    }

    #[test]
    fn genius_html_nested_containers_respect_exclusions_and_order() {
        let mut html = String::from("<div class='LyricsHeader__Container-x'><div data-lyrics-container='true'>Outside</div></div>");
        html.push_str(&"<div data-lyrics-container='true'>".repeat(1_000));
        html.push_str("First<div data-exclude-from-selection='true'><div data-lyrics-container='true'>Hidden</div></div><br>Second");
        html.push_str(&"</div>".repeat(1_000));
        html.push_str("<div data-lyrics-container='true'>Last</div><div class='lyrics'>Ignored</div>");
        let lines = parse_genius_html(&html);
        assert_eq!(lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), vec!["Outside", "First", "Second", "Last"]);
    }

    #[test]
    fn genius_html_removes_many_markers_in_one_pass() {
        let text = "界123yOu MiGhT aLsO LiKe".repeat(40_000);
        let html = format!("<div data-lyrics-container='true'>{text}</div>");
        assert!(html.len() < 4 * 1024 * 1024);
        let lines = parse_genius_html(&html);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "界".repeat(40_000));

        // Each removal exposes the next marker and its preceding ASCII digits.
        let text = format!("{}You Might Also Like{}", "you might also 123".repeat(10_000), "like".repeat(10_000));
        let html = format!("<div data-lyrics-container='true'>{text}</div>");
        assert!(parse_genius_html(&html).is_empty());

        // The first removal joins fragments into another marker, including across nodes.
        let html = "<div data-lyrics-container='true'>前 you might also <span>345YOU MIGHT ALSO LIKE</span>like 後<br>é１２3You Might Also Like界</div>";
        let lines = parse_genius_html(html);
        assert_eq!(lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), vec!["前  後", "é１２界"]);
    }

    #[test]
    fn genius_html_marker_cleanup_matches_original_repeated_removal() {
        let pieces = ["you", " might also ", "like", "You might also like", "7", "界", "x", "YOU MIGHT ALSO LIKE", "\u{200b}", "y"];
        let mut state = 7_u64;
        for _ in 0..500 {
            let mut text = String::new();
            for _ in 0..12 {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                text.push_str(pieces[(state >> 32) as usize % pieces.len()]);
            }
            let mut expected = text.replace(['\u{00a0}', '\u{200b}', '\u{feff}'], " ");
            while let Some(pos) = expected.to_ascii_lowercase().find("you might also like") {
                let mut start = pos;
                while start > 0 && expected.as_bytes()[start - 1].is_ascii_digit() {
                    start -= 1;
                }
                expected.replace_range(start..pos + "you might also like".len(), "");
            }
            let expected = expected.trim();
            let html = format!("<div data-lyrics-container='true'>{text}</div>");
            let lines = parse_genius_html(&html);
            let actual = lines.iter().map(|line| line.text.as_str()).collect::<Vec<_>>();
            let expected = if expected.is_empty() { vec![] } else { vec![expected] };
            assert_eq!(actual, expected, "input: {text:?}");
        }
    }

    #[test]
    fn enhanced_lrc_parts() {
        let l = parse_lrc("[00:31.69] <00:31.69> Like <00:35.63>   <00:35.64> the <00:35.69>\n[00:40.00] Next", 60_000);
        assert_eq!(l[0].text, "Like the");
        assert_eq!(l[0].parts[0].start, 31690);
        // Spaced files: word segments are trimmed, spaces get their own parts.
        assert_eq!(l[0].parts[0].text, "Like");
        assert_eq!(l[0].parts[1].text, " ");
        assert_eq!(l[0].parts[0].dur, 3940);
        assert_eq!(l[1].start, 40000);
        // Last line runs to the end of the song.
        assert_eq!(l[1].dur, 20000);
    }

    #[test]
    fn lrc_skips_credits_and_reads_offset() {
        // The extension reads `offset` in seconds.
        let l = parse_lrc("[offset:+0.5]\n[00:01.00]作词 : 野田洋次郎\n[00:05.00]Hello\n[00:07.00]World", 10_000);
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].start, 4500);
        assert_eq!(lrc_songwriters("[00:01.00]作词 : 野田洋次郎\n"), vec!["野田洋次郎"]);
    }

    #[test]
    fn lrc_fixers_move_space_time_to_word() {
        let mut l = parse_lrc("[00:01.00]<00:01.00>a<00:01.50> <00:02.00>b<00:02.50>\n[00:03.00]x", 5000);
        lrc_fixers(&mut l);
        assert_eq!(l[0].parts[0].dur, 1000);
    }

    #[test]
    fn qrc_drops_title_and_reads_syllables() {
        let xml = "<Lyric_1 LyricContent=\"[ti:x]\n[0,6350]Song - Artist(0,198)\n[1000,2000]Get(1000,500) (1500,10)Lucky(1510,490)\n\"/>";
        let l = parse_qrc(xml, 3000, Some(("Song", "Artist")));
        let sung: Vec<&Line> = l.iter().filter(|l| !l.instrumental).collect();
        assert_eq!(sung.len(), 1);
        assert_eq!(sung[0].text, "Get Lucky");
        assert_eq!(sung[0].parts[2].start, 1510);
    }

    #[test]
    fn ttml_parts_background_agents_and_extras() {
        let xml = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:itunes="http://music.apple.com/lyric-ttml-internal" xmlns:ttm="http://www.w3.org/ns/ttml#metadata" xml:lang="ja"><head><metadata><ttm:agent type="person" xml:id="v1"/><ttm:agent type="person" xml:id="v2"/><iTunesMetadata xmlns="http://music.apple.com/lyric-ttml-internal"><translations><translation xml:lang="en"><text for="L1">Hello world</text></translation></translations><songwriters><songwriter>野田洋次郎</songwriter></songwriters><transliterations><transliteration xml:lang="ja-Latn"><text for="L1"><span begin="1.0" end="2.0">kon</span> <span begin="2.0" end="3.0">nichiwa</span></text></transliteration></transliterations></iTunesMetadata></metadata></head><body dur="20.0"><div>
<p begin="1.0" end="3.0" itunes:key="L1" ttm:agent="v1"><span begin="1.0" end="1.5">こん</span><span begin="1.5" end="2.0">に</span> <span begin="2.0" end="3.0">ちは</span><span ttm:role="x-bg"><span begin="2.5" end="3.5">(yeah)</span></span></p>
<p begin="10.0" end="11.0" itunes:key="L2" ttm:agent="v2">Line two</p></div></body></tt>"#;
        let t = parse_ttml(xml, 0);
        assert!(t.word_synced);
        assert_eq!(t.language.as_deref(), Some("ja"));
        assert_eq!(t.songwriters, vec!["野田洋次郎"]);
        let l = &t.lines[0];
        assert_eq!(l.text, "こんに ちは(yeah)");
        assert_eq!(l.parts.len(), 5);
        assert_eq!(l.parts[2], Part { start: 2000, dur: 0, text: " ".into(), bg: false });
        assert!(l.parts[4].bg && l.parts[4].start == 2500);
        assert_eq!(l.agent.as_deref(), Some("v1"));
        assert_eq!(l.translations, vec![("en".to_owned(), "Hello world".to_owned())]);
        assert_eq!(l.romanization.as_deref(), Some("kon nichiwa"));
        assert_eq!(l.timed_roman.len(), 3);
        // A gap of more than five seconds becomes an instrumental line.
        assert!(t.lines[1].instrumental && t.lines[1].start == 3000);
        assert_eq!(t.lines[2].agent.as_deref(), Some("v2"));
        assert!(t.lines[2].parts.is_empty());
        assert!(t.lines.last().unwrap().instrumental);
    }

    #[test]
    fn ttml_times() {
        assert_eq!(ttml_time("1:02.345"), 62345);
        assert_eq!(ttml_time("12.5s"), 12500);
        assert_eq!(ttml_time("250ms"), 250);
        assert_eq!(ttml_time("01:00:01.000"), 3_601_000);
    }

    #[test]
    fn real_stream_parses_every_provider() {
        let Ok(body) = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/research/nandemonaiya.sse")) else { return };
        let q = Query { video_id: "n89SKAymNfA", song: "Nandemonaiya - movie ver.", artist: "RADWIMPS", album: None, duration: Some(343) };
        let got = from_stream(&body, &q);
        let keys: Vec<&str> = got.iter().map(|l| l.provider.key()).collect();
        assert!(keys.contains(&"bLyrics-richsynced"), "{keys:?}");
        let bl = got.iter().find(|l| l.provider == Provider::BLyricsRich).unwrap();
        assert_eq!(bl.sync, Sync::Word);
        assert!(bl.lines.iter().any(|l| l.timed_roman.len() > 3));
        assert_eq!(bl.songwriters, vec!["野田洋次郎"]);
    }
}
