//! Schizo mode, at the top of the Network page: every connection this
//! machine and its machines make goes through Tor, by way of a gateway
//! machine of the OS's own, or does not go at all.
//!
//! It is LosOS's, not derisk's: `losos-schizo status` says where it
//! stands, and starting `losos-schizo@on.service` or `@off.service` turns
//! it on or off, after polkit asks an administrator, as for any unit
//! started by hand (LosOS's nixos/modules/schizo.nix). A system without
//! the command has no such mode, and the page shows nothing of it.
//!
//! Both run on a worker thread: the status asks Tor for a name to see
//! whether it is through, which takes a moment.

use std::{
    process::{Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

use mcsapi_ui::{Theme, egui};

/// The command that says where schizo mode stands.
pub const STATUS_ARGV: [&str; 2] = ["losos-schizo", "status"];

/// The command that turns schizo mode on or off.
pub fn set_argv(on: bool) -> [String; 3] {
    [
        "systemctl".into(),
        "start".into(),
        format!("losos-schizo@{}.service", if on { "on" } else { "off" }),
    ]
}

/// Whether Tor is through, as `losos-schizo status` tells it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tor {
    /// Schizo mode is off.
    Off,
    /// On, and Tor has not answered yet.
    Connecting,
    /// On, and Tor answers.
    Connected,
}

/// Where schizo mode stands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Status {
    /// Whether it is on.
    pub on: bool,
    /// The gateway machine's state as libvirt words it (`running`, `shut
    /// off`), or `none` before it was first started.
    pub gateway: String,
    /// Whether Tor is through.
    pub tor: Tor,
}

/// Reads `losos-schizo status`: `mode=`, `gateway=` and `tor=` lines.
pub fn parse_status(out: &str) -> Option<Status> {
    let field = |key: &str| {
        out.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .map(str::trim)
    };
    let on = match field("mode")? {
        "on" => true,
        "off" => false,
        _ => return None,
    };
    let tor = match field("tor").unwrap_or("off") {
        "connected" => Tor::Connected,
        "connecting" => Tor::Connecting,
        _ => Tor::Off,
    };
    Some(Status {
        on,
        gateway: field("gateway").unwrap_or("none").to_owned(),
        tor,
    })
}

/// The line under the switch.
pub fn describe(status: &Status) -> &'static str {
    if !status.on {
        "Off. Connections go straight to the network."
    } else if status.gateway != "running" {
        "On, but the Tor gateway is not running, so nothing can go out."
    } else {
        match status.tor {
            Tor::Connected => "On. Every connection goes through Tor.",
            _ => "On. Connecting to Tor; nothing goes out until it is through.",
        }
    }
}

/// How often the status is read again while the page is shown: often
/// enough to see Tor come through, rarely enough not to ask Tor for a
/// name every frame.
const REFRESH: Duration = Duration::from_secs(10);

/// The section's state.
#[derive(Debug, Default)]
pub(crate) struct SchizoUi {
    /// `Some(None)` once it is known this system has no schizo mode.
    status: Option<Option<Status>>,
    read_at: Option<Instant>,
    reading: Option<mpsc::Receiver<Option<Status>>>,
    /// What it is being turned to, while polkit asks and the unit runs.
    switching: Option<(bool, mpsc::Receiver<Result<String, String>>)>,
    error: Option<String>,
}

impl SchizoUi {
    fn refresh(&mut self) {
        let (tx, rx) = mpsc::channel();
        derisk_sys::thread::detach(move || {
            let _ = tx.send(stdout(&STATUS_ARGV).ok().and_then(|o| parse_status(&o)));
        });
        self.reading = Some(rx);
        self.read_at = Some(Instant::now());
    }

    fn switch(&mut self, on: bool) {
        let (tx, rx) = mpsc::channel();
        derisk_sys::thread::detach(move || {
            let argv = set_argv(on);
            let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
            let _ = tx.send(stdout(&argv));
        });
        self.error = None;
        self.switching = Some((on, rx));
    }

    fn poll(&mut self) {
        if let Some((_, rx)) = &self.switching
            && let Ok(done) = rx.try_recv()
        {
            // Read back rather than assumed.
            self.error = done.err();
            self.switching = None;
            self.refresh();
        }
        if let Some(rx) = &self.reading
            && let Ok(status) = rx.try_recv()
        {
            self.status = Some(status);
            self.reading = None;
        }
        let stale = self.read_at.is_none_or(|at| at.elapsed() >= REFRESH);
        let known = !matches!(self.status, Some(None));
        if self.reading.is_none() && self.switching.is_none() && stale && known {
            self.refresh();
        }
    }
}

fn stdout(argv: &[&str]) -> Result<String, String> {
    let out = Command::new(argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {}: {e}", argv[0]))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .map_or_else(|| format!("{} failed", argv[0]), |l| l.trim().to_owned()))
    }
}

/// The section, above the network's own; nothing on a system without
/// schizo mode.
pub(crate) fn section(ui: &mut egui::Ui, state: &mut SchizoUi, theme: &Theme) {
    // The Network page repaints twice a second while shown, which is
    // what notices the worker's answers.
    state.poll();
    let status = match &state.status {
        None => return,
        Some(None) => return,
        Some(Some(status)) => status.clone(),
    };
    let tokens = mcsapi_components::Tokens::current(ui.ctx());

    ui.label(
        egui::RichText::new("Schizo mode")
            .strong()
            .color(theme.foreground),
    );
    ui.label(crate::layout::note(
        &tokens,
        "Sends every connection this computer and its machines make through Tor, by way of a gateway machine like Whonix's. If the gateway is down, nothing goes out at all. Local networks, printers and anything that is not TCP or DNS stop working while it is on. Changing it asks for an administrator's password.",
    ));
    let pending = state.switching.as_ref().map(|(on, _)| *on);
    let mut on = pending.unwrap_or(status.on);
    let changed = ui
        .add_enabled_ui(pending.is_none(), |ui| {
            ui.checkbox(&mut on, "Send everything through Tor")
        })
        .inner
        .changed();
    if changed {
        state.switch(on);
    }
    if let Some(turning) = pending {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(if turning {
                "Turning schizo mode on…"
            } else {
                "Turning schizo mode off…"
            });
        });
    } else {
        let line = describe(&status);
        if status.on && status.tor != Tor::Connected {
            ui.horizontal(|ui| {
                if status.gateway == "running" {
                    ui.spinner();
                }
                ui.label(line);
            });
        } else {
            ui.label(crate::layout::note(&tokens, line));
        }
    }
    if let Some(error) = &state.error {
        ui.label(egui::RichText::new(error).color(egui::Color32::from_rgb(248, 113, 113)));
    }
    ui.add_space(16.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_status() {
        let status = parse_status("mode=on\ngateway=running\ntor=connected\n").unwrap();
        assert_eq!(
            status,
            Status {
                on: true,
                gateway: "running".into(),
                tor: Tor::Connected
            }
        );
        let off = parse_status("mode=off\ngateway=shut off\ntor=off\n").unwrap();
        assert!(!off.on);
        assert_eq!(off.gateway, "shut off");
        assert_eq!(parse_status("usage: …"), None);
    }

    #[test]
    fn says_why_nothing_goes_out() {
        let mut status = parse_status("mode=on\ngateway=shut off\ntor=connecting").unwrap();
        assert!(describe(&status).contains("nothing can go out"));
        status.gateway = "running".into();
        assert!(describe(&status).contains("Connecting"));
        status.tor = Tor::Connected;
        assert!(describe(&status).contains("through Tor"));
    }

    #[test]
    fn is_set_through_its_unit() {
        assert_eq!(
            set_argv(true),
            ["systemctl", "start", "losos-schizo@on.service"]
        );
        assert_eq!(
            set_argv(false),
            ["systemctl", "start", "losos-schizo@off.service"]
        );
    }
}
