//! Keys and the mouse, as the bytes a program in a terminal expects.
//!
//! What xterm sends is what every program reads: control characters for
//! Ctrl with a letter, an escape before a key pressed with Alt, `CSI`
//! sequences with xterm's modifier parameter for the cursor keys, Home, End,
//! the editing keys and the function keys, and `SS3` for the cursor keys
//! when the program has asked for application cursor mode.

use alacritty_terminal::term::TermMode;
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// xterm's modifier parameter: 1, plus 1 for Shift, 2 for Alt, 4 for Ctrl.
fn parameter(mods: ModifiersState) -> u8 {
    1 + u8::from(mods.shift_key()) + 2 * u8::from(mods.alt_key()) + 4 * u8::from(mods.control_key())
}

/// The bytes for `key` (with the text it typed, if any), or `None` for a
/// key that sends nothing, such as Shift alone.
pub fn encode(
    key: &Key,
    text: Option<&str>,
    mods: ModifiersState,
    mode: TermMode,
) -> Option<Vec<u8>> {
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let modified = mods.shift_key() || mods.alt_key() || mods.control_key();
    // A cursor key: `CSI 1;m X` with modifiers, else `SS3 X` in application
    // mode and `CSI X` outside it.
    let cursor = |letter: u8| -> Vec<u8> {
        if modified {
            format!("\x1b[1;{}{}", parameter(mods), letter as char).into_bytes()
        } else if app_cursor {
            vec![0x1b, b'O', letter]
        } else {
            vec![0x1b, b'[', letter]
        }
    };
    // An editing or function key: `CSI n ~`, or `CSI n;m ~`.
    let tilde = |n: u8| -> Vec<u8> {
        if modified {
            format!("\x1b[{n};{}~", parameter(mods)).into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };
    // F1 to F4 are `SS3 P` to `SS3 S`, or `CSI 1;m P` with modifiers.
    let pf = |letter: u8| -> Vec<u8> {
        if modified {
            format!("\x1b[1;{}{}", parameter(mods), letter as char).into_bytes()
        } else {
            vec![0x1b, b'O', letter]
        }
    };
    let alt = |mut bytes: Vec<u8>| {
        if mods.alt_key() {
            bytes.insert(0, 0x1b);
        }
        bytes
    };
    let bytes = match key {
        Key::Named(named) => match named {
            NamedKey::Enter => alt(vec![b'\r']),
            NamedKey::Tab if mods.shift_key() => b"\x1b[Z".to_vec(),
            NamedKey::Tab => alt(vec![b'\t']),
            // DEL, as every terminal emulator now sends; Ctrl+Backspace is
            // BS, which shells and editors read as deleting a word.
            NamedKey::Backspace if mods.control_key() => alt(vec![0x08]),
            NamedKey::Backspace => alt(vec![0x7f]),
            NamedKey::Escape => alt(vec![0x1b]),
            NamedKey::Space if mods.control_key() => alt(vec![0]),
            NamedKey::Space => alt(vec![b' ']),
            NamedKey::ArrowUp => cursor(b'A'),
            NamedKey::ArrowDown => cursor(b'B'),
            NamedKey::ArrowRight => cursor(b'C'),
            NamedKey::ArrowLeft => cursor(b'D'),
            NamedKey::Home => cursor(b'H'),
            NamedKey::End => cursor(b'F'),
            NamedKey::Insert => tilde(2),
            NamedKey::Delete => tilde(3),
            NamedKey::PageUp => tilde(5),
            NamedKey::PageDown => tilde(6),
            NamedKey::F1 => pf(b'P'),
            NamedKey::F2 => pf(b'Q'),
            NamedKey::F3 => pf(b'R'),
            NamedKey::F4 => pf(b'S'),
            NamedKey::F5 => tilde(15),
            NamedKey::F6 => tilde(17),
            NamedKey::F7 => tilde(18),
            NamedKey::F8 => tilde(19),
            NamedKey::F9 => tilde(20),
            NamedKey::F10 => tilde(21),
            NamedKey::F11 => tilde(23),
            NamedKey::F12 => tilde(24),
            _ => {
                return text
                    .filter(|t| !t.is_empty())
                    .map(|t| alt(t.as_bytes().to_vec()));
            }
        },
        Key::Character(c) => {
            if mods.control_key()
                && let Some(control) = control(c)
            {
                alt(vec![control])
            } else {
                // With Alt the layout's text may already be something else
                // (macOS-style option characters); the key's own character
                // is what Alt is meant to prefix.
                let typed = if mods.alt_key() {
                    Some(c.as_str())
                } else {
                    text
                };
                alt(typed.unwrap_or(c.as_str()).as_bytes().to_vec())
            }
        }
        _ => return None,
    };
    Some(bytes)
}

/// The control character Ctrl makes of `key`: Ctrl+A is 0x01 through Ctrl+Z
/// 0x1a, and Ctrl+@, [, \, ], ^ and _ the rest of C0, with the digits xterm
/// maps onto them.
fn control(key: &str) -> Option<u8> {
    let mut chars = key.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    Some(match c.to_ascii_lowercase() {
        c @ 'a'..='z' => c as u8 - b'a' + 1,
        '@' | '2' | ' ' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '7' | '/' | '-' => 0x1f,
        '8' | '?' => 0x7f,
        _ => return None,
    })
}

/// Text pasted into the terminal: bracketed when the program asked
/// (`ESC [200~` ... `ESC [201~`), with any end marker inside taken out so
/// pasted text cannot end the paste early and be run as typed; otherwise
/// with newlines as Enter.
pub fn paste(text: &str, mode: TermMode) -> Vec<u8> {
    if mode.contains(TermMode::BRACKETED_PASTE) {
        let inner = text.replace("\x1b[201~", "");
        format!("\x1b[200~{inner}\x1b[201~").into_bytes()
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

/// A mouse button, as mouse reporting numbers them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Button {
    Left = 0,
    Middle = 1,
    Right = 2,
    WheelUp = 64,
    WheelDown = 65,
}

/// A mouse report for a program that asked for them, or `None` when it
/// did not ask for this kind of event. `motion` is a move with `button`
/// held; `pressed` is false for a release. Columns and lines start at 0.
pub fn mouse(
    button: Button,
    pressed: bool,
    motion: bool,
    column: usize,
    line: usize,
    mods: ModifiersState,
    mode: TermMode,
) -> Option<Vec<u8>> {
    let wanted = if motion {
        mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
    } else {
        mode.intersects(
            TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION,
        )
    };
    if !wanted {
        return None;
    }
    let mut code = button as u8;
    if motion {
        code += 32;
    }
    code += 4 * u8::from(mods.shift_key())
        + 8 * u8::from(mods.alt_key())
        + 16 * u8::from(mods.control_key());
    if mode.contains(TermMode::SGR_MOUSE) {
        let end = if pressed { 'M' } else { 'm' };
        return Some(format!("\x1b[<{code};{};{}{end}", column + 1, line + 1).into_bytes());
    }
    // The original encoding: a release is button 3, and positions are one
    // byte each, so nothing past column 222 can be reported.
    if !pressed {
        code = (code & !3) | 3;
    }
    if column > 222 || line > 222 {
        return None;
    }
    Some(vec![
        0x1b,
        b'[',
        b'M',
        32 + code,
        33 + column as u8,
        33 + line as u8,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chr(c: &str) -> Key {
        Key::Character(c.into())
    }

    const NONE: ModifiersState = ModifiersState::empty();
    const CTRL: ModifiersState = ModifiersState::CONTROL;
    const ALT: ModifiersState = ModifiersState::ALT;

    #[test]
    fn text_and_control_characters() {
        let mode = TermMode::empty();
        assert_eq!(
            encode(&chr("a"), Some("a"), NONE, mode),
            Some(b"a".to_vec())
        );
        assert_eq!(encode(&chr("c"), Some("c"), CTRL, mode), Some(vec![3]));
        assert_eq!(
            encode(&chr("C"), Some("C"), CTRL | ModifiersState::SHIFT, mode),
            Some(vec![3])
        );
        assert_eq!(encode(&chr("["), None, CTRL, mode), Some(vec![0x1b]));
        assert_eq!(
            encode(&chr("b"), Some("b"), ALT, mode),
            Some(b"\x1bb".to_vec())
        );
        assert_eq!(
            encode(&chr("é"), Some("é"), NONE, mode),
            Some("é".as_bytes().to_vec())
        );
        assert_eq!(encode(&Key::Named(NamedKey::Shift), None, NONE, mode), None);
    }

    #[test]
    fn cursor_keys_follow_application_mode_and_modifiers() {
        let up = Key::Named(NamedKey::ArrowUp);
        assert_eq!(
            encode(&up, None, NONE, TermMode::empty()),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            encode(&up, None, NONE, TermMode::APP_CURSOR),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            encode(&up, None, CTRL, TermMode::APP_CURSOR),
            Some(b"\x1b[1;5A".to_vec())
        );
        let delete = Key::Named(NamedKey::Delete);
        assert_eq!(
            encode(&delete, None, NONE, TermMode::empty()),
            Some(b"\x1b[3~".to_vec())
        );
        assert_eq!(
            encode(&delete, None, ALT, TermMode::empty()),
            Some(b"\x1b[3;3~".to_vec())
        );
        let f1 = Key::Named(NamedKey::F1);
        assert_eq!(
            encode(&f1, None, NONE, TermMode::empty()),
            Some(b"\x1bOP".to_vec())
        );
        let tab = Key::Named(NamedKey::Tab);
        assert_eq!(
            encode(&tab, None, ModifiersState::SHIFT, TermMode::empty()),
            Some(b"\x1b[Z".to_vec())
        );
    }

    #[test]
    fn pastes_cannot_escape_their_brackets() {
        let pasted = paste("ls\x1b[201~rm -rf ~\n", TermMode::BRACKETED_PASTE);
        assert_eq!(pasted, b"\x1b[200~lsrm -rf ~\n\x1b[201~".to_vec());
        assert_eq!(paste("a\nb\r\nc", TermMode::empty()), b"a\rb\rc".to_vec());
    }

    #[test]
    fn mouse_reports_in_both_encodings() {
        let mode = TermMode::MOUSE_REPORT_CLICK;
        assert_eq!(
            mouse(Button::Left, true, false, 0, 0, NONE, TermMode::empty()),
            None
        );
        assert_eq!(
            mouse(Button::Left, true, false, 0, 0, NONE, mode),
            Some(vec![0x1b, b'[', b'M', 32, 33, 33])
        );
        assert_eq!(
            mouse(Button::Left, false, false, 4, 2, NONE, mode),
            Some(vec![0x1b, b'[', b'M', 35, 37, 35])
        );
        let sgr = mode | TermMode::SGR_MOUSE;
        assert_eq!(
            mouse(Button::WheelUp, true, false, 9, 4, NONE, sgr),
            Some(b"\x1b[<64;10;5M".to_vec())
        );
        assert_eq!(
            mouse(Button::Left, false, false, 0, 0, CTRL, sgr),
            Some(b"\x1b[<16;1;1m".to_vec())
        );
        // Motion is reported only to a program that asked for it.
        assert_eq!(mouse(Button::Left, true, true, 0, 0, NONE, sgr), None);
    }
}
