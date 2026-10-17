//! Keyboard shortcuts, as a mapping from the keys pressed to an [`Action`].
//!
//! The compositor host translates keysyms into [`Key`] and feeds every
//! press to a [`Sequence`] before forwarding it to the focused client. Keys
//! that start, continue or finish a shortcut are consumed by the shell.
//!
//! | Keys | Action |
//! | --- | --- |
//! | Super+Space | Toggle the command palette |
//! | Super (tap), Super+A | Toggle the overview |
//! | Super+/ | Show the keyboard shortcuts |
//! | Super+D | Show the desktop, or bring the windows back |
//! | Print | Take a screenshot |
//! | Super+←/→/↑/↓ | Snap, maximize, restore or minimize (Windows-style) |
//! | Super+1…9 | Switch workspace |
//! | Super+Shift+1…9 | Move the focused window to a workspace (one past the last opens a new one) |
//! | Super+Ctrl+←/→ | Previous / next workspace |
//! | Super+Ctrl+Shift+←/→ | Move the focused window to the previous / next workspace |
//! | Super+J / Super+K, Alt+Tab | Focus next / previous |
//! | Super+Shift+Enter | Promote to the main pane |
//! | Super+Q, Alt+F4 | Close |
//! | Super+H | Minimize |
//! | Alt+F10 | Maximize or restore |
//! | Super+F / Super+T | Float / tile |
//! | Super+Shift+T | Keep on top |
//! | Super+M / Super+Shift+M | Monocle / tall layout |
//! | Super+W then ←/→/Q/E/Z/C | Snap to a half or a quarter |
//! | Super+Enter, Super+E, Super+I | A terminal, Files, Settings |
//! | Super+O then E/M/C/V | Text Editor, System Monitor, Calculator, Machines |
//! | Super+L | Lock |
//! | Super+X then S/O/R/P | Suspend, log out, restart, power off |
//!
//! Everything but the families (digits, arrows, Alt+Tab, Alt+F4) can be
//! rebound in the Settings app, to one chord or a sequence of up to three
//! (see [`derisk_settings::Keys`]); see [`Sequence`].

use derisk_settings::{Chord, KeyName, Resolved, Shortcut, Shortcuts};

use crate::{
    action::{Action, LayoutKind},
    snap::{Direction, SnapZone},
    systemd::SessionOp,
};

/// The terminal Super+Enter opens: derisk-terminal, which the image ships
/// in place of foot, by its desktop file ID, which is how installed apps
/// are looked up for their `Exec`. foot's name stayed here after the image
/// dropped it, which left the shortcut opening nothing.
pub const TERMINAL: &str = "org.derisk.terminal.desktop";

// The core apps' IDs, as `derisk_apps` names them; that crate is only built
// with the host, and these keys are the library's.
const FILES: &str = "org.derisk.files";
const SETTINGS: &str = "org.derisk.settings";
const EDITOR: &str = "org.derisk.editor";
const MONITOR: &str = "org.derisk.monitor";
const CALCULATOR: &str = "org.derisk.calculator";

/// Modifier state for a chord.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Mods {
    /// Super (logo) key.
    pub logo: bool,
    /// Shift.
    pub shift: bool,
    /// Control.
    pub ctrl: bool,
    /// Alt.
    pub alt: bool,
}

/// A key, independent of keyboard layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Key {
    /// An arrow key.
    Arrow(Direction),
    /// A digit 0–9 (top row or keypad).
    Digit(u8),
    /// A letter, lowercase.
    Letter(char),
    /// Enter or Return.
    Enter,
    /// Tab.
    Tab,
    /// Escape.
    Escape,
    /// The space bar.
    Space,
    /// Any other key a shortcut can name: punctuation, Print, F1…F12 and
    /// the editing keys.
    Other(KeyName),
}

impl Key {
    fn name(self) -> KeyName {
        match self {
            Key::Arrow(Direction::Left) => KeyName::Left,
            Key::Arrow(Direction::Right) => KeyName::Right,
            Key::Arrow(Direction::Up) => KeyName::Up,
            Key::Arrow(Direction::Down) => KeyName::Down,
            Key::Digit(d) => KeyName::Digit(d),
            Key::Letter(c) => KeyName::Letter(c),
            Key::Enter => KeyName::Enter,
            Key::Tab => KeyName::Tab,
            Key::Escape => KeyName::Escape,
            Key::Space => KeyName::Space,
            Key::Other(name) => name,
        }
    }
}

/// The action a shortcut performs.
pub fn action(shortcut: Shortcut) -> Action {
    let snap = |zone| Action::Snap { window: None, zone };
    let launch = |app: &str| Action::Launch { app: app.into() };
    let session = |op| Action::Session {
        op,
        confirmed: false,
    };
    // Logging out, restarting and powering off ask on screen first, as
    // the palette's commands do: one stray key must not end the session.
    let ask = |op| Action::ConfirmSession { op };
    match shortcut {
        Shortcut::Palette => Action::Palette { visible: None },
        Shortcut::Overview => Action::Overview { visible: None },
        Shortcut::Help => Action::ShortcutHelp { visible: None },
        Shortcut::ShowDesktop => Action::ShowDesktop,
        Shortcut::Screenshot => Action::Screenshot,
        Shortcut::Monocle => Action::SetLayout {
            layout: LayoutKind::Monocle,
        },
        Shortcut::Tall => Action::SetLayout {
            layout: LayoutKind::Tall,
        },
        Shortcut::FocusNext => Action::FocusNext,
        Shortcut::FocusPrevious => Action::FocusPrevious,
        Shortcut::Promote => Action::Promote,
        Shortcut::Close => Action::Close { window: None },
        Shortcut::Minimize => Action::Minimize { window: None },
        Shortcut::Maximize => Action::ToggleMaximize { window: None },
        Shortcut::Float => Action::Float { window: None },
        Shortcut::Tile => Action::Tile { window: None },
        Shortcut::KeepOnTop => Action::KeepOnTop {
            window: None,
            on: None,
        },
        Shortcut::SnapLeft => snap(SnapZone::Left),
        Shortcut::SnapRight => snap(SnapZone::Right),
        Shortcut::SnapTopLeft => snap(SnapZone::TopLeft),
        Shortcut::SnapTopRight => snap(SnapZone::TopRight),
        Shortcut::SnapBottomLeft => snap(SnapZone::BottomLeft),
        Shortcut::SnapBottomRight => snap(SnapZone::BottomRight),
        Shortcut::Files => launch(FILES),
        Shortcut::Settings => launch(SETTINGS),
        Shortcut::Terminal => launch(TERMINAL),
        Shortcut::Editor => launch(EDITOR),
        Shortcut::Monitor => launch(MONITOR),
        Shortcut::Calculator => launch(CALCULATOR),
        Shortcut::Machines => launch(derisk_machines::APP_ID),
        Shortcut::Lock => session(SessionOp::Lock),
        Shortcut::SwitchUser => session(SessionOp::SwitchUser),
        Shortcut::Suspend => session(SessionOp::Suspend),
        Shortcut::LogOut => ask(SessionOp::Logout),
        Shortcut::Reboot => ask(SessionOp::Reboot),
        Shortcut::PowerOff => ask(SessionOp::PowerOff),
    }
}

/// The fixed families: one rule over many keys each, not rebindable.
fn fixed(mods: Mods, key: Key) -> Option<Action> {
    if mods.alt && !mods.logo && !mods.ctrl && key == Key::Tab {
        return Some(if mods.shift {
            Action::FocusPrevious
        } else {
            Action::FocusNext
        });
    }
    // Windows' and GNOME's close, which the Window menu has always named.
    if mods.alt
        && !mods.logo
        && !mods.ctrl
        && !mods.shift
        && key == Key::Other(KeyName::Function(4))
    {
        return Some(Action::Close { window: None });
    }
    if !mods.logo || mods.alt {
        return None;
    }
    let step = |direction| match direction {
        Direction::Left => Some(-1),
        Direction::Right => Some(1),
        Direction::Up | Direction::Down => None,
    };
    match (mods.ctrl, mods.shift, key) {
        (false, false, Key::Arrow(direction)) => Some(Action::Nudge {
            window: None,
            direction,
        }),
        (true, carry, Key::Arrow(direction)) => step(direction).map(|by| Action::StepWorkspace {
            by,
            carry_window: carry,
        }),
        (false, false, Key::Digit(d @ 1..=9)) => Some(Action::SwitchWorkspace {
            workspace: u64::from(d),
        }),
        (false, true, Key::Digit(d @ 1..=9)) => Some(Action::MoveToWorkspace {
            window: None,
            workspace: u64::from(d),
        }),
        (false, false, Key::Tab) => Some(Action::FocusNext),
        (false, true, Key::Tab) => Some(Action::FocusPrevious),
        _ => None,
    }
}

fn chord(mods: Mods, key: Key) -> Chord {
    Chord {
        logo: mods.logo,
        shift: mods.shift,
        ctrl: mods.ctrl,
        alt: mods.alt,
        key: key.name(),
    }
}

/// The action bound to a single chord with the default shortcuts, if any.
pub fn binding(mods: Mods, key: Key) -> Option<Action> {
    binding_with(&Shortcuts::default(), mods, key)
}

/// The action bound to a single chord, with the rebindable shortcuts taken
/// from `shortcuts` (the Settings app's Shortcuts page). A rebound shortcut
/// leaves its old chord free. A chord that only starts a sequence binds
/// nothing on its own; [`Sequence`] waits for the rest.
pub fn binding_with(shortcuts: &Shortcuts, mods: Mods, key: Key) -> Option<Action> {
    fixed(mods, key).or_else(|| shortcuts.lookup(chord(mods, key)).map(action))
}

/// What one key press did to a [`Sequence`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Step {
    /// A shortcut is complete: perform this.
    Action(Action),
    /// The press began or continued a sequence; the shell waits for more.
    Pending,
    /// A sequence was under way and this press ended it without a match
    /// (or was Escape). The press is the shell's all the same.
    Cancelled,
    /// Not a shortcut: the key belongs to the focused window.
    Unbound,
}

/// The chords of a multi-key shortcut typed so far, like VS Code's
/// `Ctrl+K` waiting for its second chord.
///
/// While one is under way every key press belongs to the shell: the next
/// chord either completes a shortcut, continues a longer one, or cancels
/// the sequence, so a half-typed shortcut never leaks a letter into the
/// window.
#[derive(Clone, Debug, Default)]
pub struct Sequence {
    typed: Vec<Chord>,
}

impl Sequence {
    /// The chords pressed so far; empty when no sequence is under way.
    pub fn typed(&self) -> &[Chord] {
        &self.typed
    }

    /// Whether a sequence is waiting for its next chord.
    pub fn is_pending(&self) -> bool {
        !self.typed.is_empty()
    }

    /// Forgets a sequence under way.
    pub fn cancel(&mut self) {
        self.typed.clear();
    }

    /// Feeds a key press.
    pub fn press(&mut self, shortcuts: &Shortcuts, mods: Mods, key: Key) -> Step {
        if self.typed.is_empty() {
            if let Some(action) = fixed(mods, key) {
                return Step::Action(action);
            }
        } else if key == Key::Escape && mods == Mods::default() {
            self.typed.clear();
            return Step::Cancelled;
        }
        let first = self.typed.is_empty();
        self.typed.push(chord(mods, key));
        match shortcuts.resolve(&self.typed) {
            Resolved::Shortcut(shortcut) => {
                self.typed.clear();
                Step::Action(action(shortcut))
            }
            Resolved::Prefix => Step::Pending,
            Resolved::Unbound => {
                self.typed.clear();
                if first {
                    Step::Unbound
                } else {
                    Step::Cancelled
                }
            }
        }
    }
}

/// Detects a bare tap of the Super key (press and release with nothing in
/// between), which toggles the overview like GNOME and Windows.
#[derive(Clone, Copy, Debug, Default)]
pub struct SuperTap {
    armed: bool,
}

impl SuperTap {
    /// Feeds a key event; `is_super` says whether it is a Super key.
    /// Returns `true` when a release completes a tap.
    pub fn key(&mut self, is_super: bool, pressed: bool) -> bool {
        match (is_super, pressed) {
            (true, true) => {
                self.armed = true;
                false
            }
            (true, false) => std::mem::take(&mut self.armed),
            (false, _) => {
                self.armed = false;
                false
            }
        }
    }

    /// Cancels a pending tap (for example on a pointer click while held).
    pub fn cancel(&mut self) {
        self.armed = false;
    }
}
