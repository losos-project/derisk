//! The session's notification server.
//!
//! Apps show notifications by calling `org.freedesktop.Notifications` on
//! the session bus (the freedesktop Desktop Notifications spec, 1.2):
//! libnotify, GLib's `GNotification` outside a portal, Qt's and Electron's
//! notifications, `notify-send`, and Danube's downloads, which it shows as
//! one persistent notification each with a progress bar and a Cancel
//! button, updated in place. Without a server those calls fail and the app
//! shows nothing. `derisk session --execute` owns the name and draws each
//! notification as a card over the desktop ([`derisk::notification_stack`]
//! holds them, [`derisk::ui::ShellUi::show_notifications`] draws them).
//!
//! It never takes the name from a server already running (a mako or dunst
//! the person started): it logs that and leaves notifications to it.
//!
//! As with the polkit agent (`crate::polkit_agent`), D-Bus runs on its own
//! thread with a small tokio runtime, so a slow bus never holds up a frame.
//! The notifications themselves live with the [`Session`], in a [`Center`],
//! which the thread reaches through the compositor's [`Remote`]; what the
//! person does to a card, and what expires, comes back on a channel, and
//! this thread sends it out as `ActionInvoked` and `NotificationClosed`.

use std::{collections::HashMap, time::Instant};

use derisk::{
    notification_stack::{Hint, Hints, Incoming, Invoked, Reason, Stack},
    ui::NotificationInput,
};
use mcsapi_compositor::Remote;
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};
use zbus::{
    connection::Builder,
    fdo::{self, RequestNameFlags, RequestNameReply},
    interface,
    object_server::SignalEmitter,
    zvariant::{OwnedValue, Value},
};

use crate::host::Session;

/// The name a notification server owns.
const NAME: &str = "org.freedesktop.Notifications";

/// Where the spec says clients find it.
const PATH: &str = "/org/freedesktop/Notifications";

/// What `GetCapabilities` offers. `actions`: the buttons and the card's
/// default action. `body`: the body is shown, as plain text; `body-markup`
/// is not offered, so apps send none. `persistence`: a notification that
/// never expires stays on screen until the person or the app closes it.
/// No icons are drawn, so no `icon-static`, and there is no sound.
const CAPABILITIES: [&str; 3] = ["actions", "body", "persistence"];

/// What the session tells the bus.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// The person invoked this action of this notification.
    ActionInvoked(u32, String),
    /// This notification closed, for this reason.
    Closed(u32, Reason),
}

/// The session's notifications, and where what happens to them goes.
#[derive(Default)]
pub struct Center {
    stack: Stack,
    /// To the D-Bus thread, once it owns the name.
    events: Option<mpsc::UnboundedSender<Event>>,
}

impl Center {
    /// The notifications, for drawing.
    pub fn stack(&self) -> &Stack {
        &self.stack
    }

    fn notify(&mut self, incoming: Incoming, now: Instant) -> u32 {
        self.stack.notify(incoming, now)
    }

    /// Closes `id` for `reason`, telling the app unless it was gone.
    pub fn close(&mut self, id: u32, reason: Reason) {
        if let Some(reason) = self.stack.close(id, reason) {
            self.send(Event::Closed(id, reason));
        }
    }

    /// Closes what ran out of time by `now`.
    pub fn expire(&mut self, now: Instant) {
        for id in self.stack.expire(now) {
            self.send(Event::Closed(id, Reason::Expired));
        }
    }

    /// Acts on what the person did to a card.
    pub fn input(&mut self, input: NotificationInput, now: Instant) {
        match input {
            NotificationInput::Action(id, key) => {
                let Some(invoked) = self.stack.invoke(id, &key) else {
                    return;
                };
                self.send(Event::ActionInvoked(id, key));
                // The spec leaves closing to the server; one that is not
                // resident has done its job once acted on.
                if invoked == Invoked::Closed {
                    self.send(Event::Closed(id, Reason::Dismissed));
                }
            }
            NotificationInput::Dismiss(id) => self.close(id, Reason::Dismissed),
            NotificationInput::Hover(id) => self.stack.hold(id, now),
        }
    }

    fn send(&self, event: Event) {
        if let Some(events) = &self.events {
            let _ = events.send(event);
        }
    }
}

/// Work for the [`Center`], run where it lives.
type Job = Box<dyn FnOnce(&mut Center) + Send>;

/// Hands a [`Job`] to the center; `false` once the session has ended. In
/// the session this goes through the compositor's [`Remote`]; tests run
/// the job on a center of their own.
type Run = Box<dyn Fn(Job) -> bool + Send + Sync>;

struct Server {
    run: Run,
}

impl Server {
    /// Runs `job` on the center and waits for what it returns, `None` when
    /// the session has ended.
    async fn ask<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Center) -> T + Send + 'static,
    ) -> Option<T> {
        let (answer, answered) = oneshot::channel();
        let queued = (self.run)(Box::new(move |center: &mut Center| {
            let _ = answer.send(job(center));
        }));
        if !queued {
            return None;
        }
        answered.await.ok()
    }
}

fn ended() -> fdo::Error {
    fdo::Error::Failed("the session has ended".into())
}

#[interface(name = "org.freedesktop.Notifications")]
impl Server {
    /// Shows a notification, or updates `replaces_id` in place, and
    /// returns its id.
    // The spec's signature, argument for argument.
    #[allow(clippy::too_many_arguments)]
    async fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> fdo::Result<u32> {
        let hints: Vec<(&str, Hint)> = hints.iter().map(|(k, v)| (k.as_str(), hint(v))).collect();
        let incoming = Incoming {
            app_name,
            replaces_id,
            app_icon,
            summary,
            body,
            actions,
            hints: Hints::parse(hints.iter().map(|(k, v)| (*k, v))),
            expire_timeout,
        };
        self.ask(move |center| center.notify(incoming, Instant::now()))
            .await
            .ok_or_else(ended)
    }

    /// Closes `id` and sends `NotificationClosed` with reason 3. The spec
    /// asks for an error when `id` is gone; GNOME Shell and most servers
    /// send none, and apps closing a notification the person already
    /// dismissed should not get one, so neither does this.
    async fn close_notification(&self, id: u32) -> fdo::Result<()> {
        self.ask(move |center| center.close(id, Reason::Closed))
            .await
            .ok_or_else(ended)
    }

    /// See [`CAPABILITIES`].
    fn get_capabilities(&self) -> Vec<String> {
        CAPABILITIES.map(str::to_owned).to_vec()
    }

    #[zbus(out_args("name", "vendor", "version", "spec_version"))]
    fn get_server_information(&self) -> (String, String, String, String) {
        (
            "derisk".to_owned(),
            "derisk".to_owned(),
            env!("CARGO_PKG_VERSION").to_owned(),
            "1.2".to_owned(),
        )
    }

    /// The person invoked `action_key` of notification `id`.
    #[zbus(signal)]
    async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        id: u32,
        action_key: &str,
    ) -> zbus::Result<()>;

    /// Notification `id` closed: 1 expired, 2 dismissed by the person, 3
    /// closed by `CloseNotification`.
    #[zbus(signal)]
    async fn notification_closed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;
}

/// A hint's variant as [`Hint`]: every integer width as one, since apps do
/// not agree on them, and a variant inside a variant (which some bindings
/// send) unwrapped.
fn hint(value: &Value<'_>) -> Hint {
    match value {
        Value::Bool(b) => Hint::Bool(*b),
        Value::U8(n) => Hint::Int(i64::from(*n)),
        Value::I16(n) => Hint::Int(i64::from(*n)),
        Value::U16(n) => Hint::Int(i64::from(*n)),
        Value::I32(n) => Hint::Int(i64::from(*n)),
        Value::U32(n) => Hint::Int(i64::from(*n)),
        Value::I64(n) => Hint::Int(*n),
        Value::U64(n) => Hint::Int(i64::try_from(*n).unwrap_or(i64::MAX)),
        Value::Str(s) => Hint::Str(s.as_str().to_owned()),
        Value::Value(inner) => hint(inner),
        _ => Hint::Other,
    }
}

/// Serves the spec at [`PATH`] on the bus `builder` connects to and asks
/// for [`NAME`], without taking it from an owner already there. `None`
/// when there is one; otherwise the connection, which must be kept for as
/// long as the server runs, and the center's events, which [`relay`] sends
/// out.
async fn serve(
    builder: Builder<'static>,
    run: Run,
) -> zbus::Result<Option<(zbus::Connection, mpsc::UnboundedReceiver<Event>)>> {
    let (events, received) = mpsc::unbounded_channel();
    // The center learns where its events go before any call can reach it:
    // jobs run in order, and no client finds the server before it has the
    // name.
    if !run(Box::new(move |center: &mut Center| {
        center.events = Some(events);
    })) {
        return Ok(None);
    }
    let connection = builder.serve_at(PATH, Server { run })?.build().await?;
    // Neither queued for the name nor taking it: another server keeps its
    // notifications, and nobody waits on a queue that never moves.
    let taken = || {
        warn!("another notification server owns {NAME}; derisk does not show notifications");
        Ok(None)
    };
    match connection
        .request_name_with_flags(NAME, RequestNameFlags::DoNotQueue.into())
        .await
    {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => {
            info!("notification server on the session bus");
            Ok(Some((connection, received)))
        }
        // zbus reports the bus's "exists" as an error of its own.
        Ok(RequestNameReply::Exists | RequestNameReply::InQueue) | Err(zbus::Error::NameTaken) => {
            taken()
        }
        Err(e) => Err(e),
    }
}

/// Sends the center's events out as signals until the session ends.
async fn relay(connection: &zbus::Connection, mut events: mpsc::UnboundedReceiver<Event>) {
    let emitter = match SignalEmitter::new(connection, PATH) {
        Ok(emitter) => emitter,
        Err(e) => {
            warn!("notifications: {e}");
            return;
        }
    };
    while let Some(event) = events.recv().await {
        let sent = match &event {
            Event::ActionInvoked(id, key) => Server::action_invoked(&emitter, *id, key).await,
            Event::Closed(id, reason) => {
                Server::notification_closed(&emitter, *id, reason.code()).await
            }
        };
        if let Err(e) = sent {
            warn!("notifications: {e}");
        }
    }
}

/// Serves notifications on the session bus on a thread of its own until
/// the process exits. Failing to connect, or finding another server, is
/// logged and leaves the desktop without notifications, as before.
pub fn start(remote: Remote<Session>) {
    let run: Run = Box::new(move |job: Job| {
        remote.run(move |session: &mut Session| job(session.notifications_mut()))
    });
    let spawned = std::thread::Builder::new()
        .name("notifications".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(e) => {
                    warn!("notifications: {e}");
                    return;
                }
            };
            runtime.block_on(async move {
                let served = match Builder::session() {
                    Ok(builder) => serve(builder, run).await,
                    Err(e) => Err(e),
                };
                match served {
                    // Calls arrive on the connection's own tasks, which run
                    // while this relays the session's events.
                    Ok(Some((connection, events))) => relay(&connection, events).await,
                    Ok(None) => {}
                    Err(e) => warn!("notification server not started: {e}"),
                }
            });
        });
    if let Err(e) = spawned {
        warn!("notifications: {e}");
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead as _, BufReader},
        pin::Pin,
        process::{Child, Command, Stdio},
        sync::{Arc, Mutex},
        time::Duration,
    };

    use zbus::export::futures_core::Stream;

    use super::*;

    /// A center with its events on a channel, as the session has it.
    fn center() -> (Center, mpsc::UnboundedReceiver<Event>) {
        let (events, received) = mpsc::unbounded_channel();
        let center = Center {
            events: Some(events),
            ..Center::default()
        };
        (center, received)
    }

    fn incoming(actions: &[&str], resident: bool) -> Incoming {
        Incoming {
            summary: "report.pdf".into(),
            actions: actions.iter().map(|a| (*a).to_owned()).collect(),
            hints: Hints {
                resident,
                ..Hints::default()
            },
            expire_timeout: 0,
            ..Incoming::default()
        }
    }

    fn drain(received: &mut mpsc::UnboundedReceiver<Event>) -> Vec<Event> {
        std::iter::from_fn(|| received.try_recv().ok()).collect()
    }

    #[test]
    fn what_the_person_does_reaches_the_app() {
        let (mut center, mut received) = center();
        let now = Instant::now();
        let id = center.notify(incoming(&["cancel", "Cancel"], false), now);
        center.input(NotificationInput::Action(id, "cancel".into()), now);
        assert_eq!(
            drain(&mut received),
            [
                Event::ActionInvoked(id, "cancel".into()),
                Event::Closed(id, Reason::Dismissed)
            ]
        );
        // The card is gone: a second click sends nothing.
        center.input(NotificationInput::Action(id, "cancel".into()), now);
        assert!(drain(&mut received).is_empty());

        let resident = center.notify(incoming(&["pause", "Pause"], true), now);
        center.input(NotificationInput::Action(resident, "pause".into()), now);
        assert_eq!(
            drain(&mut received),
            [Event::ActionInvoked(resident, "pause".into())]
        );
        center.input(NotificationInput::Dismiss(resident), now);
        center.close(resident, Reason::Closed);
        assert_eq!(
            drain(&mut received),
            [Event::Closed(resident, Reason::Dismissed)]
        );
    }

    #[test]
    fn expiring_reports_reason_one() {
        let (mut center, mut received) = center();
        let now = Instant::now();
        let id = center.notify(
            Incoming {
                expire_timeout: -1,
                ..incoming(&[], false)
            },
            now,
        );
        center.expire(now + Duration::from_secs(60));
        assert_eq!(drain(&mut received), [Event::Closed(id, Reason::Expired)]);
    }

    /// A private session bus, killed when dropped.
    struct Bus {
        daemon: Child,
        address: String,
    }

    impl Bus {
        /// `None` when `dbus-daemon` is not installed or does not start.
        /// It gets a configuration of its own: the system's session one
        /// may be missing (Nix's daemon looks in an `/etc` it has none in).
        fn start() -> Option<Self> {
            let dir = std::env::temp_dir();
            let config = dir.join(format!("derisk-notifications-{}.conf", std::process::id()));
            std::fs::write(
                &config,
                format!(
                    "<busconfig><type>session</type>\
                     <listen>unix:dir={}</listen><auth>EXTERNAL</auth>\
                     <policy context=\"default\"><allow send_destination=\"*\"/>\
                     <allow receive_sender=\"*\"/><allow own=\"*\"/></policy>\
                     </busconfig>",
                    dir.display()
                ),
            )
            .ok()?;
            let mut daemon = Command::new("dbus-daemon")
                .arg(format!("--config-file={}", config.display()))
                .args(["--nofork", "--print-address=1"])
                .stdout(Stdio::piped())
                .spawn()
                .ok()?;
            let mut address = String::new();
            let read = BufReader::new(daemon.stdout.take()?).read_line(&mut address);
            let _ = std::fs::remove_file(&config);
            if read.is_err() || address.trim().is_empty() {
                let _ = daemon.kill();
                let _ = daemon.wait();
                return None;
            }
            Some(Self {
                daemon,
                address: address.trim().to_owned(),
            })
        }

        fn builder(&self) -> Builder<'static> {
            Builder::address(self.address.as_str()).expect("the daemon's address")
        }
    }

    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.daemon.kill();
            let _ = self.daemon.wait();
        }
    }

    /// The next item of `stream`, failing the test after ten seconds
    /// rather than hanging it.
    async fn next<S: Stream>(stream: &mut Pin<Box<S>>) -> S::Item {
        let (expire, expired) = oneshot::channel::<()>();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(10));
            let _ = expire.send(());
        });
        tokio::select! {
            item = std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)) => {
                item.expect("the stream ended")
            }
            _ = expired => panic!("timed out"),
        }
    }

    #[tokio::test]
    async fn serves_the_spec_on_a_bus() {
        let Some(bus) = Bus::start() else {
            eprintln!("skipped: dbus-daemon did not start");
            return;
        };
        let shared = Arc::new(Mutex::new(Center::default()));
        let run = |shared: Arc<Mutex<Center>>| -> Run {
            Box::new(move |job: Job| {
                job(&mut shared.lock().unwrap());
                true
            })
        };
        let (connection, events) = serve(bus.builder(), run(shared.clone()))
            .await
            .unwrap()
            .expect("the name was free");
        tokio::spawn(async move { relay(&connection, events).await });

        let client = bus.builder().build().await.unwrap();
        let proxy = zbus::Proxy::new(&client, NAME, PATH, NAME).await.unwrap();
        let mut closed = Box::pin(proxy.receive_signal("NotificationClosed").await.unwrap());
        let mut invoked = Box::pin(proxy.receive_signal("ActionInvoked").await.unwrap());

        let capabilities: Vec<String> = proxy.call("GetCapabilities", &()).await.unwrap();
        assert_eq!(capabilities, ["actions", "body", "persistence"]);
        let info: (String, String, String, String) =
            proxy.call("GetServerInformation", &()).await.unwrap();
        assert_eq!((info.0.as_str(), info.3.as_str()), ("derisk", "1.2"));

        let notify = |replaces: u32, value: i32| {
            let hints =
                HashMap::from([("value", Value::from(value)), ("urgency", Value::from(1u8))]);
            let proxy = proxy.clone();
            async move {
                let id: u32 = proxy
                    .call(
                        "Notify",
                        &(
                            "Danube",
                            replaces,
                            "",
                            "report.pdf",
                            "Downloading",
                            vec!["cancel", "Cancel"],
                            hints,
                            0i32,
                        ),
                    )
                    .await
                    .unwrap();
                id
            }
        };
        let first = notify(0, 10).await;
        let second = notify(0, 0).await;
        assert_eq!((first, second), (1, 2));
        // Updated in place: the same id, the new progress.
        assert_eq!(notify(first, 60).await, first);
        {
            let center = shared.lock().unwrap();
            assert_eq!(center.stack().len(), 2);
            assert_eq!(center.stack().get(first).unwrap().hints.value, Some(60));
        }

        let _: () = proxy.call("CloseNotification", &(second,)).await.unwrap();
        let signal = next(&mut closed).await;
        let body: (u32, u32) = signal.body().deserialize().unwrap();
        assert_eq!(body, (second, 3));

        shared.lock().unwrap().input(
            NotificationInput::Action(first, "cancel".into()),
            Instant::now(),
        );
        let signal = next(&mut invoked).await;
        let body: (u32, String) = signal.body().deserialize().unwrap();
        assert_eq!(body, (first, "cancel".to_owned()));
        let signal = next(&mut closed).await;
        let body: (u32, u32) = signal.body().deserialize().unwrap();
        assert_eq!(body, (first, 2));

        let quick = notify(0, 0).await;
        shared
            .lock()
            .unwrap()
            .expire(Instant::now() + Duration::from_secs(60));
        // `quick` never expires (timeout 0), so nothing is sent for it yet.
        let short: u32 = proxy
            .call(
                "Notify",
                &(
                    "app",
                    0u32,
                    "",
                    "Saved",
                    "",
                    Vec::<&str>::new(),
                    HashMap::<&str, Value>::new(),
                    -1i32,
                ),
            )
            .await
            .unwrap();
        shared
            .lock()
            .unwrap()
            .expire(Instant::now() + Duration::from_secs(60));
        let signal = next(&mut closed).await;
        let body: (u32, u32) = signal.body().deserialize().unwrap();
        assert_eq!(body, (short, 1));
        assert!(shared.lock().unwrap().stack().get(quick).is_some());

        // A second server finds the name taken and leaves it.
        let other = Arc::new(Mutex::new(Center::default()));
        assert!(serve(bus.builder(), run(other)).await.unwrap().is_none());
    }
}
