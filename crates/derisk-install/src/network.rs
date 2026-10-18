//! The network, as the setup screens need it: whether the machine is online,
//! what each wired port is doing, and Wi-Fi, all through NetworkManager.
//!
//! NetworkManager runs DHCP on every wired port with a cable and on the
//! Wi-Fi network it joins, so joining one is all there is to do here; a
//! cable plugged in gets the machine online with nothing asked. It is spoken
//! to through `nmcli` in terse mode (`-t`), whose fields are separated by
//! colons, with a colon or backslash inside a value escaped by a backslash:
//! nmcli is NetworkManager's own client, does the D-Bus and polkit work, and
//! keeps a passphrase out of any command line by reading it from a pipe.
//!
//! wpa_supplicant's control socket and networkd's state files were spoken to
//! directly before, and went with networkd: on NetworkManager, wpa_supplicant
//! has no control socket and networkd does not run.

use std::{
    io::{self, Write},
    process::{Child, Command, Stdio},
};

/// NetworkManager's client, from the system profile.
pub const NMCLI: &str = "nmcli";

/// Runs `nmcli -t` with `args` and returns what it printed. Its own error
/// line, without the `Error: ` in front, is the error; a polkit refusal is
/// [`io::ErrorKind::PermissionDenied`].
pub fn nmcli(args: &[&str]) -> io::Result<String> {
    let out = Command::new(NMCLI)
        .arg("-t")
        .args(args)
        .stdin(Stdio::null())
        .output()?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(nmcli_error(&String::from_utf8_lossy(&out.stderr)))
    }
}

/// An error from nmcli's stderr.
pub fn nmcli_error(stderr: &str) -> io::Error {
    let line = stderr
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("nmcli failed")
        .trim();
    let line = line.strip_prefix("Error: ").unwrap_or(line);
    let lower = line.to_lowercase();
    if lower.contains("not authorized") || lower.contains("insufficient privileges") {
        io::Error::new(io::ErrorKind::PermissionDenied, line.to_owned())
    } else {
        io::Error::other(line.to_owned())
    }
}

/// One line of terse output, split at the colons that are not escaped, with
/// the escapes undone.
pub fn fields(line: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    out.last_mut().expect("never empty").push(next);
                }
            }
            ':' => out.push(String::new()),
            c => out.last_mut().expect("never empty").push(c),
        }
    }
    out
}

/// Whether `nmcli -t -f STATE general` says the machine reaches beyond its
/// own networks: `connected`, and not `connected (site only)` or
/// `(local only)`.
pub fn parse_online(general: &str) -> bool {
    general.lines().next().map(str::trim) == Some("connected")
}

/// A network device, from `nmcli -t -f DEVICE,TYPE,STATE device`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// The interface name.
    pub name: String,
    /// NetworkManager's type for it, such as `ethernet` or `wifi`.
    pub kind: String,
    /// Its state, such as `connected`, `unavailable` or `connecting
    /// (getting IP configuration)`.
    pub state: String,
}

/// `nmcli -t -f DEVICE,TYPE,STATE device`, one device a line.
pub fn parse_devices(text: &str) -> Vec<Device> {
    text.lines()
        .filter_map(|line| {
            let [name, kind, state]: [String; 3] = fields(line).try_into().ok()?;
            Some(Device { name, kind, state })
        })
        .collect()
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

/// The wired ports among `devices`: NetworkManager's `ethernet` ones that it
/// manages, which leaves out loopback, bridges, veths and the machines'
/// ports, which it is told to leave alone. `unavailable` is NetworkManager's
/// word for a port with no carrier.
pub fn wired(devices: &[Device]) -> Vec<Wired> {
    let mut ports: Vec<Wired> = devices
        .iter()
        .filter(|d| d.kind == "ethernet" && d.state != "unmanaged")
        .map(|d| Wired {
            name: d.name.clone(),
            state: if d.state == "unavailable" {
                WiredState::NoCable
            } else if d.state.starts_with("connected") {
                WiredState::Online
            } else {
                WiredState::Configuring
            },
        })
        .collect();
    ports.sort_by(|a, b| a.name.cmp(&b.name));
    ports
}

/// The first Wi-Fi device among `devices`, by name.
pub fn wifi_device(devices: &[Device]) -> Option<&Device> {
    devices
        .iter()
        .filter(|d| d.kind == "wifi")
        .min_by(|a, b| a.name.cmp(&b.name))
}

/// Whether a Wi-Fi device in `state` can scan and join: not while its radio
/// is off or wpa_supplicant is not up yet (`unavailable`), nor when
/// NetworkManager does not manage it.
pub fn wifi_ready(state: &str) -> bool {
    !matches!(state, "unavailable" | "unmanaged")
}

/// `nmcli connection add`'s property-value pairs.
pub type Settings = Vec<(&'static str, String)>;

/// How a network asks to be joined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    /// No encryption.
    Open,
    /// Opportunistic Wireless Encryption: encrypted, and no passphrase.
    Owe,
    /// WPA2-Personal, WPA3-Personal, or a transition network offering both.
    Passphrase {
        /// WPA2-Personal (or the first WPA).
        psk: bool,
        /// WPA3-Personal.
        sae: bool,
    },
    /// 802.1X and WEP. The first needs more than a passphrase to describe,
    /// and the second is not worth describing; `nmcli` on another terminal
    /// can still join either.
    Unsupported(&'static str),
}

impl Security {
    /// Reads `nmcli device wifi list`'s SECURITY, such as `WPA2`, `WPA1
    /// WPA2`, `WPA2 WPA3`, `WPA3 802.1X`, `OWE` or nothing for an open
    /// network.
    pub fn from_nm(security: &str) -> Self {
        let words: Vec<&str> = security.split_whitespace().collect();
        let has = |word: &str| words.contains(&word);
        if has("802.1X") {
            Security::Unsupported("802.1X")
        } else if has("WEP") {
            Security::Unsupported("WEP")
        } else if has("WPA1") || has("WPA2") || has("WPA3") {
            Security::Passphrase {
                psk: has("WPA1") || has("WPA2"),
                sae: has("WPA3"),
            }
        } else if words.iter().any(|w| w.starts_with("OWE")) {
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

    /// The `nmcli connection add` settings for this network beside its SSID,
    /// and the secret to hand over when it is brought up, as a line of
    /// nmcli's `passwd-file`.
    pub fn settings(self, passphrase: &str) -> io::Result<(Settings, Option<String>)> {
        match self {
            Security::Open => Ok((Vec::new(), None)),
            // OWE requires management frame protection by definition.
            Security::Owe => Ok((
                vec![
                    ("wifi-sec.key-mgmt", "owe".into()),
                    ("wifi-sec.pmf", "required".into()),
                ],
                None,
            )),
            Security::Passphrase { psk, .. } => {
                if passphrase.chars().any(char::is_control) {
                    return Err(io::Error::other(
                        "The passphrase cannot hold control characters.",
                    ));
                }
                if psk {
                    // WPA2's passphrase is 8 to 63 printable ASCII characters,
                    // or 64 hex digits that are the raw key.
                    let raw =
                        passphrase.len() == 64 && passphrase.bytes().all(|b| b.is_ascii_hexdigit());
                    let ascii = passphrase.bytes().all(|b| (0x20..0x7f).contains(&b));
                    let quoted = ascii && (8..=63).contains(&passphrase.len());
                    if !(raw || quoted) {
                        return Err(io::Error::other(
                            "A WPA2 passphrase is 8 to 63 ASCII characters long.",
                        ));
                    }
                } else if passphrase.is_empty() {
                    return Err(io::Error::other("The passphrase is empty."));
                }
                // A transition network is joined as WPA2, which every card
                // can do; WPA3 alone requires protected management frames.
                let settings = if psk {
                    vec![("wifi-sec.key-mgmt", "wpa-psk".into())]
                } else {
                    vec![
                        ("wifi-sec.key-mgmt", "sae".into()),
                        ("wifi-sec.pmf", "required".into()),
                    ]
                };
                // NetworkManager keeps WPA3's passphrase in `psk` too.
                Ok((
                    settings,
                    Some(format!("802-11-wireless-security.psk:{passphrase}")),
                ))
            }
            Security::Unsupported(what) => Err(io::Error::other(format!(
                "{what} networks cannot be joined from here; use nmcli in a terminal."
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// A network seen in a scan.
pub struct Network {
    /// The SSID's raw bytes.
    pub ssid: Vec<u8>,
    /// From 0 to 100, as NetworkManager reports it: the strongest of every
    /// access point broadcasting the SSID.
    pub signal: u8,
    /// How it asks to be joined.
    pub security: Security,
}

impl Network {
    /// The SSID as text.
    pub fn name(&self) -> String {
        String::from_utf8_lossy(&self.ssid).into_owned()
    }
}

/// `nmcli -t -f IN-USE,SSID-HEX,SIGNAL,SECURITY device wifi list`, one access
/// point a line: one network per SSID, strongest first, and the SSID of the
/// one in use, if any. Hidden networks, which broadcast no SSID, are left
/// out.
pub fn parse_wifi_list(text: &str) -> (Vec<Network>, Option<Vec<u8>>) {
    let mut networks: Vec<Network> = Vec::new();
    let mut in_use = None;
    for line in text.lines() {
        let Ok([used, ssid, signal, security]) = <[String; 4]>::try_from(fields(line)) else {
            continue;
        };
        let (Some(ssid), Ok(signal)) = (decode_hex(&ssid), signal.parse::<u8>()) else {
            continue;
        };
        if ssid.iter().all(|&b| b == 0) {
            continue;
        }
        if used.trim() == "*" {
            in_use = Some(ssid.clone());
        }
        let network = Network {
            ssid,
            signal,
            security: Security::from_nm(&security),
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
    (networks, in_use)
}

/// SSID-HEX's bytes; nothing for an empty or malformed one.
pub fn decode_hex(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if text.is_empty() || !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

/// A saved Wi-Fi connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    /// NetworkManager's UUID for it.
    pub id: String,
    /// The SSID's bytes, as nmcli prints it.
    pub ssid: Vec<u8>,
    /// Whether it is the connection in use.
    pub current: bool,
}

impl Saved {
    /// The SSID as text.
    pub fn name(&self) -> String {
        String::from_utf8_lossy(&self.ssid).into_owned()
    }
}

/// `nmcli -t -f UUID,TYPE,ACTIVE connection show`: the UUIDs of the Wi-Fi
/// connections, and whether each is active.
pub fn parse_connections(text: &str) -> Vec<(String, bool)> {
    text.lines()
        .filter_map(|line| {
            let [uuid, kind, active]: [String; 3] = fields(line).try_into().ok()?;
            (kind == "802-11-wireless").then_some((uuid, active == "yes"))
        })
        .collect()
}

/// The UUID in `nmcli connection add`'s "Connection 'name' (uuid)
/// successfully added."
pub fn parse_added(text: &str) -> Option<String> {
    let start = text.rfind('(')? + 1;
    let end = start + text[start..].find(')')?;
    Some(text[start..end].to_owned())
}

/// The arguments of `nmcli connection add` for `network` on `interface`. A
/// connection added with `save` false lives only until NetworkManager
/// restarts, as on the live installer, which has nowhere to keep it.
pub fn add_args(
    network: &Network,
    interface: &str,
    settings: &Settings,
    save: bool,
) -> Vec<String> {
    let name = network.name();
    let mut args: Vec<String> = [
        "connection",
        "add",
        "type",
        "wifi",
        "ifname",
        interface,
        "con-name",
        &name,
        "ssid",
        &name,
        "save",
        if save { "yes" } else { "no" },
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    for (key, value) in settings {
        args.push((*key).to_owned());
        args.push(value.clone());
    }
    args
}

/// How long `nmcli connection up` waits for association, the handshake and
/// DHCP. A wrong passphrase looks like a handshake that never completes, so
/// this is also how long it takes to say so.
pub const CONNECT_SECONDS: u32 = 30;

/// Brings up the connection `uuid` in the background, handing `secret` (a
/// `passwd-file` line) through a pipe. The child's exit says whether the
/// join worked.
pub fn bring_up(uuid: &str, secret: Option<&str>) -> io::Result<Child> {
    let wait = CONNECT_SECONDS.to_string();
    let mut command = Command::new(NMCLI);
    command.args(["--wait", &wait, "connection", "up", "uuid", uuid]);
    if secret.is_some() {
        command.args(["passwd-file", "/dev/stdin"]);
    }
    let mut child = command
        .stdin(if secret.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    if let (Some(secret), Some(mut stdin)) = (secret, child.stdin.take()) {
        stdin.write_all(secret.as_bytes())?;
        stdin.write_all(b"\n")?;
    }
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_ssid() {
        assert_eq!(decode_hex("6120226222"), Some(b"a \"b\"".to_vec()));
        assert_eq!(decode_hex(""), None);
        assert_eq!(decode_hex("6"), None);
        assert_eq!(decode_hex("zz"), None);
    }
}
