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
use mcsapi_components::{Button, ButtonVariant, Tokens};
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

/// System Monitor's pages.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum View {
    /// The process table.
    #[default]
    Processes,
    /// CPU history, memory, swap and load.
    Resources,
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
    /// The open page.
    pub view: View,
    filter: String,
    selected: Option<u32>,
    confirm: Option<u32>,
    status: Option<Result<String, String>>,
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
            view: View::Processes,
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

    fn sidebar(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        derisk_frame::navigation(ui, tokens, "monitor-pages", |nav| {
            nav.section("System Monitor");
            for (page, label) in [
                (View::Processes, "Processes"),
                (View::Resources, "Resources"),
            ] {
                if nav.entry(label, self.view == page).clicked() {
                    self.view = page;
                }
            }
        });
    }

    fn action_bar(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        let selected = self
            .selected
            .and_then(|pid| self.snapshot.processes.iter().find(|p| p.pid == pid));
        let mut end = false;
        let mut dismiss = false;
        derisk_frame::action_bar(ui, tokens, "monitor-actions", |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Ending is the process list's, so Resources leaves it out.
                if self.view == View::Processes {
                    end = ui
                        .add(
                            Button::new("End process")
                                .variant(ButtonVariant::Destructive)
                                .enabled(selected.is_some() && self.confirm.is_none()),
                        )
                        .on_disabled_hover_text("Pick a process in the list first")
                        .clicked();
                }
                if self.status.is_some() {
                    dismiss = ui
                        .add(Button::new("Dismiss").variant(ButtonVariant::Ghost))
                        .clicked();
                }
                ui.with_layout(
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| match (&self.status, selected) {
                        (Some(Ok(done)), _) => {
                            ui.add(egui::Label::new(done.as_str()).truncate());
                        }
                        (Some(Err(error)), _) => {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(error.as_str()).color(tokens.destructive),
                                )
                                .truncate(),
                            );
                        }
                        (None, Some(p)) => {
                            ui.add(
                                egui::Label::new(format!(
                                    "{} · {:.1}% CPU · {}",
                                    p.name,
                                    p.cpu,
                                    format_kib(p.rss)
                                ))
                                .truncate(),
                            );
                        }
                        (None, None) => {
                            let m = &self.snapshot.memory;
                            ui.add(
                                egui::Label::new(derisk_frame::note(
                                    tokens,
                                    &format!(
                                        "CPU {:.0}% · Memory {} of {}",
                                        self.snapshot.cpu,
                                        format_kib(m.used()),
                                        format_kib(m.total)
                                    ),
                                ))
                                .truncate(),
                            );
                        }
                    },
                );
            });
        });
        if end {
            self.confirm = self.selected;
        }
        if dismiss {
            self.status = None;
        }
    }

    fn resources(&self, ui: &mut egui::Ui, tokens: &Tokens) {
        let s = &self.snapshot;
        derisk_frame::header(
            ui,
            tokens,
            "Resources",
            &format!(
                "Up {} · {} processes",
                format_duration(s.uptime),
                s.processes.len()
            ),
        );
        derisk_frame::subheading(ui, tokens, "Processor");
        derisk_frame::card(ui, tokens, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("{:.0}%", s.cpu))
                        .font(egui::FontId::proportional(22.0))
                        .strong(),
                );
                ui.label(derisk_frame::note(
                    tokens,
                    &format!(
                        "busy across {} {}, over the last minute",
                        s.cpus,
                        if s.cpus == 1 { "core" } else { "cores" }
                    ),
                ));
            });
            ui.add_space(8.0);
            graph(ui, &self.sampler.history, tokens);
        });
        derisk_frame::subheading(ui, tokens, "Memory");
        derisk_frame::card(ui, tokens, |ui| {
            let m = &s.memory;
            meter(ui, tokens, "Memory", m.used(), m.total);
            ui.add_space(14.0);
            meter(ui, tokens, "Swap", m.swap_used(), m.swap_total);
        });
        derisk_frame::subheading(ui, tokens, "Load");
        derisk_frame::card(ui, tokens, |ui| {
            derisk_frame::rows(ui, "monitor-load", |ui| {
                for (label, load) in ["Last minute", "Last 5 minutes", "Last 15 minutes"]
                    .into_iter()
                    .zip(s.load)
                {
                    ui.label(label);
                    ui.label(format!("{load:.2}"));
                    ui.end_row();
                }
            });
            ui.add_space(6.0);
            ui.label(derisk_frame::note(
                tokens,
                "How many processes, on average, were running or waiting for a core.",
            ));
        });
    }

    fn processes(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        let count = self.rows().len();
        let phone = ui.available_width() < derisk_frame::PHONE;
        derisk_frame::header_with(
            ui,
            tokens,
            "Processes",
            &format!(
                "{count} of {} · CPU {:.0}%",
                self.snapshot.processes.len(),
                self.snapshot.cpu
            ),
            |ui| {
                let width = if phone { ui.available_width() } else { 220.0 };
                ui.add(
                    derisk_frame::field(&mut self.filter)
                        .hint_text("Name or PID")
                        .desired_width(width),
                );
            },
        );
        self.confirmation(ui, tokens);
        derisk_frame::Card::new()
            .tight()
            .fill()
            .show(ui, tokens, |ui| self.table(ui, tokens));
    }

    fn confirmation(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        let Some(pid) = self.confirm else {
            return;
        };
        let name = self
            .snapshot
            .processes
            .iter()
            .find(|p| p.pid == pid)
            .map_or_else(|| format!("process {pid}"), |p| p.name.clone());
        let mut answer = None;
        derisk_frame::Card::new()
            .stripe(tokens.destructive)
            .show(ui, tokens, |ui| {
                ui.strong(format!("End {name}?"));
                ui.label(derisk_frame::note(
                    tokens,
                    "It is asked to close, so it can save its work first. Anything unsaved \
                     that it does not save is lost.",
                ));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(Button::new("End process").variant(ButtonVariant::Destructive))
                        .clicked()
                    {
                        answer = Some(true);
                    }
                    if ui
                        .add(Button::new("Cancel").variant(ButtonVariant::Outline))
                        .clicked()
                    {
                        answer = Some(false);
                    }
                });
            });
        ui.add_space(12.0);
        match answer {
            Some(true) => {
                self.status = Some(match terminate(pid) {
                    Ok(()) => Ok(format!("Asked {name} to end")),
                    Err(error) => Err(format!("Could not end {name}: {error}")),
                });
                self.confirm = None;
            }
            Some(false) => self.confirm = None,
            None => {}
        }
    }

    fn table(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        let mut select = None;
        let mut sort = None;
        let rows = self.rows();
        if rows.is_empty() {
            derisk_frame::empty(ui, tokens, "No processes match", "Try another name or PID.");
            return;
        }
        let row_height = ui.spacing().interact_size.y + 4.0;
        let width = ui.available_width();
        let (header, _) =
            ui.allocate_exact_size(egui::vec2(width, row_height), egui::Sense::hover());
        // Flat headers under one thin rule, as Files' are: a click on one
        // that has an order sorts by it. Labels rather than buttons, so a
        // narrow column cuts its header short instead of widening the card.
        let text = ui.visuals().text_color();
        for (i, (((name, _, _, _), key), cell)) in COLUMNS
            .iter()
            .zip(SORTS)
            .zip(column_rects(header))
            .enumerate()
        {
            if cell.width() <= 0.0 {
                continue;
            }
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(cell)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
                |ui| {
                    if key.is_some_and(|key| key == self.sort) {
                        // Numbers sort largest first, PIDs and names
                        // from the start.
                        let icon = if matches!(self.sort, SortKey::Pid | SortKey::Name) {
                            "pan-up"
                        } else {
                            "pan-down"
                        };
                        derisk_icons::show(ui, icon, 12.0, text);
                    }
                    ui.add(
                        egui::Label::new(egui::RichText::new(*name).color(text))
                            .truncate()
                            .selectable(false),
                    );
                },
            );
            if let Some(key) = key {
                let response = ui
                    .interact(
                        cell,
                        ui.id().with(("monitor-sort", i)),
                        egui::Sense::click(),
                    )
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                if response.clicked() {
                    sort = Some(key);
                }
            }
        }
        ui.painter()
            .hline(header.x_range(), header.bottom(), tokens.border_stroke());
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for p in &rows {
                    let selected = self.selected == Some(p.pid);
                    // One target per row: clicking a name or a number selects
                    // the process, not only its PID.
                    let (rect, row) = derisk_frame::list_row(ui, tokens, row_height, selected);
                    row.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::WidgetType::SelectableLabel,
                            true,
                            selected,
                            format!("{} {}", p.pid, p.name),
                        )
                    });
                    cells(
                        ui,
                        rect,
                        &[
                            p.pid.to_string(),
                            p.name.clone(),
                            state(p.state).to_owned(),
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
        if let Some(sort) = sort {
            self.sort = sort;
        }
    }
}

/// What each of [`COLUMNS`] sorts by, if anything.
const SORTS: [Option<SortKey>; 6] = [
    Some(SortKey::Pid),
    Some(SortKey::Name),
    None,
    Some(SortKey::Cpu),
    Some(SortKey::Memory),
    None,
];

/// A process's one-letter state in words, as `ps` documents the letters.
fn state(letter: char) -> &'static str {
    match letter {
        'R' => "Running",
        'S' => "Sleeping",
        'D' => "Waiting",
        'Z' => "Zombie",
        'T' | 't' => "Stopped",
        'I' => "Idle",
        'X' => "Dead",
        _ => "Unknown",
    }
}

/// Process table columns: a header, its share of the row, the least it
/// may have, so numbers stay whole, and whether a phone keeps it. A share
/// of 0 takes what the others leave, so the table spans the window.
const COLUMNS: [(&str, f32, f32, bool); 6] = [
    ("PID", 0.09, 56.0, true),
    ("Name", 0.0, 0.0, true),
    ("State", 0.12, 72.0, false),
    ("CPU", 0.1, 64.0, true),
    ("Memory", 0.13, 76.0, true),
    ("Threads", 0.1, 64.0, false),
];

/// Below this a row drops the columns a phone does without, so the name
/// keeps room to be read.
const ROW_NARROW: f32 = 480.0;

/// Where each of [`COLUMNS`] sits across `row`, inset from its neighbours.
/// A column a narrow row drops is empty.
fn column_rects(row: egui::Rect) -> impl Iterator<Item = egui::Rect> {
    let narrow = row.width() < ROW_NARROW;
    let width = move |share: f32, least: f32, phone: bool| {
        if narrow && !phone {
            0.0
        } else {
            (share * row.width()).max(least)
        }
    };
    let fixed: f32 = COLUMNS
        .iter()
        .filter(|(_, share, _, _)| *share > 0.0)
        .map(|(_, share, least, phone)| width(*share, *least, *phone))
        .sum();
    let pad = 8.0;
    let mut x = row.left();
    COLUMNS.iter().map(move |(_, share, least, phone)| {
        let w = if *share == 0.0 {
            (row.width() - fixed).max(0.0)
        } else {
            width(*share, *least, *phone)
        };
        let cell = egui::Rect::from_min_size(
            egui::pos2(x + pad, row.top()),
            egui::vec2((w - 2.0 * pad).max(0.0), row.height()),
        );
        x += w;
        cell
    })
}

/// Lays `texts` out across `row` in [`COLUMNS`].
fn cells(ui: &mut egui::Ui, row: egui::Rect, texts: &[egui::RichText; 6]) {
    for (cell, text) in column_rects(row).zip(texts) {
        if cell.width() <= 0.0 {
            continue;
        }
        // `put` centers what it adds; cells read from the left.
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(cell)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| ui.add(egui::Label::new(text.clone()).truncate().selectable(false)),
        );
    }
}

/// How much of `total` is `used`: its label and figures over a bar in the
/// accent, on the same track Store's ratings draw.
fn meter(ui: &mut egui::Ui, tokens: &Tokens, label: &str, used: u64, total: u64) {
    let fraction = if total == 0 {
        0.0
    } else {
        (used as f32 / total as f32).clamp(0.0, 1.0)
    };
    ui.horizontal(|ui| {
        ui.strong(label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(derisk_frame::note(
                tokens,
                &if total == 0 {
                    "None".to_owned()
                } else {
                    format!("{} of {}", format_kib(used), format_kib(total))
                },
            ));
        });
    });
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 8.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, tokens.field);
    let mut filled = rect;
    filled.set_width(rect.width() * fraction);
    painter.rect_filled(filled, 4.0, tokens.primary);
}

/// The last [`HISTORY`] CPU samples as a line in the accent, over a faint
/// fill, with a rule at half.
fn graph(ui: &mut egui::Ui, history: &VecDeque<f32>, tokens: &Tokens) {
    let size = egui::vec2(ui.available_width(), 120.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, tokens.control_radius(), tokens.background);
    painter.hline(
        rect.x_range(),
        rect.center().y,
        egui::Stroke::new(1.0, tokens.border),
    );
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
        egui::Stroke::new(2.0, tokens.primary),
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
        let tokens = derisk_frame::begin(ui, theme);
        self.sidebar(ui, &tokens);
        self.action_bar(ui, &tokens);
        match self.view {
            View::Processes => derisk_frame::Page::new()
                .wide()
                .fixed()
                .show(ui, &tokens, |ui| self.processes(ui, &tokens)),
            View::Resources => {
                derisk_frame::Page::new().show(ui, &tokens, |ui| self.resources(ui, &tokens))
            }
        }
    }
}
