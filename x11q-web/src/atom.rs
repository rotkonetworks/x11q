//! X11 Atoms - interned strings

use std::collections::HashMap;

/// Atom store - maps between atom IDs and strings
pub struct AtomStore {
    by_name: HashMap<String, u32>,
    by_id: HashMap<u32, String>,
    next_id: u32,
}

impl AtomStore {
    pub fn new() -> Self {
        let mut store = Self {
            by_name: HashMap::new(),
            by_id: HashMap::new(),
            next_id: 1,
        };

        // Predefined atoms (from X11/Xatom.h)
        let predefined = [
            (1, "PRIMARY"),
            (2, "SECONDARY"),
            (3, "ARC"),
            (4, "ATOM"),
            (5, "BITMAP"),
            (6, "CARDINAL"),
            (7, "COLORMAP"),
            (8, "CURSOR"),
            (9, "CUT_BUFFER0"),
            (10, "CUT_BUFFER1"),
            (11, "CUT_BUFFER2"),
            (12, "CUT_BUFFER3"),
            (13, "CUT_BUFFER4"),
            (14, "CUT_BUFFER5"),
            (15, "CUT_BUFFER6"),
            (16, "CUT_BUFFER7"),
            (17, "DRAWABLE"),
            (18, "FONT"),
            (19, "INTEGER"),
            (20, "PIXMAP"),
            (21, "POINT"),
            (22, "RECTANGLE"),
            (23, "RESOURCE_MANAGER"),
            (24, "RGB_COLOR_MAP"),
            (25, "RGB_BEST_MAP"),
            (26, "RGB_BLUE_MAP"),
            (27, "RGB_DEFAULT_MAP"),
            (28, "RGB_GRAY_MAP"),
            (29, "RGB_GREEN_MAP"),
            (30, "RGB_RED_MAP"),
            (31, "STRING"),
            (32, "VISUALID"),
            (33, "WINDOW"),
            (34, "WM_COMMAND"),
            (35, "WM_HINTS"),
            (36, "WM_CLIENT_MACHINE"),
            (37, "WM_ICON_NAME"),
            (38, "WM_ICON_SIZE"),
            (39, "WM_NAME"),
            (40, "WM_NORMAL_HINTS"),
            (41, "WM_SIZE_HINTS"),
            (42, "WM_ZOOM_HINTS"),
            (43, "MIN_SPACE"),
            (44, "NORM_SPACE"),
            (45, "MAX_SPACE"),
            (46, "END_SPACE"),
            (47, "SUPERSCRIPT_X"),
            (48, "SUPERSCRIPT_Y"),
            (49, "SUBSCRIPT_X"),
            (50, "SUBSCRIPT_Y"),
            (51, "UNDERLINE_POSITION"),
            (52, "UNDERLINE_THICKNESS"),
            (53, "STRIKEOUT_ASCENT"),
            (54, "STRIKEOUT_DESCENT"),
            (55, "ITALIC_ANGLE"),
            (56, "X_HEIGHT"),
            (57, "QUAD_WIDTH"),
            (58, "WEIGHT"),
            (59, "POINT_SIZE"),
            (60, "RESOLUTION"),
            (61, "COPYRIGHT"),
            (62, "NOTICE"),
            (63, "FONT_NAME"),
            (64, "FAMILY_NAME"),
            (65, "FULL_NAME"),
            (66, "CAP_HEIGHT"),
            (67, "WM_CLASS"),
            (68, "WM_TRANSIENT_FOR"),
        ];

        for (id, name) in predefined {
            store.by_name.insert(name.to_string(), id);
            store.by_id.insert(id, name.to_string());
        }
        store.next_id = 69;

        store
    }

    /// Intern an atom (create if doesn't exist)
    pub fn intern(&mut self, name: &str, only_if_exists: bool) -> Option<u32> {
        if let Some(&id) = self.by_name.get(name) {
            Some(id)
        } else if only_if_exists {
            None
        } else {
            let id = self.next_id;
            self.next_id += 1;
            self.by_name.insert(name.to_string(), id);
            self.by_id.insert(id, name.to_string());
            Some(id)
        }
    }

    /// Get atom name by ID
    pub fn get_name(&self, id: u32) -> Option<&str> {
        self.by_id.get(&id).map(|s| s.as_str())
    }

    /// Get atom ID by name
    pub fn get_id(&self, name: &str) -> Option<u32> {
        self.by_name.get(name).copied()
    }
}
