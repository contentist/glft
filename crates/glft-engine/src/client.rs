//! Reaching the engine from a client (`glft`, `glft-window`): connecting, starting it on
//! demand, and the heartbeat check on every request.

use std::{path::PathBuf, time::Duration};

use glft_core::{Error, ErrorKind, paths};
use tokio::net::UnixStream;

use crate::{Conn, ErrorInfo, HEARTBEAT, Reply, Request};

pub type Result<T> = std::result::Result<T, ErrorInfo>;

fn local(e: Error) -> ErrorInfo {
    ErrorInfo::from(&e)
}

/// One request and its replies.
pub struct Session {
    pub conn: Conn,
}

impl Session {
    /// Sends `request`, starting the engine first if it is not running. `on_setup` is
    /// called with the units' folder if the systemd units were just written.
    pub async fn open(request: Request, on_setup: impl FnOnce(&std::path::Path)) -> Result<Self> {
        let stream = match connect().await {
            Ok(s) => s,
            Err(_) => {
                start_engine(on_setup)?;
                let mut tries = 0;
                loop {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    match connect().await {
                        Ok(s) => break s,
                        Err(e) if tries > 40 => return Err(local(Error::new(ErrorKind::EngineNotRunning, e.to_string()))),
                        Err(_) => tries += 1,
                    }
                }
            }
        };
        Self::start(stream, request).await
    }

    /// Like [`Self::open`], but never starts the engine.
    pub async fn try_open(request: Request) -> Result<Self> {
        let stream = connect().await.map_err(|e| local(Error::new(ErrorKind::EngineNotRunning, e.to_string())))?;
        Self::start(stream, request).await
    }

    async fn start(stream: UnixStream, request: Request) -> Result<Self> {
        let mut conn = Conn::new(stream);
        conn.send(&request).await.map_err(lost_engine)?;
        // The heartbeat: a running engine acknowledges at once.
        match tokio::time::timeout(HEARTBEAT, conn.recv::<Reply>()).await {
            Ok(Ok(Some(Reply::Ack { version, .. }))) => {
                OUTDATED.store(engine_build_id().is_some_and(|installed| installed != version), std::sync::atomic::Ordering::Relaxed);
                Ok(Self { conn })
            }
            Ok(_) => Err(local(Error::new(ErrorKind::EngineNotAnswering, "it closed the connection without answering"))),
            Err(_) => Err(local(Error::new(ErrorKind::EngineNotAnswering, "no answer within 2 seconds"))),
        }
    }

    /// Whether the engine that answered the last request is older than the installed
    /// `glftd`, so `glft restart` would switch to a newer one.
    pub fn outdated() -> bool {
        OUTDATED.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The next reply, or `None` when the request is done; a failure is the `Err`.
    pub async fn next(&mut self) -> Result<Option<Reply>> {
        match self.conn.recv::<Reply>().await.map_err(lost_engine)? {
            None => Err(lost_engine("the connection closed")),
            Some(Reply::Done) => Ok(None),
            Some(Reply::Failed(e)) => Err(e),
            Some(other) => Ok(Some(other)),
        }
    }

    pub async fn send(&mut self, request: &Request) -> Result<()> {
        self.conn.send(request).await.map_err(lost_engine)
    }
}

static OUTDATED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn lost_engine(e: impl std::fmt::Display) -> ErrorInfo {
    local(Error::new(ErrorKind::EngineNotAnswering, e.to_string()))
}

pub async fn connect() -> std::io::Result<UnixStream> {
    let path = crate::socket_path().map_err(|e| std::io::Error::other(e.detail))?;
    UnixStream::connect(path).await
}

/// Waits up to 3 seconds for the engine's socket to stop answering.
pub async fn wait_for_exit() {
    for _ in 0..30 {
        if connect().await.is_err() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// `glftd`, next to the running program.
pub fn engine_path() -> PathBuf {
    sibling("glftd")
}

/// A program installed next to the running one (`glftd`, `glft-window`).
pub fn sibling(name: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    exe.parent().map(|d| d.join(name)).unwrap_or_else(|| PathBuf::from(name))
}

/// The build id `glftd` on disk would report, to spot a running engine that is older.
fn engine_build_id() -> Option<String> {
    let mtime = std::fs::metadata(engine_path()).ok()?.modified().ok()?;
    let secs = mtime.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some(format!("{} {secs}", env!("CARGO_PKG_VERSION")))
}

/// Set (to anything) to keep systemd out of it: the engine is started as a plain process
/// and no units are written. For tests, which must not touch the user's own setup.
pub const NO_SYSTEMD_ENV: &str = "GLFT_NO_SYSTEMD";

/// Whether glft was installed by a package (into /usr), which brings its own units, menu
/// entry and Nemo action.
pub fn system_install() -> bool {
    std::env::current_exe().is_ok_and(|p| p.starts_with("/usr/"))
}

/// Runs `systemctl --user ARGS` quietly; false if it failed or systemd is kept out.
pub fn systemctl(args: &[&str]) -> bool {
    if std::env::var_os(NO_SYSTEMD_ENV).is_some() {
        return false;
    }
    std::process::Command::new("systemctl")
        .arg("--user")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn systemd_user_available() -> bool {
    if std::env::var_os(NO_SYSTEMD_ENV).is_some() {
        return false;
    }
    std::process::Command::new("systemctl")
        .args(["--user", "show-environment"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Starts the engine: through systemd's socket when it can, else as a plain background
/// process (and always so when GLFT_RUNTIME_DIR points at a private socket).
pub fn start_engine(on_setup: impl FnOnce(&std::path::Path)) -> Result<()> {
    if std::env::var_os(crate::RUNTIME_DIR_ENV).is_none() && systemd_user_available() {
        if install_units()? {
            on_setup(&crate::unit_dir().map_err(local)?);
        }
        let started = std::process::Command::new("systemctl").args(["--user", "start", "glft.socket"]).status();
        if started.is_ok_and(|s| s.success()) {
            return Ok(());
        }
    }
    let data_dir = paths::data_dir().map_err(local)?;
    std::fs::create_dir_all(&data_dir).map_err(|e| local(Error::io(ErrorKind::DataDirUnwritable, &data_dir, e)))?;
    let log_path = data_dir.join("engine.log");
    let log = std::fs::OpenOptions::new().create(true).append(true).open(&log_path).map_err(|e| local(Error::io(ErrorKind::DataDirUnwritable, &log_path, e)))?;
    let err_log = log.try_clone().map_err(|e| local(Error::io(ErrorKind::DataDirUnwritable, &log_path, e)))?;
    use std::os::unix::process::CommandExt;
    std::process::Command::new(engine_path())
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(err_log)
        // Its own process group, so Ctrl-C in a terminal does not stop it.
        .process_group(0)
        .spawn()
        .map_err(|e| local(Error::new(ErrorKind::EngineNotRunning, format!("{}: {e}", engine_path().display()))))?;
    Ok(())
}

/// Writes the systemd user units if they are missing or point at another `glftd`.
/// Returns whether anything changed. A package install has its units in /usr already.
pub fn install_units() -> Result<bool> {
    if system_install() {
        return Ok(false);
    }
    install_units_for(&std::fs::canonicalize(engine_path()).unwrap_or_else(|_| engine_path()))
}

/// Writes the systemd user units for the engine at `engine`.
pub fn install_units_for(engine: &std::path::Path) -> Result<bool> {
    if std::env::var_os(NO_SYSTEMD_ENV).is_some() {
        return Ok(false);
    }
    let dir = crate::unit_dir().map_err(local)?;
    let units = [("glft.socket", crate::SOCKET_UNIT.to_string()), ("glft.service", crate::service_unit(engine))];
    let mut changed = false;
    for (name, content) in &units {
        let path = dir.join(name);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(content.as_str()) {
            std::fs::create_dir_all(&dir).map_err(|e| local(Error::io(ErrorKind::EngineNotRunning, &dir, e)))?;
            std::fs::write(&path, content).map_err(|e| local(Error::io(ErrorKind::EngineNotRunning, &path, e)))?;
            changed = true;
        }
    }
    if changed {
        systemctl(&["daemon-reload"]);
        systemctl(&["enable", "glft.socket"]);
    }
    Ok(changed)
}
