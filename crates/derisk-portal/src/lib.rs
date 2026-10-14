//! derisk's xdg-desktop-portal backend, `xdg-desktop-portal-derisk`.
//!
//! xdg-desktop-portal is the frontend every Flatpak app talks to; it hands
//! each request to the backend the desktop's `derisk-portals.conf` names.
//! This backend serves what only derisk knows:
//!
//! | Interface | From |
//! | --- | --- |
//! | `Settings` (`org.freedesktop.appearance`) | the theme derisk publishes, so GTK 4, Qt, Firefox and Electron apps follow dark mode and the accent |
//! | `Screenshot` | the compositor, over the agent socket; an area or a color picked with `derisk-gpui pick` ([`pick`]) |
//! | `Wallpaper` | derisk's settings file, which the session watches |
//! | `Background` | which apps have windows, over the agent socket |
//!
//! Everything else (the file chooser, the access dialog, printing, ...) is
//! left to the GTK backend, which `derisk-portals.conf` lists after this
//! one. The portals that show this backend's own consent ask with derisk's
//! dialog, `derisk-gpui ask`, or the GTK access dialog without it
//! ([`access`]).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod access;
pub mod agent;
pub mod appearance;
pub mod files;
pub mod pick;
pub mod portal;

/// The bus name xdg-desktop-portal looks for, from `derisk.portal`.
pub const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.derisk";
