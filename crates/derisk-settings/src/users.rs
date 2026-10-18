//! The Users page: everyone with an account on this computer, adding and
//! removing people, their names, and who administers the machine.
//!
//! An account is a systemd-homed record ([`derisk_install::account`]), so
//! every change here is a `homectl` command, run on the Sign-in page's worker
//! ([`crate::signin`]). homed asks polkit before it makes, removes or changes
//! anyone's account but the caller's own name, and derisk's polkit agent
//! asks for an administrator's password over the desktop; a standard user can
//! open this page too and have an administrator type theirs.
//!
//! An administrator is a member of `wheel`, which is how run0, polkit and
//! Flatpak know who administers the machine, the same as the account
//! first-boot setup makes. Someone else's home area is locked while they are
//! logged out, so their record is changed with `--offline`: homed updates its
//! own copy and writes it into the home area at their next login.
//!
//! Each person's virtual machines live in their home area and run as their
//! own user services, so removing someone deletes their machines with it,
//! and homed refuses to remove a home area that is open. Someone signed in
//! behind Switch user is signed out first, on purpose and with a warning.

use std::process::{Command, Stdio};

use derisk_install::account;
use mcsapi_ui::{Theme, egui};

use crate::signin::{self, Job, Running};

/// Asks homed's password quality check whether it would take a new password
/// (`password`, for `user`), and says why not. The Settings library cannot
/// load libpwquality itself; the session hands this in
/// ([`set_password_check`]).
pub type PasswordCheck = fn(password: &str, user: &str) -> Option<String>;

static PASSWORD_CHECK: std::sync::OnceLock<PasswordCheck> = std::sync::OnceLock::new();

/// Lets the Users page say why homed would refuse a new person's password
/// while it is typed, as first-boot setup does. Without it the page judges
/// nothing and homed's refusal comes back when "Add" is pressed.
pub fn set_password_check(check: PasswordCheck) {
    let _ = PASSWORD_CHECK.set(check);
}

/// The `homectl` options that put a new account under the system's password
/// age policy, as first-boot setup makes its account. The policy's reader
/// lives in the session; it hands it in ([`set_password_age`]).
pub type PasswordAgeArgs = fn() -> Vec<String>;

static PASSWORD_AGE: std::sync::OnceLock<PasswordAgeArgs> = std::sync::OnceLock::new();

/// Makes new accounts under the system's password age policy. Without it an
/// account is made without one and the display manager brings its record up
/// to the policy at the first login.
pub fn set_password_age(args: PasswordAgeArgs) {
    let _ = PASSWORD_AGE.set(args);
}

/// The group whose members administer the machine.
pub const ADMIN_GROUP: &str = "wheel";

/// Someone with an account.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Person {
    /// The name they log in with.
    pub user_name: String,
    /// Their full name, or empty.
    pub real_name: String,
    /// Groups the record lists.
    pub groups: Vec<String>,
    /// Whether they administer the machine.
    pub admin: bool,
    /// Whether homed keeps the account, so this page can change it.
    pub homed: bool,
    /// Whether their home area is open: they are signed in, or homed is
    /// still opening or closing it. homed will not remove it then.
    pub in_use: bool,
}

impl Person {
    /// What to call them: the full name, or the user name without one.
    pub fn display_name(&self) -> &str {
        if self.real_name.trim().is_empty() {
            &self.user_name
        } else {
            &self.real_name
        }
    }

    /// The letter on their picture.
    pub fn initial(&self) -> String {
        self.display_name()
            .chars()
            .next()
            .map(|c| c.to_uppercase().collect())
            .unwrap_or_default()
    }

    /// The groups their record should list once `admin` is set: theirs,
    /// with [`ADMIN_GROUP`] added or taken out.
    pub fn groups_with_admin(&self, admin: bool) -> Vec<String> {
        let mut groups: Vec<String> = self
            .groups
            .iter()
            .filter(|g| g.as_str() != ADMIN_GROUP)
            .cloned()
            .collect();
        if admin {
            groups.insert(0, ADMIN_GROUP.to_owned());
        }
        groups
    }
}

/// Reads `userdbctl user --disposition=regular --json=short` output: one
/// JSON record per person, possibly with RS separators. `wheel` lists the
/// administrators `/etc/group` names, for accounts whose record does not
/// carry its groups (an /etc/passwd user). Sorted by user name.
pub fn parse_people(userdbctl_json: &str, wheel: &[String]) -> Vec<Person> {
    let text = userdbctl_json.replace('\u{1e}', "\n");
    let mut people: Vec<Person> = serde_json::Deserializer::from_str(&text)
        .into_iter::<serde_json::Value>()
        .map_while(Result::ok)
        .filter_map(|record| {
            let user_name = record.get("userName")?.as_str()?.to_owned();
            let text = |key: &str| {
                record
                    .get(key)
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned()
            };
            let groups: Vec<String> = record
                .get("memberOf")
                .and_then(|v| v.as_array())
                .map(|list| {
                    list.iter()
                        .filter_map(|g| g.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            let admin = groups.iter().any(|g| g == ADMIN_GROUP) || wheel.contains(&user_name);
            // homed says it keeps the account, and what state the home
            // area is in, in the record's status section, one entry per
            // machine ID; this machine's is the only one a local record has.
            let status: Vec<&serde_json::Value> = record
                .get("status")
                .and_then(|v| v.as_object())
                .into_iter()
                .flat_map(|machines| machines.values())
                .collect();
            let homed = status
                .iter()
                .any(|s| s.get("service").and_then(|v| v.as_str()) == Some("io.systemd.Home"));
            let in_use = status
                .iter()
                .filter_map(|s| s.get("state")?.as_str())
                .any(|state| !CLOSED_STATES.contains(&state));
            Some(Person {
                real_name: text("realName"),
                homed,
                in_use,
                user_name,
                groups,
                admin,
            })
        })
        .collect();
    people.sort_by(|a, b| a.user_name.cmp(&b.user_name));
    people.dedup_by(|a, b| a.user_name == b.user_name);
    people
}

/// homed's states for a home area that is closed and idle; in every other
/// one it is open or busy, and `homectl remove` refuses it.
const CLOSED_STATES: &[&str] = &["unfixated", "absent", "inactive", "dirty"];

/// The members of `group` in `/etc/group`-style text.
pub fn group_members<'a>(group_file: &'a str, group: &'a str) -> impl Iterator<Item = &'a str> {
    group_file
        .lines()
        .filter(move |line| line.split(':').next() == Some(group))
        .flat_map(|line| line.split(':').nth(3).unwrap_or_default().split(','))
        .map(str::trim)
        .filter(|name| !name.is_empty())
}

/// Makes an account for `new`, in the groups `extra` names beside
/// [`ADMIN_GROUP`] when they administer the machine. The password goes
/// through `NEWPASSWORD`, which homectl reads instead of asking.
pub fn create(new: &NewPerson, extra: &[String]) -> Job {
    let groups: Vec<&str> = new
        .admin
        .then_some(ADMIN_GROUP)
        .into_iter()
        .chain(extra.iter().map(String::as_str))
        .collect();
    let mut argv = vec![
        "homectl".into(),
        "create".into(),
        new.user_name.clone(),
        format!("--real-name={}", new.real_name.trim()),
    ];
    // An empty --member-of= would be read as "no groups" anyway; leaving it
    // out keeps the command line what someone would type.
    if !groups.is_empty() {
        argv.push(format!("--member-of={}", groups.join(",")));
    }
    argv.extend(PASSWORD_AGE.get().map(|args| args()).unwrap_or_default());
    Job {
        label: "Adding the account",
        argv,
        env: vec![("NEWPASSWORD".into(), new.password.clone())],
    }
}

/// `homectl update` for `user` with `args`. Someone else's home area is
/// locked, so their change goes to homed's copy of the record (`--offline`)
/// and into the home area at their next login; one's own is open, and
/// changes in place.
fn update(user: &str, me: &str, label: &'static str, args: Vec<String>) -> Job {
    let mut argv = vec!["homectl".into(), "update".into(), user.into()];
    if user != me {
        argv.push("--offline".into());
    }
    argv.extend(args);
    Job {
        label,
        argv,
        env: Vec::new(),
    }
}

/// Changes `user`'s full name to `real_name`.
pub fn rename(user: &str, me: &str, real_name: &str) -> Job {
    update(
        user,
        me,
        "Changing the name",
        vec![format!("--real-name={}", real_name.trim())],
    )
}

/// Makes `person` an administrator, or takes it away.
pub fn set_admin(person: &Person, me: &str, admin: bool) -> Job {
    let groups = person.groups_with_admin(admin);
    update(
        &person.user_name,
        me,
        if admin {
            "Making them an administrator"
        } else {
            "Making them a standard user"
        },
        vec![format!("--member-of={}", groups.join(","))],
    )
}

/// Removes `user`'s account and their home area with every file in it.
pub fn remove(user: &str) -> Job {
    Job {
        label: "Removing the account",
        argv: vec!["homectl".into(), "remove".into(), user.into()],
        env: Vec::new(),
    }
}

/// Ends every session `user` has, which closes their apps and their virtual
/// machines and closes their home area, so it can be removed.
pub fn sign_out(user: &str) -> Job {
    Job {
        label: "Signing them out",
        argv: vec!["loginctl".into(), "terminate-user".into(), user.into()],
        env: Vec::new(),
    }
}

/// Why `person` cannot stop administering the machine (they are the last
/// administrator), or `None`. Without one nobody could install updates,
/// change the system or add anyone again.
pub fn last_admin(people: &[Person], person: &Person) -> Option<&'static str> {
    let admins = people.iter().filter(|p| p.admin).count();
    (person.admin && admins <= 1)
        .then_some("This is the only administrator. Make someone else one first.")
}

/// The person being added, as typed.
#[derive(Clone, Default, Eq, PartialEq)]
pub struct NewPerson {
    /// Their full name.
    pub real_name: String,
    /// The name they will log in with: derived from the full name until
    /// it is typed.
    pub user_name: String,
    /// Whether the user name was typed rather than derived.
    pub user_name_edited: bool,
    /// The password they will log in with first.
    pub password: String,
    /// The password again.
    pub confirm: String,
    /// Whether they will administer the machine.
    pub admin: bool,
    /// Why homed would refuse the password, from [`set_password_check`].
    pub weakness: Option<String>,
}

impl std::fmt::Debug for NewPerson {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // No passwords in debug output.
        f.debug_struct("NewPerson")
            .field("real_name", &self.real_name)
            .field("user_name", &self.user_name)
            .field("admin", &self.admin)
            .finish_non_exhaustive()
    }
}

impl NewPerson {
    /// Sets the full name and, until a user name is typed, the user name
    /// derived from it.
    pub fn set_real_name(&mut self, real_name: String) {
        if !self.user_name_edited {
            self.user_name = account::user_name_for(&real_name);
        }
        self.real_name = real_name;
    }

    /// What is wrong so far, in a sentence, or `None` when the account can
    /// be made. `taken` are the user names that exist.
    pub fn problem<'a>(&'a self, taken: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
        if self.real_name.trim().is_empty() {
            return Some("Enter their name.");
        }
        if self.real_name.contains([':', '\n']) {
            return Some("A name cannot contain a colon or a line break.");
        }
        if let Some(problem) = account::user_name_problem(&self.user_name) {
            return Some(problem);
        }
        if taken.into_iter().any(|name| name == self.user_name) {
            return Some("Someone has that user name already. Choose another.");
        }
        if self.password.is_empty() {
            return Some("Choose a password for them.");
        }
        if let Some(weakness) = &self.weakness {
            return Some(weakness);
        }
        if self.password != self.confirm {
            return Some("The passwords do not match.");
        }
        None
    }

    fn wipe(&mut self) {
        signin::wipe(&mut self.password);
        signin::wipe(&mut self.confirm);
        *self = Self::default();
    }
}

/// The page's state.
#[derive(Debug, Default)]
pub(crate) struct UsersUi {
    loaded: bool,
    /// Read the list again before the next frame: someone may have signed
    /// in or out since, which decides whether they can be removed.
    stale: bool,
    /// Who is using Settings.
    me: String,
    people: Vec<Person>,
    /// The person whose details are open, by user name.
    open: Option<String>,
    /// Their full name as typed.
    real_name: String,
    /// Whether "Remove" was pressed once, waiting for the second press.
    confirm_remove: bool,
    /// The add form, while it is open.
    adding: Option<NewPerson>,
    /// The password the quality check last judged, so it runs once per
    /// change rather than every frame.
    checked: Option<(String, String)>,
    running: Option<Running>,
    status: Option<(String, bool)>,
}

impl UsersUi {
    /// Reads who has an account; again after each change.
    fn load(&mut self) {
        self.loaded = true;
        self.me = derisk_sys::env::var("USER").unwrap_or_default();
        let json = Command::new("userdbctl")
            .args([
                "user",
                "--disposition=regular",
                "--json=short",
                "--no-pager",
            ])
            .stderr(Stdio::null())
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
            .unwrap_or_default();
        let groups = derisk_sys::fs::read_to_string("/etc/group").unwrap_or_default();
        let wheel: Vec<String> = group_members(&groups, ADMIN_GROUP)
            .map(str::to_owned)
            .collect();
        self.people = parse_people(&json, &wheel);
    }

    fn run(&mut self, job: Job) {
        self.status = None;
        self.confirm_remove = false;
        self.running = Some(signin::spawn(job));
    }

    fn poll(&mut self) {
        let Some(running) = &mut self.running else {
            return;
        };
        if let Some(result) = running.poll() {
            self.status = Some(result);
            self.running = None;
            self.load();
        }
    }

    /// Asks the quality check about the new password when it changed.
    fn check_password(&mut self) {
        let Some(new) = &mut self.adding else {
            return;
        };
        let key = (new.password.clone(), new.user_name.clone());
        if self.checked.as_ref() == Some(&key) {
            return;
        }
        new.weakness = PASSWORD_CHECK
            .get()
            .filter(|_| !new.password.is_empty())
            .and_then(|check| check(&new.password, &new.user_name))
            .map(|why| account::sentence(&why));
        if let Some((old, _)) = &mut self.checked {
            signin::wipe(old);
        }
        self.checked = Some(key);
    }
}

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(crate::layout::note(
        &mcsapi_components::Tokens::current(ui.ctx()),
        text,
    ));
}

/// A person's picture: their initial in a circle of the accent's colour.
fn avatar(ui: &mut egui::Ui, person: &Person, theme: &Theme) {
    let size = 36.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let painter = ui.painter();
    painter.circle_filled(rect.center(), size / 2.0, theme.accent.gamma_multiply(0.35));
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        person.initial(),
        egui::FontId::proportional(18.0),
        theme.foreground,
    );
}

pub(crate) fn page(ui: &mut egui::Ui, state: &mut UsersUi, theme: &Theme) {
    if !state.loaded || state.stale {
        state.stale = false;
        state.load();
    }
    state.poll();
    state.check_password();
    let busy = state.running.is_some();
    if let Some(running) = &state.running {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(running.shown());
        });
        ui.add_space(8.0);
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
    } else if let Some((status, error)) = &state.status {
        let color = if *error {
            egui::Color32::from_rgb(248, 113, 113)
        } else {
            theme.accent
        };
        ui.label(egui::RichText::new(status).color(color));
        ui.add_space(8.0);
    }
    let me_admin = state
        .people
        .iter()
        .any(|p| p.user_name == state.me && p.admin);
    if !me_admin {
        hint(
            ui,
            "Adding, removing and changing someone asks for an administrator's password.",
        );
        ui.add_space(8.0);
    }
    ui.add_enabled_ui(!busy, |ui| {
        let mut job = None;
        let people = state.people.clone();
        for person in &people {
            if let Some(next) = row(ui, state, &people, person, theme) {
                job = Some(next);
            }
            ui.add_space(4.0);
        }
        if people.is_empty() {
            hint(ui, "No accounts could be read.");
        }
        ui.add_space(12.0);
        if let Some(next) = add_form(ui, state, theme) {
            job = Some(next);
        }
        if let Some(job) = job {
            state.run(job);
        }
    });
}

/// One person: their picture, name and role, and, while open, their name
/// to edit, the role and Remove. Returns the change asked for.
fn row(
    ui: &mut egui::Ui,
    state: &mut UsersUi,
    people: &[Person],
    person: &Person,
    theme: &Theme,
) -> Option<Job> {
    let mut job = None;
    let me = person.user_name == state.me;
    let open = state.open.as_deref() == Some(person.user_name.as_str());
    let header = ui.horizontal(|ui| {
        avatar(ui, person, theme);
        ui.vertical(|ui| {
            ui.label(egui::RichText::new(person.display_name()).strong());
            let role = if person.admin {
                "Administrator"
            } else {
                "Standard"
            };
            let line = if me {
                format!("{} · {role} · You", person.user_name)
            } else {
                format!("{} · {role}", person.user_name)
            };
            hint(ui, &line);
        });
        if person.homed {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = if open { "Done" } else { "Edit" };
                ui.button(label).clicked()
            })
            .inner
        } else {
            false
        }
    });
    if header.inner {
        if open {
            state.open = None;
        } else {
            state.open = Some(person.user_name.clone());
            state.real_name = person.real_name.clone();
            state.stale = true;
        }
        state.confirm_remove = false;
    }
    if !person.homed {
        hint(ui, "This account is managed outside derisk.");
    }
    if !open || !person.homed {
        return None;
    }
    ui.indent(("user", &person.user_name), |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label("Full name");
            ui.text_edit_singleline(&mut state.real_name);
            let changed = state.real_name.trim() != person.real_name.trim()
                && !state.real_name.trim().is_empty()
                && !state.real_name.contains([':', '\n']);
            if ui
                .add_enabled(changed, egui::Button::new("Save name"))
                .clicked()
            {
                job = Some(rename(&person.user_name, &state.me, &state.real_name));
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Account type");
            let blocked = last_admin(people, person);
            let mut admin = person.admin;
            ui.selectable_value(&mut admin, true, "Administrator");
            ui.add_enabled_ui(blocked.is_none(), |ui| {
                ui.selectable_value(&mut admin, false, "Standard");
            });
            if admin != person.admin {
                job = Some(set_admin(person, &state.me, admin));
            }
            if let Some(why) = blocked {
                hint(ui, why);
            }
        });
        if me {
            hint(
                ui,
                "You cannot remove your own account. Your password is on the Sign-in page.",
            );
        } else if let Some(why) = last_admin(people, person) {
            hint(ui, why);
        } else if person.in_use && state.confirm_remove {
            hint(
                ui,
                "Signing them out closes their apps and their virtual machines. Anything they have not saved is lost.",
            );
            ui.horizontal(|ui| {
                if ui
                    .button(format!("Sign {} out", person.display_name()))
                    .clicked()
                {
                    job = Some(sign_out(&person.user_name));
                }
                if ui.button("Keep").clicked() {
                    state.confirm_remove = false;
                }
            });
        } else if person.in_use {
            hint(
                ui,
                &format!(
                    "{} is signed in. Their account can be removed once they sign out.",
                    person.display_name()
                ),
            );
            if ui.button("Sign them out…").clicked() {
                state.confirm_remove = true;
                state.stale = true;
            }
        } else if state.confirm_remove {
            hint(
                ui,
                "Their account and everything in their home folder will be deleted, their virtual machines too. This cannot be undone.",
            );
            ui.horizontal(|ui| {
                let remove_now = egui::Button::new(
                    egui::RichText::new(format!("Remove {}", person.display_name()))
                        .color(egui::Color32::from_rgb(248, 113, 113)),
                );
                if ui.add(remove_now).clicked() {
                    job = Some(remove(&person.user_name));
                }
                if ui.button("Keep").clicked() {
                    state.confirm_remove = false;
                }
            });
        } else if ui.button("Remove account…").clicked() {
            // They may have signed in since the list was read; the next
            // frame offers to sign them out instead if so.
            state.confirm_remove = true;
            state.stale = true;
        }
    });
    job
}

/// "Add someone", and the form it opens. Returns the account to make.
fn add_form(ui: &mut egui::Ui, state: &mut UsersUi, theme: &Theme) -> Option<Job> {
    let taken: Vec<String> = state.people.iter().map(|p| p.user_name.clone()).collect();
    let Some(new) = &mut state.adding else {
        if ui.button("Add someone…").clicked() {
            state.adding = Some(NewPerson::default());
            state.checked = None;
            // The last change's outcome is not about this one.
            state.status = None;
        }
        return None;
    };
    ui.heading(egui::RichText::new("Add someone").color(theme.foreground));
    let mut job = None;
    let (label, gap) = crate::layout::label_width(ui.available_width());
    egui::Grid::new("users-add")
        .num_columns(2)
        .min_col_width(label)
        .spacing([gap, 12.0])
        .show(ui, |ui| {
            ui.label("Full name");
            let mut real_name = new.real_name.clone();
            if ui.text_edit_singleline(&mut real_name).changed() {
                new.set_real_name(real_name);
            }
            ui.end_row();
            ui.label("User name");
            if ui.text_edit_singleline(&mut new.user_name).changed() {
                new.user_name_edited = true;
            }
            ui.end_row();
            ui.label("Password");
            ui.add(egui::TextEdit::singleline(&mut new.password).password(true));
            ui.end_row();
            ui.label("Confirm");
            ui.add(egui::TextEdit::singleline(&mut new.confirm).password(true));
            ui.end_row();
            ui.label("Account type");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut new.admin, false, "Standard");
                ui.selectable_value(&mut new.admin, true, "Administrator");
            });
            ui.end_row();
        });
    let problem = new
        .problem(taken.iter().map(String::as_str))
        .map(str::to_owned);
    hint(
        ui,
        problem.as_deref().unwrap_or(if new.admin {
            "They can change the system and other people's accounts."
        } else {
            "They can use apps and change their own settings."
        }),
    );
    let mut close = false;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(problem.is_none(), egui::Button::new("Add"))
            .clicked()
        {
            let extra = account::device_groups(std::path::Path::new("/etc/group"));
            job = Some(create(new, &extra));
        }
        close = ui.button("Cancel").clicked() || job.is_some();
    });
    if close {
        new.wipe();
        state.adding = None;
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shaped as userdbctl prints homed's records: the service and the home
    // area's state are in the status section, under the machine ID.
    const USERDB: &str = "\u{1e}{\"userName\":\"ada\",\"realName\":\"Ada Lovelace\",\
        \"memberOf\":[\"wheel\",\"networkmanager\"],\"status\":{\"4d25\":\
        {\"state\":\"inactive\",\"service\":\"io.systemd.Home\"}}}\n\
        \u{1e}{\"userName\":\"bob\",\"realName\":\"Bob\",\"status\":{\"4d25\":\
        {\"state\":\"active\",\"service\":\"io.systemd.Home\"}}}\n\
        \u{1e}{\"userName\":\"old\",\"realName\":\"\"}\n";

    #[test]
    fn people_come_from_userdb_with_their_role() {
        let people = parse_people(USERDB, &["old".into()]);
        let names: Vec<_> = people.iter().map(Person::display_name).collect();
        assert_eq!(names, ["Ada Lovelace", "Bob", "old"]);
        assert!(people[0].admin && people[0].homed);
        assert!(!people[1].admin);
        // Bob is signed in; Ada's home area is closed.
        assert!(people[1].in_use && !people[0].in_use);
        // /etc/group makes a passwd user an administrator; homed doesn't
        // keep them, so the page leaves them alone.
        assert!(people[2].admin && !people[2].homed);
        assert_eq!(people[1].initial(), "B");
    }

    #[test]
    fn group_file_members_are_read() {
        let group = "root:x:0:\nwheel:x:1:ada, bob\nusers:x:100:carol\n";
        assert_eq!(
            group_members(group, "wheel").collect::<Vec<_>>(),
            ["ada", "bob"]
        );
        assert_eq!(group_members(group, "root").count(), 0);
    }

    #[test]
    fn the_role_changes_only_wheel() {
        let people = parse_people(USERDB, &[]);
        let ada = &people[0];
        assert_eq!(ada.groups_with_admin(false), ["networkmanager"]);
        assert_eq!(ada.groups_with_admin(true), ["wheel", "networkmanager"]);
        let job = set_admin(&people[1], "ada", true);
        assert_eq!(
            job.argv,
            ["homectl", "update", "bob", "--offline", "--member-of=wheel"]
        );
        // One's own home is open, so it changes in place.
        let job = rename("ada", "ada", " Ada King ");
        assert_eq!(
            job.argv,
            ["homectl", "update", "ada", "--real-name=Ada King"]
        );
    }

    #[test]
    fn signing_out_ends_every_session() {
        assert_eq!(sign_out("bob").argv, ["loginctl", "terminate-user", "bob"]);
    }

    #[test]
    fn the_last_administrator_stays_one() {
        let people = parse_people(USERDB, &[]);
        assert!(last_admin(&people, &people[0]).is_some());
        assert!(last_admin(&people, &people[1]).is_none());
        let both = parse_people(USERDB, &["bob".into()]);
        assert!(last_admin(&both, &both[0]).is_none());
    }

    #[test]
    fn a_new_person_is_checked_before_homectl_runs() {
        let mut new = NewPerson::default();
        assert_eq!(new.problem([]), Some("Enter their name."));
        new.set_real_name("Bob Builder".into());
        assert_eq!(new.user_name, "bob");
        assert!(new.problem(["bob"]).unwrap().contains("already"));
        assert_eq!(new.problem([]), Some("Choose a password for them."));
        new.password = "correct horse".into();
        assert_eq!(new.problem([]), Some("The passwords do not match."));
        new.confirm = "correct horse".into();
        assert_eq!(new.problem([]), None);
        let job = create(&new, &["networkmanager".into()]);
        assert_eq!(
            job.argv,
            [
                "homectl",
                "create",
                "bob",
                "--real-name=Bob Builder",
                "--member-of=networkmanager"
            ]
        );
        assert_eq!(job.env, [("NEWPASSWORD".into(), "correct horse".into())]);
        new.admin = true;
        assert!(create(&new, &[]).argv.contains(&"--member-of=wheel".into()));
        assert!(!format!("{new:?}").contains("horse"));
    }
}
