//! X11 Graphics Context

use std::collections::HashMap;

/// Graphics Context - holds drawing state
#[derive(Debug, Clone)]
pub struct GC {
    pub id: u32,
    pub drawable: u32,
    pub function: GCFunction,
    pub plane_mask: u32,
    pub foreground: u32,
    pub background: u32,
    pub line_width: u16,
    pub line_style: u8,
    pub cap_style: u8,
    pub join_style: u8,
    pub fill_style: u8,
    pub fill_rule: u8,
    pub arc_mode: u8,
    pub tile: u32,
    pub stipple: u32,
    pub tile_stipple_x: i16,
    pub tile_stipple_y: i16,
    pub font: u32,
    pub subwindow_mode: u8,
    pub graphics_exposures: bool,
    pub clip_x: i16,
    pub clip_y: i16,
    pub clip_mask: u32,
    pub dash_offset: u16,
    pub dashes: u8,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GCFunction {
    Clear = 0,
    And = 1,
    AndReverse = 2,
    Copy = 3,
    AndInverted = 4,
    Noop = 5,
    Xor = 6,
    Or = 7,
    Nor = 8,
    Equiv = 9,
    Invert = 10,
    OrReverse = 11,
    CopyInverted = 12,
    OrInverted = 13,
    Nand = 14,
    Set = 15,
}

impl Default for GC {
    fn default() -> Self {
        Self {
            id: 0,
            drawable: 0,
            function: GCFunction::Copy,
            plane_mask: !0,
            foreground: 0,
            background: 1,
            line_width: 0,
            line_style: 0, // Solid
            cap_style: 1,  // Butt
            join_style: 0, // Miter
            fill_style: 0, // Solid
            fill_rule: 0,  // EvenOdd
            arc_mode: 1,   // PieSlice
            tile: 0,
            stipple: 0,
            tile_stipple_x: 0,
            tile_stipple_y: 0,
            font: 0,
            subwindow_mode: 0, // ClipByChildren
            graphics_exposures: true,
            clip_x: 0,
            clip_y: 0,
            clip_mask: 0,
            dash_offset: 0,
            dashes: 4,
        }
    }
}

impl GC {
    pub fn new(id: u32, drawable: u32) -> Self {
        Self {
            id,
            drawable,
            ..Default::default()
        }
    }
}

/// GC storage
pub struct GCStore {
    gcs: HashMap<u32, GC>,
}

impl GCStore {
    pub fn new() -> Self {
        Self {
            gcs: HashMap::new(),
        }
    }

    pub fn create(&mut self, gc: GC) {
        self.gcs.insert(gc.id, gc);
    }

    pub fn get(&self, id: u32) -> Option<&GC> {
        self.gcs.get(&id)
    }

    pub fn get_mut(&mut self, id: u32) -> Option<&mut GC> {
        self.gcs.get_mut(&id)
    }

    pub fn free(&mut self, id: u32) -> Option<GC> {
        self.gcs.remove(&id)
    }
}
