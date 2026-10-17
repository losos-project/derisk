//! The Store: Flathub's apps, sorted into categories from what their
//! AppStream data says of them, with the release notes they ship on the
//! front page, installed, updated, opened and removed through flatpak.
//!
//! On a PC flatpak runs in the `flatpak` machine, whose apps open on the
//! desktop like any other; on a phone, and on any computer without that
//! machine, it runs on the computer itself ([`flatpak`]).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod app;
pub mod appstream;
pub mod catalog;
pub mod category;
pub mod flatpak;
pub mod reviews;
mod worker;

pub use app::{APP_ID, StoreApp};
pub use catalog::{App, Catalog, News};
pub use category::Category;
