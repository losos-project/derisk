//! How often a password must be changed, as the system sets it.
//!
//! The policy is `/etc/derisk/sign-in.conf`, `key = value` lines:
//!
//! ```text
//! password.max_age_days = 365
//! password.warn_days = 14
//! ```
//!
//! A maximum of 0, or no file, means a password never expires. derisk does
//! not enforce the age itself: it writes it into the account's systemd-homed
//! record (`passwordChangeMaxUSec`, `passwordChangeWarnUSec`), and from there
//! pam_systemd_home warns before the date and, once it passed, makes the
//! next login change the password (`derisk display-manager` asks for the new
//! one in the same conversation). First-boot setup and Settings' Users page
//! write it when they create an account, and the display manager brings an
//! existing account's record up to the policy when that person logs in,
//! because only an administrator (or root) may change these fields: a
//! person cannot opt out of the policy by editing their own record.
//!
//! ```
//! use derisk::password_age::PasswordAge;
//!
//! let age = PasswordAge::parse("password.max_age_days = 90\npassword.warn_days = 7\n");
//! assert_eq!(
//!     age.homectl_args().collect::<Vec<_>>(),
//!     ["--password-change-max=90d", "--password-change-warn=7d"]
//! );
//! ```

use std::path::Path;

/// Where the system's policy lives.
pub const POLICY_PATH: &str = "/etc/derisk/sign-in.conf";

const DAY_USEC: u64 = 24 * 60 * 60 * 1_000_000;

/// The password age the system asks for.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PasswordAge {
    /// Days a password lasts; 0 for ever.
    pub max_days: u32,
    /// Days before that the person is warned.
    pub warn_days: u32,
}

impl PasswordAge {
    /// Reads `key = value` lines; unknown keys and values that are not whole
    /// numbers are ignored, so a garbled file leaves passwords unexpiring
    /// rather than expiring them by accident.
    pub fn parse(text: &str) -> Self {
        let mut age = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let Ok(value) = value.trim().parse() else {
                continue;
            };
            match key.trim() {
                "password.max_age_days" => age.max_days = value,
                "password.warn_days" => age.warn_days = value,
                _ => {}
            }
        }
        age
    }

    /// The policy at `path`; none when it cannot be read.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    /// The system's policy, from [`POLICY_PATH`].
    pub fn system() -> Self {
        Self::load(Path::new(POLICY_PATH))
    }

    /// `homectl create` and `update` options that set this age. With no
    /// maximum they clear both fields, so turning the policy off reaches
    /// accounts made under it.
    pub fn homectl_args(&self) -> impl Iterator<Item = String> + use<> {
        if self.max_days == 0 {
            return [
                "--password-change-max=".into(),
                "--password-change-warn=".into(),
            ]
            .into_iter();
        }
        [
            format!("--password-change-max={}d", self.max_days),
            format!("--password-change-warn={}d", self.warn_days),
        ]
        .into_iter()
    }

    /// Whether a homed record (`homectl inspect --json=short`) already says
    /// what this policy says.
    pub fn matches(&self, record: &serde_json::Value) -> bool {
        let field = |name: &str| record.get(name).and_then(serde_json::Value::as_u64);
        let max = field("passwordChangeMaxUSec");
        let warn = field("passwordChangeWarnUSec");
        if self.max_days == 0 {
            return max.is_none() && warn.is_none();
        }
        max == Some(u64::from(self.max_days) * DAY_USEC)
            && warn == Some(u64::from(self.warn_days) * DAY_USEC)
    }

    /// The `homectl update` command that brings `user`'s record to this
    /// policy, or `None` when it is there already or is not a homed record
    /// (one with no `userName`, which `homectl inspect` printed nothing for).
    pub fn update_argv(&self, user: &str, record: &serde_json::Value) -> Option<Vec<String>> {
        if record.get("userName").is_none() || self.matches(record) {
            return None;
        }
        let mut argv = vec!["homectl".into(), "update".into(), user.into()];
        argv.extend(self.homectl_args());
        Some(argv)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn no_file_or_garbage_never_expires() {
        assert_eq!(PasswordAge::parse(""), PasswordAge::default());
        assert_eq!(
            PasswordAge::parse("password.max_age_days = soon\nnonsense\n"),
            PasswordAge::default()
        );
        assert_eq!(
            PasswordAge::load(Path::new("/nonexistent/sign-in.conf")).max_days,
            0
        );
    }

    #[test]
    fn records_are_compared_in_microseconds() {
        let age = PasswordAge {
            max_days: 365,
            warn_days: 14,
        };
        let record = json!({
            "userName": "alice",
            "passwordChangeMaxUSec": 365 * DAY_USEC,
            "passwordChangeWarnUSec": 14 * DAY_USEC,
        });
        assert!(age.matches(&record));
        assert_eq!(age.update_argv("alice", &record), None);
        let older = json!({"userName": "alice", "passwordChangeMaxUSec": 90 * DAY_USEC});
        assert_eq!(
            age.update_argv("alice", &older).unwrap(),
            [
                "homectl",
                "update",
                "alice",
                "--password-change-max=365d",
                "--password-change-warn=14d"
            ]
        );
    }

    #[test]
    fn turning_the_policy_off_clears_records() {
        let off = PasswordAge::default();
        assert!(off.matches(&json!({"userName": "alice"})));
        let under_policy = json!({"userName": "alice", "passwordChangeMaxUSec": DAY_USEC});
        assert_eq!(
            off.update_argv("alice", &under_policy).unwrap()[3..],
            ["--password-change-max=", "--password-change-warn="]
        );
    }

    #[test]
    fn accounts_homed_does_not_know_are_left_alone() {
        let age = PasswordAge {
            max_days: 30,
            warn_days: 3,
        };
        assert_eq!(age.update_argv("root", &serde_json::Value::Null), None);
    }
}
