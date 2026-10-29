//! Blacks out the secrets in a screenshot: passwords, keys and tokens,
//! card numbers, IBANs, email addresses, phone numbers, onion and wallet
//! addresses, public IP addresses.
//!
//! Three things say where they are:
//!
//! - [`ocr`] reads the screen's text, and [`patterns`] finds the secrets
//!   in each line; each one's characters give its box.
//! - The desktop's [`Hint::Secret`]s are black whatever they show: a
//!   password field, a password manager's window.
//! - Its [`Hint::Text`]s are text it knows the place of, from the
//!   accessibility tree, which needs no reading and is never misread.
//!
//! A screenshot is redacted before it is encoded, so the secrets never
//! reach a file, an app or an agent. Without the OCR models only the hints
//! are used, and the [`Report`] says so.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod ocr;
pub mod patterns;

use std::collections::BTreeMap;

pub use ocr::Ocr;
pub use patterns::Kind;

/// A rectangle in the screenshot's pixels.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: u32,
    /// Top edge.
    pub y: u32,
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
}

impl Rect {
    /// The smallest rectangle holding both.
    pub fn union(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = (self.x + self.width).max(other.x + other.width);
        let bottom = (self.y + self.height).max(other.y + other.height);
        Self {
            x,
            y,
            width: right - x,
            height: bottom - y,
        }
    }

    /// Whether the two share any pixel.
    pub fn intersects(self, other: Self) -> bool {
        self.x < other.x + other.width
            && other.x < self.x + self.width
            && self.y < other.y + other.height
            && other.y < self.y + self.height
    }

    /// Grown by `dx` left and right and `dy` up and down, without going
    /// past zero.
    fn grown(self, dx: u32, dy: u32) -> Self {
        let x = self.x.saturating_sub(dx);
        let y = self.y.saturating_sub(dy);
        Self {
            x,
            y,
            width: self.x + self.width + dx - x,
            height: self.y + self.height + dy - y,
        }
    }

    fn area(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}

/// A screenshot: rows top to bottom, 4 bytes (RGBA) per pixel.
#[derive(Clone, Copy, Debug)]
pub struct Image<'a> {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// The pixels.
    pub rgba: &'a [u8],
}

/// What the desktop knows about the screen, in the screenshot's pixels.
#[derive(Clone, Debug, PartialEq)]
pub enum Hint {
    /// Blacked out whatever it shows.
    Secret {
        /// Where.
        rect: Rect,
        /// What it is, for the report.
        kind: Kind,
    },
    /// Text known to be at `rect`, such as an accessibility label or a
    /// field's value. If it holds a secret, OCR's box for it is used when
    /// OCR found one there; otherwise the whole rectangle is blacked out.
    Text {
        /// Where.
        rect: Rect,
        /// What it says.
        text: String,
    },
}

/// What was blacked out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    /// The boxes, with what each covers.
    pub boxes: Vec<(Rect, Kind)>,
    /// Why the screen's text could not be read, if it could not: only the
    /// hints were used then.
    pub unread: Option<String>,
}

impl Report {
    /// How many of each kind, in [`Kind::ALL`]'s order.
    pub fn counts(&self) -> Vec<(Kind, usize)> {
        let mut counts = BTreeMap::new();
        for (_, kind) in &self.boxes {
            *counts.entry(*kind).or_insert(0) += 1;
        }
        Kind::ALL
            .iter()
            .filter_map(|k| counts.get(k).map(|n| (*k, *n)))
            .collect()
    }

    /// "2 emails and 1 key", or `None` when nothing was blacked out.
    pub fn summary(&self) -> Option<String> {
        let parts: Vec<String> = self
            .counts()
            .into_iter()
            .map(|(kind, n)| match n {
                1 => format!("1 {}", kind.singular()),
                n => format!("{n} {}", kind.plural()),
            })
            .collect();
        match parts.as_slice() {
            [] => None,
            [one] => Some(one.clone()),
            [rest @ .., last] => Some(format!("{} and {last}", rest.join(", "))),
        }
    }
}

/// Text hints larger than this share of the screen (a document, a whole
/// window's text) are blacked out whole only when OCR found nothing in
/// them, rather than hiding everything around a secret.
const LARGE_HINT: u64 = 16;

/// Finds the secrets in `image` with `hints` and, unless `ocr` is an
/// error saying why there is none, the screen's text.
pub fn find(image: &Image<'_>, hints: &[Hint], ocr: Result<&Ocr, &str>) -> Report {
    let mut report = Report::default();
    let screen = Rect {
        x: 0,
        y: 0,
        width: image.width,
        height: image.height,
    };
    let lines = ocr.and_then(|ocr| {
        ocr.lines(image).map_err(|e| {
            tracing::warn!(error = %e, "cannot read the screenshot's text");
            "the screen's text could not be read"
        })
    });
    match &lines {
        Ok(lines) => {
            for line in lines {
                for found in patterns::find(&line.text) {
                    if let Some(rect) = line_box(line, &found.range) {
                        report.boxes.push((rect, found.kind));
                    }
                }
            }
        }
        Err(why) => report.unread = Some((*why).to_owned()),
    }
    let read = report.boxes.clone();
    for hint in hints {
        match hint {
            Hint::Secret { rect, kind } => report.boxes.push((*rect, *kind)),
            Hint::Text { rect, text } => {
                let Some(found) = patterns::find(text).into_iter().next() else {
                    continue;
                };
                let large = rect.area() * LARGE_HINT > screen.area();
                let covered = read.iter().any(|(r, _)| r.intersects(*rect));
                if !(large && covered) {
                    report.boxes.push((*rect, found.kind));
                }
            }
        }
    }
    report
        .boxes
        .retain(|(r, _)| r.intersects(screen) && r.area() > 0);
    report
}

/// Blacks out every box in `report`, clipped to the image.
pub fn black_out(width: u32, height: u32, rgba: &mut [u8], report: &Report) {
    for (rect, _) in &report.boxes {
        let right = (rect.x + rect.width).min(width) as usize;
        let bottom = (rect.y + rect.height).min(height) as usize;
        for y in rect.y as usize..bottom {
            let row = y * width as usize * 4;
            for px in rgba[row + rect.x as usize * 4..row + right * 4].chunks_exact_mut(4) {
                px.copy_from_slice(&[0, 0, 0, 255]);
            }
        }
    }
}

/// Finds the secrets and blacks them out: [`find`], then [`black_out`].
pub fn redact(
    width: u32,
    height: u32,
    rgba: &mut [u8],
    hints: &[Hint],
    ocr: Result<&Ocr, &str>,
) -> Report {
    let report = find(
        &Image {
            width,
            height,
            rgba,
        },
        hints,
        ocr,
    );
    black_out(width, height, rgba, &report);
    report
}

/// The box of the characters of `line` in the byte `range` of its text,
/// grown a little so no anti-aliased edge of a glyph shows past it: less
/// up and down, where a terminal's next line sits close.
fn line_box(line: &ocr::Line, range: &std::ops::Range<usize>) -> Option<Rect> {
    let rect = line
        .text
        .char_indices()
        .zip(&line.boxes)
        .filter(|((i, c), _)| range.contains(i) && !c.is_whitespace())
        .map(|(_, rect)| *rect)
        .reduce(Rect::union)?;
    Some(rect.grown((rect.height / 5).max(2), (rect.height / 10).max(1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u32, y: u32, width: u32, height: u32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn secret_hints_are_always_black() {
        let mut rgba = vec![200; 8 * 4 * 4];
        let report = redact(
            8,
            4,
            &mut rgba,
            &[Hint::Secret {
                rect: rect(2, 1, 3, 2),
                kind: Kind::Password,
            }],
            Err("no OCR in this test"),
        );
        assert_eq!(report.summary().as_deref(), Some("1 password"));
        assert_eq!(report.unread.as_deref(), Some("no OCR in this test"));
        for y in 0..4 {
            for x in 0..8 {
                let black = (2..5).contains(&x) && (1..3).contains(&y);
                let px = &rgba[(y * 8 + x) * 4..][..4];
                assert_eq!(px == [0, 0, 0, 255], black, "({x}, {y})");
            }
        }
    }

    #[test]
    fn text_hints_are_black_only_with_a_secret_in_them() {
        let image = Image {
            width: 100,
            height: 100,
            rgba: &[0; 100 * 100 * 4],
        };
        let hints = [
            Hint::Text {
                rect: rect(0, 0, 10, 5),
                text: "Signed in as someone@example.org".into(),
            },
            Hint::Text {
                rect: rect(0, 10, 10, 5),
                text: "Weather".into(),
            },
        ];
        let report = find(&image, &hints, Err("none"));
        assert_eq!(report.boxes, [(rect(0, 0, 10, 5), Kind::Email)]);
    }

    #[test]
    fn large_text_hints_defer_to_what_ocr_read_in_them() {
        let image = Image {
            width: 100,
            height: 100,
            rgba: &[0; 100 * 100 * 4],
        };
        let document = rect(0, 0, 100, 50);
        let hints = [Hint::Text {
            rect: document,
            text: "a long document with password=abc in it".into(),
        }];
        // Nothing was read there, so the whole document goes black.
        assert_eq!(
            find(&image, &hints, Err("none")).boxes,
            [(document, Kind::Password)]
        );
    }

    #[test]
    fn boxes_cover_just_the_secret_characters() {
        let text = "mail me@x.io now";
        let boxes = (0..text.len() as u32)
            .map(|i| rect(i * 10, 20, 10, 10))
            .collect();
        let line = ocr::Line {
            text: text.into(),
            boxes,
        };
        let found = patterns::find(&line.text);
        assert_eq!(found.len(), 1);
        // "me@x.io" is characters 5 to 11, grown by 2 left and right and
        // by 1 up and down.
        assert_eq!(line_box(&line, &found[0].range), Some(rect(48, 19, 74, 12)));
    }

    #[test]
    fn summaries() {
        let report = Report {
            boxes: vec![
                (rect(0, 0, 1, 1), Kind::Email),
                (rect(0, 0, 1, 1), Kind::Key),
                (rect(0, 0, 1, 1), Kind::Email),
                (rect(0, 0, 1, 1), Kind::Password),
            ],
            unread: None,
        };
        assert_eq!(
            report.summary().as_deref(),
            Some("1 password, 1 key and 2 emails")
        );
        assert_eq!(Report::default().summary(), None);
    }
}
