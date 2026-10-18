//! The network pieces the setup screens use, against nmcli's terse output.

use derisk::network::{self, Device, Network, Security, WiredState};

#[test]
fn terse_fields_split_at_unescaped_colons() {
    assert_eq!(network::fields("a:b\\:c:d\\\\e"), ["a", "b:c", "d\\e"]);
    assert_eq!(network::fields("::"), ["", "", ""]);
    assert_eq!(network::fields(""), [""]);
}

#[test]
fn wifi_list_keeps_the_strongest_of_each_ssid() {
    // IN-USE, SSID-HEX, SIGNAL, SECURITY: "home" twice, "cafe" open, a
    // hidden network with no SSID, "my net" offering WPA2 and WPA3, and a
    // line that is not one.
    let text = " :686F6D65:40:WPA2\n\
        *:686F6D65:82:WPA2\n\
         :63616665:60:\n\
         ::90:WPA2\n\
         :6D79206E6574:70:WPA2 WPA3\n\
        garbage\n";
    let (networks, in_use) = network::parse_wifi_list(text);
    let names: Vec<_> = networks.iter().map(|n| (n.name(), n.signal)).collect();
    assert_eq!(
        names,
        [
            ("home".to_string(), 82),
            ("my net".to_string(), 70),
            ("cafe".to_string(), 60)
        ]
    );
    assert_eq!(in_use.as_deref(), Some(&b"home"[..]));
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
fn ssids_are_read_as_hex() {
    assert_eq!(network::decode_hex("41FF00"), Some(vec![0x41, 0xff, 0]));
    assert_eq!(network::decode_hex("--"), None);
}

#[test]
fn security_from_networkmanager() {
    assert_eq!(
        Security::from_nm("WPA2 802.1X"),
        Security::Unsupported("802.1X")
    );
    assert_eq!(Security::from_nm("WEP"), Security::Unsupported("WEP"));
    assert_eq!(
        Security::from_nm("WPA3"),
        Security::Passphrase {
            psk: false,
            sae: true
        }
    );
    assert_eq!(
        Security::from_nm("WPA1 WPA2"),
        Security::Passphrase {
            psk: true,
            sae: false
        }
    );
    assert_eq!(Security::from_nm("OWE"), Security::Owe);
    assert_eq!(Security::from_nm("OWE-TM"), Security::Owe);
    assert_eq!(Security::from_nm(""), Security::Open);
}

#[test]
fn passphrase_settings() {
    let wpa2 = Security::Passphrase {
        psk: true,
        sae: false,
    };
    let (settings, secret) = wpa2.settings("correct horse").unwrap();
    assert_eq!(settings, [("wifi-sec.key-mgmt", "wpa-psk".to_string())]);
    assert_eq!(
        secret.as_deref(),
        Some("802-11-wireless-security.psk:correct horse")
    );
    assert!(wpa2.settings(&"0123456789abcdef".repeat(4)).is_ok());
    assert!(wpa2.settings("short").is_err());
    assert!(wpa2.settings(&"x".repeat(64)).is_err());
    assert!(wpa2.settings("pässword").is_err());

    let wpa3 = Security::Passphrase {
        psk: false,
        sae: true,
    };
    let (settings, secret) = wpa3.settings("pässwörd \"quoted\"").unwrap();
    assert_eq!(
        settings,
        [
            ("wifi-sec.key-mgmt", "sae".to_string()),
            ("wifi-sec.pmf", "required".to_string())
        ]
    );
    assert_eq!(
        secret.as_deref(),
        Some("802-11-wireless-security.psk:pässwörd \"quoted\"")
    );
    assert!(wpa3.settings("").is_err());
    // A line break would end nmcli's passwd-file line.
    assert!(wpa3.settings("line\nbreak").is_err());

    let transition = Security::Passphrase {
        psk: true,
        sae: true,
    };
    let (settings, _) = transition.settings("12345678").unwrap();
    assert_eq!(settings, [("wifi-sec.key-mgmt", "wpa-psk".to_string())]);

    assert_eq!(Security::Open.settings("").unwrap(), (Vec::new(), None));
    assert!(Security::Owe.settings("").unwrap().1.is_none());
    assert!(
        Security::Unsupported("802.1X")
            .settings("anything")
            .is_err()
    );
}

#[test]
fn online_state() {
    assert!(network::parse_online("connected\n"));
    assert!(!network::parse_online("connected (site only)\n"));
    assert!(!network::parse_online("connecting\n"));
    assert!(!network::parse_online("disconnected\n"));
    assert!(!network::parse_online(""));
}

#[test]
fn wired_ports_and_their_state() {
    let devices = network::parse_devices(
        "wlan0:wifi:disconnected\n\
         eth1:ethernet:unavailable\n\
         eth0:ethernet:connected\n\
         enp3s0:ethernet:connecting (getting IP configuration)\n\
         virbr0:bridge:unmanaged\n\
         vnet0:ethernet:unmanaged\n\
         adblock0:dummy:connected\n\
         lo:loopback:connected (externally)\n",
    );
    let ports: Vec<_> = network::wired(&devices)
        .into_iter()
        .map(|w| (w.name, w.state))
        .collect();
    assert_eq!(
        ports,
        [
            ("enp3s0".to_string(), WiredState::Configuring),
            ("eth0".to_string(), WiredState::Online),
            ("eth1".to_string(), WiredState::NoCable),
        ]
    );
    assert_eq!(
        network::wifi_device(&devices),
        Some(&Device {
            name: "wlan0".into(),
            kind: "wifi".into(),
            state: "disconnected".into()
        })
    );
    assert!(network::wifi_ready("disconnected"));
    assert!(!network::wifi_ready("unavailable"));
}

#[test]
fn saved_connections_and_new_ones() {
    let saved = network::parse_connections(
        "0b1c2d3e-0000-4000-8000-000000000001:802-11-wireless:yes\n\
         0b1c2d3e-0000-4000-8000-000000000002:802-3-ethernet:yes\n\
         0b1c2d3e-0000-4000-8000-000000000003:802-11-wireless:no\n",
    );
    assert_eq!(
        saved,
        [
            ("0b1c2d3e-0000-4000-8000-000000000001".to_string(), true),
            ("0b1c2d3e-0000-4000-8000-000000000003".to_string(), false)
        ]
    );
    assert_eq!(
        network::parse_added(
            "Connection 'my (home) net' (0b1c2d3e-0000-4000-8000-000000000004) successfully added.\n"
        )
        .as_deref(),
        Some("0b1c2d3e-0000-4000-8000-000000000004")
    );

    let net = Network {
        ssid: b"home".to_vec(),
        signal: 80,
        security: Security::Passphrase {
            psk: true,
            sae: false,
        },
    };
    let (settings, _) = net.security.settings("correct horse").unwrap();
    let args = network::add_args(&net, "wlan0", &settings, false);
    assert_eq!(
        args,
        [
            "connection",
            "add",
            "type",
            "wifi",
            "ifname",
            "wlan0",
            "con-name",
            "home",
            "ssid",
            "home",
            "save",
            "no",
            "wifi-sec.key-mgmt",
            "wpa-psk"
        ]
    );
    // The passphrase is never on the command line.
    assert!(!args.iter().any(|a| a.contains("correct horse")));
}

#[test]
fn polkit_refusals_are_permission_errors() {
    let error = network::nmcli_error(
        "Error: Connection activation failed: Not authorized to control networking.\n",
    );
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    let error = network::nmcli_error(
        "Error: Connection activation failed: Secrets were required, but not provided.\n",
    );
    assert_eq!(error.kind(), std::io::ErrorKind::Other);
    assert_eq!(
        error.to_string(),
        "Connection activation failed: Secrets were required, but not provided."
    );
}
