//! The Android app: Home, the Pixel with Start and Stop and its setup's
//! progress, and Apps, every app on it as an icon that opens it in a
//! window of its own, drawn as Settings is.

use mcsapi_components::{Button, ButtonVariant, Tokens};
use mcsapi_ui::{App, Theme, egui};

use crate::{AndroidApp, State, Status};

#[cfg(not(target_arch = "wasm32"))]
use crate::worker::{Event, Order, Worker};

/// The app's ID.
pub const APP_ID: &str = "org.derisk.android";

/// What the Pixel is, as `android-app-support` makes it.
const DEVICE: &str = "Pixel 10 Pro";
const SYSTEM: &str = "Android 17 with Google Play";

/// The Android app.
pub struct AndroidSupport {
    #[cfg(not(target_arch = "wasm32"))]
    worker: Worker,
    status: Status,
    /// Why `android-app-support` cannot be asked, while it cannot.
    unavailable: Option<String>,
    apps: Vec<AndroidApp>,
    page: Page,
    search: String,
    /// What the thread is doing for the person, while it is.
    busy: Option<String>,
    said: Option<Result<String, String>>,
    /// Whether the setup was started from here already: the first time
    /// the app sees a Pixel that was never set up it sets it up, as
    /// WinBoat's wizard would once it has its answers, and only once.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    asked_setup: bool,
    /// The apps' icons, by package, once loaded.
    icons: std::collections::HashMap<String, Option<egui::TextureHandle>>,
}

/// The page shown.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Page {
    #[default]
    Home,
    Apps,
}

impl std::fmt::Debug for AndroidSupport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AndroidSupport")
            .field("status", &self.status)
            .field("apps", &self.apps.len())
            .finish_non_exhaustive()
    }
}

impl Default for AndroidSupport {
    fn default() -> Self {
        Self::new()
    }
}

impl AndroidSupport {
    /// Starts asking `android-app-support` about the Pixel.
    pub fn new() -> Self {
        Self {
            #[cfg(not(target_arch = "wasm32"))]
            worker: Worker::start(crate::data_dir()),
            status: Status::default(),
            #[cfg(not(target_arch = "wasm32"))]
            unavailable: None,
            #[cfg(target_arch = "wasm32")]
            unavailable: Some(
                "Android runs in a virtual Pixel on the computer itself, which this demo in a \
                 browser does not have."
                    .into(),
            ),
            apps: Vec::new(),
            page: Page::Home,
            search: String::new(),
            busy: None,
            said: None,
            asked_setup: false,
            icons: std::collections::HashMap::new(),
        }
    }

    /// The Pixel's state as last read.
    pub fn status(&self) -> &Status {
        &self.status
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn receive(&mut self) {
        let events: Vec<Event> = self.worker.events().collect();
        for event in events {
            match event {
                Event::Status(status) => {
                    self.unavailable = None;
                    self.status = status;
                }
                Event::Unavailable(why) => self.unavailable = Some(why),
                Event::Apps(apps) => {
                    // Icons are drawn again by the Pixel when the list is.
                    self.icons.clear();
                    self.apps = apps;
                }
                Event::Done(done) => {
                    self.busy = None;
                    self.said = Some(done);
                }
            }
        }
        if self.status.state == State::New && !self.asked_setup {
            self.asked_setup = true;
            self.order("Setting Android up".into(), Order::SetUp);
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn receive(&mut self) {}

    #[cfg(not(target_arch = "wasm32"))]
    fn order(&mut self, doing: String, order: Order) {
        if self.worker.send(order) {
            self.busy = Some(doing);
            self.said = None;
        } else {
            self.said = Some(Err("The Android thread is not answering".into()));
        }
    }

    /// Opens `package` in a window of its own.
    fn open(&mut self, app: &AndroidApp) {
        #[cfg(not(target_arch = "wasm32"))]
        self.order(
            format!("Opening {}", app.name),
            Order::Open(app.package.clone()),
        );
        #[cfg(target_arch = "wasm32")]
        let _ = app;
    }

    fn sidebar(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        let mut picked = None;
        derisk_frame::navigation(ui, tokens, "android-pages", |nav| {
            nav.section("Android");
            if nav
                .entry_icon("phone", "Home", self.page == Page::Home)
                .clicked()
            {
                picked = Some(Page::Home);
            }
            if nav
                .entry_icon("view-app-grid", "Apps", self.page == Page::Apps)
                .clicked()
            {
                picked = Some(Page::Apps);
            }
        });
        if let Some(page) = picked {
            self.page = page;
        }
    }

    /// What the Pixel is doing along the bottom, in words.
    fn action_bar(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        let mut dismiss = false;
        derisk_frame::action_bar(ui, tokens, "android-actions", |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.said.is_some() && self.busy.is_none() {
                    dismiss = ui
                        .add(Button::new("Dismiss").variant(ButtonVariant::Ghost))
                        .clicked();
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    if let Some(busy) = &self.busy {
                        ui.spinner();
                        ui.label(busy);
                        return;
                    }
                    match &self.said {
                        Some(Ok(done)) => {
                            ui.add(egui::Label::new(done.as_str()).truncate());
                        }
                        Some(Err(error)) => {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(error.as_str()).color(tokens.destructive),
                                )
                                .truncate(),
                            );
                        }
                        None => {
                            let count = match self.apps.len() {
                                0 => "No apps listed yet".to_owned(),
                                1 => "1 app".to_owned(),
                                n => format!("{n} apps"),
                            };
                            ui.label(derisk_frame::note(tokens, &count));
                        }
                    }
                });
            });
        });
        if dismiss {
            self.said = None;
        }
    }

    fn page(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        if let Some(why) = self.unavailable.clone() {
            derisk_frame::header(ui, tokens, "Android", "");
            derisk_frame::card(ui, tokens, |ui| {
                derisk_frame::empty(ui, tokens, "Android app support is not available", &why);
            });
            return;
        }
        match self.page {
            Page::Home => self.home(ui, tokens),
            Page::Apps => self.apps_page(ui, tokens),
        }
    }

    /// The Pixel: its state with Start or Stop beside it, what it is, and
    /// the apps the person installed.
    fn home(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        let idle = self.busy.is_none();
        let state = self.status.state;
        let mut order = None;
        derisk_frame::header_with(ui, tokens, DEVICE, state.describe(), |ui| match state {
            State::Running | State::Starting => {
                if ui.add(Button::new("Stop").enabled(idle)).clicked() {
                    order = Some(Act::Stop);
                }
                if ui
                    .add(
                        Button::new("Show phone")
                            .variant(ButtonVariant::Outline)
                            .enabled(idle && state == State::Running),
                    )
                    .clicked()
                {
                    order = Some(Act::Phone);
                }
            }
            State::Off => {
                if ui.add(Button::new("Start").enabled(idle)).clicked() {
                    order = Some(Act::Start);
                }
            }
            State::New | State::Failed => {
                let label = if state == State::Failed {
                    "Try again"
                } else {
                    "Set up"
                };
                if ui.add(Button::new(label).enabled(idle)).clicked() {
                    order = Some(Act::SetUp);
                }
            }
            State::SettingUp | State::Unsupported | State::Unknown => {}
        });
        if let Some(act) = order {
            self.act(act);
        }

        match state {
            State::SettingUp | State::New => {
                derisk_frame::card(ui, tokens, |ui| {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(egui::RichText::new("Setting Android up").strong());
                    });
                    let detail = if self.status.detail.is_empty() {
                        "Starting"
                    } else {
                        &self.status.detail
                    };
                    ui.label(detail);
                    ui.label(derisk_frame::note(
                        tokens,
                        "Android is downloaded from Google the first time, about 3 GB, and \
                         the Pixel then starts for the first time. It carries on while this \
                         window is closed, and a notification says when it is ready.",
                    ));
                });
            }
            State::Failed => {
                derisk_frame::Card::new()
                    .stripe(tokens.destructive)
                    .show(ui, tokens, |ui| {
                        ui.label(egui::RichText::new("Setup did not finish").strong());
                        ui.label(&self.status.detail);
                    });
            }
            State::Unsupported => {
                derisk_frame::card(ui, tokens, |ui| {
                    derisk_frame::empty(
                        ui,
                        tokens,
                        "Android is not available on this computer",
                        &self.status.detail,
                    );
                });
                return;
            }
            _ => {}
        }

        derisk_frame::card(ui, tokens, |ui| {
            derisk_frame::rows(ui, "android-pixel", |ui| {
                ui.label("Device");
                ui.label(DEVICE);
                ui.end_row();
                ui.label("System");
                ui.label(SYSTEM);
                ui.end_row();
                ui.label("State");
                ui.label(state.describe());
                ui.end_row();
            });
        });

        let installed: Vec<AndroidApp> = self.apps.iter().filter(|a| !a.system).cloned().collect();
        derisk_frame::subheading(ui, tokens, "Your apps");
        derisk_frame::card(ui, tokens, |ui| {
            if installed.is_empty() {
                ui.label(derisk_frame::note(
                    tokens,
                    if state.is_set_up() {
                        "Apps you install from Google Play, or by opening an APK in Files, \
                         show up here and in the app grid."
                    } else {
                        "Once Android is set up, the apps you install show up here and in \
                         the app grid."
                    },
                ));
            } else {
                self.tiles(ui, tokens, &installed);
            }
        });
        ui.label(derisk_frame::note(
            tokens,
            "Each app opens in a window of its own. Android runs in a virtual Pixel of \
             yours: its apps see its files, not this computer's.",
        ));
    }

    /// Every app on the Pixel, the person's first, with a search field.
    fn apps_page(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        derisk_frame::header(
            ui,
            tokens,
            "Apps",
            "Everything on the Pixel. Each opens in a window of its own.",
        );
        ui.add(derisk_frame::field(&mut self.search).hint_text("Search apps"));
        ui.add_space(8.0);
        let query = self.search.trim().to_lowercase();
        let shown: Vec<AndroidApp> = self
            .apps
            .iter()
            .filter(|a| {
                query.is_empty()
                    || a.name.to_lowercase().contains(&query)
                    || a.package.contains(&query)
            })
            .cloned()
            .collect();
        derisk_frame::card(ui, tokens, |ui| {
            if self.apps.is_empty() {
                derisk_frame::empty(
                    ui,
                    tokens,
                    "No apps yet",
                    "The Pixel lists its apps once it has started.",
                );
            } else if shown.is_empty() {
                ui.label(derisk_frame::note(tokens, "No app matches."));
            } else {
                self.tiles(ui, tokens, &shown);
            }
        });
    }

    fn tiles(&mut self, ui: &mut egui::Ui, tokens: &Tokens, apps: &[AndroidApp]) {
        let idle = self.busy.is_none();
        let mut open = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            for app in apps {
                let texture = self.icon(ui.ctx(), app);
                let response = app_tile(ui, tokens, &app.name, texture.as_ref());
                if idle && response.clicked() {
                    open = Some(app.clone());
                }
            }
        });
        if let Some(app) = open {
            self.open(&app);
        }
    }

    /// `app`'s icon as the Pixel drew it, loaded once.
    fn icon(&mut self, ctx: &egui::Context, app: &AndroidApp) -> Option<egui::TextureHandle> {
        self.icons
            .entry(app.package.clone())
            .or_insert_with(|| {
                derisk_icons::load(&app.icon, 48).map(|image| {
                    ctx.load_texture(
                        format!("derisk-android-app:{}", app.package),
                        image,
                        egui::TextureOptions::LINEAR,
                    )
                })
            })
            .clone()
    }

    fn act(&mut self, act: Act) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let (doing, order) = match act {
                Act::SetUp => ("Setting Android up", Order::SetUp),
                Act::Start => ("Starting the Pixel", Order::Start),
                Act::Stop => ("Stopping the Pixel", Order::Stop),
                Act::Phone => ("Opening the phone's screen", Order::Phone),
            };
            self.order(doing.into(), order);
        }
        #[cfg(target_arch = "wasm32")]
        let _ = act;
    }
}

#[derive(Clone, Copy, Debug)]
enum Act {
    SetUp,
    Start,
    Stop,
    Phone,
}

/// One of the Pixel's apps: its icon over its name, a tile that opens it.
/// An app with no icon yet shows its initial.
fn app_tile(
    ui: &mut egui::Ui,
    tokens: &Tokens,
    name: &str,
    icon: Option<&egui::TextureHandle>,
) -> egui::Response {
    derisk_frame::tile(ui, tokens, egui::vec2(96.0, 92.0), name, |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(4.0);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(40.0, 40.0), egui::Sense::hover());
            match icon {
                Some(texture) => {
                    egui::Image::new(texture)
                        .fit_to_exact_size(rect.size())
                        .paint_at(ui, rect);
                }
                None => {
                    ui.painter().rect_filled(rect, 20.0, tokens.field);
                    let initial: String =
                        name.chars().take(1).flat_map(char::to_uppercase).collect();
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        initial,
                        egui::FontId::proportional(20.0),
                        tokens.muted_foreground,
                    );
                }
            }
            ui.add(egui::Label::new(name).truncate().selectable(false));
        });
    })
}

impl App for AndroidSupport {
    fn title(&self) -> &str {
        "Android"
    }

    fn ui(&mut self, ui: &mut egui::Ui, theme: &Theme) {
        self.receive();
        // The setup's progress moves every second; otherwise the state
        // changes at most as often as the thread reads it.
        let every = if self.status.state == State::SettingUp || self.busy.is_some() {
            250
        } else {
            1000
        };
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(every));
        let tokens = derisk_frame::begin(ui, theme);
        self.sidebar(ui, &tokens);
        self.action_bar(ui, &tokens);
        derisk_frame::Page::new().show(ui, &tokens, |ui| self.page(ui, &tokens));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn app(name: &str, system: bool) -> AndroidApp {
        AndroidApp {
            package: format!("org.example.{}", name.to_lowercase()),
            name: name.into(),
            system,
            icon: PathBuf::from("/nonexistent.png"),
        }
    }

    #[test]
    fn draws_every_state_and_page() {
        let context = egui::Context::default();
        let mut android = AndroidSupport::new();
        android.apps = vec![app("Notes", false), app("Settings", true)];
        android.asked_setup = true;
        for state in [
            State::Unknown,
            State::Unsupported,
            State::New,
            State::SettingUp,
            State::Failed,
            State::Off,
            State::Starting,
            State::Running,
        ] {
            for page in [Page::Home, Page::Apps] {
                android.unavailable = None;
                android.status = Status {
                    state,
                    detail: "Downloading the Android Emulator: 12M of 333M".into(),
                };
                android.page = page;
                let mut output = mcsapi_ui::run_frame(
                    &mut android,
                    &context,
                    egui::RawInput::default(),
                    &Theme::default(),
                );
                assert!(!output.shapes.is_empty());
                output.textures_delta.clear();
            }
        }
        assert_eq!(android.title(), "Android");
    }
}
