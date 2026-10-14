//! What the derisk apps ask of the system, in one place, so they also run
//! in a browser.
//!
//! On Linux every item here is std's own, re-exported or called straight
//! through, and the apps behave exactly as if they used std. Built for
//! `wasm32-unknown-unknown`, std has no filesystem, and its clock,
//! environment, process ID and threads panic when asked. There the same
//! names reach a demo system instead: an in-memory filesystem holding a
//! home folder with a few documents and a `/proc` whose numbers move, a
//! fixed environment (`HOME=/home/demo`), and the browser's clock. Nothing
//! leaves the page, and a reload starts it over.
//!
//! An app imports `derisk_sys::fs` where it imported `std::fs`, and asks
//! [`fs::is_dir`] rather than `Path::is_dir`, which would go to std.
//!
//! ```
//! use derisk_sys::fs;
//!
//! let dir = std::env::temp_dir().join(format!("derisk-sys-{}", derisk_sys::process::id()));
//! fs::create_dir_all(&dir)?;
//! fs::write(dir.join("note.txt"), "hello")?;
//! assert!(fs::is_file(dir.join("note.txt")));
//! assert_eq!(fs::read_to_string(dir.join("note.txt"))?, "hello");
//! fs::remove_dir_all(&dir)?;
//! # Ok::<(), std::io::Error>(())
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

#[cfg(target_family = "wasm")]
mod demo;

/// std's filesystem, or the demo system's in a browser.
pub mod fs {
    use std::path::Path;

    #[cfg(target_family = "wasm")]
    pub use crate::demo::fs::*;
    #[cfg(not(target_family = "wasm"))]
    pub use std::fs::*;

    /// Whether `path` is a directory, following links: `Path::is_dir`
    /// through this module.
    pub fn is_dir(path: impl AsRef<Path>) -> bool {
        metadata(path).is_ok_and(|m| m.is_dir())
    }

    /// Whether `path` is a regular file, following links: `Path::is_file`
    /// through this module.
    pub fn is_file(path: impl AsRef<Path>) -> bool {
        metadata(path).is_ok_and(|m| m.is_file())
    }

    /// Whether anything is at `path`, following links: `Path::exists`
    /// through this module.
    pub fn exists(path: impl AsRef<Path>) -> bool {
        metadata(path).is_ok()
    }
}

/// The Unix extensions to [`fs`]'s types: modes and open flags. In the
/// demo system modes are kept and reported but never enforced, and open
/// flags are ignored.
pub mod unix {
    #[cfg(target_family = "wasm")]
    pub use crate::demo::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    #[cfg(not(target_family = "wasm"))]
    pub use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
}

/// Paths.
pub mod path {
    use std::{io, path::Path, path::PathBuf};

    /// `std::path::absolute`. The demo system's working directory is `/`.
    pub fn absolute(path: impl AsRef<Path>) -> io::Result<PathBuf> {
        #[cfg(target_family = "wasm")]
        return Ok(crate::demo::normalize(path.as_ref()));
        #[cfg(not(target_family = "wasm"))]
        std::path::absolute(path)
    }
}

/// Environment variables.
pub mod env {
    use std::ffi::OsString;

    /// `std::env::var_os`. The demo system's environment is fixed: a user
    /// called `demo` with its home in `/home/demo`.
    pub fn var_os(key: &str) -> Option<OsString> {
        #[cfg(target_family = "wasm")]
        return crate::demo::env(key).map(OsString::from);
        #[cfg(not(target_family = "wasm"))]
        std::env::var_os(key)
    }

    /// `std::env::var`.
    pub fn var(key: &str) -> Result<String, std::env::VarError> {
        #[cfg(target_family = "wasm")]
        return crate::demo::env(key)
            .map(str::to_owned)
            .ok_or(std::env::VarError::NotPresent);
        #[cfg(not(target_family = "wasm"))]
        std::env::var(key)
    }
}

/// The clock.
pub mod time {
    use std::time::SystemTime;

    #[cfg(not(target_family = "wasm"))]
    pub use std::time::Instant;
    /// A monotonic clock reading; `std::time::Instant` outside a browser.
    #[cfg(target_family = "wasm")]
    pub use web_time::Instant;

    /// `SystemTime::now()`. In a browser this is the page's clock, as a
    /// std `SystemTime` so it compares with file times.
    pub fn now() -> SystemTime {
        #[cfg(target_family = "wasm")]
        return crate::demo::now();
        #[cfg(not(target_family = "wasm"))]
        SystemTime::now()
    }
}

/// The running process.
pub mod process {
    /// `std::process::id()`. The demo system is one process, number 1.
    pub fn id() -> u32 {
        #[cfg(target_family = "wasm")]
        return 1;
        #[cfg(not(target_family = "wasm"))]
        std::process::id()
    }
}

/// Background work.
pub mod thread {
    /// Runs `work` on a thread of its own that nobody joins. A browser page
    /// has one thread, so there `work` runs before this returns: callers
    /// hand work here whose results come back over a channel, and the
    /// demo system has nothing that blocks for long.
    pub fn detach(work: impl FnOnce() + Send + 'static) {
        #[cfg(target_family = "wasm")]
        work();
        #[cfg(not(target_family = "wasm"))]
        drop(std::thread::spawn(work));
    }
}
