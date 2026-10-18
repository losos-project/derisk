//! People's accounts: the rules for a user name, the groups a new account
//! joins, and reading what homectl says, for first-boot setup (which makes
//! the first account) and the Settings app's Users page (which makes the
//! rest) alike.
//!
//! An account is a systemd-homed record, made and changed with `homectl`.
//! Nothing here runs it; this is the part both callers would otherwise
//! each keep a copy of.

use crate::locale;

/// Names a user may not take: system accounts that exist or that systemd
/// reserves.
pub const RESERVED: &[&str] = &[
    "root",
    "nobody",
    "daemon",
    "bin",
    "sys",
    "adm",
    "wheel",
    "users",
    "systemd",
    "messagebus",
    "polkituser",
    "derisk-greeter",
];

/// A user name from a full name: the first word, lowercase, with accents
/// dropped and anything a user name cannot hold left out. `Matúš Novák`
/// becomes `matus`.
pub fn user_name_for(real_name: &str) -> String {
    let first = real_name.split_whitespace().next().unwrap_or_default();
    let mut out = String::new();
    for c in first.chars().flat_map(char::to_lowercase) {
        let c = locale::fold_accent(c);
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_' {
            out.push(c);
        }
    }
    // A user name starts with a letter or an underscore.
    let start = out
        .find(|c: char| c.is_ascii_lowercase() || c == '_')
        .unwrap_or(out.len());
    out.drain(..start);
    out.truncate(31);
    out
}

/// What is wrong with a user name, or `None`. The rules are systemd's for a
/// portable name (`valid_user_group_name` without `--relaxed`), kept to
/// lowercase so a name is typed the same way everywhere.
pub fn user_name_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("Choose a user name.");
    }
    if name.len() > 31 {
        return Some("A user name is at most 31 characters.");
    }
    let mut chars = name.chars();
    let first = chars.next()?;
    if !(first.is_ascii_lowercase() || first == '_') {
        return Some("A user name starts with a lowercase letter.");
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
        return Some("A user name holds only lowercase letters, digits, - and _.");
    }
    if RESERVED.contains(&name) {
        return Some("That user name belongs to the system. Choose another.");
    }
    None
}

/// Groups that let a session use a device, which the first account joins
/// when the system has them. NixOS's polkit rule lets `networkmanager`
/// change the networks the Settings app's Network page joins and forgets;
/// setup itself runs as root and needs no group for its own Network page.
pub const DEVICE_GROUPS: &[&str] = &["networkmanager"];

/// Which of [`DEVICE_GROUPS`] `group_file` (`/etc/group`) lists. homectl
/// refuses an account in a group that does not exist, so a system without
/// Wi-Fi gets an account without that group.
pub fn device_groups(group_file: &std::path::Path) -> Vec<String> {
    let groups = std::fs::read_to_string(group_file).unwrap_or_default();
    DEVICE_GROUPS
        .iter()
        .filter(|name| {
            groups
                .lines()
                .any(|line| line.split(':').next() == Some(**name))
        })
        .map(|name| (*name).to_owned())
        .collect()
}

/// Whether `userdbctl user --disposition=regular --json=short` output lists
/// anyone: if so, the machine is set up already and `derisk setup` has
/// nothing to ask.
pub fn has_regular_users(userdbctl_json: &str) -> bool {
    let text = userdbctl_json.replace('\u{1e}', "\n");
    serde_json::Deserializer::from_str(&text)
        .into_iter::<serde_json::Value>()
        .map_while(Result::ok)
        .any(|record| record.get("userName").is_some())
}

/// Where systemd's password agents find the questions a program on this
/// machine is waiting on, one `ask.*` file each.
pub const ASK_PASSWORD_DIR: &str = "/run/systemd/ask-password";

/// The socket that answers `ask_file` (an `ask.*` file under
/// [`ASK_PASSWORD_DIR`]) when it is the question process `pid` is waiting
/// on, or `None` when it is another program's.
///
/// homectl reads the new password from `NEWPASSWORD` once. When homed
/// refuses that password (its quality policy), systemd 261's homectl asks
/// for another, and with no terminal on its stdin it asks a password agent
/// with no time limit. Nothing answers on the seat while the setup holds
/// it, so "Your account" would spin for good; the setup answers "cancel"
/// on this socket instead ([`ASK_CANCEL`]), and homectl exits with homed's
/// reason on its stderr ([`task_failure`]).
pub fn ask_socket(ask_file: &str, pid: u32) -> Option<std::path::PathBuf> {
    let field = |key: &str| {
        ask_file
            .lines()
            .find_map(|line| line.trim().strip_prefix(key)?.strip_prefix('='))
            .map(str::trim)
    };
    (field("PID")?.parse::<u32>().ok()? == pid)
        .then(|| field("Socket"))
        .flatten()
        .filter(|socket| !socket.is_empty())
        .map(Into::into)
}

/// The datagram that answers a password agent's question with "cancel".
pub const ASK_CANCEL: &[u8] = b"-";

/// What to show for a task that failed with `stderr`, or `None` when it
/// printed nothing. Normally its last line. When the setup cancelled a
/// password question, homectl's last lines are only the cancellation and a
/// hint about a command-line switch; the line before them is homed's
/// reason ("Password too weak: ...").
pub fn task_failure(stderr: &str) -> Option<&str> {
    let lines = || {
        stderr
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
    };
    lines()
        .rfind(|line| {
            !line.starts_with("Failed to acquire password")
                && !line.starts_with("(Use --enforce-password-policy=")
        })
        .or_else(|| lines().next_back())
}

/// `text` as a sentence: libpwquality's messages ("The password is shorter
/// than 8 characters") end without a full stop, and the page's own notices
/// end with one.
pub fn sentence(text: &str) -> String {
    let text = text.trim();
    if text.ends_with(['.', '!', '?']) {
        text.to_owned()
    } else {
        format!("{text}.")
    }
}
