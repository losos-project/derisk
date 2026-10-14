//! Wallpaper thumbnails for the Settings app, made on worker threads.
//!
//! Pictures decode with the `image` crate; a video's thumbnail is one frame
//! from a second in, extracted by `ffmpeg` (the same `$DERISK_FFMPEG` the
//! session plays it with); a slideshow folder shows its first picture.

use std::{
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{Receiver, Sender, channel},
};

use mcsapi_ui::egui::{self, ColorImage, TextureHandle, TextureOptions};

use crate::pages::IMAGE_EXTENSIONS;

/// The longest side of a thumbnail, in pixels.
pub const THUMB_SIDE: u32 = 320;

enum Thumb {
    Loading,
    /// The texture and the source's size in pixels.
    Ready(TextureHandle, [usize; 2]),
    Failed,
}

type Made = (PathBuf, Option<(ColorImage, [usize; 2])>);

/// Thumbnails by path, decoded once each.
pub struct Thumbnails {
    cache: HashMap<PathBuf, Thumb>,
    tx: Sender<Made>,
    rx: Receiver<Made>,
}

impl std::fmt::Debug for Thumbnails {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Thumbnails")
            .field("cached", &self.cache.len())
            .finish()
    }
}

impl Default for Thumbnails {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            cache: HashMap::new(),
            tx,
            rx,
        }
    }
}

impl Thumbnails {
    /// The thumbnail of `path` and the source's size, if ready; starts
    /// making it otherwise.
    pub fn get(
        &mut self,
        ctx: &egui::Context,
        path: &Path,
    ) -> Option<(&TextureHandle, [usize; 2])> {
        while let Ok((done, made)) = self.rx.try_recv() {
            let thumb = match made {
                Some((image, size)) => Thumb::Ready(
                    ctx.load_texture(
                        format!("wallpaper-thumb-{}", done.display()),
                        image,
                        TextureOptions::LINEAR,
                    ),
                    size,
                ),
                None => Thumb::Failed,
            };
            self.cache.insert(done, thumb);
        }
        if !self.cache.contains_key(path) {
            self.cache.insert(path.to_owned(), Thumb::Loading);
            let (tx, path, ctx) = (self.tx.clone(), path.to_owned(), ctx.clone());
            derisk_sys::thread::detach(move || {
                let image = thumbnail(&path);
                let _ = tx.send((path, image));
                ctx.request_repaint();
            });
        }
        match self.cache.get(path) {
            Some(Thumb::Ready(texture, size)) => Some((texture, *size)),
            _ => None,
        }
    }

    /// Whether `path` could not be thumbnailed.
    pub fn failed(&self, path: &Path) -> bool {
        matches!(self.cache.get(path), Some(Thumb::Failed))
    }
}

fn is_picture(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// A thumbnail of a picture, a video, or a folder's first picture, with
/// the source's size (a video's is the thumbnail's own).
pub fn thumbnail(path: &Path) -> Option<(ColorImage, [usize; 2])> {
    if derisk_sys::fs::is_dir(path) {
        let mut pictures: Vec<PathBuf> = derisk_sys::fs::read_dir(path)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| is_picture(p))
            .collect();
        pictures.sort();
        return thumbnail(pictures.first()?);
    }
    let image = if is_picture(path) {
        // ImageReader::open, through derisk-sys.
        image::ImageReader::new(std::io::BufReader::new(
            derisk_sys::fs::File::open(path).ok()?,
        ))
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?
    } else {
        image::load_from_memory_with_format(&video_frame(path)?, image::ImageFormat::Png).ok()?
    };
    let size = [image.width() as usize, image.height() as usize];
    let small = image.thumbnail(THUMB_SIDE, THUMB_SIDE).into_rgba8();
    Some((
        ColorImage::from_rgba_unmultiplied(
            [small.width() as usize, small.height() as usize],
            small.as_raw(),
        ),
        size,
    ))
}

/// One PNG frame of a video, from a second in (or the first frame of a
/// shorter one).
fn video_frame(path: &Path) -> Option<Vec<u8>> {
    let program = derisk_sys::env::var_os("DERISK_FFMPEG").unwrap_or_else(|| "ffmpeg".into());
    for seek in ["1", "0"] {
        let mut child = Command::new(&program)
            .args(["-nostdin", "-loglevel", "error", "-ss", seek, "-i"])
            .arg(path)
            .args([
                "-frames:v",
                "1",
                "-vf",
                &format!("scale={THUMB_SIDE}:-2"),
                "-f",
                "image2pipe",
                "-vcodec",
                "png",
                "pipe:1",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut png = Vec::new();
        child.stdout.take()?.read_to_end(&mut png).ok()?;
        let _ = child.wait();
        if !png.is_empty() {
            return Some(png);
        }
    }
    None
}
