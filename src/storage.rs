use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::{
    model::{
        PlaybackOrder, Playlist, PlaylistId, ReplayGainInfo, ReplayGainMode, ScannedTrack, Track,
        TrackId, TrackOrigin,
    },
    path_codec,
};

pub struct Store {
    connection: Connection,
}

#[derive(Clone, Debug)]
pub struct SavedState {
    pub active_playlist: usize,
    pub volume: f32,
    pub playback_order: PlaybackOrder,
    pub cursor_follows_playback: bool,
    pub replay_gain_mode: ReplayGainMode,
    pub replay_gain_preamp: f32,
    pub replay_gain_prevent_clip: bool,
    pub show_album_art: bool,
    pub show_spectrum: bool,
    pub nerd_font: bool,
    pub preferred_output_device: Option<String>,
}

impl Default for SavedState {
    fn default() -> Self {
        Self {
            active_playlist: 0,
            volume: 0.8,
            playback_order: PlaybackOrder::Default,
            cursor_follows_playback: true,
            replay_gain_mode: ReplayGainMode::Album,
            replay_gain_preamp: 0.0,
            replay_gain_prevent_clip: true,
            show_album_art: true,
            show_spectrum: true,
            nerd_font: false,
            preferred_output_device: None,
        }
    }
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating data directory {}", parent.display()))?;
        }
        let connection = Connection::open(path)
            .with_context(|| format!("opening database {}", path.display()))?;
        let mut store = Self { connection };
        store.migrate()?;
        Ok(store)
    }

    #[cfg(test)]
    pub fn in_memory() -> Result<Self> {
        let connection = Connection::open_in_memory()?;
        let mut store = Self { connection };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<()> {
        self.connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS tracks (
                id INTEGER PRIMARY KEY,
                path BLOB NOT NULL UNIQUE,
                title TEXT NOT NULL,
                artist TEXT NOT NULL,
                album TEXT NOT NULL,
                date INTEGER,
                track_number INTEGER,
                duration_ms INTEGER NOT NULL,
                codec TEXT NOT NULL,
                sample_rate INTEGER,
                channels INTEGER,
                file_size INTEGER NOT NULL,
                modified_ns INTEGER NOT NULL,
                unavailable INTEGER NOT NULL DEFAULT 0,
                scan_error TEXT
             );
             CREATE TABLE IF NOT EXISTS library_roots (
                path BLOB PRIMARY KEY
             );
             CREATE TABLE IF NOT EXISTS playlists (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                position INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS playlist_items (
                playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
                position INTEGER NOT NULL,
                track_id INTEGER NOT NULL REFERENCES tracks(id),
                PRIMARY KEY (playlist_id, position)
             );
             CREATE TABLE IF NOT EXISTS app_state (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
             );
             ",
        )?;
        let has_scan_error: bool = self.connection.query_row(
            "SELECT count(*) > 0 FROM pragma_table_info('tracks') WHERE name = 'scan_error'",
            [],
            |row| row.get(0),
        )?;
        if !has_scan_error {
            self.connection
                .execute("ALTER TABLE tracks ADD COLUMN scan_error TEXT", [])?;
        }
        add_column_if_missing(
            &self.connection,
            "tracks",
            "origin",
            "TEXT NOT NULL DEFAULT 'local'",
        )?;
        add_column_if_missing(&self.connection, "tracks", "remote_id", "TEXT")?;
        add_column_if_missing(&self.connection, "tracks", "server_fingerprint", "TEXT")?;
        add_column_if_missing(&self.connection, "tracks", "server_name", "TEXT")?;
        add_column_if_missing(&self.connection, "tracks", "rg_track_gain", "REAL")?;
        add_column_if_missing(&self.connection, "tracks", "rg_track_peak", "REAL")?;
        add_column_if_missing(&self.connection, "tracks", "rg_album_gain", "REAL")?;
        add_column_if_missing(&self.connection, "tracks", "rg_album_peak", "REAL")?;
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS pairing_tokens (
                token TEXT PRIMARY KEY,
                created_ns INTEGER NOT NULL
             );",
        )?;
        self.connection.pragma_update(None, "user_version", 3)?;
        Ok(())
    }

    pub fn load_tracks(&self) -> Result<BTreeMap<TrackId, Track>> {
        let mut statement = self.connection.prepare(
            "SELECT id, path, title, artist, album, date, track_number, duration_ms,
                    codec, sample_rate, channels, file_size, modified_ns, unavailable, scan_error,
                    origin, remote_id, server_fingerprint, server_name,
                    rg_track_gain, rg_track_peak, rg_album_gain, rg_album_peak
             FROM tracks ORDER BY artist COLLATE NOCASE, album COLLATE NOCASE, track_number, title COLLATE NOCASE",
        )?;
        let rows = statement.query_map([], |row| {
            let path_bytes: Vec<u8> = row.get(1)?;
            let path = path_codec::decode(&path_bytes);
            let origin = track_origin(
                row.get::<_, String>(15)?,
                row.get(16)?,
                row.get(17)?,
                row.get(18)?,
            );
            let marked_unavailable: bool = row.get::<_, i64>(13)? != 0;
            let unavailable = marked_unavailable || (!origin.is_remote() && !path.exists());
            Ok(Track {
                id: row.get(0)?,
                path,
                title: row.get(2)?,
                artist: row.get(3)?,
                album: row.get(4)?,
                date: row.get(5)?,
                track_number: row.get(6)?,
                duration: Duration::from_millis(row.get::<_, i64>(7)?.max(0) as u64),
                codec: row.get(8)?,
                sample_rate: row.get(9)?,
                channels: row.get(10)?,
                file_size: row.get::<_, i64>(11)?.max(0) as u64,
                modified_ns: row.get(12)?,
                unavailable,
                scan_error: row.get(14)?,
                origin,
                replay_gain: ReplayGainInfo {
                    track_gain: row.get(19)?,
                    track_peak: row.get(20)?,
                    album_gain: row.get(21)?,
                    album_peak: row.get(22)?,
                },
            })
        })?;
        Ok(rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|track| (track.id, track))
            .collect())
    }

    pub fn upsert_track(&mut self, track: &ScannedTrack) -> Result<Track> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO tracks
                (path, title, artist, album, date, track_number, duration_ms, codec,
                 sample_rate, channels, file_size, modified_ns, unavailable, scan_error,
                 rg_track_gain, rg_track_peak, rg_album_gain, rg_album_peak)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 0, ?13, ?14, ?15, ?16, ?17)
             ON CONFLICT(path) DO UPDATE SET
                title=excluded.title, artist=excluded.artist, album=excluded.album,
                date=excluded.date, track_number=excluded.track_number,
                duration_ms=excluded.duration_ms, codec=excluded.codec,
                sample_rate=excluded.sample_rate, channels=excluded.channels,
                file_size=excluded.file_size, modified_ns=excluded.modified_ns,
                unavailable=0, scan_error=excluded.scan_error,
                rg_track_gain=excluded.rg_track_gain, rg_track_peak=excluded.rg_track_peak,
                rg_album_gain=excluded.rg_album_gain, rg_album_peak=excluded.rg_album_peak",
            params![
                path_codec::encode(&track.path),
                track.title,
                track.artist,
                track.album,
                track.date,
                track.track_number,
                track.duration.as_millis().min(i64::MAX as u128) as i64,
                track.codec,
                track.sample_rate,
                track.channels,
                track.file_size.min(i64::MAX as u64) as i64,
                track.modified_ns,
                track.scan_error,
                track.replay_gain.track_gain,
                track.replay_gain.track_peak,
                track.replay_gain.album_gain,
                track.replay_gain.album_peak,
            ],
        )?;
        let id = transaction.query_row(
            "SELECT id FROM tracks WHERE path = ?1",
            params![path_codec::encode(&track.path)],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        Ok(Track {
            id,
            path: track.path.clone(),
            title: track.title.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            date: track.date,
            track_number: track.track_number,
            duration: track.duration,
            codec: track.codec.clone(),
            sample_rate: track.sample_rate,
            channels: track.channels,
            file_size: track.file_size,
            modified_ns: track.modified_ns,
            unavailable: false,
            scan_error: track.scan_error.clone(),
            origin: TrackOrigin::Local,
            replay_gain: track.replay_gain.clone(),
        })
    }

    pub fn update_replay_gain(
        &mut self,
        id: TrackId,
        info: &ReplayGainInfo,
        file_size: u64,
        modified_ns: i64,
    ) -> Result<()> {
        self.connection.execute(
            "UPDATE tracks SET rg_track_gain=?1, rg_track_peak=?2, rg_album_gain=?3,
                    rg_album_peak=?4, file_size=?5, modified_ns=?6
             WHERE id=?7",
            params![
                info.track_gain,
                info.track_peak,
                info.album_gain,
                info.album_peak,
                file_size.min(i64::MAX as u64) as i64,
                modified_ns,
                id,
            ],
        )?;
        Ok(())
    }

    pub fn upsert_remote_track(
        &mut self,
        fingerprint: &str,
        server_name: &str,
        track: &crate::net::protocol::CatalogTrack,
    ) -> Result<Track> {
        let path = remote_placeholder_path(fingerprint, &track.remote_id);
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO tracks
                (path, title, artist, album, date, track_number, duration_ms, codec,
                 sample_rate, channels, file_size, modified_ns, unavailable, scan_error,
                 origin, remote_id, server_fingerprint, server_name)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 0, NULL,
                     'remote', ?13, ?14, ?15)
             ON CONFLICT(path) DO UPDATE SET
                title=excluded.title, artist=excluded.artist, album=excluded.album,
                date=excluded.date, track_number=excluded.track_number,
                duration_ms=excluded.duration_ms, codec=excluded.codec,
                sample_rate=excluded.sample_rate, channels=excluded.channels,
                file_size=excluded.file_size, modified_ns=excluded.modified_ns,
                unavailable=0, scan_error=NULL,
                origin='remote', remote_id=excluded.remote_id,
                server_fingerprint=excluded.server_fingerprint,
                server_name=excluded.server_name",
            params![
                path_codec::encode(&path),
                track.title,
                track.artist,
                track.album,
                track.date,
                track.track_number,
                track.duration_ms.min(i64::MAX as u64) as i64,
                track.codec,
                track.sample_rate,
                track.channels,
                track.file_size.min(i64::MAX as u64) as i64,
                track.modified_ns,
                track.remote_id,
                fingerprint,
                server_name,
            ],
        )?;
        let id = transaction.query_row(
            "SELECT id FROM tracks WHERE path = ?1",
            params![path_codec::encode(&path)],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        Ok(Track {
            id,
            path,
            title: track.title.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            date: track.date,
            track_number: track.track_number,
            duration: Duration::from_millis(track.duration_ms),
            codec: track.codec.clone(),
            sample_rate: track.sample_rate,
            channels: track.channels,
            file_size: track.file_size,
            modified_ns: track.modified_ns,
            unavailable: false,
            scan_error: None,
            origin: TrackOrigin::Remote {
                fingerprint: fingerprint.to_owned(),
                remote_id: track.remote_id.clone(),
                server_name: server_name.to_owned(),
            },
            replay_gain: ReplayGainInfo::default(),
        })
    }

    pub fn prune_local_tracks(&mut self, keep: &[PathBuf]) -> Result<usize> {
        let keep: BTreeSet<Vec<u8>> = keep.iter().map(|path| path_codec::encode(path)).collect();
        let mut statement = self.connection.prepare(
            "SELECT id, path FROM tracks WHERE origin = 'local' OR origin IS NULL OR origin = ''",
        )?;
        let stale: Vec<TrackId> = statement
            .query_map([], |row| {
                let path: Vec<u8> = row.get(1)?;
                Ok((row.get::<_, TrackId>(0)?, path))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter(|(_, path)| !keep.contains(path))
            .map(|(id, _)| id)
            .collect();
        drop(statement);
        let transaction = self.connection.transaction()?;
        for id in &stale {
            transaction.execute("DELETE FROM playlist_items WHERE track_id = ?1", [*id])?;
            transaction.execute("DELETE FROM tracks WHERE id = ?1", [*id])?;
        }
        transaction.commit()?;
        Ok(stale.len())
    }

    pub fn remove_remote_tracks(&mut self, fingerprint: &str, remote_ids: &[String]) -> Result<()> {
        let transaction = self.connection.transaction()?;
        for remote_id in remote_ids {
            transaction.execute(
                "DELETE FROM playlist_items WHERE track_id IN (
                    SELECT id FROM tracks
                    WHERE origin = 'remote' AND server_fingerprint = ?1 AND remote_id = ?2
                )",
                params![fingerprint, remote_id],
            )?;
            transaction.execute(
                "DELETE FROM tracks WHERE origin = 'remote' AND server_fingerprint = ?1 AND remote_id = ?2",
                params![fingerprint, remote_id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn library_revision(&self) -> Result<u64> {
        Ok(self
            .connection
            .query_row(
                "SELECT value FROM app_state WHERE key = 'library_revision'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .and_then(|value| value.parse().ok())
            .unwrap_or(0))
    }

    pub fn bump_library_revision(&mut self) -> Result<u64> {
        let next = self.library_revision()?.saturating_add(1);
        let transaction = self.connection.transaction()?;
        set_state(&transaction, "library_revision", &next.to_string())?;
        transaction.commit()?;
        Ok(next)
    }

    pub fn add_pairing_token(&mut self, token: &str) -> Result<()> {
        let created = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
            .unwrap_or(0);
        self.connection.execute(
            "INSERT OR REPLACE INTO pairing_tokens(token, created_ns) VALUES (?1, ?2)",
            params![token, created],
        )?;
        Ok(())
    }

    pub fn pairing_token_exists(&self, token: &str) -> Result<bool> {
        Ok(self.connection.query_row(
            "SELECT count(*) FROM pairing_tokens WHERE token = ?1",
            [token],
            |row| row.get::<_, i64>(0),
        )? > 0)
    }

    pub fn clear_pairing_tokens(&mut self) -> Result<()> {
        self.connection.execute("DELETE FROM pairing_tokens", [])?;
        Ok(())
    }

    pub fn load_roots(&self) -> Result<Vec<PathBuf>> {
        let mut statement = self
            .connection
            .prepare("SELECT path FROM library_roots ORDER BY path")?;
        Ok(statement
            .query_map([], |row| row.get::<_, Vec<u8>>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|bytes| path_codec::decode(&bytes))
            .collect())
    }

    pub fn add_root(&mut self, path: &Path) -> Result<()> {
        self.connection.execute(
            "INSERT OR IGNORE INTO library_roots(path) VALUES (?1)",
            [path_codec::encode(path)],
        )?;
        Ok(())
    }

    pub fn load_playlists(&self) -> Result<Vec<Playlist>> {
        let mut statement = self
            .connection
            .prepare("SELECT id, name FROM playlists ORDER BY position")?;
        let mut playlists = Vec::new();
        for playlist in statement.query_map([], |row| {
            Ok((row.get::<_, PlaylistId>(0)?, row.get::<_, String>(1)?))
        })? {
            let (id, name) = playlist?;
            let mut item_statement = self.connection.prepare(
                "SELECT track_id FROM playlist_items WHERE playlist_id = ?1 ORDER BY position",
            )?;
            let items = item_statement
                .query_map([id], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<TrackId>>>()?;
            playlists.push(Playlist { id, name, items });
        }
        Ok(playlists)
    }

    pub fn create_playlist(&mut self, name: &str, position: usize) -> Result<Playlist> {
        self.connection.execute(
            "INSERT INTO playlists(name, position) VALUES (?1, ?2)",
            params![name, position as i64],
        )?;
        Ok(Playlist {
            id: self.connection.last_insert_rowid(),
            name: name.to_owned(),
            items: Vec::new(),
        })
    }

    pub fn rename_playlist(&mut self, id: PlaylistId, name: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE playlists SET name = ?1 WHERE id = ?2",
            params![name, id],
        )?;
        Ok(())
    }

    pub fn delete_playlist(&mut self, id: PlaylistId) -> Result<()> {
        self.connection
            .execute("DELETE FROM playlists WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn save_playlist_items(&mut self, playlist: &Playlist) -> Result<()> {
        let transaction = self.connection.transaction()?;
        replace_playlist_items(&transaction, playlist)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn save_playlist_positions(&mut self, playlists: &[Playlist]) -> Result<()> {
        let transaction = self.connection.transaction()?;
        for (position, playlist) in playlists.iter().enumerate() {
            transaction.execute(
                "UPDATE playlists SET position = ?1 WHERE id = ?2",
                params![position as i64, playlist.id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn load_state(&self) -> Result<SavedState> {
        let get = |key: &str| -> Result<Option<String>> {
            Ok(self
                .connection
                .query_row("SELECT value FROM app_state WHERE key = ?1", [key], |row| {
                    row.get(0)
                })
                .optional()?)
        };
        Ok(SavedState {
            active_playlist: get("active_playlist")?
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            volume: get("volume")?.and_then(|v| v.parse().ok()).unwrap_or(0.8),
            playback_order: PlaybackOrder::from_i64(
                get("playback_order")?
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0),
            ),
            cursor_follows_playback: get("cursor_follows_playback")?
                .map(|value| value != "0")
                .unwrap_or(true),
            replay_gain_mode: ReplayGainMode::from_i64(
                get("replay_gain_mode")?
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(2),
            ),
            replay_gain_preamp: get("replay_gain_preamp")?
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.0),
            replay_gain_prevent_clip: get("replay_gain_prevent_clip")?
                .map(|value| value != "0")
                .unwrap_or(true),
            show_album_art: get("show_album_art")?
                .map(|value| value != "0")
                .unwrap_or(true),
            show_spectrum: get("show_spectrum")?
                .map(|value| value != "0")
                .unwrap_or(true),
            nerd_font: get("nerd_font")?.map(|value| value != "0").unwrap_or(false),
            preferred_output_device: get("preferred_output_device")?
                .filter(|value| !value.is_empty()),
        })
    }

    pub fn save_state(&mut self, state: &SavedState) -> Result<()> {
        let transaction = self.connection.transaction()?;
        set_state(
            &transaction,
            "active_playlist",
            &state.active_playlist.to_string(),
        )?;
        set_state(&transaction, "volume", &state.volume.to_string())?;
        set_state(
            &transaction,
            "playback_order",
            &state.playback_order.as_i64().to_string(),
        )?;
        set_state(
            &transaction,
            "cursor_follows_playback",
            if state.cursor_follows_playback {
                "1"
            } else {
                "0"
            },
        )?;
        set_state(
            &transaction,
            "replay_gain_mode",
            &state.replay_gain_mode.as_i64().to_string(),
        )?;
        set_state(
            &transaction,
            "replay_gain_preamp",
            &state.replay_gain_preamp.to_string(),
        )?;
        set_state(
            &transaction,
            "replay_gain_prevent_clip",
            if state.replay_gain_prevent_clip {
                "1"
            } else {
                "0"
            },
        )?;
        set_state(
            &transaction,
            "show_album_art",
            if state.show_album_art { "1" } else { "0" },
        )?;
        set_state(
            &transaction,
            "show_spectrum",
            if state.show_spectrum { "1" } else { "0" },
        )?;
        set_state(
            &transaction,
            "nerd_font",
            if state.nerd_font { "1" } else { "0" },
        )?;
        set_state(
            &transaction,
            "preferred_output_device",
            state.preferred_output_device.as_deref().unwrap_or(""),
        )?;
        transaction.commit()?;
        Ok(())
    }
}

fn add_column_if_missing(
    connection: &Connection,
    table: &str,
    column: &str,
    declaration: &str,
) -> Result<()> {
    let exists: bool = connection.query_row(
        &format!("SELECT count(*) > 0 FROM pragma_table_info('{table}') WHERE name = ?1"),
        [column],
        |row| row.get(0),
    )?;
    if !exists {
        connection.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {declaration}"),
            [],
        )?;
    }
    Ok(())
}

fn track_origin(
    origin: String,
    remote_id: Option<String>,
    fingerprint: Option<String>,
    server_name: Option<String>,
) -> TrackOrigin {
    match (
        origin.as_str(),
        remote_id,
        fingerprint,
        server_name.unwrap_or_default(),
    ) {
        ("remote", Some(remote_id), Some(fingerprint), server_name) => TrackOrigin::Remote {
            fingerprint,
            remote_id,
            server_name,
        },
        _ => TrackOrigin::Local,
    }
}

pub fn remote_placeholder_path(fingerprint: &str, remote_id: &str) -> PathBuf {
    PathBuf::from(format!("staccato:{fingerprint}/{remote_id}"))
}

fn replace_playlist_items(transaction: &Transaction<'_>, playlist: &Playlist) -> Result<()> {
    transaction.execute(
        "DELETE FROM playlist_items WHERE playlist_id = ?1",
        [playlist.id],
    )?;
    for (position, track_id) in playlist.items.iter().enumerate() {
        transaction.execute(
            "INSERT INTO playlist_items(playlist_id, position, track_id) VALUES (?1, ?2, ?3)",
            params![playlist.id, position as i64, track_id],
        )?;
    }
    Ok(())
}

fn set_state(transaction: &Transaction<'_>, key: &str, value: &str) -> Result<()> {
    transaction.execute(
        "INSERT INTO app_state(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::fallback_title;

    fn scanned(path: PathBuf) -> ScannedTrack {
        ScannedTrack {
            title: fallback_title(&path),
            path,
            artist: "Artist".into(),
            album: "Album".into(),
            date: Some(2026),
            track_number: Some(1),
            duration: Duration::from_secs(42),
            codec: "FLAC".into(),
            sample_rate: Some(44_100),
            channels: Some(2),
            file_size: 10,
            modified_ns: 20,
            scan_error: None,
            replay_gain: ReplayGainInfo::default(),
        }
    }

    #[test]
    fn state_tracks_and_playlists_round_trip() -> Result<()> {
        let mut store = Store::in_memory()?;
        let track = store.upsert_track(&scanned(PathBuf::from("music/test.flac")))?;
        let mut playlist = store.create_playlist("Default", 0)?;
        playlist.items.push(track.id);
        store.save_playlist_items(&playlist)?;
        store.save_state(&SavedState {
            active_playlist: 0,
            volume: 0.42,
            playback_order: PlaybackOrder::Shuffle,
            cursor_follows_playback: true,
            replay_gain_mode: ReplayGainMode::Album,
            replay_gain_preamp: 0.0,
            replay_gain_prevent_clip: true,
            show_album_art: true,
            show_spectrum: true,
            nerd_font: false,
            preferred_output_device: Some("test-device".into()),
        })?;

        assert_eq!(store.load_tracks()?.len(), 1);
        assert_eq!(store.load_playlists()?, vec![playlist]);
        let state = store.load_state()?;
        assert_eq!(state.volume, 0.42);
        assert_eq!(state.playback_order, PlaybackOrder::Shuffle);
        assert_eq!(
            state.preferred_output_device.as_deref(),
            Some("test-device")
        );
        assert!(state.show_spectrum);
        assert!(!state.nerd_font);
        Ok(())
    }

    #[test]
    fn prune_local_tracks_drops_files_that_disappeared() -> Result<()> {
        let mut store = Store::in_memory()?;
        let keep = store.upsert_track(&scanned(PathBuf::from("music/keep.flac")))?;
        store.upsert_track(&scanned(PathBuf::from("music/gone.flac")))?;
        let mut playlist = store.create_playlist("Mix", 0)?;
        playlist.items = vec![keep.id];
        store.save_playlist_items(&playlist)?;

        let removed = store.prune_local_tracks(&[PathBuf::from("music/keep.flac")])?;
        assert_eq!(removed, 1);
        let tracks = store.load_tracks()?;
        assert_eq!(tracks.len(), 1);
        assert!(tracks.contains_key(&keep.id));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_round_trip() -> Result<()> {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let path = PathBuf::from(OsString::from_vec(b"bad-\xff.flac".to_vec()));
        let mut store = Store::in_memory()?;
        let track = store.upsert_track(&scanned(path.clone()))?;
        assert_eq!(track.path, path);
        assert_eq!(store.load_tracks()?.get(&track.id).unwrap().path, path);
        Ok(())
    }
}
