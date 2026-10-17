use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use derisk_store::{
    Catalog, Category, StoreApp,
    appstream::{self, Block, Kind, Language},
    category::{self, section_name},
    flatpak::{self, Flatpak},
    reviews::{Ratings, Review, Reviews},
};
use mcsapi_ui::{App as _, Theme, egui, run_frame};

const FIXTURE: &str = include_str!("data/appstream.xml");

fn components(locale: &str) -> Vec<appstream::Component> {
    appstream::parse(FIXTURE.as_bytes(), &Language::new(locale)).unwrap()
}

fn catalog() -> Catalog {
    Catalog::new(components("C"))
}

#[test]
fn reads_what_the_store_shows_and_nothing_else() {
    let all = components("en_US.UTF-8");
    let paint = &all[0];
    assert_eq!(paint.id, "org.example.Paint");
    assert_eq!(paint.kind, Kind::Desktop);
    assert_eq!(paint.name, "Paint");
    assert_eq!(paint.summary, "Draw & paint pictures");
    // <developer><name> wins over the older developer_name.
    assert_eq!(paint.developer, "Example Studio");
    assert_eq!(paint.license, "GPL-3.0-or-later");
    assert_eq!(
        paint.description,
        [
            Block::Paragraph("Paint is a simple painting program.".into()),
            Block::Item("Brushes".into()),
            Block::Item("Layers — many".into()),
        ]
    );
    assert_eq!(paint.categories, ["Graphics", "RasterGraphics", "GTK"]);
    assert_eq!(paint.keywords, ["drawing", "sketch"]);
    // The 64 px cached icon, not the HiDPI copy, the stock name or the URL.
    assert_eq!(paint.icon, Some(("org.example.Paint.png".into(), 64)));
    assert_eq!(
        paint.launchable.as_deref(),
        Some("org.example.Paint.desktop")
    );
    assert_eq!(
        paint.bundle.as_deref(),
        Some("app/org.example.Paint/x86_64/stable")
    );
    assert!(paint.verified);
    assert_eq!(
        paint.urls,
        [
            (
                "homepage".to_owned(),
                "https://example.org/paint".to_owned()
            ),
            (
                "bugtracker".to_owned(),
                "https://example.org/paint/issues".to_owned()
            ),
        ]
    );
    let versions: Vec<&str> = paint.releases.iter().map(|r| r.version.as_str()).collect();
    assert_eq!(versions, ["2.0", "1.9", "1.0"]);
    assert_eq!(paint.releases[0].timestamp, Some(1_790_000_000));
    assert_eq!(
        paint.releases[0].notes,
        [Block::Paragraph("Layers.".into())]
    );
    // An empty <release/> is a release, from its date.
    assert!(paint.releases[1].development);
    assert_eq!(
        paint.releases[1].timestamp,
        appstream::parse_date("2026-01-02")
    );
    assert_eq!(paint.releases[2].notes, [Block::Paragraph("First.".into())]);
    assert_eq!(all.len(), 8);
    assert_eq!(all[4].kind, Kind::Console);
    assert_eq!(all[6].kind, Kind::Other);
}

#[test]
fn picks_the_persons_language_and_falls_back_to_untranslated() {
    let german = &components("de_DE.UTF-8")[0];
    assert_eq!(german.name, "Malen");
    assert_eq!(german.summary, "Bilder malen");
    assert_eq!(
        german.description,
        [Block::Paragraph("Malen ist ein Malprogramm.".into())]
    );
    assert_eq!(german.keywords, ["zeichnen"]);
    assert_eq!(
        german.releases[0].notes,
        [Block::Paragraph("Ebenen.".into())]
    );
    // Untranslated where there is nothing in the language.
    assert_eq!(
        german.releases[2].notes,
        [Block::Paragraph("First.".into())]
    );
    // The exact locale beats the language alone; an unknown one falls back.
    assert_eq!(components("pt_BR")[0].name, "Pintar");
    assert_eq!(components("pt_PT")[0].name, "Paint");
    assert_eq!(components("fr")[0].name, "Paint");
}

#[test]
fn reads_a_gzipped_catalogue() {
    use std::io::Write;
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(FIXTURE.as_bytes()).unwrap();
    let bytes = gz.finish().unwrap();
    let all = appstream::parse(bytes.as_slice(), &Language::default()).unwrap();
    assert_eq!(all.len(), 8);
}

#[test]
fn dates_are_days_since_1970() {
    assert_eq!(appstream::parse_date("1970-01-01"), Some(0));
    assert_eq!(appstream::parse_date("2026-10-17"), Some(1_792_195_200));
    assert_eq!(
        appstream::parse_date("2024-02-29T10:00:00Z"),
        Some(1_709_164_800)
    );
    assert_eq!(appstream::parse_date("2024-13-01"), None);
    assert_eq!(appstream::parse_date("soon"), None);
}

#[test]
fn keeps_apps_once_with_their_stable_branch() {
    let catalog = catalog();
    let ids: Vec<&str> = catalog.apps().iter().map(|a| a.id()).collect();
    // No runtime, no add-on, Blaster once.
    assert_eq!(
        ids,
        [
            "org.example.Paint",
            "org.example.Blaster",
            "org.example.Chess",
            "org.example.Thing",
            "org.example.Tunes",
        ]
    );
    assert_eq!(
        catalog.get("org.example.Blaster").unwrap().component.name,
        "Blaster"
    );
}

#[test]
fn sorts_apps_into_categories_from_all_they_say() {
    let catalog = catalog();
    let at = |id: &str| catalog.get(id).unwrap().categories.clone();
    assert_eq!(at("org.example.Paint"), [Category::Graphics]);
    // A game that also says Utility is a game first, and a utility too.
    assert_eq!(
        at("org.example.Blaster"),
        [Category::Games, Category::Utilities]
    );
    // Only an additional category: the main one it belongs under.
    assert_eq!(at("org.example.Chess"), [Category::Games]);
    // No categories: its keywords, name and summary decide.
    assert_eq!(at("org.example.Tunes"), [Category::AudioVideo]);
    // A terminal program that says nothing of itself.
    assert_eq!(at("org.example.Thing"), [Category::Development]);

    let games: Vec<&str> = catalog
        .in_category(Category::Games, None)
        .map(|a| a.id())
        .collect();
    assert_eq!(games, ["org.example.Blaster", "org.example.Chess"]);
    let arcade: Vec<&str> = catalog
        .in_category(Category::Games, Some("ArcadeGame"))
        .map(|a| a.id())
        .collect();
    assert_eq!(arcade, ["org.example.Blaster"]);
    assert_eq!(
        catalog.sections(Category::Games, 1),
        [("ArcadeGame".to_owned(), 1), ("BoardGame".to_owned(), 1)]
    );
    assert!(catalog.sections(Category::Games, 2).is_empty());
    assert_eq!(catalog.count(Category::Science), 0);
    let paint = &catalog.get("org.example.Paint").unwrap().component;
    let sections: Vec<&str> = category::sections(paint, Category::Graphics).collect();
    assert_eq!(sections, ["RasterGraphics"]);
}

#[test]
fn names_sections_like_a_person_would() {
    assert_eq!(section_name("ArcadeGame"), "Arcade");
    assert_eq!(section_name("2DGraphics"), "2D Graphics");
    assert_eq!(section_name("RasterGraphics"), "Raster Graphics");
    assert_eq!(section_name("WebBrowser"), "Web Browsers");
    assert_eq!(section_name("IDE"), "IDEs");
    assert_eq!(section_name("Game"), "Game");
}

#[test]
fn searches_names_first() {
    let catalog = catalog();
    let names = |q: &str| {
        catalog
            .search(q)
            .into_iter()
            .map(|a| a.component.name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(names("paint"), ["Paint"]);
    assert_eq!(names("SKETCH"), ["Paint"]);
    assert_eq!(names("rocks"), ["Blaster"]);
    // Every word must be found somewhere.
    assert_eq!(names("paint rocks"), Vec::<String>::new());
    assert!(names("   ").is_empty());
}

#[test]
fn the_feed_is_the_newest_release_notes_first() {
    let catalog = catalog();
    let now = 1_792_222_000;
    let feed: Vec<(&str, &str)> = catalog
        .news(now, |_| true)
        .iter()
        .map(|n| (n.app.id(), n.release.version.as_str()))
        .collect();
    // Blaster's release from the future waits; Chess's has no notes.
    assert_eq!(
        feed,
        [("org.example.Blaster", "0.3"), ("org.example.Paint", "2.0")]
    );
    let mine = catalog.news(now, |a| a.id() == "org.example.Paint");
    assert_eq!(mine.len(), 1);
}

#[test]
fn reads_flatpaks_answers() {
    let list = "org.example.Paint\t2.0\tstable\tflathub\tsystem\n\
                org.example.Chess\t\tstable\tflathub\tuser\n\n";
    let installed: Vec<_> = flatpak::parse_installed(list).collect();
    assert_eq!(installed.len(), 2);
    assert_eq!(installed[0].version, "2.0");
    assert_eq!(installed[1].installation, "user");
    assert_eq!(
        flatpak::progress("Installing 1/2… ████▍ 45%  1.2 MB/s"),
        Some(0.45)
    );
    assert_eq!(flatpak::progress("Installing 1/2…"), None);
    assert_eq!(
        flatpak::failure(
            "Looking for matches…\nerror: No remote refs found for ‘x’\n",
            ""
        ),
        "No remote refs found for ‘x’"
    );
    assert_eq!(flatpak::failure("", ""), "flatpak failed");
    let [gz, xml] = flatpak::catalogue_paths("/var/lib/flatpak", "flathub");
    assert!(
        gz.starts_with("/var/lib/flatpak/appstream/flathub/"),
        "{gz}"
    );
    assert!(gz.ends_with("/active/appstream.xml.gz"), "{gz}");
    assert!(xml.ends_with("/active/appstream.xml"), "{xml}");
}

/// flatpak as a test wants it: the fixture for a catalogue, a list of what
/// is installed, and a record of what it was asked to run.
#[derive(Default)]
struct Fake {
    ran: Mutex<Vec<String>>,
    installed: Mutex<String>,
}

impl Flatpak for Fake {
    fn machine(&self) -> Option<&str> {
        Some("flatpak")
    }

    fn run(&self, args: &[&str], line: &mut dyn FnMut(&str)) -> Result<String, String> {
        self.ran.lock().unwrap().push(args.join(" "));
        match args {
            ["list", ..] => Ok(self.installed.lock().unwrap().clone()),
            ["remote-ls", ..] => Ok("org.example.Paint\n".into()),
            ["install", .., id] => {
                line("Installing 1/1… 50%");
                *self.installed.lock().unwrap() += &format!("{id}\t1\tstable\tflathub\tsystem\n");
                Ok(String::new())
            }
            _ => Ok(String::new()),
        }
    }

    fn read(&self, path: &str) -> Result<Vec<u8>, String> {
        if path.ends_with("appstream.xml.gz") {
            Ok(FIXTURE.as_bytes().to_vec())
        } else {
            Err(format!("{path}: not here"))
        }
    }

    fn open(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
}

/// Reviews as ODRS would give them, without asking it.
fn canned(id: &str, locale: &str) -> Result<Reviews, String> {
    assert!(!locale.is_empty());
    if id == "org.example.Missing" {
        return Err("not found".into());
    }
    Ok(Reviews {
        ratings: Some(Ratings {
            stars: [1, 0, 2, 5, 9],
        }),
        reviews: vec![Review {
            summary: "Plays well".into(),
            text: "Fast, and the pieces are easy to tell apart.".into(),
            stars: 5,
            author: "Sam".into(),
            date: 1_790_000_000,
            version: "1.2".into(),
        }],
    })
}

fn frame(store: &mut StoreApp, context: &egui::Context) {
    let mut output = run_frame(store, context, egui::RawInput::default(), &Theme::default());
    output.textures_delta.clear();
}

fn settle(store: &mut StoreApp, context: &egui::Context) {
    let deadline = Instant::now() + Duration::from_secs(10);
    frame(store, context);
    while store.busy() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        frame(store, context);
    }
    assert!(!store.busy(), "the store never finished");
}

#[test]
fn loads_the_catalogue_and_installs_in_the_machine() {
    let fake = Arc::new(Fake::default());
    let mut store = StoreApp::with_reviews(fake.clone(), canned);
    let context = egui::Context::default();
    settle(&mut store, &context);
    assert_eq!(store.catalog().unwrap().apps().len(), 5);
    assert_eq!(store.title(), "Store");
    {
        let ran = fake.ran.lock().unwrap();
        assert_eq!(
            ran[0],
            "list --app --columns=application,version,branch,origin,installation"
        );
        assert_eq!(ran[1], "remote-ls --updates --app --columns=application");
    }

    // Every page draws, with and without a catalogue entry.
    store.show_category(Category::Games);
    frame(&mut store, &context);
    store.show_installed();
    frame(&mut store, &context);
    store.search("rocks");
    frame(&mut store, &context);
    store.show_app("org.example.Missing");
    frame(&mut store, &context);
    store.show_app("org.example.Chess");
    frame(&mut store, &context);

    // Install, as the app page's button does: the machine's system
    // installation, from Flathub, and the list read again after.
    store.install("org.example.Chess");
    settle(&mut store, &context);
    {
        let ran = fake.ran.lock().unwrap();
        assert!(
            ran.iter()
                .any(|r| r == "install --system --noninteractive -y flathub org.example.Chess"),
            "{ran:?}"
        );
        assert_eq!(
            ran.iter().filter(|r| r.starts_with("list")).count(),
            2,
            "{ran:?}"
        );
    }
    store.show_app("org.example.Chess");
    frame(&mut store, &context);
}

#[test]
fn the_browser_demo_and_the_catalog_draw_a_frame() {
    // A store whose flatpak finds nothing still draws, and says why.
    #[derive(Default)]
    struct Nothing;
    impl Flatpak for Nothing {
        fn machine(&self) -> Option<&str> {
            None
        }
        fn run(&self, _: &[&str], _: &mut dyn FnMut(&str)) -> Result<String, String> {
            Err("no network".into())
        }
        fn read(&self, path: &str) -> Result<Vec<u8>, String> {
            Err(format!("{path}: missing"))
        }
        fn open(&self, _: &str) -> Result<(), String> {
            Err("no".into())
        }
    }
    let mut store = StoreApp::with_reviews(Arc::new(Nothing), canned);
    let context = egui::Context::default();
    settle(&mut store, &context);
    assert!(store.catalog().is_none());
}

#[test]
fn a_job_in_the_machine_reports_its_end() {
    use derisk_store::flatpak::parse_progress;
    assert_eq!(
        parse_progress("running\nInstalling 1/2… 45%\n"),
        (None, "Installing 1/2… 45%\n")
    );
    assert_eq!(parse_progress("running\n"), (None, ""));
    assert_eq!(
        parse_progress("done 0\nInstalling…\nInstalled\n"),
        (Some(0), "Installing…\nInstalled\n")
    );
    assert_eq!(
        parse_progress("done 1\nerror: No remote refs found\n"),
        (Some(1), "error: No remote refs found\n")
    );
    assert_eq!(parse_progress("done ?\n"), (Some(1), ""));
}

#[test]
fn reads_what_odrs_answers() {
    use derisk_store::reviews::{fetch_body, locale, parse_ratings, parse_reviews};
    let ratings = parse_ratings(
        r#"{"star0": 0, "star1": 37, "star2": 41, "star3": 29, "star4": 30, "star5": 54, "total": 191}"#,
    )
    .unwrap()
    .unwrap();
    assert_eq!(ratings.stars, [37, 41, 29, 30, 54]);
    assert_eq!(ratings.total(), 191);
    let average = ratings.average().unwrap();
    assert!((average - 596.0 / 191.0).abs() < 1e-4, "{average}");
    assert_eq!(parse_ratings(r#"{"star0": 3, "total": 3}"#).unwrap(), None);
    assert!(parse_ratings("<html>").is_err());

    let reviews = parse_reviews(
        r#"[
            {"summary": " Does not load any maps ", "description": "Never loads.", "rating": 40,
             "user_display": "Daniel", "date_created": 1791042655.0, "version": "51.1", "reported": 0},
            {"summary": "spam", "description": "spam", "rating": 100, "reported": 2},
            {"summary": "", "description": "", "rating": 60},
            {"description": "Just text", "rating": 0, "user_display": null}
        ]"#,
    )
    .unwrap();
    assert_eq!(reviews.len(), 2);
    assert_eq!(reviews[0].summary, "Does not load any maps");
    assert_eq!(reviews[0].stars, 2);
    assert_eq!(reviews[0].author, "Daniel");
    assert_eq!(reviews[0].date, 1_791_042_655);
    assert_eq!(reviews[1].stars, 1);
    assert_eq!(reviews[1].author, "");

    assert_eq!(locale("de_DE.UTF-8"), "de_DE");
    assert_eq!(locale("C.UTF-8"), "en_US");
    assert_eq!(locale(""), "en_US");
    let body: serde_json::Value =
        serde_json::from_str(&fetch_body("org.gnome.Maps", "en_US")).unwrap();
    assert_eq!(body["app_id"], "org.gnome.Maps");
    assert_eq!(body["user_hash"].as_str().unwrap(), "0".repeat(40));
    assert_eq!(derisk_store::reviews::stars(3.6), "★★★★☆");
}
