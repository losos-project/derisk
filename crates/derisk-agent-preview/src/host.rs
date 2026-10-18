// Previews run on threads of their own: one writes requests, one reads frames
// and one keeps stderr. Nothing here runs a process on the UI thread, which is
// what the workspace's ban on `std::process::Command` guards against.
#![allow(clippy::disallowed_methods)]

use std::{
    collections::{HashMap, VecDeque},
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

use egui::{Event, Pos2, Rect, Sense, Shape, TextureId, Vec2};

use crate::{ClippedMesh, GuestFrame, GuestMessage, HostMessage, PreviewTheme, read_message};

const STDERR_LINES: usize = 200;

/// What to run in the preview pane.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PreviewSource {
    /// A native program that calls [`crate::serve`].
    Native {
        program: PathBuf,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        cwd: Option<PathBuf>,
    },
    /// A `wasm32-wasip2` component that calls [`crate::serve`], run by
    /// `runner preview-wasm <component>` (the Agent's own binary).
    Wasm { component: PathBuf, runner: PathBuf },
}

impl PreviewSource {
    pub fn label(&self) -> String {
        match self {
            Self::Native { program, .. } => program.display().to_string(),
            Self::Wasm { component, .. } => format!("{} (wasm)", component.display()),
        }
    }

    fn command(&self) -> Command {
        match self {
            Self::Native { program, args, cwd } => {
                let mut command = Command::new(program);
                command.args(args);
                if let Some(cwd) = cwd {
                    command.current_dir(cwd);
                }
                command
            }
            Self::Wasm { component, runner } => {
                let mut command = Command::new(runner);
                command.arg("preview-wasm").arg(component);
                command
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreviewStatus {
    Starting,
    Running { title: String },
    Exited { message: String },
}

#[derive(Default)]
struct Shared {
    frame: Option<GuestFrame>,
    title: Option<String>,
    ended: Option<String>,
    stderr: VecDeque<String>,
}

/// A running preview and what it last drew.
pub struct Preview {
    source: PreviewSource,
    child: Child,
    to_guest: mpsc::Sender<HostMessage>,
    shared: Arc<Mutex<Shared>>,
    textures: HashMap<TextureId, TextureId>,
    meshes: Vec<ClippedMesh>,
    cursor_icon: egui::CursorIcon,
    awaiting_frame: bool,
    repaint_at: Option<Instant>,
    last_size: Vec2,
    pointer_inside: bool,
    started: Instant,
    frames: u64,
}

impl Preview {
    /// Starts `source`; `context` is repainted whenever a frame arrives.
    pub fn spawn(source: PreviewSource, context: &egui::Context) -> std::io::Result<Self> {
        let mut child = source
            .command()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (to_guest, requests) = mpsc::channel::<HostMessage>();

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("no stdin"))?;
        let mut stdout = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| std::io::Error::other("no stdout"))?,
        );
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| std::io::Error::other("no stderr"))?;

        std::thread::Builder::new()
            .name("preview-writer".into())
            .spawn(move || {
                for request in requests {
                    if let Err(error) = crate::write_message(&mut stdin, &request) {
                        tracing::debug!("preview stopped reading requests: {error}");
                        break;
                    }
                }
            })?;

        std::thread::Builder::new()
            .name("preview-reader".into())
            .spawn({
                let shared = shared.clone();
                let context = context.clone();
                move || {
                    let ended = loop {
                        match read_message::<GuestMessage>(&mut stdout) {
                            Ok(Some(GuestMessage::Hello { title })) => {
                                lock(&shared).title = Some(title);
                            }
                            Ok(Some(GuestMessage::Frame(frame))) => {
                                let mut shared = lock(&shared);
                                // Texture deltas must be applied in order, so a frame
                                // that lands before the last was painted keeps them.
                                if let Some(mut previous) = shared.frame.take() {
                                    previous.textures_delta.append(frame.textures_delta);
                                    shared.frame = Some(GuestFrame {
                                        textures_delta: previous.textures_delta,
                                        ..frame
                                    });
                                } else {
                                    shared.frame = Some(frame);
                                }
                            }
                            Ok(None) => break "the preview closed its output".to_owned(),
                            Err(error) => break format!("the preview sent a bad frame: {error}"),
                        }
                        context.request_repaint();
                    };
                    lock(&shared).ended = Some(ended);
                    context.request_repaint();
                }
            })?;

        std::thread::Builder::new()
            .name("preview-stderr".into())
            .spawn({
                let shared = shared.clone();
                move || {
                    for line in BufReader::new(stderr).lines() {
                        let Ok(line) = line else { break };
                        let mut shared = lock(&shared);
                        if shared.stderr.len() == STDERR_LINES {
                            shared.stderr.pop_front();
                        }
                        shared.stderr.push_back(line);
                    }
                }
            })?;

        Ok(Self {
            source,
            child,
            to_guest,
            shared,
            textures: HashMap::new(),
            meshes: Vec::new(),
            cursor_icon: egui::CursorIcon::Default,
            awaiting_frame: false,
            repaint_at: Some(Instant::now()),
            last_size: Vec2::ZERO,
            pointer_inside: false,
            started: Instant::now(),
            frames: 0,
        })
    }

    pub fn source(&self) -> &PreviewSource {
        &self.source
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    pub fn status(&mut self) -> PreviewStatus {
        if let Ok(Some(exit)) = self.child.try_wait() {
            return PreviewStatus::Exited {
                message: format!("exited with {exit}"),
            };
        }
        let shared = lock(&self.shared);
        if let Some(ended) = &shared.ended {
            return PreviewStatus::Exited {
                message: ended.clone(),
            };
        }
        match &shared.title {
            Some(title) => PreviewStatus::Running {
                title: title.clone(),
            },
            None => PreviewStatus::Starting,
        }
    }

    /// The last lines the preview wrote to stderr.
    pub fn stderr(&self) -> Vec<String> {
        lock(&self.shared).stderr.iter().cloned().collect()
    }

    /// Fills the rest of `ui` with the preview and forwards input inside it.
    pub fn show(&mut self, ui: &mut egui::Ui, theme: PreviewTheme) -> egui::Response {
        let (rect, response) = ui.allocate_exact_size(
            ui.available_size(),
            // Focusable, or egui drops the focus a click gives it on the next frame
            // and keys never reach the preview.
            Sense::click_and_drag() | Sense::FOCUSABLE,
        );
        if response.clicked() || response.drag_started() {
            response.request_focus();
        }
        if response.has_focus() {
            // Keep Tab, arrows and Escape inside the preview while it has focus.
            ui.memory_mut(|memory| {
                memory.set_focus_lock_filter(
                    response.id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
        }

        self.take_frame(ui.ctx());
        let events = self.forward_events(ui, rect, response.has_focus());
        let size_changed = rect.size() != self.last_size;
        let due = self
            .repaint_at
            .is_some_and(|repaint_at| Instant::now() >= repaint_at);
        if !self.awaiting_frame && (size_changed || due || !events.is_empty()) {
            self.request_frame(ui, rect, events, theme);
        }

        let painter = ui.painter().with_clip_rect(rect);
        let offset = rect.min.to_vec2();
        for clipped in &self.meshes {
            let mut mesh = clipped.mesh.clone();
            mesh.texture_id = self
                .textures
                .get(&mesh.texture_id)
                .copied()
                .unwrap_or(mesh.texture_id);
            mesh.translate(offset);
            painter
                .with_clip_rect(clipped.clip_rect.translate(offset).intersect(rect))
                .add(Shape::mesh(mesh));
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(self.cursor_icon);
        }
        if let Some(repaint_at) = self.repaint_at {
            ui.ctx()
                .request_repaint_after(repaint_at.saturating_duration_since(Instant::now()));
        }
        response
    }

    fn take_frame(&mut self, context: &egui::Context) {
        let Some(mut frame) = lock(&self.shared).frame.take() else {
            return;
        };
        self.awaiting_frame = false;
        self.frames += 1;
        let textures = context.tex_manager();
        let mut textures = textures.write();
        for (guest_id, deltas) in std::mem::take(&mut frame.textures_delta.set) {
            for delta in deltas {
                match (self.textures.get(&guest_id), delta.pos) {
                    (Some(host_id), _) => textures.set(*host_id, delta),
                    (None, None) => {
                        let host_id = textures.alloc(
                            format!("preview {guest_id:?}"),
                            delta.image,
                            delta.options,
                        );
                        self.textures.insert(guest_id, host_id);
                    }
                    (None, Some(_)) => {
                        tracing::warn!("preview patched texture {guest_id:?} before creating it");
                    }
                }
            }
        }
        for guest_id in std::mem::take(&mut frame.textures_delta.free) {
            if let Some(host_id) = self.textures.remove(&guest_id) {
                textures.free(host_id);
            }
        }
        self.meshes = std::mem::take(&mut frame.meshes);
        self.cursor_icon = frame.cursor_icon;
        self.repaint_at = frame
            .repaint_after_ms
            .map(|ms| Instant::now() + Duration::from_millis(ms));
    }

    fn forward_events(&mut self, ui: &egui::Ui, rect: Rect, focused: bool) -> Vec<Event> {
        let offset = rect.min.to_vec2();
        let inside = |pos: Pos2| rect.contains(pos);
        let mut forwarded = Vec::new();
        let events = ui.input(|input| input.events.clone());
        let pointer_down = ui.input(|input| input.pointer.any_down());
        for event in events {
            match event {
                Event::PointerMoved(pos) => {
                    if inside(pos) || (self.pointer_inside && pointer_down) {
                        self.pointer_inside = true;
                        forwarded.push(Event::PointerMoved(pos - offset));
                    } else if self.pointer_inside {
                        self.pointer_inside = false;
                        forwarded.push(Event::PointerGone);
                    }
                }
                Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    modifiers,
                } if inside(pos) || (self.pointer_inside && !pressed) => {
                    forwarded.push(Event::PointerButton {
                        pos: pos - offset,
                        button,
                        pressed,
                        modifiers,
                    });
                }
                Event::PointerGone if self.pointer_inside => {
                    self.pointer_inside = false;
                    forwarded.push(Event::PointerGone);
                }
                Event::MouseWheel { .. } | Event::Zoom(_) if self.pointer_inside => {
                    forwarded.push(event)
                }
                Event::Key { .. }
                | Event::Text(_)
                | Event::Copy
                | Event::Cut
                | Event::Paste(_)
                | Event::Ime(_)
                | Event::ModifiersChanged(_)
                    if focused =>
                {
                    forwarded.push(event)
                }
                _ => {}
            }
        }
        forwarded
    }

    fn request_frame(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        events: Vec<Event>,
        theme: PreviewTheme,
    ) {
        let pixels_per_point = ui.ctx().pixels_per_point();
        let mut input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, rect.size())),
            time: Some(self.started.elapsed().as_secs_f64()),
            predicted_dt: ui.input(|input| input.predicted_dt),
            focused: true,
            events,
            ..Default::default()
        };
        input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .native_pixels_per_point = Some(pixels_per_point);
        if self
            .to_guest
            .send(HostMessage::Frame { input, theme })
            .is_ok()
        {
            self.awaiting_frame = true;
            self.last_size = rect.size();
            self.repaint_at = None;
        }
    }

    /// Frees the preview's textures from `context`. Dropping the preview stops
    /// its process.
    pub fn free_textures(&mut self, context: &egui::Context) {
        let textures = context.tex_manager();
        let mut textures = textures.write();
        for (_, host_id) in self.textures.drain() {
            textures.free(host_id);
        }
        self.meshes.clear();
    }
}

impl Drop for Preview {
    fn drop(&mut self) {
        // The guest exits on Shutdown; kill covers one that is stuck in a frame.
        self.to_guest.send(HostMessage::Shutdown).ok();
        if let Err(error) = self.child.kill() {
            tracing::debug!("preview already gone: {error}");
        }
        if let Err(error) = self.child.wait() {
            tracing::warn!("could not reap the preview: {error}");
        }
    }
}

fn lock(shared: &Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
