//! `glft`: the terminal program, and the reference for what the window and phone show.
//!
//! Most commands are requests to the engine (`glftd`), which is started on first use and
//! keeps running, so receiving, pairings and unfinished transfers outlive the terminal.

use std::{
    io::{BufRead, IsTerminal, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{Parser, Subcommand, ValueEnum};
use glft_core::{
    Error, ErrorKind,
    devices::Remember,
    events::{human_bytes, local_time},
    identity, paths,
    storage::VERIFIED_XATTR,
};
use glft_engine::{ErrorInfo, LogLine, Reply, Request, client};

#[derive(Parser)]
#[command(name = "glft", version, about = "Send files directly between your devices, verified.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum OnOff {
    On,
    Off,
}

#[derive(Subcommand)]
enum Command {
    /// Print this device's id.
    Id,
    /// Show what glft is doing: receiving, sending, offers waiting.
    Status,
    /// Switch receiving on or off. While on, paired devices can send files here.
    Receive {
        #[arg(value_enum, default_value = "on")]
        state: OnOff,
        /// Folder to save into [default: ~/Downloads/glft].
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Listen on this UDP port instead of the preferred one (0 = any free port).
        #[arg(long)]
        port: Option<u16>,
    },
    /// Show the event log.
    Log {
        /// Keep showing new lines until Ctrl-C.
        #[arg(short, long)]
        follow: bool,
    },
    /// Show a pairing code for your phone (or another computer) to scan.
    Pair,
    /// Pair using another device's code, as printed under its QR code.
    Join { code: String },
    /// List paired devices.
    Devices,
    /// Forget a paired device; it can no longer send files here.
    Forget { device: String },
    /// Accept a paired device's files without asking, or ask again.
    AutoAccept {
        device: String,
        #[arg(value_enum)]
        state: OnOff,
    },
    /// Send files and folders to a paired device.
    Send {
        /// The device's name or the start of its id, as `glft devices` lists it.
        device: String,
        /// Address of the receiving device, e.g. 192.168.1.40:47382; repeatable.
        /// Without it, the device is looked for at its last address and on the local network.
        #[arg(long = "addr")]
        addrs: Vec<SocketAddr>,
        /// Keep sending after this command exits.
        #[arg(long)]
        detach: bool,
        /// Files and folders to send.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Stop a send, by the number `glft status` shows.
    Cancel { send: u64 },
    /// List offers waiting for an answer.
    Offers,
    /// Accept an offer (the only one waiting, or the one numbered).
    Accept { offer: Option<u64> },
    /// Decline an offer (the only one waiting, or the one numbered).
    Decline { offer: Option<u64> },
    /// Check received files against the fingerprints glft saved with them.
    Verify {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Install the engine as a systemd user service (done automatically on first use).
    Setup,
    /// Restart the engine, e.g. after updating glft.
    Restart,
    /// Stop the engine; receiving stops too. The next command starts it again.
    Quit,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli.command).await {
        Ok(code) => code,
        Err(e) => {
            out().clear();
            print_error(&e);
            ExitCode::FAILURE
        }
    }
}

type Result<T> = std::result::Result<T, ErrorInfo>;

fn local(e: Error) -> ErrorInfo {
    ErrorInfo::from(&e)
}

async fn run(command: Command) -> Result<ExitCode> {
    match command {
        Command::Id => {
            let (secret, _) = identity::load_or_create(&paths::data_dir().map_err(local)?).map_err(local)?;
            println!("{}", secret.public());
        }
        Command::Status => {
            let mut s = Session::open(Request::Status).await?;
            while let Some(reply) = s.next().await? {
                if let Reply::Status(st) = reply {
                    println!("glft engine running (pid {}), version {}", st.pid, st.version.split(' ').next().unwrap_or_default());
                    println!("This device: \"{}\", id {}", st.name, st.id);
                    match st.receiving {
                        Some(r) => {
                            let addrs: Vec<String> = r.addrs.iter().map(|a| a.to_string()).collect();
                            println!("Receiving: on, into {} ({})", r.folder, addrs.join(", "));
                        }
                        None => println!("Receiving: off (`glft receive on` switches it on)"),
                    }
                    for s in &st.sending {
                        println!("Sending {}: {s} (`glft cancel {}` stops it)", s.id, s.id);
                    }
                    for i in &st.incoming {
                        let pct = (i.done * 100).checked_div(i.total).unwrap_or(0);
                        println!("Receiving from {}: {} of {} ({pct}%)", i.peer, human_bytes(i.done), human_bytes(i.total));
                    }
                    if st.offers > 0 {
                        println!("Offers waiting: {} (`glft offers`)", st.offers);
                    }
                }
            }
        }
        Command::Receive { state, dir, port } => {
            let dir = dir.map(|d| std::path::absolute(&d).unwrap_or(d));
            let mut s = Session::open(Request::Receive { on: state == OnOff::On, dir, port }).await?;
            s.drain().await?;
            if state == OnOff::On {
                out().line("Paired devices can now send files here. `glft receive off` stops it; `glft log -f` follows along.");
            }
        }
        Command::Log { follow } => {
            let mut s = Session::open(Request::Log { follow }).await?;
            s.drain().await?;
        }
        Command::Pair => pair(Request::Pair).await?,
        Command::Join { code } => pair(Request::Join { code }).await?,
        Command::Devices => {
            let mut s = Session::open(Request::Devices).await?;
            while let Some(reply) = s.next().await? {
                if let Reply::Devices { devices } = reply {
                    if devices.is_empty() {
                        println!("No paired devices. `glft pair` shows a code to scan with the phone.");
                    }
                    for d in devices {
                        let kind = if d.remembered { "remembered" } else { "until you log out" };
                        let accept = if d.device.auto_accept { "accepts without asking" } else { "asks first" };
                        let seen = d.device.last_seen.map(|t| format!(", last transfer {}", local_time(t))).unwrap_or_default();
                        println!("{:<24} {}  {kind}, {accept}{seen}", d.device.name, d.device.short_id());
                    }
                }
            }
        }
        Command::Forget { device } => Session::open(Request::Forget { device }).await?.drain().await?,
        Command::AutoAccept { device, state } => {
            Session::open(Request::AutoAccept { device, on: state == OnOff::On }).await?.drain().await?
        }
        Command::Send { device, addrs, detach, paths } => {
            let paths = paths.into_iter().map(|p| std::path::absolute(&p).unwrap_or(p)).collect();
            Session::open(Request::Send { device, addrs, paths, detach }).await?.drain().await?;
        }
        Command::Cancel { send } => Session::open(Request::CancelSend { id: send }).await?.drain().await?,
        Command::Offers => {
            for o in offers().await? {
                println!("{:>3}  {} offers {}, to be saved to {}", o.id, o.peer, o.summary, o.dest);
            }
        }
        Command::Accept { offer } => answer(offer, true).await?,
        Command::Decline { offer } => answer(offer, false).await?,
        Command::Verify { paths } => return Ok(verify(&paths)),
        Command::Setup => {
            install_units(true)?;
            install_desktop()?;
        }
        Command::Restart => {
            if let Ok(s) = Session::try_open(Request::Quit).await {
                let _ = s.finish().await;
                client::wait_for_exit().await;
            }
            let mut s = Session::open(Request::Status).await?;
            while let Some(reply) = s.next().await? {
                if let Reply::Status(st) = reply {
                    println!("glft engine restarted (pid {})", st.pid);
                }
            }
        }
        Command::Quit => match Session::try_open(Request::Quit).await {
            Ok(s) => {
                s.finish().await?;
                println!("glft engine stopped");
            }
            Err(_) => println!("glft engine was not running"),
        },
    }
    Ok(ExitCode::SUCCESS)
}

async fn offers() -> Result<Vec<glft_engine::OfferInfo>> {
    let mut s = Session::open(Request::Offers).await?;
    let mut list = Vec::new();
    while let Some(reply) = s.next().await? {
        if let Reply::Offers { offers } = reply {
            list = offers;
        }
    }
    if list.is_empty() {
        out().line("No offers are waiting for an answer.");
    }
    Ok(list)
}

async fn answer(offer: Option<u64>, accept: bool) -> Result<()> {
    let offer = match offer {
        Some(o) => o,
        None => {
            let list = offers().await?;
            match list.as_slice() {
                [] => return Ok(()),
                [one] => one.id,
                many => {
                    for o in many {
                        println!("{:>3}  {} offers {}", o.id, o.peer, o.summary);
                    }
                    return Err(local(Error::new(ErrorKind::NoSuchOffer, "several offers are waiting; give the number")));
                }
            }
        }
    };
    Session::open(Request::Answer { offer, accept }).await?.drain().await
}

/// `glft pair` and `glft join`: show or use the code, then ask "Remember this device?".
async fn pair(request: Request) -> Result<()> {
    let mut s = Session::open(request).await?;
    while let Some(reply) = s.next().await? {
        match reply {
            Reply::PairCode { code } => {
                print_qr(&code);
                println!();
                println!("{code}");
                println!();
                out().line("Scan this with glft on the phone (Pair with a computer), or run `glft join <code>` on another computer.");
                out().line("The code works once, for 5 minutes. Ctrl-C cancels.");
            }
            Reply::PairPeer { name, .. } => {
                let (remember, auto_accept) = tokio::task::spawn_blocking(move || ask_remember(&name)).await.unwrap_or((Remember::No, false));
                s.send(&Request::PairAnswer { remember, auto_accept }).await?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn ask_remember(name: &str) -> (Remember, bool) {
    let remember = loop {
        let a = prompt(&format!("Remember \"{name}\"? [Y]es / [j]ust this time / [n]o: "));
        match a.to_lowercase().as_str() {
            "" | "y" | "yes" => break Remember::Yes,
            "j" | "just this time" => break Remember::JustThisTime,
            "n" | "no" => break Remember::No,
            _ => println!("Please answer y, j or n."),
        }
    };
    let auto = remember == Remember::Yes
        && matches!(prompt(&format!("Accept files from \"{name}\" without asking? [y/N]: ")).to_lowercase().as_str(), "y" | "yes");
    (remember, auto)
}

fn prompt(question: &str) -> String {
    print!("{:>10}  {question}", "");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok();
    line.trim().to_string()
}

/// Draws the code with half blocks, black on white whatever the terminal's theme, with
/// the quiet zone scanners need.
fn print_qr(code: &str) {
    let Ok(qr) = qrcode::QrCode::with_error_correction_level(code, qrcode::EcLevel::M) else { return };
    let width = qr.width();
    let colors = qr.to_colors();
    let quiet = 2isize;
    let dark = |x: isize, y: isize| {
        x >= 0 && y >= 0 && (x as usize) < width && (y as usize) < width && colors[y as usize * width + x as usize] == qrcode::Color::Dark
    };
    let size = width as isize;
    let mut y = -quiet;
    while y < size + quiet {
        let mut row = String::from("  \x1b[30;47m");
        for x in -quiet..size + quiet {
            row.push(match (dark(x, y), dark(x, y + 1)) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        row.push_str("\x1b[0m");
        println!("{row}");
        y += 2;
    }
}

/// `glft verify`: re-hashes files and compares them with the fingerprint saved on arrival.
fn verify(paths: &[PathBuf]) -> ExitCode {
    let (mut ok, mut changed, mut unmarked) = (0, 0, 0);
    for root in paths {
        for entry in walkdir::WalkDir::new(root).sort_by_file_name().into_iter().flatten() {
            if !entry.file_type().is_file() || entry.file_name().to_string_lossy().ends_with(glft_core::manifest::PARTIAL_SUFFIX) {
                continue;
            }
            let path = entry.path();
            let mark = xattr::get(path, VERIFIED_XATTR).ok().flatten().and_then(|v| String::from_utf8(v).ok());
            let Some(mark) = mark else {
                unmarked += 1;
                println!("  -  {}  (no glft fingerprint)", path.display());
                continue;
            };
            match hash_file(path) {
                Ok(h) if h == mark => {
                    ok += 1;
                    println!("  ✓  {}", path.display());
                }
                Ok(_) => {
                    changed += 1;
                    println!("  ✗  {}  CHANGED since it arrived", path.display());
                }
                Err(e) => {
                    changed += 1;
                    println!("  ✗  {}  could not be read: {e}", path.display());
                }
            }
        }
    }
    println!();
    println!("{ok} verified, {changed} changed or unreadable, {unmarked} without a fingerprint");
    if unmarked > 0 {
        println!("Files without a fingerprint were not received by glft, or were copied without their extended attributes.");
    }
    if changed > 0 { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}

fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(std::fs::File::open(path)?)?;
    Ok(hasher.finalize().to_hex().to_string())
}

// ---------------------------------------------------------------- talking to the engine

/// A request to the engine, printing log lines and progress as they come.
struct Session {
    inner: client::Session,
}

impl Session {
    /// Sends `request`, starting the engine first if it is not running.
    async fn open(request: Request) -> Result<Self> {
        let inner = client::Session::open(request, announce_setup).await?;
        Ok(Self::checked(inner))
    }

    /// Like [`Self::open`], but never starts the engine.
    async fn try_open(request: Request) -> Result<Self> {
        Ok(Self::checked(client::Session::try_open(request).await?))
    }

    fn checked(inner: client::Session) -> Self {
        if client::Session::outdated() {
            out().line("Note: the running engine is older than the installed glftd; `glft restart` switches to it.");
        }
        Self { inner }
    }

    /// The next reply that is not a log line or progress (those are printed), or `None`
    /// when the request is done.
    async fn next(&mut self) -> Result<Option<Reply>> {
        loop {
            match self.inner.next().await {
                Ok(None) => {
                    out().clear();
                    return Ok(None);
                }
                Ok(Some(Reply::Line(l))) => out().log(&l),
                Ok(Some(Reply::Progress { done, total, sending, .. })) => out().progress(done, total, sending),
                Ok(Some(Reply::Offer(o))) => {
                    out().line(&format!("{} offers {}; `glft accept {}` or `glft decline {}`", o.peer, o.summary, o.id, o.id));
                }
                Ok(Some(other)) => return Ok(Some(other)),
                Err(e) => return Err(e),
            }
        }
    }

    async fn send(&mut self, request: &Request) -> Result<()> {
        self.inner.send(request).await
    }

    async fn drain(&mut self) -> Result<()> {
        while self.next().await?.is_some() {}
        Ok(())
    }

    async fn finish(mut self) -> Result<()> {
        self.drain().await
    }
}

fn announce_setup(dir: &Path) {
    out().line(&format!(
        "Set up glft's engine as a systemd user service (glft.socket, glft.service in {}). It starts when a glft command needs it and listens on the network only while receiving is on.",
        tilde(dir)
    ));
}

/// `glft setup`: the systemd units.
fn install_units(verbose: bool) -> Result<()> {
    let dir = glft_engine::unit_dir().map_err(local)?;
    if client::install_units()? {
        announce_setup(&dir);
    } else if verbose {
        out().line(&format!("glft's engine is set up (units in {})", tilde(&dir)));
    }
    Ok(())
}

/// The window's menu entry, icon and Nemo's "Send to phone…", for this user, when
/// `glft-window` is installed next to `glft`. Packages install them system-wide instead.
fn install_desktop() -> Result<()> {
    let window = client::sibling("glft-window");
    let Ok(window) = std::fs::canonicalize(&window) else {
        out().line("The glft window isn't installed next to glft, so no menu entry or Nemo action was set up.");
        return Ok(());
    };
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .ok_or_else(|| local(Error::new(ErrorKind::NoHomeDir, "neither XDG_DATA_HOME nor HOME is set")))?;
    let exe = window.display().to_string();
    let exe = if exe.contains(' ') { format!("\"{exe}\"") } else { exe };
    let exec = |template: &str| template.replace("Exec=glft-window", &format!("Exec={exe}"));
    let files = [
        ("applications/io.github.contentist.glft.desktop", exec(include_str!("../../../contrib/desktop/io.github.contentist.glft.desktop"))),
        ("icons/hicolor/scalable/apps/io.github.contentist.glft.svg", include_str!("../../../contrib/desktop/io.github.contentist.glft.svg").to_string()),
        ("nemo/actions/glft-send.nemo_action", exec(include_str!("../../../contrib/nemo/glft-send.nemo_action"))),
    ];
    for (rel, content) in &files {
        let path = data.join(rel);
        if std::fs::read_to_string(&path).ok().as_deref() == Some(content.as_str()) {
            continue;
        }
        let dir = path.parent().expect("has a folder");
        std::fs::create_dir_all(dir).map_err(|e| local(Error::io(ErrorKind::DataDirUnwritable, dir, e)))?;
        std::fs::write(&path, content).map_err(|e| local(Error::io(ErrorKind::DataDirUnwritable, &path, e)))?;
        out().line(&format!("Installed {}", tilde(&path)));
    }
    let _ = std::process::Command::new("update-desktop-database")
        .arg(data.join("applications"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    out().line("glft is in the menu, and Nemo's right-click menu has \"Send to phone…\" (open a new Nemo window to see it).");
    Ok(())
}

fn tilde(path: &Path) -> String {
    let p = path.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && p.starts_with(&home) => format!("~{}", &p[home.len()..]),
        _ => p,
    }
}

// ---------------------------------------------------------------- output

/// Prints `HH:MM:SS  sentence`, with a progress line kept at the bottom on a terminal.
struct Out {
    tty: bool,
}

fn out() -> Out {
    Out { tty: std::io::stdout().is_terminal() }
}

static PROGRESS_SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl Out {
    fn clear(&self) {
        if PROGRESS_SHOWN.swap(false, std::sync::atomic::Ordering::Relaxed) {
            print!("\r\x1b[2K");
            let _ = std::io::stdout().flush();
        }
    }

    fn line(&self, sentence: &str) {
        self.clear();
        println!("{}    {sentence}", chrono::Local::now().format("%H:%M:%S"));
    }

    fn log(&self, l: &LogLine) {
        self.clear();
        let time = chrono::DateTime::from_timestamp_millis(l.at_unix_ms)
            .map(|t| t.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
            .unwrap_or_default();
        if l.is_error {
            eprintln!("{time}    {}", l.sentence);
            if let Some(n) = &l.next_step {
                eprintln!("{:>10}  What to do: {n}", "");
            }
            if let Some(d) = &l.detail {
                eprintln!("{:>10}  details: {d}", "");
            }
        } else {
            println!("{time}    {}", l.sentence);
        }
    }

    fn progress(&self, done: u64, total: u64, sending: bool) {
        if !self.tty {
            return;
        }
        let pct = (done * 100).checked_div(total).unwrap_or(100);
        let what = if sending { "sent" } else { "verified" };
        print!("\r\x1b[2K{:>10}  {} of {} {what} ({pct}%)", "", human_bytes(done), human_bytes(total));
        let _ = std::io::stdout().flush();
        PROGRESS_SHOWN.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

fn print_error(e: &ErrorInfo) {
    let time = chrono::Local::now().format("%H:%M:%S");
    eprintln!("{time}    {}", e.headline);
    eprintln!("{:>10}  What to do: {}", "", e.next_step);
    eprintln!("{:>10}  details: [{}] {}", "", e.id, e.detail);
}
