//! X11 Drawables (Windows and Pixmaps)

use std::collections::HashMap;

/// A drawable is either a window or a pixmap
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DrawableType {
    Window,
    Pixmap,
}

/// Pixmap data
#[derive(Debug)]
pub struct Pixmap {
    pub id: u32,
    pub drawable: u32, // the window/pixmap it was created relative to
    pub width: u16,
    pub height: u16,
    pub depth: u8,
}

/// Manages pixmaps
pub struct PixmapStore {
    pixmaps: HashMap<u32, Pixmap>,
}

impl PixmapStore {
    pub fn new() -> Self {
        Self {
            pixmaps: HashMap::new(),
        }
    }

    pub fn create(&mut self, id: u32, drawable: u32, width: u16, height: u16, depth: u8) {
        self.pixmaps.insert(id, Pixmap {
            id,
            drawable,
            width,
            height,
            depth,
        });
    }

    pub fn get(&self, id: u32) -> Option<&Pixmap> {
        self.pixmaps.get(&id)
    }

    pub fn free(&mut self, id: u32) -> Option<Pixmap> {
        self.pixmaps.remove(&id)
    }
}
