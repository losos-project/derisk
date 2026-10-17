//! The first login after first-boot setup, straight to the desktop.
//!
//! `derisk setup` has just made the account and knows its password; asking
//! for it again on the login screen a minute later only shows someone the
//! machine they set up a moment ago in a less friendly light. So setup
//! leaves a ticket for the display manager: the user and the password, in a
//! root-only file on `/run` (a tmpfs, so it never reaches a disk). The
//! display manager takes it on its next round, deletes it before anything
//! else, and logs that user in through the same PAM stack the login screen
//! uses, so a homed home area is unlocked with the password as usual.
//!
//! Only that first login skips the login screen. Every later boot, and
//! every log out, shows it as before. A ticket that is not root's, not
//! private, a symlink, or older than [`MAX_AGE`] is deleted and ignored.

use std::{
    fs,
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
    time::{Duration, SystemTime},
};

/// Where setup leaves the ticket.
pub const PATH: &str = "/run/derisk-first-login";

/// How long a ticket stays good: setup hands over to the display manager
/// within seconds, so anything older was left behind by a crash.
pub const MAX_AGE: Duration = Duration::from_secs(10 * 60);

/// Who to log in, and with what.
pub struct Ticket {
    /// The account setup made.
    pub user: String,
    password: String,
}

impl Ticket {
    /// The password, for PAM's conversation.
    pub fn password(&self) -> &str {
        &self.password
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        wipe(&mut self.password);
    }
}

fn wipe(secret: &mut String) {
    let mut bytes = std::mem::take(secret).into_bytes();
    bytes.fill(0);
    // The zeroes are written before the allocation is freed.
    std::hint::black_box(&bytes);
}

/// Leaves a ticket for `user` at `path`, readable by its owner only. Fails
/// rather than replacing a file already there.
pub fn write(path: &Path, user: &str, password: &str) -> io::Result<()> {
    if user.is_empty() || user.contains('\0') || password.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a user name and password without NUL",
        ));
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    let mut body = format!("{user}\0{password}").into_bytes();
    let written = file.write_all(&body).and_then(|()| file.sync_all());
    body.fill(0);
    if written.is_err() {
        let _ = fs::remove_file(path);
    }
    written
}

/// Takes the ticket at `path`, if there is a good one owned by `owner`:
/// the file is gone afterwards either way.
pub fn take(path: &Path, owner: u32) -> Option<Ticket> {
    let file = fs::OpenOptions::new()
        .read(true)
        // A symlink planted where the ticket goes is not followed.
        .custom_flags(libc::O_NOFOLLOW)
        .open(path);
    // Whatever it held, it is used once at most.
    let _ = fs::remove_file(path);
    let file = file.ok()?;
    let meta = file.metadata().ok()?;
    let fresh = meta
        .modified()
        .ok()
        .and_then(|m| SystemTime::now().duration_since(m).ok())
        .is_some_and(|age| age <= MAX_AGE);
    if !meta.is_file() || meta.uid() != owner || meta.permissions().mode() & 0o077 != 0 || !fresh {
        return None;
    }
    let mut body = String::new();
    file.take(4096).read_to_string(&mut body).ok()?;
    let ticket = body.split_once('\0').and_then(|(user, password)| {
        (!user.is_empty()).then(|| Ticket {
            user: user.to_owned(),
            password: password.to_owned(),
        })
    });
    wipe(&mut body);
    ticket
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> u32 {
        fs::metadata("/proc/self").map(|m| m.uid()).unwrap_or(0)
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("derisk-first-login-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn a_ticket_is_taken_once() {
        let path = scratch("once");
        write(&path, "ada", "correct horse").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let ticket = take(&path, me()).unwrap();
        assert_eq!(ticket.user, "ada");
        assert_eq!(ticket.password(), "correct horse");
        assert!(!path.exists());
        assert!(take(&path, me()).is_none());
    }

    #[test]
    fn someone_elses_or_a_shared_ticket_is_refused() {
        let path = scratch("owner");
        write(&path, "ada", "pw").unwrap();
        assert!(take(&path, me() + 1).is_none());
        assert!(!path.exists(), "a refused ticket is deleted too");
        write(&path, "ada", "pw").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(take(&path, me()).is_none());
    }

    #[test]
    fn a_symlink_is_not_followed() {
        let target = scratch("target");
        write(&target, "ada", "pw").unwrap();
        let link = scratch("link");
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(take(&link, me()).is_none());
        assert!(target.exists());
    }

    #[test]
    fn an_existing_ticket_is_not_replaced() {
        let path = scratch("exists");
        let _ = fs::remove_file(&path);
        write(&path, "ada", "pw").unwrap();
        assert!(write(&path, "eve", "pw").is_err());
        assert_eq!(take(&path, me()).unwrap().user, "ada");
    }
}
