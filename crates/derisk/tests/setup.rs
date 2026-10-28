//! The installer's and first-boot setup's rules, without a screen: languages,
//! layouts and time zones, the account form, the commands that save it, the
//! installer backend's protocol and where the pages go on a screen.

use derisk::{
    install::{self, Disk, Event, Request},
    locale::{self, Keyboard},
    password_age::PasswordAge,
    setup::{self, Account, Choices, Step},
    wizard,
};

#[test]
fn languages_are_utf8_locales_by_native_name() {
    let list = "C.UTF-8\nde_DE.UTF-8\nen_US.UTF-8\nsk_SK.utf8\ncs_CZ.UTF-8\nfr_FR\nxx_YY.UTF-8\n";
    let languages = locale::languages(list);
    let names: Vec<&str> = languages.iter().map(|l| l.name.as_str()).collect();
    // Čeština sorts with the Cs, not after the Zs; an unknown locale shows
    // under its code; C and non-UTF-8 locales are left out.
    assert_eq!(
        names,
        [
            "Čeština",
            "Deutsch (Deutschland)",
            "English (United States)",
            "Slovenčina",
            "xx_YY.UTF-8"
        ]
    );
    // glibc's `locale -a` spelling, as NixOS lists them, written the way
    // localectl and homectl are given a locale; a modifier is kept.
    let glibc = locale::languages("C.utf8\nen_US.utf8\nsk_SK.utf8\nsr_RS.utf8@latin\nPOSIX\n");
    let codes: Vec<&str> = glibc.iter().map(|l| l.locale.as_str()).collect();
    assert_eq!(codes, ["en_US.UTF-8", "sk_SK.UTF-8", "sr_RS.UTF-8@latin"]);
    assert_eq!(
        locale::language_name("de_DE.UTF-8@euro"),
        Some("Deutsch (Deutschland)")
    );
}

#[test]
fn layouts_come_from_the_layout_section_of_base_lst() {
    let base = "! model\n  pc105           Generic 105-key PC\n\n! layout\n  us              English (US)\n  de              German\n  sk              Slovak\n\n! variant\n  chr             us: Cherokee\n";
    let layouts = locale::layouts(base);
    let seen: Vec<(&str, &str)> = layouts
        .iter()
        .map(|l| (l.name.as_str(), l.description.as_str()))
        .collect();
    assert_eq!(
        seen,
        [("us", "English (US)"), ("de", "German"), ("sk", "Slovak")]
    );
}

#[test]
fn saved_keyboard_becomes_xkb_defaults() {
    let conf = "# Written by systemd-localed(8)\nSection \"InputClass\"\n        Identifier \"system-keyboard\"\n        MatchIsKeyboard \"on\"\n        Option \"XkbLayout\" \"de,ru\"\n        Option \"XkbOptions\" \"grp:alt_shift_toggle\"\nEndSection\n";
    let keyboard = locale::parse_x11_keyboard(conf);
    assert_eq!(
        keyboard,
        Keyboard {
            layout: "de,ru".into(),
            variant: String::new(),
            options: "grp:alt_shift_toggle".into(),
        }
    );
    assert_eq!(
        locale::xkb_environment(&keyboard).collect::<Vec<_>>(),
        [
            "XKB_DEFAULT_LAYOUT=de,ru",
            "XKB_DEFAULT_OPTIONS=grp:alt_shift_toggle"
        ]
    );
    assert_eq!(locale::xkb_environment(&Keyboard::default()).count(), 0);
}

#[test]
fn time_zones_read_and_match_by_city() {
    assert_eq!(
        locale::time_zone_label("America/Argentina/Buenos_Aires"),
        "Buenos Aires, America, Argentina"
    );
    assert_eq!(locale::time_zone_label("UTC"), "UTC");
    assert!(locale::matches(
        "Bratislava, Europe Europe/Bratislava",
        "brat eur"
    ));
    assert!(!locale::matches("Berlin, Europe", "brat"));
    assert_eq!(
        locale::time_zone_of(std::path::Path::new("/etc/zoneinfo/Europe/Bratislava")).as_deref(),
        Some("Europe/Bratislava")
    );
}

#[test]
fn user_names_derive_from_the_full_name() {
    assert_eq!(setup::user_name_for("Matúš Novák"), "matus");
    assert_eq!(setup::user_name_for("Zoë"), "zoe");
    assert_eq!(setup::user_name_for("  Łukasz  "), "lukasz");
    assert_eq!(setup::user_name_for("42 Douglas"), "");
    assert_eq!(setup::user_name_for(""), "");

    let mut account = Account::default();
    account.set_real_name("Ada Lovelace".into());
    assert_eq!(account.user_name, "ada");
    // Once typed, the user name stays what was typed.
    account.user_name = "countess".into();
    account.user_name_edited = true;
    account.set_real_name("Ada King".into());
    assert_eq!(account.user_name, "countess");
}

#[test]
fn account_problems_in_order() {
    let mut account = Account::default();
    assert_eq!(account.problem(), Some("Enter your name."));
    account.set_real_name("Ada".into());
    assert_eq!(account.problem(), Some("Choose a password."));
    account.password = "hunter2".into();
    account.weakness = Some("The password is shorter than 8 characters.".into());
    assert_eq!(
        account.problem(),
        Some("The password is shorter than 8 characters.")
    );
    account.weakness = None;
    assert_eq!(account.problem(), Some("The passwords do not match."));
    account.confirm = "hunter2".into();
    assert_eq!(account.problem(), None);
    account.user_name = "root".into();
    assert!(account.problem().is_some());
    for bad in ["Ada", "1ada", "ada lovelace", "ä", &"a".repeat(32)] {
        assert!(setup::user_name_problem(bad).is_some(), "{bad}");
    }
    for good in ["ada", "_ada", "ada-l", "ada_2"] {
        assert_eq!(setup::user_name_problem(good), None, "{good}");
    }
}

#[test]
fn the_plan_saves_everything_and_creates_the_account_last() {
    let choices = Choices {
        locale: "sk_SK.UTF-8".into(),
        layout: "sk".into(),
        time_zone: "Europe/Bratislava".into(),
        account: Account {
            real_name: "Matúš Novák ".into(),
            user_name: "matus".into(),
            password: "correct horse".into(),
            confirm: "correct horse".into(),
            user_name_edited: false,
            weakness: None,
        },
        password_age: PasswordAge::default(),
        groups: vec!["networkmanager".into()],
    };
    let plan = setup::plan(&choices);
    let argv: Vec<Vec<&str>> = plan
        .iter()
        .map(|t| t.argv.iter().map(String::as_str).collect())
        .collect();
    assert_eq!(
        argv,
        [
            vec!["localectl", "set-locale", "LANG=sk_SK.UTF-8"],
            vec!["localectl", "set-x11-keymap", "sk"],
            vec!["timedatectl", "set-timezone", "Europe/Bratislava"],
            vec![
                "homectl",
                "create",
                "matus",
                "--real-name=Matúš Novák",
                "--member-of=wheel,networkmanager",
                "--timezone=Europe/Bratislava",
            ],
        ]
    );
    // The password is never on a command line.
    assert!(
        plan.iter()
            .all(|t| !t.argv.iter().any(|a| a.contains("horse")))
    );
    assert_eq!(
        plan[3].env,
        [("NEWPASSWORD".to_owned(), "correct horse".to_owned())]
    );

    // Nothing chosen: only the account.
    let bare = setup::plan(&Choices {
        account: choices.account.clone(),
        ..Choices::default()
    });
    assert_eq!(bare.len(), 1);
    assert!(bare[0].argv.contains(&"--member-of=wheel".to_owned()));
    assert!(!bare[0].argv.iter().any(|a| a.starts_with("--timezone")));
}

#[test]
fn the_account_is_created_under_the_password_age_policy() {
    let choices = Choices {
        account: Account {
            user_name: "ada".into(),
            ..Account::default()
        },
        password_age: PasswordAge {
            max_days: 365,
            warn_days: 14,
        },
        ..Choices::default()
    };
    let plan = setup::plan(&choices);
    let argv = &plan.last().unwrap().argv;
    assert!(argv.contains(&"--password-change-max=365d".to_owned()));
    assert!(argv.contains(&"--password-change-warn=14d".to_owned()));
}

#[test]
fn steps_run_in_order() {
    assert_eq!(Step::Language.back(), None);
    assert_eq!(Step::Language.next(), Step::Keyboard);
    assert_eq!(Step::Account.next(), Step::Applying);
    assert_eq!(Step::Account.back(), Some(Step::Network));
    assert_eq!(Step::Applying.index(), Step::PAGES.len());
}

#[test]
fn setup_runs_only_without_regular_users() {
    assert!(!setup::has_regular_users(""));
    let homed = "\u{1e}{\"userName\":\"ada\",\"disposition\":\"regular\"}\n";
    assert!(setup::has_regular_users(homed));
}

#[test]
fn a_password_question_is_matched_to_the_process_asking() {
    // What systemd 261's ask_password_agent() writes for homectl.
    let ask = "[Ask]\nPID=1875\nSocket=/run/systemd/ask-password/sck.5b0c\n\
               AcceptCached=0\nEcho=0\nNotAfter=0\nSilent=0\nIcon=user-home\n\
               Message=Please enter new password for user matus:\n";
    assert_eq!(
        setup::ask_socket(ask, 1875),
        Some("/run/systemd/ask-password/sck.5b0c".into())
    );
    assert_eq!(setup::ask_socket(ask, 18), None);
    assert_eq!(setup::ask_socket("[Ask]\nPID=1875\n", 1875), None);
    assert_eq!(setup::ask_socket("", 1875), None);
}

#[test]
fn quality_messages_become_sentences() {
    assert_eq!(
        setup::sentence("The password is shorter than 8 characters"),
        "The password is shorter than 8 characters."
    );
    assert_eq!(setup::sentence("Already one. "), "Already one.");
}

#[test]
fn a_refused_password_shows_homeds_reason() {
    let cancelled = "Password too weak: The password is shorter than 8 characters\n\
                     (Use --enforce-password-policy=no to turn off password quality checks for this account.)\n\
                     Failed to acquire password: Operation canceled\n";
    assert_eq!(
        setup::task_failure(cancelled),
        Some("Password too weak: The password is shorter than 8 characters")
    );
    assert_eq!(
        setup::task_failure("Warning: x\nFailed to set locale: Access denied\n"),
        Some("Failed to set locale: Access denied")
    );
    assert_eq!(
        setup::task_failure("Failed to acquire password: Operation canceled\n"),
        Some("Failed to acquire password: Operation canceled")
    );
    assert_eq!(setup::task_failure(" \n"), None);
}

#[test]
fn backend_protocol_round_trips() {
    assert_eq!(
        install::request_line(&Request::Disks),
        "{\"method\":\"disks\"}\n"
    );
    assert_eq!(
        install::request_line(&Request::Install {
            disk: "/dev/vda".into()
        }),
        "{\"method\":\"install\",\"disk\":\"/dev/vda\"}\n"
    );
    assert_eq!(
        install::parse_event("{\"event\":\"hello\",\"name\":\"LosOS Desktop\"}"),
        Some(Event::Hello {
            name: "LosOS Desktop".into(),
            source: None,
            release: None,
            report: None,
        })
    );
    assert_eq!(
        install::parse_event(
            "{\"event\":\"hello\",\"name\":\"LosOS Desktop\",\"release\":\"/run/losos/release\"}"
        ),
        Some(Event::Hello {
            name: "LosOS Desktop".into(),
            source: None,
            release: Some("/run/losos/release".into()),
            report: None,
        })
    );
    assert_eq!(
        install::parse_event(
            "{\"event\":\"hello\",\"name\":\"LosOS Desktop\",\"report\":\"losos-project/desktop\"}"
        ),
        Some(Event::Hello {
            name: "LosOS Desktop".into(),
            source: None,
            release: None,
            report: Some("losos-project/desktop".into()),
        })
    );
    let disks = install::parse_event(
        "{\"event\":\"disks\",\"disks\":[{\"path\":\"/dev/vda\",\"name\":\"vda\",\"size\":21474836480}]}",
    );
    let Some(Event::Disks { disks }) = disks else {
        panic!("not disks: {disks:?}");
    };
    assert_eq!(
        disks,
        [Disk {
            path: "/dev/vda".into(),
            name: "vda".into(),
            model: String::new(),
            size: 21_474_836_480,
            removable: false,
        }]
    );
    assert_eq!(disks[0].title(), "vda");
    assert_eq!(disks[0].size_text(), "21.5 GB");
    assert_eq!(install::parse_event("not json"), None);
    assert_eq!(install::parse_event("{\"event\":\"unknown\"}"), None);
}

#[test]
fn a_report_round_trips() {
    assert_eq!(
        install::request_line(&Request::Report),
        "{\"method\":\"report\"}\n"
    );
    assert_eq!(
        install::request_line(&Request::ReportSend {
            note: "It froze".into()
        }),
        "{\"method\":\"report_send\",\"note\":\"It froze\"}\n"
    );
    assert_eq!(
        install::request_line(&Request::ReportCancel),
        "{\"method\":\"report_cancel\"}\n"
    );
    assert_eq!(
        install::parse_event(
            "{\"event\":\"report\",\"repository\":\"o/r\",\"title\":\"T\",\"body\":\"B\"}"
        ),
        Some(Event::Report {
            repository: "o/r".into(),
            title: "T".into(),
            body: "B".into(),
        })
    );
    assert_eq!(
        install::parse_event(
            "{\"event\":\"report_sign_in\",\"uri\":\"https://github.com/login/device\",\"code\":\"WDJB-MJHT\"}"
        ),
        Some(Event::ReportSignIn {
            uri: "https://github.com/login/device".into(),
            code: "WDJB-MJHT".into(),
        })
    );
    assert_eq!(
        install::parse_event("{\"event\":\"reported\",\"url\":\"https://x/7\"}"),
        Some(Event::Reported {
            url: "https://x/7".into(),
            existing: false,
        })
    );
    assert_eq!(
        install::parse_event("{\"event\":\"report_failed\",\"message\":\"declined\"}"),
        Some(Event::ReportFailed {
            message: "declined".into()
        })
    );
}

#[test]
fn pages_fill_a_phone_and_float_on_a_desktop() {
    let phone = wizard::screen_layout((392, 872), false);
    assert!(phone.phone);
    assert_eq!(phone.card.width(), 392.0);
    assert_eq!(phone.card.height(), 872.0);
    let typing = wizard::screen_layout((392, 872), true);
    let keyboard = typing.keyboard.expect("a keyboard");
    assert_eq!(keyboard.loc.y + keyboard.size.h, 872);
    assert_eq!(typing.card.bottom(), keyboard.loc.y as f32);

    let desktop = wizard::screen_layout((1920, 1080), false);
    assert!(!desktop.phone);
    assert!(desktop.card.width() <= 600.0);
    assert!((desktop.card.center().x - 960.0).abs() < 1.0);
    let typing = wizard::screen_layout((1920, 1080), true);
    let keyboard = typing.keyboard.expect("a keyboard");
    assert!(typing.card.bottom() <= keyboard.loc.y as f32);
}

#[test]
fn the_account_joins_only_device_groups_that_exist() {
    let dir = std::env::temp_dir().join(format!("derisk-setup-groups-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("group");
    std::fs::write(&file, "root:x:0:\nwheel:x:1:\nnetworkmanager:x:991:\n").unwrap();
    assert_eq!(setup::device_groups(&file), ["networkmanager"]);
    std::fs::write(&file, "root:x:0:\nwheel:x:1:\n").unwrap();
    assert!(setup::device_groups(&file).is_empty());
    assert!(setup::device_groups(&dir.join("missing")).is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}
