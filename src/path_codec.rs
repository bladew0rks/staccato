use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

#[cfg(unix)]
pub fn encode(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(unix)]
pub fn decode(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(OsString::from_vec(bytes.to_vec()))
}

#[cfg(windows)]
pub fn encode(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(windows)]
pub fn decode(bytes: &[u8]) -> PathBuf {
    use std::os::windows::ffi::OsStringExt;
    let wide: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    PathBuf::from(OsString::from_wide(&wide))
}

#[cfg(not(any(unix, windows)))]
pub fn encode(path: &Path) -> Vec<u8> {
    path.to_string_lossy().as_bytes().to_vec()
}

#[cfg(not(any(unix, windows)))]
pub fn decode(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}
