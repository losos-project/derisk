//! System: the tray's items, and each failed user service with a row to
//! restart it, one to dismiss it and one to report it. derisk leaves the
//! report rows out where nothing sends reports.

use derisk_palette_sdk::{Category, Entry, Guest, Hook, Input, Manifest, View, export, json};

struct System;

impl Guest for System {
    fn describe() -> Manifest {
        Manifest::new("system", "Tray items and failed user services")
            .hooks(&[Hook::Entries])
            .inputs(&[Input::Tray, Input::Units])
            .categories(&[Category::System])
            .actions(&[
                "activate_tray",
                "restart_unit",
                "reset_failed",
                "report_unit",
            ])
    }

    fn entries(view: View) -> Vec<Entry> {
        entries(&view)
    }

    fn query(_: View, _: String) -> Vec<Entry> {
        Vec::new()
    }
}

/// The catalog, apart from the export so tests can call it.
pub fn entries(view: &View) -> Vec<Entry> {
    let tray = view.tray.iter().map(|item| {
        Entry::new(
            Category::System,
            "starred",
            item.title.clone(),
            &[json!({"action": "activate_tray", "id": item.id})],
        )
        .detail("Tray")
        .keywords(item.id.clone())
    });
    let units = view.failed_units.iter().flat_map(|unit| {
        [
            Entry::new(
                Category::System,
                "dialog-warning",
                format!("Restart {unit}"),
                &[json!({"action": "restart_unit", "unit": unit})],
            )
            .detail("Failed service")
            .keywords("unit service systemd"),
            Entry::new(
                Category::System,
                "dialog-warning",
                format!("Dismiss {unit}"),
                &[json!({"action": "reset_failed", "unit": unit})],
            )
            .detail("Failed service")
            .keywords("unit service systemd reset"),
            Entry::new(
                Category::System,
                "dialog-warning",
                format!("Report {unit}"),
                &[json!({"action": "report_unit", "unit": unit})],
            )
            .detail("Failed service")
            .keywords("unit service systemd bug issue problem"),
        ]
    });
    tray.chain(units).collect()
}

export!(System with_types_in derisk_palette_sdk::bindings);
