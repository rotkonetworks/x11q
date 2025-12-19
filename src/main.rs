//! x11quic - X11 display forwarding over QUIC
//!
//! Lowest latency remote desktop: X11 protocol over QUIC.
//! No video encoding - your local GPU renders everything.
//!
//! # Modes
//!
//! **Normal mode** (you have public IP):
//! ```text
//! [remote: bspwm] → DISPLAY=:99 → x11quic client
//!      → quicnet → x11quic server → [local Xorg :0]
//! ```
//!
//! **Reverse mode** (you're behind NAT, remote has public IP):
//! ```text
//! [remote: bspwm] → DISPLAY=:99 → x11quic rserver
//!      ← quicnet ← x11quic rclient → [local Xorg :0]
//! ```

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use quicnet::{Identity, Peer, PeerId};
use std::sync::Arc;
use tokio::io;
use tokio::net::{TcpListener, TcpStream, UnixListener, UnixStream};

const X11_UNIX_DIR: &str = "/tmp/.X11-unix";
const X11_TCP_BASE: u16 = 6000;

#[derive(Parser)]
#[command(name = "x11quic")]
#[command(about = "X11 display forwarding over QUIC - lowest latency remote desktop")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run server on local machine (you have public IP)
    /// Accepts connections and forwards X11 to local display
    Server {
        /// Local X display to forward to (e.g., :0)
        #[arg(short, long, default_value = ":0")]
        display: String,

        /// Address to bind (e.g., 0.0.0.0:5000)
        #[arg(short, long, default_value = "0.0.0.0:5000")]
        bind: String,
    },

    /// Run client on remote machine (connect to server)
    /// Creates DISPLAY=:99 for apps to use
    Client {
        /// Server address as PEERID@host:port
        #[arg(value_name = "PEER@HOST:PORT")]
        target: String,

        /// Virtual display number to create
        #[arg(short, long, default_value = "99")]
        display: u32,
    },

    /// Run reverse server on remote machine (remote has public IP)
    /// Creates DISPLAY=:99 for apps, waits for rclient to connect
    #[command(name = "rserver")]
    ReverseServer {
        /// Address to bind (use public IP for proper routing)
        #[arg(short, long, default_value = "0.0.0.0:5000")]
        bind: String,

        /// Virtual display number to create
        #[arg(short, long, default_value = "99")]
        display: u32,
    },

    /// Run reverse client on local machine (you're behind NAT)
    /// Dials out to rserver and forwards X11 to local display
    #[command(name = "rclient")]
    ReverseClient {
        /// Remote rserver address as PEERID@host:port
        #[arg(value_name = "PEER@HOST:PORT")]
        target: String,

        /// Local X display to forward to (e.g., :0)
        #[arg(short, long, default_value = ":0")]
        display: String,
    },

    /// Show peer identity (generates if needed)
    Id,
}

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install crypto provider");

    let cli = Cli::parse();

    match cli.command {
        Commands::Server { display, bind } => run_server(&display, &bind).await,
        Commands::Client { target, display } => run_client(&target, display).await,
        Commands::ReverseServer { bind, display } => run_reverse_server(&bind, display).await,
        Commands::ReverseClient { target, display } => run_reverse_client(&target, &display).await,
        Commands::Id => {
            let identity = Identity::load_or_generate()?;
            println!("{}", identity.peer_id());
            Ok(())
        }
    }
}

fn parse_display(display: &str) -> Result<u32> {
    display
        .trim_start_matches(':')
        .parse()
        .context("invalid display number")
}

fn x11_paths(display_num: u32) -> (String, String, bool) {
    let socket = format!("{}/X{}", X11_UNIX_DIR, display_num);
    let tcp = format!("127.0.0.1:{}", X11_TCP_BASE + display_num as u16);
    let use_unix = std::path::Path::new(&socket).exists();
    (socket, tcp, use_unix)
}

// Server: runs on local machine (with public IP and monitor)
async fn run_server(display: &str, bind: &str) -> Result<()> {
    let display_num = parse_display(display)?;
    let (x11_socket, x11_tcp, use_unix) = x11_paths(display_num);

    eprintln!(
        "X11 target: {} ({})",
        if use_unix { &x11_socket } else { &x11_tcp },
        if use_unix { "unix" } else { "tcp" }
    );

    let identity = Identity::load_or_generate()?;
    let peer = Peer::new(bind.parse()?, identity)?;

    eprintln!("x11quic server on {}", peer.local_addr()?);
    eprintln!("peer id: {}", peer.identity().peer_id());
    eprintln!("waiting for remote to connect...");

    while let Some(incoming) = peer.accept().await {
        let x11_socket = x11_socket.clone();
        let x11_tcp = x11_tcp.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_server_connection(incoming, &x11_socket, &x11_tcp, use_unix).await
            {
                eprintln!("session error: {e}");
            }
        });
    }
    Ok(())
}

async fn handle_server_connection(
    incoming: quicnet::IncomingConnection,
    x11_socket: &str,
    x11_tcp: &str,
    use_unix: bool,
) -> Result<()> {
    let (conn, peer_id) = incoming.accept().await?;
    eprintln!("[{}] connected", peer_id.short());

    loop {
        let (quic_send, quic_recv) = match conn.accept_bi().await {
            Ok(s) => s,
            Err(_) => break,
        };

        let x11_socket = x11_socket.to_string();
        let x11_tcp = x11_tcp.to_string();

        tokio::spawn(async move {
            let result = if use_unix {
                proxy_to_unix(quic_send, quic_recv, &x11_socket).await
            } else {
                proxy_to_tcp(quic_send, quic_recv, &x11_tcp).await
            };
            if let Err(e) = result {
                eprintln!("stream error: {e}");
            }
        });
    }

    eprintln!("[{}] disconnected", peer_id.short());
    Ok(())
}

// Client: runs on remote machine, creates virtual display
async fn run_client(target: &str, display_num: u32) -> Result<()> {
    let (peer_id, addr) = parse_target(target)?;

    let identity = Identity::load_or_generate()?;
    let peer = Peer::new("[::]:0".parse()?, identity)?;

    eprintln!("connecting to {} @ {}", peer_id.short(), addr);

    let (conn, remote_id) = peer.dial(addr.parse()?, Some(&peer_id)).await?;
    let conn = Arc::new(conn);
    eprintln!("connected to {}", remote_id.short());

    let (unix_listener, tcp_listener) = create_x11_listeners(display_num).await?;

    eprintln!("X11 proxy ready - DISPLAY=:{}", display_num);

    loop {
        tokio::select! {
            Ok((stream, _)) = unix_listener.accept() => {
                let conn = Arc::clone(&conn);
                tokio::spawn(async move {
                    if let Err(e) = forward_to_quic_unix(stream, conn).await {
                        eprintln!("x11 error: {e}");
                    }
                });
            }
            Ok((stream, _)) = tcp_listener.accept() => {
                let conn = Arc::clone(&conn);
                tokio::spawn(async move {
                    if let Err(e) = forward_to_quic_tcp(stream, conn).await {
                        eprintln!("x11 error: {e}");
                    }
                });
            }
        }
    }
}

// Reverse server: runs on remote (has public IP, runs bspwm)
async fn run_reverse_server(bind: &str, display_num: u32) -> Result<()> {
    let (unix_listener, tcp_listener) = create_x11_listeners(display_num).await?;

    let identity = Identity::load_or_generate()?;
    let peer = Peer::new(bind.parse()?, identity)?;

    eprintln!("x11quic rserver on {}", peer.local_addr()?);
    eprintln!("peer id: {}", peer.identity().peer_id());
    eprintln!();
    eprintln!("X11 ready: DISPLAY=:{}", display_num);

    loop {
        eprintln!("waiting for rclient...");

        let incoming = match peer.accept().await {
            Some(i) => i,
            None => {
                eprintln!("accept failed, retrying...");
                continue;
            }
        };

        let (conn, peer_id) = match incoming.accept().await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("connection error: {e}");
                continue;
            }
        };

        let conn = Arc::new(conn);
        eprintln!("[{}] connected - X11 forwarding active", peer_id.short());

        loop {
            tokio::select! {
                result = unix_listener.accept() => {
                    if let Ok((stream, _)) = result {
                        let conn = Arc::clone(&conn);
                        tokio::spawn(async move {
                            if let Err(e) = forward_to_quic_unix(stream, conn).await {
                                eprintln!("x11 error: {e}");
                            }
                        });
                    }
                }
                result = tcp_listener.accept() => {
                    if let Ok((stream, _)) = result {
                        let conn = Arc::clone(&conn);
                        tokio::spawn(async move {
                            if let Err(e) = forward_to_quic_tcp(stream, conn).await {
                                eprintln!("x11 error: {e}");
                            }
                        });
                    }
                }
            }

            if conn.close_reason().is_some() {
                eprintln!("[{}] disconnected", peer_id.short());
                break;
            }
        }
    }
}

// Reverse client: runs on local machine (behind NAT, has monitor)
async fn run_reverse_client(target: &str, display: &str) -> Result<()> {
    let display_num = parse_display(display)?;
    let (x11_socket, x11_tcp, use_unix) = x11_paths(display_num);

    eprintln!(
        "X11 target: {} ({})",
        if use_unix { &x11_socket } else { &x11_tcp },
        if use_unix { "unix" } else { "tcp" }
    );

    let (peer_id, addr) = parse_target(target)?;

    let identity = Identity::load_or_generate()?;
    let peer = Peer::new("[::]:0".parse()?, identity)?;

    eprintln!("connecting to {} @ {}", peer_id.short(), addr);

    let (conn, remote_id) = peer.dial(addr.parse()?, Some(&peer_id)).await?;
    eprintln!("connected to {} - forwarding X11 to :{}", remote_id.short(), display_num);

    loop {
        let (quic_send, quic_recv) = match conn.accept_bi().await {
            Ok(s) => s,
            Err(_) => break,
        };

        let x11_socket = x11_socket.clone();
        let x11_tcp = x11_tcp.clone();

        tokio::spawn(async move {
            let result = if use_unix {
                proxy_to_unix(quic_send, quic_recv, &x11_socket).await
            } else {
                proxy_to_tcp(quic_send, quic_recv, &x11_tcp).await
            };
            if let Err(e) = result {
                eprintln!("stream error: {e}");
            }
        });
    }

    eprintln!("connection closed");
    Ok(())
}

// Helper functions

fn parse_target(target: &str) -> Result<(PeerId, &str)> {
    let (peer_id_str, addr) = target
        .split_once('@')
        .ok_or_else(|| anyhow::anyhow!("target must be PEERID@host:port"))?;
    let peer_id: PeerId = peer_id_str.parse().context("invalid peer id")?;
    Ok((peer_id, addr))
}

async fn create_x11_listeners(display_num: u32) -> Result<(UnixListener, TcpListener)> {
    let x11_socket = format!("{}/X{}", X11_UNIX_DIR, display_num);
    let _ = std::fs::remove_file(&x11_socket);
    std::fs::create_dir_all(X11_UNIX_DIR).ok();

    let unix_listener =
        UnixListener::bind(&x11_socket).context("failed to create X11 socket")?;

    let tcp_port = X11_TCP_BASE + display_num as u16;
    let tcp_listener = TcpListener::bind(format!("127.0.0.1:{}", tcp_port))
        .await
        .context("failed to bind X11 TCP port")?;

    Ok((unix_listener, tcp_listener))
}

async fn proxy_to_unix(
    mut quic_send: quinn::SendStream,
    mut quic_recv: quinn::RecvStream,
    socket_path: &str,
) -> Result<()> {
    let unix = UnixStream::connect(socket_path).await?;
    let (mut unix_read, mut unix_write) = unix.into_split();

    tokio::select! {
        r = io::copy(&mut quic_recv, &mut unix_write) => { r?; }
        r = io::copy(&mut unix_read, &mut quic_send) => { r?; }
    }
    Ok(())
}

async fn proxy_to_tcp(
    mut quic_send: quinn::SendStream,
    mut quic_recv: quinn::RecvStream,
    addr: &str,
) -> Result<()> {
    let tcp = TcpStream::connect(addr).await?;
    let (mut tcp_read, mut tcp_write) = tcp.into_split();

    tokio::select! {
        r = io::copy(&mut quic_recv, &mut tcp_write) => { r?; }
        r = io::copy(&mut tcp_read, &mut quic_send) => { r?; }
    }
    Ok(())
}

async fn forward_to_quic_unix(unix: UnixStream, conn: Arc<quinn::Connection>) -> Result<()> {
    let (quic_send, quic_recv) = conn.open_bi().await?;
    let (mut unix_read, mut unix_write) = unix.into_split();
    let (mut quic_send, mut quic_recv) = (quic_send, quic_recv);

    tokio::select! {
        r = io::copy(&mut unix_read, &mut quic_send) => { r?; }
        r = io::copy(&mut quic_recv, &mut unix_write) => { r?; }
    }
    Ok(())
}

async fn forward_to_quic_tcp(tcp: TcpStream, conn: Arc<quinn::Connection>) -> Result<()> {
    let (quic_send, quic_recv) = conn.open_bi().await?;
    let (mut tcp_read, mut tcp_write) = tcp.into_split();
    let (mut quic_send, mut quic_recv) = (quic_send, quic_recv);

    tokio::select! {
        r = io::copy(&mut tcp_read, &mut quic_send) => { r?; }
        r = io::copy(&mut quic_recv, &mut tcp_write) => { r?; }
    }
    Ok(())
}
