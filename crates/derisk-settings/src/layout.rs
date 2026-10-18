//! The Settings app's frame: the grouped sidebar, the page's header and the
//! cards its rows sit in, and the spacing between them.
//!
//! Every colour, radius and font size here comes from the theme's
//! component tokens ([`Tokens`]), so a theme restyles the whole app; the
//! pages only lay out rows.

use mcsapi_components::Tokens;
use mcsapi_ui::egui::{self, Color32, CornerRadius, Margin, RichText, Sense, Stroke, StrokeKind};

use crate::Page;

/// The sidebar's sections, in order. A page missing from the list the app
/// shows (a choice screen the policy leaves off) is skipped, and so is a
/// section left empty.
pub const GROUPS: [(&str, &[Page]); 4] = [
    (
        "Personalize",
        &[
            Page::Appearance,
            Page::Wallpaper,
            Page::TopBar,
            Page::Desktop,
        ],
    ),
    (
        "Devices",
        &[Page::Input, Page::Shortcuts, Page::Network, Page::Bluetooth],
    ),
    ("Security", &[Page::Privacy, Page::SignIn, Page::Users]),
    (
        "System",
        &[
            Page::Notifications,
            Page::Power,
            Page::Backups,
            Page::DefaultApps,
            Page::System,
            Page::About,
        ],
    ),
];

/// The widest a page's column grows: rows wider than this are hard to read
/// across, so a wide window gets margins instead.
pub const COLUMN: f32 = 760.0;

/// The width of a row's label in a card, before its control, and the gap
/// after it: a phone's card gives the control most of its width.
pub fn label_width(card: f32) -> (f32, f32) {
    if card < PHONE {
        (110.0, 16.0)
    } else {
        (200.0, 40.0)
    }
}

/// Below this a page is laid out for a phone: tighter margins and labels.
const PHONE: f32 = 600.0;

/// Room around a page's column: at the sides at least, above and below.
const SIDE: f32 = 32.0;
const TOP: f32 = 28.0;
const BOTTOM: f32 = 32.0;

/// Sets the app's spacing and control shapes from `tokens`: roomier rows
/// and buttons than egui's defaults, controls rounded as the theme's.
pub fn style(ui: &mut egui::Ui, tokens: &Tokens) {
    let style = ui.style_mut();
    style.spacing.item_spacing = egui::vec2(10.0, 12.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.interact_size.y = 30.0;
    style.spacing.icon_width = 18.0;
    style.spacing.icon_width_inner = 10.0;
    let radius = tokens.control_radius();
    let visuals = &mut style.visuals;
    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = radius;
    }
    visuals.widgets.inactive.weak_bg_fill = tokens.field;
    visuals.widgets.inactive.bg_fill = tokens.field;
    // Checkboxes and fields outlined, so they read on a card as well as on
    // the page.
    visuals.widgets.inactive.bg_stroke = tokens.border_stroke();
    visuals.widgets.hovered.weak_bg_fill = tokens.hover;
    visuals.selection.bg_fill = tokens.primary;
    visuals.selection.stroke = Stroke::new(1.0, tokens.primary_foreground);
    visuals.extreme_bg_color = tokens.field;
    visuals.window_corner_radius = tokens.card_radius();
    visuals.menu_corner_radius = tokens.card_radius();
}

/// The sidebar: each section's title over its pages, the open one filled.
/// Returns the page picked this frame.
pub fn sidebar(
    ui: &mut egui::Ui,
    tokens: &Tokens,
    open: Page,
    listed: impl Fn(Page) -> bool,
) -> Option<Page> {
    let mut picked = None;
    // Rows close together, sections apart.
    ui.spacing_mut().item_spacing.y = 2.0;
    ui.add_space(8.0);
    for (title, pages) in GROUPS {
        let pages: Vec<Page> = pages.iter().copied().filter(|&p| listed(p)).collect();
        if pages.is_empty() {
            continue;
        }
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            // In line with the pages' names.
            ui.add_space(14.0);
            ui.label(
                RichText::new(title)
                    .font(tokens.small_font())
                    .strong()
                    .color(tokens.muted_foreground),
            );
        });
        ui.add_space(4.0);
        for page in pages {
            if entry(ui, tokens, page.label(), page == open).clicked() {
                picked = Some(page);
            }
        }
    }
    picked
}

/// One page in the sidebar: a full-width row, filled with the accent's tint
/// while open and the hover colour under the pointer.
fn entry(ui: &mut egui::Ui, tokens: &Tokens, label: &str, open: bool) -> egui::Response {
    let height = 36.0;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, open, label)
    });
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let radius = tokens.control_radius();
        let fill = if open {
            tint(tokens.primary)
        } else if response.hovered() {
            tokens.hover
        } else {
            Color32::TRANSPARENT
        };
        painter.rect_filled(rect, radius, fill);
        if open {
            // A short bar at the row's start, as the open page's marker
            // that does not rely on the tint alone.
            let bar = egui::Rect::from_center_size(
                egui::pos2(rect.left() + 2.0, rect.center().y),
                egui::vec2(3.0, height * 0.5),
            );
            painter.rect_filled(bar, CornerRadius::same(2), tokens.primary);
        }
        painter.text(
            egui::pos2(rect.left() + 14.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(15.0),
            if open {
                tokens.foreground
            } else {
                tokens.foreground.gamma_multiply(0.85)
            },
        );
        if response.has_focus() {
            painter.rect_stroke(rect, radius, tokens.ring_stroke(), StrokeKind::Inside);
        }
    }
    response
}

/// `color` faint enough to fill behind text.
fn tint(color: Color32) -> Color32 {
    color.gamma_multiply(0.22)
}

/// Lays `content` out in the page's column: centred, no wider than
/// [`COLUMN`], with room around it.
pub fn column<R>(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let full = ui.available_width();
    // A phone's width is too precious for the desktop's margins.
    let side = if full < PHONE { 12.0 } else { SIDE };
    let width = (full - 2.0 * side).clamp(0.0, COLUMN);
    let left = ((full - width) / 2.0).max(0.0);
    ui.add_space(TOP);
    let inner = ui
        .horizontal_top(|ui| {
            ui.add_space(left);
            ui.vertical(|ui| {
                ui.set_width(width);
                content(ui)
            })
            .inner
        })
        .inner;
    ui.add_space(BOTTOM);
    inner
}

/// The page's title and the line under it saying what it holds.
pub fn header(ui: &mut egui::Ui, tokens: &Tokens, page: Page) {
    ui.label(
        RichText::new(page.label())
            .font(egui::FontId::proportional(26.0))
            .strong()
            .color(tokens.foreground),
    );
    ui.add_space(-4.0);
    ui.label(
        RichText::new(page.description())
            .font(egui::FontId::proportional(14.0))
            .color(tokens.muted_foreground),
    );
    ui.add_space(12.0);
}

/// A card holding a group of rows.
pub fn card<R>(ui: &mut egui::Ui, tokens: &Tokens, content: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let margin = if ui.available_width() < PHONE {
        Margin::symmetric(14, 14)
    } else {
        Margin::symmetric(24, 20)
    };
    egui::Frame::new()
        .fill(tokens.card)
        .stroke(tokens.border_stroke())
        .corner_radius(tokens.card_radius())
        .inner_margin(margin)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            content(ui)
        })
        .inner
}

/// A note under a row, in the muted colour the theme gives descriptions.
pub fn note(tokens: &Tokens, text: &str) -> RichText {
    RichText::new(text)
        .font(tokens.small_font())
        .color(tokens.muted_foreground)
}
