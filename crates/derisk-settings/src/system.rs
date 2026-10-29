//! The System page: the release cadence updates follow, the security report,
//! Secure Boot's setup and the factory reset.
//!
//! None is derisk's own setting. The cadence is LosOS's: which of its
//! releases, nightly, weekly, monthly or yearly, the machine updates to.
//! `losos-update-channel` prints it, and starting
//! `losos-update-channel@<cadence>.service` changes it, after polkit asks
//! an administrator, as for any unit started by hand. A system without that
//! command (another OS, or LosOS updating from a proxy) has no choice to
//! offer, and the page says so. The report is `io.losos.Security1` on
//! the system bus, LosOS's OS-level checks (disk encryption, kernel
//! lockdown, the usual hardening sysctls), each with where it looked; it is
//! asked through `busctl --json`. The factory reset is systemd's: starting
//! `factory-reset.target` asks the firmware to reset on the next boot and
//! restarts, and that boot's systemd-repart empties every partition marked
//! for it, every account and file included. polkit asks an administrator to
//! authenticate first. Whether this machine can be reset at all, and
//! whether a reset is already waiting, come from systemd's Varlink API.
//!
//! Secure Boot is LosOS's too (its docs/secure-boot.md): `losos-secure-boot`
//! says whether the firmware has it on and whether LosOS's key is in shim's
//! MOK list, and `losos-secure-boot-enroll.service` asks shim to enroll it
//! at the next start, behind a code MokManager asks for. Enrolled before
//! Secure Boot is on, the key never makes shim stop with a "Security
//! Violation"; the page walks a person through MokManager and then into the
//! firmware's settings. A release built without the key has no
//! `losos-secure-boot`, and the page says so.
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

/// One release cadence LosOS publishes, as `losos-update-channel` names it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cadence {
    /// The name `losos-update-channel` and its unit take.
    pub id: &'static str,
    /// What the page calls it.
    pub name: &'static str,
    /// One line on how often it brings an update.
    pub detail: &'static str,
}

/// Every cadence, fastest first.
pub const CADENCES: [Cadence; 4] = [
    Cadence {
        id: "nightly",
        name: "Nightly",
        detail: "Every build that passes its checks, often more than once a day.",
    },
    Cadence {
        id: "weekly",
        name: "Weekly",
        detail: "One release each week, out on Monday.",
    },
    Cadence {
        id: "monthly",
        name: "Monthly",
        detail: "One release each month, out on the 1st.",
    },
    Cadence {
        id: "yearly",
        name: "Yearly",
        detail: "One release each year, out on 1 January. The release GitHub shows as latest.",
    },
];

/// The command that prints the cadence this machine follows.
pub const CADENCE_ARGV: [&str; 1] = ["losos-update-channel"];

/// The command that makes the machine follow `cadence`.
pub fn set_cadence_argv(cadence: &str) -> [String; 3] {
    [
        "systemctl".into(),
        "start".into(),
        format!("losos-update-channel@{cadence}.service"),
    ]
}

/// Reads `losos-update-channel`'s answer: the cadence's name on one line.
pub fn parse_cadence(out: &str) -> Option<&'static str> {
    let name = out.lines().next()?.trim();
    CADENCES.iter().find(|c| c.id == name).map(|c| c.id)
}

/// Where LosOS's key stands in shim's MOK list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Key {
    /// Not there, and not asked for.
    Missing,
    /// Asked for: MokManager enrolls it at the next start.
    Requested,
    /// There: shim starts what the key signed.
    Enrolled,
}

/// Where Secure Boot stands, as `losos-secure-boot` prints it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecureBoot {
    /// Whether the firmware enforces it.
    pub on: bool,
    /// Where LosOS's key stands.
    pub key: Key,
    /// The code MokManager asks for, while a request is waiting and this
    /// start made it.
    pub code: Option<String>,
}

/// The command that says where Secure Boot stands.
pub const SECURE_BOOT_ARGV: [&str; 1] = ["losos-secure-boot"];

/// The command that asks shim to enroll LosOS's key at the next start.
pub const ENROLL_ARGV: [&str; 3] = ["systemctl", "start", "losos-secure-boot-enroll.service"];

/// The command that takes that request back.
pub const CANCEL_ARGV: [&str; 3] = ["systemctl", "start", "losos-secure-boot-cancel.service"];

/// The command that restarts the machine.
pub const RESTART_ARGV: [&str; 2] = ["systemctl", "reboot"];

/// The command that restarts into the firmware's own settings.
pub const FIRMWARE_ARGV: [&str; 3] = ["systemctl", "reboot", "--firmware-setup"];

/// Reads `losos-secure-boot`'s line: `{"secure_boot": …, "key": …, "code": …}`.
pub fn parse_secure_boot(json: &str) -> Option<SecureBoot> {
    let v: Value = serde_json::from_str(json.trim()).ok()?;
    let key = match v.get("key")?.as_str()? {
        "missing" => Key::Missing,
        "requested" => Key::Requested,
        "enrolled" => Key::Enrolled,
        _ => return None,
    };
    Some(SecureBoot {
        on: v.get("secure_boot")?.as_bool()?,
        key,
        code: v.get("code").and_then(Value::as_str).map(str::to_owned),
    })
}

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
    /// `None` when this system has no cadence to choose.
    cadence: Option<&'static str>,
    /// `None` when this system has no security service.
    report: Option<Vec<Check>>,
    /// `None` when this system cannot set up Secure Boot.
    secure_boot: Option<SecureBoot>,
    reset: Reset,
}

fn read() -> Snapshot {
    Snapshot {
        cadence: stdout(&CADENCE_ARGV).ok().and_then(|o| parse_cadence(&o)),
        report: stdout(&REPORT_ARGV).ok().and_then(|j| parse_report(&j)),
        secure_boot: stdout(&SECURE_BOOT_ARGV)
            .ok()
            .and_then(|j| parse_secure_boot(&j)),
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
    /// The cadence being switched to, while polkit asks and the unit runs.
    switching: Option<(&'static str, mpsc::Receiver<Result<String, String>>)>,
    cadence_error: Option<String>,
    /// A Secure Boot step running: enrolling, cancelling or restarting.
    securing: Option<mpsc::Receiver<Result<String, String>>>,
    secure_error: Option<String>,
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

    fn switch(&mut self, cadence: &'static str) {
        let (tx, rx) = mpsc::channel();
        derisk_sys::thread::detach(move || {
            let argv = set_cadence_argv(cadence);
            let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
            let _ = tx.send(stdout(&argv));
        });
        self.cadence_error = None;
        self.switching = Some((cadence, rx));
    }

    fn secure(&mut self, argv: &'static [&'static str]) {
        let (tx, rx) = mpsc::channel();
        derisk_sys::thread::detach(move || {
            let _ = tx.send(stdout(argv));
        });
        self.secure_error = None;
        self.securing = Some(rx);
    }

    fn poll(&mut self) {
        if let Some(rx) = &self.securing
            && let Ok(done) = rx.try_recv()
        {
            // Read back: the request is shim's variables, not this page's.
            self.secure_error = done.err();
            self.securing = None;
            self.refresh();
        }
        if let Some((_, rx)) = &self.switching
            && let Ok(done) = rx.try_recv()
        {
            // Read back rather than assumed: the file is what updates use.
            self.cadence_error = done.err();
            self.switching = None;
            self.refresh();
        }
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

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(crate::layout::note(
        &mcsapi_components::Tokens::current(ui.ctx()),
        text,
    ));
}

const RED: egui::Color32 = egui::Color32::from_rgb(248, 113, 113);

/// What MokManager shows, step by step, once a request is waiting. Its
/// menus are in English whatever the desktop's language, so the names are
/// quoted as it shows them.
fn mok_steps(code: &str) -> [String; 4] {
    [
        "Restart this machine.".into(),
        "A blue screen says \"Press any key to perform MOK management\". Press any key.".into(),
        "Choose \"Enroll MOK\", then \"Continue\", then \"Yes\".".into(),
        format!("Type {code}, press Enter, then choose \"Reboot\"."),
    ]
}

fn secure_boot(ui: &mut egui::Ui, state: &mut SystemUi, status: Option<&SecureBoot>) {
    let Some(status) = status else {
        hint(
            ui,
            "This system cannot set up Secure Boot from here. A LosOS release signed for Secure Boot can.",
        );
        return;
    };
    let busy = state.securing.is_some();
    match (status.on, status.key) {
        (true, _) => {
            ui.label("Secure Boot is on. The firmware starts only what LosOS's key signed.");
        }
        (false, Key::Enrolled) => {
            ui.label("LosOS's key is enrolled. Turn Secure Boot on in the firmware's settings to finish.");
            hint(
                ui,
                "The machine restarts into its firmware's settings. Secure Boot is often under Security or Boot.",
            );
            if ui
                .add_enabled(!busy, egui::Button::new("Restart into firmware settings"))
                .clicked()
            {
                state.secure(&FIRMWARE_ARGV);
            }
        }
        (false, Key::Requested) => match &status.code {
            Some(code) => {
                ui.label("LosOS's key is ready to enroll. Write down this code first:");
                ui.label(egui::RichText::new(code).monospace().size(28.0).strong());
                for (i, step) in mok_steps(code).iter().enumerate() {
                    ui.label(format!("{}. {step}", i + 1));
                }
                hint(ui, "Then come back here to turn Secure Boot on.");
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!busy, egui::Button::new("Restart now"))
                        .clicked()
                    {
                        state.secure(&RESTART_ARGV);
                    }
                    if ui.add_enabled(!busy, egui::Button::new("Cancel")).clicked() {
                        state.secure(&CANCEL_ARGV);
                    }
                });
            }
            None => {
                ui.label("A request to enroll LosOS's key is waiting, but its code is gone.");
                if ui
                    .add_enabled(!busy, egui::Button::new("Start over"))
                    .clicked()
                {
                    state.secure(&ENROLL_ARGV);
                }
            }
        },
        (false, Key::Missing) => {
            ui.label("Secure Boot is off.");
            hint(
                ui,
                "With it on, the firmware refuses to start a system nobody signed. Setting it up asks for an administrator's password, then takes one restart and a code typed into a blue screen.",
            );
            if ui
                .add_enabled(!busy, egui::Button::new("Set up Secure Boot…"))
                .clicked()
            {
                state.secure(&ENROLL_ARGV);
            }
        }
    }
    if busy {
        ui.spinner();
    }
    if let Some(error) = &state.secure_error {
        ui.label(egui::RichText::new(error).color(RED));
    }
    if ui.link("How Secure Boot works on LosOS").clicked() {
        let _ = mcsapi_ui::Docs::from_env().open(&mcsapi_ui::DocLink::new("secure-boot"));
    }
}

pub(crate) fn page(ui: &mut egui::Ui, state: &mut SystemUi, theme: &Theme) {
    if state.snapshot.is_none() && state.reading.is_none() {
        state.refresh();
    }
    state.poll();
    if state.reading.is_some()
        || state.resetting.is_some()
        || state.switching.is_some()
        || state.securing.is_some()
    {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(200));
    }
    let Some(snapshot) = state.snapshot.clone() else {
        ui.spinner();
        return;
    };

    ui.heading(egui::RichText::new("Updates").color(theme.foreground));
    match snapshot.cadence {
        None => hint(
            ui,
            "This system does not offer a choice of releases to update to.",
        ),
        Some(current) => {
            hint(
                ui,
                "Which releases this machine updates to. Changing it asks for an administrator's password. A slower choice keeps the version you have until it has a newer one.",
            );
            let pending = state.switching.as_ref().map(|(c, _)| *c);
            let mut picked = None;
            ui.add_enabled_ui(pending.is_none(), |ui| {
                for cadence in &CADENCES {
                    let selected = pending.unwrap_or(current) == cadence.id;
                    ui.horizontal(|ui| {
                        if ui.radio(selected, cadence.name).clicked() && !selected {
                            picked = Some(cadence.id);
                        }
                        ui.label(crate::layout::note(
                            &mcsapi_components::Tokens::current(ui.ctx()),
                            cadence.detail,
                        ));
                    });
                }
            });
            if let Some(cadence) = picked {
                state.switch(cadence);
            }
            if pending.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Changing which releases this machine updates to…");
                });
            }
            if let Some(error) = &state.cadence_error {
                ui.label(egui::RichText::new(error).color(RED));
            }
        }
    }
    ui.add_space(16.0);

    ui.heading(egui::RichText::new("Security report").color(theme.foreground));
    match &snapshot.report {
        None => hint(ui, "This system has no security report."),
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
                // Words, not check marks: the Settings app's font has none.
                let (verdict, color) = if check.passed {
                    ("passed", theme.accent)
                } else {
                    ("did not pass", RED)
                };
                let title =
                    egui::RichText::new(format!("{} · {verdict}", check.summary)).color(color);
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

    ui.heading(egui::RichText::new("Secure Boot").color(theme.foreground));
    secure_boot(ui, state, snapshot.secure_boot.as_ref());
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
            "This machine cannot be reset from here: the request is kept in UEFI firmware, and this machine did not start with UEFI.",
        );
        return;
    }
    hint(
        ui,
        "Erases every account, every file and every setting, then restarts. The system itself stays as it is now, with its updates.",
    );
    if let Some(error) = &state.error {
        ui.label(egui::RichText::new(error).color(RED));
    }
    if state.resetting.is_some() {
        crate::stuck::working(ui, "Asking for a factory reset…");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cadence_is_one_settings_knows() {
        assert_eq!(parse_cadence("weekly\n"), Some("weekly"));
        assert_eq!(parse_cadence("yearly"), Some("yearly"));
        assert_eq!(parse_cadence("hourly\n"), None);
        assert_eq!(parse_cadence(""), None);
    }

    #[test]
    fn secure_boot_reads_losos_secure_boot() {
        assert_eq!(
            parse_secure_boot(r#"{"secure_boot":false,"key":"requested","code":"01234567"}"#),
            Some(SecureBoot {
                on: false,
                key: Key::Requested,
                code: Some("01234567".into()),
            })
        );
        assert_eq!(
            parse_secure_boot("{\"secure_boot\":true,\"key\":\"enrolled\",\"code\":null}\n"),
            Some(SecureBoot {
                on: true,
                key: Key::Enrolled,
                code: None,
            })
        );
        assert_eq!(
            parse_secure_boot(r#"{"secure_boot":false,"key":"lost"}"#),
            None
        );
        assert_eq!(parse_secure_boot(""), None);
    }

    #[test]
    fn the_steps_name_the_code() {
        assert!(mok_steps("01234567")[3].contains("01234567"));
    }

    #[test]
    fn a_cadence_is_set_through_its_unit() {
        assert_eq!(
            set_cadence_argv("monthly"),
            ["systemctl", "start", "losos-update-channel@monthly.service"]
        );
    }
}
