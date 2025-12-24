//! X11 Input handling - keyboard and mouse

use std::collections::HashMap;

/// Keyboard state
pub struct Keyboard {
    /// Keycode to keysym mapping
    keymap: HashMap<u8, u32>,
    /// Currently pressed keys (keycode -> pressed)
    pressed: [bool; 256],
    /// Modifier state
    pub modifiers: u16,
}

impl Keyboard {
    pub fn new() -> Self {
        Self {
            keymap: Self::default_keymap(),
            pressed: [false; 256],
            modifiers: 0,
        }
    }

    fn default_keymap() -> HashMap<u8, u32> {
        let mut map = HashMap::new();
        // Basic ASCII mapping (simplified)
        // In real X11 this comes from XKB
        for i in 0..=127u8 {
            map.insert(i + 8, i as u32); // X11 keycodes start at 8
        }
        map
    }

    /// Convert JS key code to X11 keycode
    pub fn js_to_keycode(&self, code: &str) -> u8 {
        // Map JS KeyboardEvent.code to X11 keycode
        match code {
            "Escape" => 9,
            "Digit1" => 10, "Digit2" => 11, "Digit3" => 12, "Digit4" => 13,
            "Digit5" => 14, "Digit6" => 15, "Digit7" => 16, "Digit8" => 17,
            "Digit9" => 18, "Digit0" => 19,
            "Minus" => 20, "Equal" => 21, "Backspace" => 22, "Tab" => 23,
            "KeyQ" => 24, "KeyW" => 25, "KeyE" => 26, "KeyR" => 27, "KeyT" => 28,
            "KeyY" => 29, "KeyU" => 30, "KeyI" => 31, "KeyO" => 32, "KeyP" => 33,
            "BracketLeft" => 34, "BracketRight" => 35, "Enter" => 36,
            "ControlLeft" => 37, "KeyA" => 38, "KeyS" => 39, "KeyD" => 40,
            "KeyF" => 41, "KeyG" => 42, "KeyH" => 43, "KeyJ" => 44, "KeyK" => 45,
            "KeyL" => 46, "Semicolon" => 47, "Quote" => 48, "Backquote" => 49,
            "ShiftLeft" => 50, "Backslash" => 51, "KeyZ" => 52, "KeyX" => 53,
            "KeyC" => 54, "KeyV" => 55, "KeyB" => 56, "KeyN" => 57, "KeyM" => 58,
            "Comma" => 59, "Period" => 60, "Slash" => 61, "ShiftRight" => 62,
            "NumpadMultiply" => 63, "AltLeft" => 64, "Space" => 65,
            "CapsLock" => 66,
            "F1" => 67, "F2" => 68, "F3" => 69, "F4" => 70, "F5" => 71,
            "F6" => 72, "F7" => 73, "F8" => 74, "F9" => 75, "F10" => 76,
            "NumLock" => 77, "ScrollLock" => 78,
            "Numpad7" => 79, "Numpad8" => 80, "Numpad9" => 81, "NumpadSubtract" => 82,
            "Numpad4" => 83, "Numpad5" => 84, "Numpad6" => 85, "NumpadAdd" => 86,
            "Numpad1" => 87, "Numpad2" => 88, "Numpad3" => 89, "Numpad0" => 90,
            "NumpadDecimal" => 91,
            "F11" => 95, "F12" => 96,
            "NumpadEnter" => 104, "ControlRight" => 105,
            "NumpadDivide" => 106, "PrintScreen" => 107, "AltRight" => 108,
            "Home" => 110, "ArrowUp" => 111, "PageUp" => 112,
            "ArrowLeft" => 113, "ArrowRight" => 114,
            "End" => 115, "ArrowDown" => 116, "PageDown" => 117,
            "Insert" => 118, "Delete" => 119,
            "MetaLeft" => 133, "MetaRight" => 134, "ContextMenu" => 135,
            _ => 0,
        }
    }

    pub fn key_press(&mut self, keycode: u8) {
        if keycode < 255 {
            self.pressed[keycode as usize] = true;
        }
        self.update_modifiers(keycode, true);
    }

    pub fn key_release(&mut self, keycode: u8) {
        if keycode < 255 {
            self.pressed[keycode as usize] = false;
        }
        self.update_modifiers(keycode, false);
    }

    fn update_modifiers(&mut self, keycode: u8, pressed: bool) {
        let bit = match keycode {
            50 | 62 => 0x01,   // Shift
            37 | 105 => 0x04, // Control
            66 => 0x02,       // Lock (CapsLock)
            64 | 108 => 0x08, // Mod1 (Alt)
            77 => 0x10,       // Mod2 (NumLock)
            133 | 134 => 0x40, // Mod4 (Super)
            _ => 0,
        };

        if pressed {
            self.modifiers |= bit;
        } else {
            self.modifiers &= !bit;
        }
    }

    pub fn get_keymap(&self) -> &[bool; 256] {
        &self.pressed
    }
}

/// Mouse/pointer state
pub struct Pointer {
    pub x: i16,
    pub y: i16,
    pub buttons: u8,
}

impl Pointer {
    pub fn new() -> Self {
        Self {
            x: 0,
            y: 0,
            buttons: 0,
        }
    }

    pub fn move_to(&mut self, x: i16, y: i16) {
        self.x = x;
        self.y = y;
    }

    pub fn button_press(&mut self, button: u8) {
        self.buttons |= 1 << button;
    }

    pub fn button_release(&mut self, button: u8) {
        self.buttons &= !(1 << button);
    }

    /// Get button mask for event (includes button state)
    pub fn button_mask(&self) -> u16 {
        (self.buttons as u16) << 8
    }
}
