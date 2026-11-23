//! Custom PNG title logos for albums and playlists, matching BitChord mobile's
//! AlbumLogoRepository.
//!
//! Stores user-imported transparent PNG title logos under `data_dir/album_logos/<sanitized_id>.png`.
//! When present, the collection detail header renders the stylized logo graphic
//! instead of standard headline typography.

use std::path::{Path, PathBuf};

const LOGO_DIR: &str = "album_logos";

pub fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' { c } else { '_' })
        .collect()
}

pub fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join(LOGO_DIR)
}

pub fn logo_path(data_dir: &Path, id: &str) -> PathBuf {
    dir(data_dir).join(format!("{}.png", sanitize_id(id)))
}

pub fn has_logo(data_dir: &Path, id: &str) -> bool {
    let p = logo_path(data_dir, id);
    p.is_file() && std::fs::metadata(&p).map(|m| m.len() > 0).unwrap_or(false)
}

pub fn save_logo(data_dir: &Path, id: &str, src_file: &Path) -> anyhow::Result<()> {
    let d = dir(data_dir);
    std::fs::create_dir_all(&d)?;
    let target = logo_path(data_dir, id);
    let tmp = d.join(format!("{}.tmp", sanitize_id(id)));
    std::fs::copy(src_file, &tmp)?;
    std::fs::rename(tmp, target)?;
    Ok(())
}

pub fn remove_logo(data_dir: &Path, id: &str) -> bool {
    let p = logo_path(data_dir, id);
    std::fs::remove_file(p).is_ok()
}
