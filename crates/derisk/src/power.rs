//! The power modes in the command palette: power-profiles-daemon's
//! profiles, published as the `power` source that the bundled `power`
//! plugin turns into "Power Mode: ..." rows, and switched when one is
//! picked.
//!
//! The profiles are read when the palette opens and again after a switch,
//! on a thread of their own, since `powerprofilesctl` is a Python program
//! that takes a moment to start (see `derisk_settings::profiles`, which the
//! Settings app's Power page uses the same way).

use crossbeam_channel::{Receiver, bounded};
use derisk_settings::profiles::{self, Profiles};
use serde_json::{Value, json};

/// The palette source the bundled plugin reads; only the shell registers it.
pub const SOURCE: &str = "power";

/// The last read of the profiles, and one under way.
#[derive(Debug, Default)]
pub struct PowerModes {
    pending: Option<Receiver<Option<Profiles>>>,
}

impl PowerModes {
    /// Reads the profiles, after switching to `set` if given. A read
    /// already under way is dropped; this one starts later and so sees
    /// any switch it would have missed.
    pub fn start(&mut self, set: Option<String>) {
        let (tx, rx) = bounded(1);
        std::thread::spawn(move || {
            if let Some(profile) = set {
                profiles::set(&profile);
            }
            let _ = tx.send(profiles::read());
        });
        self.pending = Some(rx);
    }

    /// What the last read found once it is done: `Some(None)` when
    /// power-profiles-daemon is not there.
    pub fn poll(&mut self) -> Option<Option<Profiles>> {
        let read = self.pending.as_ref()?.try_recv().ok()?;
        self.pending = None;
        Some(read)
    }
}

/// The registration for the palette.
pub fn palette((profiles, active): &Profiles) -> Value {
    let profiles: Vec<_> = profiles
        .iter()
        .map(|id| json!({"id": id, "name": profiles::label(id)}))
        .collect();
    json!({"profiles": profiles, "active": active})
}

/// The profile a palette pick asks for, if it is a switch.
pub fn picked(command: &Value) -> Option<&str> {
    (command["op"] == "set")
        .then(|| command["profile"].as_str())
        .flatten()
        .filter(|p| !p.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_names_each_profile_and_the_active_one() {
        let read = (
            vec![
                "performance".into(),
                "balanced".into(),
                "power-saver".into(),
            ],
            Some("balanced".into()),
        );
        let data = palette(&read);
        assert_eq!(data["active"], "balanced");
        assert_eq!(
            data["profiles"][2],
            json!({"id": "power-saver", "name": "Power saver"})
        );
    }

    #[test]
    fn only_a_set_with_a_profile_is_a_switch() {
        assert_eq!(
            picked(&json!({"op": "set", "profile": "power-saver"})),
            Some("power-saver")
        );
        assert_eq!(picked(&json!({"op": "set", "profile": ""})), None);
        assert_eq!(picked(&json!({"op": "start", "profile": "balanced"})), None);
    }
}
