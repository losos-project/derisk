//! derisk Agent: a window for building software and changing the desktop
//! with an agent, on LosOS.
//!
//! The window is an mcsapi app ([`AgentApp`]), drawn with mcsapi's components
//! in the desktop theme, in a window of its own (`derisk-agent`). The left
//! column holds projects, chats, routines and artifacts; the chat runs Claude
//! Code with the Agent's MCP server and its built-in skills ([`skills`]); the
//! right side shows the app the agent is building, live, next to the forge's
//! pull or merge requests and the project's MCP servers.
//!
//! This was Sonne's: its agent window, its project setup, its skills and its
//! previews, moved here when Sonne, a fork of Zed, left the image. The editor
//! that came with Sonne is not coming back with them; the code view and the
//! Text Editor are what open files.

pub mod agent;
mod code_view;
pub mod forge;
pub mod mcp;
pub mod questions;
pub mod routines;
pub mod setup;
pub mod skills;
pub mod store;
pub mod tools;
mod ui;

pub use ui::AgentApp;

use std::path::{Path, PathBuf};

use mcsapi_ui::{Theme, egui::Color32};

/// The app ID the window and its desktop entry share.
pub const APP_ID: &str = "org.derisk.agent";

/// The theme derisk publishes to `$XDG_RUNTIME_DIR/derisk/theme.json`, so the
/// window, a process of its own, matches the desktop. `None` outside a derisk
/// session, or when a colour does not parse.
pub fn desktop_theme() -> Option<Theme> {
    let path = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?)
        .join("derisk")
        .join("theme.json");
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let color = |name: &str| parse_hex(json["palette"][name].as_str()?);
    Some(Theme {
        background: color("background")?,
        surface: color("surface")?,
        foreground: color("foreground")?,
        border: color("border")?,
        accent: color("accent")?,
    })
}

fn parse_hex(text: &str) -> Option<Color32> {
    let hex = text.strip_prefix('#')?;
    let byte = |index: usize| u8::from_str_radix(hex.get(index..index + 2)?, 16).ok();
    match hex.len() {
        6 => Some(Color32::from_rgb(byte(0)?, byte(2)?, byte(4)?)),
        8 => Some(Color32::from_rgba_unmultiplied(
            byte(0)?,
            byte(2)?,
            byte(4)?,
            byte(6)?,
        )),
        _ => None,
    }
}

/// The path to run this program by in something that outlives it: a
/// routine's systemd unit, an app artifact's wasm runner. Under NixOS the
/// running binary is a store path that the next system update replaces, so
/// the system profile's link to it is used when there is one.
pub fn stable_executable() -> std::io::Result<PathBuf> {
    let current = std::env::current_exe()?;
    let profile = Path::new("/run/current-system/sw/bin");
    match current.file_name() {
        Some(name) if current.starts_with("/nix/store") && profile.join(name).exists() => {
            Ok(profile.join(name))
        }
        _ => Ok(current),
    }
}

/// An error and its causes on one line, "reading x: permission denied", for a
/// toast or a tool result.
pub fn describe(error: &miette::Report) -> String {
    error
        .chain()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
}

/// The request in a `derisk-agent:ask?prompt=...` link, which the palette's
/// "Ask the agent" row opens: the query string's `prompt`, with `+` for a
/// space and `%XX` escapes. `None` for any other link.
pub fn ask_prompt(url: &str) -> Option<String> {
    let query = url
        .strip_prefix("derisk-agent:")?
        .trim_start_matches("//")
        .strip_prefix("ask?")?;
    let value = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("prompt="))?;
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => out.push(b' '),
            b'%' => {
                let byte = value
                    .get(index + 1..index + 3)
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok())?;
                out.push(byte);
                index += 2;
            }
            byte => out.push(byte),
        }
        index += 1;
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ask_links_decode() {
        assert_eq!(
            ask_prompt("derisk-agent:ask?prompt=make+it+blue%3F%20%C3%A9").as_deref(),
            Some("make it blue? é")
        );
        assert_eq!(
            ask_prompt("derisk-agent://ask?prompt=hi").as_deref(),
            Some("hi")
        );
        assert_eq!(ask_prompt("zed://agent?prompt=hi"), None);
        assert_eq!(ask_prompt("derisk-agent:ask?prompt=%zz"), None);
    }

    #[test]
    fn theme_colors_parse_from_hex() {
        assert_eq!(parse_hex("#ff8000"), Some(Color32::from_rgb(255, 128, 0)));
        assert_eq!(
            parse_hex("#00000080"),
            Some(Color32::from_rgba_unmultiplied(0, 0, 0, 128))
        );
        assert_eq!(parse_hex("ff8000"), None);
        assert_eq!(parse_hex("#ff80"), None);
    }
}
