//! Computer use: what the live session tells agents about the screen and
//! how it turns their requests into compositor input.
//!
//! The agent protocol types are in [`derisk::ipc`]; this module converts
//! between them and [`mcsapi_compositor`]'s accessibility tree, injected
//! input and frame captures.

use std::{
    io::Write as _,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt as _},
    path::{Path, PathBuf},
};

use derisk::{
    geom::Rect,
    ipc::{ElementAction, InputEvent, MouseButton, TreeNode, UNTRUSTED_TEXT},
};
use mcsapi::WindowId;
use mcsapi_compositor::{
    Capture, Input, Keysym, Modifiers,
    a11y::{Element, Origin, Snapshot, Subtree},
    accesskit::{self, Action, Node, NodeId, Role, Toggled},
};
use serde_json::{Value, json};

/// Most nodes one window may register, so a runaway program cannot make
/// every tree build slow.
pub const MAX_REGISTERED_NODES: usize = 4096;

/// Registered node IDs must stay below this; the shell's own nodes for a
/// window (its title-bar buttons) use the IDs above.
pub const REGISTERED_ID_LIMIT: u64 = 1 << 62;

/// Longest name or value kept from a registered node.
const MAX_TEXT: usize = 4096;

/// Roles agents and registering programs can name.
const ROLES: &[Role] = &[
    Role::Button,
    Role::CheckBox,
    Role::RadioButton,
    Role::Switch,
    Role::TextInput,
    Role::MultilineTextInput,
    Role::SearchInput,
    Role::PasswordInput,
    Role::Label,
    Role::Paragraph,
    Role::Heading,
    Role::Link,
    Role::Image,
    Role::List,
    Role::ListItem,
    Role::ListBox,
    Role::ListBoxOption,
    Role::Menu,
    Role::MenuBar,
    Role::MenuItem,
    Role::MenuItemCheckBox,
    Role::ComboBox,
    Role::Tab,
    Role::TabList,
    Role::TabPanel,
    Role::Slider,
    Role::SpinButton,
    Role::ProgressIndicator,
    Role::ScrollBar,
    Role::ScrollView,
    Role::Toolbar,
    Role::Dialog,
    Role::AlertDialog,
    Role::Alert,
    Role::Tooltip,
    Role::Table,
    Role::Row,
    Role::Cell,
    Role::Tree,
    Role::TreeItem,
    Role::Group,
    Role::Pane,
    Role::Window,
    Role::Document,
    Role::GenericContainer,
    Role::Unknown,
];

/// `CheckBox` → `check_box`.
pub fn role_name(role: Role) -> String {
    let mut name = String::new();
    for (i, c) in format!("{role:?}").chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                name.push('_');
            }
            name.push(c.to_ascii_lowercase());
        } else {
            name.push(c);
        }
    }
    name
}

/// The role named `name`, or a plain container.
pub fn parse_role(name: &str) -> Role {
    let name = name.trim().to_ascii_lowercase();
    ROLES
        .iter()
        .copied()
        .find(|r| role_name(*r) == name)
        .unwrap_or(Role::GenericContainer)
}

pub fn element_action(action: ElementAction) -> Action {
    match action {
        ElementAction::Click => Action::Click,
        ElementAction::Focus => Action::Focus,
        ElementAction::SetValue => Action::SetValue,
        ElementAction::Increment => Action::Increment,
        ElementAction::Decrement => Action::Decrement,
        ElementAction::Expand => Action::Expand,
        ElementAction::Collapse => Action::Collapse,
        ElementAction::ScrollIntoView => Action::ScrollIntoView,
        ElementAction::ScrollUp => Action::ScrollUp,
        ElementAction::ScrollDown => Action::ScrollDown,
    }
}

fn action_name(action: Action) -> Option<&'static str> {
    Some(match action {
        Action::Click => "click",
        Action::Focus => "focus",
        Action::SetValue => "set_value",
        Action::Increment => "increment",
        Action::Decrement => "decrement",
        Action::Expand => "expand",
        Action::Collapse => "collapse",
        Action::ScrollIntoView => "scroll_into_view",
        Action::ScrollUp => "scroll_up",
        Action::ScrollDown => "scroll_down",
        _ => return None,
    })
}

/// The protocol name of an action on a registered node, for the event
/// sent to the program that registered it.
pub fn action_event(window: WindowId, node: NodeId, action: Action, value: Option<&str>) -> Value {
    json!({
        "event": "action",
        "window": window.get(),
        "node": node.0,
        "action": action_name(action),
        "value": value,
    })
}

fn origin_name(origin: Origin) -> &'static str {
    match origin {
        Origin::Desktop => "desktop",
        Origin::Chrome => "chrome",
        Origin::Window => "window",
        Origin::App => "app",
        Origin::Shell => "shell",
        _ => "other",
    }
}

fn element_json(e: &Element) -> Value {
    let mut v = json!({
        "id": e.id,
        "role": role_name(e.role),
        "name": e.name,
        "origin": origin_name(e.origin),
        "actions": e.actions.iter().filter_map(|a| action_name(*a)).collect::<Vec<_>>(),
    });
    let o = v.as_object_mut().expect("an object");
    if let Some(value) = &e.value {
        o.insert("value".into(), json!(value));
    }
    if let Some(d) = e.description.as_ref().filter(|d| !d.is_empty()) {
        o.insert("description".into(), json!(d));
    }
    if let Some(b) = e.bounds {
        o.insert("bounds".into(), json!(Rect::from(b)));
        o.insert(
            "center".into(),
            json!([b.loc.x + b.size.w / 2, b.loc.y + b.size.h / 2]),
        );
    }
    if let Some(w) = e.window {
        o.insert("window".into(), json!(w.get()));
    }
    if let Some(p) = e.parent {
        o.insert("parent".into(), json!(p));
    }
    for (key, on) in [("focused", e.focused), ("disabled", e.disabled)] {
        if on {
            o.insert(key.into(), json!(true));
        }
    }
    for (key, state) in [
        ("toggled", e.toggled),
        ("selected", e.selected),
        ("expanded", e.expanded),
    ] {
        if let Some(state) = state {
            o.insert(key.into(), json!(state));
        }
    }
    if !e.children.is_empty() {
        o.insert("children".into(), json!(e.children));
    }
    v
}

/// The `tree` result: every element, or one window's.
pub fn tree_json(snapshot: &Snapshot, window: Option<u64>) -> Value {
    let elements: Vec<Value> = snapshot
        .elements
        .iter()
        .filter(|e| window.is_none_or(|w| e.window.map(WindowId::get) == Some(w)))
        .map(element_json)
        .collect();
    json!({
        "elements": elements,
        "focus": snapshot.focus,
        "note": UNTRUSTED_TEXT,
    })
}

/// The `find` result: elements of `role` whose name contains `name`.
pub fn find_json(
    snapshot: &Snapshot,
    role: Option<&str>,
    name: Option<&str>,
    window: Option<u64>,
) -> Value {
    let role = role.map(|r| r.trim().to_ascii_lowercase());
    let name = name.map(str::to_lowercase);
    let elements: Vec<Value> = snapshot
        .elements
        .iter()
        .filter(|e| window.is_none_or(|w| e.window.map(WindowId::get) == Some(w)))
        .filter(|e| role.as_ref().is_none_or(|r| role_name(e.role) == *r))
        .filter(|e| {
            name.as_ref().is_none_or(|n| {
                e.name.to_lowercase().contains(n)
                    || e.value
                        .as_ref()
                        .is_some_and(|v| v.to_lowercase().contains(n))
            })
        })
        .map(element_json)
        .collect();
    json!({"elements": elements, "note": UNTRUSTED_TEXT})
}

/// A key chord such as `ctrl+shift+t` or `super+space`.
pub fn parse_chord(chord: &str) -> Result<(Keysym, Modifiers), String> {
    let mut mods = Modifiers::default();
    let parts: Vec<&str> = chord.split('+').map(str::trim).collect();
    let (key, held) = match parts.split_last() {
        // `ctrl++` means Ctrl and the plus key.
        Some((last, rest)) if last.is_empty() && rest.last() == Some(&"") => {
            ("+", &rest[..rest.len() - 1])
        }
        Some((last, rest)) => (*last, rest),
        None => return Err("empty key".into()),
    };
    for m in held {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods.ctrl = true,
            "alt" => mods.alt = true,
            "shift" => mods.shift = true,
            "super" | "logo" | "meta" | "win" | "cmd" => mods.logo = true,
            other => return Err(format!("unknown modifier {other:?}")),
        }
    }
    let lower = key.to_ascii_lowercase();
    let sym = match lower.as_str() {
        "enter" | "return" => Keysym::Return,
        "tab" => Keysym::Tab,
        "escape" | "esc" => Keysym::Escape,
        "space" => Keysym::space,
        "backspace" => Keysym::BackSpace,
        "delete" | "del" => Keysym::Delete,
        "insert" => Keysym::Insert,
        "home" => Keysym::Home,
        "end" => Keysym::End,
        "pageup" | "page_up" => Keysym::Page_Up,
        "pagedown" | "page_down" => Keysym::Page_Down,
        "up" => Keysym::Up,
        "down" => Keysym::Down,
        "left" => Keysym::Left,
        "right" => Keysym::Right,
        "super" => Keysym::Super_L,
        "menu" => Keysym::Menu,
        f if f.len() > 1
            && f.starts_with('f')
            && f[1..].parse::<u32>().is_ok_and(|n| (1..=12).contains(&n)) =>
        {
            Keysym::new(Keysym::F1.raw() + f[1..].parse::<u32>().expect("checked") - 1)
        }
        _ => {
            let mut chars = key.chars();
            match (chars.next(), chars.next()) {
                // Chords name the unshifted key: ctrl+s, not ctrl+S.
                (Some(c), None) => Keysym::from_char(c.to_ascii_lowercase()),
                _ => return Err(format!("unknown key {key:?}")),
            }
        }
    };
    Ok((sym, mods))
}

fn button(b: MouseButton) -> mcsapi_compositor::MouseButton {
    match b {
        MouseButton::Left => mcsapi_compositor::MouseButton::Left,
        MouseButton::Right => mcsapi_compositor::MouseButton::Right,
        MouseButton::Middle => mcsapi_compositor::MouseButton::Middle,
    }
}

/// Compositor input for protocol events; checks every event first, so a
/// bad one leaves nothing half done.
pub fn inputs(events: &[InputEvent]) -> Result<Vec<Input>, String> {
    let mut out = Vec::new();
    for event in events {
        match event {
            InputEvent::Move { x, y } => out.push(Input::Move { x: *x, y: *y }),
            InputEvent::Click {
                x,
                y,
                button: b,
                count,
            } => {
                for _ in 0..(*count).clamp(1, 3) {
                    out.push(Input::Click {
                        x: *x,
                        y: *y,
                        button: button(*b),
                    });
                }
            }
            InputEvent::Press { button: b } => out.push(Input::Button {
                button: button(*b),
                pressed: true,
            }),
            InputEvent::Release { button: b } => out.push(Input::Button {
                button: button(*b),
                pressed: false,
            }),
            InputEvent::Scroll { dx, dy } => out.push(Input::Scroll { dx: *dx, dy: *dy }),
            InputEvent::Key { key } => {
                let (sym, mods) = parse_chord(key)?;
                out.push(Input::Key { sym, mods });
            }
            InputEvent::Text { text } => out.push(Input::Text(text.clone())),
        }
    }
    Ok(out)
}

fn truncated(text: &str) -> String {
    text.chars().take(MAX_TEXT).collect()
}

/// A registered tree as compositor nodes, after checking it.
pub fn subtree(window: WindowId, nodes: &[TreeNode]) -> Result<Subtree, String> {
    if nodes.len() > MAX_REGISTERED_NODES {
        return Err(format!("at most {MAX_REGISTERED_NODES} nodes per window"));
    }
    let ids: std::collections::HashSet<u64> = nodes.iter().map(|n| n.id).collect();
    if ids.len() != nodes.len() {
        return Err("node IDs must be unique".into());
    }
    if let Some(n) = nodes.iter().find(|n| n.id >= REGISTERED_ID_LIMIT) {
        return Err(format!("node ID {} is not below 2^62", n.id));
    }
    let children: std::collections::HashSet<u64> = nodes
        .iter()
        .flat_map(|n| n.children.iter().copied())
        .collect();
    let converted = nodes
        .iter()
        .map(|n| {
            let mut node = Node::new(parse_role(&n.role));
            if !n.name.is_empty() {
                node.set_label(truncated(&n.name));
            }
            if let Some(value) = &n.value {
                node.set_value(truncated(value));
            }
            if let Some(b) = n.bounds {
                node.set_bounds(accesskit::Rect {
                    x0: f64::from(b.x),
                    y0: f64::from(b.y),
                    x1: f64::from(b.x + b.w),
                    y1: f64::from(b.y + b.h),
                });
            }
            node.set_children(
                n.children
                    .iter()
                    .filter(|c| ids.contains(c))
                    .map(|c| NodeId(*c))
                    .collect::<Vec<_>>(),
            );
            for action in &n.actions {
                node.add_action(element_action(*action));
            }
            if let Some(t) = n.toggled {
                node.set_toggled(if t { Toggled::True } else { Toggled::False });
            }
            if n.disabled {
                node.set_disabled();
            }
            (NodeId(n.id), node)
        })
        .collect();
    Ok(Subtree {
        window,
        roots: nodes
            .iter()
            .filter(|n| !children.contains(&n.id))
            .map(|n| NodeId(n.id))
            .collect(),
        nodes: converted,
        origin: Origin::App,
    })
}

/// Where screenshots go: `$XDG_RUNTIME_DIR/derisk/screenshots`, private to
/// the user.
pub fn screenshot_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("derisk").join("screenshots"))
}

/// Encodes a capture as PNG.
pub fn png(capture: &Capture) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, capture.width, capture.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer
        .write_image_data(&capture.rgba)
        .map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    Ok(out)
}

/// Writes `png` as screenshot number `n` in `dir`, readable only by the
/// user, keeping the last few.
pub fn save_screenshot(dir: &Path, n: u64, png: &[u8]) -> std::io::Result<PathBuf> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let path = dir.join(format!("screen-{n}.png"));
    // A fresh file every time: O_EXCL also refuses a planted symlink.
    let _ = std::fs::remove_file(&path);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(png)?;
    if let Some(old) = n.checked_sub(KEEP_SCREENSHOTS) {
        let _ = std::fs::remove_file(dir.join(format!("screen-{old}.png")));
    }
    Ok(path)
}

/// Saves a screenshot the person took (Print) in `dir`, normally
/// `~/Pictures/Screenshots`, as `Screenshot <stamp>.png`, numbering it when
/// two land in the same minute. Unlike an agent's screenshots these are the
/// person's own pictures, kept until they delete them.
pub fn save_picture(dir: &Path, stamp: &str, png: &[u8]) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    for n in 1u32.. {
        let name = match n {
            1 => format!("Screenshot {stamp}.png"),
            n => format!("Screenshot {stamp} ({n}).png"),
        };
        let path = dir.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(png)?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    unreachable!("some number is free")
}

/// Screenshots kept on disk; older ones are removed.
const KEEP_SCREENSHOTS: u64 = 8;

/// Standard base64 with padding.
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_round_trip_through_their_names() {
        for role in ROLES {
            assert_eq!(parse_role(&role_name(*role)), *role, "{role:?}");
        }
        assert_eq!(role_name(Role::CheckBox), "check_box");
        assert_eq!(parse_role("no_such_role"), Role::GenericContainer);
    }

    #[test]
    fn chords_parse_with_modifiers() {
        let (sym, mods) = parse_chord("ctrl+shift+T").unwrap();
        assert_eq!(sym, Keysym::t);
        assert!(mods.ctrl && mods.shift && !mods.alt && !mods.logo);
        assert_eq!(parse_chord("super+space").unwrap().0, Keysym::space);
        assert_eq!(parse_chord("F5").unwrap().0, Keysym::F5);
        assert_eq!(parse_chord("ctrl++").unwrap().0, Keysym::plus);
        assert!(parse_chord("hyper+x").is_err());
        assert!(parse_chord("nosuchkey").is_err());
    }

    #[test]
    fn bad_input_events_reject_the_whole_batch() {
        let events = [
            InputEvent::Text { text: "a".into() },
            InputEvent::Key {
                key: "nosuchkey".into(),
            },
        ];
        assert!(inputs(&events).is_err());
        let ok = inputs(&[InputEvent::Click {
            x: 1,
            y: 2,
            button: MouseButton::Left,
            count: 2,
        }])
        .unwrap();
        assert_eq!(ok.len(), 2, "a double click is two clicks");
    }

    #[test]
    fn registered_trees_are_checked() {
        let w = WindowId::new(1).unwrap();
        let node = |id, children: Vec<u64>| TreeNode {
            id,
            role: "button".into(),
            name: "Play".into(),
            value: None,
            bounds: None,
            children,
            actions: vec![ElementAction::Click],
            focused: false,
            toggled: None,
            disabled: false,
        };
        let tree = subtree(w, &[node(1, vec![2, 99]), node(2, vec![])]).unwrap();
        assert_eq!(tree.roots, [NodeId(1)]);
        assert_eq!(
            tree.nodes[0].1.children(),
            [NodeId(2)],
            "unknown children dropped"
        );
        assert!(subtree(w, &[node(1, vec![]), node(1, vec![])]).is_err());
        assert!(subtree(w, &[node(REGISTERED_ID_LIMIT, vec![])]).is_err());
    }

    #[test]
    fn base64_matches_the_rfc_examples() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn find_matches_role_and_part_of_the_name() {
        let element = |id, role, name: &str| Element {
            id,
            parent: None,
            role,
            name: name.into(),
            value: None,
            description: None,
            bounds: Some(mcsapi::Geometry::new((10, 10).into(), (20, 10).into())),
            window: None,
            origin: Origin::Chrome,
            focused: false,
            disabled: false,
            toggled: None,
            selected: None,
            expanded: None,
            actions: vec![Action::Click],
            children: Vec::new(),
        };
        let snapshot = Snapshot {
            elements: vec![
                element(1, Role::Button, "Save file"),
                element(2, Role::Label, "Save"),
            ],
            focus: None,
        };
        let found = find_json(&snapshot, Some("button"), Some("SAVE"), None);
        let elements = found["elements"].as_array().unwrap();
        assert_eq!(elements.len(), 1);
        assert_eq!(elements[0]["id"], 1);
        assert_eq!(elements[0]["center"], json!([20, 15]));
        assert_eq!(elements[0]["actions"], json!(["click"]));
    }
}
