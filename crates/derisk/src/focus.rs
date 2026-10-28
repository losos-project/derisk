//! Focus mode's switch: GameMode, asked on behalf of the windows of games
//! and of editors for 3D, CAD, video and music
//! ([`derisk::desktop::FOCUS_CATEGORIES`]).
//!
//! The OS turns focus mode on when GameMode's first program registers and
//! off when its last one goes (the OS's docs/focus-mode.md). Games built
//! for GameMode register themselves; derisk registers the rest by the
//! process behind their window, with `RegisterGameByPID`, as a launcher
//! does, and unregisters it when the last of that process's windows
//! closes. GameMode also forgets a process that exits, so a crash leaves
//! nothing on.
//!
//! As with the notification server (`crate::notifications`), D-Bus runs on
//! a thread of its own with a small tokio runtime, and a frame only puts a
//! request in a bounded queue. Without GameMode on the bus every request
//! fails, which is logged once.

use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// More than a person opens windows in the time one call takes; a request
/// that does not fit is dropped rather than a frame waiting.
const QUEUE: usize = 64;

#[derive(Debug)]
enum Request {
    Register(u32),
    Unregister(u32),
}

/// The GameMode thread.
#[derive(Debug)]
pub struct Focus {
    requests: mpsc::Sender<Request>,
}

#[zbus::proxy(
    interface = "com.feralinteractive.GameMode",
    default_service = "com.feralinteractive.GameMode",
    default_path = "/com/feralinteractive/GameMode"
)]
trait GameMode {
    /// 0 when registered, -1 when refused (or already registered).
    #[zbus(name = "RegisterGameByPID")]
    fn register_game_by_pid(&self, caller: i32, game: i32) -> zbus::Result<i32>;
    #[zbus(name = "UnregisterGameByPID")]
    fn unregister_game_by_pid(&self, caller: i32, game: i32) -> zbus::Result<i32>;
}

impl Focus {
    /// Starts the thread. It ends when this is dropped.
    pub fn start() -> Option<Self> {
        let (requests, mut queue) = mpsc::channel(QUEUE);
        let spawned = std::thread::Builder::new()
            .name("focus".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(e) => {
                        warn!("focus mode: {e}");
                        return;
                    }
                };
                runtime.block_on(async move {
                    let connection = match zbus::Connection::session().await {
                        Ok(connection) => connection,
                        Err(e) => {
                            warn!("focus mode: no session bus: {e}");
                            return;
                        }
                    };
                    let me = std::process::id() as i32;
                    let mut told = false;
                    while let Some(request) = queue.recv().await {
                        let proxy = match GameModeProxy::new(&connection).await {
                            Ok(proxy) => proxy,
                            Err(e) => {
                                warn!("focus mode: {e}");
                                continue;
                            }
                        };
                        let answer = match request {
                            Request::Register(pid) => {
                                proxy.register_game_by_pid(me, pid as i32).await
                            }
                            Request::Unregister(pid) => {
                                proxy.unregister_game_by_pid(me, pid as i32).await
                            }
                        };
                        match answer {
                            Ok(0) => debug!(?request, "GameMode took it"),
                            // Already registered by the program itself, or
                            // already gone.
                            Ok(_) => debug!(?request, "GameMode refused it"),
                            Err(e) if !told => {
                                info!("focus mode is off: GameMode does not answer: {e}");
                                told = true;
                            }
                            Err(_) => {}
                        }
                    }
                });
            });
        match spawned {
            Ok(_) => Some(Self { requests }),
            Err(e) => {
                warn!("focus mode: {e}");
                None
            }
        }
    }

    /// Asks for focus while `pid` runs.
    pub fn register(&self, pid: u32) {
        let _ = self.requests.try_send(Request::Register(pid));
    }

    /// Gives up the focus asked for `pid`.
    pub fn unregister(&self, pid: u32) {
        let _ = self.requests.try_send(Request::Unregister(pid));
    }
}
