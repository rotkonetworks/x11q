//! x11q - X11 display forwarding over QUIC with P2P holepunching
//!
//! Lowest latency remote desktop: X11 protocol over QUIC.
//! No video encoding - your local GPU renders everything.
//! Built-in NAT traversal via iroh holepunching.
//!
//! # Easy mode (word codes)
//!
//! ```text
//! local:  x11q serve           → prints "7-tiger-lamp"
//! remote: x11q join 7-tiger-lamp  → DISPLAY=:99 ready
//! ```
//!
//! # Direct mode (node ids)
//!
//! ```text
//! local:  x11q server          → prints node id
//! remote: x11q client <nodeid> → DISPLAY=:99 ready
//! ```

mod mirror;
mod rendezvous;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use iroh::{Endpoint, NodeId};
use std::str::FromStr;
use std::sync::Arc;
use tokio::io;
use tokio::net::{TcpListener, TcpStream, UnixListener, UnixStream};

const X11_UNIX_DIR: &str = "/tmp/.X11-unix";
const X11_TCP_BASE: u16 = 6000;
const ALPN: &[u8] = b"x11quic/1";

#[derive(Parser)]
#[command(name = "x11q")]
#[command(about = "X11 display forwarding over QUIC with P2P holepunching")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Easy mode: serve X11 with a word code (e.g., "7-tiger-lamp")
    /// Publishes to DHT, authenticates with PAKE
    Serve {
        /// Local X display to forward (e.g., :0)
        #[arg(short, long, default_value = ":0")]
        display: String,
    },

    /// Easy mode: join using a word code
    /// Looks up DHT, authenticates with PAKE, creates DISPLAY=:99
    Join {
        /// Word code from server (e.g., "7-tiger-lamp")
        code: String,

        /// Virtual display number to create
        #[arg(short, long, default_value = "99")]
        display: u32,
    },

    /// Direct mode: server (use node id instead of word code)
    Server {
        /// Local X display to forward to (e.g., :0)
        #[arg(short, long, default_value = ":0")]
        display: String,

        /// Optional bind address (e.g., 0.0.0.0:5000)
        #[arg(short, long)]
        bind: Option<String>,
    },

    /// Direct mode: client (use node id instead of word code)
    Client {
        /// Server NodeId (base32 public key)
        #[arg(value_name = "NODE_ID")]
        node_id: String,

        /// Virtual display number to create
        #[arg(short, long, default_value = "99")]
        display: u32,

        /// Direct address hint (optional)
        #[arg(long)]
        addr: Option<String>,
    },

    /// Show node identity
    Id,

    /// Share your screen (mirror server)
    /// Captures display and streams to connected viewers
    #[command(name = "mirror-server")]
    MirrorServer {
        /// X display to capture (e.g., :0)
        #[arg(short, long, default_value = ":0")]
        display: String,

        /// Optional bind address
        #[arg(short, long)]
        bind: Option<String>,
    },

    /// View a remote screen (mirror client)
    /// Connects to mirror-server and displays in a window
    Mirror {
        /// Server NodeId (base32 public key)
        #[arg(value_name = "NODE_ID")]
        node_id: String,

        /// Direct address hint (optional)
        #[arg(long)]
        addr: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("iroh=warn".parse().unwrap())
                .add_directive("quinn=warn".parse().unwrap()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Serve { display } => run_serve(&display).await,
        Commands::Join { code, display } => run_join(&code, display).await,
        Commands::Server { display, bind } => run_server(&display, bind.as_deref()).await,
        Commands::Client { node_id, display, addr } => {
            run_client(&node_id, display, addr.as_deref()).await
        }
        Commands::Id => {
            let endpoint = Endpoint::builder().alpns(vec![ALPN.to_vec()]).bind().await?;
            println!("{}", endpoint.node_id());
            endpoint.close().await;
            Ok(())
        }
        Commands::MirrorServer { display, bind } => {
            mirror::run_mirror_server(&display, bind.as_deref()).await
        }
        Commands::Mirror { node_id, addr } => {
            mirror::run_mirror_client(&node_id, addr.as_deref()).await
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

/// Parse NodeId from string
fn parse_node_id(s: &str) -> Result<NodeId> {
    NodeId::from_str(s).context("invalid node id (expected base32 public key)")
}

// Easy mode: serve with word code + PAKE
async fn run_serve(display: &str) -> Result<()> {
    let display_num = parse_display(display)?;
    let (x11_socket, x11_tcp, use_unix) = x11_paths(display_num);

    // generate word code and publish to dht
    let code = rendezvous::generate_code();

    let endpoint = Endpoint::builder()
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await?;

    let node_id = endpoint.node_id();

    eprintln!("publishing to dht...");
    rendezvous::publish_nodeid(&code, node_id).await?;

    eprintln!();
    eprintln!("  x11q join {}", code);
    eprintln!();
    eprintln!("waiting for connection...");

    // accept connection
    let incoming = endpoint.accept().await.context("no incoming connection")?;
    let conn = incoming.await?;

    // do pake handshake
    let pake = rendezvous::PakeServer::new(&code);
    let (mut send, mut recv) = conn.open_bi().await?;

    // send our pake message
    let msg = pake.message();
    send.write_all(&(msg.len() as u32).to_le_bytes()).await?;
    send.write_all(msg).await?;

    // receive client's pake message
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut client_msg = vec![0u8; len];
    recv.read_exact(&mut client_msg).await?;

    // verify pake
    let _shared_key = pake.finish(&client_msg)?;
    eprintln!("authenticated!");

    eprintln!(
        "X11: {} ({})",
        if use_unix { &x11_socket } else { &x11_tcp },
        if use_unix { "unix" } else { "tcp" }
    );

    // now proxy x11
    let remote_id = conn.remote_node_id()?;
    handle_server_connection(conn, &x11_socket, &x11_tcp, use_unix, remote_id).await
}

// Easy mode: join with word code + PAKE
async fn run_join(code: &str, display_num: u32) -> Result<()> {
    eprintln!("looking up {} on dht...", code);

    let remote_node_id = rendezvous::resolve_nodeid(code).await?;
    eprintln!("found node: {}", &remote_node_id.to_string()[..8]);

    let endpoint = Endpoint::builder()
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await?;

    let node_addr = iroh::NodeAddr::new(remote_node_id);
    let conn = endpoint.connect(node_addr, ALPN).await?;

    // do pake handshake
    let pake = rendezvous::PakeClient::new(code);
    let (mut send, mut recv) = conn.accept_bi().await?;

    // receive server's pake message
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut server_msg = vec![0u8; len];
    recv.read_exact(&mut server_msg).await?;

    // send our pake message
    let msg = pake.message();
    send.write_all(&(msg.len() as u32).to_le_bytes()).await?;
    send.write_all(msg).await?;

    // verify pake
    let _shared_key = pake.finish(&server_msg)?;
    eprintln!("authenticated!");

    let conn = Arc::new(conn);
    let (unix_listener, tcp_listener) = create_x11_listeners(display_num).await?;

    eprintln!("DISPLAY=:{} ready", display_num);

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

// Server: runs on local machine with display
async fn run_server(display: &str, bind: Option<&str>) -> Result<()> {
    let display_num = parse_display(display)?;
    let (x11_socket, x11_tcp, use_unix) = x11_paths(display_num);

    eprintln!(
        "X11 target: {} ({})",
        if use_unix { &x11_socket } else { &x11_tcp },
        if use_unix { "unix" } else { "tcp" }
    );

    // Build endpoint with optional bind address
    let mut builder = Endpoint::builder().alpns(vec![ALPN.to_vec()]);

    if let Some(addr) = bind {
        builder = builder.bind_addr_v4(addr.parse().context("invalid bind address")?);
    }

    let endpoint = builder.bind().await?;

    eprintln!("x11q server started");
    eprintln!("node id: {}", endpoint.node_id());

    // Wait for relay connection and print info
    let relay_url = endpoint.home_relay().initialized().await?;
    eprintln!("relay: {}", relay_url);
    eprintln!("holepunching ready - waiting for connections...");
    eprintln!();
    eprintln!("connect with: x11q client {}", endpoint.node_id());

    // Accept connections
    while let Some(incoming) = endpoint.accept().await {
        let conn = incoming.await?;
        let remote_id = conn.remote_node_id()?;
        eprintln!("[{}] connected", &remote_id.to_string()[..8]);

        let x11_socket = x11_socket.clone();
        let x11_tcp = x11_tcp.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_server_connection(conn, &x11_socket, &x11_tcp, use_unix, remote_id).await {
                eprintln!("[{}] error: {e}", &remote_id.to_string()[..8]);
            }
        });
    }

    Ok(())
}

async fn handle_server_connection(
    conn: iroh::endpoint::Connection,
    x11_socket: &str,
    x11_tcp: &str,
    use_unix: bool,
    remote_id: NodeId,
) -> Result<()> {
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

    eprintln!("[{}] disconnected", &remote_id.to_string()[..8]);
    Ok(())
}

// Client: runs on remote machine, creates virtual display
async fn run_client(node_id: &str, display_num: u32, addr_hint: Option<&str>) -> Result<()> {
    let remote_node_id = parse_node_id(node_id)?;

    let endpoint = Endpoint::builder()
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await?;

    eprintln!("connecting to {}...", &node_id[..8.min(node_id.len())]);

    // Build node address
    let mut node_addr = iroh::NodeAddr::new(remote_node_id);

    // Add direct address hint if provided
    if let Some(addr) = addr_hint {
        node_addr = node_addr.with_direct_addresses([addr.parse().context("invalid address hint")?]);
    }

    // Connect (iroh handles holepunching automatically)
    let conn = endpoint.connect(node_addr, ALPN).await?;
    let remote_id = conn.remote_node_id()?;
    let conn = Arc::new(conn);

    eprintln!("connected to {}", &remote_id.to_string()[..8]);

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

// Helper functions

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
    mut quic_send: iroh::endpoint::SendStream,
    mut quic_recv: iroh::endpoint::RecvStream,
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
    mut quic_send: iroh::endpoint::SendStream,
    mut quic_recv: iroh::endpoint::RecvStream,
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

async fn forward_to_quic_unix(unix: UnixStream, conn: Arc<iroh::endpoint::Connection>) -> Result<()> {
    let (quic_send, quic_recv) = conn.open_bi().await?;
    let (mut unix_read, mut unix_write) = unix.into_split();
    let (mut quic_send, mut quic_recv) = (quic_send, quic_recv);

    tokio::select! {
        r = io::copy(&mut unix_read, &mut quic_send) => { r?; }
        r = io::copy(&mut quic_recv, &mut unix_write) => { r?; }
    }
    Ok(())
}

async fn forward_to_quic_tcp(tcp: TcpStream, conn: Arc<iroh::endpoint::Connection>) -> Result<()> {
    let (quic_send, quic_recv) = conn.open_bi().await?;
    let (mut tcp_read, mut tcp_write) = tcp.into_split();
    let (mut quic_send, mut quic_recv) = (quic_send, quic_recv);

    tokio::select! {
        r = io::copy(&mut tcp_read, &mut quic_send) => { r?; }
        r = io::copy(&mut quic_recv, &mut tcp_write) => { r?; }
    }
    Ok(())
}
