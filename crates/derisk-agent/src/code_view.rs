//! The code view: the project's files and an editor, in the place the chat
//! takes in the chat view, so switching between talking to the agent and
//! reading what it wrote keeps the preview and the rest of the window still.

use std::{
    fs,
    path::{Path, PathBuf},
};

use egui::{Align, Layout, RichText, ScrollArea, TextEdit, Ui};
use mcsapi_components::{
    Badge, BadgeVariant, Button, ButtonSize, ButtonVariant, Tokens, typography,
};
use miette::{Context as _, IntoDiagnostic as _, Result};

/// Folders nobody reads by hand, and which can hold more files than the tree
/// should list.
const SKIPPED: [&str; 4] = [".git", "target", "node_modules", ".flatpak-builder"];
/// Files larger than this open read-only: egui lays out the whole text every
/// frame.
const EDITABLE_BYTES: u64 = 1024 * 1024;

#[derive(Default)]
pub struct CodeView {
    root: Option<PathBuf>,
    /// Folders the user opened in the tree.
    expanded: Vec<PathBuf>,
    open: Option<OpenFile>,
}

struct OpenFile {
    path: PathBuf,
    text: String,
    saved: String,
    /// Why the file cannot be edited, if it cannot.
    read_only: Option<String>,
}

impl CodeView {
    /// Shows `root`, forgetting what was open when the root changes.
    pub fn set_root(&mut self, root: Option<PathBuf>) {
        if self.root != root {
            *self = Self {
                root,
                ..Self::default()
            };
        }
    }

    pub fn has_unsaved_changes(&self) -> bool {
        self.open
            .as_ref()
            .is_some_and(|open| open.read_only.is_none() && open.text != open.saved)
    }

    pub fn show(&mut self, ui: &mut Ui, tokens: &Tokens) -> Result<()> {
        let Some(root) = self.root.clone() else {
            ui.add_space(ui.available_height() * 0.3);
            ui.vertical_centered(|ui| {
                ui.label(typography::h4(tokens, "No folder to show"));
                ui.label(typography::muted(
                    tokens,
                    "Add a folder to the project and its files show up here.",
                ));
            });
            return Ok(());
        };
        let mut result = Ok(());
        egui::Panel::left("code-tree")
            .resizable(true)
            .default_size(220.0)
            .size_range(140.0..=420.0)
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(0, 4)))
            .show(ui, |ui| {
                ui.add(
                    egui::Label::new(
                        typography::small(tokens, root.display().to_string())
                            .color(tokens.muted_foreground),
                    )
                    .truncate(),
                );
                ui.add_space(4.0);
                ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    if let Some(path) = self.tree(ui, tokens, &root, 0)
                        && let Err(error) = self.open_file(path)
                    {
                        result = Err(error);
                    }
                });
            });
        egui::CentralPanel::no_frame().show(ui, |ui| {
            if let Err(error) = self.editor(ui, tokens) {
                result = Err(error);
            }
        });
        result
    }

    /// Draws `dir`'s entries, returning a file the user clicked.
    fn tree(&mut self, ui: &mut Ui, tokens: &Tokens, dir: &Path, depth: usize) -> Option<PathBuf> {
        let mut entries = match list(dir) {
            Ok(entries) => entries,
            Err(error) => {
                ui.label(
                    typography::small(tokens, crate::describe(&error)).color(tokens.destructive),
                );
                return None;
            }
        };
        // Folders first, then files, each alphabetically, like a file manager.
        entries.sort_by(|(a, a_dir), (b, b_dir)| b_dir.cmp(a_dir).then_with(|| a.cmp(b)));
        let mut clicked = None;
        for (path, is_dir) in entries {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let expanded = self.expanded.contains(&path);
            let selected = self.open.as_ref().is_some_and(|open| open.path == path);
            let icon = match (is_dir, expanded) {
                (true, true) => "⏷",
                (true, false) => "⏵",
                (false, _) => " ",
            };
            let text = RichText::new(format!("{icon} {name}")).color(if selected {
                tokens.foreground
            } else {
                tokens.muted_foreground
            });
            let response = ui
                .horizontal(|ui| {
                    ui.add_space(depth as f32 * 12.0);
                    ui.add(
                        egui::Label::new(text)
                            .truncate()
                            .sense(egui::Sense::click()),
                    )
                })
                .inner;
            if response.clicked() {
                if !is_dir {
                    clicked = Some(path.clone());
                } else if expanded {
                    self.expanded.retain(|open| open != &path);
                } else {
                    self.expanded.push(path.clone());
                }
            }
            if is_dir
                && expanded
                && let Some(path) = self.tree(ui, tokens, &path, depth + 1)
            {
                clicked = Some(path);
            }
        }
        clicked
    }

    fn open_file(&mut self, path: PathBuf) -> Result<()> {
        let size = fs::metadata(&path)
            .into_diagnostic()
            .with_context(|| format!("reading {}", path.display()))?
            .len();
        let bytes = fs::read(&path)
            .into_diagnostic()
            .with_context(|| format!("reading {}", path.display()))?;
        let (text, read_only) = match String::from_utf8(bytes) {
            Ok(text) if size <= EDITABLE_BYTES => (text, None),
            Ok(text) => (text, Some("too large to edit here".to_owned())),
            Err(_) => (String::new(), Some("not text".to_owned())),
        };
        self.open = Some(OpenFile {
            path,
            saved: text.clone(),
            text,
            read_only,
        });
        Ok(())
    }

    fn editor(&mut self, ui: &mut Ui, tokens: &Tokens) -> Result<()> {
        let Some(open) = &mut self.open else {
            ui.add_space(ui.available_height() * 0.3);
            ui.vertical_centered(|ui| {
                ui.label(typography::muted(tokens, "Pick a file on the left."));
            });
            return Ok(());
        };
        let dirty = open.read_only.is_none() && open.text != open.saved;
        let mut save = false;
        ui.horizontal(|ui| {
            let name = open
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            ui.label(typography::large(tokens, name));
            if let Some(reason) = &open.read_only {
                ui.add(Badge::new(format!("Read-only: {reason}")).variant(BadgeVariant::Outline));
            } else if dirty {
                ui.add(Badge::new("Unsaved").variant(BadgeVariant::Secondary));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let shortcut =
                    ui.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::S));
                save = ui
                    .add(
                        Button::new("Save")
                            .size(ButtonSize::Sm)
                            .variant(ButtonVariant::Secondary)
                            .enabled(dirty),
                    )
                    .clicked()
                    || (shortcut && dirty);
                if dirty
                    && ui
                        .add(
                            Button::new("Revert")
                                .size(ButtonSize::Sm)
                                .variant(ButtonVariant::Ghost),
                        )
                        .clicked()
                {
                    open.text = open.saved.clone();
                }
            });
        });
        ui.separator();
        ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            let mut editor = TextEdit::multiline(&mut open.text)
                .code_editor()
                .desired_width(f32::INFINITY)
                .desired_rows(40);
            if open.read_only.is_some() {
                editor = editor.interactive(false);
            }
            ui.add(editor);
        });
        if save {
            fs::write(&open.path, &open.text)
                .into_diagnostic()
                .with_context(|| format!("saving {}", open.path.display()))?;
            open.saved = open.text.clone();
        }
        Ok(())
    }
}

fn list(dir: &Path) -> Result<Vec<(PathBuf, bool)>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)
        .into_diagnostic()
        .with_context(|| format!("listing {}", dir.display()))?
    {
        let entry = entry.into_diagnostic()?;
        let name = entry.file_name();
        if SKIPPED.iter().any(|skipped| name == *skipped) {
            continue;
        }
        entries.push((entry.path(), entry.file_type().into_diagnostic()?.is_dir()));
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_skips_build_output_and_git() -> Result<()> {
        let dir = tempfile::tempdir().into_diagnostic()?;
        for name in [".git", "target", "src"] {
            fs::create_dir(dir.path().join(name)).into_diagnostic()?;
        }
        fs::write(dir.path().join("Cargo.toml"), "").into_diagnostic()?;
        let mut names: Vec<_> = list(dir.path())?
            .into_iter()
            .map(|(path, is_dir)| (path.file_name().map(|name| name.to_owned()), is_dir))
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                (Some("Cargo.toml".into()), false),
                (Some("src".into()), true)
            ]
        );
        Ok(())
    }

    #[test]
    fn opening_tracks_unsaved_changes() -> Result<()> {
        let dir = tempfile::tempdir().into_diagnostic()?;
        let path = dir.path().join("main.rs");
        fs::write(&path, "fn main() {}\n").into_diagnostic()?;
        let mut view = CodeView::default();
        view.set_root(Some(dir.path().to_owned()));
        view.open_file(path)?;
        assert!(!view.has_unsaved_changes());
        if let Some(open) = &mut view.open {
            open.text.push_str("// edited\n");
        }
        assert!(view.has_unsaved_changes());
        view.set_root(Some(dir.path().join("elsewhere")));
        assert!(view.open.is_none());
        Ok(())
    }

    #[test]
    fn binary_files_open_read_only() -> Result<()> {
        let dir = tempfile::tempdir().into_diagnostic()?;
        let path = dir.path().join("icon.png");
        fs::write(&path, [0x89, 0x50, 0xff, 0xfe]).into_diagnostic()?;
        let mut view = CodeView::default();
        view.open_file(path)?;
        assert!(
            view.open
                .as_ref()
                .is_some_and(|open| open.read_only.is_some())
        );
        assert!(!view.has_unsaved_changes());
        Ok(())
    }
}
