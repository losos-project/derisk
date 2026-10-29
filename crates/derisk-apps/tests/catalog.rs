use std::collections::BTreeSet;

use derisk_apps::{APPS, CALCULATOR, FILES, Session, find, register_all, visuals};
use mcsapi_runtime::{AppId, Error, Runtime};
use mcsapi_ui::{Theme, egui, run_frame};

#[test]
fn catalog_ids_are_valid_and_unique() {
    let ids: BTreeSet<_> = APPS.iter().map(|app| app.app_id()).collect();
    assert_eq!(ids.len(), APPS.len());
    for app in &APPS {
        assert_eq!(find(app.id).unwrap().name, app.name);
        assert_eq!(app.manifest().id.as_str(), app.id);
    }
    assert!(find("org.derisk.nope").is_none());
}

#[test]
fn every_desktop_action_opens_its_app() {
    // The palette lists a desktop file's actions, and an action the app
    // does not know opens nothing. Settings' System page, where updates are
    // chosen, was missing from its list once, so the palette could not
    // reach it.
    for app in &APPS {
        let actions = app
            .desktop_file
            .lines()
            .find_map(|line| line.strip_prefix("Actions="))
            .unwrap_or_default();
        for action in actions.split(';').filter(|a| !a.is_empty()) {
            assert!(
                app.desktop_file
                    .contains(&format!("[Desktop Action {action}]")),
                "{} lists {action} without its group",
                app.id
            );
            assert!(
                app.create_action(action).is_some(),
                "{} does not open {action}",
                app.id
            );
        }
    }
    let settings = find("org.derisk.settings").unwrap();
    let actions = settings
        .desktop_file
        .lines()
        .find(|l| l.starts_with("Actions="));
    assert!(actions.is_some_and(|l| l.contains(";system;")));
}

#[test]
fn search_matches_names_summaries_and_keywords() {
    let hits = |q: &str| {
        APPS.iter()
            .filter(|a| a.matches(q))
            .map(|a| a.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(hits("task manager"), ["org.derisk.monitor"]);
    assert_eq!(hits("FOLDER"), [FILES]);
    assert_eq!(hits("math"), [CALCULATOR]);
    assert_eq!(hits("  ").len(), APPS.len());
}

#[test]
fn registers_with_a_runtime_once() {
    let mut runtime = Runtime::new();
    register_all(&mut runtime).unwrap();
    assert_eq!(runtime.apps().count(), APPS.len());
    assert!(matches!(
        register_all(&mut runtime),
        Err(Error::DuplicateApp(_))
    ));
}

#[test]
fn session_keeps_apps_with_instances() {
    let mut session = Session::new().unwrap();
    let calculator = AppId::new(CALCULATOR).unwrap();
    let first = session.launch(&calculator).unwrap();
    let second = session.launch(&calculator).unwrap();
    assert_ne!(first, second);
    assert_eq!(session.running().count(), 2);
    assert_eq!(session.runtime().instances().count(), 2);
    assert_eq!(session.stop(first).unwrap(), calculator);
    assert!(session.app_mut(first).is_none());
    assert_eq!(session.app_mut(second).unwrap().title(), "Calculator");
    assert!(matches!(
        session.stop(first),
        Err(Error::UnknownInstance(_))
    ));
    let unknown = AppId::new("org.example.other").unwrap();
    assert_eq!(session.launch(&unknown), Err(Error::UnknownApp(unknown)));
}

#[test]
fn every_app_draws_a_frame() {
    let context = egui::Context::default();
    for app in &APPS {
        let mut instance = app.create();
        let mut output = run_frame(
            instance.as_mut(),
            &context,
            egui::RawInput::default(),
            &Theme::default(),
        );
        assert!(!output.shapes.is_empty(), "{} drew nothing", app.id);
        output.textures_delta.clear();
    }
}

#[test]
fn visuals_follow_theme_brightness() {
    let dark = Theme::default();
    assert!(visuals(&dark).dark_mode);
    let light = Theme {
        background: egui::Color32::from_rgb(250, 250, 250),
        ..dark
    };
    assert!(!visuals(&light).dark_mode);
    assert_eq!(visuals(&light).panel_fill, light.background);
}
