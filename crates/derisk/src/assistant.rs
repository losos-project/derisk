//! The built-in assistant: turns short natural-language requests into
//! [`Action`]s. External LLM agents use the same actions through IPC.

use crate::{
    action::{Action, LayoutKind},
    snap::{Direction, SnapZone},
    systemd::SessionOp,
};

/// A request clause the assistant did not understand.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error, miette::Diagnostic)]
#[error("I don't know how to \"{0}\"")]
#[diagnostic(code(derisk::assistant::not_understood))]
pub struct NotUnderstood(pub String);

const NUMBERS: [&str; 9] = [
    "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
];
const FILLER: [&str; 9] = [
    "it", "the", "window", "this", "that", "to", "please", "a", "app",
];

fn number(word: &str) -> Option<u64> {
    word.parse::<u64>().ok().or_else(|| {
        NUMBERS
            .iter()
            .position(|n| *n == word)
            .map(|i| i as u64 + 1)
    })
}

fn zone(words: &[&str]) -> Option<SnapZone> {
    let has = |w: &str| words.contains(&w);
    let (top, bottom) = (has("top") || has("upper"), has("bottom") || has("lower"));
    let (left, right) = (has("left"), has("right"));
    Some(match (top, bottom, left, right) {
        (true, false, true, false) => SnapZone::TopLeft,
        (true, false, false, true) => SnapZone::TopRight,
        (false, true, true, false) => SnapZone::BottomLeft,
        (false, true, false, true) => SnapZone::BottomRight,
        (false, false, true, false) => SnapZone::Left,
        (false, false, false, true) => SnapZone::Right,
        _ if has("maximize") || has("maximise") || has("full") || has("fullscreen") => {
            SnapZone::Maximize
        }
        _ => return None,
    })
}

fn clause(text: &str) -> Result<Action, NotUnderstood> {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '.' && c != '_')
        .filter(|w| !w.is_empty())
        .collect();
    let not_understood = || NotUnderstood(text.trim().to_owned());
    let Some(&verb) = words.first() else {
        return Err(not_understood());
    };
    let rest = &words[1..];
    let has = |w: &str| words.contains(&w);
    let last_number = || words.iter().rev().find_map(|w| number(w));

    let session = |op| Action::Session {
        op,
        confirmed: false,
    };
    let action = match verb {
        "lock" => session(SessionOp::Lock),
        "suspend" | "sleep" => session(SessionOp::Suspend),
        "hibernate" => session(SessionOp::Hibernate),
        "logout" | "log" | "sign" if verb == "logout" || has("out") || has("off") => {
            session(SessionOp::Logout)
        }
        "reboot" => session(SessionOp::Reboot),
        "restart" if has("computer") || has("system") || has("pc") || has("machine") => {
            session(SessionOp::Reboot)
        }
        "shutdown" | "poweroff" => session(SessionOp::PowerOff),
        "shut" | "power" if has("down") || has("off") => session(SessionOp::PowerOff),
        "show" | "open" | "toggle" | "overview" if has("overview") || has("desktop") => {
            Action::Overview {
                visible: (verb != "toggle").then_some(true),
            }
        }
        "hide" | "close" | "exit" | "leave" if has("overview") => Action::Overview {
            visible: Some(false),
        },
        "open" | "launch" | "start" | "run" => {
            let app: Vec<&str> = rest
                .iter()
                .copied()
                .filter(|w| !FILLER.contains(w))
                .collect();
            if app.is_empty() {
                return Err(not_understood());
            }
            Action::Launch { app: app.join("-") }
        }
        "keep" | "pin" if has("top") => Action::KeepOnTop {
            window: None,
            on: Some(true),
        },
        "unpin" => Action::KeepOnTop {
            window: None,
            on: Some(false),
        },
        "close" | "quit" | "kill" => Action::Close { window: None },
        "minimize" | "minimise" | "hide" if !has("overview") => Action::Minimize { window: None },
        "maximize" | "maximise" | "fullscreen" => Action::ToggleMaximize { window: None },
        "restore" | "unmaximize" | "unmaximise" => Action::ToggleMaximize { window: None },
        "float" | "unsnap" | "untile" => Action::Float { window: None },
        "tile" | "retile" if zone(rest).is_none() => Action::Tile { window: None },
        "snap" | "tile" | "put" | "move" | "send" | "throw" if zone(rest).is_some() => {
            Action::Snap {
                window: None,
                zone: zone(rest).expect("checked"),
            }
        }
        "move" | "send" | "throw" | "put" if has("workspace") || last_number().is_some() => {
            Action::MoveToWorkspace {
                window: None,
                workspace: last_number().ok_or_else(not_understood)?,
            }
        }
        "go" | "switch" | "workspace" | "show" | "goto" if last_number().is_some() => {
            Action::SwitchWorkspace {
                workspace: last_number().expect("checked"),
            }
        }
        "focus" | "next" | "previous" | "prev" | "go" | "switch"
            if has("next") || has("previous") || has("prev") =>
        {
            if has("next") {
                Action::FocusNext
            } else {
                Action::FocusPrevious
            }
        }
        "promote" | "make" | "swap" if has("main") || verb == "promote" => Action::Promote,
        "use" | "set" | "layout" | "switch" | "monocle" | "tall" | "stack"
            if has("monocle") || has("stack") || has("stacked") || has("tall") || has("tiling") =>
        {
            Action::SetLayout {
                layout: if has("monocle") || has("stack") || has("stacked") {
                    LayoutKind::Monocle
                } else {
                    LayoutKind::Tall
                },
            }
        }
        "nudge" | "push" => {
            let direction = if has("left") {
                Direction::Left
            } else if has("right") {
                Direction::Right
            } else if has("up") {
                Direction::Up
            } else if has("down") {
                Direction::Down
            } else {
                return Err(not_understood());
            };
            Action::Nudge {
                window: None,
                direction,
            }
        }
        _ => return Err(not_understood()),
    };
    Ok(action)
}

/// Interprets a request such as "open firefox and snap it left, then go to
/// workspace 2". Clauses are split on `,`, `;`, "and" and "then".
pub fn interpret(text: &str) -> Result<Vec<Action>, NotUnderstood> {
    let normalized = text
        .replace([',', ';'], " | ")
        .split_whitespace()
        .map(|w| match w.to_lowercase().as_str() {
            "and" | "then" | "&" => "|".to_owned(),
            _ => w.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(" ");
    let actions: Vec<Action> = normalized
        .split('|')
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(clause)
        .collect::<Result<_, _>>()?;
    if actions.is_empty() {
        return Err(NotUnderstood(text.trim().to_owned()));
    }
    Ok(actions)
}
