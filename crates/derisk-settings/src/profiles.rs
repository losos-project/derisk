//! The Power page's power mode: power-profiles-daemon's profile, which
//! sets the CPU's and the platform's energy preference for the whole
//! machine.
//!
//! It is the daemon's setting, not derisk's, so it applies at once rather
//! than on Save, and it is read and set through `powerprofilesctl`, on a
//! worker thread since that is a Python program that takes a moment to
//! start. power-profiles-daemon's polkit policy lets the person at the seat
//! switch it without a password.

use std::{
    process::{Command, Stdio},
    sync::mpsc,
};

use mcsapi_ui::{Theme, egui};

/// The profiles power-profiles-daemon offers, in its order, and the active
/// one.
pub type Profiles = (Vec<String>, Option<String>);

/// The profiles `powerprofilesctl list` offers, in its order, and the one
/// it marks active with `*`.
pub fn parse_list(output: &str) -> Profiles {
    let mut profiles = Vec::new();
    let mut active = None;
    for line in output.lines() {
        // A profile's line is its name and a colon, indented two columns,
        // the first of which is `*` for the active one; its details are
        // indented further.
        let (mark, rest) = line.split_at_checked(2).unwrap_or(("", ""));
        let Some(name) = rest.strip_suffix(':') else {
            continue;
        };
        if name.is_empty() || name.starts_with(char::is_whitespace) {
            continue;
        }
        if mark.starts_with('*') {
            active = Some(name.to_owned());
        }
        profiles.push(name.to_owned());
    }
    (profiles, active)
}

/// What a profile is called on the page.
pub fn label(profile: &str) -> &str {
    match profile {
        "power-saver" => "Power saver",
        "balanced" => "Balanced",
        "performance" => "Performance",
        other => other,
    }
}

/// The command that switches to `profile`.
pub fn set_argv(profile: &str) -> [&str; 3] {
    ["powerprofilesctl", "set", profile]
}

/// What power-profiles-daemon offers now, or `None` without it. It runs
/// `powerprofilesctl`, so it blocks: call it off the UI thread.
pub fn read() -> Option<Profiles> {
    let out = Command::new("powerprofilesctl")
        .arg("list")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    Some(parse_list(&String::from_utf8_lossy(&out.stdout)))
}

/// Switches the whole machine to `profile`; blocks like [`read`].
pub fn set(profile: &str) {
    let argv = set_argv(profile);
    let _ = Command::new(argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// The page's state: what the daemon last said, while a read or a switch
/// runs.
#[derive(Debug, Default)]
pub(crate) struct ProfilesUi {
    /// `Some(None)` once read and the daemon is not there.
    read: Option<Option<Profiles>>,
    pending: Option<mpsc::Receiver<Option<Profiles>>>,
}

impl ProfilesUi {
    /// Reads the profiles, after switching to `set` if given.
    fn start(&mut self, set: Option<String>) {
        let (tx, rx) = mpsc::channel();
        derisk_sys::thread::detach(move || {
            if let Some(profile) = set {
                self::set(&profile);
            }
            let _ = tx.send(read());
        });
        self.pending = Some(rx);
    }
}

/// The power mode row, in the Power page's grid.
pub(crate) fn row(ui: &mut egui::Ui, state: &mut ProfilesUi, theme: &Theme) {
    if state.read.is_none() && state.pending.is_none() {
        state.start(None);
    }
    if let Some(rx) = &state.pending {
        if let Ok(read) = rx.try_recv() {
            state.read = Some(read);
            state.pending = None;
        } else {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(200));
        }
    }
    // Without power-profiles-daemon there is nothing to choose, and the row
    // is left out rather than shown dead.
    let Some(Some((profiles, active))) = state.read.clone() else {
        return;
    };
    ui.label("Power mode");
    let mut pick = None;
    ui.add_enabled_ui(state.pending.is_none(), |ui| {
        ui.horizontal(|ui| {
            for profile in &profiles {
                if ui
                    .selectable_label(active.as_ref() == Some(profile), label(profile))
                    .clicked()
                    && active.as_ref() != Some(profile)
                {
                    pick = Some(profile.clone());
                }
            }
        });
    });
    ui.end_row();
    ui.label("");
    ui.label(
        egui::RichText::new("Applies at once, to the whole machine.")
            .small()
            .color(theme.border),
    );
    ui.end_row();
    if pick.is_some() {
        state.start(pick);
    }
}
