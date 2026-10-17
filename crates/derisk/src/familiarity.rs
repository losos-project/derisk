//! The session's side of [`derisk_settings::familiarity`]: it counts the
//! time the person spends at the desktop, keeps the count on disk, and
//! says how fast motion runs because of it.
//!
//! Only time in use counts: the session is unlocked and not dimmed, so a
//! desktop left alone stops counting once it dims, and a machine that slept
//! between two ticks adds nothing for the night. The count reaches the disk
//! once a minute; a session that ends loses at most that minute.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use derisk_settings::familiarity as count;
use tracing::warn;

/// How often the count reaches the disk.
const SAVE_EVERY: Duration = Duration::from_secs(60);

/// A gap between two ticks longer than this is the machine asleep or the
/// session stalled, not use.
const MAX_GAP: Duration = Duration::from_secs(5);

/// The time at the desktop, as counted so far.
#[derive(Debug)]
pub struct Familiarity {
    /// The count's file; `None` when there is nowhere to keep it, and then
    /// the session counts in memory only.
    path: Option<PathBuf>,
    /// What the file held when last read or written.
    saved: Duration,
    /// Counted since, not yet on disk.
    unsaved: Duration,
    last_tick: Option<Instant>,
}

impl Familiarity {
    /// Reads the count in `path`.
    pub fn open(path: Option<PathBuf>) -> Self {
        Self {
            saved: path.as_deref().map(count::read).unwrap_or_default(),
            path,
            unsaved: Duration::ZERO,
            last_tick: None,
        }
    }

    /// The time at the desktop so far.
    pub fn active(&self) -> Duration {
        self.saved + self.unsaved
    }

    /// How many times as fast as the film motion runs now.
    pub fn speed(&self) -> f32 {
        count::speed(self.active())
    }

    /// Counts the time since the last tick if the desktop was `in_use`,
    /// and saves the count when it is due. Called about once a second.
    pub fn tick(&mut self, now: Instant, in_use: bool) {
        let gap = self.last_tick.replace(now).map(|t| now.duration_since(t));
        if in_use && let Some(gap) = gap.filter(|g| *g <= MAX_GAP) {
            self.unsaved += gap;
        }
        let Some(path) = &self.path else {
            return;
        };
        // Settings › Appearance started over: motion runs as filmed again
        // from the next frame, not from the next save.
        if self.saved > Duration::ZERO && !path.exists() {
            self.saved = Duration::ZERO;
            self.unsaved = Duration::ZERO;
        }
        if self.unsaved < SAVE_EVERY {
            return;
        }
        match count::add(path, self.unsaved) {
            Ok(total) => self.saved = total,
            Err(e) => {
                // Warned once: from here on it counts in memory only.
                warn!(
                    "{}: {e}; animations quicken for this session only",
                    path.display()
                );
                self.saved += self.unsaved;
                self.path = None;
            }
        }
        self.unsaved = Duration::ZERO;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_time_in_use_counts_and_it_reaches_the_disk() {
        let dir = std::env::temp_dir().join(format!("derisk-familiar-{}", std::process::id()));
        let path = dir.join("familiarity");
        let mut f = Familiarity::open(Some(path.clone()));
        let start = Instant::now();
        let s = |n: u64| start + Duration::from_secs(n);
        f.tick(s(0), true);
        for n in 1..=30 {
            f.tick(s(n), true);
        }
        // Dimmed or locked.
        for n in 31..=50 {
            f.tick(s(n), false);
        }
        // Asleep overnight.
        f.tick(s(50_000), true);
        for n in 50_001..=50_030 {
            f.tick(s(n), true);
        }
        assert_eq!(f.active(), Duration::from_secs(60));
        assert_eq!(count::read(&path), Duration::from_secs(60));
        // Started over in Settings.
        count::reset(&path).unwrap();
        f.tick(s(50_031), true);
        assert_eq!(f.active(), Duration::ZERO);
        assert_eq!(f.speed(), 1.0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
