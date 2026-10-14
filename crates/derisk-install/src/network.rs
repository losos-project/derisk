//! The network, as the setup screens need it: whether the machine is online,
//! what each wired port is doing, and Wi-Fi through wpa_supplicant.
//!
//! Links are systemd-networkd's: it runs DHCP on every wired port with a
//! cable and on whatever wireless link wpa_supplicant brings up, and writes
//! its verdict under /run/systemd/netif. `routable` there means some link has
//! an address and a route beyond itself, which is what downloading an image
//! or a time zone's clock needs. So joining a Wi-Fi network is all there is
//! to do here; a cable plugged in gets the machine online with nothing asked.
//!
//! wpa_supplicant's control interface is spoken directly: one datagram per
//! command and one per reply over a Unix socket, which is less code than
//! parsing `wpa_cli`'s output and needs no second process. Two details come
//! from how NixOS runs the daemon: it is unprivileged and sandboxed, with
//! the control sockets under /run/wpa_supplicant/control, and it can only
//! answer a client socket that sits under /run/wpa_supplicant/client and that
//! its group may write. nixpkgs patches `wpa_ctrl.c` to do exactly that for
//! `wpa_cli`; [`Control::open_at`] does the same here.
//!
//! This came from LosOS's text installer, which the setup screens replaced.

use std::{
    fs, io,
    os::unix::{fs::PermissionsExt, net::UnixDatagram},
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

/// networkd's overall state file.
pub const NETIF_STATE: &str = "/run/systemd/netif/state";
/// networkd's per-link state files, named by ifindex.
pub const NETIF_LINKS: &str = "/run/systemd/netif/links";
/// Network devices.
pub const SYS_CLASS_NET: &str = "/sys/class/net";

/// Whether networkd says some link is routable.
pub fn online(state_file: &Path) -> bool {
    fs::read_to_string(state_file)
        .map(|state| parse_online(&state))
        .unwrap_or(false)
}

/// Whether a networkd state file's `OPER_STATE` is `routable`.
pub fn parse_online(state: &str) -> bool {
    state
        .lines()
        .filter_map(|line| line.split_once('='))
        .any(|(key, value)| key == "OPER_STATE" && value == "routable")
}

/// What a wired port is doing, in the order someone plugging in a cable
/// sees it happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WiredState {
    /// Nothing on the other end.
    NoCable,
    /// A cable, and no address yet.
    Configuring,
    /// An address and a route.
    Online,
}

impl WiredState {
    /// How the setup screens say it.
    pub fn label(self) -> &'static str {
        match self {
            WiredState::NoCable => "no cable",
            WiredState::Configuring => "cable in, getting an address",
            WiredState::Online => "online",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// A wired port.
pub struct Wired {
    /// The interface name.
    pub name: String,
    /// What it is doing.
    pub state: WiredState,
}

/// The physical wired ports under `sys_class_net`, as the installer's
/// `20-wired` network matches them: Ethernet hardware (`type` 1, ARPHRD_ETHER)
/// with a device behind it, which leaves out loopback, bridges and veths,
/// and without a `wireless` directory, which leaves out Wi-Fi, whose type is
/// Ethernet too. Each one's state comes from its carrier and from networkd's
/// own file for it under `netif_links`, named by ifindex.
pub fn wired(sys_class_net: &Path, netif_links: &Path) -> Vec<Wired> {
    let mut ports: Vec<Wired> = fs::read_dir(sys_class_net)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let dir = entry.path();
            let read = |name: &str| {
                fs::read_to_string(dir.join(name))
                    .map(|s| s.trim().to_string())
                    .ok()
            };
            if read("type").as_deref() != Some("1")
                || !dir.join("device").exists()
                || dir.join("wireless").exists()
            {
                return None;
            }
            // Reading `carrier` fails while the link is down, which is the
            // same answer: nothing on the other end yet.
            let state = if read("carrier").as_deref() != Some("1") {
                WiredState::NoCable
            } else if read("ifindex")
                .and_then(|index| fs::read_to_string(netif_links.join(index)).ok())
                .is_some_and(|link| parse_online(&link))
            {
                WiredState::Online
            } else {
                WiredState::Configuring
            };
            Some(Wired {
                name: entry.file_name().into_string().ok()?,
                state,
            })
        })
        .collect();
    ports.sort_by(|a, b| a.name.cmp(&b.name));
    ports
}

/// Where NixOS's wpa_supplicant puts its control sockets.
pub const CONTROL_DIR: &str = "/run/wpa_supplicant/control";
/// Where a client's reply socket must sit for the daemon to answer it.
pub const CLIENT_DIR: &str = "/run/wpa_supplicant/client";

static CLIENTS: AtomicUsize = AtomicUsize::new(0);

/// A connection to one interface's wpa_supplicant control socket.
pub struct Control {
    socket: UnixDatagram,
    local: PathBuf,
}

impl Control {
    /// Connects to `interface`'s control socket.
    pub fn open(interface: &str) -> io::Result<Self> {
        Self::open_at(
            &Path::new(CONTROL_DIR).join(interface),
            Path::new(CLIENT_DIR),
        )
    }

    /// Connects to the control socket at `remote`, replying through a socket
    /// in `client_dir`.
    pub fn open_at(remote: &Path, client_dir: &Path) -> io::Result<Self> {
        let local = client_dir.join(format!(
            "derisk-{}-{}",
            std::process::id(),
            CLIENTS.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_file(&local);
        let socket = UnixDatagram::bind(&local)?;
        let control = Control { socket, local };
        // A datagram reply needs write permission on the socket it is sent
        // to, and the daemon runs as its own user.
        if let Some(gid) = group_id(Path::new("/etc/group"), "wpa_supplicant") {
            let _ = std::os::unix::fs::chown(&control.local, None, Some(gid));
        }
        fs::set_permissions(&control.local, fs::Permissions::from_mode(0o660))?;
        control.socket.connect(remote)?;
        control
            .socket
            .set_read_timeout(Some(Duration::from_secs(5)))?;
        Ok(control)
    }

    /// Sends a command and returns its reply.
    pub fn request(&self, command: &str) -> io::Result<String> {
        self.socket.send(command.as_bytes())?;
        // SCAN_RESULTS is the longest reply, and wpa_supplicant caps every
        // reply at 4096 bytes itself.
        let mut buf = vec![0u8; 8192];
        loop {
            let n = self.socket.recv(&mut buf)?;
            let reply = String::from_utf8_lossy(&buf[..n]).into_owned();
            // An unsolicited event, which only an ATTACHed client receives.
            // This one never attaches; skipping them costs nothing if it did.
            if reply.starts_with('<') {
                continue;
            }
            return Ok(reply);
        }
    }

    /// A command whose only successful answer is `OK`.
    pub fn ok(&self, command: &str) -> io::Result<()> {
        let reply = self.request(command)?;
        if reply.trim_end() == "OK" {
            Ok(())
        } else {
            // The command line holds no secret worth hiding from the person who
            // typed it, but it is not needed to say what failed either.
            let verb = command
                .split_whitespace()
                .take(3)
                .collect::<Vec<_>>()
                .join(" ");
            Err(io::Error::other(format!(
                "wpa_supplicant refused {verb}: {}",
                reply.trim_end()
            )))
        }
    }

    /// STATUS, as key-value pairs.
    pub fn status(&self) -> io::Result<Vec<(String, String)>> {
        Ok(parse_status(&self.request("STATUS")?))
    }

    /// Adds a network for `network` and selects it, which disables every
    /// other one. Returns the network id, for [`Control::forget`].
    pub fn connect(&self, network: &Network, passphrase: &str) -> io::Result<u32> {
        let reply = self.request("ADD_NETWORK")?;
        let id: u32 = reply.trim().parse().map_err(|_| {
            io::Error::other(format!("wpa_supplicant: ADD_NETWORK said {}", reply.trim()))
        })?;
        let set = |key: &str, value: &str| self.ok(&format!("SET_NETWORK {id} {key} {value}"));
        let result = (|| {
            // Hex, so an SSID may hold quotes, spaces or bytes that are not
            // UTF-8 without any quoting rule mattering.
            set("ssid", &hex(&network.ssid))?;
            for (key, value) in network.security.settings(passphrase)? {
                set(key, &value)?;
            }
            self.ok(&format!("SELECT_NETWORK {id}"))
        })();
        match result {
            Ok(()) => Ok(id),
            Err(e) => {
                let _ = self.forget(id);
                Err(e)
            }
        }
    }

    /// Writes the networks wpa_supplicant knows to its configuration file,
    /// so a network joined during setup is joined again after a reboot.
    /// Needs `update_config=1`, which NixOS sets for a user-controlled
    /// wpa_supplicant.
    pub fn save(&self) -> io::Result<()> {
        self.ok("SAVE_CONFIG")
    }

    /// Removes a network added with [`Control::connect`].
    pub fn forget(&self, id: u32) -> io::Result<()> {
        self.ok(&format!("REMOVE_NETWORK {id}"))
    }

    /// The networks wpa_supplicant knows, joined before or added now.
    pub fn saved(&self) -> io::Result<Vec<Saved>> {
        Ok(parse_list_networks(&self.request("LIST_NETWORKS")?))
    }

    /// Joins a network wpa_supplicant already knows, which, like
    /// [`Control::connect`], disables every other one.
    pub fn select(&self, id: u32) -> io::Result<()> {
        self.ok(&format!("SELECT_NETWORK {id}"))
    }

    /// Lets wpa_supplicant pick among every known network again, after a
    /// SELECT_NETWORK disabled the rest. Without it, the networks saved
    /// before would be written to the configuration as disabled and never
    /// joined by themselves again.
    pub fn enable_all(&self) -> io::Result<()> {
        self.ok("ENABLE_NETWORK all")
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.local);
    }
}

fn group_id(group_file: &Path, name: &str) -> Option<u32> {
    let groups = fs::read_to_string(group_file).ok()?;
    groups.lines().find_map(|line| {
        let mut fields = line.split(':');
        (fields.next()? == name).then_some(())?;
        fields.nth(1)?.parse().ok()
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// How a network asks to be joined, read from the flags in a scan result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    /// No encryption.
    Open,
    /// Opportunistic Wireless Encryption: encrypted, and no passphrase.
    Owe,
    /// WPA2-Personal, WPA3-Personal, or a transition network offering both.
    Passphrase {
        /// WPA2-Personal.
        psk: bool,
        /// WPA3-Personal.
        sae: bool,
    },
    /// 802.1X and WEP. The first needs more than a passphrase to describe,
    /// and the second is not worth describing; `wpa_cli` on another terminal
    /// can still join either.
    Unsupported(&'static str),
}

impl Security {
    /// Reads a scan result's flags, such as `[WPA2-PSK-CCMP][ESS]`.
    pub fn from_flags(flags: &str) -> Self {
        if flags.contains("EAP") {
            Security::Unsupported("802.1X")
        } else if flags.contains("WEP") {
            Security::Unsupported("WEP")
        } else if flags.contains("PSK") || flags.contains("SAE") {
            Security::Passphrase {
                psk: flags.contains("PSK"),
                sae: flags.contains("SAE"),
            }
        } else if flags.contains("OWE") {
            Security::Owe
        } else {
            Security::Open
        }
    }

    /// Whether joining asks for a passphrase.
    pub fn needs_passphrase(self) -> bool {
        matches!(self, Security::Passphrase { .. })
    }

    /// A short name, such as `WPA2`.
    pub fn label(self) -> &'static str {
        match self {
            Security::Open => "open",
            Security::Owe => "OWE",
            Security::Passphrase {
                psk: true,
                sae: true,
            } => "WPA2/WPA3",
            Security::Passphrase { sae: true, .. } => "WPA3",
            Security::Passphrase { .. } => "WPA2",
            Security::Unsupported(what) => what,
        }
    }

    /// The SET_NETWORK settings for this network beside its SSID.
    pub fn settings(self, passphrase: &str) -> io::Result<Vec<(&'static str, String)>> {
        match self {
            Security::Open => Ok(vec![("key_mgmt", "NONE".into())]),
            // OWE requires management frame protection by definition.
            Security::Owe => Ok(vec![("key_mgmt", "OWE".into()), ("ieee80211w", "2".into())]),
            Security::Passphrase { psk, sae } => {
                if passphrase.chars().any(char::is_control) {
                    return Err(io::Error::other(
                        "The passphrase cannot hold control characters.",
                    ));
                }
                let mut mgmt = Vec::new();
                let mut out = Vec::new();
                if psk {
                    // WPA2's passphrase is 8 to 63 printable ASCII characters;
                    // 64 hex digits are the raw key and go unquoted.
                    let raw =
                        passphrase.len() == 64 && passphrase.bytes().all(|b| b.is_ascii_hexdigit());
                    let ascii = passphrase.bytes().all(|b| (0x20..0x7f).contains(&b));
                    let quoted = ascii && (8..=63).contains(&passphrase.len());
                    if !(raw || quoted) {
                        return Err(io::Error::other(
                            "A WPA2 passphrase is 8 to 63 ASCII characters long.",
                        ));
                    }
                    mgmt.push("WPA-PSK WPA-PSK-SHA256");
                    out.push((
                        "psk",
                        if raw {
                            passphrase.to_owned()
                        } else {
                            format!("\"{passphrase}\"")
                        },
                    ));
                }
                if sae {
                    if passphrase.is_empty() {
                        return Err(io::Error::other("The passphrase is empty."));
                    }
                    mgmt.push("SAE");
                    // wpa_supplicant reads a quoted value up to its last quote,
                    // so a quote inside the passphrase survives.
                    out.push(("sae_password", format!("\"{passphrase}\"")));
                }
                // WPA3 alone requires protected management frames; a
                // transition network only offers them.
                let pmf = if sae && !psk { "2" } else { "1" };
                out.insert(0, ("key_mgmt", mgmt.join(" ")));
                out.push(("ieee80211w", pmf.into()));
                Ok(out)
            }
            Security::Unsupported(what) => Err(io::Error::other(format!(
                "{what} networks cannot be joined from here; use wpa_cli in a terminal."
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// A network seen in a scan.
pub struct Network {
    /// The SSID's raw bytes.
    pub ssid: Vec<u8>,
    /// dBm, the strongest of every access point broadcasting the SSID.
    pub signal: i32,
    /// How it asks to be joined.
    pub security: Security,
}

impl Network {
    /// The SSID as text.
    pub fn name(&self) -> String {
        String::from_utf8_lossy(&self.ssid).into_owned()
    }
}

/// SCAN_RESULTS: a header line, then one tab-separated line per access point
/// -- bssid, frequency, signal, flags, ssid. One network per SSID, strongest
/// first; hidden networks, which broadcast an empty SSID, are left out.
pub fn parse_scan_results(reply: &str) -> Vec<Network> {
    let mut networks: Vec<Network> = Vec::new();
    for line in reply.lines().skip(1) {
        let fields: Vec<&str> = line.splitn(5, '\t').collect();
        let [_, _, signal, flags, ssid] = fields[..] else {
            continue;
        };
        let Ok(signal) = signal.parse::<i32>() else {
            continue;
        };
        let ssid = decode_ssid(ssid);
        if ssid.iter().all(|&b| b == 0) {
            continue;
        }
        let network = Network {
            ssid,
            signal,
            security: Security::from_flags(flags),
        };
        match networks
            .iter_mut()
            .find(|n| n.ssid == network.ssid && n.security == network.security)
        {
            Some(known) if known.signal < network.signal => *known = network,
            Some(_) => {}
            None => networks.push(network),
        }
    }
    networks.sort_by(|a, b| b.signal.cmp(&a.signal).then_with(|| a.ssid.cmp(&b.ssid)));
    networks
}

/// Undoes wpa_supplicant's printf_encode(), which is how an SSID's raw bytes
/// reach a text protocol.
pub fn decode_ssid(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' || i + 1 == bytes.len() {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        i += 1;
        match bytes[i] {
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'e' => out.push(0x1b),
            b'x' => match bytes.get(i + 1..i + 3) {
                Some(&[hi, lo]) if hi.is_ascii_hexdigit() && lo.is_ascii_hexdigit() => {
                    out.push(hex_value(hi) << 4 | hex_value(lo));
                    i += 2;
                }
                _ => out.extend_from_slice(b"\\x"),
            },
            other => out.push(other),
        }
        i += 1;
    }
    out
}

fn hex_value(digit: u8) -> u8 {
    match digit {
        b'0'..=b'9' => digit - b'0',
        b'a'..=b'f' => digit - b'a' + 10,
        _ => digit - b'A' + 10,
    }
}

/// A network wpa_supplicant knows, from LIST_NETWORKS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    /// wpa_supplicant's id for it.
    pub id: u32,
    /// The SSID's raw bytes.
    pub ssid: Vec<u8>,
    /// Whether it is the network in use.
    pub current: bool,
}

impl Saved {
    /// The SSID as text.
    pub fn name(&self) -> String {
        String::from_utf8_lossy(&self.ssid).into_owned()
    }
}

/// LIST_NETWORKS: a header line, then one tab-separated line per network --
/// id, ssid, bssid, flags such as `[CURRENT]` or `[DISABLED]`.
pub fn parse_list_networks(reply: &str) -> Vec<Saved> {
    reply
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut fields = line.splitn(4, '\t');
            let id = fields.next()?.parse().ok()?;
            let ssid = decode_ssid(fields.next()?);
            let flags = fields.nth(1).unwrap_or_default();
            Some(Saved {
                id,
                ssid,
                current: flags.contains("[CURRENT]"),
            })
        })
        .collect()
}

/// STATUS's reply as key-value pairs.
pub fn parse_status(reply: &str) -> Vec<(String, String)> {
    reply
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

/// Wireless interfaces are the network devices with a `wireless` directory.
pub fn interfaces(sys_class_net: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(sys_class_net)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().join("wireless").is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_lookup() {
        let dir = std::env::temp_dir().join(format!("derisk-group-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("group");
        fs::write(&file, "root:x:0:\nwpa_supplicant:x:991:\nwheel:x:1:a\n").unwrap();
        assert_eq!(group_id(&file, "wpa_supplicant"), Some(991));
        assert_eq!(group_id(&file, "nobody"), None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn hex_ssid() {
        assert_eq!(hex(b"a \"b\""), "6120226222");
    }
}
