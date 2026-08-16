use crate::{
    action::Action,
    model::{Focus, format_duration},
};

use super::{App, overlay::Overlay};

pub fn menu_actions(menu: usize) -> &'static [(&'static str, Action)] {
    match menu {
        0 => &[
            ("Add files…        Ctrl+O", Action::OpenFiles),
            ("Add folder… Ctrl+Shift+O", Action::OpenFolder),
            ("New playlist…     Ctrl+N", Action::NewPlaylist),
            ("Preferences…      Ctrl+,", Action::OpenSettings),
            ("Exit             Ctrl+Q", Action::Quit),
        ],
        1 => &[
            ("Rename playlist       F2", Action::BeginRenamePlaylist),
            ("Remove selected      Del", Action::RemoveSelection),
            ("Add to playback queue", Action::QueueSelection),
            ("Move selection up  Alt+Up", Action::MovePlaylistItems(-1)),
            ("Move selection down Alt+Dn", Action::MovePlaylistItems(1)),
            ("Close playlist    Ctrl+W", Action::ClosePlaylist),
        ],
        2 => &[
            ("Keyboard shortcuts    F1", Action::ToggleHelp),
            ("Preferences…      Ctrl+,", Action::OpenSettings),
        ],
        3 => &[
            ("Play / Pause       Space", Action::TogglePlay),
            ("Stop", Action::Stop),
            ("Previous", Action::Previous),
            ("Next", Action::Next),
            ("Change playback order", Action::CyclePlaybackOrder),
            ("Stop after current", Action::ToggleStopAfterCurrent),
            ("Scan ReplayGain", Action::ScanReplayGain),
            ("Retry audio", Action::RetryAudio),
        ],
        4 => &[
            ("Add folder… Ctrl+Shift+O", Action::OpenFolder),
            ("Add from clipboard", Action::AddFromClipboard),
            ("Rescan library", Action::RescanLibrary),
            ("Connect to server…", Action::BeginConnect),
            ("Disconnect server", Action::DisconnectServer),
            ("Soulseek…", Action::BeginSoulseek),
        ],
        _ => &[("About Staccato", Action::ToggleHelp)],
    }
}

impl App {
    pub(crate) fn open_list_context(&mut self, row: Option<usize>, x: Option<u16>, y: Option<u16>) {
        if self.soulseek_open {
            self.open_soulseek_context(row, x, y);
            return;
        }
        if self.queue_open {
            if let Some(index) = row {
                self.queue_selection = index.min(self.queue.len().saturating_sub(1));
            }
            self.focus = Focus::Queue;
        } else if self.focus == Focus::AlbumList || self.focus == Focus::AlbumFilter {
            if let Some(index) = row {
                let len = self.visible_album_entries().len();
                self.album_selection = super::util::clamp_index(index, len);
            }
            self.focus = Focus::AlbumList;
        } else if let Some(index) = row {
            let visible = self.visible_playlist_indices();
            if let Some(&item) = visible.get(index) {
                self.playlist_selection = item;
            }
            self.focus = Focus::Playlist;
        }
        self.open_context_menu(self.list_context_items(), x.zip(y));
    }

    fn list_context_items(&self) -> Vec<(String, Action)> {
        if self.queue_open {
            return vec![
                ("Play".into(), Action::ActivateSelection),
                ("Remove from queue".into(), Action::RemoveSelection),
                ("Move up".into(), Action::MovePlaylistItems(-1)),
                ("Move down".into(), Action::MovePlaylistItems(1)),
                ("Clear queue".into(), Action::ClearQueue),
                ("Properties".into(), Action::ShowProperties),
                (
                    "Open containing folder".into(),
                    Action::OpenContainingFolder,
                ),
            ];
        }
        let mut items = vec![
            (
                if self.focus == Focus::AlbumList {
                    "Add to playlist".into()
                } else {
                    "Play".into()
                },
                Action::ActivateSelection,
            ),
            ("Add to playback queue".into(), Action::QueueSelection),
        ];
        if self.focus == Focus::Playlist {
            items.push(("Remove".into(), Action::RemoveSelection));
            items.push(("Move up".into(), Action::MovePlaylistItems(-1)));
            items.push(("Move down".into(), Action::MovePlaylistItems(1)));
        }
        for (index, playlist) in self.playlists.iter().enumerate() {
            items.push((
                format!("Add to {}", playlist.name),
                Action::AddSelectionToPlaylist(index),
            ));
        }
        items.push(("Scan ReplayGain".into(), Action::ScanReplayGain));
        items.push(("Properties".into(), Action::ShowProperties));
        items.push((
            "Open containing folder".into(),
            Action::OpenContainingFolder,
        ));
        items
    }

    pub(crate) fn show_properties(&mut self) {
        let ids = self.selected_context_tracks();
        if ids.is_empty() {
            self.status = "Nothing selected".into();
            return;
        }
        if ids.len() > 1 {
            self.overlay = Overlay::Properties {
                title: " Properties ".into(),
                body: format!("{} tracks selected", ids.len()),
            };
            return;
        }
        let Some(track) = self.tracks.get(&ids[0]) else {
            return;
        };
        self.overlay = Overlay::Properties {
            title: " Properties ".into(),
            body: format!(
                "Title: {}\nArtist: {}\nAlbum: {}\nDate: {}\nTrack: {}\nTime: {}\nCodec: {} | {} Hz | {} ch\nSize: {} bytes\nReplayGain: {}\nPath: {}",
                track.title,
                track.artist,
                track.album,
                track
                    .date
                    .map(|date| date.to_string())
                    .unwrap_or_else(|| "—".into()),
                track
                    .track_number
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "—".into()),
                format_duration(track.duration),
                track.codec,
                track
                    .sample_rate
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "?".into()),
                track
                    .channels
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "?".into()),
                track.file_size,
                format_replay_gain(&track.replay_gain),
                track.path.display()
            ),
        };
    }

    pub(crate) fn open_containing_folder(&mut self) {
        let ids = self.selected_context_tracks();
        let Some(track) = ids.first().and_then(|id| self.tracks.get(id)) else {
            self.status = "Nothing selected".into();
            return;
        };
        let Some(parent) = track.path.parent() else {
            self.status = "Track has no parent folder".into();
            return;
        };
        match std::process::Command::new("xdg-open").arg(parent).spawn() {
            Ok(_) => self.status = format!("Opened {}", parent.display()),
            Err(error) => self.status = format!("Could not open folder: {error}"),
        }
    }
}

fn format_replay_gain(info: &crate::model::ReplayGainInfo) -> String {
    let fmt = |gain: Option<f32>, peak: Option<f32>| match (gain, peak) {
        (Some(gain), Some(peak)) => format!("{gain:+.2} dB, peak {peak:.3}"),
        (Some(gain), None) => format!("{gain:+.2} dB"),
        _ => "—".into(),
    };
    format!(
        "track {} | album {}",
        fmt(info.track_gain, info.track_peak),
        fmt(info.album_gain, info.album_peak)
    )
}
