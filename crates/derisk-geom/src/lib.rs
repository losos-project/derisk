//! Geometry, Windows-style snapping and the window buttons:
//! the layout primitives the derisk shell places windows with.
//!
//! - [`geom`]: helpers around mcsapi's logical geometry.
//! - [`snap`]: edge and corner drop zones and Super+arrow nudging.
//! - [`decorations`]: the window buttons the top bar shows.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod decorations;
pub mod geom;
pub mod snap;
