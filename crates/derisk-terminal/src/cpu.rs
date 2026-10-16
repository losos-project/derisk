//! The software renderer: the frame drawn by the CPU into a shared-memory
//! buffer, for machines with no GPU (or only Mesa's llvmpipe, which would
//! draw the same quads more slowly through a whole GL pipeline).

use std::{num::NonZeroU32, rc::Rc};

use alacritty_terminal::vte::ansi::Rgb;
use winit::window::Window;

use crate::{
    atlas::Atlas,
    frame::{Frame, Quad},
};

pub struct Renderer {
    // The context must outlive the surface; field order drops it last.
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
    _context: softbuffer::Context<Rc<Window>>,
}

#[derive(Debug, thiserror::Error)]
#[error("no software buffer for the window: {0}")]
pub struct Error(#[from] softbuffer::SoftBufferError);

impl Renderer {
    pub fn new(window: Rc<Window>) -> Result<Self, Error> {
        let context = softbuffer::Context::new(window.clone())?;
        let surface = softbuffer::Surface::new(&context, window)?;
        Ok(Self {
            surface,
            _context: context,
        })
    }

    pub fn draw(&mut self, frame: &Frame, atlas: &Atlas) -> Result<(), Error> {
        let (Some(w), Some(h)) = (NonZeroU32::new(frame.width), NonZeroU32::new(frame.height))
        else {
            return Ok(());
        };
        self.surface.resize(w, h)?;
        let mut buffer = self.surface.buffer_mut()?;
        paint(&mut buffer, frame, atlas);
        buffer.present()?;
        Ok(())
    }
}

/// `0x00RRGGBB`, softbuffer's pixel format on every platform.
fn pixel(color: Rgb) -> u32 {
    (u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b)
}

/// Draws `frame` into `pixels`, `frame.width` pixels to a row.
pub fn paint(pixels: &mut [u32], frame: &Frame, atlas: &Atlas) {
    pixels.fill(pixel(frame.clear));
    for quad in &frame.rects {
        fill(pixels, frame, quad);
    }
    for quad in &frame.glyphs {
        blend(pixels, frame, atlas, quad);
    }
}

/// The part of `quad` inside the frame, as pixel ranges.
fn clip(frame: &Frame, quad: &Quad) -> Option<(usize, usize, usize, usize)> {
    let x0 = quad.x.max(0) as usize;
    let y0 = quad.y.max(0) as usize;
    let x1 = (quad.x + quad.width as i32).clamp(0, frame.width as i32) as usize;
    let y1 = (quad.y + quad.height as i32).clamp(0, frame.height as i32) as usize;
    (x0 < x1 && y0 < y1).then_some((x0, y0, x1, y1))
}

fn fill(pixels: &mut [u32], frame: &Frame, quad: &Quad) {
    let Some((x0, y0, x1, y1)) = clip(frame, quad) else {
        return;
    };
    let width = frame.width as usize;
    let color = pixel(quad.color);
    for y in y0..y1 {
        pixels[y * width + x0..y * width + x1].fill(color);
    }
}

fn blend(pixels: &mut [u32], frame: &Frame, atlas: &Atlas, quad: &Quad) {
    let Some((ax, ay)) = quad.atlas else {
        return;
    };
    let Some((x0, y0, x1, y1)) = clip(frame, quad) else {
        return;
    };
    let width = frame.width as usize;
    let stride = atlas.width() as usize;
    let coverage = atlas.pixels();
    let (sr, sg, sb) = (
        u32::from(quad.color.r),
        u32::from(quad.color.g),
        u32::from(quad.color.b),
    );
    for y in y0..y1 {
        let row = (ay as usize + (y as i32 - quad.y) as usize) * stride;
        for x in x0..x1 {
            let a = u32::from(coverage[row + ax as usize + (x as i32 - quad.x) as usize]);
            if a == 0 {
                continue;
            }
            let dst = &mut pixels[y * width + x];
            if a == 255 {
                *dst = (sr << 16) | (sg << 8) | sb;
                continue;
            }
            // Straight alpha over an opaque background, rounded to nearest:
            // d + (s - d) * a / 255 for each channel.
            let mix = |s: u32, d: u32| (s * a + d * (255 - a) + 127) / 255;
            let (dr, dg, db) = ((*dst >> 16) & 0xff, (*dst >> 8) & 0xff, *dst & 0xff);
            *dst = (mix(sr, dr) << 16) | (mix(sg, dg) << 8) | mix(sb, db);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rects_clip_and_glyphs_blend() {
        let frame = Frame {
            width: 4,
            height: 2,
            clear: Rgb { r: 0, g: 0, b: 0 },
            rects: vec![Quad {
                x: 3,
                y: -1,
                width: 5,
                height: 2,
                atlas: None,
                color: Rgb { r: 255, g: 0, b: 0 },
            }],
            glyphs: Vec::new(),
        };
        let atlas = Atlas::new();
        let mut pixels = vec![0; 8];
        paint(&mut pixels, &frame, &atlas);
        assert_eq!(pixels, [0, 0, 0, 0xff0000, 0, 0, 0, 0]);
    }
}
