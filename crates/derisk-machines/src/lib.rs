//! The user's machines, as derisk shows and drives them.
//!
//! On LosOS the apps a person works in run in virtual machines of their
//! own. pm makes them (`pm machine create`) through its quickemu plugin,
//! each a folder under `$XDG_DATA_HOME/pm/machines` with pm's record of it,
//! `machine.json`, beside quickemu's `<name>.conf`, and each runs as the
//! person's own service, `pm-machine-<name>`. This crate is derisk's side
//! of that, reading the same folders pm writes.
//!
//! - [`Machine`] is one machine as derisk shows it: its name, its
//!   [`Label`] (the colour its windows are drawn in), the template it was
//!   made from, whether it runs, and the vsock address its windows arrive
//!   from. [`Machine::from_json`] reads one from its record, so everything
//!   derisk knows about a machine is what pm wrote.
//! - [`Hypervisor`] (not on wasm32) is the machines' folder: it lists
//!   machines, finds the one behind a vsock address, starts, shuts down and
//!   forces off machines, and runs a program in one through the QEMU guest
//!   agent. [`watch`] keeps a caller's copy of the list current, and
//!   [`Hypervisor::open_app`] opens an app in a machine.
//! - [`display`] (not on wasm32) brings a machine's windows to the desktop
//!   and makes sure each one is known by the machine it came from.
//! - [`flatpak`] (not on wasm32) is the machine Flatpak runs in, as
//!   Settings' Privacy page reads and changes its apps' permissions.
//! - [`MachinesApp`] is the Machines core app (`org.derisk.machines`): the
//!   list, with buttons to make machines through `pm machine create`, start,
//!   shut down, force off and remove them, and to set pm's quickemu plugin
//!   up.
//!
//! libvirt kept the machines before, in its system daemon, and this crate
//! read them from there. The daemon was a second record of every machine
//! beside its files, shared by every account and reached through polkit;
//! with quickemu a person's machines are their own files, and there is
//! nothing between derisk and them to ask.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod app;
mod label;
mod machine;
pub mod setup;

#[cfg(not(target_arch = "wasm32"))]
pub mod display;
#[cfg(not(target_arch = "wasm32"))]
pub mod flatpak;
#[cfg(not(target_arch = "wasm32"))]
mod guest;
#[cfg(not(target_arch = "wasm32"))]
mod hypervisor;
#[cfg(not(target_arch = "wasm32"))]
mod worker;

pub use app::{APP_ID, MachinesApp, colour};
pub use label::{Label, UnknownLabel};
pub use machine::{Machine, State, TemplateRef};

#[cfg(not(target_arch = "wasm32"))]
pub use guest::{GUEST_RUN, GuestApp, list_apps, parse_apps, template_apps};
#[cfg(not(target_arch = "wasm32"))]
pub use hypervisor::{Exit, Hypervisor, Program, machines_dir, screen_socket, watch};

/// The file in a machine's folder that is pm's record of it. pm's quickemu
/// plugin (losos-project/pm, `plugins/quickemu/src/machines.rs`) writes it;
/// this crate reads it. A change here is a change there.
pub const RECORD: &str = "machine.json";

/// The `wp_security_context_v1` sandbox engine of a Wayland socket that
/// carries one machine's windows. The socket's app id is the machine's name
/// and its instance id the machine's UUID, so the compositor knows which
/// machine a window came from without asking the guest.
pub const SANDBOX_ENGINE: &str = "org.quickemu";

/// The vsock port the desktop listens on for guests' windows. A guest's
/// apps draw through `waypipe --vsock -s 2:7350 server`; 2 is the host's
/// address.
pub const DISPLAY_PORT: u32 = 7350;

/// What can go wrong between derisk and the machines.
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum Error {
    /// A machine's folder, its service or its QEMU refused or failed.
    #[error("machines: {0}")]
    #[diagnostic(
        code(derisk::machines::hypervisor),
        help(
            "machines are kept under ~/.local/share/pm/machines and run as the services `systemctl --user list-units 'pm-machine-*'` lists"
        )
    )]
    Hypervisor(String),
    /// No machine has that name.
    #[error("there is no machine called {0}")]
    #[diagnostic(
        code(derisk::machines::no_machine),
        help("`pm machine list` lists them")
    )]
    NoMachine(String),
    /// A machine's record is not one this crate can read.
    #[error("{name}'s definition is not readable: {reason}")]
    #[diagnostic(code(derisk::machines::definition))]
    Definition {
        /// The machine.
        name: String,
        /// What was wrong with it.
        reason: String,
    },
    /// The machine's guest agent did not answer: the machine is off, still
    /// booting, or runs no agent.
    #[error("the guest agent of {0} does not answer")]
    #[diagnostic(
        code(derisk::machines::agent_silent),
        help("the guest agent is qemu-guest-agent in the machine; LosOS's guest template runs it")
    )]
    AgentSilent(String),
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
