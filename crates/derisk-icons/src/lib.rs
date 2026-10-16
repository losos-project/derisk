//! Icons for the shell and the core apps, as egui images, from the icon
//! theme the person has set.
//!
//! Two kinds. App icons ([`find`], [`load`]) keep their colors. Everything
//! else, the buttons, places, file types and status the chrome and apps
//! show, is a symbolic icon named by its freedesktop name and drawn as a
//! greyscale mask in the theme's text color ([`paint`], [`button`],
//! [`show`]), the way GNOME and KDE draw symbolic icons. When no theme has
//! one it falls back to a glyph from egui's built-in fonts ([`glyph`]), so
//! every icon still shows something.
//!
//! Both follow the [icon theme spec]'s lookup without its size tables: the
//! theme set with [`set_theme`] (the session passes the one its theme file
//! names), then the themes it `Inherits`, then Papirus, which is the default
//! and fills any symbolic icon the set theme lacks, then `hicolor`, where
//! apps install their own icons. App icons then try every other installed
//! theme, where generic names such as `system-file-manager` live, and
//! `pixmaps`.
//!
//! [icon theme spec]: https://specifications.freedesktop.org/icon-theme-spec/latest/

use std::{
    path::{Path, PathBuf},
    sync::{
        RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

use derisk_sys::fs;
use mcsapi_ui::egui::{
    self, Align2, Atom, Color32, ColorImage, Context, FontId, Id, Painter, Pos2, Rect, Response,
    RichText, Sense, TextureHandle, TextureOptions, Ui, WidgetInfo, WidgetText, WidgetType, pos2,
    vec2,
};

/// Icons bigger than this are not decoded, so a stray wallpaper-sized file
/// named like an icon cannot stall a frame.
const MAX_BYTES: u64 = 4 << 20;

/// `icons` and `pixmaps` roots in XDG precedence order.
fn roots() -> (Vec<PathBuf>, Vec<PathBuf>) {
    let home = derisk_sys::env::var_os("HOME").map(PathBuf::from);
    let data_home = derisk_sys::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".local/share")));
    let data_dirs = derisk_sys::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());
    let data: Vec<PathBuf> = data_home
        .into_iter()
        .chain(
            data_dirs
                .split(':')
                .filter(|d| !d.is_empty())
                .map(PathBuf::from),
        )
        .collect();
    let icons = home
        .map(|h| h.join(".icons"))
        .into_iter()
        .chain(data.iter().map(|d| d.join("icons")))
        .collect();
    let pixmaps = data.iter().map(|d| d.join("pixmaps")).collect();
    (icons, pixmaps)
}

/// The nominal size of a theme directory such as `48x48`, `48x48@2` or
/// `48`, or `None` for `scalable` and anything else.
fn dir_size(name: &str) -> Option<u32> {
    let name = name.split('@').next().unwrap_or(name);
    name.split('x').next()?.parse().ok()
}

/// How well a file fits `want` pixels; lower is better. A PNG at least as
/// big scales down cleanly, an SVG renders at any size, and a smaller PNG
/// would be blurry, so it is the last resort.
fn fit(path: &Path, size: Option<u32>, want: u32) -> (u8, u32) {
    match (path.extension().and_then(|e| e.to_str()), size) {
        (Some("svg"), _) => (1, 0),
        (_, Some(n)) if n >= want => (0, n - want),
        (_, Some(n)) => (2, want - n),
        (_, None) => (3, 0),
    }
}

/// Every `<theme>/<a>/<b>/<name>.{png,svg}` under `theme`, which covers both
/// `48x48/apps` and `apps/48` layouts, scored by [`fit`]. With `small`, only
/// directories for 24 px or less count, closest to 16 px first.
fn in_theme(
    theme: &Path,
    name: &str,
    want: u32,
    small: bool,
    best: &mut Option<((u8, u32), PathBuf)>,
) {
    let Ok(outer) = fs::read_dir(theme) else {
        return;
    };
    for a in outer.filter_map(Result::ok) {
        let Ok(inner) = fs::read_dir(a.path()) else {
            continue;
        };
        let a_size = dir_size(&a.file_name().to_string_lossy());
        for b in inner.filter_map(Result::ok) {
            let size = a_size.or_else(|| dir_size(&b.file_name().to_string_lossy()));
            for ext in ["png", "svg"] {
                let path = b.path().join(format!("{name}.{ext}"));
                if !fs::is_file(&path) {
                    continue;
                }
                let score = match (small, size) {
                    (false, _) => fit(&path, size, want),
                    (true, Some(n)) if n <= 24 => (0, n.abs_diff(16)),
                    (true, _) => continue,
                };
                if best.as_ref().is_none_or(|(s, _)| score < *s) {
                    *best = Some((score, path));
                }
            }
        }
    }
}

/// The icon theme set with [`set_theme`]; empty until then.
static THEME: RwLock<String> = RwLock::new(String::new());

/// Bumped by every [`set_theme`] that changes the theme, so caches keyed by
/// it drop icons from the old one.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Sets the icon theme, by its directory name (`Papirus-Dark`, `Adwaita`),
/// that every lookup starts from.
pub fn set_theme(name: &str) {
    let mut theme = THEME.write().unwrap_or_else(|e| e.into_inner());
    if *theme != name {
        *theme = name.to_owned();
        GENERATION.fetch_add(1, Ordering::Relaxed);
    }
}

/// The icon theme set with [`set_theme`].
pub fn current_theme() -> String {
    THEME.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Changes whenever the icon theme does. [`texture`] keys on it; a cache
/// of [`find`] results should too.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// Papirus variants, dark first because the shell's default theme is dark.
/// Each is tinted at paint time, so the variant only matters for shapes.
const PAPIRUS: [&str; 3] = ["Papirus-Dark", "Papirus", "Papirus-Light"];

/// The `Inherits` list of `theme`'s `index.theme`, from the first root
/// that has one.
fn inherits(theme: &str, icons: &[PathBuf]) -> Vec<String> {
    let Some(text) = icons
        .iter()
        .find_map(|root| fs::read_to_string(root.join(theme).join("index.theme")).ok())
    else {
        return Vec::new();
    };
    let mut section = "";
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line;
        } else if section == "[Icon Theme]"
            && let Some(value) = line.strip_prefix("Inherits").map(str::trim_start)
            && let Some(value) = value.strip_prefix('=')
        {
            return value
                .split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_owned)
                .collect();
        }
    }
    Vec::new()
}

/// The themes to search, in order: `theme`, everything it inherits
/// (breadth first, each once), Papirus, then `hicolor`.
pub fn theme_chain(theme: &str, icons: &[PathBuf]) -> Vec<String> {
    let mut chain: Vec<String> = Vec::new();
    let mut queue = std::collections::VecDeque::from([theme.to_owned()]);
    while let Some(next) = queue.pop_front() {
        // A theme name with a path in it would escape the icon roots.
        if next.is_empty() || next.contains('/') || next.starts_with('.') || chain.contains(&next) {
            continue;
        }
        queue.extend(inherits(&next, icons));
        chain.push(next);
    }
    for theme in PAPIRUS.into_iter().chain(["hicolor"]) {
        if !chain.iter().any(|t| t == theme) {
            chain.push(theme.to_owned());
        }
    }
    chain
}

/// The file for icon `name` (a theme name or an absolute path) closest to
/// `want` pixels square, from the XDG icon directories.
pub fn find(name: &str, want: u32) -> Option<PathBuf> {
    let (icons, pixmaps) = roots();
    let themes = theme_chain(&current_theme(), &icons);
    find_in(name, want, &icons, &pixmaps, &themes)
}

/// [`find`] in the given `icons` and `pixmaps` roots, in precedence order,
/// trying `themes` (see [`theme_chain`]) before any other theme.
pub fn find_in(
    name: &str,
    want: u32,
    icons: &[PathBuf],
    pixmaps: &[PathBuf],
    themes: &[String],
) -> Option<PathBuf> {
    if name.starts_with('/') {
        return Some(PathBuf::from(name)).filter(|p| fs::is_file(p));
    }
    // A name with a path separator or a leading dot would escape the theme.
    if name.is_empty() || name.contains('/') || name.starts_with('.') {
        return None;
    }
    for theme in themes {
        let mut best = None;
        for root in icons {
            in_theme(&root.join(theme), name, want, false, &mut best);
        }
        if best.is_some() {
            return best.map(|(_, p)| p);
        }
    }
    for root in icons {
        let Ok(others) = fs::read_dir(root) else {
            continue;
        };
        let mut others: Vec<_> = others
            .filter_map(Result::ok)
            .filter(|t| !themes.iter().any(|n| t.file_name() == n.as_str()))
            .map(|t| t.path())
            .collect();
        others.sort();
        let mut best = None;
        for theme in others {
            in_theme(&theme, name, want, false, &mut best);
        }
        if best.is_some() {
            return best.map(|(_, p)| p);
        }
    }
    pixmaps.iter().find_map(|dir| {
        ["png", "svg"]
            .iter()
            .map(|ext| dir.join(format!("{name}.{ext}")))
            .find(|p| fs::is_file(p))
    })
}

/// Decodes a PNG or SVG icon to at most `size` pixels square.
pub fn load(path: &Path, size: u32) -> Option<ColorImage> {
    if fs::metadata(path).ok()?.len() > MAX_BYTES {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    if path.extension().is_some_and(|e| e == "svg") {
        let tree = resvg::usvg::Tree::from_data(&bytes, &resvg::usvg::Options::default()).ok()?;
        let s = tree.size();
        let scale = size as f32 / s.width().max(s.height());
        let w = ((s.width() * scale).round() as u32).max(1);
        let h = ((s.height() * scale).round() as u32).max(1);
        let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h)?;
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::from_scale(scale, scale),
            &mut pixmap.as_mut(),
        );
        Some(ColorImage::from_rgba_premultiplied(
            [w as usize, h as usize],
            pixmap.data(),
        ))
    } else {
        let mut image = image::load_from_memory(&bytes).ok()?.into_rgba8();
        let (w, h) = (image.width(), image.height());
        if w > size || h > size {
            let scale = size as f32 / w.max(h) as f32;
            image = image::imageops::thumbnail(
                &image,
                ((w as f32 * scale).round() as u32).max(1),
                ((h as f32 * scale).round() as u32).max(1),
            );
        }
        Some(ColorImage::from_rgba_unmultiplied(
            [image.width() as usize, image.height() as usize],
            image.as_raw(),
        ))
    }
}

/// The file for symbolic icon `name`: its `-symbolic` variant, else the
/// plain icon at a small size (at most 24 px, where themes draw actions as
/// outlines rather than colored pictures), from the first theme in the
/// [`theme_chain`] that has either.
///
/// Every `<theme>/<a>/<b>/` directory is searched, which covers Papirus's
/// `symbolic/actions/`, Adwaita's `scalable/actions/` and `16x16/actions/`,
/// and Breeze's `actions/16/`.
pub fn find_action(name: &str) -> Option<PathBuf> {
    let (icons, _) = roots();
    find_action_in(name, &icons, &theme_chain(&current_theme(), &icons))
}

/// [`find_action`] in the given `icons` roots and `themes`.
pub fn find_action_in(name: &str, icons: &[PathBuf], themes: &[String]) -> Option<PathBuf> {
    if name.is_empty() || name.contains('/') || name.starts_with('.') {
        return None;
    }
    let symbolic = format!("{name}-symbolic");
    for theme in themes {
        let mut best = None;
        for root in icons {
            in_theme(&root.join(theme), &symbolic, 16, false, &mut best);
        }
        if best.is_none() {
            // Only small sizes: a big plain icon is a colored picture, and
            // its mask a blob.
            for root in icons {
                in_theme(&root.join(theme), name, 16, true, &mut best);
            }
        }
        if best.is_some() {
            return best.map(|(_, p)| p);
        }
    }
    None
}

/// Decodes an icon as a white mask: only its shape survives, so painting it
/// with a tint gives a greyscale icon in the theme's own text color whatever
/// colors the file uses.
pub fn load_mask(path: &Path, size: u32) -> Option<ColorImage> {
    let mut image = load(path, size)?;
    for pixel in &mut image.pixels {
        *pixel = Color32::from_white_alpha(pixel.a());
    }
    Some(image)
}

/// What to draw for symbolic icon `name` when no Papirus theme has it: a
/// glyph egui's built-in fonts cover, or `•` for a name this table lacks.
pub fn glyph(name: &str) -> &'static str {
    match name {
        "view-app-grid" => "◆",
        "system-search" => "🔍",
        "go-previous" => "⬅",
        "go-next" => "➡",
        "go-up" => "⬆",
        "go-home" => "🏠",
        "view-dual" => "⊟",
        "view-fullscreen" => "▣",
        "view-refresh" => "⟳",
        "video-display" => "🖥",
        "go-jump" => "⎆",
        "system-lock-screen" => "🔒",
        "system-suspend" => "🌙",
        "system-hibernate" => "❄",
        "system-log-out" => "🚪",
        "system-reboot" => "⟳",
        "system-shutdown" => "⏻",
        "starred" => "★",
        "dialog-warning" => "⚠",
        "dialog-error" => "✖",
        "dialog-question" => "❓",
        "object-select" => "✔",
        "view-restore" => "🗗",
        "window-maximize" => "🗖",
        "window-minimize" => "🗕",
        "window-close" => "×",
        "open-menu" => "☰",
        "list-add" => "+",
        "tool-magic" => "✨",
        "input-keyboard" => "⌨",
        "keyboard-shift-filled" => "⬆",
        "edit-clear" => "⬅",
        "pan-up" => "⏶",
        "pan-down" => "⏷",
        "media-playback-start" => "▶",
        "folder" => "🗀",
        "text-x-generic" => "🗋",
        "insert-link" => "🔗",
        "user-home" => "🏠",
        "user-desktop" => "🖥",
        "folder-documents" => "🗐",
        "folder-download" => "⬇",
        "folder-music" => "🎵",
        "folder-pictures" => "🖼",
        "folder-videos" => "🎞",
        "user-trash" => "🗑",
        "computer" => "💻",
        "power-profile-performance" => "⚡",
        "power-profile-balanced" => "◑",
        "power-profile-power-saver" => "🍃",
        n if n.starts_with("battery-") && n.ends_with("-charging") => "⚡",
        n if n.starts_with("battery-") => "▮",
        _ => "•",
    }
}

/// The symbolic battery icon for `percent`, in Papirus's steps of ten.
pub fn battery(percent: u8, charging: bool) -> String {
    let level = (u32::from(percent.min(100)) + 5) / 10 * 10;
    if charging {
        format!("battery-level-{level}-charging")
    } else {
        format!("battery-level-{level}")
    }
}

/// Pixels symbolic icons are decoded at: up to 24 points at 2x.
const SYMBOLIC_PX: u32 = 48;

/// Symbolic icon `name` as a texture of `ctx`, or `None` when no Papirus
/// theme has it. Each egui context (the chrome, each title bar, each app)
/// has its own textures, so each decodes and uploads an icon once.
pub fn texture(ctx: &Context, name: &str) -> Option<TextureHandle> {
    let id = Id::new(("derisk-symbolic-icon", generation(), name));
    if let Some(cached) = ctx.data(|d| d.get_temp::<Option<TextureHandle>>(id)) {
        return cached;
    }
    let texture = find_action(name)
        .and_then(|path| load_mask(&path, SYMBOLIC_PX))
        .map(|image| {
            ctx.load_texture(
                format!("derisk-symbolic-icon:{name}"),
                image,
                TextureOptions::LINEAR,
            )
        });
    ctx.data_mut(|d| d.insert_temp(id, texture.clone()));
    texture
}

/// Paints symbolic icon `name` into `r` in `color`: the Papirus icon, else
/// its [`glyph`] at the same size.
pub fn paint(painter: &Painter, r: Rect, name: &str, color: Color32) {
    match texture(painter.ctx(), name) {
        Some(texture) => {
            painter.image(
                texture.id(),
                r,
                Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
                color,
            );
        }
        None => {
            painter.text(
                r.center(),
                Align2::CENTER_CENTER,
                glyph(name),
                FontId::proportional(r.height()),
                color,
            );
        }
    }
}

/// Symbolic icon `name` as a widget `size` points square, laid out like a
/// label.
pub fn show(ui: &mut Ui, name: &str, size: f32, color: Color32) -> Response {
    let (r, response) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    paint(ui.painter(), r, name, color);
    response
}

/// Symbolic icon `name`, `size` points square in `color`, as an atom that
/// egui buttons, selectable labels and menu items take beside text.
pub fn atom<'a>(ctx: &Context, name: &str, size: f32, color: Color32) -> Atom<'a> {
    match texture(ctx, name) {
        Some(texture) => egui::Image::new((texture.id(), vec2(size, size)))
            .tint(color)
            .into(),
        None => RichText::new(glyph(name)).size(size).color(color).into(),
    }
}

/// A button showing symbolic icon `name`, framed like any other button;
/// `.frame(false)` makes it a bare icon. Name it for screen readers with
/// [`tool`] or `Response::widget_info`: it has no text for them to read.
pub fn button<'a>(ctx: &Context, name: &str, size: f32, color: Color32) -> egui::Button<'a> {
    egui::Button::new(atom(ctx, name, size, color))
}

/// A button with symbolic icon `name` before `text`.
pub fn button_with_text<'a>(
    ctx: &Context,
    name: &str,
    text: impl Into<WidgetText>,
    size: f32,
    color: Color32,
) -> egui::Button<'a> {
    egui::Button::new((atom(ctx, name, size, color), text.into()))
}

/// A toolbar button: symbolic icon `name` in the text color, with `label`
/// as its tooltip and as the name screen readers read.
pub fn tool(ui: &mut Ui, enabled: bool, name: &str, label: &str) -> Response {
    let color = ui.visuals().text_color();
    let response = ui
        .add_enabled(enabled, button(ui.ctx(), name, 16.0, color))
        .on_hover_text(label);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, label));
    response
}
