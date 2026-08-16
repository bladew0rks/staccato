use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use crossbeam_channel::{Receiver, Sender, unbounded};
use lofty::{
    file::{AudioFile, TaggedFileExt},
    prelude::Accessor,
    probe::Probe,
};
use walkdir::WalkDir;

use crate::model::{ReplayGainInfo, ScannedTrack, fallback_title};
use crate::replaygain;

#[derive(Debug)]
pub enum ScanEvent {
    Started {
        total_hint: usize,
        add_to_playlist: bool,
    },
    Track {
        track: ScannedTrack,
        add_to_playlist: bool,
    },
    Failed {
        path: PathBuf,
        error: String,
    },
    Finished {
        discovered: usize,
        failed: usize,
    },
}

pub struct Scanner {
    sender: Sender<ScanEvent>,
    receiver: Receiver<ScanEvent>,
}

impl Scanner {
    pub fn new() -> Self {
        let (sender, receiver) = unbounded();
        Self { sender, receiver }
    }

    pub fn receiver(&self) -> Receiver<ScanEvent> {
        self.receiver.clone()
    }

    pub fn scan(
        &self,
        paths: Vec<PathBuf>,
        add_to_playlist: bool,
        known: BTreeMap<PathBuf, ScannedTrack>,
    ) {
        let sender = self.sender.clone();
        thread::spawn(move || scan_paths(paths, add_to_playlist, known, sender));
    }
}

fn scan_paths(
    paths: Vec<PathBuf>,
    add_to_playlist: bool,
    known: BTreeMap<PathBuf, ScannedTrack>,
    sender: Sender<ScanEvent>,
) {
    let files = collect_audio_files(&paths);
    let _ = sender.send(ScanEvent::Started {
        total_hint: files.len(),
        add_to_playlist,
    });
    let mut discovered = 0;
    let mut failed = 0;
    for path in files {
        let cached = fs::metadata(&path).ok().and_then(|metadata| {
            let modified_ns = metadata
                .modified()
                .ok()?
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_nanos()
                .min(i64::MAX as u128) as i64;
            known
                .get(&path)
                .filter(|track| {
                    track.file_size == metadata.len() && track.modified_ns == modified_ns
                })
                .cloned()
        });
        match cached.map(Ok).unwrap_or_else(|| scan_file(&path)) {
            Ok(track) => {
                discovered += 1;
                let _ = sender.send(ScanEvent::Track {
                    track,
                    add_to_playlist,
                });
            }
            Err(error) => {
                failed += 1;
                let error = error.to_string();
                if let Ok(track) = fallback_scanned_track(&path, error.clone()) {
                    let _ = sender.send(ScanEvent::Track {
                        track,
                        add_to_playlist,
                    });
                }
                let _ = sender.send(ScanEvent::Failed { path, error });
            }
        }
    }
    let _ = sender.send(ScanEvent::Finished { discovered, failed });
}

pub fn collect_audio_files(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_file() {
            if is_supported(path) {
                files.push(path.clone());
            }
            continue;
        }
        if path.is_dir() {
            files.extend(
                WalkDir::new(path)
                    .follow_links(false)
                    .into_iter()
                    .filter_map(Result::ok)
                    .filter(|entry| entry.file_type().is_file())
                    .map(|entry| entry.into_path())
                    .filter(|path| is_supported(path)),
            );
        }
    }
    files.sort_by(|a, b| {
        a.to_string_lossy()
            .to_lowercase()
            .cmp(&b.to_string_lossy().to_lowercase())
    });
    files.dedup();
    files
}

pub fn is_supported(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some(
            "mp3"
                | "flac"
                | "wav"
                | "wave"
                | "ogg"
                | "oga"
                | "aac"
                | "m4a"
                | "mp4"
                | "alac"
                | "aif"
                | "aiff"
        )
    )
}

pub fn scan_file(path: &Path) -> anyhow::Result<ScannedTrack> {
    let metadata = fs::metadata(path)?;
    let modified_ns = metadata
        .modified()
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(i64::MAX as u128) as i64;
    let tagged = Probe::open(path)?.read()?;
    let properties = tagged.properties();
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let text = |value: Option<std::borrow::Cow<'_, str>>, fallback: &str| {
        value
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| fallback.to_owned())
    };
    let title = tag
        .and_then(Accessor::title)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| fallback_title(path));
    let artist = text(tag.and_then(Accessor::artist), "Unknown artist");
    let album = text(tag.and_then(Accessor::album), "Unknown album");
    let codec = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_uppercase())
        .unwrap_or_else(|| "Unknown".to_owned());
    Ok(ScannedTrack {
        path: path.to_path_buf(),
        title,
        artist,
        album,
        date: tag
            .and_then(|tag| tag.date())
            .map(|date| u32::from(date.year)),
        track_number: tag.and_then(|tag| tag.track()),
        duration: properties.duration(),
        codec,
        sample_rate: properties.sample_rate(),
        channels: properties.channels(),
        file_size: metadata.len(),
        modified_ns,
        scan_error: None,
        replay_gain: tag.map(replaygain::read_from_tag).unwrap_or_default(),
    })
}

fn fallback_scanned_track(path: &Path, error: String) -> anyhow::Result<ScannedTrack> {
    let metadata = fs::metadata(path)?;
    let modified_ns = metadata
        .modified()
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(i64::MAX as u128) as i64;
    Ok(ScannedTrack {
        path: path.to_path_buf(),
        title: fallback_title(path),
        artist: "Unknown artist".into(),
        album: "Unknown album".into(),
        date: None,
        track_number: None,
        duration: std::time::Duration::ZERO,
        codec: path
            .extension()
            .map(|extension| extension.to_string_lossy().to_ascii_uppercase())
            .unwrap_or_else(|| "Unknown".into()),
        sample_rate: None,
        channels: None,
        file_size: metadata.len(),
        modified_ns,
        scan_error: Some(error),
        replay_gain: ReplayGainInfo::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn filters_supported_extensions_case_insensitively() {
        assert!(is_supported(Path::new("song.FLAC")));
        assert!(is_supported(Path::new("song.m4a")));
        assert!(!is_supported(Path::new("cover.jpg")));
    }

    #[test]
    fn reads_metadata_from_a_minimal_wave_file() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("fixture.wav");
        let mut file = fs::File::create(&path)?;
        let data_len = 8u32;
        file.write_all(b"RIFF")?;
        file.write_all(&(36 + data_len).to_le_bytes())?;
        file.write_all(b"WAVEfmt ")?;
        file.write_all(&16u32.to_le_bytes())?;
        file.write_all(&1u16.to_le_bytes())?;
        file.write_all(&1u16.to_le_bytes())?;
        file.write_all(&8_000u32.to_le_bytes())?;
        file.write_all(&16_000u32.to_le_bytes())?;
        file.write_all(&2u16.to_le_bytes())?;
        file.write_all(&16u16.to_le_bytes())?;
        file.write_all(b"data")?;
        file.write_all(&data_len.to_le_bytes())?;
        file.write_all(&[0; 8])?;
        drop(file);

        let track = scan_file(&path)?;
        assert_eq!(track.title, "fixture");
        assert_eq!(track.codec, "WAV");
        assert_eq!(track.sample_rate, Some(8_000));
        assert_eq!(track.channels, Some(1));
        Ok(())
    }

    #[test]
    fn corrupt_supported_file_gets_visible_fallback_metadata() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("broken.flac");
        fs::write(&path, b"not a flac file")?;
        assert!(scan_file(&path).is_err());
        let track = fallback_scanned_track(&path, "invalid stream".into())?;
        assert_eq!(track.title, "broken");
        assert_eq!(track.scan_error.as_deref(), Some("invalid stream"));
        Ok(())
    }
}
