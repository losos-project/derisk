//! Virtual machines: "Start work", "Shut down work", each app a machine
//! has, as "Foot in work", and "Show work's screen" for a running machine
//! with a screen of its own.
//!
//! The session publishes its machines as the `machines` source, which only
//! the shell itself may register (an agent connection cannot speak for it),
//! and carries out what is picked:
//!
//! ```text
//! {"machines":[{"name":"work","label":"blue","running":true,"screen":false,
//!   "apps":[{"id":"foot","name":"Foot"}]}]}
//! {"op":"open","machine":"work","app":"foot"}
//! {"op":"start","machine":"work"}
//! {"op":"shut_down","machine":"work"}
//! {"op":"screen","machine":"work"}
//! ```
//!
//! A machine's apps are what its guest agent listed the last time the
//! session asked, kept while the machine is off, or until then what its
//! template ships; picking one of a machine that is off starts it. A
//! machine nothing is known of yet has none.

use derisk_palette_sdk::{
    Category, Entry, Guest, Hook, Input, Manifest, Value, View, export, json,
};

/// The registration this plugin reads.
const SOURCE: &str = "machines";

struct Machines;

impl Guest for Machines {
    fn describe() -> Manifest {
        Manifest::new("machines", "Virtual machines and the apps in them")
            .hooks(&[Hook::Entries])
            .inputs(&[Input::Registered])
            .sources(&[SOURCE])
            .categories(&[Category::App, Category::Command])
            .actions(&["palette_command"])
    }

    fn entries(view: View) -> Vec<Entry> {
        entries(&view)
    }

    fn query(_view: View, _text: String) -> Vec<Entry> {
        Vec::new()
    }
}

fn command(command: Value) -> Value {
    json!({"action": "palette_command", "source": SOURCE, "command": command})
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}

/// The catalog: each machine's apps, and starting or shutting it down.
pub fn entries(view: &View) -> Vec<Entry> {
    let Some(registration) = view.registered.iter().find(|r| r.source == SOURCE) else {
        return Vec::new();
    };
    let Ok(data) = derisk_palette_sdk::serde_json::from_str::<Value>(&registration.data) else {
        return Vec::new();
    };
    let none = Vec::new();
    let mut out = Vec::new();
    for machine in data["machines"].as_array().unwrap_or(&none) {
        let name = text(machine, "name");
        if name.is_empty() {
            continue;
        }
        // The label goes in the search terms, so "blue" finds the machine
        // whose windows are drawn blue.
        let label = text(machine, "label");
        let running = machine["running"].as_bool() == Some(true);
        if !running {
            out.push(
                Entry::new(
                    Category::Command,
                    "media-playback-start",
                    format!("Start {name}"),
                    &[command(json!({"op": "start", "machine": name}))],
                )
                .detail("Virtual machine")
                .keywords(format!("machine vm boot run {label}")),
            );
        }
        let detail = if running {
            format!("Opens in the {name} machine")
        } else {
            format!("Starts the {name} machine and opens there")
        };
        for app in machine["apps"].as_array().unwrap_or(&none) {
            let (id, app_name) = (text(app, "id"), text(app, "name"));
            if id.is_empty() || app_name.is_empty() {
                continue;
            }
            out.push(
                Entry::new(
                    Category::App,
                    "computer",
                    format!("{app_name} in {name}"),
                    &[command(json!({"op": "open", "machine": name, "app": id}))],
                )
                .detail(detail.clone())
                .keywords(format!("{id} machine vm {label}")),
            );
        }
        if !running {
            continue;
        }
        // The session shows a running machine's screen by itself; this is
        // for after its window was closed.
        if machine["screen"].as_bool() == Some(true) {
            out.push(
                Entry::new(
                    Category::Command,
                    "video-display",
                    format!("Show {name}'s screen"),
                    &[command(json!({"op": "screen", "machine": name}))],
                )
                .detail("Virtual machine")
                .keywords(format!("machine vm display console viewer {label}")),
            );
        }
        out.push(
            Entry::new(
                Category::Command,
                "system-shutdown",
                format!("Shut down {name}"),
                &[command(json!({"op": "shut_down", "machine": name}))],
            )
            .detail("Virtual machine")
            .keywords(format!("machine vm stop power off {label}")),
        );
    }
    out
}

export!(Machines with_types_in derisk_palette_sdk::bindings);

#[cfg(test)]
mod tests {
    use derisk_palette_sdk::Registration;

    use super::*;

    fn view(data: Value) -> View {
        View {
            registered: vec![Registration {
                source: SOURCE.into(),
                data: data.to_string(),
            }],
            ..View::default()
        }
    }

    #[test]
    fn a_running_machine_offers_its_apps_and_a_stopped_one_starting() {
        let rows = entries(&view(json!({"machines": [
            {"name": "work", "label": "blue", "running": true,
             "apps": [{"id": "foot", "name": "Foot"}, {"id": "", "name": "Nameless"}]},
            {"name": "web", "label": "red", "running": false, "apps": []},
        ]})));
        let titles: Vec<_> = rows.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(titles, ["Foot in work", "Shut down work", "Start web"]);
        assert_eq!(
            rows[0].parsed_actions(),
            [json!({"action": "palette_command", "source": "machines",
                    "command": {"op": "open", "machine": "work", "app": "foot"}})]
        );
        assert_eq!(rows[0].category, Category::App);
        assert!(rows[2].keywords.contains("red"));
    }

    #[test]
    fn a_stopped_machine_offers_the_apps_it_had_and_starts_for_them() {
        let rows = entries(&view(json!({"machines": [
            {"name": "flatpak", "label": "purple", "running": false,
             "apps": [{"id": "io.github.kolunmi.Bazaar", "name": "Bazaar"}]},
        ]})));
        let titles: Vec<_> = rows.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(titles, ["Start flatpak", "Bazaar in flatpak"]);
        assert_eq!(
            rows[1].parsed_actions(),
            [json!({"action": "palette_command", "source": "machines",
                    "command": {"op": "open", "machine": "flatpak",
                                "app": "io.github.kolunmi.Bazaar"}})]
        );
        assert_eq!(rows[1].detail, "Starts the flatpak machine and opens there");
    }

    #[test]
    fn a_running_machine_with_a_screen_offers_showing_it() {
        let rows = entries(&view(json!({"machines": [
            {"name": "win", "label": "purple", "running": true, "screen": true, "apps": []},
            {"name": "off", "label": "green", "running": false, "screen": true, "apps": []},
        ]})));
        let titles: Vec<_> = rows.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(titles, ["Show win's screen", "Shut down win", "Start off"]);
        assert_eq!(
            rows[0].parsed_actions(),
            [json!({"action": "palette_command", "source": "machines",
                    "command": {"op": "screen", "machine": "win"}})]
        );
    }

    #[test]
    fn nothing_without_the_shells_registration() {
        assert!(entries(&View::default()).is_empty());
        assert!(entries(&view(json!("not machines"))).is_empty());
    }
}
