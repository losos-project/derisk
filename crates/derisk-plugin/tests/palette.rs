//! The bundled palette plugins, loaded and called through the sandbox.

use std::time::Instant;

use derisk_plugin::palette::{
    App, Category, Desk, File, Hook, Input, Plugins, Registration, View, Workspace,
};

fn interpret(text: &str) -> Result<Vec<String>, String> {
    match text {
        "lock" => Ok(vec![r#"{"action":"session","op":"lock"}"#.into()]),
        _ => Err(format!("I don't know how to \"{text}\"")),
    }
}

fn plugins() -> Plugins {
    Plugins::bundled(interpret).expect("the bundled plugins load")
}

fn view() -> View {
    View {
        apps: vec![App {
            id: "files".into(),
            name: "Files".into(),
            summary: "Browse files".into(),
            icon: "🗂".into(),
            keywords: "files folder".into(),
            actions: Vec::new(),
        }],
        desk: Desk {
            workspaces: vec![
                Workspace {
                    number: 1,
                    windows: 0,
                    active: true,
                },
                Workspace {
                    number: 2,
                    windows: 0,
                    active: false,
                },
            ],
            window_focused: false,
            can_add_workspace: true,
        },
        files: (0..20_000)
            .map(|i| File {
                path: format!("/home/me/file{i}.txt"),
                shown: format!("~/file{i}.txt"),
                folder: false,
            })
            .collect(),
        search_engine: Some("DuckDuckGo".into()),
        registered: vec![Registration {
            source: "danube".into(),
            data: r#"{"tabs":[{"id":"1","title":"Example","url":"https://example.com"}]}"#.into(),
        }],
        ..View::default()
    }
}

#[test]
fn every_bundled_plugin_loads_with_a_manifest() {
    let started = Instant::now();
    let plugins = plugins();
    eprintln!(
        "compiled {} plugins in {:?}",
        plugins.len(),
        started.elapsed()
    );
    let names: Vec<_> = plugins.iter().map(|p| p.name().to_owned()).collect();
    assert_eq!(
        names,
        [
            "apps",
            "windows",
            "menus",
            "commands",
            "workspaces",
            "session",
            "power",
            "system",
            "browser",
            "machines",
            "files",
            "assistant",
            "agent",
            "web"
        ]
    );
    // Only the files plugin sees file names.
    let readers: Vec<_> = plugins
        .iter()
        .filter(|p| p.reads(Input::Files))
        .map(|p| p.name())
        .collect();
    assert_eq!(readers, ["files"]);
}

#[test]
fn the_catalog_comes_from_the_plugins() {
    let plugins = plugins();
    let view = view();
    let started = Instant::now();
    let answers = plugins.each(|_, p| p.has(Hook::Entries), |p| plugins.entries(p, &view));
    eprintln!("catalog in {:?}", started.elapsed());
    let rows: Vec<_> = answers.into_iter().flat_map(|(_, rows)| rows).collect();
    let has = |category: Category, title: &str| {
        rows.iter()
            .any(|r| r.category == category && r.title == title)
    };
    assert!(has(Category::App, "Files"));
    assert!(has(Category::Command, "Overview"));
    assert!(has(Category::Workspace, "Go to Workspace 2"));
    assert!(has(Category::Session, "Shut Down"));
    assert!(has(Category::Tab, "Example"));
    assert!(has(Category::File, "file19999.txt"));
    let shut_down = rows.iter().find(|r| r.title == "Shut Down").unwrap();
    assert!(shut_down.confirm);
}

#[test]
fn query_rows_use_the_assistant() {
    let plugins = plugins();
    let view = view();
    let ask = |text: &str| -> Vec<_> {
        plugins
            .each(|_, p| p.has(Hook::Query), |p| plugins.query(p, &view, text))
            .into_iter()
            .flat_map(|(_, rows)| rows)
            .map(|r| (r.category, r.title))
            .collect()
    };
    assert_eq!(
        ask("lock"),
        [
            (Category::Ask, "Ask derisk: lock".to_owned()),
            (Category::Web, "Search the web for “lock”".to_owned()),
        ]
    );
    assert_eq!(
        ask("frob"),
        [
            (Category::Ask, "Ask derisk: frob".to_owned()),
            (Category::Agent, "Ask the agent: frob".to_owned()),
            (Category::Web, "Search the web for “frob”".to_owned()),
        ]
    );
    assert_eq!(ask("https://example.org")[0].1, "Open https://example.org");
}

#[test]
fn a_plugin_sees_only_what_it_asked_for() {
    let plugins = plugins();
    let view = view();
    let web = plugins.iter().find(|p| p.name() == "web").unwrap();
    let seen = web.view(&view);
    assert!(seen.files.is_empty() && seen.apps.is_empty() && seen.registered.is_empty());
    assert_eq!(seen.search_engine.as_deref(), Some("DuckDuckGo"));
}

#[test]
#[ignore = "timing, run with --release --ignored --nocapture"]
fn files_catalog_timing() {
    let plugins = plugins();
    let view = view();
    let files = plugins.iter().find(|p| p.name() == "files").unwrap();
    for _ in 0..3 {
        let started = Instant::now();
        let rows = plugins.entries(files, &view).unwrap();
        eprintln!("files: {} rows in {:?}", rows.len(), started.elapsed());
    }
}
