use std::collections::BTreeSet;
use std::net::ToSocketAddrs;

use anyhow::{Context, Result, anyhow};

use crate::{
    model::{TrackId, TrackOrigin},
    net::{self, RemoteEvent, connect},
};

use super::{App, overlay::Overlay};

impl App {
    pub(crate) fn begin_connect(&mut self) {
        tracing::info!("user opened connect overlay");
        self.discovery = Some(net::browse_mdns());
        self.overlay = Overlay::Connect {
            text: String::new(),
            discovered: Vec::new(),
        };
    }

    pub(crate) fn drain_remote_events(&mut self) {
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
            RemoteEvent::Cover { remote_id, data } => {
                if let Some(bytes) = data
                    && let Some(track) = self.tracks.values().find(|track| match &track.origin {
                        TrackOrigin::Remote { remote_id: id, .. } => id == &remote_id,
                        TrackOrigin::Local => false,
                    })
                    && let TrackOrigin::Remote { fingerprint, .. } = &track.origin
                {
                    let path = crate::cover::album_cache_path(
                        &self.cache_dir(),
                        fingerprint,
                        &track.artist,
                        &track.album,
                    );
                    if let Err(error) = crate::cover::save_album_cover(&path, &bytes) {
                        tracing::warn!(error = %error, "could not cache album art");
                    } else {
                        self.covers.invalidate();
                    }
                }
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

    pub(crate) fn submit_connect(&mut self) -> Result<()> {
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

    pub(crate) fn submit_pair(&mut self) {
        let Overlay::Pair { text } = &self.overlay else {
            return;
        };
        let code = text.trim().to_owned();
        if let Some(remote) = &self.remote {
            remote.pair(code);
            self.status = "Sending pairing code…".into();
        }
    }

    pub(crate) fn disconnect_remote(&mut self) {
        if let Some(remote) = &self.remote {
            remote.disconnect();
        }
        self.remote = None;
        self.pending_play = None;
        self.status = "Disconnected from server".into();
    }

    pub(crate) fn prefetch_neighbors(&self, playlist_index: usize, item_index: usize) {
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
                let cached = crate::net::cache_path(&self.cache_dir(), fingerprint, remote_id);
                if !crate::net::cache_is_complete(&cached, track.file_size) {
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
