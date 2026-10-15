//! The network pieces the setup screens use, against fixture trees and a
//! fake wpa_supplicant.

use std::fs;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::thread;

use derisk::network::{self, Control, Network, Security};

/// A scratch directory per test, removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("derisk-network-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn write(&self, path: &str, contents: &str) {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn scan_results_keep_the_strongest_of_each_ssid() {
    let reply = "bssid / frequency / signal level / flags / ssid\n\
        aa:aa:aa:aa:aa:01\t2412\t-70\t[WPA2-PSK-CCMP][ESS]\thome\n\
        aa:aa:aa:aa:aa:02\t5180\t-48\t[WPA2-PSK-CCMP][ESS]\thome\n\
        aa:aa:aa:aa:aa:03\t2437\t-60\t[ESS]\tcafe\n\
        aa:aa:aa:aa:aa:04\t2437\t-30\t[WPA2-PSK-CCMP][ESS]\t\n\
        aa:aa:aa:aa:aa:05\t2437\t-55\t[WPA2-PSK+SAE-CCMP][ESS]\tmy\\x20net\n\
        garbage\n";
    let networks = network::parse_scan_results(reply);
    let names: Vec<_> = networks.iter().map(|n| (n.name(), n.signal)).collect();
    assert_eq!(
        names,
        [
            ("home".to_string(), -48),
            ("my net".to_string(), -55),
            ("cafe".to_string(), -60)
        ]
    );
    assert_eq!(
        networks[1].security,
        Security::Passphrase {
            psk: true,
            sae: true
        }
    );
    assert_eq!(networks[2].security, Security::Open);
}

#[test]
fn ssids_decode_like_printf_encode() {
    assert_eq!(network::decode_ssid(r#"a\"b\\c\x41\xff"#), b"a\"b\\cA\xff");
    assert_eq!(network::decode_ssid(r"tab\there\n"), b"tab\there\n");
    // A truncated escape is kept as it was rather than dropped.
    assert_eq!(network::decode_ssid(r"end\x4"), b"end\\x4");
    assert_eq!(network::decode_ssid("trailing\\"), b"trailing\\");
}

#[test]
fn security_from_flags() {
    assert_eq!(
        Security::from_flags("[WPA2-EAP-CCMP][ESS]"),
        Security::Unsupported("802.1X")
    );
    assert_eq!(
        Security::from_flags("[WEP][ESS]"),
        Security::Unsupported("WEP")
    );
    assert_eq!(
        Security::from_flags("[RSN-SAE-CCMP][ESS]"),
        Security::Passphrase {
            psk: false,
            sae: true
        }
    );
    assert_eq!(Security::from_flags("[RSN-OWE-CCMP][ESS]"), Security::Owe);
    assert_eq!(Security::from_flags("[ESS]"), Security::Open);
}

#[test]
fn passphrase_settings() {
    let wpa2 = Security::Passphrase {
        psk: true,
        sae: false,
    };
    assert_eq!(
        wpa2.settings("correct horse").unwrap(),
        [
            ("key_mgmt", "WPA-PSK WPA-PSK-SHA256".to_string()),
            ("psk", "\"correct horse\"".to_string()),
            ("ieee80211w", "1".to_string()),
        ]
    );
    let raw = "0123456789abcdef".repeat(4);
    assert_eq!(wpa2.settings(&raw).unwrap()[1], ("psk", raw.clone()));
    assert!(wpa2.settings("short").is_err());
    assert!(wpa2.settings(&"x".repeat(64)).is_err());
    assert!(wpa2.settings("pässword").is_err());

    let wpa3 = Security::Passphrase {
        psk: false,
        sae: true,
    };
    let settings = wpa3.settings("pässwörd \"quoted\"").unwrap();
    assert_eq!(settings[0], ("key_mgmt", "SAE".to_string()));
    assert_eq!(
        settings[1],
        ("sae_password", "\"pässwörd \"quoted\"\"".to_string())
    );
    assert_eq!(settings[2], ("ieee80211w", "2".to_string()));
    assert!(wpa3.settings("").is_err());
    assert!(wpa3.settings("line\nbreak").is_err());

    let transition = Security::Passphrase {
        psk: true,
        sae: true,
    };
    let settings = transition.settings("12345678").unwrap();
    assert_eq!(
        settings[0],
        ("key_mgmt", "WPA-PSK WPA-PSK-SHA256 SAE".to_string())
    );
    assert_eq!(settings.last().unwrap(), &("ieee80211w", "1".to_string()));

    assert!(
        Security::Unsupported("802.1X")
            .settings("anything")
            .is_err()
    );
}

#[test]
fn status_and_online_state() {
    let status = network::parse_status("bssid=aa:aa:aa:aa:aa:01\nssid=home\nwpa_state=COMPLETED\n");
    assert!(status.contains(&("wpa_state".to_string(), "COMPLETED".to_string())));
    assert!(network::parse_online(
        "OPER_STATE=routable\nCARRIER_STATE=carrier\n"
    ));
    assert!(!network::parse_online("OPER_STATE=degraded\n"));
    assert!(!network::online(Path::new("/nonexistent/state")));
}

#[test]
fn wireless_interfaces_have_a_wireless_directory() {
    let sys = Scratch::new("net");
    fs::create_dir_all(sys.0.join("wlp2s0/wireless")).unwrap();
    fs::create_dir_all(sys.0.join("enp1s0")).unwrap();
    fs::create_dir_all(sys.0.join("lo")).unwrap();
    assert_eq!(network::interfaces(&sys.0), ["wlp2s0"]);
}

#[test]
fn wired_ports_and_their_state() {
    let root = Scratch::new("wired");
    let port = |name: &str, kind: &str, index: &str, carrier: Option<&str>| {
        root.write(&format!("sys/{name}/type"), &format!("{kind}\n"));
        root.write(&format!("sys/{name}/ifindex"), &format!("{index}\n"));
        fs::create_dir_all(root.0.join(format!("sys/{name}/device"))).unwrap();
        if let Some(carrier) = carrier {
            root.write(&format!("sys/{name}/carrier"), &format!("{carrier}\n"));
        }
    };
    // Online, cable without an address yet, cable out, and a link that is
    // down (no readable carrier).
    port("enp1s0", "1", "2", Some("1"));
    port("enp2s0", "1", "3", Some("1"));
    port("enp3s0", "1", "4", Some("0"));
    port("enx001122334455", "1", "5", None);
    // Not wired ports: Wi-Fi, loopback, and a bridge with no device.
    port("wlp4s0", "1", "6", Some("1"));
    fs::create_dir_all(root.0.join("sys/wlp4s0/wireless")).unwrap();
    port("lo", "772", "1", Some("1"));
    root.write("sys/br0/type", "1\n");
    root.write("sys/br0/carrier", "1\n");
    root.write("links/2", "ADMIN_STATE=configured\nOPER_STATE=routable\n");
    root.write("links/3", "ADMIN_STATE=configuring\nOPER_STATE=carrier\n");
    root.write("links/6", "OPER_STATE=routable\n");

    let ports = network::wired(&root.0.join("sys"), &root.0.join("links"));
    let seen: Vec<(&str, network::WiredState)> =
        ports.iter().map(|w| (w.name.as_str(), w.state)).collect();
    assert_eq!(
        seen,
        [
            ("enp1s0", network::WiredState::Online),
            ("enp2s0", network::WiredState::Configuring),
            ("enp3s0", network::WiredState::NoCable),
            ("enx001122334455", network::WiredState::NoCable),
        ]
    );
    assert!(network::wired(Path::new("/nonexistent"), Path::new("/nonexistent")).is_empty());
}

#[test]
fn control_socket_round_trip() {
    let dir = Scratch::new("ctrl");
    let server_path = dir.0.join("wlan0");
    let server = UnixDatagram::bind(&server_path).unwrap();
    let daemon = thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut seen = Vec::new();
        loop {
            let (n, from) = server.recv_from(&mut buf).unwrap();
            let command = String::from_utf8_lossy(&buf[..n]).into_owned();
            let from = from.as_pathname().unwrap().to_path_buf();
            let reply = match command.as_str() {
                "ADD_NETWORK" => "3\n",
                "SELECT_NETWORK 3" => {
                    // An unsolicited event first, which the client must skip.
                    server
                        .send_to(b"<3>CTRL-EVENT-SCAN-STARTED", &from)
                        .unwrap();
                    "OK\n"
                }
                c if c.starts_with("SET_NETWORK 3 ") => "OK\n",
                "QUIT" => break,
                _ => "FAIL\n",
            };
            seen.push(command);
            server.send_to(reply.as_bytes(), &from).unwrap();
        }
        seen
    });

    let client_dir = dir.0.join("client");
    fs::create_dir_all(&client_dir).unwrap();
    let control = Control::open_at(&server_path, &client_dir).unwrap();
    let network = Network {
        ssid: b"home".to_vec(),
        signal: -50,
        security: Security::Passphrase {
            psk: true,
            sae: false,
        },
    };
    assert_eq!(control.connect(&network, "correct horse").unwrap(), 3);
    assert!(control.forget(3).is_err());
    // The fake daemon stops answering at QUIT, so this one times out.
    control.request("QUIT").unwrap_err();
    let seen = daemon.join().unwrap();
    assert_eq!(
        seen,
        [
            "ADD_NETWORK",
            "SET_NETWORK 3 ssid 686f6d65",
            "SET_NETWORK 3 key_mgmt WPA-PSK WPA-PSK-SHA256",
            "SET_NETWORK 3 psk \"correct horse\"",
            "SET_NETWORK 3 ieee80211w 1",
            "SELECT_NETWORK 3",
            "REMOVE_NETWORK 3",
        ]
    );
    drop(control);
    assert_eq!(fs::read_dir(&client_dir).unwrap().count(), 0);
}

#[test]
fn saved_networks_list_with_their_ids() {
    let reply = "network id / ssid / bssid / flags\n\
        0\thome\tany\t[CURRENT]\n\
        1\tmy\\x20net\tany\t[DISABLED]\n\
        2\tcafe\tany\t\n\
        garbage\n";
    let saved = network::parse_list_networks(reply);
    let seen: Vec<_> = saved.iter().map(|s| (s.id, s.name(), s.current)).collect();
    assert_eq!(
        seen,
        [
            (0, "home".to_string(), true),
            (1, "my net".to_string(), false),
            (2, "cafe".to_string(), false)
        ]
    );
}
