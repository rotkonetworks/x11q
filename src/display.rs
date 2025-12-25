//! Native X11 display server with minifb rendering
//!
//! A cross-platform X11 server that runs natively on Windows, Linux, or macOS.
//! Renders X11 windows using minifb. Useful for running Linux X11 apps
//! with display on Windows (connecting from WSL or remote Linux).

use anyhow::Result;
use minifb::{Key, Window, WindowOptions};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

#[cfg(unix)]
use tokio::net::UnixListener;

const X11_UNIX_DIR: &str = "/tmp/.X11-unix";
const X11_TCP_BASE: u16 = 6000;

// Extension major opcodes
const RANDR_MAJOR_OPCODE: u8 = 140;
const XINPUT_MAJOR_OPCODE: u8 = 131;
const XKB_MAJOR_OPCODE: u8 = 135;

/// Window data with pixel buffer
#[derive(Debug)]
struct X11Window {
    x: i16,
    y: i16,
    width: u16,
    height: u16,
    mapped: bool,
    parent: u32,
    children: Vec<u32>,
    /// BGRA pixel buffer
    pixels: Vec<u8>,
    depth: u8,
}

impl X11Window {
    fn new(parent: u32, x: i16, y: i16, width: u16, height: u16, depth: u8) -> Self {
        let size = (width as usize) * (height as usize) * 4;
        Self {
            x,
            y,
            width,
            height,
            mapped: false,
            parent,
            children: Vec::new(),
            pixels: vec![0; size],
            depth,
        }
    }

    fn new_root(width: u16, height: u16) -> Self {
        let size = (width as usize) * (height as usize) * 4;
        Self {
            x: 0,
            y: 0,
            width,
            height,
            mapped: true,
            parent: 0,
            children: Vec::new(),
            pixels: vec![0x20; size], // dark gray background
            depth: 24,
        }
    }

    fn resize(&mut self, width: u16, height: u16) {
        self.width = width;
        self.height = height;
        let size = (width as usize) * (height as usize) * 4;
        self.pixels = vec![0; size];
    }
}

/// Shared display state
struct DisplayState {
    windows: HashMap<u32, X11Window>,
    root_id: u32,
    width: u32,
    height: u32,
    /// Composite buffer for display (0RGB format for minifb)
    framebuffer: Vec<u32>,
    dirty: bool,
}

impl DisplayState {
    fn new(width: u32, height: u32) -> Self {
        let root_id = 1u32;
        let mut windows = HashMap::new();
        windows.insert(root_id, X11Window::new_root(width as u16, height as u16));

        Self {
            windows,
            root_id,
            width,
            height,
            framebuffer: vec![0x202020; (width * height) as usize],
            dirty: true,
        }
    }

    /// Composite all mapped windows into framebuffer
    fn composite(&mut self) {
        if !self.dirty {
            return;
        }

        // Clear to dark gray
        self.framebuffer.fill(0x202020);

        // Collect mapped windows with absolute positions
        let mapped = self.collect_mapped_windows();

        let fb_w = self.width as i32;
        let fb_h = self.height as i32;

        // Composite each window
        for (wid, abs_x, abs_y, _w, _h) in mapped {
            if let Some(win) = self.windows.get(&wid) {
                // Blit inline to avoid borrow issues
                for sy in 0..win.height as i32 {
                    let dy = abs_y as i32 + sy;
                    if dy < 0 || dy >= fb_h {
                        continue;
                    }

                    for sx in 0..win.width as i32 {
                        let dx = abs_x as i32 + sx;
                        if dx < 0 || dx >= fb_w {
                            continue;
                        }

                        let src_idx = (sy as usize * win.width as usize + sx as usize) * 4;
                        if src_idx + 3 >= win.pixels.len() {
                            continue;
                        }

                        // BGRA -> 0RGB
                        let b = win.pixels[src_idx] as u32;
                        let g = win.pixels[src_idx + 1] as u32;
                        let r = win.pixels[src_idx + 2] as u32;

                        let dst_idx = dy as usize * fb_w as usize + dx as usize;
                        self.framebuffer[dst_idx] = (r << 16) | (g << 8) | b;
                    }
                }
            }
        }

        self.dirty = false;
    }

    fn collect_mapped_windows(&self) -> Vec<(u32, i16, i16, u16, u16)> {
        let mut result = Vec::new();
        self.collect_mapped_recursive(self.root_id, 0, 0, &mut result);
        result
    }

    fn collect_mapped_recursive(
        &self,
        id: u32,
        parent_x: i16,
        parent_y: i16,
        result: &mut Vec<(u32, i16, i16, u16, u16)>,
    ) {
        if let Some(win) = self.windows.get(&id) {
            if win.mapped {
                let abs_x = parent_x + win.x;
                let abs_y = parent_y + win.y;

                // Skip root in output, but include children
                if id != self.root_id {
                    result.push((id, abs_x, abs_y, win.width, win.height));
                }

                for &child in &win.children {
                    self.collect_mapped_recursive(child, abs_x, abs_y, result);
                }
            }
        }
    }

}

/// X11 protocol handler for a single client
struct ClientHandler {
    state: Arc<Mutex<DisplayState>>,
    sequence: u16,
    resource_id_base: u32,
    resource_id_mask: u32,
    atoms: HashMap<String, u32>,
    next_atom: u32,
    extensions: HashMap<String, (u8, u8, u8)>,
    connected: bool,
}

impl ClientHandler {
    fn new(state: Arc<Mutex<DisplayState>>) -> Self {
        let mut extensions = HashMap::new();
        extensions.insert("RANDR".to_string(), (RANDR_MAJOR_OPCODE, 89, 147));
        extensions.insert(
            "XInputExtension".to_string(),
            (XINPUT_MAJOR_OPCODE, 147, 135),
        );
        extensions.insert("XKEYBOARD".to_string(), (XKB_MAJOR_OPCODE, 85, 137));
        extensions.insert("Generic Event Extension".to_string(), (128, 0, 0));

        Self {
            state,
            sequence: 0,
            resource_id_base: 0x00200000,
            resource_id_mask: 0x001fffff,
            atoms: HashMap::new(),
            next_atom: 1,
            extensions,
            connected: false,
        }
    }

    async fn process(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        if data.is_empty() {
            return Ok(Vec::new());
        }

        // Connection setup
        if !self.connected && (data[0] == 0x6c || data[0] == 0x42) {
            if data.len() < 12 {
                return Ok(Vec::new());
            }
            self.connected = true;
            return self.connection_reply().await;
        }

        if data.len() < 4 {
            return Ok(Vec::new());
        }

        self.sequence = self.sequence.wrapping_add(1);
        let opcode = data[0];

        match opcode {
            1 => self.handle_create_window(data).await,
            2 => Ok(Vec::new()), // ChangeWindowAttributes
            3 => self.get_window_attributes().await,
            4 => self.handle_destroy_window(data).await,
            8 => self.handle_map_window(data).await,
            10 => self.handle_unmap_window(data).await,
            12 => self.handle_configure_window(data).await,
            14 => self.get_geometry(data).await,
            15 => self.query_tree(data).await,
            16 => self.intern_atom(data),
            17 => self.get_atom_name(data),
            18 => Ok(Vec::new()), // ChangeProperty
            20 => self.get_property(),
            23 => self.get_selection_owner(),
            38 => self.query_pointer().await,
            42 => Ok(Vec::new()), // SetInputFocus
            43 => self.get_input_focus().await,
            45 => Ok(Vec::new()), // OpenFont
            47 => self.query_font(),
            53 => self.handle_create_pixmap(data).await,
            54 => Ok(Vec::new()), // FreePixmap
            55 => Ok(Vec::new()), // CreateGC
            56 => Ok(Vec::new()), // ChangeGC
            60 => Ok(Vec::new()), // FreeGC
            61 => self.handle_clear_area(data).await,
            62 => self.handle_copy_area(data).await,
            70 => self.handle_poly_fill_rectangle(data).await,
            72 => self.handle_put_image(data).await,
            73 => self.get_image(data).await,
            78 => Ok(Vec::new()), // CreateColormap
            84 => self.alloc_color(),
            91 => self.query_colors(data),
            97 => self.query_best_size(),
            98 => self.query_extension(data),
            99 => self.query_keymap(),
            101 => self.get_keyboard_mapping(data),
            119 => self.get_modifier_mapping(),
            RANDR_MAJOR_OPCODE => self.handle_randr(data).await,
            XINPUT_MAJOR_OPCODE => self.handle_xinput(data).await,
            XKB_MAJOR_OPCODE => self.handle_xkb(data),
            128 => self.handle_ge(data),
            _ => {
                eprintln!("unhandled opcode: {} (len {} bytes)", opcode, data.len());
                Ok(Vec::new())
            }
        }
    }

    async fn connection_reply(&self) -> Result<Vec<u8>> {
        let state = self.state.lock().await;
        let width = state.width;
        let height = state.height;
        let root_id = state.root_id;
        drop(state);

        let vendor = b"x11q-native";
        let vendor_len = vendor.len();
        let vendor_pad = (4 - (vendor_len % 4)) % 4;
        let vendor_total = vendor_len + vendor_pad;
        let num_formats = 1u8;
        let format_size = 8;
        let screen_size = 40;
        let depth_size = 8 + 24;
        let fixed_size = 32;
        let additional_data_len =
            fixed_size + vendor_total + format_size + screen_size + depth_size;
        let additional_words = additional_data_len / 4;

        let mut reply = Vec::with_capacity(8 + additional_data_len);
        reply.push(1); // success
        reply.push(0);
        reply.extend_from_slice(&11u16.to_le_bytes());
        reply.extend_from_slice(&0u16.to_le_bytes());
        reply.extend_from_slice(&(additional_words as u16).to_le_bytes());

        reply.extend_from_slice(&0u32.to_le_bytes());
        reply.extend_from_slice(&self.resource_id_base.to_le_bytes());
        reply.extend_from_slice(&self.resource_id_mask.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes());
        reply.extend_from_slice(&(vendor_len as u16).to_le_bytes());
        reply.extend_from_slice(&0xffffu16.to_le_bytes());
        reply.push(1);
        reply.push(num_formats);
        reply.push(0);
        reply.push(0);
        reply.push(8);
        reply.push(32);
        reply.push(8);
        reply.push(255);
        reply.extend_from_slice(&[0u8; 4]);

        reply.extend_from_slice(vendor);
        for _ in 0..vendor_pad {
            reply.push(0);
        }

        reply.push(24);
        reply.push(32);
        reply.push(32);
        reply.extend_from_slice(&[0u8; 5]);

        reply.extend_from_slice(&root_id.to_le_bytes());
        reply.extend_from_slice(&0x20u32.to_le_bytes());
        reply.extend_from_slice(&0xffffffffu32.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes());
        reply.extend_from_slice(&(width as u16).to_le_bytes());
        reply.extend_from_slice(&(height as u16).to_le_bytes());
        reply.extend_from_slice(&((width / 4) as u16).to_le_bytes());
        reply.extend_from_slice(&((height / 4) as u16).to_le_bytes());
        reply.extend_from_slice(&1u16.to_le_bytes());
        reply.extend_from_slice(&1u16.to_le_bytes());
        reply.extend_from_slice(&0x21u32.to_le_bytes());
        reply.push(0);
        reply.push(1);
        reply.push(24);
        reply.push(1);

        reply.push(24);
        reply.push(0);
        reply.extend_from_slice(&1u16.to_le_bytes());
        reply.extend_from_slice(&[0u8; 4]);

        reply.extend_from_slice(&0x21u32.to_le_bytes());
        reply.push(4);
        reply.push(8);
        reply.extend_from_slice(&256u16.to_le_bytes());
        reply.extend_from_slice(&0x00ff0000u32.to_le_bytes());
        reply.extend_from_slice(&0x0000ff00u32.to_le_bytes());
        reply.extend_from_slice(&0x000000ffu32.to_le_bytes());
        reply.extend_from_slice(&[0u8; 4]);

        Ok(reply)
    }

    async fn handle_create_window(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        if data.len() < 32 {
            return Ok(Vec::new());
        }
        let depth = data[1];
        let wid = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let parent = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        let x = i16::from_le_bytes([data[12], data[13]]);
        let y = i16::from_le_bytes([data[14], data[15]]);
        let width = u16::from_le_bytes([data[16], data[17]]).max(1);
        let height = u16::from_le_bytes([data[18], data[19]]).max(1);

        let mut state = self.state.lock().await;
        let window = X11Window::new(parent, x, y, width, height, if depth == 0 { 24 } else { depth });
        state.windows.insert(wid, window);
        if let Some(p) = state.windows.get_mut(&parent) {
            p.children.push(wid);
        }
        state.dirty = true;

        Ok(Vec::new())
    }

    async fn handle_destroy_window(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        if data.len() < 8 {
            return Ok(Vec::new());
        }
        let wid = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);

        let mut state = self.state.lock().await;
        if let Some(win) = state.windows.remove(&wid) {
            if let Some(parent) = state.windows.get_mut(&win.parent) {
                parent.children.retain(|&id| id != wid);
            }
        }
        state.dirty = true;

        Ok(Vec::new())
    }

    async fn handle_map_window(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        if data.len() < 8 {
            return Ok(Vec::new());
        }
        let wid = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);

        let mut state = self.state.lock().await;
        if let Some(win) = state.windows.get_mut(&wid) {
            win.mapped = true;
            state.dirty = true;
        }

        Ok(Vec::new())
    }

    async fn handle_unmap_window(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        if data.len() < 8 {
            return Ok(Vec::new());
        }
        let wid = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);

        let mut state = self.state.lock().await;
        if let Some(win) = state.windows.get_mut(&wid) {
            win.mapped = false;
            state.dirty = true;
        }

        Ok(Vec::new())
    }

    async fn handle_configure_window(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        if data.len() < 12 {
            return Ok(Vec::new());
        }
        let wid = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let value_mask = u16::from_le_bytes([data[8], data[9]]);

        let mut state = self.state.lock().await;
        if let Some(win) = state.windows.get_mut(&wid) {
            let mut offset = 12;
            if value_mask & 0x01 != 0 && offset + 4 <= data.len() {
                win.x = i16::from_le_bytes([data[offset], data[offset + 1]]);
                offset += 4;
            }
            if value_mask & 0x02 != 0 && offset + 4 <= data.len() {
                win.y = i16::from_le_bytes([data[offset], data[offset + 1]]);
                offset += 4;
            }
            if value_mask & 0x04 != 0 && offset + 4 <= data.len() {
                let new_w = u16::from_le_bytes([data[offset], data[offset + 1]]).max(1);
                offset += 4;
                if value_mask & 0x08 != 0 && offset + 4 <= data.len() {
                    let new_h = u16::from_le_bytes([data[offset], data[offset + 1]]).max(1);
                    win.resize(new_w, new_h);
                }
            } else if value_mask & 0x08 != 0 && offset + 4 <= data.len() {
                let new_h = u16::from_le_bytes([data[offset], data[offset + 1]]).max(1);
                win.resize(win.width, new_h);
            }
            state.dirty = true;
        }

        Ok(Vec::new())
    }

    async fn handle_clear_area(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        if data.len() < 16 {
            return Ok(Vec::new());
        }
        let wid = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let x = u16::from_le_bytes([data[8], data[9]]) as usize;
        let y = u16::from_le_bytes([data[10], data[11]]) as usize;
        let width = u16::from_le_bytes([data[12], data[13]]) as usize;
        let height = u16::from_le_bytes([data[14], data[15]]) as usize;

        let mut state = self.state.lock().await;
        if let Some(win) = state.windows.get_mut(&wid) {
            let w = if width == 0 {
                win.width as usize - x
            } else {
                width
            };
            let h = if height == 0 {
                win.height as usize - y
            } else {
                height
            };

            for py in y..y + h {
                if py >= win.height as usize {
                    break;
                }
                for px in x..x + w {
                    if px >= win.width as usize {
                        break;
                    }
                    let idx = (py * win.width as usize + px) * 4;
                    if idx + 3 < win.pixels.len() {
                        win.pixels[idx] = 0;
                        win.pixels[idx + 1] = 0;
                        win.pixels[idx + 2] = 0;
                        win.pixels[idx + 3] = 255;
                    }
                }
            }
            state.dirty = true;
        }

        Ok(Vec::new())
    }

    async fn handle_copy_area(&mut self, _data: &[u8]) -> Result<Vec<u8>> {
        // TODO: implement copy area between drawables
        Ok(Vec::new())
    }

    async fn handle_poly_fill_rectangle(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        if data.len() < 12 {
            return Ok(Vec::new());
        }
        let drawable = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let _gc = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);

        // Parse rectangles
        let mut offset = 12;
        let mut state = self.state.lock().await;

        while offset + 8 <= data.len() {
            let x = i16::from_le_bytes([data[offset], data[offset + 1]]);
            let y = i16::from_le_bytes([data[offset + 2], data[offset + 3]]);
            let w = u16::from_le_bytes([data[offset + 4], data[offset + 5]]);
            let h = u16::from_le_bytes([data[offset + 6], data[offset + 7]]);
            offset += 8;

            if let Some(win) = state.windows.get_mut(&drawable) {
                // Fill with white for now (should use GC foreground)
                for py in 0..h {
                    let dy = (y as i32 + py as i32) as usize;
                    if dy >= win.height as usize {
                        continue;
                    }
                    for px in 0..w {
                        let dx = (x as i32 + px as i32) as usize;
                        if dx >= win.width as usize {
                            continue;
                        }
                        let idx = (dy * win.width as usize + dx) * 4;
                        if idx + 3 < win.pixels.len() {
                            win.pixels[idx] = 255;
                            win.pixels[idx + 1] = 255;
                            win.pixels[idx + 2] = 255;
                            win.pixels[idx + 3] = 255;
                        }
                    }
                }
                state.dirty = true;
            }
        }

        Ok(Vec::new())
    }

    async fn handle_put_image(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        if data.len() < 24 {
            return Ok(Vec::new());
        }
        let format = data[1];
        let drawable = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let _gc = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        let width = u16::from_le_bytes([data[12], data[13]]) as usize;
        let height = u16::from_le_bytes([data[14], data[15]]) as usize;
        let dst_x = i16::from_le_bytes([data[16], data[17]]) as i32;
        let dst_y = i16::from_le_bytes([data[18], data[19]]) as i32;
        let _left_pad = data[20];
        let depth = data[21];

        let image_data = &data[24..];

        let mut state = self.state.lock().await;
        if let Some(win) = state.windows.get_mut(&drawable) {
            // ZPixmap format = 2
            if format == 2 {
                let bytes_per_pixel = if depth == 24 || depth == 32 { 4 } else { 1 };
                let row_bytes = width * bytes_per_pixel;
                let padded_row = (row_bytes + 3) & !3; // pad to 4 bytes

                for sy in 0..height {
                    let dy = dst_y + sy as i32;
                    if dy < 0 || dy >= win.height as i32 {
                        continue;
                    }
                    for sx in 0..width {
                        let dx = dst_x + sx as i32;
                        if dx < 0 || dx >= win.width as i32 {
                            continue;
                        }
                        let src_idx = sy * padded_row + sx * bytes_per_pixel;
                        let dst_idx = (dy as usize * win.width as usize + dx as usize) * 4;

                        if src_idx + bytes_per_pixel <= image_data.len()
                            && dst_idx + 3 < win.pixels.len()
                        {
                            // BGRA format (X11 uses BGRA for 24/32 bit)
                            win.pixels[dst_idx] = image_data[src_idx]; // B
                            win.pixels[dst_idx + 1] = image_data[src_idx + 1]; // G
                            win.pixels[dst_idx + 2] = image_data[src_idx + 2]; // R
                            win.pixels[dst_idx + 3] = if bytes_per_pixel == 4 {
                                image_data[src_idx + 3]
                            } else {
                                255
                            };
                        }
                    }
                }
                state.dirty = true;
            }
        }

        Ok(Vec::new())
    }

    async fn handle_create_pixmap(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        // For now, just accept pixmap creation but treat as window
        if data.len() < 16 {
            return Ok(Vec::new());
        }
        let depth = data[1];
        let pid = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let _drawable = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        let width = u16::from_le_bytes([data[12], data[13]]).max(1);
        let height = u16::from_le_bytes([data[14], data[15]]).max(1);

        let mut state = self.state.lock().await;
        let pixmap = X11Window::new(0, 0, 0, width, height, depth);
        state.windows.insert(pid, pixmap);

        Ok(Vec::new())
    }

    async fn get_window_attributes(&self) -> Result<Vec<u8>> {
        let mut reply = vec![1u8];
        reply.push(0); // backing-store
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&3u32.to_le_bytes());
        reply.extend_from_slice(&0x21u32.to_le_bytes()); // visual
        reply.extend_from_slice(&1u16.to_le_bytes()); // class
        reply.push(0); // bit-gravity
        reply.push(0); // win-gravity
        reply.extend_from_slice(&0u32.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes());
        reply.push(0);
        reply.push(1);
        reply.push(2); // map-state
        reply.push(0);
        reply.extend_from_slice(&0x20u32.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes());
        reply.extend_from_slice(&0u16.to_le_bytes());
        reply.extend_from_slice(&[0u8; 2]);
        Ok(reply)
    }

    async fn get_geometry(&self, data: &[u8]) -> Result<Vec<u8>> {
        let drawable = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let state = self.state.lock().await;

        let (depth, x, y, w, h) = if let Some(win) = state.windows.get(&drawable) {
            (win.depth, win.x, win.y, win.width, win.height)
        } else {
            (24, 0, 0, state.width as u16, state.height as u16)
        };

        let mut reply = vec![1u8];
        reply.push(depth);
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes());
        reply.extend_from_slice(&state.root_id.to_le_bytes());
        reply.extend_from_slice(&x.to_le_bytes());
        reply.extend_from_slice(&y.to_le_bytes());
        reply.extend_from_slice(&w.to_le_bytes());
        reply.extend_from_slice(&h.to_le_bytes());
        reply.extend_from_slice(&0u16.to_le_bytes());
        reply.extend_from_slice(&[0u8; 10]);
        Ok(reply)
    }

    async fn query_tree(&self, data: &[u8]) -> Result<Vec<u8>> {
        let window = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let state = self.state.lock().await;

        let (parent, children) = if let Some(win) = state.windows.get(&window) {
            (win.parent, win.children.clone())
        } else {
            (0, Vec::new())
        };

        let num_children = children.len() as u16;
        let length = num_children as u32;

        let mut reply = vec![1u8];
        reply.push(0);
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&length.to_le_bytes());
        reply.extend_from_slice(&state.root_id.to_le_bytes());
        reply.extend_from_slice(&parent.to_le_bytes());
        reply.extend_from_slice(&num_children.to_le_bytes());
        reply.extend_from_slice(&[0u8; 14]);

        for child in children {
            reply.extend_from_slice(&child.to_le_bytes());
        }

        Ok(reply)
    }

    async fn query_pointer(&self) -> Result<Vec<u8>> {
        let state = self.state.lock().await;
        let mut reply = vec![1u8];
        reply.push(1);
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes());
        reply.extend_from_slice(&state.root_id.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes());
        reply.extend_from_slice(&0i16.to_le_bytes());
        reply.extend_from_slice(&0i16.to_le_bytes());
        reply.extend_from_slice(&0i16.to_le_bytes());
        reply.extend_from_slice(&0i16.to_le_bytes());
        reply.extend_from_slice(&0u16.to_le_bytes());
        reply.extend_from_slice(&[0u8; 6]);
        Ok(reply)
    }

    async fn get_input_focus(&self) -> Result<Vec<u8>> {
        let state = self.state.lock().await;
        let mut reply = vec![1u8];
        reply.push(0);
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes());
        reply.extend_from_slice(&state.root_id.to_le_bytes());
        reply.extend_from_slice(&[0u8; 20]);
        Ok(reply)
    }

    async fn get_image(&self, data: &[u8]) -> Result<Vec<u8>> {
        if data.len() < 20 {
            let mut reply = vec![1u8];
            reply.push(24);
            reply.extend_from_slice(&self.sequence.to_le_bytes());
            reply.extend_from_slice(&0u32.to_le_bytes());
            reply.extend_from_slice(&0x21u32.to_le_bytes());
            reply.extend_from_slice(&[0u8; 20]);
            return Ok(reply);
        }

        let drawable = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let x = i16::from_le_bytes([data[8], data[9]]) as i32;
        let y = i16::from_le_bytes([data[10], data[11]]) as i32;
        let width = u16::from_le_bytes([data[12], data[13]]) as usize;
        let height = u16::from_le_bytes([data[14], data[15]]) as usize;

        let state = self.state.lock().await;
        let mut image_data = Vec::new();

        if let Some(win) = state.windows.get(&drawable) {
            for sy in 0..height {
                let src_y = y + sy as i32;
                for sx in 0..width {
                    let src_x = x + sx as i32;
                    if src_x >= 0
                        && src_x < win.width as i32
                        && src_y >= 0
                        && src_y < win.height as i32
                    {
                        let idx = (src_y as usize * win.width as usize + src_x as usize) * 4;
                        if idx + 3 < win.pixels.len() {
                            image_data.push(win.pixels[idx]);
                            image_data.push(win.pixels[idx + 1]);
                            image_data.push(win.pixels[idx + 2]);
                            image_data.push(win.pixels[idx + 3]);
                        } else {
                            image_data.extend_from_slice(&[0, 0, 0, 255]);
                        }
                    } else {
                        image_data.extend_from_slice(&[0, 0, 0, 255]);
                    }
                }
                // Pad row
                while image_data.len() % 4 != 0 {
                    image_data.push(0);
                }
            }
        }

        let length = (image_data.len() / 4) as u32;
        let mut reply = vec![1u8];
        reply.push(24);
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&length.to_le_bytes());
        reply.extend_from_slice(&0x21u32.to_le_bytes());
        reply.extend_from_slice(&[0u8; 20]);
        reply.extend_from_slice(&image_data);

        Ok(reply)
    }

    fn intern_atom(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        let only_if_exists = data[1] != 0;
        let name_len = u16::from_le_bytes([data[4], data[5]]) as usize;
        let name = String::from_utf8_lossy(&data[8..8 + name_len]).to_string();

        let atom = if let Some(&id) = self.atoms.get(&name) {
            id
        } else if only_if_exists {
            0
        } else {
            let id = self.next_atom;
            self.next_atom += 1;
            self.atoms.insert(name, id);
            id
        };

        let mut reply = vec![1u8];
        reply.push(0);
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes());
        reply.extend_from_slice(&atom.to_le_bytes());
        reply.extend_from_slice(&[0u8; 20]);
        Ok(reply)
    }

    fn get_atom_name(&self, data: &[u8]) -> Result<Vec<u8>> {
        let atom = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let name = self
            .atoms
            .iter()
            .find(|(_, &v)| v == atom)
            .map(|(k, _)| k.as_str())
            .unwrap_or("");
        let name_len = name.len();
        let pad = (4 - (name_len % 4)) % 4;
        let words = (name_len + pad) / 4;

        let mut reply = vec![1u8];
        reply.push(0);
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&(words as u32).to_le_bytes());
        reply.extend_from_slice(&(name_len as u16).to_le_bytes());
        reply.extend_from_slice(&[0u8; 22]);
        reply.extend_from_slice(name.as_bytes());
        for _ in 0..pad {
            reply.push(0);
        }
        Ok(reply)
    }

    fn get_property(&self) -> Result<Vec<u8>> {
        let mut reply = vec![1u8];
        reply.push(0);
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes());
        reply.extend_from_slice(&[0u8; 12]);
        Ok(reply)
    }

    fn get_selection_owner(&self) -> Result<Vec<u8>> {
        let mut reply = vec![1u8];
        reply.push(0);
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes());
        reply.extend_from_slice(&[0u8; 20]);
        Ok(reply)
    }

    fn query_font(&self) -> Result<Vec<u8>> {
        let mut reply = vec![1u8];
        reply.push(0);
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&7u32.to_le_bytes());
        reply.extend_from_slice(&[0u8; 28]);
        Ok(reply)
    }

    fn alloc_color(&self) -> Result<Vec<u8>> {
        let mut reply = vec![1u8];
        reply.push(0);
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes());
        reply.extend_from_slice(&0xffffu16.to_le_bytes());
        reply.extend_from_slice(&0xffffu16.to_le_bytes());
        reply.extend_from_slice(&0xffffu16.to_le_bytes());
        reply.extend_from_slice(&[0u8; 2]);
        reply.extend_from_slice(&0u32.to_le_bytes());
        reply.extend_from_slice(&[0u8; 12]);
        Ok(reply)
    }

    fn query_colors(&self, data: &[u8]) -> Result<Vec<u8>> {
        let num_pixels = if data.len() >= 8 {
            (data.len() - 8) / 4
        } else {
            0
        };

        let mut reply = vec![1u8];
        reply.push(0);
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&((num_pixels * 2) as u32).to_le_bytes());
        reply.extend_from_slice(&(num_pixels as u16).to_le_bytes());
        reply.extend_from_slice(&[0u8; 22]);

        for _ in 0..num_pixels {
            reply.extend_from_slice(&0xffffu16.to_le_bytes());
            reply.extend_from_slice(&0xffffu16.to_le_bytes());
            reply.extend_from_slice(&0xffffu16.to_le_bytes());
            reply.extend_from_slice(&[0u8; 2]);
        }
        Ok(reply)
    }

    fn query_best_size(&self) -> Result<Vec<u8>> {
        let mut reply = vec![1u8];
        reply.push(0);
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes());
        reply.extend_from_slice(&16u16.to_le_bytes());
        reply.extend_from_slice(&16u16.to_le_bytes());
        reply.extend_from_slice(&[0u8; 20]);
        Ok(reply)
    }

    fn query_extension(&self, data: &[u8]) -> Result<Vec<u8>> {
        let name_len = u16::from_le_bytes([data[4], data[5]]) as usize;
        let end = (8 + name_len).min(data.len());
        let name = String::from_utf8_lossy(&data[8..end]).to_string();

        let mut reply = vec![1u8];
        reply.push(0);
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes());

        if let Some(&(major, first_event, first_error)) = self.extensions.get(&name) {
            reply.push(1);
            reply.push(major);
            reply.push(first_event);
            reply.push(first_error);
        } else {
            reply.push(0);
            reply.push(0);
            reply.push(0);
            reply.push(0);
        }
        reply.extend_from_slice(&[0u8; 20]);
        Ok(reply)
    }

    fn query_keymap(&self) -> Result<Vec<u8>> {
        let mut reply = vec![1u8];
        reply.push(0);
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes());
        reply.extend_from_slice(&[0u8; 24]);
        Ok(reply)
    }

    fn get_keyboard_mapping(&self, data: &[u8]) -> Result<Vec<u8>> {
        let first = data[4];
        let count = data[5];
        let keysyms_per_keycode = 4u8;
        let words = (count as u32) * (keysyms_per_keycode as u32);

        let mut reply = vec![1u8];
        reply.push(keysyms_per_keycode);
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&words.to_le_bytes());
        reply.extend_from_slice(&[0u8; 24]);

        for i in 0..count {
            let keycode = first + i;
            let keysym = match keycode {
                10..=19 => (keycode - 10 + b'1') as u32,
                24..=33 => (keycode - 24 + b'q') as u32,
                38..=46 => (keycode - 38 + b'a') as u32,
                52..=58 => (keycode - 52 + b'z') as u32,
                _ => 0,
            };
            for j in 0..keysyms_per_keycode {
                let sym = if j == 0 { keysym } else { 0 };
                reply.extend_from_slice(&sym.to_le_bytes());
            }
        }
        Ok(reply)
    }

    fn get_modifier_mapping(&self) -> Result<Vec<u8>> {
        let keycodes_per_mod = 2u8;
        let words = 8 * (keycodes_per_mod as u32) / 4;

        let mut reply = vec![1u8];
        reply.push(keycodes_per_mod);
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&words.to_le_bytes());
        reply.extend_from_slice(&[0u8; 24]);

        for _ in 0..8 {
            for _ in 0..keycodes_per_mod {
                reply.push(0);
            }
        }
        Ok(reply)
    }

    async fn handle_randr(&self, data: &[u8]) -> Result<Vec<u8>> {
        let minor = data[1];
        let state = self.state.lock().await;

        match minor {
            0 => {
                // RRQueryVersion
                let mut reply = vec![1u8];
                reply.push(0);
                reply.extend_from_slice(&self.sequence.to_le_bytes());
                reply.extend_from_slice(&0u32.to_le_bytes());
                reply.extend_from_slice(&1u32.to_le_bytes());
                reply.extend_from_slice(&6u32.to_le_bytes());
                reply.extend_from_slice(&[0u8; 16]);
                Ok(reply)
            }
            5 => {
                // RRGetScreenResources
                let num_crtcs = 1u16;
                let num_outputs = 1u16;
                let num_modes = 1u16;
                let names_len = 8u16;
                let length =
                    (8 + num_crtcs as u32 * 4 + num_outputs as u32 * 4 + num_modes as u32 * 32 + names_len as u32) / 4;

                let mut reply = vec![1u8];
                reply.push(0);
                reply.extend_from_slice(&self.sequence.to_le_bytes());
                reply.extend_from_slice(&length.to_le_bytes());
                reply.extend_from_slice(&0u32.to_le_bytes());
                reply.extend_from_slice(&0u32.to_le_bytes());
                reply.extend_from_slice(&num_crtcs.to_le_bytes());
                reply.extend_from_slice(&num_outputs.to_le_bytes());
                reply.extend_from_slice(&num_modes.to_le_bytes());
                reply.extend_from_slice(&names_len.to_le_bytes());
                reply.extend_from_slice(&[0u8; 8]);
                reply.extend_from_slice(&1u32.to_le_bytes()); // CRTC ID
                reply.extend_from_slice(&1u32.to_le_bytes()); // Output ID
                // Mode info
                reply.extend_from_slice(&1u32.to_le_bytes()); // Mode ID
                reply.extend_from_slice(&(state.width as u16).to_le_bytes());
                reply.extend_from_slice(&(state.height as u16).to_le_bytes());
                reply.extend_from_slice(&60000000u32.to_le_bytes());
                reply.extend_from_slice(&(state.width as u16).to_le_bytes());
                reply.extend_from_slice(&(state.width as u16).to_le_bytes());
                reply.extend_from_slice(&(state.width as u16).to_le_bytes());
                reply.extend_from_slice(&0u16.to_le_bytes());
                reply.extend_from_slice(&(state.height as u16).to_le_bytes());
                reply.extend_from_slice(&(state.height as u16).to_le_bytes());
                reply.extend_from_slice(&(state.height as u16).to_le_bytes());
                reply.extend_from_slice(&7u16.to_le_bytes());
                reply.extend_from_slice(&0u32.to_le_bytes());
                reply.extend_from_slice(b"default\0");
                Ok(reply)
            }
            25 => {
                // RRGetOutputPrimary
                let mut reply = vec![1u8];
                reply.push(0);
                reply.extend_from_slice(&self.sequence.to_le_bytes());
                reply.extend_from_slice(&0u32.to_le_bytes());
                reply.extend_from_slice(&1u32.to_le_bytes());
                reply.extend_from_slice(&[0u8; 20]);
                Ok(reply)
            }
            _ => Ok(Vec::new()),
        }
    }

    async fn handle_xinput(&self, data: &[u8]) -> Result<Vec<u8>> {
        let minor = data[1];
        let state = self.state.lock().await;

        match minor {
            1 => {
                // XIQueryVersion
                let mut reply = vec![1u8];
                reply.push(0);
                reply.extend_from_slice(&self.sequence.to_le_bytes());
                reply.extend_from_slice(&0u32.to_le_bytes());
                reply.extend_from_slice(&2u16.to_le_bytes());
                reply.extend_from_slice(&3u16.to_le_bytes());
                reply.extend_from_slice(&[0u8; 20]);
                Ok(reply)
            }
            46 => {
                // XIQueryDevice
                let mut devices = Vec::new();
                // Master pointer
                devices.extend_from_slice(&2u16.to_le_bytes());
                devices.extend_from_slice(&1u16.to_le_bytes());
                devices.extend_from_slice(&3u16.to_le_bytes());
                devices.extend_from_slice(&0u16.to_le_bytes());
                devices.extend_from_slice(&14u16.to_le_bytes());
                devices.push(1);
                devices.push(0);
                devices.extend_from_slice(b"Master pointer");
                devices.extend_from_slice(&[0u8; 2]);

                // Master keyboard
                devices.extend_from_slice(&3u16.to_le_bytes());
                devices.extend_from_slice(&2u16.to_le_bytes());
                devices.extend_from_slice(&2u16.to_le_bytes());
                devices.extend_from_slice(&0u16.to_le_bytes());
                devices.extend_from_slice(&15u16.to_le_bytes());
                devices.push(1);
                devices.push(0);
                devices.extend_from_slice(b"Master keyboard");
                devices.push(0);

                let length = devices.len() / 4;
                let mut reply = vec![1u8];
                reply.push(0);
                reply.extend_from_slice(&self.sequence.to_le_bytes());
                reply.extend_from_slice(&(length as u32).to_le_bytes());
                reply.extend_from_slice(&2u16.to_le_bytes());
                reply.extend_from_slice(&[0u8; 22]);
                reply.extend_from_slice(&devices);
                Ok(reply)
            }
            47 => {
                // XIQueryPointer
                let mut reply = vec![1u8];
                reply.push(0);
                reply.extend_from_slice(&self.sequence.to_le_bytes());
                reply.extend_from_slice(&4u32.to_le_bytes());
                reply.extend_from_slice(&state.root_id.to_le_bytes());
                reply.extend_from_slice(&0u32.to_le_bytes());
                let pos = 100i32 << 16;
                reply.extend_from_slice(&pos.to_le_bytes());
                reply.extend_from_slice(&pos.to_le_bytes());
                reply.extend_from_slice(&pos.to_le_bytes());
                reply.extend_from_slice(&pos.to_le_bytes());
                reply.push(1);
                reply.push(0);
                reply.extend_from_slice(&0u16.to_le_bytes());
                reply.extend_from_slice(&[0u8; 12]);
                Ok(reply)
            }
            48 => {
                // XIGetFocus
                let mut reply = vec![1u8];
                reply.push(0);
                reply.extend_from_slice(&self.sequence.to_le_bytes());
                reply.extend_from_slice(&0u32.to_le_bytes());
                reply.extend_from_slice(&state.root_id.to_le_bytes());
                reply.extend_from_slice(&[0u8; 20]);
                Ok(reply)
            }
            52 => Ok(Vec::new()), // XISelectEvents
            _ => Ok(Vec::new()),
        }
    }

    fn handle_xkb(&self, data: &[u8]) -> Result<Vec<u8>> {
        let minor = data[1];

        match minor {
            0 => {
                // XkbUseExtension
                let mut reply = vec![1u8];
                reply.push(1);
                reply.extend_from_slice(&self.sequence.to_le_bytes());
                reply.extend_from_slice(&0u32.to_le_bytes());
                reply.extend_from_slice(&1u16.to_le_bytes());
                reply.extend_from_slice(&0u16.to_le_bytes());
                reply.extend_from_slice(&[0u8; 20]);
                Ok(reply)
            }
            1 => Ok(Vec::new()), // XkbSelectEvents
            6 => {
                // XkbGetControls
                let mut reply = vec![1u8];
                reply.push(3);
                reply.extend_from_slice(&self.sequence.to_le_bytes());
                reply.extend_from_slice(&15u32.to_le_bytes());
                reply.extend_from_slice(&[0u8; 24]);
                reply.extend_from_slice(&[0u8; 32]);
                reply.extend_from_slice(&[0xffu8; 32]);
                Ok(reply)
            }
            8 => {
                // XkbGetMap
                let mut reply = vec![1u8];
                reply.push(3);
                reply.extend_from_slice(&self.sequence.to_le_bytes());
                reply.extend_from_slice(&2u32.to_le_bytes());
                reply.extend_from_slice(&[0u8; 2]);
                reply.push(8);
                reply.push(255);
                reply.extend_from_slice(&[0u8; 28]);
                Ok(reply)
            }
            _ => Ok(Vec::new()),
        }
    }

    fn handle_ge(&self, data: &[u8]) -> Result<Vec<u8>> {
        let minor = data[1];

        match minor {
            0 => {
                // GEQueryVersion
                let mut reply = vec![1u8];
                reply.push(0);
                reply.extend_from_slice(&self.sequence.to_le_bytes());
                reply.extend_from_slice(&0u32.to_le_bytes());
                reply.extend_from_slice(&1u16.to_le_bytes());
                reply.extend_from_slice(&0u16.to_le_bytes());
                reply.extend_from_slice(&[0u8; 20]);
                Ok(reply)
            }
            _ => Ok(Vec::new()),
        }
    }
}

async fn handle_client<S>(mut stream: S, state: Arc<Mutex<DisplayState>>, client_id: u32)
where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
{
    let mut handler = ClientHandler::new(state);
    let mut buf = vec![0u8; 65536];
    let mut pending = Vec::new();

    loop {
        let n = match stream.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        pending.extend_from_slice(&buf[..n]);

        loop {
            let needed = if !handler.connected {
                12
            } else if pending.len() >= 4 {
                let len = u16::from_le_bytes([pending[2], pending[3]]) as usize * 4;
                if len == 0 {
                    4
                } else {
                    len
                }
            } else {
                4
            };

            if pending.len() < needed {
                break;
            }

            let reply = match handler.process(&pending).await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[{}] error: {}", client_id, e);
                    break;
                }
            };

            pending.drain(..needed);

            if !reply.is_empty() {
                if stream.write_all(&reply).await.is_err() {
                    return;
                }
            }
        }
    }
    eprintln!("[{}] disconnected", client_id);
}

/// Run native X11 display server
pub async fn run_display(display_num: u32, width: u32, height: u32) -> Result<()> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static CLIENT_COUNTER: AtomicU32 = AtomicU32::new(0);

    let state = Arc::new(Mutex::new(DisplayState::new(width, height)));

    // TCP listener (works on all platforms including Windows)
    let tcp_port = X11_TCP_BASE + display_num as u16;
    let tcp_listener = TcpListener::bind(format!("0.0.0.0:{}", tcp_port)).await?;
    eprintln!("listening on TCP port {} (DISPLAY=hostname:{})", tcp_port, display_num);

    // Unix socket (only on Unix platforms)
    #[cfg(unix)]
    let unix_listener = {
        let socket_path = format!("{}/X{}", X11_UNIX_DIR, display_num);
        let _ = std::fs::remove_file(&socket_path);
        std::fs::create_dir_all(X11_UNIX_DIR)?;
        let listener = UnixListener::bind(&socket_path)?;
        eprintln!("listening on Unix socket {} (DISPLAY=:{})", socket_path, display_num);
        Some(listener)
    };

    #[cfg(not(unix))]
    let unix_listener: Option<()> = None;

    // Spawn connection acceptor
    let state_clone = Arc::clone(&state);
    tokio::spawn(async move {
        loop {
            #[cfg(unix)]
            {
                if let Some(ref unix) = unix_listener {
                    tokio::select! {
                        Ok((stream, _)) = tcp_listener.accept() => {
                            let id = CLIENT_COUNTER.fetch_add(1, Ordering::SeqCst);
                            eprintln!("[{}] TCP client connected", id);
                            let s = Arc::clone(&state_clone);
                            tokio::spawn(handle_client(stream, s, id));
                        }
                        Ok((stream, _)) = unix.accept() => {
                            let id = CLIENT_COUNTER.fetch_add(1, Ordering::SeqCst);
                            eprintln!("[{}] Unix client connected", id);
                            let s = Arc::clone(&state_clone);
                            tokio::spawn(handle_client(stream, s, id));
                        }
                    }
                }
            }
            #[cfg(not(unix))]
            {
                if let Ok((stream, _)) = tcp_listener.accept().await {
                    let id = CLIENT_COUNTER.fetch_add(1, Ordering::SeqCst);
                    eprintln!("[{}] TCP client connected", id);
                    let s = Arc::clone(&state_clone);
                    tokio::spawn(handle_client(stream, s, id));
                }
            }
        }
    });

    // Create minifb window for display
    let mut window = Window::new(
        &format!("x11q display :{}", display_num),
        width as usize,
        height as usize,
        WindowOptions {
            resize: false,
            ..Default::default()
        },
    )
    .map_err(|e| anyhow::anyhow!("failed to create window: {}", e))?;

    window.set_target_fps(60);

    eprintln!("x11q native display ready - DISPLAY=:{}", display_num);
    eprintln!("for WSL/remote: DISPLAY=<windows-ip>:{}", display_num);

    // Main render loop
    loop {
        if !window.is_open() || window.is_key_down(Key::Escape) {
            break;
        }

        // Composite and render
        {
            let mut display = state.lock().await;
            display.dirty = true; // always refresh for now
            display.composite();
            window
                .update_with_buffer(&display.framebuffer, width as usize, height as usize)
                .map_err(|e| anyhow::anyhow!("render error: {}", e))?;
        }

        // Small sleep to not spin CPU
        tokio::time::sleep(tokio::time::Duration::from_millis(16)).await;
    }

    Ok(())
}
