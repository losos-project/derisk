//! The System page: the security report and the factory reset.
//!
//! Neither is derisk's own setting. The report is `io.losos.Security1` on
//! the system bus, LosOS's OS-level checks (disk encryption, kernel
//! lockdown, the usual hardening sysctls), each with where it looked; it is
//! asked through `busctl --json`. The factory reset is systemd's: starting
//! `factory-reset.target` asks the firmware to reset on the next boot and
//! restarts, and that boot's systemd-repart empties every partition marked
//! for it, every account and file included. polkit asks an administrator to
//! authenticate first. Whether this machine can be reset at all, and
//! whether a reset is already waiting, come from systemd's Varlink API.
//!
//! Everything runs on a worker thread: the security service is started on
//! demand by the bus, and that takes a moment.

use std::{
    process::{Command, Stdio},
    sync::mpsc,
};

use mcsapi_ui::{Theme, egui};
use serde_json::Value;

/// One check from the security report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Check {
    /// One line: what was checked.
    pub summary: String,
    /// What it means, and where it looked.
    pub description: String,
    /// Whether the machine passed.
    pub passed: bool,
}

/// fwupd's `FWUPD_SECURITY_ATTR_FLAG_SUCCESS`, which io.losos.Security1 sets
/// on a check that passed.
const FLAG_SUCCESS: u64 = 1;

/// Reads `busctl --json=short call … GetHostSecurityAttrs`: one `aa{sv}`,
/// each variant as `{"type": …, "data": …}`.
pub fn parse_report(json: &str) -> Option<Vec<Check>> {
    let reply: Value = serde_json::from_str(json).ok()?;
    let attrs = reply.get("data")?.get(0)?.as_array()?;
    Some(
        attrs
            .iter()
            .map(|attr| {
                let text = |key: &str| {
                    attr.pointer(&format!("/{key}/data"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                };
                let flags = attr
                    .pointer("/Flags/data")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                Check {
                    summary: text("Summary"),
                    description: text("Description"),
                    passed: flags & FLAG_SUCCESS != 0,
                }
            })
            .collect(),
    )
}

/// The command that asks for the report.
pub const REPORT_ARGV: [&str; 8] = [
    "busctl",
    "--system",
    "--json=short",
    "call",
    "io.losos.Security1",
    "/io/losos/Security1",
    "io.losos.Security1",
    "GetHostSecurityAttrs",
];

/// systemd's factory reset Varlink socket.
const FACTORY_RESET: &str = "/run/systemd/io.systemd.FactoryReset";

/// The command that asks for a factory reset, and restarts.
pub const RESET_ARGV: [&str; 3] = ["systemctl", "start", "factory-reset.target"];

/// Where the factory reset stands.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Reset {
    /// Whether this machine can ask for one: the request is an EFI
    /// variable, so a machine started by a BIOS cannot.
    pub supported: bool,
    /// systemd's `FactoryResetMode`, such as `unspecified` or `pending`.
    pub mode: String,
}

/// Reads the two Varlink replies, `{"supported": …}` and `{"mode": …}`.
pub fn parse_reset(can: &str, mode: &str) -> Reset {
    let field = |json: &str, key: &str| {
        serde_json::from_str::<Value>(json)
            .ok()
            .and_then(|v| v.get(key).cloned())
    };
    Reset {
        supported: field(can, "supported").and_then(|v| v.as_bool()) == Some(true),
        mode: field(mode, "mode")
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default(),
    }
}

fn varlink(method: &str) -> String {
    stdout(&[
        "varlinkctl",
        "call",
        "--json=short",
        FACTORY_RESET,
        &format!("io.systemd.FactoryReset.{method}"),
        "{}",
    ])
    .unwrap_or_default()
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

#[derive(Clone, Debug)]
struct Snapshot {
    /// `None` when this system has no security service.
    report: Option<Vec<Check>>,
    reset: Reset,
}

fn read() -> Snapshot {
    Snapshot {
        report: stdout(&REPORT_ARGV).ok().and_then(|j| parse_report(&j)),
        reset: parse_reset(
            &varlink("CanRequestFactoryReset"),
            &varlink("GetFactoryResetMode"),
        ),
    }
}

/// The page's state.
#[derive(Debug, Default)]
pub(crate) struct SystemUi {
    snapshot: Option<Snapshot>,
    reading: Option<mpsc::Receiver<Snapshot>>,
    /// The reset's confirmation is showing.
    confirm: bool,
    resetting: Option<mpsc::Receiver<Result<String, String>>>,
    error: Option<String>,
}

impl SystemUi {
    fn refresh(&mut self) {
        let (tx, rx) = mpsc::channel();
        derisk_sys::thread::detach(move || {
            let _ = tx.send(read());
        });
        self.reading = Some(rx);
    }

    fn reset(&mut self) {
        let (tx, rx) = mpsc::channel();
        derisk_sys::thread::detach(move || {
            let _ = tx.send(stdout(&RESET_ARGV));
        });
        self.error = None;
        self.resetting = Some(rx);
    }

    fn poll(&mut self) {
        if let Some(rx) = &self.reading
            && let Ok(snapshot) = rx.try_recv()
        {
            self.snapshot = Some(snapshot);
            self.reading = None;
        }
        if let Some(rx) = &self.resetting
            && let Ok(done) = rx.try_recv()
        {
            // On success the machine is restarting; refreshing shows the
            // reset as pending until it does.
            self.error = done.err();
            self.resetting = None;
            self.confirm = false;
            self.refresh();
        }
    }
}

fn hint(ui: &mut egui::Ui, theme: &Theme, text: &str) {
    ui.label(egui::RichText::new(text).small().color(theme.border));
}

const RED: egui::Color32 = egui::Color32::from_rgb(248, 113, 113);

pub(crate) fn page(ui: &mut egui::Ui, state: &mut SystemUi, theme: &Theme) {
    if state.snapshot.is_none() && state.reading.is_none() {
        state.refresh();
    }
    state.poll();
    if state.reading.is_some() || state.resetting.is_some() {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(200));
    }
    let Some(snapshot) = state.snapshot.clone() else {
        ui.spinner();
        return;
    };

    ui.heading(egui::RichText::new("Security report").color(theme.foreground));
    match &snapshot.report {
        None => hint(ui, theme, "This system has no security report."),
        Some(checks) => {
            let failed = checks.iter().filter(|c| !c.passed).count();
            ui.label(if failed == 0 {
                format!("All {} checks passed.", checks.len())
            } else {
                format!("{failed} of {} checks did not pass.", checks.len())
            });
            // Failures first: they are what someone can act on.
            let mut sorted: Vec<&Check> = checks.iter().collect();
            sorted.sort_by_key(|c| c.passed);
            for (i, check) in sorted.into_iter().enumerate() {
                let (mark, color) = if check.passed {
                    ("✓", theme.accent)
                } else {
                    ("✗", RED)
                };
                let title = egui::RichText::new(format!("{mark} {}", check.summary)).color(color);
                egui::CollapsingHeader::new(title)
                    .id_salt(("security-check", i))
                    .show(ui, |ui| ui.label(&check.description));
            }
        }
    }
    if ui
        .add_enabled(state.reading.is_none(), egui::Button::new("Check again"))
        .clicked()
    {
        state.refresh();
    }
    ui.add_space(16.0);

    ui.heading(egui::RichText::new("Factory reset").color(theme.foreground));
    let reset = &snapshot.reset;
    if reset.mode == "pending" {
        ui.label("A factory reset happens the next time this machine starts.");
        return;
    }
    if !reset.supported {
        hint(
            ui,
            theme,
            "This machine cannot be reset from here: the request is kept in UEFI firmware, and this machine did not start with UEFI.",
        );
        return;
    }
    hint(
        ui,
        theme,
        "Erases every account, every file and every setting, then restarts. The system itself stays as it is now, with its updates.",
    );
    if let Some(error) = &state.error {
        ui.label(egui::RichText::new(error).color(RED));
    }
    if state.resetting.is_some() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Asking for a factory reset…");
        });
    } else if state.confirm {
        ui.label(
            egui::RichText::new(
                "Everything on this machine will be erased. This cannot be undone.",
            )
            .strong()
            .color(RED),
        );
        ui.horizontal(|ui| {
            if ui
                .button(egui::RichText::new("Erase everything and restart").color(RED))
                .clicked()
            {
                state.reset();
            }
            if ui.button("Cancel").clicked() {
                state.confirm = false;
            }
        });
    } else if ui.button("Reset this machine…").clicked() {
        state.confirm = true;
    }
}
