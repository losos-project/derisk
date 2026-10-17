//! First-boot setup: what `derisk setup` asks before anyone can log in, and
//! the commands that save the answers.
//!
//! A freshly installed system has no users (LosOS has no account in its
//! image; a user is a systemd-homed home area), so the first boot asks for a
//! language, a keyboard layout, a time zone, a network and the first account,
//! and then hands the screen to the login screen. This module holds the
//! steps and their rules without a toolkit, so they can be tested headless;
//! `derisk setup` draws them with [`crate::wizard`].

use crate::{locale, password_age::PasswordAge};

/// One page of the setup, in order.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Step {
    /// The language, on the welcome page.
    Language,
    /// The keyboard layout, with a field to try it in.
    Keyboard,
    /// The time zone.
    TimeZone,
    /// Wired status and Wi-Fi.
    Network,
    /// The first account.
    Account,
    /// Saving everything and creating the account.
    Applying,
}

impl Step {
    /// The pages someone moves through with Back and Next.
    pub const PAGES: [Step; 5] = [
        Step::Language,
        Step::Keyboard,
        Step::TimeZone,
        Step::Network,
        Step::Account,
    ];

    /// The page's heading.
    pub fn title(self) -> &'static str {
        match self {
            Step::Language => "Welcome",
            Step::Keyboard => "Keyboard",
            Step::TimeZone => "Time zone",
            Step::Network => "Network",
            Step::Account => "Your account",
            Step::Applying => "Setting up",
        }
    }

    /// The line under the heading.
    pub fn subtitle(self) -> &'static str {
        match self {
            Step::Language => "Choose your language.",
            Step::Keyboard => "Choose a keyboard layout, and try it below.",
            Step::TimeZone => "Search for your city or region.",
            Step::Network => "Connect now, or later from the desktop.",
            Step::Account => "This account can install apps and change system settings.",
            Step::Applying => "Saving your choices and creating your account.",
        }
    }

    /// The page after this one.
    pub fn next(self) -> Step {
        let i = Self::PAGES.iter().position(|s| *s == self);
        match i {
            Some(i) if i + 1 < Self::PAGES.len() => Self::PAGES[i + 1],
            _ => Step::Applying,
        }
    }

    /// The page before this one, if any.
    pub fn back(self) -> Option<Step> {
        let i = Self::PAGES.iter().position(|s| *s == self)?;
        i.checked_sub(1).map(|i| Self::PAGES[i])
    }

    /// Where this page is among [`Step::PAGES`], for the progress dots.
    pub fn index(self) -> usize {
        Self::PAGES
            .iter()
            .position(|s| *s == self)
            .unwrap_or(Self::PAGES.len())
    }
}

/// The first account, as typed.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Account {
    /// The full name, as others see it.
    pub real_name: String,
    /// The user name, which logging in uses.
    pub user_name: String,
    /// The password.
    pub password: String,
    /// The password again.
    pub confirm: String,
    /// Whether the user name was typed rather than derived from the full
    /// name; once it was, the full name stops changing it.
    pub user_name_edited: bool,
    /// Why homed's quality check would refuse the password, set by whoever
    /// can ask libpwquality (`derisk setup`); `None` when it would take it
    /// or nobody asked.
    pub weakness: Option<String>,
}

/// Names a user may not take: system accounts that exist or that systemd
/// reserves.
const RESERVED: &[&str] = &[
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

impl Account {
    /// Sets the full name and, until someone types a user name of their
    /// own, the user name derived from it.
    pub fn set_real_name(&mut self, real_name: String) {
        if !self.user_name_edited {
            self.user_name = user_name_for(&real_name);
        }
        self.real_name = real_name;
    }

    /// What is wrong with the account so far, in a sentence, or `None` when
    /// it can be created.
    pub fn problem(&self) -> Option<&str> {
        if self.real_name.trim().is_empty() {
            return Some("Enter your name.");
        }
        if self.real_name.contains([':', '\n']) {
            return Some("Your name cannot contain a colon or a line break.");
        }
        if let Some(problem) = user_name_problem(&self.user_name) {
            return Some(problem);
        }
        if self.password.is_empty() {
            return Some("Choose a password.");
        }
        // Before the confirmation, so a weak password is said while it is
        // typed the first time.
        if let Some(weakness) = &self.weakness {
            return Some(weakness);
        }
        if self.password != self.confirm {
            return Some("The passwords do not match.");
        }
        None
    }
}

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

/// Everything the setup asks for.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Choices {
    /// The locale, such as `de_DE.UTF-8`.
    pub locale: String,
    /// The XKB layout, such as `de`.
    pub layout: String,
    /// The time zone, such as `Europe/Berlin`.
    pub time_zone: String,
    /// The first account.
    pub account: Account,
    /// How often the system wants passwords changed
    /// ([`crate::password_age`]); the account is created under it.
    pub password_age: PasswordAge,
    /// The groups beyond `wheel` the account joins: those of
    /// [`DEVICE_GROUPS`] the system has ([`device_groups`]).
    pub groups: Vec<String>,
}

/// Groups that let a session use a device, which the first account joins
/// when the system has them. `wpa_supplicant` owns the control sockets that
/// the Settings app's Network page joins Wi-Fi networks through; setup
/// itself runs as root and needs no group for its own Network page.
pub const DEVICE_GROUPS: &[&str] = &["wpa_supplicant"];

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

/// A command the setup runs to save a choice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Task {
    /// What the progress list says while it runs.
    pub label: &'static str,
    /// The command line.
    pub argv: Vec<String>,
    /// Extra environment, for secrets that must not be on a command line.
    pub env: Vec<(String, String)>,
}

/// The commands that save `choices`, in order. The account comes last: it
/// takes longest (homed formats an encrypted home area) and is the one
/// most likely to be refused, by the password policy, so everything else is
/// already saved when the person is sent back to fix it.
///
/// The user joins `wheel`, which is how run0, polkit and Flatpak know who
/// administers the machine, and [`Choices::groups`]. The password goes through `NEWPASSWORD`, which
/// homectl reads instead of asking, so it never shows in `ps`.
pub fn plan(choices: &Choices) -> Vec<Task> {
    let mut tasks = Vec::new();
    if !choices.locale.is_empty() {
        tasks.push(Task {
            label: "Language",
            argv: locale::set_locale_argv(&choices.locale),
            env: Vec::new(),
        });
    }
    if !choices.layout.is_empty() {
        tasks.push(Task {
            label: "Keyboard",
            argv: locale::set_keyboard_argv(&choices.layout),
            env: Vec::new(),
        });
    }
    if !choices.time_zone.is_empty() {
        tasks.push(Task {
            label: "Time zone",
            argv: locale::set_time_zone_argv(&choices.time_zone),
            env: Vec::new(),
        });
    }
    let account = &choices.account;
    tasks.push(Task {
        label: "Your account",
        argv: vec![
            "homectl".into(),
            "create".into(),
            account.user_name.clone(),
            format!("--real-name={}", account.real_name.trim()),
            format!(
                "--member-of={}",
                std::iter::once("wheel")
                    .chain(choices.groups.iter().map(String::as_str))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            // No --language: systemd 261's homectl turns it into a record
            // with `"perMachine": null`, which it then refuses as "not an
            // array". The session takes its language from localed's
            // /etc/locale.conf, which the Language task sets.
            format!("--timezone={}", choices.time_zone),
        ]
        .into_iter()
        // With no policy the age options would only clear fields a new
        // record does not have; the filter below drops them.
        .chain(choices.password_age.homectl_args())
        .filter(|a| !a.ends_with('='))
        .collect(),
        env: vec![("NEWPASSWORD".into(), account.password.clone())],
    });
    tasks
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
