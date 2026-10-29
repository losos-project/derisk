//! The Store app: the front page with the categories, the waiting updates
//! and the release feed; a page per category, split into its sections; a
//! page per app; and the installed apps.

use std::{collections::HashMap, sync::Arc};

use mcsapi_components::{Button, ButtonVariant, Tokens};
use mcsapi_ui::{
    App as UiApp, Theme,
    egui::{self, RichText, Sense, TextureHandle, Vec2},
};

use crate::{
    appstream::Block,
    catalog::{App, Catalog},
    category::{self, Category},
    flatpak::Installed,
    reviews::{Ratings, Review, Reviews, stars},
};

use crate::worker::{Event, Order, Worker};

/// The app's ID.
pub const APP_ID: &str = "org.derisk.store";

/// An app card's size: narrowest it is laid out at, and its height.
const CARD_WIDTH: f32 = 280.0;
const CARD_HEIGHT: f32 = 76.0;
/// Room between cards and rows of them.
const GAP: f32 = 10.0;
/// How many releases the front page's feed shows.
const FEED: usize = 40;
/// How many of your apps' releases the front page shows above the rest.
const YOURS: usize = 6;

/// Where the store is.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Page {
    /// The front page.
    Explore,
    /// A category, or one of its sections.
    Category(Category, Option<String>),
    /// An app.
    App(String),
    /// The installed apps and their updates.
    Installed,
}

/// An icon's texture, once asked for.
enum Icon {
    Asked,
    Ready(TextureHandle),
    Missing,
}

/// An app's ratings and reviews, once asked for.
enum Said {
    Asked,
    Ready(Reviews),
    Failed(String),
}

/// The release feed, worked out once per catalogue: app IDs, newest
/// release first.
#[derive(Default)]
struct Feed {
    ids: Vec<String>,
}

/// The Store app.
pub struct StoreApp {
    worker: Worker,
    /// Where flatpak runs, once the thread knows: the machine's name, or
    /// `None` for this computer.
    place: Option<Option<String>>,
    catalog: Option<Arc<Catalog>>,
    catalog_error: Option<String>,
    feed: Feed,
    installed: HashMap<String, Installed>,
    updates: Vec<String>,
    /// What the thread is doing, while it is.
    doing: Option<String>,
    progress: Option<f32>,
    line: String,
    status: Option<Result<String, String>>,
    page: Page,
    back: Vec<Page>,
    query: String,
    /// The last search and its results, by ID.
    results: Option<(String, Vec<String>)>,
    icons: HashMap<String, Icon>,
    /// Ratings and reviews, by app ID, for the pages opened so far.
    said: HashMap<String, Said>,
    /// An app waiting for the person to confirm its removal.
    confirm: Option<String>,
}

impl std::fmt::Debug for StoreApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreApp")
            .field("page", &self.page)
            .field("apps", &self.catalog.as_ref().map(|c| c.apps().len()))
            .field("installed", &self.installed.len())
            .field("doing", &self.doing)
            .finish_non_exhaustive()
    }
}

impl Default for StoreApp {
    fn default() -> Self {
        Self::new()
    }
}

impl StoreApp {
    /// Starts reading Flathub's catalogue from flatpak wherever it runs.
    pub fn new() -> Self {
        let mut app = Self::with_worker(Worker::start());
        if cfg!(target_arch = "wasm32") {
            app.doing = None;
            app.catalog_error = Some(
                "The Store installs apps with flatpak on the computer itself, which this demo \
                 in a browser does not have."
                    .into(),
            );
        }
        app
    }

    /// The store on `flatpak`, for tests and for hosts that know better
    /// where flatpak is.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn on(flatpak: Arc<dyn crate::flatpak::Flatpak>) -> Self {
        Self::with_worker(Worker::start_with(move || flatpak, crate::reviews::fetch))
    }

    /// The store on `flatpak`, with reviews from `fetch` (given an app ID
    /// and a locale) instead of ODRS: for tests, which ask nothing of the
    /// network.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn with_reviews(
        flatpak: Arc<dyn crate::flatpak::Flatpak>,
        fetch: crate::reviews::Fetch,
    ) -> Self {
        Self::with_worker(Worker::start_with(move || flatpak, fetch))
    }

    fn with_worker(worker: Worker) -> Self {
        Self {
            worker,
            place: None,
            catalog: None,
            catalog_error: None,
            feed: Feed::default(),
            installed: HashMap::new(),
            updates: Vec::new(),
            doing: Some("Reading Flathub's catalogue".into()),
            progress: None,
            line: String::new(),
            status: None,
            page: Page::Explore,
            back: Vec::new(),
            query: String::new(),
            results: None,
            icons: HashMap::new(),
            said: HashMap::new(),
            confirm: None,
        }
    }

    /// The catalogue, once read.
    pub fn catalog(&self) -> Option<&Catalog> {
        self.catalog.as_deref()
    }

    /// Whether the thread is still busy: reading, installing, updating.
    pub fn busy(&self) -> bool {
        self.doing.is_some()
    }

    /// Opens `id`'s page, as a desktop action or a link from elsewhere
    /// would.
    pub fn show_app(&mut self, id: &str) {
        self.go(Page::App(id.to_owned()));
    }

    /// Opens a category's page.
    pub fn show_category(&mut self, category: Category) {
        self.go(Page::Category(category, None));
    }

    /// Shows the installed apps.
    pub fn show_installed(&mut self) {
        self.go(Page::Installed);
    }

    /// The store opened on a `.flatpakref` (an app to install) or a
    /// `.flatpakrepo` (a remote to add), as the desktop opens one: it does
    /// that once the catalogue is read, then shows the installed apps.
    pub fn open_file(path: &std::path::Path) -> Self {
        let mut app = Self::new();
        app.install_file(path);
        app
    }

    /// Installs the app a `.flatpakref` names, or adds the remote a
    /// `.flatpakrepo` describes, and shows the installed apps.
    pub fn install_file(&mut self, path: &std::path::Path) {
        self.order(Order::File(path.to_owned()));
        self.show_installed();
    }

    /// Installs `id` from Flathub, as its page's Install button does.
    pub fn install(&mut self, id: &str) {
        let name = self.name(id);
        self.order(Order::Install {
            id: id.to_owned(),
            name,
        });
    }

    /// The name of app `id`, or its ID when the catalogue lacks it.
    fn name(&self, id: &str) -> String {
        self.catalog
            .as_ref()
            .and_then(|c| c.get(id))
            .map_or_else(|| id.to_owned(), |a| a.component.name.clone())
    }

    /// Searches, as typing in the search field does.
    pub fn search(&mut self, query: &str) {
        self.query = query.to_owned();
    }

    fn go(&mut self, page: Page) {
        if self.page != page {
            let previous = std::mem::replace(&mut self.page, page);
            self.back.push(previous);
        }
        self.query.clear();
        self.confirm = None;
    }

    fn receive(&mut self, ctx: &egui::Context) {
        let events: Vec<Event> = self.worker.events().collect();
        for event in events {
            match event {
                Event::Place(place) => self.place = Some(place),
                Event::Doing(doing) => {
                    self.doing = Some(doing);
                    self.progress = None;
                    self.line.clear();
                }
                Event::Catalog(Ok(catalog)) => {
                    self.catalog_error = None;
                    self.results = None;
                    let now = now();
                    self.feed = Feed {
                        ids: catalog
                            .news(now, |_| true)
                            .into_iter()
                            .map(|n| n.app.id().to_owned())
                            .collect(),
                    };
                    self.catalog = Some(catalog);
                }
                Event::Catalog(Err(error)) => self.catalog_error = Some(error),
                Event::Installed(installed) => {
                    self.installed = installed.into_iter().map(|i| (i.id.clone(), i)).collect();
                }
                Event::Updates(updates) => self.updates = updates.unwrap_or_default(),
                Event::Idle => {
                    self.doing = None;
                    self.progress = None;
                }
                Event::Line { line, progress } => {
                    self.line = line;
                    if progress.is_some() {
                        self.progress = progress;
                    }
                }
                Event::Done(done) => {
                    self.doing = None;
                    self.progress = None;
                    self.status = match done {
                        Ok(message) if message.is_empty() => None,
                        done => Some(done),
                    };
                }
                Event::Icon { file, image } => {
                    let icon = match image {
                        Some(image) => Icon::Ready(ctx.load_texture(
                            format!("derisk-store-icon:{file}"),
                            image,
                            egui::TextureOptions::LINEAR,
                        )),
                        None => Icon::Missing,
                    };
                    self.icons.insert(file, icon);
                }
                Event::Reviews { id, reviews } => {
                    let said = match reviews {
                        Ok(reviews) => Said::Ready(reviews),
                        Err(error) => Said::Failed(error),
                    };
                    self.said.insert(id, said);
                }
            }
        }
    }

    fn order(&mut self, order: Order) {
        if self.worker.send(order) {
            self.status = None;
            // Busy from the moment it is asked, not from when the thread
            // gets to it: every order ends in a Done, whose words replace
            // these, and a click in between must not send a second order.
            self.doing = Some("Working…".into());
        } else {
            self.status = Some(Err("The store's thread is not answering".into()));
        }
    }

    /// The machine apps install into, or `None` for this computer.
    fn machine(&self) -> Option<&str> {
        self.place.as_ref().and_then(|p| p.as_deref())
    }

    /// The pages: the front page and your apps, then every category.
    fn sidebar(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        let searching = !self.query.trim().is_empty();
        let installed = match self.updates.len() {
            0 => "Installed".to_owned(),
            n => format!("Installed ({n} to update)"),
        };
        // Before the catalogue is read every category is listed; after,
        // only the ones with apps in it.
        let categories: Vec<Category> = Category::ALL
            .into_iter()
            .filter(|&c| self.catalog.as_ref().is_none_or(|cat| cat.count(c) > 0))
            .collect();
        let mut picked = None;
        derisk_frame::navigation(ui, tokens, "store-pages", |nav| {
            nav.section("Flathub");
            let explore = self.page == Page::Explore && !searching;
            if nav.entry("Explore", explore).clicked() {
                picked = Some(Page::Explore);
            }
            let on = self.page == Page::Installed && !searching;
            if nav.entry(&installed, on).clicked() {
                picked = Some(Page::Installed);
            }
            nav.section("Categories");
            for category in categories {
                let on = !searching && matches!(&self.page, Page::Category(c, _) if *c == category);
                if nav.entry(category.name(), on).clicked() {
                    picked = Some(Page::Category(category, None));
                }
            }
        });
        if let Some(page) = picked {
            self.go(page);
        }
    }

    /// The page's title and what it holds, with the search field and Back
    /// at its right, as every page has them.
    fn header(&mut self, ui: &mut egui::Ui, tokens: &Tokens, title: &str, description: &str) {
        let narrow = ui.available_width() < derisk_frame::PHONE;
        derisk_frame::header_with(ui, tokens, title, description, |ui| {
            // Under the title on a phone, with Back beside it.
            let width = if narrow {
                (ui.available_width() - 74.0).max(120.0)
            } else {
                220.0
            };
            ui.add(
                derisk_frame::field(&mut self.query)
                    .hint_text("Search Flathub")
                    .desired_width(width),
            );
            let back = Button::new("Back")
                .variant(ButtonVariant::Outline)
                .enabled(!self.back.is_empty());
            if ui.add(back).clicked()
                && let Some(page) = self.back.pop()
            {
                self.page = page;
                self.query.clear();
                self.confirm = None;
            }
        });
    }

    /// What the store is doing, or what it did, along the action bar, with
    /// Refresh at its right as Settings has Save.
    fn status_bar(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        let mut dismiss = false;
        derisk_frame::action_bar(ui, tokens, "store-actions", |ui| {
            let refresh = Button::new("Refresh")
                .variant(ButtonVariant::Outline)
                .enabled(self.doing.is_none());
            // Right to left: Refresh last on the line, the status before it
            // taking what is left.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(refresh)
                    .on_hover_text("Fetch Flathub's catalogue again and look for updates")
                    .clicked()
                {
                    self.order(Order::Refresh);
                }
                if self.status.is_some() && self.doing.is_none() {
                    dismiss = ui
                        .add(Button::new("Dismiss").variant(ButtonVariant::Ghost))
                        .clicked();
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    if let Some(doing) = &self.doing {
                        ui.spinner();
                        ui.label(doing);
                        if let Some(progress) = self.progress {
                            ui.add(
                                egui::ProgressBar::new(progress)
                                    .desired_width(160.0)
                                    .show_percentage(),
                            );
                        }
                        if !self.line.is_empty() {
                            ui.add(
                                egui::Label::new(derisk_frame::note(tokens, &self.line)).truncate(),
                            );
                        }
                        return;
                    }
                    match &self.status {
                        Some(Ok(message)) => {
                            ui.add(egui::Label::new(message.as_str()).truncate());
                        }
                        Some(Err(error)) => {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(error.as_str()).color(tokens.destructive),
                                )
                                .truncate(),
                            );
                        }
                        None => {
                            let text = match self.machine() {
                                Some(machine) => format!("Apps install into the {machine} machine"),
                                None => "Apps from Flathub".to_owned(),
                            };
                            ui.label(derisk_frame::note(tokens, &text));
                        }
                    }
                });
            });
        });
        if dismiss {
            self.status = None;
        }
    }

    /// The texture for `app`'s icon, asking the thread for it the first
    /// time it is drawn.
    fn icon(&mut self, app: &App) -> Option<TextureHandle> {
        let (file, size) = app.component.icon.as_ref()?;
        match self.icons.get(file) {
            Some(Icon::Ready(texture)) => Some(texture.clone()),
            Some(Icon::Asked | Icon::Missing) => None,
            None => {
                if self.worker.icon(file, *size) {
                    self.icons.insert(file.clone(), Icon::Asked);
                }
                None
            }
        }
    }

    /// Draws `app`'s icon into `rect`: its picture, or its initial on a
    /// tile while there is none.
    fn paint_icon(&mut self, ui: &egui::Ui, app: &App, rect: egui::Rect) {
        match self.icon(app) {
            Some(texture) => {
                egui::Image::new(&texture)
                    .fit_to_exact_size(rect.size())
                    .paint_at(ui, rect);
            }
            None => {
                let visuals = ui.visuals();
                ui.painter().rect_filled(
                    rect,
                    rect.width() / 5.0,
                    visuals.widgets.inactive.bg_fill,
                );
                let initial = app
                    .component
                    .name
                    .chars()
                    .next()
                    .map(|c| c.to_uppercase().to_string())
                    .unwrap_or_default();
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    initial,
                    egui::FontId::proportional(rect.height() * 0.5),
                    visuals.weak_text_color(),
                );
            }
        }
    }

    /// An app's tile: icon, name, summary. Returns whether it was clicked.
    fn card(&mut self, ui: &mut egui::Ui, tokens: &Tokens, app: &App, width: f32) -> bool {
        let installed = self.installed.contains_key(app.id());
        let update = self.updates.iter().any(|u| u == app.id());
        let size = egui::vec2(width, CARD_HEIGHT);
        let mut icon = None;
        let response = derisk_frame::tile(ui, tokens, size, &app.component.name, |ui| {
            ui.horizontal_centered(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(48.0), Sense::hover());
                icon = Some(rect);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.add(
                        egui::Label::new(RichText::new(&app.component.name).strong())
                            .truncate()
                            .selectable(false),
                    );
                    ui.add(
                        egui::Label::new(derisk_frame::note(tokens, &app.component.summary))
                            .truncate()
                            .selectable(false),
                    );
                    let mut marks = Vec::new();
                    if update {
                        marks.push("Update ready".to_owned());
                    } else if installed {
                        marks.push("Installed".to_owned());
                    }
                    if app.component.verified {
                        marks.push("✔ Verified".to_owned());
                    }
                    if !marks.is_empty() {
                        ui.add(
                            egui::Label::new(derisk_frame::note(tokens, &marks.join(" · ")))
                                .truncate()
                                .selectable(false),
                        );
                    }
                });
            });
        });
        if let Some(rect) = icon {
            self.paint_icon(ui, app, rect);
        }
        response.clicked()
    }

    /// Tiles for `apps` in as many columns as fit, only the rows in view
    /// laid out, so a category of a thousand apps scrolls as smoothly as one
    /// of ten.
    fn grid(&mut self, ui: &mut egui::Ui, tokens: &Tokens, apps: &[&App], salt: &str) {
        // Clear of the scroll bar, as derisk_frame::scroll keeps things.
        let width = ui.available_width() - derisk_frame::GUTTER;
        let columns = (((width + GAP) / (CARD_WIDTH + GAP)).floor() as usize).max(1);
        let card = ((width - GAP * (columns as f32 - 1.0)) / columns as f32).max(120.0);
        let rows = apps.len().div_ceil(columns);
        let mut open = None;
        egui::ScrollArea::vertical()
            .id_salt(salt)
            .auto_shrink([false, false])
            .show_rows(ui, CARD_HEIGHT + GAP, rows, |ui, range| {
                ui.spacing_mut().item_spacing = egui::vec2(GAP, GAP);
                for row in range {
                    ui.horizontal(|ui| {
                        for app in apps.iter().skip(row * columns).take(columns) {
                            if self.card(ui, tokens, app, card) {
                                open = Some(app.id().to_owned());
                            }
                        }
                    });
                }
            });
        if let Some(id) = open {
            self.go(Page::App(id));
        }
    }

    /// The catalogue, or a card saying why there is none yet.
    fn loading(&mut self, ui: &mut egui::Ui, tokens: &Tokens) -> Option<Arc<Catalog>> {
        if let Some(catalog) = &self.catalog {
            return Some(catalog.clone());
        }
        derisk_frame::card(ui, tokens, |ui| match &self.catalog_error {
            Some(error) => {
                derisk_frame::empty(ui, tokens, "Flathub's catalogue could not be read", error);
                ui.vertical_centered(|ui| {
                    let again = Button::new("Try again").enabled(self.doing.is_none());
                    if ui.add(again).clicked() {
                        self.order(Order::Refresh);
                    }
                });
            }
            None => {
                ui.vertical_centered(|ui| {
                    ui.add_space(24.0);
                    ui.spinner();
                    ui.label(match self.machine() {
                        Some(machine) => format!("Reading Flathub's catalogue in {machine}"),
                        None => "Reading Flathub's catalogue".to_owned(),
                    });
                    ui.add_space(24.0);
                });
            }
        });
        None
    }

    fn explore(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        let description = match self.machine() {
            Some(machine) => {
                format!(
                    "Apps install into the {machine} machine and open on your desktop from there."
                )
            }
            None => "Apps from Flathub, and what is new in them.".to_owned(),
        };
        self.header(ui, tokens, "Explore", &description);
        let Some(catalog) = self.loading(ui, tokens) else {
            return;
        };
        let mut open: Option<Page> = None;
        derisk_frame::scroll(ui, "explore", |ui| {
            if !self.updates.is_empty() {
                self.updates_section(ui, tokens, &catalog, &mut open);
            }
            // Your apps' release notes come first: they are the news
            // that is about you.
            let yours: Vec<String> = self
                .feed
                .ids
                .iter()
                .filter(|id| self.installed.contains_key(*id))
                .take(YOURS)
                .cloned()
                .collect();
            if !yours.is_empty() {
                derisk_frame::subheading(ui, tokens, "What's new in your apps");
                for id in &yours {
                    if let Some(app) = catalog.get(id) {
                        self.news(ui, tokens, app, "yours", &mut open);
                    }
                }
            }
            derisk_frame::subheading(ui, tokens, "Categories");
            self.categories(ui, tokens, &catalog, &mut open);
            derisk_frame::subheading(ui, tokens, "Just released on Flathub");
            ui.label(derisk_frame::note(
                tokens,
                "The newest releases of Flathub's apps, with what their developers say changed.",
            ));
            let ids: Vec<String> = self.feed.ids.iter().take(FEED).cloned().collect();
            if ids.is_empty() {
                ui.label("No app in the catalogue has release notes yet.");
            }
            for id in &ids {
                if let Some(app) = catalog.get(id) {
                    self.news(ui, tokens, app, "all", &mut open);
                }
            }
        });
        if let Some(page) = open {
            self.go(page);
        }
    }

    /// The category tiles, as many to a row as fit.
    fn categories(
        &mut self,
        ui: &mut egui::Ui,
        tokens: &Tokens,
        catalog: &Catalog,
        open: &mut Option<Page>,
    ) {
        let width = ui.available_width();
        let columns = (((width + GAP) / (200.0 + GAP)).floor() as usize).clamp(1, 5);
        let tile = (width - GAP * (columns as f32 - 1.0)) / columns as f32;
        let listed: Vec<Category> = Category::ALL
            .into_iter()
            .filter(|&c| catalog.count(c) > 0)
            .collect();
        for row in listed.chunks(columns) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                for &category in row {
                    let size = egui::vec2(tile, 56.0);
                    let response = derisk_frame::tile(ui, tokens, size, category.name(), |ui| {
                        ui.horizontal_centered(|ui| {
                            let (icon, _) =
                                ui.allocate_exact_size(Vec2::splat(28.0), Sense::hover());
                            match derisk_icons::texture(ui.ctx(), category.icon()) {
                                Some(texture) => egui::Image::new(&texture)
                                    .fit_to_exact_size(icon.size())
                                    .paint_at(ui, icon),
                                None => {
                                    derisk_icons::paint(
                                        ui.painter(),
                                        icon,
                                        "folder",
                                        tokens.muted_foreground,
                                    );
                                }
                            }
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                ui.add(
                                    egui::Label::new(RichText::new(category.name()).strong())
                                        .truncate()
                                        .selectable(false),
                                );
                                let count = format!("{} apps", catalog.count(category));
                                ui.add(
                                    egui::Label::new(derisk_frame::note(tokens, &count))
                                        .selectable(false),
                                );
                            });
                        });
                    });
                    if response.clicked() {
                        *open = Some(Page::Category(category, None));
                    }
                }
            });
            ui.add_space(GAP - ui.spacing().item_spacing.y);
        }
    }

    /// One release in the feed, as a card: the app, its version and when,
    /// and its notes. A click opens the app's page.
    fn news(
        &mut self,
        ui: &mut egui::Ui,
        tokens: &Tokens,
        app: &App,
        list: &str,
        open: &mut Option<Page>,
    ) {
        let Some(release) = app
            .component
            .releases
            .iter()
            .find(|r| !r.development && !r.notes.is_empty())
        else {
            return;
        };
        let release = release.clone();
        let top = ui.cursor().top();
        derisk_frame::card(ui, tokens, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::hover());
                self.paint_icon(ui, app, rect);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.add(
                        egui::Label::new(RichText::new(&app.component.name).strong())
                            .truncate()
                            .selectable(false),
                    );
                    let when = release.timestamp.map(|t| ago(now(), t)).unwrap_or_default();
                    let version = if release.version.is_empty() {
                        "New release".to_owned()
                    } else {
                        format!("Version {}", release.version)
                    };
                    let line = if when.is_empty() {
                        version
                    } else {
                        format!("{version} · {when}")
                    };
                    ui.add(egui::Label::new(derisk_frame::note(tokens, &line)).selectable(false));
                });
            });
            notes(ui, &release.notes, 4);
        });
        let rect = egui::Rect::from_min_max(
            egui::pos2(ui.min_rect().left(), top),
            egui::pos2(ui.min_rect().right(), ui.cursor().top()),
        );
        let response = ui.interact(rect, ui.id().with(("news", list, app.id())), Sense::click());
        if response.clicked() {
            *open = Some(Page::App(app.id().to_owned()));
        }
        response.on_hover_cursor(egui::CursorIcon::PointingHand);
    }

    /// The updates waiting, in a card with Update all over it.
    fn updates_section(
        &mut self,
        ui: &mut egui::Ui,
        tokens: &Tokens,
        catalog: &Catalog,
        open: &mut Option<Page>,
    ) {
        derisk_frame::subheading(ui, tokens, "Updates");
        let ids = self.updates.clone();
        derisk_frame::card(ui, tokens, |ui| {
            ui.horizontal(|ui| {
                let waiting = match ids.len() {
                    1 => "1 app has an update waiting".to_owned(),
                    n => format!("{n} apps have an update waiting"),
                };
                ui.label(derisk_frame::note(tokens, &waiting));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(Button::new("Update all").enabled(self.doing.is_none()))
                        .clicked()
                    {
                        self.order(Order::Update(None));
                    }
                });
            });
            for id in &ids {
                let name = catalog
                    .get(id)
                    .map_or(id.as_str(), |app| app.component.name.as_str())
                    .to_owned();
                ui.horizontal(|ui| {
                    if let Some(app) = catalog.get(id) {
                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::hover());
                        self.paint_icon(ui, app, rect);
                    }
                    if ui.link(RichText::new(&name).strong()).clicked() {
                        *open = Some(Page::App(id.clone()));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let update = Button::new("Update")
                            .variant(ButtonVariant::Outline)
                            .enabled(self.doing.is_none());
                        if ui.add(update).clicked() {
                            self.order(Order::Update(Some((id.clone(), name.clone()))));
                        }
                    });
                });
            }
        });
    }

    fn category(
        &mut self,
        ui: &mut egui::Ui,
        tokens: &Tokens,
        category: Category,
        section: Option<String>,
    ) {
        let count = self
            .catalog
            .as_ref()
            .map(|c| format!("{} apps", c.count(category)))
            .unwrap_or_default();
        self.header(ui, tokens, category.name(), &count);
        let Some(catalog) = self.loading(ui, tokens) else {
            return;
        };
        let sections = catalog.sections(category, 3);
        if !sections.is_empty() {
            let mut picked = None;
            ui.horizontal_wrapped(|ui| {
                if ui.selectable_label(section.is_none(), "All").clicked() {
                    picked = Some(None);
                }
                for (name, count) in &sections {
                    let on = section.as_deref() == Some(name.as_str());
                    if ui
                        .selectable_label(on, format!("{} ({count})", category::section_name(name)))
                        .clicked()
                    {
                        picked = Some(Some(name.clone()));
                    }
                }
            });
            // Sections replace each other rather than stacking up Back.
            if let Some(picked) = picked {
                self.page = Page::Category(category, picked);
            }
        }
        ui.add_space(4.0);
        let apps: Vec<&App> = catalog.in_category(category, section.as_deref()).collect();
        let salt = format!("category-{category:?}-{section:?}");
        self.grid(ui, tokens, &apps, &salt);
    }

    fn results(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        let query = self.query.trim().to_lowercase();
        if let Some(catalog) = &self.catalog {
            let stale = self.results.as_ref().is_none_or(|(q, _)| *q != query);
            if stale {
                let ids = catalog
                    .search(&query)
                    .into_iter()
                    .map(|a| a.id().to_owned())
                    .collect();
                self.results = Some((query.clone(), ids));
            }
        }
        let ids = self
            .results
            .as_ref()
            .map(|(_, ids)| ids.clone())
            .unwrap_or_default();
        let shown = format!("{} apps match “{}”", ids.len(), self.query.trim());
        self.header(ui, tokens, "Search", &shown);
        let Some(catalog) = self.loading(ui, tokens) else {
            return;
        };
        let apps: Vec<&App> = ids.iter().filter_map(|id| catalog.get(id)).collect();
        if apps.is_empty() {
            let text = format!("Nothing on Flathub matches “{}”.", self.query.trim());
            derisk_frame::card(ui, tokens, |ui| {
                derisk_frame::empty(ui, tokens, "No apps found", &text);
            });
            return;
        }
        self.grid(ui, tokens, &apps, "results");
    }

    fn app_page(&mut self, ui: &mut egui::Ui, tokens: &Tokens, id: &str) {
        let (title, by) = match self.catalog.as_ref().and_then(|c| c.get(id)) {
            Some(app) => {
                let mut by = app.component.developer.clone();
                if app.component.verified {
                    by.push_str(if by.is_empty() {
                        "✔ Verified"
                    } else {
                        " · ✔ Verified"
                    });
                }
                (app.component.name.clone(), by)
            }
            None => (id.to_owned(), String::new()),
        };
        self.header(ui, tokens, &title, &by);
        let Some(catalog) = self.loading(ui, tokens) else {
            return;
        };
        let Some(app) = catalog.get(id) else {
            derisk_frame::card(ui, tokens, |ui| {
                let text = format!("{id} is not in Flathub's catalogue.");
                derisk_frame::empty(ui, tokens, "No such app", &text);
            });
            return;
        };
        let installed = self.installed.get(id).cloned();
        let update = self.updates.iter().any(|u| u == id);
        let mut open = None;
        derisk_frame::scroll(ui, ("app", id), |ui| {
            derisk_frame::card(ui, tokens, |ui| {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(96.0), Sense::hover());
                    self.paint_icon(ui, app, rect);
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        ui.add(egui::Label::new(&app.component.summary).wrap());
                        self.actions(ui, app, installed.as_ref(), update);
                        ui.horizontal_wrapped(|ui| {
                            for &category in &app.categories {
                                if ui.button(category.name()).clicked() {
                                    open = Some(Page::Category(category, None));
                                }
                                for section in category::sections(&app.component, category) {
                                    if ui.small_button(category::section_name(section)).clicked() {
                                        open = Some(Page::Category(
                                            category,
                                            Some(section.to_owned()),
                                        ));
                                    }
                                }
                            }
                        });
                    });
                });
            });
            // What the app is and what people think of it come first;
            // its release notes are for those who already know it.
            if !app.component.description.is_empty() {
                derisk_frame::subheading(ui, tokens, "About");
                derisk_frame::card(ui, tokens, |ui| {
                    notes(ui, &app.component.description, usize::MAX);
                });
            }
            derisk_frame::subheading(ui, tokens, "Ratings and reviews");
            derisk_frame::card(ui, tokens, |ui| self.reviews(ui, tokens, id));
            let releases: Vec<_> = app
                .component
                .releases
                .iter()
                .filter(|r| !r.development)
                .collect();
            if let Some((latest, earlier)) = releases.split_first() {
                derisk_frame::subheading(ui, tokens, "What's new");
                derisk_frame::card(ui, tokens, |ui| {
                    release(ui, latest, usize::MAX);
                    if !earlier.is_empty() {
                        egui::CollapsingHeader::new("Earlier releases")
                            .id_salt(("earlier", id))
                            .show(ui, |ui| {
                                for r in earlier {
                                    release(ui, r, usize::MAX);
                                    ui.add_space(6.0);
                                }
                            });
                    }
                });
            }
            derisk_frame::subheading(ui, tokens, "Details");
            derisk_frame::card(ui, tokens, |ui| {
                derisk_frame::rows(ui, ("details", id), |ui| {
                    let mut row = |label: &str, value: &str| {
                        if !value.is_empty() {
                            ui.label(label);
                            ui.add(egui::Label::new(value).wrap());
                            ui.end_row();
                        }
                    };
                    row("App ID", &app.component.id);
                    row("Licence", &app.component.license);
                    if let Some(installed) = &installed {
                        row("Installed", &installed.version);
                        let place = match self.machine() {
                            Some(machine) => format!("In the {machine} machine"),
                            None if installed.installation == "user" => "For you".to_owned(),
                            None => "For everyone on this computer".to_owned(),
                        };
                        row("Where", &place);
                    }
                });
                let links: Vec<(String, String)> = app
                    .component
                    .urls
                    .iter()
                    .filter_map(|(kind, url)| Some((link_name(kind)?.to_owned(), url.clone())))
                    .collect();
                if !links.is_empty() {
                    ui.add_space(8.0);
                    ui.horizontal_wrapped(|ui| {
                        for (name, url) in &links {
                            if ui.link(name).on_hover_text(url).clicked() {
                                open_link(url);
                            }
                        }
                    });
                }
            });
        });
        if let Some(page) = open {
            self.go(page);
        }
    }

    /// App `id`'s ratings and reviews, asking for them the first time.
    fn reviews(&mut self, ui: &mut egui::Ui, tokens: &Tokens, id: &str) {
        if !self.said.contains_key(id) && self.worker.reviews(id) {
            self.said.insert(id.to_owned(), Said::Asked);
        }
        match self.said.get(id) {
            None if cfg!(target_arch = "wasm32") => {
                ui.label(derisk_frame::note(tokens, "Reviews are read from the Open Desktop Ratings Service, which a browser cannot reach from here."));
            }
            None | Some(Said::Asked) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(derisk_frame::note(tokens, "Reading what people say…"));
                });
            }
            Some(Said::Failed(error)) => {
                ui.label(derisk_frame::note(
                    tokens,
                    "Ratings and reviews could not be read just now.",
                ))
                .on_hover_text(error);
            }
            Some(Said::Ready(said)) => {
                match &said.ratings {
                    Some(ratings) => self::ratings(ui, tokens, ratings),
                    None => {
                        ui.label(derisk_frame::note(tokens, "Nobody has rated it yet."));
                    }
                }
                let now = now();
                for review in &said.reviews {
                    ui.add_space(8.0);
                    self::review(ui, tokens, review, now);
                }
            }
        }
    }

    /// Install, or Open, Update and Remove, with what the thread is doing.
    fn actions(
        &mut self,
        ui: &mut egui::Ui,
        app: &App,
        installed: Option<&Installed>,
        update: bool,
    ) {
        let idle = self.doing.is_none() && self.place.is_some();
        let id = app.id().to_owned();
        if self.confirm.as_deref() == Some(id.as_str()) {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!(
                    "Remove {}? Its data stays until you remove that too.",
                    app.component.name
                ));
                let remove = Button::new("Remove").variant(ButtonVariant::Destructive);
                if ui.add(remove).clicked()
                    && let Some(installed) = installed
                {
                    self.confirm = None;
                    self.order(Order::Remove {
                        id: id.clone(),
                        name: app.component.name.clone(),
                        installation: installed.installation.clone(),
                    });
                }
                if ui
                    .add(Button::new("Cancel").variant(ButtonVariant::Outline))
                    .clicked()
                {
                    self.confirm = None;
                }
            });
            return;
        }
        ui.horizontal_wrapped(|ui| match installed {
            None => {
                let label = match self.machine() {
                    Some(machine) => format!("Install in {machine}"),
                    None => "Install".to_owned(),
                };
                if ui.add(Button::new(label).enabled(idle)).clicked() {
                    self.order(Order::Install {
                        id: id.clone(),
                        name: app.component.name.clone(),
                    });
                }
            }
            Some(_) => {
                if ui.add(Button::new("Open").enabled(idle)).clicked() {
                    let desktop = app
                        .component
                        .launchable
                        .as_deref()
                        .map_or(id.as_str(), |l| l.trim_end_matches(".desktop"))
                        .to_owned();
                    self.order(Order::Open(desktop));
                }
                let outline = |label| Button::new(label).variant(ButtonVariant::Outline);
                if update && ui.add(outline("Update").enabled(idle)).clicked() {
                    self.order(Order::Update(Some((
                        id.clone(),
                        app.component.name.clone(),
                    ))));
                }
                if ui.add(outline("Remove").enabled(idle)).clicked() {
                    self.confirm = Some(id.clone());
                }
            }
        });
    }

    fn installed_page(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        let catalog = self.catalog.clone().unwrap_or_default();
        let title = match self.machine() {
            Some(machine) => format!("Installed in {machine}"),
            None => "Installed".to_owned(),
        };
        let count = match self.installed.len() {
            0 => String::new(),
            1 => "1 app from Flathub".to_owned(),
            n => format!("{n} apps from Flathub"),
        };
        self.header(ui, tokens, &title, &count);
        let mut open = None;
        derisk_frame::scroll(ui, "installed", |ui| {
            if !self.updates.is_empty() {
                self.updates_section(ui, tokens, &catalog, &mut open);
                derisk_frame::subheading(ui, tokens, "Your apps");
            }
            let mut installed: Vec<Installed> = self.installed.values().cloned().collect();
            let name = |id: &str| {
                catalog
                    .get(id)
                    .map_or(id.to_owned(), |a| a.component.name.clone())
            };
            installed.sort_by_cached_key(|i| name(&i.id).to_lowercase());
            derisk_frame::card(ui, tokens, |ui| {
                if installed.is_empty() {
                    if self.doing.is_some() {
                        derisk_frame::empty(ui, tokens, "Looking…", "");
                    } else {
                        derisk_frame::empty(
                            ui,
                            tokens,
                            "No apps from Flathub yet",
                            "Explore finds some.",
                        );
                    }
                }
                for item in &installed {
                    let app = catalog.get(&item.id).cloned();
                    ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::hover());
                        match &app {
                            Some(app) => self.paint_icon(ui, app, rect),
                            None => {
                                ui.painter().rect_filled(rect, 8.0, tokens.field);
                            }
                        }
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            if ui.link(RichText::new(name(&item.id)).strong()).clicked() {
                                open = Some(Page::App(item.id.clone()));
                            }
                            let version = if item.version.is_empty() {
                                item.branch.clone()
                            } else {
                                item.version.clone()
                            };
                            ui.label(derisk_frame::note(tokens, &version));
                        });
                    });
                }
            });
        });
        if let Some(page) = open {
            self.go(page);
        }
    }
}

impl UiApp for StoreApp {
    fn title(&self) -> &str {
        "Store"
    }

    fn ui(&mut self, ui: &mut egui::Ui, theme: &Theme) {
        self.receive(ui.ctx());
        let tokens = derisk_frame::begin(ui, theme);
        self.sidebar(ui, &tokens);
        self.status_bar(ui, &tokens);
        derisk_frame::Page::new()
            .wide()
            .fixed()
            .show(ui, &tokens, |ui| {
                if !self.query.trim().is_empty() {
                    self.results(ui, &tokens);
                } else {
                    match self.page.clone() {
                        Page::Explore => self.explore(ui, &tokens),
                        Page::Category(category, section) => {
                            self.category(ui, &tokens, category, section)
                        }
                        Page::App(id) => self.app_page(ui, &tokens, &id),
                        Page::Installed => self.installed_page(ui, &tokens),
                    }
                }
            });
        // The threads answer between frames; look again soon while
        // anything is owed, and now and then otherwise.
        let waiting = self.doing.is_some()
            || self.icons.values().any(|i| matches!(i, Icon::Asked))
            || self.said.values().any(|s| matches!(s, Said::Asked));
        let every = if waiting { 150 } else { 2000 };
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(every));
    }
}

/// An app's average and how many people gave each number of stars, as
/// bars in the accent colour.
fn ratings(ui: &mut egui::Ui, tokens: &Tokens, ratings: &Ratings) {
    let total = ratings.total();
    let Some(average) = ratings.average() else {
        return;
    };
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(RichText::new(format!("{average:.1}")).size(32.0).strong());
            ui.label(stars(average));
            let count = if total == 1 {
                "1 rating".to_owned()
            } else {
                format!("{total} ratings")
            };
            ui.label(derisk_frame::note(tokens, &count));
        });
        ui.add_space(16.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            for n in (1..=5).rev() {
                let count = ratings.stars[n - 1];
                ui.horizontal(|ui| {
                    ui.label(format!("{n} ★"));
                    let (rect, _) = ui.allocate_exact_size(Vec2::new(160.0, 8.0), Sense::hover());
                    let painter = ui.painter();
                    painter.rect_filled(rect, 4.0, tokens.field);
                    let mut filled = rect;
                    filled.set_width(rect.width() * count as f32 / total as f32);
                    painter.rect_filled(filled, 4.0, tokens.primary);
                    ui.label(derisk_frame::note(tokens, &count.to_string()));
                });
            }
        });
    });
}

/// One review: its stars and summary, who wrote it and when, and what they
/// wrote, under a rule from the one before.
fn review(ui: &mut egui::Ui, tokens: &Tokens, review: &Review, now: i64) {
    ui.separator();
    ui.horizontal_wrapped(|ui| {
        ui.label(stars(f32::from(review.stars)));
        if !review.summary.is_empty() {
            ui.strong(&review.summary);
        }
    });
    let mut by = vec![if review.author.is_empty() {
        "Someone".to_owned()
    } else {
        review.author.clone()
    }];
    by.push(ago(now, review.date));
    if !review.version.is_empty() {
        by.push(format!("version {}", review.version));
    }
    ui.label(derisk_frame::note(tokens, &by.join(" · ")));
    if !review.text.is_empty() {
        ui.add(egui::Label::new(&review.text).wrap());
    }
}

/// A description or release notes: paragraphs, and list items as bullets.
/// At most `blocks` of them; a feed entry shows the start only.
fn notes(ui: &mut egui::Ui, blocks: &[Block], at_most: usize) {
    for block in blocks.iter().take(at_most) {
        match block {
            Block::Paragraph(text) => {
                ui.add(egui::Label::new(text.as_str()).wrap().selectable(false));
            }
            Block::Item(text) => {
                ui.horizontal_top(|ui| {
                    ui.label("•");
                    ui.add(egui::Label::new(text.as_str()).wrap().selectable(false));
                });
            }
        }
    }
    if blocks.len() > at_most {
        ui.weak("…");
    }
}

/// A release on an app's page: its version and date over its notes.
fn release(ui: &mut egui::Ui, release: &crate::appstream::Release, at_most: usize) {
    let mut title = if release.version.is_empty() {
        "Release".to_owned()
    } else {
        format!("Version {}", release.version)
    };
    if let Some(timestamp) = release.timestamp {
        title.push_str(" · ");
        title.push_str(&date(timestamp));
    }
    ui.label(RichText::new(title).strong());
    if release.notes.is_empty() {
        ui.weak("No release notes.");
    } else {
        notes(ui, &release.notes, at_most);
    }
}

/// What an AppStream `<url type>` is called on an app's page, for the ones
/// worth a link.
fn link_name(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "homepage" => "Website",
        "bugtracker" => "Report a problem",
        "help" => "Help",
        "faq" => "Questions",
        "donation" => "Donate",
        "translate" => "Translate",
        "contribute" => "Contribute",
        "vcs-browser" => "Source code",
        "contact" => "Contact",
        _ => return None,
    })
}

/// Opens a link in the person's browser.
fn open_link(url: &str) {
    #[cfg(not(target_arch = "wasm32"))]
    if let Err(error) = std::process::Command::new("xdg-open")
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        tracing::warn!(%error, url, "cannot open a link");
    }
    #[cfg(target_arch = "wasm32")]
    let _ = url;
}

/// Seconds since 1970, now.
fn now() -> i64 {
    derisk_sys::time::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// How long ago `then` was, the way a person says it.
pub fn ago(now: i64, then: i64) -> String {
    let days = (now - then).div_euclid(86_400);
    match days {
        ..=0 => "today".into(),
        1 => "yesterday".into(),
        2..=13 => format!("{days} days ago"),
        14..=59 => format!("{} weeks ago", days / 7),
        _ => date(then),
    }
}

/// A day as `17 October 2026`, in UTC, which is what AppStream dates are.
pub fn date(timestamp: i64) -> String {
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    // Howard Hinnant's civil_from_days.
    let z = timestamp.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{day} {} {year}", MONTHS[(month - 1) as usize])
}
