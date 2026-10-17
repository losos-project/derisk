//! The glyph atlas both renderers draw from.
//!
//! Glyphs are rasterised once, the first time a frame needs them, into one
//! alpha texture kept in memory. The software renderer blends straight from
//! it; the GPU renderer uploads the rows that changed since its last frame.
//! Glyphs are packed in shelves, rows as tall as their tallest glyph, which
//! suits a terminal well because nearly every glyph is about a cell tall.
//! When the atlas fills it grows downwards, which keeps every coordinate
//! already handed out valid.

use std::collections::HashMap;

use crate::{
    boxdraw,
    font::{Fonts, Raster, Style},
};

/// Where a glyph is in the atlas and how it sits on its cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// From the left edge of the character's first cell.
    pub left: i32,
    /// From the baseline up to the glyph's top row.
    pub top: i32,
}

/// An alpha-only texture of glyphs.
pub struct Atlas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    /// The shelf being filled: its top, its height so far, and the next
    /// free column on it.
    shelf_y: u32,
    shelf_height: u32,
    shelf_x: u32,
    /// Rows written since [`Atlas::take_dirty`] was last called.
    dirty: Option<(u32, u32)>,
}

/// Width of the atlas, and its first height. 1024 holds every glyph of a
/// screen of mixed text at the sizes a terminal uses.
const SIDE: u32 = 1024;
/// GLES 3 promises textures this tall; past it the atlas starts over.
const MAX_HEIGHT: u32 = 4096;

impl Default for Atlas {
    fn default() -> Self {
        Self::new()
    }
}

impl Atlas {
    pub fn new() -> Self {
        Self {
            width: SIDE,
            height: SIDE,
            pixels: vec![0; (SIDE * SIDE) as usize],
            shelf_y: 0,
            shelf_height: 0,
            shelf_x: 0,
            dirty: None,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Coverage, one byte per pixel, row by row.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// The rows changed since the last call, as `start..end`, and forgets
    /// them.
    pub fn take_dirty(&mut self) -> Option<(u32, u32)> {
        self.dirty.take()
    }

    /// Copies `raster` in, or `None` when it cannot fit even after growing.
    fn insert(&mut self, raster: &Raster) -> Option<(u32, u32)> {
        // A pixel of empty border keeps linear filtering, and a glyph drawn
        // a pixel off, from picking up its neighbour.
        let (w, h) = (raster.width + 1, raster.height + 1);
        if w > self.width {
            return None;
        }
        if self.shelf_x + w > self.width {
            self.shelf_y += self.shelf_height;
            self.shelf_x = 0;
            self.shelf_height = 0;
        }
        while self.shelf_y + h > self.height {
            if self.height * 2 > MAX_HEIGHT {
                return None;
            }
            self.height *= 2;
            self.pixels.resize((self.width * self.height) as usize, 0);
            self.mark(0, self.height);
        }
        let (x, y) = (self.shelf_x, self.shelf_y);
        for row in 0..raster.height {
            let from = (row * raster.width) as usize;
            let to = ((y + row) * self.width + x) as usize;
            self.pixels[to..to + raster.width as usize]
                .copy_from_slice(&raster.alpha[from..from + raster.width as usize]);
        }
        self.shelf_x += w;
        self.shelf_height = self.shelf_height.max(h);
        self.mark(y, y + raster.height);
        Some((x, y))
    }

    fn mark(&mut self, start: u32, end: u32) {
        self.dirty = Some(match self.dirty {
            Some((s, e)) => (s.min(start), e.max(end)),
            None => (start, end),
        });
    }

    /// Empties the atlas, keeping its size.
    fn clear(&mut self) {
        self.pixels.fill(0);
        self.shelf_y = 0;
        self.shelf_x = 0;
        self.shelf_height = 0;
        self.mark(0, self.height);
    }
}

/// What a glyph is cached by: the character, its style, and how many cells
/// it may cover.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct Key {
    c: char,
    style: Style,
    span: u8,
}

/// The fonts and the atlas, and which glyph is where.
pub struct Glyphs {
    pub fonts: Fonts,
    pub atlas: Atlas,
    /// `None` for a character no font draws, or one that draws nothing
    /// (a space), so neither is looked up again.
    slots: HashMap<Key, Option<Slot>>,
    /// Bumped whenever the atlas starts over, so a frame built across it
    /// knows its earlier glyphs moved.
    generation: u64,
}

impl Glyphs {
    pub fn new(fonts: Fonts) -> Self {
        Self {
            fonts,
            atlas: Atlas::new(),
            slots: HashMap::new(),
            generation: 0,
        }
    }

    /// Changes whenever slots handed out before stop being valid.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The fonts at another size; every glyph is rasterised again.
    pub fn resize(&mut self, size: f32) {
        self.fonts.resize(size);
        self.slots.clear();
        self.atlas.clear();
        self.generation += 1;
    }

    /// The slot for `c`, rasterising it on first use. `span` is how many
    /// cells it may cover.
    pub fn get(&mut self, c: char, style: Style, span: u8) -> Option<Slot> {
        let key = Key { c, style, span };
        if let Some(slot) = self.slots.get(&key) {
            return *slot;
        }
        let slot = self.rasterize(key);
        self.slots.insert(key, slot);
        slot
    }

    fn rasterize(&mut self, key: Key) -> Option<Slot> {
        let raster = match boxdraw::raster(key.c, self.fonts.cell()) {
            Some(raster) => raster,
            None => {
                let (face, glyph) = self.fonts.lookup(key.c, key.style)?;
                let fit = self.fonts.cell().width * u32::from(key.span);
                self.fonts.rasterize(face, glyph, Some(fit))?
            }
        };
        if raster.width == 0 || raster.height == 0 {
            return None;
        }
        let (x, y) = match self.atlas.insert(&raster) {
            Some(at) => at,
            None => {
                // Full at its largest: start over. Glyphs already handed out
                // this frame point into the old contents, which the bumped
                // generation tells the frame builder.
                self.atlas.clear();
                self.slots.clear();
                self.generation += 1;
                self.atlas.insert(&raster)?
            }
        };
        Some(Slot {
            x,
            y,
            width: raster.width,
            height: raster.height,
            left: raster.left,
            top: raster.top,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raster(width: u32, height: u32) -> Raster {
        Raster {
            left: 0,
            top: 0,
            width,
            height,
            alpha: vec![255; (width * height) as usize],
        }
    }

    #[test]
    fn packs_in_shelves_and_grows_down() {
        let mut atlas = Atlas::new();
        assert_eq!(atlas.insert(&raster(10, 20)), Some((0, 0)));
        assert_eq!(atlas.insert(&raster(10, 10)), Some((11, 0)));
        assert_eq!(atlas.take_dirty(), Some((0, 20)));
        assert_eq!(atlas.take_dirty(), None);
        // A glyph that does not fit on the shelf starts the next one below
        // the tallest glyph so far.
        assert_eq!(atlas.insert(&raster(1020, 5)), Some((0, 21)));
        for _ in 0..60 {
            atlas.insert(&raster(1000, 20)).unwrap();
        }
        assert_eq!(atlas.height(), 2048);
        assert_eq!(atlas.pixels().len(), 1024 * 2048);
        assert_eq!(atlas.pixels()[0], 255);
        assert_eq!(atlas.pixels()[10], 0, "the border column stays empty");
    }

    #[test]
    fn refuses_what_cannot_fit() {
        let mut atlas = Atlas::new();
        assert_eq!(atlas.insert(&raster(2000, 10)), None);
    }
}
