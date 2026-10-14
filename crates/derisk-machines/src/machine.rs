use crate::{Error, Label, METADATA_NAMESPACE, Result};

/// Whether a machine runs, as libvirt reports it.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum State {
    /// Running, or blocked waiting on the host, which to a person is running.
    Running,
    /// Paused by libvirt or a person; it holds its memory but runs nothing.
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
    /// From libvirt's `virDomainState`, as `virDomainGetState` returns it.
    pub fn from_libvirt(state: u32) -> State {
        // VIR_DOMAIN_NOSTATE (0) is a domain libvirt has not looked at yet,
        // which only happens to one that is starting.
        match state {
            0..=2 => State::Running,
            3 => State::Paused,
            4 => State::ShuttingDown,
            6 => State::Crashed,
            7 => State::Suspended,
            _ => State::Off,
        }
    }

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

/// The template a machine was made from: a disk image pm turned into a
/// read-only base volume, which the machine's own disk is layered on.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TemplateRef {
    /// The template's name, from its image's file name or the package.
    pub name: String,
    /// The SHA-256 of the image, in hex. Two templates with one name but
    /// different digests are two versions of it.
    pub digest: String,
}

/// One libvirt domain, as derisk shows it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Machine {
    /// The domain's name, which is what pm and virsh call it.
    pub name: String,
    /// The domain's UUID, which stays the same for the machine's life.
    pub uuid: String,
    /// The colour its windows are drawn in.
    pub label: Label,
    /// The template it was made from, when pm made it.
    pub template: Option<TemplateRef>,
    /// Memory it is given, in MiB.
    pub memory_mib: u64,
    /// Virtual processors it is given.
    pub cpus: u32,
    /// Whether it runs.
    pub state: State,
    /// Its vsock address while it runs. libvirt picks it when the machine
    /// starts, and the guest cannot change it, so it is how the desktop
    /// knows which machine a connection came from.
    pub cid: Option<u32>,
}

impl Machine {
    /// Reads a machine from its domain XML (`virsh dumpxml`), in the state
    /// libvirt reports separately.
    ///
    /// The live XML of a running machine carries its vsock address; the
    /// inactive XML does not, and an address is only kept while the machine
    /// is [`State::is_active`].
    pub fn from_xml(xml: &str, state: State) -> Result<Machine> {
        let document = roxmltree::Document::parse(xml).map_err(|e| Error::Definition {
            name: "a machine".into(),
            reason: e.to_string(),
        })?;
        let domain = document.root_element();
        let child = |name: &str| domain.children().find(|n| n.has_tag_name(name));
        let text = |name: &str| child(name).and_then(|n| n.text()).map(str::trim);

        let name = text("name").unwrap_or_default().to_owned();
        let broken = |reason: &str| Error::Definition {
            name: name.clone(),
            reason: reason.into(),
        };
        if name.is_empty() {
            return Err(broken("it has no <name>"));
        }
        let uuid = text("uuid").ok_or_else(|| broken("it has no <uuid>"))?;

        let ours = child("metadata").and_then(|metadata| {
            metadata
                .children()
                .find(|n| n.has_tag_name((METADATA_NAMESPACE, "machine")))
        });
        let ours_child = |name: &str| {
            ours.and_then(|machine| {
                machine
                    .children()
                    .find(|n| n.has_tag_name((METADATA_NAMESPACE, name)))
            })
        };
        // A label this version does not know (a newer pm's) is drawn gray
        // rather than refusing the whole machine.
        let label = ours_child("label")
            .and_then(|n| n.text())
            .and_then(|t| t.trim().parse().ok())
            .unwrap_or_default();
        let template = ours_child("template").and_then(|n| {
            Some(TemplateRef {
                name: n.attribute("name")?.to_owned(),
                digest: n.attribute("digest")?.to_owned(),
            })
        });

        let memory = child("memory").ok_or_else(|| broken("it has no <memory>"))?;
        let amount: u64 = memory
            .text()
            .and_then(|t| t.trim().parse().ok())
            .ok_or_else(|| broken("its <memory> is not a number"))?;
        let memory_mib = to_mib(amount, memory.attribute("unit").unwrap_or("KiB"))
            .ok_or_else(|| broken("its <memory> has a unit libvirt does not use"))?;
        let cpus = text("vcpu").and_then(|t| t.parse().ok()).unwrap_or(1);

        let cid = state
            .is_active()
            .then(|| {
                child("devices")?
                    .children()
                    .find(|n| n.has_tag_name("vsock"))?
                    .children()
                    .find(|n| n.has_tag_name("cid"))?
                    .attribute("address")?
                    .parse()
                    .ok()
            })
            .flatten();

        Ok(Machine {
            uuid: uuid.to_owned(),
            name,
            label,
            template,
            memory_mib,
            cpus,
            state,
            cid,
        })
    }

    /// Whether pm made it: only those carry a template.
    pub fn from_pm(&self) -> bool {
        self.template.is_some()
    }
}

/// libvirt's memory units (`formatdomain.html#memory-allocation`), in MiB.
fn to_mib(amount: u64, unit: &str) -> Option<u64> {
    let bytes: u128 = match unit {
        "b" | "bytes" => 1,
        "KB" => 1_000,
        "k" | "KiB" => 1 << 10,
        "MB" => 1_000_000,
        "M" | "MiB" => 1 << 20,
        "GB" => 1_000_000_000,
        "G" | "GiB" => 1 << 30,
        "TB" => 1_000_000_000_000,
        "T" | "TiB" => 1 << 40,
        _ => return None,
    };
    u64::try_from(u128::from(amount) * bytes / (1 << 20)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `virsh dumpxml` gives for a running machine pm made, trimmed to
    /// what this crate reads plus enough around it to be realistic.
    const RUNNING: &str = r#"<domain type='kvm' id='4'>
  <name>work</name>
  <uuid>6b1f4b6e-0c1e-4b2a-9d0e-6f6f8a0c2d11</uuid>
  <title>blue, from losos-guest</title>
  <metadata>
    <pm:machine xmlns:pm="https://losos-project.github.io/xmlns/pm-machine/1">
      <pm:label>blue</pm:label>
      <pm:template name="losos-guest" digest="9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"/>
    </pm:machine>
  </metadata>
  <memory unit='KiB'>2097152</memory>
  <currentMemory unit='KiB'>2097152</currentMemory>
  <vcpu placement='static'>2</vcpu>
  <devices>
    <emulator>/run/libvirt/nix-emulators/qemu-system-x86_64</emulator>
    <disk type='volume' device='disk'>
      <source pool='default' volume='pm-machine-work.qcow2'/>
      <target dev='vda' bus='virtio'/>
    </disk>
    <vsock model='virtio'>
      <cid auto='yes' address='3'/>
      <alias name='vsock0'/>
    </vsock>
  </devices>
</domain>"#;

    #[test]
    fn reads_a_running_machine() {
        let machine = Machine::from_xml(RUNNING, State::Running).unwrap();
        assert_eq!(machine.name, "work");
        assert_eq!(machine.uuid, "6b1f4b6e-0c1e-4b2a-9d0e-6f6f8a0c2d11");
        assert_eq!(machine.label, Label::Blue);
        assert_eq!(
            machine.template.as_ref().map(|t| t.name.as_str()),
            Some("losos-guest")
        );
        assert_eq!(machine.memory_mib, 2048);
        assert_eq!(machine.cpus, 2);
        assert_eq!(machine.cid, Some(3));
        assert!(machine.from_pm());
    }

    #[test]
    fn a_stopped_machine_has_no_address() {
        // libvirt can leave the last address in the XML; it is not the
        // machine's while it is off, and another machine may have it now.
        let machine = Machine::from_xml(RUNNING, State::Off).unwrap();
        assert_eq!(machine.cid, None);
    }

    #[test]
    fn a_domain_pm_did_not_make_is_gray() {
        let xml = "<domain type='kvm'><name>win11</name><uuid>u</uuid>\
                   <memory>4194304</memory><devices/></domain>";
        let machine = Machine::from_xml(xml, State::Off).unwrap();
        assert_eq!(machine.label, Label::Gray);
        assert_eq!(machine.template, None);
        assert_eq!(machine.memory_mib, 4096);
        assert_eq!(machine.cpus, 1);
        assert!(!machine.from_pm());
    }

    #[test]
    fn metadata_in_another_namespace_is_not_ours() {
        let xml = RUNNING.replace(
            "https://losos-project.github.io/xmlns/pm-machine/1",
            "https://example.org/other",
        );
        let machine = Machine::from_xml(&xml, State::Running).unwrap();
        assert_eq!(machine.label, Label::Gray);
        assert_eq!(machine.template, None);
    }

    #[test]
    fn an_unknown_label_is_gray() {
        let xml = RUNNING.replace("<pm:label>blue", "<pm:label>teal");
        let machine = Machine::from_xml(&xml, State::Running).unwrap();
        assert_eq!(machine.label, Label::Gray);
    }

    #[test]
    fn memory_units() {
        let xml = RUNNING.replace("<memory unit='KiB'>2097152", "<memory unit='GiB'>3");
        assert_eq!(
            Machine::from_xml(&xml, State::Off).unwrap().memory_mib,
            3072
        );
        let xml = RUNNING.replace("<memory unit='KiB'>", "<memory unit='parsecs'>");
        assert!(Machine::from_xml(&xml, State::Off).is_err());
    }

    #[test]
    fn broken_definitions_are_errors() {
        assert!(Machine::from_xml("<domain>", State::Off).is_err());
        assert!(Machine::from_xml("<domain><uuid>u</uuid></domain>", State::Off).is_err());
        let no_uuid = "<domain><name>a</name><memory>1</memory></domain>";
        assert!(matches!(
            Machine::from_xml(no_uuid, State::Off),
            Err(Error::Definition { name, .. }) if name == "a"
        ));
    }

    #[test]
    fn states() {
        assert_eq!(State::from_libvirt(1), State::Running);
        assert_eq!(State::from_libvirt(5), State::Off);
        assert!(State::from_libvirt(3).is_active());
        assert!(!State::from_libvirt(6).is_active());
    }
}
