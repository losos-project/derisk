//! Encrypted DNS, on the Network page: whether this computer looks names up
//! over DNS over HTTPS and DNS over TLS, and how strictly.
//!
//! It is LosOS's ad blocker's, not derisk's: `losos-adblock encrypted-dns
//! status` says where it stands, and starting
//! `losos-adblock-encrypted-dns@on.service`, `@strict` or `@off` sets it,
//! after polkit asks an administrator, as for any unit started by hand
//! (LosOS's nixos/modules/adblock.nix, docs/adblock.md). A system without
//! the command has no such setting, and the page shows nothing of it.
//!
//! Both run on a worker thread, so a slow polkit prompt never holds a
//! frame.

use std::{
    process::{Command, Stdio},
    sync::mpsc,
};

use mcsapi_ui::{Theme, egui};

/// The command that says where encrypted DNS stands.
pub const STATUS_ARGV: [&str; 3] = ["losos-adblock", "encrypted-dns", "status"];

/// What encrypted DNS is set to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    /// Names over HTTPS or TLS, except while a captive portal is in the way.
    On,
    /// Names over HTTPS or TLS, or not at all.
    Strict,
    /// Names to the network's own server in plain text.
    Off,
}

impl Mode {
    fn word(self) -> &'static str {
        match self {
            Mode::On => "on",
            Mode::Strict => "strict",
            Mode::Off => "off",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Mode::On => "On",
            Mode::Strict => "Strict",
            Mode::Off => "Off",
        }
    }
}

/// The command that sets it.
pub fn set_argv(mode: Mode) -> [String; 3] {
    [
        "systemctl".into(),
        "start".into(),
        format!("losos-adblock-encrypted-dns@{}.service", mode.word()),
    ]
}

/// Where encrypted DNS stands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Status {
    pub mode: Mode,
    /// Off because schizo mode or an edge's tunnel decides where names go.
    pub forced: bool,
    /// The servers it asks, as their URLs.
    pub servers: Vec<String>,
}

/// Reads `losos-adblock encrypted-dns status`: the mode on the first line,
/// then the servers (`URL at [addresses]`), or why it is off.
pub fn parse_status(out: &str) -> Option<Status> {
    let mut lines = out.lines();
    let mode = match lines.next()?.trim() {
        "on" => Mode::On,
        "strict" => Mode::Strict,
        "off" => Mode::Off,
        _ => return None,
    };
    let mut forced = false;
    let mut servers = Vec::new();
    for line in lines {
        if line.starts_with("forced") {
            forced = true;
        } else if let Some(url) = line.split_whitespace().next() {
            servers.push(url.to_owned());
        }
    }
    Some(Status {
        mode,
        forced,
        servers,
    })
}

/// The line under the choice.
pub fn describe(status: &Status) -> String {
    let servers = if status.servers.is_empty() {
        "no server".to_owned()
    } else {
        status.servers.join(", ")
    };
    match (status.mode, status.forced) {
        (_, true) => "Set aside while schizo mode or an edge decides where names go.".into(),
        (Mode::On, _) => format!(
            "Names go to {servers}, encrypted. Behind a hotel's or a train's sign-in page they go to the network's server until you are signed in."
        ),
        (Mode::Strict, _) => format!(
            "Names go to {servers}, encrypted, or not at all. Wi-Fi with a sign-in page may not open its page."
        ),
        (Mode::Off, _) => "Names go to the network's own server, which sees every one.".into(),
    }
}

/// The section's state.
#[derive(Debug, Default)]
pub(crate) struct EncryptedDnsUi {
    /// `Some(None)` once it is known this system has no such setting.
    status: Option<Option<Status>>,
    reading: Option<mpsc::Receiver<Option<Status>>>,
    /// What it is being set to, while polkit asks and the unit runs.
    switching: Option<(Mode, mpsc::Receiver<Result<String, String>>)>,
    error: Option<String>,
}

impl EncryptedDnsUi {
    fn refresh(&mut self) {
        let (tx, rx) = mpsc::channel();
        derisk_sys::thread::detach(move || {
            let _ = tx.send(stdout(&STATUS_ARGV).ok().and_then(|o| parse_status(&o)));
        });
        self.reading = Some(rx);
    }

    fn switch(&mut self, mode: Mode) {
        let (tx, rx) = mpsc::channel();
        derisk_sys::thread::detach(move || {
            let argv = set_argv(mode);
            let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
            let _ = tx.send(stdout(&argv));
        });
        self.error = None;
        self.switching = Some((mode, rx));
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
        if self.status.is_none() && self.reading.is_none() {
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

/// The section; nothing on a system without encrypted DNS.
pub(crate) fn section(ui: &mut egui::Ui, state: &mut EncryptedDnsUi, theme: &Theme) {
    // The Network page repaints twice a second while shown, which is what
    // notices the worker's answers.
    state.poll();
    let status = match &state.status {
        None | Some(None) => return,
        Some(Some(status)) => status.clone(),
    };
    let tokens = mcsapi_components::Tokens::current(ui.ctx());

    ui.label(
        egui::RichText::new("Encrypted DNS")
            .strong()
            .color(theme.foreground),
    );
    ui.label(crate::layout::note(
        &tokens,
        "Looks names up over DNS over HTTPS or TLS, so the network and its provider see neither the names nor a chance to answer for the server. Changing it asks for an administrator's password.",
    ));
    let pending = state.switching.as_ref().map(|(mode, _)| *mode);
    let mut mode = pending.unwrap_or(status.mode);
    ui.add_enabled_ui(pending.is_none() && !status.forced, |ui| {
        ui.horizontal_wrapped(|ui| {
            for choice in [Mode::On, Mode::Strict, Mode::Off] {
                ui.selectable_value(&mut mode, choice, choice.label());
            }
        });
    });
    if pending.is_none() && mode != status.mode {
        state.switch(mode);
    }
    if pending.is_some() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Changing encrypted DNS…");
        });
    } else {
        ui.label(crate::layout::note(&tokens, &describe(&status)));
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
        let status = parse_status(
            "on\nhttps://dns.quad9.net/dns-query at [9.9.9.9:443]\ntls://dns.mullvad.net at [194.242.2.2:853]\n",
        )
        .unwrap();
        assert_eq!(
            status,
            Status {
                mode: Mode::On,
                forced: false,
                servers: vec![
                    "https://dns.quad9.net/dns-query".into(),
                    "tls://dns.mullvad.net".into()
                ],
            }
        );
        let forced = parse_status("off\nforced to /run/losos-adblock/upstreams\n").unwrap();
        assert!(forced.forced);
        assert!(forced.servers.is_empty());
        assert_eq!(parse_status("strict\n").unwrap().mode, Mode::Strict);
        assert_eq!(parse_status("usage: …"), None);
    }

    #[test]
    fn says_where_names_go() {
        let mut status = parse_status("strict\ntls://dns.example at [192.0.2.1:853]").unwrap();
        assert!(describe(&status).contains("tls://dns.example"));
        assert!(describe(&status).contains("or not at all"));
        status.forced = true;
        assert!(describe(&status).contains("schizo"));
        status = parse_status("off").unwrap();
        assert!(describe(&status).contains("sees every one"));
    }

    #[test]
    fn is_set_through_its_unit() {
        assert_eq!(
            set_argv(Mode::Strict),
            [
                "systemctl",
                "start",
                "losos-adblock-encrypted-dns@strict.service"
            ]
        );
        assert_eq!(
            set_argv(Mode::Off),
            [
                "systemctl",
                "start",
                "losos-adblock-encrypted-dns@off.service"
            ]
        );
    }
}
