//! The machine Flatpak runs in, as Settings' Privacy page sees it.
//!
//! On LosOS's PCs Flatpak, Flathub and their store run in a machine of
//! their own, made from the template [`TEMPLATE`] (losos-desktop,
//! `nixos/guest/flatpak.nix`), and nowhere on the host. The Privacy page
//! still lists those apps and changes what they may reach: through
//! [`FlatpakMachine`], an [`Installation`] whose files are in the guest.
//!
//! Everything goes through the guest agent to `losos-guest-flatpak`, which
//! the template has at [`GUEST_FLATPAK`]. Its contract:
//!
//! - `losos-guest-flatpak apps` prints a line `global <TAB> <overrides>`,
//!   then one line per installed app: `app`, its ID, `user` or `system`,
//!   its name, its `metadata` keyfile and the guest user's overrides of
//!   it, separated by tabs, both files base64-encoded.
//! - `losos-guest-flatpak overrides <app>` makes what it reads on standard
//!   input the guest user's overrides of `app`; empty input removes them.
//! - `losos-guest-flatpak flatpak <permission-show|permission-set|permission-remove> ...`
//!   runs that `flatpak` command as the guest user, in its session, where
//!   the portals' permission store is.
//!
//! The agent is a round trip away and the page is drawn by the compositor,
//! so all of it runs on a thread of its own and the page is told what is
//! known so far.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use base64::{Engine, engine::general_purpose::STANDARD};
use crossbeam_channel::{Receiver, Sender, bounded};
use derisk_settings::flatpak::{
    App, Context, Installation, Portal, Scan, parse_permission_show, portal_args,
};

use crate::{Error, Hypervisor, Program, Result};

/// The name of the template the Flatpak machine is made from. Settings
/// takes the first machine made from it for Flatpak's.
pub const TEMPLATE: &str = "losos-flatpak";

/// Where the template has `losos-guest-flatpak`.
pub const GUEST_FLATPAK: &str = "/run/current-system/sw/bin/losos-guest-flatpak";

/// How long `losos-guest-flatpak` may take: listing the apps reads a few
/// files per app, and `flatpak permission-*` may start the permission
/// store first.
const GUEST_WAIT: Duration = Duration::from_secs(15);

/// How long after a look found the machine off or still booting to look
/// again, while the page asks.
const LOOK_AGAIN: Duration = Duration::from_secs(1);

/// Finds the machine Flatpak runs in: the first made from [`TEMPLATE`].
/// `None` without one, or without libvirt, when the page reads this
/// computer's own Flatpak instead. Fits
/// [`derisk_settings::flatpak::FindInstallation`].
pub fn find() -> Option<Box<dyn Installation>> {
    let hypervisor = Hypervisor::open_read_only().ok()?;
    let machine = hypervisor
        .machines()
        .ok()?
        .find(|m| m.template.as_ref().is_some_and(|t| t.name == TEMPLATE))?;
    Some(Box::new(FlatpakMachine::new(machine.name)))
}

/// What the page asks of the machine, done one after another.
enum Job {
    Scan,
    Overrides { app: String, text: String },
    Portals(String),
    Portal { app: String, args: Vec<String> },
    Start,
}

/// What comes back.
enum Done {
    Scan(Scan),
    Portals(String, Result<BTreeMap<Portal, bool>, String>),
    Failed(String),
}

/// The machine Flatpak runs in, as an [`Installation`].
#[derive(Debug)]
pub struct FlatpakMachine {
    name: String,
    jobs: Sender<Job>,
    done: Receiver<Done>,
    /// The last look, and when it came back.
    scan: Option<(Scan, Instant)>,
    /// Whether a look is on its way.
    scanning: bool,
    portals: HashMap<String, Result<BTreeMap<Portal, bool>, String>>,
    /// Apps whose portal permissions are being read.
    reading: Vec<String>,
    failed: Option<String>,
}

impl FlatpakMachine {
    /// Drives the machine called `name` from a thread of its own, which
    /// ends when this is dropped.
    pub fn new(name: String) -> FlatpakMachine {
        let (jobs, jobs_rx) = bounded::<Job>(16);
        let (done_tx, done) = bounded(16);
        let machine = name.clone();
        let spawned = std::thread::Builder::new()
            .name("derisk-flatpak-machine".into())
            .spawn(move || work(&machine, &jobs_rx, &done_tx));
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "cannot reach the Flatpak machine");
        }
        FlatpakMachine {
            name,
            jobs,
            done,
            scan: None,
            scanning: false,
            portals: HashMap::new(),
            reading: Vec::new(),
            failed: None,
        }
    }

    fn send(&mut self, job: Job) -> bool {
        let sent = self.jobs.try_send(job).is_ok();
        if !sent {
            self.failed = Some(format!("the {} machine is not answering", self.name));
        }
        sent
    }

    /// Takes in what the thread has finished.
    fn collect(&mut self) {
        for done in self.done.try_iter() {
            match done {
                Done::Scan(scan) => {
                    self.scanning = false;
                    self.scan = Some((scan, Instant::now()));
                }
                Done::Portals(app, read) => {
                    self.reading.retain(|a| *a != app);
                    self.portals.insert(app, read);
                }
                Done::Failed(why) => self.failed = Some(why),
            }
        }
    }
}

impl Installation for FlatpakMachine {
    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn scan(&mut self, again: bool) -> Scan {
        self.collect();
        if again {
            self.scan = None;
        }
        // A machine that was off or booting is looked at again while the
        // page waits on it; one whose apps came back, only when asked.
        let stale = match &self.scan {
            None => true,
            Some((Scan::Apps { .. }, _)) => false,
            Some((_, at)) => at.elapsed() >= LOOK_AGAIN,
        };
        if stale && !self.scanning && self.send(Job::Scan) {
            self.scanning = true;
        }
        match &self.scan {
            Some((scan, _)) => scan.clone(),
            None => Scan::Waiting(format!("Reading the apps in the {} machine…", self.name)),
        }
    }

    fn save_overrides(&mut self, app: &str, overrides: &Context) -> Result<(), String> {
        // The guest's file holds only the [Context] group derisk writes
        // (flatpak keeps nothing else in an override), so it is written
        // whole rather than merged into what is there.
        let text = overrides.write_into("");
        self.send(Job::Overrides {
            app: app.to_owned(),
            text,
        });
        // The page's own copy already has the change; the look it shows
        // after a Rescan is the guest's.
        if let Some((Scan::Apps { apps, .. }, _)) = &mut self.scan
            && let Some(found) = apps.iter_mut().find(|a| a.id == app)
        {
            found.overrides = overrides.clone();
        }
        Ok(())
    }

    fn portal_permissions(&mut self, app: &str) -> Option<Result<BTreeMap<Portal, bool>, String>> {
        self.collect();
        if let Some(read) = self.portals.get(app) {
            return Some(read.clone());
        }
        if !self.reading.iter().any(|a| a == app) && self.send(Job::Portals(app.to_owned())) {
            self.reading.push(app.to_owned());
        }
        None
    }

    fn set_portal(&mut self, app: &str, portal: Portal, allow: Option<bool>) -> Result<(), String> {
        self.portals.remove(app);
        if self.send(Job::Portal {
            app: app.to_owned(),
            args: portal_args(app, portal, allow),
        }) {
            Ok(())
        } else {
            Err(self.failed.take().unwrap_or_default())
        }
    }

    fn status(&mut self) -> Option<String> {
        self.collect();
        self.failed.take()
    }

    fn start(&mut self) {
        if self.send(Job::Start) {
            self.scan = None;
        }
    }
}

/// Does each job for the machine `name`, with a read-write connection made
/// at the first and kept; polkit grants it to administrators outright.
fn work(name: &str, jobs: &Receiver<Job>, done: &Sender<Done>) {
    let mut hypervisor = None;
    for job in jobs {
        if hypervisor.is_none() {
            match Hypervisor::open() {
                Ok(opened) => hypervisor = Some(opened),
                Err(e) => {
                    let answer = match job {
                        Job::Scan => Done::Scan(Scan::Unavailable {
                            reason: e.to_string(),
                            startable: false,
                        }),
                        Job::Portals(app) => Done::Portals(app, Err(e.to_string())),
                        _ => Done::Failed(e.to_string()),
                    };
                    if done.send(answer).is_err() {
                        return;
                    }
                    continue;
                }
            }
        }
        let Some(hypervisor) = &hypervisor else {
            continue;
        };
        let answer = match job {
            Job::Scan => Done::Scan(scan(hypervisor, name)),
            Job::Overrides { app, text } => {
                match run(hypervisor, name, &["overrides", &app], text.into_bytes()) {
                    Ok(_) => continue,
                    Err(e) => Done::Failed(format!("Could not update {app}: {e}")),
                }
            }
            Job::Portals(app) => {
                let read = run(
                    hypervisor,
                    name,
                    &["flatpak", "permission-show", &app],
                    Vec::new(),
                )
                .map(|out| parse_permission_show(&out))
                .map_err(|e| e.to_string());
                Done::Portals(app, read)
            }
            Job::Portal { app, args } => {
                let mut all = vec!["flatpak"];
                all.extend(args.iter().map(String::as_str));
                match run(hypervisor, name, &all, Vec::new()) {
                    Ok(_) => continue,
                    Err(e) => Done::Failed(format!("Could not update {app}: {e}")),
                }
            }
            Job::Start => match hypervisor.start(name) {
                Ok(()) => continue,
                Err(e) => Done::Failed(e.to_string()),
            },
        };
        if done.send(answer).is_err() {
            return;
        }
    }
}

/// Looks at the machine: off, booting, or its apps.
fn scan(hypervisor: &Hypervisor, name: &str) -> Scan {
    match hypervisor.machine(name) {
        Ok(machine) if !machine.state.is_active() => {
            return Scan::Unavailable {
                reason: format!("The {name} machine is off; its apps are read while it runs."),
                startable: true,
            };
        }
        Ok(_) => {}
        Err(e) => {
            return Scan::Unavailable {
                reason: e.to_string(),
                startable: false,
            };
        }
    }
    if !hypervisor.agent_ready(name).unwrap_or(false) {
        return Scan::Waiting(format!("Waiting for the {name} machine to start…"));
    }
    match run(hypervisor, name, &["apps"], Vec::new()) {
        Ok(list) => {
            let (apps, global) = parse_apps(&list);
            Scan::Apps { apps, global }
        }
        Err(e) => Scan::Unavailable {
            reason: e.to_string(),
            startable: false,
        },
    }
}

/// Runs `losos-guest-flatpak` with `args` in the machine, giving it
/// `input`, and returns what it printed.
fn run(hypervisor: &Hypervisor, name: &str, args: &[&str], input: Vec<u8>) -> Result<String> {
    let program = Program {
        path: GUEST_FLATPAK.into(),
        args: args.iter().map(|&a| a.to_owned()).collect(),
        env: Vec::new(),
        capture_output: true,
        input,
    };
    let pid = hypervisor.spawn(name, &program)?;
    let what = format!("losos-guest-flatpak {}", args.first().unwrap_or(&""));
    let exit = hypervisor.wait(name, pid, GUEST_WAIT, &what)?;
    if exit.success() {
        Ok(String::from_utf8_lossy(&exit.stdout).into_owned())
    } else {
        Err(Error::Agent {
            name: name.into(),
            reply: format!(
                "{what} failed: {}",
                String::from_utf8_lossy(&exit.stderr).trim()
            ),
        })
    }
}

/// The apps and the global overrides in `losos-guest-flatpak apps`'s
/// output, apps by name. A line it cannot read is left out.
pub fn parse_apps(list: &str) -> (Vec<App>, Context) {
    let decode = |field: &str| {
        STANDARD
            .decode(field.trim())
            .ok()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    };
    let mut global = Context::default();
    let mut apps: Vec<App> = Vec::new();
    for line in list.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        match fields[..] {
            ["global", overrides] => {
                if let Some(text) = decode(overrides) {
                    global = Context::parse(&text);
                }
            }
            ["app", id, kind, name, metadata, overrides] if !id.is_empty() => {
                if apps.iter().any(|a| a.id == id) {
                    continue;
                }
                let Some(metadata) = decode(metadata) else {
                    continue;
                };
                let overrides = decode(overrides).unwrap_or_default();
                let name = if name.is_empty() { id } else { name };
                apps.push(App::from_files(
                    id.to_owned(),
                    name.to_owned(),
                    kind == "user",
                    &metadata,
                    &overrides,
                ));
            }
            _ => {}
        }
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    (apps, global)
}

#[cfg(test)]
mod tests {
    use derisk_settings::flatpak::{Grant, Key};

    use super::*;

    #[test]
    fn reads_the_guests_apps() {
        let metadata = STANDARD.encode(
            "[Application]\nname=org.gnome.Maps\n\n[Context]\nshared=network;\n\n\
             [Session Bus Policy]\norg.freedesktop.Notifications=talk\n",
        );
        let overrides = STANDARD.encode("[Context]\nshared=!network;\n");
        let global = STANDARD.encode("[Context]\nfilesystems=!home;\n");
        let list = format!(
            "global\t{global}\n\
             app\torg.gnome.Maps\tsystem\tMaps\t{metadata}\t{overrides}\n\
             app\tio.github.kolunmi.Bazaar\tuser\t\t{metadata}\t\n\
             app\torg.gnome.Maps\tuser\tMaps again\t{metadata}\t\n\
             app\tbroken\tsystem\tBroken\tnot base64!\t\n\
             something else\n"
        );
        let (apps, global) = parse_apps(&list);
        assert_eq!(global.get(Key::Filesystems, "home"), Some(Grant::No));
        let ids: Vec<_> = apps.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["io.github.kolunmi.Bazaar", "org.gnome.Maps"]);
        let maps = &apps[1];
        assert!(!maps.user);
        assert_eq!(maps.name, "Maps");
        assert_eq!(maps.requested.get(Key::Shared, "network"), Some(Grant::Yes));
        assert_eq!(maps.overrides.get(Key::Shared, "network"), Some(Grant::No));
        assert_eq!(maps.bus_names, ["org.freedesktop.Notifications (talk)"]);
        // A nameless app goes by its ID; one with no overrides has none.
        assert_eq!(apps[0].name, "io.github.kolunmi.Bazaar");
        assert!(apps[0].user && apps[0].overrides.is_empty());
    }

    #[test]
    fn portal_commands_go_to_the_guests_flatpak() {
        assert_eq!(
            portal_args("org.gnome.Maps", Portal::Location, Some(false)),
            [
                "permission-set",
                "location",
                "location",
                "org.gnome.Maps",
                "NONE",
                "0"
            ]
        );
        assert_eq!(
            portal_args("org.gnome.Maps", Portal::Camera, None),
            ["permission-remove", "devices", "camera", "org.gnome.Maps"]
        );
    }
}
