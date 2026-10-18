//! derisk Text Editor: open, edit, find, and save UTF-8 text files.
//!
//! [`Document`] is the file model; [`EditorApp`] draws it.
//!
//! ```
//! use derisk_editor::Document;
//!
//! let path = std::env::temp_dir().join(format!("derisk-editor-doc-{}.txt", std::process::id()));
//! let mut doc = Document::default();
//! doc.text.push_str("hello");
//! assert!(doc.is_dirty());
//! doc.save_as(&path)?;
//! assert!(!doc.is_dirty());
//! assert_eq!(Document::open(&path)?.text, "hello");
//! # std::fs::remove_file(&path)?;
//! # Ok::<(), std::io::Error>(())
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
};

use derisk_sys::{
    fs,
    unix::{OpenOptionsExt, PermissionsExt},
};

use mcsapi_components::{Button, ButtonVariant, ErrorDialog, Tokens};
use mcsapi_ui::{App, Context as _, DocLink, Error, Theme, egui};

/// The documentation section on files the editor could not open or save.
fn doc() -> DocLink {
    DocLink::new("troubleshooting").section("the-editor-could-not-open-or-save-a-file")
}

/// Files larger than this are refused rather than loaded into the editor.
pub const MAX_FILE_SIZE: u64 = 8 * 1024 * 1024;

/// A text file being edited.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Document {
    path: Option<PathBuf>,
    saved: String,
    /// The current contents.
    pub text: String,
}

impl Document {
    /// Reads a UTF-8 file of at most [`MAX_FILE_SIZE`] bytes.
    pub fn open(path: &Path) -> io::Result<Self> {
        let size = fs::metadata(path)?.len();
        if size > MAX_FILE_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                format!("file is larger than {} MiB", MAX_FILE_SIZE / 1024 / 1024),
            ));
        }
        let text = String::from_utf8(fs::read(path)?)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not a UTF-8 text file"))?;
        Ok(Self {
            path: Some(path.to_owned()),
            saved: text.clone(),
            text,
        })
    }

    /// The file this document is saved to, if any.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The file name, or "Untitled".
    pub fn name(&self) -> String {
        self.path
            .as_deref()
            .and_then(Path::file_name)
            .map_or_else(|| "Untitled".into(), |n| n.to_string_lossy().into_owned())
    }

    /// Whether the text differs from what was last opened or saved.
    pub fn is_dirty(&self) -> bool {
        self.text != self.saved
    }

    /// Saves to the document's file. Fails for an untitled document.
    pub fn save(&mut self) -> io::Result<()> {
        let path = self
            .path
            .clone()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "choose where to save"))?;
        self.save_as(&path)
    }

    /// Saves to `path` atomically and makes it the document's file.
    pub fn save_as(&mut self, path: &Path) -> io::Result<()> {
        // Keep the original file's permissions, such as an executable bit,
        // and give a new file what a plain create would under the umask.
        let mode = fs::metadata(path).map_or(0o666 & !umask(), |m| m.permissions().mode() & 0o7777);
        let (temporary, mut file) = create_beside(path)?;
        let written = file
            .set_permissions(fs::Permissions::from_mode(mode))
            .and_then(|()| file.write_all(self.text.as_bytes()))
            .and_then(|()| file.sync_all());
        drop(file);
        written
            .and_then(|()| fs::rename(&temporary, path))
            .inspect_err(|_| {
                let _ = fs::remove_file(&temporary);
            })?;
        self.path = Some(path.to_owned());
        self.saved.clone_from(&self.text);
        Ok(())
    }

    /// Byte offsets of every case-insensitive match of `needle` (exact-case
    /// when the text has characters whose lowercase form changes length).
    pub fn find(&self, needle: &str) -> Vec<usize> {
        if needle.is_empty() {
            return Vec::new();
        }
        let haystack = self.text.to_lowercase();
        // Lowercasing can change byte lengths, which would make the offsets
        // wrong; fall back to an exact-case search then.
        if haystack.len() != self.text.len() {
            return self.text.match_indices(needle).map(|(i, _)| i).collect();
        }
        haystack
            .match_indices(&needle.to_lowercase())
            .map(|(i, _)| i)
            .collect()
    }

    /// Replaces every case-sensitive occurrence of `from`. Returns the count.
    pub fn replace_all(&mut self, from: &str, to: &str) -> usize {
        if from.is_empty() {
            return 0;
        }
        let count = self.text.matches(from).count();
        if count > 0 {
            self.text = self.text.replace(from, to);
        }
        count
    }

    /// Puts back the text as it was last opened or saved.
    pub fn revert(&mut self) {
        self.text.clone_from(&self.saved);
    }

    /// 1-based line and column of a character index into the text.
    pub fn line_column(&self, char_index: usize) -> (usize, usize) {
        let before: String = self.text.chars().take(char_index).collect();
        let line = before.matches('\n').count() + 1;
        let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
        (line, column)
    }
}

/// The Text Editor app.
#[derive(Debug, Default)]
pub struct EditorApp {
    /// The open document.
    pub document: Document,
    location: String,
    find: String,
    replace: String,
    show_find: bool,
    cursor: usize,
    status: Option<String>,
    error: Option<Error>,
    confirm_discard: Option<Pending>,
}

#[derive(Clone, Debug)]
enum Pending {
    New,
    Open(PathBuf),
}

impl EditorApp {
    /// Opens `path`, or starts an untitled document when it does not exist yet.
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let mut app = Self {
            location: path.display().to_string(),
            ..Self::default()
        };
        match Document::open(&path) {
            Ok(document) => app.document = document,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                app.status = Some("New file; it is created when you save".into());
            }
            Err(error) => app.fail_open(error, &path),
        }
        app
    }

    /// An untitled document.
    pub fn new() -> Self {
        Self::default()
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

    fn fail_open(&mut self, error: io::Error, path: &std::path::Path) {
        self.fail(
            Error::plain(error)
                .context(format!("Could not open {}", path.display()))
                .with_doc(doc()),
        );
    }

    /// Saves to the location field, which may differ from the open file.
    pub fn save(&mut self) {
        let target = PathBuf::from(self.location.trim());
        let result = if self.location.trim().is_empty() {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "type a file path to save to",
            ))
        } else if self.document.path() == Some(target.as_path()) {
            self.document.save()
        } else {
            self.document.save_as(&target)
        };
        self.error = None;
        match result.context("Could not save").doc(doc()) {
            Ok(()) => self.status = Some(format!("Saved {}", self.document.name())),
            Err(error) => self.fail(error),
        }
    }

    fn run(&mut self, pending: Pending) {
        if self.document.is_dirty() && self.confirm_discard.is_none() {
            self.confirm_discard = Some(pending);
            return;
        }
        self.confirm_discard = None;
        self.error = None;
        match pending {
            Pending::New => {
                self.document = Document::default();
                self.location.clear();
                self.status = None;
            }
            Pending::Open(path) => match Document::open(&path) {
                Ok(document) => {
                    self.document = document;
                    self.status = None;
                }
                Err(error) => self.fail_open(error, &path),
            },
        }
    }

    /// Whether Save has something to do: changes, or a location other
    /// than the open file's.
    fn can_save(&self) -> bool {
        let location = self.location.trim();
        self.document.is_dirty()
            || (!location.is_empty() && self.document.path() != Some(Path::new(location)))
    }

    fn header(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        let description = match self.document.path() {
            Some(path) => path
                .parent()
                .map_or_else(String::new, |p| p.display().to_string()),
            None => "Not saved yet".to_owned(),
        };
        let mut pending = None;
        derisk_frame::header_with(ui, tokens, &self.document.name(), &description, |ui| {
            if ui
                .add(Button::new("Open").variant(ButtonVariant::Outline))
                .on_hover_text("Opens the file the location names")
                .clicked()
            {
                pending = Some(Pending::Open(PathBuf::from(self.location.trim())));
            }
            if ui
                .add(Button::new("New").variant(ButtonVariant::Outline))
                .clicked()
            {
                pending = Some(Pending::New);
            }
            let find = if self.show_find {
                ButtonVariant::Secondary
            } else {
                ButtonVariant::Ghost
            };
            if ui
                .add(Button::new("Find").variant(find))
                .on_hover_text("Ctrl+F")
                .clicked()
            {
                self.show_find = !self.show_find;
            }
        });
        if let Some(pending) = pending {
            self.run(pending);
        }
        ui.horizontal(|ui| {
            ui.label("Location");
            ui.add(
                derisk_frame::field(&mut self.location)
                    .hint_text("/path/to/file.txt")
                    .desired_width(f32::INFINITY),
            );
        });
        ui.add_space(4.0);
    }

    fn confirmation(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        let Some(pending) = self.confirm_discard.clone() else {
            return;
        };
        let mut answer = None;
        derisk_frame::Card::new()
            .stripe(tokens.destructive)
            .show(ui, tokens, |ui| {
                ui.strong(format!("Discard changes to {}?", self.document.name()));
                ui.label(derisk_frame::note(
                    tokens,
                    "What you changed since it was last saved is lost.",
                ));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(Button::new("Discard").variant(ButtonVariant::Destructive))
                        .clicked()
                    {
                        answer = Some(true);
                    }
                    if ui
                        .add(Button::new("Keep editing").variant(ButtonVariant::Outline))
                        .clicked()
                    {
                        answer = Some(false);
                    }
                });
            });
        ui.add_space(4.0);
        match answer {
            Some(true) => self.run(pending),
            Some(false) => self.confirm_discard = None,
            None => {}
        }
    }

    fn find_bar(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        if !self.show_find {
            return;
        }
        let phone = ui.available_width() < derisk_frame::PHONE;
        derisk_frame::Card::new().tight().show(ui, tokens, |ui| {
            // Wraps where the window is too narrow, as on a phone.
            ui.horizontal_wrapped(|ui| {
                let width = if phone {
                    ui.available_width()
                } else {
                    (ui.available_width() * 0.32).min(260.0)
                };
                ui.add(
                    derisk_frame::field(&mut self.find)
                        .hint_text("Find")
                        .desired_width(width),
                );
                ui.add(
                    derisk_frame::field(&mut self.replace)
                        .hint_text("Replace with")
                        .desired_width(width),
                );
                if ui
                    .add(Button::new("Replace all").variant(ButtonVariant::Outline))
                    .clicked()
                {
                    let count = self.document.replace_all(&self.find, &self.replace);
                    self.status = Some(format!("Replaced {count} match(es)"));
                }
                if !self.find.is_empty() {
                    let matches = self.document.find(&self.find).len();
                    ui.label(derisk_frame::note(tokens, &format!("{matches} match(es)")));
                }
            });
        });
        ui.add_space(4.0);
    }

    fn action_bar(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        let can_save = self.can_save();
        let dirty = self.document.is_dirty();
        let (mut save, mut revert) = (false, false);
        derisk_frame::action_bar(ui, tokens, "editor-actions", |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                save = ui
                    .add(Button::new("Save").enabled(can_save))
                    .on_hover_text("Ctrl+S")
                    .clicked();
                revert = ui
                    .add(
                        Button::new("Revert")
                            .variant(ButtonVariant::Outline)
                            .enabled(dirty && self.document.path().is_some()),
                    )
                    .clicked();
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    // An error has its own dialog, so the bar does not
                    // repeat it.
                    let status = self.status.as_ref().filter(|_| self.error.is_none());
                    if dirty {
                        ui.label(egui::RichText::new("Unsaved changes").color(tokens.primary));
                    } else if let Some(status) = status {
                        ui.add(egui::Label::new(status.as_str()).truncate());
                    }
                    let (line, column) = self.document.line_column(self.cursor);
                    ui.add(
                        egui::Label::new(derisk_frame::note(
                            tokens,
                            &format!(
                                "Line {line}, column {column} · {} lines",
                                self.document.text.lines().count().max(1)
                            ),
                        ))
                        .truncate(),
                    );
                });
            });
        });
        if save {
            self.save();
        }
        if revert {
            self.document.revert();
            self.status = Some(format!("Reverted {}", self.document.name()));
        }
    }

    fn text(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        derisk_frame::Card::new()
            .tight()
            .fill()
            .show(ui, tokens, |ui| {
                egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                    // The text area fills the card, so a click anywhere below
                    // the last line still puts the cursor in the document.
                    let output = egui::TextEdit::multiline(&mut self.document.text)
                        .code_editor()
                        .desired_width(f32::INFINITY)
                        .min_size(ui.available_size())
                        .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(16, 12)))
                        .show(ui);
                    if let Some(range) = output.cursor_range {
                        self.cursor = range.primary.index.into();
                    }
                });
            });
    }
}

impl App for EditorApp {
    fn title(&self) -> &str {
        "Text Editor"
    }

    fn ui(&mut self, ui: &mut egui::Ui, theme: &Theme) {
        let (save, find) = ui.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::S),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::F),
            )
        });
        if save {
            self.save();
        }
        if find {
            self.show_find = !self.show_find;
        }
        let tokens = derisk_frame::begin(ui, theme);
        self.action_bar(ui, &tokens);
        if self.error.is_some() {
            let shown = ErrorDialog::new("editor-error", &mut self.error).show(ui.ctx());
            if shown.closed {
                self.status = None;
            }
        }
        derisk_frame::Page::new()
            .wide()
            .fixed()
            .show(ui, &tokens, |ui| {
                self.header(ui, &tokens);
                self.confirmation(ui, &tokens);
                self.find_bar(ui, &tokens);
                self.text(ui, &tokens);
            });
    }
}

/// Creates a new, empty file next to `path` for an atomic save, readable only
/// by its owner until the caller sets its mode.
///
/// The name is fresh each time and the file is created with `O_EXCL`, which
/// never follows a symlink and never opens a file that is already there. A
/// fixed name (`<file>.derisk-save`, as this used to be) could be planted in a
/// directory another user can write, as a symlink to one of this user's files
/// or as a file the other user owns, and the save would then write the
/// document through it.
fn create_beside(path: &Path) -> io::Result<(PathBuf, fs::File)> {
    static SAVES: AtomicU32 = AtomicU32::new(0);
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a file name"))?;
    let nanos = derisk_sys::time::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    for _ in 0..64 {
        let mut temporary = std::ffi::OsString::from(".");
        temporary.push(name);
        temporary.push(format!(
            ".derisk-save-{}-{}-{nanos:x}",
            derisk_sys::process::id(),
            SAVES.fetch_add(1, Ordering::Relaxed)
        ));
        let temporary = path.with_file_name(temporary);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
        {
            Ok(file) => return Ok((temporary, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free name to save through",
    ))
}

/// The process umask, read from /proc so it is not changed to find out.
fn umask() -> u32 {
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Umask:"))
                .and_then(|v| u32::from_str_radix(v.trim(), 8).ok())
        })
        .unwrap_or(0o022)
}
