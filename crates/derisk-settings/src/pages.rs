//! The Wallpaper, Top bar and Shortcuts pages of the Settings app, and the
//! text being typed into them.

use std::path::{Path, PathBuf};

use mcsapi_ui::{Theme, egui};

use crate::{
    model::{
        BarPosition, Fit, Rgb, Settings, TopBar, Wallpaper, WallpaperKind, web_background_url,
    },
    shortcuts::{Group, Keys, Shortcut, Shortcuts},
    thumbs::Thumbnails,
};

/// The screen width the preview stands for, so Center and Tile show
/// pictures at about the size they will have.
const PREVIEW_SCREEN_WIDTH: f32 = 1920.0;

/// Picture extensions the session can decode.
pub const IMAGE_EXTENSIONS: [&str; 4] = ["png", "jpg", "jpeg", "webp"];

/// Video extensions offered in the file list. ffmpeg plays more; any path
/// typed in works.
const VIDEO_EXTENSIONS: [&str; 5] = ["mp4", "webm", "mkv", "mov", "gif"];

/// Typed text that is only applied once it parses, so a half-typed path
/// or chord never reaches the settings file.
#[derive(Debug, Default)]
pub(crate) struct Drafts {
    path: String,
    /// The web page's address as typed.
    url: String,
    /// The lock screen's picture as typed.
    lock_path: String,
    chords: Vec<String>,
    /// Files and folders offered for the current wallpaper kind.
    candidates: Option<(WallpaperKind, Vec<PathBuf>)>,
}

impl Drafts {
    /// Drafts showing `settings` as saved.
    pub(crate) fn new(settings: &Settings) -> Self {
        Self {
            path: settings.wallpaper.path.display().to_string(),
            url: settings.wallpaper.url.clone(),
            lock_path: settings.wallpaper.lock_screen.display().to_string(),
            chords: Shortcut::ALL
                .iter()
                .map(|s| chord_text(settings.shortcuts.get(*s)))
                .collect(),
            candidates: None,
        }
    }
}

fn chord_text(keys: Option<Keys>) -> String {
    keys.map_or_else(|| "none".to_owned(), |k| k.to_string())
}

/// Where pictures and videos usually live: the user's Pictures and Videos
/// folders, and `backgrounds` under each `$XDG_DATA_DIRS` entry.
fn wallpaper_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = derisk_sys::env::var_os("HOME").map(PathBuf::from) {
        dirs.extend(["Pictures", "Pictures/Wallpapers", "Videos"].map(|d| home.join(d)));
    }
    let data = derisk_sys::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_owned());
    dirs.extend(
        data.split(':')
            .filter(|d| Path::new(d).is_absolute())
            .map(|d| Path::new(d).join("backgrounds")),
    );
    dirs
}

/// Files of `kind` (or folders holding pictures, for a slideshow) directly
/// inside `dirs`, sorted, at most 200.
pub fn candidates(kind: WallpaperKind, dirs: &[PathBuf]) -> Vec<PathBuf> {
    let has_ext = |path: &Path, exts: &[&str]| {
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| exts.contains(&e.to_ascii_lowercase().as_str()))
    };
    let mut found: Vec<PathBuf> = Vec::new();
    for dir in dirs {
        if kind == WallpaperKind::Slideshow
            && let Some(true) = has_pictures(dir)
        {
            found.push(dir.clone());
        }
        let Ok(entries) = derisk_sys::fs::read_dir(dir) else {
            continue;
        };
        for path in entries.flatten().map(|e| e.path()) {
            let wanted = match kind {
                WallpaperKind::Image => has_ext(&path, &IMAGE_EXTENSIONS),
                WallpaperKind::Video => has_ext(&path, &VIDEO_EXTENSIONS),
                WallpaperKind::Slideshow => {
                    derisk_sys::fs::is_dir(&path) && has_pictures(&path) == Some(true)
                }
                _ => false,
            };
            if wanted {
                found.push(path);
            }
        }
    }
    found.sort();
    found.dedup();
    found.truncate(200);
    found
}

fn has_pictures(dir: &Path) -> Option<bool> {
    Some(derisk_sys::fs::read_dir(dir).ok()?.flatten().any(|e| {
        e.path()
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| IMAGE_EXTENSIONS.contains(&x.to_ascii_lowercase().as_str()))
    }))
}

fn color_row(ui: &mut egui::Ui, label: &str, color: &mut Rgb) {
    ui.label(label);
    let mut rgb = [color.0, color.1, color.2];
    if ui.color_edit_button_srgb(&mut rgb).changed() {
        *color = Rgb(rgb[0], rgb[1], rgb[2]);
    }
    ui.end_row();
}

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label("");
    ui.label(crate::layout::note(
        &mcsapi_components::Tokens::current(ui.ctx()),
        text,
    ));
    ui.end_row();
}

pub(crate) fn wallpaper(
    ui: &mut egui::Ui,
    w: &mut Wallpaper,
    drafts: &mut Drafts,
    thumbs: &mut Thumbnails,
    theme: &Theme,
) {
    ui.label("Preview");
    preview(ui, w, thumbs, theme, egui::vec2(320.0, 200.0));
    ui.end_row();
    ui.label("Background");
    ui.horizontal_wrapped(|ui| {
        for kind in WallpaperKind::ALL {
            ui.selectable_value(&mut w.kind, kind, kind.label());
        }
    });
    ui.end_row();
    match w.kind {
        WallpaperKind::Default => {
            hint(ui, "derisk's gradient, tinted with the accent color.");
        }
        WallpaperKind::Color => color_row(ui, "Color", &mut w.color),
        WallpaperKind::Gradient => {
            color_row(ui, "Top", &mut w.color);
            color_row(ui, "Bottom", &mut w.color2);
        }
        WallpaperKind::Image | WallpaperKind::Slideshow | WallpaperKind::Video => {
            file_rows(ui, w, drafts, thumbs, theme);
            ui.label("Fit");
            ui.horizontal_wrapped(|ui| {
                for fit in Fit::ALL {
                    ui.selectable_value(&mut w.fit, fit, fit.label());
                }
            });
            ui.end_row();
            if w.fit == Fit::Fit {
                color_row(ui, "Bars", &mut w.color);
            }
        }
        WallpaperKind::Web => url_rows(ui, w, drafts),
    }
    if w.kind == WallpaperKind::Slideshow {
        ui.label("Change every");
        ui.add(
            egui::Slider::new(&mut w.interval_min, 1..=1440)
                .logarithmic(true)
                .suffix(" min"),
        );
        ui.end_row();
        ui.label("Order");
        ui.checkbox(&mut w.shuffle, "Shuffle");
        ui.end_row();
    }
    if matches!(w.kind, WallpaperKind::Video | WallpaperKind::Web) {
        ui.label("Pause");
        ui.vertical(|ui| {
            ui.checkbox(&mut w.pause_when_covered, "While a window fills the screen");
            ui.checkbox(&mut w.pause_in_low_power, "In low power mode");
        });
        ui.end_row();
    }
    if w.kind == WallpaperKind::Video {
        hint(
            ui,
            "Videos loop without sound. They are decoded by ffmpeg at up to 30 fps.",
        );
    }
    lock_screen_rows(ui, w, drafts);
}

/// The web page's address: http(s), a file:/// URL, or a file's full path.
fn url_rows(ui: &mut egui::Ui, w: &mut Wallpaper, drafts: &mut Drafts) {
    ui.label("Address");
    let edit = ui.add(
        egui::TextEdit::singleline(&mut drafts.url)
            .hint_text("https://example.org/ or /home/you/page/index.html")
            .desired_width(ui.spacing().text_edit_width),
    );
    let typed = drafts.url.trim();
    // A full path is a local page.
    let url = if typed.starts_with('/') {
        format!("file://{typed}")
    } else {
        typed.to_owned()
    };
    let valid = typed.is_empty() || web_background_url(&url);
    if edit.changed() && valid {
        w.url = url;
    }
    ui.end_row();
    hint(
        ui,
        if !valid {
            "Type an address starting with https:// or http://, or a full path."
        } else {
            "Danube draws the page behind every window, without sound, \
             keyboard or pointer, and with nothing of your browser's. If it \
             fails to load, the default shows instead."
        },
    );
}

/// The lock screen's picture: a path, or empty for derisk's gradient.
fn lock_screen_rows(ui: &mut egui::Ui, w: &mut Wallpaper, drafts: &mut Drafts) {
    ui.label("Lock screen");
    ui.horizontal_wrapped(|ui| {
        let edit = ui.add(
            egui::TextEdit::singleline(&mut drafts.lock_path)
                .hint_text("Empty for derisk's gradient")
                .desired_width(ui.spacing().text_edit_width),
        );
        let typed = PathBuf::from(drafts.lock_path.trim());
        if edit.changed() && (drafts.lock_path.trim().is_empty() || typed.is_absolute()) {
            w.lock_screen = typed;
        }
        let desktop = w.kind == WallpaperKind::Image && !w.path.as_os_str().is_empty();
        if desktop && w.lock_screen != w.path && ui.button("Use the background").clicked() {
            w.lock_screen = w.path.clone();
            drafts.lock_path = w.path.display().to_string();
        }
        if !w.lock_screen.as_os_str().is_empty() && ui.button("Gradient").clicked() {
            w.lock_screen = PathBuf::new();
            drafts.lock_path.clear();
        }
    });
    ui.end_row();
    let typed = drafts.lock_path.trim();
    if !typed.is_empty() && !Path::new(typed).is_absolute() {
        hint(ui, "Type a full path, starting with /.");
    } else if !w.lock_screen.as_os_str().is_empty() && !derisk_sys::fs::exists(&w.lock_screen) {
        hint(ui, "Nothing is there yet; the gradient shows instead.");
    } else {
        hint(
            ui,
            "A picture fills the screen, darkened so the clock stays readable.",
        );
    }
}

fn file_rows(
    ui: &mut egui::Ui,
    w: &mut Wallpaper,
    drafts: &mut Drafts,
    thumbs: &mut Thumbnails,
    theme: &Theme,
) {
    ui.label(if w.kind == WallpaperKind::Slideshow {
        "Folder"
    } else {
        "File"
    });
    let edit = ui.add(
        egui::TextEdit::singleline(&mut drafts.path)
            .hint_text("/home/you/Pictures/wallpaper.jpg")
            .desired_width(ui.spacing().text_edit_width),
    );
    let typed = PathBuf::from(drafts.path.trim());
    let valid = drafts.path.trim().is_empty() || typed.is_absolute();
    if edit.changed() && valid {
        w.path = typed;
    }
    ui.end_row();
    if !valid {
        hint(ui, "Type a full path, starting with /.");
    } else if !w.path.as_os_str().is_empty() && !derisk_sys::fs::exists(&w.path) {
        hint(ui, "Nothing is there yet; the default shows instead.");
    }
    if drafts.candidates.as_ref().is_none_or(|(k, _)| *k != w.kind) {
        drafts.candidates = Some((w.kind, candidates(w.kind, &wallpaper_dirs())));
    }
    let Some((_, found)) = &drafts.candidates else {
        return;
    };
    if found.is_empty() {
        hint(ui, "Nothing found in Pictures, Videos or backgrounds.");
        return;
    }
    ui.label("Found");
    // Top-aligned: `horizontal_wrapped` centers each item on the first
    // line's height, which pushes the tall tiles over the next grid row.
    let tiles = egui::Layout::left_to_right(egui::Align::Min).with_main_wrap(true);
    ui.with_layout(tiles, |ui| {
        ui.set_max_width(520.0);
        for path in found {
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            let tile = Wallpaper {
                path: path.clone(),
                fit: Fit::Fill,
                ..w.clone()
            };
            let selected = w.path == *path;
            let response = ui
                .vertical(|ui| {
                    ui.set_width(156.0);
                    let r = preview(ui, &tile, thumbs, theme, egui::vec2(156.0, 98.0));
                    if selected {
                        ui.painter().rect_stroke(
                            r.rect.expand(2.0),
                            6,
                            egui::Stroke::new(2.0, theme.accent),
                            egui::StrokeKind::Outside,
                        );
                    }
                    ui.label(egui::RichText::new(elide(&name, 24)).small());
                    r
                })
                .inner;
            if response.on_hover_text(path.display().to_string()).clicked() {
                w.path = path.clone();
                drafts.path = path.display().to_string();
            }
        }
    });
    ui.end_row();
}

fn elide(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut short: String = text.chars().take(max - 1).collect();
    short.push('…');
    short
}

/// A miniature screen showing `w`, clickable. Pictures, videos and
/// slideshows show their thumbnail as the session would fit it.
fn preview(
    ui: &mut egui::Ui,
    w: &Wallpaper,
    thumbs: &mut Thumbnails,
    theme: &Theme,
    size: egui::Vec2,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let painter = ui.painter_at(rect);
    let gradient = |top: egui::Color32, bottom: egui::Color32| {
        let mut mesh = egui::Mesh::default();
        mesh.colored_vertex(rect.left_top(), top);
        mesh.colored_vertex(rect.right_top(), top);
        mesh.colored_vertex(rect.left_bottom(), bottom);
        mesh.colored_vertex(rect.right_bottom(), bottom);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(1, 2, 3);
        painter.add(egui::Shape::mesh(mesh));
    };
    let default = || {
        gradient(Rgb(17, 24, 39).color(), Rgb(30, 27, 75).color());
        painter.circle_filled(
            rect.right_bottom() - egui::vec2(size.x * 0.05, size.y * 0.07),
            size.y * 0.04,
            theme.accent.gamma_multiply(0.35),
        );
    };
    let badge = match w.kind {
        WallpaperKind::Default => {
            default();
            None
        }
        WallpaperKind::Color => {
            painter.rect_filled(rect, 0, w.color.color());
            None
        }
        WallpaperKind::Gradient => {
            gradient(w.color.color(), w.color2.color());
            None
        }
        WallpaperKind::Web => {
            default();
            Some("Web page")
        }
        WallpaperKind::Image | WallpaperKind::Slideshow | WallpaperKind::Video => {
            match thumbs.get(ui.ctx(), &w.path) {
                Some((texture, source)) => {
                    painter.rect_filled(rect, 0, w.color.color());
                    // Videos always fill (see the session's ffmpeg filter).
                    let fit = if w.kind == WallpaperKind::Video && w.fit != Fit::Fit {
                        Fit::Fill
                    } else {
                        w.fit
                    };
                    // Center and Tile work at actual size: scale the source
                    // as a 1920-wide screen would show it in this preview.
                    let k = size.x / PREVIEW_SCREEN_WIDTH;
                    let shown = [
                        (source[0] as f32 * k).max(1.0) as usize,
                        (source[1] as f32 * k).max(1.0) as usize,
                    ];
                    let (to, uv) = fit.place(shown, rect);
                    painter.image(texture.id(), to, uv, egui::Color32::WHITE);
                }
                None => {
                    default();
                    let text = if w.path.as_os_str().is_empty() {
                        "Choose a file"
                    } else if thumbs.failed(&w.path) {
                        "No preview"
                    } else {
                        "Loading…"
                    };
                    painter.text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        text,
                        egui::FontId::proportional(12.0),
                        theme.foreground,
                    );
                }
            }
            match w.kind {
                WallpaperKind::Video => Some("Video"),
                WallpaperKind::Slideshow => Some("Slideshow"),
                _ => None,
            }
        }
    };
    if let Some(badge) = badge {
        let galley = painter.layout_no_wrap(
            badge.to_owned(),
            egui::FontId::proportional(11.0),
            egui::Color32::WHITE,
        );
        // A video's badge leads with a play icon.
        let icon = (w.kind == WallpaperKind::Video).then_some("media-playback-start");
        let side = galley.size().y;
        let lead = if icon.is_some() { side + 3.0 } else { 0.0 };
        let at = rect.left_bottom() + egui::vec2(6.0, -6.0 - side);
        painter.rect_filled(
            egui::Rect::from_min_size(at, galley.size() + egui::vec2(lead, 0.0)).expand(3.0),
            4,
            egui::Color32::from_black_alpha(160),
        );
        if let Some(icon) = icon {
            let r = egui::Rect::from_min_size(at, egui::vec2(side, side));
            derisk_icons::paint(&painter, r, icon, egui::Color32::WHITE);
        }
        painter.galley(at + egui::vec2(lead, 0.0), galley, egui::Color32::WHITE);
    }
    painter.rect_stroke(
        rect,
        6,
        egui::Stroke::new(1.0, theme.border),
        egui::StrokeKind::Inside,
    );
    response
}

pub(crate) fn top_bar(ui: &mut egui::Ui, b: &mut TopBar) {
    ui.label("Position");
    ui.horizontal(|ui| {
        ui.selectable_value(&mut b.position, BarPosition::Top, "Top");
        ui.selectable_value(&mut b.position, BarPosition::Bottom, "Bottom");
    });
    ui.end_row();
    ui.label("Auto-hide");
    ui.checkbox(&mut b.autohide, "Hide until the pointer touches the edge");
    ui.end_row();
    ui.label("Show");
    ui.vertical(|ui| {
        ui.checkbox(&mut b.search, "Search button");
        ui.checkbox(&mut b.app_name, "Focused app's name");
        ui.checkbox(&mut b.date, "Date");
        ui.checkbox(&mut b.battery, "Battery");
    });
    ui.end_row();
    ui.label("Clock");
    ui.horizontal(|ui| {
        ui.selectable_value(&mut b.clock_24h, true, "24-hour");
        ui.selectable_value(&mut b.clock_24h, false, "AM/PM");
    });
    ui.end_row();
    hint(
        ui,
        "The overview button, menus, tray and warnings always show.",
    );
}

pub(crate) fn shortcuts(ui: &mut egui::Ui, s: &mut Shortcuts, drafts: &mut Drafts, theme: &Theme) {
    let clashes = s.clashes();
    for group in Group::ALL {
        ui.label(egui::RichText::new(group.label()).strong());
        ui.end_row();
        for (i, shortcut) in Shortcut::ALL.into_iter().enumerate() {
            if shortcut.group() != group {
                continue;
            }
            ui.label(shortcut.label());
            ui.horizontal(|ui| {
                let text = &mut drafts.chords[i];
                let edit = ui.add(egui::TextEdit::singleline(text).desired_width(160.0));
                let parsed = if text.trim().eq_ignore_ascii_case("none") {
                    Some(None)
                } else {
                    Keys::parse(text).map(Some)
                };
                match parsed {
                    Some(keys) if edit.changed() => s.set(shortcut, keys),
                    None => {
                        ui.label(egui::RichText::new("Not a shortcut").color(theme.accent));
                    }
                    _ => {}
                }
                if clashes.contains(&shortcut) {
                    ui.label(egui::RichText::new("Clashes").color(theme.accent));
                }
                if s.get(shortcut) != Some(shortcut.default_keys())
                    && ui.small_button("Reset").clicked()
                {
                    s.set(shortcut, Some(shortcut.default_keys()));
                    *text = chord_text(s.get(shortcut));
                }
            });
            ui.end_row();
        }
    }
    hint(
        ui,
        "Write a chord like Super+Shift+M, or a sequence like Super+W Left: press the first \
         chord, then the next key. \"none\" turns one off. The first chord needs Super, Ctrl or \
         Alt, unless it is Print or F1…F12. Super+1…9, Super+arrows, Super+Ctrl+arrows, Alt+Tab \
         and Alt+F4 are fixed.",
    );
}
