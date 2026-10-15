//! The session's side of virtual machines' windows (`derisk_machines::display`):
//! a compositor socket and a waypipe client for each running machine, and
//! the vsock gate that hands each guest's connection to its machine's.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    process::{Child, Stdio},
    time::Duration,
};

use crossbeam_channel::{Receiver, Sender, bounded};
use derisk_machines::{
    Hypervisor, Machine,
    display::{self, Sockets},
};
use mcsapi_compositor::{Command, Sandbox};
use tracing::{info, warn};

/// How often libvirt is asked whether machines started or stopped.
const WATCH: Duration = Duration::from_secs(1);

/// Machines and their windows' plumbing, for one session.
pub struct MachineDisplays {
    sockets: Sockets,
    changes: Receiver<Vec<Machine>>,
    /// Ends the watching thread when this is dropped.
    _stop: Sender<()>,
    machines: Vec<Machine>,
    /// The waypipe client of each machine whose windows can come in.
    clients: HashMap<String, Child>,
    /// Machines whose waypipe client could not start, not tried again
    /// until they stop: waypipe is missing, and saying so every second
    /// helps nobody.
    failed: HashSet<String>,
}

impl MachineDisplays {
    /// Starts watching libvirt and listening for guests. `None` without
    /// `$XDG_RUNTIME_DIR`, which a session always has.
    ///
    /// Where libvirt cannot be reached (a nested session on a developer's
    /// machine, say) the watch thread logs why and the session simply has
    /// no machines; likewise for the gate without vhost-vsock.
    pub fn start() -> Option<Self> {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)?;
        let sockets = Sockets::new(&runtime);
        if let Err(e) = sockets.create() {
            warn!(error = %e, "cannot make the machines' socket directory");
            return None;
        }
        let (changes_tx, changes) = bounded(4);
        let (stop, stop_rx) = bounded::<()>(0);
        let spawned = std::thread::Builder::new()
            .name("derisk-machines-watch".into())
            .spawn(move || match Hypervisor::open_read_only() {
                Ok(hypervisor) => {
                    if let Err(e) =
                        derisk_machines::watch(&hypervisor, WATCH, &changes_tx, &stop_rx)
                    {
                        warn!(error = %e, "stopped watching machines");
                    }
                }
                Err(e) => info!(error = %e, "no machines: libvirt cannot be reached"),
            });
        if let Err(e) = spawned {
            warn!(error = %e, "cannot watch machines");
        }
        let gate_sockets = sockets.clone();
        // The gate serves until the session ends; nothing joins it.
        let spawned = std::thread::Builder::new()
            .name("derisk-machines-gate".into())
            .spawn(move || {
                let served = Hypervisor::open_read_only()
                    .map_err(std::io::Error::other)
                    .and_then(|hypervisor| display::gate(&gate_sockets, &hypervisor));
                if let Err(e) = served {
                    info!(error = %e, "machines' windows cannot come in");
                }
            });
        if let Err(e) = spawned {
            warn!(error = %e, "cannot listen for machines' windows");
        }
        Some(Self {
            sockets,
            changes,
            _stop: stop,
            machines: Vec::new(),
            clients: HashMap::new(),
            failed: HashSet::new(),
        })
    }

    /// The machine called `name`, as last seen.
    pub fn machine(&self, name: &str) -> Option<&Machine> {
        self.machines.iter().find(|m| m.name == name)
    }

    /// Machines that run, in libvirt's order.
    pub fn running(&self) -> impl Iterator<Item = &Machine> {
        self.machines.iter().filter(|m| m.state.is_active())
    }

    /// Takes the latest list from libvirt and opens or closes each
    /// machine's sockets to match, queueing what the compositor has to do.
    pub fn poll(&mut self, commands: &mut Vec<Command>) {
        if let Some(latest) = self.changes.try_iter().last() {
            self.machines = latest;
        }
        // A waypipe client that died (killed, or crashed on a bad stream)
        // is started again below, while its machine runs.
        self.clients.retain(|name, child| match child.try_wait() {
            Ok(None) => true,
            Ok(Some(status)) => {
                warn!(machine = name, %status, "waypipe client exited");
                false
            }
            Err(_) => false,
        });
        let wanted: Vec<&Machine> = self
            .machines
            .iter()
            .filter(|m| m.state.is_active() && display::has_display(m))
            .collect();
        let gone: Vec<String> = self
            .clients
            .keys()
            .filter(|name| wanted.iter().all(|m| &m.name != *name))
            .cloned()
            .collect();
        self.failed
            .retain(|name| wanted.iter().any(|m| &m.name == name));
        for name in gone {
            if let Some(mut child) = self.clients.remove(&name) {
                let _ = child.kill();
                let _ = child.wait();
            }
            let _ = std::fs::remove_file(self.sockets.waypipe(&name));
            commands.push(Command::CloseSandbox(self.sockets.wayland(&name)));
            info!(
                machine = name,
                "machine stopped; its windows' socket is closed"
            );
        }
        for machine in wanted {
            if self.clients.contains_key(&machine.name) || self.failed.contains(&machine.name) {
                continue;
            }
            let [engine, app_id, instance_id] = display::sandbox(machine);
            commands.push(Command::Sandbox {
                socket: self.sockets.wayland(&machine.name),
                sandbox: Sandbox {
                    engine: Some(engine),
                    app_id: Some(app_id),
                    instance_id: Some(instance_id),
                },
            });
            // A socket left by a client of an earlier session would make
            // this one's fail to listen.
            let _ = std::fs::remove_file(self.sockets.waypipe(&machine.name));
            match self
                .sockets
                .waypipe_client(&machine.name)
                .stdin(Stdio::null())
                .spawn()
            {
                Ok(child) => {
                    info!(
                        machine = machine.name,
                        "machine runs; its windows can come in"
                    );
                    self.clients.insert(machine.name.clone(), child);
                }
                Err(e) => {
                    warn!(machine = machine.name, error = %e, "cannot start waypipe");
                    self.failed.insert(machine.name.clone());
                    commands.push(Command::CloseSandbox(self.sockets.wayland(&machine.name)));
                }
            }
        }
    }
}

impl Drop for MachineDisplays {
    fn drop(&mut self) {
        for (name, mut child) in self.clients.drain() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(self.sockets.waypipe(&name));
        }
    }
}
