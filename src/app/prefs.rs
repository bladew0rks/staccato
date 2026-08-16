use anyhow::Result;

use crate::{
    audio::output_devices,
    model::{ReplayGainMode, Track},
    replaygain::{self, ReplayGainEvent},
    storage::SavedState,
};

use super::{App, util::on_off};

impl App {
    pub fn cache_dir(&self) -> std::path::PathBuf {
        self.data_dir.join("cache")
    }

    pub fn spectrum(&self) -> [f32; crate::spectrum::SPECTRUM_BANDS] {
        self.audio.spectrum()
    }

    pub fn output_device_warning(&self) -> Option<String> {
        if self.no_audio {
            return None;
        }
        let preferred = self.preferred_output_device.as_ref()?;
        (self.audio_snapshot.active_device_id.as_ref() != Some(preferred)).then(|| {
            format!(
                "Preferred audio device unavailable — using {}",
                self.audio_snapshot.active_device
            )
        })
    }

    pub fn cover_track(&self) -> Option<&Track> {
        if let Some(id) = self.audio_snapshot.track_id {
            return self.tracks.get(&id);
        }
        let id = *self.active_playlist().items.get(self.playlist_selection)?;
        self.tracks.get(&id)
    }

    pub fn setting_value(&self, id: crate::settings::SettingId) -> String {
        use crate::settings::SettingId;
        match id {
            SettingId::OutputDevice => match self.preferred_output_device.as_ref() {
                None => format!("System default ({})", self.audio.snapshot().active_device),
                Some(preferred)
                    if self.audio.snapshot().active_device_id.as_ref() == Some(preferred) =>
                {
                    self.audio.snapshot().active_device
                }
                Some(preferred) => {
                    let requested = self
                        .output_devices
                        .iter()
                        .find(|device| &device.id == preferred)
                        .map(|device| device.name.as_str())
                        .unwrap_or("Unavailable device");
                    format!(
                        "{requested} (using {})",
                        self.audio.snapshot().active_device
                    )
                }
            },
            SettingId::ReplayGainMode => self.replay_gain_mode.label().into(),
            SettingId::ReplayGainPreamp => format!("{:+.0} dB", self.replay_gain_preamp),
            SettingId::ReplayGainClip => on_off(self.replay_gain_prevent_clip),
            SettingId::CursorFollow => on_off(self.cursor_follows_playback),
            SettingId::AlbumArt => on_off(self.show_album_art),
            SettingId::Spectrum => on_off(self.show_spectrum),
            SettingId::NerdFont => on_off(self.nerd_font),
        }
    }

    pub(crate) fn adjust_setting(&mut self, delta: i32) -> Result<()> {
        if !self.settings_open {
            return Ok(());
        }
        let Some(id) = crate::settings::ROWS
            .get(self.settings_selected)
            .and_then(|row| row.id())
        else {
            return Ok(());
        };
        use crate::settings::SettingId;
        match id {
            SettingId::OutputDevice => {
                if self.no_audio {
                    self.status = "Audio is disabled by --no-audio".into();
                    return Ok(());
                }
                self.output_devices = output_devices();
                let count = self.output_devices.len() + 1;
                let current = self
                    .preferred_output_device
                    .as_ref()
                    .and_then(|id| {
                        self.output_devices
                            .iter()
                            .position(|device| &device.id == id)
                    })
                    .map_or(0, |index| index + 1);
                let next = super::util::wrap_index(current, delta.signum(), count);
                self.preferred_output_device = if next == 0 {
                    None
                } else {
                    Some(self.output_devices[next - 1].id.clone())
                };
                self.switch_output_device()?;
            }
            SettingId::ReplayGainMode => {
                if delta >= 0 {
                    self.replay_gain_mode = self.replay_gain_mode.next();
                } else {
                    for _ in 0..ReplayGainMode::ALL.len() - 1 {
                        self.replay_gain_mode = self.replay_gain_mode.next();
                    }
                }
                self.apply_replay_gain();
            }
            SettingId::ReplayGainPreamp => {
                self.replay_gain_preamp =
                    (self.replay_gain_preamp + delta as f32).clamp(-15.0, 15.0);
                self.apply_replay_gain();
            }
            SettingId::ReplayGainClip => {
                self.replay_gain_prevent_clip = !self.replay_gain_prevent_clip;
                self.apply_replay_gain();
            }
            SettingId::CursorFollow => {
                self.cursor_follows_playback = !self.cursor_follows_playback;
            }
            SettingId::AlbumArt => {
                self.show_album_art = !self.show_album_art;
            }
            SettingId::Spectrum => {
                self.show_spectrum = !self.show_spectrum;
            }
            SettingId::NerdFont => {
                self.nerd_font = !self.nerd_font;
            }
        }
        if matches!(
            id,
            SettingId::ReplayGainMode | SettingId::ReplayGainPreamp | SettingId::ReplayGainClip
        ) {
            self.restage_successor();
        }
        self.status = format!("{}: {}", id.label(), self.setting_value(id));
        self.save_state()?;
        Ok(())
    }

    pub(crate) fn apply_replay_gain(&mut self) {
        let track = self
            .audio
            .snapshot()
            .track_id
            .and_then(|id| self.tracks.get(&id));
        let info = track
            .map(|track| track.replay_gain.clone())
            .unwrap_or_default();
        let linear = info.apply(
            self.replay_gain_mode,
            self.replay_gain_preamp,
            self.replay_gain_prevent_clip,
        );
        self.audio.set_output_gain(linear);
    }

    pub fn replay_gain_status(&self) -> Option<String> {
        if self.replay_gain_mode == ReplayGainMode::None {
            return None;
        }
        let track = self
            .audio_snapshot
            .track_id
            .and_then(|id| self.tracks.get(&id))?;
        let gain = match self.replay_gain_mode {
            ReplayGainMode::Track => track
                .replay_gain
                .track_gain
                .or(track.replay_gain.album_gain),
            ReplayGainMode::Album => track
                .replay_gain
                .album_gain
                .or(track.replay_gain.track_gain),
            ReplayGainMode::None => return None,
        };
        Some(match gain {
            Some(db) => format!("RG {:+.1} dB", db + self.replay_gain_preamp),
            None => format!("RG {}", self.replay_gain_mode.label()),
        })
    }

    pub(crate) fn begin_replaygain_scan(&mut self) {
        let mut jobs = Vec::new();
        let ids = {
            let selected = self.selected_library_tracks();
            if selected.is_empty() {
                self.active_playlist().items.clone()
            } else {
                selected
            }
        };
        for id in ids {
            if let Some(track) = self.tracks.get(&id)
                && !track.origin.is_remote()
                && track.path.exists()
            {
                jobs.push((
                    id,
                    track.path.clone(),
                    track.artist.clone(),
                    track.album.clone(),
                ));
            }
        }
        if jobs.is_empty() {
            self.status = "No local files to scan for ReplayGain".into();
            return;
        }
        self.status = format!(
            "Scanning ReplayGain for {}…",
            super::util::counted(jobs.len(), "file")
        );
        self.replaygain = Some(replaygain::start(jobs));
    }

    pub(crate) fn drain_replaygain_events(&mut self) {
        let Some(handle) = &self.replaygain else {
            return;
        };
        let events: Vec<_> = handle.events.try_iter().collect();
        for event in events {
            match event {
                ReplayGainEvent::Started { total } => {
                    self.status = format!("Scanning ReplayGain (0/{total})…");
                }
                ReplayGainEvent::Track { id, info, path } => {
                    let metadata = std::fs::metadata(&path).ok();
                    if let Some(track) = self.tracks.get_mut(&id) {
                        track.replay_gain = info.clone();
                        if let Some(metadata) = &metadata {
                            track.file_size = metadata.len();
                            if let Some(modified) = metadata
                                .modified()
                                .ok()
                                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                            {
                                track.modified_ns =
                                    modified.as_nanos().min(i64::MAX as u128) as i64;
                            }
                        }
                    }
                    if let Some(track) = self.tracks.get(&id) {
                        let _ = self.store.update_replay_gain(
                            id,
                            &info,
                            track.file_size,
                            track.modified_ns,
                        );
                    }
                    let snapshot = self.audio.snapshot();
                    if snapshot.track_id == Some(id) {
                        self.apply_replay_gain();
                    }
                    if snapshot.track_id == Some(id) || snapshot.staged_track_id == Some(id) {
                        self.restage_successor();
                    }
                    self.status = format!("ReplayGain: {}", path.display());
                }
                ReplayGainEvent::Failed { error } => {
                    self.status = format!("ReplayGain: {error}");
                }
                ReplayGainEvent::Finished { scanned, failed } => {
                    self.status =
                        format!("ReplayGain scan complete: {scanned} tagged, {failed} failed");
                    self.replaygain = None;
                }
            }
        }
    }

    pub(crate) fn save_state(&mut self) -> Result<()> {
        self.store.save_state(&SavedState {
            active_playlist: self.active_playlist,
            volume: self.audio.snapshot().volume,
            playback_order: self.playback_order,
            cursor_follows_playback: self.cursor_follows_playback,
            replay_gain_mode: self.replay_gain_mode,
            replay_gain_preamp: self.replay_gain_preamp,
            replay_gain_prevent_clip: self.replay_gain_prevent_clip,
            show_album_art: self.show_album_art,
            show_spectrum: self.show_spectrum,
            nerd_font: self.nerd_font,
            preferred_output_device: self.preferred_output_device.clone(),
        })
    }
}
