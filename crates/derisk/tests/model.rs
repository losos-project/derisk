//! Ask derisk's language model: the tools it gets, what the person is asked
//! about, and the conversation with the server, against a scripted server
//! and, when `DERISK_TEST_OLLAMA` names one, a real Ollama.

use std::collections::VecDeque;

use derisk::{
    action::Action,
    conversation::{Conversation, Source, StepStatus},
    geom::rect,
    ipc,
    model::{self, Config, Desktop, Ran, Server, ServerError, Tool},
    palette::{self, History},
    shell::Shell,
    snap::SnapZone,
    systemd::SessionOp,
};
use serde_json::{Value, json};

fn config() -> Config {
    Config {
        url: "http://localhost:11434".into(),
        model: "qwen3:4b-instruct".into(),
    }
}

#[test]
fn tools_parse_what_small_models_send() {
    // A window ID as a string, and null for "the focused window".
    assert_eq!(
        Tool::parse("snap_window", &json!({"zone": "left", "window": "4"})),
        Ok(Tool::Act(Action::Snap {
            window: Some(4),
            zone: SnapZone::Left
        }))
    );
    assert_eq!(
        Tool::parse("focus_window", &json!({"window": 4, "extra": null})),
        Ok(Tool::Act(Action::Focus { window: 4 }))
    );
    // Never "the focused window": the model names the one it means.
    assert_eq!(
        Tool::parse("close_window", &json!({"window": null})),
        Err("close_window needs \"window\" as a number".into())
    );
    // A boolean setting sent as one.
    assert_eq!(
        Tool::parse(
            "change_setting",
            &json!({"key": "top_bar.autohide", "value": true})
        ),
        Ok(Tool::ChangeSetting {
            key: "top_bar.autohide".into(),
            value: "true".into()
        })
    );
    // The palette is where the person asked, but a destructive operation
    // is still confirmed on screen: the model runs as an agent.
    assert_eq!(
        Tool::parse("session", &json!({"op": "reboot"})),
        Ok(Tool::Act(Action::Session {
            op: SessionOp::Reboot,
            confirmed: true
        }))
    );
    assert!(Tool::parse("snap_window", &json!({"zone": "sideways"})).is_err());
    assert!(Tool::parse("open_app", &json!({})).is_err());
    assert_eq!(
        Tool::parse("rm_rf", &json!({})),
        Err("there is no tool called rm_rf".into())
    );
}

#[test]
fn every_listed_tool_parses_and_has_a_label() {
    let tools = model::tools();
    let samples = json!({
        "app": "files", "window": 4, "zone": "left", "workspace": 2, "layout": "tall",
        "visible": true, "url": "https://example.org", "query": "nixos", "path": "/tmp",
        "key": "desktop.gaps", "value": "8", "element": 3, "text": "hi", "keys": "ctrl+s",
        "op": "lock",
    });
    for tool in tools.as_array().unwrap() {
        let function = &tool["function"];
        let name = function["name"].as_str().unwrap();
        let args: serde_json::Map<String, Value> = function["parameters"]["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(|k| (k.clone(), samples[k].clone()))
            .collect();
        let parsed = Tool::parse(name, &Value::Object(args));
        assert!(parsed.is_ok(), "{name}: {parsed:?}");
        assert!(!parsed.unwrap().label().is_empty(), "{name}");
    }
}

#[test]
fn clicks_typing_settings_files_and_session_operations_ask_first() {
    let asks = |name: &str, args: Value| Tool::parse(name, &args).unwrap().needs_approval();
    assert!(asks("click_element", json!({"element": 3})));
    assert!(asks("type_text", json!({"text": "hello"})));
    assert!(asks("press_keys", json!({"keys": "ctrl+s"})));
    assert!(asks(
        "change_setting",
        json!({"key": "appearance.scheme", "value": "light"})
    ));
    assert!(asks("open_file", json!({"path": "/home/me/notes.txt"})));
    assert!(asks("session", json!({"op": "lock"})));
    // Looking, opening apps and pages, and moving windows don't.
    assert!(!asks("get_desktop", json!({})));
    assert!(!asks("find_elements", json!({"name": "Save"})));
    assert!(!asks("open_app", json!({"app": "files"})));
    assert!(!asks("snap_window", json!({"zone": "right", "window": 2})));
    assert!(!asks("open_url", json!({"url": "https://example.org"})));
}

#[test]
fn live_tools_become_agent_requests() {
    assert_eq!(
        Tool::Click { element: 7 }.request(),
        Some(json!({"method": "act", "element": 7, "action": "click"}))
    );
    assert_eq!(
        Tool::PressKeys {
            keys: "ctrl+s".into()
        }
        .request(),
        Some(json!({"method": "input", "events": [{"type": "key", "key": "ctrl+s"}]}))
    );
    assert_eq!(Tool::GetDesktop.request(), None);
}

#[test]
fn the_desktop_reads_as_a_line_per_window() {
    let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
    let (files, _) = shell.map_window("org.derisk.files", "Home");
    shell.map_window("firefox", "News - Firefox");
    shell
        .apply(Action::MoveToWorkspace {
            window: Some(files.get()),
            workspace: 2,
        })
        .unwrap();
    let text = model::describe(&ipc::state(&shell), &shell.apps);
    assert!(text.starts_with("Workspace 1 (showing):\n"), "{text}");
    assert!(text.contains("\"News - Firefox\", focused\n"), "{text}");
    assert!(text.contains("Workspace 2:\n  window "), "{text}");
    assert!(text.contains("\"Home\"\n"), "{text}");
}

#[test]
fn calls_come_as_objects_or_as_json_strings() {
    let message = json!({"tool_calls": [
        {"function": {"name": "get_desktop", "arguments": {}}},
        {"function": {"name": "snap_window", "arguments": "{\"zone\":\"left\"}"}},
    ]});
    let calls: Vec<(String, Value)> = model::calls(&message).collect();
    assert_eq!(calls[0], ("get_desktop".into(), json!({})));
    assert_eq!(calls[1], ("snap_window".into(), json!({"zone": "left"})));
}

/// A server that answers from a script and remembers what it was sent.
#[derive(Default)]
struct Scripted {
    answers: VecDeque<Result<Value, ServerError>>,
    sent: Vec<Value>,
    pulled: Vec<String>,
}

impl Scripted {
    fn new(answers: impl IntoIterator<Item = Result<Value, ServerError>>) -> Self {
        Self {
            answers: answers.into_iter().collect(),
            ..Self::default()
        }
    }
}

fn says(text: &str) -> Result<Value, ServerError> {
    Ok(json!({"message": {"role": "assistant", "content": text}}))
}

fn calls(calls: &[(&str, Value)]) -> Result<Value, ServerError> {
    let calls: Vec<Value> = calls
        .iter()
        .map(|(name, args)| json!({"function": {"name": name, "arguments": args}}))
        .collect();
    Ok(json!({"message": {"role": "assistant", "content": "", "tool_calls": calls}}))
}

impl Server for Scripted {
    fn chat(&mut self, body: &Value) -> Result<Value, ServerError> {
        self.sent.push(body.clone());
        self.answers
            .pop_front()
            .unwrap_or_else(|| Err(ServerError::Other("script ran out".into())))
    }

    fn pull(&mut self, model: &str, progress: &mut dyn FnMut(f32)) -> Result<(), String> {
        self.pulled.push(model.to_owned());
        progress(0.5);
        progress(1.0);
        Ok(())
    }
}

/// The desktop as the session gives it to the model, on a headless shell:
/// its steps go into a turn of the conversation, and the person's answer
/// to every question is `allow`.
struct Headless {
    shell: Shell,
    turn: u64,
    allow: bool,
    ran: Vec<Tool>,
    working: Vec<String>,
}

impl Headless {
    fn new(request: &str) -> Self {
        let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
        shell.map_window("firefox", "News - Firefox");
        shell.map_window("org.derisk.files", "Home");
        let turn = shell.conversation.start_model(request, "Thinking…");
        Self {
            shell,
            turn,
            allow: true,
            ran: Vec::new(),
            working: Vec::new(),
        }
    }
}

impl Desktop for Headless {
    fn run(&mut self, tool: &Tool) -> Ran {
        let action = match tool {
            Tool::Act(a) => Some(a.clone()),
            _ => None,
        };
        let status = if tool.needs_approval() {
            StepStatus::Asking
        } else {
            StepStatus::Waiting
        };
        let step = self
            .shell
            .conversation
            .push_step(self.turn, tool.label(), action, status)
            .unwrap();
        if tool.needs_approval() && !self.allow {
            self.shell
                .conversation
                .set(step, StepStatus::Failed("You didn't allow it".into()));
            return Ran::Refused;
        }
        self.ran.push(tool.clone());
        let result = match tool {
            Tool::GetDesktop => Ok(model::describe(&ipc::state(&self.shell), &self.shell.apps)),
            Tool::Act(action) => {
                let outcome = self
                    .shell
                    .as_agent(|shell| shell.run_step(action.clone(), step));
                return match outcome.result {
                    Ok(()) => Ran::Done(model::did(action)),
                    Err(e) => Ran::Failed(e.to_string()),
                };
            }
            Tool::OpenApp { app } => {
                let (w, _) = self.shell.map_window(app, app);
                Ok(format!("Opened {app} as window {}.", w.get()))
            }
            _ => Ok("Done.".into()),
        };
        self.shell.conversation.set(step, StepStatus::Done);
        match result {
            Ok(text) => Ran::Done(text),
            Err(e) => Ran::Failed(e),
        }
    }

    fn working(&mut self, what: &str) {
        self.working.push(what.to_owned());
        self.shell.conversation.set_working(self.turn, what);
    }

    fn stopped(&self) -> bool {
        false
    }
}

fn window_of(shell: &Shell, app: &str) -> u64 {
    ipc::state(shell)
        .windows
        .iter()
        .find(|w| w.app_id == app)
        .map(|w| w.id)
        .unwrap()
}

#[test]
fn the_model_looks_acts_and_answers() {
    let mut desktop = Headless::new("put files on the left");
    let files = window_of(&desktop.shell, "org.derisk.files");
    let mut server = Scripted::new([
        calls(&[("get_desktop", json!({}))]),
        calls(&[("snap_window", json!({"zone": "left", "window": files}))]),
        says("Files is on the left."),
    ]);
    let answer = model::converse(
        &config(),
        "put files on the left",
        &mut server,
        &mut desktop,
    );
    assert_eq!(answer, Ok("Files is on the left.".into()));
    // The session ends the turn with it.
    let turn = desktop.turn;
    desktop.shell.conversation.finish(turn, answer);

    // The first request carries the instructions, the request and the tools.
    let first = &server.sent[0];
    assert_eq!(first["model"], "qwen3:4b-instruct");
    assert_eq!(first["messages"][0]["role"], "system");
    assert_eq!(first["messages"][1]["content"], "put files on the left");
    assert!(first["tools"].as_array().unwrap().len() > 10);
    // Each later one adds the call and what it returned.
    let second = server.sent[1]["messages"].as_array().unwrap();
    assert_eq!(second[3]["role"], "tool");
    assert_eq!(second[3]["tool_name"], "get_desktop");
    assert!(
        second[3]["content"]
            .as_str()
            .unwrap()
            .contains(&format!("window {files}: "))
    );

    // The turn shows a step per tool, done, then the answer.
    let shell = desktop.shell;
    let turn = shell.conversation.turns().last().unwrap();
    assert_eq!(turn.source, Source::User);
    let labels: Vec<&str> = turn.steps.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "Look at the open windows",
            &format!("Snap window {files} left")
        ]
    );
    assert!(turn.steps.iter().all(|s| s.status == StepStatus::Done));
    assert!(turn.is_settled());
    assert_eq!(turn.reply(), "Files is on the left.");
}

#[test]
fn a_missing_model_is_downloaded_first() {
    let mut desktop = Headless::new("hi");
    let mut server = Scripted::new([Err(ServerError::Missing), says("Hello.")]);
    let answer = model::converse(&config(), "hi", &mut server, &mut desktop);
    assert_eq!(answer, Ok("Hello.".into()));
    assert_eq!(server.pulled, ["qwen3:4b-instruct"]);
    assert!(
        desktop
            .working
            .contains(&"Downloading qwen3:4b-instruct… 50%".to_owned())
    );
    // Only once: a server that still lacks it after the download says so.
    let mut server = Scripted::new([Err(ServerError::Missing), Err(ServerError::Missing)]);
    let answer = model::converse(&config(), "hi", &mut server, &mut Headless::new("hi"));
    assert_eq!(
        answer,
        Err("http://localhost:11434 has no model called qwen3:4b-instruct".into())
    );
}

#[test]
fn a_step_the_person_refuses_is_not_run_and_the_model_hears_so() {
    let mut desktop = Headless::new("dark theme please");
    desktop.allow = false;
    let mut server = Scripted::new([
        calls(&[(
            "change_setting",
            json!({"key": "appearance.scheme", "value": "dark"}),
        )]),
        says("I didn't change it."),
    ]);
    let answer = model::converse(&config(), "dark theme please", &mut server, &mut desktop);
    assert_eq!(answer, Ok("I didn't change it.".into()));
    assert!(desktop.ran.is_empty());
    let told = &server.sent[1]["messages"][3]["content"];
    assert!(told.as_str().unwrap().contains("did not allow"), "{told}");
    let turn = desktop.shell.conversation.turns().last().unwrap();
    assert!(matches!(turn.steps[0].status, StepStatus::Failed(_)));
}

#[test]
fn bad_calls_go_back_to_the_model_as_errors() {
    let mut desktop = Headless::new("snap it");
    let mut server = Scripted::new([
        calls(&[("snap_window", json!({"zone": "sideways", "window": 1}))]),
        says("Sorry."),
    ]);
    model::converse(&config(), "snap it", &mut server, &mut desktop).unwrap();
    let told = server.sent[1]["messages"][3]["content"].as_str().unwrap();
    assert!(told.starts_with("Error: snap_window"), "{told}");
    assert!(desktop.ran.is_empty());
}

#[test]
fn a_model_that_loops_is_stopped() {
    let mut desktop = Headless::new("loop");
    let mut server =
        Scripted::new((0..=model::MAX_CALLS).map(|_| calls(&[("get_desktop", json!({}))])));
    let answer = model::converse(&config(), "loop", &mut server, &mut desktop);
    assert!(answer.unwrap_err().starts_with("Stopped after"));
    assert_eq!(desktop.ran.len(), model::MAX_CALLS);
}

#[test]
fn a_model_turn_is_pending_until_it_answers() {
    let mut conversation = Conversation::default();
    let turn = conversation.start_model("what is open?", "Thinking…");
    let t = conversation.turn(turn).unwrap();
    assert!(!t.is_settled());
    assert_eq!(t.reply(), "Thinking…");
    let step = conversation
        .push_step(turn, "Click element 3".into(), None, StepStatus::Asking)
        .unwrap();
    assert_eq!(
        conversation.turn(turn).unwrap().reply(),
        "Waiting for you to allow the step above…"
    );
    conversation.set(step, StepStatus::Done);
    conversation.finish(turn, Ok("Firefox and Files.".into()));
    let t = conversation.turn(turn).unwrap();
    assert!(t.is_settled());
    assert_eq!(t.reply(), "Firefox and Files.");
    // A turn that is gone takes no more steps.
    conversation.clear();
    assert!(
        conversation
            .push_step(turn, "x".into(), None, StepStatus::Done)
            .is_none()
    );
}

#[test]
fn the_palette_offers_what_the_rules_dont_follow_to_the_model() {
    let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
    shell.map_window("firefox", "News - Firefox");
    let entries = palette::entries(&shell, &[], &[]);
    // "news" matches the window, so the rules' row would only show when
    // nothing else did; offered to the model, it shows with the window.
    let query = "news";
    let hits = palette::search(&entries, query, &History::default());
    assert!(!hits.is_empty());
    let mut rows = palette::rows(&shell, query);
    assert!(rows.ask.as_ref().unwrap().actions.is_empty());
    rows.offer_model("qwen3:4b-instruct");
    assert!(rows.model);
    let listed = palette::list(&entries, &hits, &rows, query);
    let ask = listed
        .iter()
        .find(|e| e.category == palette::Category::Ask)
        .expect("the model's row is listed");
    assert_eq!(ask.detail, "Press Enter to ask qwen3:4b-instruct");
    // A request the rules follow stays theirs.
    let mut rows = palette::rows(&shell, "open files");
    rows.offer_model("qwen3:4b-instruct");
    assert!(!rows.model);
    assert_eq!(rows.ask.unwrap().detail, "1 step");
}

/// The real thing: `DERISK_TEST_OLLAMA=http://localhost:11434 cargo test
/// --features host -p derisk --test model -- --ignored`, with the default
/// model pulled (or pulled by the test).
#[cfg(feature = "host")]
#[test]
#[ignore = "needs an Ollama server"]
fn a_real_model_snaps_windows() {
    let Ok(url) = std::env::var("DERISK_TEST_OLLAMA") else {
        return;
    };
    let config = Config {
        url: url.clone(),
        model: std::env::var("DERISK_TEST_MODEL")
            .unwrap_or_else(|_| derisk_settings::AssistantPrefs::DEFAULT_MODEL.into()),
    };
    let request = "put firefox on the left and files on the right";
    let mut desktop = Headless::new(request);
    let mut server = model::Http::new(&url);
    let answer = model::converse(&config, request, &mut server, &mut desktop);
    eprintln!(
        "{answer:?}\n{:#?}\n{}",
        desktop.ran,
        model::describe(&ipc::state(&desktop.shell), &desktop.shell.apps)
    );
    assert!(answer.is_ok());
    let placements = desktop.shell.placements();
    let left_of = |app: &str| {
        let id = window_of(&desktop.shell, app);
        placements
            .iter()
            .find(|p| p.window.get() == id)
            .unwrap()
            .frame
            .loc
            .x
    };
    assert!(left_of("firefox") < left_of("org.derisk.files"));
}
