//! Apple's motion sleeves, played locally.
//!
//! The clip is HLS, but each variant's media playlist is a single byte-ranged
//! fragmented MP4, so it downloads as one file and Media Foundation decodes it
//! off the UI thread. Only the newest frame is kept: the UI turns it into a
//! texture and draws it in place of the still sleeve.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::{Duration, Instant};

pub struct Frame {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
}

pub struct Motion {
    url: String,
    latest: Mutex<Option<Arc<Frame>>>,
    version: AtomicU64,
    /// Where the newest frame belongs in the file, in 100 ns units from its start.
    at: AtomicU64,
    stop: Arc<AtomicBool>,
    /// Held while the song is paused, so the picture waits with it instead of running on.
    pause: Arc<AtomicBool>,
    /// Set once the thread that fetches and decodes the clip has had its whole try: a
    /// picture that never gave a frame is not still on its way.
    done: Arc<AtomicBool>,
}

impl Motion {
    fn new(url: &str) -> Self {
        Self { url: url.to_owned(), latest: Mutex::new(None), version: AtomicU64::new(0), at: AtomicU64::new(0), stop: Arc::new(AtomicBool::new(false)), pause: Arc::new(AtomicBool::new(false)), done: Arc::new(AtomicBool::new(false)) }
    }

    /// Freeze the decoder between frames: the picture holds where it is and picks up there.
    pub fn set_pause(&self, on: bool) {
        self.pause.store(on, Ordering::Relaxed);
    }

    /// How far into the clip the picture on screen is; the music-video player uses it to notice the
    /// song has moved out from under it.
    pub fn position(&self) -> Duration {
        Duration::from_nanos(self.at.load(Ordering::Relaxed) * 100)
    }

    /// The newest frame, with the counter that says it changed.
    pub fn latest(&self) -> Option<(u64, Arc<Frame>)> {
        let frame = self.latest.lock().ok()?.clone()?;
        Some((self.version.load(Ordering::Acquire), frame))
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Stop decoding: the page that showed this clip is gone.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Whether the clip's thread has finished with nothing to show: the download and
    /// the decode both had their try, and the picture is not still on its way.
    pub fn done(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }

    /// Puts a frame on screen and hands back the one it displaced: two frames later the
    /// decoder fills that buffer again, which is cheaper than a fresh allocation and a
    /// zeroing of eight megabytes every frame.
    fn publish(&self, ctx: &egui::Context, w: usize, h: usize, rgba: Vec<u8>) -> Option<Arc<Frame>> {
        let old = match self.latest.lock() {
            Ok(mut slot) => slot.replace(Arc::new(Frame { w, h, rgba })),
            Err(_) => None,
        };
        self.version.fetch_add(1, Ordering::Release);
        ctx.request_repaint();
        old
    }
}

impl Drop for Motion {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

static STREAMS: LazyLock<Mutex<HashMap<String, Weak<Motion>>>> = LazyLock::new(Default::default);

/// Start (or reuse) the clip at `master`, decoded no wider than `max_width`.
pub fn play(ctx: &egui::Context, master: &str, max_width: u32) -> Arc<Motion> {
    let key = format!("{max_width}:{master}");
    if let Ok(mut streams) = STREAMS.lock() {
        // Entries whose view is gone would otherwise pile up for the whole session.
        streams.retain(|_, w| w.strong_count() > 0);
        if let Some(m) = streams.get(&key).and_then(Weak::upgrade) {
            return m;
        }
        let motion = Arc::new(Motion::new(master));
        streams.insert(key, Arc::downgrade(&motion));
        let (this, ctx, url) = (motion.clone(), ctx.clone(), master.to_owned());
        let _ = std::thread::Builder::new().name("motion".into()).spawn(move || run(this, ctx, url, max_width));
        return motion;
    }
    Arc::new(Motion::new(master))
}

fn run(motion: Arc<Motion>, ctx: egui::Context, master: String, max_width: u32) {
    let Some(path) = fetch(&master, max_width) else {
        log::info!("apple: no motion clip downloaded");
        return;
    };
    decode(&motion, &path.to_string_lossy(), &ctx, max_width, Duration::ZERO);
}

/// The music-video player's picture, saved under its video id and decoded from the file: Media
/// Foundation's own HTTP source never gives a frame for a YouTube stream (it asks without
/// the client the signature was made for), so the bytes come through the app's client —
/// which also means a song skipped back to replays from a copy already on disk.
/// The plain mp4 is asked for first, since it is the size the player wanted; the signed
/// playlist is the fallback, for the clips whose mp4 address sits behind a cipher.
/// `start_at` runs the clip on from where the song already is.
pub fn play_video(ctx: &egui::Context, id: &str, url: &str, hls: Option<&str>, max_width: u32, start_at: Duration) -> Arc<Motion> {
    let motion = Arc::new(Motion::new(if url.is_empty() { hls.unwrap_or(url) } else { url }));
    let (this, ctx, id, url, hls, at) = (motion.clone(), ctx.clone(), id.to_owned(), url.to_owned(), hls.map(str::to_owned), start_at);
    let _ = std::thread::Builder::new().name("video".into()).spawn(move || {
        let done = this.done.clone();
        let run = move || {
            // The plain mp4 first: it is the width that was asked for, while the playlist's
            // own variants stop at 720p however big the player wants the picture. The
            // playlist answers for the clips whose mp4 address YouTube withholds behind a
            // cipher, and needs no signature work of its own.
            if !url.is_empty() {
                match save(&id, &url) {
                    Some(path) => {
                        decode(&this, &path.to_string_lossy(), &ctx, max_width, at);
                        if this.stop.load(Ordering::Relaxed) || this.latest().is_some() {
                            return;
                        }
                        log::info!("video: the plain clip decoded to nothing");
                    }
                    None => log::info!("video: the picture didn't download"),
                }
            }
            let Some(master) = hls.as_deref() else {
                if url.is_empty() {
                    log::info!("video: the answer held nothing to download");
                }
                return;
            };
            if let Some(path) = save_hls(&id, master, max_width) {
                decode(&this, &path.to_string_lossy(), &ctx, max_width, at);
            }
        };
        run();
        done.store(true, Ordering::Relaxed);
    });
    motion
}

/// Spotify's canvas: a short clip with no sound of its own, played over the sleeve and
/// looping until the song changes. Its file is cached under the name Spotify gave it,
/// which is no YouTube id, so it goes to a cache of its own.
pub fn play_canvas(ctx: &egui::Context, name: &str, url: &str, max_width: u32) -> Arc<Motion> {
    let motion = Arc::new(Motion::new(url));
    let (this, ctx, name, url) = (motion.clone(), ctx.clone(), name.to_owned(), url.to_owned());
    let _ = std::thread::Builder::new().name("canvas".into()).spawn(move || {
        let done = this.done.clone();
        if let Some(path) = save_canvas(&name, &url) {
            decode(&this, &path.to_string_lossy(), &ctx, max_width, Duration::ZERO);
        }
        done.store(true, Ordering::Relaxed);
    });
    motion
}

/// The canvas on disk, or as it comes down. Its name is Spotify's, so only what a file
/// name can carry is kept, and a name too short to be one is refused.
fn save_canvas(name: &str, url: &str) -> Option<PathBuf> {
    let clean: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
    if clean.len() < 8 {
        return None;
    }
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;
    rt.block_on(async {
        let dir = crate::data_dir().join("canvas");
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join(format!("{clean}.mp4"));
        if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > 1024 {
            return Some(path);
        }
        // A canvas is a few megabytes: small enough to land in one go.
        let bytes = crate::proxy::http()
            .get(url)
            .header("User-Agent", crate::innertube::UA)
            .timeout(Duration::from_secs(60))
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .bytes()
            .await
            .ok()?;
        if bytes.len() <= 1024 {
            log::info!("canvas: {} came down {} bytes, which is no picture", name, bytes.len());
            return None;
        }
        let tmp = path.with_extension("part");
        std::fs::write(&tmp, &bytes).ok()?;
        std::fs::rename(&tmp, &path).ok()?;
        Some(path)
    })
}

/// The music video as YouTube signs it: the master playlist, the variant no wider than
/// the player, and its pieces written into one file. A playlist of fragmented-MP4 pieces
/// names the header it starts with and is written out as it came; one of transport-stream
/// segments has its pictures lifted out into an MP4 of our own, since Media Foundation
/// has no reader for the stream they come in.
fn save_hls(id: &str, master: &str, max_width: u32) -> Option<PathBuf> {
    // Only an id YouTube Music could have handed out reaches the disk, so nothing in a
    // playlist off the network can choose a path.
    video_name(id)?;
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;
    rt.block_on(async {
        let http = crate::proxy::http();
        let media = crate::apple::variant(&http, master, max_width).await?;
        let (init, segments) = crate::apple::media_parts(&http, &media).await?;
        let dir = crate::data_dir().join("video");
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join(format!("{id}.mp4"));
        // A clip is only called a clip when the last byte of it has landed.
        if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > 1024 {
            if init.is_some() {
                zero_the_edit_list(&path);
            }
            return Some(path);
        }
        let tmp = path.with_extension("part");
        let mut out = std::fs::File::create(&tmp).ok()?;
        let mut total = 0usize;
        if let Some(init) = init.as_ref() {
            for part in std::iter::once(init).chain(segments.iter()) {
                total += pull(&http, part, Some(crate::innertube::UA), &mut out).await?;
            }
        } else {
            use std::io::{Seek, SeekFrom, Write};
            let header = crate::remux::Remux::header();
            out.write_all(&header).ok()?;
            let mdat_at = header.len() as u64;
            // The pictures' run, its 64-bit size patched in once it has ended.
            out.write_all(&[0, 0, 0, 1, b'm', b'd', b'a', b't', 0, 0, 0, 0, 0, 0, 0, 0]).ok()?;
            let mut remux = crate::remux::Remux::new(max_width, (max_width as f32 * 9.0 / 16.0) as u32);
            let mut payload = 0u64;
            for part in segments.iter() {
                let bytes = pull_bytes(&http, part, Some(crate::innertube::UA)).await?;
                total += bytes.len();
                let pics = remux.push(&bytes);
                out.write_all(&pics).ok()?;
                payload += pics.len() as u64;
            }
            let Some((tail, moov)) = remux.finish(mdat_at + 16) else {
                drop(out);
                let _ = std::fs::remove_file(&tmp);
                log::info!("video: the playlist held no pictures");
                return None;
            };
            out.write_all(&tail).ok()?;
            payload += tail.len() as u64;
            out.seek(SeekFrom::Start(mdat_at + 8)).ok()?;
            out.write_all(&(16 + payload).to_be_bytes()).ok()?;
            out.seek(SeekFrom::End(0)).ok()?;
            out.write_all(&moov).ok()?;
        }
        drop(out);
        if total <= 1024 || std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return None;
        }
        if init.is_some() {
            zero_the_edit_list(&path);
        }
        log::info!("video: {} pieces of the clip cached ({:.0} KB)", segments.len(), total as f32 / 1024.0);
        trim_cache(&dir, CACHE_BUDGET);
        Some(path)
    })
}

/// The file a clip is kept under. Only an id of the shape YouTube Music hands out —
/// eleven characters, letters, digits and the two it uses itself — reaches the disk, so
/// nothing in an answer off the network can choose a path.
fn video_name(id: &str) -> Option<String> {
    let ok = id.len() == 11 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    ok.then(|| format!("{id}.mp4"))
}

/// Write the stream out, or find the copy an earlier watch left behind.
fn save(id: &str, url: &str) -> Option<PathBuf> {
    let name = video_name(id)?;
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;
    rt.block_on(async {
        let dir = crate::data_dir().join("video");
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join(name);
        // A clip is only called a clip when the last byte of it has landed. A copy left
        // half-written by an exit in the middle of a download would sit in the cache
        // forever and decode nothing at all.
        let tmp = path.with_extension("part");
        if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) <= 1024 {
            let mut out = std::fs::File::create(&tmp).ok()?;
            let mut resp = match crate::proxy::http()
                .get(url)
                .header("User-Agent", crate::innertube::UA)
                .timeout(Duration::from_secs(180))
                .send()
                .await
            {
                Ok(r) => match r.error_for_status() {
                    Ok(r) => r,
                    Err(e) => {
                        log::info!("video: the clip's address was refused ({e})");
                        return None;
                    }
                },
                Err(e) => {
                    log::info!("video: the clip wouldn't come down ({e})");
                    return None;
                }
            };
            let mut total = 0u64;
            loop {
                match resp.chunk().await {
                    Ok(Some(chunk)) => {
                        if std::io::Write::write_all(&mut out, &chunk).is_err() {
                            log::info!("video: the clip wouldn't write to disk");
                            break None;
                        }
                        total += chunk.len() as u64;
                    }
                    Ok(None) => break Some(()),
                    Err(e) => {
                        log::info!("video: the clip stopped coming down at {:.1} MB ({e})", total as f64 / 1_048_576.0);
                        break None;
                    }
                }
            }?;
            drop(out);
            if total <= 1024 || std::fs::rename(&tmp, &path).is_err() {
                log::info!("video: the clip came down {:.0} bytes, which is no picture", total);
                let _ = std::fs::remove_file(&tmp);
                return None;
            }
        }
        zero_the_edit_list(&path);
        trim_cache(&dir, CACHE_BUDGET);
        Some(path)
    })
}

/// Media Foundation's MP4 demuxer refuses a DASH clip whose track carries an edit list
/// that starts it late: YouTube puts a frame or two of `media_time` in front of the
/// video-only stream, and the demuxer answers end of stream before the first picture.
/// Zeroing that delay is the one change that makes the file decode — durations, the
/// segment index and the request shape make no difference to it. It costs the clip a
/// frame of its own pacing, and the player is slaved to the audio anyway, so the file is
/// rewritten where it lies and an older cache entry heals on the next watch.
fn zero_the_edit_list(path: &std::path::Path) {
    use std::io::{Read, Seek, SeekFrom, Write};
    let Ok(mut file) = std::fs::OpenOptions::new().read(true).write(true).open(path) else { return };
    // The header of a DASH clip is a few hundred bytes in; this is far past it.
    let len = file.metadata().map(|m| m.len() as usize).unwrap_or(0).min(64 * 1024);
    let mut head = vec![0u8; len];
    if file.read_exact(&mut head).is_err() {
        return;
    }
    let mut times = Vec::new();
    edit_times(&head, 0, len, &mut times);
    for (at, width) in times {
        if file.seek(SeekFrom::Start(at as u64)).is_ok() {
            let _ = file.write_all(&vec![0u8; width]);
        }
    }
}

/// Where the media times of the edit lists in the front of a clip are, and how wide each
/// one is. The tree is walked rather than the name searched for: `elst` turns up inside
/// picture data too.
fn edit_times(b: &[u8], from: usize, to: usize, times: &mut Vec<(usize, usize)>) {
    let mut i = from;
    while i + 8 <= to {
        let Some(len) = be32(b, i) else { break };
        let len = len as usize;
        if len < 8 || i + len > to {
            break;
        }
        match &b[i + 4..i + 8] {
            b"elst" => {
                // An entry is segment duration, media time, media rate; version 1 makes
                // the two times 64-bit. The count comes off the network like everything
                // else here, so an entry only counts when it fits inside the box.
                let (entry, inside, width) = if b.get(i + 8) == Some(&1) { (20, 8, 8) } else { (12, 4, 4) };
                for n in 0..be32(b, i + 12).unwrap_or(0) {
                    let start = i + 16 + n as usize * entry;
                    if start + entry > i + len {
                        break;
                    }
                    times.push((start + inside, width));
                }
            }
            b"moov" | b"trak" | b"edts" => edit_times(b, i + 8, i + len, times),
            _ => {}
        }
        i += len;
    }
}

/// A big-endian 32-bit number, or nothing when the bytes run out.
fn be32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4).map(|w| u32::from_be_bytes(w.try_into().unwrap()))
}

/// A gigabyte of clips: music videos run to tens of megabytes each, so the cache holds a
/// few dozen and then starts letting the oldest go.
const CACHE_BUDGET: u64 = 1024 * 1024 * 1024;

/// Delete the clips downloaded longest ago until the directory fits `budget` again.
fn trim_cache(dir: &std::path::Path, budget: u64) {
    let mut clips: Vec<(std::time::SystemTime, u64, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((m.modified().ok()?, m.len(), e.path()))
        })
        .collect();
    let mut total: u64 = clips.iter().map(|(_, len, _)| *len).sum();
    if total <= budget {
        return;
    }
    clips.sort_by_key(|(at, _, _)| *at);
    for (_, len, path) in clips {
        if total <= budget {
            break;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => total -= len,
            // A clip still playing can't be dropped; the next watch tries again.
            Err(e) => log::info!("video: {} stayed in the cache ({e})", path.display()),
        }
    }
    log::info!("video: the cache holds {:.0} MB of clips", total as f64 / (1024.0 * 1024.0));
}

/// Pick a variant and save the clip to the cache; a saved copy is reused.
fn fetch(master: &str, max_width: u32) -> Option<PathBuf> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;
    rt.block_on(async {
        use sha1::{Digest, Sha1};
        let http = crate::proxy::http();
        let media = crate::apple::variant(&http, master, max_width).await?;
        let (init, segments) = crate::apple::media_parts(&http, &media).await?;
        // Apple's sleeves are fragmented MP4: a playlist with no header in it is not one
        // of theirs, and a headerless file decodes nothing at all.
        let init = init?;
        let dir = crate::data_dir().join("motion");
        std::fs::create_dir_all(&dir).ok()?;
        let name = format!("{:x}", Sha1::digest(init.url.as_bytes()));
        let path = dir.join(format!("{}.mp4", &name[..32]));
        if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > 1024 {
            return Some(path);
        }
        // The header and the pictures are written into one file: a clip with only the
        // header in it decodes nothing at all.
        let tmp = path.with_extension("part");
        let mut out = std::fs::File::create(&tmp).ok()?;
        let mut total = 0usize;
        let mut parts = Vec::with_capacity(segments.len() + 1);
        parts.push(init);
        parts.extend(segments);
        for part in &parts {
            total += pull(&http, part, None, &mut out).await?;
        }
        drop(out);
        if total < 1024 {
            std::fs::remove_file(&tmp).ok()?;
            return None;
        }
        std::fs::rename(&tmp, &path).ok()?;
        log::info!("apple: motion clip {:.0} KB cached", total as f32 / 1024.0);
        trim_cache(&dir, CACHE_BUDGET);
        Some(path)
    })
}

/// Append one piece of the clip to the file. Returns how many bytes it added. `ua` is the
/// client to ask as: Google's CDN refuses a piece it can't tie to one.
async fn pull(http: &reqwest::Client, part: &crate::apple::Part, ua: Option<&str>, out: &mut std::fs::File) -> Option<usize> {
    let mut req = http.get(&part.url).timeout(Duration::from_secs(60));
    if let Some(ua) = ua {
        req = req.header("User-Agent", ua);
    }
    if let Some((start, end)) = part.range {
        req = req.header("Range", format!("bytes={start}-{end}"));
    }
    let mut resp = req.send().await.ok()?.error_for_status().ok()?;
    let mut total = 0usize;
    while let Some(chunk) = resp.chunk().await.ok()? {
        std::io::Write::write_all(out, &chunk).ok()?;
        total += chunk.len();
    }
    Some(total)
}

/// One piece of the clip in hand rather than on the disk: the remux reads the transport
/// stream as it comes down, a piece at a time.
async fn pull_bytes(http: &reqwest::Client, part: &crate::apple::Part, ua: Option<&str>) -> Option<Vec<u8>> {
    let mut req = http.get(&part.url).timeout(Duration::from_secs(60));
    if let Some(ua) = ua {
        req = req.header("User-Agent", ua);
    }
    if let Some((start, end)) = part.range {
        req = req.header("Range", format!("bytes={start}-{end}"));
    }
    let resp = req.send().await.ok()?.error_for_status().ok()?;
    resp.bytes().await.ok().map(|b| b.to_vec())
}

#[cfg(windows)]
fn decode(motion: &Arc<Motion>, src: &str, ctx: &egui::Context, max_width: u32, at: Duration) {
    use windows::Win32::Media::MediaFoundation::{MFShutdown, MFStartup, MFSTARTUP_FULL, MF_VERSION};
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
    unsafe {
        if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
            return;
        }
        if MFStartup(MF_VERSION, MFSTARTUP_FULL).is_err() {
            CoUninitialize();
            return;
        }
        let mut wide: Vec<u16> = src.encode_utf16().collect();
        wide.push(0);
        let url = windows::core::PCWSTR(wide.as_ptr());
        // Each pass plays the clip once; then it starts again from the top. The song,
        // though, only starts at `at` once.
        let mut at = at;
        for _ in 0..100 {
            if motion.stop.load(Ordering::Relaxed) || !play_once(motion, ctx, url, max_width, std::mem::take(&mut at)) {
                break;
            }
        }
        let _ = MFShutdown();
        CoUninitialize();
    }
}

#[cfg(windows)]
unsafe fn play_once(motion: &Motion, ctx: &egui::Context, url: windows::core::PCWSTR, max_width: u32, at: Duration) -> bool {
    use windows::Win32::Media::MediaFoundation::*;
    let played = unsafe {
        (|| -> windows::core::Result<bool> {
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 1)?;
        let attrs = attrs.unwrap();
        // Lets the reader hand back RGB32 rather than the decoder's NV12.
        attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
        let reader = MFCreateSourceReaderFromURL(url, &attrs)?;
        let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
        reader.SetStreamSelection(stream, true)?;
        let ty = MFCreateMediaType()?;
        ty.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        ty.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
        // Decode straight to the size on screen: a native 1080p frame is ~4.6 MB of
        // RGBA and the texture holds another copy. The video processor does the scale.
        if max_width >= 2
            && let Ok(native) = reader.GetNativeMediaType(stream, 0)
            && let Ok(size) = native.GetUINT64(&MF_MT_FRAME_SIZE)
        {
            let (nw, nh) = ((size >> 32) as u32, (size & 0xffff_ffff) as u32);
            if nw > 0 && nh > 0 && max_width < nw {
                let w = max_width & !1;
                let h = (((nh as u64 * w as u64) / nw as u64) as u32 & !1).max(2);
                let _ = ty.SetUINT64(&MF_MT_FRAME_SIZE, ((w as u64) << 32) | h as u64);
            }
        }
        if reader.SetCurrentMediaType(stream, None, &ty).is_err() {
            // A clip that refuses the scaled type still plays at its native size.
            let ty = MFCreateMediaType()?;
            ty.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            ty.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
            reader.SetCurrentMediaType(stream, None, &ty)?;
        }
        let current = reader.GetCurrentMediaType(stream)?;
        let size = current.GetUINT64(&MF_MT_FRAME_SIZE)?;
        let (w, h) = ((size >> 32) as usize, (size & 0xffff_ffff) as usize);
        if w == 0 || h == 0 {
            return Ok(false);
        }
        let stride = current.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32).unwrap_or(-(w as i32 * 4));
        let row = w * 4;
        // On from where the song already is. Media Foundation's own seek lands on the
        // keyframe at or before the mark, and the frames between it and the mark are
        // decoded and dropped: a handful of them, rather than the clip's whole first half,
        // which is what made a video taken up mid-song stutter, fall behind and give up.
        let skip = at.as_secs_f64() * 1e7;
        if skip > 0.0 {
            // A reader that took the seek has a keyframe's worth left to drop; one that
            // refused is run through to the mark the slow way, as it always was.
            let mut left = if seek(&reader, stream, skip) { 600 } else { i64::MAX };
            loop {
                if motion.stop.load(Ordering::Relaxed) {
                    return Ok(false);
                }
                let (mut flags, mut ts) = (0u32, 0i64);
                let mut sample: Option<IMFSample> = None;
                reader.ReadSample(stream, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample))?;
                if flags & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
                    return Ok(false);
                }
                if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 || ts as f64 >= skip {
                    break;
                }
                left -= 1;
                if left < 0 {
                    break;
                }
            }
        }
        let mut start = Instant::now();
        let mut base: Option<i64> = None;
        let mut last: Option<Instant> = None;
        // The frame the last publish displaced, if nothing is drawing it any more: its
        // bytes become the next frame's buffer, so a clip isn't reallocated 8 MB a second.
        let mut spare: Option<Arc<Frame>> = None;
        loop {
            if motion.stop.load(Ordering::Relaxed) {
                return Ok(false);
            }
            // Paused with the song: no sample is taken until it moves again, and the clock
            // the frames are paced by gives the waiting back, so the picture holds its place.
            if motion.pause.load(Ordering::Relaxed) {
                let waited = Instant::now();
                while motion.pause.load(Ordering::Relaxed) && !motion.stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(50));
                }
                if motion.stop.load(Ordering::Relaxed) {
                    return Ok(false);
                }
                start += waited.elapsed();
                last = None;
            }
            let (mut flags, mut ts) = (0u32, 0i64);
            let mut sample: Option<IMFSample> = None;
            reader.ReadSample(stream, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample))?;
            if flags & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
                return Ok(false);
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                return Ok(true);
            }
            let Some(sample) = sample else { continue };
            let base = *base.get_or_insert(ts);
            motion.at.store(ts.max(0) as u64, Ordering::Relaxed);
            // Timestamps are in 100 ns units.
            let due = Duration::from_micros(((ts - base).max(0) / 10) as u64);
            let elapsed = start.elapsed();
            if due > elapsed {
                std::thread::sleep(due - elapsed);
            }
            // 30 fps is plenty for a sleeve; faster clips would only churn uploads.
            if last.is_some_and(|t| t.elapsed() < Duration::from_millis(33)) {
                continue;
            }
            last = Some(Instant::now());
            let buffer = sample.ConvertToContiguousBuffer()?;
            let (mut src, mut len) = (std::ptr::null_mut(), 0u32);
            buffer.Lock(&mut src, None, Some(&mut len))?;
            let mut rgba = spare.take().and_then(|f| Arc::try_unwrap(f).ok()).map(|f| f.rgba).unwrap_or_default();
            rgba.resize(row * h, 0);
            if !src.is_null() && len as usize >= row * h {
                let data = std::slice::from_raw_parts(src, len as usize);
                let (stride, bottom_up) = layout(w, h, data.len(), stride);
                for y in 0..h {
                    let o = if bottom_up { h - 1 - y } else { y } * stride;
                    if o + row > data.len() {
                        break;
                    }
                    // Swapped a word at a time: the same RGB→RGBA turn, four bytes per step.
                    let (from, to) = (&data[o..o + row], &mut rgba[y * row..y * row + row]);
                    for (d, s) in to.chunks_exact_mut(4).zip(from.chunks_exact(4)) {
                        let v = u32::from_le_bytes(s.try_into().unwrap());
                        let x = 0xFF00_0000u32 | ((v & 0xFF) << 16) | (v & 0xFF00) | ((v >> 16) & 0xFF);
                        d.copy_from_slice(&x.to_le_bytes());
                    }
                }
            }
            buffer.Unlock()?;
            if motion.version.load(Ordering::Acquire) == 0 {
                log::info!("apple: motion playing at {w}x{h}");
            }
            spare = motion.publish(ctx, w, h, rgba);
        }
    })()
    };
    match played {
        Ok(ok) => ok,
        Err(e) => {
            log::warn!("apple: motion decode failed: {e}");
            false
        }
    }
}

/// Media Foundation's seek to a point in the clip, in 100 ns units from its start, and a
/// flush of what the reader had already buffered. The source reader answers with the
/// nearest keyframe at or before the mark, so the caller still drops what lies between it
/// and the mark. A zero GUID is the reader's own default clock, `MFTimeFormat`; the two
/// constants the metadata would have needed for anything else aren't projected by the
/// windows crate, and nothing else would do.
#[cfg(windows)]
unsafe fn seek(reader: &windows::Win32::Media::MediaFoundation::IMFSourceReader, stream: u32, to: f64) -> bool {
    use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
    use windows::Win32::System::Variant::VT_I8;
    let mut pos = PROPVARIANT::default();
    unsafe {
        (*pos.Anonymous.Anonymous).vt = VT_I8;
        (*pos.Anonymous.Anonymous).Anonymous.hVal = to as i64;
    }
    let clock = windows::core::GUID::zeroed();
    if unsafe { reader.SetCurrentPosition(&clock, &pos) }.is_err() {
        log::info!("video: the clip wouldn't seek");
        return false;
    }
    unsafe { reader.Flush(stream) }.is_ok()
}

#[cfg(not(windows))]
fn decode(_motion: &Arc<Motion>, _src: &str, _ctx: &egui::Context, _max_width: u32, _at: Duration) {}

/// The frame buffer's real row length in bytes, and whether its rows run bottom-up. Media
/// Foundation's video processor pads the frame (a 1080 px clip comes back 1088 px wide, rows
/// 4352 bytes apart) but still reports the unpadded stride, which would shear every row by
/// 8 px; the buffer's length gives the padded one away.
#[cfg(windows)]
fn layout(w: usize, h: usize, len: usize, reported: i32) -> (usize, bool) {
    let row = w * 4;
    let bottom_up = reported < 0;
    let reported = if reported == 0 { row as i32 } else { reported }.unsigned_abs() as usize;
    let fits = |s: usize| s >= row && len % s == 0 && (h..h + 64).contains(&(len / s));
    if fits(reported) {
        return (reported, bottom_up);
    }
    for rows in h..h + 64 {
        let s = len / rows;
        if len % rows == 0 && s >= row && s <= row + 256 {
            return (s, bottom_up);
        }
    }
    (reported.max(row), bottom_up)
}

/// Walk a clip on disk once, handing the kept frames to `frame` as (w, h, rgba, delay ms)
/// — the Discord card's animated artwork is encoded from exactly these. Frames are
/// sampled to `fps` and never wider than `side`, so a bounded GIF comes out of a big clip.
#[cfg(windows)]
pub fn visit_clip(src: &Path, side: u32, fps: u8, mut frame: impl FnMut(usize, usize, Vec<u8>, u32) -> anyhow::Result<()>) -> anyhow::Result<()> {
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe {
                let _ = MFShutdown();
                CoUninitialize();
            }
        }
    }
    unsafe {
        anyhow::ensure!(CoInitializeEx(None, COINIT_MULTITHREADED).is_ok(), "COM wouldn't start");
        anyhow::ensure!(MFStartup(MF_VERSION, MFSTARTUP_FULL).is_ok(), "Media Foundation wouldn't start");
        let _guard = Guard;
        let mut wide: Vec<u16> = src.to_string_lossy().encode_utf16().collect();
        wide.push(0);
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 1)?;
        let attrs = attrs.unwrap();
        attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
        let reader = MFCreateSourceReaderFromURL(windows::core::PCWSTR(wide.as_ptr()), &attrs)?;
        let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
        reader.SetStreamSelection(stream, true)?;
        let ty = MFCreateMediaType()?;
        ty.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        ty.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
        if side >= 2
            && let Ok(native) = reader.GetNativeMediaType(stream, 0)
            && let Ok(size) = native.GetUINT64(&MF_MT_FRAME_SIZE)
        {
            let (nw, nh) = ((size >> 32) as u32, (size & 0xffff_ffff) as u32);
            let wide_side = side.max(side * 16 / 9); // wide clips keep their shape
            let fit = |w: u32, h: u32| {
                if w.max(h) <= wide_side {
                    (w, h)
                } else {
                    let k = wide_side as f64 / w.max(h) as f64;
                    (((w as f64 * k) as u32 & !1).max(2), ((h as f64 * k) as u32 & !1).max(2))
                }
            };
            if nw > 0 && nh > 0 {
                let (w, h) = fit(nw, nh);
                let _ = ty.SetUINT64(&MF_MT_FRAME_SIZE, ((w as u64) << 32) | h as u64);
            }
        }
        if reader.SetCurrentMediaType(stream, None, &ty).is_err() {
            let ty = MFCreateMediaType()?;
            ty.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            ty.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
            reader.SetCurrentMediaType(stream, None, &ty)?;
        }
        let current = reader.GetCurrentMediaType(stream)?;
        let size = current.GetUINT64(&MF_MT_FRAME_SIZE)?;
        let (w, h) = ((size >> 32) as usize, (size & 0xffff_ffff) as usize);
        anyhow::ensure!(w > 0 && h > 0, "the clip names no picture size");
        let stride = current.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32).unwrap_or(-(w as i32 * 4));
        let row = w * 4;
        let interval = (1000.0 / fps.max(1) as f64).max(33.0);
        let mut kept: Vec<i64> = Vec::new();
        let mut next_ms = 0.0f64;
        let mut previous: Option<(usize, usize, Vec<u8>)> = None;
        loop {
            let (mut flags, mut ts) = (0u32, 0i64);
            let mut sample: Option<IMFSample> = None;
            reader.ReadSample(stream, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample))?;
            anyhow::ensure!(flags & MF_SOURCE_READERF_ERROR.0 as u32 == 0, "the clip stopped decoding");
            let at_ms = (ts.max(0) as f64 / 10_000.0) as u32;
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                let length_ms = kept.last().copied().unwrap_or(0).max(at_ms as i64) as u32;
                if let Some((w, h, rgba)) = previous.take() {
                    frame(w, h, rgba, (length_ms.saturating_sub(*kept.last().unwrap_or(&0) as u32)).max(20))?;
                }
                anyhow::ensure!(kept.len() >= 2, "the clip has no animation");
                return Ok(());
            }
            let Some(sample) = sample else { continue };
            if (at_ms as f64 + 0.5 < next_ms && !kept.is_empty()) || kept.len() >= 360 {
                continue;
            }
            let buffer = sample.ConvertToContiguousBuffer()?;
            let (mut src, mut len) = (std::ptr::null_mut(), 0u32);
            buffer.Lock(&mut src, None, Some(&mut len))?;
            anyhow::ensure!(!src.is_null() && len as usize >= row * h, "a frame came back too small");
            let data = std::slice::from_raw_parts(src, len as usize);
            let (stride, bottom_up) = layout(w, h, data.len(), stride);
            let mut rgba = vec![0u8; row * h];
            for y in 0..h {
                let o = if bottom_up { h - 1 - y } else { y } * stride;
                if o + row > data.len() {
                    break;
                }
                let (from, to) = (&data[o..o + row], &mut rgba[y * row..y * row + row]);
                for (d, s) in to.chunks_exact_mut(4).zip(from.chunks_exact(4)) {
                    let v = u32::from_le_bytes(s.try_into().unwrap());
                    let x = 0xFF00_0000u32 | ((v & 0xFF) << 16) | (v & 0xFF00) | ((v >> 16) & 0xFF);
                    d.copy_from_slice(&x.to_le_bytes());
                }
            }
            buffer.Unlock()?;
            let delay = kept.last().map_or(40, |last| (i64::from(at_ms) - last).max(20) as u32);
            if let Some((pw, ph, prgba)) = previous.replace((w, h, rgba)) {
                frame(pw, ph, prgba, delay)?;
            }
            kept.push(at_ms as i64);
            next_ms = (kept.len() as f64 * interval).max(at_ms as f64 + 1.0);
        }
    }
}

/// The animated artwork needs a clip decoder; other platforms still draw the still.
#[cfg(not(windows))]
pub fn visit_clip(_src: &Path, _side: u32, _fps: u8, _frame: impl FnMut(usize, usize, Vec<u8>, u32) -> anyhow::Result<()>) -> anyhow::Result<()> {
    anyhow::bail!("animated artwork needs this platform's clip decoder")
}

/// The name a clip is cached under is the one part of the cache's address that an answer
/// off the network gets to choose.
#[cfg(test)]
mod names {
    use super::video_name;

    #[test]
    fn only_an_id_of_its_own_shape_reaches_the_disk() {
        assert_eq!(video_name("dQw4w9WgXcQ").as_deref(), Some("dQw4w9WgXcQ.mp4"));
        assert_eq!(video_name("_a-b9C0dEfG").as_deref(), Some("_a-b9C0dEfG.mp4"));
        for bad in ["", "a", "..", "dQw4w9WgX", "dQw4w9WgXcQx", "a/b/c/dEfGh", "a\\b9C0dEfGh", "a b9C0dEfG", "a:b9C0dEfGh", "a.mp4x9C0dE", "a?b9C0dEfGh", "éQw4w9WgXcQ"] {
            assert_eq!(video_name(bad), None, "{bad} isn't a video id");
        }
    }
}

/// One ISO BMFF box, as the header of a clip would carry it.
#[cfg(test)]
fn boxed(name: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    b.extend_from_slice(name);
    b.extend_from_slice(body);
    b
}

#[cfg(test)]
mod edits {
    use super::{boxed, edit_times};

    /// A clip header of our own making: `moov > trak > edts > elst`, with a decoy `elst`
    /// written in the picture data of the `mdat` after it.
    fn clip(version: u8, media_time: u64) -> Vec<u8> {
        let mut body = vec![version, 0, 0, 0, 0, 0, 0, 1];
        if version == 1 {
            body.extend_from_slice(&2726912u64.to_be_bytes());
        } else {
            body.extend_from_slice(&2726912u32.to_be_bytes());
        }
        let wide = if version == 1 { 8 } else { 4 };
        body.extend_from_slice(&media_time.to_be_bytes()[8 - wide..]);
        body.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        let trak = boxed(b"trak", &[boxed(b"tkhd", &[0u8; 84]), boxed(b"edts", &boxed(b"elst", &body))].concat());
        let mut b = boxed(b"ftyp", b"isomiso");
        b.extend(boxed(b"moov", &trak));
        b.extend(boxed(b"mdat", b"elst picture elst"));
        b
    }

    #[test]
    fn the_media_time_is_found_where_it_hides() {
        for (version, time, width) in [(0u8, 512u64, 4usize), (1, 1024, 8)] {
            let b = clip(version, time);
            let mut times = Vec::new();
            edit_times(&b, 0, b.len(), &mut times);
            assert_eq!(times.len(), 1, "the elst inside the picture isn't an edit list");
            let (at, wide) = times[0];
            assert_eq!(wide, width, "version {version} media times are {width} bytes");
            assert_eq!(&b[at..at + width], &time.to_be_bytes()[8 - width..], "that isn't the media time");
        }
    }

    /// The entry count arrives off the network like everything else in the file, so a
    /// number bigger than the box holds is read only as far as the box goes.
    #[test]
    fn an_impossible_entry_count_stops_at_the_box() {
        let mut b = clip(0, 512);
        let mut times = Vec::new();
        edit_times(&b, 0, b.len(), &mut times);
        let (first, _) = times[0];
        b[first - 8..first - 4].copy_from_slice(&16_777_216u32.to_be_bytes());
        times.clear();
        edit_times(&b, 0, b.len(), &mut times);
        assert_eq!(times.len(), 1, "only the entries the box really holds are counted");
    }
}

/// Media Foundation decoding of a real cached sleeve. Needs a clip in the cache (play
/// something in the app first): `cargo test --release -- --ignored decodes_a_cached_clip`.
#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// What YouTube Music's visionOS client hands out for one video, through the app's own
    /// client. The address it returns is signed, so it is used and never written down.
    fn stream_of(id: &str, width: u32) -> crate::innertube::Video {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            let api = crate::ytmapi::Api::new(&crate::data_dir());
            let vd = api.visitor_data(false).await.expect("no visitor id");
            crate::innertube::video_stream(&crate::proxy::http(), id, &vd, width).await.expect("no video stream")
        })
    }

    #[test]
    #[ignore]
    fn decodes_a_cached_clip() {
        let dir = crate::data_dir().join("motion");
        let path = std::fs::read_dir(&dir)
            .expect("no motion cache")
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "mp4"))
            .expect("no cached clip");
        let ctx = egui::Context::default();
        let motion = Arc::new(Motion::new("test"));
        let this = motion.clone();
        std::thread::spawn(move || decode(&this, &path.to_string_lossy(), &ctx, 512, Duration::ZERO));
        let start = Instant::now();
        while motion.latest().is_none() && start.elapsed() < Duration::from_secs(15) {
            std::thread::sleep(Duration::from_millis(50));
        }
        motion.stop();
        let (_, frame) = motion.latest().expect("no frame decoded");
        assert!(frame.w > 0 && frame.h > 0, "empty frame");
        assert_eq!(frame.rgba.len(), frame.w * frame.h * 4);
    }

    /// The music-video player's download on its own, timed: how long a song waits for its first
    /// frame is a real limit on the feature, so it gets measured rather than guessed.
    /// Needs the network: `cargo test --release -- --ignored downloads_a_music_video`.
    #[test]
    #[ignore]
    fn downloads_a_music_video() {
        let stream = stream_of("dQw4w9WgXcQ", 1280);
        // The URL is signed: report only what it says about the picture.
        println!("video: {}x{}, {:.1} MB", stream.width, stream.height, stream.size as f64 / 1_048_576.0);
        let start = Instant::now();
        let path = save("dQw4w9WgXcQ", &stream.url).expect("the clip didn't download");
        let bytes = std::fs::metadata(&path).expect("no clip on disk").len();
        let secs = start.elapsed().as_secs_f64();
        println!("video: {:.1} MB in {secs:.1} s, {:.1} MB/s", bytes as f64 / 1_048_576.0, bytes as f64 / 1_048_576.0 / secs.max(0.1));
        assert!(bytes > 1_048_576, "the clip is too small to be a video");
    }

    /// The music-video player's whole path for a real music video: the stream the visionOS client
    /// hands out, downloaded, decoded, and framed — then the same file decoded again,
    /// which is what a song skipped back to does. Needs the network:
    /// `cargo test --release -- --ignored plays_a_music_video`.
    #[test]
    #[ignore]
    fn plays_a_music_video() {
        downloads_a_music_video();
        let stream = stream_of("dQw4w9WgXcQ", 1280);
        let ctx = egui::Context::default();
        let start = Instant::now();
        let motion = play_video(&ctx, "dQw4w9WgXcQ", &stream.url, stream.hls.as_deref(), 1280, Duration::from_secs(20));
        while motion.latest().is_none() && start.elapsed() < Duration::from_secs(90) {
            std::thread::sleep(Duration::from_millis(50));
        }
        println!("video: first frame after {:.1} s", start.elapsed().as_secs_f32());
        let (_, frame) = motion.latest().expect("no frame from the music video");
        assert!(frame.w > 0 && frame.h > 0, "empty frame");
        assert_eq!(frame.rgba.len(), frame.w * frame.h * 4);
        // Thirty seconds on from the top, and it keeps moving.
        let at = motion.position();
        std::thread::sleep(Duration::from_secs(5));
        let (frames, moved) = (motion.version.load(Ordering::Acquire), motion.position());
        motion.stop();
        println!("video: {frames} frames in 5 s, {:.1} s to {:.1} s of the clip", at.as_secs_f32(), moved.as_secs_f32());
        assert!(moved > at, "the picture isn't moving");
        assert!(frames > 40, "the video isn't keeping up");
    }

    /// The playlist's transport-stream pieces, remuxed into the mp4 Media Foundation
    /// reads and framed from it. Point YTM_DATA_DIR at an empty folder first, so the
    /// pieces really come down rather than a cached file standing in:
    /// `YTM_DATA_DIR=... cargo test --release -- --ignored remuxes_a_playlist --nocapture`
    #[test]
    #[ignore]
    fn remuxes_a_playlist() {
        let stream = stream_of("dQw4w9WgXcQ", 1280);
        let master = stream.hls.as_deref().expect("the answer holds no playlist");
        let path = save_hls("dQw4w9WgXcQ", master, 1280).expect("no mp4 out of the playlist");
        let bytes = std::fs::metadata(&path).expect("no file on disk").len();
        let mut tag = [0u8; 8];
        use std::io::Read;
        std::fs::File::open(&path).expect("the file won't open").read_exact(&mut tag).expect("a short file");
        assert_eq!(&tag[4..], b"ftyp", "the file opens with a file-type box");
        println!("remux: {bytes} bytes at {}", path.display());
        let ctx = egui::Context::default();
        let motion = Arc::new(Motion::new("remux"));
        let (this, ctx2, path2) = (motion.clone(), ctx.clone(), path.clone());
        let worker = std::thread::spawn(move || decode(&this, &path2.to_string_lossy(), &ctx2, 1280, Duration::ZERO));
        let start = Instant::now();
        while motion.latest().is_none() && start.elapsed() < Duration::from_secs(60) {
            std::thread::sleep(Duration::from_millis(50));
        }
        let (_, frame) = motion.latest().expect("no frame from the remuxed clip");
        println!("remux: first picture {}x{} after {:.1} s", frame.w, frame.h, start.elapsed().as_secs_f32());
        assert!(frame.w > 0 && frame.h > 0, "empty frame");
        motion.stop();
        let _ = worker.join();
    }

    /// One Media Foundation pass over a clip: open it, ask for a picture in one shape or
    /// another, and say how many frames came back and what the pipeline reported.
    #[cfg(windows)]
    unsafe fn mf_pass(label: &str, path: &std::path::Path, processing: bool, dxva_off: bool, rgb: bool) {
        use windows::core::{Interface, PCWSTR};
        use windows::Win32::Media::MediaFoundation::*;
        let mut wide: Vec<u16> = path.to_string_lossy().encode_utf16().collect();
        wide.push(0);
        let mut attrs = None;
        let _ = MFCreateAttributes(&mut attrs, 2);
        let Some(attrs) = attrs else { return };
        if processing {
            let _ = attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1);
        }
        if dxva_off {
            let _ = attrs.SetUINT32(&MF_SOURCE_READER_DISABLE_DXVA, 1);
        }
        let Ok(reader) = MFCreateSourceReaderFromURL(PCWSTR(wide.as_ptr()), &attrs) else {
            println!("{label}: open failed");
            return;
        };
        let video = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        let _ = reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false);
        let _ = reader.SetStreamSelection(video, true);
        if rgb {
            let ty = MFCreateMediaType().expect("media type");
            let _ = ty.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video);
            let _ = ty.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32);
            println!("{label}: set RGB32 -> {:?}", reader.SetCurrentMediaType(video, None, &ty).ok());
        }
        let mut got = 0;
        let mut end = "";
        for _ in 0..8 {
            let (mut flags, mut ts) = (0u32, 0i64);
            let mut sample: Option<IMFSample> = None;
            if reader.ReadSample(video, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample)).is_err() {
                end = "call failed";
                break;
            }
            if flags & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
                end = "ERROR flag";
                break;
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                end = "end of stream";
                break;
            }
            got += sample.is_some() as u8;
        }
        println!("{label}: {got} samples, {end}");
        if let Ok(events) = reader.cast::<IMFMediaEventGenerator>() {
            for _ in 0..4 {
                let Ok(event) = events.GetEvent(MF_EVENT_FLAG_NO_WAIT) else { break };
                println!("{label}: event {:?} status {:?}", event.GetType(), event.GetStatus());
            }
        }
    }

    /// Diagnosis only: what Media Foundation makes of the cached music video, against the
    /// Apple sleeve that is known to work. YTM_MF_CLIP points it at one file instead.
    /// `cargo test --release -- --ignored mf_reads_the_cached_clip --nocapture`
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn mf_reads_the_cached_clip() {
        use windows::Win32::Media::MediaFoundation::{MFShutdown, MFStartup, MFSTARTUP_FULL, MF_VERSION};
        use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
        let cached = |dir: &str| {
            std::fs::read_dir(crate::data_dir().join(dir))
                .ok()?
                .flatten()
                .map(|e| e.path())
                .find(|p| p.extension().is_some_and(|e| e == "mp4"))
        };
        let clips: Vec<(String, PathBuf)> = match std::env::var("YTM_MF_CLIP") {
            Ok(path) => vec![("clip".to_owned(), PathBuf::from(path))],
            Err(_) => [cached("video"), cached("motion")]
                .into_iter()
                .flatten()
                .enumerate()
                .map(|(i, p)| (if i == 0 { "youtube".to_owned() } else { "sleeve".to_owned() }, p))
                .collect(),
        };
        assert!(!clips.is_empty(), "no clip to read");
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
            for (label, path) in &clips {
                mf_pass(label, path, true, false, true);
                mf_pass(&format!("{label}, native"), path, true, false, false);
            }
            let _ = MFShutdown();
            CoUninitialize();
        }
    }
}
