use std::{
    collections::HashMap,
    fs::File,
    io::Read,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, bail};
use quinn::Endpoint;
use rand::Rng;
use sha2::{Digest, Sha256};

use crate::{
    library::{collect_audio_files, scan_file},
    model::{ScannedTrack, fallback_title},
    net::{
        protocol::{self, CatalogTrack, Message, duration_ms},
        tls,
    },
    path_codec,
    storage::Store,
};

pub const SERVER_DB_NAME: &str = "staccato-server.db";

pub struct ServeOptions {
    pub data_dir: PathBuf,
    pub roots: Vec<PathBuf>,
    pub bind: SocketAddr,
    pub reset_pairing: bool,
    pub advertise: bool,
    pub server_name: Option<String>,
}

pub struct IndexStats {
    pub discovered: usize,
    pub failed: usize,
    pub revision: u64,
}

struct ServerState {
    store: Mutex<Store>,
    roots: Vec<PathBuf>,
    pairing_code: Mutex<String>,
    server_name: String,
    fingerprint: String,
    authorized: Mutex<HashMap<usize, bool>>,
}

pub fn run(options: ServeOptions) -> Result<()> {
    let runtime = tokio::runtime::Runtime::new().context("starting async runtime")?;
    runtime.block_on(run_async(options))
}

pub fn index_roots(store: &mut Store, roots: &[PathBuf]) -> Result<IndexStats> {
    for root in roots {
        store.add_root(root)?;
    }
    let mut discovered = 0;
    let mut failed = 0;
    let files = collect_audio_files(roots);
    for path in &files {
        match scan_or_fallback(path) {
            Ok(track) => {
                let unreadable = track.scan_error.is_some();
                store.upsert_track(&track)?;
                if unreadable {
                    failed += 1;
                } else {
                    discovered += 1;
                }
            }
            Err(_) => failed += 1,
        }
    }
    store.prune_local_tracks(&files)?;
    let revision = store.bump_library_revision()?;
    Ok(IndexStats {
        discovered,
        failed,
        revision,
    })
}

async fn run_async(options: ServeOptions) -> Result<()> {
    tls::require_dir(&options.data_dir)?;
    let database = options.data_dir.join(SERVER_DB_NAME);
    let mut store = Store::open(&database)?;
    if options.reset_pairing {
        store.clear_pairing_tokens()?;
        println!("Cleared pairing tokens.");
    }

    let roots = if options.roots.is_empty() {
        let existing = store.load_roots()?;
        if existing.is_empty() {
            bail!("staccato serve requires at least one music folder");
        }
        println!("Reusing {} saved library folder(s).", existing.len());
        existing
    } else {
        options.roots
    };
    print_index(&index_roots(&mut store, &roots)?);

    let (cert, key) = tls::load_or_generate_server_cert(&options.data_dir)?;
    let fingerprint = tls::fingerprint(&cert);
    let mut server_config =
        quinn::ServerConfig::with_crypto(Arc::new(tls::server_crypto(cert, key)?));
    server_config.transport_config(crate::net::quic_transport());
    tracing::info!(
        bind = %options.bind,
        "QUIC endpoint starting (idle timeout disabled, no keep-alives)"
    );
    let endpoint = Endpoint::server(server_config, options.bind)
        .with_context(|| format!("listening on {}", options.bind))?;
    let local = endpoint.local_addr()?;
    tracing::info!(%local, fingerprint = %fingerprint, "QUIC endpoint bound");

    let pairing_code = generate_pairing_code();
    let server_name = options.server_name.unwrap_or_else(default_server_name);
    println!(
        "Staccato server `{server_name}` listening on UDP {}",
        local.port()
    );
    println!("Certificate fingerprint: {fingerprint}");
    println!("Pairing code: {pairing_code}");
    print_connect_hints(local, &pairing_code);

    let _mdns = if options.advertise {
        match advertise_mdns(&server_name, local.port()) {
            Ok(daemon) => Some(daemon),
            Err(error) => {
                eprintln!("mDNS advertisement unavailable: {error:#}");
                None
            }
        }
    } else {
        None
    };

    let state = Arc::new(ServerState {
        store: Mutex::new(store),
        roots,
        pairing_code: Mutex::new(pairing_code),
        server_name,
        fingerprint,
        authorized: Mutex::new(HashMap::new()),
    });

    loop {
        tokio::select! {
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else {
                    tracing::warn!("endpoint accept() returned None — listener closed");
                    break;
                };
                let remote = incoming.remote_address();
                tracing::info!(%remote, "incoming QUIC handshake");
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    match accept_connection(incoming, state).await {
                        Ok(()) => tracing::info!(%remote, "connection task finished cleanly"),
                        Err(error) => {
                            tracing::error!(%remote, error = %error, "connection task failed");
                            eprintln!("connection error: {error:#}");
                        }
                    }
                });
            }
            _ = tokio::signal::ctrl_c() => {
                println!("Stopping server.");
                break;
            }
        }
    }
    endpoint.close(0u32.into(), b"shutdown");
    Ok(())
}

async fn accept_connection(incoming: quinn::Incoming, state: Arc<ServerState>) -> Result<()> {
    let remote = incoming.remote_address();
    let connection = incoming.await.context("accepting QUIC connection")?;
    let conn_id = connection.stable_id();
    tracing::info!(
        %remote,
        conn_id,
        handshake_rtt = ?connection.rtt(),
        "handshake complete"
    );
    loop {
        let (send, recv) = match connection.accept_bi().await {
            Ok(streams) => {
                tracing::debug!(conn_id, "accepted bidi stream");
                streams
            }
            Err(error) => {
                tracing::warn!(
                    conn_id,
                    %remote,
                    error = ?error,
                    "accept_bi ended — this is why the session dropped"
                );
                eprintln!("connection {conn_id} from {remote} ended: {error:?}");
                return Ok(());
            }
        };
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            if let Err(error) = handle_stream(conn_id, send, recv, state).await {
                tracing::error!(conn_id, error = %error, "stream handler failed");
                eprintln!("stream error: {error:#}");
            }
        });
    }
}

async fn handle_stream(
    conn_id: usize,
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    state: Arc<ServerState>,
) -> Result<()> {
    let first = match protocol::read_message(&mut recv).await {
        Ok(message) => message,
        Err(error) => {
            let _ = protocol::write_message(
                &mut send,
                &Message::Error {
                    code: "bad_message".into(),
                    message: error.to_string(),
                },
            )
            .await;
            return Ok(());
        }
    };
    match first {
        Message::Hello { client_name, token } => {
            handle_control(conn_id, client_name, token, send, recv, state).await
        }
        Message::OpenMedia {
            remote_id,
            offset,
            length,
        } => handle_media(conn_id, remote_id, offset, length, send, state).await,
        Message::GetCover { remote_id } => handle_cover(conn_id, remote_id, send, state).await,
        other => {
            protocol::write_message(
                &mut send,
                &Message::Error {
                    code: "unexpected".into(),
                    message: format!("first message cannot be {other:?}"),
                },
            )
            .await
        }
    }
}

async fn handle_control(
    conn_id: usize,
    client_name: String,
    token: Option<String>,
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    state: Arc<ServerState>,
) -> Result<()> {
    let authorized = token
        .as_deref()
        .map(|token| {
            state
                .store
                .lock()
                .expect("store")
                .pairing_token_exists(token)
        })
        .transpose()?
        .unwrap_or(false);
    set_authorized(&state, conn_id, authorized);
    tracing::info!(
        conn_id,
        %client_name,
        authorized,
        has_token = token.is_some(),
        "hello"
    );
    let revision = state
        .store
        .lock()
        .expect("store")
        .library_revision()
        .unwrap_or(0);
    protocol::write_message(
        &mut send,
        &Message::Welcome {
            server_name: state.server_name.clone(),
            library_revision: revision,
            pair_required: !authorized,
            fingerprint: state.fingerprint.clone(),
        },
    )
    .await?;

    loop {
        let message = match protocol::read_message(&mut recv).await {
            Ok(message) => message,
            Err(error) => {
                tracing::warn!(
                    conn_id,
                    error = %error,
                    "control stream read failed — session is over"
                );
                break;
            }
        };
        tracing::debug!(conn_id, message = ?message_kind(&message), "control message");
        match message {
            Message::Pair { code } => {
                let expected = state.pairing_code.lock().expect("code").clone();
                if code != expected {
                    protocol::write_message(
                        &mut send,
                        &Message::Error {
                            code: "bad_pair".into(),
                            message: "pairing code does not match".into(),
                        },
                    )
                    .await?;
                    continue;
                }
                let token = generate_token();
                state
                    .store
                    .lock()
                    .expect("store")
                    .add_pairing_token(&token)?;
                set_authorized(&state, conn_id, true);
                tracing::info!(conn_id, "paired");
                protocol::write_message(&mut send, &Message::Paired { token }).await?;
            }
            Message::GetCatalog { since_revision } => {
                if !is_authorized(&state, conn_id) {
                    protocol::write_message(
                        &mut send,
                        &Message::Error {
                            code: "unpaired".into(),
                            message: "pair this client before reading the catalog".into(),
                        },
                    )
                    .await?;
                    continue;
                }
                let catalog = catalog_snapshot(&state, since_revision)?;
                if let Message::Catalog {
                    revision, tracks, ..
                } = &catalog
                {
                    tracing::info!(
                        conn_id,
                        revision,
                        tracks = tracks.len(),
                        ?since_revision,
                        "sending catalog"
                    );
                }
                protocol::write_message(&mut send, &catalog).await?;
            }
            Message::Rescan => {
                if !is_authorized(&state, conn_id) {
                    protocol::write_message(
                        &mut send,
                        &Message::Error {
                            code: "unpaired".into(),
                            message: "pair this client before rescanning".into(),
                        },
                    )
                    .await?;
                    continue;
                }
                tracing::info!(conn_id, "rescan requested");
                let roots = state.roots.clone();
                let scan_state = Arc::clone(&state);
                let stats = tokio::task::spawn_blocking(move || {
                    let mut store = scan_state.store.lock().expect("store");
                    index_roots(&mut store, &roots)
                })
                .await
                .context("joining rescan task")??;
                tracing::info!(
                    conn_id,
                    discovered = stats.discovered,
                    failed = stats.failed,
                    revision = stats.revision,
                    "rescan finished"
                );
                let catalog = catalog_snapshot(&state, None)?;
                protocol::write_message(&mut send, &catalog).await?;
            }
            Message::Ping => {
                protocol::write_message(&mut send, &Message::Pong).await?;
            }
            Message::OpenMedia {
                remote_id,
                offset,
                length,
            } => {
                handle_media(conn_id, remote_id, offset, length, send, state).await?;
                return Ok(());
            }
            other => {
                protocol::write_message(
                    &mut send,
                    &Message::Error {
                        code: "unknown_type".into(),
                        message: format!("unsupported message on control stream: {other:?}"),
                    },
                )
                .await?;
            }
        }
    }
    tracing::info!(conn_id, "control loop exited");
    Ok(())
}

fn message_kind(message: &Message) -> &'static str {
    match message {
        Message::Hello { .. } => "hello",
        Message::Welcome { .. } => "welcome",
        Message::Pair { .. } => "pair",
        Message::Paired { .. } => "paired",
        Message::GetCatalog { .. } => "get_catalog",
        Message::Rescan => "rescan",
        Message::Catalog { .. } => "catalog",
        Message::OpenMedia { .. } => "open_media",
        Message::MediaHeader { .. } => "media_header",
        Message::GetCover { .. } => "get_cover",
        Message::Cover { .. } => "cover",
        Message::Ping => "ping",
        Message::Pong => "pong",
        Message::LibraryChanged { .. } => "library_changed",
        Message::Error { .. } => "error",
    }
}

async fn handle_cover(
    conn_id: usize,
    remote_id: String,
    mut send: quinn::SendStream,
    state: Arc<ServerState>,
) -> Result<()> {
    tracing::info!(conn_id, %remote_id, "get_cover");
    if !is_authorized(&state, conn_id) {
        protocol::write_message(
            &mut send,
            &Message::Error {
                code: "unpaired".into(),
                message: "pair this client before streaming".into(),
            },
        )
        .await?;
        return Ok(());
    }
    let track = {
        let store = state.store.lock().expect("store");
        store
            .load_tracks()?
            .into_values()
            .find(|track| remote_id_for(&track.path) == remote_id)
    };
    let Some(track) = track else {
        protocol::write_message(
            &mut send,
            &Message::Cover {
                remote_id,
                data: None,
            },
        )
        .await?;
        return Ok(());
    };
    if !path_allowed(&track.path, &state.roots) {
        protocol::write_message(
            &mut send,
            &Message::Error {
                code: "forbidden".into(),
                message: "track is outside the configured library".into(),
            },
        )
        .await?;
        return Ok(());
    }
    let data = crate::cover::extract_cover_bytes(&track.path)
        .as_deref()
        .map(crate::cover::encode_cover_data);
    protocol::write_message(&mut send, &Message::Cover { remote_id, data }).await?;
    Ok(())
}

async fn handle_media(
    conn_id: usize,
    remote_id: String,
    offset: u64,
    length: Option<u64>,
    mut send: quinn::SendStream,
    state: Arc<ServerState>,
) -> Result<()> {
    tracing::info!(conn_id, %remote_id, offset, ?length, "open_media");
    if !is_authorized(&state, conn_id) {
        protocol::write_message(
            &mut send,
            &Message::Error {
                code: "unpaired".into(),
                message: "pair this client before streaming".into(),
            },
        )
        .await?;
        return Ok(());
    }
    let track = {
        let store = state.store.lock().expect("store");
        store
            .load_tracks()?
            .into_values()
            .find(|track| remote_id_for(&track.path) == remote_id)
    };
    let Some(track) = track else {
        protocol::write_message(
            &mut send,
            &Message::Error {
                code: "not_found".into(),
                message: format!("unknown track {remote_id}"),
            },
        )
        .await?;
        return Ok(());
    };
    if !path_allowed(&track.path, &state.roots) {
        protocol::write_message(
            &mut send,
            &Message::Error {
                code: "forbidden".into(),
                message: "track is outside the configured library".into(),
            },
        )
        .await?;
        return Ok(());
    }
    let etag = format!("{}-{}", track.modified_ns, track.file_size);
    protocol::write_message(
        &mut send,
        &Message::MediaHeader {
            size: track.file_size,
            etag,
            codec: track.codec.clone(),
        },
    )
    .await?;

    let mut file =
        File::open(&track.path).with_context(|| format!("opening {}", track.path.display()))?;
    if offset > 0 {
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(offset))
            .with_context(|| format!("seeking {}", track.path.display()))?;
    }
    let mut remaining = length.unwrap_or(track.file_size.saturating_sub(offset));
    let mut buffer = vec![0u8; 64 * 1024];
    while remaining > 0 {
        let want = remaining.min(buffer.len() as u64) as usize;
        let read = file.read(&mut buffer[..want])?;
        if read == 0 {
            break;
        }
        send.write_all(&buffer[..read]).await?;
        remaining -= read as u64;
    }
    send.finish()?;
    tracing::info!(
        conn_id,
        %remote_id,
        sent = track.file_size.saturating_sub(offset).saturating_sub(remaining),
        leftover = remaining,
        "media stream finished"
    );
    Ok(())
}

fn catalog_snapshot(state: &ServerState, since_revision: Option<u64>) -> Result<Message> {
    let store = state.store.lock().expect("store");
    let revision = store.library_revision()?;
    if since_revision == Some(revision) {
        return Ok(Message::Catalog {
            revision,
            tracks: Vec::new(),
            removed: Vec::new(),
            replace: false,
        });
    }
    let tracks = store
        .load_tracks()?
        .into_values()
        .filter(|track| !track.origin.is_remote())
        .map(|track| CatalogTrack {
            remote_id: remote_id_for(&track.path),
            title: track.title,
            artist: track.artist,
            album: track.album,
            date: track.date,
            track_number: track.track_number,
            duration_ms: duration_ms(track.duration),
            codec: track.codec,
            sample_rate: track.sample_rate,
            channels: track.channels,
            file_size: track.file_size,
            modified_ns: track.modified_ns,
        })
        .collect();
    Ok(Message::Catalog {
        revision,
        tracks,
        removed: Vec::new(),
        replace: true,
    })
}

fn path_allowed(path: &Path, roots: &[PathBuf]) -> bool {
    let Ok(canonical) = path.canonicalize() else {
        return false;
    };
    roots.iter().any(|root| {
        root.canonicalize()
            .ok()
            .is_some_and(|root| canonical.starts_with(root))
    })
}

fn set_authorized(state: &ServerState, conn_id: usize, authorized: bool) {
    state
        .authorized
        .lock()
        .expect("auth")
        .insert(conn_id, authorized);
}

fn is_authorized(state: &ServerState, conn_id: usize) -> bool {
    state
        .authorized
        .lock()
        .expect("auth")
        .get(&conn_id)
        .copied()
        .unwrap_or(false)
}

pub fn connectable_addrs(bind: SocketAddr) -> Vec<SocketAddr> {
    if !bind.ip().is_unspecified() {
        return vec![bind];
    }
    let port = bind.port();
    let mut addrs = Vec::new();
    if let Ok(interfaces) = if_addrs::get_if_addrs() {
        for interface in interfaces {
            if interface.is_loopback() || is_virtual_interface(&interface.name) {
                continue;
            }
            if let std::net::IpAddr::V4(ip) = interface.ip() {
                addrs.push(SocketAddr::from((ip, port)));
            }
        }
    }
    addrs.sort();
    addrs.dedup();
    addrs
}

fn is_virtual_interface(name: &str) -> bool {
    name == "docker0"
        || name == "podman0"
        || name.starts_with("br-")
        || name.starts_with("veth")
        || name.starts_with("cni")
        || name.starts_with("virbr")
}

fn print_connect_hints(bind: SocketAddr, pairing_code: &str) {
    let addrs = connectable_addrs(bind);
    if addrs.is_empty() {
        println!(
            "Connect from this machine:  staccato connect 127.0.0.1:{} --code {pairing_code}",
            bind.port()
        );
        println!(
            "From another computer use this Pi's LAN IP, not 0.0.0.0 (that is only the listen address)."
        );
        return;
    }
    println!("Connect from another computer (not 0.0.0.0):");
    for address in &addrs {
        println!("  staccato connect {address} --code {pairing_code}");
    }
    println!(
        "On this machine:  staccato connect 127.0.0.1:{} --code {pairing_code}",
        bind.port()
    );
}

fn print_index(stats: &IndexStats) {
    println!(
        "Indexed {} tracks ({} unreadable). Library revision {}.",
        stats.discovered, stats.failed, stats.revision
    );
}

fn default_server_name() -> String {
    hostname::get()
        .ok()
        .and_then(|name| name.into_string().ok())
        .filter(|name| !name.is_empty())
        .map(|name| format!("Staccato on {name}"))
        .unwrap_or_else(|| "Staccato".into())
}

fn generate_pairing_code() -> String {
    format!("{:06}", rand::thread_rng().gen_range(0..1_000_000))
}

fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill(&mut bytes);
    hex::encode(bytes)
}

pub fn remote_id_for(path: &Path) -> String {
    hex::encode(Sha256::digest(path_codec::encode(path)))
}

fn scan_or_fallback(path: &Path) -> Result<ScannedTrack> {
    match scan_file(path) {
        Ok(track) => Ok(track),
        Err(error) => Ok(ScannedTrack {
            path: path.to_path_buf(),
            title: fallback_title(path),
            artist: "Unknown artist".into(),
            album: "Unknown album".into(),
            date: None,
            track_number: None,
            duration: std::time::Duration::ZERO,
            codec: path
                .extension()
                .map(|extension| extension.to_string_lossy().to_ascii_uppercase())
                .unwrap_or_else(|| "Unknown".into()),
            sample_rate: None,
            channels: None,
            file_size: std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0),
            modified_ns: 0,
            scan_error: Some(error.to_string()),
            replay_gain: crate::model::ReplayGainInfo::default(),
        }),
    }
}

fn advertise_mdns(server_name: &str, port: u16) -> Result<mdns_sd::ServiceDaemon> {
    let daemon = mdns_sd::ServiceDaemon::new().context("starting mDNS")?;
    let instance = server_name.replace(' ', "-");
    let host = hostname::get()
        .ok()
        .and_then(|name| name.into_string().ok())
        .unwrap_or_else(|| "staccato".into());
    let service = mdns_sd::ServiceInfo::new(
        "_staccato._udp.local.",
        &instance,
        &format!("{host}.local."),
        "",
        port,
        None,
    )
    .context("creating mDNS service")?
    .enable_addr_auto();
    daemon
        .register(service)
        .context("registering mDNS service")?;
    Ok(daemon)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_wav(path: &Path) -> Result<()> {
        let mut file = std::fs::File::create(path)?;
        let data_len = 8u32;
        file.write_all(b"RIFF")?;
        file.write_all(&(36 + data_len).to_le_bytes())?;
        file.write_all(b"WAVEfmt ")?;
        file.write_all(&16u32.to_le_bytes())?;
        file.write_all(&1u16.to_le_bytes())?;
        file.write_all(&1u16.to_le_bytes())?;
        file.write_all(&8_000u32.to_le_bytes())?;
        file.write_all(&16_000u32.to_le_bytes())?;
        file.write_all(&2u16.to_le_bytes())?;
        file.write_all(&16u16.to_le_bytes())?;
        file.write_all(b"data")?;
        file.write_all(&data_len.to_le_bytes())?;
        file.write_all(&[0; 8])?;
        Ok(())
    }

    #[test]
    fn index_roots_records_scanned_tracks() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let music = directory.path().join("music");
        std::fs::create_dir_all(&music)?;
        write_wav(&music.join("tone.wav"))?;
        let mut store = Store::in_memory()?;
        let stats = index_roots(&mut store, &[music])?;
        assert_eq!(stats.discovered, 1);
        assert_eq!(store.load_tracks()?.len(), 1);
        assert!(stats.revision >= 1);
        Ok(())
    }

    #[test]
    fn unspecified_bind_is_not_advertised_as_a_client_address() {
        let addrs = connectable_addrs("0.0.0.0:1744".parse().unwrap());
        assert!(addrs.iter().all(|address| !address.ip().is_unspecified()));
    }

    #[test]
    fn remote_ids_are_stable_for_a_path() {
        let path = PathBuf::from("/music/album/track.flac");
        assert_eq!(remote_id_for(&path), remote_id_for(&path));
        assert_ne!(
            remote_id_for(&path),
            remote_id_for(Path::new("/music/album/other.flac"))
        );
    }

    #[test]
    fn client_pairs_and_reads_catalog() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let music = directory.path().join("music");
        std::fs::create_dir_all(&music)?;
        write_wav(&music.join("tone.wav"))?;
        let data_dir = directory.path().join("server");
        std::fs::create_dir_all(&data_dir)?;
        let music_root = music.clone();

        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().expect("runtime");
            runtime.block_on(async move {
                let mut store = match Store::open(&data_dir.join(SERVER_DB_NAME)) {
                    Ok(store) => store,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                if let Err(error) = index_roots(&mut store, &[music]) {
                    let _ = ready_tx.send(Err(error));
                    return;
                }
                let (cert, key) = match crate::net::tls::load_or_generate_server_cert(&data_dir) {
                    Ok(pair) => pair,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                let fingerprint = crate::net::tls::fingerprint(&cert);
                let pairing_code = "654321".to_owned();
                let server_config = match crate::net::tls::server_crypto(cert, key) {
                    Ok(config) => quinn::ServerConfig::with_crypto(Arc::new(config)),
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                let endpoint = match Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap())
                {
                    Ok(endpoint) => endpoint,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error.into()));
                        return;
                    }
                };
                let address = endpoint.local_addr().expect("local addr");
                let state = Arc::new(ServerState {
                    store: Mutex::new(store),
                    roots: vec![music_root],
                    pairing_code: Mutex::new(pairing_code.clone()),
                    server_name: "Test".into(),
                    fingerprint: fingerprint.clone(),
                    authorized: Mutex::new(HashMap::new()),
                });
                let _ = ready_tx.send(Ok((address, pairing_code, fingerprint)));
                while let Some(incoming) = endpoint.accept().await {
                    let state = Arc::clone(&state);
                    tokio::spawn(async move {
                        let _ = accept_connection(incoming, state).await;
                    });
                }
            });
        });

        let (address, pairing_code, fingerprint) = ready_rx.recv().expect("server started")?;
        let cache = directory.path().join("cache");
        let report =
            crate::net::connect_once(address, Some(pairing_code), Some(cache.clone()), None)?;
        assert_eq!(report.server_name, "Test");
        assert_eq!(report.fingerprint, fingerprint);
        assert_eq!(report.tracks, 1);
        assert!(report.token.is_some());
        let cached = std::fs::read_dir(cache.join(&fingerprint))?
            .filter_map(Result::ok)
            .any(|entry| {
                entry.path().extension().is_none()
                    && std::fs::metadata(entry.path())
                        .ok()
                        .is_some_and(|meta| meta.len() > 0)
            });
        assert!(cached, "expected a cached media file");
        Ok(())
    }
}
