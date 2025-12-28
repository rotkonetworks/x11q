//! GUI mode - cross-platform connection dialog with egui
//!
//! Provides a simple native window for entering join codes,
//! then connects to remote x11q server and renders the display.

use anyhow::{Context, Result};
use eframe::egui;
use iroh::Endpoint;
use std::sync::Arc;
use tokio::io;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};

const ALPN: &[u8] = b"x11quic/1";
const X11_TCP_BASE: u16 = 6000;
#[cfg(unix)]
const X11_UNIX_DIR: &str = "/tmp/.X11-unix";

/// Connection state for the GUI
#[derive(Clone, PartialEq)]
enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Error(String),
}

/// GUI application state
struct X11qApp {
    code: String,
    display_num: u32,
    state: ConnectionState,
    status_msg: String,
    tx: mpsc::Sender<GuiCommand>,
    rx: mpsc::Receiver<GuiEvent>,
}

enum GuiCommand {
    Connect { code: String, display_num: u32 },
    Disconnect,
}

enum GuiEvent {
    Connecting,
    LookingUp,
    Found(String),
    Authenticating,
    Connected,
    Error(String),
}

impl X11qApp {
    fn new(
        _cc: &eframe::CreationContext<'_>,
        tx: mpsc::Sender<GuiCommand>,
        rx: mpsc::Receiver<GuiEvent>,
    ) -> Self {
        Self {
            code: String::new(),
            display_num: 99,
            state: ConnectionState::Disconnected,
            status_msg: String::new(),
            tx,
            rx,
        }
    }
}

impl eframe::App for X11qApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Process events from background task
        while let Ok(event) = self.rx.try_recv() {
            match event {
                GuiEvent::Connecting => {
                    self.state = ConnectionState::Connecting;
                    self.status_msg = "connecting...".into();
                }
                GuiEvent::LookingUp => {
                    self.status_msg = "looking up on dht...".into();
                }
                GuiEvent::Found(node) => {
                    self.status_msg = format!("found node: {}...", &node[..8.min(node.len())]);
                }
                GuiEvent::Authenticating => {
                    self.status_msg = "authenticating...".into();
                }
                GuiEvent::Connected => {
                    self.state = ConnectionState::Connected;
                    self.status_msg = format!("connected! DISPLAY=:{}", self.display_num);
                }
                GuiEvent::Error(e) => {
                    self.state = ConnectionState::Error(e.clone());
                    self.status_msg = e;
                }
            }
        }

        // Request repaint to keep receiving events
        if self.state == ConnectionState::Connecting {
            ctx.request_repaint();
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(20.0);
                ui.heading("x11q");
                ui.add_space(20.0);

                match &self.state {
                    ConnectionState::Disconnected | ConnectionState::Error(_) => {
                        ui.label("enter join code:");
                        ui.add_space(8.0);

                        let text_edit = egui::TextEdit::singleline(&mut self.code)
                            .hint_text("e.g. 7-tiger-lamp")
                            .desired_width(200.0)
                            .font(egui::TextStyle::Monospace);
                        let response = ui.add(text_edit);

                        ui.add_space(8.0);

                        ui.horizontal(|ui| {
                            ui.label("display:");
                            ui.add(
                                egui::DragValue::new(&mut self.display_num)
                                    .range(0..=255)
                                    .speed(1),
                            );
                        });

                        ui.add_space(16.0);

                        let connect_btn =
                            ui.add_enabled(!self.code.is_empty(), egui::Button::new("connect"));

                        // Connect on button click or Enter key
                        if connect_btn.clicked()
                            || (response.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                        {
                            let _ = self.tx.try_send(GuiCommand::Connect {
                                code: self.code.clone(),
                                display_num: self.display_num,
                            });
                        }

                        if let ConnectionState::Error(e) = &self.state {
                            ui.add_space(12.0);
                            ui.colored_label(egui::Color32::RED, e);
                        }
                    }
                    ConnectionState::Connecting => {
                        ui.spinner();
                        ui.add_space(8.0);
                        ui.label(&self.status_msg);
                    }
                    ConnectionState::Connected => {
                        ui.colored_label(egui::Color32::GREEN, &self.status_msg);
                        ui.add_space(16.0);
                        ui.label("x11 apps can now connect to this display");
                        ui.add_space(16.0);
                        if ui.button("disconnect").clicked() {
                            let _ = self.tx.try_send(GuiCommand::Disconnect);
                            self.state = ConnectionState::Disconnected;
                            self.status_msg.clear();
                        }
                    }
                }
            });
        });
    }
}

/// Run the GUI application
pub fn run_gui() -> Result<()> {
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<GuiCommand>(16);
    let (evt_tx, evt_rx) = mpsc::channel::<GuiEvent>(16);

    // Spawn tokio runtime for background networking
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    GuiCommand::Connect { code, display_num } => {
                        let evt_tx = evt_tx.clone();
                        tokio::spawn(async move {
                            if let Err(e) = do_connect(&code, display_num, evt_tx.clone()).await {
                                let _ = evt_tx.send(GuiEvent::Error(e.to_string())).await;
                            }
                        });
                    }
                    GuiCommand::Disconnect => {
                        // TODO: graceful disconnect
                    }
                }
            }
        });
    });

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([320.0, 240.0])
            .with_min_inner_size([280.0, 200.0]),
        ..Default::default()
    };

    eframe::run_native(
        "x11q",
        options,
        Box::new(|cc| Ok(Box::new(X11qApp::new(cc, cmd_tx, evt_rx)))),
    )
    .map_err(|e| anyhow::anyhow!("gui error: {}", e))
}

/// Perform connection in background
async fn do_connect(code: &str, display_num: u32, tx: mpsc::Sender<GuiEvent>) -> Result<()> {
    tx.send(GuiEvent::Connecting).await.ok();
    tx.send(GuiEvent::LookingUp).await.ok();

    let remote_node_id = crate::rendezvous::resolve_nodeid(code).await?;
    tx.send(GuiEvent::Found(remote_node_id.to_string()))
        .await
        .ok();

    let endpoint = Endpoint::builder()
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await?;

    let node_addr = iroh::NodeAddr::new(remote_node_id);
    let conn = endpoint.connect(node_addr, ALPN).await?;

    tx.send(GuiEvent::Authenticating).await.ok();

    // PAKE handshake
    let pake = crate::rendezvous::PakeClient::new(code);
    let (mut send, mut recv) = conn.accept_bi().await?;

    // Receive server's PAKE message
    let mut len_buf = [0u8; 4];
    tokio::io::AsyncReadExt::read_exact(&mut recv, &mut len_buf).await?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut server_msg = vec![0u8; len];
    tokio::io::AsyncReadExt::read_exact(&mut recv, &mut server_msg).await?;

    // Send our PAKE message
    let msg = pake.message();
    tokio::io::AsyncWriteExt::write_all(&mut send, &(msg.len() as u32).to_le_bytes()).await?;
    tokio::io::AsyncWriteExt::write_all(&mut send, msg).await?;

    // Verify PAKE
    let _shared_key = pake.finish(&server_msg)?;

    tx.send(GuiEvent::Connected).await.ok();

    let conn = Arc::new(conn);

    // Start X11 listener
    #[cfg(unix)]
    {
        let (unix_listener, tcp_listener) = create_x11_listeners(display_num).await?;

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

    #[cfg(not(unix))]
    {
        let tcp_listener = create_x11_listener_tcp(display_num).await?;

        loop {
            let (stream, _) = tcp_listener.accept().await?;
            let conn = Arc::clone(&conn);
            tokio::spawn(async move {
                if let Err(e) = forward_to_quic_tcp(stream, conn).await {
                    eprintln!("x11 error: {e}");
                }
            });
        }
    }
}

#[cfg(unix)]
async fn forward_to_quic_unix(
    unix: UnixStream,
    conn: Arc<iroh::endpoint::Connection>,
) -> Result<()> {
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

#[cfg(unix)]
async fn create_x11_listeners(display_num: u32) -> Result<(UnixListener, TcpListener)> {
    let socket_path = format!("{}/X{}", X11_UNIX_DIR, display_num);
    let _ = std::fs::remove_file(&socket_path);
    std::fs::create_dir_all(X11_UNIX_DIR).ok();

    let unix_listener =
        UnixListener::bind(&socket_path).context("failed to create X11 unix socket")?;

    let tcp_port = X11_TCP_BASE + display_num as u16;
    let tcp_listener = TcpListener::bind(format!("127.0.0.1:{}", tcp_port))
        .await
        .context("failed to bind X11 TCP port")?;

    Ok((unix_listener, tcp_listener))
}

#[cfg(not(unix))]
async fn create_x11_listener_tcp(display_num: u32) -> Result<TcpListener> {
    let tcp_port = X11_TCP_BASE + display_num as u16;
    let tcp_listener = TcpListener::bind(format!("127.0.0.1:{}", tcp_port))
        .await
        .context("failed to bind X11 TCP port")?;
    Ok(tcp_listener)
}
