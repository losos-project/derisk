//! The project setup drawn in the Agent's window with mcsapi's egui
//! components; the answers and the prompt are [`questions`](crate::questions)'s. Everything
//! it draws comes from mcsapi's components and the theme's [`Tokens`], so
//! restyling those restyles the wizard.

pub use crate::questions::*;
use egui::{Align, Layout, RichText, ScrollArea, Ui};
use mcsapi_components::{
    Alert, Badge, BadgeVariant, Button, ButtonSize, ButtonVariant, Card, Checkbox, Input,
    RadioGroup, Select, Textarea, Tokens, typography,
};

pub fn show(form: &mut SetupForm, ui: &mut Ui, tokens: &Tokens) -> Option<SetupAction> {
    let mut action = None;
    let width = ui.available_width().min(720.0);
    let bare = egui::Frame::new().inner_margin(egui::Margin::symmetric(0, 6));
    egui::Panel::top("setup-steps").frame(bare).show(ui, |ui| {
        column(ui, width, |ui| {
            ui.horizontal(|ui| {
                let title = if form.project_id.is_some() {
                    "Set up the project's app"
                } else {
                    "New project"
                };
                ui.label(typography::h3(tokens, title));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add(
                            Button::new("Cancel")
                                .variant(ButtonVariant::Ghost)
                                .size(ButtonSize::Sm),
                        )
                        .clicked()
                    {
                        action = Some(SetupAction::Cancel);
                    }
                });
            });
            ui.add_space(8.0);
            stepper(form, ui, tokens);
        });
    });
    // The buttons stay put below the card, however tall a step grows.
    let problem = form.setup.problem(form.step.min(STEPS.len() - 2));
    egui::Panel::bottom("setup-buttons")
        .frame(bare)
        .show(ui, |ui| {
            column(ui, width, |ui| {
                if let Some(problem) = &problem {
                    ui.label(typography::small(tokens, problem).color(tokens.destructive));
                    ui.add_space(4.0);
                }
                ui.horizontal(|ui| {
                    if form.step > 0
                        && ui
                            .add(Button::new("Back").variant(ButtonVariant::Outline))
                            .clicked()
                    {
                        form.go_to(form.step - 1);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if form.step + 1 < STEPS.len() {
                            if ui
                                .add(Button::new("Next").enabled(problem.is_none()))
                                .clicked()
                            {
                                form.go_to(form.step + 1);
                            }
                        } else if ui
                            .add(
                                Button::new("Create project and start")
                                    .enabled(problem.is_none() && !form.prompt.trim().is_empty()),
                            )
                            .clicked()
                        {
                            action = Some(SetupAction::Start);
                        }
                    });
                });
            });
        });
    egui::CentralPanel::no_frame().show(ui, |ui| {
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.set_max_width(width);
            Card::new().show(ui, |ui| {
                ui.set_width(ui.available_width());
                match form.step {
                    0 => kind_step(form, ui, tokens),
                    1 => language_step(form, ui, tokens),
                    2 => metadata_step(form, ui, tokens),
                    3 => description_step(form, ui, tokens),
                    _ => prompt_step(form, ui, tokens),
                }
            });
        });
    });
    action
}

fn stepper(form: &mut SetupForm, ui: &mut Ui, tokens: &Tokens) {
    let reachable = form.reachable();
    ui.horizontal_wrapped(|ui| {
        for (index, title) in STEPS.iter().enumerate() {
            let variant = if index == form.step {
                ButtonVariant::Default
            } else if index < form.step {
                ButtonVariant::Secondary
            } else {
                ButtonVariant::Ghost
            };
            let text = format!("{} {title}", index + 1);
            if ui
                .add(
                    Button::new(text)
                        .variant(variant)
                        .size(ButtonSize::Sm)
                        .enabled(index <= reachable),
                )
                .clicked()
            {
                form.go_to(index);
            }
            if index + 1 < STEPS.len() {
                ui.label(RichText::new("›").color(tokens.muted_foreground));
            }
        }
    });
}

fn kind_step(form: &mut SetupForm, ui: &mut Ui, tokens: &Tokens) {
    ui.label(typography::h4(tokens, "What kind of app is it?"));
    ui.label(typography::muted(tokens, "Pick one or more."));
    ui.add_space(8.0);
    let setup = &mut form.setup;
    ui.add(Checkbox::new(&mut setup.losos).label("A LosOS app"));
    hint(
        ui,
        tokens,
        "The agent writes a pm recipe (build.rhai) for it.",
    );
    ui.add(Checkbox::new(&mut setup.flatpak).label("A Flatpak"));
    hint(
        ui,
        tokens,
        "The agent writes a flatpak-builder manifest, installable from Bazaar.",
    );
    ui.add(Checkbox::new(&mut setup.wasm).label("A WASM app"));
    hint(
        ui,
        tokens,
        "Built for wasm32-wasip2 and run sandboxed; the Agent can preview it live.",
    );
    ui.add(Checkbox::new(&mut setup.palette_plugin).label("A derisk palette plugin"));
    hint(
        ui,
        tokens,
        "Rows in the desktop's command palette; the Agent writes a starter crate.",
    );
    ui.add(Checkbox::new(&mut setup.widget_plugin).label("A derisk overview widget"));
    hint(
        ui,
        tokens,
        "Cards on the desktop's overview, drawn in the theme; the Agent writes a starter crate.",
    );
}

fn language_step(form: &mut SetupForm, ui: &mut Ui, tokens: &Tokens) {
    ui.label(typography::h4(
        tokens,
        "Which language should the agent write it in?",
    ));
    ui.label(typography::muted(
        tokens,
        "These are the languages mcsapi supports. Rust is strongly recommended.",
    ));
    ui.add_space(8.0);
    let labels: Vec<&str> = Language::ALL
        .iter()
        .map(|language| language.label())
        .collect();
    let mut selected = Language::ALL
        .iter()
        .position(|language| *language == form.setup.language)
        .unwrap_or(0);
    if ui.add(RadioGroup::new(&mut selected, &labels)).changed()
        && let Some(language) = Language::ALL.get(selected)
    {
        form.setup.set_language(*language);
    }
    ui.add_space(8.0);
    let language = form.setup.language;
    if language == Language::Rust {
        ui.label(typography::muted(tokens, language.note()));
    } else {
        ui.add(
            Alert::new(format!("{} is not the recommended choice", language.name())).description(
                format!(
                    "{} Rust gets all of that and is what the Agent's tools are built around.",
                    language.note()
                ),
            ),
        );
    }
}

fn metadata_step(form: &mut SetupForm, ui: &mut Ui, tokens: &Tokens) {
    ui.label(typography::h4(tokens, "Package metadata"));
    ui.label(typography::muted(
        tokens,
        "Shared by every package below; the agent copies it into each format.",
    ));
    ui.add_space(8.0);
    let folder_hint = form.setup.folder().display().to_string();
    let display_hint = form.setup.display_name();
    let metadata = &mut form.setup.metadata;
    field(ui, tokens, "Package name", |ui| {
        ui.add(
            Input::new(&mut metadata.name)
                .placeholder("todo-app")
                .width(320.0),
        );
    });
    field(ui, tokens, "Display name", |ui| {
        let placeholder = if display_hint.is_empty() {
            "Todo App".to_owned()
        } else {
            display_hint
        };
        ui.add(
            Input::new(&mut metadata.display_name)
                .placeholder(placeholder)
                .width(320.0),
        );
    });
    field(ui, tokens, "Summary", |ui| {
        ui.add(
            Input::new(&mut metadata.summary)
                .placeholder("One line for app stores")
                .width(480.0),
        );
    });
    field(ui, tokens, "Version", |ui| {
        ui.add(Input::new(&mut metadata.version).width(120.0));
    });
    field(ui, tokens, "Licence", |ui| {
        ui.add(
            Input::new(&mut metadata.license)
                .placeholder("SPDX expression")
                .width(240.0),
        );
    });
    field(ui, tokens, "Homepage", |ui| {
        ui.add(
            Input::new(&mut metadata.homepage)
                .placeholder("https://…")
                .width(480.0),
        );
    });
    field(ui, tokens, "Folder", |ui| {
        ui.add(
            Input::new(&mut metadata.folder)
                .placeholder(folder_hint)
                .width(480.0),
        );
    });

    if form.setup.losos {
        ui.add_space(12.0);
        ui.label(typography::large(tokens, "pm"));
        let metadata = &mut form.setup.metadata;
        field(ui, tokens, "Dependencies", |ui| {
            ui.add(
                Input::new(&mut metadata.pm_dependencies)
                    .placeholder("pm packages, comma separated")
                    .width(480.0),
            );
        });
    }
    if form.setup.flatpak {
        ui.add_space(12.0);
        ui.label(typography::large(tokens, "Flatpak"));
        let metadata = &mut form.setup.metadata;
        field(ui, tokens, "App ID", |ui| {
            ui.add(
                Input::new(&mut metadata.flatpak_id)
                    .placeholder("org.example.TodoApp")
                    .width(320.0),
            );
        });
        field(ui, tokens, "Runtime", |ui| {
            let runtimes: Vec<&str> = FlatpakRuntime::ALL
                .iter()
                .map(|runtime| runtime.id())
                .collect();
            let mut selected = FlatpakRuntime::ALL
                .iter()
                .position(|runtime| *runtime == metadata.flatpak_runtime);
            if ui
                .add(Select::new("flatpak-runtime", &mut selected, &runtimes).width(240.0))
                .changed()
                && let Some(runtime) = selected.and_then(|index| FlatpakRuntime::ALL.get(index))
            {
                metadata.flatpak_runtime = *runtime;
                metadata.flatpak_runtime_version = runtime.default_version().into();
            }
            ui.add(Input::new(&mut metadata.flatpak_runtime_version).width(80.0));
        });
        field(ui, tokens, "Permissions", |ui| {
            ui.vertical(|ui| {
                ui.add(Checkbox::new(&mut metadata.flatpak_gpu).label("GPU"));
                ui.add(Checkbox::new(&mut metadata.flatpak_network).label("Network"));
                ui.add(Checkbox::new(&mut metadata.flatpak_home).label("Home folder"));
                ui.add(Checkbox::new(&mut metadata.flatpak_audio).label("Audio"));
            });
        });
    }
    if form.setup.wasm {
        ui.add_space(12.0);
        ui.label(typography::large(tokens, "WASM"));
        let metadata = &mut form.setup.metadata;
        field(ui, tokens, "Build", |ui| {
            let labels: Vec<&str> = WasmKind::ALL.iter().map(|kind| kind.label()).collect();
            let mut selected = WasmKind::ALL
                .iter()
                .position(|kind| *kind == metadata.wasm_kind)
                .unwrap_or(0);
            if ui.add(RadioGroup::new(&mut selected, &labels)).changed()
                && let Some(kind) = WasmKind::ALL.get(selected)
            {
                metadata.wasm_kind = *kind;
            }
        });
        field(ui, tokens, "Web app", |ui| {
            ui.add(Checkbox::new(&mut metadata.pwa).label("Also a PWA, with a web app manifest"));
        });
        if metadata.pwa {
            let display_hint = form.setup.display_name();
            let metadata = &mut form.setup.metadata;
            field(ui, tokens, "Short name", |ui| {
                let placeholder = if display_hint.is_empty() {
                    "Todo".to_owned()
                } else {
                    display_hint
                };
                ui.add(
                    Input::new(&mut metadata.pwa_short_name)
                        .placeholder(placeholder)
                        .width(200.0),
                );
            });
            field(ui, tokens, "Start URL", |ui| {
                ui.add(Input::new(&mut metadata.pwa_start_url).width(200.0));
            });
            field(ui, tokens, "Display", |ui| {
                let labels: Vec<&str> = PwaDisplay::ALL
                    .iter()
                    .map(|display| display.value())
                    .collect();
                let mut selected = PwaDisplay::ALL
                    .iter()
                    .position(|display| *display == metadata.pwa_display);
                if ui
                    .add(Select::new("pwa-display", &mut selected, &labels).width(200.0))
                    .changed()
                    && let Some(display) = selected.and_then(|index| PwaDisplay::ALL.get(index))
                {
                    metadata.pwa_display = *display;
                }
            });
            field(ui, tokens, "Colours", |ui| {
                ui.label(typography::small(tokens, "theme"));
                ui.add(Input::new(&mut metadata.pwa_theme_color).width(90.0));
                ui.label(typography::small(tokens, "background"));
                ui.add(Input::new(&mut metadata.pwa_background_color).width(90.0));
            });
        }
    }
}

fn description_step(form: &mut SetupForm, ui: &mut Ui, tokens: &Tokens) {
    ui.label(typography::h4(tokens, "What should the app do?"));
    ui.label(typography::muted(
        tokens,
        "Describe it the way you would to a person: what it is for, what is on screen, what happens when you use it.",
    ));
    ui.add_space(8.0);
    ui.add(
        Textarea::new(&mut form.setup.description)
            .placeholder(
                "A todo list with due dates. Items can be filtered by done, today and overdue…",
            )
            .rows(10),
    );
}

fn prompt_step(form: &mut SetupForm, ui: &mut Ui, tokens: &Tokens) {
    ui.horizontal(|ui| {
        ui.label(typography::h4(tokens, "The prompt"));
        if form.prompt_edited {
            ui.add(Badge::new("Edited").variant(BadgeVariant::Secondary));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if form.prompt_edited
                && ui
                    .add(
                        Button::new("Rebuild from answers")
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Sm),
                    )
                    .clicked()
            {
                form.prompt = form.setup.prompt();
                form.prompt_edited = false;
            }
        });
    });
    ui.label(typography::muted(
        tokens,
        "This is what the agent gets as the project's first message. Edit it freely.",
    ));
    ui.add_space(8.0);
    if ui.add(Textarea::new(&mut form.prompt).rows(22)).changed() {
        form.prompt_edited = true;
    }
}

/// A column `width` wide at the left of `ui`, so the steps, the card and
/// the buttons line up.
fn column(ui: &mut Ui, width: f32, content: impl FnOnce(&mut Ui)) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, 0.0),
        Layout::top_down(Align::Min),
        content,
    );
}

fn hint(ui: &mut Ui, tokens: &Tokens, text: &str) {
    ui.horizontal(|ui| {
        ui.add_space(26.0);
        ui.label(typography::small(tokens, text).color(tokens.muted_foreground));
    });
    ui.add_space(6.0);
}

fn field(ui: &mut Ui, tokens: &Tokens, label: &str, content: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [120.0, 20.0],
            egui::Label::new(typography::small(tokens, label).color(tokens.muted_foreground)),
        );
        content(ui);
    });
    ui.add_space(4.0);
}
