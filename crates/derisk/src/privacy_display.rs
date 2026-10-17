//! The privacy display: Samsung's of the same name, done in software.
//!
//! Samsung's turns on under conditions the user picks (while chosen apps
//! are open, while a PIN or password is entered, over notification pop-ups)
//! and then narrows the panel's viewing angle; its maximum protection also
//! dims the screen. A PC's panel cannot narrow its angle, so here the same
//! conditions start the camera instead, and the protected part of the
//! screen is covered only while someone else is looking at it:
//!
//! - [`protected`] says which part a condition protects now: the whole
//!   screen for a password or a chosen app, the pop-ups alone for a
//!   notification.
//! - While it says any, the session runs `derisk-faces watch`, which counts
//!   the faces in a few small camera frames a second and prints each count
//!   ([`Report`]). It stops as soon as no condition holds, so the camera is
//!   on only then; frames never leave that process's memory.
//! - [`Onlooker`] turns the counts into "someone else is looking": a second
//!   face in two searches in a row, gone again after [`CLEAR_AFTER`] without
//!   one, so a passer-by flickering in and out of view does not flash the
//!   cover.
//! - [`PrivacyDisplay::update`] puts it together into what to cover, which
//!   the shell draws as an opaque sheet with a line saying why and a "Show
//!   anyway" button that holds until the condition ends.
//!
//! Off unless the user turns it on in Settings, on the Privacy page.

use std::time::{Duration, Instant};

use derisk_settings::PrivacyDisplay as Prefs;

/// The helper that counts faces, and how it is run.
pub const HELPER: [&str; 2] = ["derisk-faces", "watch"];

/// How many searches in a row must see a second face before the screen is
/// covered: one could be a face on a poster caught at an angle.
pub const ONLOOKER_SEARCHES: u32 = 2;

/// How long without a second face before the cover lifts.
pub const CLEAR_AFTER: Duration = Duration::from_millis(1500);

/// What a condition protects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Area {
    /// The whole screen.
    Screen,
    /// The notification pop-ups on screen, and nothing else.
    Notifications,
}

/// What is on screen, for the conditions.
#[derive(Clone, Copy, Debug, Default)]
pub struct Situation<'a> {
    /// A password is asked for: the lock screen, or a polkit dialog.
    pub password: bool,
    /// The focused window's app ID.
    pub focused_app: Option<&'a str>,
    /// A notification pop-up is on screen.
    pub notifications: bool,
}

/// The app ID a desktop file ID names: without `.desktop`.
fn app(id: &str) -> &str {
    id.strip_suffix(".desktop").unwrap_or(id)
}

/// What `prefs` protects in `now`, if anything: the screen wins over the
/// pop-ups, since it covers them too.
pub fn protected(prefs: &Prefs, now: &Situation<'_>) -> Option<Area> {
    if !prefs.enabled {
        return None;
    }
    let chosen = now
        .focused_app
        .is_some_and(|focused| prefs.apps.iter().any(|a| app(a) == app(focused)));
    if (prefs.passwords && now.password) || chosen {
        Some(Area::Screen)
    } else if prefs.notifications && now.notifications {
        Some(Area::Notifications)
    } else {
        None
    }
}

/// A line of the helper's output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Report {
    /// How many faces the last search found.
    Faces(u32),
    /// There is no camera; the helper has exited.
    NoCamera,
}

impl Report {
    /// The report `line` makes, if it is one.
    pub fn parse(line: &str) -> Option<Self> {
        match line.trim().split_once(' ')? {
            ("faces", n) => n.parse().ok().map(Self::Faces),
            ("camera", "none") => Some(Self::NoCamera),
            _ => None,
        }
    }
}

/// Whether someone besides the user is looking, from the face counts.
#[derive(Clone, Copy, Debug, Default)]
pub struct Onlooker {
    /// Whether any search has reported yet.
    searched: bool,
    /// Searches in a row that saw a second face.
    streak: u32,
    /// When a second face was last seen.
    seen: Option<Instant>,
}

impl Onlooker {
    /// Takes the count of one search at `now`.
    pub fn faces(&mut self, faces: u32, now: Instant) {
        self.searched = true;
        if faces >= 2 {
            self.streak = self.streak.saturating_add(1);
            if self.streak >= ONLOOKER_SEARCHES {
                self.seen = Some(now);
            }
        } else {
            self.streak = 0;
        }
    }

    /// Whether someone else is looking at `now`.
    pub fn looking(&self, now: Instant) -> bool {
        self.seen
            .is_some_and(|seen| now.saturating_duration_since(seen) < CLEAR_AFTER)
    }

    /// Whether it cannot yet say nobody else is looking: no search has
    /// reported, or the last saw a second face that is not yet confirmed.
    pub fn unsure(&self) -> bool {
        !self.searched || self.streak > 0
    }
}

/// What the privacy display wants this frame.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Frame {
    /// Run the camera's helper.
    pub camera: bool,
    /// Cover this.
    pub cover: Option<Area>,
    /// Turn the backlight down too (maximum protection while covered).
    pub dim: bool,
}

/// The privacy display's state across frames.
#[derive(Debug, Default)]
pub struct PrivacyDisplay {
    onlooker: Onlooker,
    /// "Show anyway" was pressed; it holds until no condition does.
    shown_anyway: bool,
    /// The helper found no camera, or stopped; it is not started again
    /// until no condition holds.
    camera_gone: bool,
    area: Option<Area>,
}

impl PrivacyDisplay {
    /// Takes a line the helper printed.
    pub fn report(&mut self, report: Report, now: Instant) {
        match report {
            Report::Faces(n) => self.onlooker.faces(n, now),
            Report::NoCamera => self.camera_gone = true,
        }
    }

    /// The helper exited.
    pub fn camera_stopped(&mut self) {
        self.camera_gone = true;
    }

    /// Uncovers until the condition ends.
    pub fn show_anyway(&mut self) {
        self.shown_anyway = true;
    }

    /// What to do this frame, with `prefs` and what `now` has on screen.
    pub fn update(&mut self, prefs: &Prefs, situation: &Situation<'_>, now: Instant) -> Frame {
        let area = protected(prefs, situation);
        if area.is_none() {
            // Nothing to protect: forget everything, so the next condition
            // starts afresh with the camera off until then.
            *self = Self::default();
            return Frame::default();
        }
        self.area = area;
        // A pop-up is on screen from its first frame, before the camera
        // has had a look: it stays hidden until a search says nobody else
        // is there. The whole screen is not covered on a guess, which
        // would flash the cover at every lock.
        let unsure =
            area == Some(Area::Notifications) && !self.camera_gone && self.onlooker.unsure();
        let cover = ((self.onlooker.looking(now) || unsure) && !self.shown_anyway)
            .then_some(area)
            .flatten();
        Frame {
            camera: !self.camera_gone,
            cover,
            dim: cover.is_some() && prefs.maximum,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs() -> Prefs {
        Prefs {
            enabled: true,
            apps: vec!["org.mozilla.firefox.desktop".into()],
            ..Prefs::default()
        }
    }

    #[test]
    fn conditions_pick_what_to_protect() {
        let p = prefs();
        let none = Situation::default();
        assert_eq!(protected(&p, &none), None);
        let password = Situation {
            password: true,
            ..none
        };
        assert_eq!(protected(&p, &password), Some(Area::Screen));
        let firefox = Situation {
            focused_app: Some("org.mozilla.firefox"),
            ..none
        };
        assert_eq!(protected(&p, &firefox), Some(Area::Screen));
        let other = Situation {
            focused_app: Some("org.derisk.files"),
            notifications: true,
            ..none
        };
        assert_eq!(protected(&p, &other), Some(Area::Notifications));
        let off = Prefs {
            enabled: false,
            ..prefs()
        };
        assert_eq!(protected(&off, &password), None);
        let no_passwords = Prefs {
            passwords: false,
            ..prefs()
        };
        assert_eq!(protected(&no_passwords, &password), None);
    }

    #[test]
    fn the_helper_speaks_in_lines() {
        assert_eq!(Report::parse("faces 2\n"), Some(Report::Faces(2)));
        assert_eq!(Report::parse("camera none"), Some(Report::NoCamera));
        assert_eq!(Report::parse("faces many"), None);
        assert_eq!(Report::parse(""), None);
    }

    #[test]
    fn a_second_face_twice_covers_and_lifts_a_moment_after_it_goes() {
        let p = Prefs {
            maximum: true,
            ..prefs()
        };
        let s = Situation {
            password: true,
            ..Situation::default()
        };
        let mut d = PrivacyDisplay::default();
        let t = Instant::now();
        assert_eq!(
            d.update(&p, &s, t),
            Frame {
                camera: true,
                cover: None,
                dim: false
            }
        );
        d.report(Report::Faces(1), t);
        d.report(Report::Faces(2), t);
        assert_eq!(d.update(&p, &s, t).cover, None, "one search is not enough");
        d.report(Report::Faces(2), t);
        let covered = d.update(&p, &s, t);
        assert_eq!(covered.cover, Some(Area::Screen));
        assert!(covered.dim);
        d.report(Report::Faces(1), t + Duration::from_millis(250));
        assert!(
            d.update(&p, &s, t + Duration::from_millis(500))
                .cover
                .is_some()
        );
        assert_eq!(d.update(&p, &s, t + CLEAR_AFTER).cover, None);
    }

    #[test]
    fn a_pop_up_stays_hidden_until_the_camera_has_looked() {
        let p = prefs();
        let s = Situation {
            notifications: true,
            ..Situation::default()
        };
        let mut d = PrivacyDisplay::default();
        let t = Instant::now();
        assert_eq!(d.update(&p, &s, t).cover, Some(Area::Notifications));
        d.report(Report::Faces(1), t);
        assert_eq!(d.update(&p, &s, t).cover, None);
        d.report(Report::Faces(2), t);
        assert_eq!(d.update(&p, &s, t).cover, Some(Area::Notifications));
        d.report(Report::Faces(1), t);
        assert_eq!(d.update(&p, &s, t).cover, None, "one search is not enough");
        // Without a camera there is nothing to wait for.
        let mut d = PrivacyDisplay::default();
        d.update(&p, &s, t);
        d.report(Report::NoCamera, t);
        assert_eq!(d.update(&p, &s, t).cover, None);
    }

    #[test]
    fn show_anyway_holds_until_the_condition_ends() {
        let p = prefs();
        let s = Situation {
            password: true,
            ..Situation::default()
        };
        let mut d = PrivacyDisplay::default();
        let t = Instant::now();
        d.update(&p, &s, t);
        d.report(Report::Faces(3), t);
        d.report(Report::Faces(3), t);
        assert!(d.update(&p, &s, t).cover.is_some());
        d.show_anyway();
        assert_eq!(d.update(&p, &s, t).cover, None);
        // The prompt goes, and with it the camera and the exception.
        assert_eq!(d.update(&p, &Situation::default(), t), Frame::default());
        d.update(&p, &s, t);
        d.report(Report::Faces(2), t);
        d.report(Report::Faces(2), t);
        assert!(d.update(&p, &s, t).cover.is_some());
    }

    #[test]
    fn no_camera_means_no_retrying_until_the_condition_ends() {
        let p = prefs();
        let s = Situation {
            password: true,
            ..Situation::default()
        };
        let mut d = PrivacyDisplay::default();
        let t = Instant::now();
        assert!(d.update(&p, &s, t).camera);
        d.report(Report::NoCamera, t);
        assert!(!d.update(&p, &s, t).camera);
        d.update(&p, &Situation::default(), t);
        assert!(d.update(&p, &s, t).camera);
    }
}
