//! Projects, chats, routines and artifacts, as JSON files under
//! `$DERISK_AGENT_DATA_DIR` (default `$XDG_DATA_HOME/derisk-agent`).
//!
//! The window, `derisk-agent mcp` and `derisk-agent routine run` are separate processes that
//! all write here, so every change re-reads the file it edits and replaces it
//! by rename: a reader never sees half a file, and a writer never works from a
//! copy that another process has since changed.

use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    time::SystemTime,
};

use chrono::{DateTime, Utc};
use derisk_agent_preview::PreviewSource;
use miette::{Context as _, IntoDiagnostic as _, Result};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::setup::AppSetup;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Project {
    pub id: String,
    pub name: String,
    /// Added to every chat's system prompt, like a Claude project's instructions.
    #[serde(default)]
    pub instructions: String,
    /// The repositories and folders the project's agents work in. The first is
    /// the working directory.
    #[serde(default)]
    pub folders: Vec<PathBuf>,
    #[serde(default)]
    pub permission_mode: PermissionMode,
    #[serde(default)]
    pub mcp_servers: Vec<McpServer>,
    /// The answers from the project setup wizard, when the project was set up
    /// with it.
    #[serde(default)]
    pub app: Option<AppSetup>,
    pub created: DateTime<Utc>,
}

/// Claude Code's `--permission-mode`.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    Default,
    #[default]
    AcceptEdits,
    Plan,
    BypassPermissions,
}

impl PermissionMode {
    pub const ALL: [Self; 4] = [
        Self::Default,
        Self::AcceptEdits,
        Self::Plan,
        Self::BypassPermissions,
    ];

    pub fn flag(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::AcceptEdits => "acceptEdits",
            Self::Plan => "plan",
            Self::BypassPermissions => "bypassPermissions",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "Ask for every tool",
            Self::AcceptEdits => "Accept file edits",
            Self::Plan => "Plan only",
            Self::BypassPermissions => "Run everything",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct McpServer {
    pub name: String,
    #[serde(flatten)]
    pub transport: McpTransport,
    #[serde(default = "enabled")]
    pub enabled: bool,
}

fn enabled() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransport {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
    Http {
        url: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Chat {
    pub id: String,
    pub project_id: String,
    pub title: String,
    /// Claude Code's session, resumed by every later turn.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Set when a routine started the chat.
    #[serde(default)]
    pub routine_id: Option<String>,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    #[serde(default)]
    pub entries: Vec<Entry>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub role: Role,
    pub text: String,
    pub at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    /// A tool the agent called, with its input.
    Tool,
    /// What the tool returned.
    ToolResult,
    /// The turn's end: cost and duration.
    Status,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Routine {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub prompt: String,
    /// A systemd calendar expression (`systemd.time(7)`), such as `daily` or
    /// `Mon..Fri 09:00`.
    pub schedule: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub last_run: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Artifact {
    pub id: String,
    pub project_id: String,
    #[serde(default)]
    pub chat_id: Option<String>,
    pub title: String,
    pub kind: ArtifactKind,
    pub created: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ArtifactKind {
    /// A document, image or any other file the agent produced.
    File { path: PathBuf },
    /// An app that opens in the preview pane.
    App { source: PreviewSource },
}

#[derive(Clone, Debug)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn open_default() -> Result<Self> {
        let root = match std::env::var_os("DERISK_AGENT_DATA_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => dirs::data_dir()
                .context("no data directory: set XDG_DATA_HOME or DERISK_AGENT_DATA_DIR")?
                .join("derisk-agent"),
        };
        Self::open(root)
    }

    pub fn open(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(root.join("chats"))
            .into_diagnostic()
            .with_context(|| format!("creating {}", root.display()))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn projects(&self) -> Result<Vec<Project>> {
        read_or_default(&self.root.join("projects.json"))
    }

    pub fn routines(&self) -> Result<Vec<Routine>> {
        read_or_default(&self.root.join("routines.json"))
    }

    pub fn artifacts(&self) -> Result<Vec<Artifact>> {
        read_or_default(&self.root.join("artifacts.json"))
    }

    pub fn project(&self, id: &str) -> Result<Project> {
        self.projects()?
            .into_iter()
            .find(|project| project.id == id)
            .with_context(|| format!("no project {id}"))
    }

    pub fn routine(&self, id: &str) -> Result<Routine> {
        self.routines()?
            .into_iter()
            .find(|routine| routine.id == id)
            .with_context(|| format!("no routine {id}"))
    }

    pub fn update_projects<R>(&self, edit: impl FnOnce(&mut Vec<Project>) -> R) -> Result<R> {
        update(&self.root.join("projects.json"), edit)
    }

    pub fn update_routines<R>(&self, edit: impl FnOnce(&mut Vec<Routine>) -> R) -> Result<R> {
        update(&self.root.join("routines.json"), edit)
    }

    pub fn update_artifacts<R>(&self, edit: impl FnOnce(&mut Vec<Artifact>) -> R) -> Result<R> {
        update(&self.root.join("artifacts.json"), edit)
    }

    /// Every chat's header, newest first, without loading every transcript into
    /// the caller's hands twice.
    pub fn chats(&self) -> Result<Vec<Chat>> {
        let mut chats = Vec::new();
        for entry in fs::read_dir(self.root.join("chats")).into_diagnostic()? {
            let path = entry.into_diagnostic()?.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                match read::<Chat>(&path) {
                    Ok(chat) => chats.push(chat),
                    Err(error) => {
                        tracing::warn!("skipping {}: {}", path.display(), crate::describe(&error))
                    }
                }
            }
        }
        chats.sort_by_key(|chat| std::cmp::Reverse(chat.updated));
        Ok(chats)
    }

    pub fn chat(&self, id: &str) -> Result<Chat> {
        read(&self.chat_path(id))
    }

    pub fn save_chat(&self, chat: &Chat) -> Result<()> {
        write(&self.chat_path(&chat.id), chat)
    }

    pub fn delete_chat(&self, id: &str) -> Result<()> {
        fs::remove_file(self.chat_path(id))
            .into_diagnostic()
            .with_context(|| format!("deleting chat {id}"))
    }

    fn chat_path(&self, id: &str) -> PathBuf {
        self.root.join("chats").join(format!("{id}.json"))
    }

    /// The newest modification time of anything in the store, for a window that
    /// reloads when another process wrote.
    pub fn modified(&self) -> Option<SystemTime> {
        let files = ["projects.json", "routines.json", "artifacts.json"]
            .iter()
            .map(|name| self.root.join(name));
        let chats = fs::read_dir(self.root.join("chats"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path());
        files
            .chain(chats)
            .filter_map(|path| fs::metadata(path).and_then(|meta| meta.modified()).ok())
            .chain(fs::metadata(self.root.join("chats")).and_then(|meta| meta.modified()))
            .max()
    }

    pub fn new_project(&self, name: &str, folders: Vec<PathBuf>) -> Result<Project> {
        let project = Project {
            id: new_id(),
            name: name.to_owned(),
            instructions: String::new(),
            folders,
            permission_mode: PermissionMode::default(),
            mcp_servers: Vec::new(),
            app: None,
            created: Utc::now(),
        };
        self.update_projects(|projects| projects.push(project.clone()))?;
        Ok(project)
    }

    pub fn new_chat(&self, project_id: &str, title: &str) -> Result<Chat> {
        let now = Utc::now();
        let chat = Chat {
            id: new_id(),
            project_id: project_id.to_owned(),
            title: title.to_owned(),
            session_id: None,
            routine_id: None,
            created: now,
            updated: now,
            entries: Vec::new(),
        };
        self.save_chat(&chat)?;
        Ok(chat)
    }

    pub fn add_artifact(
        &self,
        project_id: &str,
        chat_id: Option<&str>,
        title: &str,
        kind: ArtifactKind,
    ) -> Result<Artifact> {
        let artifact = Artifact {
            id: new_id(),
            project_id: project_id.to_owned(),
            chat_id: chat_id.map(str::to_owned),
            title: title.to_owned(),
            kind,
            created: Utc::now(),
        };
        self.update_artifacts(|artifacts| {
            // Rebuilding the same app replaces its artifact instead of
            // stacking a copy per build.
            artifacts.retain(|existing| {
                existing.project_id != artifact.project_id || existing.kind != artifact.kind
            });
            artifacts.push(artifact.clone());
        })?;
        Ok(artifact)
    }
}

pub fn new_id() -> String {
    uuid::Uuid::now_v7().simple().to_string()
}

fn read<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = fs::read(path)
        .into_diagnostic()
        .with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .into_diagnostic()
        .with_context(|| format!("parsing {}", path.display()))
}

fn read_or_default<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .into_diagnostic()
            .with_context(|| format!("parsing {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(error)
            .into_diagnostic()
            .with_context(|| format!("reading {}", path.display())),
    }
}

fn write<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let mut file = fs::File::create(&temporary)
        .into_diagnostic()
        .with_context(|| format!("creating {}", temporary.display()))?;
    file.write_all(&serde_json::to_vec_pretty(value).into_diagnostic()?)
        .into_diagnostic()?;
    file.sync_all().into_diagnostic()?;
    fs::rename(&temporary, path)
        .into_diagnostic()
        .with_context(|| format!("replacing {}", path.display()))
}

fn update<T: Serialize + DeserializeOwned + Default, R>(
    path: &Path,
    edit: impl FnOnce(&mut T) -> R,
) -> Result<R> {
    let mut value = read_or_default::<T>(path)?;
    let result = edit(&mut value);
    write(path, &value)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_chats_and_artifacts_round_trip() -> Result<()> {
        let dir = tempfile::tempdir().into_diagnostic()?;
        let store = Store::open(dir.path().to_path_buf())?;
        let project = store.new_project("Agent", vec![PathBuf::from("/src/agent")])?;
        assert_eq!(store.projects()?, vec![project.clone()]);

        let mut chat = store.new_chat(&project.id, "Build a counter")?;
        chat.entries.push(Entry {
            role: Role::User,
            text: "hello".into(),
            at: Utc::now(),
        });
        store.save_chat(&chat)?;
        assert_eq!(store.chat(&chat.id)?, chat);
        assert_eq!(store.chats()?.len(), 1);

        let app = ArtifactKind::App {
            source: PreviewSource::Native {
                program: "/tmp/counter".into(),
                args: Vec::new(),
                cwd: None,
            },
        };
        store.add_artifact(&project.id, Some(&chat.id), "Counter", app.clone())?;
        store.add_artifact(&project.id, Some(&chat.id), "Counter v2", app)?;
        let artifacts = store.artifacts()?;
        assert_eq!(artifacts.len(), 1, "a rebuild replaces the app's artifact");
        assert_eq!(artifacts[0].title, "Counter v2");
        Ok(())
    }

    #[test]
    fn mcp_servers_keep_their_claude_code_shape() -> Result<()> {
        let server: McpServer = serde_json::from_str(
            r#"{"name":"mcsapi","type":"http","url":"http://127.0.0.1:8787/mcp"}"#,
        )
        .into_diagnostic()?;
        assert!(server.enabled);
        assert_eq!(
            server.transport,
            McpTransport::Http {
                url: "http://127.0.0.1:8787/mcp".into()
            }
        );
        Ok(())
    }
}
