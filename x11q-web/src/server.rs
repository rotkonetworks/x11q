//! X11 Server implementation

use crate::atom::AtomStore;
use crate::drawable::PixmapStore;
use crate::gc::{GCStore, GC};
use crate::input::{Keyboard, Pointer};
use crate::renderer::Renderer;
use crate::window::{Window, WindowClass, WindowTree};
use std::collections::{HashMap, VecDeque};

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
    pub const GET_SELECTION_OWNER: u8 = 23;
    pub const GRAB_POINTER: u8 = 26;
    pub const UNGRAB_POINTER: u8 = 27;
    pub const GRAB_KEYBOARD: u8 = 31;
    pub const UNGRAB_KEYBOARD: u8 = 32;
    pub const QUERY_POINTER: u8 = 38;
    pub const SET_INPUT_FOCUS: u8 = 42;
    pub const GET_INPUT_FOCUS: u8 = 43;
    pub const OPEN_FONT: u8 = 45;
    pub const CLOSE_FONT: u8 = 46;
    pub const QUERY_FONT: u8 = 47;
    pub const LIST_FONTS_WITH_INFO: u8 = 49;
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
    pub const QUERY_COLORS: u8 = 91;
    pub const QUERY_BEST_SIZE: u8 = 97;
    pub const QUERY_EXTENSION: u8 = 98;
    pub const QUERY_KEYMAP: u8 = 99;
    pub const GET_KEYBOARD_MAPPING: u8 = 101;
    pub const GET_MODIFIER_MAPPING: u8 = 119;

    // Extension major opcodes
    pub const GE_MAJOR: u8 = 128;
    pub const XINPUT_MAJOR: u8 = 131;
    pub const XKB_MAJOR: u8 = 135;
    pub const RANDR_MAJOR: u8 = 140;
}

/// Connection state
#[derive(Debug, Clone, Copy, PartialEq)]
enum ConnectionState {
    /// Waiting for initial connection setup
    AwaitingSetup,
    /// Connected and processing requests
    Connected,
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
    /// Connection state
    state: ConnectionState,
    /// Resource ID base for this client
    resource_id_base: u32,
    /// Resource ID mask
    resource_id_mask: u32,
    /// Extensions: name -> (major_opcode, first_event, first_error)
    extensions: HashMap<String, (u8, u8, u8)>,
}

impl X11Server {
    pub fn new(renderer: Renderer, width: u32, height: u32) -> Self {
        let root_id = 0x00000001;
        let windows = WindowTree::new(root_id, width as u16, height as u16);

        let mut extensions = HashMap::new();
        // RANDR: major_opcode=140, first_event=89, first_error=147
        extensions.insert("RANDR".to_string(), (opcodes::RANDR_MAJOR, 89, 147));
        // XInputExtension: major_opcode=131, first_event=147, first_error=135
        extensions.insert("XInputExtension".to_string(), (opcodes::XINPUT_MAJOR, 147, 135));
        // XKEYBOARD: major_opcode=135, first_event=85, first_error=137
        extensions.insert("XKEYBOARD".to_string(), (opcodes::XKB_MAJOR, 85, 137));
        // Generic Event Extension: major_opcode=128, first_event=0, first_error=0
        extensions.insert("Generic Event Extension".to_string(), (opcodes::GE_MAJOR, 0, 0));

        Self {
            renderer,
            windows,
            pixmaps: PixmapStore::new(),
            gcs: GCStore::new(),
            atoms: AtomStore::new(),
            keyboard: Keyboard::new(),
            pointer: Pointer::new(),
            events: VecDeque::new(),
            next_id: 0x00200000,
            focus: root_id,
            width,
            height,
            sequence: 0,
            state: ConnectionState::AwaitingSetup,
            resource_id_base: 0x00200000,
            resource_id_mask: 0x001fffff,
            extensions,
        }
    }

    /// Reset server state for a new connection
    pub fn reset(&mut self) {
        self.state = ConnectionState::AwaitingSetup;
        self.sequence = 0;
        self.events.clear();
        self.windows = WindowTree::new(self.windows.root_id(), self.width as u16, self.height as u16);
        self.pixmaps = PixmapStore::new();
        self.gcs = GCStore::new();
        self.atoms = AtomStore::new();
        self.focus = self.windows.root_id();
        log::info!("server state reset - full");
    }

    /// Check if we're still awaiting connection setup
    pub fn is_awaiting_setup(&self) -> bool {
        self.state == ConnectionState::AwaitingSetup
    }

    /// Process X11 protocol data, return reply if any
    pub fn process_request(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        match self.state {
            ConnectionState::AwaitingSetup => self.handle_connection_setup(data),
            ConnectionState::Connected => self.handle_request(data),
        }
    }

    /// Handle initial connection setup
    fn handle_connection_setup(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        if data.len() < 12 {
            return Err("connection setup too short".into());
        }

        let byte_order = data[0];
        let _protocol_major = u16::from_le_bytes([data[2], data[3]]);
        let _protocol_minor = u16::from_le_bytes([data[4], data[5]]);
        let auth_name_len = u16::from_le_bytes([data[6], data[7]]) as usize;
        let auth_data_len = u16::from_le_bytes([data[8], data[9]]) as usize;

        log::info!("X11 connection setup: byte_order={:#x} auth_name_len={} auth_data_len={}",
            byte_order, auth_name_len, auth_data_len);

        // Build connection setup reply
        let reply = self.build_connection_reply();

        self.state = ConnectionState::Connected;
        log::info!("X11 connection established");

        Ok(reply)
    }

    /// Build the connection setup reply
    fn build_connection_reply(&self) -> Vec<u8> {
        let vendor = b"x11q-web";
        let vendor_len = vendor.len();
        let vendor_pad = (4 - (vendor_len % 4)) % 4;
        let vendor_total = vendor_len + vendor_pad;

        let num_formats = 1u8;
        let format_size = 8 * num_formats as usize;

        // Screen: 40 bytes base + depths
        // Each depth: 8 bytes header + 24 bytes per visual
        let screen_size = 40;
        let depth_size = 8 + 24; // 1 depth with 1 visual

        // Additional data = 32 bytes fixed + vendor + formats + screens
        let fixed_size = 32;
        let additional_data_len = fixed_size + vendor_total + format_size + screen_size + depth_size;
        let additional_words = additional_data_len / 4;

        let root_id = self.windows.root_id();
        let root_visual = 0x21u32;
        let default_colormap = 0x20u32;

        let mut reply = Vec::with_capacity(8 + additional_data_len);

        // Header (8 bytes)
        reply.push(1); // success
        reply.push(0); // unused
        reply.extend_from_slice(&11u16.to_le_bytes()); // protocol major
        reply.extend_from_slice(&0u16.to_le_bytes());  // protocol minor
        reply.extend_from_slice(&(additional_words as u16).to_le_bytes());

        // Fixed fields (32 bytes)
        reply.extend_from_slice(&0u32.to_le_bytes()); // release number
        reply.extend_from_slice(&self.resource_id_base.to_le_bytes());
        reply.extend_from_slice(&self.resource_id_mask.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes()); // motion buffer size
        reply.extend_from_slice(&(vendor_len as u16).to_le_bytes());
        reply.extend_from_slice(&0xffffu16.to_le_bytes()); // max request length
        reply.push(1); // number of screens
        reply.push(num_formats); // number of formats
        reply.push(0); // image byte order (LSB first)
        reply.push(0); // bitmap bit order (LSB first)
        reply.push(8); // bitmap scanline unit
        reply.push(32); // bitmap scanline pad
        reply.push(8); // min keycode
        reply.push(255); // max keycode
        reply.extend_from_slice(&[0u8; 4]); // unused padding

        // Vendor string (padded to 4 bytes)
        reply.extend_from_slice(vendor);
        for _ in 0..vendor_pad {
            reply.push(0);
        }

        // Pixmap formats (8 bytes each)
        reply.push(24); // depth
        reply.push(32); // bits per pixel
        reply.push(32); // scanline pad
        reply.extend_from_slice(&[0u8; 5]); // padding

        // Screen info (40 bytes)
        reply.extend_from_slice(&root_id.to_le_bytes()); // root window
        reply.extend_from_slice(&default_colormap.to_le_bytes()); // default colormap
        reply.extend_from_slice(&0xffffffffu32.to_le_bytes()); // white pixel
        reply.extend_from_slice(&0x00000000u32.to_le_bytes()); // black pixel
        reply.extend_from_slice(&0u32.to_le_bytes()); // current input masks
        reply.extend_from_slice(&(self.width as u16).to_le_bytes()); // width in pixels
        reply.extend_from_slice(&(self.height as u16).to_le_bytes()); // height in pixels
        reply.extend_from_slice(&((self.width / 4) as u16).to_le_bytes()); // width in mm
        reply.extend_from_slice(&((self.height / 4) as u16).to_le_bytes()); // height in mm
        reply.extend_from_slice(&1u16.to_le_bytes()); // min installed maps
        reply.extend_from_slice(&1u16.to_le_bytes()); // max installed maps
        reply.extend_from_slice(&root_visual.to_le_bytes()); // root visual
        reply.push(0); // backing stores (Never)
        reply.push(1); // save unders (Yes)
        reply.push(24); // root depth
        reply.push(1); // number of allowed depths

        // Depth info (8 bytes header)
        reply.push(24); // depth
        reply.push(0); // unused
        reply.extend_from_slice(&1u16.to_le_bytes()); // number of visuals
        reply.extend_from_slice(&[0u8; 4]); // padding

        // Visual info (24 bytes)
        reply.extend_from_slice(&root_visual.to_le_bytes()); // visual ID
        reply.push(4); // class (TrueColor)
        reply.push(8); // bits per RGB value
        reply.extend_from_slice(&256u16.to_le_bytes()); // colormap entries
        reply.extend_from_slice(&0x00ff0000u32.to_le_bytes()); // red mask
        reply.extend_from_slice(&0x0000ff00u32.to_le_bytes()); // green mask
        reply.extend_from_slice(&0x000000ffu32.to_le_bytes()); // blue mask
        reply.extend_from_slice(&[0u8; 4]); // padding

        log::info!("connection reply: {} bytes (header says 8+{}*4={} bytes)",
            reply.len(), additional_words, 8 + additional_words * 4);

        reply
    }

    /// Handle a regular X11 request
    fn handle_request(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        if data.len() < 4 {
            return Err("request too short".into());
        }

        let opcode = data[0];
        let detail = data[1];
        let length = u16::from_le_bytes([data[2], data[3]]) as usize * 4;

        if length > 0 && data.len() < length {
            return Err(format!("incomplete request: have {} need {}", data.len(), length));
        }

        self.sequence = self.sequence.wrapping_add(1);

        log::info!("X11 request: opcode={} detail={} len={} seq={}", opcode, detail, length, self.sequence);

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
            opcodes::GET_SELECTION_OWNER => self.handle_get_selection_owner(data),
            opcodes::QUERY_POINTER => self.handle_query_pointer(data),
            opcodes::SET_INPUT_FOCUS => self.handle_set_input_focus(data),
            opcodes::GET_INPUT_FOCUS => self.handle_get_input_focus(data),
            opcodes::OPEN_FONT => Ok(Vec::new()),
            opcodes::CLOSE_FONT => Ok(Vec::new()),
            opcodes::QUERY_FONT => self.handle_query_font(data),
            opcodes::LIST_FONTS_WITH_INFO => self.handle_list_fonts_with_info(data),
            opcodes::CREATE_PIXMAP => self.handle_create_pixmap(data),
            opcodes::FREE_PIXMAP => self.handle_free_pixmap(data),
            opcodes::CREATE_GC => self.handle_create_gc(data),
            opcodes::CHANGE_GC => self.handle_change_gc(data),
            opcodes::FREE_GC => self.handle_free_gc(data),
            opcodes::CLEAR_AREA => self.handle_clear_area(data),
            opcodes::COPY_AREA => Ok(Vec::new()),
            opcodes::POLY_FILL_RECTANGLE => self.handle_poly_fill_rectangle(data),
            opcodes::PUT_IMAGE => self.handle_put_image(data),
            opcodes::GET_IMAGE => self.handle_get_image(data),
            opcodes::CREATE_COLORMAP => Ok(Vec::new()),
            opcodes::ALLOC_COLOR => self.handle_alloc_color(data),
            opcodes::QUERY_COLORS => self.handle_query_colors(data),
            opcodes::QUERY_BEST_SIZE => self.handle_query_best_size(data),
            opcodes::QUERY_EXTENSION => self.handle_query_extension(data),
            opcodes::QUERY_KEYMAP => self.handle_query_keymap(data),
            opcodes::GET_KEYBOARD_MAPPING => self.handle_get_keyboard_mapping(data),
            opcodes::GET_MODIFIER_MAPPING => self.handle_get_modifier_mapping(data),
            // Extensions
            opcodes::GE_MAJOR => self.handle_ge(data),
            opcodes::XINPUT_MAJOR => self.handle_xinput(data),
            opcodes::XKB_MAJOR => self.handle_xkb(data),
            opcodes::RANDR_MAJOR => self.handle_randr(data),
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

        log::info!("QueryExtension: {}", name);

        let mut reply = vec![0u8; 32];
        reply[0] = 1;
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());

        if let Some(&(major, first_event, first_error)) = self.extensions.get(name) {
            reply[8] = 1; // present = true
            reply[9] = major; // major-opcode
            reply[10] = first_event; // first-event
            reply[11] = first_error; // first-error
            log::info!("  -> present: major={} event={} error={}", major, first_event, first_error);
        } else {
            reply[8] = 0; // present = false
            log::info!("  -> not present");
        }

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

    fn handle_get_selection_owner(&mut self, _data: &[u8]) -> Result<Vec<u8>, String> {
        let mut reply = vec![0u8; 32];
        reply[0] = 1;
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        reply[8..12].copy_from_slice(&0u32.to_le_bytes()); // owner = None
        Ok(reply)
    }

    fn handle_query_font(&mut self, _data: &[u8]) -> Result<Vec<u8>, String> {
        let mut reply = vec![0u8; 60];
        reply[0] = 1;
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        reply[4..8].copy_from_slice(&7u32.to_le_bytes()); // length
        Ok(reply)
    }

    fn handle_list_fonts_with_info(&mut self, _data: &[u8]) -> Result<Vec<u8>, String> {
        // Return "no more fonts" reply (name length 0 terminates)
        let mut reply = vec![0u8; 60];
        reply[0] = 1;
        reply[1] = 0; // name-length = 0 means no more fonts
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        reply[4..8].copy_from_slice(&7u32.to_le_bytes()); // length
        Ok(reply)
    }

    fn handle_get_image(&mut self, _data: &[u8]) -> Result<Vec<u8>, String> {
        let mut reply = vec![0u8; 32];
        reply[0] = 1;
        reply[1] = 24; // depth
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        reply[8..12].copy_from_slice(&0x21u32.to_le_bytes()); // visual
        Ok(reply)
    }

    fn handle_alloc_color(&mut self, _data: &[u8]) -> Result<Vec<u8>, String> {
        let mut reply = vec![0u8; 32];
        reply[0] = 1;
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        reply[8..10].copy_from_slice(&0xffffu16.to_le_bytes()); // red
        reply[10..12].copy_from_slice(&0xffffu16.to_le_bytes()); // green
        reply[12..14].copy_from_slice(&0xffffu16.to_le_bytes()); // blue
        reply[16..20].copy_from_slice(&0xffffffu32.to_le_bytes()); // pixel
        Ok(reply)
    }

    fn handle_query_colors(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let num_pixels = if data.len() >= 8 { (data.len() - 8) / 4 } else { 0 };
        let mut reply = vec![0u8; 32 + num_pixels * 8];
        reply[0] = 1;
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        reply[4..8].copy_from_slice(&((num_pixels * 2) as u32).to_le_bytes());
        reply[8..10].copy_from_slice(&(num_pixels as u16).to_le_bytes());
        for i in 0..num_pixels {
            let offset = 32 + i * 8;
            reply[offset..offset+2].copy_from_slice(&0xffffu16.to_le_bytes());
            reply[offset+2..offset+4].copy_from_slice(&0xffffu16.to_le_bytes());
            reply[offset+4..offset+6].copy_from_slice(&0xffffu16.to_le_bytes());
        }
        Ok(reply)
    }

    fn handle_query_best_size(&mut self, _data: &[u8]) -> Result<Vec<u8>, String> {
        let mut reply = vec![0u8; 32];
        reply[0] = 1;
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        reply[8..10].copy_from_slice(&16u16.to_le_bytes()); // width
        reply[10..12].copy_from_slice(&16u16.to_le_bytes()); // height
        Ok(reply)
    }

    fn handle_query_keymap(&mut self, _data: &[u8]) -> Result<Vec<u8>, String> {
        let mut reply = vec![0u8; 32];
        reply[0] = 1;
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        Ok(reply)
    }

    // Extension handlers

    fn handle_ge(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let minor = data[1];
        log::info!("GE request: minor={}", minor);

        match minor {
            0 => {
                // GEQueryVersion - return version 1.0
                let mut reply = vec![0u8; 32];
                reply[0] = 1;
                reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
                reply[8..10].copy_from_slice(&1u16.to_le_bytes()); // major
                reply[10..12].copy_from_slice(&0u16.to_le_bytes()); // minor
                Ok(reply)
            }
            _ => {
                log::warn!("unhandled GE minor: {}", minor);
                Ok(Vec::new())
            }
        }
    }

    fn handle_xinput(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let minor = data[1];
        log::info!("XInput request: minor={}", minor);

        match minor {
            1 => self.xinput_query_version(),
            46 => self.xinput_query_device(),
            47 => self.xinput_query_pointer(),
            48 => self.xinput_get_focus(),
            52 => Ok(Vec::new()), // XISelectEvents - no reply
            61 => self.xinput_get_selected_events(),
            _ => {
                log::warn!("unhandled XInput minor: {}", minor);
                Ok(Vec::new())
            }
        }
    }

    fn xinput_query_version(&self) -> Result<Vec<u8>, String> {
        let mut reply = vec![0u8; 32];
        reply[0] = 1;
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        reply[8..10].copy_from_slice(&2u16.to_le_bytes()); // major
        reply[10..12].copy_from_slice(&3u16.to_le_bytes()); // minor
        Ok(reply)
    }

    fn xinput_query_device(&self) -> Result<Vec<u8>, String> {
        let num_devices = 2u16;
        let mut devices = Vec::new();

        // Master pointer (device ID 2)
        devices.extend_from_slice(&2u16.to_le_bytes());
        devices.extend_from_slice(&1u16.to_le_bytes()); // type = MasterPointer
        devices.extend_from_slice(&3u16.to_le_bytes()); // attachment
        devices.extend_from_slice(&0u16.to_le_bytes()); // num_classes
        devices.extend_from_slice(&14u16.to_le_bytes()); // name_len
        devices.push(1); // enabled
        devices.push(0);
        devices.extend_from_slice(b"Master pointer");
        devices.extend_from_slice(&[0u8; 2]); // pad to 4

        // Master keyboard (device ID 3)
        devices.extend_from_slice(&3u16.to_le_bytes());
        devices.extend_from_slice(&2u16.to_le_bytes()); // type = MasterKeyboard
        devices.extend_from_slice(&2u16.to_le_bytes()); // attachment
        devices.extend_from_slice(&0u16.to_le_bytes()); // num_classes
        devices.extend_from_slice(&15u16.to_le_bytes()); // name_len
        devices.push(1); // enabled
        devices.push(0);
        devices.extend_from_slice(b"Master keyboard");
        devices.push(0); // pad to 4

        let length = devices.len() / 4;
        let mut reply = vec![0u8; 32];
        reply[0] = 1;
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        reply[4..8].copy_from_slice(&(length as u32).to_le_bytes());
        reply[8..10].copy_from_slice(&num_devices.to_le_bytes());
        reply.extend_from_slice(&devices);
        Ok(reply)
    }

    fn xinput_query_pointer(&self) -> Result<Vec<u8>, String> {
        let mut reply = vec![0u8; 56];
        reply[0] = 1;
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        reply[4..8].copy_from_slice(&4u32.to_le_bytes()); // length
        reply[8..12].copy_from_slice(&self.windows.root_id().to_le_bytes());
        // root_x, root_y, win_x, win_y as Fixed (16.16)
        let pos = (self.pointer.x as i32) << 16;
        reply[16..20].copy_from_slice(&pos.to_le_bytes());
        reply[20..24].copy_from_slice(&pos.to_le_bytes());
        reply[24..28].copy_from_slice(&pos.to_le_bytes());
        reply[28..32].copy_from_slice(&pos.to_le_bytes());
        reply[32] = 1; // same_screen
        Ok(reply)
    }

    fn xinput_get_focus(&self) -> Result<Vec<u8>, String> {
        let mut reply = vec![0u8; 32];
        reply[0] = 1;
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        reply[8..12].copy_from_slice(&self.windows.root_id().to_le_bytes());
        Ok(reply)
    }

    fn xinput_get_selected_events(&self) -> Result<Vec<u8>, String> {
        let mut reply = vec![0u8; 32];
        reply[0] = 1;
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        reply[8..10].copy_from_slice(&0u16.to_le_bytes()); // num_masks = 0
        Ok(reply)
    }

    fn handle_xkb(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let minor = data[1];
        log::info!("XKB request: minor={}", minor);

        match minor {
            0 => self.xkb_use_extension(),
            1 => Ok(Vec::new()), // XkbSelectEvents - no reply
            6 => self.xkb_get_controls(),
            8 => self.xkb_get_map(),
            9 => Ok(Vec::new()), // XkbSetMap - no reply
            10 => self.xkb_get_compat_map(),
            13 => self.xkb_get_indicator_map(),
            17 | 24 => self.xkb_get_names(),
            _ => {
                log::warn!("unhandled XKB minor: {}", minor);
                Ok(Vec::new())
            }
        }
    }

    fn xkb_use_extension(&self) -> Result<Vec<u8>, String> {
        let mut reply = vec![0u8; 32];
        reply[0] = 1;
        reply[1] = 1; // supported
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        reply[8..10].copy_from_slice(&1u16.to_le_bytes()); // major
        reply[10..12].copy_from_slice(&0u16.to_le_bytes()); // minor
        Ok(reply)
    }

    fn xkb_get_map(&self) -> Result<Vec<u8>, String> {
        let mut reply = vec![0u8; 40];
        reply[0] = 1;
        reply[1] = 3; // deviceID
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        reply[4..8].copy_from_slice(&2u32.to_le_bytes()); // length
        reply[10] = 8; // min keycode
        reply[11] = 255; // max keycode
        Ok(reply)
    }

    fn xkb_get_names(&self) -> Result<Vec<u8>, String> {
        let mut reply = vec![0u8; 32];
        reply[0] = 1;
        reply[1] = 3; // deviceID
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        reply[12] = 8; // min keycode
        reply[13] = 255; // max keycode
        Ok(reply)
    }

    fn xkb_get_controls(&self) -> Result<Vec<u8>, String> {
        let mut reply = vec![0u8; 92];
        reply[0] = 1;
        reply[1] = 3; // deviceID
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        reply[4..8].copy_from_slice(&15u32.to_le_bytes()); // length
        reply[9] = 1; // numGroups
        reply[22..24].copy_from_slice(&500u16.to_le_bytes()); // repeatDelay
        reply[24..26].copy_from_slice(&30u16.to_le_bytes()); // repeatInterval
        // perKeyRepeat (32 bytes) at offset 60
        for i in 60..92 {
            reply[i] = 0xff;
        }
        Ok(reply)
    }

    fn xkb_get_compat_map(&self) -> Result<Vec<u8>, String> {
        let mut reply = vec![0u8; 32];
        reply[0] = 1;
        reply[1] = 3; // deviceID
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        Ok(reply)
    }

    fn xkb_get_indicator_map(&self) -> Result<Vec<u8>, String> {
        let mut reply = vec![0u8; 32];
        reply[0] = 1;
        reply[1] = 3; // deviceID
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        Ok(reply)
    }

    fn handle_randr(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let minor = data[1];
        log::info!("RANDR request: minor={}", minor);

        match minor {
            0 => self.randr_query_version(),
            4 => Ok(Vec::new()), // RRSelectInput - no reply
            5 => self.randr_get_screen_resources(),
            6 => self.randr_get_output_info(),
            9 => self.randr_get_crtc_info(),
            25 => self.randr_get_output_primary(),
            31 => self.randr_get_providers(),
            _ => {
                log::warn!("unhandled RANDR minor: {}", minor);
                Ok(Vec::new())
            }
        }
    }

    fn randr_query_version(&self) -> Result<Vec<u8>, String> {
        let mut reply = vec![0u8; 32];
        reply[0] = 1;
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        reply[8..12].copy_from_slice(&1u32.to_le_bytes()); // major
        reply[12..16].copy_from_slice(&6u32.to_le_bytes()); // minor
        Ok(reply)
    }

    fn randr_get_screen_resources(&self) -> Result<Vec<u8>, String> {
        let num_crtcs = 1u16;
        let num_outputs = 1u16;
        let num_modes = 1u16;
        let names_len = 8u16;

        let length = (8 + num_crtcs as u32 * 4 + num_outputs as u32 * 4 + num_modes as u32 * 32 + names_len as u32) / 4;

        let mut reply = vec![0u8; 32];
        reply[0] = 1;
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        reply[4..8].copy_from_slice(&length.to_le_bytes());
        reply[16..18].copy_from_slice(&num_crtcs.to_le_bytes());
        reply[18..20].copy_from_slice(&num_outputs.to_le_bytes());
        reply[20..22].copy_from_slice(&num_modes.to_le_bytes());
        reply[22..24].copy_from_slice(&names_len.to_le_bytes());

        // CRTCs
        reply.extend_from_slice(&1u32.to_le_bytes());
        // Outputs
        reply.extend_from_slice(&1u32.to_le_bytes());
        // Mode (32 bytes)
        reply.extend_from_slice(&1u32.to_le_bytes()); // id
        reply.extend_from_slice(&(self.width as u16).to_le_bytes());
        reply.extend_from_slice(&(self.height as u16).to_le_bytes());
        reply.extend_from_slice(&60000000u32.to_le_bytes()); // dotClock
        reply.extend_from_slice(&(self.width as u16).to_le_bytes());
        reply.extend_from_slice(&(self.width as u16).to_le_bytes());
        reply.extend_from_slice(&(self.width as u16).to_le_bytes());
        reply.extend_from_slice(&0u16.to_le_bytes()); // hSkew
        reply.extend_from_slice(&(self.height as u16).to_le_bytes());
        reply.extend_from_slice(&(self.height as u16).to_le_bytes());
        reply.extend_from_slice(&(self.height as u16).to_le_bytes());
        reply.extend_from_slice(&7u16.to_le_bytes()); // name_len
        reply.extend_from_slice(&0u32.to_le_bytes()); // flags
        // Names
        reply.extend_from_slice(b"default\0");

        Ok(reply)
    }

    fn randr_get_output_info(&self) -> Result<Vec<u8>, String> {
        let num_crtcs = 1u16;
        let num_modes = 1u16;
        let name_len = 7u16;

        let length = (12 + num_crtcs as u32 * 4 + num_modes as u32 * 4 + ((name_len as u32 + 3) & !3)) / 4;

        let mut reply = vec![0u8; 36];
        reply[0] = 1;
        reply[1] = 0; // status
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        reply[4..8].copy_from_slice(&length.to_le_bytes());
        reply[12..16].copy_from_slice(&1u32.to_le_bytes()); // crtc
        reply[16..20].copy_from_slice(&((self.width / 4) as u32).to_le_bytes());
        reply[20..24].copy_from_slice(&((self.height / 4) as u32).to_le_bytes());
        reply[24] = 1; // connection = Connected
        reply[26..28].copy_from_slice(&num_crtcs.to_le_bytes());
        reply[28..30].copy_from_slice(&num_modes.to_le_bytes());
        reply[30..32].copy_from_slice(&1u16.to_le_bytes()); // preferred
        reply[34..36].copy_from_slice(&name_len.to_le_bytes());

        reply.extend_from_slice(&1u32.to_le_bytes()); // CRTC
        reply.extend_from_slice(&1u32.to_le_bytes()); // Mode
        reply.extend_from_slice(b"default\0");

        Ok(reply)
    }

    fn randr_get_crtc_info(&self) -> Result<Vec<u8>, String> {
        let num_outputs = 1u16;
        let num_possible = 1u16;

        let length = (8 + num_outputs as u32 * 4 + num_possible as u32 * 4) / 4;

        let mut reply = vec![0u8; 32];
        reply[0] = 1;
        reply[1] = 0; // status
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        reply[4..8].copy_from_slice(&length.to_le_bytes());
        reply[12..14].copy_from_slice(&0i16.to_le_bytes()); // x
        reply[14..16].copy_from_slice(&0i16.to_le_bytes()); // y
        reply[16..18].copy_from_slice(&(self.width as u16).to_le_bytes());
        reply[18..20].copy_from_slice(&(self.height as u16).to_le_bytes());
        reply[20..24].copy_from_slice(&1u32.to_le_bytes()); // mode
        reply[24..26].copy_from_slice(&1u16.to_le_bytes()); // rotation
        reply[26..28].copy_from_slice(&1u16.to_le_bytes()); // rotations
        reply[28..30].copy_from_slice(&num_outputs.to_le_bytes());
        reply[30..32].copy_from_slice(&num_possible.to_le_bytes());

        reply.extend_from_slice(&1u32.to_le_bytes()); // output
        reply.extend_from_slice(&1u32.to_le_bytes()); // possible

        Ok(reply)
    }

    fn randr_get_output_primary(&self) -> Result<Vec<u8>, String> {
        let mut reply = vec![0u8; 32];
        reply[0] = 1;
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        reply[8..12].copy_from_slice(&1u32.to_le_bytes()); // output
        Ok(reply)
    }

    fn randr_get_providers(&self) -> Result<Vec<u8>, String> {
        let mut reply = vec![0u8; 32];
        reply[0] = 1;
        reply[2..4].copy_from_slice(&self.sequence.to_le_bytes());
        reply[12..14].copy_from_slice(&0u16.to_le_bytes()); // num_providers
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
