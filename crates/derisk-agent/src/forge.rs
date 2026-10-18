//! Pull requests on GitHub, merge requests on GitLab, and pull requests on
//! Forgejo and Gitea, read from whichever forge a folder's `origin` points at.

// Runs `git` on the calling thread; callers are background threads.
#![allow(clippy::disallowed_methods)]

use std::{path::Path, process::Command, time::Duration};

use miette::{Context as _, IntoDiagnostic as _, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Forge {
    GitHub,
    GitLab,
    /// Forgejo and Gitea share one API.
    Forgejo,
}

impl Forge {
    /// What the forge calls a change proposal.
    pub fn noun(self) -> &'static str {
        match self {
            Self::GitHub | Self::Forgejo => "pull request",
            Self::GitLab => "merge request",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::GitHub => "GitHub",
            Self::GitLab => "GitLab",
            Self::Forgejo => "Forgejo",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForgeRepo {
    pub forge: Forge,
    pub host: String,
    /// `owner/name`, or a GitLab group path such as `group/subgroup/name`.
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub author: String,
    pub url: String,
    pub draft: bool,
    pub branch: String,
}

/// Which forge serves `url`, from the host name. Self-hosted instances are
/// recognised when their host says what they run (`gitlab.example.com`,
/// `forgejo.example.org`), or through `DERISK_AGENT_FORGE_<HOST>` set to `github`,
/// `gitlab` or `forgejo`, with the host's dots and dashes as underscores.
pub fn parse_remote(url: &str) -> Option<ForgeRepo> {
    let url = url.trim();
    let rest = if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .or_else(|| url.strip_prefix("ssh://"))
        .or_else(|| url.strip_prefix("git://"))
    {
        rest.to_owned()
    } else {
        // scp-like `git@host:owner/repo.git`.
        let (host, path) = url.split_once(':')?;
        format!("{host}/{path}")
    };
    let rest = rest
        .rsplit_once('@')
        .map_or(rest.as_str(), |(_, rest)| rest);
    let (host, path) = rest.split_once('/')?;
    let host = host.split(':').next()?.to_ascii_lowercase();
    let path = path.trim_end_matches('/').trim_end_matches(".git");
    if path.split('/').filter(|part| !part.is_empty()).count() < 2 {
        return None;
    }
    Some(ForgeRepo {
        forge: forge_for_host(&host)?,
        host,
        path: path.to_owned(),
    })
}

fn forge_for_host(host: &str) -> Option<Forge> {
    let variable = format!(
        "DERISK_AGENT_FORGE_{}",
        host.replace(['.', '-'], "_").to_ascii_uppercase()
    );
    if let Ok(forge) = std::env::var(variable) {
        return match forge.to_ascii_lowercase().as_str() {
            "github" => Some(Forge::GitHub),
            "gitlab" => Some(Forge::GitLab),
            "forgejo" | "gitea" => Some(Forge::Forgejo),
            _ => None,
        };
    }
    if host == "github.com" {
        Some(Forge::GitHub)
    } else if host == "gitlab.com" || host.contains("gitlab") {
        Some(Forge::GitLab)
    } else if host == "codeberg.org" || host.contains("forgejo") || host.contains("gitea") {
        Some(Forge::Forgejo)
    } else {
        None
    }
}

/// The forge repository behind `folder`'s `origin` remote.
pub fn repo_for_folder(folder: &Path) -> Result<ForgeRepo> {
    let output = Command::new("git")
        .arg("-C")
        .arg(folder)
        .args(["remote", "get-url", "origin"])
        .output()
        .into_diagnostic()
        .context("running git")?;
    if !output.status.success() {
        bail!(
            "{} has no origin remote: {}",
            folder.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    parse_remote(&url).with_context(|| format!("{url} is not on a forge the Agent knows"))
}

fn token(repo: &ForgeRepo) -> Option<String> {
    let from_env = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
    };
    match repo.forge {
        Forge::GitHub => from_env(&["GITHUB_TOKEN", "GH_TOKEN"]).or_else(|| {
            // The token gh already holds, so a signed-in user needs no setup.
            let output = Command::new("gh")
                .args(["auth", "token", "--hostname", &repo.host])
                .output()
                .ok()?;
            output
                .status
                .success()
                .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
                .filter(|token| !token.is_empty())
        }),
        Forge::GitLab => from_env(&["GITLAB_TOKEN"]),
        Forge::Forgejo => from_env(&["FORGEJO_TOKEN", "GITEA_TOKEN"]),
    }
}

/// An HTTP client that trusts the system's certificate store (and
/// `SSL_CERT_FILE`), so a forge behind a company or distribution CA works, and
/// that goes through `HTTPS_PROXY` when one is set.
pub fn http_agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .timeout_global(Some(timeout))
        .build()
        .into()
}

/// The open pull or merge requests of `repo`, newest first.
pub fn open_pull_requests(repo: &ForgeRepo) -> Result<Vec<PullRequest>> {
    let token = token(repo);
    let (url, auth) = match repo.forge {
        Forge::GitHub => {
            let api = if repo.host == "github.com" {
                "https://api.github.com".to_owned()
            } else {
                format!("https://{}/api/v3", repo.host)
            };
            (
                format!("{api}/repos/{}/pulls?state=open&per_page=50", repo.path),
                token.map(|token| ("Authorization", format!("Bearer {token}"))),
            )
        }
        Forge::GitLab => (
            format!(
                "https://{}/api/v4/projects/{}/merge_requests?state=opened&per_page=50",
                repo.host,
                repo.path.replace('/', "%2F")
            ),
            token.map(|token| ("PRIVATE-TOKEN", token)),
        ),
        Forge::Forgejo => (
            format!(
                "https://{}/api/v1/repos/{}/pulls?state=open&limit=50",
                repo.host, repo.path
            ),
            token.map(|token| ("Authorization", format!("token {token}"))),
        ),
    };
    let mut request = http_agent(Duration::from_secs(30))
        .get(&url)
        .header("User-Agent", "derisk-agent")
        .header("Accept", "application/json");
    if let Some((name, value)) = &auth {
        request = request.header(*name, value);
    }
    let mut response = request
        .call()
        .into_diagnostic()
        .with_context(|| format!("GET {url}"))?;
    let body: serde_json::Value = response.body_mut().read_json().into_diagnostic()?;
    parse_pull_requests(repo.forge, &body)
}

fn parse_pull_requests(forge: Forge, body: &serde_json::Value) -> Result<Vec<PullRequest>> {
    let items = body
        .as_array()
        .with_context(|| format!("the forge answered with {body}"))?;
    let text = |item: &serde_json::Value, path: &[&str]| {
        path.iter()
            .try_fold(item, |value, key| value.get(key))
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_owned()
    };
    Ok(items
        .iter()
        .map(|item| match forge {
            Forge::GitHub | Forge::Forgejo => PullRequest {
                number: item["number"].as_u64().unwrap_or_default(),
                title: text(item, &["title"]),
                author: text(item, &["user", "login"]),
                url: text(item, &["html_url"]),
                draft: item["draft"].as_bool().unwrap_or(false),
                branch: text(item, &["head", "ref"]),
            },
            Forge::GitLab => PullRequest {
                number: item["iid"].as_u64().unwrap_or_default(),
                title: text(item, &["title"]),
                author: text(item, &["author", "username"]),
                url: text(item, &["web_url"]),
                draft: item["draft"].as_bool().unwrap_or(false)
                    || item["work_in_progress"].as_bool().unwrap_or(false),
                branch: text(item, &["source_branch"]),
            },
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remotes_resolve_to_their_forge() {
        let github = parse_remote("git@github.com:dasmatus/sonne.git").expect("github");
        assert_eq!(github.forge, Forge::GitHub);
        assert_eq!(github.path, "dasmatus/sonne");

        let gitlab =
            parse_remote("https://gitlab.gnome.org/GNOME/gtk.git").expect("self-hosted gitlab");
        assert_eq!(gitlab.forge, Forge::GitLab);
        assert_eq!(gitlab.host, "gitlab.gnome.org");

        let codeberg = parse_remote("ssh://git@codeberg.org:22/forgejo/forgejo").expect("codeberg");
        assert_eq!(codeberg.forge, Forge::Forgejo);
        assert_eq!(codeberg.path, "forgejo/forgejo");

        let proxied =
            parse_remote("http://user:secret@github.com/dasmatus/derisk").expect("credentials");
        assert_eq!(proxied.path, "dasmatus/derisk");

        assert!(parse_remote("https://example.com/only-one").is_none());
        assert!(parse_remote("https://git.example.com/a/b").is_none());
    }

    #[test]
    fn each_forge_shape_parses() -> Result<()> {
        let github = serde_json::json!([{
            "number": 7, "title": "Add MCP tab", "draft": true,
            "user": {"login": "dasmatus"}, "html_url": "https://github.com/o/r/pull/7",
            "head": {"ref": "mcp"}
        }]);
        let pulls = parse_pull_requests(Forge::GitHub, &github)?;
        assert_eq!(pulls[0].number, 7);
        assert_eq!(pulls[0].author, "dasmatus");
        assert!(pulls[0].draft);

        let gitlab = serde_json::json!([{
            "iid": 3, "title": "Fix", "author": {"username": "a"},
            "web_url": "https://gitlab.com/g/p/-/merge_requests/3",
            "source_branch": "fix", "work_in_progress": true
        }]);
        let merges = parse_pull_requests(Forge::GitLab, &gitlab)?;
        assert_eq!(merges[0].number, 3);
        assert_eq!(merges[0].branch, "fix");
        assert!(merges[0].draft);

        assert!(
            parse_pull_requests(Forge::GitHub, &serde_json::json!({"message": "Not Found"}))
                .is_err()
        );
        Ok(())
    }
}
