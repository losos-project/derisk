//! derisk Settings: appearance, desktop, input, notification, and power
//! preferences, stored as `key = value` lines in
//! `$XDG_CONFIG_HOME/derisk/settings.conf`.
//!
//! [`Settings`] is the model the shell and other apps read; [`SettingsApp`]
//! is the [`App`] that edits it.
//!
//! ```
//! use derisk_settings::{Accent, Settings};
//!
//! let (settings, warnings) = Settings::parse("appearance.accent = sky\nbogus = 1\n");
//! assert_eq!(settings.appearance.accent, Accent::Sky);
//! assert_eq!(warnings.len(), 1);
//! assert_eq!(Settings::parse(&settings.to_text()).0, settings);
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod backup;
pub mod bluetooth;
pub mod choice;
pub mod familiarity;
pub mod flatpak;
mod layout;
mod model;
#[cfg(not(target_family = "wasm"))]
pub mod network;
// A browser page has no wpa_supplicant to talk to, and std there has no Unix
// sockets to talk to it with.
#[cfg(target_family = "wasm")]
#[path = "network_demo.rs"]
mod network;
mod pages;
mod privacy;
pub mod profiles;
mod shortcuts;
mod signin;
pub mod system;
mod thumbs;
mod totp;
pub mod users;

use std::{path::PathBuf, time::Duration};

use mcsapi_components::{Button, ButtonVariant, ErrorDialog, Tokens};
use mcsapi_ui::{
    App, Context as _, DocLink, Error, Theme, egui,
    error::miette::{MietteDiagnostic, Severity},
};
pub use model::{
    Accent, Appearance, AssistantPrefs, BarPosition, ColorScheme, Defaults, DesktopPrefs, Fit,
    Input, Layout, LowPower, MachinePrefs, Notifications, PanelOpacity, Power, Privacy,
    PrivacyDisplay, Profile, Rgb, Settings, ThemeId, TopBar, Vrr, Wallpaper, WallpaperKind,
    Warning, WindowStyle, default_path, machine_name, model_name, model_server_url,
    web_background_url,
};
pub use pages::{IMAGE_EXTENSIONS, candidates};
pub use shortcuts::{Chord, Group, KeyName, Keys, Resolved, Shortcut, Shortcuts};
pub use thumbs::thumbnail;

/// A page of the Settings app.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Page {
    /// Colors, text size, motion, blur, panel opacity, and window frames.
    #[default]
    Appearance,
    /// The desktop background: color, gradient, picture, slideshow, video.
    Wallpaper,
    /// The top bar's position and contents.
    TopBar,
    /// Layout, gaps, workspaces, profile, and variable refresh rate.
    Desktop,
    /// The palette's assistant and the language model it can hand
    /// requests to.
    Assistant,
    /// Keyboard and pointer.
    Input,
    /// Rebindable keyboard shortcuts.
    Shortcuts,
    /// Wired ports and Wi-Fi networks.
    Network,
    /// The adapter and paired devices.
    Bluetooth,
    /// Device access, history, trash, and Flatpak app permissions.
    Privacy,
    /// The password, fingerprints, verification codes and security keys.
    SignIn,
    /// Everyone with an account, and who administers the machine.
    Users,
    /// Banners and sounds.
    Notifications,
    /// Dimming, locking, suspend, low power mode, and the power mode.
    Power,
    /// Backups of the home to a LosOS box ([`backup`]).
    Backups,
    /// The browser and search engine choice screens; listed only while the
    /// policy turns one on ([`choice`]).
    DefaultApps,
    /// The security report and the factory reset.
    System,
    /// Version and file location.
    About,
}

impl Page {
    /// Every page, in sidebar order.
    pub const ALL: [Self; 18] = [
        Self::Appearance,
        Self::Wallpaper,
        Self::TopBar,
        Self::Desktop,
        Self::Assistant,
        Self::Input,
        Self::Shortcuts,
        Self::Network,
        Self::Bluetooth,
        Self::Privacy,
        Self::SignIn,
        Self::Users,
        Self::Notifications,
        Self::Power,
        Self::Backups,
        Self::DefaultApps,
        Self::System,
        Self::About,
    ];

    /// The page's sidebar label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Wallpaper => "Wallpaper",
            Self::TopBar => "Top bar",
            Self::Shortcuts => "Shortcuts",
            Self::Network => "Network",
            Self::Bluetooth => "Bluetooth",
            Self::Privacy => "Privacy",
            Self::SignIn => "Sign-in",
            Self::Users => "Users",
            Self::Desktop => "Desktop",
            Self::Assistant => "Assistant",
            Self::Input => "Keyboard & pointer",
            Self::Notifications => "Notifications",
            Self::Power => "Power",
            Self::Backups => "Backups",
            Self::DefaultApps => "Default apps",
            Self::System => "System",
            Self::About => "About",
        }
    }

    /// The line under the page's title, saying what it holds.
    pub const fn description(self) -> &'static str {
        match self {
            Self::Appearance => "Theme, accent colour, text size, blur and motion.",
            Self::Wallpaper => "What the desktop and the lock screen show behind windows.",
            Self::TopBar => "Where the top bar sits and what it shows.",
            Self::Desktop => "How windows are laid out, and on which workspaces.",
            Self::Assistant => {
                "The language model Ask derisk hands requests to, and where it runs."
            }
            Self::Input => "Scrolling, the touchpad and key repeat.",
            Self::Shortcuts => "Keys for the desktop's actions, single or in sequence.",
            Self::Network => "Wired ports and Wi-Fi networks.",
            Self::Bluetooth => "The adapter and the devices paired with it.",
            Self::Privacy => "What apps may reach, what is remembered, and the privacy display.",
            Self::SignIn => "Your password and the other ways you can sign in.",
            Self::Users => "Everyone who can log in to this computer, and who administers it.",
            Self::Notifications => "Banners, sounds and what the lock screen shows.",
            Self::Power => "When the screen dims and locks, and when the computer sleeps.",
            Self::Backups => "Copies of your files and repositories on a LosOS box.",
            Self::DefaultApps => "The browser and search engine the desktop opens.",
            Self::System => "The security report and resetting this computer.",
            Self::About => "This version of Settings and where it keeps them.",
        }
    }
}

/// The Settings app.
#[derive(Debug)]
pub struct SettingsApp {
    path: Option<PathBuf>,
    saved: Settings,
    /// The settings being edited; saved with [`SettingsApp::save`].
    pub settings: Settings,
    /// The visible page.
    pub page: Page,
    status: Option<String>,
    error: Option<Error>,
    drafts: pages::Drafts,
    thumbs: thumbs::Thumbnails,
    privacy: privacy::PrivacyUi,
    signin: signin::SigninUi,
    users: users::UsersUi,
    network: network::NetworkUi,
    bluetooth: bluetooth::BluetoothUi,
    profiles: profiles::ProfilesUi,
    system: system::SystemUi,
    backup: backup::BackupUi,
    /// Theme IDs offered on the Appearance page, read once when opened.
    themes: Vec<String>,
    /// Which choice screens are on, read once when opened.
    policy: choice::Policy,
    /// The choice screens' lists, built when first shown.
    choice: Option<choice::ChoiceUi>,
    /// Time spent at the desktop, read when the Appearance page first shows.
    familiar: Option<Duration>,
}

/// The documentation section on `heading` of the troubleshooting page.
fn doc(heading: &'static str) -> DocLink {
    DocLink::new("troubleshooting").section(heading)
}

/// Settings edits in memory when there is nowhere to save them.
fn no_config_dir() -> Error {
    let diagnostic = MietteDiagnostic::new("Not saved: no config directory")
        .with_help("Settings saves to $XDG_CONFIG_HOME/derisk, or ~/.config/derisk without it.");
    Error::new(diagnostic).with_doc(doc("settings-could-not-be-read-or-saved"))
}

impl Default for SettingsApp {
    /// Opens the user's settings file from [`default_path`].
    fn default() -> Self {
        Self::open(default_path())
    }
}

impl SettingsApp {
    /// Reads Flatpak apps from `dirs` instead of the standard places.
    pub fn with_flatpak_dirs(mut self, dirs: flatpak::Dirs) -> Self {
        self.privacy = privacy::PrivacyUi::with_dirs(dirs);
        self
    }

    /// Looks for Flatpak apps with `find` before this computer's standard
    /// places: derisk-machines finds the machine Flatpak runs in.
    pub fn with_flatpak_finder(mut self, find: flatpak::FindInstallation) -> Self {
        self.privacy = privacy::PrivacyUi::with_finder(find);
        self
    }

    /// Opens the Privacy page on one Flatpak app's permissions.
    pub fn show_flatpak_app(&mut self, id: &str) {
        self.page = Page::Privacy;
        self.privacy.select(id);
    }

    /// Opens the settings file at `path`, or edits in memory when `None`.
    ///
    /// A missing file starts from defaults; an unreadable or partly invalid
    /// one is reported in the status line.
    pub fn open(path: Option<PathBuf>) -> Self {
        let (saved, error) = match path.as_deref().map(Settings::load) {
            None => (Settings::default(), Some(no_config_dir())),
            Some(Ok((settings, warnings))) if warnings.is_empty() => (settings, None),
            Some(Ok((settings, warnings))) => {
                let skipped = MietteDiagnostic::new(format!(
                    "Skipped {} invalid line(s): {}",
                    warnings.len(),
                    warnings[0]
                ))
                .with_severity(Severity::Warning)
                .with_help("Those settings keep their defaults until the lines are fixed.");
                let error = Error::new(skipped).with_doc(doc("settings-skipped-some-lines"));
                (settings, Some(error))
            }
            Some(Err(error)) => (
                Settings::default(),
                Some(
                    Error::plain(error)
                        .context("Could not read the settings file")
                        .with_doc(doc("settings-could-not-be-read-or-saved")),
                ),
            ),
        };
        Self {
            path,
            settings: saved.clone(),
            drafts: pages::Drafts::new(&saved),
            thumbs: thumbs::Thumbnails::default(),
            privacy: privacy::PrivacyUi::default(),
            signin: signin::SigninUi::default(),
            users: users::UsersUi::default(),
            network: network::NetworkUi::default(),
            bluetooth: bluetooth::BluetoothUi::default(),
            profiles: profiles::ProfilesUi::default(),
            system: system::SystemUi::default(),
            backup: backup::BackupUi::default(),
            saved,
            page: Page::default(),
            status: error.as_ref().map(ToString::to_string),
            error,
            themes: mcsapi_theme::Library::xdg("derisk").ids(),
            policy: choice::Policy::load(),
            choice: None,
            familiar: None,
        }
    }

    /// Shows the choice screens as `policy` says, instead of the policy file.
    pub fn with_policy(mut self, policy: choice::Policy) -> Self {
        self.policy = policy;
        self
    }

    /// Whether `page` is in the sidebar: every page but Default apps, which
    /// shows only once the policy turns a choice screen on.
    pub fn listed(&self, page: Page) -> bool {
        page != Page::DefaultApps || self.policy.any()
    }

    /// The file being edited.
    pub fn path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }

    /// Whether there are unsaved changes.
    pub fn is_dirty(&self) -> bool {
        self.settings != self.saved
    }

    /// The latest status or error message.
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    /// The error or warning the app is showing, with its causes and
    /// documentation.
    pub fn error(&self) -> Option<&Error> {
        self.error.as_ref()
    }

    /// Shows `error` in a dialog until the next save or revert, or until
    /// "OK"; the status line carries its message too.
    fn fail(&mut self, error: Error) {
        self.status = Some(error.to_string());
        self.error = Some(error);
    }

    /// Writes the edited settings to the file.
    pub fn save(&mut self) {
        self.error = None;
        let Some(path) = &self.path else {
            self.fail(no_config_dir());
            return;
        };
        let saved = self
            .settings
            .save(path)
            .context("Could not save")
            .doc(doc("settings-could-not-be-read-or-saved"));
        match saved {
            Err(error) => self.fail(error),
            Ok(()) => {
                let before = std::mem::replace(&mut self.saved, self.settings.clone());
                let browser = &self.settings.defaults.browser;
                let mimeapps = (before.defaults.browser != *browser && !browser.is_empty())
                    .then(choice::mimeapps_path)
                    .flatten()
                    .and_then(|path| choice::apply_browser(&path, browser).err());
                if let Some(error) = mimeapps {
                    self.fail(
                        Error::plain(error)
                            .context("Saved, but could not set the browser")
                            .with_doc(doc("the-default-browser-did-not-change")),
                    );
                    return;
                }
                self.status = Some(
                    self.privacy
                        .apply_masters(&before.privacy, &self.settings.privacy)
                        .unwrap_or_else(|| "Saved".into()),
                );
            }
        }
    }

    /// Discards unsaved changes.
    pub fn revert(&mut self) {
        self.settings = self.saved.clone();
        self.drafts = pages::Drafts::new(&self.saved);
        self.status = None;
        self.error = None;
    }

    fn page_ui(&mut self, ui: &mut egui::Ui, theme: &Theme, tokens: &Tokens) {
        layout::header(ui, tokens, self.page);
        layout::card(ui, tokens, |ui| self.page_body(ui, theme, tokens));
    }

    fn page_body(&mut self, ui: &mut egui::Ui, theme: &Theme, tokens: &Tokens) {
        let s = &mut self.settings;
        // Sliders and fields grow with the card instead of sitting at
        // egui's 100 and 280 pixels, beside the label column and the gap
        // after it, up to where a row is still easy to read across.
        let (label, gap) = layout::label_width(ui.available_width());
        let control = (ui.available_width() - label - gap).clamp(120.0, 440.0);
        ui.spacing_mut().slider_width = control - 64.0;
        ui.spacing_mut().text_edit_width = control;
        if self.page == Page::Privacy {
            return privacy::page(
                ui,
                &mut s.privacy,
                &mut s.privacy_display,
                &mut self.privacy,
                theme,
            );
        }
        if self.page == Page::SignIn {
            return signin::page(ui, &mut self.signin, theme);
        }
        if self.page == Page::Users {
            return users::page(ui, &mut self.users, theme);
        }
        if self.page == Page::Network {
            return network::page(ui, &mut self.network, theme);
        }
        if self.page == Page::Bluetooth {
            return bluetooth::page(ui, &mut self.bluetooth, theme);
        }
        if self.page == Page::System {
            return system::page(ui, &mut self.system, theme);
        }
        if self.page == Page::Backups {
            return backup::page(ui, &mut self.backup, theme);
        }
        if self.page == Page::DefaultApps {
            let state = self.choice.get_or_insert_with(choice::ChoiceUi::default);
            if self.policy.browser {
                ui.heading(egui::RichText::new("Browser").color(theme.foreground));
                choice::browser_page(ui, &mut s.defaults, state, theme);
                ui.add_space(16.0);
            }
            if self.policy.search {
                ui.heading(egui::RichText::new("Search engine").color(theme.foreground));
                choice::search_page(ui, &mut s.defaults, state, theme);
            }
            return;
        }
        egui::Grid::new("settings-page")
            .num_columns(2)
            .min_col_width(label)
            .spacing([gap, 18.0])
            .show(ui, |ui| match self.page {
                Page::Appearance => {
                    let a = &mut s.appearance;
                    ui.label("Theme");
                    egui::ComboBox::from_id_salt("settings-theme")
                        .selected_text(if a.theme.is_automatic() {
                            "Automatic"
                        } else {
                            a.theme.as_str()
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut a.theme, ThemeId::AUTOMATIC, "Automatic");
                            for id in &self.themes {
                                if let Some(theme) = ThemeId::parse(id) {
                                    ui.selectable_value(&mut a.theme, theme, id.as_str());
                                }
                            }
                        });
                    ui.end_row();
                    // A named theme brings its own colors.
                    let automatic = a.theme.is_automatic();
                    ui.label("Style");
                    ui.add_enabled_ui(automatic, |ui| {
                        ui.horizontal(|ui| {
                            for scheme in [ColorScheme::Dark, ColorScheme::Light] {
                                let text = if scheme == ColorScheme::Dark {
                                    "Dark"
                                } else {
                                    "Light"
                                };
                                ui.selectable_value(&mut a.scheme, scheme, text);
                            }
                        })
                    });
                    ui.end_row();
                    ui.label("Accent");
                    ui.add_enabled_ui(automatic, |ui| {
                        ui.horizontal(|ui| {
                            for accent in Accent::ALL {
                                swatch(ui, &mut a.accent, accent, theme);
                            }
                        })
                    });
                    ui.end_row();
                    ui.label("Text size");
                    ui.add(egui::Slider::new(&mut a.text_scale, 0.75..=2.0).step_by(0.05));
                    ui.end_row();
                    ui.label("Reduce motion");
                    ui.checkbox(&mut a.reduce_motion, "Cross-fade instead of animating");
                    ui.end_row();
                    // With motion reduced there is nothing to quicken.
                    ui.label("Familiar motion");
                    ui.add_enabled(
                        !a.reduce_motion,
                        egui::Checkbox::new(
                            &mut a.settle_motion,
                            "Speed up as you get used to them",
                        ),
                    );
                    ui.end_row();
                    if a.settle_motion && !a.reduce_motion {
                        let active = *self.familiar.get_or_insert_with(|| {
                            familiarity::default_path()
                                .map(|path| familiarity::read(&path))
                                .unwrap_or_default()
                        });
                        ui.label("");
                        ui.vertical(|ui| {
                            let speed = familiarity::speed(active);
                            let text = if speed < 1.05 {
                                "Full length while the desktop is new.".to_owned()
                            } else {
                                format!("Now {speed:.1}× as fast as at first.")
                            };
                            ui.label(layout::note(tokens, &text));
                            let again = Button::new("Start over")
                                .variant(ButtonVariant::Outline)
                                .enabled(active > Duration::ZERO);
                            if ui.add(again).clicked() {
                                if let Some(path) = familiarity::default_path()
                                    && let Err(error) = familiarity::reset(&path)
                                {
                                    self.status = Some(format!("Could not start over: {error}"));
                                }
                                self.familiar = Some(Duration::ZERO);
                            }
                        });
                        ui.end_row();
                    }
                    ui.label("Background blur");
                    ui.add(
                        egui::Slider::new(&mut a.blur, 0..=10).custom_formatter(|v, _| {
                            if v == 0.0 {
                                "Off".into()
                            } else {
                                format!("{v}")
                            }
                        }),
                    );
                    ui.end_row();
                    for (label, value) in [
                        ("Top bar opacity", &mut a.panels.top_bar),
                        ("Overview opacity", &mut a.panels.overview),
                        ("Snap Assist opacity", &mut a.panels.snap_assist),
                    ] {
                        ui.label(label);
                        ui.add(egui::Slider::new(value, 20..=100).suffix(" %"));
                        ui.end_row();
                    }
                    let w = &mut s.windows;
                    // No "Window corners" slider: windows have had no frame
                    // of the shell's own to round since their headers went,
                    // and apps round their own corners. The setting is still
                    // read, so older files load as they were.
                    ui.label("Window shadows");
                    ui.checkbox(&mut w.shadows, "Soft shadow under windows");
                    ui.end_row();
                }
                Page::Wallpaper => pages::wallpaper(
                    ui,
                    &mut s.wallpaper,
                    &mut self.drafts,
                    &mut self.thumbs,
                    theme,
                ),
                Page::TopBar => pages::top_bar(ui, &mut s.top_bar),
                Page::Assistant => pages::assistant(ui, &mut s.assistant, &mut self.drafts),
                Page::Privacy
                | Page::SignIn
                | Page::Users
                | Page::Network
                | Page::Bluetooth
                | Page::DefaultApps
                | Page::Backups
                | Page::System => {}
                Page::Shortcuts => pages::shortcuts(ui, &mut s.shortcuts, &mut self.drafts, theme),
                Page::Desktop => {
                    let d = &mut s.desktop;
                    ui.label("Default layout");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut d.layout, Layout::Tall, "Tall");
                        ui.selectable_value(&mut d.layout, Layout::Monocle, "Monocle");
                    });
                    ui.end_row();
                    ui.label("Window gaps");
                    ui.add(egui::Slider::new(&mut d.gaps, 0..=64).suffix(" px"));
                    ui.end_row();
                    ui.label("Workspaces");
                    ui.add(egui::Slider::new(&mut d.workspaces, 1..=9));
                    ui.end_row();
                    ui.label("Profile");
                    egui::ComboBox::from_id_salt("profile")
                        .selected_text(profile_label(d.profile))
                        .show_ui(ui, |ui| {
                            for profile in [
                                Profile::Automatic,
                                Profile::Phone,
                                Profile::Tablet,
                                Profile::Desktop,
                            ] {
                                ui.selectable_value(
                                    &mut d.profile,
                                    profile,
                                    profile_label(profile),
                                );
                            }
                        });
                    ui.end_row();
                    ui.label("Variable refresh rate");
                    ui.horizontal(|ui| {
                        for vrr in Vrr::ALL {
                            ui.selectable_value(&mut d.vrr, vrr, vrr.label());
                        }
                    });
                    ui.end_row();
                }
                Page::Input => {
                    let i = &mut s.input;
                    ui.label("Scrolling");
                    ui.checkbox(&mut i.natural_scroll, "Natural scrolling");
                    ui.end_row();
                    ui.label("Touchpad");
                    ui.checkbox(&mut i.tap_to_click, "Tap to click");
                    ui.end_row();
                    ui.label("Repeat delay");
                    ui.add(egui::Slider::new(&mut i.repeat_delay_ms, 100..=1000).suffix(" ms"));
                    ui.end_row();
                    ui.label("Repeat rate");
                    ui.add(egui::Slider::new(&mut i.repeat_rate, 1..=60).suffix(" /s"));
                    ui.end_row();
                }
                Page::Notifications => {
                    let n = &mut s.notifications;
                    ui.label("Do not disturb");
                    ui.checkbox(&mut n.do_not_disturb, "Hide banners");
                    ui.end_row();
                    ui.label("Sounds");
                    ui.checkbox(&mut n.sounds, "Play a sound");
                    ui.end_row();
                    ui.label("Lock screen");
                    ui.checkbox(&mut n.lock_screen_previews, "Show message previews");
                    ui.end_row();
                }
                Page::Power => {
                    let p = &mut s.power;
                    for (label, value) in [
                        ("Dim screen after", &mut p.dim_after_min),
                        ("Lock after", &mut p.lock_after_min),
                        ("Suspend after", &mut p.suspend_after_min),
                    ] {
                        ui.label(label);
                        ui.add(
                            egui::Slider::new(value, 0..=240)
                                .suffix(" min")
                                .custom_formatter(|v, _| {
                                    if v == 0.0 {
                                        "Never".into()
                                    } else {
                                        format!("{v}")
                                    }
                                }),
                        );
                        ui.end_row();
                    }
                    ui.label("Low power mode");
                    ui.horizontal(|ui| {
                        for mode in LowPower::ALL {
                            ui.selectable_value(&mut p.low_power, mode, mode.label());
                        }
                    });
                    ui.end_row();
                    ui.label("");
                    ui.label(layout::note(
                        tokens,
                        "Turns off blur, shadows and animations. Frames stay at 60 per second or more.",
                    ));
                    ui.end_row();
                    profiles::row(ui, &mut self.profiles);
                }
                Page::About => {
                    ui.label("derisk Settings");
                    ui.label(env!("CARGO_PKG_VERSION"));
                    ui.end_row();
                    ui.label("Settings file");
                    ui.label(match &self.path {
                        Some(path) => path.display().to_string(),
                        None => "Not available".into(),
                    });
                    ui.end_row();
                    ui.label("License");
                    ui.label(env!("CARGO_PKG_LICENSE"));
                    ui.end_row();
                }
            });
    }
}

fn profile_label(profile: Profile) -> &'static str {
    match profile {
        Profile::Automatic => "Automatic",
        Profile::Phone => "Phone",
        Profile::Tablet => "Tablet",
        Profile::Desktop => "Desktop",
    }
}

fn swatch(ui: &mut egui::Ui, current: &mut Accent, accent: Accent, theme: &Theme) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::click());
    let selected = *current == accent;
    let painter = ui.painter();
    painter.circle_filled(rect.center(), 10.0, accent.color());
    if selected || response.hovered() {
        painter.circle_stroke(
            rect.center(),
            12.0,
            egui::Stroke::new(2.0, theme.foreground),
        );
    }
    if response.on_hover_text(accent.as_str()).clicked() {
        *current = accent;
    }
}

impl App for SettingsApp {
    fn title(&self) -> &str {
        "Settings"
    }

    fn ui(&mut self, ui: &mut egui::Ui, theme: &Theme) {
        let narrow = ui.available_width() < NARROW;
        // Everything below draws with the theme's component tokens, so a
        // theme restyles the app along with the desktop.
        let tokens = Tokens::from_theme(theme);
        tokens.install(ui.ctx());
        layout::style(ui, &tokens);
        if narrow {
            // A sidebar would leave a phone half a page, so the
            // pages become a row of tabs across the top that scrolls sideways.
            egui::Panel::top("settings-pages").show(ui, |ui| {
                egui::ScrollArea::horizontal().show(ui, |ui| {
                    ui.horizontal(|ui| self.page_buttons(ui));
                });
            });
        } else {
            let sidebar = (ui.available_width() * 0.2).clamp(200.0, 248.0);
            egui::Panel::left("settings-pages")
                .resizable(false)
                .exact_size(sidebar)
                .frame(
                    egui::Frame::new()
                        .fill(tokens.card)
                        .stroke(tokens.border_stroke())
                        .inner_margin(egui::Margin::symmetric(12, 8)),
                )
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        let listed = |page| self.listed(page);
                        if let Some(page) = layout::sidebar(ui, &tokens, self.page, listed) {
                            self.page = page;
                        }
                    });
                });
        }
        egui::Panel::bottom("settings-actions")
            .frame(
                egui::Frame::new()
                    .fill(tokens.background)
                    .stroke(tokens.border_stroke())
                    .inner_margin(egui::Margin::symmetric(20, 12)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let dirty = self.is_dirty();
                    if dirty {
                        ui.label(egui::RichText::new("Unsaved changes").color(theme.accent));
                    } else if let Some(status) =
                        self.status.as_ref().filter(|_| self.error.is_none())
                    {
                        // An error has its own dialog, so the line does not
                        // repeat it.
                        ui.label(egui::RichText::new(status).color(tokens.muted_foreground));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(Button::new("Save").enabled(dirty)).clicked() {
                            self.save();
                        }
                        let revert = Button::new("Revert")
                            .variant(ButtonVariant::Outline)
                            .enabled(dirty);
                        if ui.add(revert).clicked() {
                            self.revert();
                        }
                    });
                });
            });
        if self.error.is_some() {
            let shown = ErrorDialog::new("settings-error", &mut self.error).show(ui.ctx());
            if shown.closed {
                self.status = None;
            }
        }
        egui::CentralPanel::no_frame().show(ui, |ui| {
            ui.painter()
                .rect_filled(ui.max_rect(), 0, tokens.background);
            // A row of controls wider than a phone scrolls rather than being
            // cut off where nothing can reach it.
            let scroll = if narrow {
                egui::ScrollArea::both()
            } else {
                egui::ScrollArea::vertical()
            };
            scroll.show(ui, |ui| {
                layout::column(ui, |ui| self.page_ui(ui, theme, &tokens));
            });
        });
    }
}

/// Below this width the page list moves from a sidebar to tabs on top.
const NARROW: f32 = 560.0;

impl SettingsApp {
    fn page_buttons(&mut self, ui: &mut egui::Ui) {
        let pages: Vec<Page> = Page::ALL.into_iter().filter(|&p| self.listed(p)).collect();
        for page in pages {
            if ui
                .selectable_label(self.page == page, page.label())
                .clicked()
            {
                self.page = page;
            }
        }
    }
}
