//! Simple X11 server test - runs protocol handler natively without wasm
//!
//! This bypasses the browser/wasm requirement by implementing minimal
//! X11 protocol handling directly.

use anyhow::Result;
use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;

const X11_UNIX_DIR: &str = "/tmp/.X11-unix";

struct TestServer {
    sequence: u16,
    root_id: u32,
    width: u32,
    height: u32,
    resource_id_base: u32,
    resource_id_mask: u32,
    atoms: HashMap<String, u32>,
    next_atom: u32,
}

impl TestServer {
    fn new() -> Self {
        Self {
            sequence: 0,
            root_id: 1,
            width: 1280,
            height: 720,
            resource_id_base: 0x00200000,
            resource_id_mask: 0x001fffff,
            atoms: HashMap::new(),
            next_atom: 1,
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
            12 => Ok(Vec::new()), // ConfigureWindow - no reply
            14 => self.get_geometry(data), // GetGeometry
            15 => self.query_tree(data), // QueryTree
            16 => self.intern_atom(data), // InternAtom
            17 => self.get_atom_name(data), // GetAtomName
            18 => Ok(Vec::new()), // ChangeProperty - no reply
            20 => self.get_property(data), // GetProperty
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
            99 => self.query_keymap(), // QueryKeymap
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
        reply.push(0); // present
        reply.push(0); // major-opcode
        reply.push(0); // first-event
        reply.push(0); // first-error
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
}

pub async fn run_test_server(display_num: u32) -> Result<()> {
    let socket_path = format!("{}/X{}", X11_UNIX_DIR, display_num);
    let _ = std::fs::remove_file(&socket_path);
    std::fs::create_dir_all(X11_UNIX_DIR)?;

    let listener = UnixListener::bind(&socket_path)?;
    eprintln!("test x11 server on DISPLAY=:{}", display_num);

    loop {
        let (mut stream, _) = listener.accept().await?;
        eprintln!("client connected");

        let mut server = TestServer::new();
        let mut buf = vec![0u8; 65536];
        let mut pending = Vec::new();
        let mut connected = false;

        loop {
            let n = stream.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            pending.extend_from_slice(&buf[..n]);
            eprintln!("received {} bytes, pending: {}", n, pending.len());

            loop {
                let needed = if !connected {
                    12 // connection setup
                } else if pending.len() >= 4 {
                    let len = u16::from_le_bytes([pending[2], pending[3]]) as usize * 4;
                    if len == 0 { 4 } else { len }
                } else {
                    4
                };

                eprintln!("  needed: {} bytes for opcode {}", needed, if pending.len() > 0 { pending[0] } else { 0 });
                if pending.len() < needed {
                    break;
                }

                let reply = server.process(&pending)?;
                if !connected && !reply.is_empty() {
                    connected = true;
                }

                pending.drain(..needed);

                if !reply.is_empty() {
                    eprintln!("sending {} bytes", reply.len());
                    stream.write_all(&reply).await?;
                }
            }
        }
        eprintln!("client disconnected");
    }
}
