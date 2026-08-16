mod action;
mod app;
mod audio;
mod cover;
mod input;
mod library;
mod model;
mod net;
mod path_codec;
mod replaygain;
mod settings;
mod soulseek;
mod spectrum;
mod storage;
mod ui;

use std::{
    io::{self, IsTerminal, Stdout},
    net::{SocketAddr, ToSocketAddrs},
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use directories::ProjectDirs;
use input::InputMapper;
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::{action::Action, app::App};

#[derive(Debug, Parser)]
#[command(version, about = "A foobar2000-inspired terminal music player")]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,

    /// Files or folders to add to the active playlist
    #[arg(value_name = "PATH")]
    paths: Vec<PathBuf>,

    /// Override the directory containing staccato.db
    #[arg(long, value_name = "DIRECTORY")]
    data_dir: Option<PathBuf>,

    /// Run the complete interface without opening an audio output device
    #[arg(long)]
    no_audio: bool,

    /// Use Nerd Font icons for the playback controls
    #[arg(long)]
    nerd_font: bool,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Index a music folder and serve it over QUIC
    Serve {
        /// Folders to index. Omit to reuse the folders from the last serve.
        #[arg(value_name = "PATH")]
        paths: Vec<PathBuf>,
        /// Override the directory containing the server database and TLS cert
        #[arg(long, value_name = "DIRECTORY")]
        data_dir: Option<PathBuf>,
        /// Listen address
        #[arg(long, default_value = "0.0.0.0")]
        bind: String,
        /// UDP port
        #[arg(long, default_value_t = crate::net::DEFAULT_PORT)]
        port: u16,
        /// Forget issued pairing tokens
        #[arg(long)]
        reset_pairing: bool,
        /// Do not advertise the server on the LAN
        #[arg(long)]
        no_mdns: bool,
    },
    /// Handshake with a server and print its catalog size
    Connect {
        /// host:port of the server
        endpoint: String,
        /// Pairing code printed by the server
        #[arg(long)]
        code: Option<String>,
        /// Override the directory used for the client pin/token
        #[arg(long, value_name = "DIRECTORY")]
        data_dir: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let args = Args::parse();
    match args.command {
        Some(Command::Serve {
            paths,
            data_dir,
            bind,
            port,
            reset_pairing,
            no_mdns,
        }) => {
            let data_dir = resolve_data_dir(data_dir)?;
            let bind = format!("{bind}:{port}")
                .parse::<SocketAddr>()
                .with_context(|| format!("invalid listen address {bind}:{port}"))?;
            let log_path = crate::net::log::init(&data_dir, "staccato-server.log", true)?;
            println!("Logging to {}", log_path.display());
            return crate::net::run_server(crate::net::ServeOptions {
                data_dir,
                roots: paths,
                bind,
                reset_pairing,
                advertise: !no_mdns,
                server_name: None,
            });
        }
        Some(Command::Connect {
            endpoint,
            code,
            data_dir,
        }) => {
            let data_dir = resolve_data_dir(data_dir)?;
            let log_path = crate::net::log::init(&data_dir, "staccato-client.log", true)?;
            eprintln!("Logging to {}", log_path.display());
            let address = crate::net::normalize_server_addr(parse_endpoint(&endpoint)?);
            let saved = crate::net::credentials::load_for_address(&data_dir, &address.to_string());
            let token = saved
                .as_ref()
                .filter(|saved| !saved.token.is_empty())
                .map(|saved| saved.token.clone());
            let report = crate::net::connect_once(address, code, None, token)?;
            if let Some(token) = report
                .token
                .as_deref()
                .filter(|token| !token.is_empty())
                .or(saved
                    .as_ref()
                    .map(|saved| saved.token.as_str())
                    .filter(|token| !token.is_empty()))
            {
                crate::net::credentials::save_credentials(
                    &data_dir,
                    &address.to_string(),
                    &report.fingerprint,
                    token,
                    &report.server_name,
                )?;
            }
            println!(
                "Connected to {} ({})\nlibrary revision {}, {} tracks",
                report.server_name, report.fingerprint, report.revision, report.tracks
            );
            println!("Saved in {}", data_dir.display());
            return Ok(());
        }
        None => {}
    }

    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(anyhow!("Staccato requires an interactive terminal"));
    }
    let data_dir = resolve_data_dir(args.data_dir)?;
    let log_path = crate::net::log::init(&data_dir, "staccato.log", false)?;
    crate::soulseek::init_logging(&data_dir);
    let mut app = App::open(&data_dir.join("staccato.db"), args.no_audio)
        .with_context(|| format!("initializing Staccato in {}", data_dir.display()))?;
    app.status = app
        .output_device_warning()
        .unwrap_or_else(|| format!("Ready — log {}", log_path.display()));
    if args.nerd_font {
        app.nerd_font = true;
    }
    if !args.paths.is_empty() {
        app.handle(Action::AddPaths(args.paths));
    }

    let mut terminal = init_terminal()?;
    let _guard = TerminalGuard;
    // Query after the alternate screen is up, before the event loop.
    let picker = ratatui_image::picker::Picker::from_query_stdio().unwrap_or_else(|error| {
        tracing::warn!(%error, "graphics query failed; using unicode halfblocks");
        ratatui_image::picker::Picker::halfblocks()
    });
    app.covers.set_picker(picker);
    tracing::info!(protocol = app.covers.protocol_label(), "album art renderer");
    let mut input = InputMapper::default();
    let mut regions = ui::UiRegions::default();

    while !app.should_quit {
        app.tick();
        let icons = if app.nerd_font {
            ui::IconSet::NerdFont
        } else {
            ui::IconSet::default()
        };
        terminal.draw(|frame| regions = ui::draw(frame, &mut app, icons))?;
        if event::poll(Duration::from_millis(50))? {
            let action = input.map(event::read()?, &app, &regions);
            app.handle(action);
        }
    }
    Ok(())
}

fn resolve_data_dir(data_dir: Option<PathBuf>) -> Result<PathBuf> {
    match data_dir {
        Some(path) => Ok(path),
        None => Ok(ProjectDirs::from("com", "Staccato", "Staccato")
            .context("could not determine the platform data directory")?
            .data_local_dir()
            .to_path_buf()),
    }
}

fn parse_endpoint(endpoint: &str) -> Result<SocketAddr> {
    if let Ok(address) = endpoint.parse() {
        return Ok(address);
    }
    let with_port = if endpoint.contains(':') {
        endpoint.to_owned()
    } else {
        format!("{}:{}", endpoint, crate::net::DEFAULT_PORT)
    };
    with_port
        .to_socket_addrs()
        .with_context(|| format!("resolving {endpoint}"))?
        .next()
        .ok_or_else(|| anyhow!("could not resolve {endpoint}"))
}

fn init_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().context("enabling terminal raw mode")?;
    let mut stdout = io::stdout();
    if let Err(error) = execute!(stdout, EnterAlternateScreen, EnableMouseCapture) {
        let _ = disable_raw_mode();
        return Err(error).context("entering the alternate screen");
    }
    Terminal::new(CrosstermBackend::new(stdout)).context("creating terminal backend")
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
    }
}
