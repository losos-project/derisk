//! How a machine's windows reach the desktop.
//!
//! In the guest, apps connect to a waypipe server (`waypipe --vsock -s 2:7350
//! server`) as their compositor, and it carries each app's Wayland connection
//! over virtio-vsock to the host (address 2), one stream per app. On the
//! host, three things meet it:
//!
//! 1. [`gate`] accepts on vsock port [`DISPLAY_PORT`]. The kernel stamps each
//!    connection with the address libvirt gave the sending machine, and the
//!    gate asks libvirt which machine has that address now.
//! 2. It hands the connection to that machine's waypipe client, listening on
//!    [`Sockets::waypipe`], which turns waypipe's stream back into Wayland.
//! 3. The waypipe client connects to [`Sockets::wayland`], a socket the
//!    compositor opened for that machine alone and marks every client of
//!    with the [`SANDBOX_ENGINE`] sandbox, the machine's name and its UUID.
//!
//! So a window's machine is known from where its connection came in, never
//! from anything the guest says, and a guest cannot reach another machine's
//! socket: it only ever talks to the gate, and the gate picks the socket.

use std::{
    io::{self, Read, Write},
    net::Shutdown,
    os::unix::{fs::DirBuilderExt, net::UnixStream},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use vsock::{VMADDR_CID_ANY, VsockListener, VsockStream};

use crate::{DISPLAY_PORT, Hypervisor, Machine, SANDBOX_ENGINE};

/// How long the gate waits for a machine's waypipe client to listen. The
/// desktop starts one as soon as it sees the machine running, which is
/// before the guest has booted far enough to open a window; this covers a
/// desktop that saw it late.
const CLIENT_WAIT: Duration = Duration::from_secs(5);

/// Where one session keeps its machines' sockets:
/// `$XDG_RUNTIME_DIR/derisk/machines`, which only the session's user can
/// enter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Sockets {
    dir: PathBuf,
}

impl Sockets {
    /// The sockets under `runtime_dir` (`$XDG_RUNTIME_DIR`).
    pub fn new(runtime_dir: &Path) -> Sockets {
        Sockets {
            dir: runtime_dir.join("derisk").join("machines"),
        }
    }

    /// The directory, made if it is missing, readable by its owner alone.
    pub fn create(&self) -> io::Result<&Path> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)?;
        Ok(&self.dir)
    }

    /// The compositor's socket for `machine`'s windows.
    pub fn wayland(&self, machine: &str) -> PathBuf {
        self.dir.join(format!("{machine}.wayland"))
    }

    /// Where `machine`'s waypipe client listens for the gate.
    pub fn waypipe(&self, machine: &str) -> PathBuf {
        self.dir.join(format!("{machine}.waypipe"))
    }

    /// The waypipe client for `machine`: it listens on
    /// [`Sockets::waypipe`] and connects each stream the gate brings it to
    /// [`Sockets::wayland`], one Wayland connection per app.
    pub fn waypipe_client(&self, machine: &str) -> Command {
        let mut command = Command::new("waypipe");
        command
            .arg("--socket")
            .arg(self.waypipe(machine))
            .arg("client")
            // An absolute WAYLAND_DISPLAY is used as the socket's path.
            .env("WAYLAND_DISPLAY", self.wayland(machine))
            // The guest's apps reach no other host program through it.
            .env_remove("WAYLAND_SOCKET")
            .env_remove("DISPLAY");
        command
    }
}

/// Whether `machine` can have sockets: its name is a file name. pm's
/// machines always can; a domain made by hand might be called anything.
pub fn has_display(machine: &Machine) -> bool {
    let name = machine.name.as_str();
    !name.is_empty() && name != "." && name != ".." && !name.contains('/')
}

/// The `wp_security_context_v1` metadata for `machine`'s socket: sandbox
/// engine, app id and instance id.
pub fn sandbox(machine: &Machine) -> [String; 3] {
    [
        SANDBOX_ENGINE.to_owned(),
        machine.name.clone(),
        machine.uuid.clone(),
    ]
}

/// Accepts guests' windows on vsock port [`DISPLAY_PORT`] and hands each
/// connection to its machine's waypipe client, for as long as the session
/// runs.
///
/// Returns only when the port cannot be listened on: without vhost-vsock
/// (`modprobe vhost_vsock`), or when another session holds it.
pub fn gate(sockets: &Sockets, hypervisor: &Hypervisor) -> io::Result<()> {
    let listener = VsockListener::bind_with_cid_port(VMADDR_CID_ANY, DISPLAY_PORT)?;
    std::thread::scope(|scope| {
        for accepted in listener.incoming() {
            let stream = match accepted {
                Ok(stream) => stream,
                Err(error) => {
                    tracing::warn!(%error, "cannot accept a guest's connection");
                    continue;
                }
            };
            let cid = match stream.peer_addr() {
                Ok(address) => address.cid(),
                Err(error) => {
                    tracing::warn!(%error, "a guest's connection has no address");
                    continue;
                }
            };
            let machine = match hypervisor.machine_at(cid) {
                Ok(Some(machine)) if has_display(&machine) => machine,
                Ok(_) => {
                    tracing::warn!(cid, "no running machine has this address; refused");
                    continue;
                }
                Err(error) => {
                    tracing::warn!(%error, cid, "cannot ask libvirt whose address this is");
                    continue;
                }
            };
            let client = sockets.waypipe(&machine.name);
            scope.spawn(move || {
                let routed = connect_when_ready(&client, CLIENT_WAIT)
                    .and_then(|desktop| splice(stream, desktop));
                if let Err(error) = routed {
                    tracing::warn!(%error, machine = machine.name, "a window's connection ended");
                }
            });
        }
    });
    Ok(())
}

/// Connects to the Unix socket at `path`, trying again until it listens or
/// `wait` has passed.
fn connect_when_ready(path: &Path, wait: Duration) -> io::Result<UnixStream> {
    let deadline = Instant::now() + wait;
    loop {
        match UnixStream::connect(path) {
            Ok(stream) => return Ok(stream),
            Err(error) if Instant::now() >= deadline => {
                return Err(io::Error::new(
                    error.kind(),
                    format!("{}: {error}", path.display()),
                ));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// A stream with two directions that can be closed one at a time.
trait Duplex: Read + Write + Send + Sized {
    fn try_clone(&self) -> io::Result<Self>;
    fn shutdown(&self, how: Shutdown) -> io::Result<()>;
}

impl Duplex for UnixStream {
    fn try_clone(&self) -> io::Result<Self> {
        UnixStream::try_clone(self)
    }
    fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        UnixStream::shutdown(self, how)
    }
}

impl Duplex for VsockStream {
    fn try_clone(&self) -> io::Result<Self> {
        VsockStream::try_clone(self)
    }
    fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        VsockStream::shutdown(self, how)
    }
}

/// Copies `a` to `b` and `b` to `a` until both have ended. When one side
/// stops sending, the other is told no more is coming, as waypipe expects
/// of a connection closing.
fn splice<A: Duplex, B: Duplex>(a: A, b: B) -> io::Result<()> {
    let (mut a_in, mut a_out) = (a.try_clone()?, a);
    let (mut b_in, mut b_out) = (b.try_clone()?, b);
    std::thread::scope(|scope| {
        let there = scope.spawn(move || {
            let copied = io::copy(&mut a_in, &mut b_out);
            let _ = b_out.shutdown(Shutdown::Write);
            copied
        });
        let back = io::copy(&mut b_in, &mut a_out);
        let _ = a_out.shutdown(Shutdown::Write);
        let there = there
            .join()
            .unwrap_or_else(|_| Err(io::Error::other("the copying thread panicked")));
        back.and(there).map(drop)
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;

    use super::*;
    use crate::{Label, State};

    #[test]
    fn sockets_live_under_the_runtime_dir() {
        let sockets = Sockets::new(Path::new("/run/user/1000"));
        assert_eq!(
            sockets.wayland("work"),
            Path::new("/run/user/1000/derisk/machines/work.wayland")
        );
        assert_eq!(
            sockets.waypipe("work"),
            Path::new("/run/user/1000/derisk/machines/work.waypipe")
        );
        let client = sockets.waypipe_client("work");
        let args: Vec<_> = client.get_args().collect();
        assert_eq!(
            args,
            [
                "--socket",
                "/run/user/1000/derisk/machines/work.waypipe",
                "client"
            ]
        );
        assert!(
            client
                .get_envs()
                .any(|(key, value)| key == "WAYLAND_DISPLAY"
                    && value == Some("/run/user/1000/derisk/machines/work.wayland".as_ref()))
        );
    }

    #[test]
    fn names_that_are_not_file_names_get_no_display() {
        let mut machine = Machine {
            name: "work".into(),
            uuid: "u".into(),
            label: Label::Blue,
            template: None,
            memory_mib: 1,
            cpus: 1,
            state: State::Running,
            cid: Some(3),
        };
        assert!(has_display(&machine));
        assert_eq!(sandbox(&machine), ["org.libvirt", "work", "u"]);
        for bad in ["", ".", "..", "a/b"] {
            machine.name = bad.into();
            assert!(!has_display(&machine), "{bad:?}");
        }
    }

    #[test]
    fn splice_carries_both_ways_and_closes() {
        let (guest, gate_end) = UnixStream::pair().unwrap();
        let (desktop_end, mut desktop) = UnixStream::pair().unwrap();
        std::thread::scope(|scope| {
            let spliced = scope.spawn(|| splice(gate_end, desktop_end));
            let mut guest_reader = guest.try_clone().unwrap();
            let mut guest_writer = guest;
            guest_writer.write_all(b"hello from the guest").unwrap();
            guest_writer.shutdown(Shutdown::Write).unwrap();
            let mut heard = Vec::new();
            desktop.read_to_end(&mut heard).unwrap();
            assert_eq!(heard, b"hello from the guest");
            desktop.write_all(b"and back").unwrap();
            desktop.shutdown(Shutdown::Write).unwrap();
            let mut back = Vec::new();
            guest_reader.read_to_end(&mut back).unwrap();
            assert_eq!(back, b"and back");
            spliced.join().unwrap().unwrap();
        });
    }

    #[test]
    fn waits_for_the_client_to_listen() {
        let dir = std::env::temp_dir().join(format!("derisk-gate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sockets = Sockets::new(&dir);
        sockets.create().unwrap();
        let path = sockets.waypipe("work");
        assert!(connect_when_ready(&path, Duration::from_millis(200)).is_err());
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(300));
                let listener = UnixListener::bind(&path).unwrap();
                drop(listener.accept().unwrap());
            });
            assert!(connect_when_ready(&path, Duration::from_secs(5)).is_ok());
        });
        std::fs::remove_dir_all(dir).unwrap();
    }
}
