//! How used to the desktop someone is, and how much faster its motion runs
//! for them because of it.
//!
//! The launch film's timings suit someone seeing the desktop for the first
//! time: a window growing in over 22 frames shows where it came from. Seen
//! a thousand times, the same motion is only a wait. So the session counts
//! the time each person spends at the desktop (unlocked, and not left long
//! enough to dim) in `$XDG_STATE_HOME/derisk/familiarity`, and runs every
//! animation [`speed`] times as fast: as filmed for the first couple of
//! hours, then quicker, settling at [`MAX_SPEED`] after a few days of use.
//! Animations stay; they get shorter, so a familiar desktop still shows
//! where things went, only without waiting on it.
//!
//! The file belongs to its person, so each account settles on its own.
//! Settings › Appearance turns the quickening off (`appearance.settle_motion`)
//! and [`reset`]s the count, which the session notices within a second.
//!
//! ```
//! use std::time::Duration;
//! use derisk_settings::familiarity::{MAX_SPEED, speed};
//!
//! assert_eq!(speed(Duration::ZERO), 1.0);
//! let hours = |h: u64| Duration::from_secs(h * 3600);
//! assert!(speed(hours(10)) > 1.5 && speed(hours(10)) < speed(hours(30)));
//! assert!(speed(hours(200)) > MAX_SPEED - 0.01);
//! ```

use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use derisk_sys::fs;

/// Hours at the desktop before motion starts to quicken: while it is all
/// new, every animation runs as filmed.
const LEARNING_H: f64 = 2.0;

/// Hours, after [`LEARNING_H`], by which motion has made about two thirds
/// of its way to [`MAX_SPEED`]: a couple of days of steady use.
const SETTLE_H: f64 = 12.0;

/// The fastest motion runs, as a multiple of the film's speed. At four
/// times, a window opens in about 90 ms and one picked in the overview
/// grows out of its card in about 225: still seen, never waited on.
pub const MAX_SPEED: f32 = 4.0;

/// How many times as fast as the film motion runs after `active` at the
/// desktop: 1 for the first [`LEARNING_H`] hours, then easing towards
/// [`MAX_SPEED`].
pub fn speed(active: Duration) -> f32 {
    let past = active.as_secs_f64() / 3600.0 - LEARNING_H;
    if past <= 0.0 {
        return 1.0;
    }
    let settled = 1.0 - (-past / SETTLE_H).exp();
    1.0 + (MAX_SPEED - 1.0) * settled as f32
}

/// The count of time at the desktop: `$XDG_STATE_HOME/derisk/familiarity`,
/// falling back to `~/.local/state`. `None` when neither variable is set.
pub fn default_path() -> Option<PathBuf> {
    let state = derisk_sys::env::var_os("XDG_STATE_HOME")
        .filter(|dir| Path::new(dir).is_absolute())
        .map(PathBuf::from)
        .or_else(|| derisk_sys::env::var_os("HOME").map(|h| Path::new(&h).join(".local/state")))?;
    Some(state.join("derisk").join("familiarity"))
}

/// The time at the desktop counted in `path`. A missing or unreadable file
/// counts as none: the worst it does is show the film's motion again.
pub fn read(path: &Path) -> Duration {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| parse(&text))
        .unwrap_or_default()
}

fn parse(text: &str) -> Option<Duration> {
    text.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "active_seconds")
            .then(|| value.trim().parse().ok())
            .flatten()
            .map(Duration::from_secs)
    })
}

/// Adds `more` to the count in `path`, creating it, and returns the new
/// total. The file is read again first, so a [`reset`] made since, or a
/// second session of the same person, is added to rather than overwritten.
pub fn add(path: &Path, more: Duration) -> io::Result<Duration> {
    let total = read(path) + more;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    fs::write(
        &temporary,
        format!(
            "# Time spent at the desktop, which quickens its animations; kept by derisk.\n\
             active_seconds = {}\n",
            total.as_secs()
        ),
    )?;
    fs::rename(&temporary, path)?;
    Ok(total)
}

/// Starts the count over, so motion runs as filmed again.
pub fn reset(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn motion_runs_as_filmed_at_first_and_only_ever_quickens() {
        let hours = |h: f64| Duration::from_secs_f64(h * 3600.0);
        assert_eq!(speed(hours(1.9)), 1.0);
        let mut last = 1.0;
        for h in 3..300 {
            let s = speed(hours(f64::from(h)));
            // Never slower; from about 150 hours on, as fast as it gets.
            assert!(s >= last && s <= MAX_SPEED, "{h} h: {s}");
            assert!(s > last || h > 100, "{h} h: {s}");
            last = s;
        }
        assert!(speed(hours(14.0)) > 2.5 && speed(hours(14.0)) < 3.2);
    }

    #[test]
    fn the_count_adds_up_and_resets() {
        let dir =
            std::env::temp_dir().join(format!("derisk-familiarity-{}", derisk_sys::process::id()));
        let path = dir.join("state").join("familiarity");
        assert_eq!(read(&path), Duration::ZERO);
        assert_eq!(
            add(&path, Duration::from_secs(90)).unwrap(),
            Duration::from_secs(90)
        );
        assert_eq!(
            add(&path, Duration::from_secs(30)).unwrap(),
            Duration::from_secs(120)
        );
        assert_eq!(read(&path), Duration::from_secs(120));
        reset(&path).unwrap();
        reset(&path).unwrap();
        assert_eq!(read(&path), Duration::ZERO);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_garbled_file_counts_as_none() {
        assert_eq!(parse("active_seconds = lots\n"), None);
        assert_eq!(
            parse("# note\nactive_seconds=42\n"),
            Some(Duration::from_secs(42))
        );
    }
}
