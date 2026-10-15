//! The session's side of virtual machines' windows (`derisk_machines::display`):
//! a compositor socket and a waypipe client for each running machine, the
//! vsock gate that hands each guest's connection to its machine's, opening
//! apps in a machine when the settings say apps open there, and what the
//! palette and the overview show of the machines.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    process::{Child, Stdio},
    time::{Duration, Instant},
};

use crossbeam_channel::{Receiver, Sender, bounded};
use derisk_machines::{
    GuestApp, Hypervisor, Machine,
    display::{self, Sockets},
};
use mcsapi_compositor::{Command, Sandbox};
use serde_json::{Value, json};
use tracing::{info, warn};

use derisk::widgets::{CustomWidget, MAX_ROWS, WidgetRow};

/// How often libvirt is asked whether machines started or stopped.
const WATCH: Duration = Duration::from_secs(1);

/// Orders waiting for libvirt. Opening an app can take as long as its
/// machine takes to boot, and what is asked meanwhile waits its turn; past
/// this many, more are dropped with a warning rather than queued.
const ORDERS: usize = 8;

/// How long after asking a running machine for its apps to ask again while
/// it has not answered: its guest agent comes up some way into its boot.
const APPS_UNANSWERED: Duration = Duration::from_secs(20);

/// How long a machine's list of apps is kept before it is asked again, so
/// an app installed in it shows up without restarting anything.
const APPS_FRESH: Duration = Duration::from_secs(300);

/// The palette source and overview widget the session publishes its
/// machines as, which only the session itself may register
/// (`plugins/palette/machines`).
pub const SOURCE: &str = "machines";

/// The overview button that opens the Machines app.
pub const MANAGE: &str = "manage";

/// What the session asks of libvirt, carried out one after another on a
/// thread of its own, since any of it can take seconds.
enum Order {
    /// Open an app (a desktop file ID) in a machine, starting it first if
    /// it is off.
    Open { machine: String, app: String },
    /// Start a machine.
    Start(String),
    /// Ask a machine to shut down.
    ShutDown(String),
    /// Ask a running machine which apps it has.
    Apps(String),
}

/// Machines and their windows' plumbing, for one session.
pub struct MachineDisplays {
    sockets: Sockets,
    changes: Receiver<Vec<Machine>>,
    /// Ends the watching thread when this is dropped.
    _stop: Sender<()>,
    /// Orders for the ordering thread; dropping it ends that thread once
    /// it is idle.
    orders: Sender<Order>,
    /// Each machine's apps, as its guest agent listed them.
    listed: Receiver<(String, Vec<GuestApp>)>,
    machines: Vec<Machine>,
    /// The apps of each running machine that has answered.
    apps: HashMap<String, Vec<GuestApp>>,
    /// When each running machine was last asked for its apps.
    asked: HashMap<String, Instant>,
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
        let (orders, orders_rx) = bounded::<Order>(ORDERS);
        let (listed_tx, listed) = bounded(ORDERS);
        let spawned = std::thread::Builder::new()
            .name("derisk-machines-orders".into())
            .spawn(move || carry_out(&orders_rx, &listed_tx));
        if let Err(e) = spawned {
            warn!(error = %e, "cannot drive machines");
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
            orders,
            listed,
            machines: Vec::new(),
            apps: HashMap::new(),
            asked: HashMap::new(),
            clients: HashMap::new(),
            failed: HashSet::new(),
        })
    }

    /// The machine called `name`, as last seen.
    pub fn machine(&self, name: &str) -> Option<&Machine> {
        self.machines.iter().find(|m| m.name == name)
    }

    /// Opens `app` (a desktop file ID) in the machine called `machine`,
    /// starting it first if it is off. Returns `false`, opening nothing,
    /// when there is no such machine; the caller opens the app here then.
    pub fn open(&self, machine: &str, app: &str) -> bool {
        if self.machine(machine).is_none() {
            return false;
        }
        self.send(Order::Open {
            machine: machine.to_owned(),
            app: app.to_owned(),
        });
        true
    }

    /// Carries out a pick from the palette's machine rows or a press on
    /// the overview's machines card: `op` is `open` (with `app`), `start`
    /// or `shut_down`. False when there is no such machine or `op`.
    pub fn act(&self, op: &str, machine: &str, app: Option<&str>) -> bool {
        if self.machine(machine).is_none() {
            return false;
        }
        let order = match (op, app) {
            ("open", Some(app)) => Order::Open {
                machine: machine.to_owned(),
                app: app.to_owned(),
            },
            ("start", _) => Order::Start(machine.to_owned()),
            ("shut_down", _) => Order::ShutDown(machine.to_owned()),
            _ => return false,
        };
        self.send(order);
        true
    }

    fn send(&self, order: Order) {
        if self.orders.try_send(order).is_err() {
            warn!("too much asked of machines at once; dropped");
        }
    }

    /// The machines as the palette's `machines` plugin reads them: each
    /// one's name, label and whether it runs, and the apps of each running
    /// one that has said which it has.
    pub fn palette(&self) -> Value {
        palette(&self.machines, &self.apps)
    }

    /// The overview's card: how many machines run, a button starting or
    /// shutting down each, and one opening the Machines app. `None` when
    /// there are no machines, so the overview has no empty card.
    pub fn card(&self) -> Option<CustomWidget> {
        card(&self.machines)
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
        for (machine, apps) in self.listed.try_iter() {
            self.apps.insert(machine, apps);
        }
        self.ask_for_apps();
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

/// What [`MachineDisplays::palette`] says of `machines` and their `apps`.
fn palette(machines: &[Machine], apps: &HashMap<String, Vec<GuestApp>>) -> Value {
    let machines: Vec<Value> = machines
        .iter()
        .map(|m| {
            let apps: Vec<Value> = apps
                .get(&m.name)
                .into_iter()
                .flatten()
                .map(|a| json!({"id": a.id, "name": a.name}))
                .collect();
            json!({
                "name": m.name,
                "label": m.label.name(),
                "running": m.state.is_active(),
                "apps": apps,
            })
        })
        .collect();
    json!({ "machines": machines })
}

/// [`MachineDisplays::card`] for `machines`.
fn card(machines: &[Machine]) -> Option<CustomWidget> {
    if machines.is_empty() {
        return None;
    }
    let running = machines.iter().filter(|m| m.state.is_active()).count();
    let mut rows = vec![WidgetRow::Text {
        text: format!("{running} of {} running", machines.len()),
        dim: true,
    }];
    // A button each, as many as fit beside the count and the Machines
    // button; the app lists every one.
    for m in machines.iter().take(MAX_ROWS - 2) {
        let (label, op) = if m.state.is_active() {
            (format!("Shut down {}", m.name), "shut_down")
        } else {
            (format!("Start {}", m.name), "start")
        };
        rows.push(WidgetRow::Button {
            label,
            item: format!("{op}:{}", m.name),
        });
    }
    rows.push(WidgetRow::Button {
        label: "Machines…".into(),
        item: MANAGE.into(),
    });
    Some(CustomWidget {
        id: SOURCE.into(),
        title: "Machines".into(),
        icon: Some("computer".into()),
        rows,
    })
}

impl MachineDisplays {
    /// Forgets the apps of machines that stopped, and asks the running ones
    /// that have not answered lately which apps they have.
    fn ask_for_apps(&mut self) {
        let running: HashSet<&str> = self
            .machines
            .iter()
            .filter(|m| m.state.is_active())
            .map(|m| m.name.as_str())
            .collect();
        self.apps.retain(|name, _| running.contains(name.as_str()));
        self.asked.retain(|name, _| running.contains(name.as_str()));
        for name in running {
            let wait = if self.apps.contains_key(name) {
                APPS_FRESH
            } else {
                APPS_UNANSWERED
            };
            if self.asked.get(name).is_some_and(|t| t.elapsed() < wait) {
                continue;
            }
            // Asked or not, the clock restarts: a full queue now is a
            // busy thread, and asking again next second would not help.
            self.asked.insert(name.to_owned(), Instant::now());
            let _ = self.orders.try_send(Order::Apps(name.to_owned()));
        }
    }
}

/// Carries out each order it is sent, one after another, and sends back
/// the apps machines list. A read-write connection, which polkit grants an
/// administrator outright and asks anyone else about, made at the first
/// order and kept.
fn carry_out(orders: &Receiver<Order>, listed: &Sender<(String, Vec<GuestApp>)>) {
    let mut hypervisor = None;
    for order in orders {
        if hypervisor.is_none() {
            match Hypervisor::open() {
                Ok(opened) => hypervisor = Some(opened),
                Err(e) => {
                    warn!(error = %e, "cannot reach libvirt to drive a machine");
                    continue;
                }
            }
        }
        let Some(hypervisor) = &hypervisor else {
            continue;
        };
        match order {
            Order::Open { machine, app } => match hypervisor.open_app(&machine, &app) {
                Ok(()) => info!(machine, app, "opened an app in a machine"),
                Err(e) => warn!(machine, app, error = %e, "cannot open an app in a machine"),
            },
            Order::Start(machine) => match hypervisor.start(&machine) {
                Ok(()) => info!(machine, "started a machine"),
                Err(e) => warn!(machine, error = %e, "cannot start a machine"),
            },
            Order::ShutDown(machine) => match hypervisor.shut_down(&machine) {
                Ok(()) => info!(machine, "asked a machine to shut down"),
                Err(e) => warn!(machine, error = %e, "cannot shut a machine down"),
            },
            // A machine still booting has no agent yet; it is asked again.
            Order::Apps(machine) => match hypervisor.guest_apps(&machine) {
                Ok(apps) => {
                    if listed.send((machine, apps)).is_err() {
                        return;
                    }
                }
                Err(e) => tracing::debug!(machine, error = %e, "no apps from a machine yet"),
            },
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

#[cfg(test)]
mod tests {
    use derisk_machines::{Label, State};

    use super::*;

    fn machine(name: &str, label: Label, state: State) -> Machine {
        Machine {
            name: name.into(),
            uuid: String::new(),
            label,
            template: None,
            memory_mib: 2048,
            cpus: 2,
            state,
            cid: None,
        }
    }

    #[test]
    fn the_palette_hears_of_running_machines_apps() {
        let machines = [
            machine("work", Label::Blue, State::Running),
            machine("web", Label::Red, State::Off),
        ];
        let apps = HashMap::from([(
            "work".to_owned(),
            vec![GuestApp {
                id: "foot".into(),
                name: "Foot".into(),
            }],
        )]);
        assert_eq!(
            palette(&machines, &apps),
            json!({"machines": [
                {"name": "work", "label": "blue", "running": true,
                 "apps": [{"id": "foot", "name": "Foot"}]},
                {"name": "web", "label": "red", "running": false, "apps": []},
            ]})
        );
    }

    #[test]
    fn the_card_starts_and_shuts_down_and_fits_the_overview() {
        assert_eq!(card(&[]), None);
        let card = card(&[
            machine("work", Label::Blue, State::Running),
            machine("web", Label::Red, State::Off),
        ])
        .unwrap();
        card.check().unwrap();
        let items: Vec<_> = card
            .rows
            .iter()
            .map(|r| match r {
                WidgetRow::Text { text, .. } => text.clone(),
                WidgetRow::Button { label, item } => format!("{label} [{item}]"),
                WidgetRow::Progress { .. } => String::new(),
            })
            .collect();
        assert_eq!(
            items,
            [
                "1 of 2 running",
                "Shut down work [shut_down:work]",
                "Start web [start:web]",
                "Machines… [manage]",
            ]
        );
        let many: Vec<Machine> = (0..20)
            .map(|i| machine(&format!("m{i}"), Label::Gray, State::Off))
            .collect();
        super::card(&many).unwrap().check().unwrap();
    }
}
