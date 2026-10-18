//! The Agent's window: the left column for projects, chats, routines and
//! artifacts, the chat in the middle, and the live preview, pull requests and
//! MCP servers on the right.

// Processes started from here (xdg-open, the agent, previews) are spawned and
// left to run; none is waited on from the UI thread.
#![allow(clippy::disallowed_methods)]

use std::{
    collections::HashMap,
    io::{BufRead as _, BufReader, Write as _},
    os::unix::net::UnixListener,
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant, SystemTime},
};

use chrono::Utc;
use derisk_agent_preview::{Preview, PreviewSource, PreviewStatus, PreviewTheme};
use egui::{Align, Color32, Layout, RichText, ScrollArea, Ui};
use mcsapi_components::{
    Badge, BadgeVariant, Button, ButtonSize, ButtonVariant, Card, Empty, Input, Select, Spinner,
    Switch, Tabs, Textarea, Toaster, ToggleGroup, Tokens, toast, typography,
};
use mcsapi_ui::Theme;
use serde_json::{Value, json};

use crate::{
    agent::{self, AgentEvent, Turn},
    code_view::CodeView,
    forge::{self, ForgeRepo, PullRequest},
    mcp::{self, ToolInfo},
    routines,
    setup::{self, AppSetup, SetupAction, SetupForm},
    store::{
        Artifact, ArtifactKind, Chat, Entry, McpServer, McpTransport, PermissionMode, Project,
        Role, Routine, Store,
    },
    tools,
};

const LEFT_WIDTH: f32 = 264.0;
const STORE_POLL: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Page {
    NewChat,
    Chat(String),
    Project(String),
    Routine(String),
    Artifact(String),
    Setup,
}

/// What the middle of the window shows: the agent's conversation, or the
/// project's code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CenterView {
    Chat = 0,
    Code = 1,
}

const CENTER_VIEWS: [&str; 2] = ["Chat", "Code"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RightTab {
    Preview = 0,
    PullRequests = 1,
    Mcp = 2,
}

const RIGHT_TABS: [&str; 3] = ["Preview", "Pull requests", "MCP"];

enum Job {
    Pulls(Vec<FolderPulls>),
    Tools {
        server: String,
        result: Result<Vec<ToolInfo>, String>,
    },
    Routine {
        name: String,
        result: Result<String, String>,
    },
}

struct FolderPulls {
    folder: PathBuf,
    result: Result<(ForgeRepo, Vec<PullRequest>), String>,
}

struct RunningTurn {
    turn: Turn,
}

struct ControlRequest {
    request: Value,
    reply: mpsc::Sender<Value>,
}

/// The Agent's whole window, as an mcsapi app, which `derisk-agent` hosts in a
/// window of its own: the agents it starts are processes and threads that
/// should not share derisk's.
pub struct AgentApp {
    store: Store,
    projects: Vec<Project>,
    chats: Vec<Chat>,
    routines: Vec<Routine>,
    artifacts: Vec<Artifact>,
    store_modified: Option<SystemTime>,
    last_poll: Instant,
    project_id: Option<String>,
    page: Page,
    composer: String,
    turns: HashMap<String, RunningTurn>,
    /// Each chat's MCP server statuses from Claude Code's last start.
    servers: HashMap<String, Vec<(String, String)>>,
    right_tab: usize,
    preview: Option<Preview>,
    preview_error: Option<String>,
    pulls: Option<Vec<FolderPulls>>,
    pulls_loading: bool,
    pulls_project: Option<String>,
    tool_lists: HashMap<String, Result<Vec<ToolInfo>, String>>,
    new_server_name: String,
    new_server_target: String,
    new_folder: String,
    setup: Option<SetupForm>,
    view: CenterView,
    code: CodeView,
    context: Option<egui::Context>,
    jobs: (mpsc::Sender<Job>, mpsc::Receiver<Job>),
    control: Option<mpsc::Receiver<ControlRequest>>,
    error: Option<String>,
}

impl AgentApp {
    pub fn new(store: Store) -> Self {
        let mut app = Self {
            store,
            projects: Vec::new(),
            chats: Vec::new(),
            routines: Vec::new(),
            artifacts: Vec::new(),
            store_modified: None,
            last_poll: Instant::now(),
            project_id: None,
            page: Page::NewChat,
            composer: String::new(),
            turns: HashMap::new(),
            servers: HashMap::new(),
            right_tab: RightTab::Preview as usize,
            preview: None,
            preview_error: None,
            pulls: None,
            pulls_loading: false,
            pulls_project: None,
            tool_lists: HashMap::new(),
            new_server_name: String::new(),
            new_server_target: String::new(),
            new_folder: String::new(),
            setup: None,
            view: CenterView::Chat,
            code: CodeView::default(),
            context: None,
            jobs: mpsc::channel(),
            control: None,
            error: None,
        };
        app.reload();
        app.project_id = app.projects.first().map(|project| project.id.clone());
        app
    }

    /// Opens a new chat with `text` typed in for the person to send: the
    /// palette's "Ask the agent", which never sends on its own, so nothing
    /// runs on a row's say-so.
    pub fn ask(&mut self, text: &str) {
        self.page = Page::NewChat;
        self.view = CenterView::Chat;
        self.composer = text.trim().to_owned();
        if let Some(context) = &self.context {
            context.send_viewport_cmd(egui::ViewportCommand::Focus);
            context.request_repaint();
        }
    }

    fn reload(&mut self) {
        let result = (|| -> miette::Result<()> {
            self.projects = self.store.projects()?;
            self.chats = self.store.chats()?;
            self.routines = self.store.routines()?;
            self.artifacts = self.store.artifacts()?;
            Ok(())
        })();
        if let Err(error) = result {
            self.error = Some(crate::describe(&error));
        }
        self.store_modified = self.store.modified();
    }

    fn report(&mut self, result: miette::Result<()>) {
        if let Err(error) = result {
            self.error = Some(crate::describe(&error));
        }
    }

    fn project(&self) -> Option<&Project> {
        let id = self.project_id.as_deref()?;
        self.projects.iter().find(|project| project.id == id)
    }

    fn chat_mut(&mut self, id: &str) -> Option<&mut Chat> {
        self.chats.iter_mut().find(|chat| chat.id == id)
    }

    fn spawn_job(&self, work: impl FnOnce() -> Job + Send + 'static) {
        let sender = self.jobs.0.clone();
        let context = self.context.clone();
        std::thread::spawn(move || {
            if sender.send(work()).is_ok()
                && let Some(context) = context
            {
                context.request_repaint();
            }
        });
    }

    /// Everything that happened off the UI thread since the last frame.
    fn poll(&mut self, context: &egui::Context) {
        if self.context.is_none() {
            self.context = Some(context.clone());
            self.control = start_control_server(context);
        }

        while let Ok(job) = self.jobs.1.try_recv() {
            match job {
                Job::Pulls(pulls) => {
                    self.pulls = Some(pulls);
                    self.pulls_loading = false;
                }
                Job::Tools { server, result } => {
                    self.tool_lists.insert(server, result);
                }
                Job::Routine { name, result } => match result {
                    Ok(message) => toast(context, name, Some(message)),
                    Err(error) => self.error = Some(error),
                },
            }
        }

        let mut finished = Vec::new();
        let mut events = Vec::new();
        for (chat_id, running) in &self.turns {
            while let Ok(event) = running.turn.events.try_recv() {
                if matches!(event, AgentEvent::Finished { .. }) {
                    finished.push(chat_id.clone());
                }
                events.push((chat_id.clone(), event));
            }
        }
        for (chat_id, event) in events {
            self.apply_agent_event(&chat_id, event);
        }
        for chat_id in finished {
            self.turns.remove(&chat_id);
        }

        if let Some(control) = &self.control {
            let requests: Vec<_> = control.try_iter().collect();
            for request in requests {
                let reply = self.handle_control(context, &request.request);
                request.reply.send(reply).ok();
            }
        }

        if self.last_poll.elapsed() >= STORE_POLL {
            self.last_poll = Instant::now();
            let modified = self.store.modified();
            // A turn's own saves would reload over the transcript being
            // streamed, so wait for the turns to end.
            if modified != self.store_modified && self.turns.is_empty() {
                self.reload();
            }
            context.request_repaint_after(STORE_POLL);
        }
    }

    fn apply_agent_event(&mut self, chat_id: &str, event: AgentEvent) {
        let event = match event {
            AgentEvent::Started {
                session_id,
                servers,
            } => {
                self.servers.insert(chat_id.to_owned(), servers);
                AgentEvent::Started {
                    session_id,
                    servers: Vec::new(),
                }
            }
            event => event,
        };
        let Some(chat) = self.chat_mut(chat_id) else {
            return;
        };
        match event {
            AgentEvent::Started { session_id, .. } => {
                chat.session_id = Some(session_id);
            }
            AgentEvent::Entry(entry) => chat.entries.push(entry),
            AgentEvent::Finished { error } => {
                if let Some(error) = error {
                    chat.entries.push(Entry {
                        role: Role::Error,
                        text: error,
                        at: Utc::now(),
                    });
                }
            }
        }
        chat.updated = Utc::now();
        let chat = chat.clone();
        let result = self.store.save_chat(&chat);
        self.report(result);
        self.store_modified = self.store.modified();
    }

    fn handle_control(&mut self, context: &egui::Context, request: &Value) -> Value {
        match request["method"].as_str() {
            Some("preview_run") => {
                match serde_json::from_value::<PreviewSource>(request["source"].clone()) {
                    Ok(source) => {
                        self.run_preview(context, source);
                        self.right_tab = RightTab::Preview as usize;
                        match &self.preview_error {
                            Some(error) => json!({"ok": false, "error": error}),
                            None => json!({"ok": true}),
                        }
                    }
                    Err(error) => json!({"ok": false, "error": error.to_string()}),
                }
            }
            Some("preview_status") => match &mut self.preview {
                Some(preview) => {
                    let status = match preview.status() {
                        PreviewStatus::Starting => "starting".to_owned(),
                        PreviewStatus::Running { title } => format!("running: {title}"),
                        PreviewStatus::Exited { message } => format!("exited: {message}"),
                    };
                    json!({"ok": true, "source": preview.source().label(), "status": status,
                        "frames_drawn": preview.frames(), "stderr": preview.stderr()})
                }
                None => json!({"ok": true, "status": "nothing is running",
                    "last_error": self.preview_error}),
            },
            Some("store_changed") => {
                self.reload();
                json!({"ok": true})
            }
            Some("ask") => {
                self.ask(request["text"].as_str().unwrap_or_default());
                json!({"ok": true})
            }
            _ => json!({"ok": false, "error": "unknown method"}),
        }
    }

    fn run_preview(&mut self, context: &egui::Context, source: PreviewSource) {
        if let Some(mut old) = self.preview.take() {
            old.free_textures(context);
        }
        match Preview::spawn(source.clone(), context) {
            Ok(preview) => {
                self.preview = Some(preview);
                self.preview_error = None;
            }
            Err(error) => {
                self.preview_error = Some(format!("could not start {}: {error}", source.label()))
            }
        }
    }

    fn send(&mut self) {
        let prompt = self.composer.trim().to_owned();
        if prompt.is_empty() {
            return;
        }
        let Some(project) = self.project().cloned() else {
            self.error =
                Some("Create a project first: it says which folders the agent works in.".into());
            return;
        };
        let chat_id = match &self.page {
            Page::Chat(id) if !self.turns.contains_key(id) => id.clone(),
            Page::Chat(_) => return,
            _ => {
                let title: String = prompt
                    .lines()
                    .next()
                    .unwrap_or("New chat")
                    .chars()
                    .take(60)
                    .collect();
                match self.store.new_chat(&project.id, &title) {
                    Ok(chat) => {
                        let id = chat.id.clone();
                        self.chats.insert(0, chat);
                        self.page = Page::Chat(id.clone());
                        id
                    }
                    Err(error) => {
                        self.error = Some(crate::describe(&error));
                        return;
                    }
                }
            }
        };
        let Some(chat) = self.chat_mut(&chat_id) else {
            return;
        };
        chat.entries.push(Entry {
            role: Role::User,
            text: prompt.clone(),
            at: Utc::now(),
        });
        chat.updated = Utc::now();
        let chat = chat.clone();
        let context = self.context.clone();
        let started = self.store.save_chat(&chat).and_then(|()| {
            agent::start(&project, &chat, &prompt, &self.store, move || {
                if let Some(context) = &context {
                    context.request_repaint();
                }
            })
        });
        match started {
            Ok(turn) => {
                self.turns.insert(chat_id, RunningTurn { turn });
                self.composer.clear();
            }
            Err(error) => self.error = Some(crate::describe(&error)),
        }
    }

    fn refresh_pulls(&mut self) {
        let Some(project) = self.project() else {
            return;
        };
        let folders = project.folders.clone();
        self.pulls_project = Some(project.id.clone());
        self.pulls_loading = true;
        self.spawn_job(move || {
            Job::Pulls(
                folders
                    .into_iter()
                    .map(|folder| {
                        let result = forge::repo_for_folder(&folder)
                            .and_then(|repo| {
                                let pulls = forge::open_pull_requests(&repo)?;
                                Ok((repo, pulls))
                            })
                            .map_err(|error| crate::describe(&error));
                        FolderPulls { folder, result }
                    })
                    .collect(),
            )
        });
    }

    fn list_tools(&mut self, server: &McpServer) {
        let name = server.name.clone();
        let transport = server.transport.clone();
        self.tool_lists.remove(&name);
        self.spawn_job(move || Job::Tools {
            server: name,
            result: mcp::list_tools(&transport).map_err(|error| crate::describe(&error)),
        });
    }

    fn update_project(&mut self, edit: impl FnOnce(&mut Project)) {
        let Some(id) = self.project_id.clone() else {
            return;
        };
        let result = self.store.update_projects(|projects| {
            if let Some(project) = projects.iter_mut().find(|project| project.id == id) {
                edit(project);
            }
        });
        self.report(result);
        self.reload();
    }
}

impl mcsapi_ui::App for AgentApp {
    fn title(&self) -> &str {
        "Agent"
    }

    fn ui(&mut self, ui: &mut Ui, theme: &Theme) {
        let tokens = Tokens::from_theme(theme);
        tokens.install(ui.ctx());
        self.poll(&ui.ctx().clone());

        let panel = |fill: Color32| {
            egui::Frame::new()
                .fill(fill)
                .inner_margin(egui::Margin::same(12))
        };
        egui::Panel::left("agent-left")
            .resizable(true)
            .default_size(LEFT_WIDTH)
            .size_range(200.0..=420.0)
            .frame(panel(tokens.card))
            .show(ui, |ui| self.left_column(ui, &tokens));
        egui::Panel::right("agent-right")
            .resizable(true)
            .default_size(ui.available_width() * 0.42)
            .size_range(280.0..=ui.available_width() * 0.7)
            .frame(panel(tokens.background.lerp_to_gamma(tokens.card, 0.35)))
            .show(ui, |ui| self.right_side(ui, &tokens, theme));
        egui::CentralPanel::default()
            .frame(panel(tokens.background))
            .show(ui, |ui| self.center(ui, &tokens));

        if let Some(error) = self.error.take() {
            toast(ui.ctx(), "Something went wrong", Some(error));
        }
        Toaster::show(ui.ctx());
    }
}

fn section_header(ui: &mut Ui, tokens: &Tokens, title: &str) -> bool {
    let mut add = false;
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.label(typography::small(tokens, title.to_uppercase()).color(tokens.muted_foreground));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            add = ui
                .add(
                    Button::new("+")
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Sm),
                )
                .on_hover_text(format!(
                    "New {}",
                    title.trim_end_matches('s').to_lowercase()
                ))
                .clicked();
        });
    });
    add
}

fn nav_item(
    ui: &mut Ui,
    tokens: &Tokens,
    selected: bool,
    icon: &str,
    text: &str,
    trailing: Option<&str>,
) -> bool {
    let fill = if selected {
        tokens.hover
    } else {
        Color32::TRANSPARENT
    };
    let response = egui::Frame::new()
        .fill(fill)
        .corner_radius(tokens.radius)
        .inner_margin(egui::Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon).color(tokens.muted_foreground));
                // The trailing text first, right to left, so the title truncates
                // into what is left instead of widening the column.
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if let Some(trailing) = trailing {
                        ui.label(
                            typography::small(tokens, trailing).color(tokens.muted_foreground),
                        );
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(text).color(tokens.foreground))
                                .truncate()
                                .selectable(false),
                        );
                    });
                });
            })
        })
        .response
        .interact(egui::Sense::click());
    if response.hovered() && !selected {
        ui.painter().rect_filled(
            response.rect,
            tokens.radius,
            tokens.hover.gamma_multiply(0.5),
        );
    }
    response.clicked()
}

impl AgentApp {
    fn left_column(&mut self, ui: &mut Ui, tokens: &Tokens) {
        ui.horizontal(|ui| {
            ui.label(typography::h4(tokens, "Agent"));
        });
        ui.add_space(8.0);
        if ui
            .add_sized([ui.available_width(), 36.0], Button::new("New chat"))
            .clicked()
        {
            self.page = Page::NewChat;
        }

        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            if section_header(ui, tokens, "Projects") {
                self.open_setup(AppSetup::default(), None);
            }
            if self.projects.is_empty() {
                ui.label(typography::muted(
                    tokens,
                    "A project holds instructions, folders and MCP servers, like a Claude project.",
                ));
            }
            for project in self.projects.clone() {
                let selected = self.project_id.as_deref() == Some(&project.id);
                let folders = project.folders.len().to_string();
                if nav_item(ui, tokens, selected, "▣", &project.name, Some(&folders)) {
                    if selected {
                        self.page = Page::Project(project.id.clone());
                    }
                    self.project_id = Some(project.id.clone());
                    self.pulls = None;
                }
            }

            let project_id = self.project_id.clone();
            if section_header(ui, tokens, "Chats") {
                self.page = Page::NewChat;
            }
            let chats: Vec<_> = self
                .chats
                .iter()
                .filter(|chat| Some(&chat.project_id) == project_id.as_ref())
                .map(|chat| {
                    (
                        chat.id.clone(),
                        chat.title.clone(),
                        chat.routine_id.is_some(),
                    )
                })
                .collect();
            if chats.is_empty() {
                ui.label(typography::muted(tokens, "No chats in this project yet."));
            }
            for (id, title, from_routine) in chats {
                let selected = self.page == Page::Chat(id.clone());
                let running = self.turns.contains_key(&id).then_some("●");
                let icon = if from_routine { "⟳" } else { "💬" };
                if nav_item(ui, tokens, selected, icon, &title, running) {
                    self.page = Page::Chat(id);
                }
            }

            if section_header(ui, tokens, "Routines") {
                if let Some(project_id) = project_id.clone() {
                    let routine = Routine {
                        id: crate::store::new_id(),
                        project_id,
                        name: "New routine".into(),
                        prompt: String::new(),
                        schedule: "daily".into(),
                        enabled: false,
                        last_run: None,
                    };
                    let id = routine.id.clone();
                    let result = self
                        .store
                        .update_routines(|routines| routines.push(routine));
                    self.report(result.map(|_| ()));
                    self.reload();
                    self.page = Page::Routine(id);
                } else {
                    self.error = Some("Create a project first.".into());
                }
            }
            let routines: Vec<_> = self
                .routines
                .iter()
                .filter(|routine| Some(&routine.project_id) == project_id.as_ref())
                .cloned()
                .collect();
            if routines.is_empty() {
                ui.label(typography::muted(
                    tokens,
                    "Prompts that run on a schedule, as systemd timers.",
                ));
            }
            for routine in routines {
                let selected = self.page == Page::Routine(routine.id.clone());
                let state = if routine.enabled {
                    routine.schedule.as_str()
                } else {
                    "off"
                };
                if nav_item(ui, tokens, selected, "⏰", &routine.name, Some(state)) {
                    self.page = Page::Routine(routine.id);
                }
            }

            section_header(ui, tokens, "Artifacts");
            let artifacts: Vec<_> = self
                .artifacts
                .iter()
                .rev()
                .filter(|artifact| Some(&artifact.project_id) == project_id.as_ref())
                .cloned()
                .collect();
            if artifacts.is_empty() {
                ui.label(typography::muted(
                    tokens,
                    "Apps and files the agent makes show up here.",
                ));
            }
            for artifact in artifacts {
                let selected = self.page == Page::Artifact(artifact.id.clone());
                let (icon, kind) = match &artifact.kind {
                    ArtifactKind::App {
                        source: PreviewSource::Wasm { .. },
                    } => ("▶", "wasm"),
                    ArtifactKind::App { .. } => ("▶", "app"),
                    ArtifactKind::File { .. } => ("📄", "file"),
                };
                if nav_item(ui, tokens, selected, icon, &artifact.title, Some(kind)) {
                    if let ArtifactKind::App { source } = &artifact.kind {
                        let context = ui.ctx().clone();
                        self.run_preview(&context, source.clone());
                        self.right_tab = RightTab::Preview as usize;
                    }
                    self.page = Page::Artifact(artifact.id);
                }
            }
        });
    }

    fn open_setup(&mut self, setup: AppSetup, project_id: Option<String>) {
        self.setup = Some(SetupForm::new(setup, project_id));
        self.page = Page::Setup;
        self.view = CenterView::Chat;
    }

    /// Creates the wizard's project, or updates the one it set up again, and
    /// sends its prompt as the project's first chat.
    fn finish_setup(&mut self, form: SetupForm) {
        let setup = form.setup;
        let folder = setup.folder();
        if let Err(error) = setup.write_files() {
            self.error = Some(format!("could not set up {}: {error}", folder.display()));
            self.setup = Some(SetupForm { setup, ..form });
            return;
        }
        let result = match &form.project_id {
            Some(id) => self
                .store
                .update_projects(|projects| {
                    let project = projects.iter_mut().find(|project| &project.id == id)?;
                    // Instructions the user wrote themselves stay; ones the
                    // wizard wrote follow the new answers.
                    let generated = project.app.as_ref().map(AppSetup::instructions);
                    if project.instructions.trim().is_empty()
                        || Some(&project.instructions) == generated.as_ref()
                    {
                        project.instructions = setup.instructions();
                    }
                    if !project.folders.contains(&folder) {
                        project.folders.insert(0, folder.clone());
                    }
                    project.app = Some(setup.clone());
                    Some(project.id.clone())
                })
                .and_then(|id| id.ok_or_else(|| miette::miette!("the project was deleted"))),
            None => self
                .store
                .new_project(&setup.display_name(), vec![folder])
                .and_then(|project| {
                    self.store.update_projects(|projects| {
                        if let Some(stored) =
                            projects.iter_mut().find(|stored| stored.id == project.id)
                        {
                            stored.instructions = setup.instructions();
                            stored.app = Some(setup.clone());
                        }
                    })?;
                    Ok(project.id)
                }),
        };
        match result {
            Ok(id) => {
                self.reload();
                self.project_id = Some(id);
                self.pulls = None;
                self.page = Page::NewChat;
                self.composer = form.prompt;
                self.send();
            }
            Err(error) => self.error = Some(crate::describe(&error)),
        }
    }

    fn center(&mut self, ui: &mut Ui, tokens: &Tokens) {
        if self.page == Page::Setup {
            let Some(mut form) = self.setup.take() else {
                self.page = Page::NewChat;
                return;
            };
            match setup::show(&mut form, ui, tokens) {
                Some(SetupAction::Cancel) => self.page = Page::NewChat,
                Some(SetupAction::Start) => self.finish_setup(form),
                None => self.setup = Some(form),
            }
            return;
        }
        ui.horizontal(|ui| {
            let mut selected = Some(self.view as usize);
            let unsaved = self.code.has_unsaved_changes();
            if ui
                .add(ToggleGroup::new(&mut selected, &CENTER_VIEWS))
                .on_hover_text("Switch between the agent's chat and the project's code")
                .changed()
            {
                // A second click on the pressed item unpresses it; the view
                // stays as it was.
                self.view = match selected {
                    Some(1) => CenterView::Code,
                    Some(_) => CenterView::Chat,
                    None => self.view,
                };
            }
            if unsaved && self.view == CenterView::Chat {
                ui.add(Badge::new("Unsaved code").variant(BadgeVariant::Secondary));
            }
        });
        ui.add_space(4.0);
        if self.view == CenterView::Code {
            self.code.set_root(
                self.project()
                    .and_then(|project| project.folders.first().cloned()),
            );
            let result = self.code.show(ui, tokens);
            self.report(result);
            return;
        }
        match self.page.clone() {
            Page::Setup => {}
            Page::NewChat => self.new_chat_page(ui, tokens),
            Page::Chat(id) => self.chat_page(ui, tokens, &id),
            Page::Project(id) => self.project_page(ui, tokens, &id),
            Page::Routine(id) => self.routine_page(ui, tokens, &id),
            Page::Artifact(id) => self.artifact_page(ui, tokens, &id),
        }
    }

    fn composer(&mut self, ui: &mut Ui, tokens: &Tokens, busy: bool) {
        Card::new().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let response = ui.add(
                Textarea::new(&mut self.composer)
                    .placeholder("Ask the agent to build, fix or explain something…")
                    .rows(3),
            );
            let enter = response.has_focus()
                && ui.input(|input| input.key_pressed(egui::Key::Enter) && !input.modifiers.shift);
            ui.horizontal(|ui| {
                if let Some(project) = self.project() {
                    ui.add(Badge::new(&project.name).variant(BadgeVariant::Secondary));
                    ui.label(
                        typography::small(tokens, project.permission_mode.label())
                            .color(tokens.muted_foreground),
                    );
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if busy {
                        if ui
                            .add(
                                Button::new("Stop")
                                    .variant(ButtonVariant::Destructive)
                                    .size(ButtonSize::Sm),
                            )
                            .clicked()
                            && let Page::Chat(id) = &self.page
                            && let Some(running) = self.turns.get_mut(id)
                        {
                            running.turn.stop();
                        }
                        ui.add(Spinner::new().size(16.0));
                    } else if ui.add(Button::new("Send").size(ButtonSize::Sm)).clicked() || enter {
                        if enter {
                            // The Enter that sent the message also typed a newline.
                            let trimmed = self.composer.trim_end_matches('\n').len();
                            self.composer.truncate(trimmed);
                        }
                        self.send();
                    }
                });
            });
        });
    }

    fn new_chat_page(&mut self, ui: &mut Ui, tokens: &Tokens) {
        ui.add_space(ui.available_height() * 0.22);
        ui.vertical_centered(|ui| {
            ui.label(typography::h2(tokens, "What should we build?"));
            ui.add_space(4.0);
            let hint = match self.project() {
                Some(project) if project.folders.is_empty() => format!(
                    "{} has no folders yet; the agent will work in the Agent's data folder.",
                    project.name
                ),
                Some(project) => format!("Working in {}", project.folders[0].display()),
                None => "Set up a project to start.".to_owned(),
            };
            ui.label(typography::muted(tokens, hint));
            ui.add_space(8.0);
            if ui
                .add(
                    Button::new("Set up a new app")
                        .variant(ButtonVariant::Outline)
                        .size(ButtonSize::Sm),
                )
                .clicked()
            {
                self.open_setup(AppSetup::default(), None);
            }
        });
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            ui.add_space((ui.available_width() - 640.0).max(0.0) / 2.0);
            ui.vertical(|ui| {
                ui.set_max_width(ui.available_width().min(640.0));
                self.composer(ui, tokens, false);
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    for idea in [
                        "Build a todo app with filters in the preview",
                        "Write a pm build file for ripgrep and explain it",
                        "Snap my terminal left and this window right",
                        "Review the open pull requests",
                    ] {
                        if ui
                            .add(
                                Button::new(idea)
                                    .variant(ButtonVariant::Outline)
                                    .size(ButtonSize::Sm),
                            )
                            .clicked()
                        {
                            self.composer = idea.to_owned();
                        }
                    }
                });
            });
        });
    }

    fn chat_page(&mut self, ui: &mut Ui, tokens: &Tokens, id: &str) {
        let Some(chat) = self.chats.iter().find(|chat| chat.id == id).cloned() else {
            self.page = Page::NewChat;
            return;
        };
        let busy = self.turns.contains_key(id);
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if !busy
                    && ui
                        .add(
                            Button::new("Delete")
                                .variant(ButtonVariant::Ghost)
                                .size(ButtonSize::Sm),
                        )
                        .clicked()
                {
                    let result = self.store.delete_chat(id);
                    self.report(result);
                    self.reload();
                    self.page = Page::NewChat;
                }
                for (name, status) in self.servers.get(id).into_iter().flatten() {
                    let variant = if status == "connected" {
                        BadgeVariant::Secondary
                    } else {
                        BadgeVariant::Destructive
                    };
                    ui.add(Badge::new(format!("{name}: {status}")).variant(variant));
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(typography::h4(tokens, &chat.title)).truncate());
                });
            });
        });
        ui.separator();
        egui::Panel::bottom("composer")
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(0, 8)))
            .show(ui, |ui| self.composer(ui, tokens, busy));
        egui::CentralPanel::no_frame().show(ui, |ui| {
            ScrollArea::vertical()
                .auto_shrink(false)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.set_max_width(ui.available_width().min(760.0));
                    for entry in &chat.entries {
                        transcript_entry(ui, tokens, entry);
                    }
                    if busy {
                        ui.horizontal(|ui| {
                            ui.add(Spinner::new().size(14.0));
                            ui.label(typography::muted(tokens, "Working…"));
                        });
                    }
                });
        });
    }

    fn project_page(&mut self, ui: &mut Ui, tokens: &Tokens, id: &str) {
        let Some(mut project) = self
            .projects
            .iter()
            .find(|project| project.id == id)
            .cloned()
        else {
            self.page = Page::NewChat;
            return;
        };
        self.project_id = Some(project.id.clone());
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.set_max_width(ui.available_width().min(720.0));
            ui.label(typography::h3(tokens, "Project"));
            ui.add_space(8.0);
            let mut changed = false;
            ui.label("Name");
            changed |= ui
                .add(Input::new(&mut project.name).width(360.0))
                .lost_focus();
            ui.add_space(8.0);
            ui.label("Instructions");
            ui.label(typography::muted(
                tokens,
                "Given to every chat in this project.",
            ));
            changed |= ui
                .add(Textarea::new(&mut project.instructions).rows(6))
                .lost_focus();
            ui.add_space(8.0);
            ui.label("Folders");
            ui.label(typography::muted(
                tokens,
                "Repositories the agent works in; pull requests come from their origin remotes.",
            ));
            let mut remove = None;
            for (index, folder) in project.folders.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(folder.display().to_string()).monospace());
                    if ui
                        .add(
                            Button::new("Remove")
                                .variant(ButtonVariant::Ghost)
                                .size(ButtonSize::Sm),
                        )
                        .clicked()
                    {
                        remove = Some(index);
                    }
                });
            }
            if let Some(index) = remove {
                project.folders.remove(index);
                changed = true;
            }
            ui.horizontal(|ui| {
                ui.add(
                    Input::new(&mut self.new_folder)
                        .placeholder("/home/you/src/repo")
                        .width(360.0),
                );
                if ui
                    .add(
                        Button::new("Add folder")
                            .variant(ButtonVariant::Secondary)
                            .size(ButtonSize::Sm),
                    )
                    .clicked()
                {
                    let folder = PathBuf::from(self.new_folder.trim());
                    if folder.is_dir() {
                        project.folders.push(folder);
                        self.new_folder.clear();
                        changed = true;
                    } else {
                        self.error = Some(format!("{} is not a folder", folder.display()));
                    }
                }
            });
            ui.add_space(8.0);
            ui.label("App");
            ui.horizontal(|ui| {
                let summary = match &project.app {
                    Some(app) => format!("{} in {}", app.display_name(), app.folder().display()),
                    None => "Not set up with the wizard.".to_owned(),
                };
                ui.label(typography::muted(tokens, summary));
                let label = if project.app.is_some() {
                    "Edit setup"
                } else {
                    "Set up"
                };
                if ui
                    .add(
                        Button::new(label)
                            .variant(ButtonVariant::Secondary)
                            .size(ButtonSize::Sm),
                    )
                    .clicked()
                {
                    self.open_setup(
                        project.app.clone().unwrap_or_default(),
                        Some(project.id.clone()),
                    );
                }
            });
            ui.add_space(8.0);
            ui.label("Permissions");
            let labels: Vec<&str> = PermissionMode::ALL
                .iter()
                .map(|mode| mode.label())
                .collect();
            let mut selected = PermissionMode::ALL
                .iter()
                .position(|mode| *mode == project.permission_mode);
            if ui
                .add(Select::new("permission-mode", &mut selected, &labels))
                .changed()
                && let Some(mode) = selected.and_then(|index| PermissionMode::ALL.get(index))
            {
                project.permission_mode = *mode;
                changed = true;
            }
            if changed {
                let result = self.store.update_projects(|projects| {
                    if let Some(stored) = projects.iter_mut().find(|stored| stored.id == project.id)
                    {
                        *stored = project.clone();
                    }
                });
                self.report(result);
                self.reload();
            }
            ui.add_space(24.0);
            if ui
                .add(
                    Button::new("Delete project")
                        .variant(ButtonVariant::Destructive)
                        .size(ButtonSize::Sm),
                )
                .clicked()
            {
                let result = self
                    .store
                    .update_projects(|projects| projects.retain(|stored| stored.id != project.id));
                self.report(result);
                self.reload();
                self.project_id = self.projects.first().map(|project| project.id.clone());
                self.page = Page::NewChat;
            }
        });
    }

    fn routine_page(&mut self, ui: &mut Ui, tokens: &Tokens, id: &str) {
        let Some(mut routine) = self
            .routines
            .iter()
            .find(|routine| routine.id == id)
            .cloned()
        else {
            self.page = Page::NewChat;
            return;
        };
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.set_max_width(ui.available_width().min(720.0));
            ui.label(typography::h3(tokens, "Routine"));
            ui.label(typography::muted(
                tokens,
                format!(
                    "Runs as the systemd user timer {}.timer and opens a new chat each time.",
                    routines::unit_name(&routine)
                ),
            ));
            ui.add_space(8.0);
            ui.label("Name");
            ui.add(Input::new(&mut routine.name).width(360.0));
            ui.add_space(8.0);
            ui.label("Prompt");
            ui.add(
                Textarea::new(&mut routine.prompt)
                    .rows(6)
                    .placeholder("Review yesterday's pull requests and summarize what needs me."),
            );
            ui.add_space(8.0);
            ui.label("Schedule");
            ui.label(typography::muted(
                tokens,
                "A systemd calendar expression: daily, hourly, Mon..Fri 09:00, *-*-* 18:30.",
            ));
            ui.add(Input::new(&mut routine.schedule).width(240.0));
            ui.add_space(8.0);
            ui.add(Switch::new(&mut routine.enabled).label("Enabled"));
            if let Some(last) = routine.last_run {
                ui.label(typography::muted(
                    tokens,
                    format!("Last run {}", last.format("%Y-%m-%d %H:%M UTC")),
                ));
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.add(Button::new("Save")).clicked() {
                    let saved = routine.clone();
                    let result = self.store.update_routines(|routines| {
                        if let Some(stored) =
                            routines.iter_mut().find(|stored| stored.id == saved.id)
                        {
                            *stored = saved.clone();
                        }
                    });
                    self.report(result);
                    self.reload();
                    let store = self.store.clone();
                    let name = routine.name.clone();
                    self.spawn_job(move || Job::Routine {
                        result: routines::install(&saved, &store)
                            .map(|()| {
                                if saved.enabled {
                                    "Timer installed".to_owned()
                                } else {
                                    "Timer off".to_owned()
                                }
                            })
                            .map_err(|error| crate::describe(&error)),
                        name,
                    });
                }
                if ui
                    .add(Button::new("Run now").variant(ButtonVariant::Secondary))
                    .clicked()
                {
                    let store = self.store.clone();
                    let routine_id = routine.id.clone();
                    let name = routine.name.clone();
                    self.spawn_job(move || Job::Routine {
                        result: routines::run(&store, &routine_id)
                            .map(|_| "Finished; its chat is in the left column".to_owned())
                            .map_err(|error| crate::describe(&error)),
                        name,
                    });
                    toast(
                        ui.ctx(),
                        routine.name.clone(),
                        Some("Running in the background".into()),
                    );
                }
                if ui
                    .add(Button::new("Delete").variant(ButtonVariant::Destructive))
                    .clicked()
                {
                    let doomed = routine.clone();
                    let result = self.store.update_routines(|routines| {
                        routines.retain(|stored| stored.id != doomed.id)
                    });
                    self.report(result);
                    self.reload();
                    self.page = Page::NewChat;
                    let name = routine.name.clone();
                    self.spawn_job(move || Job::Routine {
                        result: routines::uninstall(&doomed)
                            .map(|()| "Removed".to_owned())
                            .map_err(|error| crate::describe(&error)),
                        name,
                    });
                }
            });
            // Edits stay local until Save, so a half-typed schedule never
            // reaches systemd.
            if let Some(stored) = self
                .routines
                .iter_mut()
                .find(|stored| stored.id == routine.id)
            {
                *stored = routine;
            }
        });
    }

    fn artifact_page(&mut self, ui: &mut Ui, tokens: &Tokens, id: &str) {
        let Some(artifact) = self
            .artifacts
            .iter()
            .find(|artifact| artifact.id == id)
            .cloned()
        else {
            self.page = Page::NewChat;
            return;
        };
        ui.label(typography::h3(tokens, &artifact.title));
        ui.label(typography::muted(
            tokens,
            format!("Made {}", artifact.created.format("%Y-%m-%d %H:%M UTC")),
        ));
        if let Some(chat_id) = &artifact.chat_id
            && self.chats.iter().any(|chat| &chat.id == chat_id)
            && ui
                .add(Button::new("Open its chat").variant(ButtonVariant::Link))
                .clicked()
        {
            self.page = Page::Chat(chat_id.clone());
        }
        ui.add_space(8.0);
        match &artifact.kind {
            ArtifactKind::App { source } => {
                ui.label(RichText::new(source.label()).monospace());
                if ui.add(Button::new("Run in preview")).clicked() {
                    let context = ui.ctx().clone();
                    self.run_preview(&context, source.clone());
                    self.right_tab = RightTab::Preview as usize;
                }
            }
            ArtifactKind::File { path } => {
                ui.label(RichText::new(path.display().to_string()).monospace());
                if ui
                    .add(Button::new("Open").variant(ButtonVariant::Secondary))
                    .clicked()
                {
                    self.report(open_external(&path.display().to_string()));
                }
                ui.add_space(8.0);
                match std::fs::read(path) {
                    Ok(bytes) if bytes.len() <= 512 * 1024 => match String::from_utf8(bytes) {
                        Ok(text) => {
                            Card::new().show(ui, |ui| {
                                ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                                    ui.label(RichText::new(text).monospace());
                                });
                            });
                        }
                        Err(_) => {
                            ui.label(typography::muted(tokens, "Not text; open it to view."));
                        }
                    },
                    Ok(_) => {
                        ui.label(typography::muted(
                            tokens,
                            "Too large to show here; open it to view.",
                        ));
                    }
                    Err(error) => {
                        ui.label(typography::muted(tokens, format!("Can't read it: {error}")));
                    }
                }
            }
        }
    }

    fn right_side(&mut self, ui: &mut Ui, tokens: &Tokens, theme: &Theme) {
        ui.add(Tabs::new(&mut self.right_tab, &RIGHT_TABS));
        ui.add_space(8.0);
        match self.right_tab {
            tab if tab == RightTab::PullRequests as usize => self.pulls_tab(ui, tokens),
            tab if tab == RightTab::Mcp as usize => self.mcp_tab(ui, tokens),
            _ => self.preview_tab(ui, tokens, theme),
        }
    }

    fn preview_tab(&mut self, ui: &mut Ui, tokens: &Tokens, theme: &Theme) {
        let context = ui.ctx().clone();
        let Some(preview) = &mut self.preview else {
            if let Some(error) = &self.preview_error {
                ui.label(RichText::new(error).color(tokens.destructive));
            }
            Empty::new("Nothing to preview yet")
                .icon("▶")
                .description("Ask for an app in the chat. The agent builds it in Rust, natively or for WebAssembly, and it runs here live.")
                .show(ui, |_| {});
            return;
        };
        let status = preview.status();
        let mut restart = false;
        let mut stop = false;
        ui.horizontal(|ui| {
            let (text, variant) = match &status {
                PreviewStatus::Starting => ("starting".to_owned(), BadgeVariant::Secondary),
                PreviewStatus::Running { title } => (title.clone(), BadgeVariant::Default),
                PreviewStatus::Exited { .. } => ("stopped".to_owned(), BadgeVariant::Destructive),
            };
            ui.add(Badge::new(text).variant(variant));
            if matches!(preview.source(), PreviewSource::Wasm { .. }) {
                ui.add(Badge::new("wasm").variant(BadgeVariant::Outline));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                stop = ui
                    .add(
                        Button::new("Stop")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Sm),
                    )
                    .clicked();
                restart = ui
                    .add(
                        Button::new("Restart")
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Sm),
                    )
                    .clicked();
            });
        });
        if let PreviewStatus::Exited { message } = &status {
            ui.label(RichText::new(message).color(tokens.destructive));
            let stderr = preview.stderr();
            if !stderr.is_empty() {
                Card::new().title("stderr").show(ui, |ui| {
                    ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                        ui.label(RichText::new(stderr.join("\n")).monospace().small());
                    });
                });
            }
        } else {
            egui::Frame::new()
                .stroke(egui::Stroke::new(1.0, tokens.border))
                .corner_radius(tokens.radius * 2)
                .show(ui, |ui| {
                    preview.show(ui, preview_theme(theme));
                });
        }
        if restart {
            let source = preview.source().clone();
            self.run_preview(&context, source);
        } else if stop && let Some(mut preview) = self.preview.take() {
            preview.free_textures(&context);
        }
    }

    fn pulls_tab(&mut self, ui: &mut Ui, tokens: &Tokens) {
        if self.pulls_project != self.project_id && !self.pulls_loading {
            self.refresh_pulls();
        }
        ui.horizontal(|ui| {
            ui.label(typography::muted(
                tokens,
                "From each project folder's origin remote.",
            ));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if self.pulls_loading {
                    ui.add(Spinner::new().size(16.0));
                } else if ui
                    .add(
                        Button::new("Refresh")
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Sm),
                    )
                    .clicked()
                {
                    self.refresh_pulls();
                }
            });
        });
        let Some(pulls) = &self.pulls else {
            return;
        };
        if pulls.is_empty() {
            Empty::new("No folders")
                .icon("⑂")
                .description(
                    "Add a repository folder to the project to see its pull or merge requests.",
                )
                .show(ui, |_| {});
            return;
        }
        let mut review = None;
        let mut open = None;
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for folder in pulls {
                ui.add_space(6.0);
                match &folder.result {
                    Err(error) => {
                        ui.label(RichText::new(folder.folder.display().to_string()).strong());
                        ui.label(RichText::new(error).color(tokens.muted_foreground));
                    }
                    Ok((repo, items)) => {
                        ui.horizontal(|ui| {
                            ui.add(Badge::new(repo.forge.name()).variant(BadgeVariant::Secondary));
                            ui.label(RichText::new(format!("{}/{}", repo.host, repo.path)).strong());
                        });
                        if items.is_empty() {
                            ui.label(typography::muted(tokens, format!("No open {}s.", repo.forge.noun())));
                        }
                        for pull in items {
                            Card::new().show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    let sigil = if repo.forge == forge::Forge::GitLab { "!" } else { "#" };
                                    ui.label(RichText::new(format!("{sigil}{}", pull.number)).color(tokens.muted_foreground));
                                    ui.add(egui::Label::new(RichText::new(&pull.title).strong()).truncate());
                                });
                                ui.horizontal(|ui| {
                                    // Buttons first, right to left, so the author and
                                    // branch truncate into what is left.
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if ui.add(Button::new("Open").variant(ButtonVariant::Ghost).size(ButtonSize::Sm)).clicked() {
                                            open = Some(pull.url.clone());
                                        }
                                        if ui.add(Button::new("Review").variant(ButtonVariant::Outline).size(ButtonSize::Sm)).clicked() {
                                            review = Some(format!(
                                                "Review {} {}{} \"{}\" ({}). Check out its branch {}, read the diff, run what tests you can, and tell me what needs to change before it merges.",
                                                repo.forge.noun(),
                                                if repo.forge == forge::Forge::GitLab { "!" } else { "#" },
                                                pull.number, pull.title, pull.url, pull.branch
                                            ));
                                        }
                                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                            if pull.draft {
                                                ui.add(Badge::new("draft").variant(BadgeVariant::Outline));
                                            }
                                            ui.add(
                                                egui::Label::new(
                                                    typography::small(tokens, format!("{} · {}", pull.author, pull.branch))
                                                        .color(tokens.muted_foreground),
                                                )
                                                .truncate(),
                                            );
                                        });
                                    });
                                });
                            });
                        }
                    }
                }
            }
        });
        if let Some(prompt) = review {
            self.composer = prompt;
            self.page = Page::NewChat;
        }
        if let Some(url) = open {
            self.report(open_external(&url));
        }
    }

    fn mcp_tab(&mut self, ui: &mut Ui, tokens: &Tokens) {
        let Some(project) = self.project().cloned() else {
            ui.label(typography::muted(
                tokens,
                "Pick a project to manage its MCP servers.",
            ));
            return;
        };
        let mut list = None;
        let mut toggle = None;
        let mut remove = None;
        let mut add = None;
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.label(typography::muted(
                tokens,
                "Every chat in this project gets the Agent's tools plus the servers switched on here.",
            ));
            ui.add_space(6.0);
            Card::new()
                .title("desktop")
                .description("Built in: preview, pm, Nix, derisk, forge, artifacts")
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if let Some(tools) = tools::definitions().as_array() {
                        ui.horizontal_wrapped(|ui| {
                            for tool in tools {
                                ui.add(
                                    Badge::new(tool["name"].as_str().unwrap_or_default())
                                        .variant(BadgeVariant::Secondary),
                                )
                                .on_hover_text(tool["description"].as_str().unwrap_or_default());
                            }
                        });
                    }
                });
            for (index, server) in project.mcp_servers.iter().enumerate() {
                ui.add_space(6.0);
                Card::new()
                    .title(&server.name)
                    .description(transport_label(&server.transport))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let mut enabled = server.enabled;
                            if ui.add(Switch::new(&mut enabled).label("On")).changed() {
                                toggle = Some((index, enabled));
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui
                                    .add(
                                        Button::new("Remove")
                                            .variant(ButtonVariant::Ghost)
                                            .size(ButtonSize::Sm),
                                    )
                                    .clicked()
                                {
                                    remove = Some(index);
                                }
                                if ui
                                    .add(
                                        Button::new("Tools")
                                            .variant(ButtonVariant::Outline)
                                            .size(ButtonSize::Sm),
                                    )
                                    .clicked()
                                {
                                    list = Some(server.clone());
                                }
                            });
                        });
                        tool_listing(ui, tokens, self.tool_lists.get(&server.name));
                    });
            }
            for folder in &project.folders {
                for server in mcp::folder_servers(folder) {
                    ui.add_space(6.0);
                    Card::new()
                        .title(&server.name)
                        .description(format!(
                            "{} · from {}/.mcp.json",
                            transport_label(&server.transport),
                            folder.display()
                        ))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            if ui
                                .add(
                                    Button::new("Tools")
                                        .variant(ButtonVariant::Outline)
                                        .size(ButtonSize::Sm),
                                )
                                .clicked()
                            {
                                list = Some(server.clone());
                            }
                            tool_listing(ui, tokens, self.tool_lists.get(&server.name));
                        });
                }
            }
            ui.add_space(10.0);
            ui.label(RichText::new("Add a server").strong());
            for suggested in mcp::suggested_servers() {
                if !project
                    .mcp_servers
                    .iter()
                    .any(|server| server.name == suggested.name)
                    && ui
                        .add(
                            Button::new(format!(
                                "Add {} ({})",
                                suggested.name,
                                transport_label(&suggested.transport)
                            ))
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Sm),
                        )
                        .clicked()
                {
                    add = Some(suggested);
                }
            }
            ui.horizontal(|ui| {
                ui.add(
                    Input::new(&mut self.new_server_name)
                        .placeholder("name")
                        .width(110.0),
                );
                ui.add(
                    Input::new(&mut self.new_server_target)
                        .placeholder("https://…/mcp or a command")
                        .width(220.0),
                );
                if ui.add(Button::new("Add").size(ButtonSize::Sm)).clicked() {
                    let target = self.new_server_target.trim();
                    let transport =
                        if target.starts_with("http://") || target.starts_with("https://") {
                            McpTransport::Http {
                                url: target.to_owned(),
                            }
                        } else {
                            let mut words = target.split_whitespace().map(str::to_owned);
                            McpTransport::Stdio {
                                command: words.next().unwrap_or_default(),
                                args: words.collect(),
                            }
                        };
                    let name = self.new_server_name.trim().to_owned();
                    if name.is_empty() || target.is_empty() || name == "desktop" {
                        self.error = Some(
                            "A server needs a name other than desktop, and a URL or command.".into(),
                        );
                    } else {
                        add = Some(McpServer {
                            name,
                            transport,
                            enabled: true,
                        });
                        self.new_server_name.clear();
                        self.new_server_target.clear();
                    }
                }
            });
        });
        if let Some(server) = list {
            self.list_tools(&server);
        }
        if let Some((index, enabled)) = toggle {
            self.update_project(|project| {
                if let Some(server) = project.mcp_servers.get_mut(index) {
                    server.enabled = enabled;
                }
            });
        }
        if let Some(index) = remove {
            self.update_project(|project| {
                if index < project.mcp_servers.len() {
                    project.mcp_servers.remove(index);
                }
            });
        }
        if let Some(server) = add {
            self.update_project(|project| {
                project
                    .mcp_servers
                    .retain(|existing| existing.name != server.name);
                project.mcp_servers.push(server);
            });
        }
    }
}

fn transport_label(transport: &McpTransport) -> String {
    match transport {
        McpTransport::Stdio { command, args } => std::iter::once(command.as_str())
            .chain(args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        McpTransport::Http { url } => url.clone(),
    }
}

fn tool_listing(ui: &mut Ui, tokens: &Tokens, listing: Option<&Result<Vec<ToolInfo>, String>>) {
    match listing {
        None => {}
        Some(Err(error)) => {
            ui.label(RichText::new(error).color(tokens.destructive));
        }
        Some(Ok(tools)) => {
            ui.horizontal_wrapped(|ui| {
                for tool in tools {
                    ui.add(Badge::new(&tool.name).variant(BadgeVariant::Secondary))
                        .on_hover_text(&tool.description);
                }
            });
        }
    }
}

fn transcript_entry(ui: &mut Ui, tokens: &Tokens, entry: &Entry) {
    ui.add_space(6.0);
    match entry.role {
        Role::User => {
            ui.with_layout(Layout::top_down(Align::Max), |ui| {
                let width = (ui.available_width() * 0.8).min(520.0);
                egui::Frame::new()
                    .fill(tokens.muted)
                    .corner_radius(tokens.radius * 2)
                    .inner_margin(egui::Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.set_max_width(width);
                        ui.add(
                            egui::Label::new(RichText::new(&entry.text).color(tokens.foreground))
                                .wrap(),
                        );
                    });
            });
        }
        Role::Assistant => {
            ui.label(RichText::new(&entry.text).color(tokens.foreground));
        }
        Role::Tool => {
            let (name, input) = entry.text.split_once(' ').unwrap_or((&entry.text, ""));
            let name = name
                .strip_prefix("mcp__")
                .unwrap_or(name)
                .replace("__", " · ");
            egui::CollapsingHeader::new(
                RichText::new(format!("⚙ {name}"))
                    .color(tokens.muted_foreground)
                    .small(),
            )
            .id_salt(entry.at.timestamp_nanos_opt())
            .show(ui, |ui| {
                ui.label(RichText::new(input).monospace().small());
            });
        }
        Role::ToolResult => {
            let first = entry.text.lines().next().unwrap_or_default();
            egui::CollapsingHeader::new(
                RichText::new(format!("result · {}", truncate(first, 80)))
                    .color(tokens.muted_foreground)
                    .small(),
            )
            .id_salt(("result", entry.at.timestamp_nanos_opt()))
            .show(ui, |ui| {
                ui.label(RichText::new(&entry.text).monospace().small());
            });
        }
        Role::Status => {
            ui.label(typography::small(tokens, &entry.text).color(tokens.muted_foreground));
        }
        Role::Error => {
            ui.label(RichText::new(&entry.text).color(tokens.destructive));
        }
    }
}

fn truncate(text: &str, chars: usize) -> String {
    if text.chars().count() > chars {
        format!("{}…", text.chars().take(chars).collect::<String>())
    } else {
        text.to_owned()
    }
}

fn preview_theme(theme: &Theme) -> PreviewTheme {
    PreviewTheme {
        background: theme.background,
        surface: theme.surface,
        foreground: theme.foreground,
        border: theme.border,
        accent: theme.accent,
    }
}

/// Opens a URL or file with the desktop's handler.
fn open_external(target: &str) -> miette::Result<()> {
    std::process::Command::new("xdg-open")
        .arg(target)
        .spawn()
        .map(|_| ())
        .map_err(|error| miette::miette!("could not run xdg-open for {target}: {error}"))
}

/// Listens on [`tools::control_socket`] for `derisk-agent mcp`'s preview and store
/// messages. Each connection is one request line and one reply line.
fn start_control_server(context: &egui::Context) -> Option<mpsc::Receiver<ControlRequest>> {
    let path = tools::control_socket();
    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        tracing::warn!("no control socket: {error}");
        return None;
    }
    // A socket left by an Agent window that crashed refuses connections; one that
    // answers belongs to an Agent window still running, which keeps it.
    if std::os::unix::net::UnixStream::connect(&path).is_ok() {
        tracing::warn!(
            "another Agent window owns {}; previews go there",
            path.display()
        );
        return None;
    }
    std::fs::remove_file(&path).ok();
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(error) => {
            tracing::warn!("no control socket at {}: {error}", path.display());
            return None;
        }
    };
    let (sender, receiver) = mpsc::channel();
    let context = context.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let sender = sender.clone();
            let context = context.clone();
            std::thread::spawn(move || {
                let mut line = String::new();
                let Ok(reader) = stream.try_clone() else {
                    return;
                };
                if BufReader::new(reader).read_line(&mut line).is_err() {
                    return;
                }
                let request = serde_json::from_str(&line).unwrap_or(Value::Null);
                let (reply, replied) = mpsc::channel();
                if sender.send(ControlRequest { request, reply }).is_err() {
                    return;
                }
                context.request_repaint();
                let reply = replied
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap_or_else(
                        |_| json!({"ok": false, "error": "The Agent's window is busy"}),
                    );
                let mut stream = stream;
                writeln!(stream, "{reply}").ok();
            });
        }
    });
    Some(receiver)
}
