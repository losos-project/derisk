use std::{
    fmt, io,
    path::{Path, PathBuf},
};

use derisk_sys::fs;

use mcsapi_ui::{
    Theme,
    egui::{Color32, Rect, pos2, vec2},
};

use crate::shortcuts::Shortcuts;

/// Light or dark shell colors.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ColorScheme {
    /// Dark surfaces with light text (the mcsapi default).
    #[default]
    Dark,
    /// Light surfaces with dark text.
    Light,
}

/// Highlight color for active controls and the focused workspace.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Accent {
    /// Teal, the mcsapi default and the LosOS web interface's accent.
    #[default]
    Teal,
    /// Lime, the default before mcsapi took the web interface's look.
    Lime,
    /// Sky blue.
    Sky,
    /// Violet.
    Violet,
    /// Rose.
    Rose,
    /// Amber.
    Amber,
}

impl Accent {
    /// Every accent, in display order.
    pub const ALL: [Self; 6] = [
        Self::Teal,
        Self::Lime,
        Self::Sky,
        Self::Violet,
        Self::Rose,
        Self::Amber,
    ];

    /// The accent's color, from the theming engine's built-in accents.
    pub fn spec_color(self) -> mcsapi_theme::Color {
        // Every name in `enum_text!` below is one of mcsapi_theme::ACCENTS.
        mcsapi_theme::accent(self.as_str()).unwrap_or(mcsapi_theme::ACCENTS[0].1)
    }

    /// The accent's color.
    pub fn color(self) -> Color32 {
        let c = self.spec_color();
        Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
    }
}

/// Which theme the desktop uses: [`ThemeId::AUTOMATIC`] (the built-in light
/// or dark theme for [`ColorScheme`], with the chosen [`Accent`]) or the ID of
/// a theme file, `derisk/themes/<id>.theme` in an XDG data directory.
///
/// Stored inline so [`Settings`] stays `Copy`.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct ThemeId {
    bytes: [u8; Self::MAX],
    len: u8,
}

impl ThemeId {
    /// Longest ID accepted.
    pub const MAX: usize = 48;
    /// Follow the color scheme and accent settings.
    pub const AUTOMATIC: Self = Self {
        bytes: [0; Self::MAX],
        len: 0,
    };

    /// Parses `auto` or a theme ID: letters, digits, `-`, `_` and `.`, not
    /// starting with a dot, at most [`ThemeId::MAX`] bytes.
    pub fn parse(text: &str) -> Option<Self> {
        if text == "auto" {
            return Some(Self::AUTOMATIC);
        }
        let valid = !text.is_empty()
            && text.len() <= Self::MAX
            && !text.starts_with('.')
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
        valid.then(|| {
            let mut bytes = [0; Self::MAX];
            bytes[..text.len()].copy_from_slice(text.as_bytes());
            Self {
                bytes,
                len: text.len() as u8,
            }
        })
    }

    /// Whether this follows the scheme and accent settings.
    pub fn is_automatic(self) -> bool {
        self.len == 0
    }

    /// The ID, or `auto`.
    pub fn as_str(&self) -> &str {
        if self.is_automatic() {
            return "auto";
        }
        // Infallible: `parse` only stores ASCII.
        std::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or("auto")
    }
}

impl Default for ThemeId {
    fn default() -> Self {
        Self::AUTOMATIC
    }
}

impl fmt::Debug for ThemeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ThemeId({})", self.as_str())
    }
}

impl fmt::Display for ThemeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Default tiling layout for new workspaces.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Layout {
    /// Main pane plus a stack, like xmonad's Tall.
    #[default]
    Tall,
    /// One full-size window at a time.
    Monocle,
}

/// Which adaptive profile the shell uses.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Profile {
    /// Pick from the output size.
    #[default]
    Automatic,
    /// Monocle, no gaps, large targets.
    Phone,
    /// Touch-friendly tiling.
    Tablet,
    /// Full desktop.
    Desktop,
}

/// When the shell saves power by dropping blur, animations and frame rate.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LowPower {
    /// Never.
    Off,
    /// While running on battery.
    #[default]
    OnBattery,
    /// Always.
    On,
}

impl LowPower {
    /// Every mode, in display order.
    pub const ALL: [Self; 3] = [Self::Off, Self::OnBattery, Self::On];

    /// The mode's label in the Settings app.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::OnBattery => "On battery",
            Self::On => "Always",
        }
    }

    /// Whether low power mode applies, given whether the device runs on
    /// battery right now.
    pub const fn active(self, on_battery: bool) -> bool {
        match self {
            Self::Off => false,
            Self::OnBattery => on_battery,
            Self::On => true,
        }
    }
}

/// How opaque each shell panel is, in percent (20–100). Below 100 the
/// blurred desktop shows through.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PanelOpacity {
    /// The top bar.
    pub top_bar: u8,
    /// The overview backdrop.
    pub overview: u8,
    /// The Snap Assist picker.
    pub snap_assist: u8,
}

/// Appearance preferences.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Appearance {
    /// The theme; when automatic, `scheme` and `accent` pick a built-in one.
    pub theme: ThemeId,
    /// Light or dark colors.
    pub scheme: ColorScheme,
    /// Highlight color.
    pub accent: Accent,
    /// Interface text scale, from 0.75 to 2.0.
    pub text_scale: f32,
    /// Replace motion with cross-fades.
    pub reduce_motion: bool,
    /// Background blur under translucent panels, 0 (off) to 10.
    pub blur: u8,
    /// Panel opacity.
    pub panels: PanelOpacity,
}

/// Which screen edge the top bar sits on.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BarPosition {
    /// Along the top edge.
    #[default]
    Top,
    /// Along the bottom edge, like a taskbar.
    Bottom,
}

/// The top bar's place and what it shows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TopBar {
    /// Top or bottom edge.
    pub position: BarPosition,
    /// Hide the bar until the pointer touches its edge; windows then use
    /// the whole screen.
    pub autohide: bool,
    /// The search button that opens the command palette.
    pub search: bool,
    /// The focused app's name.
    pub app_name: bool,
    /// The date beside the clock.
    pub date: bool,
    /// The battery level.
    pub battery: bool,
    /// A 24-hour clock instead of AM/PM.
    pub clock_24h: bool,
}

/// How window frames look.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowStyle {
    /// Radius of the title bar's top corners, 0 to 20 logical pixels.
    pub corner_radius: u8,
    /// A soft shadow under each window (low power mode drops it anyway).
    pub shadows: bool,
}

/// An sRGB color, written `#rrggbb`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Parses `#rrggbb`.
    pub fn parse(text: &str) -> Option<Self> {
        let hex = text.strip_prefix('#')?;
        if hex.len() != 6 || !hex.is_ascii() {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        Some(Self(byte(0)?, byte(2)?, byte(4)?))
    }

    /// The color as `#rrggbb`.
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }

    /// The color for egui.
    pub const fn color(self) -> Color32 {
        Color32::from_rgb(self.0, self.1, self.2)
    }
}

/// What the desktop background shows.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WallpaperKind {
    /// derisk's own gradient, tinted by the accent.
    #[default]
    Default,
    /// One color.
    Color,
    /// A top-to-bottom gradient between two colors.
    Gradient,
    /// A PNG, JPEG or WebP picture.
    Image,
    /// Pictures from a folder, changing on a timer.
    Slideshow,
    /// A looping video, decoded by ffmpeg.
    Video,
    /// A web page, drawn by Danube.
    Web,
}

impl WallpaperKind {
    /// Every kind, in display order.
    pub const ALL: [Self; 7] = [
        Self::Default,
        Self::Color,
        Self::Gradient,
        Self::Image,
        Self::Slideshow,
        Self::Video,
        Self::Web,
    ];

    /// The kind's label in the Settings app.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Default => "derisk",
            Self::Color => "Color",
            Self::Gradient => "Gradient",
            Self::Image => "Picture",
            Self::Slideshow => "Slideshow",
            Self::Video => "Video",
            Self::Web => "Web page",
        }
    }
}

/// Whether `url` can be a web page background: an http(s) address or a
/// local file (`file:///…`). Nothing else: the settings file is plain
/// text a program could write, and `javascript:` or `data:` make no
/// background.
pub fn web_background_url(url: &str) -> bool {
    !url.chars().any(char::is_whitespace)
        && ["https://", "http://", "file:///"]
            .iter()
            .any(|scheme| url.len() > scheme.len() && url.starts_with(scheme))
}

/// How a picture or video fills a screen of a different shape.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Fit {
    /// Cover the screen, cropping the overflow.
    #[default]
    Fill,
    /// Show all of it, with bars of the first color around it.
    Fit,
    /// Cover the screen, distorting it.
    Stretch,
    /// Actual size in the middle.
    Center,
    /// Repeat at actual size.
    Tile,
}

impl Fit {
    /// Every mode, in display order.
    pub const ALL: [Self; 5] = [
        Self::Fill,
        Self::Fit,
        Self::Stretch,
        Self::Center,
        Self::Tile,
    ];

    /// Where a `size` picture lands on `screen`, as the rectangle to draw and
    /// the part of the texture to sample (in 0–1 texture coordinates; above 1
    /// repeats, for tiling).
    pub fn place(self, size: [usize; 2], screen: Rect) -> (Rect, Rect) {
        let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        let (w, h) = (size[0].max(1) as f32, size[1].max(1) as f32);
        match self {
            Fit::Stretch => (screen, full),
            Fit::Fill => {
                // Sample the centered part of the picture with the screen's shape.
                let scale = (screen.width() / w).max(screen.height() / h);
                let (uw, uh) = (screen.width() / (w * scale), screen.height() / (h * scale));
                let uv = Rect::from_center_size(pos2(0.5, 0.5), vec2(uw, uh));
                (screen, uv)
            }
            Fit::Fit => {
                let scale = (screen.width() / w).min(screen.height() / h);
                (
                    Rect::from_center_size(screen.center(), vec2(w * scale, h * scale)),
                    full,
                )
            }
            Fit::Center => {
                // Actual size, cropped to the screen when larger.
                let shown = vec2(w.min(screen.width()), h.min(screen.height()));
                let uv = Rect::from_center_size(pos2(0.5, 0.5), vec2(shown.x / w, shown.y / h));
                (Rect::from_center_size(screen.center(), shown), uv)
            }
            Fit::Tile => (
                screen,
                Rect::from_min_size(
                    pos2(0.0, 0.0),
                    vec2(screen.width() / w, screen.height() / h),
                ),
            ),
        }
    }

    /// The mode's label in the Settings app.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Fill => "Fill",
            Self::Fit => "Fit",
            Self::Stretch => "Stretch",
            Self::Center => "Center",
            Self::Tile => "Tile",
        }
    }
}

/// The desktop background.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Wallpaper {
    /// What to show.
    pub kind: WallpaperKind,
    /// The picture or video file, or the slideshow's folder.
    pub path: PathBuf,
    /// The web page's address (see [`web_background_url`]).
    pub url: String,
    /// How pictures and videos fill the screen.
    pub fit: Fit,
    /// The color, the gradient's top, and the bars around a fitted picture.
    pub color: Rgb,
    /// The gradient's bottom.
    pub color2: Rgb,
    /// Minutes between slideshow pictures, 1 to 1440.
    pub interval_min: u16,
    /// Show slideshow pictures in random order.
    pub shuffle: bool,
    /// Stop a video or web page while a window covers the whole screen.
    pub pause_when_covered: bool,
    /// Stop a video or web page in low power mode (on battery, by default).
    pub pause_in_low_power: bool,
    /// The lock screen's picture, filling the screen; empty for derisk's
    /// gradient. Apps set it through the Wallpaper portal.
    pub lock_screen: PathBuf,
}

impl Default for Wallpaper {
    fn default() -> Self {
        Self {
            kind: WallpaperKind::Default,
            path: PathBuf::new(),
            url: String::new(),
            fit: Fit::Fill,
            color: Rgb(17, 24, 39),
            color2: Rgb(30, 27, 75),
            interval_min: 30,
            shuffle: false,
            pause_when_covered: true,
            pause_in_low_power: true,
            lock_screen: PathBuf::new(),
        }
    }
}

/// Privacy preferences.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Privacy {
    /// Apps may ask for the location. Off denies it to every Flatpak app.
    pub location: bool,
    /// Apps may ask for the camera. Off denies it to every Flatpak app.
    pub camera: bool,
    /// Apps may ask for the microphone. Off denies it to every Flatpak app.
    pub microphone: bool,
    /// Keep a list of recently used files and palette picks.
    pub remember_recent: bool,
    /// Days after which trashed files are deleted for good; 0 keeps them.
    pub empty_trash_days: u16,
    /// Let the OS count this user as active, once a day. derisk only keeps
    /// the switch; the OS's ping reads `privacy.usage_ping` from this file
    /// (see [`crate::choice`]).
    pub usage_ping: bool,
}

/// The privacy display: Samsung's of the same name, in software. While one
/// of its conditions holds, the camera looks for a second face, and when it
/// finds one the protected part of the screen is covered (and, at maximum
/// protection, the backlight turned down). Off unless the user turns it on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrivacyDisplay {
    /// Whether it is on at all.
    pub enabled: bool,
    /// While a password is asked for: the lock screen, a polkit dialog.
    pub passwords: bool,
    /// Over notification pop-ups alone, while one is on screen.
    pub notifications: bool,
    /// While one of these apps (by app ID) has the focus.
    pub apps: Vec<String>,
    /// Also turn the backlight down while covered.
    pub maximum: bool,
}

impl Default for PrivacyDisplay {
    fn default() -> Self {
        Self {
            enabled: false,
            passwords: true,
            notifications: true,
            apps: Vec::new(),
            maximum: false,
        }
    }
}

/// Whether `id` can be an app ID in the comma-separated list: a desktop file
/// ID or Wayland app ID, never empty, with no separator in it.
fn app_id(id: &str) -> bool {
    !id.is_empty() && !id.contains([',', '\n', '/']) && id.trim() == id
}

/// The apps and services chosen on the choice screens ([`crate::choice`]).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Defaults {
    /// Desktop file ID of the default browser, empty until chosen.
    pub browser: String,
    /// The palette's web search engine, `None` until chosen; the palette
    /// offers no web search until then.
    pub search: Option<crate::choice::SearchEngine>,
}

/// Whether the display has variable refresh rate (VRR, Adaptive-Sync), which
/// lets the shell pick any frame rate instead of a fraction of the refresh
/// rate.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Vrr {
    /// Use what the compositor detects.
    #[default]
    Automatic,
    /// The display has VRR (for example a nested session on a VRR monitor).
    On,
    /// Treat the display as fixed-rate.
    Off,
}

impl Vrr {
    /// Every mode, in display order.
    pub const ALL: [Self; 3] = [Self::Automatic, Self::On, Self::Off];

    /// The mode's label in the Settings app.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Automatic => "Automatic",
            Self::On => "On",
            Self::Off => "Off",
        }
    }

    /// Whether to treat the display as VRR, given what was detected.
    pub const fn resolve(self, detected: bool) -> bool {
        match self {
            Self::Automatic => detected,
            Self::On => true,
            Self::Off => false,
        }
    }
}

/// Window management preferences.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DesktopPrefs {
    /// Default layout.
    pub layout: Layout,
    /// Gap between tiled windows in logical pixels, up to 64.
    pub gaps: u8,
    /// Number of workspaces, from 1 to 9.
    pub workspaces: u8,
    /// Adaptive profile.
    pub profile: Profile,
    /// Variable refresh rate.
    pub vrr: Vrr,
}

/// Keyboard and pointer preferences.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Input {
    /// Content follows the fingers when scrolling.
    pub natural_scroll: bool,
    /// Tap on a touchpad to click.
    pub tap_to_click: bool,
    /// Delay before a held key repeats, 100 to 1000 ms.
    pub repeat_delay_ms: u16,
    /// Repeats per second, 1 to 60.
    pub repeat_rate: u8,
}

/// Virtual machines (the Machines app, LosOS's machine desktop).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MachinePrefs {
    /// The machine that apps opened from the desktop (the palette, the
    /// overview, the dock) open in, by name; empty for this computer.
    /// derisk's own apps always open on this computer, which is the only
    /// place they can draw.
    pub open_apps_in: String,
}

/// Whether `name` can name a machine: what pm accepts for one, lowercase
/// letters, digits and dashes, a letter first, at most 32 long.
pub fn machine_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && name.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Notification preferences.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Notifications {
    /// Hide banners; they still go to the notification list.
    pub do_not_disturb: bool,
    /// Play a sound for new notifications.
    pub sounds: bool,
    /// Show message text on the lock screen.
    pub lock_screen_previews: bool,
}

/// Power preferences. Zero minutes means never.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Power {
    /// Minutes of inactivity before the screen dims.
    pub dim_after_min: u16,
    /// Minutes of inactivity before the session locks.
    pub lock_after_min: u16,
    /// Minutes of inactivity before suspending.
    pub suspend_after_min: u16,
    /// When low power mode turns on.
    pub low_power: LowPower,
}

/// All derisk preferences.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Appearance.
    pub appearance: Appearance,
    /// The top bar.
    pub top_bar: TopBar,
    /// Window frames.
    pub windows: WindowStyle,
    /// The desktop background.
    pub wallpaper: Wallpaper,
    /// Rebindable keyboard shortcuts.
    pub shortcuts: Shortcuts,
    /// Device access, history and trash.
    pub privacy: Privacy,
    /// The privacy display.
    pub privacy_display: PrivacyDisplay,
    /// The default browser and search engine.
    pub defaults: Defaults,
    /// Window management.
    pub desktop: DesktopPrefs,
    /// Keyboard and pointer.
    pub input: Input,
    /// Notifications.
    pub notifications: Notifications,
    /// Power.
    pub power: Power,
    /// Virtual machines.
    pub machines: MachinePrefs,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            appearance: Appearance {
                theme: ThemeId::AUTOMATIC,
                scheme: ColorScheme::Dark,
                accent: Accent::Teal,
                text_scale: 1.0,
                reduce_motion: false,
                blur: 6,
                panels: PanelOpacity {
                    top_bar: 70,
                    overview: 80,
                    snap_assist: 85,
                },
            },
            top_bar: TopBar {
                position: BarPosition::Top,
                autohide: false,
                search: true,
                app_name: true,
                date: true,
                battery: true,
                clock_24h: true,
            },
            windows: WindowStyle {
                corner_radius: 10,
                shadows: true,
            },
            wallpaper: Wallpaper::default(),
            shortcuts: Shortcuts::default(),
            privacy: Privacy {
                location: true,
                camera: true,
                microphone: true,
                remember_recent: true,
                empty_trash_days: 30,
                usage_ping: true,
            },
            privacy_display: PrivacyDisplay::default(),
            defaults: Defaults::default(),
            desktop: DesktopPrefs {
                layout: Layout::Tall,
                gaps: 8,
                workspaces: 9,
                profile: Profile::Automatic,
                vrr: Vrr::Automatic,
            },
            input: Input {
                natural_scroll: true,
                tap_to_click: true,
                repeat_delay_ms: 400,
                repeat_rate: 25,
            },
            notifications: Notifications {
                do_not_disturb: false,
                sounds: true,
                lock_screen_previews: false,
            },
            power: Power {
                dim_after_min: 5,
                lock_after_min: 10,
                suspend_after_min: 30,
                low_power: LowPower::OnBattery,
            },
            machines: MachinePrefs::default(),
        }
    }
}

/// A line of a settings file that was ignored while loading.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Warning {
    /// 1-based line number.
    pub line: usize,
    /// What was wrong.
    pub message: String,
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

macro_rules! enum_text {
    ($ty:ty { $($variant:ident => $text:literal),+ $(,)? }) => {
        impl $ty {
            /// The value as written in the settings file.
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }

            fn parse(text: &str) -> Option<Self> {
                match text { $($text => Some(Self::$variant),)+ _ => None }
            }
        }
    };
}

enum_text!(ColorScheme { Dark => "dark", Light => "light" });
enum_text!(Accent { Teal => "teal", Lime => "lime", Sky => "sky", Violet => "violet", Rose => "rose", Amber => "amber" });
enum_text!(Layout { Tall => "tall", Monocle => "monocle" });
enum_text!(Vrr { Automatic => "auto", On => "on", Off => "off" });
enum_text!(LowPower { Off => "off", OnBattery => "on_battery", On => "on" });
enum_text!(Profile { Automatic => "automatic", Phone => "phone", Tablet => "tablet", Desktop => "desktop" });
enum_text!(BarPosition { Top => "top", Bottom => "bottom" });
enum_text!(WallpaperKind { Default => "default", Color => "color", Gradient => "gradient", Image => "image", Slideshow => "slideshow", Video => "video", Web => "web" });
enum_text!(Fit { Fill => "fill", Fit => "fit", Stretch => "stretch", Center => "center", Tile => "tile" });

/// The line without its comment. `#` starts a comment at the start of a
/// line or as a word of its own after a space, so `#rrggbb` colors and
/// paths with `#` in them survive.
fn strip_comment(line: &str) -> &str {
    if line.trim_start().starts_with('#') {
        return "";
    }
    let bytes = line.as_bytes();
    let end = (1..bytes.len())
        .find(|&i| {
            bytes[i] == b'#'
                && bytes[i - 1].is_ascii_whitespace()
                && bytes.get(i + 1).is_none_or(|b| b.is_ascii_whitespace())
        })
        .unwrap_or(bytes.len());
    &line[..end]
}

fn parse_bool(text: &str) -> Option<bool> {
    match text {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn parse_in<T: std::str::FromStr + PartialOrd>(text: &str, min: T, max: T) -> Option<T> {
    text.parse().ok().filter(|v| *v >= min && *v <= max)
}

impl Settings {
    /// Parses `key = value` lines. `#` starts a comment (see
    /// [`strip_comment`]).
    ///
    /// Unknown keys and invalid values are skipped with a warning and leave
    /// the default in place, so an old or hand-edited file never blocks login.
    pub fn parse(text: &str) -> (Self, Vec<Warning>) {
        let mut settings = Self::default();
        let mut warnings = Vec::new();
        // A file without a `shortcut.edition` line predates it.
        settings.shortcuts.read_as(1);
        for (index, raw) in text.lines().enumerate() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            let warn = |message: String| Warning {
                line: index + 1,
                message,
            };
            let Some((key, value)) = line.split_once('=') else {
                warnings.push(warn(format!("expected `key = value`, found `{line}`")));
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            if !settings.set(key, value) {
                warnings.push(warn(format!("ignored `{key} = {value}`")));
            }
        }
        settings.shortcuts.upgrade();
        (settings, warnings)
    }

    /// Sets one key from its text value. Returns `false` if the key is
    /// unknown or the value is invalid, leaving the setting unchanged.
    pub fn set(&mut self, key: &str, value: &str) -> bool {
        fn put<T>(slot: &mut T, value: Option<T>) -> bool {
            value.map(|v| *slot = v).is_some()
        }
        let (a, d, i, n, p) = (
            &mut self.appearance,
            &mut self.desktop,
            &mut self.input,
            &mut self.notifications,
            &mut self.power,
        );
        let (b, win, w) = (&mut self.top_bar, &mut self.windows, &mut self.wallpaper);
        let pv = &mut self.privacy;
        if let Some(id) = key.strip_prefix("shortcut.") {
            return self.shortcuts.set_text(id, value);
        }
        match key {
            "appearance.theme" => put(&mut a.theme, ThemeId::parse(value)),
            "appearance.scheme" => put(&mut a.scheme, ColorScheme::parse(value)),
            "appearance.accent" => put(&mut a.accent, Accent::parse(value)),
            "appearance.text_scale" => put(&mut a.text_scale, parse_in(value, 0.75, 2.0)),
            "appearance.reduce_motion" => put(&mut a.reduce_motion, parse_bool(value)),
            "appearance.blur" => put(&mut a.blur, parse_in(value, 0, 10)),
            "appearance.top_bar_opacity" => put(&mut a.panels.top_bar, parse_in(value, 20, 100)),
            "appearance.overview_opacity" => put(&mut a.panels.overview, parse_in(value, 20, 100)),
            "appearance.snap_assist_opacity" => {
                put(&mut a.panels.snap_assist, parse_in(value, 20, 100))
            }
            "desktop.layout" => put(&mut d.layout, Layout::parse(value)),
            "desktop.gaps" => put(&mut d.gaps, parse_in(value, 0, 64)),
            "desktop.workspaces" => put(&mut d.workspaces, parse_in(value, 1, 9)),
            "desktop.profile" => put(&mut d.profile, Profile::parse(value)),
            "desktop.vrr" => put(&mut d.vrr, Vrr::parse(value)),
            "input.natural_scroll" => put(&mut i.natural_scroll, parse_bool(value)),
            "input.tap_to_click" => put(&mut i.tap_to_click, parse_bool(value)),
            "input.repeat_delay_ms" => put(&mut i.repeat_delay_ms, parse_in(value, 100, 1000)),
            "input.repeat_rate" => put(&mut i.repeat_rate, parse_in(value, 1, 60)),
            "notifications.do_not_disturb" => put(&mut n.do_not_disturb, parse_bool(value)),
            "notifications.sounds" => put(&mut n.sounds, parse_bool(value)),
            "notifications.lock_screen_previews" => {
                put(&mut n.lock_screen_previews, parse_bool(value))
            }
            "power.dim_after_min" => put(&mut p.dim_after_min, parse_in(value, 0, 240)),
            "power.lock_after_min" => put(&mut p.lock_after_min, parse_in(value, 0, 240)),
            "power.suspend_after_min" => put(&mut p.suspend_after_min, parse_in(value, 0, 240)),
            "power.low_power" => put(&mut p.low_power, LowPower::parse(value)),
            "top_bar.position" => put(&mut b.position, BarPosition::parse(value)),
            "top_bar.autohide" => put(&mut b.autohide, parse_bool(value)),
            "top_bar.search" => put(&mut b.search, parse_bool(value)),
            "top_bar.app_name" => put(&mut b.app_name, parse_bool(value)),
            "top_bar.date" => put(&mut b.date, parse_bool(value)),
            "top_bar.battery" => put(&mut b.battery, parse_bool(value)),
            "top_bar.clock_24h" => put(&mut b.clock_24h, parse_bool(value)),
            "windows.corner_radius" => put(&mut win.corner_radius, parse_in(value, 0, 20)),
            "windows.shadows" => put(&mut win.shadows, parse_bool(value)),
            "wallpaper.kind" => put(&mut w.kind, WallpaperKind::parse(value)),
            // Empty means none; anything else must be absolute, because the
            // session and the Settings app run from different directories.
            "wallpaper.path" => put(
                &mut w.path,
                Some(PathBuf::from(value)).filter(|p| value.is_empty() || p.is_absolute()),
            ),
            // Empty means none.
            "wallpaper.url" => put(
                &mut w.url,
                Some(value.to_owned()).filter(|u| u.is_empty() || web_background_url(u)),
            ),
            "wallpaper.fit" => put(&mut w.fit, Fit::parse(value)),
            "wallpaper.color" => put(&mut w.color, Rgb::parse(value)),
            "wallpaper.color2" => put(&mut w.color2, Rgb::parse(value)),
            "wallpaper.interval_min" => put(&mut w.interval_min, parse_in(value, 1, 1440)),
            "wallpaper.shuffle" => put(&mut w.shuffle, parse_bool(value)),
            "wallpaper.pause_when_covered" => put(&mut w.pause_when_covered, parse_bool(value)),
            "wallpaper.pause_in_low_power" => put(&mut w.pause_in_low_power, parse_bool(value)),
            "wallpaper.lock_screen" => put(
                &mut w.lock_screen,
                Some(PathBuf::from(value)).filter(|p| value.is_empty() || p.is_absolute()),
            ),
            "privacy.location" => put(&mut pv.location, parse_bool(value)),
            "privacy.camera" => put(&mut pv.camera, parse_bool(value)),
            "privacy.microphone" => put(&mut pv.microphone, parse_bool(value)),
            "privacy.remember_recent" => put(&mut pv.remember_recent, parse_bool(value)),
            "privacy.empty_trash_days" => put(&mut pv.empty_trash_days, parse_in(value, 0, 365)),
            "privacy.usage_ping" => put(&mut pv.usage_ping, parse_bool(value)),
            "privacy_display.enabled" => put(&mut self.privacy_display.enabled, parse_bool(value)),
            "privacy_display.passwords" => {
                put(&mut self.privacy_display.passwords, parse_bool(value))
            }
            "privacy_display.notifications" => {
                put(&mut self.privacy_display.notifications, parse_bool(value))
            }
            "privacy_display.maximum" => put(&mut self.privacy_display.maximum, parse_bool(value)),
            // Comma-separated app IDs; empty means none.
            "privacy_display.apps" => put(
                &mut self.privacy_display.apps,
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .map(|id| app_id(id).then(|| id.to_owned()))
                    .collect::<Option<Vec<_>>>(),
            ),
            // A desktop file ID, which is a file name: never a path.
            "defaults.browser" => put(
                &mut self.defaults.browser,
                Some(value.to_owned()).filter(|v| {
                    v.is_empty() || (v.ends_with(".desktop") && !v.contains(['/', ';', '\\']))
                }),
            ),
            "machines.open_apps_in" => put(
                &mut self.machines.open_apps_in,
                Some(value.to_owned()).filter(|v| v.is_empty() || machine_name(v)),
            ),
            "defaults.search" => put(
                &mut self.defaults.search,
                if value.is_empty() {
                    Some(None)
                } else {
                    crate::choice::SearchEngine::parse(value).map(Some)
                },
            ),
            _ => false,
        }
    }

    /// Serializes every setting, one `key = value` per line.
    pub fn to_text(&self) -> String {
        let (a, d, i, n, p) = (
            &self.appearance,
            &self.desktop,
            &self.input,
            &self.notifications,
            &self.power,
        );
        let (b, win, w) = (&self.top_bar, &self.windows, &self.wallpaper);
        format!(
            "# derisk settings, written by the Settings app.\n\
             appearance.theme = {}\n\
             appearance.scheme = {}\n\
             appearance.accent = {}\n\
             appearance.text_scale = {}\n\
             appearance.reduce_motion = {}\n\
             appearance.blur = {}\n\
             appearance.top_bar_opacity = {}\n\
             appearance.overview_opacity = {}\n\
             appearance.snap_assist_opacity = {}\n\
             desktop.layout = {}\n\
             desktop.gaps = {}\n\
             desktop.workspaces = {}\n\
             desktop.profile = {}\n\
             desktop.vrr = {}\n\
             input.natural_scroll = {}\n\
             input.tap_to_click = {}\n\
             input.repeat_delay_ms = {}\n\
             input.repeat_rate = {}\n\
             notifications.do_not_disturb = {}\n\
             notifications.sounds = {}\n\
             notifications.lock_screen_previews = {}\n\
             power.dim_after_min = {}\n\
             power.lock_after_min = {}\n\
             power.suspend_after_min = {}\n\
             power.low_power = {}\n\
             top_bar.position = {}\n\
             top_bar.autohide = {}\n\
             top_bar.search = {}\n\
             top_bar.app_name = {}\n\
             top_bar.date = {}\n\
             top_bar.battery = {}\n\
             top_bar.clock_24h = {}\n\
             windows.corner_radius = {}\n\
             windows.shadows = {}\n\
             wallpaper.kind = {}\n\
             wallpaper.path = {}\n\
             wallpaper.url = {}\n\
             wallpaper.fit = {}\n\
             wallpaper.color = {}\n\
             wallpaper.color2 = {}\n\
             wallpaper.interval_min = {}\n\
             wallpaper.shuffle = {}\n\
             wallpaper.pause_when_covered = {}\n\
             wallpaper.pause_in_low_power = {}\n\
             wallpaper.lock_screen = {}\n\
             privacy.location = {}\n\
             privacy.camera = {}\n\
             privacy.microphone = {}\n\
             privacy.remember_recent = {}\n\
             privacy.empty_trash_days = {}\n\
             privacy.usage_ping = {}\n\
             privacy_display.enabled = {}\n\
             privacy_display.passwords = {}\n\
             privacy_display.notifications = {}\n\
             privacy_display.apps = {}\n\
             privacy_display.maximum = {}\n\
             defaults.browser = {}\n\
             defaults.search = {}\n\
             machines.open_apps_in = {}\n\
             {}",
            a.theme.as_str(),
            a.scheme.as_str(),
            a.accent.as_str(),
            a.text_scale,
            a.reduce_motion,
            a.blur,
            a.panels.top_bar,
            a.panels.overview,
            a.panels.snap_assist,
            d.layout.as_str(),
            d.gaps,
            d.workspaces,
            d.profile.as_str(),
            d.vrr.as_str(),
            i.natural_scroll,
            i.tap_to_click,
            i.repeat_delay_ms,
            i.repeat_rate,
            n.do_not_disturb,
            n.sounds,
            n.lock_screen_previews,
            p.dim_after_min,
            p.lock_after_min,
            p.suspend_after_min,
            p.low_power.as_str(),
            b.position.as_str(),
            b.autohide,
            b.search,
            b.app_name,
            b.date,
            b.battery,
            b.clock_24h,
            win.corner_radius,
            win.shadows,
            w.kind.as_str(),
            w.path.display(),
            w.url,
            w.fit.as_str(),
            w.color.to_hex(),
            w.color2.to_hex(),
            w.interval_min,
            w.shuffle,
            w.pause_when_covered,
            w.pause_in_low_power,
            w.lock_screen.display(),
            self.privacy.location,
            self.privacy.camera,
            self.privacy.microphone,
            self.privacy.remember_recent,
            self.privacy.empty_trash_days,
            self.privacy.usage_ping,
            self.privacy_display.enabled,
            self.privacy_display.passwords,
            self.privacy_display.notifications,
            self.privacy_display.apps.join(","),
            self.privacy_display.maximum,
            self.defaults.browser,
            self.defaults.search.map_or("", |e| e.as_str()),
            self.machines.open_apps_in,
            self.shortcuts.to_text(),
        )
    }

    /// Loads a settings file. A missing file yields the defaults.
    pub fn load(path: &Path) -> io::Result<(Self, Vec<Warning>)> {
        match fs::read_to_string(path) {
            Ok(text) => Ok(Self::parse(&text)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok((Self::default(), vec![])),
            Err(error) => Err(error),
        }
    }

    /// Writes the settings file atomically, creating its directory.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut temporary = path.as_os_str().to_owned();
        temporary.push(".tmp");
        let temporary = PathBuf::from(temporary);
        fs::write(&temporary, self.to_text())?;
        fs::rename(&temporary, path)
    }

    /// The theme these settings select, looking theme files up in
    /// `library` (usually [`mcsapi_theme::Library::xdg`]`("derisk")`).
    ///
    /// An automatic theme is the built-in `derisk-dark` or `derisk-light`
    /// with the chosen accent, kept legible on the background. A named theme
    /// brings its own accent; if it cannot be loaded, the automatic theme is
    /// used and the error is returned alongside it.
    pub fn theme_spec(
        &self,
        library: &mcsapi_theme::Library,
    ) -> (mcsapi_theme::Theme, Option<mcsapi_theme::LoadError>) {
        let a = &self.appearance;
        let automatic = || {
            let base = match a.scheme {
                ColorScheme::Dark => mcsapi_theme::Theme::dark(),
                ColorScheme::Light => mcsapi_theme::Theme::light(),
            };
            base.with_accent(a.accent.spec_color())
        };
        if a.theme.is_automatic() {
            return (automatic(), None);
        }
        match library.load(a.theme.as_str()) {
            Ok(parsed) => (parsed.theme, None),
            Err(error) => (automatic(), Some(error)),
        }
    }

    /// The shell and app colors these settings select, with theme files from
    /// the XDG data directories.
    pub fn theme(&self) -> Theme {
        Theme::from(&self.theme_spec(&mcsapi_theme::Library::xdg("derisk")).0)
    }
}

/// Where derisk keeps its settings: `$XDG_CONFIG_HOME/derisk/settings.conf`,
/// falling back to `~/.config`. `None` when neither variable is set.
pub fn default_path() -> Option<PathBuf> {
    let config = derisk_sys::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| Path::new(dir).is_absolute())
        .map(PathBuf::from)
        .or_else(|| derisk_sys::env::var_os("HOME").map(|home| Path::new(&home).join(".config")))?;
    Some(config.join("derisk").join("settings.conf"))
}
