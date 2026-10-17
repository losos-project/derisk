//! Reads a real Flathub catalogue when `FLATHUB_APPSTREAM` names one
//! (`dl.flathub.org/repo/appstream/x86_64/appstream.xml.gz`), to see how the
//! whole of Flathub sorts. `cargo test -p derisk-store --test flathub -- --ignored --nocapture`.

use std::time::Instant;

use derisk_store::{Catalog, Category, appstream};

#[test]
#[ignore = "needs FLATHUB_APPSTREAM"]
fn sorts_all_of_flathub() {
    let path = std::env::var("FLATHUB_APPSTREAM").expect("FLATHUB_APPSTREAM");
    let started = Instant::now();
    let file = std::fs::File::open(path).unwrap();
    let components = appstream::parse(file, &appstream::Language::new("en_GB.UTF-8")).unwrap();
    let read = started.elapsed();
    let catalog = Catalog::new(components);
    println!(
        "{} apps in {read:?} (+{:?} sorting)",
        catalog.apps().len(),
        started.elapsed() - read
    );
    for category in Category::ALL {
        let sections = catalog.sections(category, 3);
        let names: Vec<String> = sections
            .iter()
            .take(6)
            .map(|(s, n)| format!("{s} {n}"))
            .collect();
        println!(
            "{:24} {:5}  {}",
            category.name(),
            catalog.count(category),
            names.join(", ")
        );
    }
    let now = 1_792_222_000;
    for news in catalog.news(now, |_| true).iter().take(8) {
        println!(
            "{} {} {:?}: {}",
            news.app.component.name,
            news.release.version,
            news.release.timestamp,
            news.release
                .notes
                .first()
                .map(|b| b.text())
                .unwrap_or_default()
        );
    }
    for query in ["firefox", "photo editor", "chess"] {
        let hits: Vec<&str> = catalog
            .search(query)
            .iter()
            .take(5)
            .map(|a| a.component.name.as_str())
            .collect();
        println!("{query}: {hits:?}");
    }
    let guessed: Vec<String> = catalog
        .apps()
        .iter()
        .filter(|a| a.component.categories.is_empty())
        .take(12)
        .map(|a| format!("{} → {}", a.id(), a.categories[0].name()))
        .collect();
    println!("{guessed:#?}");
    assert!(catalog.apps().len() > 1000);
}

/// Asks the real ODRS for a well-known app's ratings and reviews.
#[test]
#[ignore = "asks odrs.gnome.org"]
fn odrs_answers() {
    let said = derisk_store::reviews::fetch("org.gnome.Maps", "en_US").unwrap();
    let ratings = said.ratings.expect("Maps has ratings");
    assert!(ratings.total() > 0);
    assert!(!said.reviews.is_empty());
    eprintln!(
        "{:.1} from {} ratings; first review: {:?}",
        ratings.average().unwrap(),
        ratings.total(),
        said.reviews[0].summary
    );
}
