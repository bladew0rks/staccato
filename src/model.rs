use std::{path::PathBuf, time::Duration};

pub type TrackId = i64;
pub type PlaylistId = i64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackOrigin {
    Local,
    Remote {
        fingerprint: String,
        remote_id: String,
        server_name: String,
    },
}

impl TrackOrigin {
    pub fn is_remote(&self) -> bool {
        matches!(self, Self::Remote { .. })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReplayGainInfo {
    pub track_gain: Option<f32>,
    pub track_peak: Option<f32>,
    pub album_gain: Option<f32>,
    pub album_peak: Option<f32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReplayGainMode {
    None,
    Track,
    #[default]
    Album,
}

impl ReplayGainMode {
    pub const ALL: [Self; 3] = [Self::None, Self::Track, Self::Album];

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Track => "Track",
            Self::Album => "Album",
        }
    }

    pub fn as_i64(self) -> i64 {
        match self {
            Self::None => 0,
            Self::Track => 1,
            Self::Album => 2,
        }
    }

    pub fn from_i64(value: i64) -> Self {
        match value {
            1 => Self::Track,
            2 => Self::Album,
            _ => Self::None,
        }
    }

    pub fn next(self) -> Self {
        Self::ALL[(self.as_i64() as usize + 1) % Self::ALL.len()]
    }
}

impl ReplayGainInfo {
    pub fn apply(&self, mode: ReplayGainMode, preamp_db: f32, prevent_clipping: bool) -> f32 {
        if mode == ReplayGainMode::None {
            return 1.0;
        }
        let (gain, peak) = match mode {
            ReplayGainMode::None => (None, None),
            ReplayGainMode::Track => (
                self.track_gain.or(self.album_gain),
                self.track_peak.or(self.album_peak),
            ),
            ReplayGainMode::Album => (
                self.album_gain.or(self.track_gain),
                self.album_peak.or(self.track_peak),
            ),
        };
        let mut linear = 10f32.powf((preamp_db + gain.unwrap_or(0.0)) / 20.0);
        if prevent_clipping
            && let Some(peak) = peak
            && peak > 0.0
            && peak * linear > 1.0
        {
            linear = 1.0 / peak;
        }
        linear.max(0.0)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub id: TrackId,
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub date: Option<u32>,
    pub track_number: Option<u32>,
    pub duration: Duration,
    pub codec: String,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
    pub file_size: u64,
    pub modified_ns: i64,
    pub unavailable: bool,
    pub scan_error: Option<String>,
    pub origin: TrackOrigin,
    pub replay_gain: ReplayGainInfo,
}

impl Track {
    pub fn to_scanned(&self) -> ScannedTrack {
        ScannedTrack {
            path: self.path.clone(),
            title: self.title.clone(),
            artist: self.artist.clone(),
            album: self.album.clone(),
            date: self.date,
            track_number: self.track_number,
            duration: self.duration,
            codec: self.codec.clone(),
            sample_rate: self.sample_rate,
            channels: self.channels,
            file_size: self.file_size,
            modified_ns: self.modified_ns,
            scan_error: self.scan_error.clone(),
            replay_gain: self.replay_gain.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScannedTrack {
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub date: Option<u32>,
    pub track_number: Option<u32>,
    pub duration: Duration,
    pub codec: String,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
    pub file_size: u64,
    pub modified_ns: i64,
    pub scan_error: Option<String>,
    pub replay_gain: ReplayGainInfo,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Playlist {
    pub id: PlaylistId,
    pub name: String,
    pub items: Vec<TrackId>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlaybackOrder {
    #[default]
    Default,
    RepeatPlaylist,
    RepeatTrack,
    Shuffle,
    ShuffleAlbums,
}

impl PlaybackOrder {
    pub const ALL: [Self; 5] = [
        Self::Default,
        Self::RepeatPlaylist,
        Self::RepeatTrack,
        Self::Shuffle,
        Self::ShuffleAlbums,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::RepeatPlaylist => "Repeat (playlist)",
            Self::RepeatTrack => "Repeat (track)",
            Self::Shuffle => "Shuffle (tracks)",
            Self::ShuffleAlbums => "Shuffle (albums)",
        }
    }

    pub fn as_i64(self) -> i64 {
        match self {
            Self::Default => 0,
            Self::RepeatPlaylist => 1,
            Self::RepeatTrack => 2,
            Self::Shuffle => 3,
            Self::ShuffleAlbums => 4,
        }
    }

    pub fn from_i64(value: i64) -> Self {
        match value {
            1 => Self::RepeatPlaylist,
            2 => Self::RepeatTrack,
            3 => Self::Shuffle,
            4 => Self::ShuffleAlbums,
            _ => Self::Default,
        }
    }

    pub fn next(self) -> Self {
        Self::ALL[(self.as_i64() as usize + 1) % Self::ALL.len()]
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlaybackState {
    #[default]
    Stopped,
    Loading,
    Buffering,
    Playing,
    Paused,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Focus {
    AlbumList,
    #[default]
    Playlist,
    PlaylistTabs,
    SoulseekQuery,
    SoulseekFilter,
    PlaylistFilter,
    AlbumFilter,
    Queue,
    Settings,
    Toolbar,
}

impl Focus {
    pub fn cycle(self, backwards: bool, ring: &[Self]) -> Self {
        if ring.is_empty() {
            return self;
        }
        let at = ring.iter().position(|item| *item == self).unwrap_or(0);
        let next = if backwards {
            (at + ring.len() - 1) % ring.len()
        } else {
            (at + 1) % ring.len()
        };
        ring[next]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistColumn {
    Artist,
    Title,
    Album,
    Date,
    Time,
    Number,
}

impl PlaylistColumn {
    pub fn label(self) -> &'static str {
        match self {
            Self::Number => "#",
            Self::Artist => "Artist",
            Self::Title => "Title",
            Self::Album => "Album",
            Self::Date => "Date",
            Self::Time => "Time",
        }
    }
}

pub fn text_matches(needle: &str, parts: &[&str]) -> bool {
    if needle.is_empty() {
        return true;
    }
    let needle = needle.to_lowercase();
    parts
        .iter()
        .any(|part| part.to_lowercase().contains(&needle))
}

pub fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

pub fn fallback_title(path: &std::path::Path) -> String {
    path.file_stem()
        .or_else(|| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Unknown title".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_order_cycles() {
        assert_eq!(PlaybackOrder::Default.next(), PlaybackOrder::RepeatPlaylist);
        assert_eq!(PlaybackOrder::Shuffle.next(), PlaybackOrder::ShuffleAlbums);
        assert_eq!(PlaybackOrder::ShuffleAlbums.next(), PlaybackOrder::Default);
    }

    #[test]
    fn replay_gain_album_falls_back_to_track() {
        let info = ReplayGainInfo {
            track_gain: Some(-6.0),
            track_peak: Some(0.5),
            album_gain: None,
            album_peak: None,
        };
        let linear = info.apply(ReplayGainMode::Album, 0.0, false);
        assert!((linear - 10f32.powf(-6.0 / 20.0)).abs() < 1e-5);
    }

    #[test]
    fn replay_gain_prevents_clipping_from_peak() {
        let info = ReplayGainInfo {
            track_gain: Some(6.0),
            track_peak: Some(1.0),
            album_gain: None,
            album_peak: None,
        };
        let linear = info.apply(ReplayGainMode::Track, 0.0, true);
        assert!((linear - 1.0).abs() < 1e-5);
    }
}
