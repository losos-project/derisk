//! The store's threads: everything that runs flatpak or reads its files.
//!
//! The store is drawn on the compositor's thread, between every other
//! window's frames, so nothing it does there may wait. Reading Flathub's
//! catalogue takes a second or two, an install minutes, and in the flatpak
//! machine each first has to boot it. So the app hands orders to these
//! threads and reads their events each frame: one thread carries out the
//! orders, one at a time, another loads icons, so a long install never
//! holds up the pictures of the page being scrolled, and a third asks for
//! ratings and reviews, which come from the network and not from flatpak.

use std::sync::Arc;

use mcsapi_ui::egui::ColorImage;

use crate::{catalog::Catalog, flatpak::Installed, reviews::Reviews};

#[cfg(target_arch = "wasm32")]
pub(crate) use stub::Worker;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use threads::Worker;

/// What the app asks for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Order {
    /// Fetch Flathub's catalogue anew, then load.
    Refresh,
    /// Install an app from Flathub: its ID, and its name to tell the person.
    Install { id: String, name: String },
    /// Remove an installed app from its installation.
    Remove {
        id: String,
        name: String,
        installation: String,
    },
    /// Update one app (its ID and name), or with `None` every app with an
    /// update.
    Update(Option<(String, String)>),
    /// Open an installed app through its desktop file ID.
    Open(String),
}

/// What the threads tell the app. A browser's build has no threads, and
/// never makes one.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) enum Event {
    /// Where flatpak runs: the machine's name, or `None` for this computer.
    Place(Option<String>),
    /// What the order thread is doing, while it does.
    Doing(String),
    /// The catalogue, read.
    Catalog(Result<Arc<Catalog>, String>),
    /// The installed apps.
    Installed(Vec<Installed>),
    /// The apps with an update waiting, or why they could not be asked for.
    Updates(Result<Vec<String>, String>),
    /// A line flatpak printed, and how far along it says it is.
    Line { line: String, progress: Option<f32> },
    /// An order finished: what to tell the person, or what went wrong.
    Done(Result<String, String>),
    /// The first load finished, and the thread waits for orders.
    Idle,
    /// An icon, decoded, or `None` when it could not be.
    Icon {
        file: String,
        image: Option<ColorImage>,
    },
    /// An app's ratings and reviews, or why they could not be had.
    Reviews {
        id: String,
        reviews: Result<Reviews, String>,
    },
}

/// A browser has no flatpak and no threads to run it on: nothing is ever
/// done, and the app says so.
#[cfg(target_arch = "wasm32")]
mod stub {
    use super::{Event, Order};

    pub(crate) struct Worker;

    impl Worker {
        pub fn start() -> Worker {
            Worker
        }

        pub fn send(&self, _: Order) -> bool {
            false
        }

        pub fn icon(&self, _: &str, _: u32) -> bool {
            false
        }

        pub fn reviews(&self, _: &str) -> bool {
            false
        }

        pub fn events(&self) -> impl Iterator<Item = Event> + '_ {
            std::iter::empty()
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod threads {
    use std::{
        collections::HashSet,
        sync::{Arc, Mutex},
    };

    use crossbeam_channel::{Receiver, Sender, bounded};
    use mcsapi_ui::egui::ColorImage;

    use super::{Event, Order};
    use crate::{
        appstream::{self, Language},
        catalog::Catalog,
        flatpak::{self, Flatpak, LIST, REMOTE, UPDATES},
        reviews,
    };

    /// The app's end of the threads. Dropping it ends them at their next look
    /// at the channels.
    pub(crate) struct Worker {
        orders: Sender<Order>,
        icons: Sender<(String, u32)>,
        reviews: Sender<String>,
        events: Receiver<Event>,
    }

    impl Worker {
        /// Starts the threads on flatpak where [`place`] finds it.
        pub fn start() -> Worker {
            Self::start_with(place, reviews::fetch)
        }

        /// Starts the threads on the flatpak `place` gives, called on the
        /// threads since finding a machine reads the person's machines, with reviews from
        /// `fetch`.
        pub fn start_with(
            place: impl FnOnce() -> Arc<dyn Flatpak> + Send + 'static,
            fetch: reviews::Fetch,
        ) -> Worker {
            let (orders, orders_rx) = bounded(16);
            let (icons, icons_rx) = bounded::<(String, u32)>(256);
            let (reviews, reviews_rx) = bounded::<String>(16);
            let (events_tx, events) = bounded(512);
            let spawned = std::thread::Builder::new()
                .name("derisk-store".into())
                .spawn(move || {
                    let flatpak = place();
                    if events_tx
                        .send(Event::Place(flatpak.machine().map(str::to_owned)))
                        .is_err()
                    {
                        return;
                    }
                    let installation: Found = Mutex::new(None);
                    std::thread::scope(|scope| {
                        let (flatpak, events, installation) = (&flatpak, &events_tx, &installation);
                        scope.spawn(move || {
                            icon_loop(flatpak.as_ref(), &icons_rx, events, installation)
                        });
                        scope.spawn(move || reviews_loop(&reviews_rx, events, fetch));
                        order_loop(flatpak.as_ref(), &orders_rx, events, installation);
                    });
                });
            if let Err(error) = spawned {
                tracing::error!(%error, "cannot start the store's thread");
            }
            Worker {
                orders,
                icons,
                reviews,
                events,
            }
        }

        /// Hands `order` over; false when the threads have gone or are full.
        pub fn send(&self, order: Order) -> bool {
            self.orders.try_send(order).is_ok()
        }

        /// Asks for the icon `file`, cached at `size`; false when the queue is
        /// full, and the app should ask again on a later frame.
        pub fn icon(&self, file: &str, size: u32) -> bool {
            self.icons.try_send((file.to_owned(), size)).is_ok()
        }

        /// Asks for app `id`'s ratings and reviews; false when the queue is
        /// full, and the app should ask again on a later frame.
        pub fn reviews(&self, id: &str) -> bool {
            self.reviews.try_send(id.to_owned()).is_ok()
        }

        /// Whatever the threads have said since the last call.
        pub fn events(&self) -> impl Iterator<Item = Event> + '_ {
            self.events.try_iter()
        }
    }

    /// Fetches the ratings and reviews asked for, one app at a time.
    fn reviews_loop(ids: &Receiver<String>, events: &Sender<Event>, fetch: reviews::Fetch) {
        let locale = reviews::locale(&appstream::session_locale());
        while let Ok(id) = ids.recv() {
            let reviews = fetch(&id, &locale);
            if let Err(error) = &reviews {
                tracing::warn!(%id, %error, "cannot read the app's reviews");
            }
            if events.send(Event::Reviews { id, reviews }).is_err() {
                return;
            }
        }
    }

    /// Where the store finds flatpak: in the Flatpak machine when the person
    /// has one, otherwise on this computer.
    pub fn place() -> Arc<dyn Flatpak> {
        match flatpak::Machine::find() {
            Some(machine) => Arc::new(machine),
            None => Arc::new(flatpak::Local),
        }
    }

    /// The installation the catalogue was found in, which its icons sit beside.
    type Found = Mutex<Option<String>>;

    fn order_loop(
        flatpak: &dyn Flatpak,
        orders: &Receiver<Order>,
        events: &Sender<Event>,
        found: &Found,
    ) {
        let language = Language::from_env();
        if load(flatpak, events, &language, found, true).is_err()
            || events.send(Event::Idle).is_err()
        {
            return;
        }
        while let Ok(order) = orders.recv() {
            // Only a refresh changes the catalogue; anything else changes
            // what is installed, and an open changes nothing.
            let (done, reread) = match order {
                Order::Refresh => (
                    refresh(flatpak, events).map(|()| "Flathub's catalogue is up to date".into()),
                    Some(true),
                ),
                Order::Install { id, name } => (install(flatpak, events, &id, &name), Some(false)),
                Order::Remove {
                    id,
                    name,
                    installation,
                } => {
                    let _ = events.send(Event::Doing(format!("Removing {name}")));
                    let flag = format!("--{installation}");
                    let args = ["uninstall", &flag, "--noninteractive", "-y", &id];
                    (
                        run(flatpak, events, &args).map(|_| format!("Removed {name}")),
                        Some(false),
                    )
                }
                Order::Update(app) => {
                    let _ = events.send(Event::Doing(match &app {
                        Some((_, name)) => format!("Updating {name}"),
                        None => "Updating every app".into(),
                    }));
                    let mut args = vec!["update", "--noninteractive", "-y"];
                    args.extend(app.as_ref().map(|(id, _)| id.as_str()));
                    let done = run(flatpak, events, &args).map(|_| match &app {
                        Some((_, name)) => format!("Updated {name}"),
                        None => "Every app is up to date".into(),
                    });
                    (done, Some(false))
                }
                Order::Open(id) => (flatpak.open(&id).map(|()| String::new()), None),
            };
            // Read again before saying it is done, so the page never shows
            // the old state with the buttons back on.
            if let Some(catalogue) = reread
                && load(flatpak, events, &language, found, catalogue).is_err()
            {
                return;
            }
            if events.send(Event::Done(done)).is_err() {
                return;
            }
        }
    }

    /// Runs flatpak, passing its lines on as events.
    fn run(flatpak: &dyn Flatpak, events: &Sender<Event>, args: &[&str]) -> Result<String, String> {
        flatpak.run(args, &mut |line| {
            let _ = events.try_send(Event::Line {
                line: line.to_owned(),
                progress: flatpak::progress(line),
            });
        })
    }

    /// Installs `id` for the whole system, which the OS lets its
    /// administrators do without a password; failing that on this computer,
    /// for this person alone, from the app's own flatpakref, which needs no
    /// remote set up.
    fn install(
        flatpak: &dyn Flatpak,
        events: &Sender<Event>,
        id: &str,
        name: &str,
    ) -> Result<String, String> {
        let _ = events.send(Event::Doing(format!("Installing {name}")));
        let system = run(
            flatpak,
            events,
            &["install", "--system", "--noninteractive", "-y", REMOTE, id],
        );
        match system {
            Ok(_) => Ok(format!("Installed {name}")),
            Err(error) if flatpak.machine().is_some() => Err(error),
            Err(_) => {
                let flatpakref = format!("https://dl.flathub.org/repo/appstream/{id}.flatpakref");
                run(
                    flatpak,
                    events,
                    &["install", "--user", "--noninteractive", "-y", &flatpakref],
                )
                .map(|_| format!("Installed {name} for you"))
            }
        }
    }

    /// Fetches Flathub's catalogue, which any user may do without a password.
    fn refresh(flatpak: &dyn Flatpak, events: &Sender<Event>) -> Result<(), String> {
        let _ = events.send(Event::Doing("Fetching Flathub's catalogue".into()));
        run(flatpak, events, &["update", "--appstream", REMOTE]).map(drop)
    }

    /// Reads the catalogue when `catalogue` says to (fetching it first when
    /// there is none yet), then the installed apps, then the updates,
    /// sending each as it is known. Err only when the app has gone.
    fn load(
        flatpak: &dyn Flatpak,
        events: &Sender<Event>,
        language: &Language,
        found: &Found,
        catalogue: bool,
    ) -> Result<(), ()> {
        let send = |event| events.send(event).map_err(drop);
        if catalogue {
            read(flatpak, events, language, found)?;
        }
        let installed = flatpak
            .run(&LIST, &mut |_| {})
            .map(|out| flatpak::parse_installed(&out).collect())
            .unwrap_or_default();
        send(Event::Installed(installed))?;
        // Asking for updates needs the network; the page is usable before.
        let updates = flatpak.run(&UPDATES, &mut |_| {}).map(|out| {
            let mut ids: Vec<String> = out
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect();
            ids.dedup();
            ids
        });
        send(Event::Updates(updates))
    }

    /// Reads the catalogue, fetching it first when there is none yet, and
    /// sends it.
    fn read(
        flatpak: &dyn Flatpak,
        events: &Sender<Event>,
        language: &Language,
        found: &Found,
    ) -> Result<(), ()> {
        let send = |event| events.send(event).map_err(drop);
        send(Event::Doing("Reading Flathub's catalogue".into()))?;
        let mut catalogue = read_catalogue(flatpak);
        if catalogue.is_err() {
            // A system that never fetched it: a fresh machine, a first boot
            // without the network.
            if let Err(error) = refresh(flatpak, events) {
                catalogue = Err(error);
            } else {
                catalogue = read_catalogue(flatpak);
            }
        }
        let catalog = catalogue.and_then(|(installation, bytes)| {
            let components =
                appstream::parse(bytes.as_slice(), language).map_err(|e| e.to_string())?;
            if let Ok(mut found) = found.lock() {
                *found = Some(installation);
            }
            Ok(Arc::new(Catalog::new(components)))
        });
        send(Event::Catalog(catalog))
    }

    /// The catalogue's bytes and the installation they are in: the first
    /// installation that has one.
    fn read_catalogue(flatpak: &dyn Flatpak) -> Result<(String, Vec<u8>), String> {
        let mut last = String::from("No installation has Flathub's catalogue");
        for installation in flatpak.installations() {
            for path in flatpak::catalogue_paths(&installation, REMOTE) {
                match flatpak.read(&path) {
                    Ok(bytes) => return Ok((installation, bytes)),
                    Err(error) => last = error,
                }
            }
        }
        Err(last)
    }

    /// The biggest icon edge the store draws, in pixels.
    const ICON: u32 = 128;

    fn icon_loop(
        flatpak: &dyn Flatpak,
        icons: &Receiver<(String, u32)>,
        events: &Sender<Event>,
        found: &Found,
    ) {
        // Each file is decoded once; the app keeps the textures.
        let mut seen = HashSet::new();
        while let Ok((file, size)) = icons.recv() {
            if !seen.insert(file.clone()) {
                continue;
            }
            let installation = found.lock().ok().and_then(|f| f.clone());
            let image = installation.and_then(|installation| {
                let path = flatpak::icon_path(&installation, REMOTE, &file, size);
                decode(&flatpak.read(&path).ok()?)
            });
            if events.send(Event::Icon { file, image }).is_err() {
                return;
            }
        }
    }

    /// A PNG icon, shrunk to at most [`ICON`] pixels square.
    fn decode(bytes: &[u8]) -> Option<ColorImage> {
        let mut image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
            .ok()?
            .into_rgba8();
        if image.width() > ICON || image.height() > ICON {
            image = image::imageops::thumbnail(&image, ICON, ICON);
        }
        Some(ColorImage::from_rgba_unmultiplied(
            [image.width() as usize, image.height() as usize],
            image.as_raw(),
        ))
    }
}
