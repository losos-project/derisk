//! Finding fonts, measuring the cell, and rasterising glyphs.
//!
//! The family comes from the theme ("Hack" by default). Its Nerd Font build,
//! "Hack Nerd Font", is tried first because it is the same letters with the
//! icons added in the Private Use Area, then the family itself, then the
//! symbols-only Nerd Font, so a theme naming any monospace font still gets
//! the icons a prompt or a file manager prints. A character none of those
//! has is looked for in every other installed font with outlines, once, and
//! the answer is remembered.
//!
//! Nerd Font icons are drawn wider than a cell. A terminal can only give a
//! character the one cell its width says (two for a wide character), so an
//! icon followed by a blank cell is allowed to spill into it, as kitty and
//! WezTerm do, and anything still too big is scaled down to fit and centred.

use std::{collections::HashMap, sync::Arc};

use swash::{
    CacheKey, FontRef,
    scale::{Render, ScaleContext, Source, StrikeWith},
    zeno::{Angle, Format, Transform},
};

/// Bold, italic, both or neither, as the cell's flags ask.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Style {
    Regular,
    Bold,
    Italic,
    BoldItalic,
}

impl Style {
    pub fn new(bold: bool, italic: bool) -> Self {
        match (bold, italic) {
            (false, false) => Self::Regular,
            (true, false) => Self::Bold,
            (false, true) => Self::Italic,
            (true, true) => Self::BoldItalic,
        }
    }

    fn bold(self) -> bool {
        matches!(self, Self::Bold | Self::BoldItalic)
    }

    fn italic(self) -> bool {
        matches!(self, Self::Italic | Self::BoldItalic)
    }
}

/// One face, kept loaded. swash borrows the bytes per call, so the face
/// holds them and the offset and cache key swash would otherwise compute
/// again (a fresh key per call would also defeat its outline cache).
struct Face {
    data: Arc<Vec<u8>>,
    offset: u32,
    key: CacheKey,
    /// The face has no bold or italic of its own for this style, so swash
    /// thickens or slants the regular outlines.
    synthetic: Option<Style>,
}

impl Face {
    fn load(data: Arc<Vec<u8>>, index: u32) -> Option<Self> {
        let font = FontRef::from_index(&data, index as usize)?;
        let (offset, key) = (font.offset, font.key);
        Some(Self {
            data,
            offset,
            key,
            synthetic: None,
        })
    }

    fn font(&self) -> FontRef<'_> {
        FontRef {
            data: &self.data,
            offset: self.offset,
            key: self.key,
        }
    }
}

/// Where a face came from in the font database, so a face is read once
/// however many styles or fallbacks name it.
type FaceIndex = usize;

/// The cell every character is drawn in, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    pub width: u32,
    pub height: u32,
    /// From the cell's top to the baseline.
    pub baseline: i32,
    /// From the baseline down to the underline's top.
    pub underline: i32,
    /// Underline and strikeout thickness.
    pub stroke: u32,
}

/// A rasterised glyph: an alpha mask and where it sits relative to the
/// baseline at the cell's left edge.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Raster {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub alpha: Vec<u8>,
}

/// The fonts in use at one size.
pub struct Fonts {
    db: fontdb::Database,
    faces: Vec<Face>,
    /// `faces` indices for the primary family's four styles.
    primary: [FaceIndex; 4],
    /// `faces` indices tried, in order, after the primary family.
    fallbacks: Vec<FaceIndex>,
    /// Faces loaded from the database, by its ID.
    loaded: HashMap<fontdb::ID, FaceIndex>,
    /// Which face has a character none of the named ones has; `None` when
    /// no installed font does.
    found: HashMap<char, Option<FaceIndex>>,
    scale: ScaleContext,
    size: f32,
    cell: Cell,
}

/// Families every terminal can fall back to, after the theme's own: the
/// icons alone, then the monospace fonts most systems carry.
const FALLBACKS: [&str; 6] = [
    "Symbols Nerd Font Mono",
    "Symbols Nerd Font",
    "DejaVu Sans Mono",
    "Noto Sans Mono",
    "Liberation Mono",
    "Noto Sans Symbols 2",
];

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum Error {
    #[error("no monospace font is installed")]
    #[diagnostic(help(
        "install a monospace font, for example Hack Nerd Font, or name one in the theme's [fonts] monospace"
    ))]
    NoFont,
}

impl Fonts {
    /// The system's fonts, with `family` (the theme's monospace family) at
    /// `size` physical pixels.
    pub fn new(family: &str, size: f32) -> Result<Self, Error> {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        Self::with_database(db, family, size)
    }

    /// Like [`Fonts::new`], from fonts already loaded into `db`.
    pub fn with_database(db: fontdb::Database, family: &str, size: f32) -> Result<Self, Error> {
        let mut fonts = Self {
            db,
            faces: Vec::new(),
            primary: [0; 4],
            fallbacks: Vec::new(),
            loaded: HashMap::new(),
            found: HashMap::new(),
            scale: ScaleContext::new(),
            size,
            cell: Cell {
                width: 1,
                height: 1,
                baseline: 1,
                underline: 0,
                stroke: 1,
            },
        };
        let names = [
            format!("{family} Nerd Font"),
            format!("{family} Nerd Font Mono"),
            family.to_owned(),
        ];
        let regular = names
            .iter()
            .find_map(|name| fonts.query(name, Style::Regular))
            .or_else(|| {
                FALLBACKS[2..]
                    .iter()
                    .find_map(|name| fonts.query(name, Style::Regular))
            })
            .or_else(|| {
                let id = fonts.db.faces().find(|face| face.monospaced)?.id;
                Some((id, fonts.db.face(id)?.families.first()?.0.clone()))
            })
            .ok_or(Error::NoFont)?;
        let (regular_id, name) = regular;
        let regular = fonts.face(regular_id).ok_or(Error::NoFont)?;
        fonts.primary = [regular; 4];
        for (slot, style) in [(1, Style::Bold), (2, Style::Italic), (3, Style::BoldItalic)] {
            fonts.primary[slot] = match fonts.query(&name, style) {
                Some((id, _)) if id != regular_id => fonts.face(id).unwrap_or(regular),
                _ => fonts.synthetic(regular, style),
            };
        }
        for name in FALLBACKS {
            if let Some((id, _)) = fonts.query(name, Style::Regular)
                && let Some(face) = fonts.face(id)
                && face != regular
                && !fonts.fallbacks.contains(&face)
            {
                fonts.fallbacks.push(face);
            }
        }
        fonts.cell = fonts.measure();
        Ok(fonts)
    }

    /// The best face of `name` for `style`, if the family is installed, with
    /// the family's name as the database spells it.
    fn query(&self, name: &str, style: Style) -> Option<(fontdb::ID, String)> {
        let query = fontdb::Query {
            families: &[fontdb::Family::Name(name)],
            weight: if style.bold() {
                fontdb::Weight::BOLD
            } else {
                fontdb::Weight::NORMAL
            },
            stretch: fontdb::Stretch::Normal,
            style: if style.italic() {
                fontdb::Style::Italic
            } else {
                fontdb::Style::Normal
            },
        };
        let id = self.db.query(&query)?;
        let face = self.db.face(id)?;
        // fontdb answers with the nearest weight and style it has; a regular
        // face standing in for bold is better synthesised.
        let matches = (face.weight.0 >= 600) == style.bold()
            && (face.style != fontdb::Style::Normal) == style.italic();
        matches.then(|| {
            (
                id,
                face.families
                    .first()
                    .map_or(name.to_owned(), |f| f.0.clone()),
            )
        })
    }

    /// Loads face `id`, or returns it if it already is.
    fn face(&mut self, id: fontdb::ID) -> Option<FaceIndex> {
        if let Some(&index) = self.loaded.get(&id) {
            return Some(index);
        }
        let (source, index) = self.db.face_source(id)?;
        let data = match source {
            fontdb::Source::Binary(data) | fontdb::Source::SharedFile(_, data) => {
                (*data).as_ref().to_vec()
            }
            fontdb::Source::File(path) => std::fs::read(path).ok()?,
        };
        let face = Face::load(Arc::new(data), index)?;
        self.faces.push(face);
        let at = self.faces.len() - 1;
        self.loaded.insert(id, at);
        Some(at)
    }

    /// A copy of face `regular` drawn thicker or slanted for `style`.
    fn synthetic(&mut self, regular: FaceIndex, style: Style) -> FaceIndex {
        let base = &self.faces[regular];
        self.faces.push(Face {
            data: base.data.clone(),
            offset: base.offset,
            key: base.key,
            synthetic: Some(style),
        });
        self.faces.len() - 1
    }

    /// The cell: the primary face's advance for "M" across, its ascent plus
    /// descent down, whole pixels so the grid never blurs.
    fn measure(&self) -> Cell {
        let font = self.faces[self.primary[0]].font();
        let metrics = font.metrics(&[]).scale(self.size);
        let glyph = font.charmap().map('M');
        let advance = font
            .glyph_metrics(&[])
            .scale(self.size)
            .advance_width(glyph);
        let ascent = metrics.ascent.round();
        let height = (metrics.ascent + metrics.descent + metrics.leading).round();
        Cell {
            width: (advance.round() as u32).max(1),
            height: (height as u32).max(1),
            baseline: ascent as i32,
            underline: (-metrics.underline_offset).round().max(1.0) as i32,
            stroke: (metrics.stroke_size.round() as u32).max(1),
        }
    }

    /// The cell at this size.
    pub fn cell(&self) -> Cell {
        self.cell
    }

    /// The size in physical pixels.
    pub fn size(&self) -> f32 {
        self.size
    }

    /// The same fonts at another size.
    pub fn resize(&mut self, size: f32) {
        self.size = size;
        self.cell = self.measure();
    }

    /// The face that draws `c` in `style`, and its glyph: the primary family
    /// first, then the fallbacks, then whatever installed font has it.
    pub fn lookup(&mut self, c: char, style: Style) -> Option<(FaceIndex, u16)> {
        let styled = self.primary[style as usize];
        let regular = self.primary[0];
        for face in [styled, regular]
            .into_iter()
            .chain(self.fallbacks.iter().copied())
        {
            let glyph = self.faces[face].font().charmap().map(c);
            if glyph != 0 {
                return Some((face, glyph));
            }
        }
        let face = match self.found.get(&c) {
            Some(found) => *found,
            None => {
                let found = self.search(c);
                self.found.insert(c, found);
                found
            }
        }?;
        Some((face, self.faces[face].font().charmap().map(c)))
    }

    /// The first installed face with an outline for `c`. Faces with only
    /// colour bitmaps (emoji) are skipped: their glyphs would come out empty
    /// from an alpha-only rasteriser, hiding a font that could draw them.
    fn search(&mut self, c: char) -> Option<FaceIndex> {
        let id = self.db.faces().map(|face| face.id).find(|&id| {
            self.db
                .with_face_data(id, |data, index| {
                    let Some(font) = FontRef::from_index(data, index as usize) else {
                        return false;
                    };
                    let outlines = font.table(u32::from_be_bytes(*b"glyf")).is_some()
                        || font.table(u32::from_be_bytes(*b"CFF ")).is_some()
                        || font.table(u32::from_be_bytes(*b"CFF2")).is_some();
                    outlines && font.charmap().map(c) != 0
                })
                .unwrap_or(false)
        })?;
        self.face(id)
    }

    /// Rasterises `glyph` of `face`. With `fit` (the width of the cells the
    /// character may cover), a glyph wider than that or taller than the cell
    /// is scaled down until it fits and centred in that width.
    pub fn rasterize(&mut self, face: FaceIndex, glyph: u16, fit: Option<u32>) -> Option<Raster> {
        let raster = self.render(face, glyph, self.size)?;
        let Some(width) = fit else {
            return Some(raster);
        };
        let cell = self.cell;
        // Italics and a few letters lean a pixel or two past their cell in
        // every monospace font; only a real overflow is worth shrinking.
        let slack = (cell.width / 6).max(1);
        let too_wide = raster.width > width + slack;
        let too_tall = raster.height > cell.height + 1;
        if !too_wide && !too_tall {
            return Some(raster);
        }
        let factor = (width as f32 / raster.width as f32)
            .min(cell.height as f32 / raster.height as f32)
            .min(1.0);
        let mut fitted = self.render(face, glyph, self.size * factor)?;
        // Centre across the cells, and on the cell's middle vertically, the
        // way Nerd Fonts' own Mono builds place a shrunk icon.
        fitted.left = (width as i32 - fitted.width as i32) / 2;
        let middle = cell.baseline - cell.height as i32 / 2;
        fitted.top = middle + fitted.height as i32 / 2;
        Some(fitted)
    }

    fn render(&mut self, face: FaceIndex, glyph: u16, size: f32) -> Option<Raster> {
        let face = &self.faces[face];
        let synthetic = face.synthetic;
        let mut scaler = self
            .scale
            .builder(face.font())
            .size(size)
            .hint(true)
            .build();
        let mut render = Render::new(&[Source::Outline, Source::Bitmap(StrikeWith::BestFit)]);
        render.format(Format::Alpha);
        if let Some(style) = synthetic {
            if style.bold() {
                render.embolden(size / 24.0);
            }
            if style.italic() {
                render.transform(Some(Transform::skew(
                    Angle::from_degrees(12.0),
                    Angle::ZERO,
                )));
            }
        }
        let image = render.render(&mut scaler, glyph)?;
        Some(Raster {
            left: image.placement.left,
            top: image.placement.top,
            width: image.placement.width,
            height: image.placement.height,
            alpha: image.data,
        })
    }
}

/// Whether `c` is one of the icons Nerd Fonts adds: they live in the
/// Private Use Areas, which nothing else a terminal prints uses.
pub fn is_icon(c: char) -> bool {
    matches!(c, '\u{e000}'..='\u{f8ff}' | '\u{f0000}'..='\u{ffffd}' | '\u{100000}'..='\u{10fffd}')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fonts this machine has, or `None` on one with no monospace font
    /// at all (a bare build sandbox), where these tests have nothing to
    /// measure.
    fn system() -> Option<Fonts> {
        Fonts::new("Hack", 16.0).ok()
    }

    #[test]
    fn cell_is_whole_pixels_and_holds_the_baseline() {
        let Some(fonts) = system() else { return };
        let cell = fonts.cell();
        assert!(cell.width >= 6 && cell.width <= 16, "{cell:?}");
        assert!(cell.height > cell.width, "{cell:?}");
        assert!(cell.baseline > 0 && (cell.baseline as u32) < cell.height);
    }

    #[test]
    fn fitted_glyphs_stay_in_their_cells() {
        let Some(mut fonts) = system() else { return };
        let cell = fonts.cell();
        for c in ['M', 'g', '\u{e0b0}', '\u{f115}', '\u{f0219}'] {
            let Some((face, glyph)) = fonts.lookup(c, Style::Regular) else {
                continue;
            };
            let raster = fonts.rasterize(face, glyph, Some(cell.width)).unwrap();
            assert!(
                raster.width <= cell.width + cell.width / 6 + 1,
                "{c:?}: {raster:?}"
            );
            assert!(raster.height <= cell.height + 1, "{c:?}");
        }
    }

    #[test]
    fn icons_are_private_use() {
        assert!(is_icon('\u{e0b0}'));
        assert!(is_icon('\u{f0219}'));
        assert!(!is_icon('a'));
        assert!(!is_icon('─'));
    }
}
