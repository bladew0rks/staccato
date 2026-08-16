mod library;
mod menus;
mod overlay;
mod playback;
mod playlists;
mod prefs;
mod remote;
mod soulseek;
mod util;

#[cfg(test)]
mod tests;

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Result;
use crossbeam_channel::Receiver;

use crate::{
    action::Action,
    audio::{AudioDevice, AudioEngine, AudioSnapshot, SilentEngine, create_engine, output_devices},
    cover::CoverView,
    library::Scanner,
    model::{Focus, PlaybackOrder, Playlist, PlaylistColumn, ReplayGainMode, Track, TrackId},
    net::RemoteHandle,
    replaygain::ReplayGainHandle,
    soulseek::{SoulseekHandle, SoulseekUi},
    storage::Store,
};

#[allow(unused_imports)]
pub use library::AlbumEntry;
pub use menus::menu_actions;
#[allow(unused_imports)]
pub use overlay::{Overlay, PathPicker, PickerEntry, PickerMode};

use playback::StagedPlayback;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContentPane {
    Playlist,
    Queue,
    Soulseek,
    Settings,
}

pub struct App {
    pub tracks: BTreeMap<TrackId, Track>,
    pub playlists: Vec<Playlist>,
    pub active_playlist: usize,
    pub playlist_selection: usize,
    pub album_selection: usize,
    pub focus: Focus,
    pub playback_order: PlaybackOrder,
    pub status: String,
    pub overlay: Overlay,
    pub should_quit: bool,
    pub scan_progress: Option<(usize, usize)>,
    pub audio_error: Option<String>,
    pub no_audio: bool,
    pub audio_snapshot: AudioSnapshot,
    pub playing: Option<(crate::model::PlaylistId, usize)>,
    store: Store,
    scanner: Scanner,
    scan_receiver: Receiver<crate::library::ScanEvent>,
    audio: Box<dyn AudioEngine>,
    scan_seen: usize,
    shuffle: Vec<usize>,
    shuffle_cursor: usize,
    shuffle_playlist: Option<crate::model::PlaylistId>,
    data_dir: PathBuf,
    remote: Option<RemoteHandle>,
    remote_name: Option<String>,
    remote_fingerprint: Option<String>,
    remote_revision: Option<u64>,
    pending_play: Option<(usize, usize)>,
    discovery: Option<RemoteHandle>,
    soulseek: Option<SoulseekHandle>,
    pub soulseek_open: bool,
    pub soulseek_ui: SoulseekUi,
    pub soulseek_downloads_active: usize,
    pub soulseek_throbber: throbber_widgets_tui::ThrobberState,
    pub covers: CoverView,
    pub album_filter: String,
    pub playlist_filter: String,
    pub album_marked: BTreeSet<usize>,
    pub playlist_marked: BTreeSet<TrackId>,
    pub album_anchor: usize,
    pub playlist_anchor: usize,
    pub queue: Vec<TrackId>,
    pub queue_open: bool,
    pub queue_selection: usize,
    pub stop_after_current: bool,
    pub cursor_follows_playback: bool,
    pub playlist_sort: Option<(PlaylistColumn, bool)>,
    pub replay_gain_mode: ReplayGainMode,
    pub replay_gain_preamp: f32,
    pub replay_gain_prevent_clip: bool,
    pub show_album_art: bool,
    pub show_spectrum: bool,
    pub nerd_font: bool,
    pub output_devices: Vec<AudioDevice>,
    pub preferred_output_device: Option<String>,
    pub settings_open: bool,
    pub settings_selected: usize,
    pub(crate) album_scroll_offset: usize,
    pub(crate) playlist_scroll_offset: usize,
    pub(crate) queue_scroll_offset: usize,
    pub(crate) soulseek_scroll_offset: usize,
    pub(crate) settings_scroll_offset: usize,
    pending_track: Option<TrackId>,
    album_groups: Vec<Vec<usize>>,
    replaygain: Option<ReplayGainHandle>,
    staged_playback: Option<StagedPlayback>,
    playback_generation: u64,
    audio_started_generation: Option<u64>,
}

impl App {
    pub fn open(database: &Path, no_audio: bool) -> Result<Self> {
        let mut store = Store::open(database)?;
        let tracks = store.load_tracks()?;
        let mut playlists = store.load_playlists()?;
        if playlists.is_empty() {
            playlists.push(store.create_playlist("Default", 0)?);
        }
        let saved = store.load_state()?;
        let active_playlist = saved.active_playlist.min(playlists.len().saturating_sub(1));
        let scanner = Scanner::new();
        let scan_receiver = scanner.receiver();
        let (audio, audio_error) = match create_engine(
            no_audio,
            saved.volume,
            saved.preferred_output_device.as_deref(),
        ) {
            Ok(engine) => (engine, None),
            Err(error) => (
                Box::new(SilentEngine::new(saved.volume)) as Box<dyn AudioEngine>,
                Some(error.to_string()),
            ),
        };
        let audio_snapshot = audio.snapshot();
        let data_dir = database
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let mut app = Self {
            tracks,
            playlists,
            active_playlist,
            playlist_selection: 0,
            album_selection: 0,
            focus: Focus::Playlist,
            playback_order: saved.playback_order,
            status: "Ready".to_owned(),
            overlay: Overlay::None,
            should_quit: false,
            scan_progress: None,
            audio_error,
            no_audio,
            audio_snapshot,
            playing: None,
            store,
            scanner,
            scan_receiver,
            audio,
            scan_seen: 0,
            shuffle: Vec::new(),
            shuffle_cursor: 0,
            shuffle_playlist: None,
            data_dir,
            remote: None,
            remote_name: None,
            remote_fingerprint: None,
            remote_revision: None,
            pending_play: None,
            discovery: None,
            soulseek: None,
            soulseek_open: false,
            soulseek_ui: SoulseekUi::default(),
            soulseek_downloads_active: 0,
            soulseek_throbber: throbber_widgets_tui::ThrobberState::default(),
            covers: CoverView::default(),
            album_filter: String::new(),
            playlist_filter: String::new(),
            album_marked: BTreeSet::new(),
            playlist_marked: BTreeSet::new(),
            album_anchor: 0,
            playlist_anchor: 0,
            queue: Vec::new(),
            queue_open: false,
            queue_selection: 0,
            stop_after_current: false,
            cursor_follows_playback: saved.cursor_follows_playback,
            playlist_sort: None,
            replay_gain_mode: saved.replay_gain_mode,
            replay_gain_preamp: saved.replay_gain_preamp,
            replay_gain_prevent_clip: saved.replay_gain_prevent_clip,
            show_album_art: saved.show_album_art,
            show_spectrum: saved.show_spectrum,
            nerd_font: saved.nerd_font,
            output_devices: if no_audio {
                Vec::new()
            } else {
                output_devices()
            },
            preferred_output_device: saved.preferred_output_device,
            settings_open: false,
            settings_selected: crate::settings::first_item(),
            album_scroll_offset: 0,
            playlist_scroll_offset: 0,
            queue_scroll_offset: 0,
            soulseek_scroll_offset: 0,
            settings_scroll_offset: 0,
            pending_track: None,
            album_groups: Vec::new(),
            replaygain: None,
            staged_playback: None,
            playback_generation: 0,
            audio_started_generation: None,
        };
        app.rebuild_shuffle();
        let roots = app.store.load_roots()?;
        if !roots.is_empty() {
            app.begin_scan(roots, false);
        }
        Ok(app)
    }

    pub(crate) fn content_pane(&self) -> ContentPane {
        if self.soulseek_open {
            ContentPane::Soulseek
        } else if self.queue_open {
            ContentPane::Queue
        } else if self.settings_open {
            ContentPane::Settings
        } else {
            ContentPane::Playlist
        }
    }

    pub fn handle(&mut self, action: Action) {
        if let Err(error) = self.try_handle(action) {
            self.status = format!("Error: {error:#}");
        }
    }

    fn try_handle(&mut self, action: Action) -> Result<()> {
        match action {
            Action::None => {}
            Action::Quit => self.should_quit = true,
            Action::TogglePlay => self.toggle_play()?,
            Action::Stop => {
                self.clear_transport();
                self.status = "Stopped".into();
            }
            Action::Previous => self.previous()?,
            Action::Next => self.advance(false)?,
            Action::SeekRelative(seconds) => {
                let now = self.audio.snapshot().position.as_secs() as i64;
                self.audio.seek(Duration::from_secs(
                    now.saturating_add(seconds).max(0) as u64
                ))?;
            }
            Action::SeekFraction(fraction) => {
                let duration = self.audio.snapshot().duration;
                self.audio
                    .seek(duration.mul_f64(fraction.clamp(0.0, 1.0)))?;
            }
            Action::VolumeRelative(delta) => {
                let volume = (self.audio.snapshot().volume + delta).clamp(0.0, 1.0);
                self.audio.set_volume(volume);
                self.apply_replay_gain();
                self.status = format!("Volume: {:.0}%", volume * 100.0);
                self.save_state()?;
            }
            Action::SetVolume(volume) => {
                self.audio.set_volume(volume.clamp(0.0, 1.0));
                self.apply_replay_gain();
                self.save_state()?;
            }
            Action::CyclePlaybackOrder => {
                self.playback_order = self.playback_order.next();
                self.rebuild_shuffle();
                self.restage_successor();
                self.status = format!("Playback order: {}", self.playback_order.label());
                self.save_state()?;
            }
            Action::SelectPlaylist(index) => self.select_playlist_tab(index)?,
            Action::SelectAlbumRow(index) => self.select_album_row(index),
            Action::SelectPlaylistRow(index) => self.select_playlist_row(index),
            Action::NewPlaylist => self.new_playlist()?,
            Action::BeginRenamePlaylist => self.begin_rename_playlist(),
            Action::RenamePlaylist(id, name) => self.rename_playlist(id, name)?,
            Action::ClosePlaylist => self.close_current_view()?,
            Action::MoveSelection(delta) => self.move_selection(delta),
            Action::PageSelection(delta) => self.move_selection(delta.saturating_mul(10)),
            Action::ActivateSelection => self.activate_selection()?,
            Action::RemoveSelection => self.remove_current()?,
            Action::AddPaths(paths) => self.add_paths(paths)?,
            Action::AddFromClipboard => match crate::drop::clipboard_paths() {
                Ok(paths) => self.add_paths(paths)?,
                Err(error) => self.status = error,
            },
            Action::OpenFiles => {
                self.overlay = Overlay::PathPicker(PathPicker::new(PickerMode::Files))
            }
            Action::OpenFolder => {
                self.overlay = Overlay::PathPicker(PathPicker::new(PickerMode::Folder))
            }
            Action::ToggleHelp => {
                self.overlay = if matches!(self.overlay, Overlay::Help) {
                    Overlay::None
                } else {
                    Overlay::Help
                };
            }
            Action::OpenMenu(menu) => self.overlay = Overlay::Menu { menu, selected: 0 },
            Action::ActivateMenuItem(selected) => self.activate_menu_item(selected)?,
            Action::OverlayMove(delta) => self.overlay_move(delta),
            Action::OverlayActivate => self.overlay_activate()?,
            Action::TextInput(character) => self.text_input(character),
            Action::PasteText(text) => {
                for character in text.chars() {
                    self.text_input(character);
                }
            }
            Action::TextBackspace => self.text_backspace(),
            Action::PickerChooseCurrent => self.picker_choose_current()?,
            Action::CloseOverlay => self.overlay = Overlay::None,
            Action::FocusNext(backwards) => {
                let ring = self.focus_ring();
                self.focus = self.focus.cycle(backwards, &ring);
            }
            Action::RetryAudio => self.retry_audio()?,
            Action::RescanLibrary => self.rescan_library(),
            Action::BeginConnect => self.begin_connect(),
            Action::DisconnectServer => self.disconnect_remote(),
            Action::SubmitConnect => self.submit_connect()?,
            Action::SubmitPair => self.submit_pair(),
            Action::BeginSoulseek => self.open_soulseek_tab()?,
            Action::SoulseekFold(expand) => {
                if self.soulseek_open {
                    self.soulseek_ui.toggle(Some(expand));
                }
            }
            Action::SoulseekDownload(scope) => self.soulseek_download(scope),
            Action::SoulseekHide(scope) => self.soulseek_hide(scope),
            Action::SoulseekSetFormat(format) => self.apply_soulseek_format(format),
            Action::SoulseekCycleFormat(delta) => self.cycle_soulseek_format(delta),
            Action::SoulseekToggleFreeSlot => self.toggle_soulseek_free_slot(),
            Action::BeginFilter => self.begin_filter(),
            Action::ClearFilter => self.clear_filter(),
            Action::ToggleMark => self.toggle_mark(),
            Action::TogglePlaylistRow(index) => self.toggle_playlist_row(index),
            Action::ExtendSelection(delta) => self.extend_selection(delta),
            Action::MovePlaylistItems(delta) => self.move_playlist_items(delta)?,
            Action::SortPlaylist(column) => self.sort_playlist(column)?,
            Action::OpenListContext { row, x, y } => self.open_list_context(row, x, y),
            Action::QueueSelection => self.queue_selection(),
            Action::ClearQueue => {
                self.queue.clear();
                self.queue_selection = 0;
                self.restage_successor();
                self.status = "Playback queue cleared".into();
            }
            Action::ToggleStopAfterCurrent => {
                self.stop_after_current = !self.stop_after_current;
                self.restage_successor();
                self.status = if self.stop_after_current {
                    "Stop after current: on".into()
                } else {
                    "Stop after current: off".into()
                };
            }
            Action::ShowProperties => self.show_properties(),
            Action::OpenContainingFolder => self.open_containing_folder(),
            Action::AddSelectionToPlaylist(index) => self.add_selection_to_playlist(index)?,
            Action::ScanReplayGain => self.begin_replaygain_scan(),
            Action::OpenSettings => self.open_settings_tab(),
            Action::SettingsAdjust(delta) => self.adjust_setting(delta)?,
        }
        Ok(())
    }

    pub fn tick(&mut self) {
        if self.soulseek_downloads_active > 0 {
            self.soulseek_throbber.calc_next();
        }
        while let Ok(event) = self.scan_receiver.try_recv() {
            if let Err(error) = self.process_scan_event(event) {
                self.status = format!("Library error: {error:#}");
            }
        }
        self.drain_remote_events();
        self.drain_audio_events();
        self.drain_soulseek_events();
        self.drain_replaygain_events();
        self.audio_snapshot = self.audio.snapshot();
        let cache_dir = self.cache_dir();
        let cover_track = self.cover_track().cloned();
        self.covers.sync(cover_track.as_ref(), &cache_dir);
    }
}
