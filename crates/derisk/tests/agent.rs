use std::{
    io::Write,
    process::{Command, Stdio},
};

use derisk::{
    action::{Action, Effect},
    assistant,
    geom::rect,
    ipc,
    mcsapi::WindowId,
    menu::{Menu, MenuEntry},
    shell::Shell,
    snap::SnapZone,
    systemd::SessionOp,
};
use serde_json::{Value, json};

#[test]
fn assistant_understands_compound_requests() {
    assert_eq!(
        assistant::interpret("open firefox and snap it to the left").unwrap(),
        vec![
            Action::Launch {
                app: "firefox".into()
            },
            Action::Snap {
                window: None,
                zone: SnapZone::Left
            },
        ]
    );
    assert_eq!(
        assistant::interpret("Go to workspace 3").unwrap(),
        vec![Action::SwitchWorkspace { workspace: 3 }]
    );
    assert_eq!(
        assistant::interpret("show the overview").unwrap(),
        vec![Action::Overview {
            visible: Some(true)
        }]
    );
    assert!(assistant::interpret("make me a sandwich").is_err());
}

#[test]
fn assistant_never_confirms_destructive_ops_itself() {
    assert_eq!(
        assistant::interpret("shut down").unwrap(),
        vec![Action::Session {
            op: SessionOp::PowerOff,
            confirmed: false
        }]
    );
}

#[test]
fn ipc_dispatch_state_and_tools() {
    let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
    let (resp, effects) = ipc::handle_line(
        &mut shell,
        r#"{"method":"dispatch","actions":[{"action":"launch","app":"foot"}]}"#,
    );
    assert_eq!(effects, vec![Effect::Launch { app: "foot".into() }]);
    assert_eq!(serde_json::from_str::<Value>(&resp).unwrap()["ok"], true);

    shell.map_window("foot", "foot");
    let (resp, _) = ipc::handle_line(&mut shell, r#"{"method":"state"}"#);
    let v: Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(v["result"]["windows"][0]["app_id"], "foot");
    assert_eq!(v["result"]["form_factor"], "desktop");

    let (resp, _) = ipc::handle_line(&mut shell, r#"{"method":"tools"}"#);
    let v: Value = serde_json::from_str(&resp).unwrap();
    let names: Vec<_> = v["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(names.contains(&"dispatch".to_owned()));

    let (resp, effects) = ipc::handle_line(&mut shell, "{nope");
    assert!(effects.is_empty());
    assert_eq!(serde_json::from_str::<Value>(&resp).unwrap()["ok"], false);
}

#[test]
fn ipc_menus_feed_the_global_menu() {
    let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
    let (w, _) = shell.map_window("app", "App");
    let req = json!({
        "method": "register_menu",
        "window": w.get(),
        "menus": [{"title": "File", "entries": [{"kind": "item", "id": "open", "label": "Open"}]}],
    });
    let (resp, _) = ipc::handle_line(&mut shell, &req.to_string());
    assert_eq!(
        serde_json::from_str::<Value>(&resp).unwrap()["ok"],
        true,
        "{resp}"
    );
    let bar: Vec<_> = shell.menus.bar(Some(w.get())).collect();
    assert_eq!(bar[0].title, "File");
    assert_eq!(bar.last().unwrap().title, "Window");
}

#[test]
fn an_apps_window_menu_takes_the_shells_entries() {
    let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
    let (w, _) = shell.map_window("macos.com.apple.Calculator", "Calculator");
    shell.menus.register(
        w.get(),
        vec![
            Menu {
                title: "Calculator".into(),
                entries: vec![MenuEntry::item("1.0", "About Calculator")],
            },
            Menu {
                title: "Window".into(),
                entries: vec![MenuEntry::item("4.0", "Zoom")],
            },
            Menu {
                title: "Help".into(),
                entries: vec![],
            },
        ],
    );
    let bar: Vec<_> = shell.menus.bar(Some(w.get())).collect();
    let titles: Vec<_> = bar.iter().map(|m| m.title.as_str()).collect();
    assert_eq!(titles, ["Calculator", "Window", "Help"]);
    let window = &bar[1].entries;
    assert_eq!(window[0], MenuEntry::item("4.0", "Zoom"));
    assert_eq!(window[1], MenuEntry::Separator);
    assert!(window.contains(&MenuEntry::item("derisk.float", "Float").with_shortcut("Super+F")));

    // The first menu is named after the app, and the title shows it.
    assert!(derisk::menu::named_after(
        &bar,
        &["Calculator", "Calculator"]
    ));
    assert!(!derisk::menu::named_after(&bar[1..], &["Calculator"]));
    assert!(!derisk::menu::named_after(&bar, &["TextEdit", "Untitled"]));
}

fn tabs(titles: &[&str]) -> Vec<Menu> {
    vec![Menu {
        title: "Tabs".into(),
        entries: titles
            .iter()
            .enumerate()
            .map(|(i, t)| MenuEntry::item(format!("switch-{i}"), format!("Switch to {t}")))
            .chain([MenuEntry::Separator, MenuEntry::item("new-tab", "New Tab")])
            .collect(),
    }]
}

#[test]
fn live_menus_belong_to_the_connection_that_registered_them() {
    let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
    let (w, _) = shell.map_window("uranium", "Uranium");
    let (other, _) = shell.map_window("foot", "~");
    let (w, other) = (w.get(), other.get());

    assert_eq!(
        ipc::register_menu(&mut shell, 7, 999, tabs(&["a"])),
        Err("unknown window: 999".into())
    );
    assert_eq!(shell.menus.app_menus(999), &[]);

    assert_eq!(
        ipc::register_menu(&mut shell, 7, w, tabs(&["a"])),
        Ok(Value::Null)
    );
    assert_eq!(shell.menus.owner(w), Some(7));
    assert_eq!(
        ipc::register_menu(&mut shell, 8, w, tabs(&["b"])),
        Err(format!("another connection registered window {w}'s menus"))
    );
    assert_eq!(shell.menus.app_menus(w), tabs(&["a"]).as_slice());

    // The owner replaces its menus as tabs come and go.
    ipc::register_menu(&mut shell, 7, w, tabs(&["a", "b"])).unwrap();
    assert_eq!(shell.menus.app_menus(w), tabs(&["a", "b"]).as_slice());
    ipc::register_menu(&mut shell, 7, other, tabs(&["c"])).unwrap();

    // Picks go to the owner only for items it listed, never shell items.
    assert_eq!(shell.menus.recipient(w, "switch-1"), Some(7));
    assert_eq!(shell.menus.recipient(w, "new-tab"), Some(7));
    assert_eq!(shell.menus.recipient(w, "switch-9"), None);
    assert_eq!(shell.menus.recipient(w, "derisk.close"), None);

    // Its connection closing takes all its menus away.
    let mut gone = shell.menus.disown(7);
    gone.sort_unstable();
    assert_eq!(gone, vec![w, other]);
    assert_eq!(shell.menus.app_menus(w), &[]);
    assert_eq!(shell.menus.owner(w), None);
    ipc::register_menu(&mut shell, 8, w, tabs(&["b"])).unwrap();
    assert_eq!(shell.menus.owner(w), Some(8));
}

#[test]
fn unowned_menus_have_nobody_to_tell() {
    let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
    let (w, _) = shell.map_window("uranium", "Uranium");
    let w = w.get();
    ipc::register_menu(&mut shell, 7, w, tabs(&["a"])).unwrap();
    // The request without a live connection, as `derisk agent` handles it,
    // registers menus nobody owns.
    let req = json!({"method": "register_menu", "window": w, "menus": tabs(&["b"])});
    ipc::handle_line(&mut shell, &req.to_string());
    assert_eq!(shell.menus.owner(w), None);
    assert_eq!(shell.menus.recipient(w, "switch-0"), None);

    ipc::register_menu(&mut shell, 7, w, tabs(&["a"])).unwrap();
    shell.unmap_window(WindowId::new(w).unwrap()).unwrap();
    assert_eq!(shell.menus.owner(w), None);
}

#[test]
fn menu_picks_become_one_event_line() {
    let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
    let (w, _) = shell.map_window("uranium", "Uranium");
    let w = w.get();
    ipc::register_menu(&mut shell, 7, w, tabs(&["a"])).unwrap();
    let effects = shell
        .run([Action::ActivateMenu {
            window: Some(w),
            item: "switch-0".into(),
        }])
        .into_result()
        .unwrap();
    let [Effect::MenuActivated { window, item }] = effects.as_slice() else {
        panic!("{effects:?}");
    };
    assert_eq!(shell.menus.recipient(*window, item), Some(7));
    let line = ipc::menu_event(*window, item).to_string();
    assert!(!line.contains('\n'));
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap(),
        json!({"event": "menu", "window": w, "item": "switch-0"})
    );
}

#[test]
fn agent_binary_speaks_json_lines_on_stdio() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_derisk"))
        .arg("agent")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"{\"method\":\"ask\",\"text\":\"open kitty and snap it right\"}\n{\"method\":\"state\"}\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let lines: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    let window = &lines[1]["result"]["windows"][0];
    assert_eq!(window["app_id"], "kitty");
    assert_eq!(window["zone"], "right");
}

#[test]
fn requests_are_recorded_with_step_progress() {
    use derisk::conversation::{Source, StepStatus};

    let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
    shell
        .ask("open kitty and snap it left", Source::User, false)
        .into_result()
        .unwrap();
    let turn = shell.conversation.turns().last().unwrap().clone();
    assert_eq!(turn.source, Source::User);
    assert_eq!(turn.steps[0].label, "Open kitty");
    assert_eq!(turn.steps[0].status, StepStatus::Waiting);
    assert_eq!(turn.steps[1].label, "Snap the window left");
    assert_eq!(turn.steps[1].status, StepStatus::Waiting);
    assert!(!turn.is_settled());
    assert!(turn.reply().starts_with("Waiting"));

    // Both finish when kitty's window maps: the launch first, then the snap.
    shell.map_window("kitty", "~");
    let turn = shell.conversation.turns().last().unwrap();
    assert_eq!(turn.steps[0].status, StepStatus::Done);
    assert_eq!(turn.steps[1].status, StepStatus::Done);
    assert_eq!(turn.reply(), "Done.");

    // A failing step stops the rest.
    assert!(
        shell
            .ask("go to workspace 42 and close it", Source::User, false)
            .into_result()
            .is_err()
    );
    let turn = shell.conversation.turns().last().unwrap();
    assert!(matches!(turn.steps[0].status, StepStatus::Failed(_)));
    assert_eq!(turn.steps[1].status, StepStatus::Skipped);
    assert!(
        turn.reply()
            .starts_with("Stopped at \"Go to workspace 42\"")
    );

    // Not understood.
    assert!(
        shell
            .ask("make me a sandwich", Source::User, false)
            .into_result()
            .is_err()
    );
    assert!(shell.conversation.turns().last().unwrap().error.is_some());

    // Destructive operations need the person's confirmation.
    assert!(matches!(
        shell.ask("reboot", Source::User, false).into_result(),
        Err(derisk::shell::Error::NeedsConfirmation(SessionOp::Reboot))
    ));
    assert!(
        shell
            .ask("reboot", Source::User, true)
            .into_result()
            .is_ok()
    );
    assert!(
        shell
            .ask("lock the screen", Source::User, false)
            .into_result()
            .is_ok()
    );
}

#[test]
fn agent_requests_show_in_the_conversation() {
    use derisk::conversation::Source;

    let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
    ipc::handle_line(&mut shell, r#"{"method":"ask","text":"go to workspace 2"}"#);
    ipc::handle_line(
        &mut shell,
        r#"{"method":"dispatch","actions":[{"action":"switch_workspace","workspace":3}]}"#,
    );
    let (_, effects) = ipc::handle_line(&mut shell, r#"{"method":"ask","text":"shut down"}"#);
    assert!(
        effects.is_empty(),
        "agents still cannot power off unconfirmed"
    );
    let turns: Vec<_> = shell.conversation.turns().collect();
    assert_eq!(turns.len(), 3);
    assert!(turns.iter().all(|t| t.source == Source::Agent));
    assert_eq!(turns[0].request, "go to workspace 2");
    assert_eq!(turns[1].request, "1 action");
    assert_eq!(turns[1].steps[0].label, "Go to workspace 3");
}

#[test]
fn a_failed_step_keeps_the_effects_of_the_steps_before_it() {
    use derisk::conversation::{Source, StepStatus};

    let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
    // Launching kitty succeeds, then the workspace does not exist: the
    // launch must still happen.
    let outcome = shell.ask("open kitty then go to workspace 42", Source::User, false);
    assert!(outcome.result.is_err());
    assert_eq!(
        outcome.effects,
        [Effect::Launch {
            app: "kitty".into()
        }]
    );
    let turn = shell.conversation.turns().last().unwrap();
    assert_eq!(turn.steps[0].status, StepStatus::Done);
    assert!(matches!(turn.steps[1].status, StepStatus::Failed(_)));

    // Over the agent protocol, the failed request still carries its effects.
    let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
    let request = json!({
        "method": "dispatch",
        "actions": [
            {"action": "launch", "app": "kitty"},
            {"action": "switch_workspace", "workspace": 42}
        ]
    });
    let (reply, effects) = ipc::handle_line(&mut shell, &request.to_string());
    assert!(reply.contains("\"ok\":false"), "{reply}");
    assert_eq!(
        effects,
        [Effect::Launch {
            app: "kitty".into()
        }]
    );
}

#[test]
fn deferred_steps_stop_at_the_first_failure() {
    use derisk::conversation::{Source, StepStatus};

    let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
    let outcome = shell.run_recorded(
        "open kitty, move it to workspace 42 and close it",
        Source::User,
        vec![
            Action::Launch {
                app: "kitty".into(),
            },
            Action::MoveToWorkspace {
                window: None,
                workspace: 42,
            },
            Action::Close { window: None },
        ],
    );
    assert!(outcome.result.is_ok());
    let (kitty, effects) = shell.map_window("kitty", "~");
    let turn = shell.conversation.turns().last().unwrap();
    assert!(matches!(turn.steps[1].status, StepStatus::Failed(_)));
    assert_eq!(turn.steps[2].status, StepStatus::Skipped);
    assert!(effects.is_empty(), "the close was skipped: {effects:?}");
    assert!(shell.window_label(kitty).is_some());
}

#[test]
fn socket_requests_are_capped() {
    use std::io::Cursor;

    let mut ok = Cursor::new(b"{\"method\":\"state\"}\nlast".to_vec());
    assert_eq!(
        derisk::ipc::read_request(&mut ok).unwrap().as_deref(),
        Some("{\"method\":\"state\"}")
    );
    assert_eq!(
        derisk::ipc::read_request(&mut ok).unwrap().as_deref(),
        Some("last")
    );
    assert_eq!(derisk::ipc::read_request(&mut ok).unwrap(), None);

    // A client that never sends a newline is cut off, not buffered forever.
    let endless = vec![b'a'; derisk::ipc::MAX_REQUEST as usize + 10];
    assert!(derisk::ipc::read_request(&mut Cursor::new(endless)).is_err());
}

#[test]
fn socket_dirs_others_could_swap_are_refused() {
    use std::{fs, os::unix::fs::PermissionsExt};
    let root = std::env::temp_dir().join(format!("derisk-sockdir-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let dir = |path: &str, mode: u32| {
        let path = root.join(path);
        fs::create_dir_all(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        path
    };
    dir("", 0o700);

    assert!(ipc::check_socket_dir(&dir("private", 0o700)).is_ok());
    assert!(ipc::check_socket_dir(&dir("open", 0o777)).is_err());
    // The check must not follow a link it cannot trust to stay put.
    std::os::unix::fs::symlink(root.join("private"), root.join("link")).unwrap();
    assert!(ipc::check_socket_dir(&root.join("link")).is_err());
    // Another user could rename `shared/private` away and put theirs there.
    dir("shared", 0o777);
    assert!(ipc::check_socket_dir(&dir("shared/private", 0o700)).is_err());
    // A sticky directory, like /tmp, does not let them.
    dir("sticky", 0o1777);
    assert!(ipc::check_socket_dir(&dir("sticky/private", 0o700)).is_ok());

    fs::set_permissions(root.join("shared"), fs::Permissions::from_mode(0o700)).unwrap();
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn custom_widgets_register_press_and_go_with_their_owner() {
    let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
    let line = json!({"method": "register_widget", "widget": {
        "id": "weather", "title": "Weather", "icon": "weather-clear",
        "rows": [
            {"type": "text", "text": "18 °C, clear"},
            {"type": "progress", "value": 0.4, "label": "Rain 40%"},
            {"type": "button", "label": "Refresh", "item": "refresh"}
        ]
    }});
    let (resp, _) = ipc::handle_line(&mut shell, &line.to_string());
    assert!(resp.contains("\"ok\":true"), "{resp}");
    assert_eq!(shell.widgets.list().len(), 1);
    assert_eq!(shell.widgets.list()[0].rows.len(), 3);

    // A press on a button the widget has becomes an effect for its owner;
    // one it never listed is refused.
    let effects = shell
        .apply(Action::ActivateWidget {
            id: "weather".into(),
            item: "refresh".into(),
        })
        .unwrap();
    assert!(
        matches!(&effects[..], [Effect::WidgetActivated { id, item }] if id == "weather" && item == "refresh")
    );
    assert!(
        shell
            .apply(Action::ActivateWidget {
                id: "weather".into(),
                item: "delete".into(),
            })
            .is_err()
    );

    // Owned widgets: another connection can neither replace nor remove one,
    // and it goes when its owner's connection closes.
    let mut widgets = derisk::widgets::CustomWidgets::default();
    let widget: derisk::widgets::CustomWidget =
        serde_json::from_value(line["widget"].clone()).unwrap();
    widgets.register(widget.clone(), Some(1)).unwrap();
    assert!(widgets.register(widget.clone(), Some(2)).is_err());
    assert!(widgets.remove("weather", Some(2)).is_err());
    assert_eq!(widgets.recipient("weather", "refresh"), Some(1));
    assert_eq!(widgets.recipient("weather", "other"), None);
    widgets.disown(1);
    assert!(widgets.list().is_empty());

    // Oversized widgets are refused instead of crowding the overview.
    let mut long = widget;
    long.title = "x".repeat(500);
    assert!(widgets.register(long, None).is_err());

    let (resp, _) = ipc::handle_line(&mut shell, r#"{"method":"remove_widget","id":"weather"}"#);
    assert!(resp.contains("\"ok\":true"), "{resp}");
    assert!(shell.widgets.list().is_empty());
}

#[test]
fn steps_finish_in_the_order_they_were_asked_for() {
    use derisk::conversation::{Source, StepStatus};

    let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
    let statuses = |shell: &Shell| -> Vec<StepStatus> {
        let turn = shell.conversation.turns().last().unwrap();
        turn.steps.iter().map(|s| s.status.clone()).collect()
    };
    let effects = shell
        .ask(
            "open files and snap it left, then open editor and snap it right",
            Source::User,
            false,
        )
        .into_result()
        .unwrap();
    // Only the first app starts; the editor waits for Files' window.
    assert_eq!(
        effects,
        vec![Effect::Launch {
            app: "files".into()
        }]
    );
    assert_eq!(statuses(&shell), vec![StepStatus::Waiting; 4]);

    let (_, effects) = shell.map_window("org.derisk.files", "Files");
    assert_eq!(
        effects
            .iter()
            .filter(|e| matches!(e, Effect::Launch { .. }))
            .collect::<Vec<_>>(),
        vec![&Effect::Launch {
            app: "editor".into()
        }]
    );
    assert_eq!(
        statuses(&shell),
        [
            StepStatus::Done,
            StepStatus::Done,
            StepStatus::Waiting,
            StepStatus::Waiting
        ]
    );

    shell.map_window("org.derisk.editor", "Text Editor");
    assert_eq!(statuses(&shell), vec![StepStatus::Done; 4]);
    assert_eq!(shell.conversation.turns().last().unwrap().reply(), "Done.");
}
