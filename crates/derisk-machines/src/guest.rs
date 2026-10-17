//! What the desktop asks of a guest: which apps it has, and to open one.
//!
//! Both go through the QEMU guest agent to `losos-guest-run`, a program
//! LosOS's guest template has (losos-desktop, `nixos/guest/`), and which the
//! Linux tools CD installs in any other distribution (its `setup.sh`), at
//! one of [`GUEST_RUN`]. Its contract:
//!
//! - `losos-guest-run --list` prints one line per app the guest can show on
//!   the desktop: its desktop file ID and its name, separated by a tab.
//! - `losos-guest-run <desktop file ID>` starts that app as the guest's
//!   user, whose waypipe server (`waypipe --vsock -s 2:7350 server`) is the
//!   app's display, so its windows come to the desktop through
//!   [`crate::display`], and returns once it has started.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use crate::{Error, Exit, Hypervisor, Program, Result, setup::TEMPLATE_DIR};

/// Where `losos-guest-run` is: the guest template's system profile, then
/// where the Linux tools CD puts it in another distribution. The agent's own
/// `PATH` is not a login's, so the paths are spelled out.
pub const GUEST_RUN: [&str; 2] = [
    "/run/current-system/sw/bin/losos-guest-run",
    "/usr/local/bin/losos-guest-run",
];

/// The guest program that runs the first of [`GUEST_RUN`] there is with
/// `args`. The agent runs one path and cannot look for it, so `/bin/sh`,
/// which every Linux has there, does the looking; exit status 127 and a
/// line on stderr say that neither is installed.
fn guest_run(args: &[&str]) -> Program {
    let mut script = String::from("for run in");
    for path in GUEST_RUN {
        script.push(' ');
        script.push_str(path);
    }
    script.push_str(
        "; do [ -x \"$run\" ] && exec \"$run\" \"$@\"; done; \
         echo 'losos-guest-run is not installed: run setup.sh from the LosOS tools CD' >&2; exit 127",
    );
    let mut all = vec!["-c".to_owned(), script, "losos-guest-run".to_owned()];
    all.extend(args.iter().map(|&arg| arg.to_owned()));
    Program {
        path: "/bin/sh".into(),
        args: all,
        env: Vec::new(),
        capture_output: true,
        input: Vec::new(),
    }
}

/// How long a machine that was off may take to boot far enough to answer.
const BOOT_WAIT: Duration = Duration::from_secs(120);

/// How long `losos-guest-run` may take to list the apps or start one.
const LIST_WAIT: Duration = Duration::from_secs(10);

/// An app a guest can open.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuestApp {
    /// Its desktop file ID, without `.desktop`.
    pub id: String,
    /// Its name.
    pub name: String,
}

impl Hypervisor {
    /// The apps `machine` can open, by name. It must be running with its
    /// guest agent up.
    pub fn guest_apps(&self, machine: &str) -> Result<Vec<GuestApp>> {
        let pid = self.spawn(machine, &guest_run(&["--list"]))?;
        let exit = self.wait(machine, pid, LIST_WAIT, "losos-guest-run --list")?;
        if !exit.success() {
            return Err(Error::Agent {
                name: machine.into(),
                reply: format!(
                    "losos-guest-run --list failed: {}",
                    String::from_utf8_lossy(&exit.stderr).trim()
                ),
            });
        }
        let mut apps: Vec<GuestApp> = parse_apps(&String::from_utf8_lossy(&exit.stdout)).collect();
        apps.sort_by_key(|app| app.name.to_lowercase());
        Ok(apps)
    }

    /// Opens the app `app` (a desktop file ID) in `machine`, starting the
    /// machine and waiting for it to boot first if it is off. Fails with
    /// what the guest said when it has no such app.
    pub fn open_app(&self, machine: &str, app: &str) -> Result<()> {
        self.start(machine)?;
        let deadline = Instant::now() + BOOT_WAIT;
        while !self.agent_ready(machine)? {
            if Instant::now() >= deadline {
                return Err(Error::Agent {
                    name: machine.into(),
                    reply: "the guest agent did not answer after the machine started".into(),
                });
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        let pid = self.spawn(machine, &guest_run(&[app]))?;
        // It returns as soon as the app's unit has started, so this is a
        // moment's wait, and it is how an app the guest lacks is told apart
        // from one that opened.
        let exit = self.wait(machine, pid, LIST_WAIT, &format!("losos-guest-run {app}"))?;
        if exit.success() {
            Ok(())
        } else {
            Err(Error::Agent {
                name: machine.into(),
                reply: String::from_utf8_lossy(&exit.stderr).trim().to_owned(),
            })
        }
    }

    /// Waits up to `wait` for the guest program `pid` to exit.
    pub(crate) fn wait(&self, machine: &str, pid: u32, wait: Duration, what: &str) -> Result<Exit> {
        let deadline = Instant::now() + wait;
        loop {
            if let Some(exit) = self.exit(machine, pid)? {
                return Ok(exit);
            }
            if Instant::now() >= deadline {
                return Err(Error::Agent {
                    name: machine.into(),
                    reply: format!("{what} did not finish"),
                });
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// The apps in `losos-guest-run --list`'s output. A line without a tab, or
/// with an empty ID, is not an app.
pub fn parse_apps(list: &str) -> impl Iterator<Item = GuestApp> + '_ {
    list.lines().filter_map(|line| {
        let (id, name) = line.split_once('\t')?;
        let id = id.trim().trim_end_matches(".desktop");
        let name = name.trim();
        (!id.is_empty()).then(|| GuestApp {
            id: id.to_owned(),
            name: if name.is_empty() { id } else { name }.to_owned(),
        })
    })
}

/// The apps a template's guest has when it is new, in the file the OS ships
/// beside the template's image: `<template>.apps` under
/// [`TEMPLATE_DIR`] in the first of `data_dirs` that has one, in
/// `losos-guest-run --list`'s format. It is how a machine that has never
/// run offers its apps before its guest could be asked, and how a machine
/// made to hold one kind of app (Flatpak's, say) is found by what it holds.
/// `None` when no template of that name ships a list.
pub fn template_apps(data_dirs: &[PathBuf], template: &str) -> Option<Vec<GuestApp>> {
    // A name pm accepts cannot climb out of the directory, but this one
    // came from a domain definition anyone with libvirt's ear could write.
    if template.is_empty() || template.contains('/') || template.starts_with('.') {
        return None;
    }
    data_dirs.iter().find_map(|dir| {
        let list =
            derisk_sys::fs::read_to_string(dir.join(TEMPLATE_DIR).join(format!("{template}.apps")))
                .ok()?;
        Some(parse_apps(&list).collect())
    })
}

/// `apps` in `losos-guest-run --list`'s format, which [`parse_apps`] reads
/// back: how the desktop keeps a machine's apps while it is off.
pub fn list_apps(apps: &[GuestApp]) -> String {
    apps.iter()
        .map(|app| format!("{}\t{}\n", app.id, app.name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_for_guest_run_in_the_template_then_the_tools_cds_place() {
        let program = guest_run(&["org.derisk.files"]);
        assert_eq!(program.path, "/bin/sh");
        assert_eq!(program.args[0], "-c");
        let script = &program.args[1];
        let template = script.find(GUEST_RUN[0]).unwrap();
        let tools = script.find(GUEST_RUN[1]).unwrap();
        assert!(template < tools, "{script}");
        assert!(script.contains("exit 127"), "{script}");
        // $0, then the app, as "$@".
        assert_eq!(program.args[2..], ["losos-guest-run", "org.derisk.files"]);
    }

    #[test]
    fn a_guest_without_guest_run_is_told_how_to_get_it() {
        let program = guest_run(&["--list"]);
        let out = std::process::Command::new(&program.path)
            .args(&program.args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(127));
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("LosOS tools CD"), "{err}");
    }

    #[test]
    fn a_template_ships_its_apps_beside_its_image() {
        let root = std::env::temp_dir().join(format!(
            "derisk-machines-template-apps-{}",
            derisk_sys::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let (a, b) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(b.join(TEMPLATE_DIR)).unwrap();
        std::fs::write(
            b.join(TEMPLATE_DIR).join("losos-flatpak.apps"),
            "io.github.kolunmi.Bazaar\tBazaar\n",
        )
        .unwrap();
        let dirs = [a, b];
        assert_eq!(
            template_apps(&dirs, "losos-flatpak").unwrap(),
            [GuestApp {
                id: "io.github.kolunmi.Bazaar".into(),
                name: "Bazaar".into()
            }]
        );
        assert_eq!(template_apps(&dirs, "losos-guest"), None);
        assert_eq!(
            template_apps(&dirs, "../b/losos/machines/losos-flatpak"),
            None
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_list_reads_back_as_it_was() {
        let apps = vec![
            GuestApp {
                id: "org.gnome.Maps".into(),
                name: "Maps".into(),
            },
            GuestApp {
                id: "foot".into(),
                name: "Foot".into(),
            },
        ];
        assert_eq!(parse_apps(&list_apps(&apps)).collect::<Vec<_>>(), apps);
    }

    #[test]
    fn lists_apps() {
        let list = "org.derisk.files\tFiles\nfoot.desktop\tTerminal\n\
                    \nnot an app\nbare\t\n\tNameless\n";
        let apps: Vec<_> = parse_apps(list).collect();
        assert_eq!(
            apps,
            [
                GuestApp {
                    id: "org.derisk.files".into(),
                    name: "Files".into()
                },
                GuestApp {
                    id: "foot".into(),
                    name: "Terminal".into()
                },
                GuestApp {
                    id: "bare".into(),
                    name: "bare".into()
                },
            ]
        );
    }
}
