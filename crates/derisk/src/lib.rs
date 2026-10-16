//! derisk: an adaptive, agent-first Wayland desktop shell built on
//! [mcsapi](https://github.com/dasmatus/mcsapi).
//!
//! mcsapi supplies workspaces, focus and tiling policy in Smithay's logical
//! coordinates. derisk layers the rest of a desktop on top:
//!
//! - [`shell`]: tiling plus floating, snapped and minimized windows, and
//!   Windows-style dragging with edge/corner snapping and Snap Assist.
//! - [`decorations`]: server-side title bars with the buttons on the left.
//! - [`adaptive`]: phone/tablet/desktop profiles and learned app suggestions.
//! - [`mobile`]: the touch navigation bar and edge swipes on phones.
//! - [`keyboard`]: the on-screen keyboard and its word prediction.
//! - [`overview`]: the overview grid and its widgets.
//! - [`desktop`]: installed apps and their actions from `.desktop` files.
//! - [`apps`] and [`icons`]: app names and icon theme icons for app IDs.
//! - [`menu`]: the global menu.
//! - [`palette`]: the command palette (Super+Space), the center of the
//!   workflow: apps, windows, commands, settings and files in one search,
//!   with the assistant as the fallback.
//! - [`tray`]: monochrome system tray icons.
//! - [`animation`]: the startup animation.
//! - [`effects`]: translucent, blurred panels and low power mode.
//! - [`assistant`] and [`ipc`]: natural-language and agent control, and
//!   [`mcp`]: the agent tools as an MCP server.
//! - [`conversation`]: requests and their progress, shown in the palette.
//! - [`notification_stack`]: the notifications apps send the session over
//!   D-Bus, and which of them are on screen.
//! - [`keys`]: keyboard shortcuts.
//! - [`lock`] and [`greetd`]: the lock screen, and the login conversation
//!   `derisk greeter` holds with `derisk display-manager` (or greetd);
//!   [`polkit`]: the dialog the session's polkit agent shows.
//! - [`idle`]: dimming, locking and suspending when nobody uses the
//!   session, through swayidle.
//! - [`systemd`]: apps as transient units, sd_notify, socket activation,
//!   journald, logind session actions and focus-aware resource weights.
//! - [`ui`]: egui rendering of all shell surfaces.
//! - [`privacy`]: forgetting recent files and emptying old trash.
//! - [`wallpaper`]: the desktop background, from a color to a looping video.
//! - [`wizard`], [`setup`], [`install`], [`locale`] and [`network`]: the
//!   installer (`derisk installer`) and first-boot setup (`derisk setup`),
//!   their pages, the backend protocol an installer speaks, languages,
//!   keyboard layouts and time zones, and Wi-Fi.
//!
//! The shell's lowest layers are crates of their own, re-exported here at the
//! paths they always had: `derisk-geom` ([`geom`], [`snap`], [`decorations`]),
//! `derisk-desktop` ([`desktop`], [`apps`]), `derisk-install` ([`install`],
//! [`locale`], [`network`]) and `derisk-login` ([`greetd`], [`lock`], [`polkit`]).
//!
//! ```
//! use derisk::{action::Action, geom::rect, shell::Shell, snap::SnapZone};
//!
//! let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
//! let (editor, _) = shell.map_window("editor", "notes.txt");
//! shell.map_window("terminal", "~");
//! shell.apply(Action::Snap { window: Some(editor.get()), zone: SnapZone::Left })?;
//! assert_eq!(shell.placements().len(), 2);
//! # Ok::<(), derisk::shell::Error>(())
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod action;
pub mod adaptive;
pub mod animation;
pub mod assistant;
pub mod conversation;
pub mod effects;
pub mod icons;
pub mod idle;
pub mod ipc;
pub mod keyboard;
pub mod keys;
pub mod mcp;
pub mod menu;
pub mod mobile;
pub mod motion;
pub mod notification_stack;
pub mod overview;
pub mod palette;
pub mod password_age;
pub mod plugins;
pub mod privacy;
pub mod setup;
pub mod shell;
pub mod systemd;
pub mod theme;
pub mod time;
pub mod tray;
pub mod ui;
pub mod wallpaper;
pub mod widgets;
pub mod wifi;
pub mod window_motion;
pub mod wizard;

// The layers with nothing above them in the shell live in their own crates;
// they stay at the paths they had when they were modules here.
pub use derisk_desktop::apps;
pub use derisk_desktop::desktop;
pub use derisk_geom::decorations;
pub use derisk_geom::geom;
pub use derisk_geom::snap;
pub use derisk_install::install;
pub use derisk_install::locale;
pub use derisk_install::network;
pub use derisk_login::greetd;
pub use derisk_login::lock;
pub use derisk_login::polkit;
pub use mcsapi;
