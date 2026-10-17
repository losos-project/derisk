//! The frame: what to draw, built once per redraw and handed to a renderer.
//!
//! This is the deferred half of the renderer. Output from the shell only
//! changes the grid; nothing is drawn while it arrives. When the compositor
//! asks for a frame, the visible grid is turned into one list of rectangles
//! (backgrounds, selection, underlines, the cursor) and one of glyph quads
//! pointing into the [`Atlas`](crate::atlas::Atlas), and the renderer draws
//! both lists in one pass. However much the shell printed in between, that
//! is one frame's work, and the GPU and software renderers draw exactly the
//! same frame.

use alacritty_terminal::{
    grid::Indexed,
    index::Point,
    term::{
        RenderableContent,
        cell::{Cell, Flags},
        color::{COUNT, Colors},
        point_to_viewport,
    },
    vte::ansi::{Color, CursorShape, NamedColor, Rgb},
};

use crate::{
    atlas::Glyphs,
    font::{self, Style},
};

/// A rectangle in window pixels, either solid or a glyph's alpha from the
/// atlas tinted with `color`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quad {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// The glyph's top-left in the atlas, `None` for a solid rectangle.
    pub atlas: Option<(u32, u32)>,
    pub color: Rgb,
}

/// One frame.
#[derive(Default)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// Behind everything: the theme's background, or what the shell set it
    /// to.
    pub clear: Rgb,
    /// Drawn first.
    pub rects: Vec<Quad>,
    /// Drawn over the rectangles.
    pub glyphs: Vec<Quad>,
}

/// Where the grid sits in the window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub width: u32,
    pub height: u32,
    /// Empty pixels left of and above the first cell.
    pub padding: u32,
    pub columns: usize,
    pub lines: usize,
}

impl Layout {
    /// The grid that fits `width`×`height` pixels with `padding` around it.
    pub fn fit(width: u32, height: u32, padding: u32, cell: font::Cell) -> Self {
        let inner = |side: u32| side.saturating_sub(2 * padding);
        Self {
            width,
            height,
            padding,
            columns: (inner(width) / cell.width).max(2) as usize,
            lines: (inner(height) / cell.height).max(1) as usize,
        }
    }
}

/// The colour a cell names, with the shell's overrides (OSC 4, 10, 11)
/// over the theme's palette.
fn resolve(color: Color, overrides: &Colors, palette: &[Rgb; COUNT]) -> Rgb {
    let index = match color {
        Color::Spec(rgb) => return rgb,
        Color::Named(named) => named as usize,
        Color::Indexed(i) => usize::from(i),
    };
    overrides[index].unwrap_or(palette[index])
}

/// The foreground and background a cell is drawn in.
fn cell_colors(cell: &Cell, overrides: &Colors, palette: &[Rgb; COUNT]) -> (Rgb, Rgb) {
    let fg = match cell.fg {
        // Dim text of a named colour has its own palette entry.
        Color::Named(named) if cell.flags.contains(Flags::DIM) => {
            resolve(Color::Named(named.to_dim()), overrides, palette)
        }
        Color::Indexed(i) if cell.flags.contains(Flags::DIM) && i < 8 => resolve(
            Color::Named(NamedColor::from_index(i).to_dim()),
            overrides,
            palette,
        ),
        color => resolve(color, overrides, palette),
    };
    let bg = resolve(cell.bg, overrides, palette);
    if cell.flags.contains(Flags::INVERSE) {
        (bg, fg)
    } else {
        (fg, bg)
    }
}

/// `NamedColor`'s constructor from an ANSI index, which vte does not have.
trait FromIndex {
    fn from_index(i: u8) -> Self;
}

impl FromIndex for NamedColor {
    fn from_index(i: u8) -> Self {
        use NamedColor::*;
        [Black, Red, Green, Yellow, Blue, Magenta, Cyan, White][usize::from(i & 7)]
    }
}

/// What the frame needs from a cell, copied out of the grid so a row can be
/// looked ahead in.
struct Item {
    column: usize,
    c: char,
    flags: Flags,
    fg: Rgb,
    bg: Rgb,
    zerowidth: Vec<char>,
}

/// The selection colour, and whether the window has focus, which the
/// cursor's shape depends on.
pub struct Look<'a> {
    pub palette: &'a [Rgb; COUNT],
    pub selection: Rgb,
    pub focused: bool,
}

/// Builds the frame for `content` into `frame`, rasterising glyphs it has
/// not drawn before.
pub fn build(
    frame: &mut Frame,
    content: RenderableContent<'_>,
    look: &Look<'_>,
    glyphs: &mut Glyphs,
    layout: Layout,
) {
    let RenderableContent {
        display_iter,
        selection,
        cursor,
        display_offset,
        colors,
        mode: _,
    } = content;
    let cells: Vec<Indexed<&Cell>> = display_iter.collect();
    // Rasterising can fill the atlas, which then starts over and moves the
    // glyphs placed so far; building again places them all in the new one.
    for _ in 0..2 {
        let generation = glyphs.generation();
        fill(
            frame,
            &cells,
            selection,
            cursor,
            display_offset,
            colors,
            look,
            glyphs,
            layout,
        );
        if glyphs.generation() == generation {
            break;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn fill(
    frame: &mut Frame,
    cells: &[Indexed<&Cell>],
    selection: Option<alacritty_terminal::selection::SelectionRange>,
    cursor: alacritty_terminal::term::RenderableCursor,
    display_offset: usize,
    colors: &Colors,
    look: &Look<'_>,
    glyphs: &mut Glyphs,
    layout: Layout,
) {
    let cell = glyphs.fonts.cell();
    let background = resolve(Color::Named(NamedColor::Background), colors, look.palette);
    let cursor_color = resolve(Color::Named(NamedColor::Cursor), colors, look.palette);
    frame.width = layout.width;
    frame.height = layout.height;
    frame.clear = background;
    frame.rects.clear();
    frame.glyphs.clear();

    let pad = layout.padding as i32;
    let (cw, ch) = (cell.width as i32, cell.height as i32);
    let cursor_at = point_to_viewport(display_offset, cursor.point)
        .filter(|_| cursor.shape != CursorShape::Hidden);
    // A block cursor is drawn by colouring its cell; every other shape, and
    // the block when the window is not focused, is drawn over the text.
    let solid_cursor = look.focused && cursor.shape == CursorShape::Block;

    let mut row: Vec<Item> = Vec::new();
    let mut i = 0;
    while i < cells.len() {
        let line = cells[i].point.line;
        row.clear();
        while i < cells.len() && cells[i].point.line == line {
            let indexed = &cells[i];
            let (mut fg, mut bg) = cell_colors(indexed.cell, colors, look.palette);
            let point = Point::new(indexed.point.line, indexed.point.column);
            if selection.is_some_and(|s| s.contains(point)) {
                bg = look.selection;
            }
            let at_cursor = cursor_at
                .is_some_and(|c| point_to_viewport(display_offset, point).is_some_and(|p| p == c));
            if at_cursor && solid_cursor {
                fg = background;
                bg = cursor_color;
            }
            if indexed.cell.flags.contains(Flags::HIDDEN) {
                fg = bg;
            }
            row.push(Item {
                column: indexed.point.column.0,
                c: indexed.cell.c,
                flags: indexed.cell.flags,
                fg,
                bg,
                zerowidth: indexed
                    .cell
                    .zerowidth()
                    .map(<[char]>::to_vec)
                    .unwrap_or_default(),
            });
            i += 1;
        }
        let Some(viewport) =
            point_to_viewport(display_offset, Point::new(line, Default::default()))
        else {
            continue;
        };
        let y = pad + viewport.line as i32 * ch;
        draw_row(frame, &row, y, background, cell, pad, glyphs);
    }

    if let Some(at) = cursor_at
        && !solid_cursor
    {
        let x = pad + at.column.0 as i32 * cw;
        let y = pad + at.line as i32 * ch;
        let wide = cells
            .iter()
            .find(|c| point_to_viewport(display_offset, c.point) == Some(at))
            .is_some_and(|c| c.cell.flags.contains(Flags::WIDE_CHAR));
        let width = cell.width * if wide { 2 } else { 1 };
        let thick = cell.stroke.max(2);
        let mut rect = |x: i32, y: i32, width: u32, height: u32| {
            frame.rects.push(Quad {
                x,
                y,
                width,
                height,
                atlas: None,
                color: cursor_color,
            })
        };
        match cursor.shape {
            CursorShape::Beam => rect(x, y, thick, cell.height),
            CursorShape::Underline => rect(x, y + ch - thick as i32, width, thick),
            // Hollow: the block's outline, which is also how an unfocused
            // window shows where its cursor is.
            _ => {
                let t = thick as i32 / 2;
                rect(x, y, width, t as u32);
                rect(x, y + ch - t, width, t as u32);
                rect(x, y, t as u32, cell.height);
                rect(x + width as i32 - t, y, t as u32, cell.height);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_row(
    frame: &mut Frame,
    row: &[Item],
    y: i32,
    background: Rgb,
    cell: font::Cell,
    pad: i32,
    glyphs: &mut Glyphs,
) {
    let cw = cell.width as i32;
    for (k, item) in row.iter().enumerate() {
        let x = pad + item.column as i32 * cw;
        // Backgrounds of neighbouring cells in the same colour become one
        // rectangle, so a coloured status line is one quad, not a hundred.
        if item.bg != background {
            match frame.rects.last_mut() {
                Some(last)
                    if last.atlas.is_none()
                        && last.color == item.bg
                        && last.y == y
                        && last.x + last.width as i32 == x
                        && last.height == cell.height =>
                {
                    last.width += cell.width;
                }
                _ => frame.rects.push(Quad {
                    x,
                    y,
                    width: cell.width,
                    height: cell.height,
                    atlas: None,
                    color: item.bg,
                }),
            }
        }
        if item.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }

        let style = Style::new(
            item.flags.contains(Flags::BOLD),
            item.flags.contains(Flags::ITALIC),
        );
        let span: u8 = if item.flags.contains(Flags::WIDE_CHAR) {
            2
        } else if font::is_icon(item.c)
            && row.get(k + 1).is_some_and(|next| {
                next.column == item.column + 1
                    && next.c == ' '
                    && next.bg == item.bg
                    && next.zerowidth.is_empty()
            })
        {
            // An icon with room after it may use it (the module docs).
            2
        } else {
            1
        };
        if item.c != ' ' && item.c != '\t' {
            for (k, c) in std::iter::once(item.c)
                .chain(item.zerowidth.iter().copied())
                .enumerate()
            {
                if let Some(slot) = glyphs.get(c, style, span) {
                    // A combining mark has no advance of its own and sits
                    // left of its origin, over where the letter before it
                    // ended; here that origin is the letter's, so it moves
                    // over by a cell.
                    let shift = if k > 0 && slot.left < 0 { cw } else { 0 };
                    frame.glyphs.push(Quad {
                        x: x + shift + slot.left,
                        y: y + cell.baseline - slot.top,
                        width: slot.width,
                        height: slot.height,
                        atlas: Some((slot.x, slot.y)),
                        color: item.fg,
                    });
                }
            }
        }

        let width = cell.width
            * if item.flags.contains(Flags::WIDE_CHAR) {
                2
            } else {
                1
            };
        let stroke = cell.stroke;
        let mut line = |dy: i32| {
            frame.rects.push(Quad {
                x,
                y: y + dy,
                width,
                height: stroke,
                atlas: None,
                color: item.fg,
            })
        };
        let underline = (cell.baseline + cell.underline).min(cell.height as i32 - stroke as i32);
        if item.flags.intersects(Flags::ALL_UNDERLINES) {
            line(underline);
            if item.flags.contains(Flags::DOUBLE_UNDERLINE) {
                line(underline - 2 * stroke as i32);
            }
        }
        if item.flags.contains(Flags::STRIKEOUT) {
            line(cell.baseline - cell.height as i32 / 4);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell() -> font::Cell {
        font::Cell {
            width: 8,
            height: 16,
            baseline: 12,
            underline: 2,
            stroke: 1,
        }
    }

    #[test]
    fn layout_fits_whole_cells_inside_the_padding() {
        let layout = Layout::fit(8 * 80 + 8, 16 * 24 + 8 + 5, 4, cell());
        assert_eq!((layout.columns, layout.lines), (80, 24));
        // A window too small for anything still has a cell to put the
        // cursor in.
        let tiny = Layout::fit(3, 3, 4, cell());
        assert_eq!((tiny.columns, tiny.lines), (2, 1));
    }

    #[test]
    fn inverse_swaps_and_dim_uses_the_dim_palette() {
        let palette = crate::theme::Look::from_theme(&mcsapi_theme::Theme::dark()).palette();
        let overrides = Colors::default();
        let mut c = Cell {
            fg: Color::Named(NamedColor::Red),
            ..Cell::default()
        };
        assert_eq!(
            cell_colors(&c, &overrides, &palette),
            (palette[1], palette[257])
        );
        c.flags = Flags::INVERSE;
        assert_eq!(
            cell_colors(&c, &overrides, &palette),
            (palette[257], palette[1])
        );
        c.flags = Flags::DIM;
        assert_eq!(
            cell_colors(&c, &overrides, &palette).0,
            palette[NamedColor::DimRed as usize]
        );
    }

    #[test]
    fn shell_overrides_win_over_the_palette() {
        let palette = [Rgb::default(); COUNT];
        let mut overrides = Colors::default();
        overrides[NamedColor::Background] = Some(Rgb { r: 1, g: 2, b: 3 });
        let bg = resolve(Color::Named(NamedColor::Background), &overrides, &palette);
        assert_eq!(bg, Rgb { r: 1, g: 2, b: 3 });
    }
}
