//! The Bluetooth page: the adapter, the devices paired with it, and the
//! ones nearby.
//!
//! BlueZ owns all of it, so the page applies at once, like the Sign-in
//! page, and asks through `bluetoothctl`, whose one-shot commands answer
//! on stdout and exit non-zero when BlueZ refuses. Every command runs on a
//! worker thread with `--timeout`, because bluetoothctl waits for
//! bluetoothd forever when the daemon is not running.
//!
//! Pairing registers bluetoothctl's agent as NoInputNoOutput: there is no
//! terminal for it to show a passkey on or read one from, so headphones,
//! speakers, mice and controllers, which pair with "just works", pair
//! here, and a keyboard that asks for a typed passkey does not yet.

use std::{
    process::{Command, Stdio},
    sync::mpsc,
};

use mcsapi_ui::{Theme, egui};

/// How long a command may wait for bluetoothd, in seconds.
const WAIT: &str = "5";
/// How long a scan looks for devices, in seconds.
const SCAN: &str = "10";
/// How long pairing may take, in seconds: the device may need its own
/// button pressed.
const PAIR: &str = "30";

/// The adapter, as `bluetoothctl show` describes it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Adapter {
    /// Its name.
    pub name: String,
    /// Whether its radio is on.
    pub powered: bool,
}

/// Reads `bluetoothctl show`: `None` without an adapter.
pub fn parse_show(output: &str) -> Option<Adapter> {
    if !output.lines().any(|l| l.starts_with("Controller ")) {
        return None;
    }
    let field = |key: &str| {
        output.lines().find_map(|l| {
            l.trim()
                .strip_prefix(key)
                .and_then(|rest| rest.strip_prefix(':'))
                .map(|v| v.trim().to_owned())
        })
    };
    Some(Adapter {
        name: field("Alias").or_else(|| field("Name")).unwrap_or_default(),
        powered: field("Powered").as_deref() == Some("yes"),
    })
}

/// A device BlueZ knows.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Device {
    /// Its address, `AA:BB:CC:DD:EE:FF`.
    pub address: String,
    /// What it calls itself.
    pub name: String,
    /// Whether it is paired with this machine.
    pub paired: bool,
    /// Whether it is connected now.
    pub connected: bool,
}

impl Device {
    /// Whether the device never said its name, so BlueZ named it after its
    /// address. A scan finds many of these, and a list of them helps nobody
    /// pick one.
    pub fn unnamed(&self) -> bool {
        self.name
            .replace('-', ":")
            .eq_ignore_ascii_case(&self.address)
    }
}

/// Reads `bluetoothctl devices`: one `Device <address> <name>` per line,
/// in BlueZ's order.
pub fn parse_devices(output: &str) -> impl Iterator<Item = (String, String)> + '_ {
    output.lines().filter_map(|line| {
        let rest = line.strip_prefix("Device ")?;
        let (address, name) = rest.split_once(' ').unwrap_or((rest, ""));
        (address.len() == 17).then(|| (address.to_owned(), name.trim().to_owned()))
    })
}

/// Every device from the three `devices` listings, paired ones first.
pub fn merge(all: &str, paired: &str, connected: &str) -> Vec<Device> {
    let paired: Vec<String> = parse_devices(paired).map(|(a, _)| a).collect();
    let connected: Vec<String> = parse_devices(connected).map(|(a, _)| a).collect();
    let mut devices: Vec<Device> = parse_devices(all)
        .map(|(address, name)| Device {
            paired: paired.contains(&address),
            connected: connected.contains(&address),
            address,
            name,
        })
        .collect();
    devices.sort_by_key(|d| !d.paired);
    devices
}

/// A change, as the bluetoothctl commands that make it, run in order until
/// one fails.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Job {
    /// What the page says while it runs.
    pub label: String,
    /// The command lines.
    pub steps: Vec<Vec<String>>,
}

fn bluetoothctl(timeout: &str, args: &[&str]) -> Vec<String> {
    ["bluetoothctl", "--timeout", timeout]
        .into_iter()
        .chain(args.iter().copied())
        .map(str::to_owned)
        .collect()
}

/// Turns the adapter's radio on or off.
pub fn power(on: bool) -> Job {
    Job {
        label: if on {
            "Turning Bluetooth on"
        } else {
            "Turning Bluetooth off"
        }
        .into(),
        steps: vec![bluetoothctl(
            WAIT,
            &["power", if on { "on" } else { "off" }],
        )],
    }
}

/// Looks for devices nearby.
pub fn scan() -> Job {
    Job {
        label: "Looking for devices".into(),
        steps: vec![bluetoothctl(SCAN, &["scan", "on"])],
    }
}

/// Pairs with `device`, trusts it so it may reconnect by itself, and
/// connects to it.
pub fn pair(device: &Device) -> Job {
    let a = device.address.as_str();
    Job {
        label: format!("Pairing with {}", device.name),
        steps: vec![
            bluetoothctl(PAIR, &["--agent", "NoInputNoOutput", "pair", a]),
            bluetoothctl(WAIT, &["trust", a]),
            bluetoothctl(PAIR, &["connect", a]),
        ],
    }
}

/// Connects to a paired device.
pub fn connect(device: &Device) -> Job {
    Job {
        label: format!("Connecting to {}", device.name),
        steps: vec![bluetoothctl(PAIR, &["connect", &device.address])],
    }
}

/// Disconnects a device.
pub fn disconnect(device: &Device) -> Job {
    Job {
        label: format!("Disconnecting {}", device.name),
        steps: vec![bluetoothctl(WAIT, &["disconnect", &device.address])],
    }
}

/// Unpairs a device, which BlueZ then forgets.
pub fn forget(device: &Device) -> Job {
    Job {
        label: format!("Forgetting {}", device.name),
        steps: vec![bluetoothctl(WAIT, &["remove", &device.address])],
    }
}

/// What the page last read.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Snapshot {
    /// `None` without an adapter, or without bluetoothd.
    adapter: Option<Adapter>,
    devices: Vec<Device>,
}

fn output(argv: &[String]) -> Result<String, String> {
    let out = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {}: {e}", argv[0]))?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        Ok(text)
    } else {
        // bluetoothctl says why on its last line, such as "Failed to pair:
        // org.bluez.Error.AuthenticationFailed".
        Err(text
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .map_or_else(|| format!("{} failed", argv[0]), |l| l.trim().to_owned()))
    }
}

fn read() -> Snapshot {
    let list = |args: &[&str]| output(&bluetoothctl(WAIT, args)).unwrap_or_default();
    let adapter = parse_show(&list(&["show"]));
    if adapter.is_none() {
        return Snapshot::default();
    }
    Snapshot {
        adapter,
        devices: merge(
            &list(&["devices"]),
            &list(&["devices", "Paired"]),
            &list(&["devices", "Connected"]),
        ),
    }
}

enum Event {
    Read(Snapshot),
    Done(Result<(), String>),
}

/// The page's state.
#[derive(Debug, Default)]
pub(crate) struct BluetoothUi {
    snapshot: Option<Snapshot>,
    reading: Option<mpsc::Receiver<Event>>,
    running: Option<(String, mpsc::Receiver<Event>)>,
    status: Option<String>,
}

impl BluetoothUi {
    fn refresh(&mut self) {
        let (tx, rx) = mpsc::channel();
        derisk_sys::thread::detach(move || {
            let _ = tx.send(Event::Read(read()));
        });
        self.reading = Some(rx);
    }

    fn run(&mut self, job: Job) {
        let (tx, rx) = mpsc::channel();
        let label = job.label.clone();
        derisk_sys::thread::detach(move || {
            let done = job.steps.iter().try_for_each(|argv| output(argv).map(drop));
            let _ = tx.send(Event::Done(done));
        });
        self.status = None;
        self.running = Some((label, rx));
    }

    fn poll(&mut self) {
        if let Some(rx) = &self.reading
            && let Ok(Event::Read(snapshot)) = rx.try_recv()
        {
            self.snapshot = Some(snapshot);
            self.reading = None;
        }
        if let Some((label, rx)) = &self.running
            && let Ok(Event::Done(result)) = rx.try_recv()
        {
            self.status = match result {
                Ok(()) => None,
                Err(e) => Some(format!("{label}: {e}")),
            };
            self.running = None;
            self.refresh();
        }
    }
}

fn hint(ui: &mut egui::Ui, theme: &Theme, text: &str) {
    ui.label(egui::RichText::new(text).small().color(theme.border));
}

pub(crate) fn page(ui: &mut egui::Ui, state: &mut BluetoothUi, theme: &Theme) {
    if state.snapshot.is_none() && state.reading.is_none() {
        state.refresh();
    }
    state.poll();
    if state.reading.is_some() || state.running.is_some() {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(200));
    }
    let Some(snapshot) = state.snapshot.clone() else {
        ui.spinner();
        return;
    };
    let Some(adapter) = &snapshot.adapter else {
        hint(
            ui,
            theme,
            "No Bluetooth adapter was found, or Bluetooth is not running on this machine.",
        );
        return;
    };

    if let Some((label, _)) = &state.running {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(label);
        });
        ui.add_space(8.0);
    } else if let Some(status) = &state.status {
        ui.label(egui::RichText::new(status).color(egui::Color32::from_rgb(248, 113, 113)));
        ui.add_space(8.0);
    }

    let mut job = None;
    let busy = state.running.is_some();
    ui.add_enabled_ui(!busy, |ui| {
        let mut on = adapter.powered;
        if ui.checkbox(&mut on, "Bluetooth").changed() {
            job = Some(power(on));
        }
        if !adapter.name.is_empty() {
            hint(ui, theme, &format!("Visible to devices pairing with it as {}", adapter.name));
        }
        if !adapter.powered {
            return;
        }
        ui.add_space(12.0);

        ui.label(
            egui::RichText::new("My devices")
                .strong()
                .color(theme.foreground),
        );
        let paired: Vec<&Device> = snapshot.devices.iter().filter(|d| d.paired).collect();
        if paired.is_empty() {
            hint(ui, theme, "Nothing is paired yet.");
        }
        for device in paired {
            ui.horizontal(|ui| {
                ui.label(&device.name);
                hint(
                    ui,
                    theme,
                    if device.connected { "Connected" } else { "Not connected" },
                );
                let toggle = if device.connected { "Disconnect" } else { "Connect" };
                if ui.button(toggle).clicked() {
                    job = Some(if device.connected {
                        disconnect(device)
                    } else {
                        connect(device)
                    });
                }
                if ui.button("Forget").clicked() {
                    job = Some(forget(device));
                }
            });
        }
        ui.add_space(12.0);

        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Nearby")
                    .strong()
                    .color(theme.foreground),
            );
            if ui.button("Look for devices").clicked() {
                job = Some(scan());
            }
        });
        hint(
            ui,
            theme,
            "Put the device in pairing mode first. Devices that show a code to type are not supported yet.",
        );
        for device in snapshot
            .devices
            .iter()
            .filter(|d| !d.paired && !d.unnamed())
        {
            ui.horizontal(|ui| {
                ui.label(&device.name);
                if ui.button("Pair").clicked() {
                    job = Some(pair(device));
                }
            });
        }
    });
    if let Some(job) = job {
        state.run(job);
    }
}
