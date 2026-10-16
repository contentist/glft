//! Where glft keeps its own data and where received files go.

use std::path::PathBuf;

use crate::error::{Error, ErrorKind};

/// Overrides the data directory, so two copies can run side by side on one computer.
pub const DATA_DIR_ENV: &str = "GLFT_DATA_DIR";

/// The directory holding the key pair and, later, paired devices and transfer state.
///
/// Defaults to `~/.local/share/glft`.
pub fn data_dir() -> Result<PathBuf, Error> {
    if let Some(dir) = std::env::var_os(DATA_DIR_ENV) {
        return Ok(PathBuf::from(dir));
    }
    directories::ProjectDirs::from("", "", "glft")
        .map(|d| d.data_dir().to_path_buf())
        .ok_or_else(|| Error::new(ErrorKind::NoHomeDir, "no home directory in the environment"))
}

/// The default receive folder: `~/Downloads/glft`.
pub fn default_receive_dir() -> Result<PathBuf, Error> {
    let user = directories::UserDirs::new()
        .ok_or_else(|| Error::new(ErrorKind::NoHomeDir, "no home directory in the environment"))?;
    let downloads = user
        .download_dir()
        .map(|d| d.to_path_buf())
        .unwrap_or_else(|| user.home_dir().join("Downloads"));
    Ok(downloads.join("glft"))
}

/// This device's name as shown to the other side.
pub fn device_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Linux computer".to_string())
}
