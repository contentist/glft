//! `glft setup` and `glft uninstall`: installing glft for one user, without a package.
//!
//! Where glft runs from decides what setup does:
//! - **a package** (`/usr/bin`): its units, menu entry and Nemo action are system-wide, so
//!   setup only switches the engine's socket on for this user;
//! - **this user's copy** (`~/.local/lib/glft`, linked from `~/.local/bin`): setup writes
//!   the units, menu entry, icon and Nemo action again;
//! - **anywhere else** (an unpacked tarball, a build folder): setup copies the programs
//!   into `~/.local/lib/glft` first, then does the above.
//!
//! Before files are replaced or removed, the running engine is stopped (design rule 4).

use std::path::{Path, PathBuf};

use glft_core::{Error, ErrorKind};
use glft_engine::{Request, client};

use crate::{Result, Session, local, out, tilde};

/// The programs glft is made of; `glftd` is needed, the window is optional.
const PROGRAMS: [&str; 3] = ["glft", "glftd", "glft-window"];

/// The desktop files, with `Exec=glft-window` as the packages have it.
const DESKTOP: [(&str, &str); 3] = [
    ("applications/io.github.contentist.glft.desktop", include_str!("../../../contrib/desktop/io.github.contentist.glft.desktop")),
    ("icons/hicolor/scalable/apps/io.github.contentist.glft.svg", include_str!("../../../contrib/desktop/io.github.contentist.glft.svg")),
    ("nemo/actions/glft-send.nemo_action", include_str!("../../../contrib/nemo/glft-send.nemo_action")),
];

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| local(Error::new(ErrorKind::NoHomeDir, "HOME is not set")))
}

fn data_home() -> Result<PathBuf> {
    match std::env::var_os("XDG_DATA_HOME") {
        Some(d) => Ok(PathBuf::from(d)),
        None => Ok(home()?.join(".local/share")),
    }
}

/// Where this user's copy of the programs lives.
fn lib_dir() -> Result<PathBuf> {
    Ok(home()?.join(".local/lib/glft"))
}

fn bin_dir() -> Result<PathBuf> {
    Ok(home()?.join(".local/bin"))
}

fn running_from(dir: &Path) -> bool {
    std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf())).and_then(|p| std::fs::canonicalize(p).ok())
        == std::fs::canonicalize(dir).ok()
}

fn io_err(path: &Path, e: std::io::Error) -> glft_engine::ErrorInfo {
    local(Error::io(ErrorKind::DataDirUnwritable, path, e))
}

/// Stops the engine, if one runs, and keeps systemd from starting it again until setup
/// or the next login.
async fn stop_engine() {
    client::systemctl(&["stop", "glft.socket"]);
    if let Ok(s) = Session::try_open(Request::Quit).await {
        let _ = s.finish().await;
        client::wait_for_exit().await;
        out().line("Stopped the running engine first");
    }
    client::systemctl(&["stop", "glft.service"]);
}

pub async fn setup() -> Result<()> {
    if client::system_install() {
        // An older per-user setup would shadow the package's units.
        remove_units()?;
        client::systemctl(&["daemon-reload"]);
        client::systemctl(&["enable", "--now", "glft.socket"]);
        out().line("glft is installed as a package; its engine, menu entry and Nemo action are set up for every user.");
        return Ok(());
    }
    let lib = lib_dir()?;
    if !running_from(&lib) {
        copy_programs(&lib).await?;
    }
    let engine = lib.join("glftd");
    if client::install_units_for(&engine)? {
        out().line(&format!(
            "Set up glft's engine as a systemd user service (in {}). It starts when glft needs it and listens on the network only while receiving is on.",
            tilde(&glft_engine::unit_dir().map_err(local)?)
        ));
    }
    client::systemctl(&["start", "glft.socket"]);
    let window = lib.join("glft-window");
    if window.exists() {
        install_desktop(&window)?;
    } else {
        out().line("The glft window isn't part of this copy, so no menu entry or Nemo action was set up.");
    }
    let bin = bin_dir()?;
    let on_path = std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d == bin));
    if on_path {
        out().line("Done. `glft` works in any terminal; glft is in the menu.");
    } else {
        out().line(&format!(
            "Done. {} isn't on your PATH yet, so run glft as {} for now; it is on the PATH after your next login on most systems.",
            tilde(&bin),
            tilde(&bin.join("glft"))
        ));
    }
    Ok(())
}

/// Copies the programs next to this one into `lib`, replacing an older copy, and links
/// them from `~/.local/bin`.
async fn copy_programs(lib: &Path) -> Result<()> {
    let from = std::env::current_exe().map_err(|e| local(Error::new(ErrorKind::NoHomeDir, e.to_string())))?;
    let from = from.parent().expect("a program is in a folder").to_path_buf();
    if !from.join("glftd").exists() {
        return Err(local(Error::new(ErrorKind::EngineNotRunning, format!("glftd is missing from {}", from.display()))));
    }
    // Whatever engine runs now runs from somewhere else; the next one runs from `lib`.
    stop_engine().await;
    std::fs::create_dir_all(lib).map_err(|e| io_err(lib, e))?;
    let bin = bin_dir()?;
    std::fs::create_dir_all(&bin).map_err(|e| io_err(&bin, e))?;
    for name in PROGRAMS {
        let src = from.join(name);
        if !src.exists() {
            continue;
        }
        // A new file renamed into place: a program still running from the old one keeps it.
        let dest = lib.join(name);
        let temp = lib.join(format!(".{name}.new"));
        std::fs::copy(&src, &temp).map_err(|e| io_err(&temp, e))?;
        std::fs::rename(&temp, &dest).map_err(|e| io_err(&dest, e))?;
        let link = bin.join(name);
        if std::fs::read_link(&link).ok().as_deref() != Some(dest.as_path()) {
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(&dest, &link).map_err(|e| io_err(&link, e))?;
        }
    }
    out().line(&format!("Installed glft in {}, linked from {}", tilde(lib), tilde(&bin)));
    Ok(())
}

/// The menu entry, icon and Nemo's "Send to phone…", for `window`.
fn install_desktop(window: &Path) -> Result<()> {
    let data = data_home()?;
    let exe = window.display().to_string();
    let exe = if exe.contains(' ') { format!("\"{exe}\"") } else { exe };
    for (rel, template) in DESKTOP {
        let content = template.replace("Exec=glft-window", &format!("Exec={exe}"));
        let path = data.join(rel);
        if std::fs::read_to_string(&path).ok().as_deref() == Some(content.as_str()) {
            continue;
        }
        let dir = path.parent().expect("has a folder");
        std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
        std::fs::write(&path, &content).map_err(|e| io_err(&path, e))?;
    }
    let _ = std::process::Command::new("update-desktop-database")
        .arg(data.join("applications"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    out().line("glft is in the menu, and Nemo's right-click menu has \"Send to phone…\" (open a new Nemo window to see it).");
    Ok(())
}

/// Removes the user units glft wrote, leaving any others alone.
fn remove_units() -> Result<bool> {
    let dir = glft_engine::unit_dir().map_err(local)?;
    let mut removed = false;
    for name in ["glft.socket", "glft.service"] {
        let path = dir.join(name);
        let ours = std::fs::read_to_string(&path).is_ok_and(|s| s.contains("Description=glft engine"));
        if ours {
            std::fs::remove_file(&path).map_err(|e| io_err(&path, e))?;
            let _ = std::fs::remove_file(dir.join("sockets.target.wants").join(name));
            removed = true;
        }
    }
    Ok(removed)
}

pub async fn uninstall(purge: bool) -> Result<std::process::ExitCode> {
    if client::system_install() {
        out().line("glft was installed as a package: remove it with `sudo apt remove glft`. Your key and pairings stay in ~/.local/share/glft.");
        return Ok(std::process::ExitCode::FAILURE);
    }
    client::systemctl(&["disable", "glft.socket"]);
    stop_engine().await;
    let mut gone = Vec::new();
    if remove_units()? {
        client::systemctl(&["daemon-reload"]);
        gone.push("the engine's systemd units".to_string());
    }
    let data = data_home()?;
    for (rel, _) in DESKTOP {
        let path = data.join(rel);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| io_err(&path, e))?;
            gone.push(tilde(&path));
        }
    }
    let lib = lib_dir()?;
    let bin = bin_dir()?;
    for name in PROGRAMS {
        let link = bin.join(name);
        if std::fs::read_link(&link).is_ok_and(|t| t.starts_with(&lib)) {
            std::fs::remove_file(&link).map_err(|e| io_err(&link, e))?;
        }
    }
    if lib.exists() {
        // Linux lets a running program's file be deleted, so this copy can remove itself.
        std::fs::remove_dir_all(&lib).map_err(|e| io_err(&lib, e))?;
        gone.push(format!("the programs in {} and their links in {}", tilde(&lib), tilde(&bin)));
    }
    let data_dir = glft_core::paths::data_dir().map_err(local)?;
    if purge && data_dir.exists() {
        std::fs::remove_dir_all(&data_dir).map_err(|e| io_err(&data_dir, e))?;
        gone.push(format!("this computer's key and pairings ({})", tilde(&data_dir)));
    }
    if gone.is_empty() {
        out().line("Nothing of glft was installed for this user.");
    } else {
        out().line(&format!("Removed {}.", gone.join("; ")));
    }
    if !purge && data_dir.exists() {
        out().line(&format!(
            "Kept this computer's key and pairings in {}, so a reinstall needs no new pairing (`glft uninstall --purge` removes them).",
            tilde(&data_dir)
        ));
    }
    out().line("Received files are yours and stay where they are (~/Downloads/glft).");
    Ok(std::process::ExitCode::SUCCESS)
}
