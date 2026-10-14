//! The libvirt machines on this computer, as derisk shows and drives them.
//!
//! On LosOS the apps a person works in run in virtual machines, and libvirt's
//! system daemon (`qemu:///system`) is the one place those machines are
//! defined: pm creates them (`pm machine create`), and derisk, virsh and
//! virt-manager all see the same list. This crate is derisk's side of that.
//!
//! - [`Machine`] is one libvirt domain as derisk shows it: its name, its
//!   [`Label`] (the colour its windows are drawn in), the template it was
//!   made from, whether it runs, and the vsock address its windows arrive
//!   from. [`Machine::from_xml`] reads one from the domain's XML, so
//!   everything derisk knows about a machine is what libvirt says.
//! - [`Hypervisor`] (not on wasm32) is a connection to libvirt: it lists
//!   machines, finds the one behind a vsock address, starts, shuts down and
//!   forces off machines, and runs a program in one through the QEMU guest
//!   agent. [`watch`] keeps a caller's copy of the list current.
//!
//! The label and template live in the domain's `<metadata>` under
//! [`METADATA_NAMESPACE`], where pm's libvirt plugin writes them. A domain
//! made some other way (virt-manager, a hand-written XML) is still a
//! machine; it is gray and has no template.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod label;
mod machine;

#[cfg(not(target_arch = "wasm32"))]
mod hypervisor;

pub use label::{Label, UnknownLabel};
pub use machine::{Machine, State, TemplateRef};

#[cfg(not(target_arch = "wasm32"))]
pub use hypervisor::{Exit, Hypervisor, Program, watch};

/// The libvirt connection every part of LosOS uses: the system daemon, the
/// only one that can give a guest a vsock device.
pub const URI: &str = "qemu:///system";

/// The XML namespace of pm's `<metadata>` on a machine's domain. pm's libvirt
/// plugin (losos-project/pm, `plugins/libvirt/src/machines.rs`) writes it;
/// this crate reads it. A change here is a change there.
pub const METADATA_NAMESPACE: &str = "https://losos-project.github.io/xmlns/pm-machine/1";

/// The `wp_security_context_v1` sandbox engine of a Wayland socket that
/// carries one machine's windows. The socket's app id is the machine's name
/// and its instance id the machine's UUID, so the compositor knows which
/// machine a window came from without asking the guest.
pub const SANDBOX_ENGINE: &str = "org.libvirt";

/// The vsock port the desktop listens on for guests' windows. A guest runs
/// its apps under `waypipe --vsock -s 2:7350 server`; 2 is the host's
/// address.
pub const DISPLAY_PORT: u32 = 7350;

/// What can go wrong between derisk and libvirt.
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum Error {
    /// libvirt refused or failed a call.
    #[error("libvirt: {0}")]
    #[diagnostic(
        code(derisk::machines::libvirt),
        help(
            "derisk talks to libvirt's system daemon at qemu:///system; `systemctl status libvirtd` says whether it runs"
        )
    )]
    Libvirt(String),
    /// No machine has that name.
    #[error("there is no machine called {0}")]
    #[diagnostic(
        code(derisk::machines::no_machine),
        help("`pm machine list` lists them")
    )]
    NoMachine(String),
    /// libvirt handed back a definition this crate cannot read.
    #[error("{name}'s definition is not readable: {reason}")]
    #[diagnostic(code(derisk::machines::definition))]
    Definition {
        /// The machine.
        name: String,
        /// What was wrong with it.
        reason: String,
    },
    /// The machine's guest agent answered something that is not a reply.
    #[error("{name}'s guest agent answered something unexpected: {reply}")]
    #[diagnostic(
        code(derisk::machines::agent),
        help("the guest agent is qemu-guest-agent in the machine; LosOS's guest template runs it")
    )]
    Agent {
        /// The machine.
        name: String,
        /// What the agent sent.
        reply: String,
    },
}

/// A result with this crate's [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;
