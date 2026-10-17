//! Power modes: a row for each profile power-profiles-daemon offers, such as
//! "Power Mode: Power Saver", which switches the whole machine to it, as the
//! Power page in Settings does.
//!
//! The session reads the profiles when the palette opens and publishes them
//! as the `power` source, which only the shell itself may register, and
//! carries out what is picked:
//!
//! ```text
//! {"profiles":[{"id":"performance","name":"Performance"},
//!   {"id":"balanced","name":"Balanced"},{"id":"power-saver","name":"Power saver"}],
//!   "active":"balanced"}
//! {"op":"set","profile":"power-saver"}
//! ```
//!
//! Without power-profiles-daemon nothing is registered and there are no
//! rows.

use derisk_palette_sdk::{
    Category, Entry, Guest, Hook, Input, Manifest, Value, View, export, json,
};

/// The registration this plugin reads.
const SOURCE: &str = "power";

struct Power;

impl Guest for Power {
    fn describe() -> Manifest {
        Manifest::new("power", "Power modes to switch between")
            .hooks(&[Hook::Entries])
            .inputs(&[Input::Registered])
            .sources(&[SOURCE])
            .categories(&[Category::Command])
            .actions(&["palette_command"])
    }

    fn entries(view: View) -> Vec<Entry> {
        entries(&view)
    }

    fn query(_view: View, _text: String) -> Vec<Entry> {
        Vec::new()
    }
}

/// The icon theme's symbolic icon for a profile, as GNOME's quick settings
/// show it.
fn icon(profile: &str) -> &'static str {
    match profile {
        "power-saver" => "power-profile-power-saver",
        "performance" => "power-profile-performance",
        _ => "power-profile-balanced",
    }
}

/// The catalog: one row per profile, the active one marked.
pub fn entries(view: &View) -> Vec<Entry> {
    let Some(registration) = view.registered.iter().find(|r| r.source == SOURCE) else {
        return Vec::new();
    };
    let Ok(data) = derisk_palette_sdk::serde_json::from_str::<Value>(&registration.data) else {
        return Vec::new();
    };
    let active = data["active"].as_str().unwrap_or_default();
    let none = Vec::new();
    data["profiles"]
        .as_array()
        .unwrap_or(&none)
        .iter()
        .filter_map(|profile| {
            let id = profile["id"].as_str().filter(|id| !id.is_empty())?;
            let name = profile["name"].as_str().unwrap_or(id);
            let command = json!({"op": "set", "profile": id});
            Some(
                Entry::new(
                    Category::Command,
                    icon(id),
                    format!("Power Mode: {name}"),
                    &[json!({"action": "palette_command", "source": SOURCE, "command": command})],
                )
                .detail(if id == active {
                    "Current power mode"
                } else {
                    "Switch the whole machine to it"
                })
                .keywords(format!("power profile mode battery energy {id}")),
            )
        })
        .collect()
}

export!(Power with_types_in derisk_palette_sdk::bindings);
