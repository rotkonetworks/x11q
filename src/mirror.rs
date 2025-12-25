//! Screen mirroring over QUIC with P2P holepunching
//!
//! Captures the screen, compresses, and streams over QUIC.
//! Receives input events and injects them via XTest.

use anyhow::{Context, Result};
use iroh::{Endpoint, NodeAddr, NodeId};
use minifb::{Key, MouseButton, MouseMode, Window, WindowOptions};
use std::str::FromStr;
use std::sync::Arc;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::shm;
use x11rb::protocol::xproto::{ConnectionExt, ImageFormat};
use x11rb::protocol::xtest::ConnectionExt as XTestExt;

const ALPN: &[u8] = b"x11quic-mirror/1";

// Message types for the protocol
const MSG_FRAME: u8 = 1; // Full or partial frame data
#[allow(dead_code)]
const MSG_CURSOR: u8 = 2; // Cursor position update
const MSG_KEY: u8 = 3; // Keyboard event
const MSG_MOUSE: u8 = 4; // Mouse button event
const MSG_MOTION: u8 = 5; // Mouse motion event

/// Server: captures screen and streams to client
pub async fn run_mirror_server(display: &str, bind: Option<&str>) -> Result<()> {
    let display_num: u32 = display
        .trim_start_matches(':')
        .parse()
        .context("invalid display number")?;

    // Connect to X11
    let (conn, screen_num) = x11rb::connect(Some(&format!(":{}", display_num)))
        .context("failed to connect to X display")?;
    let conn = Arc::new(conn);

    let screen = &conn.setup().roots[screen_num];
    let width = screen.width_in_pixels as u32;
    let height = screen.height_in_pixels as u32;
    let root = screen.root;

    eprintln!("Screen: {}x{}", width, height);

    // Check for SHM extension
    let shm_supported = conn
        .extension_information(shm::X11_EXTENSION_NAME)?
        .is_some();
    eprintln!(
        "SHM extension: {}",
        if shm_supported { "yes" } else { "no (slower)" }
    );

    // Set up iroh endpoint
    let mut builder = Endpoint::builder().alpns(vec![ALPN.to_vec()]);

    if let Some(addr) = bind {
        builder = builder.bind_addr_v4(addr.parse().context("invalid bind address")?);
    }

    let endpoint = builder.bind().await?;

    eprintln!("x11q mirror-server started");
    eprintln!("node id: {}", endpoint.node_id());

    // Wait for relay connection
    let relay_url = endpoint.home_relay().initialized().await?;
    eprintln!("relay: {}", relay_url);
    eprintln!("holepunching ready - waiting for viewer...");
    eprintln!();
    eprintln!("connect with: x11q mirror {}", endpoint.node_id());

    loop {
        let incoming = match endpoint.accept().await {
            Some(i) => i,
            None => continue,
        };

        let quic_conn = match incoming.await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("connection error: {e}");
                continue;
            }
        };

        let remote_id = quic_conn.remote_node_id()?;
        eprintln!("[{}] viewer connected", &remote_id.to_string()[..8]);

        let conn_clone = Arc::clone(&conn);
        tokio::spawn(async move {
            if let Err(e) = handle_viewer(quic_conn, conn_clone, root, width, height).await {
                eprintln!("viewer error: {e}");
            }
        });
    }
}

async fn handle_viewer(
    quic_conn: iroh::endpoint::Connection,
    x_conn: Arc<x11rb::rust_connection::RustConnection>,
    root: u32,
    width: u32,
    height: u32,
) -> Result<()> {
    // Open streams for video and input
    let (mut send, recv) = quic_conn.open_bi().await?;

    // Send screen dimensions
    send.write_all(&width.to_le_bytes()).await?;
    send.write_all(&height.to_le_bytes()).await?;

    // Spawn input handler
    let x_conn_input = Arc::clone(&x_conn);
    let input_handle = tokio::spawn(async move { handle_input(recv, x_conn_input).await });

    // Capture and send frames
    let mut last_frame: Vec<u8> = vec![0; (width * height * 4) as usize];
    let mut frame_count = 0u64;

    loop {
        // Capture screen
        let image = x_conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                root,
                0,
                0,
                width as u16,
                height as u16,
                !0,
            )?
            .reply()?;

        let pixels = &image.data;

        // Convert BGRA to RGBA if needed and check for changes
        let mut frame: Vec<u8> = Vec::with_capacity(pixels.len());
        let mut changed = false;

        for (i, chunk) in pixels.chunks(4).enumerate() {
            let b = chunk[0];
            let g = chunk[1];
            let r = chunk[2];
            let a = if chunk.len() > 3 { chunk[3] } else { 255 };

            let idx = i * 4;
            if !changed
                && idx + 3 < last_frame.len()
                && (last_frame[idx] != r
                    || last_frame[idx + 1] != g
                    || last_frame[idx + 2] != b
                    || last_frame[idx + 3] != a)
            {
                changed = true;
            }

            frame.push(r);
            frame.push(g);
            frame.push(b);
            frame.push(a);
        }

        if changed || frame_count == 0 {
            // Compress with zstd
            let compressed = zstd::encode_all(&frame[..], 1)?;

            // Send frame
            send.write_all(&[MSG_FRAME]).await?;
            send.write_all(&(compressed.len() as u32).to_le_bytes())
                .await?;
            send.write_all(&compressed).await?;

            last_frame = frame;

            if frame_count.is_multiple_of(60) {
                eprintln!(
                    "frame {} - {}x{} compressed {}KB",
                    frame_count,
                    width,
                    height,
                    compressed.len() / 1024
                );
            }
        }

        frame_count += 1;

        // ~30 FPS
        tokio::time::sleep(tokio::time::Duration::from_millis(33)).await;

        if quic_conn.close_reason().is_some() {
            break;
        }
    }

    input_handle.abort();
    Ok(())
}

async fn handle_input(
    mut recv: iroh::endpoint::RecvStream,
    x_conn: Arc<x11rb::rust_connection::RustConnection>,
) -> Result<()> {
    let mut buf = [0u8; 32];

    loop {
        // Read message type
        if recv.read_exact(&mut buf[..1]).await.is_err() {
            break;
        }

        match buf[0] {
            MSG_KEY => {
                recv.read_exact(&mut buf[..5]).await?;
                let keycode = buf[0];
                let pressed = buf[1] != 0;

                x_conn.xtest_fake_input(
                    if pressed { 2 } else { 3 }, // KeyPress / KeyRelease
                    keycode,
                    0,
                    x_conn.setup().roots[0].root,
                    0,
                    0,
                    0,
                )?;
                x_conn.flush()?;
            }
            MSG_MOUSE => {
                recv.read_exact(&mut buf[..2]).await?;
                let button = buf[0];
                let pressed = buf[1] != 0;

                x_conn.xtest_fake_input(
                    if pressed { 4 } else { 5 }, // ButtonPress / ButtonRelease
                    button,
                    0,
                    x_conn.setup().roots[0].root,
                    0,
                    0,
                    0,
                )?;
                x_conn.flush()?;
            }
            MSG_MOTION => {
                recv.read_exact(&mut buf[..4]).await?;
                let x = i16::from_le_bytes([buf[0], buf[1]]);
                let y = i16::from_le_bytes([buf[2], buf[3]]);

                x_conn.xtest_fake_input(
                    6, // MotionNotify
                    0,
                    0,
                    x_conn.setup().roots[0].root,
                    x,
                    y,
                    0,
                )?;
                x_conn.flush()?;
            }
            _ => {}
        }
    }

    Ok(())
}

/// Client: displays remote screen and sends input
pub async fn run_mirror_client(node_id: &str, addr_hint: Option<&str>) -> Result<()> {
    let remote_node_id = NodeId::from_str(node_id).context("invalid node id")?;

    let endpoint = Endpoint::builder()
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await?;

    eprintln!("connecting to {}...", &node_id[..8.min(node_id.len())]);

    let mut node_addr = NodeAddr::new(remote_node_id);

    if let Some(addr) = addr_hint {
        node_addr = node_addr.with_direct_addresses([addr.parse().context("invalid address")?]);
    }

    let conn = endpoint.connect(node_addr, ALPN).await?;

    let remote_id = conn.remote_node_id()?;
    eprintln!("connected to {}", &remote_id.to_string()[..8]);

    // Accept the video/input stream
    let (mut send, mut recv) = conn.accept_bi().await?;

    // Read screen dimensions
    let mut dim_buf = [0u8; 4];
    recv.read_exact(&mut dim_buf).await?;
    let width = u32::from_le_bytes(dim_buf);
    recv.read_exact(&mut dim_buf).await?;
    let height = u32::from_le_bytes(dim_buf);

    eprintln!("remote screen: {}x{}", width, height);

    // Create window
    let mut window = Window::new(
        &format!("x11quic mirror - {}x{}", width, height),
        width as usize,
        height as usize,
        WindowOptions {
            resize: true,
            scale: minifb::Scale::X1,
            ..Default::default()
        },
    )
    .context("failed to create window")?;

    window.set_target_fps(60);

    let mut buffer: Vec<u32> = vec![0; (width * height) as usize];
    let mut last_mouse_pos = (0i16, 0i16);
    let mut last_keys: Vec<Key> = vec![];

    loop {
        // Check for incoming frame
        let mut msg_type = [0u8; 1];
        match tokio::time::timeout(
            tokio::time::Duration::from_millis(5),
            recv.read_exact(&mut msg_type),
        )
        .await
        {
            Ok(Ok(_)) if msg_type[0] == MSG_FRAME => {
                // Read compressed frame size
                let mut size_buf = [0u8; 4];
                recv.read_exact(&mut size_buf).await?;
                let size = u32::from_le_bytes(size_buf) as usize;

                // Read compressed data
                let mut compressed = vec![0u8; size];
                recv.read_exact(&mut compressed).await?;

                // Decompress
                let frame = zstd::decode_all(&compressed[..])?;

                // Convert RGBA to 0RGB for minifb
                for (i, chunk) in frame.chunks(4).enumerate() {
                    if i < buffer.len() {
                        let r = chunk[0] as u32;
                        let g = chunk[1] as u32;
                        let b = chunk[2] as u32;
                        buffer[i] = (r << 16) | (g << 8) | b;
                    }
                }
            }
            Ok(Err(_)) => break, // Connection closed
            _ => {}
        }

        // Update window
        if !window.is_open() {
            break;
        }

        window.update_with_buffer(&buffer, width as usize, height as usize)?;

        // Send mouse position if changed
        if let Some((mx, my)) = window.get_mouse_pos(MouseMode::Clamp) {
            let mx = mx as i16;
            let my = my as i16;
            if mx != last_mouse_pos.0 || my != last_mouse_pos.1 {
                send.write_all(&[MSG_MOTION]).await?;
                send.write_all(&mx.to_le_bytes()).await?;
                send.write_all(&my.to_le_bytes()).await?;
                last_mouse_pos = (mx, my);
            }
        }

        // Send mouse button events
        for (button, code) in [
            (MouseButton::Left, 1u8),
            (MouseButton::Middle, 2),
            (MouseButton::Right, 3),
        ] {
            if window.get_mouse_down(button) {
                send.write_all(&[MSG_MOUSE, code, 1]).await?;
            }
        }

        // Send key events
        let keys: Vec<Key> = window.get_keys();
        for key in &keys {
            if !last_keys.contains(key) {
                if let Some(keycode) = key_to_x11_keycode(*key) {
                    send.write_all(&[MSG_KEY, keycode, 1, 0, 0]).await?;
                }
            }
        }
        for key in &last_keys {
            if !keys.contains(key) {
                if let Some(keycode) = key_to_x11_keycode(*key) {
                    send.write_all(&[MSG_KEY, keycode, 0, 0, 0]).await?;
                }
            }
        }
        last_keys = keys;
    }

    eprintln!("mirror closed");
    Ok(())
}

fn key_to_x11_keycode(key: Key) -> Option<u8> {
    // Map minifb keys to X11 keycodes
    Some(match key {
        Key::A => 38,
        Key::B => 56,
        Key::C => 54,
        Key::D => 40,
        Key::E => 26,
        Key::F => 41,
        Key::G => 42,
        Key::H => 43,
        Key::I => 31,
        Key::J => 44,
        Key::K => 45,
        Key::L => 46,
        Key::M => 58,
        Key::N => 57,
        Key::O => 32,
        Key::P => 33,
        Key::Q => 24,
        Key::R => 27,
        Key::S => 39,
        Key::T => 28,
        Key::U => 30,
        Key::V => 55,
        Key::W => 25,
        Key::X => 53,
        Key::Y => 29,
        Key::Z => 52,
        Key::Key0 => 19,
        Key::Key1 => 10,
        Key::Key2 => 11,
        Key::Key3 => 12,
        Key::Key4 => 13,
        Key::Key5 => 14,
        Key::Key6 => 15,
        Key::Key7 => 16,
        Key::Key8 => 17,
        Key::Key9 => 18,
        Key::Space => 65,
        Key::Enter => 36,
        Key::Escape => 9,
        Key::Backspace => 22,
        Key::Tab => 23,
        Key::Left => 113,
        Key::Right => 114,
        Key::Up => 111,
        Key::Down => 116,
        Key::LeftShift | Key::RightShift => 50,
        Key::LeftCtrl | Key::RightCtrl => 37,
        Key::LeftAlt | Key::RightAlt => 64,
        Key::LeftSuper | Key::RightSuper => 133,
        Key::F1 => 67,
        Key::F2 => 68,
        Key::F3 => 69,
        Key::F4 => 70,
        Key::F5 => 71,
        Key::F6 => 72,
        Key::F7 => 73,
        Key::F8 => 74,
        Key::F9 => 75,
        Key::F10 => 76,
        Key::F11 => 95,
        Key::F12 => 96,
        _ => return None,
    })
}
