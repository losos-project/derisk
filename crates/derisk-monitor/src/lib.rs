//! derisk System Monitor: CPU, memory, swap, load, and processes, read from
//! Linux `/proc`, with a CPU history graph and an End process action.
//!
//! [`Sampler`] reads `/proc` (or any directory laid out like it, for tests);
//! [`MonitorApp`] draws its samples.
//!
//! ```
//! use derisk_monitor::proc::{parse_pid_stat, parse_stat};
//!
//! let a = parse_stat("cpu  10 0 10 80 0 0 0 0 0 0\n").unwrap();
//! let b = parse_stat("cpu  40 0 20 140 0 0 0 0 0 0\n").unwrap();
//! assert_eq!(b.usage_since(a), 40.0);
//! let stat = parse_pid_stat("42 (my (odd) app) S 1 42 42 0 -1 0 0 0 0 0 7 3 0 0 20 0 4 0").unwrap();
//! assert_eq!((stat.name.as_str(), stat.ticks, stat.threads), ("my (odd) app", 10, 4));
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod proc;

use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    time::Duration,
};

use derisk_sys::{fs, time::Instant};
use mcsapi_ui::{App, Theme, egui};
use proc::{CpuTimes, Memory, format_duration, format_kib};

/// How many CPU samples the history graph keeps.
pub const HISTORY: usize = 60;

/// One running process.
#[derive(Clone, Debug, PartialEq)]
pub struct Process {
    /// Process ID.
    pub pid: u32,
    /// Executable name.
    pub name: String,
    /// One-letter state.
    pub state: char,
    /// Share of all CPUs since the previous sample, 0 to 100.
    pub cpu: f32,
    /// Resident memory in KiB.
    pub rss: u64,
    /// Number of threads.
    pub threads: u64,
}

/// One reading of the whole system.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    /// Busy share of all CPUs since the previous sample, 0 to 100.
    pub cpu: f32,
    /// Number of logical CPUs.
    pub cpus: usize,
    /// Memory and swap.
    pub memory: Memory,
    /// Load averages over 1, 5, and 15 minutes.
    pub load: [f32; 3],
    /// Seconds since boot.
    pub uptime: f64,
    /// Processes, in PID order.
    pub processes: Vec<Process>,
}

/// Reads `/proc` and turns cumulative counters into rates.
#[derive(Debug)]
pub struct Sampler {
    root: PathBuf,
    previous_cpu: Option<CpuTimes>,
    previous_ticks: HashMap<u32, u64>,
    /// CPU usage history, oldest first, at most [`HISTORY`] entries.
    pub history: VecDeque<f32>,
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new("/proc")
    }
}

impl Sampler {
    /// Reads from `root`, normally `/proc`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            previous_cpu: None,
            previous_ticks: HashMap::new(),
            history: VecDeque::with_capacity(HISTORY),
        }
    }

    fn read(&self, path: impl AsRef<Path>) -> Option<String> {
        fs::read_to_string(self.root.join(path)).ok()
    }

    /// Takes a sample. Rates are zero on the first call. Processes that exit
    /// while being read are skipped.
    pub fn sample(&mut self) -> Snapshot {
        let stat = self.read("stat").unwrap_or_default();
        let cpu_times = proc::parse_stat(&stat).unwrap_or_default();
        let elapsed = self
            .previous_cpu
            .map_or(0, |previous| cpu_times.total.saturating_sub(previous.total));
        let cpu = self
            .previous_cpu
            .map_or(0.0, |previous| cpu_times.usage_since(previous));
        self.previous_cpu = Some(cpu_times);
        if self.history.len() == HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(cpu);

        let mut processes = Vec::new();
        let mut ticks = HashMap::new();
        if let Ok(dir) = fs::read_dir(&self.root) {
            for item in dir.flatten() {
                let Some(pid) = item
                    .file_name()
                    .to_str()
                    .and_then(|n| n.parse::<u32>().ok())
                else {
                    continue;
                };
                let Some(stat) = self
                    .read(format!("{pid}/stat"))
                    .as_deref()
                    .and_then(proc::parse_pid_stat)
                else {
                    continue;
                };
                let rss = self
                    .read(format!("{pid}/status"))
                    .map_or(0, |status| proc::parse_pid_rss(&status));
                let used = self
                    .previous_ticks
                    .get(&pid)
                    .map_or(0, |&before| stat.ticks.saturating_sub(before));
                ticks.insert(pid, stat.ticks);
                processes.push(Process {
                    pid,
                    name: stat.name,
                    state: stat.state,
                    cpu: if elapsed == 0 {
                        0.0
                    } else {
                        (used as f64 * 100.0 / elapsed as f64).min(100.0) as f32
                    },
                    rss,
                    threads: stat.threads,
                });
            }
        }
        processes.sort_by_key(|p| p.pid);
        self.previous_ticks = ticks;

        Snapshot {
            cpu,
            cpus: proc::parse_cpu_count(&stat).max(1),
            memory: self
                .read("meminfo")
                .as_deref()
                .and_then(proc::parse_meminfo)
                .unwrap_or_default(),
            load: self
                .read("loadavg")
                .as_deref()
                .and_then(proc::parse_loadavg)
                .unwrap_or_default(),
            uptime: self
                .read("uptime")
                .as_deref()
                .and_then(proc::parse_uptime)
                .unwrap_or_default(),
            processes,
        }
    }
}

/// Process table column to sort by.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SortKey {
    /// Highest CPU first.
    #[default]
    Cpu,
    /// Most memory first.
    Memory,
    /// Lowest PID first.
    Pid,
    /// Name, A to Z.
    Name,
}

/// Sends SIGTERM to a process with the `kill` command.
#[cfg(not(target_family = "wasm"))]
pub fn terminate(pid: u32) -> std::io::Result<()> {
    let status = std::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("kill exited with {status}")))
    }
}

/// Ends one of the browser demo's made-up processes by taking it out of
/// its `/proc`, the way an ended process leaves.
#[cfg(target_family = "wasm")]
pub fn terminate(pid: u32) -> std::io::Result<()> {
    fs::remove_dir_all(format!("/proc/{pid}"))
}

/// The System Monitor app.
#[derive(Debug)]
pub struct MonitorApp {
    sampler: Sampler,
    /// The latest sample.
    pub snapshot: Snapshot,
    last: Option<Instant>,
    /// Time between samples.
    pub interval: Duration,
    /// Process table order.
    pub sort: SortKey,
    filter: String,
    selected: Option<u32>,
    confirm: Option<u32>,
    status: Option<String>,
}

impl Default for MonitorApp {
    fn default() -> Self {
        Self::new(Sampler::default())
    }
}

impl MonitorApp {
    /// Shows samples from `sampler`, refreshing every second.
    pub fn new(sampler: Sampler) -> Self {
        Self {
            sampler,
            snapshot: Snapshot::default(),
            last: None,
            interval: Duration::from_secs(1),
            sort: SortKey::Cpu,
            filter: String::new(),
            selected: None,
            confirm: None,
            status: None,
        }
    }

    /// Takes a new sample now.
    pub fn refresh(&mut self) {
        self.snapshot = self.sampler.sample();
        self.last = Some(Instant::now());
    }

    /// Visible processes, filtered and sorted.
    pub fn rows(&self) -> Vec<&Process> {
        let filter = self.filter.to_lowercase();
        let mut rows: Vec<&Process> = self
            .snapshot
            .processes
            .iter()
            .filter(|p| {
                filter.is_empty()
                    || p.name.to_lowercase().contains(&filter)
                    || p.pid.to_string() == filter
            })
            .collect();
        match self.sort {
            SortKey::Cpu => rows.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(a.pid.cmp(&b.pid))),
            SortKey::Memory => rows.sort_by(|a, b| b.rss.cmp(&a.rss).then(a.pid.cmp(&b.pid))),
            SortKey::Pid => rows.sort_by_key(|p| p.pid),
            SortKey::Name => rows.sort_by(|a, b| {
                a.name
                    .to_lowercase()
                    .cmp(&b.name.to_lowercase())
                    .then(a.pid.cmp(&b.pid))
            }),
        }
        rows
    }

    fn summary(&self, ui: &mut egui::Ui, theme: &Theme) {
        let s = &self.snapshot;
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_width(ui.available_width() * 0.45);
                ui.strong(format!("CPU {:.0}%  ·  {} cores", s.cpu, s.cpus));
                graph(ui, &self.sampler.history, theme);
            });
            ui.add_space(16.0);
            ui.vertical(|ui| {
                let m = &s.memory;
                bar(ui, "Memory", m.used(), m.total, theme);
                bar(ui, "Swap", m.swap_used(), m.swap_total, theme);
                ui.label(format!(
                    "Load {:.2} {:.2} {:.2}  ·  Up {}  ·  {} processes",
                    s.load[0],
                    s.load[1],
                    s.load[2],
                    format_duration(s.uptime),
                    s.processes.len()
                ));
            });
        });
    }

    fn table(&mut self, ui: &mut egui::Ui) {
        // Wraps on a phone. A row wider than the window would widen the
        // panel under it and push the table's columns off the screen.
        ui.horizontal_wrapped(|ui| {
            let color = ui.visuals().weak_text_color();
            derisk_icons::show(ui, "system-search", 16.0, color);
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("Name or PID")
                    .desired_width((ui.available_width() * 0.4).min(240.0)),
            );
            ui.label("Sort");
            for (key, label) in [
                (SortKey::Cpu, "CPU"),
                (SortKey::Memory, "Memory"),
                (SortKey::Pid, "PID"),
                (SortKey::Name, "Name"),
            ] {
                ui.selectable_value(&mut self.sort, key, label);
            }
            ui.separator();
            let selected = self.selected;
            if ui
                .add_enabled(selected.is_some(), egui::Button::new("End process"))
                .clicked()
            {
                self.confirm = selected;
            }
        });
        if let Some(pid) = self.confirm {
            ui.horizontal(|ui| {
                ui.label(format!("Send SIGTERM to process {pid}?"));
                if ui.button("End process").clicked() {
                    self.status = Some(match terminate(pid) {
                        Ok(()) => format!("Asked process {pid} to end"),
                        Err(error) => format!("Could not end {pid}: {error}"),
                    });
                    self.confirm = None;
                }
                if ui.button("Cancel").clicked() {
                    self.confirm = None;
                }
            });
        }
        if let Some(status) = &self.status {
            ui.label(status);
        }
        let mut select = None;
        let rows = self.rows();
        let row_height = ui.spacing().interact_size.y;
        let border = ui.visuals().widgets.noninteractive.bg_stroke.color;
        let width = ui.available_width();
        let (header, _) =
            ui.allocate_exact_size(egui::vec2(width, row_height), egui::Sense::hover());
        cells(
            ui,
            header,
            &COLUMNS.map(|(name, _, _)| egui::RichText::new(name).strong()),
        );
        ui.painter().hline(
            header.x_range(),
            header.bottom(),
            egui::Stroke::new(1.0, border),
        );
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let width = ui.available_width();
                for (i, p) in rows.iter().enumerate() {
                    let selected = self.selected == Some(p.pid);
                    // One target per row: clicking a name or a number selects
                    // the process, not only its PID.
                    let (rect, row) =
                        ui.allocate_exact_size(egui::vec2(width, row_height), egui::Sense::click());
                    row.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::WidgetType::SelectableLabel,
                            true,
                            selected,
                            format!("{} {}", p.pid, p.name),
                        )
                    });
                    let visuals = ui.visuals();
                    let fill = if selected {
                        Some(visuals.selection.bg_fill)
                    } else if row.hovered() {
                        Some(visuals.widgets.hovered.weak_bg_fill)
                    } else if i % 2 == 1 {
                        Some(visuals.faint_bg_color)
                    } else {
                        None
                    };
                    if let Some(fill) = fill {
                        ui.painter().rect_filled(rect, 2.0, fill);
                    }
                    cells(
                        ui,
                        rect,
                        &[
                            p.pid.to_string(),
                            p.name.clone(),
                            p.state.to_string(),
                            format!("{:.1}%", p.cpu),
                            format_kib(p.rss),
                            p.threads.to_string(),
                        ]
                        .map(egui::RichText::new),
                    );
                    if row.clicked() {
                        select = Some(p.pid);
                    }
                }
            });
        if select.is_some() {
            self.selected = select;
        }
    }
}

/// Process table columns: a header, its share of the row and the least it
/// may have, so numbers stay whole on a phone. A share of 0 takes what the
/// others leave, so the table spans the window.
const COLUMNS: [(&str, f32, f32); 6] = [
    ("PID", 0.09, 56.0),
    ("Name", 0.0, 0.0),
    ("State", 0.12, 44.0),
    ("CPU", 0.1, 52.0),
    ("Memory", 0.13, 72.0),
    ("Threads", 0.1, 36.0),
];

/// Lays `texts` out across `row` in [`COLUMNS`].
fn cells(ui: &mut egui::Ui, row: egui::Rect, texts: &[egui::RichText; 6]) {
    let width = |share: f32, least: f32| (share * row.width()).max(least);
    let fixed: f32 = COLUMNS
        .iter()
        .filter(|(_, share, _)| *share > 0.0)
        .map(|(_, share, least)| width(*share, *least))
        .sum();
    let pad = 4.0;
    let mut x = row.left();
    for ((_, share, least), text) in COLUMNS.iter().zip(texts) {
        let w = if *share == 0.0 {
            (row.width() - fixed).max(0.0)
        } else {
            width(*share, *least)
        };
        let cell = egui::Rect::from_min_size(
            egui::pos2(x + pad, row.top()),
            egui::vec2((w - 2.0 * pad).max(0.0), row.height()),
        );
        // `put` centers what it adds; cells read from the left.
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(cell)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| ui.add(egui::Label::new(text.clone()).truncate().selectable(false)),
        );
        x += w;
    }
}

fn bar(ui: &mut egui::Ui, label: &str, used: u64, total: u64, theme: &Theme) {
    let fraction = if total == 0 {
        0.0
    } else {
        used as f32 / total as f32
    };
    ui.horizontal(|ui| {
        ui.strong(label);
        ui.label(format!("{} of {}", format_kib(used), format_kib(total)));
    });
    ui.add(
        egui::ProgressBar::new(fraction)
            .fill(theme.accent)
            .desired_height(8.0),
    );
}

fn graph(ui: &mut egui::Ui, history: &VecDeque<f32>, theme: &Theme) {
    let size = egui::vec2(ui.available_width(), 64.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, theme.surface);
    let step = rect.width() / (HISTORY - 1) as f32;
    let offset = HISTORY - history.len();
    let points: Vec<egui::Pos2> = history
        .iter()
        .enumerate()
        .map(|(i, &cpu)| {
            egui::pos2(
                rect.left() + (offset + i) as f32 * step,
                rect.bottom() - rect.height() * cpu.clamp(0.0, 100.0) / 100.0,
            )
        })
        .collect();
    painter.add(egui::Shape::line(
        points,
        egui::Stroke::new(2.0, theme.accent),
    ));
}

impl App for MonitorApp {
    fn title(&self) -> &str {
        "System Monitor"
    }

    fn ui(&mut self, ui: &mut egui::Ui, theme: &Theme) {
        if self.last.is_none_or(|last| last.elapsed() >= self.interval) {
            self.refresh();
        }
        ui.ctx().request_repaint_after(self.interval);
        egui::Panel::top("monitor-summary").show(ui, |ui| {
            ui.add_space(4.0);
            self.summary(ui, theme);
            ui.add_space(4.0);
        });
        egui::CentralPanel::default_margins().show(ui, |ui| self.table(ui));
    }
}
