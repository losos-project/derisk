//! The Machines app's thread: everything that talks to libvirt or runs pm.
//!
//! The app is drawn on the compositor's thread, between every other
//! window's frames, so nothing it does there may wait. libvirt answers on a
//! local socket but can take seconds (a polkit prompt, a machine slow to
//! shut down), and `pm machine create` uploads a template of gigabytes. So
//! the app hands orders to this thread and reads its events each frame.

use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};

use crossbeam_channel::{Receiver, Sender, bounded, select};

use crate::{Hypervisor, Label, Machine, URI};

/// How often the list is read again when nothing has been asked of the
/// thread, so a machine started from virsh or pm shows up.
const REFRESH: Duration = Duration::from_secs(2);

/// What the app asks for.
#[derive(Clone, Debug)]
pub(crate) enum Order {
    Start(String),
    ShutDown(String),
    ForceOff(String),
    Create(Create),
    Remove(String),
    /// Ask a running machine which apps it has.
    Apps(String),
    /// Open an app in a machine, starting it first if it is off.
    Open {
        machine: String,
        app: crate::GuestApp,
    },
    /// Copy the OS's libvirt plugin into the person's plugin directory and
    /// sign it with their own key.
    SetUpPlugin {
        shipped: PathBuf,
        installed: PathBuf,
    },
}

/// `pm machine create`'s arguments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Create {
    pub name: String,
    pub image: PathBuf,
    pub label: Label,
    pub memory_mib: u32,
    pub cpus: u32,
    pub disk_gib: u32,
}

impl Create {
    /// The pm command line, after `pm`.
    pub fn args(&self) -> Vec<String> {
        vec![
            "machine".into(),
            "create".into(),
            self.name.clone(),
            "--image".into(),
            self.image.display().to_string(),
            "--label".into(),
            self.label.name().into(),
            "--memory".into(),
            self.memory_mib.to_string(),
            "--cpus".into(),
            self.cpus.to_string(),
            "--disk".into(),
            self.disk_gib.to_string(),
        ]
    }
}

/// What the thread tells the app.
#[derive(Clone, Debug)]
pub(crate) enum Event {
    /// The machines, whenever they change.
    Machines(Vec<Machine>),
    /// libvirt cannot be reached, and why.
    Unavailable(String),
    /// A line of what pm is printing.
    Line(String),
    /// An order finished: what to tell the person, or what went wrong.
    Done(Result<String, String>),
    /// The apps a machine has, or why they could not be listed.
    Apps {
        machine: String,
        apps: Result<Vec<crate::GuestApp>, String>,
    },
}

/// The app's end of the thread. Dropping it ends the thread at its next
/// look at the channels.
pub(crate) struct Worker {
    orders: Sender<Order>,
    events: Receiver<Event>,
}

impl Worker {
    pub fn start() -> Worker {
        let (orders, orders_rx) = bounded(8);
        let (events_tx, events) = bounded(64);
        let spawned = std::thread::Builder::new()
            .name("derisk-machines".into())
            .spawn(move || run(&orders_rx, &events_tx));
        if let Err(error) = spawned {
            tracing::error!(%error, "cannot start the machines thread");
        }
        Worker { orders, events }
    }

    /// Hands `order` over; false when the thread has gone, or already has
    /// as many orders as it takes.
    pub fn send(&self, order: Order) -> bool {
        self.orders.try_send(order).is_ok()
    }

    /// Whatever the thread has said since the last call.
    pub fn events(&self) -> impl Iterator<Item = Event> + '_ {
        self.events.try_iter()
    }
}

fn run(orders: &Receiver<Order>, events: &Sender<Event>) {
    let looking = match Hypervisor::open_read_only() {
        Ok(hypervisor) => hypervisor,
        Err(error) => {
            let _ = events.send(Event::Unavailable(error.to_string()));
            return;
        }
    };
    // Opened at the first order that changes something: polkit may ask for a
    // password, and only a person pressing a button should cause that.
    let mut changing: Option<Hypervisor> = None;
    let mut last: Option<Vec<Machine>> = None;
    loop {
        match looking.machines() {
            Ok(machines) => {
                let machines: Vec<Machine> = machines.collect();
                if last.as_ref() != Some(&machines) {
                    last = Some(machines.clone());
                    if events.send(Event::Machines(machines)).is_err() {
                        return;
                    }
                }
            }
            Err(error) => {
                last = None;
                if events.send(Event::Unavailable(error.to_string())).is_err() {
                    return;
                }
            }
        }
        select! {
            recv(orders) -> order => match order {
                // Asked in the background, when a machine is chosen: its
                // answer is a list, not something the person did.
                Ok(Order::Apps(machine)) => {
                    let apps = changing_connection(&mut changing)
                        .and_then(|h| h.guest_apps(&machine).map_err(|e| e.to_string()));
                    if events.send(Event::Apps { machine, apps }).is_err() {
                        return;
                    }
                }
                Ok(order) => {
                    let done = carry_out(order, &mut changing, events);
                    if events.send(Event::Done(done)).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            },
            default(REFRESH) => {}
        }
    }
}

fn carry_out(
    order: Order,
    changing: &mut Option<Hypervisor>,
    events: &Sender<Event>,
) -> Result<String, String> {
    match order {
        Order::Start(name) => changing_connection(changing)?
            .start(&name)
            .map(|()| format!("Started {name}"))
            .map_err(|e| e.to_string()),
        Order::ShutDown(name) => changing_connection(changing)?
            .shut_down(&name)
            .map(|()| format!("Asked {name} to shut down"))
            .map_err(|e| e.to_string()),
        Order::ForceOff(name) => changing_connection(changing)?
            .force_off(&name)
            .map(|()| format!("Forced {name} off"))
            .map_err(|e| e.to_string()),
        Order::Create(create) => {
            pm(&create.args(), events)?;
            Ok(format!("Made {}", create.name))
        }
        Order::Remove(name) => {
            pm(&["machine".into(), "remove".into(), name.clone()], events)?;
            Ok(format!("Removed {name}"))
        }
        Order::Open { machine, app } => changing_connection(changing)?
            .open_app(&machine, &app.id)
            .map(|()| format!("Opened {} in {machine}", app.name))
            .map_err(|e| e.to_string()),
        // `run` answers it with Event::Apps before it would come here.
        Order::Apps(machine) => Err(format!("listing {machine}'s apps is not an action")),
        Order::SetUpPlugin { shipped, installed } => {
            set_up_plugin(&shipped, &installed, events)?;
            Ok("pm can manage machines now".into())
        }
    }
}

/// The read-write connection, opened the first time it is needed.
fn changing_connection(slot: &mut Option<Hypervisor>) -> Result<&Hypervisor, String> {
    if slot.is_none() {
        *slot = Some(Hypervisor::open().map_err(|e| e.to_string())?);
    }
    Ok(slot.as_ref().expect("opened above"))
}

/// Copies the OS's plugin in and signs it with `pm sign`, which makes the
/// person a key and trusts it the first time.
fn set_up_plugin(
    shipped: &std::path::Path,
    installed: &std::path::Path,
    events: &Sender<Event>,
) -> Result<(), String> {
    let dir = installed
        .parent()
        .ok_or_else(|| format!("{} has no directory", installed.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    // A copy, not a link: the signature covers these bytes, and an update
    // that changed the file under a link would leave pm refusing it.
    std::fs::copy(shipped, installed)
        .map_err(|e| format!("cannot copy {}: {e}", shipped.display()))?;
    pm(&["sign".into(), installed.display().to_string()], events)
}

/// Runs pm with `args`, sending each line it prints, and fails with its
/// last line of complaint when it fails.
fn pm(args: &[String], events: &Sender<Event>) -> Result<(), String> {
    let mut child = Command::new("pm")
        .args(args)
        // virsh, which pm's plugin runs, connects where this says. LosOS
        // makes it the default for everyone; saying so here keeps a person
        // whose environment says otherwise from making machines the
        // desktop cannot see.
        .env("LIBVIRT_DEFAULT_URI", URI)
        // Its lines are shown as they are, in the app rather than a
        // terminal: without this its log lines carry colour escapes.
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run pm: {e}"))?;
    let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
    let (lines_tx, lines) = bounded::<(bool, String)>(64);
    let mut complaints = Vec::new();
    std::thread::scope(|scope| {
        let streams: [(bool, Option<Box<dyn std::io::Read + Send>>); 2] = [
            (false, stdout.map(|s| Box::new(s) as _)),
            (true, stderr.map(|s| Box::new(s) as _)),
        ];
        for (is_stderr, stream) in streams {
            let Some(stream) = stream else { continue };
            let lines_tx = lines_tx.clone();
            scope.spawn(move || {
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    if lines_tx.send((is_stderr, line)).is_err() {
                        return;
                    }
                }
            });
        }
        drop(lines_tx);
        for (is_stderr, line) in lines {
            if is_stderr && !line.trim().is_empty() {
                complaints.push(line.clone());
            }
            let _ = events.send(Event::Line(line));
        }
    });
    let status = child.wait().map_err(|e| format!("pm: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(complaint(&complaints).unwrap_or_else(|| format!("pm failed ({status})")))
    }
}

/// The line of pm's standard error that says what went wrong: miette
/// marks the report's message with `×` (or `Error:` without its graphical
/// theme); a warning pm logged on the way is not it.
fn complaint(stderr: &[String]) -> Option<String> {
    stderr
        .iter()
        .find_map(|line| {
            let line = line.trim();
            line.strip_prefix('×')
                .or_else(|| line.strip_prefix("Error:"))
                .map(|message| message.trim().to_owned())
        })
        .or_else(|| stderr.last().map(|line| line.trim().to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complaints() {
        let stderr = [
            "WARN generated a new signing key".to_owned(),
            "  × no plugin manages machines here, so pm cannot create work".to_owned(),
            "  help: see plugins/README.md".to_owned(),
        ];
        assert_eq!(
            complaint(&stderr).as_deref(),
            Some("no plugin manages machines here, so pm cannot create work")
        );
        assert_eq!(
            complaint(&["Error: virsh failed".to_owned()]).as_deref(),
            Some("virsh failed")
        );
        assert_eq!(complaint(&["odd".to_owned()]).as_deref(), Some("odd"));
        assert_eq!(complaint(&[]), None);
    }

    #[test]
    fn create_arguments() {
        let create = Create {
            name: "work".into(),
            image: "/run/current-system/sw/share/losos/machines/losos-guest.qcow2".into(),
            label: Label::Blue,
            memory_mib: 4096,
            cpus: 4,
            disk_gib: 64,
        };
        assert_eq!(
            create.args().join(" "),
            "machine create work \
             --image /run/current-system/sw/share/losos/machines/losos-guest.qcow2 \
             --label blue --memory 4096 --cpus 4 --disk 64"
        );
    }
}
