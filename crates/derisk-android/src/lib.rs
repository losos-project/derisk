//! The Android app (`org.derisk.android`): a person's own Pixel, a virtual
//! one, and its apps, each of which opens as a window of its own on the
//! desktop. It is to Android apps what WinBoat is to Windows' apps: the
//! Pixel's state with Start and Stop beside it, its setup's progress the
//! first time, and every app the Pixel has, as icons that open them.
//!
//! The Pixel itself is the OS's: LosOS's `android-app-support`
//! (losos-desktop, `nixos/pkgs/android-app-support.nix`) downloads Google's
//! Android Emulator and Android with Google Play, makes a Pixel 10 Pro of
//! them, runs it as the person's user unit and opens each app on a display
//! of the Pixel's own through scrcpy. This crate only asks it:
//!
//! - [`Status`] is its answer to `android-app-support status`, a line of
//!   JSON ([`Status::parse`]).
//! - [`AndroidApp`] is one of the Pixel's apps from the list it keeps,
//!   `apps.tsv` beside the apps' icons ([`read_apps`]).
//! - [`AndroidSupport`] is the app the person sees.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod app;
mod pixel;
#[cfg(not(target_arch = "wasm32"))]
mod worker;

pub use app::{APP_ID, AndroidSupport};
pub use pixel::{AndroidApp, State, Status, data_dir, parse_apps, read_apps};

/// The program the OS ships that does everything this app asks for.
pub const COMMAND: &str = "android-app-support";
