use std::{
    io::Cursor,
    path::{Path, PathBuf},
};

use image::{DynamicImage, ImageReader};
use lofty::{file::TaggedFileExt, picture::PictureType, probe::Probe};
use ratatui::layout::Rect;
use ratatui_image::{
    Resize, StatefulImage,
    picker::{Picker, ProtocolType},
    protocol::StatefulProtocol,
};

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
        if self.key.as_ref() == Some(&key) && (self.protocol.is_some() || self.missing) {
            return;
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

    fn clear(&mut self) {
        self.key = None;
        self.protocol = None;
        self.missing = false;
    }
}

fn load_cover(track: &Track, cache_dir: &Path) -> Option<DynamicImage> {
    let path = source_path(track, cache_dir)?;
    if let Some(bytes) = embedded_cover(&path) {
        return decode(&bytes);
    }
    folder_cover(path.parent()?)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_has_no_cover() {
        let track = Track {
            id: 1,
            path: PathBuf::from("/no/such/file.flac"),
            title: "T".into(),
            artist: "A".into(),
            album: "B".into(),
            date: None,
            track_number: None,
            duration: std::time::Duration::ZERO,
            codec: "FLAC".into(),
            sample_rate: None,
            channels: None,
            file_size: 0,
            modified_ns: 0,
            unavailable: true,
            scan_error: None,
            origin: TrackOrigin::Local,
            replay_gain: crate::model::ReplayGainInfo::default(),
        };
        assert!(load_cover(&track, Path::new("/tmp")).is_none());
    }

    #[test]
    fn art_panel_leaves_room_for_the_album_list() {
        assert!(art_panel_height(32, 30) < 30);
        assert!(art_panel_height(32, 12) <= 4);
    }
}
