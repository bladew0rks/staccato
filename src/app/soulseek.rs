use anyhow::Result;

use crate::{
    action::Action,
    model::Focus,
    soulseek::{self, SoulseekEvent, SoulseekPhase, SoulseekRowKind, SoulseekScope, SoulseekUi},
};

use super::{App, ContentPane, overlay::Overlay};

impl App {
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

    pub(crate) fn soulseek_text_mut(&mut self) -> Option<&mut String> {
        if !self.soulseek_captures_text() {
            return None;
        }
        Some(match self.soulseek_ui.phase {
            SoulseekPhase::Username => &mut self.soulseek_ui.username,
            SoulseekPhase::Password => &mut self.soulseek_ui.password,
            SoulseekPhase::Ready => &mut self.soulseek_ui.query,
        })
    }

    pub(crate) fn soulseek_refresh_filter_status(&mut self) {
        if self.soulseek_ui.hits.is_empty() {
            return;
        }
        self.soulseek_ui.status = self.soulseek_ui.results_status();
        self.status = self.soulseek_ui.status.clone();
    }

    pub(crate) fn set_soulseek_status(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.status = message.clone();
        self.soulseek_ui.status = message;
    }

    pub(crate) fn open_soulseek_context(
        &mut self,
        row: Option<usize>,
        x: Option<u16>,
        y: Option<u16>,
    ) {
        if !self.soulseek_open {
            return;
        }
        self.focus = Focus::Playlist;
        if let Some(index) = row {
            self.soulseek_ui.selected =
                super::util::clamp_index(index, self.soulseek_ui.visible_rows().len());
        }
        self.open_context_menu(self.soulseek_context_items(), x.zip(y));
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

    pub(crate) fn soulseek_download(&mut self, scope: SoulseekScope) {
        let hits = self.soulseek_ui.hits_in_scope(scope);
        if hits.is_empty() {
            return;
        }
        if self.soulseek.is_none() {
            self.set_soulseek_status("Soulseek is not connected");
            return;
        }
        let label = if hits.len() == 1 {
            hits[0].file_name().to_owned()
        } else {
            format!("{} files", hits.len())
        };
        self.set_soulseek_status(format!("Downloading {label}…"));
        self.soulseek_downloads_active = self.soulseek_downloads_active.saturating_add(hits.len());
        if let Some(handle) = &self.soulseek {
            handle.download_all(&hits);
        }
    }

    pub(crate) fn soulseek_hide(&mut self, scope: SoulseekScope) {
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
        self.set_soulseek_status(format!("Hidden {label}"));
    }

    pub(crate) fn open_soulseek_tab(&mut self) -> Result<()> {
        self.show_pane(ContentPane::Soulseek);
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
        self.set_soulseek_status("Connecting to Soulseek…");
        Ok(())
    }

    pub(crate) fn soulseek_activate(&mut self) -> Result<()> {
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
                        self.set_soulseek_status("Soulseek is not connected");
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

    pub(crate) fn drain_soulseek_events(&mut self) {
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
                SoulseekEvent::Status(message) => self.set_soulseek_status(message),
                SoulseekEvent::Finished(path) => {
                    self.soulseek_downloads_active =
                        self.soulseek_downloads_active.saturating_sub(1);
                    self.set_soulseek_status(format!("Downloaded {}", path.display()));
                    let _ = self.add_paths(vec![path]);
                }
                SoulseekEvent::DownloadFailed(message) => {
                    self.soulseek_downloads_active =
                        self.soulseek_downloads_active.saturating_sub(1);
                    self.set_soulseek_status(format!("Soulseek: {message}"));
                    self.soulseek_ui.status = message;
                }
                SoulseekEvent::Error(message) => {
                    self.set_soulseek_status(format!("Soulseek: {message}"));
                    self.soulseek_ui.status = message;
                }
            }
        }
    }

    pub(crate) fn apply_soulseek_format(&mut self, format: crate::soulseek::SoulseekFormat) {
        if self.soulseek_open {
            self.focus = Focus::SoulseekFilter;
            self.soulseek_ui.set_format(format);
            self.soulseek_refresh_filter_status();
        }
    }

    pub(crate) fn cycle_soulseek_format(&mut self, delta: i32) {
        if self.soulseek_open {
            self.focus = Focus::SoulseekFilter;
            if delta != 0 {
                self.soulseek_ui.cycle_format(delta);
            }
            self.soulseek_refresh_filter_status();
        }
    }

    pub(crate) fn toggle_soulseek_free_slot(&mut self) {
        if self.soulseek_open {
            self.focus = Focus::SoulseekFilter;
            self.soulseek_ui.toggle_free_slot();
            self.soulseek_refresh_filter_status();
        }
    }
}
