use std::{fs, io, path::Path};

use anyhow::{Context, Result};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

/// File + optional stderr. Safe to call more than once (later calls are ignored).
///
/// `RUST_LOG` overrides the default filter `staccato=debug,quinn=info`.
pub fn init(data_dir: &Path, filename: &str, stderr: bool) -> Result<std::path::PathBuf> {
    fs::create_dir_all(data_dir)
        .with_context(|| format!("creating log directory {}", data_dir.display()))?;
    let path = data_dir.join(filename);
    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening log file {}", path.display()))?;

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("staccato=debug,quinn=info"));

    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(true)
        .with_writer(move || file.try_clone().expect("clone log file"));

    let registry = tracing_subscriber::registry().with(filter).with(file_layer);

    let result = if stderr {
        registry
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(true)
                    .with_target(true)
                    .with_writer(io::stderr),
            )
            .try_init()
    } else {
        registry.try_init()
    };
    if let Err(error) = result {
        eprintln!("logging already initialized ({error}); continuing");
    }
    Ok(path)
}
