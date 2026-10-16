//! Rebindable keyboard shortcuts, written `shortcut.<id> = Super+Shift+M`.
//!
//! A shortcut is a sequence of up to [`Keys::MAX`] chords, as in VS Code
//! and Zed: `shortcut.snap_left = Super+W Left` is Super+W, then Left.
//! After the first chord the shell waits for the next one and shows what
//! can follow it, so a family of related shortcuts can share one easy
//! prefix instead of each taking a chord of its own.
//!
//! The families (Super+1…9, Super+Shift+1…9, Super+arrows, Alt+Tab) stay
//! fixed: they are one rule over ten keys each, and a per-key table for them
//! would be noise.

use std::fmt;

/// A key, independent of keyboard layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum KeyName {
    /// A letter, lowercase.
    Letter(char),
    /// A digit 0–9.
    Digit(u8),
    /// Enter or Return.
    Enter,
    /// Tab.
    Tab,
    /// Escape.
    Escape,
    /// The space bar.
    Space,
    /// Left arrow.
    Left,
    /// Right arrow.
    Right,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
    /// `/`, which with Shift is `?` on most layouts.
    Slash,
    /// `,`.
    Comma,
    /// `.`.
    Period,
    /// `-`.
    Minus,
    /// `=`.
    Equal,
    /// Backspace.
    Backspace,
    /// Delete.
    Delete,
    /// Home.
    Home,
    /// End.
    End,
    /// Page Up.
    PageUp,
    /// Page Down.
    PageDown,
    /// Print Screen.
    Print,
    /// F1–F12.
    Function(u8),
}

impl KeyName {
    /// Whether the key never types text, so it may start a shortcut without
    /// a modifier (Print, F1…F12) and no app loses a character to it.
    pub fn types_nothing(self) -> bool {
        matches!(self, Self::Print | Self::Function(_))
    }

    fn parse(text: &str) -> Option<Self> {
        Some(match text.to_ascii_lowercase().as_str() {
            "enter" | "return" => Self::Enter,
            "tab" => Self::Tab,
            "escape" | "esc" => Self::Escape,
            "space" => Self::Space,
            "left" => Self::Left,
            "right" => Self::Right,
            "up" => Self::Up,
            "down" => Self::Down,
            "/" | "slash" => Self::Slash,
            "," | "comma" => Self::Comma,
            "." | "period" => Self::Period,
            "-" | "minus" => Self::Minus,
            "=" | "equal" => Self::Equal,
            "backspace" => Self::Backspace,
            "delete" | "del" => Self::Delete,
            "home" => Self::Home,
            "end" => Self::End,
            "pageup" => Self::PageUp,
            "pagedown" => Self::PageDown,
            "print" | "printscreen" => Self::Print,
            k => {
                let mut chars = k.chars();
                match (chars.next(), chars.next()) {
                    (Some(c @ 'a'..='z'), None) => Self::Letter(c),
                    (Some(c @ '0'..='9'), None) => Self::Digit(c as u8 - b'0'),
                    (Some('f'), Some(_)) => match k[1..].parse() {
                        Ok(n @ 1..=12) => Self::Function(n),
                        _ => return None,
                    },
                    _ => return None,
                }
            }
        })
    }
}

impl fmt::Display for KeyName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Letter(c) => write!(f, "{}", c.to_ascii_uppercase()),
            Self::Digit(d) => write!(f, "{d}"),
            Self::Function(n) => write!(f, "F{n}"),
            other => f.write_str(match other {
                Self::Enter => "Enter",
                Self::Tab => "Tab",
                Self::Escape => "Escape",
                Self::Space => "Space",
                Self::Left => "Left",
                Self::Right => "Right",
                Self::Up => "Up",
                Self::Down => "Down",
                Self::Slash => "/",
                Self::Comma => ",",
                Self::Period => ".",
                Self::Minus => "-",
                Self::Equal => "=",
                Self::Backspace => "Backspace",
                Self::Delete => "Delete",
                Self::Home => "Home",
                Self::End => "End",
                Self::PageUp => "PageUp",
                Self::PageDown => "PageDown",
                Self::Print => "Print",
                Self::Letter(_) | Self::Digit(_) | Self::Function(_) => unreachable!(),
            }),
        }
    }
}

/// A key with modifiers, such as `Super+Shift+M`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct Chord {
    /// Super (logo) key.
    pub logo: bool,
    /// Shift.
    pub shift: bool,
    /// Control.
    pub ctrl: bool,
    /// Alt.
    pub alt: bool,
    /// The key.
    pub key: KeyName,
}

impl Chord {
    /// A key with no modifiers.
    pub const fn bare(key: KeyName) -> Self {
        Self {
            logo: false,
            shift: false,
            ctrl: false,
            alt: false,
            key,
        }
    }

    /// Parses `Super+Shift+M`: modifiers in any order and case, then one key.
    /// A chord that starts a shortcut needs Super, Ctrl or Alt, so a plain
    /// letter can never be taken away from the focused app; only keys that
    /// type nothing, such as Print and F1…F12, may go without.
    pub fn parse(text: &str) -> Option<Self> {
        Self::parse_any(text).filter(|c| c.can_start())
    }

    /// Whether the chord may be the first of a shortcut (see [`Chord::parse`]).
    pub fn can_start(&self) -> bool {
        self.logo || self.ctrl || self.alt || self.key.types_nothing()
    }

    /// Parses a chord with or without modifiers: what may follow the first
    /// chord of a sequence, when the shell already holds the keyboard.
    fn parse_any(text: &str) -> Option<Self> {
        let mut parts: Vec<&str> = text.split('+').map(str::trim).collect();
        let mut chord = Self::bare(KeyName::parse(parts.pop()?)?);
        for part in parts {
            let flag = match part.to_ascii_lowercase().as_str() {
                "super" | "logo" | "meta" | "win" => &mut chord.logo,
                "shift" => &mut chord.shift,
                "ctrl" | "control" => &mut chord.ctrl,
                "alt" => &mut chord.alt,
                _ => return None,
            };
            if std::mem::replace(flag, true) {
                return None;
            }
        }
        Some(chord)
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (on, name) in [
            (self.logo, "Super+"),
            (self.ctrl, "Ctrl+"),
            (self.alt, "Alt+"),
            (self.shift, "Shift+"),
        ] {
            if on {
                f.write_str(name)?;
            }
        }
        write!(f, "{}", self.key)
    }
}

/// A shortcut's keys: one chord, or a sequence of them pressed one after
/// another (`Super+W Left`). Fixed-size, so [`Shortcuts`] stays `Copy`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct Keys {
    chords: [Chord; Keys::MAX],
    len: u8,
}

impl Keys {
    /// The longest sequence: three chords is already more than anyone
    /// remembers, and VS Code stops at two.
    pub const MAX: usize = 3;

    /// One chord.
    pub fn one(chord: Chord) -> Self {
        Self {
            chords: [chord; Self::MAX],
            len: 1,
        }
    }

    /// Up to [`Keys::MAX`] chords, the first of which can start a shortcut
    /// ([`Chord::can_start`]).
    pub fn new(chords: &[Chord]) -> Option<Self> {
        let first = *chords.first()?;
        if chords.len() > Self::MAX || !first.can_start() {
            return None;
        }
        let mut keys = Self::one(first);
        keys.chords[..chords.len()].copy_from_slice(chords);
        keys.len = chords.len() as u8;
        Some(keys)
    }

    /// Parses chords separated by spaces, as VS Code writes them, or by
    /// commas too: `Super+W Left`, `Ctrl+K, Ctrl+S`. The first chord needs
    /// a modifier, as [`Chord::parse`] says; the rest may be bare keys,
    /// since the shell already holds the keyboard by then.
    pub fn parse(text: &str) -> Option<Self> {
        let chords: Vec<Chord> = text
            .split_whitespace()
            .filter(|c| *c != ",")
            .map(|c| {
                // A separating comma, not the comma key (`Super+,`).
                let c = match c.strip_suffix(',') {
                    Some(rest) if !rest.is_empty() && !rest.ends_with('+') => rest,
                    _ => c,
                };
                Chord::parse_any(c)
            })
            .collect::<Option<_>>()?;
        Self::new(&chords)
    }

    /// The chords, in the order they are pressed.
    pub fn chords(&self) -> &[Chord] {
        &self.chords[..usize::from(self.len)]
    }

    /// Whether these keys begin with `prefix` (or are it).
    pub fn starts_with(&self, prefix: &[Chord]) -> bool {
        self.chords().starts_with(prefix)
    }
}

impl From<Chord> for Keys {
    fn from(chord: Chord) -> Self {
        Self::one(chord)
    }
}

impl fmt::Display for Keys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, chord) in self.chords().iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{chord}")?;
        }
        Ok(())
    }
}

/// A group of shortcuts, for the Settings page and the shortcuts overlay.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Group {
    /// The palette, the overview and the desktop as a whole.
    Desktop,
    /// The focused window.
    Windows,
    /// Opening apps.
    Apps,
    /// Locking, suspending, logging out.
    Session,
}

impl Group {
    /// Every group, in the order they are shown.
    pub const ALL: [Self; 4] = [Self::Desktop, Self::Windows, Self::Apps, Self::Session];

    /// The group's heading.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Desktop => "Desktop",
            Self::Windows => "Windows",
            Self::Apps => "Apps",
            Self::Session => "Session",
        }
    }
}

macro_rules! shortcuts {
    ($($(#[doc = $doc:literal])* $name:ident = $id:literal, $group:ident, $label:literal, $keys:literal;)*) => {
        /// A rebindable shortcut.
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
        pub enum Shortcut {
            $($(#[doc = $doc])* $name,)*
        }

        impl Shortcut {
            /// Every shortcut, in the Settings app's order.
            pub const ALL: [Self; [$(Self::$name),*].len()] = [$(Self::$name),*];

            /// The ID in `shortcut.<id>`.
            pub const fn id(self) -> &'static str {
                match self {
                    $(Self::$name => $id,)*
                }
            }

            /// What it does, for the Settings app and the shortcuts overlay.
            pub const fn label(self) -> &'static str {
                match self {
                    $(Self::$name => $label,)*
                }
            }

            /// Which heading it is listed under.
            pub const fn group(self) -> Group {
                match self {
                    $(Self::$name => Group::$group,)*
                }
            }

            /// The keys it has until rebound.
            pub fn default_keys(self) -> Keys {
                let text = match self {
                    $(Self::$name => $keys,)*
                };
                Keys::parse(text).expect("default shortcuts parse")
            }
        }
    };
}

// The single chords follow GNOME's and Windows' habits where one exists
// (Super+L locks, Super+E opens files, Super+D shows the desktop, Alt+F10
// maximizes, Ctrl+Alt+T opens a terminal). The sequences group what has no
// such habit under one prefix each: Super+W for where a window goes,
// Super+O for opening an app, Super+X for the session, as Windows' Super+X
// menu holds shut down and sign out.
shortcuts! {
    /// Toggle the command palette.
    Palette = "palette", Desktop, "Command palette", "Super+Space";
    /// Toggle the overview.
    Overview = "overview", Desktop, "Overview", "Super+A";
    /// Show the keyboard shortcuts.
    Help = "help", Desktop, "Keyboard shortcuts", "Super+/";
    /// Minimize every window, or bring them back.
    ShowDesktop = "show_desktop", Desktop, "Show the desktop", "Super+D";
    /// Save a picture of the screen.
    Screenshot = "screenshot", Desktop, "Take a screenshot", "Print";
    /// Switch to the monocle layout.
    Monocle = "monocle", Desktop, "Monocle layout", "Super+M";
    /// Switch to the tall layout.
    Tall = "tall", Desktop, "Tall layout", "Super+Shift+M";
    /// Focus the next window.
    FocusNext = "focus_next", Windows, "Focus next window", "Super+J";
    /// Focus the previous window.
    FocusPrevious = "focus_previous", Windows, "Focus previous window", "Super+K";
    /// Move the focused window to the main pane.
    Promote = "promote", Windows, "Move to main pane", "Super+Enter";
    /// Close the focused window.
    Close = "close", Windows, "Close window", "Super+Q";
    /// Minimize the focused window.
    Minimize = "minimize", Windows, "Minimize window", "Super+H";
    /// Maximize the focused window, or restore it.
    Maximize = "maximize", Windows, "Maximize or restore window", "Alt+F10";
    /// Float the focused window.
    Float = "float", Windows, "Float window", "Super+F";
    /// Tile the focused window.
    Tile = "tile", Windows, "Tile window", "Super+T";
    /// Keep the focused window on top, or stop.
    KeepOnTop = "keep_on_top", Windows, "Keep window on top", "Super+Shift+T";
    /// Snap the focused window to the left half.
    SnapLeft = "snap_left", Windows, "Snap to the left half", "Super+W Left";
    /// Snap the focused window to the right half.
    SnapRight = "snap_right", Windows, "Snap to the right half", "Super+W Right";
    /// Snap the focused window to the top-left quarter.
    SnapTopLeft = "snap_top_left", Windows, "Snap to the top left", "Super+W Q";
    /// Snap the focused window to the top-right quarter.
    SnapTopRight = "snap_top_right", Windows, "Snap to the top right", "Super+W E";
    /// Snap the focused window to the bottom-left quarter.
    SnapBottomLeft = "snap_bottom_left", Windows, "Snap to the bottom left", "Super+W Z";
    /// Snap the focused window to the bottom-right quarter.
    SnapBottomRight = "snap_bottom_right", Windows, "Snap to the bottom right", "Super+W C";
    /// Open Files.
    Files = "files", Apps, "Files", "Super+E";
    /// Open Settings.
    Settings = "settings", Apps, "Settings", "Super+I";
    /// Open a terminal.
    Terminal = "terminal", Apps, "Terminal", "Ctrl+Alt+T";
    /// Open the Text Editor.
    Editor = "editor", Apps, "Text Editor", "Super+O E";
    /// Open the System Monitor.
    Monitor = "monitor", Apps, "System Monitor", "Super+O M";
    /// Open the Calculator.
    Calculator = "calculator", Apps, "Calculator", "Super+O C";
    /// Open Machines.
    Machines = "machines", Apps, "Machines", "Super+O V";
    /// Lock the screen.
    Lock = "lock", Session, "Lock the screen", "Super+L";
    /// Suspend.
    Suspend = "suspend", Session, "Suspend", "Super+X S";
    /// Log out, after asking.
    LogOut = "log_out", Session, "Log out", "Super+X O";
    /// Restart, after asking.
    Reboot = "reboot", Session, "Restart", "Super+X R";
    /// Power off, after asking.
    PowerOff = "power_off", Session, "Power off", "Super+X P";
}

impl Shortcut {
    fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.id() == id)
    }
}

/// What a sequence of chords typed so far means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Resolved {
    /// It is a whole shortcut, and no longer one starts with it.
    Shortcut(Shortcut),
    /// It begins one or more longer shortcuts: wait for the next chord.
    Prefix,
    /// Nothing starts with it.
    Unbound,
}

/// The keys for each rebindable shortcut. `None` turns one off.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Shortcuts {
    keys: [Option<Keys>; Shortcut::ALL.len()],
}

impl Default for Shortcuts {
    fn default() -> Self {
        Self {
            keys: Shortcut::ALL.map(|s| Some(s.default_keys())),
        }
    }
}

impl Shortcuts {
    /// The keys bound to `shortcut`.
    pub fn get(&self, shortcut: Shortcut) -> Option<Keys> {
        self.keys[shortcut as usize]
    }

    /// Binds `shortcut` to `keys`, or turns it off with `None`.
    pub fn set(&mut self, shortcut: Shortcut, keys: Option<Keys>) {
        self.keys[shortcut as usize] = keys;
    }

    /// Every bound shortcut and its keys, in [`Shortcut::ALL`]'s order.
    pub fn bound(&self) -> impl Iterator<Item = (Shortcut, Keys)> + '_ {
        Shortcut::ALL
            .into_iter()
            .filter_map(|s| self.get(s).map(|k| (s, k)))
    }

    /// What `typed` means. A shortcut that is also the start of a longer
    /// one waits for the next chord like a prefix would; the Settings app
    /// flags it as a clash, since it can then never run.
    pub fn resolve(&self, typed: &[Chord]) -> Resolved {
        if typed.is_empty() {
            return Resolved::Unbound;
        }
        let mut whole = None;
        for (shortcut, keys) in self.bound() {
            if !keys.starts_with(typed) {
                continue;
            }
            if keys.chords().len() > typed.len() {
                return Resolved::Prefix;
            }
            whole = whole.or(Some(shortcut));
        }
        whole.map_or(Resolved::Unbound, Resolved::Shortcut)
    }

    /// The shortcut exactly `chord` triggers, ignoring sequences. When two
    /// share a chord, the first in [`Shortcut::ALL`] wins.
    pub fn lookup(&self, chord: Chord) -> Option<Shortcut> {
        match self.resolve(&[chord]) {
            Resolved::Shortcut(s) => Some(s),
            _ => None,
        }
    }

    /// What may follow `typed`: each shortcut it begins, with the keys left
    /// to press. The shell shows these while it waits.
    pub fn continuations<'a>(
        &'a self,
        typed: &'a [Chord],
    ) -> impl Iterator<Item = (Shortcut, &'a [Chord])> + 'a {
        Shortcut::ALL.into_iter().filter_map(move |s| {
            let keys = self.keys[s as usize].as_ref()?;
            (keys.starts_with(typed) && keys.chords().len() > typed.len())
                .then(|| (s, &keys.chords()[typed.len()..]))
        })
    }

    /// Shortcuts that can never run: they share their keys with another
    /// one, or their keys begin another one's.
    pub fn clashes(&self) -> Vec<Shortcut> {
        self.bound()
            .filter(|(s, keys)| {
                self.bound()
                    .any(|(other, o)| other != *s && o.starts_with(keys.chords()))
                    || self
                        .bound()
                        .any(|(other, o)| other != *s && keys.starts_with(o.chords()))
            })
            .map(|(s, _)| s)
            .collect()
    }

    /// Sets `shortcut.<id>` from its text: keys or `none`.
    pub(crate) fn set_text(&mut self, id: &str, value: &str) -> bool {
        let Some(shortcut) = Shortcut::from_id(id) else {
            return false;
        };
        if value.eq_ignore_ascii_case("none") {
            self.set(shortcut, None);
            return true;
        }
        Keys::parse(value)
            .map(|k| self.set(shortcut, Some(k)))
            .is_some()
    }

    /// `shortcut.<id> = <keys>` lines for every shortcut.
    pub(crate) fn to_text(self) -> String {
        Shortcut::ALL
            .into_iter()
            .map(|s| match self.get(s) {
                Some(keys) => format!("shortcut.{} = {keys}\n", s.id()),
                None => format!("shortcut.{} = none\n", s.id()),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequences_parse_and_print() {
        let keys = Keys::parse("super+w  left").unwrap();
        assert_eq!(keys.to_string(), "Super+W Left");
        assert_eq!(
            Keys::parse("Ctrl+K, Ctrl+S").unwrap().to_string(),
            "Ctrl+K Ctrl+S"
        );
        // The first chord still needs a modifier; later ones don't.
        assert_eq!(Keys::parse("W Left"), None);
        assert_eq!(Keys::parse("Super+W A B C"), None);
        assert_eq!(Keys::parse("Print").unwrap().to_string(), "Print");
        assert_eq!(Keys::parse("Alt+F10").unwrap().to_string(), "Alt+F10");
        assert_eq!(Keys::parse("Super+/").unwrap().to_string(), "Super+/");
        assert_eq!(
            Keys::parse("Super+, Super+.").unwrap().to_string(),
            "Super+, Super+."
        );
    }

    #[test]
    fn a_prefix_waits_and_a_whole_sequence_resolves() {
        let s = Shortcuts::default();
        let w = Chord::parse("Super+W").unwrap();
        assert_eq!(s.resolve(&[w]), Resolved::Prefix);
        let left = Chord::bare(KeyName::Left);
        assert_eq!(
            s.resolve(&[w, left]),
            Resolved::Shortcut(Shortcut::SnapLeft)
        );
        assert_eq!(
            s.resolve(&[w, Chord::bare(KeyName::Letter('x'))]),
            Resolved::Unbound
        );
        let next: Vec<_> = s.continuations(&[w]).map(|(s, _)| s).collect();
        assert!(next.contains(&Shortcut::SnapTopRight), "{next:?}");
        assert!(!next.contains(&Shortcut::Files));
    }

    #[test]
    fn the_defaults_never_clash() {
        assert_eq!(Shortcuts::default().clashes(), []);
    }

    #[test]
    fn a_chord_that_starts_a_sequence_clashes() {
        let mut s = Shortcuts::default();
        s.set(Shortcut::Close, Keys::parse("Super+W"));
        let clashes = s.clashes();
        assert!(clashes.contains(&Shortcut::Close));
        assert!(clashes.contains(&Shortcut::SnapLeft));
    }
}
