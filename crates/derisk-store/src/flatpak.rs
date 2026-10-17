//! Where flatpak runs for the store, and what it answers.
//!
//! On a PC the Flatpak apps live in a machine of their own (losos-desktop's
//! `flatpak` machine), so the store runs flatpak there through the machine's
//! guest agent and reads Flathub's catalogue from the machine's disk. A
//! phone, which has no machines, keeps Flatpak on itself, and so does any
//! computer without that machine. Both are a [`Flatpak`].

use std::{
    io::Read,
    process::{Command, Stdio},
};

/// The remote the store offers apps from.
pub const REMOTE: &str = "flathub";

/// The machine the PC's Flatpak apps run in (losos-desktop's
/// `losos.machines.guests.flatpak`).
pub const MACHINE: &str = "flatpak";

/// An installed app, as `flatpak list` lists it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Installed {
    /// Its application ID.
    pub id: String,
    /// Its version, which some apps leave empty.
    pub version: String,
    /// Its branch, `stable` for most.
    pub branch: String,
    /// The remote it came from.
    pub origin: String,
    /// `system` or `user`.
    pub installation: String,
}

/// Where flatpak runs.
pub trait Flatpak: Send + Sync {
    /// The machine it runs in, or `None` for this computer.
    fn machine(&self) -> Option<&str>;

    /// Runs `flatpak` with `args`, handing each line it prints, progress
    /// included, to `line` as it comes when this place can. Returns what it
    /// printed, or why it failed in its own words.
    fn run(&self, args: &[&str], line: &mut dyn FnMut(&str)) -> Result<String, String>;

    /// The bytes of `path`, a file of the system installation.
    fn read(&self, path: &str) -> Result<Vec<u8>, String>;

    /// Opens installed app `id` on the desktop.
    fn open(&self, id: &str) -> Result<(), String>;

    /// The installations a catalogue is looked for in, in order.
    fn installations(&self) -> Vec<String> {
        vec!["/var/lib/flatpak".into()]
    }
}

/// The architecture as flatpak names it, which for the two the OS runs on
/// is Rust's own name.
pub fn arch() -> &'static str {
    std::env::consts::ARCH
}

/// Where `remote`'s catalogue is in an installation: flatpak 1.x writes the
/// compressed one, and the uncompressed one older trees still have.
pub fn catalogue_paths(installation: &str, remote: &str) -> [String; 2] {
    let dir = format!("{installation}/appstream/{remote}/{}/active", arch());
    [
        format!("{dir}/appstream.xml.gz"),
        format!("{dir}/appstream.xml"),
    ]
}

/// Where a cached icon is: `icons/<size>x<size>/<file>` beside the
/// catalogue.
pub fn icon_path(installation: &str, remote: &str, file: &str, size: u32) -> String {
    format!(
        "{installation}/appstream/{remote}/{}/active/icons/{size}x{size}/{file}",
        arch()
    )
}

/// `flatpak list --app --columns=application,version,branch,origin,installation`'s
/// output, tab-separated as flatpak prints it to a pipe.
pub fn parse_installed(output: &str) -> impl Iterator<Item = Installed> + '_ {
    output.lines().filter_map(|line| {
        let mut columns = line.split('\t').map(str::trim);
        let id = columns.next().filter(|id| !id.is_empty())?;
        Some(Installed {
            id: id.to_owned(),
            version: columns.next().unwrap_or_default().to_owned(),
            branch: columns.next().unwrap_or_default().to_owned(),
            origin: columns.next().unwrap_or_default().to_owned(),
            installation: columns.next().unwrap_or("system").to_owned(),
        })
    })
}

/// The arguments that list the installed apps.
pub const LIST: [&str; 3] = [
    "list",
    "--app",
    "--columns=application,version,branch,origin,installation",
];

/// The arguments that list the apps with an update waiting.
pub const UPDATES: [&str; 4] = ["remote-ls", "--updates", "--app", "--columns=application"];

/// The newest percentage in a line of flatpak's progress (`Installing…
/// ████▍ 45%  1.2 MB/s`), as a fraction.
pub fn progress(line: &str) -> Option<f32> {
    line.split_whitespace().rev().find_map(|word| {
        let number: f32 = word.strip_suffix('%')?.parse().ok()?;
        (0.0..=100.0).contains(&number).then_some(number / 100.0)
    })
}

/// flatpak on this computer.
#[derive(Clone, Debug, Default)]
pub struct Local;

impl Local {
    fn program() -> std::ffi::OsString {
        derisk_sys::env::var_os("DERISK_FLATPAK").unwrap_or_else(|| "flatpak".into())
    }
}

impl Flatpak for Local {
    fn machine(&self) -> Option<&str> {
        None
    }

    fn run(&self, args: &[&str], line: &mut dyn FnMut(&str)) -> Result<String, String> {
        let mut child = Command::new(Self::program())
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Could not run flatpak: {e}"))?;
        let stderr = child.stderr.take();
        // stderr on a thread of its own, so a full pipe on either side
        // never stops flatpak.
        let (output, errors) = std::thread::scope(|scope| {
            let errors = scope.spawn(move || {
                let mut text = String::new();
                if let Some(mut stderr) = stderr {
                    let _ = stderr.read_to_string(&mut text);
                }
                text
            });
            let mut output = String::new();
            if let Some(mut stdout) = child.stdout.take() {
                // Progress is redrawn with carriage returns rather than new
                // lines, so either ends a line.
                let mut chunk = [0; 4096];
                let mut pending = Vec::new();
                while let Ok(read) = stdout.read(&mut chunk) {
                    if read == 0 {
                        break;
                    }
                    for &byte in &chunk[..read] {
                        if byte == b'\r' || byte == b'\n' {
                            let text = String::from_utf8_lossy(&pending);
                            if !text.trim().is_empty() {
                                line(text.trim_end());
                            }
                            pending.clear();
                        } else {
                            pending.push(byte);
                        }
                    }
                    output.push_str(&String::from_utf8_lossy(&chunk[..read]));
                }
            }
            (output, errors.join().unwrap_or_default())
        });
        let status = child.wait().map_err(|e| e.to_string())?;
        if status.success() {
            Ok(output)
        } else {
            Err(failure(&errors, &output))
        }
    }

    fn read(&self, path: &str) -> Result<Vec<u8>, String> {
        std::fs::read(path).map_err(|e| format!("{path}: {e}"))
    }

    fn open(&self, id: &str) -> Result<(), String> {
        Command::new(Self::program())
            .args(["run", id])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(drop)
            .map_err(|e| format!("Could not run flatpak: {e}"))
    }

    fn installations(&self) -> Vec<String> {
        // A person without the system remote (a phone set up by hand) may
        // have Flathub for themselves only.
        let mut all = vec!["/var/lib/flatpak".to_owned()];
        if let Ok(home) = derisk_sys::env::var("HOME") {
            let data = derisk_sys::env::var("XDG_DATA_HOME")
                .unwrap_or_else(|_| format!("{home}/.local/share"));
            all.push(format!("{data}/flatpak"));
        }
        all
    }
}

/// What to tell the person of a failed flatpak: its last error line, which
/// says what went wrong (`error: No remote refs found similar to ‘x’`).
pub fn failure(stderr: &str, stdout: &str) -> String {
    let last = |text: &str| {
        text.lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty())
            .map(str::to_owned)
    };
    last(stderr)
        .or_else(|| last(stdout))
        .map(|l| l.strip_prefix("error: ").map(str::to_owned).unwrap_or(l))
        .unwrap_or_else(|| "flatpak failed".into())
}

#[cfg(not(target_arch = "wasm32"))]
pub use machine::Machine;

#[cfg(not(target_arch = "wasm32"))]
mod machine {
    use std::time::{Duration, Instant};

    use derisk_machines::{Hypervisor, Program};

    use super::{Flatpak, failure};

    /// flatpak in the guest template's system profile; the agent's own
    /// `PATH` is not a login's.
    const FLATPAK: &str = "/run/current-system/sw/bin/flatpak";

    /// How long an install, an update or a catalogue refresh may take:
    /// a large app over a slow line is a long download.
    const RUN_WAIT: Duration = Duration::from_secs(60 * 60);

    /// flatpak in a machine, through its guest agent, as the guest's root,
    /// whose system installation is the one the machine's apps run from.
    pub struct Machine {
        name: String,
        hypervisor: Hypervisor,
    }

    impl Machine {
        /// The machine called `name`, if libvirt has one.
        pub fn find(name: &str) -> Option<Machine> {
            let hypervisor = Hypervisor::open().ok()?;
            hypervisor.machine(name).ok()?;
            Some(Machine {
                name: name.to_owned(),
                hypervisor,
            })
        }

        /// Starts the machine if it is off, and waits for its agent.
        fn up(&self) -> Result<(), String> {
            self.hypervisor
                .boot(&self.name)
                .map_err(|e| format!("Could not start {}: {e}", self.name))
        }
    }

    impl Flatpak for Machine {
        fn machine(&self) -> Option<&str> {
            Some(&self.name)
        }

        fn run(&self, args: &[&str], line: &mut dyn FnMut(&str)) -> Result<String, String> {
            self.up()?;
            let program = Program {
                path: FLATPAK.into(),
                args: args.iter().map(|&a| a.to_owned()).collect(),
                env: vec!["LANG=C.UTF-8".into()],
                capture_output: true,
            };
            let pid = self
                .hypervisor
                .spawn(&self.name, &program)
                .map_err(|e| e.to_string())?;
            // The agent hands output over only once the program has exited,
            // so there is nothing to show until then.
            line(&format!("Working in {}…", self.name));
            let deadline = Instant::now() + RUN_WAIT;
            let exit = loop {
                if let Some(exit) = self
                    .hypervisor
                    .exit(&self.name, pid)
                    .map_err(|e| e.to_string())?
                {
                    break exit;
                }
                if Instant::now() >= deadline {
                    return Err(format!("flatpak did not finish in {}", self.name));
                }
                std::thread::sleep(Duration::from_millis(250));
            };
            let stdout = String::from_utf8_lossy(&exit.stdout).into_owned();
            if exit.success() {
                Ok(stdout)
            } else {
                Err(failure(&String::from_utf8_lossy(&exit.stderr), &stdout))
            }
        }

        fn read(&self, path: &str) -> Result<Vec<u8>, String> {
            self.up()?;
            self.hypervisor
                .read_file(&self.name, path)
                .map_err(|e| e.to_string())
        }

        fn open(&self, id: &str) -> Result<(), String> {
            self.hypervisor
                .open_app(&self.name, id)
                .map_err(|e| e.to_string())
        }
    }
}
