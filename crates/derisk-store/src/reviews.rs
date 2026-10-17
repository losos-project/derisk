//! What people say of an app: its star ratings and reviews, from the Open
//! Desktop Ratings Service (ODRS), the one GNOME Software reads too.
//! Flathub's catalogue carries no ratings of its own.
//!
//! The store asks only when an app's page opens, and says nothing of who
//! is asking: the user hash ODRS wants (to mark a person's own reviews and
//! votes) is all zeros, as nobody here writes reviews.

use serde::Deserialize;

/// The service's API.
pub const ODRS: &str = "https://odrs.gnome.org/1.0/reviews/api";

/// How many reviews an app's page shows.
pub const SHOWN: usize = 5;

/// How many people gave an app each number of stars.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Ratings {
    /// `stars[n]` people gave it `n + 1` stars.
    pub stars: [u32; 5],
}

impl Ratings {
    /// How many people rated it.
    pub fn total(&self) -> u32 {
        self.stars.iter().sum()
    }

    /// The mean number of stars, or `None` when nobody rated it.
    pub fn average(&self) -> Option<f32> {
        let total = self.total();
        (total > 0).then(|| {
            let sum: u32 = (1..).zip(self.stars).map(|(n, count)| n * count).sum();
            sum as f32 / total as f32
        })
    }
}

/// `stars` of five as filled and empty stars.
pub fn stars(stars: f32) -> String {
    let full = stars.round().clamp(0.0, 5.0) as usize;
    format!("{}{}", "★".repeat(full), "☆".repeat(5 - full))
}

/// One person's review.
#[derive(Clone, Debug, PartialEq)]
pub struct Review {
    /// Its one-line summary.
    pub summary: String,
    /// What they wrote.
    pub text: String,
    /// Stars, one to five.
    pub stars: u8,
    /// The name they gave, which may be empty.
    pub author: String,
    /// When, in seconds since 1970.
    pub date: i64,
    /// The app's version they reviewed, which may be empty.
    pub version: String,
}

/// An app's ratings and its most helpful reviews.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reviews {
    /// Its ratings, `None` when nobody rated it.
    pub ratings: Option<Ratings>,
    /// The reviews, most helpful first.
    pub reviews: Vec<Review>,
}

/// The ratings endpoint's answer: `{"star0": .., .., "star5": .., "total": ..}`.
/// `star0` is never given by GNOME Software and is not counted.
pub fn parse_ratings(json: &str) -> Result<Option<Ratings>, String> {
    #[derive(Deserialize)]
    struct Answer {
        #[serde(default)]
        star1: u32,
        #[serde(default)]
        star2: u32,
        #[serde(default)]
        star3: u32,
        #[serde(default)]
        star4: u32,
        #[serde(default)]
        star5: u32,
    }
    let a: Answer = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let ratings = Ratings {
        stars: [a.star1, a.star2, a.star3, a.star4, a.star5],
    };
    Ok((ratings.total() > 0).then_some(ratings))
}

/// The fetch endpoint's answer: a list of reviews, each rated 0 to 100 in
/// steps of 20, most helpful first. A review someone reported, or one with
/// nothing written, is left out.
pub fn parse_reviews(json: &str) -> Result<Vec<Review>, String> {
    #[derive(Deserialize)]
    struct Answer {
        #[serde(default)]
        summary: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        rating: u32,
        #[serde(default)]
        user_display: Option<String>,
        #[serde(default)]
        date_created: f64,
        #[serde(default)]
        version: Option<String>,
        #[serde(default)]
        reported: u32,
    }
    let answers: Vec<Answer> = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(answers
        .into_iter()
        .filter(|a| a.reported == 0)
        .filter_map(|a| {
            let text = a.description.unwrap_or_default().trim().to_owned();
            let summary = a.summary.unwrap_or_default().trim().to_owned();
            (!text.is_empty() || !summary.is_empty()).then(|| Review {
                summary,
                text,
                stars: (a.rating.min(100) as f32 / 20.0).round().clamp(1.0, 5.0) as u8,
                author: a.user_display.unwrap_or_default().trim().to_owned(),
                date: a.date_created as i64,
                version: a.version.unwrap_or_default(),
            })
        })
        .collect())
}

/// The locale ODRS picks reviews by, `en_US` from `en_US.UTF-8`, from the
/// session's language as the catalogue's is.
pub fn locale(lang: &str) -> String {
    let lang = lang.split(['.', '@']).next().unwrap_or_default();
    if lang.is_empty() || lang == "C" || lang == "POSIX" {
        "en_US".into()
    } else {
        lang.into()
    }
}

/// The body of a fetch for `id`'s reviews in `locale`.
pub fn fetch_body(id: &str, locale: &str) -> String {
    serde_json::json!({
        "app_id": id,
        "user_hash": "0".repeat(40),
        "locale": locale,
        "distro": "LosOS",
        "version": "unknown",
        "limit": SHOWN,
    })
    .to_string()
}

/// Where reviews come from: [`fetch`], or a stand-in.
pub type Fetch = fn(&str, &str) -> Result<Reviews, String>;

/// Asks ODRS for `id`'s ratings and reviews.
#[cfg(not(target_arch = "wasm32"))]
pub fn fetch(id: &str, locale: &str) -> Result<Reviews, String> {
    use std::time::Duration;

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .build()
        .into();
    let read = |response: Result<ureq::http::Response<ureq::Body>, ureq::Error>| {
        response
            .map_err(|e| e.to_string())?
            .into_body()
            .read_to_string()
            .map_err(|e| e.to_string())
    };
    let ratings = parse_ratings(&read(agent.get(format!("{ODRS}/ratings/{id}")).call())?)?;
    let reviews = parse_reviews(&read(
        agent
            .post(format!("{ODRS}/fetch"))
            .content_type("application/json")
            .send(fetch_body(id, locale)),
    )?)?;
    Ok(Reviews { ratings, reviews })
}
