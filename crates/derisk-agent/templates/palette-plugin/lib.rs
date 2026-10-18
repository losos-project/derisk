//! {{display_name}}: a derisk command palette plugin.
//!
//! derisk asks it for `entries`, its rows, whenever a part of the desktop it
//! reads changes, and for `query`, rows for what the person typed. Choosing
//! a row runs its actions: derisk `Action`s as JSON, the same ones the agent
//! socket's `dispatch` takes. The manifest says what the plugin may see and
//! run; derisk drops any row outside it.

use derisk_palette_sdk::{Category, Entry, Guest, Hook, Manifest, View, export, json};

struct Plugin;

impl Guest for Plugin {
    fn describe() -> Manifest {
        Manifest::new("{{name}}", "{{summary}}")
            .hooks(&[Hook::Entries])
            .categories(&[Category::Command])
            .actions(&["overview"])
    }

    fn entries(view: View) -> Vec<Entry> {
        entries(&view)
    }

    fn query(_view: View, _text: String) -> Vec<Entry> {
        Vec::new()
    }
}

/// The rows, apart from the export so tests can call them.
pub fn entries(_view: &View) -> Vec<Entry> {
    vec![
        Entry::new(
            Category::Command,
            "view-grid-symbolic",
            "{{display_name}}: Show the overview",
            &[json!({"action": "overview", "visible": true})],
        )
        .keywords("{{name}}"),
    ]
}

export!(Plugin with_types_in derisk_palette_sdk::bindings);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offers_its_command() {
        let rows = entries(&View::default());
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].parsed_actions(),
            [json!({"action": "overview", "visible": true})]
        );
    }
}
