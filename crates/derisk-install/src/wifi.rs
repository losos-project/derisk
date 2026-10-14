//! The network, watched from a thread of its own: whether the machine is
//! online, what each wired port is doing, the Wi-Fi networks in range and
//! the ones wpa_supplicant knows. The installer's and first-boot setup's
//! Network page and the Settings app's Network page both draw from it.
//!
//! The talking to networkd's files and wpa_supplicant happens on that thread
//! (see [`crate::network`]), so a slow or restarting daemon never holds up a
//! frame; a page reads the latest [`Status`] it published.

use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use crate::network::{self, Control, Network, Saved, Wired};

/// How long a scan is given before its results are final.
const SCAN_TIME: Duration = Duration::from_secs(4);
/// Association, the handshake and DHCP together. A wrong passphrase looks
/// like a handshake that never completes, so this is also how long it takes
/// to say so.
const CONNECT_TIME: Duration = Duration::from_secs(30);
/// How often the worker looks again.
const POLL: Duration = Duration::from_millis(500);

/// What the worker last saw.
#[derive(Clone, Debug, Default)]
pub struct Status {
    /// Whether some link is routable.
    pub online: bool,
    /// The wired ports.
    pub wired: Vec<Wired>,
    /// The Wi-Fi interface, if there is one.
    pub interface: Option<String>,
    /// Whether wpa_supplicant answers on it.
    pub wifi_ready: bool,
    /// Networks in range, strongest first.
    pub networks: Vec<Network>,
    /// Networks wpa_supplicant knows, in its order.
    pub saved: Vec<Saved>,
    /// Whether a scan is still running.
    pub scanning: bool,
    /// The network being joined.
    pub joining: Option<String>,
    /// The network wpa_supplicant is associated with.
    pub connected: Option<String>,
    /// The last thing that went wrong, as a sentence.
    pub message: Option<String>,
}

impl Status {
    /// The known network with `ssid`, if wpa_supplicant has one, so a page
    /// can join it again without asking for its passphrase.
    pub fn saved(&self, ssid: &[u8]) -> Option<&Saved> {
        self.saved.iter().find(|s| s.ssid == ssid)
    }
}

enum Order {
    Scan,
    Join(Network, String),
    Forget(u32),
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
    /// Starts watching. With `save`, a network joined or forgotten is
    /// written to wpa_supplicant's configuration so the installed system
    /// sees the change after a reboot; a live installer has nothing to save
    /// it to.
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

    /// Forgets the saved network `id`, and its passphrase with it.
    pub fn forget(&self, id: u32) {
        let _ = self.orders.send(Order::Forget(id));
    }
}

struct Joining {
    name: String,
    id: u32,
    /// Whether the network was saved before this join, so a failed join
    /// leaves it saved instead of removing it.
    existing: bool,
    since: Instant,
}

fn worker(shared: &Mutex<Status>, orders: &Receiver<Order>, save: bool) {
    let mut control: Option<Control> = None;
    let mut scan_since: Option<Instant> = None;
    let mut joining: Option<Joining> = None;
    loop {
        let order = match orders.recv_timeout(POLL) {
            Ok(order) => Some(order),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let mut status = shared.lock().map(|s| s.clone()).unwrap_or_default();
        status.online = network::online(Path::new(network::NETIF_STATE));
        status.wired = network::wired(
            Path::new(network::SYS_CLASS_NET),
            Path::new(network::NETIF_LINKS),
        );

        if control.is_none() {
            status.interface = network::interfaces(Path::new(network::SYS_CLASS_NET))
                .into_iter()
                .next();
            // wpa_supplicant makes its socket a moment after the interface
            // appears, so this is tried again every poll until it is there.
            control = match status.interface.as_deref().map(Control::open) {
                Some(Ok(c)) => Some(c),
                // The socket is wpa_supplicant's group's alone. Setup runs
                // as root; a session reaches it only through that group.
                Some(Err(e)) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    status.message = Some(
                        "This account may not change Wi-Fi networks. It needs to be \
                         in the wpa_supplicant group."
                            .into(),
                    );
                    None
                }
                _ => None,
            };
            if let Some(c) = &control
                && c.request("SCAN").is_ok()
            {
                scan_since = Some(Instant::now());
            }
        }
        // NixOS restarts wpa_supplicant whenever a wireless interface comes
        // or goes, and the restarted daemon listens on a new socket.
        if control.as_ref().is_some_and(|c| c.request("PING").is_err()) {
            control = None;
            joining = None;
        }
        status.wifi_ready = control.is_some();

        if let Some(c) = &control {
            let saved = c.saved().unwrap_or_default();
            match order {
                // A reply of FAIL-BUSY means a scan is already running,
                // which serves as well.
                Some(Order::Scan) => {
                    let _ = c.request("SCAN");
                    scan_since = Some(Instant::now());
                }
                Some(Order::Join(network, passphrase)) => {
                    if let Some(old) = joining.take()
                        && !old.existing
                    {
                        let _ = c.forget(old.id);
                    }
                    let known = saved
                        .iter()
                        .find(|s| s.ssid == network.ssid)
                        .filter(|_| passphrase.is_empty());
                    let started = match known {
                        Some(known) => c.select(known.id).map(|()| (known.id, true)),
                        None => c.connect(&network, &passphrase).map(|id| (id, false)),
                    };
                    match started {
                        Ok((id, existing)) => {
                            status.message = None;
                            joining = Some(Joining {
                                name: network.name(),
                                id,
                                existing,
                                since: Instant::now(),
                            });
                        }
                        Err(e) => status.message = Some(e.to_string()),
                    }
                }
                Some(Order::Forget(id)) => {
                    let forgotten = c
                        .forget(id)
                        .and_then(|()| if save { c.save() } else { Ok(()) });
                    if let Err(e) = forgotten {
                        status.message = Some(e.to_string());
                    }
                }
                None => {}
            }
            if scan_since.is_some()
                && let Ok(reply) = c.request("SCAN_RESULTS")
            {
                status.networks = network::parse_scan_results(&reply);
            }
            if scan_since.is_some_and(|t| t.elapsed() > SCAN_TIME) {
                scan_since = None;
            }
            let wpa = c.status().unwrap_or_default();
            let get = |key: &str| wpa.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
            status.connected = (get("wpa_state").as_deref() == Some("COMPLETED"))
                .then(|| {
                    get("ssid")
                        .map(|s| String::from_utf8_lossy(&network::decode_ssid(&s)).into_owned())
                })
                .flatten();
            if let Some(j) = &joining {
                if status.connected.as_deref() == Some(j.name.as_str()) {
                    finish(c, j, save, &mut status);
                    joining = None;
                } else if j.since.elapsed() > CONNECT_TIME {
                    if !j.existing {
                        let _ = c.forget(j.id);
                    }
                    let _ = c.enable_all();
                    status.message = Some(format!(
                        "Could not join {}. Check the passphrase and try again.",
                        j.name
                    ));
                    joining = None;
                }
            }
            status.saved = c.saved().unwrap_or_default();
        }
        status.scanning = scan_since.is_some();
        status.joining = joining.as_ref().map(|j| j.name.clone());
        if let Ok(mut s) = shared.lock() {
            *s = status;
        }
    }
}

/// A join went through: the other saved networks become candidates again,
/// an older entry for the same network gives way to the one whose
/// passphrase just worked, and with `save` the result is written down.
fn finish(c: &Control, joined: &Joining, save: bool, status: &mut Status) {
    let _ = c.enable_all();
    let saved = c.saved().unwrap_or_default();
    let ssid = saved
        .iter()
        .find(|s| s.id == joined.id)
        .map(|s| s.ssid.clone());
    for stale in saved
        .iter()
        .filter(|s| s.id != joined.id && Some(&s.ssid) == ssid.as_ref())
    {
        let _ = c.forget(stale.id);
    }
    if save && let Err(e) = c.save() {
        status.message = Some(format!("Joined, but not saved: {e}"));
    }
}
