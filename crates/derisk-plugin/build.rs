//! Builds the bundled plugins under `plugins/palette/` and
//! `plugins/widgets/` and wraps each in a component for `src/lib.rs` to
//! embed, the way pm's build script builds pm's bundled plugins.
//!
//! The plugins are members of derisk's workspace, so one Cargo.lock (and one
//! vendored set of sources in a Nix build) covers them, but they have to be
//! compiled for `wasm32-unknown-unknown`, which the outer build is not doing.
//! A nested cargo builds them into `wasm-plugins/` in the target directory,
//! so it never waits on the lock the outer build holds, and the
//! `wasm-plugin` profile in the workspace's Cargo.toml keeps them small.

use std::{
    env,
    ffi::OsString,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use wit_component::ComponentEncoder;

const TARGET: &str = "wasm32-unknown-unknown";
const PROFILE: &str = "wasm-plugin";

/// The bundled palette plugins, in the order the palette asks them. Each is
/// the package `palette-<name>` under `plugins/palette/<name>`.
const PALETTE: [&str; 15] = [
    "apps",
    "windows",
    "menus",
    "commands",
    "workspaces",
    "session",
    "power",
    "system",
    "browser",
    "machines",
    "files",
    "boxes",
    "assistant",
    "agent",
    "web",
];

/// The bundled widget plugins, in the order the overview shows their cards.
/// Each is the package `widget-<name>` under `plugins/widgets/<name>`.
const WIDGETS: [&str; 7] = [
    "clock",
    "suggestions",
    "services",
    "calendar",
    "battery",
    "notes",
    "programs",
];

/// A kind of bundled plugin. Its packages are `<prefix>-<name>`, and
/// `bundled.rs` lists its components under `constant`.
#[derive(Clone, Copy)]
enum Set {
    Palette,
    Widget,
}

impl Set {
    const ALL: [Self; 2] = [Self::Palette, Self::Widget];

    fn prefix(self) -> &'static str {
        match self {
            Self::Palette => "palette",
            Self::Widget => "widget",
        }
    }

    fn names(self) -> &'static [&'static str] {
        match self {
            Self::Palette => &PALETTE,
            Self::Widget => &WIDGETS,
        }
    }

    fn constant(self) -> &'static str {
        match self {
            Self::Palette => "PALETTE",
            Self::Widget => "WIDGETS",
        }
    }

    /// The set's packages, `<prefix>-<name>`, in order.
    fn packages(self) -> impl Iterator<Item = (&'static str, String)> {
        self.names()
            .iter()
            .map(move |name| (*name, format!("{}-{name}", self.prefix())))
    }
}

/// What cargo sets for this build script that describes derisk's own build.
/// Passed through, the nested cargo would build the plugins with derisk's
/// flags or into derisk's target directory.
const NOT_INHERITED: [&str; 6] = [
    "CARGO_ENCODED_RUSTFLAGS",
    "RUSTFLAGS",
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_TARGET",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_PRIMARY_PACKAGE",
];

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("set by cargo"));
    let workspace = manifest
        .ancestors()
        .nth(2)
        .expect("crates/derisk-plugin sits two levels below the workspace")
        .to_path_buf();
    let plugins = workspace.join("plugins");
    println!("cargo::rerun-if-changed={}", plugins.display());
    println!("cargo::rerun-if-changed={}", manifest.join("wit").display());
    println!(
        "cargo::rerun-if-changed={}",
        workspace.join("Cargo.lock").display()
    );

    let target_dir = shared_target_dir(&out);
    compile(&workspace, &target_dir);

    let components = out.join("bundled");
    fs::create_dir_all(&components).expect("create the bundled components directory");
    let mut list = String::new();
    for set in Set::ALL {
        writeln!(
            list,
            "/// The bundled {} plugins, in the order they are asked.\n\
             pub(crate) const {}: &[(&str, &[u8])] = &[",
            set.prefix(),
            set.constant(),
        )
        .expect("write to a string");
        for (name, package) in set.packages() {
            // Cargo names a wasm module after its crate, with `_` for `-`.
            let module = target_dir
                .join(TARGET)
                .join(PROFILE)
                .join(format!("{}.wasm", package.replace('-', "_")));
            let component = components.join(format!("{package}.wasm"));
            encode(&module, &component);
            writeln!(
                list,
                "    ({name:?}, include_bytes!({:?})),",
                component.display().to_string()
            )
            .expect("write to a string");
        }
        list.push_str("];\n");
    }
    write_if_changed(list.as_bytes(), &out.join("bundled.rs"));
}

/// `wasm-plugins/` in the target directory, found as the ancestor of
/// `OUT_DIR` that cargo marked with a `CACHEDIR.TAG`; under `OUT_DIR` if
/// there is none.
fn shared_target_dir(out: &Path) -> PathBuf {
    out.ancestors()
        .find(|dir| dir.join("CACHEDIR.TAG").is_file())
        .map_or_else(
            || out.join("wasm-plugins"),
            |root| root.join("wasm-plugins"),
        )
}

fn compile(workspace: &Path, target_dir: &Path) {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let mut command = Command::new(cargo);
    command
        .current_dir(workspace)
        .args([
            "build",
            "--locked",
            "--profile",
            PROFILE,
            "--target",
            TARGET,
        ])
        .arg("--target-dir")
        .arg(target_dir);
    for (_, package) in Set::ALL.into_iter().flat_map(Set::packages) {
        command.arg("-p").arg(package);
    }
    for key in NOT_INHERITED {
        command.env_remove(key);
    }
    let status = command
        .status()
        .unwrap_or_else(|error| panic!("cannot run cargo to build the plugins: {error}"));
    assert!(
        status.success(),
        "building the bundled plugins failed ({status}). They are WebAssembly: this \
         needs `rustup target add {TARGET}`, or with nixpkgs' rustc, lld on PATH"
    );
}

/// Wraps the core module at `module` in a component, validated, so a module
/// that does not make one fails the build rather than the session.
fn encode(module: &Path, component: &Path) {
    let bytes = fs::read(module)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", module.display()));
    let encoded = ComponentEncoder::default()
        .module(&bytes)
        .and_then(|encoder| encoder.validate(true).encode())
        .unwrap_or_else(|error| {
            panic!(
                "cannot encode {} as a component: {error:?}",
                module.display()
            )
        });
    write_if_changed(&encoded, component);
}

/// Rewriting an unchanged file would rebuild derisk for nothing.
fn write_if_changed(bytes: &[u8], destination: &Path) {
    if fs::read(destination).ok().as_deref() != Some(bytes) {
        fs::write(destination, bytes)
            .unwrap_or_else(|error| panic!("cannot write {}: {error}", destination.display()));
    }
}
