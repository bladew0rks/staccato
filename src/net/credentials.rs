use std::{fs, path::Path};

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SavedRemote {
    pub address: String,
    pub fingerprint: String,
    pub token: String,
    pub server_name: String,
}

fn remote_file(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("remote.json")
}

pub fn load(data_dir: &Path) -> Option<SavedRemote> {
    let bytes = fs::read(remote_file(data_dir)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn load_for_address(data_dir: &Path, address: &str) -> Option<SavedRemote> {
    load(data_dir).filter(|saved| saved.address == address)
}

pub fn save(data_dir: &Path, saved: &SavedRemote) -> Result<()> {
    fs::create_dir_all(data_dir)?;
    fs::write(remote_file(data_dir), serde_json::to_vec_pretty(saved)?)?;
    Ok(())
}

pub fn save_credentials(
    data_dir: &Path,
    address: &str,
    fingerprint: &str,
    token: &str,
    server_name: &str,
) -> Result<()> {
    save(
        data_dir,
        &SavedRemote {
            address: address.to_owned(),
            fingerprint: fingerprint.to_owned(),
            token: token.to_owned(),
            server_name: server_name.to_owned(),
        },
    )
}

pub fn save_address(data_dir: &Path, address: &str) -> Result<()> {
    let mut saved = load(data_dir).unwrap_or_default();
    saved.address = address.to_owned();
    save(data_dir, &saved)
}
