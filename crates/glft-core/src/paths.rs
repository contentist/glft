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

static DEVICE_NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Sets the name shown to other devices, where the hostname is not it (Android: the
/// phone's model or the name the user gave it). First call wins.
pub fn set_device_name(name: String) {
    let _ = DEVICE_NAME.set(name);
}

/// Overrides the name shown to other devices.
pub const DEVICE_NAME_ENV: &str = "GLFT_DEVICE_NAME";

/// This device's name as shown to the other side.
pub fn device_name() -> String {
    if let Some(name) = DEVICE_NAME.get() {
        return name.clone();
    }
    if let Some(name) = std::env::var(DEVICE_NAME_ENV).ok().filter(|n| !n.trim().is_empty()) {
        return name.trim().to_string();
    }
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Linux computer".to_string())
}
