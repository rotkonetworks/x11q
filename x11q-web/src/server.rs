//! X11 Server implementation

use crate::atom::AtomStore;
use crate::drawable::PixmapStore;
use crate::gc::{GCStore, GC};
use crate::input::{Keyboard, Pointer};
use crate::renderer::Renderer;
use crate::window::{Window, WindowClass, WindowTree};
use std::collections::VecDeque;

/// X11 protocol opcodes (subset for bspwm + alacritty)
mod opcodes {
    pub const CREATE_WINDOW: u8 = 1;
    pub const CHANGE_WINDOW_ATTRIBUTES: u8 = 2;
    pub const GET_WINDOW_ATTRIBUTES: u8 = 3;
    pub const DESTROY_WINDOW: u8 = 4;
    pub const MAP_WINDOW: u8 = 8;
    pub const UNMAP_WINDOW: u8 = 10;
    pub const CONFIGURE_WINDOW: u8 = 12;
    pub const GET_GEOMETRY: u8 = 14;
    pub const QUERY_TREE: u8 = 15;
    pub const INTERN_ATOM: u8 = 16;
    pub const GET_ATOM_NAME: u8 = 17;
    pub const CHANGE_PROPERTY: u8 = 18;
    pub const DELETE_PROPERTY: u8 = 19;
    pub const GET_PROPERTY: u8 = 20;
    pub const GRAB_POINTER: u8 = 26;
    pub const UNGRAB_POINTER: u8 = 27;
    pub const GRAB_KEYBOARD: u8 = 31;
    pub const UNGRAB_KEYBOARD: u8 = 32;
    pub const QUERY_POINTER: u8 = 38;
    pub const SET_INPUT_FOCUS: u8 = 42;
    pub const GET_INPUT_FOCUS: u8 = 43;
    pub const OPEN_FONT: u8 = 45;
    pub const CLOSE_FONT: u8 = 46;
    pub const CREATE_PIXMAP: u8 = 53;
    pub const FREE_PIXMAP: u8 = 54;
    pub const CREATE_GC: u8 = 55;
    pub const CHANGE_GC: u8 = 56;
    pub const FREE_GC: u8 = 60;
    pub const CLEAR_AREA: u8 = 61;
    pub const COPY_AREA: u8 = 62;
    pub const POLY_FILL_RECTANGLE: u8 = 70;
    pub const PUT_IMAGE: u8 = 72;
    pub const GET_IMAGE: u8 = 73;
    pub const CREATE_COLORMAP: u8 = 78;
    pub const ALLOC_COLOR: u8 = 84;
    pub const QUERY_EXTENSION: u8 = 98;
    pub const GET_KEYBOARD_MAPPING: u8 = 101;
    pub const GET_MODIFIER_MAPPING: u8 = 119;
}

/// X11 Server state
pub struct X11Server {
    renderer: Renderer,
    windows: WindowTree,
    pixmaps: PixmapStore,
    gcs: GCStore,
    atoms: AtomStore,
    keyboard: Keyboard,
    pointer: Pointer,
    /// Pending events to send to client
    events: VecDeque<Vec<u8>>,
    /// Resource ID counter
    next_id: u32,
    /// Input focus window
    focus: u32,
    /// Screen dimensions
    width: u32,
    height: u32,
    /// Sequence number
    sequence: u16,
}

impl X11Server {
    pub fn new(renderer: Renderer, width: u32, height: u32) -> Self {
        let root_id = 0x00000001;
        let windows = WindowTree::new(root_id, width as u16, height as u16);

        Self {
            renderer,
            windows,
            pixmaps: PixmapStore::new(),
            gcs: GCStore::new(),
            atoms: AtomStore::new(),
            keyboard: Keyboard::new(),
            pointer: Pointer::new(),
            events: VecDeque::new(),
            next_id: 0x00200000, // client resource IDs start here
            focus: root_id,
            width,
            height,
            sequence: 0,
        }
    }

    /// Process X11 request, return reply if any
    pub fn process_request(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        if data.len() < 4 {
            return Err("request too short".into());
        }

        let opcode = data[0];
        let _detail = data[1];
        let length = u16::from_le_bytes([data[2], data[3]]) as usize * 4;

        if data.len() < length {
            return Err("incomplete request".into());
        }

        self.sequence = self.sequence.wrapping_add(1);

        match opcode {
            opcodes::CREATE_WINDOW => self.handle_create_window(data),
            opcodes::CHANGE_WINDOW_ATTRIBUTES => self.handle_change_window_attributes(data),
            opcodes::GET_WINDOW_ATTRIBUTES => self.handle_get_window_attributes(data),
            opcodes::DESTROY_WINDOW => self.handle_destroy_window(data),
            opcodes::MAP_WINDOW => self.handle_map_window(data),
            opcodes::UNMAP_WINDOW => self.handle_unmap_window(data),
            opcodes::CONFIGURE_WINDOW => self.handle_configure_window(data),
            opcodes::GET_GEOMETRY => self.handle_get_geometry(data),
            opcodes::QUERY_TREE => self.handle_query_tree(data),
            opcodes::INTERN_ATOM => self.handle_intern_atom(data),
            opcodes::GET_ATOM_NAME => self.handle_get_atom_name(data),
            opcodes::CHANGE_PROPERTY => self.handle_change_property(data),
            opcodes::GET_PROPERTY => self.handle_get_property(data),
            opcodes::QUERY_POINTER => self.handle_query_pointer(data),
            opcodes::SET_INPUT_FOCUS => self.handle_set_input_focus(data),
            opcodes::GET_INPUT_FOCUS => self.handle_get_input_focus(data),
            opcodes::CREATE_PIXMAP => self.handle_create_pixmap(data),
            opcodes::FREE_PIXMAP => self.handle_free_pixmap(data),
            opcodes::CREATE_GC => self.handle_create_gc(data),
            opcodes::CHANGE_GC => self.handle_change_gc(data),
            opcodes::FREE_GC => self.handle_free_gc(data),
            opcodes::CLEAR_AREA => self.handle_clear_area(data),
            opcodes::POLY_FILL_RECTANGLE => self.handle_poly_fill_rectangle(data),
            opcodes::PUT_IMAGE => self.handle_put_image(data),
            opcodes::QUERY_EXTENSION => self.handle_query_extension(data),
            opcodes::GET_KEYBOARD_MAPPING => self.handle_get_keyboard_mapping(data),
            opcodes::GET_MODIFIER_MAPPING => self.handle_get_modifier_mapping(data),
            _ => {
                log::warn!("unhandled opcode: {}", opcode);
                Ok(Vec::new())
            }
        }
    }

    // Window operations

    fn handle_create_window(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let depth = data[1];
        let wid = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let parent = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        let x = i16::from_le_bytes([data[12], data[13]]);
        let y = i16::from_le_bytes([data[14], data[15]]);
        let width = u16::from_le_bytes([data[16], data[17]]);
        let height = u16::from_le_bytes([data[18], data[19]]);
        let border_width = u16::from_le_bytes([data[20], data[21]]);
        let class = match u16::from_le_bytes([data[22], data[23]]) {
            0 => WindowClass::CopyFromParent,
            1 => WindowClass::InputOutput,
            2 => WindowClass::InputOnly,
            _ => WindowClass::CopyFromParent,
        };
        let visual = u32::from_le_bytes([data[24], data[25], data[26], data[27]]);

        let window = Window::new(wid, parent, x, y, width, height, border_width, depth, class, visual);
        self.windows.create_window(window);
        self.renderer.create_texture(wid, width as u32, height as u32);

        log::debug!("CreateWindow: {} parent={} {}x{}", wid, parent, width, height);
        Ok(Vec::new())
    }

    fn handle_change_window_attributes(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let _wid = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        // TODO: parse value_mask and values
        Ok(Vec::new())
    }

    fn handle_get_window_attributes(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let wid = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);

        if let Some(window) = self.windows.get(wid) {
            let mut reply = vec![0u8; 44];
            reply[0] = 1; // reply
            reply[1] = 0; // backing-store
            reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
            reply[4..8].copy_from_slice(&(44u32 / 4 - 8).to_le_bytes()); // length
            reply[8..12].copy_from_slice(&window.visual.to_le_bytes());
            reply[12..14].copy_from_slice(&(window.class as u16).to_le_bytes());
            reply[14] = window.attributes.bit_gravity;
            reply[15] = window.attributes.win_gravity;
            reply[16..20].copy_from_slice(&window.attributes.backing_planes.to_le_bytes());
            reply[20..24].copy_from_slice(&window.attributes.backing_pixel.to_le_bytes());
            reply[24] = if window.attributes.save_under { 1 } else { 0 };
            reply[25] = 1; // map-is-installed
            reply[26] = if window.mapped { 2 } else { 0 }; // map-state
            reply[27] = if window.attributes.override_redirect { 1 } else { 0 };
            reply[28..32].copy_from_slice(&window.attributes.colormap.to_le_bytes());
            reply[32..36].copy_from_slice(&window.attributes.event_mask.to_le_bytes());
            reply[36..40].copy_from_slice(&0u32.to_le_bytes()); // your-event-mask
            reply[40..42].copy_from_slice(&window.attributes.do_not_propagate_mask.to_le_bytes());
            Ok(reply)
        } else {
            self.error_reply(3, wid) // BadWindow
        }
    }

    fn handle_destroy_window(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let wid = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        self.windows.destroy_window(wid);
        self.renderer.destroy_texture(wid);
        log::debug!("DestroyWindow: {}", wid);
        Ok(Vec::new())
    }

    fn handle_map_window(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let wid = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        self.windows.map_window(wid);
        log::debug!("MapWindow: {}", wid);

        // Send MapNotify event
        self.send_map_notify(wid);
        Ok(Vec::new())
    }

    fn handle_unmap_window(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let wid = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        self.windows.unmap_window(wid);
        log::debug!("UnmapWindow: {}", wid);
        Ok(Vec::new())
    }

    fn handle_configure_window(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let wid = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let value_mask = u16::from_le_bytes([data[8], data[9]]);

        if let Some(window) = self.windows.get_mut(wid) {
            let mut offset = 12;
            if value_mask & 0x01 != 0 {
                window.x = i16::from_le_bytes([data[offset], data[offset + 1]]);
                offset += 4;
            }
            if value_mask & 0x02 != 0 {
                window.y = i16::from_le_bytes([data[offset], data[offset + 1]]);
                offset += 4;
            }
            if value_mask & 0x04 != 0 {
                window.width = u16::from_le_bytes([data[offset], data[offset + 1]]);
                offset += 4;
            }
            if value_mask & 0x08 != 0 {
                window.height = u16::from_le_bytes([data[offset], data[offset + 1]]);
                offset += 4;
            }
            if value_mask & 0x10 != 0 {
                window.border_width = u16::from_le_bytes([data[offset], data[offset + 1]]);
                // offset += 4;
            }

            log::debug!("ConfigureWindow: {} -> {}x{} at ({},{})",
                wid, window.width, window.height, window.x, window.y);
        }

        Ok(Vec::new())
    }

    fn handle_get_geometry(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let drawable = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);

        if let Some(window) = self.windows.get(drawable) {
            let mut reply = vec![0u8; 32];
            reply[0] = 1; // reply
            reply[1] = window.depth;
            reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
            reply[4..8].copy_from_slice(&0u32.to_le_bytes()); // length
            reply[8..12].copy_from_slice(&self.windows.root_id().to_le_bytes());
            reply[12..14].copy_from_slice(&window.x.to_le_bytes());
            reply[14..16].copy_from_slice(&window.y.to_le_bytes());
            reply[16..18].copy_from_slice(&window.width.to_le_bytes());
            reply[18..20].copy_from_slice(&window.height.to_le_bytes());
            reply[20..22].copy_from_slice(&window.border_width.to_le_bytes());
            Ok(reply)
        } else {
            self.error_reply(9, drawable) // BadDrawable
        }
    }

    fn handle_query_tree(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let wid = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);

        if let Some(window) = self.windows.get(wid) {
            let children = &window.children;
            let n_children = children.len() as u16;
            let reply_len = 32 + children.len() * 4;
            let mut reply = vec![0u8; reply_len];

            reply[0] = 1; // reply
            reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
            reply[4..8].copy_from_slice(&(n_children as u32).to_le_bytes());
            reply[8..12].copy_from_slice(&self.windows.root_id().to_le_bytes());
            reply[12..16].copy_from_slice(&window.parent.to_le_bytes());
            reply[16..18].copy_from_slice(&n_children.to_le_bytes());

            for (i, &child) in children.iter().enumerate() {
                let offset = 32 + i * 4;
                reply[offset..offset + 4].copy_from_slice(&child.to_le_bytes());
            }

            Ok(reply)
        } else {
            self.error_reply(3, wid)
        }
    }

    // Atom operations

    fn handle_intern_atom(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let only_if_exists = data[1] != 0;
        let name_len = u16::from_le_bytes([data[4], data[5]]) as usize;
        let name = std::str::from_utf8(&data[8..8 + name_len])
            .map_err(|e| e.to_string())?;

        let atom = self.atoms.intern(name, only_if_exists);

        let mut reply = vec![0u8; 32];
        reply[0] = 1; // reply
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        if let Some(atom) = atom {
            reply[8..12].copy_from_slice(&atom.to_le_bytes());
        }

        Ok(reply)
    }

    fn handle_get_atom_name(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let atom = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);

        if let Some(name) = self.atoms.get_name(atom) {
            let name_len = name.len();
            let padded_len = (name_len + 3) & !3;
            let mut reply = vec![0u8; 32 + padded_len];

            reply[0] = 1; // reply
            reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
            reply[4..8].copy_from_slice(&((padded_len / 4) as u32).to_le_bytes());
            reply[8..10].copy_from_slice(&(name_len as u16).to_le_bytes());
            reply[32..32 + name_len].copy_from_slice(name.as_bytes());

            Ok(reply)
        } else {
            self.error_reply(5, atom) // BadAtom
        }
    }

    fn handle_change_property(&mut self, _data: &[u8]) -> Result<Vec<u8>, String> {
        // TODO: store properties
        Ok(Vec::new())
    }

    fn handle_get_property(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let _wid = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let _property = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);

        // Return empty property for now
        let mut reply = vec![0u8; 32];
        reply[0] = 1;
        reply[1] = 0; // format
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        // rest is zeros (no value)

        Ok(reply)
    }

    // Input focus

    fn handle_set_input_focus(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let wid = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        self.focus = wid;
        Ok(Vec::new())
    }

    fn handle_get_input_focus(&mut self, _data: &[u8]) -> Result<Vec<u8>, String> {
        let mut reply = vec![0u8; 32];
        reply[0] = 1;
        reply[1] = 0; // revert-to
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        reply[8..12].copy_from_slice(&self.focus.to_le_bytes());
        Ok(reply)
    }

    fn handle_query_pointer(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let _wid = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);

        let mut reply = vec![0u8; 32];
        reply[0] = 1;
        reply[1] = 1; // same-screen
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        reply[8..12].copy_from_slice(&self.windows.root_id().to_le_bytes());
        reply[12..16].copy_from_slice(&self.focus.to_le_bytes()); // child
        reply[16..18].copy_from_slice(&self.pointer.x.to_le_bytes());
        reply[18..20].copy_from_slice(&self.pointer.y.to_le_bytes());
        reply[20..22].copy_from_slice(&self.pointer.x.to_le_bytes()); // win-x
        reply[22..24].copy_from_slice(&self.pointer.y.to_le_bytes()); // win-y
        reply[24..26].copy_from_slice(&(self.keyboard.modifiers | self.pointer.button_mask()).to_le_bytes());

        Ok(reply)
    }

    // Pixmap operations

    fn handle_create_pixmap(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let depth = data[1];
        let pid = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let drawable = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        let width = u16::from_le_bytes([data[12], data[13]]);
        let height = u16::from_le_bytes([data[14], data[15]]);

        self.pixmaps.create(pid, drawable, width, height, depth);
        self.renderer.create_texture(pid, width as u32, height as u32);

        log::debug!("CreatePixmap: {} {}x{}", pid, width, height);
        Ok(Vec::new())
    }

    fn handle_free_pixmap(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let pid = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        self.pixmaps.free(pid);
        self.renderer.destroy_texture(pid);
        Ok(Vec::new())
    }

    // GC operations

    fn handle_create_gc(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let gcid = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let drawable = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);

        let gc = GC::new(gcid, drawable);
        self.gcs.create(gc);

        log::debug!("CreateGC: {}", gcid);
        Ok(Vec::new())
    }

    fn handle_change_gc(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let gcid = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let value_mask = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);

        if let Some(gc) = self.gcs.get_mut(gcid) {
            let mut offset = 12;
            if value_mask & 0x04 != 0 { // foreground
                gc.foreground = u32::from_le_bytes([data[offset], data[offset+1], data[offset+2], data[offset+3]]);
                offset += 4;
            }
            if value_mask & 0x08 != 0 { // background
                gc.background = u32::from_le_bytes([data[offset], data[offset+1], data[offset+2], data[offset+3]]);
                // offset += 4;
            }
            // TODO: handle more GC values
        }

        Ok(Vec::new())
    }

    fn handle_free_gc(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let gcid = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        self.gcs.free(gcid);
        Ok(Vec::new())
    }

    // Drawing operations

    fn handle_clear_area(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let wid = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let x = u16::from_le_bytes([data[8], data[9]]) as u32;
        let y = u16::from_le_bytes([data[10], data[11]]) as u32;
        let width = u16::from_le_bytes([data[12], data[13]]) as u32;
        let height = u16::from_le_bytes([data[14], data[15]]) as u32;

        if let Some(window) = self.windows.get(wid) {
            let bg = window.attributes.background_pixel.unwrap_or(0x000000);
            let w = if width == 0 { window.width as u32 - x } else { width };
            let h = if height == 0 { window.height as u32 - y } else { height };
            self.renderer.fill_rect(wid, x, y, w, h, bg);
        }

        Ok(Vec::new())
    }

    fn handle_poly_fill_rectangle(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let drawable = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let gcid = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);

        let foreground = self.gcs.get(gcid).map(|gc| gc.foreground).unwrap_or(0xffffff);

        // Parse rectangles
        let mut offset = 12;
        while offset + 8 <= data.len() {
            let x = i16::from_le_bytes([data[offset], data[offset + 1]]);
            let y = i16::from_le_bytes([data[offset + 2], data[offset + 3]]);
            let width = u16::from_le_bytes([data[offset + 4], data[offset + 5]]);
            let height = u16::from_le_bytes([data[offset + 6], data[offset + 7]]);

            self.renderer.fill_rect(drawable, x as u32, y as u32, width as u32, height as u32, foreground);
            offset += 8;
        }

        Ok(Vec::new())
    }

    fn handle_put_image(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let format = data[1];
        let drawable = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let _gcid = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        let width = u16::from_le_bytes([data[12], data[13]]) as u32;
        let height = u16::from_le_bytes([data[14], data[15]]) as u32;
        let dst_x = i16::from_le_bytes([data[16], data[17]]) as u32;
        let dst_y = i16::from_le_bytes([data[18], data[19]]) as u32;
        let _left_pad = data[20];
        let depth = data[21];

        let image_data = &data[24..];

        // Convert to RGBA
        let rgba = match (format, depth) {
            (2, 24) | (2, 32) => {
                // ZPixmap, 24/32 bit - already close to RGBA
                let mut rgba = Vec::with_capacity((width * height * 4) as usize);
                let bytes_per_pixel = if depth == 24 { 3 } else { 4 };
                let row_bytes = ((width * bytes_per_pixel as u32 + 3) & !3) as usize;

                for y in 0..height as usize {
                    for x in 0..width as usize {
                        let offset = y * row_bytes + x * bytes_per_pixel;
                        if offset + 2 < image_data.len() {
                            // X11 is usually BGRX
                            let b = image_data[offset];
                            let g = image_data[offset + 1];
                            let r = image_data[offset + 2];
                            rgba.extend_from_slice(&[r, g, b, 255]);
                        }
                    }
                }
                rgba
            }
            _ => {
                log::warn!("unsupported image format: format={} depth={}", format, depth);
                return Ok(Vec::new());
            }
        };

        self.renderer.update_texture(drawable, dst_x, dst_y, width, height, &rgba);

        Ok(Vec::new())
    }

    // Extensions

    fn handle_query_extension(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let name_len = u16::from_le_bytes([data[4], data[5]]) as usize;
        let name = std::str::from_utf8(&data[8..8 + name_len]).unwrap_or("");

        log::debug!("QueryExtension: {}", name);

        // Return "not present" for all extensions for now
        let mut reply = vec![0u8; 32];
        reply[0] = 1;
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        reply[8] = 0; // present = false

        Ok(reply)
    }

    fn handle_get_keyboard_mapping(&mut self, _data: &[u8]) -> Result<Vec<u8>, String> {
        // Return minimal keyboard mapping
        let keysyms_per_keycode = 2u8;
        let count = 248u8; // 256 - 8 (first keycode)
        let total_syms = count as usize * keysyms_per_keycode as usize;

        let mut reply = vec![0u8; 32 + total_syms * 4];
        reply[0] = 1;
        reply[1] = keysyms_per_keycode;
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        reply[4..8].copy_from_slice(&(total_syms as u32).to_le_bytes());

        // Fill with basic Latin-1 mapping
        for i in 0..count as usize {
            let keycode = i + 8;
            let offset = 32 + i * keysyms_per_keycode as usize * 4;
            // Just map to ASCII for now
            if keycode >= 0x20 && keycode < 0x80 {
                reply[offset..offset + 4].copy_from_slice(&(keycode as u32).to_le_bytes());
            }
        }

        Ok(reply)
    }

    fn handle_get_modifier_mapping(&mut self, _data: &[u8]) -> Result<Vec<u8>, String> {
        let keycodes_per_modifier = 2u8;

        let mut reply = vec![0u8; 32 + 8 * keycodes_per_modifier as usize];
        reply[0] = 1;
        reply[1] = keycodes_per_modifier;
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        reply[4..8].copy_from_slice(&(keycodes_per_modifier as u32 * 2).to_le_bytes());

        // Shift
        reply[32] = 50; // Shift_L
        reply[33] = 62; // Shift_R
        // Lock
        reply[34] = 66; // Caps_Lock
        // Control
        reply[36] = 37; // Control_L
        reply[37] = 105; // Control_R
        // Mod1 (Alt)
        reply[38] = 64; // Alt_L
        reply[39] = 108; // Alt_R
        // Mod2
        reply[40] = 77; // Num_Lock
        // Mod4 (Super)
        reply[44] = 133; // Super_L
        reply[45] = 134; // Super_R

        Ok(reply)
    }

    // Error reply helper
    fn error_reply(&self, error: u8, bad_value: u32) -> Result<Vec<u8>, String> {
        let mut reply = vec![0u8; 32];
        reply[0] = 0; // error
        reply[1] = error;
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        reply[4..8].copy_from_slice(&bad_value.to_le_bytes());
        Ok(reply)
    }

    // Events

    fn send_map_notify(&mut self, window: u32) {
        let mut event = vec![0u8; 32];
        event[0] = 19; // MapNotify
        event[4..8].copy_from_slice(&window.to_le_bytes()); // event window
        event[8..12].copy_from_slice(&window.to_le_bytes()); // window
        event[12] = 0; // override-redirect
        self.events.push_back(event);
    }

    // Input handling

    pub fn handle_key_event(&mut self, code: &str, _key: &str, _modifiers: u32, pressed: bool) {
        let keycode = self.keyboard.js_to_keycode(code);
        if keycode == 0 {
            return;
        }

        if pressed {
            self.keyboard.key_press(keycode);
        } else {
            self.keyboard.key_release(keycode);
        }

        let mut event = vec![0u8; 32];
        event[0] = if pressed { 2 } else { 3 }; // KeyPress/KeyRelease
        event[1] = keycode;
        event[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        event[4..8].copy_from_slice(&0u32.to_le_bytes()); // time
        event[8..12].copy_from_slice(&self.windows.root_id().to_le_bytes());
        event[12..16].copy_from_slice(&self.focus.to_le_bytes()); // event window
        event[16..20].copy_from_slice(&self.focus.to_le_bytes()); // child
        event[20..22].copy_from_slice(&self.pointer.x.to_le_bytes());
        event[22..24].copy_from_slice(&self.pointer.y.to_le_bytes());
        event[24..26].copy_from_slice(&self.pointer.x.to_le_bytes()); // event-x
        event[26..28].copy_from_slice(&self.pointer.y.to_le_bytes()); // event-y
        event[28..30].copy_from_slice(&self.keyboard.modifiers.to_le_bytes());
        event[30] = 1; // same-screen

        self.events.push_back(event);
    }

    pub fn handle_mouse_move(&mut self, x: i32, y: i32) {
        self.pointer.move_to(x as i16, y as i16);

        let mut event = vec![0u8; 32];
        event[0] = 6; // MotionNotify
        event[1] = 0; // detail (Normal)
        event[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        event[4..8].copy_from_slice(&0u32.to_le_bytes()); // time
        event[8..12].copy_from_slice(&self.windows.root_id().to_le_bytes());
        event[12..16].copy_from_slice(&self.focus.to_le_bytes());
        event[16..20].copy_from_slice(&0u32.to_le_bytes()); // child
        event[20..22].copy_from_slice(&(x as i16).to_le_bytes());
        event[22..24].copy_from_slice(&(y as i16).to_le_bytes());
        event[24..26].copy_from_slice(&(x as i16).to_le_bytes());
        event[26..28].copy_from_slice(&(y as i16).to_le_bytes());
        event[28..30].copy_from_slice(&(self.keyboard.modifiers | self.pointer.button_mask()).to_le_bytes());
        event[30] = 1;

        self.events.push_back(event);
    }

    pub fn handle_mouse_button(&mut self, button: u8, pressed: bool, x: i32, y: i32) {
        if pressed {
            self.pointer.button_press(button);
        } else {
            self.pointer.button_release(button);
        }

        let mut event = vec![0u8; 32];
        event[0] = if pressed { 4 } else { 5 }; // ButtonPress/ButtonRelease
        event[1] = button;
        event[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        event[4..8].copy_from_slice(&0u32.to_le_bytes()); // time
        event[8..12].copy_from_slice(&self.windows.root_id().to_le_bytes());
        event[12..16].copy_from_slice(&self.focus.to_le_bytes());
        event[16..20].copy_from_slice(&0u32.to_le_bytes()); // child
        event[20..22].copy_from_slice(&(x as i16).to_le_bytes());
        event[22..24].copy_from_slice(&(y as i16).to_le_bytes());
        event[24..26].copy_from_slice(&(x as i16).to_le_bytes());
        event[26..28].copy_from_slice(&(y as i16).to_le_bytes());
        event[28..30].copy_from_slice(&(self.keyboard.modifiers | self.pointer.button_mask()).to_le_bytes());
        event[30] = 1;

        self.events.push_back(event);
    }

    pub fn drain_events(&mut self) -> Vec<u8> {
        let mut result = Vec::new();
        while let Some(event) = self.events.pop_front() {
            result.extend(event);
        }
        result
    }

    pub fn render(&mut self) -> Result<(), String> {
        let windows = self.windows.mapped_windows();
        self.renderer.render(&windows)
    }
}
