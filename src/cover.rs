use std::{
    io::Cursor,
    path::{Path, PathBuf},
};

use image::{DynamicImage, ImageFormat, ImageReader};
use lofty::{file::TaggedFileExt, picture::PictureType, probe::Probe};
use ratatui::layout::Rect;
use ratatui_image::{
    Resize, StatefulImage,
    picker::{Picker, ProtocolType},
    protocol::StatefulProtocol,
};
use sha2::{Digest, Sha256};

use crate::model::{Track, TrackOrigin};

#[derive(Clone, Debug, PartialEq, Eq)]
struct CoverKey {
    artist: String,
    album: String,
}

pub struct CoverView {
    picker: Picker,
    key: Option<CoverKey>,
    protocol: Option<StatefulProtocol>,
    missing: bool,
}

impl Default for CoverView {
    fn default() -> Self {
        Self {
            picker: Picker::halfblocks(),
            key: None,
            protocol: None,
            missing: false,
        }
    }
}

impl CoverView {
    pub fn set_picker(&mut self, picker: Picker) {
        self.picker = picker;
        self.protocol = None;
    }

    pub fn protocol_label(&self) -> &'static str {
        match self.picker.protocol_type() {
            ProtocolType::Kitty => "Kitty",
            ProtocolType::Sixel => "Sixel",
            ProtocolType::Iterm2 => "iTerm2",
            ProtocolType::Halfblocks => "halfblocks",
        }
    }

    pub fn sync(&mut self, track: Option<&Track>, cache_dir: &Path) {
        let Some(track) = track else {
            self.clear();
            return;
        };
        let key = CoverKey {
            artist: track.artist.clone(),
            album: track.album.clone(),
        };
        if self.key.as_ref() == Some(&key) {
            if self.protocol.is_some() {
                return;
            }
            if self.missing && !can_retry_cover(track, cache_dir) {
                return;
            }
        }
        self.key = Some(key);
        match load_cover(track, cache_dir) {
            Some(image) => {
                self.protocol = Some(self.picker.new_resize_protocol(image));
                self.missing = false;
            }
            None => {
                self.protocol = None;
                self.missing = true;
            }
        }
    }

    pub fn render(&mut self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        if let Some(protocol) = &mut self.protocol {
            let widget = StatefulImage::new().resize(Resize::Fit(None));
            frame.render_stateful_widget(widget, area, protocol);
        }
    }

    pub fn has_image(&self) -> bool {
        self.protocol.is_some()
    }

    pub fn invalidate(&mut self) {
        self.key = None;
        self.missing = false;
    }

    fn clear(&mut self) {
        self.key = None;
        self.protocol = None;
        self.missing = false;
    }
}

pub fn album_cache_path(cache_dir: &Path, fingerprint: &str, artist: &str, album: &str) -> PathBuf {
    let digest = Sha256::digest(format!("{artist}\0{album}").as_bytes());
    cache_dir
        .join("covers")
        .join(fingerprint)
        .join(hex::encode(digest))
}

pub fn save_album_cover(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)
}

pub fn extract_cover_bytes(path: &Path) -> Option<Vec<u8>> {
    if let Some(bytes) = embedded_cover(path) {
        return Some(bytes);
    }
    let image = folder_cover(path.parent()?)?;
    let mut out = Cursor::new(Vec::new());
    image.write_to(&mut out, ImageFormat::Jpeg).ok()?;
    Some(out.into_inner())
}

pub fn encode_cover_data(bytes: &[u8]) -> String {
    encode_base64(bytes)
}

pub fn decode_cover_data(data: &str) -> Option<Vec<u8>> {
    decode_base64(data)
}

fn load_cover(track: &Track, cache_dir: &Path) -> Option<DynamicImage> {
    if let TrackOrigin::Remote { fingerprint, .. } = &track.origin {
        let cached = album_cache_path(cache_dir, fingerprint, &track.artist, &track.album);
        if cached.is_file()
            && let Ok(bytes) = std::fs::read(&cached)
            && let Some(image) = decode(&bytes)
        {
            return Some(image);
        }
    }
    let path = source_path(track, cache_dir)?;
    if let Some(bytes) = embedded_cover(&path) {
        return decode(&bytes);
    }
    folder_cover(path.parent()?)
}

fn can_retry_cover(track: &Track, cache_dir: &Path) -> bool {
    if let TrackOrigin::Remote { fingerprint, .. } = &track.origin
        && album_cache_path(cache_dir, fingerprint, &track.artist, &track.album).is_file()
    {
        return true;
    }
    source_path(track, cache_dir).is_some()
}

fn source_path(track: &Track, cache_dir: &Path) -> Option<PathBuf> {
    match &track.origin {
        TrackOrigin::Local if track.path.is_file() => Some(track.path.clone()),
        TrackOrigin::Remote {
            fingerprint,
            remote_id,
            ..
        } => {
            let cached = crate::net::cache_path(cache_dir, fingerprint, remote_id);
            crate::net::cache_is_complete(&cached, track.file_size).then_some(cached)
        }
        _ => None,
    }
}

fn embedded_cover(path: &Path) -> Option<Vec<u8>> {
    let tagged = Probe::open(path).ok()?.read().ok()?;
    tagged.tags().iter().find_map(|tag| {
        tag.get_picture_type(PictureType::CoverFront)
            .or_else(|| tag.pictures().first())
            .map(|picture| picture.data().to_vec())
    })
}

fn folder_cover(directory: &Path) -> Option<DynamicImage> {
    const NAMES: &[&str] = &[
        "cover.jpg",
        "cover.jpeg",
        "cover.png",
        "cover.webp",
        "folder.jpg",
        "folder.png",
        "front.jpg",
        "Front.jpg",
        "Cover.jpg",
        "Folder.jpg",
        "artwork.jpg",
        "albumart.jpg",
        "AlbumArt.jpg",
    ];
    for name in NAMES {
        let path = directory.join(name);
        if path.is_file()
            && let Some(image) = ImageReader::open(&path)
                .ok()
                .and_then(|reader| reader.decode().ok())
        {
            return Some(image);
        }
    }
    None
}

fn decode(bytes: &[u8]) -> Option<DynamicImage> {
    ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()
}

pub fn art_panel_height(column_width: u16, content_height: u16) -> u16 {
    let square = column_width.saturating_add(1) / 2 + 2;
    square.clamp(8, 18).min(content_height.saturating_sub(8))
}

fn encode_base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(b2 & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn decode_base64(input: &str) -> Option<Vec<u8>> {
    fn value(byte: u8) -> Option<u8> {
        Some(match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    }
    let bytes: Vec<u8> = input
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace() && *byte != b'=')
        .collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4 + 1);
    for chunk in bytes.chunks(4) {
        if chunk.len() < 2 {
            return None;
        }
        let a = value(chunk[0])?;
        let b = value(chunk[1])?;
        out.push((a << 2) | (b >> 4));
        if chunk.len() >= 3 {
            let c = value(chunk[2])?;
            out.push((b << 4) | (c >> 2));
            if chunk.len() == 4 {
                let d = value(chunk[3])?;
                out.push((c << 6) | d);
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ReplayGainInfo, TrackId};

    fn sample_remote(id: TrackId) -> Track {
        Track {
            id,
            path: PathBuf::from("/remote/album/track.flac"),
            title: "One".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            date: None,
            track_number: Some(1),
            duration: std::time::Duration::from_secs(1),
            codec: "FLAC".into(),
            sample_rate: Some(44_100),
            channels: Some(2),
            file_size: 100,
            modified_ns: 0,
            unavailable: false,
            scan_error: None,
            origin: TrackOrigin::Remote {
                fingerprint: "fp".into(),
                remote_id: "rid".into(),
                server_name: "Server".into(),
            },
            replay_gain: ReplayGainInfo::default(),
        }
    }

    #[test]
    fn remote_cover_retries_after_cache_arrives() {
        let directory = tempfile::tempdir().unwrap();
        let track = sample_remote(1);
        let mut covers = CoverView::default();
        covers.sync(Some(&track), directory.path());
        assert!(!covers.has_image());

        let path = album_cache_path(directory.path(), "fp", "Artist", "Album");
        let mut jpeg = Cursor::new(Vec::new());
        image::RgbImage::from_pixel(4, 4, image::Rgb([20, 40, 60]))
            .write_to(&mut jpeg, ImageFormat::Jpeg)
            .unwrap();
        save_album_cover(&path, &jpeg.into_inner()).unwrap();
        covers.sync(Some(&track), directory.path());
        assert!(covers.has_image());
    }

    #[test]
    fn folder_cover_is_extracted_as_jpeg_bytes() {
        let directory = tempfile::tempdir().unwrap();
        image::RgbImage::from_pixel(2, 2, image::Rgb([10, 20, 30]))
            .save(directory.path().join("cover.jpg"))
            .unwrap();
        let bytes = extract_cover_bytes(&directory.path().join("track.flac")).unwrap();
        assert!(decode(&bytes).is_some());
    }

    #[test]
    fn cover_data_round_trips() {
        let bytes = b"cover-bytes";
        assert_eq!(decode_cover_data(&encode_cover_data(bytes)).unwrap(), bytes);
    }
}
