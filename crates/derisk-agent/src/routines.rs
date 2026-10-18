//! Routines: prompts that run on a schedule as systemd user timers.
//!
//! LosOS runs everything it can through systemd, so a routine is a
//! `derisk-agent-routine-<id>.timer` and the service it starts, `derisk-agent routine run
//! <id>`, which opens a new chat in the routine's project and runs the prompt
//! with no window. `systemctl --user list-timers` shows them like any other.

// systemctl runs on the caller's thread; the window calls these from a
// background thread.
#![allow(clippy::disallowed_methods)]

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use chrono::Utc;
use miette::{Context as _, IntoDiagnostic as _, Result, bail};

use crate::{
    agent,
    store::{Routine, Store},
};

fn unit_dir() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .context("no config directory")?
        .join("systemd")
        .join("user"))
}

pub fn unit_name(routine: &Routine) -> String {
    format!("derisk-agent-routine-{}", routine.id)
}

pub fn service_unit(routine: &Routine, executable: &Path, store_root: &Path) -> String {
    format!(
        "[Unit]\n\
         Description=Agent routine: {name}\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         Environment=DERISK_AGENT_DATA_DIR={store}\n\
         ExecStart={exe} routine run {id}\n",
        name = routine.name.replace('\n', " "),
        store = store_root.display(),
        exe = executable.display(),
        id = routine.id,
    )
}

pub fn timer_unit(routine: &Routine) -> String {
    format!(
        "[Unit]\n\
         Description=Agent routine timer: {name}\n\
         \n\
         [Timer]\n\
         OnCalendar={schedule}\n\
         Persistent=true\n\
         \n\
         [Install]\n\
         WantedBy=timers.target\n",
        name = routine.name.replace('\n', " "),
        schedule = routine.schedule.trim(),
    )
}

fn systemctl(args: &[&str]) -> Result<()> {
    let output = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .into_diagnostic()
        .context("running systemctl")?;
    if !output.status.success() {
        bail!(
            "systemctl --user {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Checks a calendar expression the way the timer will read it.
pub fn validate_schedule(schedule: &str) -> Result<()> {
    let output = Command::new("systemd-analyze")
        .args(["calendar", schedule.trim()])
        .output()
        .into_diagnostic()
        .context("running systemd-analyze")?;
    if !output.status.success() {
        bail!(
            "`{schedule}` is not a systemd calendar expression: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Writes the routine's units and starts or stops its timer to match
/// `routine.enabled`.
pub fn install(routine: &Routine, store: &Store) -> Result<()> {
    let name = unit_name(routine);
    if !routine.enabled {
        // An uninstalled timer is the normal state of a disabled routine.
        systemctl(&["disable", "--now", &format!("{name}.timer")]).ok();
        return Ok(());
    }
    validate_schedule(&routine.schedule)?;
    let dir = unit_dir()?;
    std::fs::create_dir_all(&dir).into_diagnostic()?;
    // The system profile's link, not the store path a later update removes.
    let executable = crate::stable_executable().into_diagnostic()?;
    std::fs::write(
        dir.join(format!("{name}.service")),
        service_unit(routine, &executable, store.root()),
    )
    .into_diagnostic()?;
    std::fs::write(dir.join(format!("{name}.timer")), timer_unit(routine)).into_diagnostic()?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", "--now", &format!("{name}.timer")])
}

pub fn uninstall(routine: &Routine) -> Result<()> {
    let name = unit_name(routine);
    systemctl(&["disable", "--now", &format!("{name}.timer")]).ok();
    let dir = unit_dir()?;
    for suffix in ["service", "timer"] {
        let path = dir.join(format!("{name}.{suffix}"));
        if path.exists() {
            std::fs::remove_file(&path).into_diagnostic()?;
        }
    }
    systemctl(&["daemon-reload"])
}

/// Runs a routine now: a new chat in its project, with the prompt as the first
/// message. Returns the chat's ID.
pub fn run(store: &Store, routine_id: &str) -> Result<String> {
    let routine = store.routine(routine_id)?;
    let project = store.project(&routine.project_id)?;
    let title = format!("{} · {}", routine.name, Utc::now().format("%Y-%m-%d %H:%M"));
    let mut chat = store.new_chat(&project.id, &title)?;
    chat.routine_id = Some(routine.id.clone());
    store.update_routines(|routines| {
        if let Some(routine) = routines.iter_mut().find(|r| r.id == routine_id) {
            routine.last_run = Some(Utc::now());
        }
    })?;
    agent::run_to_end(store, &project, &mut chat, &routine.prompt)?;
    Ok(chat.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_name_the_routine_and_its_schedule() {
        let routine = Routine {
            id: "r1".into(),
            project_id: "p".into(),
            name: "Morning\nreview".into(),
            prompt: "Review open PRs".into(),
            schedule: "Mon..Fri 09:00".into(),
            enabled: true,
            last_run: None,
        };
        let service = service_unit(
            &routine,
            Path::new("/usr/bin/derisk-agent"),
            Path::new("/data"),
        );
        assert!(service.contains("ExecStart=/usr/bin/derisk-agent routine run r1"));
        assert!(service.contains("Environment=DERISK_AGENT_DATA_DIR=/data"));
        assert!(service.contains("Description=Agent routine: Morning review"));
        let timer = timer_unit(&routine);
        assert!(timer.contains("OnCalendar=Mon..Fri 09:00"));
        assert!(timer.contains("Persistent=true"));
        assert_eq!(unit_name(&routine), "derisk-agent-routine-r1");
    }
}
