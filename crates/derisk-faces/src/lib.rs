//! Faces in a picture, for derisk's privacy display.
//!
//! [`Detector`] wraps libfacedetection's CNN (`libfacedetection-sys`, built
//! from the vendored source) in a type that owns the buffers the C++ writes
//! to, so nothing it hands back outlives them, and that holds a lock across
//! each call, because the library fills its weights into globals on first use.
//! [`Bgr`] is a checked view of the 8-bit BGR image it reads, and
//! [`yuyv_to_bgr`] and [`grey_to_bgr`] make one from what a webcam sends.
//!
//! The `derisk-faces` binary is the camera side: it reads small frames over
//! V4L2 and prints how many faces each one has. Frames stay in its memory;
//! nothing is written anywhere.

use std::sync::Mutex;

use libfacedetection_sys as ffi;

/// The library keeps its weights in globals it fills on the first call and
/// is not safe to enter twice at once: every call goes through this lock.
static LIBRARY: Mutex<()> = Mutex::new(());

/// What a picture cannot be read as.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ImageError {
    /// Width or height is zero, or too big for the library's `int`s.
    #[error("a {width}×{height} image cannot be searched")]
    Size {
        /// The width given.
        width: u32,
        /// The height given.
        height: u32,
    },
    /// The rows are shorter than three bytes a pixel, or the data ends
    /// before the last row does.
    #[error("{len} bytes do not hold {height} rows of {stride} bytes")]
    Short {
        /// The data's length.
        len: usize,
        /// The row stride given.
        stride: usize,
        /// The height given.
        height: u32,
    },
}

/// An 8-bit BGR image, row after row, each `stride` bytes apart.
#[derive(Clone, Copy, Debug)]
pub struct Bgr<'a> {
    data: &'a [u8],
    width: u32,
    height: u32,
    stride: usize,
}

impl<'a> Bgr<'a> {
    /// A view of `data` as a `width`×`height` BGR image with rows `stride`
    /// bytes apart, if it holds that much.
    pub fn new(data: &'a [u8], width: u32, height: u32, stride: usize) -> Result<Self, ImageError> {
        let fits = |n: u32| n > 0 && i32::try_from(n).is_ok();
        if !fits(width) || !fits(height) || i32::try_from(stride).is_err() {
            return Err(ImageError::Size { width, height });
        }
        let row = width as usize * 3;
        let needed = stride
            .checked_mul(height as usize - 1)
            .and_then(|n| n.checked_add(row));
        if stride < row || needed.is_none_or(|n| data.len() < n) {
            return Err(ImageError::Short {
                len: data.len(),
                stride,
                height,
            });
        }
        Ok(Self {
            data,
            width,
            height,
            stride,
        })
    }

    /// Its width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Its height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }
}

/// A rectangle in the image, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width.
    pub width: i32,
    /// Height.
    pub height: i32,
}

/// One face the detector found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Face {
    /// How sure it is, 0–100.
    pub confidence: u8,
    /// Where the face is.
    pub rect: Rect,
    /// The eyes, the nose and the corners of the mouth, as (x, y).
    pub landmarks: [(i32, i32); 5],
}

/// The buffer the library writes its results to: an `int` count, then a
/// fixed stride of `short`s a face. `u32`s, so it is aligned for both.
const RESULT_WORDS: usize = ffi::FACEDETECTION_RESULT_BUFFER_SIZE as usize / 4;
const STRIDE: usize = ffi::FACEDETECTION_RESULT_STRIDE_SHORTS as usize;

/// libfacedetection's face detector, with the buffers it works in.
pub struct Detector {
    results: Box<[u32; RESULT_WORDS]>,
    /// The library takes the image through a mutable pointer, so it reads a
    /// copy of the caller's rather than a cast of a shared borrow.
    image: Vec<u8>,
}

impl Default for Detector {
    fn default() -> Self {
        Self::new()
    }
}

impl Detector {
    /// A detector. The library's weights are filled on the first search.
    pub fn new() -> Self {
        Self {
            results: Box::new([0; RESULT_WORDS]),
            image: Vec::new(),
        }
    }

    /// The faces in `image`, most of them at least `min_confidence` sure,
    /// as the library ranks them.
    pub fn detect(
        &mut self,
        image: &Bgr<'_>,
        min_confidence: u8,
    ) -> impl Iterator<Item = Face> + '_ {
        self.image.clear();
        self.image.extend_from_slice(image.data);
        let count = {
            let _library = LIBRARY
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // SAFETY: the result buffer is FACEDETECTION_RESULT_BUFFER_SIZE
            // bytes, aligned for the int and shorts written into it; the
            // image is our own copy, `stride * (height - 1) + width * 3`
            // bytes at least (checked by Bgr::new), and the dimensions fit
            // in an int. The lock keeps any other call out of the library's
            // globals until this one returns.
            let found = unsafe {
                ffi::facedetect_cnn(
                    self.results.as_mut_ptr().cast(),
                    self.image.as_mut_ptr(),
                    image.width as i32,
                    image.height as i32,
                    image.stride as i32,
                )
            };
            if found.is_null() {
                0
            } else {
                // The count is the buffer's first word, where `found` points.
                (self.results[0] as i32).clamp(0, ffi::FACEDETECTION_RESULT_MAX_FACES as i32)
                    as usize
            }
        };
        let results: &[u32] = &self.results[1..];
        (0..count)
            .map(move |i| face(results, i))
            .filter(move |f| f.confidence >= min_confidence)
    }
}

/// The `i`th face in the results after the count, `STRIDE` shorts each, in
/// the machine's byte order.
fn face(results: &[u32], i: usize) -> Face {
    let short = |n: usize| {
        let at = i * STRIDE + n;
        let word = results[at / 2].to_ne_bytes();
        let half = if at.is_multiple_of(2) {
            [word[0], word[1]]
        } else {
            [word[2], word[3]]
        };
        i32::from(i16::from_ne_bytes(half))
    };
    let mut landmarks = [(0, 0); 5];
    for (k, point) in landmarks.iter_mut().enumerate() {
        *point = (short(5 + 2 * k), short(6 + 2 * k));
    }
    Face {
        confidence: short(0).clamp(0, 100) as u8,
        rect: Rect {
            x: short(1),
            y: short(2),
            width: short(3),
            height: short(4),
        },
        landmarks,
    }
}

/// Converts a YUYV (YUV 4:2:2) frame, as most webcams send, to packed BGR
/// in `out`, with BT.601's limited-range coefficients.
pub fn yuyv_to_bgr(yuyv: &[u8], width: u32, height: u32, out: &mut Vec<u8>) {
    let pixels = width as usize * height as usize;
    out.clear();
    out.reserve(pixels * 3);
    let clamp = |v: i32| v.clamp(0, 255) as u8;
    for quad in yuyv.as_chunks::<4>().0.iter().take(pixels / 2) {
        let (u, v) = (i32::from(quad[1]) - 128, i32::from(quad[3]) - 128);
        for y in [quad[0], quad[2]] {
            let c = 298 * (i32::from(y) - 16);
            out.extend([
                clamp((c + 516 * u + 128) >> 8),
                clamp((c - 100 * u - 208 * v + 128) >> 8),
                clamp((c + 409 * v + 128) >> 8),
            ]);
        }
    }
}

/// Converts an 8-bit greyscale frame (an infrared camera's) to BGR in `out`.
pub fn grey_to_bgr(grey: &[u8], width: u32, height: u32, out: &mut Vec<u8>) {
    out.clear();
    out.extend(
        grey.iter()
            .take(width as usize * height as usize)
            .flat_map(|&g| [g, g, g]),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_image_must_hold_its_rows() {
        let data = vec![0; 4 * 3 * 2];
        assert!(Bgr::new(&data, 4, 2, 12).is_ok());
        assert_eq!(
            Bgr::new(&data, 4, 3, 12).unwrap_err(),
            ImageError::Short {
                len: 24,
                stride: 12,
                height: 3
            }
        );
        assert!(matches!(
            Bgr::new(&data, 4, 2, 11),
            Err(ImageError::Short { .. })
        ));
        assert!(matches!(
            Bgr::new(&data, 0, 2, 12),
            Err(ImageError::Size { .. })
        ));
    }

    #[test]
    fn a_blank_picture_has_no_faces() {
        let data = vec![128; 160 * 120 * 3];
        let image = Bgr::new(&data, 160, 120, 160 * 3).unwrap();
        assert_eq!(Detector::new().detect(&image, 0).count(), 0);
    }

    #[test]
    fn webcam_frames_become_bgr() {
        let mut out = Vec::new();
        // Black, then white, in YUYV.
        yuyv_to_bgr(&[16, 128, 235, 128], 2, 1, &mut out);
        assert_eq!(out, [0, 0, 0, 255, 255, 255]);
        grey_to_bgr(&[7, 9], 2, 1, &mut out);
        assert_eq!(out, [7, 7, 7, 9, 9, 9]);
    }

    #[test]
    fn results_are_read_as_the_library_writes_them() {
        let mut results = vec![0u32; STRIDE];
        let shorts: [i16; 15] = [97, 10, 20, 30, 40, 1, 2, 3, 4, 5, 6, 7, 8, 9, -1];
        for (n, s) in shorts.iter().enumerate() {
            let mut word = results[n / 2].to_ne_bytes();
            let at = (n % 2) * 2;
            word[at..at + 2].copy_from_slice(&s.to_ne_bytes());
            results[n / 2] = u32::from_ne_bytes(word);
        }
        let face = face(&results, 0);
        assert_eq!(face.confidence, 97);
        assert_eq!(
            face.rect,
            Rect {
                x: 10,
                y: 20,
                width: 30,
                height: 40
            }
        );
        assert_eq!(face.landmarks[0], (1, 2));
        assert_eq!(face.landmarks[4], (9, -1));
    }
}
