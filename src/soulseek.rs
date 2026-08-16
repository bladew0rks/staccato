use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, unbounded};
use serde::{Deserialize, Serialize};
use soulseek_rs::{Client, ClientSettings, DownloadStatus};

use crate::library::is_supported;

const SEARCH_WAIT: Duration = Duration::from_secs(8);
const MAX_HITS: usize = 150;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SoulseekPhase {
    Username,
    Password,
    Ready,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoulseekRowKind {
    User,
    Folder,
    File,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoulseekScope {
    File,
    Folder,
    User,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SoulseekFormat {
    #[default]
    All,
    Flac,
    Mp3,
    Ogg,
    Aac,
    Wav,
}

impl SoulseekFormat {
    pub const ALL: [Self; 6] = [
        Self::All,
        Self::Flac,
        Self::Mp3,
        Self::Ogg,
        Self::Aac,
        Self::Wav,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Flac => "FLAC",
            Self::Mp3 => "MP3",
            Self::Ogg => "Ogg",
            Self::Aac => "AAC",
            Self::Wav => "WAV",
        }
    }

    pub fn matches(self, hit: &SoulseekHit) -> bool {
        let Some(ext) = hit.extension() else {
            return self == Self::All;
        };
        match self {
            Self::All => true,
            Self::Flac => ext == "flac",
            Self::Mp3 => ext == "mp3",
            Self::Ogg => matches!(ext.as_str(), "ogg" | "oga"),
            Self::Aac => matches!(ext.as_str(), "aac" | "m4a" | "mp4" | "alac"),
            Self::Wav => matches!(ext.as_str(), "wav" | "wave" | "aif" | "aiff"),
        }
    }

    pub fn cycle(self, delta: i32) -> Self {
        let len = Self::ALL.len() as i32;
        let at = Self::ALL.iter().position(|item| *item == self).unwrap_or(0) as i32;
        Self::ALL[(at + delta).rem_euclid(len) as usize]
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SoulseekFilter {
    pub format: SoulseekFormat,
    pub free_slot: bool,
}

impl SoulseekFilter {
    pub fn matches(&self, hit: &SoulseekHit) -> bool {
        self.format.matches(hit) && (!self.free_slot || hit.slots > 0)
    }

    pub fn is_active(&self) -> bool {
        self.format != SoulseekFormat::All || self.free_slot
    }
}

#[derive(Clone, Debug)]
pub struct SoulseekRow {
    pub kind: SoulseekRowKind,
    pub label: String,
    pub detail: String,
    pub key: String,
    pub username: String,
    pub folder: Option<String>,
    pub hit: Option<SoulseekHit>,
}

#[derive(Clone, Debug)]
pub struct SoulseekUi {
    pub phase: SoulseekPhase,
    pub query: String,
    pub username: String,
    pub password: String,
    pub status: String,
    pub hits: Vec<SoulseekHit>,
    pub selected: usize,
    pub collapsed: BTreeSet<String>,
    pub filter: SoulseekFilter,
}

impl Default for SoulseekUi {
    fn default() -> Self {
        Self {
            phase: SoulseekPhase::Username,
            query: String::new(),
            username: String::new(),
            password: String::new(),
            status: "Library > Search Soulseek to sign in".into(),
            hits: Vec::new(),
            selected: 0,
            collapsed: BTreeSet::new(),
            filter: SoulseekFilter::default(),
        }
    }
}

impl SoulseekUi {
    pub fn ready() -> Self {
        Self {
            phase: SoulseekPhase::Ready,
            status: "Type a search and press Enter".into(),
            ..Self::default()
        }
    }

    pub fn set_hits(&mut self, hits: Vec<SoulseekHit>) {
        self.collapsed.clear();
        self.hits = hits;
        self.selected = 0;
    }

    pub fn matching_hits(&self) -> Vec<SoulseekHit> {
        self.hits
            .iter()
            .filter(|hit| self.filter.matches(hit))
            .cloned()
            .collect()
    }

    pub fn visible_rows(&self) -> Vec<SoulseekRow> {
        visible_rows(&self.matching_hits(), &self.collapsed)
    }

    pub fn results_status(&self) -> String {
        let total = self.hits.len();
        let shown = self.matching_hits().len();
        if total == 0 {
            return self.status.clone();
        }
        let noun = if total == 1 { "file" } else { "files" };
        let mut text = if self.filter.is_active() {
            format!("{shown} of {total} {noun}")
        } else {
            format!("{shown} {noun}  ·  Shift+F10 or right-click")
        };
        if self.filter.format != SoulseekFormat::All {
            text.push_str("  ·  ");
            text.push_str(self.filter.format.label());
        }
        if self.filter.free_slot {
            text.push_str("  ·  free slots");
        }
        text
    }

    pub fn set_format(&mut self, format: SoulseekFormat) {
        self.filter.format = format;
        self.clamp_selected();
    }

    pub fn cycle_format(&mut self, delta: i32) {
        self.set_format(self.filter.format.cycle(delta));
    }

    pub fn toggle_free_slot(&mut self) {
        self.filter.free_slot = !self.filter.free_slot;
        self.clamp_selected();
    }

    fn clamp_selected(&mut self) {
        let len = self.visible_rows().len();
        self.selected = if len == 0 {
            0
        } else {
            self.selected.min(len - 1)
        };
    }

    pub fn prompt(&self, caret: bool) -> String {
        let caret = if caret { "█" } else { "" };
        match self.phase {
            SoulseekPhase::Username => format!("Username: {}{caret}", self.username),
            SoulseekPhase::Password => format!(
                "Password: {}{caret}",
                "*".repeat(self.password.chars().count())
            ),
            SoulseekPhase::Ready => format!("Search: {}{caret}", self.query),
        }
    }

    pub fn toggle(&mut self, expand: Option<bool>) -> Option<SoulseekHit> {
        let rows = self.visible_rows();
        let row = rows.get(self.selected)?;
        let result = match row.kind {
            SoulseekRowKind::File => row.hit.clone(),
            SoulseekRowKind::User | SoulseekRowKind::Folder => {
                let key = row.key.clone();
                let open = !self.collapsed.contains(&key);
                let should_collapse = match expand {
                    Some(true) => false,
                    Some(false) => true,
                    None => open,
                };
                if should_collapse {
                    self.collapsed.insert(key);
                } else {
                    self.collapsed.remove(&key);
                }
                None
            }
        };
        let len = self.visible_rows().len();
        self.selected = if len == 0 {
            0
        } else {
            self.selected.min(len - 1)
        };
        result
    }

    pub fn selected_kind(&self) -> Option<SoulseekRowKind> {
        self.visible_rows().get(self.selected).map(|row| row.kind)
    }

    pub fn hits_in_scope(&self, scope: SoulseekScope) -> Vec<SoulseekHit> {
        let rows = self.visible_rows();
        let Some(row) = rows.get(self.selected) else {
            return Vec::new();
        };
        match scope {
            SoulseekScope::File => row.hit.iter().cloned().collect(),
            SoulseekScope::Folder => {
                let Some((user, folder)) = scope_folder(row) else {
                    return Vec::new();
                };
                self.hits
                    .iter()
                    .filter(|hit| {
                        self.filter.matches(hit) && hit.username == user && hit.folder() == folder
                    })
                    .cloned()
                    .collect()
            }
            SoulseekScope::User => {
                if row.username.is_empty() {
                    return Vec::new();
                }
                self.hits
                    .iter()
                    .filter(|hit| self.filter.matches(hit) && hit.username == row.username)
                    .cloned()
                    .collect()
            }
        }
    }

    pub fn hide(&mut self, scope: SoulseekScope) {
        let rows = self.visible_rows();
        let Some(row) = rows.get(self.selected) else {
            return;
        };
        match scope {
            SoulseekScope::File => {
                if let Some(hit) = &row.hit {
                    let username = hit.username.clone();
                    let name = hit.name.clone();
                    self.hits
                        .retain(|item| !(item.username == username && item.name == name));
                }
            }
            SoulseekScope::Folder => {
                let Some((user, folder)) = scope_folder(row) else {
                    return;
                };
                self.hits
                    .retain(|hit| !(hit.username == user && hit.folder() == folder));
            }
            SoulseekScope::User => {
                if row.username.is_empty() {
                    return;
                }
                let username = row.username.clone();
                self.hits.retain(|hit| hit.username != username);
            }
        }
        let len = self.visible_rows().len();
        self.selected = if len == 0 {
            0
        } else {
            self.selected.min(len - 1)
        };
    }
}

fn scope_folder(row: &SoulseekRow) -> Option<(String, String)> {
    match row.kind {
        SoulseekRowKind::File => row
            .hit
            .as_ref()
            .map(|hit| (hit.username.clone(), hit.folder().to_owned())),
        SoulseekRowKind::Folder => row
            .folder
            .clone()
            .map(|folder| (row.username.clone(), folder)),
        SoulseekRowKind::User => None,
    }
}

/// Route soulseek-rs-lib's own logger off stderr (it paints through the TUI).
pub fn init_logging(data_dir: &Path) {
    let path = data_dir.join("soulseek.log");
    if std::env::var_os("LOG_FILE").is_none() {
        // The crate only honours LOG_FILE at logger::init(); after that, Once
        // ignores later calls. Set it before any Client is constructed.
        unsafe {
            // Process-global, but this runs once on the TUI thread before Client::new.
            std::env::set_var("LOG_FILE", &path);
        }
    }
    soulseek_rs::utils::logger::init();
    soulseek_rs::utils::logger::enable_buffering();
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SoulseekCredentials {
    pub username: String,
    pub password: String,
}

#[derive(Clone, Debug)]
pub struct SoulseekHit {
    pub username: String,
    pub name: String,
    pub size: u64,
    pub slots: u8,
    pub speed: u32,
    pub bitrate: Option<u32>,
}

impl SoulseekHit {
    pub fn file_name(&self) -> &str {
        self.name.rsplit(['\\', '/']).next().unwrap_or(&self.name)
    }

    pub fn folder(&self) -> &str {
        self.name
            .rsplit_once(['\\', '/'])
            .map(|(folder, _)| folder)
            .unwrap_or(".")
    }

    pub fn extension(&self) -> Option<String> {
        Path::new(&self.name.replace('\\', "/"))
            .extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
    }
}

pub enum SoulseekCommand {
    Search(String),
    Download {
        username: String,
        name: String,
        size: u64,
    },
}

pub enum SoulseekEvent {
    Ready,
    SearchResults(Vec<SoulseekHit>),
    Status(String),
    Finished(PathBuf),
    DownloadFailed(String),
    Error(String),
}

pub struct SoulseekHandle {
    pub events: Receiver<SoulseekEvent>,
    commands: Sender<SoulseekCommand>,
}

impl SoulseekHandle {
    pub fn search(&self, query: String) {
        let _ = self.commands.send(SoulseekCommand::Search(query));
    }

    pub fn download(&self, hit: &SoulseekHit) {
        let _ = self.commands.send(SoulseekCommand::Download {
            username: hit.username.clone(),
            name: hit.name.clone(),
            size: hit.size,
        });
    }

    pub fn download_all(&self, hits: &[SoulseekHit]) {
        for hit in hits {
            self.download(hit);
        }
    }
}

pub fn start(credentials: SoulseekCredentials, download_dir: PathBuf) -> SoulseekHandle {
    let (event_tx, event_rx) = unbounded();
    let (command_tx, command_rx) = unbounded();
    thread::spawn(move || {
        if let Err(error) = session(credentials, download_dir, event_tx.clone(), command_rx) {
            tracing::error!(%error, "soulseek session failed");
            let _ = event_tx.send(SoulseekEvent::Error(error.to_string()));
        }
    });
    SoulseekHandle {
        events: event_rx,
        commands: command_tx,
    }
}

fn session(
    credentials: SoulseekCredentials,
    download_dir: PathBuf,
    events: Sender<SoulseekEvent>,
    commands: Receiver<SoulseekCommand>,
) -> Result<()> {
    fs::create_dir_all(&download_dir)
        .with_context(|| format!("creating {}", download_dir.display()))?;
    let mut settings = ClientSettings::new(&credentials.username, &credentials.password);
    settings.enable_listen = true;
    let mut client = Client::with_settings(settings);
    tracing::info!(user = %credentials.username, "connecting to Soulseek");
    client.connect().context("connecting to Soulseek")?;
    if !client.login().context("logging in to Soulseek")? {
        anyhow::bail!("Soulseek login rejected");
    }
    let _ = events.send(SoulseekEvent::Ready);
    let client = std::sync::Arc::new(client);

    while let Ok(command) = commands.recv() {
        match command {
            SoulseekCommand::Search(query) => {
                let _ = events.send(SoulseekEvent::Status(format!("Searching “{query}”…")));
                match client.search(&query, SEARCH_WAIT) {
                    Ok(results) => {
                        let hits = flatten_hits(results);
                        let _ = events.send(SoulseekEvent::Status(format!(
                            "{} matching files",
                            hits.len()
                        )));
                        let _ = events.send(SoulseekEvent::SearchResults(hits));
                    }
                    Err(error) => {
                        let _ = events.send(SoulseekEvent::Error(error.to_string()));
                    }
                }
            }
            SoulseekCommand::Download {
                username,
                name,
                size,
            } => {
                let dest = dest_for_remote_path(&download_dir, &name);
                if let Err(error) = fs::create_dir_all(&dest) {
                    let _ = events.send(SoulseekEvent::DownloadFailed(format!(
                        "creating {}: {error}",
                        dest.display()
                    )));
                    continue;
                }
                let display = name.rsplit(['\\', '/']).next().unwrap_or(&name).to_owned();
                let _ = events.send(SoulseekEvent::Status(format!("Downloading {display}…")));
                match client.download(name.clone(), username, size, dest.display().to_string()) {
                    Ok((_download, status)) => {
                        let events = events.clone();
                        let dest = dest.clone();
                        let display = display.clone();
                        thread::spawn(move || {
                            for update in status {
                                match update {
                                    DownloadStatus::Queued => {
                                        let _ = events.send(SoulseekEvent::Status(format!(
                                            "Queued {display}"
                                        )));
                                    }
                                    DownloadStatus::InProgress {
                                        bytes_downloaded,
                                        total_bytes,
                                        speed_bytes_per_sec,
                                    } => {
                                        let pct = bytes_downloaded
                                            .saturating_mul(100)
                                            .checked_div(total_bytes)
                                            .unwrap_or(0);
                                        let _ = events.send(SoulseekEvent::Status(format!(
                                            "{display}  {pct}%  {:.0} KB/s",
                                            speed_bytes_per_sec / 1024.0
                                        )));
                                    }
                                    DownloadStatus::Completed => {
                                        let path = dest.join(&display);
                                        let _ = events.send(SoulseekEvent::Finished(
                                            if path.exists() { path } else { dest },
                                        ));
                                        return;
                                    }
                                    DownloadStatus::Failed(reason) => {
                                        let _ = events.send(SoulseekEvent::DownloadFailed(
                                            reason.unwrap_or_else(|| {
                                                format!("download of {display} failed")
                                            }),
                                        ));
                                        return;
                                    }
                                    DownloadStatus::TimedOut => {
                                        let _ = events.send(SoulseekEvent::DownloadFailed(
                                            format!("download of {display} timed out"),
                                        ));
                                        return;
                                    }
                                    DownloadStatus::Paused { .. } => {}
                                }
                            }
                        });
                    }
                    Err(error) => {
                        let _ = events.send(SoulseekEvent::DownloadFailed(error.to_string()));
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn flatten_hits(results: Vec<soulseek_rs::SearchResult>) -> Vec<SoulseekHit> {
    let mut hits = Vec::new();
    for result in results {
        for file in result.files {
            if !is_supported(Path::new(&file.name.replace('\\', "/"))) {
                continue;
            }
            let username = if file.username.is_empty() {
                result.username.clone()
            } else {
                file.username
            };
            hits.push(SoulseekHit {
                username,
                name: file.name,
                size: file.size,
                slots: result.slots,
                speed: result.speed,
                bitrate: file.attribs.get(&0).copied(),
            });
        }
    }
    hits.sort_by(|a, b| {
        b.slots
            .cmp(&a.slots)
            .then(b.bitrate.unwrap_or(0).cmp(&a.bitrate.unwrap_or(0)))
            .then(b.size.cmp(&a.size))
    });
    hits.truncate(MAX_HITS);
    hits
}

pub fn load_credentials(data_dir: &Path) -> Option<SoulseekCredentials> {
    let user = std::env::var("STACCATO_SOULSEEK_USER").ok();
    let password = std::env::var("STACCATO_SOULSEEK_PASSWORD").ok();
    if let (Some(username), Some(password)) = (user, password)
        && !username.is_empty()
        && !password.is_empty()
    {
        return Some(SoulseekCredentials { username, password });
    }
    let bytes = fs::read(data_dir.join("soulseek.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn save_credentials(data_dir: &Path, credentials: &SoulseekCredentials) -> Result<()> {
    fs::create_dir_all(data_dir)?;
    fs::write(
        data_dir.join("soulseek.json"),
        serde_json::to_vec_pretty(credentials)?,
    )?;
    Ok(())
}

fn dest_for_remote_path(root: &Path, remote: &str) -> PathBuf {
    let folder = remote
        .rsplit_once(['\\', '/'])
        .map(|(folder, _)| folder)
        .unwrap_or("");
    let album = folder.rsplit(['\\', '/']).next().unwrap_or("");
    let album = sanitize_component(album);
    if album.is_empty() {
        root.to_path_buf()
    } else {
        root.join(album)
    }
}

fn sanitize_component(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." {
        return String::new();
    }
    name.chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | '\0') {
                '_'
            } else {
                c
            }
        })
        .collect()
}

fn user_key(user: &str) -> String {
    format!("u:{user}")
}

fn folder_key(user: &str, folder: &str) -> String {
    format!("f:{user}:{folder}")
}

fn format_speed(speed: u32) -> String {
    if speed >= 1_048_576 {
        format!("{:.2} MiB/s", f64::from(speed) / 1_048_576.0)
    } else if speed >= 1024 {
        format!("{:.0} KiB/s", f64::from(speed) / 1024.0)
    } else if speed == 0 {
        "—".into()
    } else {
        format!("{speed} B/s")
    }
}

type UserFolders = BTreeMap<String, Vec<SoulseekHit>>;

fn visible_rows(hits: &[SoulseekHit], collapsed: &BTreeSet<String>) -> Vec<SoulseekRow> {
    let mut users: BTreeMap<String, (u8, u32, UserFolders)> = BTreeMap::new();
    let mut order = Vec::new();
    for hit in hits {
        if !users.contains_key(&hit.username) {
            order.push(hit.username.clone());
        }
        let entry = users
            .entry(hit.username.clone())
            .or_insert_with(|| (hit.slots, hit.speed, BTreeMap::new()));
        entry.0 = entry.0.max(hit.slots);
        entry.1 = entry.1.max(hit.speed);
        entry
            .2
            .entry(hit.folder().to_owned())
            .or_default()
            .push(hit.clone());
    }
    let mut rows = Vec::new();
    for user in order {
        let Some((slots, speed, folders)) = users.remove(&user) else {
            continue;
        };
        let ukey = user_key(&user);
        let files: usize = folders.values().map(Vec::len).sum();
        let chevron = if collapsed.contains(&ukey) {
            "▸"
        } else {
            "▾"
        };
        rows.push(SoulseekRow {
            kind: SoulseekRowKind::User,
            label: format!("{chevron} {user}"),
            detail: format!(
                "{}   {} file{}",
                format_speed(speed),
                files,
                if files == 1 { "" } else { "s" }
            ),
            key: ukey.clone(),
            username: user.clone(),
            folder: None,
            hit: None,
        });
        if collapsed.contains(&ukey) {
            continue;
        }
        for (folder, files) in folders {
            let fkey = folder_key(&user, &folder);
            let chevron = if collapsed.contains(&fkey) {
                "▸"
            } else {
                "▾"
            };
            rows.push(SoulseekRow {
                kind: SoulseekRowKind::Folder,
                label: format!("  {chevron} {folder}"),
                detail: if slots == 0 {
                    "queued".into()
                } else {
                    format!("{slots} slot{}", if slots == 1 { "" } else { "s" })
                },
                key: fkey.clone(),
                username: user.clone(),
                folder: Some(folder.clone()),
                hit: None,
            });
            if collapsed.contains(&fkey) {
                continue;
            }
            for file in files {
                let mb = file.size as f64 / 1_048_576.0;
                let br = file
                    .bitrate
                    .map(|rate| format!("{rate} kbps"))
                    .unwrap_or_default();
                rows.push(SoulseekRow {
                    kind: SoulseekRowKind::File,
                    label: format!("      {}", file.file_name()),
                    detail: format!("{mb:.1} M  {br}"),
                    key: format!("file:{}:{}", file.username, file.name),
                    username: file.username.clone(),
                    folder: Some(file.folder().to_owned()),
                    hit: Some(file),
                });
            }
        }
    }
    rows
}

pub fn download_dir(data_dir: &Path, roots: &[PathBuf]) -> PathBuf {
    roots
        .first()
        .cloned()
        .unwrap_or_else(|| data_dir.join("soulseek"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use soulseek_rs::SearchResult;

    #[test]
    fn flatten_hits_keeps_audio_and_prefers_free_slots() {
        let results = vec![SearchResult {
            token: 1,
            username: "a".into(),
            slots: 0,
            speed: 1,
            files: vec![soulseek_rs::File {
                username: String::new(),
                name: "x\\skip.txt".into(),
                size: 10,
                attribs: Default::default(),
            }],
        }];
        assert!(flatten_hits(results).is_empty());
    }

    #[test]
    fn results_cascade_user_then_folder_then_file() {
        let hits = vec![SoulseekHit {
            username: "alice".into(),
            name: r"album\track.flac".into(),
            size: 1_000_000,
            slots: 1,
            speed: 2_000_000,
            bitrate: Some(320),
        }];
        let mut ui = SoulseekUi::ready();
        ui.set_hits(hits);
        let expanded = ui.visible_rows();
        assert_eq!(expanded.len(), 3);
        assert_eq!(expanded[0].kind, SoulseekRowKind::User);
        assert_eq!(expanded[1].kind, SoulseekRowKind::Folder);
        assert_eq!(expanded[2].kind, SoulseekRowKind::File);
        ui.selected = 1;
        ui.toggle(Some(false));
        let collapsed = ui.visible_rows();
        assert_eq!(collapsed.len(), 2);
        assert_eq!(collapsed[1].kind, SoulseekRowKind::Folder);
    }

    #[test]
    fn enter_on_a_folder_selects_every_file_in_it() {
        let hits = vec![
            SoulseekHit {
                username: "alice".into(),
                name: r"album\a.flac".into(),
                size: 1,
                slots: 1,
                speed: 1,
                bitrate: None,
            },
            SoulseekHit {
                username: "alice".into(),
                name: r"album\b.flac".into(),
                size: 1,
                slots: 1,
                speed: 1,
                bitrate: None,
            },
            SoulseekHit {
                username: "alice".into(),
                name: r"other\c.flac".into(),
                size: 1,
                slots: 1,
                speed: 1,
                bitrate: None,
            },
        ];
        let mut ui = SoulseekUi::ready();
        ui.set_hits(hits);
        ui.selected = 1;
        let folder = ui.hits_in_scope(SoulseekScope::Folder);
        assert_eq!(folder.len(), 2);
        assert!(folder.iter().all(|hit| hit.folder() == "album"));
        ui.selected = 0;
        assert_eq!(ui.hits_in_scope(SoulseekScope::User).len(), 3);
    }

    #[test]
    fn hide_user_drops_that_user_from_the_current_search() {
        let mut ui = SoulseekUi::ready();
        ui.set_hits(vec![
            SoulseekHit {
                username: "alice".into(),
                name: r"album\a.flac".into(),
                size: 1,
                slots: 1,
                speed: 1,
                bitrate: None,
            },
            SoulseekHit {
                username: "bob".into(),
                name: r"album\b.flac".into(),
                size: 1,
                slots: 1,
                speed: 1,
                bitrate: None,
            },
        ]);
        ui.selected = 0;
        ui.hide(SoulseekScope::User);
        assert!(ui.hits.iter().all(|hit| hit.username == "bob"));
        assert_eq!(ui.hits_in_scope(SoulseekScope::User).len(), 1);
    }

    fn hit(name: &str, slots: u8) -> SoulseekHit {
        SoulseekHit {
            username: "alice".into(),
            name: name.into(),
            size: 1,
            slots,
            speed: 1,
            bitrate: None,
        }
    }

    #[test]
    fn format_filter_keeps_only_matching_extensions() {
        let mut ui = SoulseekUi::ready();
        ui.set_hits(vec![
            hit(r"album\a.flac", 1),
            hit(r"album\b.mp3", 1),
            hit(r"album\c.ogg", 0),
        ]);
        ui.set_format(SoulseekFormat::Flac);
        let rows = ui.visible_rows();
        let files: Vec<_> = rows
            .iter()
            .filter(|row| row.kind == SoulseekRowKind::File)
            .collect();
        assert_eq!(files.len(), 1);
        assert!(files[0].label.contains("a.flac"));
        ui.selected = 1;
        assert_eq!(ui.hits_in_scope(SoulseekScope::Folder).len(), 1);
        ui.toggle_free_slot();
        ui.set_format(SoulseekFormat::All);
        assert_eq!(
            ui.visible_rows()
                .iter()
                .filter(|row| row.kind == SoulseekRowKind::File)
                .count(),
            2
        );
    }

    #[test]
    fn folder_downloads_land_in_the_album_directory() {
        let dest = dest_for_remote_path(Path::new("/music"), r"shares\album\track.flac");
        assert_eq!(dest, PathBuf::from("/music/album"));
        assert_eq!(
            dest_for_remote_path(Path::new("/music"), "track.flac"),
            PathBuf::from("/music")
        );
    }
}
