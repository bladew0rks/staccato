use std::collections::BTreeSet;

use anyhow::Result;

use crate::model::{Focus, Playlist, PlaylistColumn, PlaylistId, Track};

use super::{
    App, ContentPane,
    overlay::Overlay,
    util::{counted, step_index},
};

impl App {
    pub(crate) fn new_playlist(&mut self) -> Result<()> {
        let name = unique_playlist_name(&self.playlists);
        let playlist = self.store.create_playlist(&name, self.playlists.len())?;
        self.playlists.push(playlist);
        self.active_playlist = self.playlists.len() - 1;
        self.playlist_selection = 0;
        self.rebuild_shuffle();
        self.save_state()?;
        Ok(())
    }

    pub(crate) fn begin_rename_playlist(&mut self) {
        let playlist = &self.playlists[self.active_playlist];
        self.overlay = Overlay::Rename {
            playlist_id: playlist.id,
            text: playlist.name.clone(),
        };
    }

    pub(crate) fn rename_playlist(&mut self, id: PlaylistId, name: String) -> Result<()> {
        let name = name.trim();
        if !name.is_empty() {
            self.store.rename_playlist(id, name)?;
            if let Some(playlist) = self.playlists.iter_mut().find(|playlist| playlist.id == id) {
                playlist.name = name.to_owned();
            }
        }
        self.overlay = Overlay::None;
        Ok(())
    }

    pub(crate) fn close_current_view(&mut self) -> Result<()> {
        match self.content_pane() {
            ContentPane::Soulseek => {
                self.soulseek_open = false;
                if matches!(self.focus, Focus::SoulseekQuery | Focus::SoulseekFilter) {
                    self.focus = Focus::Playlist;
                }
                Ok(())
            }
            ContentPane::Queue => {
                self.queue_open = false;
                self.focus = Focus::Playlist;
                Ok(())
            }
            ContentPane::Settings => {
                self.settings_open = false;
                self.focus = Focus::Playlist;
                Ok(())
            }
            ContentPane::Playlist => self.close_active_playlist(),
        }
    }

    pub(crate) fn close_active_playlist(&mut self) -> Result<()> {
        if self.playlists.len() == 1 {
            self.status = "At least one playlist must remain".into();
            return Ok(());
        }
        let removed = self.playlists.remove(self.active_playlist);
        if self.playing.is_some_and(|(id, _)| id == removed.id) {
            self.clear_transport();
        }
        self.store.delete_playlist(removed.id)?;
        self.active_playlist = self.active_playlist.min(self.playlists.len() - 1);
        self.store.save_playlist_positions(&self.playlists)?;
        self.save_state()?;
        self.rebuild_shuffle();
        self.restage_successor();
        Ok(())
    }

    pub(crate) fn remove_current(&mut self) -> Result<()> {
        match self.content_pane() {
            ContentPane::Soulseek => {
                if let Some(kind) = self.soulseek_ui.selected_kind() {
                    self.soulseek_hide(match kind {
                        crate::soulseek::SoulseekRowKind::File => {
                            crate::soulseek::SoulseekScope::File
                        }
                        crate::soulseek::SoulseekRowKind::Folder => {
                            crate::soulseek::SoulseekScope::Folder
                        }
                        crate::soulseek::SoulseekRowKind::User => {
                            crate::soulseek::SoulseekScope::User
                        }
                    });
                }
                Ok(())
            }
            ContentPane::Queue => {
                self.remove_from_queue();
                Ok(())
            }
            _ => self.remove_selection(),
        }
    }

    pub(crate) fn remove_selection(&mut self) -> Result<()> {
        let mut remove: BTreeSet<crate::model::TrackId> = self.playlist_marked.clone();
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
            self.clear_transport();
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

    pub(crate) fn move_playlist_items(&mut self, delta: i32) -> Result<()> {
        if self.queue_open {
            if self.queue.len() < 2 {
                return Ok(());
            }
            let from = self.queue_selection;
            let to = step_index(from, delta, self.queue.len());
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

    pub(crate) fn sort_playlist(&mut self, column: PlaylistColumn) -> Result<()> {
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

    pub(crate) fn queue_selection(&mut self) {
        let ids = self.selected_library_tracks();
        if ids.is_empty() {
            return;
        }
        self.queue.extend(ids.iter().copied());
        self.restage_successor();
        self.status = format!("Queued {}", counted(ids.len(), "track"));
    }

    pub(crate) fn remove_from_queue(&mut self) {
        if self.queue.is_empty() {
            return;
        }
        self.queue
            .remove(self.queue_selection.min(self.queue.len() - 1));
        self.queue_selection = self.queue_selection.min(self.queue.len().saturating_sub(1));
        self.restage_successor();
    }

    pub(crate) fn add_selection_to_playlist(&mut self, index: usize) -> Result<()> {
        if index >= self.playlists.len() {
            return Ok(());
        }
        let ids = self.selected_library_tracks();
        let added = self.add_tracks_to(index, ids)?;
        self.status = format!(
            "Added {} to {}",
            counted(added, "track"),
            self.playlists[index].name
        );
        Ok(())
    }

    pub(crate) fn toggle_playlist_row(&mut self, index: usize) {
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

    pub(crate) fn activate_menu_item(&mut self, selected: usize) -> Result<()> {
        match self.overlay.clone() {
            Overlay::Menu { menu, .. } => {
                self.overlay = Overlay::None;
                if let Some(action) = super::menus::menu_actions(menu)
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
        }
        Ok(())
    }

    pub(crate) fn rescan_library(&mut self) {
        let roots = match self.store.load_roots() {
            Ok(roots) => roots,
            Err(error) => {
                self.status = format!("Error: {error:#}");
                return;
            }
        };
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
