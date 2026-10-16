//! Desktop notifications: what apps asked the shell to show through the
//! freedesktop Desktop Notifications spec (1.2), and which of them are on
//! screen.
//!
//! `derisk session` owns `org.freedesktop.Notifications` on the session bus
//! (the binary's `notifications` module); every call it takes lands here,
//! in a [`Stack`] the compositor thread keeps, and [`crate::ui`] draws the
//! cards [`Stack::shown`] picks. Nothing here talks D-Bus or draws, so the
//! rules the spec sets (ids, replacing in place, expiry, actions) are tested
//! without a bus or a compositor.
//!
//! ```
//! use std::time::{Duration, Instant};
//! use derisk::notification_stack::{Incoming, Reason, Stack};
//!
//! let mut stack = Stack::default();
//! let now = Instant::now();
//! let download = Incoming {
//!     app_name: "Danube".into(),
//!     summary: "report.pdf".into(),
//!     actions: vec!["cancel".into(), "Cancel".into()],
//!     expire_timeout: 0,
//!     ..Incoming::default()
//! };
//! let id = stack.notify(download.clone(), now);
//! // Updating the progress keeps the same card.
//! let again = stack.notify(Incoming { replaces_id: id, ..download }, now);
//! assert_eq!(again, id);
//! // It never expires; the app closes it when the download is done.
//! assert!(stack.expire(now + Duration::from_secs(3600)).is_empty());
//! assert_eq!(stack.close(id, Reason::Closed), Some(Reason::Closed));
//! ```

use std::time::{Duration, Instant};

/// How long a notification that leaves it to the server (`expire_timeout`
/// of -1) stays: long enough to read a summary and a line or two of body,
/// short enough that a few in a row do not pile up. GNOME and dunst use 4
/// to 10 seconds; 5 is in the middle, and what the shell's own toasts use.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// How many cards are on screen at once. More would run down a laptop's
/// screen; the rest wait their turn behind them, still counting down.
pub const MAX_SHOWN: usize = 5;

/// How long a card stays after the pointer leaves it, when its time ran
/// out while the person was reading it.
pub const HOLD: Duration = Duration::from_secs(2);

/// How much of a summary or body is kept, in bytes. A card shows a few
/// lines of either; laying out megabytes an app sent every frame would
/// stall the desktop.
pub const MAX_TEXT: usize = 4096;

/// `text` cut to [`MAX_TEXT`] bytes, at a character's edge.
fn capped(mut text: String) -> String {
    if text.len() > MAX_TEXT {
        let mut end = MAX_TEXT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// The action key the spec reserves for clicking the notification itself.
pub const DEFAULT_ACTION: &str = "default";

/// A hint's value, as the shell reads it from the D-Bus variant. Apps do
/// not agree on integer widths (the spec says byte for `urgency` and int
/// for `value`, and some send other widths), so every integer is an
/// [`Hint::Int`] and the parsers take any of them.
#[derive(Clone, Debug, PartialEq)]
pub enum Hint {
    /// A boolean.
    Bool(bool),
    /// Any integer type.
    Int(i64),
    /// A string.
    Str(String),
    /// Anything else (`image-data`, sound files' paths as object paths, …),
    /// which the shell does not read.
    Other,
}

impl Hint {
    fn int(&self) -> Option<i64> {
        match self {
            Self::Int(n) => Some(*n),
            _ => None,
        }
    }

    /// A flag: `true`, or a non-zero integer from an app that sent one
    /// (`notify-send -h int:transient:1` is a common spelling).
    fn flag(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            Self::Int(n) => Some(*n != 0),
            _ => None,
        }
    }

    fn text(&self) -> Option<&str> {
        match self {
            Self::Str(s) if !s.is_empty() => Some(s),
            _ => None,
        }
    }
}

/// How urgent a notification is (the `urgency` hint).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, PartialOrd, Ord)]
pub enum Urgency {
    /// 0: news the person can miss.
    Low,
    /// 1: the default.
    #[default]
    Normal,
    /// 2: something that needs the person. It stays until dismissed or
    /// closed, whatever `expire_timeout` says, as the spec asks.
    Critical,
}

/// The hints the shell acts on. Unknown hints are ignored.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hints {
    /// `urgency`.
    pub urgency: Urgency,
    /// `value`, 0 to 100: draws a progress bar. Not in the spec, but GNOME,
    /// KDE, dunst and swaync all read it, so apps send it.
    pub value: Option<u8>,
    /// `resident`: invoking an action leaves the notification up.
    pub resident: bool,
    /// `transient`: it must not wait around. Here that means it always
    /// expires, even with an `expire_timeout` of 0 (but not when critical).
    pub transient: bool,
    /// `category`, such as `transfer` or `email.arrived`.
    pub category: Option<String>,
    /// `desktop-entry`: the sending app's desktop file ID, without
    /// `.desktop`, which names the app when `app_name` is empty.
    pub desktop_entry: Option<String>,
}

impl Hints {
    /// Reads the hints the shell knows from `hints`, ignoring the rest and
    /// any of the right name with a value of the wrong type.
    pub fn parse<'a>(hints: impl IntoIterator<Item = (&'a str, &'a Hint)>) -> Self {
        let mut parsed = Self::default();
        for (name, value) in hints {
            match name {
                "urgency" => {
                    parsed.urgency = match value.int() {
                        Some(0) => Urgency::Low,
                        Some(2) => Urgency::Critical,
                        // 1, and nonsense: an unknown level is not
                        // promoted to one that would stay up for ever.
                        _ => Urgency::Normal,
                    };
                }
                "value" => {
                    if let Some(n) = value.int() {
                        parsed.value = Some(n.clamp(0, 100) as u8);
                    }
                }
                "resident" => parsed.resident = value.flag().unwrap_or(false),
                "transient" => parsed.transient = value.flag().unwrap_or(false),
                "category" => parsed.category = value.text().map(str::to_owned),
                "desktop-entry" => {
                    parsed.desktop_entry = value
                        .text()
                        .map(|id| id.strip_suffix(".desktop").unwrap_or(id).to_owned());
                }
                _ => {}
            }
        }
        parsed
    }
}

/// A `Notify` call's arguments, hints already read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Incoming {
    /// The sending app's name, possibly empty.
    pub app_name: String,
    /// The notification this one updates, or 0 for a new one.
    pub replaces_id: u32,
    /// An icon name or `file://` URI, possibly empty.
    pub app_icon: String,
    /// The one-line summary.
    pub summary: String,
    /// The body, plain text (the server does not offer `body-markup`).
    pub body: String,
    /// Action keys and labels, alternating, as the spec sends them.
    pub actions: Vec<String>,
    /// The hints.
    pub hints: Hints,
    /// Milliseconds until it expires: -1 for the server's default, 0 for
    /// never.
    pub expire_timeout: i32,
}

/// A button on a notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    /// What `ActionInvoked` sends back.
    pub key: String,
    /// What the button reads.
    pub label: String,
}

/// A notification the shell holds.
#[derive(Clone, Debug, PartialEq)]
pub struct Notification {
    /// Its id, from 1, never reused in a session.
    pub id: u32,
    /// Who sent it.
    pub app_name: String,
    /// The app's icon, as sent.
    pub app_icon: String,
    /// The summary.
    pub summary: String,
    /// The body.
    pub body: String,
    /// The buttons, in the order sent; the `default` action is not one.
    pub actions: Vec<Action>,
    /// Whether it has a `default` action, invoked by clicking the card.
    pub default_action: bool,
    /// The hints it was sent with.
    pub hints: Hints,
    /// When it expires; `None` when it waits for the person or the app.
    pub expires: Option<Instant>,
}

impl Notification {
    /// Whether it stays until the app closes it or the person dismisses it.
    pub fn persistent(&self) -> bool {
        self.expires.is_none()
    }

    /// Whether its card is drawn. Do not disturb hides banners that would
    /// go away by themselves, but not persistent ones: a download's
    /// progress or a critical warning is something the person is waiting
    /// on, or must see.
    pub fn banner(&self, do_not_disturb: bool) -> bool {
        !do_not_disturb || self.persistent()
    }
}

/// Why a notification closed, as `NotificationClosed` reports it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reason {
    /// 1: its time ran out.
    Expired = 1,
    /// 2: the person dismissed it (or invoked an action of a notification
    /// that is not resident).
    Dismissed = 2,
    /// 3: the app called `CloseNotification`.
    Closed = 3,
}

impl Reason {
    /// The spec's number for it.
    pub fn code(self) -> u32 {
        self as u32
    }
}

/// When a notification sent with `timeout` and `hints` at `now` expires.
pub fn expiry(timeout: i32, hints: &Hints, now: Instant) -> Option<Instant> {
    if hints.urgency == Urgency::Critical {
        return None;
    }
    match timeout {
        0 if !hints.transient => None,
        ms if ms > 0 => Some(now + Duration::from_millis(ms as u64)),
        // -1, the spec's "server default", and any other negative number,
        // which the spec leaves undefined; and a transient one with 0.
        _ => Some(now + DEFAULT_TIMEOUT),
    }
}

/// The spec's flat `[key, label, key, label, …]` as buttons, and whether
/// a `default` action is among them. A key without a label (an odd count)
/// is dropped; an empty label reads as its key, so the button is not blank.
fn actions(flat: &[String]) -> (Vec<Action>, bool) {
    let mut default = false;
    let (pairs, _odd) = flat.as_chunks::<2>();
    let buttons = pairs
        .iter()
        .filter_map(|[key, label]| {
            if key == DEFAULT_ACTION {
                default = true;
                return None;
            }
            let label = if label.is_empty() { key } else { label };
            Some(Action {
                key: key.clone(),
                label: label.clone(),
            })
        })
        .collect();
    (buttons, default)
}

/// What invoking an action did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Invoked {
    /// The notification is resident and stays.
    Kept,
    /// It closed, for [`Reason::Dismissed`].
    Closed,
}

/// Every notification the session holds, oldest first.
#[derive(Debug)]
pub struct Stack {
    next: u32,
    list: Vec<Notification>,
}

impl Default for Stack {
    fn default() -> Self {
        // 0 means "a new one" in `replaces_id`, so ids start at 1.
        Self {
            next: 1,
            list: Vec::new(),
        }
    }
}

impl Stack {
    /// Takes a `Notify` call at `now` and returns its id. A known
    /// `replaces_id` is updated in place: same id, same place in the stack
    /// (and so no new banner), its time counted again from now. An unknown
    /// one (closed meanwhile, or from before a restart) is a new
    /// notification with a new id, since ids are never reused.
    pub fn notify(&mut self, incoming: Incoming, now: Instant) -> u32 {
        let (buttons, default_action) = actions(&incoming.actions);
        let expires = expiry(incoming.expire_timeout, &incoming.hints, now);
        let replaced = (incoming.replaces_id != 0)
            .then(|| self.list.iter_mut().find(|n| n.id == incoming.replaces_id))
            .flatten();
        let id = match &replaced {
            Some(n) => n.id,
            None => {
                let id = self.next;
                // Four billion notifications in one session would wrap; 0
                // is still skipped then.
                self.next = self.next.checked_add(1).unwrap_or(1);
                id
            }
        };
        let notification = Notification {
            id,
            app_name: incoming.app_name,
            app_icon: incoming.app_icon,
            summary: capped(incoming.summary),
            body: capped(incoming.body),
            actions: buttons,
            default_action,
            hints: incoming.hints,
            expires,
        };
        match replaced {
            Some(slot) => *slot = notification,
            None => self.list.push(notification),
        }
        id
    }

    /// The notification `id`, if it is still here.
    pub fn get(&self, id: u32) -> Option<&Notification> {
        self.list.iter().find(|n| n.id == id)
    }

    /// Removes `id` and returns `reason`, or `None` when it was not here
    /// (closed already, so no second `NotificationClosed` goes out).
    pub fn close(&mut self, id: u32, reason: Reason) -> Option<Reason> {
        let before = self.list.len();
        self.list.retain(|n| n.id != id);
        (self.list.len() != before).then_some(reason)
    }

    /// The person invoked action `key` of `id` (the `default` one by
    /// clicking the card). `None` when there is no such notification or
    /// action, so nothing is sent for a stale click.
    pub fn invoke(&mut self, id: u32, key: &str) -> Option<Invoked> {
        let n = self.get(id)?;
        let known = if key == DEFAULT_ACTION {
            n.default_action
        } else {
            n.actions.iter().any(|a| a.key == key)
        };
        if !known {
            return None;
        }
        if n.hints.resident {
            return Some(Invoked::Kept);
        }
        self.close(id, Reason::Dismissed);
        Some(Invoked::Closed)
    }

    /// Removes the notifications whose time ran out by `now`, returning
    /// their ids for `NotificationClosed`. Ones do not disturb hides
    /// expire too, rather than piling up out of sight.
    pub fn expire(&mut self, now: Instant) -> Vec<u32> {
        let mut gone = Vec::new();
        self.list.retain(|n| {
            let expired = n.expires.is_some_and(|at| at <= now);
            if expired {
                gone.push(n.id);
            }
            !expired
        });
        gone
    }

    /// When the next notification expires, for waking up then.
    pub fn next_expiry(&self) -> Option<Instant> {
        self.list.iter().filter_map(|n| n.expires).min()
    }

    /// The pointer is on `id`'s card: it does not expire out from under the
    /// person reading it, nor until [`HOLD`] after they move away.
    pub fn hold(&mut self, id: u32, now: Instant) {
        if let Some(at) = self
            .list
            .iter_mut()
            .find(|n| n.id == id)
            .and_then(|n| n.expires.as_mut())
        {
            *at = (*at).max(now + HOLD);
        }
    }

    /// The cards to draw, newest first, at most [`MAX_SHOWN`]: every one
    /// whose [`Notification::banner`] holds.
    pub fn shown(&self, do_not_disturb: bool) -> impl Iterator<Item = &Notification> {
        self.list
            .iter()
            .rev()
            .filter(move |n| n.banner(do_not_disturb))
            .take(MAX_SHOWN)
    }

    /// How many notifications there are, shown or not.
    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hints(pairs: &[(&str, Hint)]) -> Hints {
        Hints::parse(pairs.iter().map(|(k, v)| (*k, v)))
    }

    fn incoming(summary: &str, timeout: i32) -> Incoming {
        Incoming {
            app_name: "test".into(),
            summary: summary.into(),
            expire_timeout: timeout,
            ..Incoming::default()
        }
    }

    #[test]
    fn hints_are_read_and_unknown_ones_ignored() {
        let parsed = hints(&[
            ("urgency", Hint::Int(2)),
            ("value", Hint::Int(42)),
            ("resident", Hint::Bool(true)),
            ("transient", Hint::Int(1)),
            ("category", Hint::Str("transfer".into())),
            (
                "desktop-entry",
                Hint::Str("org.danube.Danube.desktop".into()),
            ),
            ("image-data", Hint::Other),
            ("x-kde-origin-name", Hint::Str("ignored".into())),
        ]);
        assert_eq!(
            parsed,
            Hints {
                urgency: Urgency::Critical,
                value: Some(42),
                resident: true,
                transient: true,
                category: Some("transfer".into()),
                desktop_entry: Some("org.danube.Danube".into()),
            }
        );
        assert_eq!(hints(&[]), Hints::default());
    }

    #[test]
    fn odd_hint_values_fall_back() {
        assert_eq!(hints(&[("urgency", Hint::Int(0))]).urgency, Urgency::Low);
        // An unknown level is not promoted to critical.
        assert_eq!(hints(&[("urgency", Hint::Int(7))]).urgency, Urgency::Normal);
        assert_eq!(
            hints(&[("urgency", Hint::Str("2".into()))]).urgency,
            Urgency::Normal
        );
        assert_eq!(hints(&[("value", Hint::Int(250))]).value, Some(100));
        assert_eq!(hints(&[("value", Hint::Int(-5))]).value, Some(0));
        assert_eq!(hints(&[("value", Hint::Str("50".into()))]).value, None);
        assert!(!hints(&[("resident", Hint::Str("yes".into()))]).resident);
        assert_eq!(
            hints(&[("category", Hint::Str(String::new()))]).category,
            None
        );
    }

    #[test]
    fn expiry_follows_the_timeout_and_urgency() {
        let now = Instant::now();
        let normal = Hints::default();
        assert_eq!(expiry(-1, &normal, now), Some(now + DEFAULT_TIMEOUT));
        assert_eq!(expiry(-30, &normal, now), Some(now + DEFAULT_TIMEOUT));
        assert_eq!(expiry(0, &normal, now), None);
        assert_eq!(
            expiry(1500, &normal, now),
            Some(now + Duration::from_millis(1500))
        );
        let critical = Hints {
            urgency: Urgency::Critical,
            ..Hints::default()
        };
        assert_eq!(expiry(1500, &critical, now), None);
        assert_eq!(expiry(-1, &critical, now), None);
        let transient = Hints {
            transient: true,
            ..Hints::default()
        };
        assert_eq!(expiry(0, &transient, now), Some(now + DEFAULT_TIMEOUT));
        let both = Hints {
            transient: true,
            ..critical
        };
        assert_eq!(expiry(0, &both, now), None);
    }

    #[test]
    fn ids_start_at_one_and_are_never_reused() {
        let mut stack = Stack::default();
        let now = Instant::now();
        let a = stack.notify(incoming("a", -1), now);
        let b = stack.notify(incoming("b", -1), now);
        assert_eq!((a, b), (1, 2));
        stack.close(b, Reason::Closed);
        assert_eq!(stack.notify(incoming("c", -1), now), 3);
        // Replacing one that is gone is a new notification.
        let replacing = Incoming {
            replaces_id: b,
            ..incoming("d", -1)
        };
        assert_eq!(stack.notify(replacing, now), 4);
    }

    #[test]
    fn replacing_updates_in_place() {
        let mut stack = Stack::default();
        let now = Instant::now();
        let download = stack.notify(incoming("download", 0), now);
        let other = stack.notify(incoming("other", 0), now);
        let update = Incoming {
            replaces_id: download,
            body: "50%".into(),
            hints: hints(&[("value", Hint::Int(50))]),
            ..incoming("download", 0)
        };
        assert_eq!(stack.notify(update, now), download);
        assert_eq!(stack.len(), 2);
        let order: Vec<u32> = stack.shown(false).map(|n| n.id).collect();
        // Still under the newer one: it kept its place.
        assert_eq!(order, [other, download]);
        let n = stack.get(download).unwrap();
        assert_eq!((n.body.as_str(), n.hints.value), ("50%", Some(50)));
    }

    #[test]
    fn replacing_restarts_the_clock() {
        let mut stack = Stack::default();
        let t0 = Instant::now();
        let id = stack.notify(incoming("a", 1000), t0);
        let t1 = t0 + Duration::from_millis(800);
        let replacing = Incoming {
            replaces_id: id,
            ..incoming("a", 1000)
        };
        stack.notify(replacing, t1);
        assert!(stack.expire(t0 + Duration::from_millis(1200)).is_empty());
        assert_eq!(stack.expire(t1 + Duration::from_millis(1000)), [id]);
    }

    #[test]
    fn expired_ones_go_and_persistent_ones_stay() {
        let mut stack = Stack::default();
        let now = Instant::now();
        let quick = stack.notify(incoming("quick", 100), now);
        let default = stack.notify(incoming("default", -1), now);
        let forever = stack.notify(incoming("forever", 0), now);
        let critical = Incoming {
            hints: hints(&[("urgency", Hint::Int(2))]),
            ..incoming("critical", 100)
        };
        let critical = stack.notify(critical, now);
        assert_eq!(stack.next_expiry(), Some(now + Duration::from_millis(100)));
        assert_eq!(stack.expire(now + Duration::from_millis(100)), [quick]);
        assert_eq!(stack.expire(now + DEFAULT_TIMEOUT), [default]);
        assert!(stack.expire(now + Duration::from_secs(86_400)).is_empty());
        let left: Vec<u32> = stack.shown(false).map(|n| n.id).collect();
        assert_eq!(left, [critical, forever]);
        assert_eq!(stack.next_expiry(), None);
    }

    #[test]
    fn holding_a_card_keeps_it() {
        let mut stack = Stack::default();
        let now = Instant::now();
        let id = stack.notify(incoming("a", 1000), now);
        let later = now + Duration::from_millis(900);
        stack.hold(id, later);
        assert!(stack.expire(now + Duration::from_millis(1000)).is_empty());
        assert_eq!(stack.expire(later + HOLD), [id]);
        // A persistent one stays persistent.
        let forever = stack.notify(incoming("b", 0), now);
        stack.hold(forever, now);
        assert!(stack.get(forever).unwrap().persistent());
    }

    #[test]
    fn closing_twice_reports_once() {
        let mut stack = Stack::default();
        let id = stack.notify(incoming("a", 0), Instant::now());
        assert_eq!(stack.close(id, Reason::Dismissed), Some(Reason::Dismissed));
        assert_eq!(stack.close(id, Reason::Closed), None);
        assert!(stack.is_empty());
    }

    #[test]
    fn actions_and_the_default_one() {
        let mut stack = Stack::default();
        let now = Instant::now();
        let with = |resident: bool| Incoming {
            actions: ["default", "Open", "cancel", "Cancel", "retry", "", "odd"]
                .map(String::from)
                .to_vec(),
            hints: Hints {
                resident,
                ..Hints::default()
            },
            ..incoming("download", 0)
        };
        let id = stack.notify(with(false), now);
        let n = stack.get(id).unwrap();
        assert!(n.default_action);
        let labels: Vec<(&str, &str)> = n
            .actions
            .iter()
            .map(|a| (a.key.as_str(), a.label.as_str()))
            .collect();
        // `default` is the card, an empty label reads as its key, and the
        // key left without a label is dropped.
        assert_eq!(labels, [("cancel", "Cancel"), ("retry", "retry")]);
        assert_eq!(stack.invoke(id, "nonsense"), None);
        assert_eq!(stack.invoke(id, "cancel"), Some(Invoked::Closed));
        assert!(stack.get(id).is_none());
        assert_eq!(stack.invoke(id, "cancel"), None);

        let resident = stack.notify(with(true), now);
        assert_eq!(stack.invoke(resident, "default"), Some(Invoked::Kept));
        assert!(stack.get(resident).is_some());

        let plain = stack.notify(incoming("no default", 0), now);
        assert_eq!(stack.invoke(plain, "default"), None);
    }

    #[test]
    fn do_not_disturb_hides_only_banners_that_would_go() {
        let mut stack = Stack::default();
        let now = Instant::now();
        let transient = stack.notify(incoming("chat", -1), now);
        let download = stack.notify(incoming("download", 0), now);
        let critical = Incoming {
            hints: hints(&[("urgency", Hint::Int(2))]),
            ..incoming("battery", -1)
        };
        let critical = stack.notify(critical, now);
        let quiet: Vec<u32> = stack.shown(true).map(|n| n.id).collect();
        assert_eq!(quiet, [critical, download]);
        let all: Vec<u32> = stack.shown(false).map(|n| n.id).collect();
        assert_eq!(all, [critical, download, transient]);
    }

    #[test]
    fn long_text_is_cut_at_a_character() {
        let mut stack = Stack::default();
        let body = "é".repeat(MAX_TEXT);
        let id = stack.notify(
            Incoming {
                body,
                ..incoming("long", 0)
            },
            Instant::now(),
        );
        let kept = &stack.get(id).unwrap().body;
        assert_eq!(kept.len(), MAX_TEXT);
        assert!(kept.chars().all(|c| c == 'é'));
    }

    #[test]
    fn at_most_five_are_shown_newest_first() {
        let mut stack = Stack::default();
        let now = Instant::now();
        for i in 0..8 {
            stack.notify(incoming(&i.to_string(), 0), now);
        }
        let shown: Vec<u32> = stack.shown(false).map(|n| n.id).collect();
        assert_eq!(shown, [8, 7, 6, 5, 4]);
        assert_eq!(stack.len(), 8);
    }
}
