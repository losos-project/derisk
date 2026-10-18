//! {{display_name}}: a derisk overview widget plugin.
//!
//! derisk asks it to `render` its cards whenever a part of the desktop it
//! reads changes. A card says what it holds, never how it looks: text in a
//! tone and an emphasis, progress bars, rows of buttons, grids, a text
//! field. derisk draws it with the desktop theme. A button runs derisk
//! `Action`s as JSON, the same ones the agent socket's `dispatch` takes. The
//! manifest says what the plugin may see and run; derisk drops any card
//! outside it.

use derisk_widget_sdk::{Card, Guest, Inline, Input, Manifest, Text, Tone, View, export, json};

struct Plugin;

impl Guest for Plugin {
    fn describe() -> Manifest {
        Manifest::new("{{name}}", "{{summary}}")
            .inputs(&[Input::Clock])
            .actions(&["overview"])
    }

    fn render(view: View) -> Vec<Card> {
        vec![card(&view)]
    }
}

/// The card, apart from the export so tests can call it.
pub fn card(view: &View) -> Card {
    Card::new("{{name}}")
        .text(Text::heading("{{display_name}}"))
        .text(Text::plain(format!("It is {}.", view.clock.time_label())).tone(Tone::Dim))
        .row([Inline::button(
            "Close the overview",
            &[json!({"action": "overview", "visible": false})],
        )])
}

export!(Plugin with_types_in derisk_widget_sdk::bindings);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_the_time_and_a_button() {
        let card = card(&View::default());
        assert_eq!(
            card.texts(),
            ["{{display_name}}", "It is 12:00.", "Close the overview"]
        );
    }
}
