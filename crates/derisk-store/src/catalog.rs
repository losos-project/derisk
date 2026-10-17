//! The apps the store offers, sorted into categories, with the release feed
//! the front page shows and the search the search field runs.

use std::collections::{BTreeMap, HashMap};

use crate::{
    appstream::{Component, Kind, Release},
    category::{self, Category},
};

/// An app in the store: its AppStream data and where it is listed.
#[derive(Clone, Debug)]
pub struct App {
    /// Its AppStream data.
    pub component: Component,
    /// The categories it is listed in, the first the one its card names.
    pub categories: Vec<Category>,
    /// Lower-case name, summary, keywords and ID, for search.
    haystack: String,
}

impl App {
    /// From its AppStream data.
    pub fn new(component: Component) -> Self {
        let categories = category::categorise(&component);
        let haystack = [
            component.name.as_str(),
            &component.summary,
            &component.keywords.join(" "),
            &component.id,
            &component.developer,
        ]
        .join("\n")
        .to_lowercase();
        Self {
            component,
            categories,
            haystack,
        }
    }

    /// Its application ID.
    pub fn id(&self) -> &str {
        &self.component.id
    }

    /// Its newest release, the one AppStream lists first.
    pub fn latest(&self) -> Option<&Release> {
        self.component.releases.first()
    }

    /// How well it matches `query` (lower case), higher being better, or
    /// `None` when it does not. Every word of the query must be found.
    pub fn score(&self, query: &str) -> Option<u32> {
        let name = self.component.name.to_lowercase();
        let mut score = 0;
        for word in query.split_whitespace() {
            score += if name == word {
                40
            } else if name.starts_with(word) {
                30
            } else if name.contains(word) {
                20
            } else if self.component.keywords.iter().any(|k| k.contains(word)) {
                10
            } else if self.haystack.contains(word) {
                5
            } else {
                return None;
            };
        }
        // A verified developer breaks a tie between two clones.
        Some(score * 2 + u32::from(self.component.verified))
    }
}

/// One entry of the release feed: an app's release with its notes.
#[derive(Clone, Copy, Debug)]
pub struct News<'a> {
    /// The app.
    pub app: &'a App,
    /// Its release.
    pub release: &'a Release,
}

/// Flathub's apps, by ID and by category.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    apps: Vec<App>,
    by_id: HashMap<String, usize>,
    by_category: BTreeMap<Category, Vec<usize>>,
}

impl Catalog {
    /// The apps among `components`: everything a person opens, in a window
    /// or a terminal, that has a Flatpak to install. A component listed
    /// twice (once per branch) is kept once, its stable branch if it has one.
    pub fn new(components: impl IntoIterator<Item = Component>) -> Self {
        let mut catalog = Self::default();
        for component in components {
            if component.kind == Kind::Other
                || !component
                    .bundle
                    .as_deref()
                    .is_some_and(|b| b.starts_with("app/"))
            {
                continue;
            }
            let stable =
                |c: &Component| c.bundle.as_deref().is_some_and(|b| b.ends_with("/stable"));
            match catalog.by_id.get(&component.id) {
                Some(&at) => {
                    if !stable(&catalog.apps[at].component) && stable(&component) {
                        catalog.apps[at] = App::new(component);
                    }
                }
                None => {
                    catalog
                        .by_id
                        .insert(component.id.clone(), catalog.apps.len());
                    catalog.apps.push(App::new(component));
                }
            }
        }
        // Each category lists its apps by name, as a person scans them.
        let mut order: Vec<usize> = (0..catalog.apps.len()).collect();
        order.sort_by_cached_key(|&i| catalog.apps[i].component.name.to_lowercase());
        for i in order {
            for &category in &catalog.apps[i].categories {
                catalog.by_category.entry(category).or_default().push(i);
            }
        }
        catalog
    }

    /// Every app, in catalogue order.
    pub fn apps(&self) -> &[App] {
        &self.apps
    }

    /// The app with ID `id`.
    pub fn get(&self, id: &str) -> Option<&App> {
        self.by_id.get(id).map(|&i| &self.apps[i])
    }

    /// How many apps `category` lists.
    pub fn count(&self, category: Category) -> usize {
        self.by_category.get(&category).map_or(0, Vec::len)
    }

    /// The apps of `category`, by name; of one of its sections only when
    /// `section` names one.
    pub fn in_category<'a>(
        &'a self,
        category: Category,
        section: Option<&'a str>,
    ) -> impl Iterator<Item = &'a App> + 'a {
        self.by_category
            .get(&category)
            .into_iter()
            .flatten()
            .map(|&i| &self.apps[i])
            .filter(move |app| {
                section.is_none_or(|s| app.component.categories.iter().any(|c| c == s))
            })
    }

    /// The sections of `category` with how many apps each has, the largest
    /// first. Sections with fewer than `at_least` apps are left out, so the
    /// row of them stays short.
    pub fn sections(&self, category: Category, at_least: usize) -> Vec<(String, usize)> {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for app in self.in_category(category, None) {
            for section in category::sections(&app.component, category) {
                *counts.entry(section).or_default() += 1;
            }
        }
        let mut sections: Vec<(String, usize)> = counts
            .into_iter()
            .filter(|&(_, n)| n >= at_least)
            .map(|(s, n)| (s.to_owned(), n))
            .collect();
        sections.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        sections
    }

    /// The apps matching `query`, best first.
    pub fn search(&self, query: &str) -> Vec<&App> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<(u32, &App)> = self
            .apps
            .iter()
            .filter_map(|app| app.score(&query).map(|score| (score, app)))
            .collect();
        hits.sort_by(|a, b| {
            b.0.cmp(&a.0).then_with(|| {
                a.1.component
                    .name
                    .to_lowercase()
                    .cmp(&b.1.component.name.to_lowercase())
            })
        });
        hits.into_iter().map(|(_, app)| app).collect()
    }

    /// The release feed: the newest release of each app `pick` keeps that
    /// has release notes and came out by `now`, newest first. A release
    /// stamped in the future (a packager's typo, or a time zone) waits.
    pub fn news<'a>(&'a self, now: i64, mut pick: impl FnMut(&App) -> bool) -> Vec<News<'a>> {
        let mut news: Vec<News<'a>> =
            self.apps
                .iter()
                .filter(|app| pick(app))
                .filter_map(|app| {
                    let release = app.component.releases.iter().find(|r| {
                        !r.development && r.timestamp.is_some_and(|t| t <= now + 86_400)
                    })?;
                    (!release.notes.is_empty()).then_some(News { app, release })
                })
                .collect();
        news.sort_by_key(|n| std::cmp::Reverse(n.release.timestamp));
        news
    }
}
