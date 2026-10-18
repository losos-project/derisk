//! The Agent: a request the built-in assistant does not understand is
//! offered to the Agent (`derisk-agent`), which can do far more with the
//! agent CLI it runs, derisk's MCP tools and its skills. It only opens the
//! request in a new chat, for the person to send.

use derisk_palette_sdk::{
    Category, Entry, Guest, Hook, Manifest, Position, View, export, interpret, json,
};

struct Agent;

impl Guest for Agent {
    fn describe() -> Manifest {
        Manifest::new(
            "agent",
            "Handing a request the assistant cannot follow to the Agent",
        )
        .hooks(&[Hook::Query])
        .categories(&[Category::Agent])
        .actions(&["ask_agent"])
    }

    fn entries(_: View) -> Vec<Entry> {
        Vec::new()
    }

    fn query(_: View, text: String) -> Vec<Entry> {
        query(&text, |t| interpret(t).is_ok()).into_iter().collect()
    }
}

/// The row for `text`, unless `understood` says the assistant has it.
pub fn query(text: &str, understood: impl Fn(&str) -> bool) -> Option<Entry> {
    if text.is_empty() || understood(text) {
        return None;
    }
    Some(
        Entry::new(
            Category::Agent,
            "tool-magic",
            format!("Ask the agent: {text}"),
            &[json!({"action": "ask_agent", "text": text})],
        )
        .detail("Opens a new chat in Agent, for you to send")
        .at(Position::Bottom),
    )
}

export!(Agent with_types_in derisk_palette_sdk::bindings);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_what_the_assistant_cannot_do() {
        assert!(query("open files", |_| true).is_none());
        let row = query("frobnicate the flux", |_| false).unwrap();
        assert_eq!(row.title, "Ask the agent: frobnicate the flux");
    }
}
