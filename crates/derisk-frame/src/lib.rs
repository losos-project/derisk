//! The frame every derisk app is drawn in, taken from Settings: a sidebar of
//! grouped pages, a page's title and the line under it, cards holding what
//! the page shows, and a bar along the bottom for what acts on it.
//!
//! Every colour, radius and font size here comes from the theme's
//! component tokens ([`Tokens`]), so a theme restyles every app along with
//! the desktop, and so does a change here: the apps only say what goes
//! where.

#![forbid(unsafe_code)]

use mcsapi_components::Tokens;
use mcsapi_ui::{
    Theme,
    egui::{
        self, Color32, CornerRadius, Margin, Rect, Response, RichText, Sense, Stroke, StrokeKind,
        Ui,
    },
};

/// Below this a page is laid out for a phone: tighter margins and labels.
pub const PHONE: f32 = 600.0;

/// Below this width an app's pages move from a sidebar to tabs on top: a
/// sidebar would leave a phone half a page.
pub const NARROW: f32 = 560.0;

/// The widest a page's column grows: rows wider than this are hard to read
/// across, so a wide window gets margins instead.
pub const COLUMN: f32 = 760.0;

/// The widest a page of tables or tiles grows ([`Page::wide`]): a listing
/// gains from the room a column of rows does not.
pub const WIDE: f32 = 1180.0;

/// Room around a page's column: at the sides at least, above and below.
const SIDE: f32 = 32.0;
const TOP: f32 = 28.0;
const BOTTOM: f32 = 32.0;

/// The tokens for `theme`, installed for the components drawn this frame
/// and with [`style`] applied to `ui`: what every app's `ui` starts with.
pub fn begin(ui: &mut Ui, theme: &Theme) -> Tokens {
    let tokens = Tokens::from_theme(theme);
    tokens.install(ui.ctx());
    style(ui, &tokens);
    tokens
}

/// Sets an app's spacing and control shapes from `tokens`: roomier rows
/// and buttons than egui's defaults, controls rounded as the theme's.
pub fn style(ui: &mut Ui, tokens: &Tokens) {
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
    visuals.faint_bg_color = tokens.card;
    visuals.window_corner_radius = tokens.card_radius();
    visuals.menu_corner_radius = tokens.card_radius();
}

/// Whether `ui` is too narrow for a sidebar beside its page.
pub fn is_narrow(ui: &Ui) -> bool {
    ui.available_width() < NARROW
}

/// The width of a row's label in a card, before its control, and the gap
/// after it: a phone's card gives the control most of its width.
pub fn label_width(card: f32) -> (f32, f32) {
    if card < PHONE {
        (110.0, 16.0)
    } else {
        (200.0, 40.0)
    }
}

/// `color` faint enough to fill behind text: the open page's and the
/// selected row's fill.
pub fn tint(color: Color32) -> Color32 {
    color.gamma_multiply(0.22)
}

/// The fill behind a row that is open or selected, under the pointer, or
/// neither.
pub fn row_fill(tokens: &Tokens, selected: bool, hovered: bool) -> Color32 {
    if selected {
        tint(tokens.primary)
    } else if hovered {
        tokens.hover
    } else {
        Color32::TRANSPARENT
    }
}

/// The app's pages: a sidebar down the left, or on a narrow window a row
/// of tabs across the top that scrolls sideways. `add` lists them through
/// a [`Nav`], which draws each the way the window's width calls for.
pub fn navigation<R>(
    ui: &mut Ui,
    tokens: &Tokens,
    id: &str,
    add: impl FnOnce(&mut Nav<'_>) -> R,
) -> R {
    if is_narrow(ui) {
        egui::Panel::top(egui::Id::new(id))
            .frame(
                egui::Frame::new()
                    .fill(tokens.card)
                    .stroke(tokens.border_stroke())
                    .inner_margin(Margin::symmetric(8, 6)),
            )
            .show(ui, |ui| {
                egui::ScrollArea::horizontal()
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            add(&mut Nav {
                                ui,
                                tokens,
                                narrow: true,
                            })
                        })
                        .inner
                    })
                    .inner
            })
            .inner
    } else {
        let sidebar = (ui.available_width() * 0.2).clamp(200.0, 248.0);
        egui::Panel::left(egui::Id::new(id))
            .resizable(false)
            .exact_size(sidebar)
            .frame(
                egui::Frame::new()
                    .fill(tokens.card)
                    .stroke(tokens.border_stroke())
                    .inner_margin(Margin::symmetric(12, 8)),
            )
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .show(ui, |ui| {
                        // Rows close together, sections apart.
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.add_space(8.0);
                        add(&mut Nav {
                            ui,
                            tokens,
                            narrow: false,
                        })
                    })
                    .inner
            })
            .inner
    }
}

/// The pages listed in [`navigation`], drawn as sidebar rows or as tabs.
pub struct Nav<'a> {
    ui: &'a mut Ui,
    tokens: &'a Tokens,
    narrow: bool,
}

impl Nav<'_> {
    /// Whether the pages are tabs across the top rather than a sidebar.
    pub fn is_narrow(&self) -> bool {
        self.narrow
    }

    /// The `Ui` the pages are listed in, for what is not a page: a button
    /// under them, a note.
    pub fn ui(&mut self) -> &mut Ui {
        self.ui
    }

    /// A section's title over the pages that follow. Tabs have no room
    /// for them, so a narrow window leaves them out.
    pub fn section(&mut self, title: &str) {
        if self.narrow {
            return;
        }
        let ui = &mut *self.ui;
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            // In line with the pages' names.
            ui.add_space(14.0);
            ui.label(
                RichText::new(title)
                    .font(self.tokens.small_font())
                    .strong()
                    .color(self.tokens.muted_foreground),
            );
        });
        ui.add_space(4.0);
    }

    /// A page, filled while `open`. The response says whether it was
    /// picked.
    pub fn entry(&mut self, label: &str, open: bool) -> Response {
        self.entry_with(None, label, open)
    }

    /// A page with its symbolic icon (a freedesktop name) before its name.
    pub fn entry_icon(&mut self, icon: &str, label: &str, open: bool) -> Response {
        self.entry_with(Some(icon), label, open)
    }

    fn entry_with(&mut self, icon: Option<&str>, label: &str, open: bool) -> Response {
        if self.narrow {
            let ui = &mut *self.ui;
            return match icon {
                Some(icon) => {
                    let color = ui.visuals().text_color();
                    let atom = derisk_icons::atom(ui.ctx(), icon, 16.0, color);
                    ui.add(egui::Button::selectable(open, (atom, label)))
                }
                None => ui.selectable_label(open, label),
            };
        }
        entry(self.ui, self.tokens, icon, label, open)
    }
}

/// One page in the sidebar: a full-width row, filled with the accent's tint
/// while open and the hover colour under the pointer.
fn entry(ui: &mut Ui, tokens: &Tokens, icon: Option<&str>, label: &str, open: bool) -> Response {
    let height = 36.0;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, open, label)
    });
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let radius = tokens.control_radius();
        painter.rect_filled(rect, radius, row_fill(tokens, open, response.hovered()));
        if open {
            // A short bar at the row's start, as the open page's marker
            // that does not rely on the tint alone.
            let bar = Rect::from_center_size(
                egui::pos2(rect.left() + 2.0, rect.center().y),
                egui::vec2(3.0, height * 0.5),
            );
            painter.rect_filled(bar, CornerRadius::same(2), tokens.primary);
        }
        let color = if open {
            tokens.foreground
        } else {
            tokens.foreground.gamma_multiply(0.85)
        };
        let mut x = rect.left() + 14.0;
        if let Some(icon) = icon {
            let size = 18.0;
            let at = Rect::from_min_size(
                egui::pos2(x, rect.center().y - size / 2.0),
                egui::vec2(size, size),
            );
            derisk_icons::paint(painter, at, icon, color);
            x += size + 10.0;
        }
        painter.text(
            egui::pos2(x, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(15.0),
            color,
        );
        if response.has_focus() {
            painter.rect_stroke(rect, radius, tokens.ring_stroke(), StrokeKind::Inside);
        }
    }
    response
}

/// A bar along the bottom of the app for what acts on the page, Settings'
/// Save and Revert: a status on the left, buttons laid out from the right.
pub fn action_bar<R>(ui: &mut Ui, tokens: &Tokens, id: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Panel::bottom(egui::Id::new(id))
        .frame(bar(tokens))
        .show(ui, |ui| ui.horizontal(add).inner)
        .inner
}

/// A bar along the top of the app, drawn as the [`action_bar`] is, for
/// what the whole app needs at hand on every page: a search field, a
/// location.
pub fn toolbar<R>(ui: &mut Ui, tokens: &Tokens, id: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Panel::top(egui::Id::new(id))
        .frame(bar(tokens))
        .show(ui, add)
        .inner
}

fn bar(tokens: &Tokens) -> egui::Frame {
    egui::Frame::new()
        .fill(tokens.background)
        .stroke(tokens.border_stroke())
        .inner_margin(Margin::symmetric(20, 12))
}

/// The page beside the sidebar: the theme's background, and a centred
/// column with room around it.
#[derive(Clone, Copy, Debug)]
pub struct Page {
    width: f32,
    scroll: bool,
}

impl Default for Page {
    fn default() -> Self {
        Self::new()
    }
}

impl Page {
    /// A page of rows, no wider than [`COLUMN`], that scrolls as a whole.
    pub const fn new() -> Self {
        Self {
            width: COLUMN,
            scroll: true,
        }
    }

    /// As wide as [`WIDE`], for tables and tiles.
    pub const fn wide(mut self) -> Self {
        self.width = WIDE;
        self
    }

    /// The column fills the window's height and does not scroll: what it
    /// holds scrolls itself, as a listing under a fixed header does.
    pub const fn fixed(mut self) -> Self {
        self.scroll = false;
        self
    }

    /// Draws the page with `add` in its column.
    pub fn show<R>(self, ui: &mut Ui, tokens: &Tokens, add: impl FnOnce(&mut Ui) -> R) -> R {
        egui::CentralPanel::no_frame()
            .show(ui, |ui| {
                ui.painter()
                    .rect_filled(ui.max_rect(), 0, tokens.background);
                if !self.scroll {
                    return fixed_column(ui, self.width, add);
                }
                // A row of controls wider than a phone scrolls rather than
                // being cut off where nothing can reach it.
                let scroll = if ui.available_width() < NARROW {
                    egui::ScrollArea::both()
                } else {
                    egui::ScrollArea::vertical()
                };
                scroll
                    .auto_shrink([false, false])
                    .show(ui, |ui| column_of(ui, self.width, add))
                    .inner
            })
            .inner
    }
}

/// The margin at the sides of a page `full` wide.
fn side(full: f32) -> f32 {
    // A phone's width is too precious for the desktop's margins.
    if full < PHONE { 12.0 } else { SIDE }
}

/// Lays `content` out in the page's column: centred, no wider than
/// [`COLUMN`], with room around it.
pub fn column<R>(ui: &mut Ui, content: impl FnOnce(&mut Ui) -> R) -> R {
    column_of(ui, COLUMN, content)
}

fn column_of<R>(ui: &mut Ui, max: f32, content: impl FnOnce(&mut Ui) -> R) -> R {
    let full = ui.available_width();
    let side = side(full);
    let width = (full - 2.0 * side).clamp(0.0, max);
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

/// The column of a [`Page::fixed`]: the window's height less the room
/// above and below, which is less on a phone.
fn fixed_column<R>(ui: &mut Ui, max: f32, content: impl FnOnce(&mut Ui) -> R) -> R {
    let full = ui.max_rect();
    let side = side(full.width());
    let width = (full.width() - 2.0 * side).clamp(0.0, max);
    let (top, bottom) = if full.width() < PHONE {
        (12.0, 12.0)
    } else {
        (TOP, 20.0)
    };
    let rect = Rect::from_min_max(
        egui::pos2(full.center().x - width / 2.0, full.top() + top),
        egui::pos2(full.center().x + width / 2.0, full.bottom() - bottom),
    );
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
        |ui| {
            ui.set_min_size(rect.size());
            content(ui)
        },
    )
    .inner
}

/// The page's title and the line under it saying what it holds. An empty
/// `description` leaves the line out.
pub fn header(ui: &mut Ui, tokens: &Tokens, title: &str, description: &str) {
    header_with(ui, tokens, title, description, |_| {});
}

/// [`header`] with what acts on the whole page at its right, as the
/// buttons that start and stop a machine. `actions` lays them out from the
/// right, so the first one added is the rightmost, on a phone too.
pub fn header_with<R>(
    ui: &mut Ui,
    tokens: &Tokens,
    title: &str,
    description: &str,
    actions: impl FnOnce(&mut Ui) -> R,
) -> R {
    let left = ui.cursor().left();
    let width = ui.available_width();
    if width < PHONE {
        // A phone has no room beside the title: the buttons go under it,
        // in the order they have beside it.
        header_text(ui, tokens, title, description, width);
        // One button tall, so centring them does not reach down the page;
        // a row that wraps grows below it.
        let row = egui::vec2(width, 36.0);
        let inner = ui
            .allocate_ui_with_layout(
                row,
                egui::Layout::right_to_left(egui::Align::Center).with_main_wrap(true),
                actions,
            )
            .inner;
        ui.add_space(12.0);
        return inner;
    }
    // The title wraps before it reaches the buttons; they keep what they
    // need and the title the rest.
    let text = (width * 0.62).max(width - 320.0);
    let block = header_text(ui, tokens, title, description, text);
    // Beside the title and its line, centred on them, without taking room
    // under them as a horizontal row would.
    let beside = Rect::from_min_max(
        egui::pos2(left + text, block.top()),
        egui::pos2(left + width, block.bottom()),
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(beside)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    let inner = actions(&mut child);
    ui.add_space(12.0);
    inner
}

/// The title and description of a [`header`], `width` at most.
fn header_text(ui: &mut Ui, tokens: &Tokens, title: &str, description: &str, width: f32) -> Rect {
    ui.vertical(|ui| {
        ui.set_max_width(width);
        ui.add(
            egui::Label::new(
                RichText::new(title)
                    .font(egui::FontId::proportional(26.0))
                    .strong()
                    .color(tokens.foreground),
            )
            .wrap(),
        );
        if !description.is_empty() {
            ui.add_space(-4.0);
            ui.add(
                egui::Label::new(
                    RichText::new(description)
                        .font(egui::FontId::proportional(14.0))
                        .color(tokens.muted_foreground),
                )
                .wrap(),
            );
        }
    })
    .response
    .rect
}

/// A heading inside a page, over the cards of one part of it.
pub fn subheading(ui: &mut Ui, tokens: &Tokens, text: &str) {
    ui.add_space(8.0);
    ui.label(
        RichText::new(text)
            .font(egui::FontId::proportional(18.0))
            .strong()
            .color(tokens.foreground),
    );
}

/// What scrolls inside a [`Page::fixed`] column, under its header: its
/// content keeps clear of the scroll bar, so the bar never runs over a
/// card's edge.
pub fn scroll<R>(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    egui::ScrollArea::vertical()
        .id_salt(id)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_max_width(ui.available_width() - GUTTER);
            add(ui)
        })
        .inner
}

/// Room between a scrolled column's content and its scroll bar.
pub const GUTTER: f32 = 14.0;

/// A card holding a group of rows.
pub fn card<R>(ui: &mut Ui, tokens: &Tokens, content: impl FnOnce(&mut Ui) -> R) -> R {
    Card::new().show(ui, tokens, content)
}

/// A card, with the margins and height a page asks of it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Card {
    tight: bool,
    fill: bool,
}

impl Card {
    /// A card of rows, as [`card`] draws.
    pub const fn new() -> Self {
        Self {
            tight: false,
            fill: false,
        }
    }

    /// Small margins, for a table or list whose rows fill to the card's
    /// edges.
    pub const fn tight(mut self) -> Self {
        self.tight = true;
        self
    }

    /// The card takes the rest of the column's height, for a listing that
    /// scrolls inside it.
    pub const fn fill(mut self) -> Self {
        self.fill = true;
        self
    }

    /// Draws the card with `content` in it.
    pub fn show<R>(self, ui: &mut Ui, tokens: &Tokens, content: impl FnOnce(&mut Ui) -> R) -> R {
        let phone = ui.available_width() < PHONE;
        let margin = match (self.tight, phone) {
            (true, _) => Margin::same(6),
            (false, true) => Margin::symmetric(14, 14),
            (false, false) => Margin::symmetric(24, 20),
        };
        egui::Frame::new()
            .fill(tokens.card)
            .stroke(tokens.border_stroke())
            .corner_radius(tokens.card_radius())
            .inner_margin(margin)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                if self.fill {
                    ui.set_min_height(ui.available_height());
                }
                content(ui)
            })
            .inner
    }
}

/// A card that is one target, as an app's tile in the Store: `size`, its
/// border the ring while focused and its fill the hover colour under the
/// pointer. `content` is laid out on it, inside its margin.
pub fn tile(
    ui: &mut Ui,
    tokens: &Tokens,
    size: egui::Vec2,
    label: &str,
    content: impl FnOnce(&mut Ui),
) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    if ui.is_rect_visible(rect) {
        let fill = if response.hovered() {
            tokens.hover
        } else {
            tokens.card
        };
        let stroke = if response.has_focus() {
            tokens.ring_stroke()
        } else {
            tokens.border_stroke()
        };
        ui.painter()
            .rect(rect, tokens.card_radius(), fill, stroke, StrokeKind::Inside);
        // Over what is painted, without moving the parent's cursor as a
        // scope would: the next tile still starts after this one.
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect.shrink2(egui::vec2(14.0, 10.0)))
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        content(&mut child);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A clickable row in a card's list, `height` tall and as wide as the
/// card, filled as a sidebar's page is: the accent's tint while selected,
/// the hover colour under the pointer.
pub fn list_row(ui: &mut Ui, tokens: &Tokens, height: f32, selected: bool) -> (Rect, Response) {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::click());
    if ui.is_rect_visible(rect) {
        let fill = row_fill(tokens, selected, response.hovered());
        ui.painter()
            .rect_filled(rect, tokens.control_radius(), fill);
    }
    (rect, response)
}

/// Lays out a card's rows as Settings does: a column of labels, then each
/// row's control. Call `end_row` after each.
pub fn rows<R>(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    let (label, gap) = label_width(ui.available_width());
    egui::Grid::new(id)
        .num_columns(2)
        .min_col_width(label)
        .spacing([gap, 18.0])
        .show(ui, add)
        .inner
}

/// A one-line text field as tall as the buttons beside it, as Settings'
/// fields are.
pub fn field(text: &mut dyn egui::TextBuffer) -> egui::TextEdit<'_> {
    egui::TextEdit::singleline(text).margin(Margin::symmetric(8, 7))
}

/// A note under a row, in the muted colour the theme gives descriptions.
pub fn note(tokens: &Tokens, text: &str) -> RichText {
    RichText::new(text)
        .font(tokens.small_font())
        .color(tokens.muted_foreground)
}

/// What a page shows while it has nothing to: a line in the middle of a
/// card saying why, and what to do about it.
pub fn empty(ui: &mut Ui, tokens: &Tokens, title: &str, text: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space(24.0);
        ui.label(
            RichText::new(title)
                .font(egui::FontId::proportional(17.0))
                .strong()
                .color(tokens.foreground),
        );
        if !text.is_empty() {
            ui.label(note(tokens, text));
        }
        ui.add_space(24.0);
    });
}
