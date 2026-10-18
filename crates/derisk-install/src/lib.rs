//! What `derisk installer` and `derisk setup` need from the system, without
//! drawing anything.
//!
//! - [`account`]: user names, groups and homectl's messages, for setup's
//!   first account and the Settings app's Users page.
//! - [`install`]: the JSON-lines protocol between the installer and the
//!   program that installs.
//! - [`locale`]: languages, keyboard layouts and time zones through
//!   localectl and timedatectl.
//! - [`network`]: whether the machine is online, and Wi-Fi, through
//!   NetworkManager.
//! - [`wifi`]: [`network`] watched from a thread of its own, for a page to
//!   draw.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod account;
pub mod install;
pub mod locale;
// NetworkManager is reached by running nmcli, which a browser page (the
// Settings app's demo) cannot do.
#[cfg(unix)]
pub mod network;
#[cfg(unix)]
pub mod wifi;
