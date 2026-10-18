use serde_json::Value;

use crate::{Error, Label, Result};

/// Whether a machine runs.
///
/// quickemu tells only whether QEMU is there, so a machine is
/// [`State::Running`] or [`State::Off`]; the rest are kept for what a
/// desktop may come to know of a machine some other way.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum State {
    /// Running, or blocked waiting on the host, which to a person is running.
    Running,
    /// Paused by a person; it holds its memory but runs nothing.
    Paused,
    /// Asked to shut down and on its way.
    ShuttingDown,
    /// Not running.
    #[default]
    Off,
    /// Stopped because the guest crashed.
    Crashed,
    /// Suspended by the guest itself (suspend to RAM).
    Suspended,
}

impl State {
    /// Whether the machine has a QEMU process, and so can have windows.
    pub fn is_active(self) -> bool {
        matches!(
            self,
            State::Running | State::Paused | State::ShuttingDown | State::Suspended
        )
    }

    /// The word for it in a list of machines.
    pub fn describe(self) -> &'static str {
        match self {
            State::Running => "Running",
            State::Paused => "Paused",
            State::ShuttingDown => "Shutting down",
            State::Off => "Off",
            State::Crashed => "Crashed",
            State::Suspended => "Suspended",
        }
    }
}

/// The template a machine was made from: a disk image pm copied in
/// read-only, which the machine's own disk is layered on.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TemplateRef {
    /// The template's name, from its image's file name or the package.
    pub name: String,
    /// The SHA-256 of the image, in hex. Two templates with one name but
    /// different digests are two versions of it.
    pub digest: String,
}

/// One of the user's machines, as derisk shows it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Machine {
    /// Its name, which is what pm and quickemu call it.
    pub name: String,
    /// Its UUID, which pm drew when it made it and which stays the same for
    /// the machine's life.
    pub uuid: String,
    /// The colour its windows are drawn in.
    pub label: Label,
    /// The template it was made from, when pm made it from one.
    pub template: Option<TemplateRef>,
    /// The system it installed from CDs, when pm made it to install one
    /// (`pm machine create --installer`): `linux` or `windows`.
    pub system: Option<String>,
    /// Whether it is seen through its screen rather than as windows on the
    /// desktop: a machine installing from a CD is, for its installer, and a
    /// Windows machine for its whole desktop.
    pub screen: bool,
    /// Memory it is given, in MiB.
    pub memory_mib: u64,
    /// Virtual processors it is given.
    pub cpus: u32,
    /// Whether it runs.
    pub state: State,
    /// Its vsock address while it runs. pm drew it when it made the
    /// machine, QEMU gives it to the machine's device, and the guest cannot
    /// change it, so it is how the desktop knows which machine a connection
    /// came from.
    pub cid: Option<u32>,
}

impl Machine {
    /// Reads a machine from pm's record of it (`machine.json`, which pm's
    /// quickemu plugin writes beside its disk), in the state its QEMU is in.
    ///
    /// The vsock address is kept only while the machine
    /// [`State::is_active`]: it is how a connection is told to be the
    /// machine's, and one that is off sends none.
    pub fn from_json(text: &str, state: State) -> Result<Machine> {
        let record: Value = serde_json::from_str(text).map_err(|e| Error::Definition {
            name: "a machine".into(),
            reason: e.to_string(),
        })?;
        let string = |key: &str| record[key].as_str().map(str::to_owned);
        let name = string("name").unwrap_or_default();
        let broken = |reason: &str| Error::Definition {
            name: name.clone(),
            reason: reason.into(),
        };
        if name.is_empty() {
            return Err(broken("it has no name"));
        }
        let uuid = string("uuid").ok_or_else(|| broken("it has no uuid"))?;
        // A label this version does not know (a newer pm's) is drawn gray
        // rather than refusing the whole machine.
        let label = record["label"]
            .as_str()
            .and_then(|label| label.parse().ok())
            .unwrap_or_default();
        let template = record["template"].as_object().and_then(|template| {
            Some(TemplateRef {
                name: template.get("name")?.as_str()?.to_owned(),
                digest: template.get("digest")?.as_str()?.to_owned(),
            })
        });
        let system = string("system").filter(|system| !system.is_empty());
        let memory_mib = record["memory_mib"]
            .as_u64()
            .ok_or_else(|| broken("its memory is not a number"))?;
        let cpus = record["cpus"]
            .as_u64()
            .and_then(|cpus| u32::try_from(cpus).ok())
            .unwrap_or(1);
        let cid = state
            .is_active()
            .then(|| {
                record["cid"]
                    .as_u64()
                    .and_then(|cid| u32::try_from(cid).ok())
            })
            .flatten();
        Ok(Machine {
            name,
            uuid,
            label,
            template,
            system,
            screen: record["screen"].as_bool().unwrap_or(false),
            memory_mib,
            cpus,
            state,
            cid,
        })
    }

    /// Whether pm made it: only those carry a template or a system.
    pub fn from_pm(&self) -> bool {
        self.template.is_some() || self.system.is_some()
    }

    /// Whether it runs Windows, which has no `losos-guest-run` and no
    /// waypipe: its windows are its screen, or WinApps'.
    pub fn is_windows(&self) -> bool {
        self.system.as_deref() == Some("windows")
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// What pm's quickemu plugin writes for a machine made from a template.
    pub(crate) const RECORD_JSON: &str = r#"{
  "name": "work",
  "uuid": "6b1f4b6e-0c1e-4b2a-9d0e-6f6f8a0c2d11",
  "label": "blue",
  "template": {"name": "losos-guest", "digest": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"},
  "system": null,
  "media": [],
  "screen": false,
  "memory_mib": 2048,
  "cpus": 2,
  "cid": 66054
}
"#;

    #[test]
    fn reads_a_running_machine() {
        let machine = Machine::from_json(RECORD_JSON, State::Running).unwrap();
        assert_eq!(machine.name, "work");
        assert_eq!(machine.uuid, "6b1f4b6e-0c1e-4b2a-9d0e-6f6f8a0c2d11");
        assert_eq!(machine.label, Label::Blue);
        assert_eq!(
            machine.template.as_ref().map(|t| t.name.as_str()),
            Some("losos-guest")
        );
        assert_eq!(machine.memory_mib, 2048);
        assert_eq!(machine.cpus, 2);
        assert_eq!(machine.cid, Some(66054));
        assert!(machine.from_pm());
        assert!(!machine.screen && !machine.is_windows());
    }

    #[test]
    fn reads_a_windows_machine_with_a_screen() {
        let json = RECORD_JSON
            .replace(
                r#"{"name": "losos-guest", "digest": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"}"#,
                "null",
            )
            .replace(r#""system": null"#, r#""system": "windows""#)
            .replace(r#""screen": false"#, r#""screen": true"#)
            .replace(r#""cid": 66054"#, r#""cid": null"#);
        let machine = Machine::from_json(&json, State::Running).unwrap();
        assert_eq!(machine.template, None);
        assert_eq!(machine.system.as_deref(), Some("windows"));
        assert!(machine.is_windows());
        assert!(machine.screen);
        assert!(machine.from_pm());
        assert_eq!(machine.cid, None);
    }

    #[test]
    fn a_stopped_machine_has_no_address() {
        // The record keeps the address for the machine's life; it is not
        // the machine's to send from while it is off.
        let machine = Machine::from_json(RECORD_JSON, State::Off).unwrap();
        assert_eq!(machine.cid, None);
    }

    #[test]
    fn an_unknown_label_is_gray() {
        let json = RECORD_JSON.replace(r#""label": "blue""#, r#""label": "teal""#);
        let machine = Machine::from_json(&json, State::Running).unwrap();
        assert_eq!(machine.label, Label::Gray);
    }

    #[test]
    fn broken_records_are_errors() {
        assert!(Machine::from_json("{", State::Off).is_err());
        assert!(Machine::from_json(r#"{"uuid": "u", "memory_mib": 1}"#, State::Off).is_err());
        let no_uuid = r#"{"name": "a", "memory_mib": 1}"#;
        assert!(matches!(
            Machine::from_json(no_uuid, State::Off),
            Err(Error::Definition { name, .. }) if name == "a"
        ));
        let no_memory = r#"{"name": "a", "uuid": "u"}"#;
        assert!(Machine::from_json(no_memory, State::Off).is_err());
    }

    #[test]
    fn states() {
        assert!(State::Running.is_active());
        assert!(State::Paused.is_active());
        assert!(!State::Off.is_active());
        assert!(!State::Crashed.is_active());
    }
}
