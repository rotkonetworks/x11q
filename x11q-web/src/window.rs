//! X11 Window management

use std::collections::HashMap;

/// X11 Window
#[derive(Debug, Clone)]
pub struct Window {
    pub id: u32,
    pub parent: u32,
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
    pub border_width: u16,
    pub depth: u8,
    pub class: WindowClass,
    pub visual: u32,
    pub mapped: bool,
    pub attributes: WindowAttributes,
    pub children: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WindowClass {
    CopyFromParent,
    InputOutput,
    InputOnly,
}

#[derive(Debug, Clone, Default)]
pub struct WindowAttributes {
    pub background_pixel: Option<u32>,
    pub border_pixel: Option<u32>,
    pub bit_gravity: u8,
    pub win_gravity: u8,
    pub backing_store: u8,
    pub backing_planes: u32,
    pub backing_pixel: u32,
    pub override_redirect: bool,
    pub save_under: bool,
    pub event_mask: u32,
    pub do_not_propagate_mask: u32,
    pub colormap: u32,
    pub cursor: u32,
}

impl Window {
    pub fn new_root(id: u32, width: u16, height: u16, depth: u8, visual: u32) -> Self {
        Self {
            id,
            parent: 0,
            x: 0,
            y: 0,
            width,
            height,
            border_width: 0,
            depth,
            class: WindowClass::InputOutput,
            visual,
            mapped: true,
            attributes: WindowAttributes::default(),
            children: Vec::new(),
        }
    }

    pub fn new(
        id: u32,
        parent: u32,
        x: i16,
        y: i16,
        width: u16,
        height: u16,
        border_width: u16,
        depth: u8,
        class: WindowClass,
        visual: u32,
    ) -> Self {
        Self {
            id,
            parent,
            x,
            y,
            width,
            height,
            border_width,
            depth,
            class,
            visual,
            mapped: false,
            attributes: WindowAttributes::default(),
            children: Vec::new(),
        }
    }
}

/// Window tree management
pub struct WindowTree {
    windows: HashMap<u32, Window>,
    root: u32,
}

impl WindowTree {
    pub fn new(root_id: u32, width: u16, height: u16) -> Self {
        let mut windows = HashMap::new();
        let root = Window::new_root(root_id, width, height, 24, 0x21); // TrueColor visual
        windows.insert(root_id, root);

        Self {
            windows,
            root: root_id,
        }
    }

    pub fn root_id(&self) -> u32 {
        self.root
    }

    pub fn get(&self, id: u32) -> Option<&Window> {
        self.windows.get(&id)
    }

    pub fn get_mut(&mut self, id: u32) -> Option<&mut Window> {
        self.windows.get_mut(&id)
    }

    pub fn create_window(&mut self, window: Window) {
        let parent_id = window.parent;
        let id = window.id;
        self.windows.insert(id, window);

        if let Some(parent) = self.windows.get_mut(&parent_id) {
            parent.children.push(id);
        }
    }

    pub fn destroy_window(&mut self, id: u32) -> Option<Window> {
        if let Some(window) = self.windows.remove(&id) {
            // Remove from parent's children
            if let Some(parent) = self.windows.get_mut(&window.parent) {
                parent.children.retain(|&child| child != id);
            }
            // Recursively destroy children
            for child_id in &window.children {
                self.destroy_window(*child_id);
            }
            Some(window)
        } else {
            None
        }
    }

    pub fn map_window(&mut self, id: u32) -> bool {
        if let Some(window) = self.windows.get_mut(&id) {
            window.mapped = true;
            true
        } else {
            false
        }
    }

    pub fn unmap_window(&mut self, id: u32) -> bool {
        if let Some(window) = self.windows.get_mut(&id) {
            window.mapped = false;
            true
        } else {
            false
        }
    }

    /// Get all mapped windows in stacking order (bottom to top)
    pub fn mapped_windows(&self) -> Vec<(u32, i16, i16, u16, u16)> {
        let mut result = Vec::new();
        self.collect_mapped(self.root, 0, 0, &mut result);
        result
    }

    fn collect_mapped(&self, id: u32, parent_x: i16, parent_y: i16, result: &mut Vec<(u32, i16, i16, u16, u16)>) {
        if let Some(window) = self.windows.get(&id) {
            if window.mapped {
                let abs_x = parent_x + window.x;
                let abs_y = parent_y + window.y;

                // Skip root window in output
                if id != self.root {
                    result.push((id, abs_x, abs_y, window.width, window.height));
                }

                for &child in &window.children {
                    self.collect_mapped(child, abs_x, abs_y, result);
                }
            }
        }
    }
}
