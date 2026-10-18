//! The network, watched from a thread of its own: whether the machine is
//! online, what each wired port is doing, the Wi-Fi networks in range and
//! the ones saved. The installer's and first-boot setup's Network page and
//! the Settings app's Network page both draw from it.
//!
//! The talking to NetworkManager happens on that thread (see
//! [`crate::network`]), so a slow or restarting daemon never holds up a
//! frame; a page reads the latest [`Status`] it published.

use std::{
    collections::HashMap,
    io::Read,
    process::Child,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use crate::network::{self, Network, Saved, Wired};

/// How long a scan is given before its results are final.
const SCAN_TIME: Duration = Duration::from_secs(4);
/// How often the worker looks again. Every look runs nmcli a few times, so
/// it is less often than a frame and often enough to follow a join.
const POLL: Duration = Duration::from_secs(1);

/// What the worker last saw.
#[derive(Clone, Debug, Default)]
pub struct Status {
    /// Whether the machine reaches beyond its own networks.
    pub online: bool,
    /// The wired ports.
    pub wired: Vec<Wired>,
    /// The Wi-Fi interface, if there is one.
    pub interface: Option<String>,
    /// Whether it can scan and join.
    pub wifi_ready: bool,
    /// Networks in range, strongest first.
    pub networks: Vec<Network>,
    /// The saved Wi-Fi connections.
    pub saved: Vec<Saved>,
    /// Whether a scan is still running.
    pub scanning: bool,
    /// The network being joined.
    pub joining: Option<String>,
    /// The network the Wi-Fi interface is joined to.
    pub connected: Option<String>,
    /// The last thing that went wrong, as a sentence.
    pub message: Option<String>,
}

impl Status {
    /// The saved connection for `ssid`, if there is one, so a page can join
    /// it again without asking for its passphrase.
    pub fn saved(&self, ssid: &[u8]) -> Option<&Saved> {
        self.saved.iter().find(|s| s.ssid == ssid)
    }
}

enum Order {
    Scan,
    Join(Network, String),
    Forget(String),
}

/// The network, watched from a thread of its own.
pub struct Wifi {
    status: Arc<Mutex<Status>>,
    orders: Sender<Order>,
}

impl std::fmt::Debug for Wifi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wifi").finish_non_exhaustive()
    }
}

impl Wifi {
    /// Starts watching. With `save`, a network joined is kept by
    /// NetworkManager across reboots; a live installer has nothing to keep
    /// it on.
    pub fn start(save: bool) -> Self {
        let status = Arc::new(Mutex::new(Status::default()));
        let (orders, rx) = mpsc::channel();
        let shared = Arc::clone(&status);
        thread::Builder::new()
            .name("derisk-wifi".into())
            .spawn(move || worker(&shared, &rx, save))
            .expect("a thread can be started");
        Self { status, orders }
    }

    /// What the worker last saw.
    pub fn status(&self) -> Status {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Looks for networks again.
    pub fn scan(&self) {
        let _ = self.orders.send(Order::Scan);
    }

    /// Joins `network` with `passphrase`. An empty passphrase joins an open
    /// network, or a saved one with the passphrase it was saved with.
    pub fn join(&self, network: Network, passphrase: String) {
        let _ = self.orders.send(Order::Join(network, passphrase));
    }

    /// Forgets the saved connection `id`, and its passphrase with it.
    pub fn forget(&self, id: String) {
        let _ = self.orders.send(Order::Forget(id));
    }
}

struct Joining {
    name: String,
    ssid: Vec<u8>,
    uuid: String,
    /// Whether the connection was saved before this join, so a failed join
    /// leaves it saved instead of removing it.
    existing: bool,
    /// `nmcli connection up`, which ends when the join worked or failed.
    child: Child,
}

/// The sentence for a failed nmcli call. NetworkManager's polkit rule lets
/// the networkmanager group change networks; setup runs as root.
fn say(error: &std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        "This account may not change Wi-Fi networks. It needs to be in the \
         networkmanager group."
            .into()
    } else {
        error.to_string()
    }
}

fn worker(shared: &Mutex<Status>, orders: &Receiver<Order>, save: bool) {
    let mut scan_since: Option<Instant> = None;
    let mut joining: Option<Joining> = None;
    // SSIDs of saved connections by UUID: reading one is a call of its own,
    // and a connection's SSID does not change.
    let mut ssids: HashMap<String, Vec<u8>> = HashMap::new();
    let mut scanned_first = false;
    loop {
        let order = match orders.recv_timeout(POLL) {
            Ok(order) => Some(order),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let mut status = shared.lock().map(|s| s.clone()).unwrap_or_default();
        status.online = network::nmcli(&["-f", "STATE", "general"])
            .is_ok_and(|general| network::parse_online(&general));
        let devices = network::nmcli(&["-f", "DEVICE,TYPE,STATE", "device"])
            .map(|text| network::parse_devices(&text))
            .unwrap_or_default();
        status.wired = network::wired(&devices);
        let wifi = network::wifi_device(&devices).cloned();
        status.interface = wifi.as_ref().map(|d| d.name.clone());
        status.wifi_ready = wifi.as_ref().is_some_and(|d| network::wifi_ready(&d.state));

        // Joins and forgets change the saved connections; they are read
        // again after one.
        let mut changed = order.is_some();
        let known = saved(&mut ssids);
        if let Some(device) = wifi.as_ref().filter(|_| status.wifi_ready) {
            let interface = device.name.as_str();
            if !scanned_first {
                scanned_first = true;
                rescan(interface);
                scan_since = Some(Instant::now());
            }
            match order {
                // A refusal to scan again so soon after the last scan
                // leaves that scan's results, which serve as well.
                Some(Order::Scan) => {
                    rescan(interface);
                    scan_since = Some(Instant::now());
                }
                Some(Order::Join(network, passphrase)) => {
                    if let Some(old) = joining.take() {
                        abandon(old);
                    }
                    match start_join(&network, &passphrase, interface, &known, save) {
                        Ok(j) => {
                            status.message = None;
                            joining = Some(j);
                        }
                        Err(e) => status.message = Some(say(&e)),
                    }
                }
                Some(Order::Forget(id)) => {
                    if let Err(e) = network::nmcli(&["connection", "delete", "uuid", &id]) {
                        status.message = Some(say(&e));
                    }
                }
                None => {}
            }
            if let Ok(list) = network::nmcli(&[
                "-f",
                "IN-USE,SSID-HEX,SIGNAL,SECURITY",
                "device",
                "wifi",
                "list",
                "ifname",
                interface,
                "--rescan",
                "no",
            ]) {
                let (networks, in_use) = network::parse_wifi_list(&list);
                status.networks = networks;
                status.connected = in_use.map(|ssid| String::from_utf8_lossy(&ssid).into_owned());
            }
            if scan_since.is_some_and(|t| t.elapsed() > SCAN_TIME) {
                scan_since = None;
            }
        } else {
            status.networks.clear();
            status.connected = None;
        }

        if let Some(j) = joining.as_mut()
            && let Ok(Some(exit)) = j.child.try_wait()
        {
            let j = joining.take().expect("just matched");
            changed = true;
            if exit.success() {
                finish(&j, &known);
            } else {
                let mut reason = String::new();
                if let Some(mut err) = j.child.stderr {
                    let _ = err.read_to_string(&mut reason);
                }
                if !j.existing {
                    let _ = network::nmcli(&["connection", "delete", "uuid", &j.uuid]);
                }
                let reason = network::nmcli_error(&reason);
                status.message = Some(if reason.kind() == std::io::ErrorKind::PermissionDenied {
                    say(&reason)
                } else {
                    format!(
                        "Could not join {}. Check the passphrase and try again.",
                        j.name
                    )
                });
            }
        }
        status.saved = if changed { saved(&mut ssids) } else { known };
        status.scanning = scan_since.is_some();
        status.joining = joining.as_ref().map(|j| j.name.clone());
        if let Ok(mut s) = shared.lock() {
            *s = status;
        }
    }
}

/// Asks the Wi-Fi interface to scan; a refusal (a scan just ran) is fine.
fn rescan(interface: &str) {
    let _ = network::nmcli(&["device", "wifi", "rescan", "ifname", interface]);
}

/// The saved Wi-Fi connections, with each one's SSID from `ssids` or read
/// once into it.
fn saved(ssids: &mut HashMap<String, Vec<u8>>) -> Vec<Saved> {
    let connections = network::nmcli(&["-f", "UUID,TYPE,ACTIVE", "connection", "show"])
        .map(|text| network::parse_connections(&text))
        .unwrap_or_default();
    ssids.retain(|uuid, _| connections.iter().any(|(u, _)| u == uuid));
    connections
        .into_iter()
        .filter_map(|(id, current)| {
            if !ssids.contains_key(&id) {
                let ssid = network::nmcli(&[
                    "-g",
                    "802-11-wireless.ssid",
                    "connection",
                    "show",
                    "uuid",
                    &id,
                ])
                .ok()?;
                ssids.insert(id.clone(), ssid.trim_end_matches('\n').as_bytes().to_vec());
            }
            Some(Saved {
                ssid: ssids[&id].clone(),
                id,
                current,
            })
        })
        .collect()
}

/// Starts joining `network`: a saved connection is brought up as it is when
/// no passphrase is given, and otherwise a new one is added for it and
/// brought up with `passphrase`.
fn start_join(
    network: &Network,
    passphrase: &str,
    interface: &str,
    saved: &[Saved],
    save: bool,
) -> std::io::Result<Joining> {
    let known = saved
        .iter()
        .find(|s| s.ssid == network.ssid)
        .filter(|_| passphrase.is_empty());
    let (uuid, existing, secret) = match known {
        Some(known) => (known.id.clone(), true, None),
        None => {
            let (settings, secret) = network.security.settings(passphrase)?;
            let args = network::add_args(network, interface, &settings, save);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let added = network::nmcli(&args)?;
            let uuid = network::parse_added(&added)
                .ok_or_else(|| std::io::Error::other(format!("nmcli said {}", added.trim())))?;
            (uuid, false, secret)
        }
    };
    match network::bring_up(&uuid, secret.as_deref()) {
        Ok(child) => Ok(Joining {
            name: network.name(),
            ssid: network.ssid.clone(),
            uuid,
            existing,
            child,
        }),
        Err(e) => {
            if !existing {
                let _ = network::nmcli(&["connection", "delete", "uuid", &uuid]);
            }
            Err(e)
        }
    }
}

/// A join replaced by another before it finished: stopped, and its new
/// connection removed.
fn abandon(mut old: Joining) {
    let _ = old.child.kill();
    let _ = old.child.wait();
    if !old.existing {
        let _ = network::nmcli(&["connection", "delete", "uuid", &old.uuid]);
    }
}

/// A join went through: an older connection for the same network gives way
/// to the one whose passphrase just worked.
fn finish(joined: &Joining, saved: &[Saved]) {
    for stale in saved
        .iter()
        .filter(|s| s.id != joined.uuid && s.ssid == joined.ssid)
    {
        let _ = network::nmcli(&["connection", "delete", "uuid", &stale.id]);
    }
}
