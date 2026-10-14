//! Asking the person before a portal request goes through.
//!
//! The frontend leaves some consent to the backend: an interactive
//! screenshot and a wallpaper with a preview are both supposed to show the
//! person something before they happen. Doing them silently would let any
//! app take the screen or change the background by setting one flag, so
//! this backend asks first.
//!
//! It asks with derisk's own dialog, `derisk-gpui ask`: an mcsapi
//! `NativeDialog` on the desktop theme, run as a process because this
//! service has no windows, answering with its exit status. Where that
//! binary is not installed it falls back to the
//! `org.freedesktop.impl.portal.Access` dialog of the GTK backend, the same
//! dialog the frontend itself uses for the non-interactive cases.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Mutex,
};

use ashpd::zbus::{
    self,
    zvariant::{ObjectPath, OwnedValue, Value},
};

/// The backend whose access dialog is used: `$DERISK_PORTAL_ACCESS`, else
/// the GTK backend's bus name.
pub fn backend() -> String {
    std::env::var("DERISK_PORTAL_ACCESS")
        .unwrap_or_else(|_| "org.freedesktop.impl.portal.desktop.gtk".to_owned())
}

/// The request object path the dialog is shown under, unique per portal
/// request so `close` can find it.
pub fn handle(token: &str) -> String {
    format!("/org/freedesktop/portal/desktop/request/derisk/{token}")
}

/// What to ask.
pub struct Question<'a> {
    /// The asking app's ID, empty for a host app.
    pub app_id: &'a str,
    /// The asking app's name, the dialog window's title.
    pub app: &'a str,
    /// The app's window, `wayland:<handle>` or empty.
    pub parent_window: &'a str,
    /// The dialog's heading.
    pub title: &'a str,
    /// The line under it.
    pub subtitle: &'a str,
    /// Smaller print.
    pub body: &'a str,
    /// The label on the button that says yes.
    pub grant_label: &'a str,
}

/// derisk's dialog program: `$DERISK_PORTAL_DIALOG`, else `derisk-gpui`
/// beside this binary or on `PATH`. `None` when there is none, and then the
/// GTK backend asks.
pub fn dialog_program() -> Option<PathBuf> {
    if let Some(program) = std::env::var_os("DERISK_PORTAL_DIALOG") {
        return Some(PathBuf::from(program));
    }
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("derisk-gpui")));
    let on_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|dir| dir.join("derisk-gpui"));
    beside.into_iter().chain(on_path).find(|p| is_executable(p))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The `derisk-gpui ask` arguments for `q`.
pub fn dialog_args<'a>(
    q: &Question<'a>,
    deny_label: &'a str,
) -> impl Iterator<Item = String> + use<'a> {
    let flags = [
        ("--title", q.title),
        ("--subtitle", q.subtitle),
        ("--body", q.body),
        ("--grant", q.grant_label),
        ("--deny", deny_label),
        ("--app", q.app),
        ("--parent", q.parent_window),
    ];
    std::iter::once("ask".to_owned()).chain(
        flags
            .into_iter()
            .filter(|(_, value)| !value.is_empty())
            .flat_map(|(flag, value)| [flag.to_owned(), value.to_owned()]),
    )
}

/// Dialogs `derisk-gpui ask` is showing, by request token, so `close` can
/// end one.
static SHOWING: Mutex<Option<HashMap<String, tokio::sync::oneshot::Sender<()>>>> = Mutex::new(None);

/// Shows the dialog and waits: `Ok(true)` only when the person grants it.
pub async fn ask(
    connection: &zbus::Connection,
    token: &str,
    q: &Question<'_>,
) -> zbus::Result<bool> {
    match dialog_program() {
        Some(program) => ask_derisk(&program, token, q)
            .await
            .map_err(|e| zbus::Error::Failure(format!("{}: {e}", program.display()))),
        None => ask_gtk(connection, token, q).await,
    }
}

async fn ask_derisk(program: &Path, token: &str, q: &Question<'_>) -> std::io::Result<bool> {
    let mut command = tokio::process::Command::new(program);
    command.args(dialog_args(q, "Cancel"));
    Ok(run_closable(token, command)
        .await?
        .is_some_and(|(granted, _)| granted))
}

/// Runs one of derisk's own windows for the request `token` until it exits,
/// returning whether it succeeded and what it printed, or `None` when the
/// app gave up on its request first and [`close`] took the window down.
pub async fn run_closable(
    token: &str,
    mut command: tokio::process::Command,
) -> std::io::Result<Option<(bool, String)>> {
    use tokio::io::AsyncReadExt;

    let mut child = command
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdout = child.stdout.take();
    let (closed, close) = tokio::sync::oneshot::channel();
    SHOWING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_default()
        .insert(token.to_owned(), closed);
    let finished = tokio::select! {
        done = async {
            let mut text = String::new();
            if let Some(out) = stdout.as_mut() {
                out.read_to_string(&mut text).await?;
            }
            Ok::<_, std::io::Error>((child.wait().await?.success(), text))
        } => Some(done),
        // The app gave up on its request: the window goes down unanswered,
        // killed when `child` is dropped.
        _ = close => None,
    };
    if let Some(showing) = SHOWING.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        showing.remove(token);
    }
    finished.transpose()
}

async fn ask_gtk(
    connection: &zbus::Connection,
    token: &str,
    q: &Question<'_>,
) -> zbus::Result<bool> {
    let handle = handle(token);
    let handle = ObjectPath::try_from(handle.as_str())?;
    let mut options: HashMap<&str, Value<'_>> = HashMap::new();
    options.insert("modal", Value::from(true));
    options.insert("grant_label", Value::from(q.grant_label));
    let reply = connection
        .call_method(
            Some(backend().as_str()),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.impl.portal.Access"),
            "AccessDialog",
            &(
                handle,
                q.app_id,
                q.parent_window,
                q.title,
                q.subtitle,
                q.body,
                options,
            ),
        )
        .await?;
    let (response, _results): (u32, HashMap<String, OwnedValue>) = reply.body().deserialize()?;
    Ok(response == 0)
}

/// Closes a dialog [`ask`] is still showing for `token`, when the app
/// gives up on its request.
pub async fn close(connection: &zbus::Connection, token: &str) {
    let derisk = SHOWING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .and_then(|showing| showing.remove(token));
    if let Some(close) = derisk {
        let _ = close.send(());
        return;
    }
    let _ = connection
        .call_method(
            Some(backend().as_str()),
            handle(token).as_str(),
            Some("org.freedesktop.impl.portal.Request"),
            "Close",
            &(),
        )
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_args_leave_out_empty_values() {
        let q = Question {
            app_id: "org.example.App",
            app: "Example",
            parent_window: "",
            title: "Take a screenshot?",
            subtitle: "Example wants a picture of the whole screen.",
            body: "",
            grant_label: "Take Screenshot",
        };
        assert_eq!(
            dialog_args(&q, "Cancel").collect::<Vec<_>>(),
            [
                "ask",
                "--title",
                "Take a screenshot?",
                "--subtitle",
                "Example wants a picture of the whole screen.",
                "--grant",
                "Take Screenshot",
                "--deny",
                "Cancel",
                "--app",
                "Example",
            ]
        );
    }
}
