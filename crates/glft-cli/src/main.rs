//! `glft`: the terminal program, and the reference for what the window and phone show.

use std::{
    io::{BufRead, Write},
    net::SocketAddr,
    path::PathBuf,
    process::ExitCode,
    sync::Arc,
};

use clap::{Parser, Subcommand};
use glft_core::{
    Error, ErrorKind, Event,
    events::EventSink,
    identity, net, paths,
    storage::FsStorage,
    transfer::{self, Authorize, Decide, IncomingOffer, ReceiveOptions},
};

#[derive(Parser)]
#[command(name = "glft", version, about = "Send files directly between your devices, verified.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print this device's id.
    Id,
    /// Wait for files from another device.
    Receive {
        /// Folder to save into [default: ~/Downloads/glft].
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Accept every offer without asking.
        #[arg(long)]
        yes: bool,
        /// Stop after the first offer.
        #[arg(long)]
        once: bool,
        /// Listen on this UDP port instead of the preferred one (0 = any free port).
        #[arg(long)]
        port: Option<u16>,
    },
    /// Send files and folders to another device.
    Send {
        /// The receiving device's id, as `glft receive` prints it.
        device: String,
        /// Address of the receiving device, e.g. 192.168.1.40:47382; repeatable.
        /// Without it, the device is looked for on the local network.
        #[arg(long = "addr")]
        addrs: Vec<SocketAddr>,
        /// Files and folders to send.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let events = printer();
    match run(cli.command, &events).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            print_error(&e);
            ExitCode::FAILURE
        }
    }
}

async fn run(command: Command, events: &EventSink) -> Result<(), Error> {
    let data_dir = paths::data_dir()?;
    let (secret, created) = identity::load_or_create(&data_dir)?;
    if created {
        events(Event::KeyCreated { id: secret.public().to_string() });
    }
    match command {
        Command::Id => {
            println!("{}", secret.public());
            Ok(())
        }
        Command::Receive { dir, yes, once, port } => {
            let dir = match dir {
                Some(d) => std::path::absolute(&d).unwrap_or(d),
                None => paths::default_receive_dir()?,
            };
            let endpoint = net::bind(secret, port, events).await?;
            let id = endpoint.id();
            let addrs = net::reachable_addrs(&endpoint).await;
            println!();
            println!("This device's id: {id}");
            println!("To send here from another computer:");
            println!("    glft send {id} <files or folders>");
            if let Some(addr) = addrs.iter().filter(|a| a.is_ipv4() && !a.ip().is_loopback()).min_by_key(|a| lan_rank(a)) {
                println!("If it is not found automatically, add: --addr {addr}");
            }
            println!();
            let decide: Decide = if yes { Arc::new(|_| Box::pin(async { true })) } else { Arc::new(ask) };
            let storage = FsStorage::new(dir.clone()).map_err(|e| Error::io(ErrorKind::ReceiveDirUnwritable, &dir, e))?;
            // Pairing arrives on Day 3; until then every device may make an offer, and each
            // offer is still asked about unless --yes is given.
            let authorize: Authorize = Arc::new(|_| true);
            let opts = ReceiveOptions { storage: Arc::new(storage), decide, authorize, once };
            let result = tokio::select! {
                r = transfer::serve(&endpoint, &opts, events) => r,
                _ = tokio::signal::ctrl_c() => Ok(()),
            };
            endpoint.close().await;
            result
        }
        Command::Send { device, addrs, paths } => {
            let peer = net::peer_addr(net::parse_id(&device)?, &addrs);
            let endpoint = net::bind_ephemeral(secret).await?;
            let result = transfer::send(&endpoint, peer, paths, events).await.map(|_| ());
            endpoint.close().await;
            result
        }
    }
}

/// Orders addresses by how likely they are the home network: 192.168/16, then 10/8,
/// then 172.16/12 (where Docker and other bridges usually sit), then the rest.
fn lan_rank(addr: &SocketAddr) -> u8 {
    match addr.ip() {
        std::net::IpAddr::V4(ip) => match ip.octets() {
            [192, 168, ..] => 0,
            [10, ..] => 1,
            [172, b, ..] if (16..32).contains(&b) => 2,
            _ => 3,
        },
        _ => 4,
    }
}

/// Asks on the terminal whether to accept an offer.
fn ask(offer: IncomingOffer) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>> {
    Box::pin(async move {
        tokio::task::spawn_blocking(move || {
            print!("{:>10}  Accept {} from {} into {}? [y/N] ", "", offer.totals, offer.peer, offer.dest);
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            std::io::stdin().lock().read_line(&mut line).ok();
            matches!(line.trim(), "y" | "Y" | "yes" | "Yes")
        })
        .await
        .unwrap_or(false)
    })
}

/// Prints each event as `HH:MM:SS  sentence`; failures get their details indented below.
fn printer() -> EventSink {
    Arc::new(|event: Event| {
        let time = chrono::Local::now().format("%H:%M:%S");
        match &event {
            Event::Failed(e) => {
                eprintln!("{time}    {event}");
                eprintln!("{:>10}  details: [{}] {}", "", e.kind.id(), e.detail);
            }
            _ => println!("{time}    {event}"),
        }
    })
}

fn print_error(e: &Error) {
    let time = chrono::Local::now().format("%H:%M:%S");
    eprintln!("{time}    {}", e.kind.headline());
    eprintln!("{:>10}  What to do: {}", "", e.kind.next_step());
    eprintln!("{:>10}  details: [{}] {}", "", e.kind.id(), e.detail);
}
