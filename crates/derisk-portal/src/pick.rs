//! Picking from the screen: the Screenshot portal's color, and the area of
//! an interactive screenshot.
//!
//! This service has no windows, so the picking is `derisk-gpui pick`, run
//! on a screenshot the session took ([`crate::agent`]). It shows the
//! screenshot frozen across the screen and prints what was picked, in the
//! screenshot's pixels: `x y` for a color, `x y width height` for an area.
//! This side reads the pixel or cuts the area out with the `image` crate.

use std::{io, path::Path};

/// What `derisk-gpui pick` printed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Picked {
    /// A pixel: column, row.
    Point(u32, u32),
    /// A rectangle: left, top, width, height.
    Area(u32, u32, u32, u32),
}

/// Reads the line `derisk-gpui pick` prints.
pub fn parse(line: &str) -> Option<Picked> {
    let numbers: Vec<u32> = line
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    match numbers[..] {
        [x, y] => Some(Picked::Point(x, y)),
        [x, y, w, h] if w > 0 && h > 0 => Some(Picked::Area(x, y, w, h)),
        _ => None,
    }
}

/// The `derisk-gpui` arguments that pick `mode` (`color` or `area`) from
/// `image` for `app`.
pub fn args(mode: &str, image: &Path, app: &str) -> Vec<String> {
    let mut args = vec![
        "pick".to_owned(),
        mode.to_owned(),
        "--image".to_owned(),
        image.to_string_lossy().into_owned(),
    ];
    if !app.is_empty() {
        args.extend(["--app".to_owned(), app.to_owned()]);
    }
    args
}

fn invalid(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

/// The color of the pixel at `x`, `y` in the picture at `png`, as the
/// portal answers it: red, green and blue from 0 to 1.
pub fn color_at(png: &Path, x: u32, y: u32) -> io::Result<[f64; 3]> {
    let picture = image::open(png).map_err(invalid)?.to_rgb8();
    let pixel = picture
        .get_pixel_checked(x, y)
        .ok_or_else(|| invalid(format!("{x},{y} is outside the screenshot")))?;
    Ok(pixel.0.map(|c| f64::from(c) / 255.0))
}

/// Writes the area of the picture at `png` to `out`, as a PNG. An area
/// reaching past the picture's edge is cut at it.
pub fn crop(png: &Path, area: (u32, u32, u32, u32), out: &Path) -> io::Result<()> {
    let picture = image::open(png).map_err(invalid)?;
    let (x, y, w, h) = area;
    if x >= picture.width() || y >= picture.height() {
        return Err(invalid("the area is outside the screenshot"));
    }
    let w = w.min(picture.width() - x);
    let h = h.min(picture.height() - y);
    picture
        .crop_imm(x, y, w, h)
        .save_with_format(out, image::ImageFormat::Png)
        .map_err(invalid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_the_picker_prints() {
        assert_eq!(parse("3 4\n"), Some(Picked::Point(3, 4)));
        assert_eq!(parse("1 2 30 40"), Some(Picked::Area(1, 2, 30, 40)));
        assert_eq!(parse("1 2 0 40"), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("x y"), None);
    }

    #[test]
    fn asks_the_picker() {
        assert_eq!(
            args("area", Path::new("/run/s.png"), "Krita"),
            ["pick", "area", "--image", "/run/s.png", "--app", "Krita"]
        );
        assert_eq!(args("color", Path::new("s.png"), "").len(), 4);
    }

    #[test]
    fn reads_and_cuts_the_screenshot() {
        let dir = std::env::temp_dir().join(format!("derisk-portal-pick-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let shot = dir.join("shot.png");
        let picture =
            image::RgbImage::from_fn(8, 6, |x, y| image::Rgb([x as u8 * 30, y as u8 * 40, 255]));
        picture.save(&shot).unwrap();

        assert_eq!(color_at(&shot, 0, 0).unwrap(), [0.0, 0.0, 1.0]);
        let [r, g, _] = color_at(&shot, 2, 5).unwrap();
        assert!((r - 60.0 / 255.0).abs() < 1e-9 && (g - 200.0 / 255.0).abs() < 1e-9);
        assert!(color_at(&shot, 8, 0).is_err());

        let out = dir.join("area.png");
        crop(&shot, (6, 4, 10, 10), &out).unwrap();
        let area = image::open(&out).unwrap().to_rgb8();
        assert_eq!(area.dimensions(), (2, 2));
        assert_eq!(area.get_pixel(0, 0).0, [180, 160, 255]);
        assert!(crop(&shot, (8, 0, 1, 1), &out).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
