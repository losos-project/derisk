use std::path::PathBuf;

use derisk_settings::choice::{Policy, SearchEngine};
use derisk_settings::{
    Accent, ColorScheme, Layout, LowPower, Page, Profile, Settings, SettingsApp, Vrr,
};
use mcsapi_ui::{Theme, egui, run_frame};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("derisk-settings-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn text_round_trips_every_field() {
    let mut settings = Settings::default();
    settings.appearance.scheme = ColorScheme::Light;
    settings.appearance.accent = Accent::Rose;
    settings.appearance.text_scale = 1.25;
    settings.appearance.reduce_motion = true;
    settings.desktop.layout = Layout::Monocle;
    settings.desktop.gaps = 0;
    settings.desktop.workspaces = 4;
    settings.desktop.profile = Profile::Tablet;
    settings.desktop.vrr = Vrr::Off;
    settings.input.natural_scroll = false;
    settings.input.repeat_delay_ms = 250;
    settings.input.repeat_rate = 40;
    settings.notifications.do_not_disturb = true;
    settings.power.suspend_after_min = 0;
    settings.appearance.blur = 0;
    settings.appearance.panels.top_bar = 100;
    settings.appearance.panels.overview = 35;
    settings.appearance.panels.snap_assist = 20;
    settings.power.low_power = LowPower::On;
    settings.privacy.usage_ping = false;
    settings.privacy_display.enabled = true;
    settings.privacy_display.notifications = false;
    settings.privacy_display.maximum = true;
    settings.privacy_display.apps = vec!["org.derisk.files".into(), "org.mozilla.firefox".into()];
    settings.defaults.browser = "org.mozilla.firefox.desktop".into();
    settings.defaults.search = Some(SearchEngine::Ecosia);
    let (parsed, warnings) = Settings::parse(&settings.to_text());
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(parsed, settings);
}

#[test]
fn invalid_lines_warn_and_keep_defaults() {
    let text = "\
        # comment\n\
        desktop.workspaces = 12\n\
        desktop.gaps = 16 # trailing comment\n\
        appearance.accent = plaid\n\
        no equals sign\n\
        unknown.key = 1\n\
        input.tap_to_click = false\n";
    let (settings, warnings) = Settings::parse(text);
    assert_eq!(settings.desktop.workspaces, 9);
    assert_eq!(settings.desktop.gaps, 16);
    assert_eq!(settings.appearance.accent, Accent::Teal);
    assert!(!settings.input.tap_to_click);
    let lines: Vec<usize> = warnings.iter().map(|w| w.line).collect();
    assert_eq!(lines, [2, 4, 5, 6]);
}

#[test]
fn set_rejects_out_of_range_values() {
    let mut settings = Settings::default();
    assert!(!settings.set("appearance.text_scale", "3"));
    assert!(!settings.set("input.repeat_rate", "0"));
    assert!(!settings.set("power.dim_after_min", "-1"));
    assert!(!settings.set("appearance.blur", "11"));
    assert!(!settings.set("appearance.top_bar_opacity", "10"));
    assert!(!settings.set("power.low_power", "sometimes"));
    assert!(!settings.set("desktop.vrr", "maybe"));
    assert_eq!(settings, Settings::default());
    assert!(settings.set("power.dim_after_min", "0"));
    assert_eq!(settings.power.dim_after_min, 0);
}

#[test]
fn save_and_load_through_a_file() {
    let dir = temp_dir("file");
    let path = dir.join("nested/settings.conf");
    assert_eq!(
        Settings::load(&path).unwrap(),
        (Settings::default(), vec![])
    );
    let mut settings = Settings::default();
    settings.desktop.layout = Layout::Monocle;
    settings.save(&path).unwrap();
    assert_eq!(Settings::load(&path).unwrap().0, settings);
    assert!(!dir.join("nested/settings.conf.tmp").exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn theme_follows_scheme_and_accent() {
    let mut settings = Settings::default();
    assert_eq!(settings.theme(), Theme::default());
    settings.appearance.accent = Accent::Sky;
    assert_eq!(settings.theme().accent, Accent::Sky.color());
    settings.appearance.scheme = ColorScheme::Light;
    let light = settings.theme();
    assert_ne!(light.background, Theme::default().background);
    assert_ne!(light.foreground, light.background);
}

#[test]
fn app_tracks_unsaved_changes() {
    let dir = temp_dir("app");
    let path = dir.join("settings.conf");
    let mut app = SettingsApp::open(Some(path.clone()));
    assert!(!app.is_dirty());
    app.settings.notifications.sounds = false;
    assert!(app.is_dirty());
    app.revert();
    assert!(app.settings.notifications.sounds);
    app.settings.desktop.gaps = 4;
    app.save();
    assert!(!app.is_dirty());
    assert_eq!(app.status(), Some("Saved"));
    assert_eq!(Settings::load(&path).unwrap().0.desktop.gaps, 4);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn app_reports_invalid_files() {
    let dir = temp_dir("invalid");
    let path = dir.join("settings.conf");
    std::fs::write(&path, "desktop.gaps = lots\n").unwrap();
    let app = SettingsApp::open(Some(path));
    assert!(app.status().unwrap().contains("line 1"));
    assert!(app.error().unwrap().is_warning());
    assert!(app.error().unwrap().doc().is_some());
    let mut memory_only = SettingsApp::open(None);
    memory_only.settings.desktop.gaps = 2;
    memory_only.save();
    assert!(memory_only.is_dirty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn every_page_renders() {
    let context = egui::Context::default();
    let mut app = SettingsApp::open(None);
    for page in Page::ALL {
        app.page = page;
        let mut output = run_frame(
            &mut app,
            &context,
            egui::RawInput::default(),
            &Theme::default(),
        );
        assert!(!output.shapes.is_empty());
        output.textures_delta.clear();
    }
}

#[test]
fn low_power_follows_the_battery_only_when_asked() {
    assert_eq!(Settings::default().power.low_power, LowPower::OnBattery);
    for on_battery in [false, true] {
        assert!(!LowPower::Off.active(on_battery));
        assert!(LowPower::On.active(on_battery));
        assert_eq!(LowPower::OnBattery.active(on_battery), on_battery);
    }
    let (settings, warnings) = Settings::parse("power.low_power = on_battery\n");
    assert!(warnings.is_empty());
    assert_eq!(settings.power.low_power, LowPower::OnBattery);
}

#[test]
fn customization_round_trips() {
    use derisk_settings::{BarPosition, Fit, Keys, Rgb, Shortcut, WallpaperKind};
    let mut settings = Settings::default();
    settings.top_bar.position = BarPosition::Bottom;
    settings.top_bar.autohide = true;
    settings.top_bar.search = false;
    settings.top_bar.clock_24h = false;
    settings.windows.corner_radius = 0;
    settings.windows.shadows = false;
    settings.wallpaper.kind = WallpaperKind::Video;
    // `#` inside a path or a color is not a comment.
    settings.wallpaper.path = PathBuf::from("/home/me/Videos/loop #2.webm");
    settings.wallpaper.fit = Fit::Fit;
    settings.wallpaper.color = Rgb(0x12, 0xab, 0xef);
    settings.wallpaper.interval_min = 5;
    settings.wallpaper.pause_in_low_power = false;
    settings.wallpaper.lock_screen = PathBuf::from("/home/me/Pictures/lake.jpg");
    settings
        .shortcuts
        .set(Shortcut::Palette, Keys::parse("Ctrl+Alt+P"));
    settings
        .shortcuts
        .set(Shortcut::SnapLeft, Keys::parse("Ctrl+K Left"));
    settings.shortcuts.set(Shortcut::Close, None);
    let text = settings.to_text();
    assert!(text.contains("wallpaper.color = #12abef\n"), "{text}");
    assert!(text.contains("shortcut.palette = Ctrl+Alt+P\n"), "{text}");
    assert!(text.contains("shortcut.close = none\n"), "{text}");
    assert!(
        text.contains("shortcut.snap_left = Ctrl+K Left\n"),
        "{text}"
    );
    let (parsed, warnings) = Settings::parse(&text);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(parsed, settings);
}

#[test]
fn wallpaper_paths_must_be_absolute() {
    let mut settings = Settings::default();
    assert!(!settings.set("wallpaper.path", "Pictures/a.png"));
    assert!(settings.set("wallpaper.path", "/a.png"));
    assert!(settings.set("wallpaper.path", ""));
    assert!(!settings.set("wallpaper.lock_screen", "lake.jpg"));
    assert!(settings.set("wallpaper.lock_screen", "/lake.jpg"));
    assert!(settings.set("wallpaper.lock_screen", ""));
    assert!(!settings.set("wallpaper.color", "red"));
    assert!(!settings.set("wallpaper.color", "#12345"));
    assert!(!settings.set("windows.corner_radius", "21"));
    let (parsed, _) = Settings::parse("wallpaper.color = #102030 # a comment\n");
    assert_eq!(
        parsed.wallpaper.color,
        derisk_settings::Rgb(0x10, 0x20, 0x30)
    );
}

#[test]
fn chords_parse_and_clash() {
    use derisk_settings::{Chord, KeyName, Keys, Shortcut, Shortcuts};
    let chord = Chord::parse("shift + SUPER + m").unwrap();
    assert!(chord.logo && chord.shift && !chord.ctrl);
    assert_eq!(chord.key, KeyName::Letter('m'));
    assert_eq!(chord.to_string(), "Super+Shift+M");
    // A plain key would steal typing from apps.
    assert_eq!(Chord::parse("Shift+M"), None);
    assert_eq!(Chord::parse("Super+Super+M"), None);
    assert_eq!(Chord::parse("Super+F13"), None);
    let mut shortcuts = Shortcuts::default();
    assert!(shortcuts.clashes().is_empty());
    shortcuts.set(Shortcut::Close, Keys::parse("Super+F"));
    assert_eq!(shortcuts.clashes(), [Shortcut::Close, Shortcut::Float]);
    // The first in the list keeps the chord until the clash is fixed.
    assert_eq!(
        shortcuts.lookup(Chord::parse("Super+F").unwrap()),
        Some(Shortcut::Close)
    );
}

#[test]
fn the_wallpaper_page_renders_every_kind() {
    let context = egui::Context::default();
    let mut app = SettingsApp::open(None);
    app.page = Page::Wallpaper;
    for kind in derisk_settings::WallpaperKind::ALL {
        app.settings.wallpaper.kind = kind;
        let mut output = run_frame(
            &mut app,
            &context,
            egui::RawInput::default(),
            &Theme::default(),
        );
        assert!(!output.shapes.is_empty());
        output.textures_delta.clear();
    }
}

#[test]
fn slideshow_candidates_are_folders_of_pictures() {
    use derisk_settings::{WallpaperKind, candidates};
    let dir = temp_dir("candidates");
    std::fs::create_dir_all(dir.join("shots")).unwrap();
    std::fs::write(dir.join("shots/a.PNG"), b"").unwrap();
    std::fs::create_dir_all(dir.join("empty")).unwrap();
    std::fs::write(dir.join("b.jpg"), b"").unwrap();
    std::fs::write(dir.join("c.webm"), b"").unwrap();
    std::fs::write(dir.join("notes.txt"), b"").unwrap();
    let dirs = [dir.clone()];
    assert_eq!(candidates(WallpaperKind::Image, &dirs), [dir.join("b.jpg")]);
    assert_eq!(
        candidates(WallpaperKind::Video, &dirs),
        [dir.join("c.webm")]
    );
    // The folder itself holds a picture (b.jpg), and so does shots/.
    assert_eq!(
        candidates(WallpaperKind::Slideshow, &dirs),
        [dir.clone(), dir.join("shots")]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_named_theme_is_loaded_and_a_missing_one_falls_back() {
    use derisk_settings::ThemeId;
    let dir = std::env::temp_dir().join(format!("derisk-settings-themes-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("paper.theme"),
        "inherits = \"derisk-light\"\n[shape]\nradius = 11\n",
    )
    .unwrap();
    let library = mcsapi_theme::Library::new([dir.clone()]);

    let (mut settings, warnings) = Settings::parse("appearance.theme = paper\n");
    assert!(warnings.is_empty());
    assert_eq!(settings.appearance.theme.as_str(), "paper");
    let (theme, error) = settings.theme_spec(&library);
    assert!(error.is_none());
    assert_eq!(theme.radius, 11);
    // The file round-trips the choice.
    assert_eq!(Settings::parse(&settings.to_text()).0, settings);

    settings.appearance.theme = ThemeId::parse("gone").unwrap();
    let (theme, error) = settings.theme_spec(&library);
    assert!(error.is_some());
    assert_eq!(theme, mcsapi_theme::Theme::dark());

    for bad in ["", "../x", ".hidden", "a/b"] {
        assert!(ThemeId::parse(bad).is_none(), "{bad:?}");
    }
    assert!(ThemeId::parse("auto").unwrap().is_automatic());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_default_browser_is_a_desktop_file_id_and_nothing_else() {
    let (settings, warnings) = Settings::parse(
        "defaults.browser = ../../bin/sh.desktop\n\
         defaults.browser = firefox\n\
         defaults.search = altavista\n",
    );
    assert_eq!(warnings.len(), 3);
    assert_eq!(settings.defaults, Default::default());
    assert!(Settings::default().privacy.usage_ping);
}

#[test]
fn default_apps_is_listed_only_when_the_policy_says() {
    let app = SettingsApp::open(None);
    assert!(app.listed(Page::Privacy));
    let hidden = app.with_policy(Policy::default());
    assert!(!hidden.listed(Page::DefaultApps));
    let shown = hidden.with_policy(Policy {
        browser: false,
        search: true,
    });
    assert!(shown.listed(Page::DefaultApps));
}

#[test]
fn the_machine_apps_open_in_round_trips_and_must_be_a_name() {
    let mut settings = Settings::default();
    assert_eq!(settings.machines.open_apps_in, "");
    settings.machines.open_apps_in = "work".into();
    let text = settings.to_text();
    assert!(text.contains("machines.open_apps_in = work\n"), "{text}");
    let (parsed, warnings) = Settings::parse(&text);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(parsed, settings);
    // Back to this computer.
    let (parsed, warnings) = Settings::parse("machines.open_apps_in =\n");
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(parsed.machines.open_apps_in, "");
    for bad in ["Work", "../work", "-work", "a b"] {
        let (parsed, warnings) = Settings::parse(&format!("machines.open_apps_in = {bad}\n"));
        assert_eq!(warnings.len(), 1, "{bad}");
        assert_eq!(parsed.machines.open_apps_in, "", "{bad}");
    }
}

#[test]
fn the_assistant_model_round_trips_and_must_be_a_server_and_a_name() {
    let mut settings = Settings::default();
    assert!(settings.assistant.use_model);
    assert_eq!(settings.assistant.url, "http://localhost:11434");
    assert_eq!(settings.assistant.model, "qwen3:4b-instruct");
    settings.assistant.url = "https://models.example.org:8443/ollama".into();
    settings.assistant.model = "hf.co/org/model:Q4_K_M".into();
    settings.assistant.use_model = false;
    let text = settings.to_text();
    assert!(
        text.contains("assistant.model = hf.co/org/model:Q4_K_M\n"),
        "{text}"
    );
    let (parsed, warnings) = Settings::parse(&text);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(parsed, settings);
    // A trailing slash is the same server.
    let (parsed, _) = Settings::parse("assistant.url = http://box.local:11434/\n");
    assert_eq!(parsed.assistant.url, "http://box.local:11434");
    for bad in [
        "assistant.url = ftp://box",
        "assistant.url = http://",
        "assistant.url = file:///etc",
        "assistant.url = http://a b",
        "assistant.model = ",
        "assistant.model = two words",
    ] {
        let (parsed, warnings) = Settings::parse(&format!("{bad}\n"));
        assert_eq!(warnings.len(), 1, "{bad}");
        assert_eq!(parsed.assistant, Settings::default().assistant, "{bad}");
    }
}

#[test]
fn network_page_says_what_it_sees() {
    use derisk_install::{
        network::{Network, Saved, Security},
        wifi::Status,
    };
    use derisk_settings::network::{detail, headline};

    let home = Network {
        ssid: b"home".to_vec(),
        signal: 90,
        security: Security::Passphrase {
            psk: true,
            sae: false,
        },
    };
    let cafe = Network {
        ssid: b"cafe".to_vec(),
        signal: 20,
        security: Security::Open,
    };
    let mut status = Status::default();
    assert_eq!(headline(&status), "Not connected");
    status.online = true;
    assert_eq!(headline(&status), "Online");
    status.connected = Some("home".into());
    assert_eq!(headline(&status), "Online, through home");
    status.joining = Some("cafe".into());
    assert_eq!(headline(&status), "Joining cafe…");

    status.saved = vec![Saved {
        id: "3".into(),
        ssid: b"cafe".to_vec(),
        current: false,
    }];
    assert!(detail(&home, &status).starts_with("Connected · Strong signal · "));
    assert!(detail(&cafe, &status).starts_with("Saved · Weak signal · "));
    assert_eq!(status.saved(b"cafe").map(|s| s.id.as_str()), Some("3"));
    assert!(status.saved(b"home").is_none());
}

#[test]
fn bluetooth_reads_what_bluetoothctl_prints() {
    use derisk_settings::bluetooth::{self, Adapter};

    let show = "Controller 00:1A:7D:DA:71:13 (public)\n\
        \tManufacturer: 0x000a (10)\n\
        \tName: losos\n\
        \tAlias: Matus's laptop\n\
        \tPowered: yes\n\
        \tDiscoverable: no\n";
    assert_eq!(
        bluetooth::parse_show(show),
        Some(Adapter {
            name: "Matus's laptop".into(),
            powered: true,
        })
    );
    assert_eq!(
        bluetooth::parse_show("No default controller available\n"),
        None
    );
    assert_eq!(bluetooth::parse_show(""), None);

    let all = "Device 11:22:33:44:55:66 WH-1000XM4\n\
        Device AA:BB:CC:DD:EE:FF AA-BB-CC-DD-EE-FF\n\
        Device 01:02:03:04:05:06 MX Master 3\n\
        Waiting to connect to bluetoothd...\n";
    let paired = "Device 01:02:03:04:05:06 MX Master 3\n";
    let connected = "Device 01:02:03:04:05:06 MX Master 3\n";
    let devices = bluetooth::merge(all, paired, connected);
    let seen: Vec<_> = devices
        .iter()
        .map(|d| (d.name.as_str(), d.paired, d.connected, d.unnamed()))
        .collect();
    assert_eq!(
        seen,
        [
            ("MX Master 3", true, true, false),
            ("WH-1000XM4", false, false, false),
            ("AA-BB-CC-DD-EE-FF", false, false, true),
        ]
    );

    let pair = bluetooth::pair(&devices[1]);
    assert_eq!(
        pair.steps[0],
        [
            "bluetoothctl",
            "--timeout",
            "30",
            "--agent",
            "NoInputNoOutput",
            "pair",
            "11:22:33:44:55:66"
        ]
    );
    assert_eq!(pair.steps.len(), 3);
    assert_eq!(
        bluetooth::forget(&devices[0]).steps[0].last().unwrap(),
        "01:02:03:04:05:06"
    );
    assert_eq!(bluetooth::power(false).steps[0][3..], ["power", "off"]);
}

#[test]
fn power_modes_are_read_from_powerprofilesctl() {
    use derisk_settings::profiles;

    let list = "  performance:\n\
        \x20   CpuDriver:\tintel_pstate\n\
        \x20   Degraded:   no\n\
        \n\
        * balanced:\n\
        \x20   CpuDriver:\tintel_pstate\n\
        \x20   PlatformDriver:\tplatform_profile\n\
        \n\
        \x20 power-saver:\n\
        \x20   CpuDriver:\tintel_pstate\n";
    let (all, active) = profiles::parse_list(list);
    assert_eq!(all, ["performance", "balanced", "power-saver"]);
    assert_eq!(active.as_deref(), Some("balanced"));
    assert_eq!(profiles::parse_list(""), (Vec::new(), None));
    assert_eq!(profiles::label("power-saver"), "Power saver");
    assert_eq!(
        profiles::set_argv("performance"),
        ["powerprofilesctl", "set", "performance"]
    );
}

#[test]
fn the_system_page_reads_the_report_and_the_reset() {
    use derisk_settings::system::{self, Check, Reset};

    let reply = r#"{"type":"aa{sv}","data":[[{"AppstreamId":{"type":"s","data":"io.losos.disk-encryption"},"Summary":{"type":"s","data":"Disk encryption"},"Description":{"type":"s","data":"Home is encrypted.\n\n/sys/block/dm-1/dm/uuid: CRYPT-LUKS2"},"HsiResult":{"type":"u","data":7},"HsiLevel":{"type":"u","data":1},"Flags":{"type":"t","data":8193}},{"Summary":{"type":"s","data":"Kernel lockdown"},"Description":{"type":"s","data":"none"},"Flags":{"type":"t","data":8192}}]]}"#;
    assert_eq!(
        system::parse_report(reply),
        Some(vec![
            Check {
                summary: "Disk encryption".into(),
                description: "Home is encrypted.\n\n/sys/block/dm-1/dm/uuid: CRYPT-LUKS2".into(),
                passed: true,
            },
            Check {
                summary: "Kernel lockdown".into(),
                description: "none".into(),
                passed: false,
            },
        ])
    );
    assert_eq!(system::parse_report(""), None);
    assert_eq!(
        system::parse_report(r#"{"type":"aa{sv}","data":[[]]}"#),
        Some(Vec::new())
    );

    assert_eq!(
        system::parse_reset(r#"{"supported":true}"#, r#"{"mode":"unspecified"}"#),
        Reset {
            supported: true,
            mode: "unspecified".into(),
        }
    );
    assert_eq!(system::parse_reset("", ""), Reset::default());
    assert_eq!(
        system::RESET_ARGV,
        ["systemctl", "start", "factory-reset.target"]
    );
}

#[test]
fn a_web_background_is_an_http_address_or_a_local_file() {
    use derisk_settings::{WallpaperKind, web_background_url};
    assert!(web_background_url("https://example.org/#top"));
    assert!(web_background_url("http://localhost:8080/"));
    assert!(web_background_url("file:///home/me/page/index.html"));
    assert!(!web_background_url("https://"));
    assert!(!web_background_url("javascript:alert(1)"));
    assert!(!web_background_url("data:text/html,hi"));
    assert!(!web_background_url("https://example.org/a b"));

    let mut settings = Settings::default();
    settings.wallpaper.kind = WallpaperKind::Web;
    // `#` in an address is not a comment.
    settings.wallpaper.url = "https://example.org/clock.html#dark".into();
    let (parsed, warnings) = Settings::parse(&settings.to_text());
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(parsed, settings);

    let (parsed, warnings) =
        Settings::parse("wallpaper.kind = web\nwallpaper.url = javascript:alert(1)\n");
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(parsed.wallpaper.url.is_empty());
}

#[test]
fn the_privacy_display_is_off_until_turned_on() {
    let defaults = Settings::default().privacy_display;
    assert!(!defaults.enabled);
    assert!(defaults.passwords && defaults.notifications && !defaults.maximum);
    assert!(defaults.apps.is_empty());
    let mut settings = Settings::default();
    assert!(settings.set("privacy_display.apps", "a.desktop, b"));
    assert_eq!(settings.privacy_display.apps, ["a.desktop", "b"]);
    assert!(settings.set("privacy_display.apps", ""));
    assert!(settings.privacy_display.apps.is_empty());
    assert!(!settings.set("privacy_display.apps", "ok,../bad/path"));
}
