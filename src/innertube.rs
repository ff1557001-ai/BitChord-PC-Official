//! Direct InnerTube `/player` call using the visionOS client.
//!
//! As of Oct 2026 this client returns plain (unciphered) stream URLs that need neither
//! signature deobfuscation nor a PO token, and googlevideo serves every byte range for
//! them; the web clients' streams need a PO token or get cut off after 1 MiB, so this
//! is the path for everyone except signed-in Premium accounts (see `webmusic.rs`).
//! Mirrors yt-dlp's `visionos` client definition; update together if YouTube changes it.

use serde_json::{Value, json};

pub const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";

pub struct Stream {
    pub url: String,
    pub size: u64,
    pub duration_ms: u64,
    /// dB above YouTube's -14 LUFS loudness target (positive = louder).
    pub loudness_db: Option<f32>,
    /// Opus in WebM; otherwise AAC in MP4.
    pub opus: bool,
}

/// A format's loudness relative to YouTube's target, falling back to the track-level
/// figure in `playerConfig.audioConfig`.
pub fn loudness(resp: &Value, format: &Value) -> Option<f32> {
    format["loudnessDb"].as_f64().map(|v| v as f32).or_else(|| {
        let a = &resp["playerConfig"]["audioConfig"];
        let track = a["trackAbsoluteLoudnessLkfs"].as_f64().or_else(|| a["perceptualLoudnessDb"].as_f64())?;
        Some((track - a["loudnessTargetLkfs"].as_f64().unwrap_or(-14.0)) as f32)
    })
}

/// The Android VR client's queue for an album's playlist (`OLAK5uy_…`). Unlike the web
/// clients, which list a track's music video when it has one, it lists the album's songs.
/// Needs no sign-in or PO token. Mirrors Morphe's "Play albums songs" patch
/// (github.com/MorpheApp/morphe-patches, `PlaylistRequest.java`); update together.
pub async fn album_queue(http: &reqwest::Client, playlist_id: &str) -> anyhow::Result<Value> {
    const VERSION: &str = "1.73.21";
    let body = json!({
        "context": {"client": {
            "clientName": "ANDROID_VR",
            "clientVersion": VERSION,
            "deviceMake": "Pico",
            "deviceModel": "A8110",
            "osName": "Android",
            "osVersion": "10",
            "androidSdkVersion": 29,
            // English, so the playlist's title says "Album - …".
            "hl": "en",
            "gl": "US",
        }},
        "playlistId": playlist_id,
        "contentCheckOk": true,
        "racyCheckOk": true,
    });
    Ok(http
        .post("https://youtubei.googleapis.com/youtubei/v1/next?prettyPrint=false")
        .header("User-Agent", format!("com.google.android.apps.youtube.vr.pico/{VERSION} (Linux; U; Android 10; A8110 Build/QKQ1) gzip"))
        .header("X-YouTube-Client-Name", "28")
        .header("X-YouTube-Client-Version", VERSION)
        .timeout(std::time::Duration::from_secs(8))
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

/// `opus`: Opus (itag 251, about 130-160 kbps) when the song has it, else AAC.
pub async fn audio_stream(http: &reqwest::Client, video_id: &str, visitor_data: &str, opus: bool) -> anyhow::Result<Stream> {
    let resp = player(http, video_id, visitor_data).await?;
    // Opus in WebM (251) when asked for; AAC-LC in MP4 (140), HE-AAC (139) as fallbacks.
    let formats = formats(&resp);
    let itags: &[u64] = if opus { &[251, 140, 139] } else { &[140, 139] };
    itags
        .iter()
        .find_map(|itag| {
            let f = formats.iter().find(|f| f["itag"] == *itag)?;
            log::info!("visionOS: {video_id} itag {itag} ({} kbps)", f["averageBitrate"].as_u64().or(f["bitrate"].as_u64()).unwrap_or(0) / 1000);
            Some(Stream {
                url: f["url"].as_str()?.to_owned(),
                size: f["contentLength"].as_str()?.parse().ok()?,
                duration_ms: f["approxDurationMs"].as_str().and_then(|d| d.parse().ok()).unwrap_or(0),
                loudness_db: loudness(&resp, f),
                opus: *itag == 251,
            })
        })
        .ok_or_else(|| anyhow::anyhow!("no audio stream in visionOS player response"))
}

/// Whether YouTube's own description says AI made the video: the watch page carries a
/// "Made with AI" block for it, which a clean title and artist never repeat.
pub async fn made_with_ai(http: &reqwest::Client, video_id: &str, visitor_data: &str) -> anyhow::Result<bool> {
    let resp = player(http, video_id, visitor_data).await?;
    let text = resp["videoDetails"]["shortDescription"].as_str().unwrap_or("").to_ascii_lowercase();
    Ok(text.contains("made with ai") || text.contains("altered or fully generated"))
}

/// youtube.com's own answer for a watch page, asked anonymously with the desktop client.
/// It is the only place the "Most replayed" heat markers sit, and the curve is the same
/// for every listener, so this sends no cookies and no visitor id. Mirrors upstream's
/// `Innertube.webNext`, retried twice because the answer is worth one more try.
pub async fn web_next(http: &reqwest::Client, video_id: &str) -> anyhow::Result<Value> {
    const VERSION: &str = "2.20260708.00.00";
    const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";
    let body = json!({
        "context": {"client": {"clientName": "WEB", "clientVersion": VERSION, "hl": "en", "gl": "US"}},
        "videoId": video_id,
    });
    let mut last = anyhow::anyhow!("youtube.com sent no answer for {video_id}");
    for _ in 0..2 {
        let res = async {
            let resp = http
                .post("https://www.youtube.com/youtubei/v1/next?prettyPrint=false")
                .header("User-Agent", UA)
                .header("Origin", "https://www.youtube.com")
                .header("Referer", "https://www.youtube.com/")
                .header("X-YouTube-Client-Name", "1")
                .header("X-YouTube-Client-Version", VERSION)
                .header("Accept-Language", "en-US,en")
                .json(&body)
                .send()
                .await?
                .error_for_status()?;
            Ok::<_, reqwest::Error>(resp.json::<Value>().await?)
        }
        .await;
        match res {
            Ok(v) => return Ok(v),
            Err(e) => last = e.into(),
        }
    }
    Err(last)
}

/// The picture to play over the sound: one of the video-only itags, which carry no audio
/// of their own, so the song keeps playing from `audio_stream`.
#[derive(Clone)]
pub struct Video {
    /// The video this picture belongs to: the file the clip is cached under.
    pub id: String,
    /// The mp4 the answer offers outright, when it offers one: empty when every format
    /// is left behind a cipher and only the playlist below is playable.
    pub url: String,
    /// The master playlist YouTube signs for the same clip. It needs no signature work of
    /// its own, so it plays where the plain mp4 doesn't.
    pub hls: Option<String>,
    pub width: u32,
    pub height: u32,
    pub size: u64,
    /// How long the video runs, as YouTube's own answer says, in seconds; 0 when it says
    /// nothing. A skip mark is only worth honouring on a cut of this length.
    pub duration_s: f64,
    /// The parts of this video the SponsorBlock community marked as not the music, which
    /// the backend fills in where the listener asks for them. Empty is the same as a clip
    /// with no marks: the song starts at the video's own top.
    pub skip: Vec<crate::sponsorblock::Segment>,
}

/// The visionOS client's request for one clip: the body both the anonymous and the
/// signed-in ask send.
fn visionos(video_id: &str, visitor_data: &str) -> Value {
    json!({
        "context": {"client": {
            "clientName": "VISIONOS",
            "clientVersion": "1.02",
            "deviceMake": "Apple",
            "deviceModel": "RealityDevice17,1",
            "userAgent": UA,
            "osName": "visionOS",
            "osVersion": "26.5.23O471",
            "hl": "en",
            "gl": "US",
            "visitorData": visitor_data,
        }},
        "videoId": video_id,
        "contentCheckOk": true,
        "racyCheckOk": true,
    })
}

/// An answer's playability, as an error when the client won't play the clip.
fn playability(resp: &Value) -> anyhow::Result<()> {
    let status = &resp["playabilityStatus"];
    if status["status"] != "OK" {
        anyhow::bail!(
            "{}: {}",
            status["status"].as_str().unwrap_or("UNPLAYABLE"),
            status["reason"].as_str().unwrap_or("no reason given")
        );
    }
    Ok(())
}

/// The visionOS client's answer for one video: stream URLs that need no signature work
/// and no PO token (see the module's note).
async fn player(http: &reqwest::Client, video_id: &str, visitor_data: &str) -> anyhow::Result<Value> {
    let body = visionos(video_id, visitor_data);
    let resp: Value = http
        .post("https://www.youtube.com/youtubei/v1/player?prettyPrint=false")
        .header("User-Agent", UA)
        .header("X-YouTube-Client-Name", "101")
        .header("X-YouTube-Client-Version", "1.02")
        .header("X-Goog-Visitor-Id", visitor_data)
        .header("Origin", "https://www.youtube.com")
        .timeout(std::time::Duration::from_secs(8))
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    playability(&resp)?;
    Ok(resp)
}

pub async fn video_stream(http: &reqwest::Client, video_id: &str, visitor_data: &str, max_width: u32) -> anyhow::Result<Video> {
    let resp = player(http, video_id, visitor_data).await?;
    let v = pick_video(&resp, video_id, max_width, "visionOS").ok_or_else(|| anyhow::anyhow!("no picture in visionOS player response"))?;
    if v.hls.is_none() && v.url.is_empty() {
        anyhow::bail!("the visionOS answer leaves every picture behind a cipher");
    }
    Ok(v)
}

/// The visionOS client asked for as the signed-in listener: the client whose addresses
/// download as they are, allowed the clips a fresh visitor is refused.
///
/// Not the account's own music client (`WEB_REMIX`), although that is what the site
/// itself uses: its answers carry `sefc`/`spc`/`bui`, a browser-only credential, so a
/// plain GET for the picture is refused with a 403 and nothing plays. Measured, not
/// assumed — see `signed_picture_fetch_live`.
async fn player_signed(http: &reqwest::Client, session: &crate::webmusic::Session, video_id: &str) -> anyhow::Result<Value> {
    let vd = session.visitor_data.clone().unwrap_or_default();
    let body = visionos(video_id, &vd);
    let req = session.sign(
        http.post("https://www.youtube.com/youtubei/v1/player?prettyPrint=false")
            .header("User-Agent", UA)
            .header("X-YouTube-Client-Name", "101")
            .header("X-YouTube-Client-Version", "1.02")
            .header("X-Goog-Visitor-Id", &vd)
            .header("Origin", "https://www.youtube.com")
            .timeout(std::time::Duration::from_secs(8)),
        "https://www.youtube.com",
    )?;
    let resp: Value = req.json(&body).send().await?.error_for_status()?.json().await?;
    playability(&resp)?;
    Ok(resp)
}

/// The same picture asked for as the signed-in account. A clip the anonymous client is
/// refused — age-gated, member-only, or simply bot-suspected — is often one the
/// listener's own session is allowed.
pub async fn video_stream_signed(api: &crate::ytmapi::Api, video_id: &str, max_width: u32) -> anyhow::Result<Video> {
    let session = api.account().ok_or_else(|| anyhow::anyhow!("sign in first"))?;
    let resp = player_signed(&crate::proxy::http(), &session, video_id).await?;
    let v = pick_video(&resp, video_id, max_width, "the account").ok_or_else(|| anyhow::anyhow!("no picture in the signed-in player answer"))?;
    if v.hls.is_none() && v.url.is_empty() {
        anyhow::bail!("the signed-in answer leaves every picture behind a cipher");
    }
    Ok(v)
}

fn formats(resp: &Value) -> &[Value] {
    resp["streamingData"]["adaptiveFormats"].as_array().map(Vec::as_slice).unwrap_or_default()
}

/// H.264 (`avc1`) only, and no wider than the space it's shown in: Media Foundation
/// decodes it on every Windows box, and a stream wider than the player only costs bytes.
/// Nothing under that width, or only wider formats, takes the smallest there is. A format
/// with no plain address still names its size, so the pick is made on that and the
/// signed playlist carries the bytes.
fn pick_video(resp: &Value, id: &str, max_width: u32, who: &str) -> Option<Video> {
    let avc = |f: &Value| f["mimeType"].as_str().is_some_and(|m| m.contains("codecs=\"avc1"));
    let width = |f: &Value| f["width"].as_u64().unwrap_or(0) as u32;
    let side = || formats(resp).iter().filter(|f| avc(f));
    let fit = || side().filter(|f| width(f) <= max_width);
    let f = match fit().max_by_key(|f| width(f)).or_else(|| side().min_by_key(|f| width(f))) {
        Some(f) => f,
        None => return None,
    };
    let hls = resp["streamingData"]["hlsManifestUrl"].as_str().map(str::to_owned);
    log::info!(
        "{who}: picture {id} itag {} {}x{} ({} kbps) address:{} playlist:{}",
        f["itag"].as_u64().unwrap_or(0),
        width(f),
        f["height"].as_u64().unwrap_or(0),
        f["averageBitrate"].as_u64().or(f["bitrate"].as_u64()).unwrap_or(0) / 1000,
        f["url"].as_str().is_some(),
        hls.is_some()
    );
    Some(Video {
        id: id.to_owned(),
        url: f["url"].as_str().unwrap_or_default().to_owned(),
        hls,
        width: width(f),
        height: f["height"].as_u64().unwrap_or(0) as u32,
        size: f["contentLength"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0),
        duration_s: duration(resp),
        skip: Vec::new(),
    })
}

/// How long the video is, in seconds. InnerTube writes it as a string, and the account's
/// own answer writes it as a number.
fn duration(resp: &Value) -> f64 {
    let d = &resp["videoDetails"]["durationSeconds"];
    d.as_str().and_then(|s| s.parse().ok()).or_else(|| d.as_f64()).unwrap_or(0.0)
}

#[cfg(test)]
mod format_probe {
    /// Every picture the visionOS client offers for one video, and the shape it comes in.
    /// The signed address is left out of the print on purpose. Needs the network:
    /// `cargo test --release -- --ignored lists_the_pictures_on_offer --nocapture`
    #[test]
    #[ignore]
    fn lists_the_pictures_on_offer() {
        let resp = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let api = crate::ytmapi::Api::new(&crate::data_dir());
                let vd = api.visitor_data(false).await.expect("no visitor id");
                super::player(&crate::proxy::http(), "dQw4w9WgXcQ", &vd).await.expect("no player answer")
            });
        println!("playability: {}", resp["playabilityStatus"]["status"].as_str().unwrap_or("?"));
        println!("hls manifest: {}", resp["streamingData"]["hlsManifestUrl"].as_str().is_some());
        for key in ["formats", "adaptiveFormats"] {
            let list = resp["streamingData"][key].as_array().cloned().unwrap_or_default();
            println!("{key}: {} entries", list.len());
            for f in list {
                let mb = |v: &serde_json::Value| v["contentLength"].as_str().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0) / 1_048_576.0;
                println!(
                    "  itag {:<4} {:<28} {:>4}x{:<4} {:>6.1} MB url:{} cipher:{}",
                    f["itag"].as_u64().unwrap_or(0),
                    f["mimeType"].as_str().unwrap_or("?"),
                    f["width"].as_u64().unwrap_or(0),
                    f["height"].as_u64().unwrap_or(0),
                    mb(&f),
                    f["url"].is_string(),
                    f["signatureCipher"].is_string()
                );
            }
        }
        let Some(master) = resp["streamingData"]["hlsManifestUrl"].as_str() else { return };
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            let http = crate::proxy::http();
            shape(&http, master, "master").await;
            let Some(media) = crate::apple::variant(&http, master, 1280).await else {
                println!("variant: none a Windows box could decode");
                return;
            };
            shape(&http, &media, "variant").await;
            let (init, segments) = crate::apple::media_parts(&http, &media).await.unwrap_or((None, Vec::new()));
            println!("pieces: init:{} segments:{}", init.is_some(), segments.len());
        });
    }

    /// Which client still hands the player a picture it can actually download. The same
    /// clips are asked three ways — the anonymous visionOS client the app leads with, the
    /// account's own WEB_REMIX client, and the account wearing the visionOS client — and
    /// every plain address an answer carries is fetched for its first 64 KB, so the
    /// output says which shape the bytes are behind instead of what the code hopes.
    /// Prints no address, only widths and statuses. Needs the network and a sign-in:
    /// `cargo test --release -- --ignored signed_picture_fetch_live --nocapture`
    #[test]
    #[ignore]
    fn signed_picture_fetch_live() {
        const MUSIC: &str = "https://music.youtube.com";
        let ids: Vec<String> = std::env::var("YTM_VIDEO")
            .map(|s| s.split(',').map(|i| i.trim().to_owned()).filter(|i| !i.is_empty()).collect())
            .unwrap_or_else(|_| ["x8tfvLUkIAs", "dQw4w9WgXcQ"].iter().map(|s| (*s).to_string()).collect());
        let avc = |f: &serde_json::Value| f["mimeType"].as_str().is_some_and(|m| m.contains("codecs=\"avc1"));
        // The widest plain H.264 address of an answer, with the size the answer says it is.
        let plain = |v: &serde_json::Value| -> Option<(String, u32)> {
            let f = super::formats(v)
                .iter()
                .filter(|f| avc(f))
                .filter(|f| f["url"].is_string())
                .max_by_key(|f| f["width"].as_u64().unwrap_or(0))?;
            Some((f["url"].as_str().unwrap().to_owned(), f["width"].as_u64().unwrap_or(0) as u32))
        };
        let read = |v: &serde_json::Value| {
            let p = &v["playabilityStatus"];
            format!("{} [{}] hls:{}", p["status"].as_str().unwrap_or("?"), p["reason"].as_str().unwrap_or("no reason"), v["streamingData"]["hlsManifestUrl"].as_str().is_some())
        };
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        // One GET, reported only as a status and a size: the address itself never reaches
        // the log, since it carries the signature.
        async fn bytes(what: &str, req: reqwest::RequestBuilder) {
            match tokio::time::timeout(std::time::Duration::from_secs(20), req.send()).await {
                Ok(Ok(r)) => println!("    {what}: {} in {} KB", r.status().as_u16(), r.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) / 1024),
                Ok(Err(e)) => println!("    {what}: {e:?}"),
                Err(_) => println!("    {what}: timed out"),
            }
        }
        rt.block_on(async {
            let api = crate::ytmapi::Api::new(&crate::data_dir());
            let session = api.account().expect("no session in the vault");
            let vd = api.visitor_data(false).await.unwrap_or_default();
            let http = crate::proxy::http();
            let sign = |r: reqwest::RequestBuilder| session.sign(r, MUSIC).expect("the session won't sign");
            for id in &ids {
                println!("--- {id}");
                // 1. The anonymous visionOS client the app leads with.
                match super::player(&http, id, &vd).await {
                    Ok(v) => {
                        println!("  anonymous visionOS: {}", read(&v));
                        match plain(&v) {
                            Some((u, w)) => bytes(&format!("plain {w} px, the app's own ask"), http.get(&u).header("User-Agent", super::UA).header("Range", "bytes=0-65535")).await,
                            None => println!("  no plain avc1 address"),
                        }
                    }
                    Err(e) => println!("  anonymous visionOS: refused {e}"),
                }
                // 2. The account's own client, which is what the app falls back to.
                let account = api.call("player", serde_json::json!({"videoId": id, "contentCheckOk": true, "racyCheckOk": true}), true).await;
                match account {
                    Ok(v) => {
                        println!("  account WEB_REMIX: {}", read(&v));
                        if let Some((u, w)) = plain(&v) {
                            let bound = ["sefc", "bui", "spc", "sig", "sver"].iter().filter(|k| u.contains(&format!("{k}="))).copied().collect::<Vec<_>>();
                            println!("  plain avc1 {w} px, gated on {:?}", bound);
                            let ask = |r: reqwest::RequestBuilder| r.header("Range", "bytes=0-65535");
                            let browser = |r: reqwest::RequestBuilder| r.header("User-Agent", crate::webmusic::UA).header("Origin", MUSIC).header("Referer", format!("{MUSIC}/watch?v={id}"));
                            bytes("the app's own ask", ask(http.get(&u).header("User-Agent", super::UA))).await;
                            bytes("cookies only", ask(http.get(&u).header("Cookie", &session.cookie).header("User-Agent", crate::webmusic::UA))).await;
                            let as_the_browser = |r: reqwest::RequestBuilder| ask(sign(browser(r)));
                            bytes("signed as the browser", as_the_browser(http.get(&u))).await;
                            bytes(
                                "signed + client headers",
                                as_the_browser(http.get(&u)).header("X-YouTube-Client-Name", "67").header("X-YouTube-Client-Version", "1.20251101.00.00"),
                            ).await;
                        } else {
                            println!("  no plain avc1 address");
                        }
                    }
                    Err(e) => println!("  account WEB_REMIX: refused {e}"),
                }
                // 3. The visionOS client asked for as the signed-in account: its addresses
                //    are the ones that always download, so if it takes the session, the
                //    fallback has a shape that works.
                let req = sign(http.post("https://www.youtube.com/youtubei/v1/player?prettyPrint=false")
                    .header("User-Agent", super::UA)
                    .header("X-YouTube-Client-Name", "101")
                    .header("X-YouTube-Client-Version", "1.02")
                    .header("Origin", "https://www.youtube.com"));
                let asked = serde_json::json!({
                    "context": {"client": {
                        "clientName": "VISIONOS", "clientVersion": "1.02", "deviceMake": "Apple",
                        "deviceModel": "RealityDevice17,1", "userAgent": super::UA, "osName": "visionOS",
                        "osVersion": "26.5.23O471", "hl": "en", "gl": "US",
                        "visitorData": session.visitor_data.clone().unwrap_or_else(|| vd.clone()),
                    }},
                    "videoId": id.as_str(), "contentCheckOk": true, "racyCheckOk": true,
                });
                match tokio::time::timeout(std::time::Duration::from_secs(20), req.json(&asked).send()).await {
                    Ok(Ok(r)) => {
                        let status = r.status();
                        let v: serde_json::Value = r.json().await.unwrap_or_default();
                        println!("  visionOS as the account: http {status} {}", read(&v));
                        if let Some((u, w)) = plain(&v) {
                            bytes(&format!("plain {w} px, the app's own ask"), http.get(&u).header("User-Agent", super::UA).header("Range", "bytes=0-65535")).await;
                        }
                    }
                    Ok(Err(e)) => println!("  visionOS as the account: {e:?}"),
                    Err(_) => println!("  visionOS as the account: timed out"),
                }
            }
        });
    }

    /// The tag lines of a playlist, and the file names of its pieces: every address
    /// YouTube signs is cut down to the name at its end, so a signed address is never
    /// written down.
    async fn shape(http: &reqwest::Client, url: &str, tag: &str) {
        let text = match http.get(url).header("User-Agent", super::UA).send().await {
            Ok(r) => r.text().await.unwrap_or_default(),
            Err(_) => String::new(),
        };
        if text.is_empty() {
            println!("{tag}: unreachable");
            return;
        }
        let name = |s: &str| s.rsplit('/').next().unwrap_or("").to_owned();
        println!("{tag}: {} lines", text.lines().count());
        for line in text.lines().take(20) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match line.find("URI=\"") {
                Some(i) => println!("  {}<{}>", &line[..i], name(line[i + 5..].split('"').next().unwrap_or(""))),
                None if line.starts_with('#') => println!("  {line}"),
                None => println!("  piece: {}", name(line)),
            }
        }
    }
}
