//! The session's side of virtual machines' windows (`derisk_machines::display`):
//! a compositor socket and a waypipe client for each running machine, the
//! vsock gate that hands each guest's connection to its machine's, opening
//! apps in a machine when the settings say apps open there, a machine's own
//! screen in a window while it has one, and what the palette and the
//! overview show of the machines.
//!
//! A machine's apps stay in the palette while it is off: the session keeps
//! the last list each machine's guest gave under `$XDG_CACHE_HOME/derisk`,
//! and a machine that has never answered offers the list its template
//! ships (`derisk_machines::template_apps`). Picking one starts the machine.

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
    setup,
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

/// The URI virt-viewer reaches pm's machines at.
const LIBVIRT_URI: &str = "qemu:///system";

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
    /// Each machine's apps: what it listed last, running or not, or what
    /// its template ships until it has listed its own.
    apps: HashMap<String, Vec<GuestApp>>,
    /// When each running machine last listed its apps, since it started.
    answered: HashMap<String, Instant>,
    /// When each running machine was last asked for its apps.
    asked: HashMap<String, Instant>,
    /// Where each machine's last list is kept, by its UUID, so the palette
    /// offers its apps in the next session too; `None` without a cache
    /// directory.
    cache: Option<PathBuf>,
    /// The waypipe client of each machine whose windows can come in.
    clients: HashMap<String, Child>,
    /// Machines whose waypipe client could not start, not tried again
    /// until they stop: waypipe is missing, and saying so every second
    /// helps nobody.
    failed: HashSet<String>,
    /// The virt-viewer showing the screen of each running machine that has
    /// one (an installer's, or Windows' whole desktop).
    viewers: HashMap<String, Child>,
    /// Running machines whose screen is not shown: its window was closed,
    /// or virt-viewer could not start. Shown again when the palette asks,
    /// or the next time the machine starts.
    hidden: HashSet<String>,
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
            answered: HashMap::new(),
            asked: HashMap::new(),
            cache: cache_dir(),
            clients: HashMap::new(),
            failed: HashSet::new(),
            viewers: HashMap::new(),
            hidden: HashSet::new(),
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
    /// the overview's machines card: `op` is `open` (with `app`), `start`,
    /// `shut_down` or `screen`. False when there is no such machine or `op`.
    pub fn act(&mut self, op: &str, machine: &str, app: Option<&str>) -> bool {
        let Some(found) = self.machine(machine) else {
            return false;
        };
        if op == "screen" {
            // The next poll opens it, as it does for a machine that just
            // started.
            let shown = found.screen && found.state.is_active();
            if shown {
                self.hidden.remove(machine);
            }
            return shown;
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
    /// one's name, label and whether it runs, and the apps it has. The
    /// OS's service machines are there for their apps, such as the
    /// Flatpak machine's, marked so the plugin offers nothing else of them.
    pub fn palette(&self) -> Value {
        palette(&self.machines, &self.apps)
    }

    /// The overview's card: how many machines run, a button starting or
    /// shutting down each, and one opening the Machines app. `None` when
    /// there are no machines, so the overview has no empty card.
    pub fn card(&self) -> Option<CustomWidget> {
        card(&self.people())
    }

    /// Machines that run, in libvirt's order, other than the OS's own.
    pub fn running(&self) -> impl Iterator<Item = &Machine> {
        self.machines
            .iter()
            .filter(|m| m.state.is_active() && !m.service)
    }

    /// The machines a person has, which the overview and the top bar show:
    /// not the OS's service machines, such as the Tor gateway, whose
    /// windows (if any) are still framed through [`Self::machine`].
    fn people(&self) -> Vec<Machine> {
        self.machines
            .iter()
            .filter(|m| !m.service)
            .cloned()
            .collect()
    }

    /// Takes the latest list from libvirt and opens or closes each
    /// machine's sockets to match, queueing what the compositor has to do.
    pub fn poll(&mut self, commands: &mut Vec<Command>) {
        if let Some(latest) = self.changes.try_iter().last() {
            self.machines = latest;
            self.recall_apps();
        }
        for (machine, apps) in self.listed.try_iter() {
            self.answered.insert(machine.clone(), Instant::now());
            if let Some(found) = self.machine(&machine) {
                self.keep_apps(&found.uuid, &apps);
            }
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
        // Before the loop below opens new machines' sockets: a machine's
        // screen is shown on its socket, so it waits for the poll after the
        // compositor has made it.
        self.show_screens();
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
            // Its viewer waits for it to start again (`--reconnect`), but
            // shows nothing meanwhile; and a screen hidden while it ran is
            // shown when it starts again.
            if let Some(mut child) = self.viewers.remove(&name) {
                let _ = child.kill();
                let _ = child.wait();
            }
            self.hidden.remove(&name);
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

/// virt-viewer on `machine`'s screen, a client of `socket`.
fn viewer(machine: &str, socket: &std::path::Path) -> std::process::Command {
    let mut command = std::process::Command::new("virt-viewer");
    command
        .args(["--connect", LIBVIRT_URI, "--attach", "--reconnect", "--"])
        .arg(machine)
        // An absolute WAYLAND_DISPLAY is used as the socket's path, and GTK
        // would take an X display over it if it had one.
        .env("WAYLAND_DISPLAY", socket)
        .env("GDK_BACKEND", "wayland")
        .env_remove("WAYLAND_SOCKET")
        .env_remove("DISPLAY")
        .stdin(Stdio::null());
    command
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
                "screen": m.screen,
                "service": m.service,
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
    /// Opens a virt-viewer on the screen of each running machine that has
    /// one, its socket open and its screen not hidden, and notes which
    /// viewers were closed.
    ///
    /// virt-viewer is a client of the machine's own socket, not the
    /// session's, so its window is framed in the machine's colour like the
    /// machine's apps' windows are, and whatever the compositor holds back
    /// from an untrusted machine it holds back from its screen too.
    fn show_screens(&mut self) {
        let mut closed = Vec::new();
        self.viewers.retain(|name, child| match child.try_wait() {
            Ok(None) => true,
            _ => {
                closed.push(name.clone());
                false
            }
        });
        for name in closed {
            info!(machine = name, "machine's screen closed");
            self.hidden.insert(name);
        }
        let screens: Vec<String> = self
            .machines
            .iter()
            .filter(|m| m.screen && m.state.is_active() && display::has_display(m))
            .map(|m| m.name.clone())
            .collect();
        for name in &screens {
            let socket = self.sockets.wayland(name);
            if self.viewers.contains_key(name)
                || self.hidden.contains(name)
                || !self.clients.contains_key(name)
                || !socket.exists()
            {
                continue;
            }
            match viewer(name, &socket).spawn() {
                Ok(child) => {
                    info!(machine = name, "showing the machine's screen");
                    self.viewers.insert(name.clone(), child);
                }
                Err(e) => {
                    warn!(machine = name, error = %e, "cannot start virt-viewer");
                    self.hidden.insert(name.clone());
                }
            }
        }
    }

    /// Forgets the apps of machines that are gone, and finds those of
    /// machines it has none for yet: the list the machine gave last, kept
    /// on disk, or else the one its template ships.
    fn recall_apps(&mut self) {
        let names: HashSet<&str> = self.machines.iter().map(|m| m.name.as_str()).collect();
        self.apps.retain(|name, _| names.contains(name.as_str()));
        let data_dirs = setup::data_dirs();
        for machine in &self.machines {
            if self.apps.contains_key(&machine.name) || machine.is_windows() {
                continue;
            }
            let kept = self
                .cache
                .as_ref()
                .and_then(|dir| std::fs::read_to_string(apps_file(dir, &machine.uuid)).ok())
                .map(|list| derisk_machines::parse_apps(&list).collect());
            let apps = kept.or_else(|| {
                let template = machine.template.as_ref()?;
                derisk_machines::template_apps(&data_dirs, &template.name)
            });
            if let Some(apps) = apps {
                self.apps.insert(machine.name.clone(), apps);
            }
        }
    }

    /// Writes the machine `uuid`'s list of apps where the next session finds
    /// it. A list that cannot be kept only costs a later session the
    /// machine's apps until it runs again.
    fn keep_apps(&self, uuid: &str, apps: &[GuestApp]) {
        let Some(dir) = &self.cache else {
            return;
        };
        let file = apps_file(dir, uuid);
        let kept = std::fs::create_dir_all(dir)
            .and_then(|()| std::fs::write(&file, derisk_machines::list_apps(apps)));
        if let Err(e) = kept {
            tracing::debug!(file = %file.display(), error = %e, "cannot keep a machine's apps");
        }
    }

    /// Asks the running machines that have not answered lately which apps
    /// they have, and forgets when it last asked those that stopped, so
    /// each is asked again once it starts. Windows machines are not asked:
    /// their apps open through WinApps, not the guest agent, which has no
    /// `losos-guest-run` to run there.
    fn ask_for_apps(&mut self) {
        let running: HashSet<&str> = self
            .machines
            .iter()
            .filter(|m| m.state.is_active())
            .map(|m| m.name.as_str())
            .collect();
        self.answered
            .retain(|name, _| running.contains(name.as_str()));
        self.asked.retain(|name, _| running.contains(name.as_str()));
        for machine in &self.machines {
            let name = machine.name.as_str();
            if !running.contains(name) || machine.is_windows() {
                continue;
            }
            if self
                .answered
                .get(name)
                .is_some_and(|t| t.elapsed() < APPS_FRESH)
            {
                continue;
            }
            if self
                .asked
                .get(name)
                .is_some_and(|t| t.elapsed() < APPS_UNANSWERED)
            {
                continue;
            }
            // Asked or not, the clock restarts: a full queue now is a
            // busy thread, and asking again next second would not help.
            self.asked.insert(name.to_owned(), Instant::now());
            let _ = self.orders.try_send(Order::Apps(name.to_owned()));
        }
    }
}

/// Where the session keeps machines' lists of apps:
/// `$XDG_CACHE_HOME/derisk/machines`, or `~/.cache/derisk/machines`.
fn cache_dir() -> Option<PathBuf> {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    Some(cache.join("derisk").join("machines"))
}

/// The file the machine `uuid`'s list of apps is kept in. By UUID, not
/// name, so a machine made again under an old name starts from its own
/// template's list rather than the old machine's.
fn apps_file(dir: &std::path::Path, uuid: &str) -> PathBuf {
    let uuid: String = uuid
        .chars()
        .filter(|c| c.is_ascii_hexdigit() || *c == '-')
        .collect();
    dir.join(format!("{uuid}.apps"))
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
        for (_, mut child) in self.viewers.drain() {
            let _ = child.kill();
            let _ = child.wait();
        }
        for (name, mut child) in self.clients.drain() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(self.sockets.waypipe(&name));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use derisk_machines::{Label, State};

    use super::*;

    fn machine(name: &str, label: Label, state: State) -> Machine {
        Machine {
            name: name.into(),
            uuid: String::new(),
            label,
            template: None,
            system: None,
            screen: false,
            memory_mib: 2048,
            cpus: 2,
            state,
            cid: None,
            service: false,
        }
    }

    #[test]
    fn the_palette_hears_of_machines_apps_running_or_not() {
        let machines = [
            machine("work", Label::Blue, State::Running),
            machine("flatpak", Label::Purple, State::Off),
            machine("web", Label::Red, State::Off),
        ];
        let apps = HashMap::from([
            (
                "work".to_owned(),
                vec![GuestApp {
                    id: "foot".into(),
                    name: "Foot".into(),
                }],
            ),
            (
                "flatpak".to_owned(),
                vec![GuestApp {
                    id: "io.github.kolunmi.Bazaar".into(),
                    name: "Bazaar".into(),
                }],
            ),
        ]);
        assert_eq!(
            palette(&machines, &apps),
            json!({"machines": [
                {"name": "work", "label": "blue", "running": true, "screen": false, "service": false,
                 "apps": [{"id": "foot", "name": "Foot"}]},
                {"name": "flatpak", "label": "purple", "running": false, "screen": false, "service": false,
                 "apps": [{"id": "io.github.kolunmi.Bazaar", "name": "Bazaar"}]},
                {"name": "web", "label": "red", "running": false, "screen": false, "service": false,
                 "apps": []},
            ]})
        );
    }

    #[test]
    fn a_machines_apps_are_kept_by_its_uuid_alone() {
        let dir = std::path::Path::new("/home/u/.cache/derisk/machines");
        assert_eq!(
            apps_file(dir, "6f1c2d9e-0a7b-4e33-9c1d-2b8f5e6a7c90"),
            dir.join("6f1c2d9e-0a7b-4e33-9c1d-2b8f5e6a7c90.apps")
        );
        // Whatever libvirt says the UUID is, the file stays in the directory.
        assert_eq!(apps_file(dir, "../../x"), dir.join(".apps"));
    }

    #[test]
    fn a_screen_is_shown_on_its_machines_socket_and_nowhere_else() {
        let socket = std::path::Path::new("/run/user/1000/derisk/machines/win.wayland");
        let command = viewer("win", socket);
        assert_eq!(command.get_program(), "virt-viewer");
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(
            args,
            [
                "--connect",
                LIBVIRT_URI,
                "--attach",
                "--reconnect",
                "--",
                "win"
            ]
        );
        let env: HashMap<&OsStr, Option<&OsStr>> = command.get_envs().collect();
        assert_eq!(env[OsStr::new("WAYLAND_DISPLAY")], Some(socket.as_os_str()));
        assert_eq!(env[OsStr::new("GDK_BACKEND")], Some(OsStr::new("wayland")));
        assert_eq!(env[OsStr::new("DISPLAY")], None);
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
