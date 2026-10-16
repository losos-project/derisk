//! The Privacy page: the privacy display, device access for every app,
//! history and trash, and each Flatpak app's permissions.
//!
//! The switches at the top are settings and apply on Save; a switched-off
//! device is then denied to every Flatpak app in the portal permission
//! store, and switching it back on forgets those answers so apps ask
//! again. Changes to one app's permissions apply at once, like Flatseal,
//! because they live in Flatpak's own files rather than in derisk's.

use std::collections::BTreeMap;

use mcsapi_ui::{Theme, egui};

use crate::{
    flatpak::{self, App, Context, Dirs, Grant, PERMISSIONS, Portal},
    model::{Privacy, PrivacyDisplay},
};

/// An app's portal permissions, or why they can't be read, by app ID.
type PortalState = (String, Result<BTreeMap<Portal, bool>, String>);

/// Privacy page state: the scanned apps and what is selected.
#[derive(Debug, Default)]
pub(crate) struct PrivacyUi {
    dirs: Option<Dirs>,
    apps: Option<Vec<App>>,
    global: Context,
    selected: Option<String>,
    filter: String,
    /// The selected app's portal permissions, or why they can't be read.
    portals: Option<PortalState>,
    status: Option<String>,
    /// The app ID being typed for the privacy display's list.
    new_app: String,
}

impl PrivacyUi {
    /// Uses `dirs` instead of the standard Flatpak locations (for tests).
    pub(crate) fn with_dirs(dirs: Dirs) -> Self {
        Self {
            dirs: Some(dirs),
            ..Self::default()
        }
    }

    pub(crate) fn select(&mut self, id: &str) {
        self.selected = Some(id.to_owned());
    }

    fn scan(&mut self) {
        if self.dirs.is_none() {
            self.dirs = Dirs::standard();
        }
        let Some(dirs) = &self.dirs else {
            self.apps = Some(Vec::new());
            return;
        };
        self.global = dirs.global_overrides();
        self.apps = Some(dirs.apps());
    }

    /// Denies, or forgets, a device for every Flatpak app after its master
    /// switch changed. Returns a line for the status bar.
    pub(crate) fn apply_masters(&mut self, before: &Privacy, after: &Privacy) -> Option<String> {
        let changed: Vec<(Portal, bool)> = [
            (Portal::Location, before.location, after.location),
            (Portal::Camera, before.camera, after.camera),
            (Portal::Microphone, before.microphone, after.microphone),
        ]
        .into_iter()
        .filter(|(_, b, a)| b != a)
        .map(|(p, _, a)| (p, a))
        .collect();
        if changed.is_empty() {
            return None;
        }
        self.scan();
        let apps = self.apps.clone().unwrap_or_default();
        let mut failed = None;
        for app in &apps {
            for (portal, on) in &changed {
                let result = flatpak::set_portal(&app.id, *portal, (!on).then_some(false));
                if let Err(e) = result {
                    failed.get_or_insert(e.0);
                }
            }
        }
        self.portals = None;
        Some(match failed {
            Some(e) if !apps.is_empty() => format!("Saved, but Flatpak apps were not updated: {e}"),
            _ => format!("Saved; {} Flatpak app(s) updated", apps.len()),
        })
    }
}

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(crate::layout::note(
        &mcsapi_components::Tokens::current(ui.ctx()),
        text,
    ));
}

/// The privacy display's switch, its conditions and its apps.
fn privacy_display(ui: &mut egui::Ui, d: &mut PrivacyDisplay, new_app: &mut String) {
    ui.label("Privacy display");
    ui.vertical(|ui| {
        ui.checkbox(
            &mut d.enabled,
            "Cover the screen when someone else is looking",
        );
        hint(
            ui,
            "While one of the conditions below holds, the camera looks for a second face. \
             Pictures stay in memory and are never saved or sent anywhere.",
        );
        ui.add_enabled_ui(d.enabled, |ui| {
            ui.add_space(4.0);
            ui.checkbox(&mut d.passwords, "When a password is asked for");
            ui.checkbox(&mut d.notifications, "Over notification pop-ups");
            ui.checkbox(&mut d.maximum, "Maximum protection: also dim the screen");
            ui.add_space(4.0);
            ui.label("When one of these apps is in use:");
            let mut remove = None;
            for (i, id) in d.apps.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(id);
                    if ui.small_button("Remove").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                d.apps.remove(i);
            }
            ui.horizontal(|ui| {
                let field = ui.add(
                    egui::TextEdit::singleline(new_app)
                        .hint_text("App ID, such as org.mozilla.firefox")
                        .desired_width(260.0),
                );
                let id = new_app.trim().to_owned();
                let ok = !id.is_empty() && !id.contains([',', '/']) && !d.apps.contains(&id);
                let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.add_enabled(ok, egui::Button::new("Add")).clicked() || (entered && ok)) && ok
                {
                    d.apps.push(id);
                    new_app.clear();
                }
            });
        });
    });
    ui.end_row();
}

pub(crate) fn page(
    ui: &mut egui::Ui,
    p: &mut Privacy,
    display: &mut PrivacyDisplay,
    state: &mut PrivacyUi,
    theme: &Theme,
) {
    egui::Grid::new("privacy-global")
        .num_columns(2)
        .spacing([24.0, 10.0])
        .show(ui, |ui| {
            privacy_display(ui, display, &mut state.new_app);
            ui.label("Devices");
            ui.vertical(|ui| {
                ui.checkbox(&mut p.location, "Location services");
                ui.checkbox(&mut p.camera, "Camera");
                ui.checkbox(&mut p.microphone, "Microphone");
                hint(
                    ui,
                    "Off denies it to every Flatpak app when you save; on lets them ask again.",
                );
            });
            ui.end_row();
            ui.label("History");
            ui.vertical(|ui| {
                ui.checkbox(&mut p.remember_recent, "Remember recently used files");
                hint(ui, "Off also clears the list and the palette's picks.");
            });
            ui.end_row();
            ui.label("Trash");
            ui.add(
                egui::Slider::new(&mut p.empty_trash_days, 0..=365)
                    .logarithmic(true)
                    .custom_formatter(|v, _| {
                        if v == 0.0 {
                            "Keep files".into()
                        } else {
                            format!("Delete after {v} days")
                        }
                    }),
            );
            ui.end_row();
            ui.label("Usage count");
            ui.vertical(|ui| {
                ui.checkbox(&mut p.usage_ping, "Count me as an active user");
                hint(
                    ui,
                    "Once a day the system sends an anonymous id that changes every month, \
                     and nothing else about you, so the project knows how many people use it.",
                );
            });
            ui.end_row();
        });
    ui.add_space(16.0);
    ui.separator();
    ui.heading(egui::RichText::new("Flatpak apps").color(theme.foreground));
    hint(
        ui,
        "What each app may reach. Changes apply right away and take effect when the app next starts.",
    );
    ui.add_space(6.0);
    if state.apps.is_none() {
        state.scan();
    }
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut state.filter)
                .hint_text("Filter apps")
                .desired_width(200.0),
        );
        if ui.button("Rescan").clicked() {
            state.scan();
            state.portals = None;
        }
        if let Some(status) = &state.status {
            ui.label(egui::RichText::new(status).color(theme.accent));
        }
    });
    let apps = state.apps.clone().unwrap_or_default();
    if apps.is_empty() {
        hint(ui, "No Flatpak apps are installed for you or system-wide.");
        return;
    }
    ui.add_space(6.0);
    let filter = state.filter.to_lowercase();
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(200.0);
            egui::ScrollArea::vertical()
                .id_salt("privacy-apps")
                .max_height(420.0)
                .show(ui, |ui| {
                    for app in apps.iter().filter(|a| {
                        filter.is_empty()
                            || a.name.to_lowercase().contains(&filter)
                            || a.id.to_lowercase().contains(&filter)
                    }) {
                        let selected = state.selected.as_deref() == Some(app.id.as_str());
                        let changed = if app.overrides.is_empty() { "" } else { " •" };
                        if ui
                            .selectable_label(selected, format!("{}{changed}", app.name))
                            .on_hover_text(&app.id)
                            .clicked()
                        {
                            state.selected = Some(app.id.clone());
                        }
                    }
                });
        });
        ui.separator();
        ui.vertical(|ui| {
            let Some(index) = state
                .selected
                .as_ref()
                .and_then(|id| apps.iter().position(|a| &a.id == id))
            else {
                hint(ui, "Pick an app to see what it may reach.");
                return;
            };
            app_permissions(ui, state, index, p, theme);
        });
    });
}

fn app_permissions(
    ui: &mut egui::Ui,
    state: &mut PrivacyUi,
    index: usize,
    masters: &Privacy,
    theme: &Theme,
) {
    let Some(mut app) = state.apps.as_ref().and_then(|a| a.get(index)).cloned() else {
        return;
    };
    let global = state.global.clone();
    ui.label(
        egui::RichText::new(&app.name)
            .strong()
            .color(theme.foreground),
    );
    hint(
        ui,
        &format!(
            "{} · installed {}",
            app.id,
            if app.user { "for you" } else { "system-wide" }
        ),
    );
    ui.add_space(6.0);
    let mut changed = false;
    let mut group = "";
    egui::Grid::new(("privacy-app", &app.id))
        .num_columns(2)
        .spacing([16.0, 4.0])
        .show(ui, |ui| {
            for perm in PERMISSIONS {
                if perm.group != group {
                    group = perm.group;
                    ui.label(egui::RichText::new(group).strong());
                    ui.end_row();
                }
                let effective = app.effective(&global, perm.key, perm.value);
                let mut on = effective.is_some();
                let requested = app
                    .requested
                    .get(perm.key, perm.value)
                    .is_some_and(|g| g != Grant::No);
                let label = match effective {
                    Some(Grant::ReadOnly) => format!("{} (read-only)", perm.label),
                    _ => perm.label.to_owned(),
                };
                if ui.checkbox(&mut on, label).changed() {
                    app.switch(&global, perm.key, perm.value, on);
                    changed = true;
                }
                let note = match (requested, app.overrides.get(perm.key, perm.value)) {
                    (_, Some(_)) => "changed by you",
                    (true, None) => "asked for by the app",
                    (false, None) => "",
                };
                hint(ui, note);
                ui.end_row();
            }
            // Folders the app asks for beyond the standard ones.
            let extra: Vec<(String, Grant)> = app
                .requested
                .values(flatpak::Key::Filesystems)
                .chain(app.overrides.values(flatpak::Key::Filesystems))
                .filter(|(v, _)| !PERMISSIONS.iter().any(|p| p.value == *v))
                .map(|(v, g)| (v.to_owned(), g))
                .collect();
            let mut seen = Vec::new();
            for (value, _) in extra {
                if seen.contains(&value) {
                    continue;
                }
                seen.push(value.clone());
                let key = flatpak::Key::Filesystems;
                let mut on = app.effective(&global, key, &value).is_some();
                if ui.checkbox(&mut on, format!("Folder {value}")).changed() {
                    app.switch(&global, key, &value, on);
                    changed = true;
                }
                hint(ui, "");
                ui.end_row();
            }
        });
    if !app.bus_names.is_empty() {
        ui.collapsing(format!("D-Bus names ({})", app.bus_names.len()), |ui| {
            for name in &app.bus_names {
                ui.label(egui::RichText::new(name).small().monospace());
            }
        });
    }
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                !app.overrides.is_empty(),
                egui::Button::new("Reset to app defaults"),
            )
            .clicked()
        {
            app.overrides = Context::default();
            changed = true;
        }
    });
    if changed {
        if let Some(dirs) = &state.dirs {
            state.status = Some(match dirs.save_overrides(&app.id, &app.overrides) {
                Ok(()) => format!("Updated {}", app.name),
                Err(e) => format!("Could not update {}: {e}", app.name),
            });
        }
        if let Some(apps) = &mut state.apps {
            apps[index] = app.clone();
        }
    }

    ui.add_space(10.0);
    ui.label(egui::RichText::new("Asked through portals").strong());
    if state.portals.as_ref().is_none_or(|(id, _)| *id != app.id) {
        state.portals = Some((
            app.id.clone(),
            flatpak::portal_permissions(&app.id).map_err(|e| e.0),
        ));
    }
    let Some((_, portals)) = &state.portals else {
        return;
    };
    let portals = match portals {
        Ok(p) => p.clone(),
        Err(e) => {
            hint(ui, &format!("Portal permissions can't be read: {e}"));
            return;
        }
    };
    let mut update = None;
    egui::Grid::new(("privacy-portals", &app.id))
        .num_columns(2)
        .spacing([16.0, 4.0])
        .show(ui, |ui| {
            for portal in Portal::ALL {
                let blocked = match portal {
                    Portal::Location => !masters.location,
                    Portal::Camera => !masters.camera,
                    Portal::Microphone => !masters.microphone,
                    _ => false,
                };
                ui.label(portal.label());
                ui.horizontal(|ui| {
                    if blocked {
                        hint(ui, "Off for every app (see Devices above)");
                        return;
                    }
                    let current = portals.get(&portal).copied();
                    for (choice, text) in
                        [(None, "Ask"), (Some(true), "Allow"), (Some(false), "Deny")]
                    {
                        if ui.selectable_label(current == choice, text).clicked()
                            && current != choice
                        {
                            update = Some((portal, choice));
                        }
                    }
                });
                ui.end_row();
            }
        });
    if let Some((portal, choice)) = update {
        state.status = Some(match flatpak::set_portal(&app.id, portal, choice) {
            Ok(()) => format!("Updated {}", app.name),
            Err(e) => format!("Could not update {}: {e}", app.name),
        });
        state.portals = None;
    }
}
