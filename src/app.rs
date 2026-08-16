use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    net::ToSocketAddrs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use crossbeam_channel::Receiver;

use crate::{
    action::Action,
    audio::{
        AudioDevice, AudioEngine, AudioEvent, AudioSnapshot, MediaSource, SilentEngine,
        StagedTrack, create_engine, output_devices,
    },
    cover::CoverView,
    library::{ScanEvent, Scanner, is_supported},
    model::{
        Focus, PlaybackOrder, PlaybackState, Playlist, PlaylistColumn, PlaylistId, ReplayGainMode,
        Track, TrackId, TrackOrigin, format_duration, text_matches,
    },
    net::{
        self, DiscoveredServer, RemoteEvent, RemoteHandle, cache_is_complete, cache_path, connect,
    },
    replaygain::{self, ReplayGainEvent, ReplayGainHandle},
    soulseek::{
        self, SoulseekEvent, SoulseekHandle, SoulseekPhase, SoulseekRowKind, SoulseekScope,
        SoulseekUi,
    },
    storage::{SavedState, Store},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerMode {
    Files,
    Folder,
}

#[derive(Clone, Debug)]
pub struct PickerEntry {
    pub path: PathBuf,
    pub is_dir: bool,
}

#[derive(Clone, Debug)]
pub struct PathPicker {
    pub mode: PickerMode,
    pub directory: PathBuf,
    pub entries: Vec<PickerEntry>,
    pub selected: usize,
}

impl PathPicker {
    fn new(mode: PickerMode) -> Self {
        let directory = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let mut picker = Self {
            mode,
            directory,
            entries: Vec::new(),
            selected: 0,
        };
        picker.refresh();
        picker
    }

    fn refresh(&mut self) {
        self.entries.clear();
        if let Some(parent) = self.directory.parent() {
            self.entries.push(PickerEntry {
                path: parent.to_path_buf(),
                is_dir: true,
            });
        }
        if let Ok(entries) = fs::read_dir(&self.directory) {
            self.entries
                .extend(entries.filter_map(Result::ok).filter_map(|entry| {
                    let path = entry.path();
                    let is_dir = path.is_dir();
                    (is_dir || (self.mode == PickerMode::Files && is_supported(&path)))
                        .then_some(PickerEntry { path, is_dir })
                }));
        }
        self.entries.sort_by(|a, b| {
            b.is_dir.cmp(&a.is_dir).then_with(|| {
                a.path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase()
                    .cmp(
                        &b.path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_lowercase(),
                    )
            })
        });
        self.selected = self.selected.min(self.entries.len().saturating_sub(1));
    }
}

#[derive(Clone, Debug)]
pub enum Overlay {
    None,
    Help,
    Menu {
        menu: usize,
        selected: usize,
    },
    PathPicker(PathPicker),
    Rename {
        playlist_id: PlaylistId,
        text: String,
    },
    Connect {
        text: String,
        discovered: Vec<DiscoveredServer>,
    },
    Pair {
        text: String,
    },
    ContextMenu {
        selected: usize,
        items: Vec<(String, Action)>,
        at: Option<(u16, u16)>,
    },
    Properties {
        title: String,
        body: String,
    },
}

#[derive(Clone, Debug)]
pub struct AlbumEntry {
    pub depth: u8,
    pub label: String,
    pub track_id: Option<TrackId>,
    pub track_ids: Vec<TrackId>,
    pub unavailable: bool,
}

#[derive(Clone, Debug)]
struct StagedPlayback {
    track_id: TrackId,
    generation: u64,
    playlist: Option<(PlaylistId, usize)>,
    queue_index: Option<usize>,
}

#[derive(Clone, Debug)]
struct PlaybackCandidate {
    track_id: TrackId,
    playlist: Option<(PlaylistId, usize)>,
    queue_index: Option<usize>,
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
    pub playing: Option<(PlaylistId, usize)>,
    store: Store,
    scanner: Scanner,
    scan_receiver: Receiver<ScanEvent>,
    audio: Box<dyn AudioEngine>,
    scan_seen: usize,
    shuffle: Vec<usize>,
    shuffle_cursor: usize,
    shuffle_playlist: Option<PlaylistId>,
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
                self.audio.stop();
                self.playing = None;
                self.staged_playback = None;
                self.audio_started_generation = None;
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
            Action::SelectPlaylist(index) => {
                if index == self.playlists.len() {
                    self.open_queue_tab();
                } else if index == self.playlists.len() + 1 {
                    self.open_soulseek_tab()?;
                } else if index == self.playlists.len() + 2 {
                    self.open_settings_tab();
                } else if index < self.playlists.len() {
                    self.close_special_tabs();
                    self.active_playlist = index;
                    self.playlist_selection = 0;
                    if matches!(
                        self.focus,
                        Focus::SoulseekQuery
                            | Focus::SoulseekFilter
                            | Focus::Queue
                            | Focus::Settings
                            | Focus::AlbumFilter
                            | Focus::PlaylistFilter
                    ) {
                        self.focus = Focus::Playlist;
                    }
                    self.rebuild_shuffle();
                    self.save_state()?;
                }
            }
            Action::SelectAlbumRow(index) => {
                self.focus = Focus::AlbumList;
                let len = self.visible_album_entries().len();
                self.album_selection = if len == 0 { 0 } else { index.min(len - 1) };
                self.album_anchor = self.album_selection;
                self.album_marked.clear();
                if len > 0 {
                    self.album_marked.insert(self.album_selection);
                }
            }
            Action::SelectPlaylistRow(index) => {
                if self.soulseek_open {
                    self.focus = Focus::Playlist;
                    let len = self.soulseek_ui.visible_rows().len();
                    self.soulseek_ui.selected = if len == 0 { 0 } else { index.min(len - 1) };
                } else if self.queue_open {
                    self.focus = Focus::Queue;
                    self.queue_selection = index.min(self.queue.len().saturating_sub(1));
                } else if self.settings_open {
                    self.focus = Focus::Settings;
                    if crate::settings::ROWS
                        .get(index)
                        .is_some_and(|row| row.is_item())
                    {
                        self.settings_selected = index;
                    }
                } else {
                    self.focus = Focus::Playlist;
                    let visible = self.visible_playlist_indices();
                    if let Some(&item) = visible.get(index) {
                        self.playlist_selection = item;
                        self.playlist_anchor = item;
                        self.playlist_marked.clear();
                        if let Some(id) = self.active_playlist().items.get(item) {
                            self.playlist_marked.insert(*id);
                        }
                    }
                }
            }
            Action::NewPlaylist => {
                let name = unique_playlist_name(&self.playlists);
                let playlist = self.store.create_playlist(&name, self.playlists.len())?;
                self.playlists.push(playlist);
                self.active_playlist = self.playlists.len() - 1;
                self.playlist_selection = 0;
                self.rebuild_shuffle();
                self.save_state()?;
            }
            Action::BeginRenamePlaylist => {
                let playlist = &self.playlists[self.active_playlist];
                self.overlay = Overlay::Rename {
                    playlist_id: playlist.id,
                    text: playlist.name.clone(),
                };
            }
            Action::RenamePlaylist(id, name) => {
                let name = name.trim();
                if !name.is_empty() {
                    self.store.rename_playlist(id, name)?;
                    if let Some(playlist) = self.playlists.iter_mut().find(|p| p.id == id) {
                        playlist.name = name.to_owned();
                    }
                }
                self.overlay = Overlay::None;
            }
            Action::ClosePlaylist => {
                if self.soulseek_open {
                    self.soulseek_open = false;
                    if matches!(self.focus, Focus::SoulseekQuery | Focus::SoulseekFilter) {
                        self.focus = Focus::Playlist;
                    }
                } else if self.queue_open {
                    self.queue_open = false;
                    self.focus = Focus::Playlist;
                } else if self.settings_open {
                    self.settings_open = false;
                    self.focus = Focus::Playlist;
                } else {
                    self.close_active_playlist()?;
                }
            }
            Action::MoveSelection(delta) => self.move_selection(delta),
            Action::PageSelection(delta) => self.move_selection(delta.saturating_mul(10)),
            Action::ActivateSelection => self.activate_selection()?,
            Action::RemoveSelection => {
                if self.soulseek_open {
                    if let Some(kind) = self.soulseek_ui.selected_kind() {
                        self.soulseek_hide(match kind {
                            SoulseekRowKind::File => SoulseekScope::File,
                            SoulseekRowKind::Folder => SoulseekScope::Folder,
                            SoulseekRowKind::User => SoulseekScope::User,
                        });
                    }
                } else if self.queue_open {
                    self.remove_from_queue();
                } else {
                    self.remove_selection()?;
                }
            }
            Action::AddPaths(paths) => self.add_paths(paths)?,
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
            Action::ActivateMenuItem(selected) => match self.overlay.clone() {
                Overlay::Menu { menu, .. } => {
                    self.overlay = Overlay::None;
                    if let Some(action) = menu_actions(menu)
                        .get(selected)
                        .map(|(_, action)| action.clone())
                    {
                        self.try_handle(action)?;
                    }
                }
                Overlay::ContextMenu { items, .. } => {
                    self.overlay = Overlay::None;
                    if let Some((_, action)) = items.get(selected) {
                        self.try_handle(action.clone())?;
                    }
                }
                _ => {}
            },
            Action::OverlayMove(delta) => self.overlay_move(delta),
            Action::OverlayActivate => self.overlay_activate()?,
            Action::TextInput(character) => self.text_input(character),
            Action::TextBackspace => self.text_backspace(),
            Action::PickerChooseCurrent => self.picker_choose_current()?,
            Action::CloseOverlay => self.overlay = Overlay::None,
            Action::FocusNext(backwards) => {
                let ring = self.focus_ring();
                self.focus = self.focus.cycle(backwards, &ring);
            }
            Action::RetryAudio => self.retry_audio()?,
            Action::RescanLibrary => {
                let roots = self.store.load_roots()?;
                let mut parts = Vec::new();
                if !roots.is_empty() {
                    self.begin_scan(roots, false);
                    parts.push("local");
                }
                if let Some(remote) = &self.remote {
                    remote.rescan();
                    parts.push("remote");
                }
                self.status = if parts.is_empty() {
                    "No library folders have been added".into()
                } else {
                    format!("Refreshing {} library…", parts.join(" and "))
                };
            }
            Action::BeginConnect => {
                tracing::info!("user opened connect overlay");
                self.discovery = Some(net::browse_mdns());
                self.overlay = Overlay::Connect {
                    text: String::new(),
                    discovered: Vec::new(),
                };
            }
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
            Action::SoulseekSetFormat(format) => {
                if self.soulseek_open {
                    self.focus = Focus::SoulseekFilter;
                    self.soulseek_ui.set_format(format);
                    self.soulseek_refresh_filter_status();
                }
            }
            Action::SoulseekCycleFormat(delta) => {
                if self.soulseek_open {
                    self.focus = Focus::SoulseekFilter;
                    if delta != 0 {
                        self.soulseek_ui.cycle_format(delta);
                    }
                    self.soulseek_refresh_filter_status();
                }
            }
            Action::SoulseekToggleFreeSlot => {
                if self.soulseek_open {
                    self.focus = Focus::SoulseekFilter;
                    self.soulseek_ui.toggle_free_slot();
                    self.soulseek_refresh_filter_status();
                }
            }
            Action::BeginFilter => self.begin_filter(),
            Action::ClearFilter => self.clear_filter(),
            Action::ToggleMark => self.toggle_mark(),
            Action::TogglePlaylistRow(index) => {
                self.focus = Focus::Playlist;
                let visible = self.visible_playlist_indices();
                if let Some(&item) = visible.get(index) {
                    self.playlist_selection = item;
                    if let Some(&id) = self.active_playlist().items.get(item)
                        && !self.playlist_marked.remove(&id)
                    {
                        self.playlist_marked.insert(id);
                    }
                }
            }
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

    pub fn cache_dir(&self) -> PathBuf {
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

    fn process_scan_event(&mut self, event: ScanEvent) -> Result<()> {
        match event {
            ScanEvent::Started {
                total_hint,
                add_to_playlist,
            } => {
                self.scan_seen = 0;
                self.scan_progress = Some((0, total_hint));
                self.status = if add_to_playlist {
                    format!("Adding {total_hint} files…")
                } else {
                    format!("Refreshing {total_hint} library files…")
                };
            }
            ScanEvent::Track {
                track,
                add_to_playlist,
            } => {
                self.scan_seen += 1;
                if let Some((_, total)) = self.scan_progress {
                    self.scan_progress = Some((self.scan_seen, total));
                }
                let stored = self.store.upsert_track(&track)?;
                let id = stored.id;
                self.tracks.insert(id, stored);
                if add_to_playlist {
                    self.add_track(id)?;
                }
            }
            ScanEvent::Failed { path, error } => {
                self.status = format!("Skipped {}: {error}", path.display());
            }
            ScanEvent::Finished { discovered, failed } => {
                self.scan_progress = None;
                self.status =
                    format!("Library scan complete: {discovered} tracks, {failed} skipped");
            }
        }
        Ok(())
    }

    fn add_paths(&mut self, paths: Vec<PathBuf>) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut normalized = Vec::new();
        for path in paths {
            let path = path.canonicalize().unwrap_or(path);
            if path.is_dir() {
                self.store.add_root(&path)?;
            }
            normalized.push(path);
        }
        self.begin_scan(normalized, true);
        self.overlay = Overlay::None;
        Ok(())
    }

    fn add_track(&mut self, track_id: TrackId) -> Result<()> {
        self.add_tracks_to(self.active_playlist, std::iter::once(track_id))
            .map(|_| ())
    }

    fn add_tracks_to(
        &mut self,
        playlist_index: usize,
        ids: impl IntoIterator<Item = TrackId>,
    ) -> Result<usize> {
        let playlist = &mut self.playlists[playlist_index];
        let mut added = 0;
        for track_id in ids {
            if !playlist.items.contains(&track_id) {
                playlist.items.push(track_id);
                added += 1;
            }
        }
        if added > 0 {
            self.store.save_playlist_items(playlist)?;
            self.rebuild_shuffle();
        }
        Ok(added)
    }

    fn toggle_play(&mut self) -> Result<()> {
        match self.audio.snapshot().state {
            PlaybackState::Playing => self.audio.pause(),
            PlaybackState::Paused => self.audio.play()?,
            PlaybackState::Loading | PlaybackState::Buffering => {
                self.audio.stop();
                self.pending_play = None;
                self.pending_track = None;
                self.playing = None;
                self.staged_playback = None;
                self.audio_started_generation = None;
                self.status = "Playback canceled".into();
            }
            PlaybackState::Stopped => self.play_selected()?,
        }
        Ok(())
    }

    fn play_selected(&mut self) -> Result<()> {
        if self.playlists[self.active_playlist].items.is_empty() {
            return Err(anyhow!("the active playlist is empty"));
        }
        self.play_at(self.active_playlist, self.playlist_selection)
    }

    fn play_at(&mut self, playlist_index: usize, item_index: usize) -> Result<()> {
        let playlist_id = self
            .playlists
            .get(playlist_index)
            .context("playlist disappeared")?
            .id;
        let track_id = *self
            .playlists
            .get(playlist_index)
            .and_then(|playlist| playlist.items.get(item_index))
            .context("track selection is out of range")?;
        let track = self
            .tracks
            .get(&track_id)
            .context("track metadata is missing")?
            .clone();
        if track.unavailable {
            return Err(anyhow!("file is unavailable: {}", track.path.display()));
        }
        if let Some(error) = &track.scan_error {
            return Err(anyhow!("{}: {error}", track.path.display()));
        }
        let source = match &track.origin {
            TrackOrigin::Local => MediaSource::LocalFile(track.path.clone()),
            TrackOrigin::Remote {
                fingerprint,
                remote_id,
                ..
            } => {
                let cached = cache_path(&self.cache_dir(), fingerprint, remote_id);
                if cache_is_complete(&cached, track.file_size) {
                    MediaSource::LocalFile(cached)
                } else if let Some(remote) = &self.remote {
                    let etag = format!("{}-{}", track.modified_ns, track.file_size);
                    remote.fetch(remote_id.clone(), track.file_size, etag);
                    self.audio
                        .set_pending(track.id, track.duration, PlaybackState::Buffering);
                    self.staged_playback = None;
                    self.audio_started_generation = None;
                    self.pending_play = Some((playlist_index, item_index));
                    self.status = format!("Buffering {} — {}", track.artist, track.title);
                    self.prefetch_neighbors(playlist_index, item_index);
                    return Ok(());
                } else {
                    return Err(anyhow!(
                        "not connected to {}",
                        self.remote_name.as_deref().unwrap_or("the server")
                    ));
                }
            }
        };
        self.audio
            .set_pending(track.id, track.duration, PlaybackState::Loading);
        let generation = self.next_playback_generation();
        let gain = self.gain_for_track(&track);
        self.audio.load_and_play(StagedTrack {
            source,
            track_id: track.id,
            duration: track.duration,
            gain,
            generation,
        })?;
        self.audio_started_generation = None;
        self.playing = Some((playlist_id, item_index));
        self.pending_play = None;
        self.pending_track = None;
        self.apply_replay_gain();
        if self.cursor_follows_playback
            && !self.queue_open
            && !self.soulseek_open
            && !self.settings_open
            && self
                .playlists
                .get(self.active_playlist)
                .is_some_and(|playlist| playlist.id == playlist_id)
        {
            self.playlist_selection = item_index;
        }
        self.status = match &track.origin {
            TrackOrigin::Remote { server_name, .. } => {
                format!(
                    "Streaming from {server_name}: {} — {}",
                    track.artist, track.title
                )
            }
            TrackOrigin::Local => format!("Playing: {} — {}", track.artist, track.title),
        };
        self.prefetch_neighbors(playlist_index, item_index);
        Ok(())
    }

    fn previous(&mut self) -> Result<()> {
        if self.audio.snapshot().position >= Duration::from_secs(5) {
            return self.audio.seek(Duration::ZERO);
        }
        let Some((playlist_id, index)) = self.playing else {
            return self.play_selected();
        };
        let playlist_index = self
            .playlists
            .iter()
            .position(|p| p.id == playlist_id)
            .unwrap_or(self.active_playlist);
        let previous = index.saturating_sub(1);
        self.play_at(playlist_index, previous)
    }

    fn advance(&mut self, automatic: bool) -> Result<()> {
        if automatic && self.stop_after_current {
            self.stop_after_current = false;
            self.audio.stop();
            self.playing = None;
            self.staged_playback = None;
            self.audio_started_generation = None;
            self.status = "Stopped after current".into();
            return Ok(());
        }
        if !self.queue.is_empty() {
            let id = self.queue.remove(0);
            self.queue_selection = self.queue_selection.min(self.queue.len().saturating_sub(1));
            return self.play_track_id(id);
        }
        let Some((playlist_id, index)) = self.playing else {
            return if automatic {
                Ok(())
            } else {
                self.play_selected()
            };
        };
        let playlist_index = self
            .playlists
            .iter()
            .position(|p| p.id == playlist_id)
            .context("playing playlist was closed")?;
        let len = self.playlists[playlist_index].items.len();
        if len == 0 {
            self.audio.stop();
            self.playing = None;
            self.audio_started_generation = None;
            return Ok(());
        }
        let candidates: Vec<usize> = match self.playback_order {
            PlaybackOrder::RepeatTrack if automatic => vec![index],
            PlaybackOrder::Shuffle => {
                if self.shuffle.len() != len || self.shuffle_playlist != Some(playlist_id) {
                    self.rebuild_shuffle_for(playlist_index);
                }
                (1..=len)
                    .map(|step| self.shuffle[(self.shuffle_cursor + step) % len])
                    .collect()
            }
            PlaybackOrder::ShuffleAlbums => self.album_shuffle_candidates(playlist_index, index),
            PlaybackOrder::RepeatPlaylist => (1..=len).map(|step| (index + step) % len).collect(),
            PlaybackOrder::Default | PlaybackOrder::RepeatTrack => {
                if index + 1 >= len {
                    self.audio.stop();
                    self.playing = None;
                    self.audio_started_generation = None;
                    self.status = "Playback finished".into();
                    return Ok(());
                }
                ((index + 1)..len).collect()
            }
        };
        for next in candidates {
            match self.play_at(playlist_index, next) {
                Ok(()) => {
                    if self.playback_order == PlaybackOrder::Shuffle {
                        self.shuffle_cursor = self
                            .shuffle
                            .iter()
                            .position(|candidate| *candidate == next)
                            .unwrap_or(self.shuffle_cursor);
                    }
                    return Ok(());
                }
                Err(error) => self.status = format!("Skipped unplayable track: {error:#}"),
            }
        }
        self.audio.stop();
        self.playing = None;
        self.staged_playback = None;
        self.audio_started_generation = None;
        self.status = "No playable tracks remain".into();
        Ok(())
    }

    fn remove_selection(&mut self) -> Result<()> {
        let mut remove: BTreeSet<TrackId> = self.playlist_marked.clone();
        if remove.is_empty()
            && let Some(&id) = self.active_playlist().items.get(self.playlist_selection)
        {
            remove.insert(id);
        }
        if remove.is_empty() {
            return Ok(());
        }
        let playing = self.audio.snapshot().track_id;
        if playing.is_some_and(|id| remove.contains(&id)) {
            self.audio.stop();
            self.playing = None;
            self.staged_playback = None;
            self.audio_started_generation = None;
        }
        let playlist = &mut self.playlists[self.active_playlist];
        playlist.items.retain(|id| !remove.contains(id));
        self.playlist_selection = self
            .playlist_selection
            .min(playlist.items.len().saturating_sub(1));
        self.playlist_marked.clear();
        self.store.save_playlist_items(playlist)?;
        self.rebuild_shuffle();
        self.restage_successor();
        Ok(())
    }

    fn close_active_playlist(&mut self) -> Result<()> {
        if self.playlists.len() == 1 {
            self.status = "At least one playlist must remain".into();
            return Ok(());
        }
        let removed = self.playlists.remove(self.active_playlist);
        if self.playing.is_some_and(|(id, _)| id == removed.id) {
            self.audio.stop();
            self.playing = None;
            self.staged_playback = None;
            self.audio_started_generation = None;
        }
        self.store.delete_playlist(removed.id)?;
        self.active_playlist = self.active_playlist.min(self.playlists.len() - 1);
        self.store.save_playlist_positions(&self.playlists)?;
        self.save_state()?;
        self.rebuild_shuffle();
        self.restage_successor();
        Ok(())
    }

    fn move_selection(&mut self, delta: i32) {
        if self.focus == Focus::AlbumFilter {
            if delta > 0 {
                self.focus = Focus::AlbumList;
            }
            return;
        }
        if self.focus == Focus::PlaylistFilter {
            if delta > 0 {
                self.focus = Focus::Playlist;
            }
            return;
        }
        if self.focus == Focus::AlbumList {
            let len = self.visible_album_entries().len();
            if len == 0 {
                self.album_selection = 0;
            } else {
                self.album_selection =
                    (self.album_selection as i32 + delta).clamp(0, len as i32 - 1) as usize;
            }
            self.album_anchor = self.album_selection;
            self.album_marked.clear();
            if len > 0 {
                self.album_marked.insert(self.album_selection);
            }
            return;
        }
        if self.focus == Focus::Queue {
            let len = self.queue.len();
            if len == 0 {
                self.queue_selection = 0;
            } else {
                self.queue_selection =
                    (self.queue_selection as i32 + delta).clamp(0, len as i32 - 1) as usize;
            }
            return;
        }
        if self.focus == Focus::Settings {
            let step = if delta < 0 { -1 } else { 1 };
            for _ in 0..delta.unsigned_abs() {
                self.settings_selected = crate::settings::step(self.settings_selected, step);
            }
            return;
        }
        if self.soulseek_open {
            if self.focus == Focus::SoulseekQuery {
                if delta > 0 {
                    self.focus = if self.soulseek_filters_available() {
                        Focus::SoulseekFilter
                    } else {
                        Focus::Playlist
                    };
                }
                return;
            }
            if self.focus == Focus::SoulseekFilter {
                if delta > 0 {
                    self.focus = Focus::Playlist;
                } else if delta < 0 {
                    self.focus = Focus::SoulseekQuery;
                }
                return;
            }
            if self.focus == Focus::Playlist {
                let len = self.soulseek_ui.visible_rows().len();
                if len == 0 {
                    if delta < 0 {
                        self.focus = if self.soulseek_filters_available() {
                            Focus::SoulseekFilter
                        } else {
                            Focus::SoulseekQuery
                        };
                    }
                    return;
                }
                let next = self.soulseek_ui.selected as i32 + delta;
                if next < 0 {
                    self.focus = if self.soulseek_filters_available() {
                        Focus::SoulseekFilter
                    } else {
                        Focus::SoulseekQuery
                    };
                    self.soulseek_ui.selected = 0;
                } else {
                    self.soulseek_ui.selected = next.clamp(0, len as i32 - 1) as usize;
                }
                return;
            }
        }
        let visible = self.visible_playlist_indices();
        if visible.is_empty() {
            return;
        }
        let current = visible
            .iter()
            .position(|&index| index == self.playlist_selection)
            .unwrap_or(0);
        let next = (current as i32 + delta).clamp(0, visible.len() as i32 - 1) as usize;
        self.playlist_selection = visible[next];
        self.playlist_anchor = self.playlist_selection;
        self.playlist_marked.clear();
        if let Some(id) = self.active_playlist().items.get(self.playlist_selection) {
            self.playlist_marked.insert(*id);
        }
    }

    fn activate_selection(&mut self) -> Result<()> {
        match self.focus {
            Focus::AlbumList | Focus::AlbumFilter => {
                let ids = self.selected_album_tracks();
                if ids.is_empty() {
                    return Ok(());
                }
                let added = self.add_tracks_to(self.active_playlist, ids)?;
                self.status = format!(
                    "Added {added} track{} to {}",
                    if added == 1 { "" } else { "s" },
                    self.active_playlist().name
                );
            }
            Focus::SoulseekQuery => self.soulseek_activate()?,
            Focus::SoulseekFilter => {
                self.soulseek_ui.toggle_free_slot();
                self.soulseek_refresh_filter_status();
            }
            Focus::Playlist if self.soulseek_open => self.soulseek_activate()?,
            Focus::Playlist | Focus::PlaylistFilter => self.play_selected()?,
            Focus::Queue => {
                if let Some(&id) = self.queue.get(self.queue_selection) {
                    self.queue.remove(self.queue_selection);
                    self.queue_selection =
                        self.queue_selection.min(self.queue.len().saturating_sub(1));
                    self.play_track_id(id)?;
                }
            }
            Focus::Settings => self.adjust_setting(1)?,
            _ => {}
        }
        Ok(())
    }

    fn overlay_move(&mut self, delta: i32) {
        match &mut self.overlay {
            Overlay::Menu { menu, selected } => {
                let len = menu_actions(*menu).len();
                if len > 0 {
                    *selected = (*selected as i32 + delta).rem_euclid(len as i32) as usize;
                }
            }
            Overlay::ContextMenu {
                selected, items, ..
            } => {
                let len = items.len();
                if len > 0 {
                    *selected = (*selected as i32 + delta).rem_euclid(len as i32) as usize;
                }
            }
            Overlay::PathPicker(picker) => {
                let len = picker.entries.len();
                if len > 0 {
                    picker.selected =
                        (picker.selected as i32 + delta).clamp(0, len as i32 - 1) as usize;
                }
            }
            _ => {}
        }
    }

    fn overlay_activate(&mut self) -> Result<()> {
        match self.overlay.clone() {
            Overlay::Menu { menu, selected } => {
                self.overlay = Overlay::None;
                if let Some(action) = menu_actions(menu)
                    .get(selected)
                    .map(|(_, action)| action.clone())
                {
                    self.try_handle(action)?;
                }
            }
            Overlay::ContextMenu {
                items, selected, ..
            } => {
                self.overlay = Overlay::None;
                if let Some((_, action)) = items.get(selected) {
                    self.try_handle(action.clone())?;
                }
            }
            Overlay::PathPicker(_) => self.picker_open_selected()?,
            Overlay::Rename { playlist_id, text } => {
                self.try_handle(Action::RenamePlaylist(playlist_id, text))?
            }
            Overlay::Connect { .. } => self.try_handle(Action::SubmitConnect)?,
            Overlay::Pair { .. } => self.try_handle(Action::SubmitPair)?,
            _ => self.overlay = Overlay::None,
        }
        Ok(())
    }

    fn picker_open_selected(&mut self) -> Result<()> {
        let Some((entry, mode)) = (match &self.overlay {
            Overlay::PathPicker(picker) => picker
                .entries
                .get(picker.selected)
                .cloned()
                .map(|entry| (entry, picker.mode)),
            _ => None,
        }) else {
            return Ok(());
        };
        if entry.is_dir {
            if let Overlay::PathPicker(picker) = &mut self.overlay {
                picker.directory = entry.path;
                picker.selected = 0;
                picker.refresh();
            }
        } else if mode == PickerMode::Files {
            self.add_paths(vec![entry.path])?;
        }
        Ok(())
    }

    fn picker_choose_current(&mut self) -> Result<()> {
        if let Overlay::PathPicker(picker) = &self.overlay
            && picker.mode == PickerMode::Folder
        {
            let path = picker.directory.clone();
            self.add_paths(vec![path])?;
        }
        Ok(())
    }

    fn text_input(&mut self, character: char) {
        if character.is_control() {
            return;
        }
        match &mut self.overlay {
            Overlay::Rename { text, .. }
            | Overlay::Connect { text, .. }
            | Overlay::Pair { text, .. } => text.push(character),
            _ => {}
        }
        if self.soulseek_captures_text() {
            match self.soulseek_ui.phase {
                SoulseekPhase::Username => self.soulseek_ui.username.push(character),
                SoulseekPhase::Password => self.soulseek_ui.password.push(character),
                SoulseekPhase::Ready => self.soulseek_ui.query.push(character),
            }
        } else if self.focus == Focus::AlbumFilter {
            self.album_filter.push(character);
            self.clamp_album_selection();
        } else if self.focus == Focus::PlaylistFilter {
            self.playlist_filter.push(character);
            self.clamp_playlist_selection();
        }
    }

    fn text_backspace(&mut self) {
        match &mut self.overlay {
            Overlay::Rename { text, .. }
            | Overlay::Connect { text, .. }
            | Overlay::Pair { text } => {
                text.pop();
            }
            _ => {}
        }
        if self.soulseek_captures_text() {
            match self.soulseek_ui.phase {
                SoulseekPhase::Username => {
                    self.soulseek_ui.username.pop();
                }
                SoulseekPhase::Password => {
                    self.soulseek_ui.password.pop();
                }
                SoulseekPhase::Ready => {
                    self.soulseek_ui.query.pop();
                }
            }
        } else if self.focus == Focus::AlbumFilter {
            self.album_filter.pop();
            self.clamp_album_selection();
        } else if self.focus == Focus::PlaylistFilter {
            self.playlist_filter.pop();
            self.clamp_playlist_selection();
        }
    }

    pub fn captures_text(&self) -> bool {
        self.soulseek_captures_text()
            || (matches!(self.overlay, Overlay::None)
                && matches!(self.focus, Focus::AlbumFilter | Focus::PlaylistFilter))
    }

    pub fn soulseek_captures_text(&self) -> bool {
        self.soulseek_open
            && matches!(self.overlay, Overlay::None)
            && (self.focus == Focus::SoulseekQuery
                || matches!(
                    self.soulseek_ui.phase,
                    SoulseekPhase::Username | SoulseekPhase::Password
                ))
    }

    pub fn soulseek_filters_available(&self) -> bool {
        self.soulseek_open && self.soulseek_ui.phase == SoulseekPhase::Ready
    }

    fn soulseek_refresh_filter_status(&mut self) {
        if self.soulseek_ui.hits.is_empty() {
            return;
        }
        self.soulseek_ui.status = self.soulseek_ui.results_status();
        self.status = self.soulseek_ui.status.clone();
    }

    pub fn focus_ring(&self) -> Vec<Focus> {
        let mut items = Vec::new();
        if !self.album_filter.is_empty() || self.focus == Focus::AlbumFilter {
            items.push(Focus::AlbumFilter);
        }
        items.push(Focus::AlbumList);
        items.push(Focus::PlaylistTabs);
        if self.soulseek_open {
            items.push(Focus::SoulseekQuery);
            if self.soulseek_filters_available() {
                items.push(Focus::SoulseekFilter);
            }
            items.push(Focus::Playlist);
        } else if self.queue_open {
            items.push(Focus::Queue);
        } else if self.settings_open {
            items.push(Focus::Settings);
        } else {
            if !self.playlist_filter.is_empty() || self.focus == Focus::PlaylistFilter {
                items.push(Focus::PlaylistFilter);
            }
            items.push(Focus::Playlist);
        }
        items.push(Focus::Toolbar);
        items
    }

    pub fn extra_tab_count(&self) -> usize {
        3
    }

    pub fn selected_tab(&self) -> usize {
        if self.settings_open {
            self.playlists.len() + 2
        } else if self.soulseek_open {
            self.playlists.len() + 1
        } else if self.queue_open {
            self.playlists.len()
        } else {
            self.active_playlist
        }
    }

    pub fn tab_count(&self) -> usize {
        self.playlists.len() + self.extra_tab_count()
    }

    fn close_special_tabs(&mut self) {
        self.soulseek_open = false;
        self.queue_open = false;
        self.settings_open = false;
    }

    fn open_queue_tab(&mut self) {
        self.close_special_tabs();
        self.queue_open = true;
        self.focus = Focus::Queue;
        self.queue_selection = self.queue_selection.min(self.queue.len().saturating_sub(1));
    }

    fn open_settings_tab(&mut self) {
        self.close_special_tabs();
        self.settings_open = true;
        self.focus = Focus::Settings;
        if !crate::settings::ROWS
            .get(self.settings_selected)
            .is_some_and(|row| row.is_item())
        {
            self.settings_selected = crate::settings::first_item();
        }
    }

    fn begin_filter(&mut self) {
        if self.soulseek_open
            || self.settings_open
            || matches!(self.overlay, Overlay::Help | Overlay::Menu { .. })
        {
            return;
        }
        if self.focus == Focus::AlbumList || self.focus == Focus::AlbumFilter {
            self.focus = Focus::AlbumFilter;
        } else if !self.queue_open {
            self.focus = Focus::PlaylistFilter;
        }
    }

    fn clear_filter(&mut self) {
        match self.focus {
            Focus::AlbumFilter | Focus::AlbumList => {
                self.album_filter.clear();
                self.focus = Focus::AlbumList;
                self.clamp_album_selection();
            }
            Focus::PlaylistFilter | Focus::Playlist => {
                self.playlist_filter.clear();
                self.focus = Focus::Playlist;
                self.clamp_playlist_selection();
            }
            _ => {}
        }
    }

    fn track_matches(&self, track: &Track, needle: &str) -> bool {
        text_matches(
            needle,
            &[
                &track.title,
                &track.artist,
                &track.album,
                &track.path.to_string_lossy(),
            ],
        )
    }

    pub fn visible_album_entries(&self) -> Vec<AlbumEntry> {
        let needle = self.album_filter.trim();
        if needle.is_empty() {
            return self.album_entries();
        }
        let needle = needle.to_lowercase();
        let filtered = self
            .tracks
            .values()
            .filter(|track| self.track_matches(track, &needle));
        let has_remote = self.tracks.values().any(|track| track.origin.is_remote());
        if !has_remote {
            return group_tracks(filtered, 0);
        }
        let mut local = Vec::new();
        let mut remotes: BTreeMap<String, Vec<&Track>> = BTreeMap::new();
        for track in filtered {
            match &track.origin {
                TrackOrigin::Local => local.push(track),
                TrackOrigin::Remote { server_name, .. } => {
                    remotes.entry(server_name.clone()).or_default().push(track);
                }
            }
        }
        let mut entries = Vec::new();
        if !local.is_empty() {
            entries.push(AlbumEntry {
                depth: 0,
                label: "This computer".into(),
                track_id: None,
                track_ids: local.iter().map(|track| track.id).collect(),
                unavailable: false,
            });
            entries.extend(group_tracks(local, 1));
        }
        for (server, tracks) in remotes {
            entries.push(AlbumEntry {
                depth: 0,
                label: server,
                track_id: None,
                track_ids: tracks.iter().map(|track| track.id).collect(),
                unavailable: self.remote.is_none(),
            });
            entries.extend(group_tracks(tracks, 1));
        }
        entries
    }

    pub fn visible_playlist_indices(&self) -> Vec<usize> {
        let needle = self.playlist_filter.trim();
        self.active_playlist()
            .items
            .iter()
            .enumerate()
            .filter(|(_, id)| {
                needle.is_empty()
                    || self
                        .tracks
                        .get(id)
                        .is_some_and(|track| self.track_matches(track, needle))
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn clamp_album_selection(&mut self) {
        let len = self.visible_album_entries().len();
        self.album_selection = if len == 0 {
            0
        } else {
            self.album_selection.min(len - 1)
        };
        self.album_marked
            .retain(|index| *index < len.max(1) && len > 0);
    }

    fn clamp_playlist_selection(&mut self) {
        let visible = self.visible_playlist_indices();
        if visible.is_empty() {
            self.playlist_selection = 0;
            return;
        }
        if !visible.contains(&self.playlist_selection) {
            self.playlist_selection = visible[0];
        }
    }

    fn selected_album_tracks(&self) -> Vec<TrackId> {
        let entries = self.visible_album_entries();
        let mut marks = self.album_marked.clone();
        if marks.is_empty() && !entries.is_empty() {
            marks.insert(self.album_selection);
        }
        let mut ids = Vec::new();
        let mut seen = BTreeSet::new();
        for index in marks {
            if let Some(entry) = entries.get(index) {
                for id in &entry.track_ids {
                    if seen.insert(*id) {
                        ids.push(*id);
                    }
                }
            }
        }
        ids
    }

    fn selected_playlist_tracks(&self) -> Vec<TrackId> {
        let mut ids: Vec<TrackId> = self
            .playlist_marked
            .iter()
            .copied()
            .filter(|id| self.active_playlist().items.contains(id))
            .collect();
        if ids.is_empty()
            && let Some(&id) = self.active_playlist().items.get(self.playlist_selection)
        {
            ids.push(id);
        }
        ids
    }

    fn toggle_mark(&mut self) {
        match self.focus {
            Focus::AlbumList => {
                if !self.album_marked.remove(&self.album_selection) {
                    self.album_marked.insert(self.album_selection);
                }
            }
            Focus::Playlist if !self.soulseek_open && !self.queue_open => {
                if let Some(&id) = self.active_playlist().items.get(self.playlist_selection)
                    && !self.playlist_marked.remove(&id)
                {
                    self.playlist_marked.insert(id);
                }
            }
            _ => {}
        }
    }

    fn extend_selection(&mut self, delta: i32) {
        if self.focus == Focus::AlbumList {
            let len = self.visible_album_entries().len();
            if len == 0 {
                return;
            }
            self.album_selection =
                (self.album_selection as i32 + delta).clamp(0, len as i32 - 1) as usize;
            self.album_marked = range_set(self.album_anchor, self.album_selection);
            return;
        }
        if self.focus == Focus::Playlist && !self.soulseek_open && !self.queue_open {
            let visible = self.visible_playlist_indices();
            if visible.is_empty() {
                return;
            }
            let current = visible
                .iter()
                .position(|&index| index == self.playlist_selection)
                .unwrap_or(0);
            let next = (current as i32 + delta).clamp(0, visible.len() as i32 - 1) as usize;
            self.playlist_selection = visible[next];
            let anchor = visible
                .iter()
                .position(|&index| index == self.playlist_anchor)
                .unwrap_or(next);
            self.playlist_marked = visible[anchor.min(next)..=anchor.max(next)]
                .iter()
                .filter_map(|&index| self.active_playlist().items.get(index).copied())
                .collect();
        }
    }

    fn move_playlist_items(&mut self, delta: i32) -> Result<()> {
        if self.queue_open {
            if self.queue.len() < 2 {
                return Ok(());
            }
            let from = self.queue_selection;
            let to = (from as i32 + delta).clamp(0, self.queue.len() as i32 - 1) as usize;
            if from != to {
                self.queue.swap(from, to);
                self.queue_selection = to;
                self.restage_successor();
            }
            return Ok(());
        }
        let marked = self.playlist_marked.clone();
        let playlist = &mut self.playlists[self.active_playlist];
        if playlist.items.len() < 2 {
            return Ok(());
        }
        let mut indices: Vec<usize> = playlist
            .items
            .iter()
            .enumerate()
            .filter(|(_, id)| marked.contains(id))
            .map(|(index, _)| index)
            .collect();
        if indices.is_empty() {
            indices.push(self.playlist_selection);
        }
        indices.sort_unstable();
        if delta < 0 {
            if indices.first().copied() == Some(0) {
                return Ok(());
            }
            for index in indices {
                playlist.items.swap(index, index - 1);
            }
            self.playlist_selection = self.playlist_selection.saturating_sub(1);
        } else {
            if indices.last().copied() == Some(playlist.items.len() - 1) {
                return Ok(());
            }
            for index in indices.into_iter().rev() {
                playlist.items.swap(index, index + 1);
            }
            self.playlist_selection = (self.playlist_selection + 1).min(playlist.items.len() - 1);
        }
        self.store.save_playlist_items(playlist)?;
        self.rebuild_shuffle();
        self.restage_successor();
        Ok(())
    }

    fn sort_playlist(&mut self, column: PlaylistColumn) -> Result<()> {
        let ascending = match self.playlist_sort {
            Some((current, asc)) if current == column => !asc,
            _ => true,
        };
        self.playlist_sort = Some((column, ascending));
        let tracks = &self.tracks;
        self.playlists[self.active_playlist]
            .items
            .sort_by(|left, right| {
                let cmp = column_cmp(tracks.get(left), tracks.get(right), column);
                if ascending { cmp } else { cmp.reverse() }
            });
        let playlist = &self.playlists[self.active_playlist];
        self.store.save_playlist_items(playlist)?;
        self.rebuild_shuffle();
        self.restage_successor();
        self.status = format!(
            "Sorted by {} ({})",
            column.label(),
            if ascending { "ascending" } else { "descending" }
        );
        Ok(())
    }

    fn queue_selection(&mut self) {
        let ids = if self.focus == Focus::AlbumList || self.focus == Focus::AlbumFilter {
            self.selected_album_tracks()
        } else {
            self.selected_playlist_tracks()
        };
        if ids.is_empty() {
            return;
        }
        self.queue.extend(ids.iter().copied());
        self.restage_successor();
        self.status = format!(
            "Queued {} track{}",
            ids.len(),
            if ids.len() == 1 { "" } else { "s" }
        );
    }

    fn remove_from_queue(&mut self) {
        if self.queue.is_empty() {
            return;
        }
        self.queue
            .remove(self.queue_selection.min(self.queue.len() - 1));
        self.queue_selection = self.queue_selection.min(self.queue.len().saturating_sub(1));
        self.restage_successor();
    }

    fn add_selection_to_playlist(&mut self, index: usize) -> Result<()> {
        if index >= self.playlists.len() {
            return Ok(());
        }
        let ids = if self.focus == Focus::AlbumList || self.focus == Focus::AlbumFilter {
            self.selected_album_tracks()
        } else {
            self.selected_playlist_tracks()
        };
        let added = self.add_tracks_to(index, ids)?;
        self.status = format!(
            "Added {added} track{} to {}",
            if added == 1 { "" } else { "s" },
            self.playlists[index].name
        );
        Ok(())
    }

    fn play_track_id(&mut self, track_id: TrackId) -> Result<()> {
        let track = self
            .tracks
            .get(&track_id)
            .context("track metadata is missing")?
            .clone();
        if track.unavailable {
            return Err(anyhow!("file is unavailable: {}", track.path.display()));
        }
        if let Some(error) = &track.scan_error {
            return Err(anyhow!("{}: {error}", track.path.display()));
        }
        let source = match &track.origin {
            TrackOrigin::Local => MediaSource::LocalFile(track.path.clone()),
            TrackOrigin::Remote {
                fingerprint,
                remote_id,
                ..
            } => {
                let cached = cache_path(&self.cache_dir(), fingerprint, remote_id);
                if cache_is_complete(&cached, track.file_size) {
                    MediaSource::LocalFile(cached)
                } else if let Some(remote) = &self.remote {
                    let etag = format!("{}-{}", track.modified_ns, track.file_size);
                    remote.fetch(remote_id.clone(), track.file_size, etag);
                    self.audio
                        .set_pending(track.id, track.duration, PlaybackState::Buffering);
                    self.staged_playback = None;
                    self.audio_started_generation = None;
                    self.pending_track = Some(track_id);
                    self.status = format!("Buffering {} — {}", track.artist, track.title);
                    return Ok(());
                } else {
                    return Err(anyhow!(
                        "not connected to {}",
                        self.remote_name.as_deref().unwrap_or("the server")
                    ));
                }
            }
        };
        self.audio
            .set_pending(track.id, track.duration, PlaybackState::Loading);
        let generation = self.next_playback_generation();
        let gain = self.gain_for_track(&track);
        self.audio.load_and_play(StagedTrack {
            source,
            track_id: track.id,
            duration: track.duration,
            gain,
            generation,
        })?;
        self.audio_started_generation = None;
        self.pending_track = None;
        self.apply_replay_gain();
        self.status = format!("Playing: {} — {}", track.artist, track.title);
        Ok(())
    }

    fn show_properties(&mut self) {
        let ids = if self.focus == Focus::AlbumList || self.focus == Focus::AlbumFilter {
            self.selected_album_tracks()
        } else if self.queue_open {
            self.queue
                .get(self.queue_selection)
                .copied()
                .into_iter()
                .collect()
        } else {
            self.selected_playlist_tracks()
        };
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

    fn open_containing_folder(&mut self) {
        let ids = if self.focus == Focus::AlbumList || self.focus == Focus::AlbumFilter {
            self.selected_album_tracks()
        } else if self.queue_open {
            self.queue
                .get(self.queue_selection)
                .copied()
                .into_iter()
                .collect()
        } else {
            self.selected_playlist_tracks()
        };
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

    fn open_list_context(&mut self, row: Option<usize>, x: Option<u16>, y: Option<u16>) {
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
                self.album_selection = if len == 0 { 0 } else { index.min(len - 1) };
            }
            self.focus = Focus::AlbumList;
        } else if let Some(index) = row {
            let visible = self.visible_playlist_indices();
            if let Some(&item) = visible.get(index) {
                self.playlist_selection = item;
            }
            self.focus = Focus::Playlist;
        }
        let items = self.list_context_items();
        if items.is_empty() {
            return;
        }
        self.overlay = Overlay::ContextMenu {
            selected: 0,
            items,
            at: x.zip(y),
        };
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

    fn rebuild_album_groups(&mut self, playlist_index: usize) {
        let Some(playlist) = self.playlists.get(playlist_index) else {
            self.album_groups.clear();
            return;
        };
        let mut order = Vec::new();
        let mut groups: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
        for (index, id) in playlist.items.iter().enumerate() {
            let Some(track) = self.tracks.get(id) else {
                continue;
            };
            let key = (track.artist.to_lowercase(), track.album.to_lowercase());
            if !groups.contains_key(&key) {
                order.push(key.clone());
            }
            groups.entry(key).or_default().push(index);
        }
        let mut state = playlist.id as u64 ^ (order.len() as u64).wrapping_mul(0x9E37_79B9);
        for index in (1..order.len()).rev() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            order.swap(index, state as usize % (index + 1));
        }
        self.album_groups = order
            .into_iter()
            .filter_map(|key| groups.remove(&key))
            .collect();
    }

    fn album_shuffle_candidates(&mut self, playlist_index: usize, current: usize) -> Vec<usize> {
        if self.album_groups.is_empty() {
            self.rebuild_album_groups(playlist_index);
        }
        let Some(group_index) = self
            .album_groups
            .iter()
            .position(|group| group.contains(&current))
        else {
            return ((current + 1)..self.playlists[playlist_index].items.len()).collect();
        };
        let group = &self.album_groups[group_index];
        if let Some(pos) = group.iter().position(|index| *index == current)
            && pos + 1 < group.len()
        {
            return group[pos + 1..].to_vec();
        }
        (1..=self.album_groups.len())
            .map(|step| {
                let next = (group_index + step) % self.album_groups.len();
                self.album_groups[next][0]
            })
            .collect()
    }

    fn next_playback_generation(&mut self) -> u64 {
        self.playback_generation = self.playback_generation.wrapping_add(1).max(1);
        self.playback_generation
    }

    fn gain_for_track(&self, track: &Track) -> f32 {
        track.replay_gain.apply(
            self.replay_gain_mode,
            self.replay_gain_preamp,
            self.replay_gain_prevent_clip,
        )
    }

    fn staging_source(&self, track: &Track) -> Option<MediaSource> {
        if track.unavailable || track.scan_error.is_some() {
            return None;
        }
        match &track.origin {
            TrackOrigin::Local => Some(MediaSource::LocalFile(track.path.clone())),
            TrackOrigin::Remote {
                fingerprint,
                remote_id,
                ..
            } => {
                let cached = cache_path(&self.cache_dir(), fingerprint, remote_id);
                cache_is_complete(&cached, track.file_size)
                    .then_some(MediaSource::LocalFile(cached))
            }
        }
    }

    fn successor_candidates(&mut self) -> Vec<PlaybackCandidate> {
        if self.stop_after_current {
            return Vec::new();
        }
        if !self.queue.is_empty() {
            return self
                .queue
                .iter()
                .copied()
                .enumerate()
                .map(|(queue_index, track_id)| PlaybackCandidate {
                    track_id,
                    playlist: None,
                    queue_index: Some(queue_index),
                })
                .collect();
        }
        let Some((playlist_id, index)) = self.playing else {
            return Vec::new();
        };
        let Some(playlist_index) = self
            .playlists
            .iter()
            .position(|playlist| playlist.id == playlist_id)
        else {
            return Vec::new();
        };
        let len = self.playlists[playlist_index].items.len();
        if len == 0 {
            return Vec::new();
        }
        let indices: Vec<usize> = match self.playback_order {
            PlaybackOrder::RepeatTrack => vec![index],
            PlaybackOrder::Shuffle => {
                if self.shuffle.len() != len || self.shuffle_playlist != Some(playlist_id) {
                    self.rebuild_shuffle_for(playlist_index);
                }
                (1..=len)
                    .map(|step| self.shuffle[(self.shuffle_cursor + step) % len])
                    .collect()
            }
            PlaybackOrder::ShuffleAlbums => self.album_shuffle_candidates(playlist_index, index),
            PlaybackOrder::RepeatPlaylist => (1..=len).map(|step| (index + step) % len).collect(),
            PlaybackOrder::Default => ((index + 1)..len).collect(),
        };
        indices
            .into_iter()
            .filter_map(|next| {
                self.playlists[playlist_index]
                    .items
                    .get(next)
                    .copied()
                    .map(|track_id| PlaybackCandidate {
                        track_id,
                        playlist: Some((playlist_id, next)),
                        queue_index: None,
                    })
            })
            .collect()
    }

    fn stage_successor(&mut self) -> Result<()> {
        let snapshot = self.audio.snapshot();
        if snapshot.track_id.is_none() || self.audio_started_generation != Some(snapshot.generation)
        {
            return Ok(());
        }
        self.audio.clear_staged();
        self.staged_playback = None;
        for candidate in self.successor_candidates() {
            let track_id = candidate.track_id;
            let Some(track) = self.tracks.get(&track_id).cloned() else {
                continue;
            };
            let Some(source) = self.staging_source(&track) else {
                continue;
            };
            let generation = self.next_playback_generation();
            self.audio.stage_next(StagedTrack {
                source,
                track_id,
                duration: track.duration,
                gain: self.gain_for_track(&track),
                generation,
            })?;
            self.staged_playback = Some(StagedPlayback {
                track_id,
                generation,
                playlist: candidate.playlist,
                queue_index: candidate.queue_index,
            });
            break;
        }
        Ok(())
    }

    fn restage_successor(&mut self) {
        if let Err(error) = self.stage_successor() {
            self.status = format!("Could not stage next track: {error:#}");
        }
    }

    fn drain_audio_events(&mut self) {
        let events = self.audio.drain_events();
        let mut stage_after_transition = false;
        let mut finished = Vec::new();
        for event in events {
            match event {
                AudioEvent::TrackStarted {
                    track_id,
                    generation,
                } => {
                    self.audio_started_generation = Some(generation);
                    if let Some(staged) = self.staged_playback.take_if(|staged| {
                        staged.track_id == track_id && staged.generation == generation
                    }) {
                        if let Some(queue_index) = staged.queue_index
                            && self.queue.get(queue_index) == Some(&track_id)
                        {
                            self.queue.drain(..=queue_index);
                            self.queue_selection =
                                self.queue_selection.min(self.queue.len().saturating_sub(1));
                        }
                        if let Some((playlist_id, index)) = staged.playlist {
                            self.playing = Some((playlist_id, index));
                            if self.playback_order == PlaybackOrder::Shuffle {
                                self.shuffle_cursor = self
                                    .shuffle
                                    .iter()
                                    .position(|candidate| *candidate == index)
                                    .unwrap_or(self.shuffle_cursor);
                            }
                            if self.cursor_follows_playback
                                && self.active_playlist().id == playlist_id
                                && !self.queue_open
                                && !self.soulseek_open
                                && !self.settings_open
                            {
                                self.playlist_selection = index;
                            }
                        }
                        if let Some(track) = self.tracks.get(&track_id) {
                            self.status = format!("Playing: {} — {}", track.artist, track.title);
                        }
                    }
                    stage_after_transition = true;
                }
                AudioEvent::TrackFinished { generation, .. } => finished.push(generation),
                AudioEvent::DeviceLost(error) => {
                    self.audio_error = Some(error.clone());
                    self.status = format!("Audio device lost: {error}");
                }
            }
        }
        if stage_after_transition {
            self.restage_successor();
        }
        if finished.contains(&self.audio.snapshot().generation)
            && let Err(error) = self.advance(true)
        {
            self.status = format!("Playback error: {error:#}");
        }
    }

    fn retry_audio(&mut self) -> Result<()> {
        if self.no_audio {
            self.status = "Audio is disabled by --no-audio".into();
            return Ok(());
        }
        self.switch_output_device()?;
        self.status = "Audio output initialized".into();
        Ok(())
    }

    fn switch_output_device(&mut self) -> Result<()> {
        let volume = self.audio.snapshot().volume;
        let snapshot = self.audio.snapshot();
        let current = snapshot
            .track_id
            .and_then(|track_id| self.tracks.get(&track_id).cloned());
        let mut replacement =
            create_engine(false, volume, self.preferred_output_device.as_deref())?;
        if let Some(track) = current
            && let Some(source) = self.staging_source(&track)
        {
            let generation = self.next_playback_generation();
            replacement.load_and_play(StagedTrack {
                source,
                track_id: track.id,
                duration: track.duration,
                gain: self.gain_for_track(&track),
                generation,
            })?;
            replacement.seek(snapshot.position)?;
            if snapshot.state == PlaybackState::Paused {
                replacement.pause();
            }
        }
        self.audio = replacement;
        self.audio_started_generation = None;
        self.staged_playback = None;
        self.audio_error = None;
        Ok(())
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

    fn adjust_setting(&mut self, delta: i32) -> Result<()> {
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
                let next = (current as i32 + delta.signum()).rem_euclid(count as i32) as usize;
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
                    for _ in 0..crate::model::ReplayGainMode::ALL.len() - 1 {
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

    fn apply_replay_gain(&mut self) {
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
        let (gain, _) = match self.replay_gain_mode {
            ReplayGainMode::Track => (
                track
                    .replay_gain
                    .track_gain
                    .or(track.replay_gain.album_gain),
                track.replay_gain.track_peak,
            ),
            ReplayGainMode::Album => (
                track
                    .replay_gain
                    .album_gain
                    .or(track.replay_gain.track_gain),
                track.replay_gain.album_peak,
            ),
            ReplayGainMode::None => return None,
        };
        Some(match gain {
            Some(db) => format!("RG {:+.1} dB", db + self.replay_gain_preamp),
            None => format!("RG {}", self.replay_gain_mode.label()),
        })
    }

    fn begin_replaygain_scan(&mut self) {
        let mut jobs = Vec::new();
        let ids = if self.focus == Focus::AlbumList || self.focus == Focus::AlbumFilter {
            self.selected_album_tracks()
        } else {
            self.selected_playlist_tracks()
        };
        let ids = if ids.is_empty() {
            self.active_playlist().items.clone()
        } else {
            ids
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
            "Scanning ReplayGain for {} file{}…",
            jobs.len(),
            if jobs.len() == 1 { "" } else { "s" }
        );
        self.replaygain = Some(replaygain::start(jobs));
    }

    fn drain_replaygain_events(&mut self) {
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

    fn save_state(&mut self) -> Result<()> {
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

    fn rebuild_shuffle(&mut self) {
        self.rebuild_shuffle_for(self.active_playlist);
        self.rebuild_album_groups(self.active_playlist);
    }

    fn rebuild_shuffle_for(&mut self, playlist_index: usize) {
        let len = self
            .playlists
            .get(playlist_index)
            .map_or(0, |p| p.items.len());
        self.shuffle = (0..len).collect();
        let mut state = self
            .playlists
            .get(playlist_index)
            .map_or(1, |p| p.id as u64)
            ^ (len as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        for index in (1..len).rev() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            self.shuffle.swap(index, state as usize % (index + 1));
        }
        self.shuffle_cursor = self
            .playing
            .and_then(|(_, current)| self.shuffle.iter().position(|item| *item == current))
            .unwrap_or(0);
        self.shuffle_playlist = self
            .playlists
            .get(playlist_index)
            .map(|playlist| playlist.id);
    }

    pub fn album_entries(&self) -> Vec<AlbumEntry> {
        let has_remote = self.tracks.values().any(|track| track.origin.is_remote());
        if !has_remote {
            return group_tracks(self.tracks.values(), 0);
        }

        let mut local = Vec::new();
        let mut remotes: BTreeMap<String, Vec<&Track>> = BTreeMap::new();
        for track in self.tracks.values() {
            match &track.origin {
                TrackOrigin::Local => local.push(track),
                TrackOrigin::Remote { server_name, .. } => {
                    remotes.entry(server_name.clone()).or_default().push(track);
                }
            }
        }
        let mut entries = Vec::new();
        entries.push(AlbumEntry {
            depth: 0,
            label: "This computer".into(),
            track_id: None,
            track_ids: local.iter().map(|track| track.id).collect(),
            unavailable: false,
        });
        entries.extend(group_tracks(local, 1));
        for (server, tracks) in remotes {
            entries.push(AlbumEntry {
                depth: 0,
                label: server,
                track_id: None,
                track_ids: tracks.iter().map(|track| track.id).collect(),
                unavailable: self.remote.is_none(),
            });
            entries.extend(group_tracks(tracks, 1));
        }
        entries
    }

    pub fn active_playlist(&self) -> &Playlist {
        &self.playlists[self.active_playlist]
    }

    fn begin_scan(&self, paths: Vec<PathBuf>, add_to_playlist: bool) {
        let known = self
            .tracks
            .values()
            .map(|track| (track.path.clone(), track.to_scanned()))
            .collect();
        self.scanner.scan(paths, add_to_playlist, known);
    }
}

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
            ("Rescan library", Action::RescanLibrary),
            ("Connect to server…", Action::BeginConnect),
            ("Disconnect server", Action::DisconnectServer),
            ("Soulseek…", Action::BeginSoulseek),
        ],
        _ => &[("About Staccato", Action::ToggleHelp)],
    }
}

impl App {
    fn open_soulseek_context(&mut self, row: Option<usize>, x: Option<u16>, y: Option<u16>) {
        if !self.soulseek_open {
            return;
        }
        self.focus = Focus::Playlist;
        if let Some(index) = row {
            let len = self.soulseek_ui.visible_rows().len();
            self.soulseek_ui.selected = if len == 0 { 0 } else { index.min(len - 1) };
        }
        let items = self.soulseek_context_items();
        if items.is_empty() {
            return;
        }
        self.overlay = Overlay::ContextMenu {
            selected: 0,
            items,
            at: x.zip(y),
        };
    }

    fn soulseek_context_items(&self) -> Vec<(String, Action)> {
        let rows = self.soulseek_ui.visible_rows();
        let Some(row) = rows.get(self.soulseek_ui.selected) else {
            return Vec::new();
        };
        let expanded = !self.soulseek_ui.collapsed.contains(&row.key);
        let fold = if expanded { "Collapse" } else { "Expand" };
        let user = format!("Hide {}", row.username);
        let from_user = format!("Download all from {}", row.username);
        match row.kind {
            SoulseekRowKind::File => vec![
                (
                    "Download file".into(),
                    Action::SoulseekDownload(SoulseekScope::File),
                ),
                (
                    "Download folder".into(),
                    Action::SoulseekDownload(SoulseekScope::Folder),
                ),
                (from_user, Action::SoulseekDownload(SoulseekScope::User)),
                (
                    "Hide file".into(),
                    Action::SoulseekHide(SoulseekScope::File),
                ),
                (
                    "Hide folder".into(),
                    Action::SoulseekHide(SoulseekScope::Folder),
                ),
                (user, Action::SoulseekHide(SoulseekScope::User)),
            ],
            SoulseekRowKind::Folder => vec![
                (
                    "Download folder".into(),
                    Action::SoulseekDownload(SoulseekScope::Folder),
                ),
                (fold.into(), Action::SoulseekFold(!expanded)),
                (from_user, Action::SoulseekDownload(SoulseekScope::User)),
                (
                    "Hide folder".into(),
                    Action::SoulseekHide(SoulseekScope::Folder),
                ),
                (user, Action::SoulseekHide(SoulseekScope::User)),
            ],
            SoulseekRowKind::User => vec![
                (from_user, Action::SoulseekDownload(SoulseekScope::User)),
                (fold.into(), Action::SoulseekFold(!expanded)),
                (user, Action::SoulseekHide(SoulseekScope::User)),
            ],
        }
    }

    fn soulseek_download(&mut self, scope: SoulseekScope) {
        let hits = self.soulseek_ui.hits_in_scope(scope);
        if hits.is_empty() {
            return;
        }
        let Some(handle) = &self.soulseek else {
            self.status = "Soulseek is not connected".into();
            self.soulseek_ui.status = self.status.clone();
            return;
        };
        let label = if hits.len() == 1 {
            hits[0].file_name().to_owned()
        } else {
            format!("{} files", hits.len())
        };
        self.status = format!("Downloading {label}…");
        self.soulseek_ui.status = self.status.clone();
        self.soulseek_downloads_active = self.soulseek_downloads_active.saturating_add(hits.len());
        handle.download_all(&hits);
    }

    fn soulseek_hide(&mut self, scope: SoulseekScope) {
        let rows = self.soulseek_ui.visible_rows();
        let label = rows
            .get(self.soulseek_ui.selected)
            .map(|row| match scope {
                SoulseekScope::File => row
                    .hit
                    .as_ref()
                    .map(|hit| hit.file_name().to_owned())
                    .unwrap_or_else(|| "file".into()),
                SoulseekScope::Folder => row.folder.clone().unwrap_or_else(|| "folder".into()),
                SoulseekScope::User => row.username.clone(),
            })
            .unwrap_or_default();
        self.soulseek_ui.hide(scope);
        self.status = format!("Hidden {label}");
        self.soulseek_ui.status = self.status.clone();
    }

    fn open_soulseek_tab(&mut self) -> Result<()> {
        self.close_special_tabs();
        self.soulseek_open = true;
        self.focus = Focus::SoulseekQuery;
        if self.soulseek.is_some() {
            if self.soulseek_ui.phase != SoulseekPhase::Ready {
                self.soulseek_ui.phase = SoulseekPhase::Ready;
            }
            return Ok(());
        }
        if let Some(credentials) = soulseek::load_credentials(&self.data_dir) {
            self.start_soulseek(credentials)?;
            self.soulseek_ui = SoulseekUi::ready();
        }
        Ok(())
    }

    fn start_soulseek(&mut self, credentials: soulseek::SoulseekCredentials) -> Result<()> {
        soulseek::init_logging(&self.data_dir);
        let roots = self.store.load_roots()?;
        let download = soulseek::download_dir(&self.data_dir, &roots);
        if roots.is_empty() {
            self.store.add_root(&download)?;
        }
        self.soulseek = Some(soulseek::start(credentials, download));
        self.status = "Connecting to Soulseek…".into();
        self.soulseek_ui.status = self.status.clone();
        Ok(())
    }

    fn soulseek_activate(&mut self) -> Result<()> {
        match self.soulseek_ui.phase {
            SoulseekPhase::Username => {
                if self.soulseek_ui.username.trim().is_empty() {
                    self.soulseek_ui.status = "Username required".into();
                } else {
                    self.soulseek_ui.phase = SoulseekPhase::Password;
                    self.soulseek_ui.status = "Password".into();
                    self.focus = Focus::SoulseekQuery;
                }
            }
            SoulseekPhase::Password => {
                let credentials = soulseek::SoulseekCredentials {
                    username: self.soulseek_ui.username.trim().to_owned(),
                    password: self.soulseek_ui.password.clone(),
                };
                if credentials.username.is_empty() || credentials.password.is_empty() {
                    self.soulseek_ui.status = "Username and password required".into();
                    return Ok(());
                }
                soulseek::save_credentials(&self.data_dir, &credentials)?;
                self.start_soulseek(credentials)?;
                self.soulseek_ui = SoulseekUi::ready();
                self.focus = Focus::SoulseekQuery;
            }
            SoulseekPhase::Ready => {
                if self.focus == Focus::SoulseekQuery {
                    let query = self.soulseek_ui.query.trim().to_owned();
                    if query.is_empty() {
                        return Ok(());
                    }
                    self.soulseek_ui.hits.clear();
                    self.soulseek_ui.collapsed.clear();
                    self.soulseek_ui.selected = 0;
                    self.soulseek_ui.status = format!("Searching “{query}”…");
                    if let Some(handle) = &self.soulseek {
                        handle.search(query);
                    } else {
                        self.status = "Soulseek is not connected".into();
                        self.soulseek_ui.status = self.status.clone();
                    }
                } else {
                    match self.soulseek_ui.selected_kind() {
                        Some(SoulseekRowKind::File) => self.soulseek_download(SoulseekScope::File),
                        Some(SoulseekRowKind::Folder | SoulseekRowKind::User) => {
                            self.soulseek_ui.toggle(None);
                        }
                        None => {}
                    }
                }
            }
        }
        Ok(())
    }

    fn drain_soulseek_events(&mut self) {
        let Some(handle) = &self.soulseek else {
            return;
        };
        let events: Vec<_> = handle.events.try_iter().collect();
        for event in events {
            match event {
                SoulseekEvent::Ready => {
                    self.status = "Soulseek ready".into();
                    self.soulseek_ui.status = "Type a search and press Enter".into();
                    self.soulseek_ui.phase = SoulseekPhase::Ready;
                }
                SoulseekEvent::SearchResults(hits) => {
                    self.soulseek_ui.set_hits(hits);
                    self.soulseek_ui.status = if self.soulseek_ui.hits.is_empty() {
                        "No matching files".into()
                    } else {
                        self.soulseek_ui.results_status()
                    };
                    if self.soulseek_open {
                        self.focus = Focus::Playlist;
                    }
                }
                SoulseekEvent::Status(message) => {
                    self.status = message.clone();
                    self.soulseek_ui.status = message;
                }
                SoulseekEvent::Finished(path) => {
                    self.soulseek_downloads_active =
                        self.soulseek_downloads_active.saturating_sub(1);
                    self.status = format!("Downloaded {}", path.display());
                    self.soulseek_ui.status = self.status.clone();
                    let _ = self.add_paths(vec![path]);
                }
                SoulseekEvent::DownloadFailed(message) => {
                    self.soulseek_downloads_active =
                        self.soulseek_downloads_active.saturating_sub(1);
                    self.status = format!("Soulseek: {message}");
                    self.soulseek_ui.status = message;
                }
                SoulseekEvent::Error(message) => {
                    self.status = format!("Soulseek: {message}");
                    self.soulseek_ui.status = message;
                }
            }
        }
    }

    fn drain_remote_events(&mut self) {
        if let Some(discovery) = &self.discovery {
            let events: Vec<_> = discovery.events.try_iter().collect();
            for event in events {
                if let RemoteEvent::Discovered(servers) = event
                    && let Overlay::Connect { discovered, .. } = &mut self.overlay
                {
                    *discovered = servers;
                }
            }
        }
        let Some(remote) = &self.remote else {
            return;
        };
        let events: Vec<_> = remote.events.try_iter().collect();
        for event in events {
            if let Err(error) = self.handle_remote_event(event) {
                self.status = format!("Remote error: {error:#}");
            }
        }
    }

    fn handle_remote_event(&mut self, event: RemoteEvent) -> Result<()> {
        match event {
            RemoteEvent::Connected {
                server_name,
                fingerprint,
                pair_required,
                revision,
            } => {
                self.remote_name = Some(server_name.clone());
                self.remote_fingerprint = Some(fingerprint);
                self.remote_revision = Some(revision);
                if pair_required {
                    self.overlay = Overlay::Pair {
                        text: String::new(),
                    };
                    self.status = format!("{server_name} requires pairing");
                } else {
                    self.status = format!("Connected to {server_name}");
                    self.overlay = Overlay::None;
                }
            }
            RemoteEvent::Paired { token, fingerprint } => {
                self.remote_fingerprint = Some(fingerprint.clone());
                if let Some(address) = self.saved_remote_address() {
                    crate::net::credentials::save_credentials(
                        &self.data_dir,
                        &address,
                        &fingerprint,
                        &token,
                        self.remote_name.as_deref().unwrap_or("Staccato"),
                    )?;
                }
                self.overlay = Overlay::None;
                self.status = "Paired with server".into();
            }
            RemoteEvent::Catalog {
                tracks,
                removed,
                revision,
                replace,
            } => {
                self.remote_revision = Some(revision);
                let fingerprint = self.remote_fingerprint.clone().unwrap_or_default();
                let server_name = self.remote_name.clone().unwrap_or_else(|| "Server".into());
                let incoming: BTreeSet<&str> = tracks
                    .iter()
                    .map(|track| track.remote_id.as_str())
                    .collect();
                let mut gone = removed;
                if replace {
                    gone.extend(
                        self.tracks
                            .values()
                            .filter_map(|track| match &track.origin {
                                TrackOrigin::Remote {
                                    fingerprint: fp,
                                    remote_id,
                                    ..
                                } if fp == &fingerprint
                                    && !incoming.contains(remote_id.as_str()) =>
                                {
                                    Some(remote_id.clone())
                                }
                                _ => None,
                            }),
                    );
                }
                let removed_ids: Vec<TrackId> = self
                    .tracks
                    .iter()
                    .filter_map(|(id, track)| match &track.origin {
                        TrackOrigin::Remote {
                            fingerprint: fp,
                            remote_id,
                            ..
                        } if fp == &fingerprint && gone.contains(remote_id) => Some(*id),
                        _ => None,
                    })
                    .collect();
                self.store.remove_remote_tracks(&fingerprint, &gone)?;
                for i in 0..self.playlists.len() {
                    let before = self.playlists[i].items.len();
                    self.playlists[i]
                        .items
                        .retain(|id| !removed_ids.contains(id));
                    if self.playlists[i].items.len() != before {
                        let playlist = self.playlists[i].clone();
                        self.store.save_playlist_items(&playlist)?;
                    }
                }
                self.tracks.retain(|_, track| match &track.origin {
                    TrackOrigin::Remote {
                        fingerprint: fp,
                        remote_id,
                        ..
                    } => !(fp == &fingerprint && gone.contains(remote_id)),
                    TrackOrigin::Local => true,
                });
                for track in tracks {
                    let stored =
                        self.store
                            .upsert_remote_track(&fingerprint, &server_name, &track)?;
                    self.tracks.insert(stored.id, stored);
                }
                self.status = format!(
                    "Streaming from {server_name} — {} remote tracks (rev {})",
                    self.tracks
                        .values()
                        .filter(|track| track.origin.is_remote())
                        .count(),
                    self.remote_revision.unwrap_or(revision)
                );
                self.playlist_selection = self
                    .playlist_selection
                    .min(self.active_playlist().items.len().saturating_sub(1));
            }
            RemoteEvent::MediaReady { remote_id } => {
                if let Some(track_id) = self.pending_track {
                    let matches =
                        self.tracks
                            .get(&track_id)
                            .is_some_and(|track| match &track.origin {
                                TrackOrigin::Remote { remote_id: id, .. } => id == &remote_id,
                                TrackOrigin::Local => false,
                            });
                    if matches {
                        self.play_track_id(track_id)?;
                        return Ok(());
                    }
                }
                if let Some((playlist, item)) = self.pending_play {
                    let matches = self
                        .playlists
                        .get(playlist)
                        .and_then(|list| list.items.get(item).and_then(|id| self.tracks.get(id)));
                    if matches.is_some_and(|track| match &track.origin {
                        TrackOrigin::Remote { remote_id: id, .. } => id == &remote_id,
                        TrackOrigin::Local => false,
                    }) {
                        self.play_at(playlist, item)?;
                    }
                }
            }
            RemoteEvent::Error(message) => self.status = format!("Remote error: {message}"),
            RemoteEvent::Disconnected => {
                tracing::warn!("TUI received Disconnected event");
                self.remote = None;
                if !matches!(
                    self.status.as_str(),
                    s if s.starts_with("Disconnected")
                ) {
                    self.status = "Disconnected from server".into();
                }
            }
            RemoteEvent::Discovered(_) => {}
        }
        Ok(())
    }

    fn submit_connect(&mut self) -> Result<()> {
        let Overlay::Connect { text, discovered } = &self.overlay else {
            return Ok(());
        };
        let endpoint = text.trim().to_owned();
        let address = if endpoint.is_empty() {
            discovered
                .first()
                .map(|server| server.address)
                .ok_or_else(|| anyhow!("type host:port or wait for a LAN server"))?
        } else {
            parse_remote_endpoint(&endpoint)?
        };
        let saved = crate::net::credentials::load_for_address(&self.data_dir, &address.to_string());
        let token = saved.as_ref().map(|saved| saved.token.clone());
        let pin = saved
            .as_ref()
            .filter(|saved| !saved.fingerprint.is_empty())
            .map(|saved| saved.fingerprint.clone());
        tracing::info!(%address, has_token = token.is_some(), "TUI connecting");
        self.remote = Some(connect(address, self.cache_dir(), token, pin));
        self.status = format!("Connecting to {address}…");
        crate::net::credentials::save_address(&self.data_dir, &address.to_string())?;
        Ok(())
    }

    fn submit_pair(&mut self) {
        let Overlay::Pair { text } = &self.overlay else {
            return;
        };
        let code = text.trim().to_owned();
        if let Some(remote) = &self.remote {
            remote.pair(code);
            self.status = "Sending pairing code…".into();
        }
    }

    fn disconnect_remote(&mut self) {
        if let Some(remote) = &self.remote {
            remote.disconnect();
        }
        self.remote = None;
        self.pending_play = None;
        self.status = "Disconnected from server".into();
    }

    fn prefetch_neighbors(&self, playlist_index: usize, item_index: usize) {
        let Some(remote) = &self.remote else {
            return;
        };
        let Some(playlist) = self.playlists.get(playlist_index) else {
            return;
        };
        for index in [item_index.saturating_add(1)] {
            let Some(track_id) = playlist.items.get(index) else {
                continue;
            };
            let Some(track) = self.tracks.get(track_id) else {
                continue;
            };
            if let TrackOrigin::Remote {
                fingerprint,
                remote_id,
                ..
            } = &track.origin
            {
                let cached = cache_path(&self.cache_dir(), fingerprint, remote_id);
                if !cache_is_complete(&cached, track.file_size) {
                    remote.fetch(
                        remote_id.clone(),
                        track.file_size,
                        format!("{}-{}", track.modified_ns, track.file_size),
                    );
                }
            }
        }
    }

    fn saved_remote_address(&self) -> Option<String> {
        crate::net::credentials::load(&self.data_dir).map(|saved| saved.address)
    }
}

fn on_off(value: bool) -> String {
    if value { "On".into() } else { "Off".into() }
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

fn range_set(a: usize, b: usize) -> BTreeSet<usize> {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    (lo..=hi).collect()
}

fn column_cmp(
    left: Option<&Track>,
    right: Option<&Track>,
    column: PlaylistColumn,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(left), Some(right)) => match column {
            PlaylistColumn::Number => left
                .track_number
                .cmp(&right.track_number)
                .then(left.title.to_lowercase().cmp(&right.title.to_lowercase())),
            PlaylistColumn::Artist => left
                .artist
                .to_lowercase()
                .cmp(&right.artist.to_lowercase())
                .then(left.album.to_lowercase().cmp(&right.album.to_lowercase()))
                .then(left.track_number.cmp(&right.track_number)),
            PlaylistColumn::Title => left.title.to_lowercase().cmp(&right.title.to_lowercase()),
            PlaylistColumn::Album => left
                .album
                .to_lowercase()
                .cmp(&right.album.to_lowercase())
                .then(left.track_number.cmp(&right.track_number)),
            PlaylistColumn::Date => left.date.cmp(&right.date),
            PlaylistColumn::Time => left.duration.cmp(&right.duration),
        },
    }
}

fn group_tracks<'a>(tracks: impl IntoIterator<Item = &'a Track>, depth: u8) -> Vec<AlbumEntry> {
    let mut grouped: BTreeMap<String, BTreeMap<String, Vec<&Track>>> = BTreeMap::new();
    for track in tracks {
        grouped
            .entry(track.artist.clone())
            .or_default()
            .entry(track.album.clone())
            .or_default()
            .push(track);
    }
    let mut entries = Vec::new();
    for (artist, albums) in grouped {
        let artist_ids = albums
            .values()
            .flatten()
            .map(|track| track.id)
            .collect::<Vec<_>>();
        entries.push(AlbumEntry {
            depth,
            label: artist,
            track_id: None,
            track_ids: artist_ids,
            unavailable: false,
        });
        for (album, mut tracks) in albums {
            tracks.sort_by_key(|track| {
                (
                    track.track_number.unwrap_or(u32::MAX),
                    track.title.to_lowercase(),
                )
            });
            let album_ids = tracks.iter().map(|track| track.id).collect::<Vec<_>>();
            entries.push(AlbumEntry {
                depth: depth + 1,
                label: album,
                track_id: None,
                track_ids: album_ids,
                unavailable: false,
            });
            for track in tracks {
                entries.push(AlbumEntry {
                    depth: depth + 2,
                    label: track.title.clone(),
                    track_id: Some(track.id),
                    track_ids: vec![track.id],
                    unavailable: track.unavailable || track.scan_error.is_some(),
                });
            }
        }
    }
    entries
}

fn parse_remote_endpoint(endpoint: &str) -> Result<std::net::SocketAddr> {
    let address = if let Ok(address) = endpoint.parse() {
        address
    } else {
        let with_port = if endpoint.contains(':') {
            endpoint.to_owned()
        } else {
            format!("{}:{}", endpoint, net::DEFAULT_PORT)
        };
        with_port
            .to_socket_addrs()
            .with_context(|| format!("resolving {endpoint}"))?
            .next()
            .ok_or_else(|| anyhow!("could not resolve {endpoint}"))?
    };
    Ok(net::normalize_server_addr(address))
}

fn unique_playlist_name(playlists: &[Playlist]) -> String {
    let names: BTreeSet<&str> = playlists
        .iter()
        .map(|playlist| playlist.name.as_str())
        .collect();
    if !names.contains("New Playlist") {
        return "New Playlist".into();
    }
    (2..)
        .map(|number| format!("New Playlist ({number})"))
        .find(|name| !names.contains(name.as_str()))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Track, TrackId, TrackOrigin};
    use std::{path::PathBuf, time::Duration};

    #[test]
    fn shuffle_is_a_stable_permutation() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut app = App::open(&directory.path().join("shuffle.db"), true)?;
        app.playlists[0].items = vec![10, 20, 30, 40, 50];
        app.rebuild_shuffle();
        let first = app.shuffle.clone();
        app.rebuild_shuffle();
        assert_eq!(app.shuffle, first);
        let mut sorted = app.shuffle.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec![0, 1, 2, 3, 4]);
        Ok(())
    }

    fn sample_track(id: TrackId, artist: &str, album: &str, title: &str) -> Track {
        Track {
            id,
            path: PathBuf::from(format!("/music/{title}.flac")),
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            date: Some(2024),
            track_number: Some(1),
            duration: Duration::from_secs(180),
            codec: "FLAC".into(),
            sample_rate: Some(44_100),
            channels: Some(2),
            file_size: 10,
            modified_ns: 0,
            unavailable: false,
            scan_error: None,
            origin: TrackOrigin::Local,
            replay_gain: crate::model::ReplayGainInfo::default(),
        }
    }

    #[test]
    fn queue_plays_before_the_playlist() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut app = App::open(&directory.path().join("queue.db"), true)?;
        app.tracks.insert(1, sample_track(1, "A", "A", "One"));
        app.tracks.insert(2, sample_track(2, "A", "A", "Two"));
        app.playlists[0].items = vec![1, 2];
        app.playlist_selection = 0;
        app.queue.push(2);
        app.playing = Some((app.playlists[0].id, 0));
        app.handle(Action::Next);
        app.tick();
        assert_eq!(app.audio_snapshot.track_id, Some(2));
        assert!(app.queue.is_empty());
        Ok(())
    }

    #[test]
    fn stop_after_current_halts_automatic_advance() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut app = App::open(&directory.path().join("sac.db"), true)?;
        app.tracks.insert(1, sample_track(1, "A", "A", "One"));
        app.tracks.insert(2, sample_track(2, "A", "A", "Two"));
        app.playlists[0].items = vec![1, 2];
        app.playing = Some((app.playlists[0].id, 0));
        app.stop_after_current = true;
        app.advance(true)?;
        assert!(app.playing.is_none());
        assert!(!app.stop_after_current);
        Ok(())
    }

    #[test]
    fn staged_transition_advances_without_polling_for_an_empty_player() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut app = App::open(&directory.path().join("gapless.db"), true)?;
        app.tracks.insert(1, sample_track(1, "A", "A", "One"));
        app.tracks.insert(2, sample_track(2, "A", "A", "Two"));
        app.playlists[0].items = vec![1, 2];

        app.play_at(0, 0)?;
        assert_eq!(app.audio.snapshot().track_id, Some(1));
        assert_eq!(app.audio.snapshot().staged_track_id, None);
        app.tick();
        assert_eq!(app.audio.snapshot().staged_track_id, Some(2));
        app.audio.simulate_staged_transition();
        app.tick();

        assert_eq!(app.audio_snapshot.track_id, Some(2));
        assert_eq!(app.playing, Some((app.playlists[0].id, 1)));
        assert_eq!(app.audio_snapshot.staged_track_id, None);
        Ok(())
    }

    #[test]
    fn queue_and_stop_after_current_replace_the_staged_successor() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut app = App::open(&directory.path().join("restage.db"), true)?;
        app.tracks.insert(1, sample_track(1, "A", "A", "One"));
        app.tracks.insert(2, sample_track(2, "A", "A", "Two"));
        app.tracks.insert(3, sample_track(3, "B", "B", "Queued"));
        app.playlists[0].items = vec![1, 2, 3];
        app.play_at(0, 0)?;
        app.tick();
        assert_eq!(app.audio.snapshot().staged_track_id, Some(2));

        app.playlist_selection = 2;
        app.queue_selection();
        assert_eq!(app.audio.snapshot().staged_track_id, Some(3));
        app.handle(Action::ToggleStopAfterCurrent);
        assert_eq!(app.audio.snapshot().staged_track_id, None);
        Ok(())
    }
}
