//! The stand-in shell: a top bar and a window per running app, coloured
//! by whatever Settings last saved, as `derisk-preview` does on Linux.

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    time::SystemTime,
};

use derisk_apps::{APPS, EDITOR, FILES, Session, visuals};
use derisk_editor::EditorApp;
use derisk_files::{Browser, FilesApp};
use derisk_settings::Settings;
use derisk_sys::fs;
use mcsapi_runtime::{AppId, InstanceId};
use mcsapi_ui::{App, Theme, egui};

/// Seconds between looks at the settings file.
const SETTINGS_POLL: f64 = 0.5;
/// Narrower than this, windows open filling the page, as on a phone.
const PHONE_WIDTH: f32 = 600.0;
/// The document the editor opens with.
const WELCOME: &str = "Documents/Welcome.md";

/// The demo desktop: every running app in a window of its own over the
/// page, and a top bar.
pub struct Desktop {
    session: Session,
    /// Running instances, in the order they opened.
    open: Vec<InstanceId>,
    /// Files Files was asked to open, for the editor to pick up; there is
    /// no `xdg-open` in a browser.
    to_open: Rc<RefCell<Vec<PathBuf>>>,
    settings_path: Option<PathBuf>,
    settings_seen: Option<SystemTime>,
    last_check: f64,
    theme: Theme,
    text_scale: f32,
    status: Option<String>,
}

impl Desktop {
    /// The desktop with Files and the welcome document open.
    pub fn new() -> Self {
        let mut desktop = Self {
            session: Session::new().expect("catalog IDs are unique"),
            open: Vec::new(),
            to_open: Rc::default(),
            settings_path: derisk_settings::default_path(),
            settings_seen: None,
            last_check: f64::NEG_INFINITY,
            theme: Theme::default(),
            text_scale: 1.0,
            status: None,
        };
        desktop.launch(FILES);
        if let Some(home) = home() {
            desktop.open_document(&home.join(WELCOME));
        }
        desktop
    }

    fn start(&mut self, id: &str, app: Option<Box<dyn App>>) {
        let result = AppId::new(id)
            .ok_or_else(|| format!("invalid app ID: {id}"))
            .and_then(|id| {
                match app {
                    Some(app) => self.session.launch_app(&id, app),
                    None => self.session.launch(&id),
                }
                .map_err(|e| e.to_string())
            });
        match result {
            Ok(instance) => self.open.push(instance),
            Err(error) => self.status = Some(error),
        }
    }

    /// Starts a core app. Files gets an opener that hands files to the
    /// editor.
    fn launch(&mut self, id: &str) {
        if id != FILES {
            return self.start(id, None);
        }
        let to_open = Rc::clone(&self.to_open);
        let opener = Box::new(move |path: &Path| {
            to_open.borrow_mut().push(path.to_owned());
            Ok(())
        });
        let browser = Browser::new(home().unwrap_or_else(|| PathBuf::from("/")));
        self.start(FILES, Some(Box::new(FilesApp::new(browser, opener))));
    }

    fn open_document(&mut self, path: &Path) {
        self.start(EDITOR, Some(Box::new(EditorApp::open(path))));
    }

    /// Picks up a newly saved theme and text size.
    fn reload_settings(&mut self) {
        let Some(path) = &self.settings_path else {
            return;
        };
        let modified = fs::metadata(path).and_then(|m| m.modified()).ok();
        if modified == self.settings_seen {
            return;
        }
        self.settings_seen = modified;
        if let Ok((settings, _)) = Settings::load(path) {
            self.theme = settings.theme();
            self.text_scale = settings.appearance.text_scale;
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        // On a phone the apps' names do not fit beside the clock.
        let phone = ui.available_width() < PHONE_WIDTH;
        egui::Panel::top("derisk-top-bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong("derisk");
                ui.separator();
                for app in &APPS {
                    let label = if phone {
                        app.icon.to_owned()
                    } else {
                        format!("{} {}", app.icon, app.name)
                    };
                    let button = ui
                        .button(label)
                        .on_hover_text(format!("{}: {}", app.name, app.summary));
                    if button.clicked() {
                        self.launch(app.id);
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(clock()).on_hover_text(
                        "A browser demo: files live in this page and are gone on reload.",
                    );
                    if let Some(status) = &self.status {
                        ui.separator();
                        ui.label(status);
                    }
                });
            });
        });
    }
}

impl Default for Desktop {
    fn default() -> Self {
        Self::new()
    }
}

impl eframe::App for Desktop {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now = ui.input(|i| i.time);
        if now - self.last_check >= SETTINGS_POLL {
            self.last_check = now;
            self.reload_settings();
        }
        // The System Monitor and the clock move on their own.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs_f64(SETTINGS_POLL));
        ui.ctx().set_visuals(visuals(&self.theme));
        ui.ctx().set_zoom_factor(self.text_scale);

        self.top_bar(ui);

        let theme = self.theme;
        let mut closed = Vec::new();
        egui::CentralPanel::default().show(ui, |ui| {
            let area = ui.max_rect();
            let phone = area.width() < PHONE_WIDTH;
            for (index, &instance) in self.open.iter().enumerate() {
                let Some(app) = self.session.app_mut(instance) else {
                    continue;
                };
                let mut open = true;
                let (pos, size) = if phone {
                    (area.min, area.size())
                } else {
                    // Two columns, so the first two windows sit side by
                    // side, then a cascade.
                    let column = 460.0 * (index % 2) as f32;
                    let offset = 28.0 * (index / 2 % 8) as f32;
                    (
                        area.min + egui::vec2(24.0 + column + offset, 24.0 + offset),
                        egui::vec2(760.0, 480.0).min(area.size() - egui::vec2(48.0, 48.0)),
                    )
                };
                egui::Window::new(app.title().to_owned())
                    .id(egui::Id::new(("app", instance.get())))
                    .open(&mut open)
                    .default_pos(pos)
                    .default_size(size)
                    .constrain_to(area)
                    .frame(egui::Frame::window(ui.style()).fill(theme.background))
                    .show(ui.ctx(), |ui| app.ui(ui, &theme));
                if !open {
                    closed.push(instance);
                }
            }
            if self.open.is_empty() {
                ui.centered_and_justified(|ui| ui.label("Open an app from the bar above."));
            }
        });
        for instance in closed {
            let _ = self.session.stop(instance);
            self.open.retain(|&i| i != instance);
        }
        let requested: Vec<PathBuf> = self.to_open.borrow_mut().drain(..).collect();
        for path in requested {
            self.open_document(&path);
        }
    }
}

fn home() -> Option<PathBuf> {
    derisk_sys::env::var_os("HOME").map(PathBuf::from)
}

/// The page's local time, `Tue 14 Oct 10:52`.
fn clock() -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let date = js_sys::Date::new_0();
    format!(
        "{} {} {} {:02}:{:02}",
        DAYS[date.get_day() as usize % 7],
        date.get_date(),
        MONTHS[date.get_month() as usize % 12],
        date.get_hours(),
        date.get_minutes(),
    )
}
