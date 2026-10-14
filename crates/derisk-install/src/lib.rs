//! What `derisk installer` and `derisk setup` need from the system, without
//! drawing anything.
//!
//! - [`install`]: the JSON-lines protocol between the installer and the
//!   program that installs.
//! - [`locale`]: languages, keyboard layouts and time zones through
//!   localectl and timedatectl.
//! - [`network`]: whether the machine is online, and Wi-Fi through
//!   wpa_supplicant.
//! - [`wifi`]: [`network`] watched from a thread of its own, for a page to
//!   draw.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod install;
pub mod locale;
pub mod network;
pub mod wifi;
