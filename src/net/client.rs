use std::{
    fs::{self, OpenOptions},
    io::Write,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use crossbeam_channel::{Receiver, Sender, unbounded};
use quinn::{ClientConfig, Endpoint};

use crate::net::{
    protocol::{self, CatalogTrack, Message},
    tls,
};

const CLIENT_NAME: &str = "staccato";

#[derive(Debug, Clone)]
pub struct DiscoveredServer {
    pub name: String,
    pub address: SocketAddr,
}

#[derive(Debug)]
pub enum RemoteEvent {
    Connected {
        server_name: String,
        fingerprint: String,
        revision: u64,
        pair_required: bool,
    },
    Paired {
        token: String,
        fingerprint: String,
    },
    Catalog {
        revision: u64,
        tracks: Vec<CatalogTrack>,
        removed: Vec<String>,
        replace: bool,
    },
    MediaReady {
        remote_id: String,
    },
    Cover {
        remote_id: String,
        data: Option<Vec<u8>>,
    },
    Discovered(Vec<DiscoveredServer>),
    Error(String),
    Disconnected,
}

pub enum RemoteCommand {
    Pair(String),
    Fetch {
        remote_id: String,
        file_size: u64,
        etag: String,
    },
    FetchCover {
        remote_id: String,
    },
    Disconnect,
    Rescan,
}

pub struct RemoteHandle {
    pub events: Receiver<RemoteEvent>,
    commands: Sender<RemoteCommand>,
}

impl RemoteHandle {
    pub fn pair(&self, code: String) {
        let _ = self.commands.send(RemoteCommand::Pair(code));
    }

    pub fn fetch(&self, remote_id: String, file_size: u64, etag: String) {
        let _ = self.commands.send(RemoteCommand::Fetch {
            remote_id,
            file_size,
            etag,
        });
    }

    pub fn fetch_cover(&self, remote_id: String) {
        let _ = self.commands.send(RemoteCommand::FetchCover { remote_id });
    }

    pub fn disconnect(&self) {
        let _ = self.commands.send(RemoteCommand::Disconnect);
    }

    pub fn rescan(&self) {
        let _ = self.commands.send(RemoteCommand::Rescan);
    }
}

pub fn connect(
    address: SocketAddr,
    cache_dir: PathBuf,
    token: Option<String>,
    pinned_fingerprint: Option<String>,
) -> RemoteHandle {
    let (event_tx, event_rx) = unbounded();
    let (command_tx, command_rx) = unbounded();
    thread::spawn(move || {
        tracing::info!(%address, "remote session thread starting");
        let runtime = match tokio::runtime::Runtime::new() {
            Ok(runtime) => runtime,
            Err(error) => {
                tracing::error!(%error, "tokio runtime failed");
                let _ = event_tx.send(RemoteEvent::Error(error.to_string()));
                return;
            }
        };
        if let Err(error) = runtime.block_on(session_loop(
            address,
            cache_dir,
            token,
            pinned_fingerprint,
            event_tx.clone(),
            command_rx,
        )) {
            tracing::error!(error = %error, "session_loop returned error");
            let _ = event_tx.send(RemoteEvent::Error(error.to_string()));
        }
        tracing::info!("remote session thread ending");
        let _ = event_tx.send(RemoteEvent::Disconnected);
    });
    RemoteHandle {
        events: event_rx,
        commands: command_tx,
    }
}

pub fn browse_mdns() -> RemoteHandle {
    let (event_tx, event_rx) = unbounded();
    let (command_tx, _command_rx) = unbounded();
    thread::spawn(move || {
        if let Err(error) = browse_mdns_inner(event_tx.clone()) {
            let _ = event_tx.send(RemoteEvent::Error(error.to_string()));
        }
    });
    RemoteHandle {
        events: event_rx,
        commands: command_tx,
    }
}

fn browse_mdns_inner(events: Sender<RemoteEvent>) -> Result<()> {
    let daemon = mdns_sd::ServiceDaemon::new().context("starting mDNS browser")?;
    let receiver = daemon
        .browse("_staccato._udp.local.")
        .context("browsing for Staccato servers")?;
    let mut found = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        match receiver.recv_timeout(Duration::from_millis(200)) {
            Ok(mdns_sd::ServiceEvent::ServiceResolved(info)) => {
                for address in info.get_addresses() {
                    found.push(DiscoveredServer {
                        name: info
                            .get_fullname()
                            .trim_end_matches("._staccato._udp.local.")
                            .to_owned(),
                        address: SocketAddr::new(*address, info.get_port()),
                    });
                }
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name).then(a.address.cmp(&b.address)));
    found.dedup_by(|a, b| a.address == b.address);
    let _ = events.send(RemoteEvent::Discovered(found));
    Ok(())
}

pub fn connect_once(
    address: SocketAddr,
    code: Option<String>,
    cache_dir: Option<PathBuf>,
    token: Option<String>,
) -> Result<ConnectReport> {
    let runtime = tokio::runtime::Runtime::new().context("starting async runtime")?;
    runtime.block_on(connect_once_async(address, code, cache_dir, token))
}

pub struct ConnectReport {
    pub server_name: String,
    pub fingerprint: String,
    pub revision: u64,
    pub tracks: usize,
    pub token: Option<String>,
}

async fn connect_once_async(
    address: SocketAddr,
    code: Option<String>,
    cache_dir: Option<PathBuf>,
    token: Option<String>,
) -> Result<ConnectReport> {
    let (endpoint, connection, verifier) =
        open_connection(crate::net::normalize_server_addr(address), None).await?;
    let fingerprint = verifier
        .fingerprint()
        .ok_or_else(|| anyhow!("server did not present a certificate"))?;
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .context("opening control stream")?;
    protocol::write_message(
        &mut send,
        &Message::Hello {
            client_name: CLIENT_NAME.into(),
            token: token.clone(),
        },
    )
    .await?;
    let Message::Welcome {
        server_name,
        library_revision,
        pair_required,
        fingerprint: announced,
    } = protocol::read_message(&mut recv).await?
    else {
        bail!("server did not send welcome");
    };
    if announced != fingerprint {
        bail!("welcome fingerprint does not match TLS certificate");
    }
    let mut issued_token = token;
    if pair_required {
        let Some(code) = code else {
            bail!("server requires pairing; pass --code from the server terminal");
        };
        protocol::write_message(&mut send, &Message::Pair { code }).await?;
        match protocol::read_message(&mut recv).await? {
            Message::Paired { token } => issued_token = Some(token),
            Message::Error { message, .. } => bail!(message),
            other => bail!("unexpected pairing reply: {other:?}"),
        }
    }
    protocol::write_message(
        &mut send,
        &Message::GetCatalog {
            since_revision: None,
        },
    )
    .await?;
    let catalog_tracks = match protocol::read_message(&mut recv).await? {
        Message::Catalog { tracks, .. } => tracks,
        Message::Error { message, .. } => bail!(message),
        other => bail!("unexpected catalog reply: {other:?}"),
    };
    let tracks = catalog_tracks.len();
    if let (Some(cache_dir), Some(first)) = (cache_dir, catalog_tracks.first()) {
        fetch_media(
            connection.clone(),
            cache_dir,
            fingerprint.clone(),
            first.remote_id.clone(),
            first.file_size,
            format!("{}-{}", first.modified_ns, first.file_size),
        )
        .await?;
    }
    connection.close(0u32.into(), b"done");
    endpoint.wait_idle().await;
    Ok(ConnectReport {
        server_name,
        fingerprint,
        revision: library_revision,
        tracks,
        token: issued_token,
    })
}

async fn session_loop(
    address: SocketAddr,
    cache_dir: PathBuf,
    token: Option<String>,
    pinned: Option<String>,
    events: Sender<RemoteEvent>,
    commands: Receiver<RemoteCommand>,
) -> Result<()> {
    let (endpoint, connection, verifier) =
        open_connection(crate::net::normalize_server_addr(address), pinned).await?;
    let fingerprint = verifier
        .fingerprint()
        .ok_or_else(|| anyhow!("server did not present a certificate"))?;
    let (mut send, mut recv) = connection.open_bi().await?;
    protocol::write_message(
        &mut send,
        &Message::Hello {
            client_name: CLIENT_NAME.into(),
            token: token.clone(),
        },
    )
    .await?;
    let Message::Welcome {
        server_name,
        library_revision,
        pair_required,
        fingerprint: announced,
    } = protocol::read_message(&mut recv).await?
    else {
        bail!("server did not send welcome");
    };
    if announced != fingerprint {
        bail!("welcome fingerprint does not match TLS certificate");
    }
    tracing::info!(
        %address,
        %server_name,
        pair_required,
        revision = library_revision,
        "welcome received"
    );
    let _ = events.send(RemoteEvent::Connected {
        server_name: server_name.clone(),
        fingerprint: fingerprint.clone(),
        revision: library_revision,
        pair_required,
    });
    if !pair_required {
        protocol::write_message(
            &mut send,
            &Message::GetCatalog {
                since_revision: None,
            },
        )
        .await?;
    }

    loop {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(40)) => {
                match commands.try_recv() {
                    Ok(RemoteCommand::Pair(code)) => {
                        protocol::write_message(&mut send, &Message::Pair { code }).await?;
                    }
                    Ok(RemoteCommand::Fetch { remote_id, file_size, etag }) => {
                        let connection = connection.clone();
                        let cache_dir = cache_dir.clone();
                        let fingerprint = fingerprint.clone();
                        let events = events.clone();
                        tokio::spawn(async move {
                            match fetch_media(
                                connection,
                                cache_dir,
                                fingerprint,
                                remote_id.clone(),
                                file_size,
                                etag,
                            )
                            .await
                            {
                                Ok(_path) => {
                                    let _ = events.send(RemoteEvent::MediaReady { remote_id });
                                }
                                Err(error) => {
                                    let _ = events.send(RemoteEvent::Error(error.to_string()));
                                }
                            }
                        });
                    }
                    Ok(RemoteCommand::FetchCover { remote_id }) => {
                        let connection = connection.clone();
                        let events = events.clone();
                        tokio::spawn(async move {
                            match fetch_cover(connection, remote_id.clone()).await {
                                Ok(data) => {
                                    let _ = events.send(RemoteEvent::Cover { remote_id, data });
                                }
                                Err(error) => {
                                    tracing::warn!(%remote_id, error = %error, "cover fetch failed");
                                    let _ = events.send(RemoteEvent::Cover {
                                        remote_id,
                                        data: None,
                                    });
                                }
                            }
                        });
                    }
                    Ok(RemoteCommand::Rescan) => {
                        tracing::info!("requesting server rescan");
                        protocol::write_message(&mut send, &Message::Rescan).await?;
                    }
                    Ok(RemoteCommand::Disconnect) => {
                        tracing::info!("client requested disconnect");
                        break;
                    }
                    Err(crossbeam_channel::TryRecvError::Disconnected) => {
                        tracing::warn!("command channel dropped — TUI handle gone");
                        break;
                    }
                    Err(crossbeam_channel::TryRecvError::Empty) => {}
                }
            }
            message = protocol::read_message(&mut recv) => {
                match message {
                    Ok(Message::Paired { token }) => {
                        let _ = events.send(RemoteEvent::Paired {
                            token,
                            fingerprint: fingerprint.clone(),
                        });
                        protocol::write_message(
                            &mut send,
                            &Message::GetCatalog { since_revision: None },
                        )
                        .await?;
                    }
                    Ok(Message::Catalog {
                        revision,
                        tracks,
                        removed,
                        replace,
                    }) => {
                        let _ = events.send(RemoteEvent::Catalog {
                            revision,
                            tracks,
                            removed,
                            replace,
                        });
                    }
                    Ok(Message::LibraryChanged { .. }) => {
                        protocol::write_message(
                            &mut send,
                            &Message::GetCatalog { since_revision: None },
                        )
                        .await?;
                    }
                    Ok(Message::Pong) => {
                        tracing::debug!("pong");
                    }
                    Ok(Message::Error { message, .. }) => {
                        tracing::error!(%message, "server error");
                        let _ = events.send(RemoteEvent::Error(message));
                    }
                    Ok(other) => {
                        tracing::debug!(kind = ?other, "ignored control message");
                    }
                    Err(error) => {
                        tracing::error!(
                            error = %error,
                            "control stream read failed — this is why the TUI disconnects"
                        );
                        let _ = events.send(RemoteEvent::Error(format!(
                            "connection dropped: {error:#}"
                        )));
                        break;
                    }
                }
            }
        }
    }
    tracing::info!("session_loop exiting, closing QUIC");
    connection.close(0u32.into(), b"bye");
    endpoint.wait_idle().await;
    Ok(())
}

async fn fetch_media(
    connection: quinn::Connection,
    cache_dir: PathBuf,
    fingerprint: String,
    remote_id: String,
    file_size: u64,
    expected_etag: String,
) -> Result<PathBuf> {
    tracing::info!(%remote_id, file_size, %expected_etag, "fetch_media start");
    let path = cache_path(&cache_dir, &fingerprint, &remote_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let etag_path = path.with_extension("etag");
    if path.exists()
        && fs::metadata(&path)?.len() == file_size
        && fs::read_to_string(&etag_path).unwrap_or_default() == expected_etag
    {
        return Ok(path);
    }

    let (mut send, mut recv) = connection.open_bi().await?;
    protocol::write_message(
        &mut send,
        &Message::OpenMedia {
            remote_id: remote_id.clone(),
            offset: 0,
            length: None,
        },
    )
    .await?;
    match protocol::read_message(&mut recv).await? {
        Message::MediaHeader { size, etag, .. } => {
            if !expected_etag.is_empty() && etag != expected_etag {
                bail!("etag mismatch for {remote_id}");
            }
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&path)?;
            let mut remaining = size;
            let mut buffer = vec![0u8; 64 * 1024];
            while remaining > 0 {
                let want = remaining.min(buffer.len() as u64) as usize;
                match recv.read(&mut buffer[..want]).await? {
                    Some(0) | None => break,
                    Some(read) => {
                        file.write_all(&buffer[..read])?;
                        remaining -= read as u64;
                    }
                }
            }
            file.flush()?;
            fs::write(&etag_path, etag)?;
            tracing::info!(%remote_id, path = %path.display(), "fetch_media complete");
            Ok(path)
        }
        Message::Error { message, .. } => bail!(message),
        other => bail!("unexpected media reply: {other:?}"),
    }
}

async fn fetch_cover(connection: quinn::Connection, remote_id: String) -> Result<Option<Vec<u8>>> {
    let (mut send, mut recv) = connection.open_bi().await?;
    protocol::write_message(
        &mut send,
        &Message::GetCover {
            remote_id: remote_id.clone(),
        },
    )
    .await?;
    match protocol::read_message(&mut recv).await? {
        Message::Cover { data, .. } => {
            Ok(data.and_then(|data| crate::cover::decode_cover_data(&data)))
        }
        Message::Error { message, .. } => {
            tracing::debug!(%remote_id, %message, "server has no cover");
            Ok(None)
        }
        other => bail!("unexpected cover reply: {other:?}"),
    }
}

pub fn cache_path(cache_dir: &Path, fingerprint: &str, remote_id: &str) -> PathBuf {
    cache_dir.join(fingerprint).join(remote_id)
}

pub fn cache_is_complete(path: &Path, file_size: u64) -> bool {
    fs::metadata(path)
        .ok()
        .is_some_and(|meta| meta.len() == file_size)
}

async fn open_connection(
    address: SocketAddr,
    pinned: Option<String>,
) -> Result<(Endpoint, quinn::Connection, Arc<tls::TofuVerifier>)> {
    tls::install_provider();
    let address = crate::net::normalize_server_addr(address);
    let (crypto, verifier) = tls::client_crypto(pinned)?;
    let mut endpoint =
        Endpoint::client(crate::net::client_bind_addr(address)).context("creating QUIC client")?;
    let mut client_config = ClientConfig::new(Arc::new(crypto));
    client_config.transport_config(crate::net::quic_transport());
    endpoint.set_default_client_config(client_config);
    tracing::info!(%address, "opening QUIC connection");
    let connection = endpoint
        .connect(address, "localhost")
        .with_context(|| format!("starting QUIC handshake with {address}"))?
        .await
        .with_context(|| {
            format!(
                "QUIC handshake with {address} timed out. \
                 Use the Pi's LAN IP (for example 192.168.178.70:{}), not 0.0.0.0. \
                 The protocol is UDP — check that UDP {} is allowed on the Pi.",
                address.port(),
                address.port()
            )
        })?;
    tracing::info!(
        %address,
        rtt = ?connection.rtt(),
        "QUIC handshake complete"
    );
    Ok((endpoint, connection, verifier))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_paths_are_per_server_and_track() {
        let dir = PathBuf::from("/tmp/cache");
        assert_ne!(
            cache_path(&dir, "aaa", "track-1"),
            cache_path(&dir, "bbb", "track-1")
        );
    }
}
