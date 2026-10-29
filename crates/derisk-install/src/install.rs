//! The protocol between `derisk installer` and the program that installs.
//!
//! derisk draws the installer; it does not know how an operating system is
//! put on a disk. That is the backend's: a program the installer starts and
//! talks to in JSON lines, requests on its stdin and events on its stdout.
//! LosOS's backend is `losos-installer serve`, which lays the disk out with
//! systemd-repart and fills it with systemd-sysupdate; any other system can
//! supply its own.
//!
//! The backend speaks first, with [`Event::Hello`], and answers each
//! [`Request`] with events. An install sends [`Event::Step`] and
//! [`Event::Line`] as it goes and ends with [`Event::Installed`] or
//! [`Event::Failed`]. What the backend prints to stderr goes to the journal.
//!
//! ```text
//! → {"event":"hello","name":"LosOS Desktop","source":"https://…","release":null}
//! ← {"method":"disks"}
//! → {"event":"disks","disks":[{"path":"/dev/vda","name":"vda","model":"","size":21474836480,"removable":false}]}
//! ← {"method":"install","disk":"/dev/vda"}
//! → {"event":"steps","labels":["Partitioning the disk","…"]}
//! → {"event":"step","index":0,"count":4,"label":"Partitioning the disk"}
//! → {"event":"line","text":"…"}
//! → {"event":"installed"}
//! ← {"method":"reboot"}
//! ```
//!
//! A backend that can encrypt the installed system's data says
//! `"encryption":true` in its hello. An install that asks for a PIN at
//! startup sends it, and hears the recovery key back before it ends:
//!
//! ```text
//! ← {"method":"install","disk":"/dev/vda","pin":"246813"}
//! → {"event":"steps","labels":["Partitioning the disk","…","Encrypting the system's data","Finishing"]}
//! → {"event":"recovery_key","key":"fgbjnkrc-ccjhnveb-…"}
//! → {"event":"installed"}
//! ```
//!
//! A backend that can report a failed install says where in
//! [`Event::Hello`]'s `report`. After [`Event::Failed`], the installer can
//! then ask for the report to show ([`Request::Report`]), and send it once
//! the person has read it ([`Request::ReportSend`]):
//!
//! ```text
//! ← {"method":"report"}
//! → {"event":"report","repository":"owner/name","title":"…","body":"…"}
//! ← {"method":"report_send","note":"It stopped at 3%"}
//! → {"event":"report_sign_in","uri":"https://github.com/login/device","code":"WDJB-MJHT"}
//! → {"event":"reported","url":"https://github.com/owner/name/issues/7","existing":false}
//! ```
//!
//! Getting online is not the backend's: the installer joins networks itself
//! (see [`crate::network`]), since a live system and an installed one do
//! that the same way.

use serde::{Deserialize, Serialize};

/// What the installer asks of the backend.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Request {
    /// List the disks that can be installed onto.
    Disks,
    /// Erase `disk` and install onto it.
    Install {
        /// The disk's device node, from [`Disk::path`].
        disk: String,
        /// The PIN the installed system asks for at every start, its data
        /// encrypted with it, when the person asked for one. Only to a
        /// backend whose [`Event::Hello`] says `encryption`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pin: Option<String>,
    },
    /// Restart into the installed system.
    Reboot,
    /// Power off.
    PowerOff,
    /// Write up the install that just failed, as [`Event::Report`].
    Report,
    /// Send the report written last, with the person's own words added.
    ReportSend {
        /// What the person wrote, which may be empty.
        note: String,
    },
    /// Stop waiting for the person to sign in.
    ReportCancel,
}

/// A disk the backend offers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Disk {
    /// The device node, such as `/dev/nvme0n1`.
    pub path: String,
    /// The kernel's name for it, such as `nvme0n1`.
    pub name: String,
    /// The model, when the disk reports one.
    #[serde(default)]
    pub model: String,
    /// Bytes.
    pub size: u64,
    /// Whether it is removable, such as a USB stick or an SD card.
    #[serde(default)]
    pub removable: bool,
}

impl Disk {
    /// The size in decimal units, as drives are sold.
    pub fn size_text(&self) -> String {
        human_size(self.size)
    }

    /// How a list names it: the model, or the kernel name when there is
    /// none.
    pub fn title(&self) -> &str {
        if self.model.trim().is_empty() {
            &self.name
        } else {
            self.model.trim()
        }
    }
}

/// What the backend tells the installer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// The backend is ready. `name` is the system being installed, and
    /// `source` where it comes from, shown before anything is erased.
    Hello {
        /// The system's name.
        name: String,
        /// Where the system is downloaded from, if anywhere.
        #[serde(default)]
        source: Option<String>,
        /// A release on a local disk that the backend installs from instead
        /// of `source`, so no network is needed. The backend may send Hello
        /// again when such a disk is plugged in or removed.
        #[serde(default)]
        release: Option<String>,
        /// Where a failed install can be reported, such as a repository's
        /// `owner/name`, when the backend can send reports.
        #[serde(default)]
        report: Option<String>,
        /// Whether the backend can encrypt the installed system's data
        /// with a PIN asked for at every start, on this computer.
        #[serde(default)]
        encryption: bool,
    },
    /// The answer to [`Request::Disks`].
    Disks {
        /// The disks, the installer's own medium left out.
        disks: Vec<Disk>,
    },
    /// The steps an install takes, sent before the first [`Event::Step`] so
    /// the whole list shows from the start.
    Steps {
        /// What each step does, as sentence fragments.
        labels: Vec<String>,
    },
    /// An install moved on to step `index` of `count`.
    Step {
        /// Zero-based.
        index: usize,
        /// How many steps there are.
        count: usize,
        /// What it is doing, as a sentence fragment.
        label: String,
        /// How far this step is, from 0 to 1, when the backend can tell.
        #[serde(default)]
        fraction: Option<f32>,
    },
    /// A line of output from the tools doing the work.
    Line {
        /// The line.
        text: String,
    },
    /// The key that unlocks the encrypted data without the PIN, sent once
    /// before [`Event::Installed`] when a PIN was asked for. Shown to the
    /// person, never logged.
    RecoveryKey {
        /// Eight groups of eight letters.
        key: String,
    },
    /// The install finished.
    Installed,
    /// The request failed.
    Failed {
        /// Why, in a sentence.
        message: String,
    },
    /// The answer to [`Request::Report`]: exactly what would be sent.
    Report {
        /// Where it goes.
        repository: String,
        /// The issue's title.
        title: String,
        /// The issue's text, in Markdown.
        body: String,
    },
    /// To send the report, the person signs in on this machine or another.
    ReportSignIn {
        /// The page to sign in at.
        uri: String,
        /// What to enter there.
        code: String,
    },
    /// The report was filed.
    Reported {
        /// Where it can be read.
        url: String,
        /// Whether it went onto a report of the same failure that was
        /// already open, rather than a new one.
        #[serde(default)]
        existing: bool,
    },
    /// The report could not be written or sent. The install's own failure
    /// stands as it was.
    ReportFailed {
        /// Why, in a sentence.
        message: String,
    },
}

/// Parses one line from the backend; `None` for a line that is not an
/// event (a backend may print other things while starting).
pub fn parse_event(line: &str) -> Option<Event> {
    serde_json::from_str(line.trim()).ok()
}

/// One request as a line to write to the backend.
pub fn request_line(request: &Request) -> String {
    let mut line = serde_json::to_string(request).expect("requests always serialize");
    line.push('\n');
    line
}

/// Bytes in decimal units: `21.5 GB`.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
