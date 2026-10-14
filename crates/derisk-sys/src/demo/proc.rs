//! A `/proc` for the System Monitor to read: a four-core machine that has
//! been up a few hours, running a LosOS session's processes, with load
//! that rises and falls so the graph has something to draw.
//!
//! Counters are integrals of smooth curves of the seconds since the page
//! loaded, so they only ever grow, as the kernel's do, and every read
//! between two frames agrees with the last.

use super::{Node, Tree, insert};

/// Clock ticks per second, `USER_HZ` on Linux.
const HZ: f64 = 100.0;
const CPUS: usize = 4;
/// Seconds the machine was up when the page loaded.
const BOOTED: f64 = 3.0 * 3600.0 + 12.0 * 60.0;
const MEM_TOTAL: u64 = 16 * 1024 * 1024;
const SWAP_TOTAL: u64 = 8 * 1024 * 1024;

/// A process: PID, name (at most 15 bytes, as the kernel cuts it),
/// threads, resident KiB, its average share of one CPU, and the period in
/// seconds its use swings over.
struct Process {
    pid: u32,
    name: &'static str,
    threads: u32,
    rss: u64,
    share: f64,
    period: f64,
}

const fn process(
    pid: u32,
    name: &'static str,
    threads: u32,
    rss: u64,
    share: f64,
    period: f64,
) -> Process {
    Process {
        pid,
        name,
        threads,
        rss,
        share,
        period,
    }
}

const PROCESSES: [Process; 18] = [
    process(1, "systemd", 1, 14_208, 0.001, 30.0),
    // A kernel thread: no memory of its own.
    process(2, "kthreadd", 1, 0, 0.0, 1.0),
    process(318, "systemd-journal", 1, 22_400, 0.004, 9.0),
    process(402, "systemd-udevd", 1, 9_856, 0.001, 40.0),
    process(611, "systemd-network", 1, 8_960, 0.001, 25.0),
    process(615, "systemd-resolve", 1, 12_032, 0.002, 18.0),
    process(688, "dbus-broker", 1, 5_120, 0.003, 7.0),
    process(702, "systemd-logind", 1, 8_448, 0.001, 33.0),
    process(1024, "derisk", 14, 212_480, 0.09, 6.0),
    process(1031, "derisk-portal", 4, 18_944, 0.002, 21.0),
    process(1036, "derisk-agent", 3, 31_232, 0.012, 11.0),
    process(1040, "pipewire", 3, 15_360, 0.015, 5.0),
    process(1042, "wireplumber", 4, 21_504, 0.004, 13.0),
    process(1108, "danube", 22, 486_400, 0.16, 8.0),
    process(1131, "WebKitWebProce", 18, 392_192, 0.11, 4.5),
    process(1132, "WebKitNetworkP", 6, 61_440, 0.01, 10.0),
    process(1290, "pm", 2, 24_576, 0.005, 15.0),
    process(1414, "sonne", 31, 318_464, 0.06, 9.5),
];

/// Ticks a process with average share `share` of one CPU, swinging by
/// `swing` of that over `period` seconds, has used by `t` seconds after
/// load: the integral of `share * (1 + swing * sin(t / period))`.
fn ticks(share: f64, swing: f64, period: f64, t: f64) -> u64 {
    let t = BOOTED + t;
    let seconds = share * (t + swing * period * (1.0 - (t / period).cos()));
    (seconds * HZ) as u64
}

/// Busy ticks of the whole machine: what the processes use, plus the
/// kernel's own work.
fn busy(t: f64) -> u64 {
    let processes: u64 = PROCESSES
        .iter()
        .map(|p| ticks(p.share, 0.8, p.period, t))
        .sum();
    processes + ticks(0.25, 0.9, 3.7, t)
}

fn stat(t: f64) -> String {
    let total = ((BOOTED + t) * HZ) as u64 * CPUS as u64;
    let busy = busy(t);
    // user nice system idle iowait irq softirq steal guest guest_nice
    let (user, system) = (busy * 7 / 10, busy * 3 / 10);
    let idle = total.saturating_sub(user + system);
    let mut out = format!("cpu  {user} 0 {system} {idle} 0 0 0 0 0 0\n");
    for cpu in 0..CPUS {
        let share = CPUS as u64;
        out += &format!(
            "cpu{cpu} {} 0 {} {} 0 0 0 0 0 0\n",
            user / share,
            system / share,
            idle / share
        );
    }
    out + "ctxt 48211907\nprocesses 18533\nprocs_running 2\nprocs_blocked 0\n"
}

fn meminfo(t: f64) -> String {
    let used = 5.6 + 0.6 * (t / 23.0).sin() + 0.2 * (t / 4.1).sin();
    let available = MEM_TOTAL - (used * 1024.0 * 1024.0) as u64;
    let swap_free = SWAP_TOTAL - 98_304;
    format!(
        "MemTotal:       {MEM_TOTAL} kB\n\
         MemFree:        {} kB\n\
         MemAvailable:   {available} kB\n\
         SwapTotal:      {SWAP_TOTAL} kB\n\
         SwapFree:       {swap_free} kB\n",
        available - 2_097_152,
    )
}

fn loadavg(t: f64) -> String {
    let wave = |period: f64, base: f64| base + 0.25 * (t / period).sin();
    format!(
        "{:.2} {:.2} {:.2} 2/{} 18533\n",
        wave(9.0, 0.62),
        wave(40.0, 0.48),
        wave(120.0, 0.41),
        PROCESSES.len() * 34,
    )
}

fn pid_stat(p: &Process, t: f64) -> String {
    let used = ticks(p.share, 0.8, p.period, t);
    let (utime, stime) = (used * 4 / 5, used / 5);
    let state = if p.share > 0.05 && (t / p.period).sin() > 0.3 {
        'R'
    } else if p.share == 0.0 {
        'I'
    } else {
        'S'
    };
    // Fields 3 to 20 of proc_pid_stat(5); the rest are left off.
    format!(
        "{pid} ({name}) {state} 1 {pid} {pid} 0 -1 4194560 1200 0 12 0 {utime} {stime} 0 0 20 0 {threads}\n",
        pid = p.pid,
        name = p.name,
        threads = p.threads,
    )
}

fn pid_status(p: &Process, t: f64) -> String {
    let mut out = format!("Name:\t{}\nThreads:\t{}\n", p.name, p.threads);
    if p.rss > 0 {
        let breathing = (p.rss as f64 * 0.03 * (t / (p.period * 3.0)).sin()) as i64;
        out += &format!("VmRSS:\t{} kB\n", p.rss.saturating_add_signed(breathing));
    }
    out
}

pub(crate) fn seed(tree: &mut Tree) {
    let text = |write: fn(f64) -> String| Node::generated(move |t| write(t).into_bytes());
    insert(tree, "/proc/stat", text(stat));
    insert(tree, "/proc/meminfo", text(meminfo));
    insert(tree, "/proc/loadavg", text(loadavg));
    insert(
        tree,
        "/proc/uptime",
        text(|t| format!("{:.2} {:.2}\n", BOOTED + t, (BOOTED + t) * 3.1)),
    );
    for (index, p) in PROCESSES.iter().enumerate() {
        let dir = format!("/proc/{}", p.pid);
        insert(
            tree,
            &format!("{dir}/stat"),
            Node::generated(move |t| pid_stat(&PROCESSES[index], t).into_bytes()),
        );
        insert(
            tree,
            &format!("{dir}/status"),
            Node::generated(move |t| pid_status(&PROCESSES[index], t).into_bytes()),
        );
    }
}
