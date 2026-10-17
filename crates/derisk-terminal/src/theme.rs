//! The terminal's colours and font, from the desktop theme.
//!
//! derisk publishes its theme for apps outside its own process as
//! `$XDG_RUNTIME_DIR/derisk/theme.json` (derisk's `theme` module), rewritten
//! by rename whenever it changes. The terminal reads that file, so it follows
//! whatever theme Settings picked, a theme file included, without linking
//! Settings. Outside a derisk session there is no file and the built-in dark
//! theme stands in.
//!
//! The theme names five shell colours, not sixteen ANSI ones, so the ANSI
//! palette is fixed per scheme and takes red and cyan from the theme's
//! destructive and accent colours: a restyled theme still recolours the
//! terminal's background, text, cursor, selection and those two.

use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

use alacritty_terminal::{term::color::COUNT, vte::ansi::Rgb};
use mcsapi_theme::{Color, Scheme, Theme};

/// Everything the terminal takes from the theme.
#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    /// Light or dark, which picks the ANSI palette.
    pub scheme: Scheme,
    /// The page behind the text.
    pub background: Rgb,
    /// Text in the default colour.
    pub foreground: Rgb,
    /// The cursor.
    pub accent: Rgb,
    /// ANSI red.
    pub destructive: Rgb,
    /// Behind selected text.
    pub selection: Rgb,
    /// Font family to look up, before its Nerd Font variants.
    pub family: String,
    /// Font size in logical pixels.
    pub size: f32,
}

impl Look {
    /// The look of a theme.
    pub fn from_theme(theme: &Theme) -> Self {
        let p = &theme.palette;
        Self {
            scheme: theme.scheme,
            background: rgb(p.background),
            foreground: rgb(p.foreground),
            accent: rgb(p.accent),
            destructive: rgb(p.destructive),
            selection: rgb(theme.tokens().selection),
            family: theme.fonts.monospace.clone(),
            size: theme.fonts.monospace_size,
        }
    }

    /// The look in a published `theme.json`, or `None` when it does not
    /// parse. A missing key keeps the built-in dark theme's value, so an
    /// older derisk's file still works.
    pub fn from_json(text: &str) -> Option<Self> {
        let json: serde_json::Value = serde_json::from_str(text).ok()?;
        let mut look = Self::from_theme(&Theme::dark());
        if let Some(scheme) = json["scheme"].as_str().and_then(Scheme::parse) {
            look.scheme = scheme;
        }
        let color = |value: &serde_json::Value| {
            value
                .as_str()
                .and_then(|text| text.parse::<Color>().ok())
                .map(rgb)
        };
        let palette = &json["palette"];
        for (key, slot) in [
            ("background", &mut look.background),
            ("foreground", &mut look.foreground),
            ("accent", &mut look.accent),
            ("destructive", &mut look.destructive),
        ] {
            if let Some(value) = color(&palette[key]) {
                *slot = value;
            }
        }
        if let Some(value) = color(&json["tokens"]["selection"]) {
            look.selection = value;
        }
        if let Some(family) = json["fonts"]["monospace"]
            .as_str()
            .filter(|f| !f.is_empty())
        {
            look.family = family.to_owned();
        }
        if let Some(size) = json["fonts"]["monospace_size"].as_f64() {
            // A theme file can say anything; a terminal at 0 or 500 pixels
            // is not one anybody meant.
            look.size = (size as f32).clamp(6.0, 72.0);
        }
        Some(look)
    }

    /// The 269 colours alacritty_terminal indexes cells by: the sixteen ANSI
    /// colours, the 6×6×6 cube and grey ramp xterm defines, then the
    /// foreground, background, cursor and dim variants.
    pub fn palette(&self) -> [Rgb; COUNT] {
        let mut colors = [Rgb::default(); COUNT];
        let ansi = match self.scheme {
            Scheme::Dark => DARK,
            Scheme::Light => LIGHT,
        };
        for (slot, value) in colors.iter_mut().zip(ansi) {
            *slot = hex(value);
        }
        colors[1] = self.destructive;
        colors[6] = self.accent;
        for r in 0..6u8 {
            for g in 0..6u8 {
                for b in 0..6u8 {
                    let level = |v: u8| if v == 0 { 0 } else { v * 40 + 55 };
                    colors[16 + usize::from(r * 36 + g * 6 + b)] = Rgb {
                        r: level(r),
                        g: level(g),
                        b: level(b),
                    };
                }
            }
        }
        for i in 0..24u8 {
            let v = 8 + i * 10;
            colors[232 + usize::from(i)] = Rgb { r: v, g: v, b: v };
        }
        colors[256] = self.foreground;
        colors[257] = self.background;
        colors[258] = self.accent;
        // Dim black through dim white, as alacritty derives them: two thirds
        // of the normal colour.
        for i in 0..8 {
            colors[259 + i] = dim(colors[i]);
        }
        colors[267] = self.foreground;
        colors[268] = dim(self.foreground);
        colors
    }
}

/// The ANSI colours on a dark background: the slate, green, amber, blue and
/// violet of the LosOS web interface, bright variants a step lighter.
const DARK: [u32; 16] = [
    0x1e2832, 0xdc2626, 0x22c55e, 0xeab308, 0x3b82f6, 0xa855f7, 0x48b3c0, 0xcbd5e1, 0x475569,
    0xf87171, 0x4ade80, 0xfacc15, 0x60a5fa, 0xc084fc, 0x67e8f9, 0xf8fafc,
];

/// The same hues a step darker, so each stays legible on a light page.
const LIGHT: [u32; 16] = [
    0x1e293b, 0xdc2626, 0x15803d, 0xa16207, 0x1d4ed8, 0x7e22ce, 0x0e7490, 0x94a3b8, 0x64748b,
    0xb91c1c, 0x16a34a, 0xca8a04, 0x2563eb, 0x9333ea, 0x0891b2, 0xcbd5e1,
];

fn hex(value: u32) -> Rgb {
    Rgb {
        r: (value >> 16) as u8,
        g: (value >> 8) as u8,
        b: value as u8,
    }
}

fn rgb(color: Color) -> Rgb {
    Rgb {
        r: color.r,
        g: color.g,
        b: color.b,
    }
}

fn dim(color: Rgb) -> Rgb {
    let d = |v: u8| (u16::from(v) * 2 / 3) as u8;
    Rgb {
        r: d(color.r),
        g: d(color.g),
        b: d(color.b),
    }
}

/// `$XDG_RUNTIME_DIR/derisk/theme.json`, where derisk publishes its theme.
pub fn published_path() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("derisk").join("theme.json"))
}

/// The published theme, re-read only when its file changes.
pub struct Watch {
    path: Option<PathBuf>,
    modified: Option<Option<SystemTime>>,
}

impl Watch {
    /// Watches `path`, usually [`published_path`].
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            modified: None,
        }
    }

    /// The look on the first call and whenever the file's modification time
    /// changes (including when it appears or goes away), else `None`.
    pub fn poll(&mut self) -> Option<Look> {
        let modified = self.path.as_deref().and_then(modified);
        if self.modified == Some(modified) {
            return None;
        }
        self.modified = Some(modified);
        let look = self
            .path
            .as_deref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| Look::from_json(&text));
        Some(look.unwrap_or_else(|| Look::from_theme(&Theme::dark())))
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_derisk_publishes() {
        let json = mcsapi_theme::export::json("derisk-light", &Theme::light());
        let look = Look::from_json(&json).expect("derisk's own JSON parses");
        assert_eq!(look, Look::from_theme(&Theme::light()));
    }

    #[test]
    fn keeps_defaults_for_what_is_missing() {
        let look = Look::from_json(r##"{"palette":{"accent":"#ff0000"}}"##).unwrap();
        assert_eq!(look.accent, Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(look.background, Look::from_theme(&Theme::dark()).background);
        assert_eq!(Look::from_json("not json"), None);
    }

    #[test]
    fn palette_has_xterm_cube_and_theme_colors() {
        let look = Look::from_theme(&Theme::dark());
        let colors = look.palette();
        assert_eq!(colors[16], Rgb { r: 0, g: 0, b: 0 });
        assert_eq!(
            colors[231],
            Rgb {
                r: 255,
                g: 255,
                b: 255
            }
        );
        assert_eq!(colors[232], Rgb { r: 8, g: 8, b: 8 });
        assert_eq!(colors[256], look.foreground);
        assert_eq!(colors[257], look.background);
        assert_eq!(colors[6], look.accent);
    }

    #[test]
    fn watch_follows_the_file() {
        let dir =
            std::env::temp_dir().join(format!("derisk-terminal-theme-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("theme.json");
        let mut watch = Watch::new(Some(path.clone()));
        assert_eq!(watch.poll(), Some(Look::from_theme(&Theme::dark())));
        assert_eq!(watch.poll(), None);
        std::fs::write(
            &path,
            mcsapi_theme::export::json("derisk-light", &Theme::light()),
        )
        .unwrap();
        assert_eq!(watch.poll().map(|l| l.scheme), Some(Scheme::Light));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
