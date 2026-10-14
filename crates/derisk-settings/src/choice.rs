//! The browser and search engine choice screens, and the policy that shows
//! them.
//!
//! These are the screens the EU's Digital Markets Act requires of an
//! operating system with 45 million monthly active users in the EU, for its
//! users in the EEA (Art. 6(3)). Both stay hidden until a policy file says
//! otherwise. The OS writes it: on LosOS a daily ping counts active users,
//! and the answer turns the screens on once the EU count is past the line
//! and the person is in the EEA. derisk only reads
//! `$XDG_STATE_HOME/derisk/policy.conf`:
//!
//! ```text
//! choice_screens.browser = true
//! choice_screens.search = true
//! ```
//!
//! Once on, the screens open at login until the person has chosen, and stay
//! under Settings, Default apps. [`browser_page`] and [`search_page`] are
//! plain egui functions so a first-boot setup can show them as its own steps.
//! Neither screen has a preselected or highlighted option, and both list
//! their options in a new random order each time they are built, which is
//! what a fair choice screen has to do.
//!
//! The choice is kept in [`Defaults`]. The browser also goes into
//! `$XDG_CONFIG_HOME/mimeapps.list` ([`apply_browser`]), which is what every
//! app that opens a link reads; the search engine is what the command
//! palette's web search uses.
//!
//! ```
//! use derisk_settings::choice::{Policy, SearchEngine};
//!
//! let policy = Policy::parse("choice_screens.browser = true\n");
//! assert!(policy.browser && !policy.search);
//! assert_eq!(
//!     SearchEngine::DuckDuckGo.url("rust & wasm"),
//!     "https://duckduckgo.com/?q=rust+%26+wasm"
//! );
//! ```

use std::{
    io,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

use derisk_sys::fs;

use mcsapi_ui::{Theme, egui};

use crate::model::Defaults;

/// Which choice screens are on.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Policy {
    /// The browser choice screen.
    pub browser: bool,
    /// The search engine choice screen.
    pub search: bool,
}

impl Policy {
    /// Reads `key = value` lines. Anything but `true` is off, so a missing,
    /// partial or garbled file hides the screens.
    pub fn parse(text: &str) -> Self {
        let mut policy = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let on = value.trim() == "true";
            match key.trim() {
                "choice_screens.browser" => policy.browser = on,
                "choice_screens.search" => policy.search = on,
                _ => {}
            }
        }
        policy
    }

    /// The policy at [`policy_path`]; all off when there is none.
    pub fn load() -> Self {
        policy_path()
            .and_then(|path| fs::read_to_string(path).ok())
            .map_or_else(Self::default, |text| Self::parse(&text))
    }

    /// Whether either screen is on.
    pub fn any(self) -> bool {
        self.browser || self.search
    }

    /// Whether a screen is on and still waiting for a choice in `defaults`,
    /// which is when the session opens it at login.
    pub fn unanswered(self, defaults: &Defaults) -> bool {
        (self.browser && defaults.browser.is_empty()) || (self.search && defaults.search.is_none())
    }
}

/// `$XDG_STATE_HOME/derisk/policy.conf`, falling back to `~/.local/state`.
pub fn policy_path() -> Option<PathBuf> {
    let state = derisk_sys::env::var_os("XDG_STATE_HOME")
        .filter(|dir| Path::new(dir).is_absolute())
        .map(PathBuf::from)
        .or_else(|| derisk_sys::env::var_os("HOME").map(|h| Path::new(&h).join(".local/state")))?;
    Some(state.join("derisk").join("policy.conf"))
}

/// A web search engine the palette can send a query to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchEngine {
    /// DuckDuckGo.
    DuckDuckGo,
    /// Google.
    Google,
    /// Microsoft Bing.
    Bing,
    /// Startpage.
    Startpage,
    /// Brave Search.
    Brave,
    /// Ecosia.
    Ecosia,
    /// Qwant.
    Qwant,
    /// Mojeek.
    Mojeek,
}

impl SearchEngine {
    /// Every engine. Not an order to show them in: see [`ChoiceUi`].
    pub const ALL: [Self; 8] = [
        Self::DuckDuckGo,
        Self::Google,
        Self::Bing,
        Self::Startpage,
        Self::Brave,
        Self::Ecosia,
        Self::Qwant,
        Self::Mojeek,
    ];

    /// The settings file's name for it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DuckDuckGo => "duckduckgo",
            Self::Google => "google",
            Self::Bing => "bing",
            Self::Startpage => "startpage",
            Self::Brave => "brave",
            Self::Ecosia => "ecosia",
            Self::Qwant => "qwant",
            Self::Mojeek => "mojeek",
        }
    }

    /// The engine named `text` in the settings file.
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.as_str() == text)
    }

    /// Its name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::DuckDuckGo => "DuckDuckGo",
            Self::Google => "Google",
            Self::Bing => "Bing",
            Self::Startpage => "Startpage",
            Self::Brave => "Brave Search",
            Self::Ecosia => "Ecosia",
            Self::Qwant => "Qwant",
            Self::Mojeek => "Mojeek",
        }
    }

    /// One line about it, in the same plain register for every engine.
    pub const fn summary(self) -> &'static str {
        match self {
            Self::DuckDuckGo => "Private search without a profile of you",
            Self::Google => "Search from Google",
            Self::Bing => "Search from Microsoft",
            Self::Startpage => "Google results without tracking",
            Self::Brave => "An independent index from Brave",
            Self::Ecosia => "Uses its profits to plant trees",
            Self::Qwant => "A European search engine",
            Self::Mojeek => "An independent index from the UK",
        }
    }

    /// The results page for `query`. The query is percent-encoded, so the
    /// URL is always one of these sites and nothing a query can redirect.
    pub fn url(self, query: &str) -> String {
        let base = match self {
            Self::DuckDuckGo => "https://duckduckgo.com/?q=",
            Self::Google => "https://www.google.com/search?q=",
            Self::Bing => "https://www.bing.com/search?q=",
            Self::Startpage => "https://www.startpage.com/do/search?q=",
            Self::Brave => "https://search.brave.com/search?q=",
            Self::Ecosia => "https://www.ecosia.org/search?q=",
            Self::Qwant => "https://www.qwant.com/?q=",
            Self::Mojeek => "https://www.mojeek.com/search?q=",
        };
        let mut url = String::from(base);
        for byte in query.trim().bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    url.push(byte as char);
                }
                b' ' => url.push('+'),
                _ => url.push_str(&format!("%{byte:02X}")),
            }
        }
        url
    }
}

/// Browsers on Flathub that the screen offers to install, by Flatpak ID and
/// name. The installed ones come from `.desktop` files instead.
pub const FLATHUB_BROWSERS: &[(&str, &str)] = &[
    ("org.mozilla.firefox", "Firefox"),
    ("org.chromium.Chromium", "Chromium"),
    ("com.brave.Browser", "Brave"),
    ("com.vivaldi.Vivaldi", "Vivaldi"),
    ("io.gitlab.librewolf-community", "LibreWolf"),
    ("org.gnome.Epiphany", "Web"),
    ("com.google.Chrome", "Google Chrome"),
    ("com.microsoft.Edge", "Microsoft Edge"),
    ("net.mullvad.MullvadBrowser", "Mullvad Browser"),
    ("app.zen_browser.zen", "Zen"),
];

/// One browser on the screen.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Browser {
    /// Desktop file ID (`org.mozilla.firefox.desktop`).
    pub id: String,
    /// Its name.
    pub name: String,
    /// Icon name or path, empty if none.
    pub icon: String,
    /// The Flathub ID to install it from, when it is not installed.
    pub flathub: Option<&'static str>,
}

impl Browser {
    /// Whether it can be chosen right away.
    pub fn installed(&self) -> bool {
        self.flathub.is_none()
    }
}

/// The `applications` directories, user first, as the XDG spec orders them.
pub fn application_dirs() -> Vec<PathBuf> {
    let data_home = derisk_sys::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| derisk_sys::env::var_os("HOME").map(|h| Path::new(&h).join(".local/share")));
    let data_dirs = derisk_sys::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    data_home
        .into_iter()
        .chain(
            data_dirs
                .split(':')
                .filter(|d| !d.is_empty())
                .map(PathBuf::from),
        )
        .map(|dir| dir.join("applications"))
        .collect()
}

/// What a `.desktop` file says about being a browser: its name and icon if
/// it is a visible application that opens `https` links.
pub fn parse_browser(text: &str) -> Option<(String, String)> {
    let (mut name, mut icon, mut https, mut hidden, mut app) = (None, "", false, false, false);
    let mut in_entry = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "Name" => name = Some(value.trim()),
            "Icon" => icon = value.trim(),
            "Type" => app = value.trim() == "Application",
            "MimeType" => {
                https = value
                    .split(';')
                    .any(|m| m.trim() == "x-scheme-handler/https")
            }
            "NoDisplay" | "Hidden" => hidden |= value.trim() == "true",
            _ => {}
        }
    }
    (app && https && !hidden)
        .then_some(name)
        .flatten()
        .map(|n| (n.to_owned(), icon.to_owned()))
}

/// The installed browsers in `dirs`, then the [`FLATHUB_BROWSERS`] that are
/// not, without duplicates. Earlier directories win, as for any lookup.
pub fn browsers(dirs: &[PathBuf]) -> Vec<Browser> {
    let mut found: Vec<Browser> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for dir in dirs {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        let mut files: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        files.sort();
        for path in files {
            let Some(id) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !id.ends_with(".desktop") || !seen.insert(id.to_owned()) {
                continue;
            }
            if let Some((name, icon)) = fs::read_to_string(&path)
                .ok()
                .and_then(|text| parse_browser(&text))
            {
                found.push(Browser {
                    id: id.to_owned(),
                    name,
                    icon,
                    flathub: None,
                });
            }
        }
    }
    for (flatpak, name) in FLATHUB_BROWSERS {
        let id = format!("{flatpak}.desktop");
        if !seen.contains(&id) {
            found.push(Browser {
                icon: (*flatpak).to_owned(),
                id,
                name: (*name).to_owned(),
                flathub: Some(flatpak),
            });
        }
    }
    found
}

/// The MIME types a browser takes over as the default.
const BROWSER_TYPES: [&str; 4] = [
    "x-scheme-handler/http",
    "x-scheme-handler/https",
    "text/html",
    "application/xhtml+xml",
];

/// `mimeapps.list` with `id` as the default for links and web pages.
/// Everything else in the file stays as it was.
pub fn with_default_browser(text: &str, id: &str) -> String {
    let mut out = String::new();
    let mut in_defaults = false;
    let mut written = false;
    let write = |out: &mut String| {
        for mime in BROWSER_TYPES {
            out.push_str(&format!("{mime}={id};\n"));
        }
    };
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if in_defaults && !written {
                write(&mut out);
                written = true;
            }
            in_defaults = trimmed == "[Default Applications]";
        } else if in_defaults
            && trimmed
                .split_once('=')
                .is_some_and(|(k, _)| BROWSER_TYPES.contains(&k.trim()))
        {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if in_defaults && !written {
        write(&mut out);
    } else if !written {
        if !out.is_empty() && !out.ends_with("\n\n") {
            out.push('\n');
        }
        out.push_str("[Default Applications]\n");
        write(&mut out);
    }
    out
}

/// `$XDG_CONFIG_HOME/mimeapps.list`, falling back to `~/.config`.
pub fn mimeapps_path() -> Option<PathBuf> {
    derisk_sys::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| Path::new(dir).is_absolute())
        .map(PathBuf::from)
        .or_else(|| derisk_sys::env::var_os("HOME").map(|h| Path::new(&h).join(".config")))
        .map(|dir| dir.join("mimeapps.list"))
}

/// Makes `id` the browser every app opens links with.
pub fn apply_browser(path: &Path, id: &str) -> io::Result<()> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let temporary = path.with_extension("list.tmp");
    fs::write(&temporary, with_default_browser(&text, id))?;
    fs::rename(&temporary, path)
}

/// Puts `items` in an order drawn from `seed` (xorshift and Fisher-Yates:
/// fair enough for a list of ten, and no dependency for it).
pub fn shuffle<T>(items: &mut [T], seed: u64) {
    let mut state = seed | 1;
    for i in (1..items.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        items.swap(i, (state % (i as u64 + 1)) as usize);
    }
}

fn seed() -> u64 {
    derisk_sys::time::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
}

/// A browser being installed from Flathub.
#[derive(Debug)]
struct Install {
    child: Child,
    flathub: &'static str,
    /// Whether this is the per-user retry.
    user: bool,
}

/// The screens' state: the lists in the order they are shown, and any
/// install in progress.
#[derive(Debug)]
pub struct ChoiceUi {
    dirs: Vec<PathBuf>,
    browsers: Vec<Browser>,
    engines: Vec<SearchEngine>,
    install: Option<Install>,
    status: Option<String>,
}

impl Default for ChoiceUi {
    fn default() -> Self {
        Self::with_dirs(application_dirs())
    }
}

impl ChoiceUi {
    /// Reads browsers from `dirs` instead of the standard places.
    pub fn with_dirs(dirs: Vec<PathBuf>) -> Self {
        let mut engines = SearchEngine::ALL.to_vec();
        shuffle(&mut engines, seed());
        let mut ui = Self {
            dirs,
            browsers: Vec::new(),
            engines,
            install: None,
            status: None,
        };
        ui.rescan();
        ui
    }

    /// The browsers in the order shown.
    pub fn browsers(&self) -> &[Browser] {
        &self.browsers
    }

    /// The engines in the order shown.
    pub fn engines(&self) -> &[SearchEngine] {
        &self.engines
    }

    /// The latest install message.
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    /// Re-reads the installed browsers and draws a new order.
    pub fn rescan(&mut self) {
        self.browsers = browsers(&self.dirs);
        shuffle(&mut self.browsers, seed());
    }

    /// Starts installing `flathub` from Flathub: system-wide first, which
    /// the OS allows its administrators, then for this user alone from the
    /// app's flatpakref, which needs no configured remote.
    fn install(&mut self, flathub: &'static str, user: bool) {
        let program = derisk_sys::env::var_os("DERISK_FLATPAK").unwrap_or_else(|| "flatpak".into());
        let mut command = Command::new(program);
        command.args(["install", "--noninteractive", "-y"]);
        if user {
            command.arg("--user").arg(format!(
                "https://dl.flathub.org/repo/appstream/{flathub}.flatpakref"
            ));
        } else {
            command.args(["flathub", flathub]);
        }
        match command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => {
                self.install = Some(Install {
                    child,
                    flathub,
                    user,
                });
                self.status = Some("Installing…".into());
            }
            Err(e) => self.status = Some(format!("Could not run flatpak: {e}")),
        }
    }

    /// Checks on an install. Returns the desktop ID of a browser that has
    /// just finished installing.
    pub fn poll(&mut self) -> Option<String> {
        let install = self.install.as_mut()?;
        let status = match install.child.try_wait() {
            Ok(None) => return None,
            Ok(Some(status)) => status,
            Err(e) => {
                self.status = Some(format!("Install failed: {e}"));
                self.install = None;
                return None;
            }
        };
        let (flathub, user) = (install.flathub, install.user);
        self.install = None;
        if status.success() {
            self.status = Some("Installed".into());
            self.rescan();
            return Some(format!("{flathub}.desktop"));
        }
        if !user {
            self.install(flathub, true);
        } else {
            self.status = Some("Install failed. Try again from Bazaar.".into());
        }
        None
    }

    fn installing(&self) -> Option<&'static str> {
        self.install.as_ref().map(|i| i.flathub)
    }
}

impl Drop for ChoiceUi {
    /// An install outlives the screen: flatpak finishes on its own, and the
    /// browser is there next time. Only the zombie is reaped here.
    fn drop(&mut self) {
        if let Some(mut install) = self.install.take() {
            std::thread::spawn(move || install.child.wait());
        }
    }
}

/// An app's full-color icon from the icon theme, decoded once per context.
/// A browser not installed yet usually has none, and gets an empty square.
fn app_icon(ctx: &egui::Context, name: &str) -> Option<egui::TextureHandle> {
    if name.is_empty() {
        return None;
    }
    let id = egui::Id::new(("derisk-choice-icon", name));
    if let Some(cached) = ctx.data(|d| d.get_temp::<Option<egui::TextureHandle>>(id)) {
        return cached;
    }
    let texture = derisk_icons::find(name, 48)
        .and_then(|path| derisk_icons::load(&path, 48))
        .map(|image| {
            ctx.load_texture(
                format!("derisk-choice-icon:{name}"),
                image,
                Default::default(),
            )
        });
    ctx.data_mut(|d| d.insert_temp(id, texture.clone()));
    texture
}

/// One option: a radio row with a name, a line under it and an icon, the
/// same size for every option so none stands out.
fn option_row(
    ui: &mut egui::Ui,
    selected: bool,
    icon: &str,
    name: &str,
    detail: &str,
    theme: &Theme,
) -> egui::Response {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::hover());
        if let Some(texture) = app_icon(ui.ctx(), icon) {
            ui.painter().image(
                texture.id(),
                rect,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
        let response = ui.radio(selected, name);
        ui.label(egui::RichText::new(detail).small().color(theme.border));
        response
    })
    .inner
}

/// The browser choice screen. Writes the choice to `defaults.browser`; the
/// caller saves it and calls [`apply_browser`] (Settings does both on Save).
pub fn browser_page(
    ui: &mut egui::Ui,
    defaults: &mut Defaults,
    state: &mut ChoiceUi,
    theme: &Theme,
) {
    if let Some(id) = state.poll() {
        defaults.browser = id;
    }
    ui.label("Choose the browser that opens links. You can change it later in Settings.");
    ui.add_space(8.0);
    let installing = state.installing();
    let mut install = None;
    for browser in &state.browsers {
        let selected = defaults.browser == browser.id;
        let detail = match browser.flathub {
            None => "Installed",
            Some(id) if Some(id) == installing => "Installing…",
            Some(_) => "From Flathub",
        };
        let enabled = browser.installed() || installing.is_none();
        let clicked = ui
            .add_enabled_ui(enabled, |ui| {
                option_row(ui, selected, &browser.icon, &browser.name, detail, theme).clicked()
            })
            .inner;
        if clicked {
            match browser.flathub {
                None => defaults.browser.clone_from(&browser.id),
                Some(id) => install = Some(id),
            }
        }
    }
    if let Some(id) = install {
        state.install(id, false);
    }
    if let Some(status) = &state.status {
        ui.add_space(8.0);
        ui.label(egui::RichText::new(status).color(theme.accent));
    }
    if state.install.is_some() {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(500));
    }
}

/// The search engine choice screen. Writes the choice to `defaults.search`.
pub fn search_page(
    ui: &mut egui::Ui,
    defaults: &mut Defaults,
    state: &mut ChoiceUi,
    theme: &Theme,
) {
    ui.label(
        "Choose the search engine the command palette sends web searches to. \
         You can change it later in Settings.",
    );
    ui.add_space(8.0);
    for &engine in &state.engines {
        let selected = defaults.search == Some(engine);
        if option_row(ui, selected, "", engine.name(), engine.summary(), theme).clicked() {
            defaults.search = Some(engine);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_garbled_policy_hides_the_screens() {
        assert_eq!(Policy::parse(""), Policy::default());
        assert_eq!(
            Policy::parse("choice_screens.browser = yes\n"),
            Policy::default()
        );
        let both = Policy::parse("choice_screens.browser = true\nchoice_screens.search=true");
        assert!(both.browser && both.search);
    }

    #[test]
    fn unanswered_only_counts_screens_that_are_on() {
        let mut defaults = Defaults::default();
        assert!(!Policy::default().unanswered(&defaults));
        let browser = Policy {
            browser: true,
            search: false,
        };
        assert!(browser.unanswered(&defaults));
        defaults.browser = "org.mozilla.firefox.desktop".into();
        assert!(!browser.unanswered(&defaults));
        let both = Policy {
            browser: true,
            search: true,
        };
        assert!(both.unanswered(&defaults));
        defaults.search = Some(SearchEngine::Qwant);
        assert!(!both.unanswered(&defaults));
    }

    #[test]
    fn search_urls_keep_the_query_inside_the_query_string() {
        assert_eq!(
            SearchEngine::Google.url(" a/b?c#d "),
            "https://www.google.com/search?q=a%2Fb%3Fc%23d"
        );
        assert_eq!(
            SearchEngine::Mojeek.url("žltý"),
            "https://www.mojeek.com/search?q=%C5%BElt%C3%BD"
        );
        for engine in SearchEngine::ALL {
            assert_eq!(SearchEngine::parse(engine.as_str()), Some(engine));
            assert!(engine.url("x").starts_with("https://"));
        }
    }

    #[test]
    fn only_visible_https_handlers_are_browsers() {
        let firefox = "[Desktop Entry]\nType=Application\nName=Firefox\nIcon=firefox\n\
                       MimeType=text/html;x-scheme-handler/http;x-scheme-handler/https;\n\
                       [Desktop Action new-window]\nName=New Window\n";
        assert_eq!(
            parse_browser(firefox),
            Some(("Firefox".into(), "firefox".into()))
        );
        assert_eq!(
            parse_browser(&firefox.replace("Icon", "NoDisplay=true\nIcon")),
            None
        );
        assert_eq!(
            parse_browser(&firefox.replace("x-scheme-handler/https;", "")),
            None
        );
        assert_eq!(
            parse_browser("[Desktop Entry]\nType=Application\nName=Files\n"),
            None
        );
    }

    #[test]
    fn installed_browsers_replace_their_flathub_entries() {
        let dir = std::env::temp_dir().join(format!("derisk-choice-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("org.mozilla.firefox.desktop"),
            "[Desktop Entry]\nType=Application\nName=Firefox\nMimeType=x-scheme-handler/https;\n",
        )
        .unwrap();
        let list = browsers(std::slice::from_ref(&dir));
        fs::remove_dir_all(&dir).unwrap();
        let firefox: Vec<_> = list
            .iter()
            .filter(|b| b.id == "org.mozilla.firefox.desktop")
            .collect();
        assert_eq!(firefox.len(), 1);
        assert!(firefox[0].installed());
        assert_eq!(list.len(), FLATHUB_BROWSERS.len());
        assert!(
            list.iter()
                .filter(|b| !b.installed())
                .all(|b| b.flathub.is_some())
        );
    }

    #[test]
    fn the_default_browser_replaces_only_its_own_lines() {
        let before = "[Added Associations]\ntext/html=old.desktop;\n\n\
                      [Default Applications]\ntext/html=old.desktop;\nimage/png=viewer.desktop;\n\n\
                      [Removed Associations]\n";
        let after = with_default_browser(before, "new.desktop");
        assert!(after.contains("[Added Associations]\ntext/html=old.desktop;\n"));
        assert!(after.contains("image/png=viewer.desktop;\n"));
        assert!(!after.contains("[Default Applications]\ntext/html=old.desktop"));
        for mime in BROWSER_TYPES {
            assert_eq!(after.matches(&format!("{mime}=new.desktop;")).count(), 1);
        }
        assert!(after.ends_with("[Removed Associations]\n"));
        let fresh = with_default_browser("", "new.desktop");
        assert!(fresh.starts_with("[Default Applications]\nx-scheme-handler/http=new.desktop;\n"));
        assert_eq!(with_default_browser(&fresh, "new.desktop"), fresh);
    }

    #[test]
    fn shuffling_keeps_every_item() {
        let mut items: Vec<u32> = (0..10).collect();
        shuffle(&mut items, 12345);
        let mut sorted = items.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..10).collect::<Vec<_>>());
        let mut other: Vec<u32> = (0..10).collect();
        shuffle(&mut other, 54321);
        assert_ne!(items, other);
    }
}
