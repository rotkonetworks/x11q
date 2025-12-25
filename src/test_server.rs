//! Simple X11 server test - runs protocol handler natively without wasm
//!
//! This bypasses the browser/wasm requirement by implementing minimal
//! X11 protocol handling directly.

use anyhow::Result;
use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;

const X11_UNIX_DIR: &str = "/tmp/.X11-unix";

// Extension major opcodes
const RANDR_MAJOR_OPCODE: u8 = 140;
const XINPUT_MAJOR_OPCODE: u8 = 131;
const XKB_MAJOR_OPCODE: u8 = 135;

struct TestServer {
    sequence: u16,
    root_id: u32,
    width: u32,
    height: u32,
    resource_id_base: u32,
    resource_id_mask: u32,
    atoms: HashMap<String, u32>,
    next_atom: u32,
    extensions: HashMap<String, (u8, u8, u8)>, // name -> (major_opcode, first_event, first_error)
}

impl TestServer {
    fn new() -> Self {
        let mut extensions = HashMap::new();
        // RANDR: major_opcode=140, first_event=89, first_error=147
        extensions.insert("RANDR".to_string(), (RANDR_MAJOR_OPCODE, 89, 147));
        // XInputExtension: major_opcode=131, first_event=147, first_error=135
        extensions.insert("XInputExtension".to_string(), (XINPUT_MAJOR_OPCODE, 147, 135));
        // XKEYBOARD: major_opcode=135, first_event=85, first_error=137
        extensions.insert("XKEYBOARD".to_string(), (XKB_MAJOR_OPCODE, 85, 137));
        // Generic Event Extension: major_opcode=128, first_event=0, first_error=0
        extensions.insert("Generic Event Extension".to_string(), (128, 0, 0));

        Self {
            sequence: 0,
            root_id: 1,
            width: 1280,
            height: 720,
            resource_id_base: 0x00200000,
            resource_id_mask: 0x001fffff,
            atoms: HashMap::new(),
            next_atom: 1,
            extensions,
        }
    }

    fn process(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        if data.is_empty() {
            return Ok(Vec::new());
        }

        // Check if connection setup (first byte is 'l' for little-endian or 'B' for big-endian)
        if data[0] == 0x6c || data[0] == 0x42 {
            if data.len() < 12 {
                return Ok(Vec::new()); // Need more data
            }
            return self.connection_reply();
        }

        // Regular request - minimum 4 bytes
        if data.len() < 4 {
            return Ok(Vec::new());
        }
        self.sequence = self.sequence.wrapping_add(1);
        let opcode = data[0];

        match opcode {
            1 => Ok(Vec::new()), // CreateWindow - no reply
            2 => Ok(Vec::new()), // ChangeWindowAttributes - no reply
            3 => self.get_window_attributes(data), // GetWindowAttributes
            8 => Ok(Vec::new()), // MapWindow - no reply
            9 => Ok(Vec::new()), // DestroySubwindows - no reply
            10 => Ok(Vec::new()), // UnmapWindow - no reply
            11 => Ok(Vec::new()), // UnmapSubwindows - no reply
            4 => Ok(Vec::new()), // DestroyWindow - no reply
            5 => Ok(Vec::new()), // DestroySubwindows - duplicate check
            6 => Ok(Vec::new()), // ChangeSaveSet - no reply
            7 => Ok(Vec::new()), // ReparentWindow - no reply
            12 => Ok(Vec::new()), // ConfigureWindow - no reply
            13 => Ok(Vec::new()), // CirculateWindow - no reply
            14 => self.get_geometry(data), // GetGeometry
            15 => self.query_tree(data), // QueryTree
            16 => self.intern_atom(data), // InternAtom
            17 => self.get_atom_name(data), // GetAtomName
            18 => Ok(Vec::new()), // ChangeProperty - no reply
            20 => self.get_property(data), // GetProperty
            22 => Ok(Vec::new()), // SetSelectionOwner - no reply
            23 => self.get_selection_owner(data), // GetSelectionOwner
            24 => Ok(Vec::new()), // ConvertSelection - no reply
            38 => self.query_pointer(data), // QueryPointer
            42 => Ok(Vec::new()), // SetInputFocus - no reply
            43 => self.get_input_focus(), // GetInputFocus
            55 => Ok(Vec::new()), // CreateGC - no reply
            98 => self.query_extension(data), // QueryExtension
            101 => self.get_keyboard_mapping(data), // GetKeyboardMapping
            119 => self.get_modifier_mapping(), // GetModifierMapping
            44 => self.list_fonts_with_info(), // ListFontsWithInfo - return empty
            45 => Ok(Vec::new()), // OpenFont - no reply
            47 => self.query_font(), // QueryFont
            53 => Ok(Vec::new()), // CreatePixmap - no reply
            54 => Ok(Vec::new()), // FreePixmap - no reply
            60 => Ok(Vec::new()), // FreeGC - no reply
            61 => Ok(Vec::new()), // ClearArea - no reply
            62 => Ok(Vec::new()), // CopyArea - no reply
            63 => Ok(Vec::new()), // CopyPlane - no reply
            64 => Ok(Vec::new()), // PolyPoint - no reply
            65 => Ok(Vec::new()), // PolyLine - no reply
            66 => Ok(Vec::new()), // PolySegment - no reply
            67 => Ok(Vec::new()), // PolyRectangle - no reply
            68 => Ok(Vec::new()), // PolyArc - no reply
            69 => Ok(Vec::new()), // FillPoly - no reply
            70 => Ok(Vec::new()), // PolyFillRectangle - no reply
            71 => Ok(Vec::new()), // PolyFillArc - no reply
            72 => Ok(Vec::new()), // PutImage - no reply
            73 => self.get_image(data), // GetImage
            78 => Ok(Vec::new()), // CreateColormap - no reply
            84 => self.alloc_color(data), // AllocColor
            91 => self.query_colors(data), // QueryColors
            97 => self.query_best_size(data), // QueryBestSize
            99 => self.query_keymap(), // QueryKeymap
            RANDR_MAJOR_OPCODE => self.handle_randr(data), // RandR extension
            XINPUT_MAJOR_OPCODE => self.handle_xinput(data), // XInput extension
            XKB_MAJOR_OPCODE => self.handle_xkb(data), // XKB extension
            128 => self.handle_ge(data), // Generic Event Extension
            _ => {
                eprintln!("unhandled opcode: {} (len {} bytes)", opcode, data.len());
                Ok(Vec::new())
            }
        }
    }

    fn connection_reply(&self) -> Result<Vec<u8>> {
        let vendor = b"x11q-test";
        let vendor_len = vendor.len();
        let vendor_pad = (4 - (vendor_len % 4)) % 4;
        let vendor_total = vendor_len + vendor_pad;
        let num_formats = 1u8;
        let format_size = 8;
        let screen_size = 40;
        let depth_size = 8 + 24;
        let fixed_size = 32;
        let additional_data_len = fixed_size + vendor_total + format_size + screen_size + depth_size;
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

        reply.extend_from_slice(&self.root_id.to_le_bytes());
        reply.extend_from_slice(&0x20u32.to_le_bytes());
        reply.extend_from_slice(&0xffffffffu32.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes());
        reply.extend_from_slice(&(self.width as u16).to_le_bytes());
        reply.extend_from_slice(&(self.height as u16).to_le_bytes());
        reply.extend_from_slice(&((self.width / 4) as u16).to_le_bytes());
        reply.extend_from_slice(&((self.height / 4) as u16).to_le_bytes());
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

        eprintln!("connection reply: {} bytes", reply.len());
        Ok(reply)
    }

    fn get_window_attributes(&self, _data: &[u8]) -> Result<Vec<u8>> {
        let mut reply = vec![1u8]; // reply type
        reply.push(0); // backing-store
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&3u32.to_le_bytes()); // length
        reply.extend_from_slice(&0x21u32.to_le_bytes()); // visual
        reply.extend_from_slice(&1u16.to_le_bytes()); // class
        reply.push(0); // bit-gravity
        reply.push(0); // win-gravity
        reply.extend_from_slice(&0u32.to_le_bytes()); // backing-planes
        reply.extend_from_slice(&0u32.to_le_bytes()); // backing-pixel
        reply.push(0); // save-under
        reply.push(1); // map-is-installed
        reply.push(2); // map-state (Viewable)
        reply.push(0); // override-redirect
        reply.extend_from_slice(&0x20u32.to_le_bytes()); // colormap
        reply.extend_from_slice(&0u32.to_le_bytes()); // all-event-masks
        reply.extend_from_slice(&0u32.to_le_bytes()); // your-event-mask
        reply.extend_from_slice(&0u16.to_le_bytes()); // do-not-propagate-mask
        reply.extend_from_slice(&[0u8; 2]); // padding
        Ok(reply)
    }

    fn get_geometry(&self, data: &[u8]) -> Result<Vec<u8>> {
        let drawable = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let (x, y, w, h) = if drawable == self.root_id {
            (0i16, 0i16, self.width as u16, self.height as u16)
        } else {
            (0, 0, 100, 100)
        };

        let mut reply = vec![1u8];
        reply.push(24); // depth
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes()); // length
        reply.extend_from_slice(&self.root_id.to_le_bytes());
        reply.extend_from_slice(&x.to_le_bytes());
        reply.extend_from_slice(&y.to_le_bytes());
        reply.extend_from_slice(&w.to_le_bytes());
        reply.extend_from_slice(&h.to_le_bytes());
        reply.extend_from_slice(&0u16.to_le_bytes()); // border-width
        reply.extend_from_slice(&[0u8; 10]); // padding
        Ok(reply)
    }

    fn query_tree(&self, data: &[u8]) -> Result<Vec<u8>> {
        let _window = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let mut reply = vec![1u8];
        reply.push(0);
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes()); // length (no children)
        reply.extend_from_slice(&self.root_id.to_le_bytes()); // root
        reply.extend_from_slice(&0u32.to_le_bytes()); // parent (none for root)
        reply.extend_from_slice(&0u16.to_le_bytes()); // num children
        reply.extend_from_slice(&[0u8; 14]); // padding
        Ok(reply)
    }

    fn intern_atom(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        let only_if_exists = data[1] != 0;
        let name_len = u16::from_le_bytes([data[4], data[5]]) as usize;
        let name = String::from_utf8_lossy(&data[8..8+name_len]).to_string();

        let atom = if let Some(&id) = self.atoms.get(&name) {
            id
        } else if only_if_exists {
            0
        } else {
            let id = self.next_atom;
            self.next_atom += 1;
            self.atoms.insert(name.clone(), id);
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
        let name = self.atoms.iter()
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

    fn get_property(&self, _data: &[u8]) -> Result<Vec<u8>> {
        let mut reply = vec![1u8];
        reply.push(0); // format
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes()); // length
        reply.extend_from_slice(&0u32.to_le_bytes()); // type
        reply.extend_from_slice(&0u32.to_le_bytes()); // bytes-after
        reply.extend_from_slice(&0u32.to_le_bytes()); // value-length
        reply.extend_from_slice(&[0u8; 12]);
        Ok(reply)
    }

    fn get_selection_owner(&self, _data: &[u8]) -> Result<Vec<u8>> {
        let mut reply = vec![1u8];
        reply.push(0);
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes()); // length
        reply.extend_from_slice(&0u32.to_le_bytes()); // owner = None
        reply.extend_from_slice(&[0u8; 20]);
        Ok(reply)
    }

    fn query_pointer(&self, _data: &[u8]) -> Result<Vec<u8>> {
        let mut reply = vec![1u8];
        reply.push(1); // same-screen
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes());
        reply.extend_from_slice(&self.root_id.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes()); // child
        reply.extend_from_slice(&0i16.to_le_bytes()); // root-x
        reply.extend_from_slice(&0i16.to_le_bytes()); // root-y
        reply.extend_from_slice(&0i16.to_le_bytes()); // win-x
        reply.extend_from_slice(&0i16.to_le_bytes()); // win-y
        reply.extend_from_slice(&0u16.to_le_bytes()); // mask
        reply.extend_from_slice(&[0u8; 6]);
        Ok(reply)
    }

    fn get_input_focus(&self) -> Result<Vec<u8>> {
        let mut reply = vec![1u8];
        reply.push(0); // revert-to
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes());
        reply.extend_from_slice(&self.root_id.to_le_bytes()); // focus
        reply.extend_from_slice(&[0u8; 20]);
        Ok(reply)
    }

    fn query_extension(&self, data: &[u8]) -> Result<Vec<u8>> {
        let name_len = u16::from_le_bytes([data[4], data[5]]) as usize;
        let name = String::from_utf8_lossy(&data[8..8.min(data.len()).max(8+name_len)]).to_string();
        eprintln!("QueryExtension: {}", name);

        let mut reply = vec![1u8];
        reply.push(0);
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes());

        if let Some(&(major, first_event, first_error)) = self.extensions.get(&name) {
            reply.push(1); // present = true
            reply.push(major); // major-opcode
            reply.push(first_event); // first-event
            reply.push(first_error); // first-error
        } else {
            reply.push(0); // present = false
            reply.push(0); // major-opcode
            reply.push(0); // first-event
            reply.push(0); // first-error
        }
        reply.extend_from_slice(&[0u8; 20]);
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

        // 8 modifiers, keycodes_per_mod each
        for _ in 0..8 {
            for _ in 0..keycodes_per_mod {
                reply.push(0);
            }
        }
        Ok(reply)
    }

    fn query_keymap(&self) -> Result<Vec<u8>> {
        // QueryKeymap reply is exactly 32 bytes:
        // 1 type + 1 unused + 2 seq + 4 length(=0) + 24 bytes keys (padded in header)
        // Actually: 32 bytes total = 1+1+2+4+24 = 32, where keys[32] fit in header
        // Wait no: reply is 1+1+2+4 (8 bytes) + remaining 24 fixed = 32, keys are PART of it
        // Actually X11: QueryKeymap reply has 32 bytes keys in the fixed part
        // Let me just make it 32 bytes total with length=0
        let mut reply = vec![1u8];
        reply.push(0); // unused
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes()); // length = 0 (no additional data)
        // The rest of the 32-byte reply: 24 bytes of key state (only first 24 bytes)
        // Actually for QueryKeymap, the spec says 32 bytes of keys. Let me look again...
        // Reply: 1 + 1 + 2 + 4 + 32-byte-keys = 40? No wait, the 32 is in addition.
        // X11 spec: REPLY header is always 32 bytes. QueryKeymap has keys[32] as "variable"
        // But since length encodes it, length=2 means 8 bytes extra...
        // Let's just return the standard 32-byte reply with no extra data
        reply.extend_from_slice(&[0u8; 24]); // fill to 32 bytes total
        Ok(reply)
    }

    fn list_fonts_with_info(&self) -> Result<Vec<u8>> {
        // Return "no more fonts" reply (name length 0 terminates)
        let mut reply = vec![1u8];
        reply.push(0); // name-length = 0 means no more fonts
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&7u32.to_le_bytes()); // length (min 7 for last reply)
        reply.extend_from_slice(&[0u8; 52]); // min/max bounds, etc
        Ok(reply)
    }

    fn query_font(&self) -> Result<Vec<u8>> {
        // Return minimal font info
        let mut reply = vec![1u8];
        reply.push(0);
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&7u32.to_le_bytes()); // length (7 words min)
        // Fill with zeros (default font properties)
        reply.extend_from_slice(&[0u8; 28]); // remaining fixed part
        Ok(reply)
    }

    fn get_image(&self, _data: &[u8]) -> Result<Vec<u8>> {
        // Return a minimal image (empty)
        let mut reply = vec![1u8];
        reply.push(24); // depth
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes()); // length (no image data)
        reply.extend_from_slice(&0x21u32.to_le_bytes()); // visual
        reply.extend_from_slice(&[0u8; 20]); // padding
        Ok(reply)
    }

    fn alloc_color(&self, _data: &[u8]) -> Result<Vec<u8>> {
        // Return the requested color
        let mut reply = vec![1u8];
        reply.push(0);
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes()); // length
        reply.extend_from_slice(&0xffffu16.to_le_bytes()); // red
        reply.extend_from_slice(&0xffffu16.to_le_bytes()); // green
        reply.extend_from_slice(&0xffffu16.to_le_bytes()); // blue
        reply.extend_from_slice(&[0u8; 2]); // pad
        reply.extend_from_slice(&0u32.to_le_bytes()); // pixel
        reply.extend_from_slice(&[0u8; 12]); // padding
        Ok(reply)
    }

    fn query_colors(&self, data: &[u8]) -> Result<Vec<u8>> {
        // Return colors for all requested pixels
        let num_pixels = if data.len() >= 8 {
            (data.len() - 8) / 4
        } else {
            0
        };

        let mut reply = vec![1u8];
        reply.push(0);
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&((num_pixels * 2) as u32).to_le_bytes()); // length (8 bytes per color / 4)
        reply.extend_from_slice(&(num_pixels as u16).to_le_bytes()); // nColors
        reply.extend_from_slice(&[0u8; 22]); // padding

        // Return white for each requested pixel
        for _ in 0..num_pixels {
            reply.extend_from_slice(&0xffffu16.to_le_bytes()); // red
            reply.extend_from_slice(&0xffffu16.to_le_bytes()); // green
            reply.extend_from_slice(&0xffffu16.to_le_bytes()); // blue
            reply.extend_from_slice(&[0u8; 2]); // pad
        }
        Ok(reply)
    }

    fn query_best_size(&self, _data: &[u8]) -> Result<Vec<u8>> {
        let mut reply = vec![1u8];
        reply.push(0);
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes()); // length
        reply.extend_from_slice(&16u16.to_le_bytes()); // width
        reply.extend_from_slice(&16u16.to_le_bytes()); // height
        reply.extend_from_slice(&[0u8; 20]); // padding
        Ok(reply)
    }

    fn handle_randr(&self, data: &[u8]) -> Result<Vec<u8>> {
        let minor = data[1];
        eprintln!("RANDR request: minor={}", minor);

        match minor {
            0 => {
                // RRQueryVersion - return version 1.6
                let mut reply = vec![1u8];
                reply.push(0);
                reply.extend_from_slice(&self.sequence.to_le_bytes());
                reply.extend_from_slice(&0u32.to_le_bytes()); // length
                reply.extend_from_slice(&1u32.to_le_bytes()); // major version
                reply.extend_from_slice(&6u32.to_le_bytes()); // minor version
                reply.extend_from_slice(&[0u8; 16]); // padding
                Ok(reply)
            }
            4 => {
                // RRSelectInput - no reply
                Ok(Vec::new())
            }
            5 => {
                // RRGetScreenResources - return minimal screen info
                self.randr_get_screen_resources()
            }
            6 => {
                // RRGetOutputInfo
                self.randr_get_output_info(data)
            }
            9 => {
                // RRGetCrtcInfo
                self.randr_get_crtc_info(data)
            }
            25 => {
                // RRGetOutputPrimary - return our single output as primary
                let mut reply = vec![1u8];
                reply.push(0);
                reply.extend_from_slice(&self.sequence.to_le_bytes());
                reply.extend_from_slice(&0u32.to_le_bytes()); // length
                reply.extend_from_slice(&1u32.to_le_bytes()); // output (OUTPUT id, 1 = our single output)
                reply.extend_from_slice(&[0u8; 20]); // padding
                Ok(reply)
            }
            31 => {
                // RRGetProviders - return empty list (no providers)
                let mut reply = vec![1u8];
                reply.push(0);
                reply.extend_from_slice(&self.sequence.to_le_bytes());
                reply.extend_from_slice(&0u32.to_le_bytes()); // length (0 = no extra data)
                reply.extend_from_slice(&0u32.to_le_bytes()); // timestamp
                reply.extend_from_slice(&0u16.to_le_bytes()); // num_providers = 0
                reply.extend_from_slice(&[0u8; 18]); // padding to 32 bytes
                Ok(reply)
            }
            _ => {
                eprintln!("unhandled RANDR minor: {}", minor);
                Ok(Vec::new())
            }
        }
    }

    fn randr_get_screen_resources(&self) -> Result<Vec<u8>> {
        // Return 1 output and 1 crtc
        let num_crtcs = 1u16;
        let num_outputs = 1u16;
        let num_modes = 1u16;
        let names_len = 8u16; // "default" + padding

        let length = (8 + num_crtcs as u32 * 4 + num_outputs as u32 * 4 + num_modes as u32 * 32 + names_len as u32) / 4;

        let mut reply = vec![1u8];
        reply.push(0);
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&length.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes()); // timestamp
        reply.extend_from_slice(&0u32.to_le_bytes()); // config-timestamp
        reply.extend_from_slice(&num_crtcs.to_le_bytes());
        reply.extend_from_slice(&num_outputs.to_le_bytes());
        reply.extend_from_slice(&num_modes.to_le_bytes());
        reply.extend_from_slice(&names_len.to_le_bytes());
        reply.extend_from_slice(&[0u8; 8]); // padding

        // CRTCs - just use ID 1
        reply.extend_from_slice(&1u32.to_le_bytes());

        // Outputs - just use ID 1
        reply.extend_from_slice(&1u32.to_le_bytes());

        // Modes (32 bytes each)
        // Mode ID
        reply.extend_from_slice(&1u32.to_le_bytes());
        // width, height
        reply.extend_from_slice(&(self.width as u16).to_le_bytes());
        reply.extend_from_slice(&(self.height as u16).to_le_bytes());
        // dotClock
        reply.extend_from_slice(&60000000u32.to_le_bytes()); // 60MHz
        // hSyncStart, hSyncEnd, hTotal
        reply.extend_from_slice(&(self.width as u16).to_le_bytes());
        reply.extend_from_slice(&(self.width as u16).to_le_bytes());
        reply.extend_from_slice(&(self.width as u16).to_le_bytes());
        // hSkew
        reply.extend_from_slice(&0u16.to_le_bytes());
        // vSyncStart, vSyncEnd, vTotal
        reply.extend_from_slice(&(self.height as u16).to_le_bytes());
        reply.extend_from_slice(&(self.height as u16).to_le_bytes());
        reply.extend_from_slice(&(self.height as u16).to_le_bytes());
        // name_len (7 = "default")
        reply.extend_from_slice(&7u16.to_le_bytes());
        // flags
        reply.extend_from_slice(&0u32.to_le_bytes());

        // Names
        reply.extend_from_slice(b"default\0");

        Ok(reply)
    }

    fn randr_get_output_info(&self, _data: &[u8]) -> Result<Vec<u8>> {
        let num_crtcs = 1u16;
        let num_modes = 1u16;
        let num_clones = 0u16;
        let name_len = 7u16; // "default"

        let length = (12 + num_crtcs as u32 * 4 + num_modes as u32 * 4 + num_clones as u32 * 4 + ((name_len as u32 + 3) & !3)) / 4;

        let mut reply = vec![1u8];
        reply.push(0); // status = RRSetConfigSuccess
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&length.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes()); // timestamp
        reply.extend_from_slice(&1u32.to_le_bytes()); // crtc = 1
        reply.extend_from_slice(&((self.width / 4) as u32).to_le_bytes()); // mm_width
        reply.extend_from_slice(&((self.height / 4) as u32).to_le_bytes()); // mm_height
        reply.push(1); // connection = Connected
        reply.push(0); // subpixel_order = Unknown
        reply.extend_from_slice(&num_crtcs.to_le_bytes());
        reply.extend_from_slice(&num_modes.to_le_bytes());
        reply.extend_from_slice(&1u16.to_le_bytes()); // num_preferred = 1
        reply.extend_from_slice(&num_clones.to_le_bytes());
        reply.extend_from_slice(&name_len.to_le_bytes());
        reply.extend_from_slice(&[0u8; 2]); // padding

        // CRTCs
        reply.extend_from_slice(&1u32.to_le_bytes());

        // Modes
        reply.extend_from_slice(&1u32.to_le_bytes());

        // Name
        reply.extend_from_slice(b"default\0");

        Ok(reply)
    }

    fn randr_get_crtc_info(&self, _data: &[u8]) -> Result<Vec<u8>> {
        let num_outputs = 1u16;
        let num_possible = 1u16;

        let length = (8 + num_outputs as u32 * 4 + num_possible as u32 * 4) / 4;

        let mut reply = vec![1u8];
        reply.push(0); // status = RRSetConfigSuccess
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&length.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes()); // timestamp
        reply.extend_from_slice(&0i16.to_le_bytes()); // x
        reply.extend_from_slice(&0i16.to_le_bytes()); // y
        reply.extend_from_slice(&(self.width as u16).to_le_bytes()); // width
        reply.extend_from_slice(&(self.height as u16).to_le_bytes()); // height
        reply.extend_from_slice(&1u32.to_le_bytes()); // mode = 1
        reply.extend_from_slice(&1u16.to_le_bytes()); // rotation = Rotate_0
        reply.extend_from_slice(&1u16.to_le_bytes()); // rotations (supported)
        reply.extend_from_slice(&num_outputs.to_le_bytes());
        reply.extend_from_slice(&num_possible.to_le_bytes());

        // Outputs
        reply.extend_from_slice(&1u32.to_le_bytes());

        // Possible outputs
        reply.extend_from_slice(&1u32.to_le_bytes());

        Ok(reply)
    }

    fn handle_xinput(&self, data: &[u8]) -> Result<Vec<u8>> {
        let minor = data[1];
        eprintln!("XInput request: minor={}", minor);

        match minor {
            1 => {
                // XIQueryVersion - return version 2.3
                let mut reply = vec![1u8];
                reply.push(0);
                reply.extend_from_slice(&self.sequence.to_le_bytes());
                reply.extend_from_slice(&0u32.to_le_bytes()); // length
                reply.extend_from_slice(&2u16.to_le_bytes()); // major version
                reply.extend_from_slice(&3u16.to_le_bytes()); // minor version
                reply.extend_from_slice(&[0u8; 20]); // padding
                Ok(reply)
            }
            46 => {
                // XIQueryDevice - return keyboard and pointer
                self.xinput_query_device()
            }
            52 => {
                // XISelectEvents - no reply
                Ok(Vec::new())
            }
            47 => {
                // XIQueryPointer - return pointer position
                self.xinput_query_pointer()
            }
            48 => {
                // XIGetFocus - return root window as focus
                let mut reply = vec![1u8];
                reply.push(0);
                reply.extend_from_slice(&self.sequence.to_le_bytes());
                reply.extend_from_slice(&0u32.to_le_bytes()); // length
                reply.extend_from_slice(&self.root_id.to_le_bytes()); // focus window
                reply.extend_from_slice(&[0u8; 20]); // padding
                Ok(reply)
            }
            61 => {
                // XIGetSelectedEvents
                let mut reply = vec![1u8];
                reply.push(0);
                reply.extend_from_slice(&self.sequence.to_le_bytes());
                reply.extend_from_slice(&0u32.to_le_bytes()); // length
                reply.extend_from_slice(&0u16.to_le_bytes()); // num_masks
                reply.extend_from_slice(&[0u8; 22]); // padding
                Ok(reply)
            }
            _ => {
                eprintln!("unhandled XInput minor: {}", minor);
                Ok(Vec::new())
            }
        }
    }

    fn xinput_query_device(&self) -> Result<Vec<u8>> {
        // Return 2 devices: master pointer and master keyboard
        let num_devices = 2u16;

        // Device info is variable length, let's build it
        let mut devices = Vec::new();

        // Master pointer (device ID 2)
        devices.extend_from_slice(&2u16.to_le_bytes()); // deviceid
        devices.extend_from_slice(&1u16.to_le_bytes()); // type = MasterPointer
        devices.extend_from_slice(&3u16.to_le_bytes()); // attachment (keyboard)
        devices.extend_from_slice(&0u16.to_le_bytes()); // num_classes
        devices.extend_from_slice(&14u16.to_le_bytes()); // name_len ("Virtual core pointer" = 20, but use shorter)
        devices.push(1); // enabled
        devices.push(0); // pad
        let name = b"Master pointer";
        devices.extend_from_slice(name);
        // Pad to 4-byte boundary
        let pad = (4 - (name.len() % 4)) % 4;
        for _ in 0..pad {
            devices.push(0);
        }

        // Master keyboard (device ID 3)
        devices.extend_from_slice(&3u16.to_le_bytes()); // deviceid
        devices.extend_from_slice(&2u16.to_le_bytes()); // type = MasterKeyboard
        devices.extend_from_slice(&2u16.to_le_bytes()); // attachment (pointer)
        devices.extend_from_slice(&0u16.to_le_bytes()); // num_classes
        devices.extend_from_slice(&15u16.to_le_bytes()); // name_len
        devices.push(1); // enabled
        devices.push(0); // pad
        let name = b"Master keyboard";
        devices.extend_from_slice(name);
        let pad = (4 - (name.len() % 4)) % 4;
        for _ in 0..pad {
            devices.push(0);
        }

        let length = devices.len() / 4;

        let mut reply = vec![1u8];
        reply.push(0);
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&(length as u32).to_le_bytes());
        reply.extend_from_slice(&num_devices.to_le_bytes());
        reply.extend_from_slice(&[0u8; 22]); // padding
        reply.extend_from_slice(&devices);

        Ok(reply)
    }

    fn xinput_query_pointer(&self) -> Result<Vec<u8>> {
        // XIQueryPointer reply - 56 bytes total
        // Structure per xinput2 protocol:
        // - reply header (8 bytes): type(1), unused(1), seq(2), length(4)
        // - root(4), child(4)
        // - root_x(4), root_y(4), win_x(4), win_y(4) - all FP1616
        // - same_screen(1), pad(1), buttons_len(2)
        // - mods: base(2), latched(2), locked(2), effective(2) = 8 bytes
        // - group: base(1), latched(1), locked(1), effective(1) = 4 bytes
        // Total fixed = 48 bytes, length = (48-32)/4 = 4

        let mut reply = vec![1u8]; // reply type
        reply.push(0); // unused
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&4u32.to_le_bytes()); // length = 4 (16 bytes after 32-byte header)
        reply.extend_from_slice(&self.root_id.to_le_bytes()); // root window
        reply.extend_from_slice(&0u32.to_le_bytes()); // child = None

        // root_x, root_y as Fixed (16.16) - 4 bytes each
        let pos = 100i32 << 16; // 100.0
        reply.extend_from_slice(&pos.to_le_bytes()); // root_x
        reply.extend_from_slice(&pos.to_le_bytes()); // root_y

        // win_x, win_y as Fixed (16.16)
        reply.extend_from_slice(&pos.to_le_bytes()); // win_x
        reply.extend_from_slice(&pos.to_le_bytes()); // win_y

        reply.push(1); // same_screen = true
        reply.push(0); // pad
        reply.extend_from_slice(&0u16.to_le_bytes()); // buttons_len = 0

        // Mods
        reply.extend_from_slice(&0u16.to_le_bytes()); // mods.base
        reply.extend_from_slice(&0u16.to_le_bytes()); // mods.latched
        reply.extend_from_slice(&0u16.to_le_bytes()); // mods.locked
        reply.extend_from_slice(&0u16.to_le_bytes()); // mods.effective

        // Group
        reply.push(0); // group.base
        reply.push(0); // group.latched
        reply.push(0); // group.locked
        reply.push(0); // group.effective

        // Total should be 56 bytes
        eprintln!("XIQueryPointer reply: {} bytes", reply.len());
        Ok(reply)
    }

    fn handle_xkb(&self, data: &[u8]) -> Result<Vec<u8>> {
        let minor = data[1];
        eprintln!("XKB request: minor={}", minor);

        match minor {
            0 => {
                // XkbUseExtension - return version 1.0
                let mut reply = vec![1u8];
                reply.push(1); // supported = true
                reply.extend_from_slice(&self.sequence.to_le_bytes());
                reply.extend_from_slice(&0u32.to_le_bytes()); // length
                reply.extend_from_slice(&1u16.to_le_bytes()); // server major
                reply.extend_from_slice(&0u16.to_le_bytes()); // server minor
                reply.extend_from_slice(&[0u8; 20]); // padding
                Ok(reply)
            }
            1 => {
                // XkbSelectEvents - no reply
                Ok(Vec::new())
            }
            6 => {
                // XkbGetControls - return minimal controls
                self.xkb_get_controls()
            }
            8 => {
                // XkbGetMap - return minimal map
                self.xkb_get_map()
            }
            9 => {
                // XkbSetMap - no reply
                Ok(Vec::new())
            }
            10 => {
                // XkbGetCompatMap - return minimal compat map
                self.xkb_get_compat_map()
            }
            13 => {
                // XkbGetIndicatorMap - return empty indicator map
                self.xkb_get_indicator_map()
            }
            17 | 24 => {
                // 17: XkbGetNames - return keyboard names
                // 24: XkbGetDeviceInfo - return minimal device info
                self.xkb_get_names()
            }
            _ => {
                eprintln!("unhandled XKB minor: {}", minor);
                Ok(Vec::new())
            }
        }
    }

    fn xkb_get_map(&self) -> Result<Vec<u8>> {
        // XkbGetMapReply - 40 bytes (8 header + 32 data)
        let mut reply = vec![1u8];
        reply.push(3); // deviceID = core keyboard
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&2u32.to_le_bytes()); // length = 2 (8 extra bytes for 40 total)
        reply.extend_from_slice(&[0u8; 2]); // 2 byte pad
        reply.push(8); // min keycode
        reply.push(255); // max keycode
        reply.extend_from_slice(&0u16.to_le_bytes()); // present (CARD16)
        reply.push(0); // first type
        reply.push(0); // n types
        reply.push(0); // total types
        reply.push(8); // first key sym
        reply.extend_from_slice(&0u16.to_le_bytes()); // total syms (CARD16)
        reply.push(0); // n key syms
        reply.push(0); // first key actions
        reply.extend_from_slice(&0u16.to_le_bytes()); // total actions (CARD16)
        reply.push(0); // n key actions
        reply.push(0); // first key behaviors
        reply.push(0); // n key behaviors
        reply.push(0); // total key behaviors
        reply.push(0); // first key explicit
        reply.push(0); // n key explicit
        reply.push(0); // total key explicit
        reply.push(0); // first mod map key
        reply.push(0); // n mod map keys
        reply.push(0); // total mod map keys
        reply.push(0); // first vmod map key
        reply.push(0); // n vmod map keys
        reply.push(0); // total vmod map keys
        reply.push(0); // pad
        reply.extend_from_slice(&0u16.to_le_bytes()); // virtual mods
        Ok(reply)
    }

    fn xkb_get_names(&self) -> Result<Vec<u8>> {
        // XkbGetNamesReply - return minimal keyboard names
        let mut reply = vec![1u8]; // reply type
        reply.push(3); // deviceID = core keyboard
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes()); // length (no extra data)
        reply.extend_from_slice(&0u32.to_le_bytes()); // which (no names present)
        reply.push(8); // min keycode
        reply.push(255); // max keycode
        reply.push(0); // n types
        reply.push(0); // group names
        reply.extend_from_slice(&0u16.to_le_bytes()); // virtual mods
        reply.push(8); // first key
        reply.push(0); // n keys
        reply.extend_from_slice(&0u16.to_le_bytes()); // indicators
        reply.push(0); // n radio groups
        reply.push(0); // n key aliases
        reply.extend_from_slice(&0u16.to_le_bytes()); // n kt levels
        reply.extend_from_slice(&[0u8; 6]); // padding to 32 bytes
        Ok(reply)
    }

    fn xkb_get_controls(&self) -> Result<Vec<u8>> {
        // XkbGetControlsReply - 92 bytes total per XKB spec
        let mut reply = vec![1u8]; // reply type
        reply.push(3); // deviceID = core keyboard
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&15u32.to_le_bytes()); // length = 15 (60 bytes extra / 4)
        reply.push(0); // mouseKeysDfltBtn
        reply.push(1); // numGroups
        reply.push(0); // groupsWrap
        reply.push(0); // internalMods
        reply.push(0); // ignoreLockMods
        reply.push(0); // internalRealMods
        reply.push(0); // ignoreLockRealMods
        reply.push(0); // unused
        reply.extend_from_slice(&0u16.to_le_bytes()); // internalVirtualMods
        reply.extend_from_slice(&0u16.to_le_bytes()); // ignoreLockVirtualMods
        reply.extend_from_slice(&500u16.to_le_bytes()); // repeatDelay
        reply.extend_from_slice(&30u16.to_le_bytes()); // repeatInterval
        reply.extend_from_slice(&300u16.to_le_bytes()); // slowKeysDelay
        reply.extend_from_slice(&300u16.to_le_bytes()); // debounceDelay
        // End of 32-byte header, now additional data
        reply.extend_from_slice(&300u16.to_le_bytes()); // mouseKeysDelay
        reply.extend_from_slice(&50u16.to_le_bytes()); // mouseKeysInterval
        reply.extend_from_slice(&10u16.to_le_bytes()); // mouseKeysTimeToMax
        reply.extend_from_slice(&10u16.to_le_bytes()); // mouseKeysMaxSpeed
        reply.extend_from_slice(&0i16.to_le_bytes()); // mouseKeysCurve
        reply.extend_from_slice(&0u16.to_le_bytes()); // accessXOption
        reply.extend_from_slice(&300u16.to_le_bytes()); // accessXTimeout
        reply.extend_from_slice(&0u16.to_le_bytes()); // accessXTimeoutOptionsMask
        reply.extend_from_slice(&0u16.to_le_bytes()); // accessXTimeoutOptionsValues
        reply.extend_from_slice(&[0u8; 2]); // pad
        reply.extend_from_slice(&0u32.to_le_bytes()); // accessXTimeoutMask
        reply.extend_from_slice(&0u32.to_le_bytes()); // accessXTimeoutValues
        reply.extend_from_slice(&0u32.to_le_bytes()); // enabledControls
        // perKeyRepeat (32 bytes)
        reply.extend_from_slice(&[0xffu8; 32]);
        Ok(reply)
    }

    fn xkb_get_compat_map(&self) -> Result<Vec<u8>> {
        // XkbGetCompatMapReply - return minimal compat map
        let mut reply = vec![1u8]; // reply type
        reply.push(3); // deviceID = core keyboard
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes()); // length
        reply.push(0); // groups in
        reply.push(0); // pad
        reply.extend_from_slice(&0u16.to_le_bytes()); // first SI
        reply.extend_from_slice(&0u16.to_le_bytes()); // n SI
        reply.extend_from_slice(&0u16.to_le_bytes()); // n total SI
        reply.extend_from_slice(&[0u8; 16]); // padding to 32 bytes
        Ok(reply)
    }

    fn xkb_get_indicator_map(&self) -> Result<Vec<u8>> {
        // XkbGetIndicatorMapReply - return empty indicator map
        let mut reply = vec![1u8]; // reply type
        reply.push(3); // deviceID = core keyboard
        reply.extend_from_slice(&self.sequence.to_le_bytes());
        reply.extend_from_slice(&0u32.to_le_bytes()); // length (no indicator data)
        reply.extend_from_slice(&0u32.to_le_bytes()); // which (no indicators)
        reply.extend_from_slice(&0u32.to_le_bytes()); // real indicators
        reply.extend_from_slice(&0u32.to_le_bytes()); // n indicators = 0
        reply.extend_from_slice(&[0u8; 12]); // padding to 32 bytes
        Ok(reply)
    }

    fn handle_ge(&self, data: &[u8]) -> Result<Vec<u8>> {
        // Generic Event Extension
        let minor = data[1];
        eprintln!("GE request: minor={}", minor);

        match minor {
            0 => {
                // GEQueryVersion - return version 1.0
                let mut reply = vec![1u8];
                reply.push(0);
                reply.extend_from_slice(&self.sequence.to_le_bytes());
                reply.extend_from_slice(&0u32.to_le_bytes()); // length
                reply.extend_from_slice(&1u16.to_le_bytes()); // major version
                reply.extend_from_slice(&0u16.to_le_bytes()); // minor version
                reply.extend_from_slice(&[0u8; 20]); // padding
                Ok(reply)
            }
            _ => {
                eprintln!("unhandled GE minor: {}", minor);
                Ok(Vec::new())
            }
        }
    }
}

async fn handle_client(mut stream: tokio::net::UnixStream, client_id: u32) {
    let mut server = TestServer::new();
    let mut buf = vec![0u8; 65536];
    let mut pending = Vec::new();
    let mut connected = false;

    loop {
        let n = match stream.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                eprintln!("[{}] read error: {}", client_id, e);
                break;
            }
        };
        pending.extend_from_slice(&buf[..n]);
        eprintln!("[{}] received {} bytes, pending: {}", client_id, n, pending.len());

        loop {
            let needed = if !connected {
                12 // connection setup
            } else if pending.len() >= 4 {
                let len = u16::from_le_bytes([pending[2], pending[3]]) as usize * 4;
                if len == 0 { 4 } else { len }
            } else {
                4
            };

            eprintln!("[{}]   needed: {} bytes for opcode {}", client_id, needed, if pending.len() > 0 { pending[0] } else { 0 });
            if pending.len() < needed {
                break;
            }

            let reply = match server.process(&pending) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[{}] process error: {}", client_id, e);
                    break;
                }
            };
            if !connected && !reply.is_empty() {
                connected = true;
            }

            pending.drain(..needed);

            if !reply.is_empty() {
                eprintln!("[{}] sending {} bytes", client_id, reply.len());
                if let Err(e) = stream.write_all(&reply).await {
                    eprintln!("[{}] write error: {}", client_id, e);
                    return;
                }
            }
        }
    }
    eprintln!("[{}] client disconnected", client_id);
}

pub async fn run_test_server(display_num: u32) -> Result<()> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static CLIENT_COUNTER: AtomicU32 = AtomicU32::new(0);

    let socket_path = format!("{}/X{}", X11_UNIX_DIR, display_num);
    let _ = std::fs::remove_file(&socket_path);
    std::fs::create_dir_all(X11_UNIX_DIR)?;

    let listener = UnixListener::bind(&socket_path)?;
    eprintln!("test x11 server on DISPLAY=:{}", display_num);

    loop {
        let (stream, _) = listener.accept().await?;
        let client_id = CLIENT_COUNTER.fetch_add(1, Ordering::SeqCst);
        eprintln!("[{}] client connected", client_id);
        tokio::spawn(handle_client(stream, client_id));
    }
}
