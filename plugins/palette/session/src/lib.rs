//! Session: lock, switch user, suspend, hibernate, log out, restart and shut down. The
//! ones that lose work ask for a second Enter.

use derisk_palette_sdk::{Category, Entry, Guest, Hook, Manifest, View, export, json};

struct Session;

impl Guest for Session {
    fn describe() -> Manifest {
        Manifest::new(
            "session",
            "Lock, switch user, suspend, hibernate, log out, restart and shut down",
        )
        .hooks(&[Hook::Entries])
        .categories(&[Category::Session])
        .actions(&["session"])
    }

    fn entries(_: View) -> Vec<Entry> {
        entries()
    }

    fn query(_: View, _: String) -> Vec<Entry> {
        Vec::new()
    }
}

/// The catalog, which needs nothing from the view.
pub fn entries() -> Vec<Entry> {
    [
        (
            "Lock Screen",
            "system-lock-screen",
            "lock",
            "lock away system session",
            false,
        ),
        (
            "Switch User",
            "system-switch-user",
            "switch_user",
            "switch user account log in someone else login screen",
            false,
        ),
        (
            "Suspend",
            "system-suspend",
            "suspend",
            "sleep system power",
            false,
        ),
        (
            "Hibernate",
            "system-hibernate",
            "hibernate",
            "sleep disk system power",
            false,
        ),
        (
            "Log Out",
            "system-log-out",
            "logout",
            "sign out logout exit system session",
            true,
        ),
        (
            "Restart",
            "system-reboot",
            "reboot",
            "reboot system power",
            true,
        ),
        (
            "Shut Down",
            "system-shutdown",
            "power_off",
            "power off poweroff shutdown system",
            true,
        ),
    ]
    .into_iter()
    .map(|(title, icon, op, keywords, destructive)| {
        // `confirmed`: picking the row is the person asking; the destructive
        // ones still wait for the palette's second Enter.
        Entry::new(
            Category::Session,
            icon,
            title,
            &[json!({"action": "session", "op": op, "confirmed": true})],
        )
        .keywords(keywords)
        .confirm(destructive)
    })
    .collect()
}

export!(Session with_types_in derisk_palette_sdk::bindings);
