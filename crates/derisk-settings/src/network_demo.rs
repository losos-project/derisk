//! The Network page in the browser demo, which has no network of its own to
//! show.

use mcsapi_ui::{Theme, egui};

/// Nothing to keep; a field, so it is built the way the real page's is.
#[derive(Debug, Default)]
pub(crate) struct NetworkUi {
    _demo: (),
}

pub(crate) fn page(ui: &mut egui::Ui, _: &mut NetworkUi, theme: &Theme) {
    ui.label(
        egui::RichText::new("Wired ports and Wi-Fi networks are listed here on a real machine.")
            .color(theme.border),
    );
}
