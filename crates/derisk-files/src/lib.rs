//! derisk Files: a file manager with places, history, sorting, filtering,
//! copy/cut/paste, renaming, and the freedesktop.org trash.
//!
//! [`Browser`] holds the state and does the filesystem work without any UI,
//! and [`FilesApp`] draws it.
//!
//! ```
//! use derisk_files::{Browser, ClipboardOp, Trash};
//!
//! let root = std::env::temp_dir().join(format!("derisk-files-doc-{}", std::process::id()));
//! std::fs::create_dir_all(root.join("a"))?;
//! let mut files = Browser::with_trash(&root, Some(Trash::at(root.join(".trash"))))?;
//! let notes = files.create_file("notes.txt")?;
//! files.set_clipboard(ClipboardOp::Copy);
//! files.navigate(root.join("a"))?;
//! assert_eq!(files.paste()?, [root.join("a/notes.txt")]);
//! files.go_back()?;
//! files.select(&notes, false);
//! assert_eq!(files.trash_selection()?, 1);
//! assert!(!notes.exists());
//! std::fs::remove_dir_all(&root)?;
//! # Ok::<(), std::io::Error>(())
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod browser;
pub mod fs_ops;

use std::{
    io,
    path::{Path, PathBuf},
};

pub use browser::{Browser, ClipboardOp, SortKey};
pub use fs_ops::{Entry, Kind, Trash};
use mcsapi_components::{Button, ButtonVariant, ErrorDialog, Tokens};
use mcsapi_ui::{App, Context as _, DocLink, Error, Theme, egui};

/// The documentation section that explains why Files could not open or
/// change something.
fn doc() -> DocLink {
    DocLink::new("troubleshooting").section("files-could-not-open-or-change-something")
}

/// Opens a file with the user's default application.
pub type Opener = Box<dyn FnMut(&Path) -> io::Result<()>>;

/// Opens files with `xdg-open`, without waiting for it.
pub fn xdg_open(path: &Path) -> io::Result<()> {
    std::process::Command::new("xdg-open")
        .arg(path)
        .stdin(std::process::Stdio::null())
        .spawn()
        .map(drop)
}

#[derive(Debug)]
enum PromptKind {
    NewFolder,
    NewFile,
    Rename(PathBuf),
}

#[derive(Debug)]
struct Prompt {
    kind: PromptKind,
    text: String,
}

/// The Files app.
pub struct FilesApp {
    /// The browsing state.
    pub browser: Option<Browser>,
    opener: Opener,
    location: String,
    prompt: Option<Prompt>,
    status: Option<String>,
    error: Option<Error>,
}

impl std::fmt::Debug for FilesApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilesApp")
            .field("browser", &self.browser)
            .field("status", &self.status)
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl Default for FilesApp {
    /// Opens the home directory, or `/` when `HOME` is unset.
    fn default() -> Self {
        let home =
            derisk_sys::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
        Self::new(Browser::new(home), Box::new(xdg_open))
    }
}

impl FilesApp {
    /// Shows `browser`, opening files with `opener`. An `Err` browser is
    /// reported and leaves the app empty until a place is chosen.
    pub fn new(browser: io::Result<Browser>, opener: Opener) -> Self {
        let (browser, error) = match browser {
            Ok(browser) => (Some(browser), None),
            Err(error) => (
                None,
                Some(
                    Error::plain(error)
                        .context("Could not open folder")
                        .with_doc(doc()),
                ),
            ),
        };
        let location = browser
            .as_ref()
            .map(|b| b.cwd().display().to_string())
            .unwrap_or_default();
        Self {
            browser,
            opener,
            location,
            prompt: None,
            status: error.as_ref().map(ToString::to_string),
            error,
        }
    }

    /// The latest status or error message.
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    /// The error the app is showing, with its causes and documentation.
    pub fn error(&self) -> Option<&Error> {
        self.error.as_ref()
    }

    /// Shows `error` in a dialog until the next action or until "OK"; the
    /// status line carries its message too.
    fn fail(&mut self, error: Error) {
        self.status = Some(error.to_string());
        self.error = Some(error);
    }

    /// Opens a folder, or a file with the default application.
    pub fn activate(&mut self, path: &Path) {
        if derisk_sys::fs::is_dir(path) {
            self.go(|b| b.navigate(path));
        } else if let Err(error) = (self.opener)(path)
            .context(format!("Could not open {}", path.display()))
            .doc(doc())
        {
            self.fail(error);
        }
    }

    /// Opens a folder, creating the browser if it failed to start.
    pub fn go(&mut self, step: impl FnOnce(&mut Browser) -> io::Result<()>) {
        let result = match &mut self.browser {
            Some(browser) => step(browser),
            None => Err(io::Error::new(io::ErrorKind::NotFound, "no folder open")),
        };
        self.report(result.map(|()| None));
        self.sync_location();
    }

    fn open_place(&mut self, dir: &Path) {
        if self.browser.is_some() {
            self.go(|b| b.navigate(dir));
        } else {
            *self = Self::new(
                Browser::new(dir),
                std::mem::replace(&mut self.opener, Box::new(xdg_open)),
            );
        }
    }

    fn sync_location(&mut self) {
        if let Some(browser) = &self.browser {
            self.location = browser.cwd().display().to_string();
        }
    }

    fn report(&mut self, result: io::Result<Option<String>>) {
        self.error = None;
        match result.context("Could not change the files").doc(doc()) {
            Ok(Some(message)) => self.status = Some(message),
            Ok(None) => self.status = None,
            Err(error) => self.fail(error),
        }
    }

    /// The sidebar's places: symbolic icon, label and folder.
    fn places(&self) -> impl Iterator<Item = (&'static str, &'static str, PathBuf)> + use<> {
        let home = derisk_sys::env::var_os("HOME").map(PathBuf::from);
        let folders = home.clone().into_iter().flat_map(|home| {
            [
                ("user-desktop", "Desktop"),
                ("folder-documents", "Documents"),
                ("folder-download", "Downloads"),
                ("folder-music", "Music"),
                ("folder-pictures", "Pictures"),
                ("folder-videos", "Videos"),
            ]
            .into_iter()
            .map(move |(icon, dir)| (icon, dir, home.join(dir)))
            .filter(|(_, _, path)| derisk_sys::fs::is_dir(path))
        });
        let trash = self.browser.as_ref().and_then(Browser::trash);
        home.map(|home| ("user-home", "Home", home))
            .into_iter()
            .chain(folders)
            .chain(trash.map(|trash| ("user-trash", "Trash", trash.files_dir())))
            .chain([("computer", "Computer", PathBuf::from("/"))])
    }

    fn submit_prompt(&mut self) {
        let (Some(prompt), Some(browser)) = (self.prompt.take(), &mut self.browser) else {
            return;
        };
        let result = match &prompt.kind {
            PromptKind::NewFolder => browser.create_folder(&prompt.text),
            PromptKind::NewFile => browser.create_file(&prompt.text),
            PromptKind::Rename(path) => browser.rename(path, &prompt.text),
        };
        if result.is_err() {
            self.prompt = Some(prompt);
        }
        self.report(result.map(|_| None));
    }

    /// Back, forward, up and refresh: beside the folder's title.
    fn moves(&mut self, ui: &mut egui::Ui) {
        let Some(browser) = &self.browser else {
            return;
        };
        let (back, forward) = (browser.can_go_back(), browser.can_go_forward());
        // Laid out from the right, so the last added sits leftmost.
        if derisk_icons::tool(ui, true, "view-refresh", "Refresh").clicked() {
            self.go(Browser::refresh);
        }
        if derisk_icons::tool(ui, true, "go-up", "Parent folder").clicked() {
            self.go(Browser::go_up);
        }
        if derisk_icons::tool(ui, forward, "go-next", "Forward").clicked() {
            self.go(Browser::go_forward);
        }
        if derisk_icons::tool(ui, back, "go-previous", "Back").clicked() {
            self.go(Browser::go_back);
        }
    }

    /// The location to type a path into, the filter and whether hidden
    /// files show: over the listing, under the folder's title.
    fn location_row(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // On a phone the filter and the location share what is left.
            let search_width = (ui.available_width() * 0.35).min(200.0);
            let hidden_width = if ui.available_width() < derisk_frame::PHONE {
                0.0
            } else {
                120.0
            };
            let location = ui.add(derisk_frame::field(&mut self.location).desired_width(
                (ui.available_width() - search_width - hidden_width - 20.0).max(60.0),
            ));
            if location.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let target = PathBuf::from(&self.location);
                self.go(|b| b.navigate(target));
            }
            if let Some(browser) = &mut self.browser {
                ui.add(
                    derisk_frame::field(&mut browser.filter)
                        .hint_text("Filter")
                        .desired_width(search_width),
                );
                if hidden_width > 0.0 {
                    ui.checkbox(&mut browser.show_hidden, "Hidden files");
                }
            }
        });
    }

    /// What acts on the folder and the selection, along the action bar:
    /// laid out from the right, so the first added sits rightmost.
    fn actions(&mut self, ui: &mut egui::Ui) {
        let Some(browser) = &mut self.browser else {
            return;
        };
        let selected = browser.selection().len();
        let mut action = None;
        let can_trash = selected > 0 && browser.trash().is_some();
        let trash = Button::new("Move to trash")
            .variant(ButtonVariant::Destructive)
            .enabled(can_trash);
        if ui.add(trash).clicked() {
            action = Some("trash");
        }
        let outline = |label| Button::new(label).variant(ButtonVariant::Outline);
        if ui
            .add(outline("Paste").enabled(browser.clipboard().is_some()))
            .clicked()
        {
            action = Some("paste");
        }
        if ui.add(outline("Cut").enabled(selected > 0)).clicked() {
            browser.set_clipboard(ClipboardOp::Cut);
        }
        if ui.add(outline("Copy").enabled(selected > 0)).clicked() {
            browser.set_clipboard(ClipboardOp::Copy);
        }
        if ui.add(outline("Rename").enabled(selected == 1)).clicked() {
            action = Some("rename");
        }
        ui.separator();
        if ui.add(outline("New file")).clicked() {
            action = Some("file");
        }
        if ui.add(Button::new("New folder")).clicked() {
            action = Some("folder");
        }
        self.run_action(action);
    }

    fn run_action(&mut self, action: Option<&str>) {
        let Some(browser) = &mut self.browser else {
            return;
        };
        match action {
            Some("folder") => {
                self.prompt = Some(Prompt {
                    kind: PromptKind::NewFolder,
                    text: "New folder".into(),
                });
            }
            Some("file") => {
                self.prompt = Some(Prompt {
                    kind: PromptKind::NewFile,
                    text: "New file.txt".into(),
                });
            }
            Some("rename") => {
                if let Some(path) = browser.selection().first().cloned() {
                    let text = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.prompt = Some(Prompt {
                        kind: PromptKind::Rename(path),
                        text,
                    });
                }
            }
            Some("paste") => {
                let result = browser
                    .paste()
                    .map(|p| Some(format!("Pasted {} item(s)", p.len())));
                self.report(result);
            }
            Some("trash") => {
                let result = browser
                    .trash_selection()
                    .map(|n| Some(format!("Moved {n} item(s) to the trash")));
                self.report(result);
            }
            _ => {}
        }
    }

    fn prompt_ui(&mut self, ui: &mut egui::Ui) {
        let Some(prompt) = &mut self.prompt else {
            return;
        };
        let mut submit = false;
        let mut cancel = false;
        ui.horizontal(|ui| {
            ui.label(match prompt.kind {
                PromptKind::NewFolder => "Folder name",
                PromptKind::NewFile => "File name",
                PromptKind::Rename(_) => "New name",
            });
            let field = ui.add(derisk_frame::field(&mut prompt.text));
            if !field.has_focus() && !field.lost_focus() {
                field.request_focus();
            }
            submit = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            submit |= ui.add(Button::new("OK")).clicked();
            cancel = ui
                .add(Button::new("Cancel").variant(ButtonVariant::Outline))
                .clicked()
                || ui.input(|i| i.key_pressed(egui::Key::Escape));
        });
        if submit {
            self.submit_prompt();
        } else if cancel {
            self.prompt = None;
        }
    }

    fn keyboard(&mut self, ui: &egui::Ui) {
        if ui.ctx().egui_wants_keyboard_input() || self.prompt.is_some() {
            return;
        }
        let (delete, rename, all, up, enter) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::Delete),
                i.key_pressed(egui::Key::F2),
                i.modifiers.command && i.key_pressed(egui::Key::A),
                i.key_pressed(egui::Key::Backspace),
                i.key_pressed(egui::Key::Enter),
            )
        });
        if delete {
            self.run_action(Some("trash"));
        } else if rename {
            self.run_action(Some("rename"));
        } else if all {
            if let Some(browser) = &mut self.browser {
                browser.select_all();
            }
        } else if up {
            self.go(Browser::go_up);
        } else if enter {
            let target = self
                .browser
                .as_ref()
                .filter(|b| b.selection().len() == 1)
                .and_then(|b| b.selection().first().cloned());
            if let Some(path) = target {
                self.activate(&path);
            }
        }
    }

    /// The open folder's name as the page's title: the place's name for a
    /// place, so home reads "Home" and `/` "Computer".
    fn title_of_cwd(&self) -> String {
        let Some(cwd) = self.browser.as_ref().map(Browser::cwd) else {
            return "Files".to_owned();
        };
        self.places()
            .find(|(_, _, path)| path == cwd)
            .map(|(_, label, _)| label.to_owned())
            .or_else(|| cwd.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| cwd.display().to_string())
    }

    fn listing(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        let Some(browser) = &mut self.browser else {
            derisk_frame::empty(ui, tokens, "No folder open", "Choose a place on the left.");
            return;
        };
        let (key, descending) = browser.sort();
        let arrow = if descending { "pan-down" } else { "pan-up" };
        let mut activate = None;
        let mut clicked = None;
        let row_height = ui.spacing().interact_size.y;
        // The name column takes whatever the fixed columns leave, so the
        // table spans the window instead of huddling in its left third.
        let columns = |width: f32| {
            let size = 80.0_f32.min(width * 0.15);
            let modified = 136.0_f32.min(width * 0.4);
            [(width - size - modified).max(0.0), size, modified]
        };
        let width = ui.available_width();
        let header_rect = ui
            .allocate_exact_size(egui::vec2(width, row_height), egui::Sense::hover())
            .0;
        let mut x = header_rect.left();
        for ((label, column), w) in [
            ("Name", SortKey::Name),
            ("Size", SortKey::Size),
            ("Modified", SortKey::Modified),
        ]
        .into_iter()
        .zip(columns(width))
        {
            let cell = egui::Rect::from_min_size(
                egui::pos2(x, header_rect.top()),
                egui::vec2(w, row_height),
            );
            // Flat headers under one thin rule, like the table below them,
            // rather than a row of framed buttons.
            let button = if key == column {
                let color = ui.visuals().text_color();
                derisk_icons::button_with_text(ui.ctx(), arrow, label, 12.0, color)
            } else {
                egui::Button::new(label)
            }
            .frame(false);
            if ui
                .put(cell, |ui: &mut egui::Ui| {
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(button)
                    })
                    .inner
                })
                .clicked()
            {
                browser.sort_by(column);
            }
            x += w;
        }
        ui.painter().hline(
            header_rect.x_range(),
            header_rect.bottom(),
            tokens.border_stroke(),
        );
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let width = ui.available_width();
                let [name_w, size_w, _] = columns(width);
                for entry in browser.visible() {
                    let icon = match (entry.kind, entry.symlink) {
                        (Kind::Directory, _) => "folder",
                        (_, true) => "insert-link",
                        (Kind::File, _) => "text-x-generic",
                        (Kind::Other, _) => "dialog-question",
                    };
                    let selected = browser.is_selected(&entry.path);
                    // The whole row is one target, so a click anywhere along
                    // it selects the entry, not only on the name's text.
                    let (rect, row) =
                        ui.allocate_exact_size(egui::vec2(width, row_height), egui::Sense::click());
                    let name = entry.name.clone();
                    row.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::WidgetType::SelectableLabel,
                            true,
                            selected,
                            &name,
                        )
                    });
                    // Filled as a sidebar's page is, so a selected file and
                    // the open place read alike.
                    let fill = derisk_frame::row_fill(tokens, selected, row.hovered());
                    ui.painter()
                        .rect_filled(rect, tokens.control_radius(), fill);
                    let mut text = egui::RichText::new(&entry.name);
                    let mut color = ui.visuals().text_color();
                    if entry.is_hidden() {
                        text = text.color(tokens.muted_foreground);
                        color = tokens.muted_foreground;
                    }
                    let size = match entry.kind {
                        Kind::Directory => "—".to_owned(),
                        _ => fs_ops::human_size(entry.size),
                    };
                    let modified = entry.modified.map_or_else(
                        || "—".to_owned(),
                        |t| fs_ops::timestamp(t)[..16].replace('T', " "),
                    );
                    // Cells keep a small inset whatever the touch padding, so
                    // a full date still fits beside the name on a phone.
                    let pad = 4.0;
                    // The entry's icon leads the name cell, which starts
                    // after it so a truncated name never runs under it.
                    let icon_size = 16.0;
                    let icon_rect = egui::Rect::from_min_size(
                        egui::pos2(rect.left() + pad, rect.center().y - icon_size / 2.0),
                        egui::vec2(icon_size, icon_size),
                    );
                    derisk_icons::paint(ui.painter(), icon_rect, icon, color);
                    let mut x = rect.left();
                    for (text, w, inset) in [
                        (text, name_w, icon_size + pad),
                        (egui::RichText::new(size), size_w, 0.0),
                        (
                            egui::RichText::new(modified),
                            rect.right() - (x + name_w + size_w),
                            0.0,
                        ),
                    ] {
                        let cell = egui::Rect::from_min_size(
                            egui::pos2(x + pad + inset, rect.top()),
                            egui::vec2((w - 2.0 * pad - inset).max(0.0), row_height),
                        );
                        // `put` centers what it adds; cells read from the left.
                        ui.scope_builder(
                            egui::UiBuilder::new()
                                .max_rect(cell)
                                .layout(egui::Layout::left_to_right(egui::Align::Center)),
                            |ui| ui.add(egui::Label::new(text).truncate().selectable(false)),
                        );
                        x += w;
                    }
                    if row.double_clicked() {
                        activate = Some(entry.path.clone());
                    } else if row.clicked() {
                        let extend = ui.input(|i| i.modifiers.command || i.modifiers.shift);
                        clicked = Some((entry.path.clone(), extend));
                    }
                }
            });
        if let Some((path, extend)) = clicked {
            browser.select(&path, extend);
        }
        if let Some(path) = activate {
            self.activate(&path);
        }
    }
}

impl App for FilesApp {
    fn title(&self) -> &str {
        "Files"
    }

    fn ui(&mut self, ui: &mut egui::Ui, theme: &Theme) {
        self.keyboard(ui);
        let tokens = derisk_frame::begin(ui, theme);
        let mut place = None;
        let cwd = self.browser.as_ref().map(|b| b.cwd().to_owned());
        derisk_frame::navigation(ui, &tokens, "files-places", |nav| {
            let mut section = None;
            for (icon, label, path) in self.places() {
                // Your folders, then the trash and the whole computer.
                let title = if matches!(label, "Trash" | "Computer") {
                    "System"
                } else {
                    "Places"
                };
                if section != Some(title) {
                    nav.section(title);
                    section = Some(title);
                }
                if nav
                    .entry_icon(icon, label, cwd.as_ref() == Some(&path))
                    .clicked()
                {
                    place = Some(path);
                }
            }
        });
        if let Some(path) = place {
            self.open_place(&path);
        }
        let summary = self.browser.as_ref().map(|b| {
            let selected = b.selection().len();
            let shown = b.visible().count();
            let clip = match b.clipboard() {
                Some((ClipboardOp::Copy, p)) => format!(" · {} to copy", p.len()),
                Some((ClipboardOp::Cut, p)) => format!(" · {} to move", p.len()),
                None => String::new(),
            };
            format!("{shown} items · {selected} selected{clip}")
        });
        let narrow = derisk_frame::is_narrow(ui);
        derisk_frame::action_bar(ui, &tokens, "files-actions", |ui| {
            // An error has its own dialog, so the line does not repeat it.
            if let Some(status) = self.status.as_ref().filter(|_| self.error.is_none()) {
                ui.add(
                    egui::Label::new(egui::RichText::new(status).color(theme.accent)).truncate(),
                );
            }
            let layout = if narrow {
                // A phone has no room for the line beside the buttons, and
                // the buttons wrap onto a second row.
                egui::Layout::right_to_left(egui::Align::Center).with_main_wrap(true)
            } else {
                egui::Layout::right_to_left(egui::Align::Center)
            };
            ui.with_layout(layout, |ui| self.actions(ui));
        });
        if self.error.is_some() {
            let shown = ErrorDialog::new("files-error", &mut self.error).show(ui.ctx());
            if shown.closed {
                self.status = None;
            }
        }
        derisk_frame::Page::new()
            .wide()
            .fixed()
            .show(ui, &tokens, |ui| {
                let title = self.title_of_cwd();
                derisk_frame::header_with(
                    ui,
                    &tokens,
                    &title,
                    summary.as_deref().unwrap_or_default(),
                    |ui| self.moves(ui),
                );
                self.location_row(ui);
                self.prompt_ui(ui);
                ui.add_space(4.0);
                derisk_frame::Card::new()
                    .tight()
                    .fill()
                    .show(ui, &tokens, |ui| self.listing(ui, &tokens));
            });
    }
}
