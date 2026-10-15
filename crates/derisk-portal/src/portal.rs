//! The portal interfaces derisk serves, on ashpd's backend traits.

use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, RwLock},
    time::Duration,
};

use ashpd::{
    MaybeAppID, PortalError, Uri, WindowIdentifierType,
    async_trait::async_trait,
    backend::{
        Result,
        background::{
            Activity, AppState, AutoStartFlags, Background, BackgroundImpl, BackgroundSignalEmitter,
        },
        request::RequestImpl,
        screenshot::ScreenshotImpl,
        settings::{SettingsImpl, SettingsSignalEmitter},
        wallpaper::WallpaperImpl,
    },
    desktop::{
        Color, HandleToken,
        screenshot::{AvailableTargets, ColorOptions, Screenshot, ScreenshotOptions},
        settings::Namespace,
        wallpaper::{SetOn, WallpaperOptions},
    },
    enumflags2::BitFlags,
    zbus::{self, zvariant::OwnedValue},
};
use serde_json::json;

use crate::{
    access::{self, Question},
    agent,
    appearance::{self, Appearance},
    files, pick,
};

/// The object path every portal interface lives at.
pub const PATH: &str = "/org/freedesktop/portal/desktop";

/// The current appearance, `None` until derisk has published a theme.
pub type Shared = Arc<RwLock<Option<Appearance>>>;

fn failed(error: impl std::fmt::Display) -> PortalError {
    PortalError::Failed(error.to_string())
}

fn app_name(app_id: Option<&MaybeAppID>) -> String {
    app_id
        .map(ToString::to_string)
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| "An app".to_owned())
}

/// `org.freedesktop.impl.portal.Settings`: the appearance keys.
pub struct Settings {
    /// What [`watch_theme`] last read.
    pub appearance: Shared,
}

#[async_trait]
impl SettingsImpl for Settings {
    async fn read_all(
        &self,
        namespaces: Vec<String>,
    ) -> std::result::Result<HashMap<String, Namespace>, PortalError> {
        let mut all = HashMap::new();
        if appearance::requested(&namespaces, appearance::NAMESPACE)
            && let Some(current) = *self.appearance.read().map_err(failed)?
        {
            all.insert(appearance::NAMESPACE.to_owned(), current.namespace());
        }
        Ok(all)
    }

    async fn read(
        &self,
        namespace: &str,
        key: &str,
    ) -> std::result::Result<OwnedValue, PortalError> {
        let current = *self.appearance.read().map_err(failed)?;
        (namespace == appearance::NAMESPACE)
            .then_some(current)
            .flatten()
            .and_then(|a| a.get(key))
            .ok_or_else(|| PortalError::NotFound(format!("{namespace}.{key}")))
    }

    // ashpd 0.13 never calls this; `watch_theme` emits SettingChanged on the
    // connection itself.
    fn set_signal_emitter(&mut self, _: Arc<dyn SettingsSignalEmitter>) {}
}

/// Re-reads `theme.json` once a second, the same cadence the session polls
/// its settings at, and emits `SettingChanged` for each key that moved.
pub async fn watch_theme(connection: zbus::Connection, shared: Shared) {
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    loop {
        ticker.tick().await;
        let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") else {
            continue;
        };
        let path = Path::new(&dir).join("derisk").join("theme.json");
        let Some(now) = tokio::fs::read_to_string(&path)
            .await
            .ok()
            .and_then(|json| Appearance::parse(&json))
        else {
            continue;
        };
        let before = shared.read().ok().and_then(|a| *a);
        if before == Some(now) {
            continue;
        }
        if let Ok(mut current) = shared.write() {
            *current = Some(now);
        }
        for key in now.changed_since(before.as_ref()) {
            let Some(value) = now.get(key) else { continue };
            let _ = connection
                .emit_signal(
                    None::<()>,
                    PATH,
                    "org.freedesktop.impl.portal.Settings",
                    "SettingChanged",
                    &(appearance::NAMESPACE, key, &value),
                )
                .await;
        }
    }
}

/// `org.freedesktop.impl.portal.Screenshot`: the whole screen, from the
/// session's compositor.
pub struct Screenshots {
    /// For the access dialog.
    pub connection: zbus::Connection,
}

#[async_trait]
impl RequestImpl for Screenshots {
    async fn close(&self, token: HandleToken) {
        access::close(&self.connection, &token.to_string()).await;
    }
}

#[async_trait]
impl ScreenshotImpl for Screenshots {
    fn available_targets(&self) -> BitFlags<AvailableTargets> {
        AvailableTargets::Screen.into()
    }

    async fn screenshot(
        &self,
        token: HandleToken,
        app_id: Option<MaybeAppID>,
        window_identifier: Option<WindowIdentifierType>,
        options: ScreenshotOptions,
    ) -> Result<Screenshot> {
        let app = app_name(app_id.as_ref());
        let interactive = options.interactive() == Some(true);
        // An interactive screenshot is the person's to frame: derisk's
        // picker shows the screen frozen and shares only what they drag
        // over, or all of it on Enter, which is also their consent.
        if interactive && let Some(program) = access::dialog_program() {
            let capture = capture().await?;
            let mut command = tokio::process::Command::new(&program);
            command.args(pick::args("area", &capture, &app));
            let Some(pick::Picked::Area(x, y, w, h)) = picked(&token, command).await? else {
                return Err(PortalError::Cancelled("no area was picked".into()));
            };
            let area = capture.with_extension("area.png");
            pick::crop(&capture, (x, y, w, h), &area).map_err(failed)?;
            let kept = keep(&area);
            let _ = std::fs::remove_file(&area);
            return kept;
        }
        // The frontend asks before a non-interactive screenshot and says so
        // in `permission_store_checked`. An interactive one is left to the
        // backend, so without the picker, ask here; the answer covers the
        // whole screen.
        let asked = options.permission_store_checked() == Some(true);
        if !asked && interactive {
            let parent = window_identifier.map(|w| w.to_string()).unwrap_or_default();
            let id = app_id.as_ref().map(ToString::to_string).unwrap_or_default();
            let granted = access::ask(
                &self.connection,
                &token.to_string(),
                &Question {
                    app_id: &id,
                    app: &app,
                    parent_window: &parent,
                    title: "Take a screenshot?",
                    subtitle: &format!("{app} wants a picture of the whole screen."),
                    body: "Everything on screen will be in it.",
                    grant_label: "Take Screenshot",
                },
            )
            .await
            .map_err(failed)?;
            if !granted {
                return Err(PortalError::Cancelled("the screenshot was declined".into()));
            }
        }
        keep(&capture().await?)
    }

    async fn pick_color(
        &self,
        token: HandleToken,
        app_id: Option<MaybeAppID>,
        _window_identifier: Option<WindowIdentifierType>,
        _options: ColorOptions,
    ) -> Result<Color> {
        // The person points at the color on a frozen picture of the screen.
        // Nothing else leaves: the app gets three numbers.
        let program = access::dialog_program().ok_or_else(|| {
            PortalError::NotAllowed("derisk-gpui, which picks colors, is not installed".into())
        })?;
        let capture = capture().await?;
        let mut command = tokio::process::Command::new(&program);
        command.args(pick::args("color", &capture, &app_name(app_id.as_ref())));
        let Some(pick::Picked::Point(x, y)) = picked(&token, command).await? else {
            return Err(PortalError::Cancelled("no color was picked".into()));
        };
        let [r, g, b] = pick::color_at(&capture, x, y).map_err(failed)?;
        Ok(Color::new(r, g, b))
    }
}

/// A screenshot of the whole screen from the session, in the runtime
/// directory where the session keeps its last few.
async fn capture() -> Result<std::path::PathBuf> {
    let result = agent::call(json!({"method": "screenshot"}))
        .await
        .map_err(PortalError::Failed)?;
    result["path"]
        .as_str()
        .map(std::path::PathBuf::from)
        .ok_or_else(|| failed("the session returned no screenshot"))
}

/// Copies a screenshot into the pictures folder, where the app is handed it.
fn keep(capture: &Path) -> Result<Screenshot> {
    let (home, config) = files::home_and_config().ok_or_else(|| failed("HOME is not set"))?;
    let kept =
        files::keep_screenshot(capture, &files::pictures_dir(&home, &config)).map_err(failed)?;
    let uri = Uri::parse(&files::uri_from_path(&kept)).map_err(failed)?;
    Ok(Screenshot::new(uri))
}

/// Runs the picker for the request `token`: what it picked, or `None` when
/// the person cancelled or the app withdrew the request.
async fn picked(
    token: &HandleToken,
    command: tokio::process::Command,
) -> Result<Option<pick::Picked>> {
    let ran = access::run_closable(&token.to_string(), command)
        .await
        .map_err(failed)?;
    Ok(ran
        .filter(|(ok, _)| *ok)
        .and_then(|(_, line)| pick::parse(&line)))
}

/// `org.freedesktop.impl.portal.Wallpaper`: a picture as the desktop
/// background, through derisk's own settings file.
pub struct Wallpaper {
    /// For the access dialog.
    pub connection: zbus::Connection,
}

#[async_trait]
impl RequestImpl for Wallpaper {
    async fn close(&self, token: HandleToken) {
        access::close(&self.connection, &token.to_string()).await;
    }
}

#[async_trait]
impl WallpaperImpl for Wallpaper {
    async fn with_uri(
        &self,
        token: HandleToken,
        app_id: Option<MaybeAppID>,
        window_identifier: Option<WindowIdentifierType>,
        uri: Uri,
        options: WallpaperOptions,
    ) -> Result<()> {
        // The lock screen draws derisk's own gradient, never a picture.
        if options.set_on() == Some(SetOn::Lockscreen) {
            return Err(PortalError::NotAllowed(
                "derisk's lock screen does not show a picture".into(),
            ));
        }
        let picture = files::path_from_uri(uri.as_str())
            .ok_or_else(|| PortalError::InvalidArgument(format!("not a local file: {uri}")))?;
        // Without a preview the frontend has already asked; with one, the
        // backend is meant to show the picture first, so ask here.
        if options.show_preview() == Some(true) {
            let app = app_name(app_id.as_ref());
            let parent = window_identifier.map(|w| w.to_string()).unwrap_or_default();
            let id = app_id.as_ref().map(ToString::to_string).unwrap_or_default();
            let name = picture
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let granted = access::ask(
                &self.connection,
                &token.to_string(),
                &Question {
                    app_id: &id,
                    app: &app,
                    parent_window: &parent,
                    title: "Change the background?",
                    subtitle: &format!("{app} wants to set {name} as the desktop background."),
                    body: "You can change it back in Settings, under Wallpaper.",
                    grant_label: "Set Background",
                },
            )
            .await
            .map_err(failed)?;
            if !granted {
                return Err(PortalError::Cancelled("the wallpaper was declined".into()));
            }
        }
        let data = files::data_home().ok_or_else(|| failed("HOME is not set"))?;
        let installed = files::install_wallpaper(&picture, &data.join("derisk").join("wallpapers"))
            .map_err(|e| PortalError::InvalidArgument(e.to_string()))?;
        let path = derisk_settings::default_path().ok_or_else(|| failed("HOME is not set"))?;
        let (mut settings, _warnings) = derisk_settings::Settings::load(&path).map_err(failed)?;
        settings.wallpaper.kind = derisk_settings::WallpaperKind::Image;
        settings.wallpaper.path = installed;
        // The session polls the file and repaints within a second.
        settings.save(&path).map_err(failed)
    }
}

/// `org.freedesktop.impl.portal.Background`: which apps have windows, from
/// the session.
pub struct Apps;

#[async_trait]
impl RequestImpl for Apps {
    async fn close(&self, _token: HandleToken) {}
}

#[async_trait]
impl BackgroundImpl for Apps {
    async fn get_app_state(
        &self,
    ) -> std::result::Result<HashMap<MaybeAppID, AppState>, PortalError> {
        let state = agent::call(json!({"method": "state"}))
            .await
            .map_err(PortalError::Failed)?;
        Ok(agent::apps(&state)
            .into_iter()
            .map(|(app, seen)| {
                let state = match seen {
                    agent::Visibility::Active => AppState::Active,
                    agent::Visibility::Running => AppState::Running,
                };
                (MaybeAppID::from(app), state)
            })
            .collect())
    }

    async fn notify_background(
        &self,
        _token: HandleToken,
        app_id: MaybeAppID,
        name: &str,
    ) -> std::result::Result<Background, PortalError> {
        // The frontend asks only about apps with no stored choice. Allow this
        // instance and store nothing: a choice nobody made should not show
        // up as the person's in Settings → Privacy, where they can forbid it.
        tracing::info!("{name} ({app_id}) is running in the background");
        Ok(Background::new(Activity::AllowInstance))
    }

    async fn enable_autostart(
        &self,
        _app_id: MaybeAppID,
        _enable: bool,
        _commandline: Vec<String>,
        _flags: BitFlags<AutoStartFlags>,
    ) -> std::result::Result<bool, PortalError> {
        // Deprecated: the frontend writes autostart files itself and no
        // longer calls this.
        Ok(false)
    }

    fn set_signal_emitter(&mut self, _: Arc<dyn BackgroundSignalEmitter>) {}
}

/// Asks the session which apps have windows every two seconds and emits
/// `RunningApplicationsChanged` when that changes, so the frontend looks
/// again.
pub async fn watch_apps(connection: zbus::Connection) {
    let mut ticker = tokio::time::interval(Duration::from_secs(2));
    let mut before = None;
    loop {
        ticker.tick().await;
        let Ok(state) = agent::call(json!({"method": "state"})).await else {
            continue;
        };
        let now = agent::apps(&state);
        if before.as_ref() == Some(&now) {
            continue;
        }
        let first = before.is_none();
        before = Some(now);
        if !first {
            let _ = connection
                .emit_signal(
                    None::<()>,
                    PATH,
                    "org.freedesktop.impl.portal.Background",
                    "RunningApplicationsChanged",
                    &(),
                )
                .await;
        }
    }
}
