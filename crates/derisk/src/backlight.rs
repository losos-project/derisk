//! Dimming the screen while a password is typed, on a display with a
//! backlight.
//!
//! Someone looking over a shoulder reads a dark screen less easily, so the
//! lock screen, the login screen and the polkit dialog turn the backlight
//! down from the first character of a password until the field has stayed
//! empty for [`HOLD`] or the prompt is gone, then put it back where it was.
//! The panel's own light is what changes, so only a display with a
//! backlight (`/sys/class/backlight`, as laptops and phones have) dims; on
//! one without, such as a desktop's monitor, nothing happens.
//!
//! The brightness goes through logind's `SetBrightness` on the caller's own
//! session, which lets the session's user change it without root, the same
//! way systemd-backlight and every desktop's brightness keys do.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Where the kernel lists backlights.
pub const CLASS_DIR: &str = "/sys/class/backlight";

/// How long the field stays empty before the brightness comes back, so a
/// wrong password and the next try don't flash the screen bright between
/// them.
pub const HOLD: Duration = Duration::from_secs(3);

/// The share of the panel's range the screen dims to: dark enough to be hard
/// to read from the side, light enough to type by.
const DIM_SHARE: u32 = 15;

/// A display's backlight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backlight {
    /// Its name under [`CLASS_DIR`], which logind takes.
    pub name: String,
    /// Its brightest level.
    pub max: u32,
    dir: PathBuf,
}

impl Backlight {
    /// The level it was last set to; `None` when it cannot be read.
    pub fn brightness(&self) -> Option<u32> {
        read_u32(&self.dir.join("brightness"))
    }
}

fn read_u32(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// The backlight in `class_dir` that controls the panel, if there is one.
/// The kernel can list several for one panel; as systemd-backlight and
/// every desktop do, the firmware's interface wins over the platform
/// driver's, and that over the GPU's raw register.
pub fn find(class_dir: &Path) -> Option<Backlight> {
    let rank = |kind: &str| match kind {
        "firmware" => 0,
        "platform" => 1,
        "raw" => 2,
        _ => 3,
    };
    std::fs::read_dir(class_dir)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let dir = entry.path();
            let max = read_u32(&dir.join("max_brightness")).filter(|&m| m > 0)?;
            let kind = std::fs::read_to_string(dir.join("type")).unwrap_or_default();
            let name = entry.file_name().into_string().ok()?;
            Some((rank(kind.trim()), Backlight { name, max, dir }))
        })
        .min_by(|(a, x), (b, y)| a.cmp(b).then_with(|| x.name.cmp(&y.name)))
        .map(|(_, backlight)| backlight)
}

/// The level to dim to from `current`: [`DIM_SHARE`] of `max`, never
/// brighter than it already is and never off.
pub fn dim_level(current: u32, max: u32) -> u32 {
    (max.saturating_mul(DIM_SHARE) / 100).max(1).min(current)
}

/// `busctl` arguments that set backlight `name` to `value` through logind,
/// on the caller's own session.
pub fn set_brightness_argv(name: &str, value: u32) -> Vec<String> {
    [
        "busctl",
        "--system",
        "call",
        "org.freedesktop.login1",
        "/org/freedesktop/login1/session/auto",
        "org.freedesktop.login1.Session",
        "SetBrightness",
        "ssu",
        "backlight",
        name,
        &value.to_string(),
    ]
    .map(str::to_owned)
    .to_vec()
}

/// Sets backlight `name` to `level` through logind, saying why not when it
/// cannot: the session is not active, or logind refused.
pub fn set(name: &str, level: u32) -> Result<(), String> {
    let output =
        crate::systemd::run(&set_brightness_argv(name, level)).map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

/// What a password field is doing, frame by frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Field {
    /// A password prompt is on screen at all.
    pub shown: bool,
    /// Its field holds something.
    pub typed: bool,
    /// What was typed is being checked.
    pub checking: bool,
}

/// The dimming for one screen's password fields.
#[derive(Debug)]
pub struct PasswordDim {
    backlight: Option<Backlight>,
    /// The level before dimming and the level dimmed to, while dimmed.
    dimmed: Option<(u32, u32)>,
    /// Since when the field has been empty, while dimmed.
    empty_since: Option<Instant>,
}

impl PasswordDim {
    /// Dims `backlight`, or nothing without one.
    pub fn new(backlight: Option<Backlight>) -> Self {
        Self {
            backlight,
            dimmed: None,
            empty_since: None,
        }
    }

    /// The panel's backlight, from [`CLASS_DIR`].
    pub fn system() -> Self {
        Self::new(find(Path::new(CLASS_DIR)))
    }

    /// Whether the screen is dimmed now.
    pub fn is_dimmed(&self) -> bool {
        self.dimmed.is_some()
    }

    /// Follows `field` at `now`. Returns the level to set the backlight to
    /// when it should change, with the backlight's name.
    pub fn update(&mut self, field: Field, now: Instant) -> Option<(String, u32)> {
        let backlight = self.backlight.as_ref()?;
        let typing = field.shown && (field.typed || (field.checking && self.dimmed.is_some()));
        if typing {
            self.empty_since = None;
            if self.dimmed.is_some() {
                return None;
            }
            let current = backlight.brightness()?;
            let level = dim_level(current, backlight.max);
            self.dimmed = Some((current, level));
            return (level < current).then(|| (backlight.name.clone(), level));
        }
        self.dimmed?;
        if field.shown {
            let since = *self.empty_since.get_or_insert(now);
            if now.duration_since(since) < HOLD {
                return None;
            }
        }
        self.restore()
    }

    /// Ends the dimming: the level to put back, unless the brightness was
    /// changed meanwhile (the brightness keys), which then stays.
    pub fn restore(&mut self) -> Option<(String, u32)> {
        self.empty_since = None;
        let (before, level) = self.dimmed.take()?;
        let backlight = self.backlight.as_ref()?;
        (before != level && backlight.brightness() == Some(level))
            .then(|| (backlight.name.clone(), before))
    }
}

/// A screen that goes away dimmed (the greeter exiting as the session
/// starts) leaves the backlight as it found it.
impl Drop for PasswordDim {
    fn drop(&mut self) {
        if let Some((name, level)) = self.restore() {
            let _ = set(&name, level);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("derisk-backlight-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn device(class: &Path, name: &str, kind: &str, max: u32, level: u32) -> PathBuf {
        let dir = class.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("type"), format!("{kind}\n")).unwrap();
        std::fs::write(dir.join("max_brightness"), format!("{max}\n")).unwrap();
        std::fs::write(dir.join("brightness"), format!("{level}\n")).unwrap();
        dir
    }

    #[test]
    fn the_firmware_backlight_wins() {
        let dir = class("rank");
        device(&dir, "intel_backlight", "raw", 96000, 50000);
        device(&dir, "acpi_video0", "firmware", 100, 80);
        let found = find(&dir).unwrap();
        assert_eq!((found.name.as_str(), found.max), ("acpi_video0", 100));
        assert_eq!(found.brightness(), Some(80));
        assert!(find(&class("none")).is_none());
    }

    #[test]
    fn dimming_goes_down_never_up_and_never_off() {
        assert_eq!(dim_level(100, 100), 15);
        assert_eq!(dim_level(10, 100), 10);
        assert_eq!(dim_level(255, 3), 1);
        assert_eq!(dim_level(0, 100), 0);
    }

    #[test]
    fn typing_dims_and_an_empty_field_restores_after_a_while() {
        let dir = class("dim");
        let panel = device(&dir, "panel", "raw", 1000, 800);
        let mut dim = PasswordDim::new(find(&dir));
        let t0 = Instant::now();
        let shown = Field {
            shown: true,
            ..Field::default()
        };
        let typed = Field {
            typed: true,
            ..shown
        };
        // A prompt alone, before anything is typed, leaves the screen be.
        assert_eq!(dim.update(shown, t0), None);
        assert_eq!(dim.update(typed, t0), Some(("panel".into(), 150)));
        std::fs::write(panel.join("brightness"), "150").unwrap();
        assert_eq!(dim.update(typed, t0), None);
        // Checking what was typed keeps it dim; a wrong answer empties the
        // field, which stays dim for a while in case of another try.
        let checking = Field {
            checking: true,
            ..shown
        };
        assert_eq!(dim.update(checking, t0), None);
        assert_eq!(dim.update(shown, t0 + Duration::from_secs(1)), None);
        assert!(dim.is_dimmed());
        assert_eq!(
            dim.update(shown, t0 + Duration::from_secs(1) + HOLD),
            Some(("panel".into(), 800))
        );
        assert!(!dim.is_dimmed());
    }

    #[test]
    fn the_prompt_going_restores_at_once_unless_the_keys_moved_it() {
        let dir = class("gone");
        let panel = device(&dir, "panel", "raw", 100, 60);
        let mut dim = PasswordDim::new(find(&dir));
        let t0 = Instant::now();
        let typed = Field {
            shown: true,
            typed: true,
            checking: false,
        };
        assert_eq!(dim.update(typed, t0), Some(("panel".into(), 15)));
        std::fs::write(panel.join("brightness"), "15").unwrap();
        assert_eq!(dim.update(Field::default(), t0), Some(("panel".into(), 60)));
        std::fs::write(panel.join("brightness"), "60").unwrap();
        // Dimmed again, then the brightness keys turned it up: it stays.
        assert!(dim.update(typed, t0).is_some());
        std::fs::write(panel.join("brightness"), "40").unwrap();
        assert_eq!(dim.update(Field::default(), t0), None);
    }

    #[test]
    fn without_a_backlight_nothing_happens() {
        let mut dim = PasswordDim::new(None);
        let typed = Field {
            shown: true,
            typed: true,
            checking: false,
        };
        assert_eq!(dim.update(typed, Instant::now()), None);
        assert!(!dim.is_dimmed());
    }

    #[test]
    fn logind_sets_it_on_the_callers_session() {
        let argv = set_brightness_argv("panel", 15);
        assert_eq!(argv[4], "/org/freedesktop/login1/session/auto");
        assert_eq!(
            &argv[6..],
            ["SetBrightness", "ssu", "backlight", "panel", "15"]
        );
    }
}
