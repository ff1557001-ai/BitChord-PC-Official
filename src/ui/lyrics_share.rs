use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use egui::{Color32, ColorImage, FontDefinitions, FontId, Pos2, Rect, TextureHandle, pos2};
use egui::epaint::text::{Fonts, Galley};
use image::{Rgba, RgbaImage, imageops};

use crate::lyrics::Lyrics;
use crate::model::{self, Track};
use crate::theme;
use crate::translate::Extras;

use super::{lyric_prefs, notes};

// Kotlin String.length in BitChord counts UTF-16 code units, not bytes or scalars.
const BUDGET: usize = 250;
const WIDTH: u32 = 1080;
const MARGIN: f32 = 72.0;
const CONTENT_WIDTH: f32 = WIDTH as f32 - 2.0 * MARGIN;
const BODY_TOP: f32 = 190.0;
const FOOTER_HEIGHT: f32 = 276.0;
// Keep pathological provider extras from causing an unbounded bitmap allocation.
const MAX_HEIGHT: u32 = 16_384;
const LADDER: &[f32] = &[96.0, 88.0, 80.0, 72.0, 64.0, 56.0, 50.0, 44.0, 38.0, 32.0];

#[derive(Clone, Debug)]
struct PickLine {
    text: String,
    roman: Option<String>,
    translation: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Passage {
    Line { text: String, sub: Vec<String> },
    Gap,
}

#[derive(Default)]
struct Selection {
    picks: BTreeSet<usize>,
    used: usize,
}

impl Selection {
    /// Returns false when the line would exceed the budget; existing picks stay intact.
    fn toggle(&mut self, lines: &[PickLine], index: usize) -> bool {
        let Some(line) = lines.get(index) else { return false };
        let count = line.text.encode_utf16().count();
        if self.picks.remove(&index) {
            self.used -= count;
        } else if count <= BUDGET.saturating_sub(self.used) {
            self.picks.insert(index);
            self.used += count;
        } else {
            return false;
        }
        true
    }

    fn passage(&self, lines: &[PickLine], roman: bool, translation: bool) -> Vec<Passage> {
        let mut out = Vec::new();
        let mut previous = None;
        for &index in &self.picks {
            let Some(line) = lines.get(index) else { continue };
            if previous.is_some_and(|p| index > p + 1) {
                out.push(Passage::Gap);
            }
            let mut sub: Vec<String> = Vec::new();
            for extra in [(roman, &line.roman), (translation, &line.translation)] {
                if let (true, Some(text)) = extra
                    && !text.trim().is_empty()
                    && !text.trim().eq_ignore_ascii_case(line.text.trim())
                    && !sub.iter().any(|s| s.trim().eq_ignore_ascii_case(text.trim()))
                {
                    sub.push(text.clone());
                }
            }
            out.push(Passage::Line { text: line.text.clone(), sub });
            previous = Some(index);
        }
        out
    }
}

#[derive(Clone)]
struct Cover {
    bytes: egui::load::Bytes,
    uv: Rect,
}

enum Artwork {
    None,
    Loading { uri: String, uv: Rect },
    LoadingFile { rx: mpsc::Receiver<Option<Vec<u8>>>, uv: Rect },
    Ready(Cover),
    Failed,
}

impl Artwork {
    fn loading(&self) -> bool {
        matches!(self, Self::Loading { .. } | Self::LoadingFile { .. })
    }
}

struct Rendered {
    image: RgbaImage,
    preview: RgbaImage,
}

enum Work {
    Render(mpsc::Receiver<Result<Rendered, String>>),
    Save(mpsc::Receiver<Result<Option<std::path::PathBuf>, String>>),
}

/// Owned, independent dialog state. Keep it until `show` clears the enclosing Option.
pub(super) struct LyricsShare {
    title: String,
    artist: String,
    lines: Vec<PickLine>,
    selection: Selection,
    roman: bool,
    translation: bool,
    artwork: Artwork,
    image: Option<Arc<RgbaImage>>,
    texture: Option<TextureHandle>,
    work: Option<Work>,
    message: Option<String>,
}

pub(super) fn open(
    track: &Track,
    lyrics: &Lyrics,
    extras: Option<&Extras>,
    roman: bool,
    translation: bool,
    target: &str,
    artwork_uri: Option<&str>,
) -> LyricsShare {
    let lines = lyrics.lines.iter().enumerate().filter(|(_, l)| !l.instrumental && !l.text.trim().is_empty()).map(|(i, line)| {
        let roman = line.romanization.as_ref().filter(|s| !s.trim().is_empty() && !s.trim().eq_ignore_ascii_case(line.text.trim()))
            .or_else(|| extras.and_then(|e| e.roman.get(i)).and_then(Option::as_ref)).cloned();
        let translation = lyric_prefs::shipped_translation(line, target).filter(|s| !s.trim().is_empty())
            .map(str::to_owned)
            .or_else(|| extras.and_then(|e| e.trans.get(i)).and_then(Option::as_ref).cloned());
        PickLine { text: line.text.clone(), roman, translation }
    }).collect();
    let cover = artwork_uri.filter(|s| !s.is_empty()).map(|uri| (uri.to_owned(), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))))
        .or_else(|| model::pick_thumb(&track.thumbs, 544).map(|t| (model::sized_square(&t.url, 544), t.square_uv())));
    LyricsShare {
        title: track.title.clone(), artist: track.artist_line(), lines, selection: Selection::default(),
        roman, translation,
        artwork: cover.map_or(Artwork::None, |(uri, uv)| load_artwork(uri, uv)),
        image: None, texture: None, work: None, message: None,
    }
}

fn load_artwork(uri: String, uv: Rect) -> Artwork {
    if uri.starts_with("http://") || uri.starts_with("https://") || uri.starts_with("bytes://") {
        return Artwork::Loading { uri, uv };
    }
    let path = if uri.starts_with("file://") {
        url::Url::parse(&uri).ok().and_then(|u| u.to_file_path().ok())
    } else if !uri.contains("://") {
        Some(std::path::PathBuf::from(&uri))
    } else {
        None
    };
    let Some(path) = path else { return Artwork::Loading { uri, uv } };
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let bytes = std::fs::metadata(&path).ok().filter(|m| m.len() <= 16 * 1024 * 1024)
            .and_then(|_| std::fs::read(path).ok()).filter(|b| b.len() <= 16 * 1024 * 1024);
        let _ = tx.send(bytes);
    });
    Artwork::LoadingFile { rx, uv }
}

pub(super) fn show(ctx: &egui::Context, slot: &mut Option<LyricsShare>) {
    let Some(state) = slot.as_mut() else { return };
    state.poll(ctx);
    let mut close = false;
    let modal = egui::Modal::new(egui::Id::unique("lyrics-share"))
        .frame(notes::surface()).backdrop_color(theme::shade(0.55)).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 48.0).clamp(160.0, 540.0));
            egui::Frame::new().inner_margin(egui::Margin { left: 24, right: 24, top: 20, bottom: 20 }).show(ui, |ui| {
                ui.label(egui::RichText::new(if state.image.is_some() { "Lyrics card" } else { "Share lyrics" }).font(theme::bold(22.0)).color(theme::text()));
                ui.add_space(6.0);
                ui.label(egui::RichText::new(format!("{} · {}", state.title, state.artist)).font(theme::regular(13.0)).color(theme::text_dim()));
                ui.add_space(12.0);
                if let Some(texture) = &state.texture {
                    let size = texture.size_vec2();
                    let max_w = ui.available_width();
                    let max_h = (ctx.content_rect().height() - 260.0).clamp(100.0, 520.0);
                    let scale = (max_w / size.x).min(max_h / size.y).min(1.0);
                    ui.vertical_centered(|ui| {
                        ui.add(egui::Image::new((texture.id(), size * scale)));
                    });
                } else {
                    ui.label(egui::RichText::new("Select the lines to include. Tap a selected line again to remove it.").color(theme::text_dim()));
                    ui.add_space(8.0);
                    let max_h = (ctx.content_rect().height() - 340.0).clamp(100.0, 420.0);
                    egui::ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                        for (i, line) in state.lines.iter().enumerate() {
                            let chosen = state.selection.picks.contains(&i);
                            let label = format!("{}  {}", if chosen { "☑" } else { "☐" }, line.text);
                            if ui.add_enabled(state.work.is_none(), egui::Button::new(label).selected(chosen).wrap().min_size(egui::vec2(ui.available_width(), 36.0))).clicked() {
                                state.message = if state.selection.toggle(&state.lines, i) { None } else { Some("This line would exceed the 250-character limit.".into()) };
                            }
                        }
                    });
                    if state.lines.is_empty() {
                        ui.label("No lyric lines are available.");
                    }
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new(format!("{} / {} characters", state.selection.used, BUDGET)).color(theme::text_dim()));
                    ui.add_enabled_ui(state.work.is_none(), |ui| {
                        ui.horizontal_wrapped(|ui| {
                            if state.lines.iter().any(|l| l.roman.as_ref().is_some_and(|s| !s.trim().is_empty())) {
                                ui.checkbox(&mut state.roman, "Romanization");
                            }
                            if state.lines.iter().any(|l| l.translation.as_ref().is_some_and(|s| !s.trim().is_empty())) {
                                ui.checkbox(&mut state.translation, "Translation");
                            }
                        });
                    });
                    if state.artwork.loading() {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Loading artwork…");
                            if ui.button("Continue without artwork").clicked() { state.artwork = Artwork::None; }
                        });
                    } else if matches!(state.artwork, Artwork::Failed) {
                        ui.label(egui::RichText::new("Artwork unavailable; the card will use a placeholder.").color(theme::text_dim()));
                    }
                }
                if state.work.is_some() {
                    ui.horizontal(|ui| { ui.spinner(); ui.label(if state.image.is_some() { "Saving PNG…" } else { "Creating card…" }); });
                }
                if let Some(message) = &state.message {
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new(message).color(theme::text_dim()));
                }
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(state.work.is_none(), egui::Button::new(if state.image.is_some() { "Back" } else { "Cancel" })).clicked() {
                        if state.image.is_some() {
                            state.image = None;
                            state.texture = None;
                            state.message = None;
                        } else { close = true; }
                    }
                    if state.image.is_some() {
                        if ui.add_enabled(state.work.is_none(), egui::Button::new("Copy image")).clicked()
                            && let Some(image) = &state.image
                        {
                            ctx.copy_image(ColorImage::from_rgba_unmultiplied([image.width() as usize, image.height() as usize], image.as_raw()));
                            state.message = Some("Image copy requested".into());
                        }
                        if ui.add_enabled(state.work.is_none(), egui::Button::new("Save PNG")).clicked() { state.save(ctx); }
                        if ui.add_enabled(state.work.is_none(), egui::Button::new("Done")).clicked() { close = true; }
                    } else if ui.add_enabled(state.work.is_none() && !state.selection.picks.is_empty() && !state.artwork.loading(), egui::Button::new("Create card")).clicked() {
                        state.render(ctx);
                    }
                });
            });
        });
    if state.work.is_none() && (close || modal.should_close() || ctx.input(|i| i.key_pressed(egui::Key::Escape))) {
        *slot = None;
    }
}

impl LyricsShare {
    fn poll(&mut self, ctx: &egui::Context) {
        match &self.artwork {
            Artwork::Loading { uri, uv } => match ctx.try_load_bytes(uri) {
                Ok(egui::load::BytesPoll::Ready { bytes, .. }) if bytes.as_ref().len() <= 16 * 1024 * 1024 => self.artwork = Artwork::Ready(Cover { bytes, uv: *uv }),
                Ok(egui::load::BytesPoll::Pending { .. }) => ctx.request_repaint_after(Duration::from_millis(250)),
                _ => self.artwork = Artwork::Failed,
            },
            Artwork::LoadingFile { rx, uv } => match rx.try_recv() {
                Ok(Some(bytes)) => self.artwork = Artwork::Ready(Cover { bytes: bytes.into(), uv: *uv }),
                Ok(None) | Err(mpsc::TryRecvError::Disconnected) => self.artwork = Artwork::Failed,
                Err(mpsc::TryRecvError::Empty) => ctx.request_repaint_after(Duration::from_millis(100)),
            },
            _ => {}
        }
        let Some(work) = self.work.take() else { return };
        match work {
            Work::Render(rx) => match rx.try_recv() {
                Ok(Ok(rendered)) => {
                    let preview = rendered.preview;
                    let color = ColorImage::from_rgba_unmultiplied([preview.width() as usize, preview.height() as usize], preview.as_raw());
                    self.texture = Some(ctx.load_texture("lyrics-share-card", color, egui::TextureOptions::LINEAR));
                    self.image = Some(Arc::new(rendered.image));
                    self.message = None;
                }
                Ok(Err(e)) => self.message = Some(e),
                Err(mpsc::TryRecvError::Empty) => self.work = Some(Work::Render(rx)),
                Err(mpsc::TryRecvError::Disconnected) => self.message = Some("Could not create the card.".into()),
            },
            Work::Save(rx) => match rx.try_recv() {
                Ok(Ok(Some(path))) => self.message = Some(format!("Saved PNG to {}", path.display())),
                Ok(Ok(None)) => self.message = None,
                Ok(Err(e)) => self.message = Some(e),
                Err(mpsc::TryRecvError::Empty) => self.work = Some(Work::Save(rx)),
                Err(mpsc::TryRecvError::Disconnected) => self.message = Some("Could not save the PNG.".into()),
            },
        }
        if self.work.is_some() { ctx.request_repaint_after(Duration::from_millis(100)); }
    }

    fn render(&mut self, ctx: &egui::Context) {
        let card = Card {
            title: self.title.clone(), artist: self.artist.clone(),
            passage: self.selection.passage(&self.lines, self.roman, self.translation),
            cover: match &self.artwork { Artwork::Ready(c) => Some(c.clone()), _ => None },
        };
        // A separate atlas prevents card rendering from growing the UI font atlas.
        let definitions = ctx.fonts(|f| f.definitions().clone());
        let (tx, rx) = mpsc::channel();
        let wake = ctx.clone();
        self.work = Some(Work::Render(rx));
        self.message = None;
        std::thread::spawn(move || {
            let result = render_card(&card, definitions).map(|image| {
                let scale = (1200.0 / image.width().max(image.height()) as f32).min(1.0);
                let preview = imageops::resize(&image, (image.width() as f32 * scale).round().max(1.0) as u32, (image.height() as f32 * scale).round().max(1.0) as u32, imageops::FilterType::Triangle);
                Rendered { image, preview }
            });
            let _ = tx.send(result);
            wake.request_repaint();
        });
    }

    fn save(&mut self, ctx: &egui::Context) {
        let Some(image) = self.image.clone() else { return };
        let (tx, rx) = mpsc::channel();
        let wake = ctx.clone();
        self.work = Some(Work::Save(rx));
        self.message = None;
        std::thread::spawn(move || {
            let path = rfd::FileDialog::new().set_title("Save lyrics card").add_filter("PNG image", &["png"]).set_file_name("BitChord lyrics.png").save_file();
            let result = match path {
                None => Ok(None),
                Some(path) if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")) => Err("Please choose a filename ending in .png.".into()),
                Some(path) => save_png(&path, &image).map(|()| Some(path)).map_err(|e| format!("Could not save PNG: {e}")),
            };
            let _ = tx.send(result);
            wake.request_repaint();
        });
    }
}

// The guard only owns a path after create_new succeeds; collisions are never removed.
struct SaveTemp {
    path: PathBuf,
    armed: bool,
}

impl SaveTemp {
    fn cleanup(&mut self) -> io::Result<()> {
        self.armed = false;
        match fs::remove_file(&self.path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }
}

impl Drop for SaveTemp {
    fn drop(&mut self) {
        if self.armed { let _ = fs::remove_file(&self.path); }
    }
}

fn create_save_temp(destination: &Path, candidates: impl IntoIterator<Item = String>) -> io::Result<(SaveTemp, File)> {
    let directory = destination.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    for name in candidates {
        // Never create the selected destination as the temporary file, even if absent.
        if destination.file_name().is_some_and(|n| n.eq_ignore_ascii_case(&name)) { continue; }
        let path = directory.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((SaveTemp { path, armed: true }, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "could not create a unique temporary file"))
}

fn write_atomically(path: &Path, write: impl FnOnce(&mut File) -> image::ImageResult<()>) -> Result<(), String> {
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let candidates = (0..128).map(|_| format!(".bitchord-lyrics-{}-{}.tmp", std::process::id(), NEXT_TEMP.fetch_add(1, Ordering::Relaxed)));
    let (mut temp, mut file) = create_save_temp(path, candidates).map_err(|e| e.to_string())?;
    let result = write(&mut file).and_then(|()| file.sync_all().map_err(image::ImageError::IoError));
    // Close before renaming or removing the temporary file, including on Windows.
    drop(file);
    let result = result.and_then(|()| fs::rename(&temp.path, path).map_err(image::ImageError::IoError));
    match result {
        Ok(()) => { temp.armed = false; Ok(()) }
        Err(e) => match temp.cleanup() {
            Ok(()) => Err(e.to_string()),
            Err(cleanup) => Err(format!("{e} (could not remove temporary file {}: {cleanup})", temp.path.display())),
        },
    }
}

// The PNG encoder writes its final chunk on drop and can discard a write error.
struct CheckedWriter<W> {
    inner: W,
    error: Option<io::Error>,
}

impl<W> CheckedWriter<W> {
    fn remember<T>(&mut self, result: io::Result<T>) -> io::Result<T> {
        if let Err(e) = &result && self.error.is_none() {
            self.error = Some(io::Error::new(e.kind(), e.to_string()));
        }
        result
    }
}

impl<W: Write> Write for CheckedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let result = self.inner.write(buf).and_then(|n| {
            if n == 0 && !buf.is_empty() { Err(io::Error::new(io::ErrorKind::WriteZero, "failed to write PNG")) } else { Ok(n) }
        });
        self.remember(result)
    }

    fn flush(&mut self) -> io::Result<()> {
        let result = self.inner.flush();
        self.remember(result)
    }
}

fn encode_png(writer: impl Write, image: &RgbaImage) -> image::ImageResult<()> {
    let mut writer = CheckedWriter { inner: writer, error: None };
    image.write_with_encoder(image::codecs::png::PngEncoder::new(&mut writer))?;
    if let Some(error) = writer.error.take() { return Err(error.into()); }
    writer.flush()?;
    Ok(())
}

fn save_png(path: &Path, image: &RgbaImage) -> Result<(), String> {
    write_atomically(path, |file| encode_png(BufWriter::new(file), image))
}

struct Card {
    title: String,
    artist: String,
    passage: Vec<Passage>,
    cover: Option<Cover>,
}

struct DrawText {
    galley: Arc<Galley>,
    at: Pos2,
    color: [u8; 3],
    reverse_rows: bool,
}

fn layout(fonts: &mut Fonts, text: &str, font: FontId, width: f32, single: bool) -> Arc<Galley> {
    // The player's bidi job reorders RTL runs but leaves shaping to egui/harfrust.
    let mut job = crate::bidi::job(text, font, Color32::WHITE);
    job.wrap.max_width = width;
    if single {
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
    }
    if crate::bidi::rtl_base(text) { job.halign = egui::Align::RIGHT; }
    fonts.with_pixels_per_point(1.0).layout_job(job)
}

// Right-aligned egui rows use negative x coordinates relative to their anchor.
fn text_anchor(text: &str, left: f32, width: f32) -> f32 {
    if crate::bidi::rtl_base(text) { left + width } else { left }
}

fn body_text(fonts: &mut Fonts, rows: &mut Vec<DrawText>, y: &mut f32, text: &str, font: FontId, color: [u8; 3]) {
    // Visually ordered RTL paragraphs need wrapped rows drawn bottom-to-top.
    for paragraph in text.split('\n') {
        let g = layout(fonts, paragraph, font.clone(), CONTENT_WIDTH, false);
        let x = text_anchor(paragraph, MARGIN, CONTENT_WIDTH);
        let height = if paragraph.is_empty() { g.size().y.max(font.size) } else { g.size().y };
        rows.push(DrawText { galley: g, at: pos2(x, *y), color, reverse_rows: crate::bidi::rtl_base(paragraph) });
        *y += height;
    }
}

fn plan_body(fonts: &mut Fonts, passage: &[Passage], size: f32) -> (Vec<DrawText>, f32) {
    let mut y = BODY_TOP;
    let mut rows = Vec::new();
    for (i, item) in passage.iter().enumerate() {
        match item {
            Passage::Gap => body_text(fonts, &mut rows, &mut y, "(...)", theme::regular(size * 0.65), [150, 150, 150]),
            Passage::Line { text, sub } => {
                if i > 0 { y += size * 0.45; }
                body_text(fonts, &mut rows, &mut y, text, theme::bold(size), [255, 255, 255]);
                let sub_size = (size * 20.0 / 34.0).max(24.0);
                for text in sub {
                    body_text(fonts, &mut rows, &mut y, text, theme::semibold(sub_size), [215, 215, 215]);
                }
            }
        }
    }
    (rows, y)
}

// Metadata is ellipsized on the card; bound it before shaping pathological titles.
fn metadata_text(text: &str) -> String {
    let mut chars = text.chars();
    let mut result: String = chars.by_ref().take(256).collect();
    if chars.next().is_some() { result.push('…'); }
    result
}

fn render_card(card: &Card, definitions: FontDefinitions) -> Result<RgbaImage, String> {
    if card.passage.is_empty() { return Err("Select at least one lyric line.".into()); }
    // Do not silently truncate extras, and reject unreasonable input before shaping it.
    let extra_chars: usize = card.passage.iter().map(|p| match p { Passage::Line { sub, .. } => sub.iter().map(|s| s.chars().count()).sum(), Passage::Gap => 0 }).sum();
    if extra_chars > 2_000 { return Err("The extra lyrics are too long for an image. Turn off translation or romanization and try again.".into()); }
    let mut chosen = None;
    for &size in LADDER {
        // Each size needs a fresh atlas; a full atlas overwrites earlier glyphs.
        let options = egui::epaint::text::TextOptions { max_texture_side: 4096, ..Default::default() };
        let mut fonts = Fonts::new(options, definitions.clone());
        let candidate = plan_body(&mut fonts, &card.passage, size);
        if candidate.1 + FOOTER_HEIGHT <= 1920.0 || size == *LADDER.last().unwrap_or(&32.0) {
            chosen = Some((fonts, candidate));
            break;
        }
    }
    let (mut fonts, (mut rows, body_bottom)) = chosen.ok_or_else(|| "Could not lay out lyrics.".to_owned())?;
    let height = (body_bottom + FOOTER_HEIGHT).ceil() as u32;
    if height > MAX_HEIGHT { return Err("The card is too tall. Deselect lines or turn off translation or romanization.".into()); }
    let brand = layout(&mut fonts, "BitChord", theme::bold(46.0), CONTENT_WIDTH, true);
    let brand_x = WIDTH as f32 - MARGIN - brand.size().x;
    rows.push(DrawText { galley: brand, at: pos2(brand_x, 72.0), color: [235, 235, 235], reverse_rows: false });
    let thumb_y = height as f32 - 256.0;
    let text_x = MARGIN + 194.0;
    let text_width = WIDTH as f32 - MARGIN - text_x;
    let title_text = metadata_text(&card.title);
    let artist_text = metadata_text(&card.artist);
    let title = layout(&mut fonts, &title_text, theme::bold(50.0), text_width, true);
    let artist = layout(&mut fonts, &artist_text, theme::regular(42.0), text_width, true);
    rows.push(DrawText { galley: title, at: pos2(text_anchor(&title_text, text_x, text_width), thumb_y + 22.0), color: [255, 255, 255], reverse_rows: false });
    rows.push(DrawText { galley: artist, at: pos2(text_anchor(&artist_text, text_x, text_width), thumb_y + 88.0), color: [180, 180, 180], reverse_rows: false });
    let atlas = fonts.image();
    let cover = card.cover.as_ref().and_then(decode_cover);
    let mut image = backdrop(height, cover.as_ref());
    draw_cover(&mut image, cover.as_ref(), MARGIN as u32, thumb_y as u32);
    draw_mark(&mut image, (brand_x - 76.0).max(MARGIN) as u32, 70);
    for row in &rows { blit_text(&mut image, &atlas, row); }
    Ok(image)
}

/// Decode and crop using the same square UV as the player's thumbnail.
fn decode_cover(cover: &Cover) -> Option<RgbaImage> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(cover.bytes.as_ref())).with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let img = reader.decode().ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 { return None; }
    let left = (cover.uv.left().clamp(0.0, 1.0) * w as f32) as u32;
    let top = (cover.uv.top().clamp(0.0, 1.0) * h as f32) as u32;
    let right = ((cover.uv.right().clamp(0.0, 1.0) * w as f32).ceil() as u32).min(w);
    let bottom = ((cover.uv.bottom().clamp(0.0, 1.0) * h as f32).ceil() as u32).min(h);
    if left >= right || top >= bottom { return None; }
    let crop = imageops::crop_imm(&img, left, top, right - left, bottom - top).to_image();
    Some(imageops::resize(&crop, 160, 160, imageops::FilterType::Lanczos3))
}

fn backdrop(height: u32, cover: Option<&RgbaImage>) -> RgbaImage {
    // A tiny, blurred sleeve supplies color; the dark overlay keeps all type legible.
    let blurred = cover.map(|img| imageops::blur(&imageops::resize(img, 48, 48, imageops::FilterType::Triangle), 8.0));
    RgbaImage::from_fn(WIDTH, height, |x, y| {
        let shade = 1.0 - 0.25 * y as f32 / height.max(1) as f32;
        let c = blurred.as_ref().map(|b| b.get_pixel(x * 48 / WIDTH, y * 48 / height.max(1)).0).unwrap_or([38, 40, 50, 255]);
        Rgba([((c[0] as f32 * 0.19 + 12.0) * shade) as u8, ((c[1] as f32 * 0.19 + 12.0) * shade) as u8, ((c[2] as f32 * 0.19 + 15.0) * shade) as u8, 255])
    })
}

fn draw_cover(image: &mut RgbaImage, cover: Option<&RgbaImage>, x: u32, y: u32) {
    for dy in 0..160 {
        for dx in 0..160 {
            let pixel = cover.map(|c| *c.get_pixel(dx, dy)).unwrap_or(Rgba([63, 65, 72, 255]));
            if x + dx < image.width() && y + dy < image.height() {
                blend(image.get_pixel_mut(x + dx, y + dy), pixel.0[..3].try_into().unwrap_or([63, 65, 72]), pixel[3]);
            }
        }
    }
}

fn draw_mark(image: &mut RgbaImage, x: u32, y: u32) {
    if let Ok(mark) = image::load_from_memory(include_bytes!("../../assets/icon/mark.png")) {
        let mark = imageops::resize(&mark.to_rgba8(), 56, 56, imageops::FilterType::Lanczos3);
        for (dx, dy, p) in mark.enumerate_pixels() {
            if x + dx < image.width() && y + dy < image.height() {
                blend(image.get_pixel_mut(x + dx, y + dy), [235, 235, 235], p[3]);
            }
        }
    }
}

fn blend(dst: &mut Rgba<u8>, rgb: [u8; 3], alpha: u8) {
    let a = alpha as u32;
    for c in 0..3 { dst[c] = ((dst[c] as u32 * (255 - a) + rgb[c] as u32 * a + 127) / 255) as u8; }
    dst[3] = 255;
}

fn row_top(text: &DrawText, row: &egui::epaint::text::PlacedRow) -> f32 {
    if text.reverse_rows { text.galley.size().y - row.pos.y - row.row.size.y } else { row.pos.y }
}

/// Use egui's shaped glyph atlas rather than drawing one Unicode scalar at a time.
fn blit_text(image: &mut RgbaImage, atlas: &ColorImage, text: &DrawText) {
    for row in &text.galley.rows {
        for glyph in &row.row.glyphs {
            let uv = glyph.uv_rect;
            if uv.is_nothing() { continue; }
            let origin = text.at + egui::vec2(row.pos.x, row_top(text, row)) + glyph.pos.to_vec2() + uv.offset;
            for (dy, ty) in (uv.min[1]..uv.max[1]).enumerate() {
                for (dx, tx) in (uv.min[0]..uv.max[0]).enumerate() {
                    let (x, y) = ((origin.x + dx as f32).round() as i64, (origin.y + dy as f32).round() as i64);
                    if x < 0 || y < 0 || x >= image.width() as i64 || y >= image.height() as i64 { continue; }
                    if let Some(pixel) = atlas.pixels.get(ty as usize * atlas.size[0] + tx as usize) {
                        if pixel.a() != 0 { blend(image.get_pixel_mut(x as u32, y as u32), text.color, pixel.a()); }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str) -> PickLine { PickLine { text: text.into(), roman: None, translation: None } }

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            static NEXT_DIR: AtomicU64 = AtomicU64::new(0);
            for _ in 0..128 {
                let path = std::env::temp_dir().join(format!("bitchord-lyrics-test-{}-{}", std::process::id(), NEXT_DIR.fetch_add(1, Ordering::Relaxed)));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(e) => panic!("could not create test directory: {e}"),
                }
            }
            panic!("could not create a unique test directory");
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
    }

    #[test]
    fn save_png_replaces_only_the_selected_path() {
        let dir = TestDir::new();
        let destination = dir.0.join("selected.PNG");
        let other = dir.0.join("unrelated.tmp");
        fs::write(&destination, b"old image").unwrap();
        fs::write(&other, b"leave me alone").unwrap();
        let image = RgbaImage::from_pixel(2, 2, Rgba([1, 2, 3, 255]));

        save_png(&destination, &image).unwrap();
        let encoded = fs::read(&destination).unwrap();
        assert_eq!(&encoded[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(image::load_from_memory(&encoded).unwrap().to_rgba8(), image);
        assert_eq!(fs::read(&other).unwrap(), b"leave me alone");
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 2);

        let new_destination = dir.0.join("new.png");
        save_png(&new_destination, &image).unwrap();
        assert_eq!(image::load_from_memory(&fs::read(new_destination).unwrap()).unwrap().to_rgba8(), image);
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 3);
    }

    #[test]
    fn temporary_file_collisions_do_not_touch_other_files() {
        let dir = TestDir::new();
        let destination = dir.0.join("selected.png");
        let collision = dir.0.join("collision.tmp");
        fs::write(&collision, b"unrelated").unwrap();
        let names = ["selected.png", "collision.tmp", "fresh.tmp"].map(str::to_owned);
        let (mut temp, file) = create_save_temp(&destination, names).unwrap();
        assert_eq!(temp.path, dir.0.join("fresh.tmp"));
        assert!(!destination.exists());
        drop(file);
        temp.cleanup().unwrap();
        assert_eq!(fs::read(collision).unwrap(), b"unrelated");
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn write_and_buffer_flush_failures_preserve_the_destination() {
        struct FailOnWrite<'a>(&'a mut File);
        impl Write for FailOnWrite<'_> {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> { Err(io::Error::other("injected write failure")) }
            fn flush(&mut self) -> io::Result<()> { self.0.flush() }
        }

        let dir = TestDir::new();
        let destination = dir.0.join("selected.png");
        fs::write(&destination, b"original").unwrap();
        let result = write_atomically(&destination, |file| {
            file.write_all(b"partial data")?;
            Err(io::Error::other("injected encoder failure").into())
        });
        assert!(result.unwrap_err().contains("injected encoder failure"));
        assert_eq!(fs::read(&destination).unwrap(), b"original");
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);

        let image = RgbaImage::from_pixel(2, 2, Rgba([1, 2, 3, 255]));
        let result = write_atomically(&destination, |file| encode_png(BufWriter::new(FailOnWrite(file)), &image));
        assert!(result.unwrap_err().contains("injected write failure"));
        assert_eq!(fs::read(&destination).unwrap(), b"original");
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn png_final_chunk_error_preserves_the_destination() {
        struct FailIend<W>(W);
        impl<W: Write> Write for FailIend<W> {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                if buf == b"IEND" { return Err(io::Error::other("injected final chunk failure")); }
                self.0.write(buf)
            }
            fn flush(&mut self) -> io::Result<()> { self.0.flush() }
        }

        let dir = TestDir::new();
        let destination = dir.0.join("selected.png");
        fs::write(&destination, b"original").unwrap();
        let image = RgbaImage::from_pixel(2, 2, Rgba([1, 2, 3, 255]));
        let result = write_atomically(&destination, |file| encode_png(FailIend(BufWriter::new(file)), &image));
        assert!(result.unwrap_err().contains("injected final chunk failure"));
        assert_eq!(fs::read(&destination).unwrap(), b"original");
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn failed_replacement_cleans_up_the_temporary_file() {
        let dir = TestDir::new();
        let destination = dir.0.join("directory.png");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("keep"), b"original").unwrap();
        assert!(write_atomically(&destination, |file| {
            file.write_all(b"new data")?;
            Ok(())
        }).is_err());
        assert_eq!(fs::read(destination.join("keep")).unwrap(), b"original");
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn selection_budget_toggle_and_utf16() {
        let lines = vec![line(&"a".repeat(248)), line("😀"), line("x"), line(&"z".repeat(251))];
        let mut s = Selection::default();
        assert!(s.toggle(&lines, 1));
        assert_eq!(s.used, 2);
        assert!(s.toggle(&lines, 0));
        assert_eq!(s.used, 250);
        assert!(!s.toggle(&lines, 2));
        assert!(!s.toggle(&lines, 3));
        assert!(!s.toggle(&lines, 99));
        assert_eq!(s.picks.iter().copied().collect::<Vec<_>>(), vec![0, 1]);
        assert!(s.toggle(&lines, 0));
        assert!(s.toggle(&lines, 2));
        assert_eq!(s.used, 3);
    }

    #[test]
    fn passage_sorts_marks_gaps_and_deduplicates_extras() {
        let mut lines = vec![line("first"), line("skip"), line("last"), line("adjacent")];
        lines[2].roman = Some("roman".into());
        lines[2].translation = Some("ROMAN".into());
        let mut s = Selection::default();
        for i in [3, 0, 2] { assert!(s.toggle(&lines, i)); }
        assert_eq!(s.passage(&lines, true, true), vec![
            Passage::Line { text: "first".into(), sub: vec![] }, Passage::Gap,
            Passage::Line { text: "last".into(), sub: vec!["roman".into()] },
            Passage::Line { text: "adjacent".into(), sub: vec![] },
        ]);
        assert_eq!(s.passage(&lines, false, false)[2], Passage::Line { text: "last".into(), sub: vec![] });
    }

    #[test]
    fn open_preserves_extra_indices_and_prefers_shipped_values() {
        use crate::lyrics::{Line, Provider, Sync};
        let lyrics = Lyrics {
            lines: vec![
                Line { text: "one".into(), romanization: Some("shipped roman".into()), translations: vec![("en".into(), "shipped translation".into())], ..Default::default() },
                Line { instrumental: true, ..Default::default() },
                Line { text: "three".into(), romanization: Some(" THREE ".into()), ..Default::default() },
            ], sync: Sync::Line, provider: Provider::LrclibSynced, language: None, songwriters: vec![],
        };
        let track = Track { id: "id".into(), title: "Song".into(), artists: vec![], album: None, duration: None, thumbs: vec![], track_nr: None, plays: None, music_video: false, episode: false };
        let extras = Extras { roman: vec![Some("fallback".into()), None, Some("third roman".into())], trans: vec![Some("fallback".into()), None, Some("third translation".into())] };
        let state = open(&track, &lyrics, Some(&extras), true, true, "en", None);
        assert_eq!(state.lines.len(), 2);
        assert_eq!(state.lines[0].roman.as_deref(), Some("shipped roman"));
        assert_eq!(state.lines[0].translation.as_deref(), Some("shipped translation"));
        assert_eq!(state.lines[1].roman.as_deref(), Some("third roman"));
        assert_eq!(state.lines[1].translation.as_deref(), Some("third translation"));
    }

    fn fonts() -> FontDefinitions {
        let mut defs = FontDefinitions::default();
        let default = defs.families[&egui::FontFamily::Proportional].clone();
        defs.families.insert(egui::FontFamily::Name("bold".into()), default.clone());
        defs.families.insert(egui::FontFamily::Name("semibold".into()), default);
        defs
    }

    #[test]
    fn wraps_long_unicode_and_anchors_rtl_within_margins() {
        let mut fonts = Fonts::new(Default::default(), fonts());
        let passage = vec![
            Passage::Line { text: "漢".repeat(80), sub: vec![] },
            Passage::Line { text: "שלום עולם ".repeat(12), sub: vec![] },
        ];
        let (rows, _) = plan_body(&mut fonts, &passage, 64.0);
        assert!(rows.iter().all(|r| !r.galley.elided && r.galley.rows.len() > 1));
        for draw in rows {
            for row in &draw.galley.rows {
                let left = draw.at.x + row.pos.x;
                assert!(left >= MARGIN - 2.0, "row starts at {left}");
                assert!(left + row.row.size.x <= MARGIN + CONTENT_WIDTH + 2.0);
            }
        }
    }

    #[test]
    fn wrapped_rtl_rows_and_explicit_paragraphs_keep_reading_order() {
        let mut fonts = Fonts::new(Default::default(), fonts());
        let passage = vec![Passage::Line { text: format!("{}\n{}", "שלום עולם ".repeat(16), "עוד מילים ".repeat(16)), sub: vec![] }];
        let (draws, _) = plan_body(&mut fonts, &passage, 64.0);
        assert_eq!(draws.len(), 2);
        assert!(draws.iter().all(|draw| draw.reverse_rows && draw.galley.rows.len() > 1));
        for draw in &draws {
            let first = draw.galley.rows.first().unwrap();
            let last = draw.galley.rows.last().unwrap();
            assert!(row_top(draw, last) < row_top(draw, first));
        }
        assert!(draws[1].at.y >= draws[0].at.y + draws[0].galley.size().y);
    }

    #[test]
    fn overlong_extras_fail_instead_of_truncating() {
        let card = Card { title: "Song".into(), artist: "Artist".into(), passage: vec![Passage::Line { text: "verse".into(), sub: vec!["字".repeat(2001)] }], cover: None };
        assert!(render_card(&card, fonts()).unwrap_err().contains("too long"));
    }

    #[test]
    fn picker_can_draw_in_an_egui_frame() {
        let ctx = egui::Context::default();
        ctx.set_fonts(fonts());
        let mut slot = Some(LyricsShare {
            title: "Song".into(), artist: "Artist".into(), lines: vec![line("one"), line("two")],
            selection: Selection::default(), roman: false, translation: false, artwork: Artwork::None,
            image: None, texture: None, work: None, message: None,
        });
        let mut output = ctx.run_ui(Default::default(), |ui| show(ui.ctx(), &mut slot));
        output.textures_delta.clear();
        assert!(slot.is_some());
    }

    #[test]
    fn card_is_png_with_art_and_grows_for_long_passage() {
        let cover = RgbaImage::from_pixel(8, 8, Rgba([240, 30, 20, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(cover).write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        let mut card = Card { title: "Song".into(), artist: "Artist".into(), passage: vec![Passage::Line { text: "Hello, 世界!".into(), sub: vec!["A translation".into()] }], cover: Some(Cover { bytes: bytes.into_inner().into(), uv: Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)) }) };
        let short = render_card(&card, fonts()).unwrap();
        assert_eq!(short.width(), WIDTH);
        assert_eq!(short.get_pixel(80, short.height() - 240).0, [240, 30, 20, 255]);
        assert!(short.enumerate_pixels().any(|(_, y, p)| y >= BODY_TOP as u32 && y < short.height() - FOOTER_HEIGHT as u32 && p[0] > 180 && p[1] > 180 && p[2] > 180));
        card.passage = (0..100).map(|_| Passage::Line { text: "A".into(), sub: vec![] }).collect();
        let long = render_card(&card, fonts()).unwrap();
        assert!(long.height() > 1920 && long.height() > short.height());
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(short).write_to(&mut encoded, image::ImageFormat::Png).unwrap();
        assert_eq!(&encoded.get_ref()[..8], b"\x89PNG\r\n\x1a\n");
    }
}
