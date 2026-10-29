//! LosOS boxes: for typed text, the files in the person's LosOS boxes that
//! have it, in LosOS Cloud and in LosOS Git's repositories, each opening in
//! the box's own pages.
//!
//! The session asks the boxes `losos` is signed in to (LosOS Desktop's
//! `losos-boxes search`) and publishes what they found as the `boxes`
//! source, which only the shell itself registers:
//!
//! ```text
//! {"text":"rent","hits":[{"box":"mattbox","app":"cloud","title":"budget.md",
//!   "place":"LosOS Cloud › Documents","snippet":"the rent is due",
//!   "url":"http://mattbox.local/cloud/files/Documents"}]}
//! ```
//!
//! A search takes a second or two, so the rows found for the text typed
//! so far stay while more is typed after it, until the boxes answer for
//! the longer text. No box, or none signed in to, has no rows.

use derisk_palette_sdk::{
    Category, Entry, Guest, Hook, Input, Manifest, Value, View, export, json,
};

/// The registration this plugin reads.
const SOURCE: &str = "boxes";

struct Boxes;

impl Guest for Boxes {
    fn describe() -> Manifest {
        Manifest::new("boxes", "Text in the files and Git of your LosOS boxes")
            .hooks(&[Hook::Query])
            .inputs(&[Input::Registered])
            .sources(&[SOURCE])
            .categories(&[Category::File])
            .actions(&["open_url"])
    }

    fn entries(_view: View) -> Vec<Entry> {
        Vec::new()
    }

    fn query(view: View, text: String) -> Vec<Entry> {
        query(&view, &text)
    }
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}

/// The rows for `typed`: what the boxes found for it, or for the text it
/// starts with while they have not answered for all of it.
pub fn query(view: &View, typed: &str) -> Vec<Entry> {
    let Some(registration) = view.registered.iter().find(|r| r.source == SOURCE) else {
        return Vec::new();
    };
    let Ok(data) = derisk_palette_sdk::serde_json::from_str::<Value>(&registration.data) else {
        return Vec::new();
    };
    let answered = text(&data, "text");
    if answered.is_empty() || !typed.to_lowercase().starts_with(&answered.to_lowercase()) {
        return Vec::new();
    }
    let none = Vec::new();
    data["hits"]
        .as_array()
        .unwrap_or(&none)
        .iter()
        .filter_map(row)
        .collect()
}

/// One hit's row: the file's name, then the box, where in it the file is,
/// and the words around the match.
fn row(hit: &Value) -> Option<Entry> {
    let (title, url) = (text(hit, "title"), text(hit, "url"));
    if title.is_empty() || !(url.starts_with("http://") || url.starts_with("https://")) {
        return None;
    }
    let mut detail = [text(hit, "box"), text(hit, "place"), text(hit, "snippet")]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    if detail.is_empty() {
        detail = url.to_owned();
    }
    // A cloud file opens its folder; a line in Git opens the file.
    let icon = if text(hit, "app") == "git" {
        "text-x-script"
    } else {
        "folder-remote"
    };
    Some(
        Entry::new(
            Category::File,
            icon,
            title,
            &[json!({"action": "open_url", "url": url})],
        )
        .detail(detail)
        .keywords(text(hit, "box")),
    )
}

export!(Boxes with_types_in derisk_palette_sdk::bindings);

#[cfg(test)]
mod tests {
    use derisk_palette_sdk::Registration;

    use super::*;

    fn view(data: &Value) -> View {
        View {
            registered: vec![Registration {
                source: SOURCE.into(),
                data: data.to_string(),
            }],
            ..View::default()
        }
    }

    fn found() -> Value {
        json!({"text": "rent", "hits": [
            {"box": "mattbox", "app": "cloud", "title": "budget.md",
             "place": "LosOS Cloud › Documents", "snippet": "the rent is due",
             "url": "http://mattbox.local/cloud/files/Documents"},
            {"box": "mattbox", "app": "git", "title": "rent.md",
             "place": "notshared/notes", "snippet": "",
             "url": "http://mattbox.local/git/notshared/notes/src/branch/main/rent.md#L3"},
        ]})
    }

    #[test]
    fn a_hit_names_the_file_and_where_it_is() {
        let rows = query(&view(&found()), "rent");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].title, "budget.md");
        assert_eq!(
            rows[0].detail,
            "mattbox · LosOS Cloud › Documents · the rent is due"
        );
        assert_eq!(rows[0].icon, "folder-remote");
        assert_eq!(
            rows[0].parsed_actions(),
            [json!({"action": "open_url", "url": "http://mattbox.local/cloud/files/Documents"})]
        );
        assert_eq!(rows[1].detail, "mattbox · notshared/notes");
        assert_eq!(rows[1].icon, "text-x-script");
    }

    #[test]
    fn rows_stay_while_more_is_typed() {
        assert_eq!(query(&view(&found()), "Rent due").len(), 2);
        assert!(query(&view(&found()), "ren").is_empty());
        assert!(query(&view(&found()), "milk").is_empty());
    }

    #[test]
    fn nothing_found_yet_is_no_rows() {
        assert!(query(&view(&json!({"text": "", "hits": []})), "rent").is_empty());
        assert!(query(&View::default(), "rent").is_empty());
    }

    #[test]
    fn only_web_addresses_open() {
        let data = json!({"text": "rent", "hits": [
            {"box": "mattbox", "title": "x", "url": "file:///etc/passwd"},
            {"box": "mattbox", "title": "", "url": "http://mattbox.local/"},
        ]});
        assert!(query(&view(&data), "rent").is_empty());
    }
}
