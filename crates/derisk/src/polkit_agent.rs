//! The session's polkit authentication agent.
//!
//! polkit decides whether a process may do something; when its answer is
//! "only after someone authenticates" (run0, installing an app for every
//! user, enrolling a fingerprint), it asks the agent registered for the
//! caller's session. Without one it simply refuses. `derisk session`
//! registers itself for its logind session on the system bus and shows each
//! request as a dialog over the desktop ([`derisk::polkit`]).
//!
//! The agent never sees whether a password is right. For each attempt it
//! connects to `polkit-agent-helper-1` on `/run/polkit/agent-helper.socket`
//! (socket-activated as root; NixOS no longer installs it setuid), names the
//! user and the request's cookie, and relays what PAM's `polkit-1` stack
//! asks and what the person types. The helper tells polkitd itself when
//! PAM accepts, then says `SUCCESS`; the agent only returns from
//! `BeginAuthentication` after that, which is what polkitd waits for.
//!
//! D-Bus runs on its own thread with a small tokio runtime, so a slow bus
//! never holds up a frame. The dialog lives in the [`Session`], which the
//! thread updates through the compositor's [`Remote`]; what the person does
//! comes back on a channel per request.

use std::{
    collections::HashMap,
    ffi::CStr,
    sync::{Arc, Mutex},
};

use derisk::polkit::{AuthDialog, HelperLine, Identity, pick};
use mcsapi_compositor::Remote;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    sync::mpsc,
};
use tracing::{info, warn};
use zbus::{OwnedVariant, Variant, interface};

use crate::{host::Session, pam};

/// Where polkit's helper listens for agents.
const HELPER_SOCKET: &str = "/run/polkit/agent-helper.socket";

/// The object path polkitd calls the agent at.
const OBJECT_PATH: &str = "/org/derisk/PolicyKit1/AuthenticationAgent";

/// How many refused attempts a request gets before the agent gives up and
/// polkit refuses the action, as other agents do.
const ATTEMPTS: usize = 3;

/// What the person did in a request's dialog.
#[derive(Debug)]
pub enum Reply {
    /// Answered PAM's prompt.
    Answer(String),
    /// Chose someone else to authenticate as.
    Identity(usize),
    /// Closed the dialog.
    Cancel,
}

/// A request the [`Session`] shows: the dialog, and where to send what the
/// person does.
pub struct Prompt {
    /// polkit's cookie for it.
    pub cookie: String,
    /// What the dialog shows.
    pub dialog: AuthDialog,
    /// Back to the request's conversation.
    pub replies: mpsc::UnboundedSender<Reply>,
}

/// The errors polkitd understands from an agent.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.PolicyKit1.Error")]
enum PolkitError {
    #[zbus(error)]
    ZBus(zbus::Error),
    /// Authentication did not happen.
    Failed(String),
    /// The person, or polkitd, cancelled it.
    Cancelled(String),
}

struct Agent {
    remote: Remote<Session>,
    /// Who is logged in, asked first when polkit accepts them.
    user: String,
    /// Each running request's channel, by cookie, so polkitd's
    /// CancelAuthentication reaches it.
    running: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<Reply>>>>,
}

#[interface(name = "org.freedesktop.PolicyKit1.AuthenticationAgent")]
impl Agent {
    /// Authenticates someone in `identities` for `cookie`, returning once
    /// polkitd has been told, or with an error when nobody did.
    async fn begin_authentication(
        &self,
        action_id: String,
        message: String,
        _icon_name: String,
        _details: HashMap<String, String>,
        cookie: String,
        identities: Vec<(String, HashMap<String, OwnedVariant>)>,
    ) -> Result<(), PolkitError> {
        let identities: Vec<Identity> = identities
            .iter()
            .filter_map(|(kind, details)| unix_user(kind, details))
            .collect();
        if identities.is_empty() {
            return Err(PolkitError::Failed(
                "no user derisk can ask to authenticate".into(),
            ));
        }
        info!(action = %action_id, "polkit asks to authenticate");
        let chosen = pick(&identities, &self.user);
        let (replies, mut rx) = mpsc::unbounded_channel();
        if let Ok(mut running) = self.running.lock() {
            running.insert(cookie.clone(), replies.clone());
        }
        let dialog = AuthDialog::new(&message, &action_id, identities.clone(), chosen);
        let shown = {
            let cookie = cookie.clone();
            self.remote.run(move |session: &mut Session| {
                session.polkit_begin(Prompt {
                    cookie,
                    dialog,
                    replies,
                });
            })
        };
        let result = if shown {
            self.authenticate(&cookie, &identities, chosen, &mut rx)
                .await
        } else {
            Err(PolkitError::Failed("the session has ended".into()))
        };
        if let Ok(mut running) = self.running.lock() {
            running.remove(&cookie);
        }
        let ended = cookie.clone();
        self.remote
            .run(move |session: &mut Session| session.polkit_end(&ended));
        match &result {
            Ok(()) => info!(action = %action_id, "authenticated"),
            Err(e) => info!(action = %action_id, "not authenticated: {e}"),
        }
        result
    }

    /// polkitd no longer needs `cookie` (the caller went away, or timed
    /// out): the dialog closes.
    async fn cancel_authentication(&self, cookie: String) {
        if let Ok(running) = self.running.lock()
            && let Some(replies) = running.get(&cookie)
        {
            let _ = replies.send(Reply::Cancel);
        }
    }
}

/// How one attempt ended.
enum Attempt {
    Success,
    Refused,
    Switch(usize),
    Cancelled,
}

impl Agent {
    /// Runs attempts until one succeeds, the person cancels, or
    /// [`ATTEMPTS`] are refused.
    async fn authenticate(
        &self,
        cookie: &str,
        identities: &[Identity],
        mut chosen: usize,
        rx: &mut mpsc::UnboundedReceiver<Reply>,
    ) -> Result<(), PolkitError> {
        let mut refused = 0;
        while refused < ATTEMPTS {
            let name = &identities[chosen].name;
            match self.attempt(cookie, name, rx).await {
                Ok(Attempt::Success) => return Ok(()),
                Ok(Attempt::Cancelled) => {
                    return Err(PolkitError::Cancelled("cancelled".into()));
                }
                Ok(Attempt::Switch(index)) if index < identities.len() => {
                    chosen = index;
                    refused = 0;
                }
                Ok(Attempt::Switch(_)) => {}
                Ok(Attempt::Refused) => {
                    refused += 1;
                    let cookie = cookie.to_owned();
                    self.remote
                        .run(move |session: &mut Session| session.polkit_failed(&cookie));
                }
                Err(e) => {
                    warn!("polkit's helper: {e}");
                    return Err(PolkitError::Failed(format!("polkit's helper: {e}")));
                }
            }
        }
        Err(PolkitError::Failed("too many failed attempts".into()))
    }

    /// One PAM conversation through the helper, as `user`.
    async fn attempt(
        &self,
        cookie: &str,
        user: &str,
        rx: &mut mpsc::UnboundedReceiver<Reply>,
    ) -> std::io::Result<Attempt> {
        let stream = UnixStream::connect(HELPER_SOCKET).await?;
        let (read, mut write) = stream.into_split();
        // The user, then the cookie, each on a line: the cookie goes over
        // the socket rather than the command line so no one else sees it.
        write
            .write_all(format!("{user}\n{cookie}\n").as_bytes())
            .await?;
        let mut lines = BufReader::new(read).lines();
        loop {
            tokio::select! {
                line = lines.next_line() => {
                    // The helper hung up without a verdict: PAM failed in a
                    // way it did not report, so this attempt counts as
                    // refused rather than ending the request.
                    let Some(line) = line? else {
                        return Ok(Attempt::Refused);
                    };
                    let Some(parsed) = HelperLine::parse(&line) else {
                        warn!("polkit's helper wrote something unknown");
                        return Ok(Attempt::Refused);
                    };
                    match parsed {
                        HelperLine::Success => return Ok(Attempt::Success),
                        HelperLine::Failure => return Ok(Attempt::Refused),
                        _ => {
                            let cookie = cookie.to_owned();
                            self.remote.run(move |session: &mut Session| {
                                session.polkit_line(&cookie, &parsed);
                            });
                        }
                    }
                }
                reply = rx.recv() => match reply {
                    Some(Reply::Answer(mut answer)) => {
                        // A newline would end the answer early and start
                        // the next one; PAM never asks for one.
                        answer.retain(|c| c != '\n');
                        answer.push('\n');
                        let sent = write.write_all(answer.as_bytes()).await;
                        pam::wipe(&mut answer);
                        sent?;
                    }
                    // Hanging up ends the helper's PAM conversation.
                    Some(Reply::Identity(index)) => return Ok(Attempt::Switch(index)),
                    Some(Reply::Cancel) | None => return Ok(Attempt::Cancelled),
                }
            }
        }
    }
}

/// A `unix-user` identity and its name; polkit's other kinds (groups, net
/// groups) never reach an agent, which authenticates people.
fn unix_user(kind: &str, details: &HashMap<String, OwnedVariant>) -> Option<Identity> {
    if kind != "unix-user" {
        return None;
    }
    let uid = match details.get("uid").map(|v| &**v) {
        Some(Variant::U32(uid)) => *uid,
        _ => return None,
    };
    Some(Identity {
        uid,
        name: user_name(uid)?,
    })
}

/// The name NSS has for `uid`; homed's users come from its NSS module, so
/// they are found too.
fn user_name(uid: u32) -> Option<String> {
    let mut buffer = vec![0 as libc::c_char; 4096];
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer is to a live local of the right type, and the
    // buffer's length is the one passed.
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            &mut entry,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut found,
        )
    };
    if status != 0 || found.is_null() {
        return None;
    }
    // SAFETY: on success pw_name points into `buffer`, NUL-terminated.
    let name = unsafe { CStr::from_ptr(entry.pw_name) };
    name.to_str().ok().map(str::to_owned)
}

/// Registers the agent for logind session `session_id` and serves it on
/// its own thread until the process exits. Failing to register is logged
/// and leaves the desktop without an agent, as before.
pub fn start(session_id: String, user: String, remote: Remote<Session>) {
    let spawned = std::thread::Builder::new()
        .name("polkit-agent".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(e) => {
                    warn!("polkit agent: {e}");
                    return;
                }
            };
            runtime.block_on(async move {
                match serve(&session_id, user, remote).await {
                    // Calls arrive on the connection's own tasks, which run
                    // while this waits.
                    Ok(_connection) => std::future::pending::<()>().await,
                    Err(e) => warn!("polkit agent not registered: {e}"),
                }
            });
        });
    if let Err(e) = spawned {
        warn!("polkit agent: {e}");
    }
}

async fn serve(
    session_id: &str,
    user: String,
    remote: Remote<Session>,
) -> zbus::Result<zbus::Connection> {
    let agent = Agent {
        remote,
        user,
        running: Arc::default(),
    };
    let connection = zbus::connection::Builder::system()
        .serve_at(OBJECT_PATH, agent)
        .build()
        .await?;
    let authority = zbus::Proxy::new(
        &connection,
        "org.freedesktop.PolicyKit1",
        "/org/freedesktop/PolicyKit1/Authority",
        "org.freedesktop.PolicyKit1.Authority",
    )
    .await?;
    let subject = (
        "unix-session",
        HashMap::from([("session-id", Variant::from(session_id))]),
    );
    // polkitd passes the locale to the action's messages.
    let locale = std::env::var("LC_MESSAGES")
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_else(|_| "C".to_owned());
    authority
        .call_method(
            "RegisterAuthenticationAgent",
            &(subject, locale.as_str(), OBJECT_PATH),
        )
        .await?;
    info!("polkit agent registered for session {session_id}");
    Ok(connection)
}
