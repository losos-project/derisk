//! The Android app's thread: everything that runs `android-app-support`.
//!
//! The app is drawn on the compositor's thread, between every other
//! window's frames, so nothing it does there may wait. Asking for the
//! Pixel's state runs a program, and starting it waits for Android to
//! boot, so the app hands orders to this thread and reads its events each
//! frame.

use std::{
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, SystemTime},
};

use crossbeam_channel::{Receiver, Sender, bounded, select};

use crate::{AndroidApp, COMMAND, Status, read_apps};

/// How often the state is asked for again when nothing has been ordered,
/// so a Pixel started from an app's window or the app grid shows up.
const REFRESH: Duration = Duration::from_secs(2);

/// What the app asks for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Order {
    /// Download Android and make the Pixel.
    SetUp,
    Start,
    Stop,
    /// Open an app in a window of its own, starting the Pixel if it is off.
    Open(String),
    /// The Pixel's own screen in a window.
    Phone,
}

/// What the thread tells the app.
#[derive(Clone, Debug)]
pub(crate) enum Event {
    Status(Status),
    /// `android-app-support` is not installed or would not run.
    Unavailable(String),
    Apps(Vec<AndroidApp>),
    /// An order finished: what to say, or why it failed.
    Done(Result<String, String>),
}

/// The app's end of the thread.
pub(crate) struct Worker {
    orders: Sender<Order>,
    events: Receiver<Event>,
}

impl Worker {
    pub fn start(dir: Option<PathBuf>) -> Self {
        let (orders, order_rx) = bounded(16);
        let (event_tx, events) = bounded(64);
        let spawned = std::thread::Builder::new()
            .name("derisk-android".into())
            .spawn(move || run(dir, &order_rx, &event_tx));
        if let Err(error) = spawned {
            tracing::warn!(%error, "could not start the Android app's thread");
        }
        Self { orders, events }
    }

    /// Hands the thread an order; false when it is gone or swamped.
    pub fn send(&self, order: Order) -> bool {
        self.orders.try_send(order).is_ok()
    }

    pub fn events(&self) -> impl Iterator<Item = Event> + '_ {
        self.events.try_iter()
    }
}

fn run(dir: Option<PathBuf>, orders: &Receiver<Order>, events: &Sender<Event>) {
    let mut listed: Option<SystemTime> = None;
    loop {
        match status() {
            Ok(status) => {
                if events.send(Event::Status(status)).is_err() {
                    return;
                }
            }
            Err(why) => {
                if events.send(Event::Unavailable(why)).is_err() {
                    return;
                }
            }
        }
        // The list is read again only when android-app-support wrote it
        // again, after the Pixel's packages changed.
        if let Some(dir) = &dir {
            let changed = std::fs::metadata(dir.join("apps.tsv"))
                .and_then(|m| m.modified())
                .ok();
            if changed != listed {
                listed = changed;
                if events.send(Event::Apps(read_apps(dir))).is_err() {
                    return;
                }
            }
        }
        select! {
            recv(orders) -> order => {
                let Ok(order) = order else { return };
                let done = obey(&order);
                if events.send(Event::Done(done)).is_err() {
                    return;
                }
            }
            default(REFRESH) => {}
        }
    }
}

/// `android-app-support status`.
fn status() -> Result<Status, String> {
    let output = Command::new(COMMAND)
        .arg("status")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|error| format!("Android app support is not part of this system ({error})."))?;
    let line = String::from_utf8_lossy(&output.stdout);
    Status::parse(&line).ok_or_else(|| "Android app support gave no answer.".into())
}

fn obey(order: &Order) -> Result<String, String> {
    match order {
        // The setup is the person's user unit, which carries on with the
        // app closed: the download alone takes minutes.
        Order::SetUp => systemctl("start", "android-app-support-setup.service")
            .map(|()| "Setting Android up".into()),
        Order::Start => wait(&["start"]).map(|()| "The Pixel is running".into()),
        Order::Stop => wait(&["stop"]).map(|()| "The Pixel is off".into()),
        // An app's window, and the phone's, are scrcpy, which runs until
        // its window closes: started, not waited for.
        Order::Open(package) => spawn(&["run", package]).map(|()| "Opening the app".into()),
        Order::Phone => spawn(&["phone"]).map(|()| "Opening the phone's screen".into()),
    }
}

fn systemctl(verb: &str, unit: &str) -> Result<(), String> {
    let status = Command::new("systemctl")
        .args(["--user", "--no-block", verb, unit])
        .stdin(Stdio::null())
        .status()
        .map_err(|error| error.to_string())?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("systemctl {verb} {unit} failed"))
}

/// Runs `android-app-support args` to its end; its last line of errors is
/// what a failure says.
fn wait(args: &[&str]) -> Result<(), String> {
    let output = Command::new(COMMAND)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let errors = String::from_utf8_lossy(&output.stderr);
    Err(errors
        .lines()
        .last()
        .unwrap_or("android-app-support failed")
        .trim_start_matches("android-app-support: ")
        .to_owned())
}

/// Starts `android-app-support args` and leaves it running, as a transient
/// user unit in the apps' slice, the way derisk launches any app: the
/// window outlives this app, and nothing here has a child to reap.
fn spawn(args: &[&str]) -> Result<(), String> {
    let status = Command::new("systemd-run")
        .args([
            "--user",
            "--no-block",
            "--collect",
            "--quiet",
            "--slice=app.slice",
            "--property=Type=exec",
            "--",
            COMMAND,
        ])
        .args(args)
        .stdin(Stdio::null())
        .status()
        .map_err(|error| error.to_string())?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| "systemd-run could not start it".into())
}
