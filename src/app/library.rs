use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use anyhow::Result;

use crate::{
    library::ScanEvent,
    model::{Focus, Playlist, Track, TrackId, TrackOrigin, text_matches},
};

use super::{
    App, ContentPane,
    overlay::Overlay,
    util::{clamp_index, range_set, step_index},
};

#[derive(Clone, Debug)]
pub struct AlbumEntry {
    pub depth: u8,
    pub label: String,
    pub track_id: Option<TrackId>,
    pub track_ids: Vec<TrackId>,
    pub unavailable: bool,
}

impl AlbumEntry {
    fn group(depth: u8, label: impl Into<String>, track_ids: Vec<TrackId>) -> Self {
        Self {
            depth,
            label: label.into(),
            track_id: None,
            track_ids,
            unavailable: false,
        }
    }

    fn remote_group(label: impl Into<String>, track_ids: Vec<TrackId>, unavailable: bool) -> Self {
        Self {
            depth: 0,
            label: label.into(),
            track_id: None,
            track_ids,
            unavailable,
        }
    }

    fn leaf(depth: u8, track: &Track) -> Self {
        Self {
            depth,
            label: track.title.clone(),
            track_id: Some(track.id),
            track_ids: vec![track.id],
            unavailable: track.unavailable || track.scan_error.is_some(),
        }
    }
}

pub(crate) fn album_tree<'a>(
    tracks: impl IntoIterator<Item = &'a Track>,
    split_origins: bool,
    remote_disconnected: bool,
    include_empty_local: bool,
) -> Vec<AlbumEntry> {
    if !split_origins {
        return group_tracks(tracks, 0);
    }
    let mut local = Vec::new();
    let mut remotes: BTreeMap<String, Vec<&Track>> = BTreeMap::new();
    for track in tracks {
        match &track.origin {
            TrackOrigin::Local => local.push(track),
            TrackOrigin::Remote { server_name, .. } => {
                remotes.entry(server_name.clone()).or_default().push(track);
            }
        }
    }
    let mut entries = Vec::new();
    if include_empty_local || !local.is_empty() {
        entries.push(AlbumEntry::remote_group(
            "This computer",
            local.iter().map(|track| track.id).collect(),
            false,
        ));
        entries.extend(group_tracks(local, 1));
    }
    for (server, tracks) in remotes {
        entries.push(AlbumEntry::remote_group(
            server,
            tracks.iter().map(|track| track.id).collect(),
            remote_disconnected,
        ));
        entries.extend(group_tracks(tracks, 1));
    }
    entries
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
        entries.push(AlbumEntry::group(depth, artist, artist_ids));
        for (album, mut tracks) in albums {
            tracks.sort_by_key(|track| {
                (
                    track.track_number.unwrap_or(u32::MAX),
                    track.title.to_lowercase(),
                )
            });
            let album_ids = tracks.iter().map(|track| track.id).collect::<Vec<_>>();
            entries.push(AlbumEntry::group(depth + 1, album, album_ids));
            entries.extend(
                tracks
                    .into_iter()
                    .map(|track| AlbumEntry::leaf(depth + 2, track)),
            );
        }
    }
    entries
}

impl App {
    pub(crate) fn process_scan_event(&mut self, event: ScanEvent) -> Result<()> {
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

    pub(crate) fn add_paths(&mut self, paths: Vec<PathBuf>) -> Result<()> {
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

    pub(crate) fn add_track(&mut self, track_id: TrackId) -> Result<()> {
        self.add_tracks_to(self.active_playlist, std::iter::once(track_id))
            .map(|_| ())
    }

    pub(crate) fn add_tracks_to(
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

    pub(crate) fn begin_scan(&self, paths: Vec<PathBuf>, add_to_playlist: bool) {
        let known = self
            .tracks
            .values()
            .map(|track| (track.path.clone(), track.to_scanned()))
            .collect();
        self.scanner.scan(paths, add_to_playlist, known);
    }

    pub fn focus_ring(&self) -> Vec<Focus> {
        let mut items = Vec::new();
        if !self.album_filter.is_empty() || self.focus == Focus::AlbumFilter {
            items.push(Focus::AlbumFilter);
        }
        items.push(Focus::AlbumList);
        items.push(Focus::PlaylistTabs);
        match self.content_pane() {
            ContentPane::Soulseek => {
                items.push(Focus::SoulseekQuery);
                if self.soulseek_filters_available() {
                    items.push(Focus::SoulseekFilter);
                }
                items.push(Focus::Playlist);
            }
            ContentPane::Queue => items.push(Focus::Queue),
            ContentPane::Settings => items.push(Focus::Settings),
            ContentPane::Playlist => {
                if !self.playlist_filter.is_empty() || self.focus == Focus::PlaylistFilter {
                    items.push(Focus::PlaylistFilter);
                }
                items.push(Focus::Playlist);
            }
        }
        items.push(Focus::Toolbar);
        items
    }

    pub fn extra_tab_count(&self) -> usize {
        1
    }

    pub fn selected_tab(&self) -> usize {
        if self.queue_open {
            self.playlists.len()
        } else {
            self.active_playlist
        }
    }

    pub fn tab_count(&self) -> usize {
        self.playlists.len() + self.extra_tab_count()
    }

    pub(crate) fn select_playlist_tab(&mut self, index: usize) -> Result<()> {
        let playlists = self.playlists.len();
        if index == playlists {
            self.open_queue_tab();
        } else if index < playlists {
            let same_playlist = index == self.active_playlist;
            self.close_special_tabs();
            self.active_playlist = index;
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
            if !same_playlist {
                self.playlist_selection = 0;
                self.rebuild_shuffle();
                self.save_state()?;
            }
        }
        Ok(())
    }

    pub(crate) fn select_album_row(&mut self, index: usize) {
        self.focus = Focus::AlbumList;
        let len = self.visible_album_entries().len();
        self.album_selection = clamp_index(index, len);
        self.album_anchor = self.album_selection;
        self.album_marked.clear();
        if len > 0 {
            self.album_marked.insert(self.album_selection);
        }
    }

    pub(crate) fn select_playlist_row(&mut self, index: usize) {
        match self.content_pane() {
            ContentPane::Soulseek => {
                self.focus = Focus::Playlist;
                self.soulseek_ui.selected =
                    clamp_index(index, self.soulseek_ui.visible_rows().len());
            }
            ContentPane::Queue => {
                self.focus = Focus::Queue;
                self.queue_selection = clamp_index(index, self.queue.len());
            }
            ContentPane::Settings => {
                self.focus = Focus::Settings;
                if crate::settings::ROWS
                    .get(index)
                    .is_some_and(|row| row.is_item())
                {
                    self.settings_selected = index;
                }
            }
            ContentPane::Playlist => {
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
    }

    pub(crate) fn begin_filter(&mut self) {
        if self.soulseek_open
            || self.settings_open
            || matches!(self.overlay, Overlay::Help(_) | Overlay::Menu { .. })
        {
            return;
        }
        if self.focus == Focus::AlbumList || self.focus == Focus::AlbumFilter {
            self.focus = Focus::AlbumFilter;
        } else if !self.queue_open {
            self.focus = Focus::PlaylistFilter;
        }
    }

    pub(crate) fn clear_filter(&mut self) {
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
        album_tree(filtered, has_remote, self.remote.is_none(), false)
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

    pub(crate) fn clamp_album_selection(&mut self) {
        let len = self.visible_album_entries().len();
        self.album_selection = clamp_index(self.album_selection, len);
        self.album_marked
            .retain(|index| *index < len.max(1) && len > 0);
    }

    pub(crate) fn clamp_playlist_selection(&mut self) {
        let visible = self.visible_playlist_indices();
        if visible.is_empty() {
            self.playlist_selection = 0;
            return;
        }
        if !visible.contains(&self.playlist_selection) {
            self.playlist_selection = visible[0];
        }
    }

    pub(crate) fn selected_album_tracks(&self) -> Vec<TrackId> {
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

    pub(crate) fn selected_playlist_tracks(&self) -> Vec<TrackId> {
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

    pub(crate) fn selected_library_tracks(&self) -> Vec<TrackId> {
        if matches!(self.focus, Focus::AlbumList | Focus::AlbumFilter) {
            self.selected_album_tracks()
        } else {
            self.selected_playlist_tracks()
        }
    }

    pub(crate) fn selected_context_tracks(&self) -> Vec<TrackId> {
        if matches!(self.focus, Focus::AlbumList | Focus::AlbumFilter) {
            self.selected_album_tracks()
        } else if self.queue_open {
            self.queue
                .get(self.queue_selection)
                .copied()
                .into_iter()
                .collect()
        } else {
            self.selected_playlist_tracks()
        }
    }

    pub(crate) fn toggle_mark(&mut self) {
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

    pub(crate) fn extend_selection(&mut self, delta: i32) {
        if self.focus == Focus::AlbumList {
            let len = self.visible_album_entries().len();
            if len == 0 {
                return;
            }
            self.album_selection = step_index(self.album_selection, delta, len);
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
            let next = step_index(current, delta, visible.len());
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

    pub(crate) fn move_selection(&mut self, delta: i32) {
        match self.focus {
            Focus::AlbumFilter => {
                if delta > 0 {
                    self.focus = Focus::AlbumList;
                }
            }
            Focus::PlaylistFilter => {
                if delta > 0 {
                    self.focus = Focus::Playlist;
                }
            }
            Focus::AlbumList => {
                let len = self.visible_album_entries().len();
                self.album_selection = step_index(self.album_selection, delta, len);
                self.album_anchor = self.album_selection;
                self.album_marked.clear();
                if len > 0 {
                    self.album_marked.insert(self.album_selection);
                }
            }
            Focus::Queue => {
                self.queue_selection = step_index(self.queue_selection, delta, self.queue.len());
            }
            Focus::Settings => {
                let step = if delta < 0 { -1 } else { 1 };
                for _ in 0..delta.unsigned_abs() {
                    self.settings_selected = crate::settings::step(self.settings_selected, step);
                }
            }
            Focus::SoulseekQuery if self.soulseek_open => {
                if delta > 0 {
                    self.focus = if self.soulseek_filters_available() {
                        Focus::SoulseekFilter
                    } else {
                        Focus::Playlist
                    };
                }
            }
            Focus::SoulseekFilter if self.soulseek_open => {
                if delta > 0 {
                    self.focus = Focus::Playlist;
                } else if delta < 0 {
                    self.focus = Focus::SoulseekQuery;
                }
            }
            Focus::Playlist if self.soulseek_open => {
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
                    self.soulseek_ui.selected = step_index(self.soulseek_ui.selected, delta, len);
                }
            }
            _ => {
                let visible = self.visible_playlist_indices();
                if visible.is_empty() {
                    return;
                }
                let current = visible
                    .iter()
                    .position(|&index| index == self.playlist_selection)
                    .unwrap_or(0);
                let next = step_index(current, delta, visible.len());
                self.playlist_selection = visible[next];
                self.playlist_anchor = self.playlist_selection;
                self.playlist_marked.clear();
                if let Some(id) = self.active_playlist().items.get(self.playlist_selection) {
                    self.playlist_marked.insert(*id);
                }
            }
        }
    }

    pub(crate) fn activate_selection(&mut self) -> Result<()> {
        match self.focus {
            Focus::AlbumList | Focus::AlbumFilter => {
                let ids = self.selected_album_tracks();
                if ids.is_empty() {
                    return Ok(());
                }
                let added = self.add_tracks_to(self.active_playlist, ids)?;
                self.status = format!(
                    "Added {} to {}",
                    super::util::counted(added, "track"),
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

    pub fn album_entries(&self) -> Vec<AlbumEntry> {
        let has_remote = self.tracks.values().any(|track| track.origin.is_remote());
        album_tree(
            self.tracks.values(),
            has_remote,
            self.remote.is_none(),
            true,
        )
    }

    pub fn active_playlist(&self) -> &Playlist {
        &self.playlists[self.active_playlist]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ReplayGainInfo, TrackOrigin};
    use std::{path::PathBuf, time::Duration};

    fn sample(id: TrackId, artist: &str, album: &str, title: &str, remote: Option<&str>) -> Track {
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
            origin: match remote {
                Some(server) => TrackOrigin::Remote {
                    fingerprint: "fp".into(),
                    remote_id: id.to_string(),
                    server_name: server.into(),
                },
                None => TrackOrigin::Local,
            },
            replay_gain: ReplayGainInfo::default(),
        }
    }

    #[test]
    fn album_tree_splits_local_and_remote() {
        let local = sample(1, "A", "A1", "One", None);
        let remote = sample(2, "B", "B1", "Two", Some("Studio"));
        let entries = album_tree([&local, &remote], true, false, true);
        assert_eq!(entries[0].label, "This computer");
        assert!(entries.iter().any(|entry| entry.label == "Studio"));
    }

    #[test]
    fn album_tree_omits_empty_local_when_filtered() {
        let remote = sample(2, "B", "B1", "Two", Some("Studio"));
        let entries = album_tree([&remote], true, true, false);
        assert!(entries.iter().all(|entry| entry.label != "This computer"));
        assert_eq!(entries[0].label, "Studio");
        assert!(entries[0].unavailable);
    }
}
