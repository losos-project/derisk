//! The desktop background: derisk's gradient, a color, a gradient, a
//! picture, a slideshow, a looping video, or a web page.
//!
//! [`WallpaperPainter`] follows the Settings app's [`Wallpaper`] and paints
//! into the compositor's background pass. Pictures decode on a worker
//! thread with the `image` crate (PNG, JPEG, WebP), scaled down to what the
//! screen can show, and become one egui texture. Videos decode in an
//! `ffmpeg` child process (`$DERISK_FFMPEG`, else `ffmpeg` on `PATH`) that
//! writes raw RGBA frames already scaled and cropped to the screen; each
//! frame replaces the texture's pixels. Linking libav or GStreamer instead
//! would pull a large C dependency into the shell for what a pipe does.
//!
//! A video pauses by not reading: the pipe fills, ffmpeg blocks on its
//! write, and both sit idle until the wallpaper is visible again.
//!
//! A web page is drawn by Danube, the LosOS browser, in the same way:
//! `danube --background URL --size WxH --scale S` (`$DERISK_DANUBE`, else
//! `danube` on `PATH`) writes each frame the page renders to stdout as a
//! width and a height (u32, little-endian) and premultiplied RGBA pixels.
//! It has no window of its own and takes no input. A page is not paused
//! by not reading, since it would go on rendering frames nobody takes:
//! derisk writes `hide` on its stdin instead, and Danube stops painting
//! it and pauses its animations and media until `show`.
//!
//! Anything that fails to load falls back to derisk's gradient, logged
//! once.

use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{Receiver, TryRecvError, sync_channel},
    time::{Duration, Instant},
};

use derisk_settings::{Fit, IMAGE_EXTENSIONS, Rgb, Wallpaper, WallpaperKind};
use egui::{Color32, ColorImage, Painter, Rect, TextureHandle, TextureOptions, pos2};
use mcsapi::{Geometry, toolkit::egui, widgets::Theme};

use crate::ui::paint_wallpaper;

/// Frames per second a video wallpaper is decoded at. Motion behind
/// windows doesn't need more, and it halves the uploads on a 60 Hz screen.
pub const VIDEO_FPS: u32 = 30;

/// The most pixels a video frame has; bigger screens stretch it. 1080p
/// RGBA at 30 fps is already about 250 MB/s of texture upload.
const VIDEO_MAX_PIXELS: u32 = 1920 * 1080;

/// The most pixels a web page frame has; a bigger screen renders the page
/// at a lower scale and stretches it. 1440p RGBA at 60 fps is already about
/// 900 MB/s through the pipe.
const WEB_MAX_PIXELS: f32 = 2560.0 * 1440.0;

/// How long slideshow pictures cross-fade.
const FADE: Duration = Duration::from_millis(800);

/// Whether the wallpaper is worth animating this frame.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Visibility {
    /// A window covers the whole work area.
    pub covered: bool,
    /// Low power mode is on.
    pub low_power: bool,
    /// Motion is reduced (no slideshow cross-fades).
    pub reduce_motion: bool,
}

/// Whether one window covers all of `area`, so nothing of the desktop
/// background shows. Tiled windows with gaps between them never do.
pub fn covered(area: Geometry, frames: impl IntoIterator<Item = Geometry>) -> bool {
    frames.into_iter().any(|f| {
        f.loc.x <= area.loc.x
            && f.loc.y <= area.loc.y
            && f.loc.x + f.size.w >= area.loc.x + area.size.w
            && f.loc.y + f.size.h >= area.loc.y + area.size.h
    })
}

/// Where a `size` picture lands on `screen`; see [`Fit::place`].
pub fn placement(fit: Fit, size: [usize; 2], screen: Rect) -> (Rect, Rect) {
    fit.place(size, screen)
}

/// The size to decode a `size` picture at for `fit` on a `screen`-pixel
/// screen: never larger than shown, nor than `max_side`.
pub fn decode_size(fit: Fit, size: [u32; 2], screen: [u32; 2], max_side: u32) -> [u32; 2] {
    let (w, h) = (size[0].max(1) as f32, size[1].max(1) as f32);
    let (sw, sh) = (screen[0].max(1) as f32, screen[1].max(1) as f32);
    let shown = match fit {
        Fit::Fill => (sw / w).max(sh / h),
        Fit::Fit => (sw / w).min(sh / h),
        // Stretch distorts anyway, so match the larger screen ratio.
        Fit::Stretch => (sw / w).max(sh / h),
        Fit::Center | Fit::Tile => 1.0,
    };
    let limit = max_side as f32 / w.max(h);
    let scale = shown.min(limit).min(1.0);
    [
        ((w * scale).round() as u32).max(1),
        ((h * scale).round() as u32).max(1),
    ]
}

/// The pictures directly inside `dir`, by name.
pub fn slideshow_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                        IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str())
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

/// `0..n` in a fixed pseudo-random order for `seed` (xorshift Fisher–Yates;
/// a slideshow needs variety, not cryptographic randomness).
pub fn shuffled(n: usize, seed: u64) -> Vec<usize> {
    let mut order: Vec<usize> = (0..n).collect();
    let mut x = seed | 1;
    for i in (1..n).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        order.swap(i, (x % (i as u64 + 1)) as usize);
    }
    order
}

/// The ffmpeg arguments that decode `path` in a loop as raw RGBA frames of
/// `size`, fitted per `fit` with `bars` around a fitted video.
pub fn ffmpeg_args(
    path: &Path,
    size: [u32; 2],
    fit: Fit,
    bars: Rgb,
) -> impl Iterator<Item = String> + use<> {
    let [w, h] = size;
    let scale = match fit {
        Fit::Stretch => format!("scale={w}:{h}"),
        Fit::Fit => format!(
            "scale={w}:{h}:force_original_aspect_ratio=decrease,\
             pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=0x{:02x}{:02x}{:02x}",
            bars.0, bars.1, bars.2
        ),
        // A video tile or a small centered video isn't worth its own
        // compositing; both fill.
        Fit::Fill | Fit::Center | Fit::Tile => {
            format!("scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h}")
        }
    };
    ["-nostdin", "-loglevel", "error", "-stream_loop", "-1", "-i"]
        .into_iter()
        .map(str::to_owned)
        .chain([path.display().to_string()])
        .chain(
            [
                "-an",
                "-sn",
                "-vf",
                &format!("fps={VIDEO_FPS},{scale},setsar=1"),
                "-pix_fmt",
                "rgba",
                "-f",
                "rawvideo",
                "pipe:1",
            ]
            .map(str::to_owned),
        )
}

/// The Danube arguments that draw `url` on a `size` (logical pixels)
/// screen at `scale`, the scale lowered so that a frame has at most
/// [`WEB_MAX_PIXELS`].
pub fn danube_args(url: &str, size: [u32; 2], scale: f32) -> impl Iterator<Item = String> + use<> {
    let [w, h] = size.map(|v| v.max(1));
    let most = (WEB_MAX_PIXELS / (w as f32 * h as f32)).sqrt();
    let scale = scale.min(most).clamp(0.5, 8.0);
    [
        "--background".to_owned(),
        url.to_owned(),
        "--size".to_owned(),
        format!("{w}x{h}"),
        "--scale".to_owned(),
        format!("{scale:.3}"),
    ]
    .into_iter()
}

/// One frame of Danube's stream: its size, then its pixels.
fn read_frame(stream: &mut impl Read) -> std::io::Result<([usize; 2], Vec<u8>)> {
    let mut header = [0; 8];
    stream.read_exact(&mut header)?;
    let w = u32::from_le_bytes(header[..4].try_into().expect("four bytes")) as usize;
    let h = u32::from_le_bytes(header[4..].try_into().expect("four bytes")) as usize;
    // Danube's sizes are a screen's: anything larger is a broken stream.
    if w == 0 || h == 0 || w > 16384 || h > 16384 {
        return Err(std::io::Error::other(format!("a {w}x{h} frame")));
    }
    let mut pixels = vec![0; w * h * 4];
    stream.read_exact(&mut pixels)?;
    Ok(([w, h], pixels))
}

/// The video frame size for a `screen`-pixel screen: its shape, at most
/// [`VIDEO_MAX_PIXELS`], even sides.
pub fn video_size(screen: [u32; 2]) -> [u32; 2] {
    let (w, h) = (screen[0].max(2) as f32, screen[1].max(2) as f32);
    let scale = (VIDEO_MAX_PIXELS as f32 / (w * h)).sqrt().min(1.0);
    let even = |v: f32| ((v * scale) as u32 / 2 * 2).max(2);
    [even(w), even(h)]
}

type Decoded = Result<ColorImage, String>;

/// Decodes a picture on a worker thread.
fn load_picture(path: PathBuf, fit: Fit, screen: [u32; 2], max_side: u32) -> Receiver<Decoded> {
    let (tx, rx) = sync_channel(1);
    std::thread::spawn(move || {
        let decoded = (|| {
            let image = image::ImageReader::open(&path)
                .map_err(|e| e.to_string())?
                .with_guessed_format()
                .map_err(|e| e.to_string())?
                .decode()
                .map_err(|e| e.to_string())?;
            let [w, h] = decode_size(fit, [image.width(), image.height()], screen, max_side);
            let image = if [w, h] == [image.width(), image.height()] {
                image.into_rgba8()
            } else {
                image
                    .resize_exact(w, h, image::imageops::FilterType::Triangle)
                    .into_rgba8()
            };
            Ok(ColorImage::from_rgba_unmultiplied(
                [w as usize, h as usize],
                image.as_raw(),
            ))
        })();
        let _ = tx.send(decoded.map_err(|e: String| format!("{}: {e}", path.display())));
    });
    rx
}

/// The texture options for `fit`: tiles repeat.
fn texture_options(fit: Fit) -> TextureOptions {
    if fit == Fit::Tile {
        TextureOptions::LINEAR_REPEAT
    } else {
        TextureOptions::LINEAR
    }
}

/// A picture being decoded or shown.
#[derive(Default)]
struct Picture {
    pending: Option<Receiver<Decoded>>,
    texture: Option<TextureHandle>,
    /// The picture before, fading out under this one.
    previous: Option<TextureHandle>,
    shown_at: Option<Instant>,
}

impl Picture {
    /// Takes a finished decode; returns an error to log.
    fn poll(&mut self, painter: &Painter, fit: Fit) -> Option<String> {
        if self.shown_at.is_some_and(|at| at.elapsed() >= FADE) {
            self.previous = None;
        }
        let rx = self.pending.as_ref()?;
        let decoded = match rx.try_recv() {
            Ok(decoded) => decoded,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("the decoder stopped".to_owned()),
        };
        self.pending = None;
        match decoded {
            Ok(image) => {
                let texture =
                    painter
                        .ctx()
                        .load_texture("derisk-wallpaper", image, texture_options(fit));
                self.previous = self.texture.replace(texture);
                self.shown_at = Some(Instant::now());
                None
            }
            Err(e) => Some(e),
        }
    }
}

/// A looping video decoded by ffmpeg.
struct Video {
    child: Child,
    frames: Receiver<Vec<u8>>,
    errors: Receiver<String>,
    size: [usize; 2],
    texture: Option<TextureHandle>,
    next_at: Option<Instant>,
}

impl Video {
    fn spawn(path: &Path, size: [u32; 2], fit: Fit, bars: Rgb) -> Result<Self, String> {
        let program = std::env::var_os("DERISK_FFMPEG").unwrap_or_else(|| "ffmpeg".into());
        let mut child = Command::new(&program)
            .args(ffmpeg_args(path, size, fit, bars))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{}: {e}", program.to_string_lossy()))?;
        let mut stdout = child.stdout.take().expect("piped stdout");
        let mut stderr = child.stderr.take().expect("piped stderr");
        let frame_len = size[0] as usize * size[1] as usize * 4;
        // One frame in flight: when the painter stops taking frames, the
        // reader blocks here and ffmpeg blocks on the full pipe.
        let (tx, frames) = sync_channel(1);
        std::thread::spawn(move || {
            loop {
                let mut frame = vec![0; frame_len];
                if stdout.read_exact(&mut frame).is_err() || tx.send(frame).is_err() {
                    return;
                }
            }
        });
        let (etx, errors) = sync_channel(1);
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            let _ = etx.send(text);
        });
        Ok(Self {
            child,
            frames,
            errors,
            size: [size[0] as usize, size[1] as usize],
            texture: None,
            next_at: None,
        })
    }

    /// Shows the next frame when it is due. `paused` holds the current one
    /// (but still takes the first, so a paused video isn't blank). Returns
    /// an error once ffmpeg has quit.
    fn advance(&mut self, painter: &Painter, paused: bool) -> Option<String> {
        let now = Instant::now();
        let interval = Duration::from_secs(1) / VIDEO_FPS;
        if paused && self.texture.is_some() {
            self.next_at = None;
            return None;
        }
        if self.next_at.is_some_and(|at| now < at) {
            painter.ctx().request_repaint_after(self.next_at? - now);
            return None;
        }
        match self.frames.try_recv() {
            Ok(rgba) => {
                // Decoded opaque, so premultiplying is a no-op.
                let image = ColorImage::from_rgba_premultiplied(self.size, &rgba);
                match &mut self.texture {
                    Some(texture) => texture.set(image, TextureOptions::LINEAR),
                    None => {
                        self.texture = Some(painter.ctx().load_texture(
                            "derisk-wallpaper-video",
                            image,
                            TextureOptions::LINEAR,
                        ));
                    }
                }
                // Keep a steady cadence; after a stall, restart it from now
                // instead of rushing through the backlog.
                let next = self.next_at.map_or(now, |at| at + interval);
                self.next_at = Some(if next + interval < now {
                    now + interval
                } else {
                    next
                });
                None
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(
                self.errors
                    .recv_timeout(Duration::from_millis(200))
                    .ok()
                    .map(|e| e.trim().to_owned())
                    .filter(|e| !e.is_empty())
                    .unwrap_or_else(|| "ffmpeg stopped".to_owned()),
            ),
        }
    }
}

impl Drop for Video {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A web page drawn by Danube.
struct Page {
    child: Child,
    stdin: ChildStdin,
    frames: Receiver<([usize; 2], Vec<u8>)>,
    errors: Receiver<String>,
    texture: Option<TextureHandle>,
    hidden: bool,
}

impl Page {
    fn spawn(url: &str, size: [u32; 2], scale: f32, ctx: &egui::Context) -> Result<Self, String> {
        let program = std::env::var_os("DERISK_DANUBE").unwrap_or_else(|| "danube".into());
        let mut child = Command::new(&program)
            .args(danube_args(url, size, scale))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{}: {e}", program.to_string_lossy()))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let mut stdout = child.stdout.take().expect("piped stdout");
        let mut stderr = child.stderr.take().expect("piped stderr");
        // One frame in flight, as for a video; Danube replaces the frames
        // nobody took yet with newer ones.
        let (tx, frames) = sync_channel(1);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            while let Ok(frame) = read_frame(&mut stdout) {
                if tx.send(frame).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        });
        let (etx, errors) = sync_channel(1);
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            let _ = etx.send(text);
        });
        Ok(Self {
            child,
            stdin,
            frames,
            errors,
            texture: None,
            hidden: false,
        })
    }

    /// Takes the newest frame, and tells Danube when the page is hidden or
    /// shown. Returns an error once Danube has quit.
    fn advance(&mut self, painter: &Painter, hidden: bool) -> Option<String> {
        if hidden != self.hidden {
            self.hidden = hidden;
            let line: &[u8] = if hidden { b"hide\n" } else { b"show\n" };
            // A Danube that has quit shows up below.
            let _ = self.stdin.write_all(line);
        }
        match self.frames.try_recv() {
            Ok((size, rgba)) => {
                let image = ColorImage::from_rgba_premultiplied(size, &rgba);
                match &mut self.texture {
                    Some(texture) => texture.set(image, TextureOptions::LINEAR),
                    None => {
                        self.texture = Some(painter.ctx().load_texture(
                            "derisk-wallpaper-web",
                            image,
                            TextureOptions::LINEAR,
                        ));
                    }
                }
                None
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(
                self.errors
                    .recv_timeout(Duration::from_millis(200))
                    .ok()
                    .map(|e| last_line(&e))
                    .filter(|e| !e.is_empty())
                    .unwrap_or_else(|| "danube stopped".to_owned()),
            ),
        }
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The last line with something in it: Danube logs as it goes, and says
/// why it stopped last.
fn last_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or_default()
        .to_owned()
}

enum Source {
    Plain,
    Picture(Picture),
    Slideshow {
        files: Vec<PathBuf>,
        order: Vec<usize>,
        index: usize,
        next_change: Instant,
        picture: Picture,
    },
    Video(Video),
    Web(Page),
    /// Loading failed; the default shows until the settings change.
    Failed,
}

/// What the lock screen shows: its own picture, filling the screen, or
/// derisk's gradient when none is set. It is a second painter beside the
/// desktop's, so locking never reloads the desktop's video or slideshow.
pub fn lock_screen(c: &Wallpaper) -> Wallpaper {
    if c.lock_screen.as_os_str().is_empty() {
        return Wallpaper::default();
    }
    Wallpaper {
        kind: WallpaperKind::Image,
        path: c.lock_screen.clone(),
        fit: Fit::Fill,
        ..Wallpaper::default()
    }
}

/// Paints the configured wallpaper.
pub struct WallpaperPainter {
    config: Wallpaper,
    /// Screen size in pixels the source was loaded for.
    loaded_for: Option<[u32; 2]>,
    source: Source,
}

impl Default for WallpaperPainter {
    fn default() -> Self {
        Self {
            config: Wallpaper::default(),
            loaded_for: None,
            source: Source::Plain,
        }
    }
}

impl WallpaperPainter {
    /// Follows new settings; reloads only when the wallpaper changed.
    pub fn configure(&mut self, config: &Wallpaper) {
        if *config != self.config {
            self.config = config.clone();
            self.loaded_for = None;
        }
    }

    /// The wallpaper being shown.
    pub fn config(&self) -> &Wallpaper {
        &self.config
    }

    /// Whether a video is playing (not paused, not failed).
    pub fn is_playing(&self) -> bool {
        matches!(&self.source, Source::Video(v) if v.next_at.is_some())
    }

    fn fail(&mut self, error: &str) {
        tracing::warn!("wallpaper: {error}");
        self.source = Source::Failed;
    }

    fn load(&mut self, painter: &Painter, screen: [u32; 2]) {
        let c = &self.config;
        let max_side = painter.ctx().input(|i| i.max_texture_side) as u32;
        let missing = c.path.as_os_str().is_empty() || !c.path.exists();
        self.source = match c.kind {
            WallpaperKind::Default | WallpaperKind::Color | WallpaperKind::Gradient => {
                Source::Plain
            }
            WallpaperKind::Web if c.url.is_empty() => return self.fail("no address given"),
            WallpaperKind::Web => {
                let ppp = painter.ctx().pixels_per_point();
                let points = screen.map(|p| (p as f32 / ppp).round() as u32);
                match Page::spawn(&c.url, points, ppp, painter.ctx()) {
                    Ok(page) => Source::Web(page),
                    Err(e) => return self.fail(&e),
                }
            }
            _ if missing => {
                let error = if c.path.as_os_str().is_empty() {
                    "no file chosen".to_owned()
                } else {
                    format!("{} does not exist", c.path.display())
                };
                return self.fail(&error);
            }
            WallpaperKind::Image => Source::Picture(Picture {
                pending: Some(load_picture(c.path.clone(), c.fit, screen, max_side)),
                ..Picture::default()
            }),
            WallpaperKind::Slideshow => {
                let files = slideshow_files(&c.path);
                if files.is_empty() {
                    return self.fail(&format!("no pictures in {}", c.path.display()));
                }
                let order = if c.shuffle {
                    let seed = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(1, |d| d.as_nanos() as u64);
                    shuffled(files.len(), seed)
                } else {
                    (0..files.len()).collect()
                };
                let first = files[order[0]].clone();
                Source::Slideshow {
                    files,
                    order,
                    index: 0,
                    next_change: Instant::now() + interval(c),
                    picture: Picture {
                        pending: Some(load_picture(first, c.fit, screen, max_side)),
                        ..Picture::default()
                    },
                }
            }
            WallpaperKind::Video => {
                match Video::spawn(&c.path, video_size(screen), c.fit, c.color) {
                    Ok(video) => Source::Video(video),
                    Err(e) => return self.fail(&e),
                }
            }
        };
    }

    /// Paints the wallpaper over `screen`.
    pub fn paint(&mut self, painter: &Painter, screen: Rect, theme: &Theme, seen: Visibility) {
        let ppp = painter.ctx().pixels_per_point();
        let pixels = [
            (screen.width() * ppp).round() as u32,
            (screen.height() * ppp).round() as u32,
        ];
        if self.loaded_for != Some(pixels) {
            self.loaded_for = Some(pixels);
            self.load(painter, pixels);
        }
        let c = self.config.clone();
        let fallback = |painter: &Painter| match c.kind {
            WallpaperKind::Color => {
                painter.rect_filled(screen, 0, c.color.color());
            }
            WallpaperKind::Gradient => gradient(painter, screen, c.color.color(), c.color2.color()),
            _ => paint_wallpaper(painter, screen, theme),
        };
        let error = match &mut self.source {
            Source::Plain | Source::Failed => {
                fallback(painter);
                None
            }
            Source::Picture(picture) => {
                let error = picture.poll(painter, c.fit);
                draw_picture(painter, screen, picture, &c, seen.reduce_motion, &fallback);
                error
            }
            Source::Slideshow {
                files,
                order,
                index,
                next_change,
                picture,
            } => {
                if Instant::now() >= *next_change && picture.pending.is_none() {
                    *index = (*index + 1) % files.len();
                    *next_change = Instant::now() + interval(&c);
                    let max_side = painter.ctx().input(|i| i.max_texture_side) as u32;
                    let next = files[order[*index]].clone();
                    picture.pending = Some(load_picture(next, c.fit, pixels, max_side));
                }
                // A picture that fails to decode is skipped, not fatal.
                let error = picture.poll(painter, c.fit);
                draw_picture(painter, screen, picture, &c, seen.reduce_motion, &fallback);
                painter
                    .ctx()
                    .request_repaint_after(next_change.saturating_duration_since(Instant::now()));
                if let Some(e) = error {
                    tracing::warn!("wallpaper: {e}");
                }
                None
            }
            Source::Web(page) => {
                let hidden = (seen.covered && c.pause_when_covered)
                    || (seen.low_power && c.pause_in_low_power);
                let error = page.advance(painter, hidden);
                match &page.texture {
                    // The page was laid out for this screen; a frame of a
                    // lowered scale stretches over it.
                    Some(texture) => {
                        let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                        painter.image(texture.id(), screen, full, Color32::WHITE);
                    }
                    // Loading: the default instead of a black flash.
                    None => fallback(painter),
                }
                error
            }
            Source::Video(video) => {
                let paused = (seen.covered && c.pause_when_covered)
                    || (seen.low_power && c.pause_in_low_power);
                let error = video.advance(painter, paused);
                match &video.texture {
                    Some(texture) => {
                        let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                        painter.image(texture.id(), screen, full, Color32::WHITE);
                    }
                    None => fallback(painter),
                }
                error
            }
        };
        if let Some(e) = error {
            self.fail(&e);
        }
    }
}

fn interval(c: &Wallpaper) -> Duration {
    Duration::from_secs(u64::from(c.interval_min.max(1)) * 60)
}

fn draw_picture(
    painter: &Painter,
    screen: Rect,
    picture: &Picture,
    c: &Wallpaper,
    reduce_motion: bool,
    fallback: &dyn Fn(&Painter),
) {
    let Some(texture) = &picture.texture else {
        // Still decoding: the default instead of a black flash.
        return fallback(painter);
    };
    if c.fit != Fit::Fill && c.fit != Fit::Stretch && c.fit != Fit::Tile {
        painter.rect_filled(screen, 0, c.color.color());
    }
    let fade = picture
        .shown_at
        .map_or(1.0, |at| at.elapsed().as_secs_f32() / FADE.as_secs_f32())
        .min(1.0);
    let fading = picture.previous.is_some() && fade < 1.0 && !reduce_motion;
    if fading && let Some(previous) = &picture.previous {
        let (rect, uv) = placement(c.fit, previous.size(), screen);
        painter.image(previous.id(), rect, uv, Color32::WHITE);
        painter.ctx().request_repaint();
    }
    let (rect, uv) = placement(c.fit, texture.size(), screen);
    let alpha = if fading { fade } else { 1.0 };
    painter.image(texture.id(), rect, uv, Color32::WHITE.gamma_multiply(alpha));
}

/// A top-to-bottom gradient.
fn gradient(painter: &Painter, screen: Rect, top: Color32, bottom: Color32) {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(screen.left_top(), top);
    mesh.colored_vertex(screen.right_top(), top);
    mesh.colored_vertex(screen.left_bottom(), bottom);
    mesh.colored_vertex(screen.right_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}
