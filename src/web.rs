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
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;

const X11_UNIX_DIR: &str = "/tmp/.X11-unix";
const X11_TCP_BASE: u16 = 6000;

struct X11Bridge {
    /// Data from X11 clients to browser
    to_browser: broadcast::Sender<Vec<u8>>,
    /// Data from browser to X11 clients
    from_browser: broadcast::Sender<Vec<u8>>,
}

/// Run web server mode - serves x11q-web and bridges X11 connections
pub async fn run_web(display_num: u32, port: u16, www_path: Option<&str>) -> Result<()> {
    // Create X11 listeners for this display
    let x11_socket = format!("{}/X{}", X11_UNIX_DIR, display_num);
    let _ = std::fs::remove_file(&x11_socket);
    std::fs::create_dir_all(X11_UNIX_DIR).ok();

    let unix_listener =
        UnixListener::bind(&x11_socket).context("failed to create X11 socket")?;

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

    // Create broadcast channels for X11 <-> browser communication
    let (to_browser_tx, _) = broadcast::channel::<Vec<u8>>(1024);
    let (from_browser_tx, _) = broadcast::channel::<Vec<u8>>(1024);

    let bridge = Arc::new(X11Bridge {
        to_browser: to_browser_tx,
        from_browser: from_browser_tx,
    });

    // Clone for handlers
    let bridge_for_ws = Arc::clone(&bridge);
    let bridge_for_unix = Arc::clone(&bridge);
    let bridge_for_tcp = Arc::clone(&bridge);

    // Build router
    let app = Router::new()
        .route("/ws", get(move |ws: WebSocketUpgrade| {
            let b = Arc::clone(&bridge_for_ws);
            async move { ws.on_upgrade(move |socket| handle_websocket(socket, b)) }
        }))
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

    // Subscribe to X11 data
    let mut x11_rx = bridge.to_browser.subscribe();
    let browser_tx = bridge.from_browser.clone();

    // Task to send X11 data to browser
    let send_task = tokio::spawn(async move {
        loop {
            match x11_rx.recv().await {
                Ok(data) => {
                    if ws_tx.send(Message::Binary(data.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Task to receive browser responses
    let recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_rx.next().await {
            match msg {
                Message::Binary(data) => {
                    let _ = browser_tx.send(data.to_vec());
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

    let mut browser_rx = bridge.from_browser.subscribe();
    let to_browser = bridge.to_browser.clone();

    let mut buf = vec![0u8; 65536];

    loop {
        tokio::select! {
            // Read from X11 client, send to browser
            result = stream.read(&mut buf) => {
                match result {
                    Ok(0) => break,
                    Ok(n) => {
                        let _ = to_browser.send(buf[..n].to_vec());
                    }
                    Err(_) => break,
                }
            }
            // Read from browser, send to X11 client
            result = browser_rx.recv() => {
                match result {
                    Ok(data) => {
                        if stream.write_all(&data).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    eprintln!("x11 client disconnected");
}

async fn handle_x11_tcp(mut stream: TcpStream, bridge: Arc<X11Bridge>) {
    eprintln!("x11 client connected (tcp)");

    let mut browser_rx = bridge.from_browser.subscribe();
    let to_browser = bridge.to_browser.clone();

    let mut buf = vec![0u8; 65536];

    loop {
        tokio::select! {
            // Read from X11 client, send to browser
            result = stream.read(&mut buf) => {
                match result {
                    Ok(0) => break,
                    Ok(n) => {
                        let _ = to_browser.send(buf[..n].to_vec());
                    }
                    Err(_) => break,
                }
            }
            // Read from browser, send to X11 client
            result = browser_rx.recv() => {
                match result {
                    Ok(data) => {
                        if stream.write_all(&data).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    eprintln!("x11 client disconnected");
}
