//! Web server for x11q-web browser client
//!
//! Serves static files and provides WebSocket bridge for X11 protocol.
//! Browser runs x11q-web (X11 server in wasm), connects here via WebSocket,
//! and this proxy accepts local X11 client connections and bridges them.

use anyhow::{Context, Result};
use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UnixListener, UnixStream};
use tokio::sync::{mpsc, Mutex};
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;

const X11_UNIX_DIR: &str = "/tmp/.X11-unix";
const X11_TCP_BASE: u16 = 6000;

/// Shared state for single-client X11 connection
struct X11Bridge {
    /// Send data to browser (X11 requests from client)
    to_browser_tx: mpsc::Sender<Vec<u8>>,
    /// Receive data from browser (X11 replies/events)
    from_browser_rx: Arc<Mutex<mpsc::Receiver<Vec<u8>>>>,
    /// Send data from browser
    from_browser_tx: mpsc::Sender<Vec<u8>>,
    /// Receive data for browser
    to_browser_rx: Arc<Mutex<mpsc::Receiver<Vec<u8>>>>,
}

impl X11Bridge {
    fn new() -> Self {
        let (to_browser_tx, to_browser_rx) = mpsc::channel::<Vec<u8>>(1024);
        let (from_browser_tx, from_browser_rx) = mpsc::channel::<Vec<u8>>(1024);
        Self {
            to_browser_tx,
            from_browser_rx: Arc::new(Mutex::new(from_browser_rx)),
            from_browser_tx,
            to_browser_rx: Arc::new(Mutex::new(to_browser_rx)),
        }
    }
}

/// Run web server mode - serves x11q-web and bridges X11 connections
pub async fn run_web(display_num: u32, port: u16, www_path: Option<&str>) -> Result<()> {
    // Create X11 listeners for this display
    let x11_socket = format!("{}/X{}", X11_UNIX_DIR, display_num);
    let _ = std::fs::remove_file(&x11_socket);
    std::fs::create_dir_all(X11_UNIX_DIR).ok();

    let unix_listener = UnixListener::bind(&x11_socket).context("failed to create X11 socket")?;

    let tcp_port = X11_TCP_BASE + display_num as u16;
    let tcp_listener = TcpListener::bind(format!("127.0.0.1:{}", tcp_port))
        .await
        .context("failed to bind X11 TCP port")?;

    eprintln!("x11 server listening:");
    eprintln!("  unix: {}", x11_socket);
    eprintln!("  tcp:  127.0.0.1:{}", tcp_port);
    eprintln!();
    eprintln!("export DISPLAY=:{}", display_num);
    eprintln!();

    let bridge = Arc::new(X11Bridge::new());

    let bridge_for_ws = Arc::clone(&bridge);
    let bridge_for_unix = Arc::clone(&bridge);
    let bridge_for_tcp = Arc::clone(&bridge);

    // Build router
    let app = Router::new()
        .route(
            "/ws",
            get(move |ws: WebSocketUpgrade| {
                let b = Arc::clone(&bridge_for_ws);
                async move { ws.on_upgrade(move |socket| handle_websocket(socket, b)) }
            }),
        )
        .layer(CorsLayer::permissive());

    // Add static file serving
    let app = if let Some(path) = www_path {
        app.fallback_service(ServeDir::new(path))
    } else {
        app.fallback_service(ServeDir::new("x11q-web/www"))
    };

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("web server: http://localhost:{}", port);
    eprintln!("waiting for browser to connect...");

    // Spawn X11 unix listener
    tokio::spawn(async move {
        loop {
            match unix_listener.accept().await {
                Ok((stream, _)) => {
                    let b = Arc::clone(&bridge_for_unix);
                    tokio::spawn(handle_x11_unix(stream, b));
                }
                Err(e) => eprintln!("unix accept error: {}", e),
            }
        }
    });

    // Spawn X11 TCP listener
    tokio::spawn(async move {
        loop {
            match tcp_listener.accept().await {
                Ok((stream, _)) => {
                    let b = Arc::clone(&bridge_for_tcp);
                    tokio::spawn(handle_x11_tcp(stream, b));
                }
                Err(e) => eprintln!("tcp accept error: {}", e),
            }
        }
    });

    // Start web server
    axum::serve(listener, app).await?;

    Ok(())
}

async fn handle_websocket(socket: WebSocket, bridge: Arc<X11Bridge>) {
    eprintln!("browser connected");

    let (mut ws_tx, mut ws_rx) = socket.split();

    let to_browser_rx = Arc::clone(&bridge.to_browser_rx);
    let from_browser_tx = bridge.from_browser_tx.clone();

    // Task to send X11 data to browser
    let send_task = tokio::spawn(async move {
        loop {
            let data = {
                let mut rx = to_browser_rx.lock().await;
                rx.recv().await
            };
            match data {
                Some(data) => {
                    eprintln!("-> browser: {} bytes", data.len());
                    if ws_tx.send(Message::Binary(data.into())).await.is_err() {
                        break;
                    }
                }
                None => break,
            }
        }
    });

    // Task to receive browser responses
    let recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_rx.next().await {
            match msg {
                Message::Binary(data) => {
                    eprintln!("<- browser: {} bytes", data.len());
                    let _ = from_browser_tx.send(data.to_vec()).await;
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    tokio::select! {
        _ = send_task => {}
        _ = recv_task => {}
    }

    eprintln!("browser disconnected");
}

async fn handle_x11_unix(mut stream: UnixStream, bridge: Arc<X11Bridge>) {
    eprintln!("x11 client connected (unix)");

    let from_browser_rx = Arc::clone(&bridge.from_browser_rx);
    let to_browser_tx = bridge.to_browser_tx.clone();

    // Drain any old data from the browser channel
    {
        let mut rx = from_browser_rx.lock().await;
        while rx.try_recv().is_ok() {
            // discard old data
        }
    }

    let mut buf = vec![0u8; 65536];
    let mut sent_request = false;

    loop {
        tokio::select! {
            biased; // prefer reading from x11 client first

            // Read from X11 client, send to browser
            result = stream.read(&mut buf) => {
                match result {
                    Ok(0) => {
                        eprintln!("x11 client EOF");
                        break;
                    }
                    Ok(n) => {
                        eprintln!("x11 -> browser: {} bytes", n);
                        if to_browser_tx.send(buf[..n].to_vec()).await.is_err() {
                            eprintln!("failed to send to browser");
                            break;
                        }
                        sent_request = true;
                    }
                    Err(e) => {
                        eprintln!("x11 read error: {}", e);
                        break;
                    }
                }
            }
            // Read from browser, send to X11 client - only after we've sent at least one request
            result = async {
                let mut rx = from_browser_rx.lock().await;
                rx.recv().await
            }, if sent_request => {
                match result {
                    Some(data) => {
                        eprintln!("browser -> x11: {} bytes", data.len());
                        if stream.write_all(&data).await.is_err() {
                            eprintln!("x11 write error");
                            break;
                        }
                    }
                    None => {
                        eprintln!("browser channel closed");
                        break;
                    }
                }
            }
        }
    }

    eprintln!("x11 client disconnected");
}

async fn handle_x11_tcp(mut stream: TcpStream, bridge: Arc<X11Bridge>) {
    eprintln!("x11 client connected (tcp)");

    let from_browser_rx = Arc::clone(&bridge.from_browser_rx);
    let to_browser_tx = bridge.to_browser_tx.clone();

    let mut buf = vec![0u8; 65536];

    loop {
        tokio::select! {
            result = stream.read(&mut buf) => {
                match result {
                    Ok(0) => break,
                    Ok(n) => {
                        eprintln!("x11 tcp -> browser: {} bytes", n);
                        if to_browser_tx.send(buf[..n].to_vec()).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            result = async {
                let mut rx = from_browser_rx.lock().await;
                rx.recv().await
            } => {
                match result {
                    Some(data) => {
                        eprintln!("browser -> x11 tcp: {} bytes", data.len());
                        if stream.write_all(&data).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
        }
    }

    eprintln!("x11 tcp client disconnected");
}
