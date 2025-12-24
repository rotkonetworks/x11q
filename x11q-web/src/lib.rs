//! x11q-web - X11 server in WebAssembly
//!
//! Renders X11 protocol to WebGPU canvas in browser.
//! Designed to work with x11q for remote X11 forwarding.

mod server;
mod window;
mod renderer;
mod drawable;
mod gc;
mod atom;
mod input;

use wasm_bindgen::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

pub use server::X11Server;
pub use renderer::Renderer;

/// Global server instance and input buffer
thread_local! {
    static SERVER: RefCell<Option<Rc<RefCell<X11Server>>>> = RefCell::new(None);
    static BUFFER: RefCell<Vec<u8>> = RefCell::new(Vec::new());
}

/// Initialize the X11 server with a canvas element
#[wasm_bindgen]
pub async fn init(canvas_id: &str) -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    console_log::init_with_level(log::Level::Debug).ok();

    log::info!("x11q-web initializing...");

    let window = web_sys::window().ok_or("no window")?;
    let document = window.document().ok_or("no document")?;
    let canvas = document
        .get_element_by_id(canvas_id)
        .ok_or("canvas not found")?
        .dyn_into::<web_sys::HtmlCanvasElement>()?;

    let width = canvas.width();
    let height = canvas.height();

    log::info!("canvas: {}x{}", width, height);

    let renderer = Renderer::new(&canvas).await?;
    let server = X11Server::new(renderer, width, height);

    SERVER.with(|s| {
        *s.borrow_mut() = Some(Rc::new(RefCell::new(server)));
    });

    log::info!("x11q-web ready");
    Ok(())
}

/// Reset the server state for a new connection
#[wasm_bindgen]
pub fn reset_connection() {
    SERVER.with(|s| {
        if let Some(server) = s.borrow().as_ref() {
            server.borrow_mut().reset();
        }
    });
    BUFFER.with(|b| {
        b.borrow_mut().clear();
    });
    log::info!("connection reset");
}

/// Process incoming X11 protocol data
#[wasm_bindgen]
pub fn process_x11_data(data: &[u8]) -> Result<Vec<u8>, JsValue> {
    // Add incoming data to buffer
    BUFFER.with(|b| {
        b.borrow_mut().extend_from_slice(data);
    });

    SERVER.with(|s| {
        let server = s.borrow();
        let server = server.as_ref().ok_or("server not initialized")?;
        let mut server = server.borrow_mut();

        let mut all_replies = Vec::new();

        loop {
            let buffer_len = BUFFER.with(|b| b.borrow().len());
            if buffer_len == 0 {
                break;
            }

            // Check state BEFORE processing
            let was_awaiting_setup = server.is_awaiting_setup();

            // Try to process one request from buffer
            let result = BUFFER.with(|b| {
                let buf = b.borrow();
                server.process_request(&buf)
            });

            match result {
                Ok(reply) => {
                    // Remove processed bytes from buffer
                    let consumed = BUFFER.with(|b| {
                        let buf = b.borrow();
                        if was_awaiting_setup {
                            // Connection setup consumes all available data
                            buf.len()
                        } else if buf.len() >= 4 {
                            // Regular request: length is in bytes 2-3 (in 4-byte words)
                            let len = u16::from_le_bytes([buf[2], buf[3]]) as usize * 4;
                            if len == 0 { 4 } else { len }
                        } else {
                            buf.len()
                        }
                    });

                    BUFFER.with(|b| {
                        let mut buf = b.borrow_mut();
                        if consumed <= buf.len() {
                            buf.drain(..consumed);
                        } else {
                            buf.clear();
                        }
                    });

                    all_replies.extend(reply);
                }
                Err(e) => {
                    if e.contains("incomplete") || e.contains("too short") {
                        // Need more data, stop processing
                        break;
                    }
                    return Err(JsValue::from_str(&e));
                }
            }
        }

        Ok(all_replies)
    })
}

/// Handle keyboard event
#[wasm_bindgen]
pub fn handle_keydown(code: &str, key: &str, modifiers: u32) {
    SERVER.with(|s| {
        if let Some(server) = s.borrow().as_ref() {
            server.borrow_mut().handle_key_event(code, key, modifiers, true);
        }
    });
}

/// Handle keyboard event
#[wasm_bindgen]
pub fn handle_keyup(code: &str, key: &str, modifiers: u32) {
    SERVER.with(|s| {
        if let Some(server) = s.borrow().as_ref() {
            server.borrow_mut().handle_key_event(code, key, modifiers, false);
        }
    });
}

/// Handle mouse move
#[wasm_bindgen]
pub fn handle_mousemove(x: i32, y: i32) {
    SERVER.with(|s| {
        if let Some(server) = s.borrow().as_ref() {
            server.borrow_mut().handle_mouse_move(x, y);
        }
    });
}

/// Handle mouse button
#[wasm_bindgen]
pub fn handle_mousebutton(button: u8, pressed: bool, x: i32, y: i32) {
    SERVER.with(|s| {
        if let Some(server) = s.borrow().as_ref() {
            server.borrow_mut().handle_mouse_button(button, pressed, x, y);
        }
    });
}

/// Render current frame
#[wasm_bindgen]
pub fn render() -> Result<(), JsValue> {
    SERVER.with(|s| {
        let server = s.borrow();
        let server = server.as_ref().ok_or("server not initialized")?;
        let result = server.borrow_mut().render()
            .map_err(|e| JsValue::from_str(&e.to_string()));
        result
    })
}

/// Get pending X11 events to send to client
#[wasm_bindgen]
pub fn get_pending_events() -> Vec<u8> {
    SERVER.with(|s| {
        if let Some(server) = s.borrow().as_ref() {
            server.borrow_mut().drain_events()
        } else {
            Vec::new()
        }
    })
}
