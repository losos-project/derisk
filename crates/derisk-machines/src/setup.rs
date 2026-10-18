//! What the Machines app finds on the system before it can make a machine:
//! the templates the OS ships and whether pm can manage machines yet.

use std::path::{Path, PathBuf};

use derisk_sys::{env, fs};

/// Where under a data directory (`/run/current-system/sw/share` on LosOS)
/// templates live. losos-desktop's `nixos/guest/` builds one into the image
/// here, inside the verity `/usr`, so nobody can swap it for another.
pub const TEMPLATE_DIR: &str = "losos/machines";

/// Where under a data directory the OS keeps pm's plugins, unsigned.
pub const PLUGIN_DIR: &str = "pm/plugins";

/// The file name of pm's quickemu plugin, the one that manages machines.
pub const QUICKEMU_PLUGIN: &str = "quickemu.wasm";

/// The file name of pm's libvirt plugin, which managed machines before
/// quickemu did. The app set it up the same way, so it takes its copy back
/// out when it sets up the quickemu one (`worker.rs`).
pub const LIBVIRT_PLUGIN: &str = "libvirt.wasm";

/// The extensions pm reads a template from: qcow2, or a raw disk.
const TEMPLATE_EXTENSIONS: [&str; 3] = ["qcow2", "raw", "img"];

/// A disk image a machine can be made from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Template {
    /// The name pm gives it, from the file's stem.
    pub name: String,
    /// The image.
    pub path: PathBuf,
}

/// Every template in `data_dirs`, by name, the first directory winning a
/// name two of them have, as XDG data directories do.
pub fn templates(data_dirs: &[PathBuf]) -> Vec<Template> {
    let mut found: Vec<Template> = Vec::new();
    for dir in data_dirs {
        let Ok(entries) = fs::read_dir(dir.join(TEMPLATE_DIR)) else {
            continue;
        };
        let mut here: Vec<Template> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| TEMPLATE_EXTENSIONS.contains(&e))
                    && fs::is_file(path)
            })
            .filter_map(|path| {
                let name = path.file_stem()?.to_str()?.to_owned();
                Some(Template { name, path })
            })
            .filter(|t| found.iter().all(|f| f.name != t.name))
            .collect();
        here.sort_by(|a, b| a.name.cmp(&b.name));
        found.append(&mut here);
    }
    found
}

/// Whether pm can manage machines for this person.
///
/// pm loads a plugin only from the person's own plugin directory and only
/// signed by a key they trust (losos-project/pm, `plugins/README.md`,
/// "Trust"). The OS ships the plugin but cannot sign it for them: a
/// signature the image made would be a key every LosOS machine trusts. So
/// the person sets it up once, from the Machines app, and the app says
/// when the OS's copy has moved on from theirs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Plugin {
    /// The OS ships no quickemu plugin, so pm is set up some other way and
    /// the app does not offer to set it up.
    NotShipped,
    /// The person has not installed it.
    NotInstalled {
        /// The OS's copy.
        shipped: PathBuf,
        /// Where it goes.
        installed: PathBuf,
    },
    /// Installed but not signed, or since changed.
    Unsigned {
        /// The OS's copy.
        shipped: PathBuf,
        /// Where it goes.
        installed: PathBuf,
    },
    /// Installed and signed, but an update has brought a different one.
    Outdated {
        /// The OS's copy.
        shipped: PathBuf,
        /// Where it goes.
        installed: PathBuf,
    },
    /// Installed, signed, and the same as the OS's.
    Ready,
}

impl Plugin {
    /// Looks in `data_dirs` for the OS's copy and in `plugins` (the
    /// person's `~/.config/pm/plugins`) for theirs.
    pub fn find(data_dirs: &[PathBuf], plugins: &Path) -> Plugin {
        let Some(shipped) = data_dirs
            .iter()
            .map(|dir| dir.join(PLUGIN_DIR).join(QUICKEMU_PLUGIN))
            .find(|path| fs::is_file(path))
        else {
            return Plugin::NotShipped;
        };
        let installed = plugins.join(QUICKEMU_PLUGIN);
        let signature = plugins.join(format!("{QUICKEMU_PLUGIN}.sig"));
        if !fs::is_file(&installed) {
            return Plugin::NotInstalled { shipped, installed };
        }
        // pm checks the signature itself when it loads the plugin; this only
        // says whether there is one, and whether it is not older than the
        // file it signs.
        let modified = |path: &Path| fs::metadata(path).and_then(|m| m.modified()).ok();
        if !fs::is_file(&signature) || modified(&signature) < modified(&installed) {
            return Plugin::Unsigned { shipped, installed };
        }
        if fs::read(&shipped).ok() != fs::read(&installed).ok() {
            return Plugin::Outdated { shipped, installed };
        }
        Plugin::Ready
    }

    /// The OS's copy and where it goes, when there is something to set up.
    pub fn paths(&self) -> Option<(&Path, &Path)> {
        match self {
            Plugin::NotInstalled { shipped, installed }
            | Plugin::Unsigned { shipped, installed }
            | Plugin::Outdated { shipped, installed } => Some((shipped, installed)),
            Plugin::NotShipped | Plugin::Ready => None,
        }
    }
}

/// `$XDG_DATA_DIRS`, or its default, as the spec says.
pub fn data_dirs() -> Vec<PathBuf> {
    let dirs = env::var_os("XDG_DATA_DIRS").filter(|v| !v.is_empty());
    let dirs = dirs.as_deref().map_or_else(
        || {
            vec![
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/usr/share"),
            ]
        },
        |v| std::env::split_paths(v).collect(),
    );
    dirs.into_iter().filter(|d| d.is_absolute()).collect()
}

/// The person's pm plugin directory, as pm finds it: `$XDG_CONFIG_HOME`
/// or `~/.config`, then `pm/plugins`.
pub fn plugin_dir() -> Option<PathBuf> {
    let config = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(config.join("pm").join("plugins"))
}

/// Whether pm would accept `name` for a machine: a lowercase letter, then
/// up to 31 lowercase letters, digits and hyphens. The same rule as pm's
/// `check_machine_name`, so the app can say so before pm runs.
pub fn valid_name(name: &str) -> bool {
    derisk_settings::machine_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "derisk-machines-{name}-{}",
            derisk_sys::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn finds_templates_first_dir_first() {
        let root = scratch("templates");
        let (a, b) = (root.join("a"), root.join("b"));
        for dir in [&a, &b] {
            fs::create_dir_all(dir.join(TEMPLATE_DIR)).unwrap();
        }
        fs::write(a.join(TEMPLATE_DIR).join("losos-guest.qcow2"), "a").unwrap();
        fs::write(a.join(TEMPLATE_DIR).join("README"), "not a disk").unwrap();
        fs::write(b.join(TEMPLATE_DIR).join("losos-guest.raw"), "b").unwrap();
        fs::write(b.join(TEMPLATE_DIR).join("debian.img"), "b").unwrap();

        let found = templates(&[a.clone(), b.clone(), root.join("missing")]);
        let names: Vec<_> = found.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["losos-guest", "debian"]);
        assert_eq!(
            found[0].path,
            a.join(TEMPLATE_DIR).join("losos-guest.qcow2")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn plugin_states() {
        let root = scratch("plugin");
        let share = root.join("share");
        let mine = root.join("config/pm/plugins");
        assert_eq!(
            Plugin::find(std::slice::from_ref(&share), &mine),
            Plugin::NotShipped
        );

        fs::create_dir_all(share.join(PLUGIN_DIR)).unwrap();
        fs::write(share.join(PLUGIN_DIR).join(QUICKEMU_PLUGIN), "v1").unwrap();
        let state = Plugin::find(std::slice::from_ref(&share), &mine);
        assert!(matches!(state, Plugin::NotInstalled { .. }));
        assert_eq!(
            state.paths().unwrap().1,
            mine.join(QUICKEMU_PLUGIN).as_path()
        );

        fs::create_dir_all(&mine).unwrap();
        fs::write(mine.join(QUICKEMU_PLUGIN), "v1").unwrap();
        assert!(matches!(
            Plugin::find(std::slice::from_ref(&share), &mine),
            Plugin::Unsigned { .. }
        ));

        fs::write(mine.join("quickemu.wasm.sig"), "sig").unwrap();
        assert_eq!(
            Plugin::find(std::slice::from_ref(&share), &mine),
            Plugin::Ready
        );

        fs::write(share.join(PLUGIN_DIR).join(QUICKEMU_PLUGIN), "v2").unwrap();
        assert!(matches!(
            Plugin::find(&[share], &mine),
            Plugin::Outdated { .. }
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn names() {
        for good in ["work", "a", "dev-2", &"a".repeat(32)] {
            assert!(valid_name(good), "{good}");
        }
        for bad in ["", "Work", "2fa", "-x", "a_b", "a.b", &"a".repeat(33)] {
            assert!(!valid_name(bad), "{bad}");
        }
    }
}
