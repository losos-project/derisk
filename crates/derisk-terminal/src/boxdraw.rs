//! Box-drawing and block characters, drawn to the cell instead of taken
//! from the font.
//!
//! A font's `─` is as wide as its advance, which is not the cell once the
//! advance is rounded to whole pixels, and its `│` is as tall as its em box,
//! not the line: from the font, boxes and TUI borders come out dashed. Drawn
//! here they meet their neighbours exactly at any size, as in every modern
//! terminal. Diagonals and arcs' curves are left to the font; rounded
//! corners are drawn square.

use crate::font::{Cell, Raster};

/// How heavy one arm of a line character is.
#[derive(Clone, Copy, PartialEq)]
enum W {
    N,
    L,
    H,
    D,
}

use W::*;

/// Up, right, down, left, for U+2500 to U+257F; `None` for the diagonals.
/// Dashed lines are drawn solid.
fn arms(c: char) -> Option<[W; 4]> {
    let i = (c as u32).checked_sub(0x2500)?;
    const T: [[W; 4]; 0x80] = [
        [N, L, N, L],
        [N, H, N, H],
        [L, N, L, N],
        [H, N, H, N], // ─━│┃
        [N, L, N, L],
        [N, H, N, H],
        [L, N, L, N],
        [H, N, H, N], // ┄┅┆┇
        [N, L, N, L],
        [N, H, N, H],
        [L, N, L, N],
        [H, N, H, N], // ┈┉┊┋
        [N, L, L, N],
        [N, H, L, N],
        [N, L, H, N],
        [N, H, H, N], // ┌┍┎┏
        [N, N, L, L],
        [N, N, L, H],
        [N, N, H, L],
        [N, N, H, H], // ┐┑┒┓
        [L, L, N, N],
        [L, H, N, N],
        [H, L, N, N],
        [H, H, N, N], // └┕┖┗
        [L, N, N, L],
        [L, N, N, H],
        [H, N, N, L],
        [H, N, N, H], // ┘┙┚┛
        [L, L, L, N],
        [L, H, L, N],
        [H, L, L, N],
        [L, L, H, N], // ├┝┞┟
        [H, L, H, N],
        [H, H, L, N],
        [L, H, H, N],
        [H, H, H, N], // ┠┡┢┣
        [L, N, L, L],
        [L, N, L, H],
        [H, N, L, L],
        [L, N, H, L], // ┤┥┦┧
        [H, N, H, L],
        [H, N, L, H],
        [L, N, H, H],
        [H, N, H, H], // ┨┩┪┫
        [N, L, L, L],
        [N, L, L, H],
        [N, H, L, L],
        [N, H, L, H], // ┬┭┮┯
        [N, L, H, L],
        [N, L, H, H],
        [N, H, H, L],
        [N, H, H, H], // ┰┱┲┳
        [L, L, N, L],
        [L, L, N, H],
        [L, H, N, L],
        [L, H, N, H], // ┴┵┶┷
        [H, L, N, L],
        [H, L, N, H],
        [H, H, N, L],
        [H, H, N, H], // ┸┹┺┻
        [L, L, L, L],
        [L, L, L, H],
        [L, H, L, L],
        [L, H, L, H], // ┼┽┾┿
        [H, L, L, L],
        [L, L, H, L],
        [H, L, H, L],
        [H, L, L, H], // ╀╁╂╃
        [H, H, L, L],
        [L, L, H, H],
        [L, H, H, L],
        [H, H, L, H], // ╄╅╆╇
        [L, H, H, H],
        [H, L, H, H],
        [H, H, H, L],
        [H, H, H, H], // ╈╉╊╋
        [N, L, N, L],
        [N, H, N, H],
        [L, N, L, N],
        [H, N, H, N], // ╌╍╎╏
        [N, D, N, D],
        [D, N, D, N],
        [N, D, L, N],
        [N, L, D, N], // ═║╒╓
        [N, D, D, N],
        [N, N, L, D],
        [N, N, D, L],
        [N, N, D, D], // ╔╕╖╗
        [L, D, N, N],
        [D, L, N, N],
        [D, D, N, N],
        [L, N, N, D], // ╘╙╚╛
        [D, N, N, L],
        [D, N, N, D],
        [L, D, L, N],
        [D, L, D, N], // ╜╝╞╟
        [D, D, D, N],
        [L, N, L, D],
        [D, N, D, L],
        [D, N, D, D], // ╠╡╢╣
        [N, D, L, D],
        [N, L, D, L],
        [N, D, D, D],
        [L, D, N, D], // ╤╥╦╧
        [D, L, N, L],
        [D, D, N, D],
        [L, D, L, D],
        [D, L, D, L], // ╨╩╪╫
        [D, D, D, D],
        [N, L, L, N],
        [N, N, L, L],
        [L, N, N, L], // ╬╭╮╯
        [L, L, N, N],
        [N, N, N, N],
        [N, N, N, N],
        [N, N, N, N], // ╰╱╲╳
        [N, N, N, L],
        [L, N, N, N],
        [N, L, N, N],
        [N, N, L, N], // ╴╵╶╷
        [N, N, N, H],
        [H, N, N, N],
        [N, H, N, N],
        [N, N, H, N], // ╸╹╺╻
        [N, H, N, L],
        [L, N, H, N],
        [N, L, N, H],
        [H, N, L, N], // ╼╽╾╿
    ];
    let arms = *T.get(i as usize)?;
    (arms != [N; 4]).then_some(arms)
}

/// A cell-sized alpha canvas.
struct Canvas {
    width: u32,
    height: u32,
    alpha: Vec<u8>,
}

impl Canvas {
    fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            alpha: vec![0; (width * height) as usize],
        }
    }

    /// Fills `x0..x1` × `y0..y1`, clipped to the canvas, with `a`.
    fn fill(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, a: u8) {
        let (w, h) = (self.width as i32, self.height as i32);
        for y in y0.clamp(0, h)..y1.clamp(0, h) {
            for x in x0.clamp(0, w)..x1.clamp(0, w) {
                self.alpha[(y * w + x) as usize] = a;
            }
        }
    }

    fn raster(self, cell: Cell) -> Raster {
        Raster {
            left: 0,
            top: cell.baseline,
            width: self.width,
            height: self.height,
            alpha: self.alpha,
        }
    }
}

/// `c` drawn to fill `cell`, if it is a line or block character drawn here.
pub fn raster(c: char, cell: Cell) -> Option<Raster> {
    let (w, h) = (cell.width as i32, cell.height as i32);
    let mut canvas = Canvas::new(cell.width, cell.height);
    if let Some(arms) = arms(c) {
        let light = (w / 8).max(cell.stroke as i32).max(1);
        let thickness = |weight: W| match weight {
            N => 0,
            L | D => light,
            H => light * 2,
        };
        // Where a stroke of thickness `t` starts so it is centred across
        // `side`; a double line's two strokes sit a stroke either side.
        let centre = |side: i32, t: i32| (side - t) / 2;
        for (direction, &weight) in arms.iter().enumerate() {
            if weight == N {
                continue;
            }
            // Along the arm, the cell's extent; across it, the other.
            let vertical = direction % 2 == 0;
            let (along, across) = if vertical { (h, w) } else { (w, h) };
            // Up and left run from the edge at 0 inward; right and down
            // from the centre out to the far edge.
            let inward = direction == 0 || direction == 3;
            // The perpendicular arms on the low and high side across this
            // one: left and right for a vertical arm, up and down for a
            // horizontal one.
            let (low, high) = if vertical {
                (arms[3], arms[1])
            } else {
                (arms[0], arms[2])
            };
            let lo = centre(along, light) - light;
            let hi = centre(along, light) + light;
            // Where a stroke of a double arm stops at the centre, from the
            // perpendicular arm on its own side (`near`) and the one
            // opposite: a double near arm stops it at that arm's inner
            // stroke, a double opposite one carries it to the outer stroke,
            // and a single one meets it across its width.
            let meet = |near: W, other: W| -> i32 {
                match (near, other) {
                    (D, _) => {
                        if inward {
                            lo + light
                        } else {
                            hi
                        }
                    }
                    (N, D) => {
                        if inward {
                            hi + light
                        } else {
                            lo
                        }
                    }
                    (N, N) => {
                        if inward {
                            centre(along, light) + light
                        } else {
                            centre(along, light)
                        }
                    }
                    _ => {
                        let t = thickness(near).max(thickness(other));
                        if inward {
                            centre(along, t) + t
                        } else {
                            centre(along, t)
                        }
                    }
                }
            };
            let strokes: Vec<(i32, i32, i32)> = if weight == D {
                // The low stroke turns towards the low perpendicular arm,
                // the high one towards the high arm, as ╔ and ╬ nest.
                let a = centre(across, light) - light;
                let b = centre(across, light) + light;
                vec![(a, light, meet(low, high)), (b, light, meet(high, low))]
            } else {
                let t = thickness(weight);
                let stop = if low == D || high == D {
                    if inward { hi + light } else { lo }
                } else {
                    let tp = thickness(low).max(thickness(high));
                    let tp = if tp == 0 { t } else { tp };
                    if inward {
                        centre(along, tp) + tp
                    } else {
                        centre(along, tp)
                    }
                };
                vec![(centre(across, t), t, stop)]
            };
            for (at, t, stop) in strokes {
                let (a0, a1) = if inward { (0, stop) } else { (stop, along) };
                if vertical {
                    canvas.fill(at, a0, at + t, a1, 255);
                } else {
                    canvas.fill(a0, at, a1, at + t, 255);
                }
            }
        }
        return Some(canvas.raster(cell));
    }
    // Block elements, U+2580 to U+259F.
    let eighth_h = |n: i32| (h * n + 4) / 8;
    let eighth_w = |n: i32| (w * n + 4) / 8;
    let (hw, hh) = (w / 2, h / 2);
    match c {
        '▀' => canvas.fill(0, 0, w, hh, 255),
        '▁'..='█' => {
            let n = c as i32 - 0x2580;
            canvas.fill(0, h - eighth_h(n), w, h, 255);
        }
        '▉'..='▏' => {
            let n = 8 - (c as i32 - 0x2588);
            canvas.fill(0, 0, eighth_w(n), h, 255);
        }
        '▐' => canvas.fill(hw, 0, w, h, 255),
        '░' => canvas.fill(0, 0, w, h, 64),
        '▒' => canvas.fill(0, 0, w, h, 128),
        '▓' => canvas.fill(0, 0, w, h, 192),
        '▔' => canvas.fill(0, 0, w, eighth_h(1), 255),
        '▕' => canvas.fill(w - eighth_w(1), 0, w, h, 255),
        '▖'..='▟' => {
            // Quadrants: upper left, upper right, lower left, lower right.
            let quads: [u8; 10] = [
                0b0010, 0b0001, 0b1000, 0b1011, 0b1001, 0b1110, 0b1101, 0b0100, 0b0110, 0b0111,
            ];
            let q = quads[(c as u32 - 0x2596) as usize];
            if q & 0b1000 != 0 {
                canvas.fill(0, 0, hw, hh, 255);
            }
            if q & 0b0100 != 0 {
                canvas.fill(hw, 0, w, hh, 255);
            }
            if q & 0b0010 != 0 {
                canvas.fill(0, hh, hw, h, 255);
            }
            if q & 0b0001 != 0 {
                canvas.fill(hw, hh, w, h, 255);
            }
        }
        _ => return None,
    }
    Some(canvas.raster(cell))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: Cell = Cell {
        width: 7,
        height: 15,
        baseline: 11,
        underline: 2,
        stroke: 1,
    };

    fn at(r: &Raster, x: u32, y: u32) -> u8 {
        r.alpha[(y * r.width + x) as usize]
    }

    #[test]
    fn lines_reach_every_edge_they_name() {
        let h = raster('─', CELL).unwrap();
        assert!(
            (0..7).all(|x| at(&h, x, 7) == 255),
            "a horizontal line spans the cell"
        );
        let v = raster('│', CELL).unwrap();
        assert!(
            (0..15).all(|y| at(&v, 3, y) == 255),
            "a vertical line spans the line"
        );
        let corner = raster('┌', CELL).unwrap();
        assert_eq!(at(&corner, 6, 7), 255);
        assert_eq!(at(&corner, 3, 14), 255);
        assert_eq!(at(&corner, 0, 7), 0, "nothing to the left");
        assert_eq!(at(&corner, 3, 0), 0, "nothing above");
    }

    #[test]
    fn blocks_fill_their_part() {
        let lower = raster('▄', CELL).unwrap();
        assert_eq!(at(&lower, 0, 14), 255);
        assert_eq!(at(&lower, 0, 0), 0);
        let full = raster('█', CELL).unwrap();
        assert!(full.alpha.iter().all(|&a| a == 255));
        let quadrant = raster('▘', CELL).unwrap();
        assert_eq!((at(&quadrant, 0, 0), at(&quadrant, 6, 14)), (255, 0));
        assert_eq!(raster('a', CELL), None);
        assert_eq!(raster('╱', CELL), None, "diagonals come from the font");
    }
}
