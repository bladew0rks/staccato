use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Result;

use crate::{
    action::Action,
    library::is_supported,
    model::{Focus, PlaylistId},
    net::DiscoveredServer,
};

use super::{
    App, ContentPane,
    menus::menu_actions,
    util::{clamp_index, step_index, wrap_index},
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
    pub(crate) fn new(mode: PickerMode) -> Self {
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
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| file_name_key(&a.path).cmp(&file_name_key(&b.path)))
        });
        self.selected = clamp_index(self.selected, self.entries.len());
    }
}

fn file_name_key(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase()
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

impl Overlay {
    pub(crate) fn text_mut(&mut self) -> Option<&mut String> {
        match self {
            Overlay::Rename { text, .. }
            | Overlay::Connect { text, .. }
            | Overlay::Pair { text, .. } => Some(text),
            _ => None,
        }
    }

    pub(crate) fn open_context(
        items: Vec<(String, Action)>,
        at: Option<(u16, u16)>,
    ) -> Option<Self> {
        (!items.is_empty()).then_some(Self::ContextMenu {
            selected: 0,
            items,
            at,
        })
    }
}

impl App {
    pub(crate) fn open_context_menu(
        &mut self,
        items: Vec<(String, Action)>,
        at: Option<(u16, u16)>,
    ) {
        if let Some(overlay) = Overlay::open_context(items, at) {
            self.overlay = overlay;
        }
    }

    pub(crate) fn overlay_move(&mut self, delta: i32) {
        match &mut self.overlay {
            Overlay::Menu { menu, selected } => {
                *selected = wrap_index(*selected, delta, menu_actions(*menu).len());
            }
            Overlay::ContextMenu {
                selected, items, ..
            } => {
                *selected = wrap_index(*selected, delta, items.len());
            }
            Overlay::PathPicker(picker) => {
                picker.selected = step_index(picker.selected, delta, picker.entries.len());
            }
            _ => {}
        }
    }

    pub(crate) fn overlay_activate(&mut self) -> Result<()> {
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

    pub(crate) fn picker_choose_current(&mut self) -> Result<()> {
        if let Overlay::PathPicker(picker) = &self.overlay
            && picker.mode == PickerMode::Folder
        {
            let path = picker.directory.clone();
            self.add_paths(vec![path])?;
        }
        Ok(())
    }

    pub(crate) fn text_input(&mut self, character: char) {
        if character.is_control() {
            return;
        }
        if let Some(text) = self.overlay.text_mut() {
            text.push(character);
        }
        if let Some(text) = self.soulseek_text_mut() {
            text.push(character);
        } else if self.focus == Focus::AlbumFilter {
            self.album_filter.push(character);
            self.clamp_album_selection();
        } else if self.focus == Focus::PlaylistFilter {
            self.playlist_filter.push(character);
            self.clamp_playlist_selection();
        }
    }

    pub(crate) fn text_backspace(&mut self) {
        if let Some(text) = self.overlay.text_mut() {
            text.pop();
        }
        if let Some(text) = self.soulseek_text_mut() {
            text.pop();
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

    pub(crate) fn show_pane(&mut self, pane: ContentPane) {
        self.close_special_tabs();
        match pane {
            ContentPane::Playlist => {}
            ContentPane::Queue => {
                self.queue_open = true;
                self.focus = Focus::Queue;
                self.queue_selection = clamp_index(self.queue_selection, self.queue.len());
            }
            ContentPane::Settings => {
                self.settings_open = true;
                self.focus = Focus::Settings;
                if !crate::settings::ROWS
                    .get(self.settings_selected)
                    .is_some_and(|row| row.is_item())
                {
                    self.settings_selected = crate::settings::first_item();
                }
            }
            ContentPane::Soulseek => {
                self.soulseek_open = true;
                self.focus = Focus::SoulseekQuery;
            }
        }
    }

    pub(crate) fn close_special_tabs(&mut self) {
        self.soulseek_open = false;
        self.queue_open = false;
        self.settings_open = false;
    }

    pub(crate) fn open_queue_tab(&mut self) {
        self.show_pane(ContentPane::Queue);
    }

    pub(crate) fn open_settings_tab(&mut self) {
        self.show_pane(ContentPane::Settings);
    }
}
