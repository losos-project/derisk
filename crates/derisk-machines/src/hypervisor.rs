use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use crossbeam_channel::{Receiver, Sender, select};
use serde_json::{Value, json};

use crate::{Error, Machine, RECORD, Result, State};

/// How long a guest-agent call may take before it is given up on. An agent
/// that is up answers in milliseconds; one that is not (the machine is still
/// booting) never does, and would otherwise hold the caller for good.
const AGENT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long QEMU's monitor may take to read a command.
const MONITOR_TIMEOUT: Duration = Duration::from_secs(2);

/// Where pm keeps the machines of whoever runs this: `pm/machines` under
/// `$XDG_DATA_HOME`, or under `~/.local/share` when that is unset. pm's
/// `machine::Place` (losos-project/pm, `src/machine.rs`) decides the same;
/// a change here is a change there.
pub fn machines_dir() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|home| home.is_absolute())
                .map(|home| home.join(".local/share"))
        })?;
    Some(data.join("pm").join("machines"))
}

/// The socket a running machine's screen is on, which a SPICE viewer
/// shows: quickemu's, in the machine's folder.
pub fn screen_socket(name: &str) -> Option<PathBuf> {
    Some(machines_dir()?.join(name).join(format!("{name}.sock")))
}

/// This user's machines, as pm's quickemu plugin keeps them.
///
/// Nothing stands between derisk and them: a machine is a folder with pm's
/// record of it (`machine.json`) beside quickemu's `<name>.conf`, and runs
/// as the user's own service `pm-machine-<name>`, started the way pm starts
/// it. While it runs its folder holds QEMU's pid file and the sockets of its
/// monitor and its guest agent, which is how this shuts it down and runs
/// programs in it.
pub struct Hypervisor {
    directory: PathBuf,
}

impl Hypervisor {
    /// The machines of whoever runs this. There may be none yet.
    pub fn open() -> Result<Hypervisor> {
        machines_dir()
            .map(|directory| Hypervisor { directory })
            .ok_or_else(|| Error::Hypervisor("neither $XDG_DATA_HOME nor $HOME is set".into()))
    }

    /// The machines in `directory`, for tests.
    #[cfg(test)]
    fn at(directory: &std::path::Path) -> Hypervisor {
        Hypervisor {
            directory: directory.to_owned(),
        }
    }

    /// Every machine, by name.
    ///
    /// A folder without a readable record is left out and logged: one a
    /// person made by hand, or one pm is still making, should not empty the
    /// desktop's list.
    pub fn machines(&self) -> Result<impl Iterator<Item = Machine> + '_> {
        let entries = match fs::read_dir(&self.directory) {
            Ok(entries) => entries.filter_map(|entry| entry.ok()).collect(),
            // No machine has been made yet.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                return Err(Error::Hypervisor(format!(
                    "cannot read {}: {error}",
                    self.directory.display()
                )));
            }
        };
        let mut names: Vec<String> = entries
            .into_iter()
            .filter(|entry| entry.path().join(RECORD).is_file())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect();
        names.sort();
        Ok(names.into_iter().filter_map(|name| {
            self.machine(&name)
                .inspect_err(|error| tracing::warn!(%error, "skipping a machine"))
                .ok()
        }))
    }

    /// The machine called `name`.
    pub fn machine(&self, name: &str) -> Result<Machine> {
        let record = fs::read_to_string(self.folder(name)?.join(RECORD)).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::NoMachine(name.into())
            } else {
                Error::Hypervisor(error.to_string())
            }
        })?;
        Machine::from_json(&record, self.state(name))
    }

    /// The running machine at vsock address `cid`, if any.
    ///
    /// The address comes from the kernel's vhost-vsock, which stamps every
    /// connection with the address the sending machine's QEMU was given;
    /// the guest has no say in it. So the answer is which machine really
    /// sent a connection, which is what lets the desktop label its windows.
    /// Only a running machine counts: the kernel keeps two running machines
    /// from sharing an address, but a stopped one's record may name the
    /// address another person's machine has now.
    pub fn machine_at(&self, cid: u32) -> Result<Option<Machine>> {
        Ok(self
            .machines()?
            .find(|machine| machine.cid == Some(cid) && machine.state == State::Running))
    }

    /// Starts the machine. Starting one that runs already does nothing.
    ///
    /// As pm's quickemu plugin starts it (losos-project/pm,
    /// `plugins/quickemu/src/machines.rs`): a service of the user's own,
    /// forking, following QEMU's pid file. A change here is a change there,
    /// or each would take the other's machine for one that does not run.
    pub fn start(&self, name: &str) -> Result<()> {
        let folder = self.folder(name)?;
        if !folder.join(RECORD).is_file() {
            return Err(Error::NoMachine(name.into()));
        }
        let unit = unit(name);
        let active = Command::new("systemctl")
            .args(["--user", "--quiet", "is-active", &unit])
            .stdin(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if active {
            return Ok(());
        }
        let output = Command::new("systemd-run")
            .args([
                "--user",
                &format!("--unit={unit}"),
                &format!("--description=The machine {name}"),
                "--collect",
                "--quiet",
                "--property=Type=forking",
                "--property",
            ])
            .arg(format!(
                "PIDFile={}",
                folder.join(format!("{name}.pid")).display()
            ))
            .args(["quickemu", "--vm"])
            .arg(self.directory.join(format!("{name}.conf")))
            .stdin(Stdio::null())
            .output()
            .map_err(|error| Error::Hypervisor(format!("cannot run systemd-run: {error}")))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(Error::Hypervisor(format!(
                "{name} did not start: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )))
        }
    }

    /// Asks the machine to shut down, the way a power button does, and
    /// returns without waiting for it to. The guest may take its time or
    /// refuse; [`Hypervisor::force_off`] does not ask.
    pub fn shut_down(&self, name: &str) -> Result<()> {
        if !self.state(name).is_active() {
            return Ok(());
        }
        self.monitor(name, "system_powerdown")
    }

    /// Stops the machine at once, as pulling its plug would. What it had
    /// not written to its disk is lost.
    pub fn force_off(&self, name: &str) -> Result<()> {
        if !self.state(name).is_active() {
            return Ok(());
        }
        self.monitor(name, "quit")
    }

    /// Whether the machine's guest agent answers yet. A machine that has
    /// just started cannot run programs until it does.
    pub fn agent_ready(&self, name: &str) -> Result<bool> {
        if !self.state(name).is_active() {
            return Ok(false);
        }
        match self.agent(name, json!({ "execute": "guest-ping" })) {
            Ok(_) => Ok(true),
            Err(Error::AgentSilent(_)) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Starts `program` in the machine through its guest agent and returns
    /// the guest's process id for it, without waiting for it to finish.
    ///
    /// The agent runs it as root; LosOS's guest template has
    /// `losos-guest-run` to drop to the guest's user and put the program's
    /// windows on the host.
    pub fn spawn(&self, name: &str, program: &Program) -> Result<u32> {
        let mut arguments = json!({
            "path": program.path,
            "arg": program.args,
            "env": program.env,
            "capture-output": program.capture_output,
        });
        if !program.input.is_empty() {
            arguments["input-data"] = STANDARD.encode(&program.input).into();
        }
        let reply = self.agent(
            name,
            json!({ "execute": "guest-exec", "arguments": arguments }),
        )?;
        reply["pid"]
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok())
            .ok_or_else(|| Error::Agent {
                name: name.into(),
                reply: reply.to_string(),
            })
    }

    /// How the program [`Hypervisor::spawn`] started as `pid` ended, or
    /// `None` while it still runs.
    pub fn exit(&self, name: &str, pid: u32) -> Result<Option<Exit>> {
        let reply = self.agent(
            name,
            json!({ "execute": "guest-exec-status", "arguments": { "pid": pid } }),
        )?;
        Exit::from_status(&reply).map_err(|()| Error::Agent {
            name: name.into(),
            reply: reply.to_string(),
        })
    }

    /// Sends `command` to the machine's guest agent and returns what it
    /// returned.
    ///
    /// The agent's socket is quickemu's, in the machine's folder, and QEMU
    /// serves one connection on it at a time. Each call is a connection of
    /// its own, which first syncs with the agent (`guest-sync-delimited`):
    /// a reply an earlier caller gave up waiting for may still be on its
    /// way, and would otherwise be taken for this one's.
    pub(crate) fn agent(&self, name: &str, command: Value) -> Result<Value> {
        let socket = self.folder(name)?.join(format!("{name}-agent.sock"));
        let silent = |reason: String| Error::AgentSilent(format!("{name}: {reason}"));
        let stream = UnixStream::connect(&socket)
            .map_err(|error| silent(format!("{}: {error}", socket.display())))?;
        stream
            .set_read_timeout(Some(AGENT_TIMEOUT))
            .and_then(|()| stream.set_write_timeout(Some(AGENT_TIMEOUT)))
            .map_err(|error| silent(error.to_string()))?;
        let mut writer = &stream;
        let mut reader = BufReader::new(&stream);
        let id = sync_id();
        // 0xFF first resets the agent's parser, should a caller before have
        // left half a command in it; its answer to the sync starts with
        // 0xFF too, and everything before that is someone else's.
        let sync = json!({ "execute": "guest-sync-delimited", "arguments": { "id": id } });
        writer
            .write_all(&[0xff])
            .and_then(|()| writeln!(writer, "{sync}"))
            .map_err(|error| silent(error.to_string()))?;
        let mut skipped = Vec::new();
        reader
            .read_until(0xff, &mut skipped)
            .map_err(|error| silent(error.to_string()))?;
        loop {
            let line = read_line(&mut reader).map_err(silent)?;
            if line.trim().is_empty() {
                continue;
            }
            let reply: Value = serde_json::from_str(&line).map_err(|_| Error::Agent {
                name: name.into(),
                reply: line.clone(),
            })?;
            if reply["return"].as_u64() == Some(id) {
                break;
            }
        }

        writeln!(writer, "{command}").map_err(|error| silent(error.to_string()))?;
        let line = read_line(&mut reader).map_err(silent)?;
        let mut reply: Value = serde_json::from_str(&line).map_err(|_| Error::Agent {
            name: name.into(),
            reply: line.clone(),
        })?;
        if let Some(error) = reply.get("error") {
            return Err(Error::Agent {
                name: name.into(),
                reply: error["desc"]
                    .as_str()
                    .map_or_else(|| error.to_string(), str::to_owned),
            });
        }
        Ok(reply["return"].take())
    }

    /// Says `command` on the running machine's QEMU monitor (quickemu's
    /// human monitor, on a socket in its folder) and returns once QEMU has
    /// read it.
    fn monitor(&self, name: &str, command: &str) -> Result<()> {
        let socket = self.folder(name)?.join(format!("{name}-monitor.socket"));
        let failed = |error: std::io::Error| {
            Error::Hypervisor(format!("{name}'s monitor, {}: {error}", socket.display()))
        };
        let mut stream = UnixStream::connect(&socket).map_err(failed)?;
        stream
            .set_read_timeout(Some(MONITOR_TIMEOUT))
            .map_err(failed)?;
        writeln!(stream, "{command}").map_err(failed)?;
        // Done writing; QEMU closes its end once it has read all of it, and
        // what it printed meanwhile (its greeting, its prompt) is of no use.
        stream.shutdown(std::net::Shutdown::Write).map_err(failed)?;
        let _ = stream.read_to_end(&mut Vec::new());
        Ok(())
    }

    /// Whether the machine runs: whether the QEMU its pid file names is
    /// there. quickemu names QEMU's process after the machine, so a pid the
    /// system has since given to something else is not taken for it.
    fn state(&self, name: &str) -> State {
        let Ok(folder) = self.folder(name) else {
            return State::Off;
        };
        let pid = fs::read_to_string(folder.join(format!("{name}.pid")))
            .ok()
            .and_then(|pid| pid.trim().parse::<u32>().ok());
        let Some(pid) = pid else {
            return State::Off;
        };
        let comm = fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
        // The kernel keeps 15 bytes of a process's name.
        let named: String = name.chars().take(15).collect();
        if comm.trim_end() == named {
            State::Running
        } else {
            State::Off
        }
    }

    /// The machine's folder. A name with a `/` in it, or `..`, names none:
    /// names come from pm, which allows neither, but also from the palette.
    fn folder(&self, name: &str) -> Result<PathBuf> {
        if name.is_empty() || name.contains('/') || name.starts_with('.') {
            return Err(Error::NoMachine(name.into()));
        }
        Ok(self.directory.join(name))
    }
}

/// The service a machine runs as.
fn unit(name: &str) -> String {
    format!("pm-machine-{name}.service")
}

/// One line from the agent, without waiting past its timeout.
fn read_line(reader: &mut impl BufRead) -> std::result::Result<String, String> {
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) => Err("the agent closed the connection".into()),
        Ok(_) => Ok(line),
        Err(error) => Err(error.to_string()),
    }
}

/// A number for one sync with the agent: it only has to differ from the
/// last caller's.
fn sync_id() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    // The agent takes an int, which JSON numbers past 2^53 are not.
    (now.as_nanos() as u64 ^ u64::from(std::process::id())) & ((1 << 53) - 1)
}

/// A program to run in a machine.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Program {
    /// Absolute path of the program in the guest. The agent's own `PATH`
    /// is not the guest user's, so a bare name finds whatever it finds.
    pub path: String,
    /// Its arguments, without the program itself.
    pub args: Vec<String>,
    /// `NAME=value` pairs for its environment, which the agent otherwise
    /// gives it empty.
    pub env: Vec<String>,
    /// Keep its output for [`Exit::stdout`] and [`Exit::stderr`]. An app
    /// that runs for hours should not have its output kept in the agent.
    pub capture_output: bool,
    /// What it reads on its standard input; it reads nothing when empty.
    pub input: Vec<u8>,
}

/// How a program run in a machine ended.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Exit {
    /// Its exit code, when it exited.
    pub code: Option<i32>,
    /// The signal that killed it, when one did.
    pub signal: Option<i32>,
    /// What it wrote to its standard output, when that was captured.
    pub stdout: Vec<u8>,
    /// What it wrote to its standard error, when that was captured.
    pub stderr: Vec<u8>,
}

impl Exit {
    /// Whether it exited with code 0.
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    /// From the agent's `guest-exec-status` reply.
    fn from_status(status: &Value) -> Result<Option<Exit>, ()> {
        if !status["exited"].as_bool().ok_or(())? {
            return Ok(None);
        }
        let number = |key: &str| status[key].as_i64().and_then(|n| i32::try_from(n).ok());
        let data = |key: &str| match status[key].as_str() {
            Some(encoded) => STANDARD.decode(encoded).map_err(drop),
            None => Ok(Vec::new()),
        };
        Ok(Some(Exit {
            code: number("exitcode"),
            signal: number("signal"),
            stdout: data("out-data")?,
            stderr: data("err-data")?,
        }))
    }
}

/// Sends the list of machines on `changes` whenever it differs from the
/// last one sent, checking every `every`, until `stop` receives or closes
/// or nobody listens on `changes` any more.
///
/// The first list goes out at once. A look is a few small files per
/// machine, so looking every second or two costs next to nothing, and
/// catches a machine started or stopped from anywhere: pm, quickemu
/// itself, or its guest powering off.
/// Run it on a thread of the caller's: it blocks.
pub fn watch(
    hypervisor: &Hypervisor,
    every: Duration,
    changes: &Sender<Vec<Machine>>,
    stop: &Receiver<()>,
) -> Result<()> {
    let mut last = None;
    loop {
        let machines: Vec<Machine> = hypervisor.machines()?.collect();
        if last.as_ref() != Some(&machines) {
            last = Some(machines.clone());
            select! {
                send(changes, machines) -> sent => if sent.is_err() { return Ok(()) },
                recv(stop) -> _ => return Ok(()),
            }
        }
        select! {
            recv(stop) -> _ => return Ok(()),
            default(every) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{os::unix::net::UnixListener, path::Path};

    use super::*;

    /// A directory of a test's own, gone when it is.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(test: &str) -> Scratch {
            let dir =
                std::env::temp_dir().join(format!("derisk-qemu-{}-{test}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn exit_status() {
        assert_eq!(Exit::from_status(&json!({ "exited": false })), Ok(None));
        let exit = Exit::from_status(&json!({
            "exited": true,
            "exitcode": 0,
            "out-data": STANDARD.encode("org.derisk.files\n"),
        }))
        .unwrap()
        .unwrap();
        assert!(exit.success());
        assert_eq!(exit.stdout, b"org.derisk.files\n");
        assert!(exit.stderr.is_empty());

        let killed = Exit::from_status(&json!({ "exited": true, "signal": 9 }))
            .unwrap()
            .unwrap();
        assert!(!killed.success());
        assert_eq!(killed.signal, Some(9));

        assert!(Exit::from_status(&json!({})).is_err());
        assert!(Exit::from_status(&json!({ "exited": true, "out-data": "%%" })).is_err());
    }

    /// A machine's folder as pm leaves it, running as this test's own
    /// process when `running`.
    fn machine(root: &Path, name: &str, running: bool) {
        let folder = root.join(name);
        fs::create_dir_all(&folder).unwrap();
        fs::write(
            folder.join(RECORD),
            crate::machine::tests::RECORD_JSON.replace("\"work\"", &format!("\"{name}\"")),
        )
        .unwrap();
        if running {
            fs::write(
                folder.join(format!("{name}.pid")),
                std::process::id().to_string(),
            )
            .unwrap();
        }
    }

    #[test]
    fn lists_the_machines_in_their_folders() {
        let root = Scratch::new("list");
        machine(root.path(), "work", false);
        machine(root.path(), "personal", false);
        // pm's shared folders, and one still being made, are no machines.
        fs::create_dir_all(root.path().join("templates")).unwrap();
        fs::create_dir_all(root.path().join("half")).unwrap();
        let hypervisor = Hypervisor::at(root.path());
        let names: Vec<String> = hypervisor.machines().unwrap().map(|m| m.name).collect();
        assert_eq!(names, ["personal", "work"]);
        assert!(matches!(
            hypervisor.machine("gone"),
            Err(Error::NoMachine(_))
        ));
        assert!(matches!(
            hypervisor.machine("../work"),
            Err(Error::NoMachine(_))
        ));
        // A machine whose pid is someone else's, here this test's, is off.
        machine(root.path(), "work", true);
        assert_eq!(hypervisor.machine("work").unwrap().state, State::Off);
        assert_eq!(hypervisor.machine_at(66054).unwrap(), None);

        let none = Hypervisor::at(&root.path().join("nothing"));
        assert_eq!(none.machines().unwrap().count(), 0);
    }

    #[test]
    fn a_machine_runs_while_its_qemu_does() {
        let root = Scratch::new("runs");
        // This process's own name, as quickemu would have named QEMU's.
        let comm = fs::read_to_string("/proc/self/comm").unwrap();
        let name = comm.trim_end();
        machine(root.path(), name, true);
        let hypervisor = Hypervisor::at(root.path());
        let running = hypervisor.machine(name).unwrap();
        assert_eq!(running.state, State::Running);
        assert_eq!(hypervisor.machine_at(66054).unwrap().unwrap().name, name);
    }

    /// A guest agent that answers a sync, after a stale reply, then answers
    /// one command with `answer`.
    fn agent(socket: &Path, answer: &'static str) -> std::thread::JoinHandle<String> {
        let listener = UnixListener::bind(socket).unwrap();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(&stream);
            let mut writer = &stream;
            let mut first = Vec::new();
            reader.read_until(b'\n', &mut first).unwrap();
            assert_eq!(first[0], 0xff);
            let sync: Value = serde_json::from_slice(&first[1..]).unwrap();
            assert_eq!(sync["execute"], "guest-sync-delimited");
            let id = sync["arguments"]["id"].as_u64().unwrap();
            writer.write_all(b"{\"return\": {\"pid\": 1}}\n").unwrap();
            writer.write_all(&[0xff]).unwrap();
            writeln!(writer, "{{\"return\": 12}}").unwrap();
            writeln!(writer, "{{\"return\": {id}}}").unwrap();
            let mut command = String::new();
            reader.read_line(&mut command).unwrap();
            writeln!(writer, "{answer}").unwrap();
            command
        })
    }

    #[test]
    fn the_agent_is_synced_with_before_it_is_asked() {
        let root = Scratch::new("agent");
        let comm = fs::read_to_string("/proc/self/comm").unwrap();
        let name = comm.trim_end().to_owned();
        machine(root.path(), &name, true);
        let socket = root.path().join(&name).join(format!("{name}-agent.sock"));
        let hypervisor = Hypervisor::at(root.path());

        let asked = agent(&socket, r#"{"return": {"pid": 42}}"#);
        let program = Program {
            path: "/bin/true".into(),
            ..Program::default()
        };
        assert_eq!(hypervisor.spawn(&name, &program).unwrap(), 42);
        let command: Value = serde_json::from_str(&asked.join().unwrap()).unwrap();
        assert_eq!(command["execute"], "guest-exec");
        assert_eq!(command["arguments"]["path"], "/bin/true");

        fs::remove_file(&socket).unwrap();
        let asked = agent(
            &socket,
            r#"{"error": {"class": "GenericError", "desc": "no such file"}}"#,
        );
        let error = hypervisor.exit(&name, 42).unwrap_err();
        asked.join().unwrap();
        assert!(error.to_string().contains("no such file"), "{error}");
    }

    #[test]
    fn a_machine_without_an_agent_is_not_ready() {
        let root = Scratch::new("silent");
        machine(root.path(), "work", false);
        let hypervisor = Hypervisor::at(root.path());
        assert!(!hypervisor.agent_ready("work").unwrap());
        // Nothing to shut down or pull the plug of.
        hypervisor.shut_down("work").unwrap();
        hypervisor.force_off("work").unwrap();
    }

    #[test]
    fn the_monitor_is_told_and_left() {
        let root = Scratch::new("monitor");
        let comm = fs::read_to_string("/proc/self/comm").unwrap();
        let name = comm.trim_end().to_owned();
        machine(root.path(), &name, true);
        let listener = UnixListener::bind(
            root.path()
                .join(&name)
                .join(format!("{name}-monitor.socket")),
        )
        .unwrap();
        let heard = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(b"QEMU monitor\n(qemu) ").unwrap();
            let mut heard = String::new();
            stream.read_to_string(&mut heard).unwrap();
            heard
        });
        Hypervisor::at(root.path()).shut_down(&name).unwrap();
        assert_eq!(heard.join().unwrap(), "system_powerdown\n");
    }
}
