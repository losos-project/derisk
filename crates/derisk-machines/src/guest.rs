//! What the desktop asks of a guest: which apps it has, and to open one.
//!
//! Both go through the QEMU guest agent to [`GUEST_RUN`], a program LosOS's
//! guest template has (losos-desktop, `nixos/guest/`). Its contract:
//!
//! - `losos-guest-run --list` prints one line per app the guest can show on
//!   the desktop: its desktop file ID and its name, separated by a tab.
//! - `losos-guest-run <desktop file ID>` starts that app as the guest's
//!   user, whose waypipe server (`waypipe --vsock -s 2:7350 server`) is the
//!   app's display, so its windows come to the desktop through
//!   [`crate::display`], and returns once it has started.

use std::time::{Duration, Instant};

use crate::{Error, Hypervisor, Program, Result};

/// Where the guest template has `losos-guest-run`. The agent's own `PATH`
/// is not a login's, so the path is spelled out.
pub const GUEST_RUN: &str = "/run/current-system/sw/bin/losos-guest-run";

/// How long a machine that was off may take to boot far enough to answer.
const BOOT_WAIT: Duration = Duration::from_secs(120);

/// How long `losos-guest-run --list` may take.
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
        let pid = self.spawn(
            machine,
            &Program {
                path: GUEST_RUN.into(),
                args: vec!["--list".into()],
                env: Vec::new(),
                capture_output: true,
            },
        )?;
        let deadline = Instant::now() + LIST_WAIT;
        loop {
            if let Some(exit) = self.exit(machine, pid)? {
                if !exit.success() {
                    return Err(Error::Agent {
                        name: machine.into(),
                        reply: format!(
                            "{GUEST_RUN} --list failed: {}",
                            String::from_utf8_lossy(&exit.stderr).trim()
                        ),
                    });
                }
                let mut apps: Vec<GuestApp> =
                    parse_apps(&String::from_utf8_lossy(&exit.stdout)).collect();
                apps.sort_by_key(|app| app.name.to_lowercase());
                return Ok(apps);
            }
            if Instant::now() >= deadline {
                return Err(Error::Agent {
                    name: machine.into(),
                    reply: format!("{GUEST_RUN} --list did not finish"),
                });
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Opens the app `app` (a desktop file ID) in `machine`, starting the
    /// machine and waiting for it to boot first if it is off.
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
        self.spawn(
            machine,
            &Program {
                path: GUEST_RUN.into(),
                args: vec![app.into()],
                env: Vec::new(),
                capture_output: false,
            },
        )
        .map(drop)
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

#[cfg(test)]
mod tests {
    use super::*;

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
