//! egui rendering of the shell surfaces.
//!
//! The compositor renders three passes per output:
//!
//! 1. [`ShellUi::paint_decorations`] below client surfaces (title bars).
//! 2. Client surfaces, at [`crate::shell::WindowPlacement::client`].
//! 3. [`ShellUi::show`] above them: top bar with global menu and tray, snap
//!    preview and Snap Assist, the overview with widgets, the command
//!    palette, and the startup animation. On phones the top bar becomes a
//!    status bar and a navigation bar runs along the bottom edge (see
//!    [`crate::mobile`]). While the screen is locked, [`show_lock`] draws
//!    the lock screen instead, over the whole output, and [`show_polkit`]
//!    draws polkit's authentication dialog over everything else.
//!
//! Logical compositor pixels map 1:1 to egui points; set
//! `pixels_per_point` to the output scale.

use std::{cell::RefCell, collections::HashMap, sync::Arc};

use derisk_settings::BarPosition;
use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Id, Key, Layout, Modifiers, Order, Painter, Pos2,
    Rect, RichText, Sense, Stroke, StrokeKind, TextureHandle, TextureId, TextureOptions, Ui,
    UiBuilder, accesskit::Role, epaint::RectShape, pos2, vec2,
};
use mcsapi::{Geometry, WindowId, toolkit::egui, widgets::Theme};

use crate::{
    action::Action,
    animation::{DOT, LoginFrame, StartupAnimation, StartupFrame, arc, ease, f, stagger},
    apps::{AppLook, Apps},
    conversation::{Source, StepStatus, Turn},
    decorations::Button,
    effects::{BlurArea, Look},
    geom::inset,
    greetd::{Login, Phase},
    icons,
    keyboard::{Key as OskKey, Keyboard, Output as OskOutput, Shift},
    lock::LockScreen,
    menu::{Menu, MenuEntry},
    mobile::{self, NavState},
    motion,
    overview::{Battery, OverviewLayout, fit, grid, miniature, row},
    palette::{self, Category, Entry, History},
    shell::{DropTarget, Shell, WindowPlacement},
    systemd::SessionOp,
    time::Clock,
    widgets::Board,
};
use derisk_plugin::widget::{Card, Emphasis, Inline, Part, Text, Tone};

/// Converts a logical geometry to an egui rectangle.
pub fn to_rect(g: Geometry) -> Rect {
    Rect::from_min_size(
        pos2(g.loc.x as f32, g.loc.y as f32),
        vec2(g.size.w as f32, g.size.h as f32),
    )
}

/// Gives a widget its role and name for screen readers and agents. egui
/// names widgets after their text, which for the chrome is often a symbol
/// (◆, ✨, ⚠) or nothing at all for areas drawn with the painter.
fn name(ui: &Ui, response: &egui::Response, role: Role, label: impl Into<String>) {
    let label = label.into();
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_role(role);
        node.set_label(label);
    });
}

fn button_color(button: Button) -> Color32 {
    match button {
        Button::Close => Color32::from_rgb(239, 68, 68),
        Button::Minimize => Color32::from_rgb(245, 158, 11),
        Button::Maximize => Color32::from_rgb(34, 197, 94),
    }
}

/// Decoded app icons by icon theme generation and icon name, `None` for
/// names no theme has.
type AppIcons = RefCell<HashMap<(u64, String), Option<Arc<egui::ColorImage>>>>;

/// The battery as its symbolic icon and percentage.
fn battery(ui: &mut Ui, b: Battery, color: Color32) -> egui::Response {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        let icon = icons::battery(b.percent, b.charging);
        let text = RichText::new(format!("{}%", b.percent)).color(color);
        // In the top bar's right-to-left half the first widget lands
        // rightmost, so the icon goes second there to read "▮ 80%".
        if ui.layout().prefer_right_to_left() {
            ui.label(text);
            icons::show(ui, &icon, 16.0, color);
        } else {
            icons::show(ui, &icon, 16.0, color);
            ui.label(text);
        }
    })
    .response
}

/// Paints an app's icon into `r`: the icon theme's, else the app's
/// glyph, else its initial on an accent tile, so every window has one.
fn paint_app_icon(app_icons: &AppIcons, theme: &Theme, painter: &Painter, r: Rect, look: &AppLook) {
    // Keyed by the icon theme too, so changing it shows its icons.
    let generation = icons::generation();
    let image = app_icons
        .borrow_mut()
        .entry((generation, look.icon.to_owned()))
        .or_insert_with(|| icons::load(&icons::find(look.icon, ICON_PX)?, ICON_PX).map(Arc::new))
        .clone();
    // Title bars and the chrome are separate egui contexts with their own
    // textures, so each context uploads the icon once and keeps it.
    let texture = image.map(|image| {
        let ctx = painter.ctx();
        let id = Id::new(("derisk-app-icon", generation, look.icon));
        ctx.data(|d| d.get_temp::<TextureHandle>(id))
            .unwrap_or_else(|| {
                let texture = ctx.load_texture(
                    format!("derisk-app-icon:{}", look.icon),
                    image,
                    TextureOptions::LINEAR,
                );
                ctx.data_mut(|d| d.insert_temp(id, texture.clone()));
                texture
            })
    });
    if let Some(texture) = texture {
        let [w, h] = texture.size().map(|n| n.max(1) as f32);
        let scale = r.width().min(r.height()) / w.max(h);
        painter.image(
            texture.id(),
            Rect::from_center_size(r.center(), vec2(w, h) * scale),
            Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    } else if !look.glyph.is_empty() {
        painter.text(
            r.center(),
            Align2::CENTER_CENTER,
            look.glyph,
            FontId::proportional(r.height() * 0.8),
            theme.foreground,
        );
    } else {
        painter.rect_filled(r, r.height() * 0.25, theme.accent);
        let initial: String = look
            .name
            .chars()
            .take(1)
            .flat_map(char::to_uppercase)
            .collect();
        painter.text(
            r.center(),
            Align2::CENTER_CENTER,
            initial,
            FontId::proportional(r.height() * 0.6),
            theme.background,
        );
    }
}

fn elide(text: &str, width: f32, size: f32) -> String {
    let max = ((width / (size * 0.55)) as usize).max(1);
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let mut s: String = text.chars().take(max.saturating_sub(1)).collect();
        s.push('…');
        s
    }
}

/// Width of the accent ring around the focused window, in logical pixels.
pub const FOCUS_RING: f32 = 2.0;

/// How far below its place an overview card starts rising in.
const RISE: f32 = 40.0;

fn card<R>(ui: &mut Ui, theme: &Theme, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(theme.surface)
        .stroke(Stroke::new(1.0, theme.border))
        .corner_radius(12)
        .inner_margin(12)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// What every window on screen shows, for the overview's cards to draw
/// instead of an icon. The compositor host hands it over each frame the
/// overview is open (mcsapi-compositor's `WindowPictures`).
pub struct WindowPictures {
    /// The screen with its windows drawn and nothing over them.
    pub texture: TextureId,
    /// Where an area of the screen (logical) lies in the texture.
    pub uv: Box<dyn Fn(Rect) -> Rect>,
}

/// Persistent UI state for the shell chrome.
pub struct ShellUi {
    /// Shared colors (from mcsapi).
    pub theme: Theme,
    /// The startup animation.
    pub startup: StartupAnimation,
    /// What is typed in widgets' text fields, by plugin, card and field,
    /// kept for the session; the plugins never see it.
    fields: HashMap<(String, String, String), String>,
    /// The overview's cards, from the widget plugins.
    board: Board,
    overview_open: bool,
    /// When the overview last opened (ms on [`motion::now`]'s clock).
    overview_since: f64,
    /// The window card pressed in the overview, and since when.
    pressed: Option<(WindowId, f64)>,
    /// A window picked in the overview and the card it was picked from,
    /// for the host to grow the window out of ([`ShellUi::take_picked`]).
    picked: Option<(WindowId, Rect)>,
    /// That card, fading off the growing window, and since when.
    grown: Option<(WindowId, Rect, f64)>,
    /// The overview's widget cards last frame, for the cascade.
    widget_cards: usize,
    /// When the widget card being drawn finished rising.
    card_risen: f64,
    /// What the windows show, while the host provides it.
    pub window_pictures: Option<WindowPictures>,
    /// Window being dragged in the overview, toward a workspace.
    overview_drag: Option<WindowId>,
    tray: Option<(u64, Vec<TextureHandle>)>,
    /// The command palette.
    pub palette: PaletteUi,
    reply: Option<String>,
    reduced_motion: bool,
    look: Look,
    blurs: Vec<BlurArea>,
    /// Whether the touch-sized style is applied to the egui context.
    touch_style: bool,
    /// Where a drag on the navigation bar started.
    swipe_from: Option<Pos2>,
    /// The on-screen keyboard (phones only).
    pub keyboard: Keyboard,
    /// Whether the keyboard showed last frame, to start it fresh each time
    /// it comes up.
    keyboard_shown: bool,
    /// Keyboard output for the chrome's own text fields, fed in next frame.
    chrome_input: Vec<egui::Event>,
    /// Keyboard output for the focused window, for the host to deliver.
    window_input: Vec<OskOutput>,
    /// When a held Backspace repeats next.
    backspace_repeat: Option<f64>,
    /// How far an auto-hidden top bar is revealed, 0 (hidden) to 1.
    bar_shown: f32,
    /// Where the top bar took the pointer in the last frame; `None` while it
    /// is hidden.
    bar_rect: Option<Rect>,
    /// Where the top bar's search field was drawn last frame.
    search_rect: Option<Rect>,
    /// Move keyboard focus into the top bar next frame (Super+B).
    focus_bar: bool,
    /// Decoded app icons by theme name or path, `None` when the theme has
    /// none. Behind a `RefCell` because title bars paint through `&self`.
    icons: AppIcons,
    /// The dot a finished request folded the palette into, on its way to
    /// the focused window: when the fold began (ms on [`motion::now`]'s
    /// clock), and where the dot is.
    relay: Option<(f64, Pos2)>,
    /// The window the focus ring is around and when its ring started
    /// tracing in; a newly focused window's ring traces itself in.
    ring: (Option<WindowId>, f64),
}

/// Pixels app icons are decoded at: crisp up to 32 points at 2x, the largest
/// the shell draws them on a HiDPI screen.
const ICON_PX: u32 = 64;

/// Command palette state. The host fills [`PaletteUi::apps`] and the files
/// ([`PaletteUi::set_files`]); the rest is kept between openings.
#[derive(Default)]
pub struct PaletteUi {
    /// Installed apps, for the apps plugin.
    pub apps: Vec<palette::App>,
    /// Indexed files (see [`palette::index_files`]), for the files plugin.
    files: Vec<palette::File>,
    /// The plugins' rows, kept until what they read changes.
    catalog: palette::Catalog,
    /// What was chosen before, for ranking.
    pub history: History,
    /// Text to start the next opening with, instead of an empty query.
    pub preset: Option<String>,
    /// Open on the conversation next time instead of the search.
    pub chat_next: bool,
    chat: bool,
    seen: u64,
    cleared: u64,
    asks: Vec<Ask>,
    query: String,
    shown_query: String,
    selected: usize,
    armed: Option<String>,
    message: Option<String>,
    open: bool,
    /// When it opened (ms on [`motion::now`]'s clock), and where from: it
    /// grows out of the top bar's search field.
    opened: Option<(f64, Option<Rect>)>,
    /// Where the card was drawn last frame.
    rect: Option<Rect>,
    /// When each character of the query was typed; the newest flash in
    /// the accent.
    typed: Vec<f64>,
    /// The newest turn when a request was sent from here: the next one is
    /// that request's, which folds the palette away once it is done.
    asked_after: Option<u64>,
    /// When that request was done.
    done_at: Option<f64>,
    /// The newest turn when it opened: newer ones animate in.
    opened_turn: u64,
    /// When it began folding into the dot, and the card it folds from.
    fold: Option<(f64, Rect)>,
}

impl PaletteUi {
    /// Replaces the indexed files. The host re-indexes each time the
    /// palette opens; when nothing changed, the files plugin is not asked
    /// again, which for a full index is the one call long enough to notice.
    pub fn set_files(&mut self, files: Vec<palette::File>) {
        if files != self.files {
            self.files = files;
            self.catalog.files_changed();
        }
    }
}

/// A key press as a hardware keyboard would send it.
fn key_event(key: Key) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }
}

/// Rows the palette shows at once before scrolling.
const PALETTE_ROWS: usize = 9;
const PALETTE_ROW_HEIGHT: f32 = 40.0;

/// Sizes egui's own widgets (buttons, check boxes, text fields, menus) for
/// fingers on phones, and back to egui's defaults elsewhere. Used for the
/// chrome and for the in-process apps. Does nothing when already applied,
/// so it can run every frame.
pub fn set_touch_style(ctx: &egui::Context, touch: bool) {
    let default = egui::style::Spacing::default();
    let target = mobile::TOUCH_TARGET as f32;
    let current = ctx.global_style().spacing.interact_size;
    if current
        == if touch {
            vec2(target, target)
        } else {
            default.interact_size
        }
    {
        return;
    }
    ctx.all_styles_mut(|style| {
        let spacing = &mut style.spacing;
        if touch {
            spacing.interact_size = vec2(target, target);
            spacing.button_padding = vec2(12.0, 8.0);
            spacing.item_spacing = vec2(10.0, 8.0);
            spacing.icon_width = 22.0;
            spacing.icon_width_inner = 12.0;
        } else {
            spacing.interact_size = default.interact_size;
            spacing.button_padding = default.button_padding;
            spacing.item_spacing = default.item_spacing;
            spacing.icon_width = default.icon_width;
            spacing.icon_width_inner = default.icon_width_inner;
        }
    });
}

fn limit_palette_hits(entries: &[Entry], hits: &[usize], limit: usize) -> Vec<usize> {
    if hits.len() <= limit {
        return hits.to_vec();
    }

    let mut categories: Vec<(Category, Vec<usize>)> = Vec::new();
    for &hit in hits {
        let category = entries[hit].category;
        if let Some((_, category_hits)) = categories
            .iter_mut()
            .find(|(existing, _)| *existing == category)
        {
            category_hits.push(hit);
        } else {
            categories.push((category, vec![hit]));
        }
    }

    let base_quota = limit / categories.len();
    let extra_quota = limit % categories.len();
    let mut included = vec![false; entries.len()];
    let mut included_count = 0;
    for (index, (_, category_hits)) in categories.iter().enumerate() {
        let quota = base_quota + usize::from(index < extra_quota);
        for &hit in category_hits.iter().take(quota) {
            included[hit] = true;
            included_count += 1;
        }
    }
    for &hit in hits {
        if included_count == limit {
            break;
        }
        if !included[hit] {
            included[hit] = true;
            included_count += 1;
        }
    }

    categories
        .into_iter()
        .flat_map(|(_, category_hits)| category_hits.into_iter().filter(|&hit| included[hit]))
        .collect()
}

impl ShellUi {
    /// Creates UI state; `reduced_motion` shortens the startup animation, as
    /// do the reduce motion setting and low power mode.
    pub fn new(shell: &Shell, reduced_motion: bool) -> Self {
        Self {
            theme: Theme::default(),
            startup: StartupAnimation {
                reduced_motion,
                bar_height: shell.profile().top_bar as f32,
            },
            fields: HashMap::new(),
            board: Board::default(),
            overview_open: false,
            overview_since: 0.0,
            pressed: None,
            picked: None,
            grown: None,
            widget_cards: 0,
            card_risen: 0.0,
            window_pictures: None,
            overview_drag: None,
            tray: None,
            palette: PaletteUi::default(),
            reply: None,
            reduced_motion,
            look: shell.look(),
            blurs: Vec::new(),
            touch_style: false,
            swipe_from: None,
            keyboard: Keyboard::new(),
            keyboard_shown: false,
            chrome_input: Vec::new(),
            window_input: Vec::new(),
            backspace_repeat: None,
            bar_shown: 1.0,
            bar_rect: None,
            focus_bar: false,
            search_rect: None,
            icons: RefCell::default(),
            relay: None,
            // What is focused already is ringed from the start.
            ring: (
                shell
                    .placements()
                    .into_iter()
                    .find(|p| p.focused)
                    .map(|p| p.window),
                f64::NEG_INFINITY,
            ),
        }
    }

    /// The window picked in the overview since the last call, and the card
    /// it was picked from: the host draws the window growing out of that
    /// card into its frame (54 frames, the film's camera curve).
    pub fn take_picked(&mut self) -> Option<(WindowId, Rect)> {
        self.picked.take()
    }

    /// After a window is picked in the overview, the rest of the overview
    /// fades back (40 frames, expo-out) and the picked card fades off the
    /// window growing under it (from 2 to 18 frames), as the film's card
    /// grows into the phone.
    fn grow(&mut self, ui: &Ui, shell: &Shell) {
        let Some((window, card, at)) = self.grown else {
            return;
        };
        let ctx = ui.ctx();
        let back = motion::since(ctx, at, f(40), ease::OUT);
        let Some(p) = shell.placements().into_iter().find(|p| p.window == window) else {
            self.grown = None;
            return;
        };
        if back >= 1.0 {
            self.grown = None;
            return;
        }
        let grown = motion::since(ctx, at, f(54), ease::CAM);
        let r = motion::lerp_rect(card, to_rect(p.frame), grown);
        let painter = ctx.layer_painter(egui::LayerId::new(
            Order::Background,
            Id::new("derisk-overview-grow"),
        ));
        // What is left of the overview's backdrop, around the window.
        let dim = self
            .theme
            .background
            .gamma_multiply(self.look.overview * (1.0 - back));
        let screen = to_rect(shell.work_area());
        for side in [
            Rect::from_min_max(screen.min, pos2(screen.max.x, r.top())),
            Rect::from_min_max(pos2(screen.min.x, r.bottom()), screen.max),
            Rect::from_min_max(pos2(screen.min.x, r.top()), pos2(r.left(), r.bottom())),
            Rect::from_min_max(pos2(r.right(), r.top()), pos2(screen.max.x, r.bottom())),
        ] {
            if side.is_positive() {
                painter.rect_filled(side, 0, dim);
            }
        }
        let card_out = motion::since(ctx, at + f64::from(f(2)), f(16), ease::SMOOTH);
        if card_out < 1.0 {
            let mut painter = painter.clone();
            painter.multiply_opacity(1.0 - card_out);
            painter.rect_filled(r, 12, self.theme.surface);
            let (app, title) = shell.window_label(window).unwrap_or_default();
            let look = shell.apps.look(app);
            self.paint_window_card(&painter, r, &look, title);
        }
    }

    /// Whether the shell animates: not when reduced motion was asked for,
    /// nor when the Look setting (or low power mode) turns animation off.
    pub fn animates(&self, shell: &Shell) -> bool {
        !self.reduced_motion && shell.look().animate
    }

    /// Paints an app's icon into `r`; see [`paint_app_icon`].
    fn paint_app_icon(&self, painter: &Painter, r: Rect, look: &AppLook) {
        paint_app_icon(&self.icons, &self.theme, painter, r, look);
    }

    /// Paints an app icon and `text` on one line, the pair centered on
    /// `center` and elided to `width`.
    #[allow(clippy::too_many_arguments)]
    fn paint_icon_label(
        &self,
        painter: &Painter,
        center: Pos2,
        width: f32,
        look: &AppLook,
        text: &str,
        size: f32,
        color: Color32,
    ) {
        let icon = (size * 1.3).round();
        let gap = (size * 0.45).round();
        let galley = painter.layout_no_wrap(
            elide(text, width - icon - gap, size),
            FontId::proportional(size),
            color,
        );
        let left = center.x - (icon + gap + galley.size().x) / 2.0;
        self.paint_app_icon(
            painter,
            Rect::from_min_size(pos2(left, center.y - icon / 2.0), vec2(icon, icon)),
            look,
        );
        painter.galley(
            pos2(left + icon + gap, center.y - galley.size().y / 2.0),
            galley,
            color,
        );
    }

    /// A window card's label (overview, Snap Assist): the app icon, the app
    /// name under it, and the window title under that when there is one.
    fn paint_window_card(&self, painter: &Painter, r: Rect, look: &AppLook, title: &str) {
        let icon = (r.height() * 0.3).clamp(16.0, 48.0);
        let name = (icon * 0.4).clamp(12.0, 18.0);
        let top = r.center().y - (icon + 6.0 + name + 4.0 + name * 0.75) / 2.0;
        self.paint_app_icon(
            painter,
            Rect::from_center_size(pos2(r.center().x, top + icon / 2.0), vec2(icon, icon)),
            look,
        );
        let name_y = top + icon + 6.0 + name / 2.0;
        painter.text(
            pos2(r.center().x, name_y),
            Align2::CENTER_CENTER,
            elide(&look.name, r.width() - 16.0, name),
            FontId::proportional(name),
            self.theme.foreground,
        );
        // The title only adds something when it is not just the name again.
        if !title.is_empty() && !title.eq_ignore_ascii_case(&look.name) {
            painter.text(
                pos2(r.center().x, name_y + name / 2.0 + 4.0 + name * 0.375),
                Align2::CENTER_CENTER,
                elide(title, r.width() - 16.0, name * 0.75),
                FontId::proportional(name * 0.75),
                self.theme.border,
            );
        }
    }

    /// A window picture's label: the app icon and name on a pill over the
    /// picture's lower edge, where it reads on any content.
    fn paint_picture_label(&self, painter: &Painter, r: Rect, look: &AppLook) {
        let size = 13.0;
        let height = 28.0;
        let width = (r.width() - 16.0).min(220.0);
        let pill = Rect::from_center_size(
            pos2(r.center().x, r.bottom() - 8.0 - height / 2.0),
            vec2(width, height),
        );
        painter.rect_filled(pill, height / 2.0, self.theme.surface.gamma_multiply(0.92));
        self.paint_icon_label(
            painter,
            pill.center(),
            width - 16.0,
            look,
            &look.name,
            size,
            self.theme.foreground,
        );
    }

    /// What the on-screen keyboard typed for the focused window since the
    /// last call (text for the chrome's own fields goes there directly).
    pub fn take_window_input(&mut self) -> Vec<OskOutput> {
        std::mem::take(&mut self.window_input)
    }

    /// Where the top bar was drawn by the last [`ShellUi::show`]; `None`
    /// while auto-hide keeps it off screen. Hosts give it the pointer there.
    pub fn bar_rect(&self) -> Option<Rect> {
        self.bar_rect
    }

    /// Moves keyboard focus to the top bar's first button on the next
    /// frame, so it can be used without a pointer: Tab and Shift+Tab walk
    /// it, Enter or Space press, Escape returns to the window.
    pub fn focus_top_bar(&mut self) {
        self.focus_bar = true;
    }

    /// Areas to blur under the translucent panels shown by the last
    /// [`ShellUi::show`], bottom to top. Empty when blur is off.
    pub fn blur_regions(&self) -> &[BlurArea] {
        &self.blurs
    }

    /// The startup animation's frame `elapsed_ms` after start. Low power mode
    /// skips the animation; reduced motion turns it into a cross-fade.
    pub fn startup_frame(&self, elapsed_ms: u32) -> StartupFrame {
        if self.look.low_power {
            StartupFrame::DONE
        } else {
            self.startup.frame(elapsed_ms)
        }
    }

    /// Records a panel filled with the background at `opacity` so the
    /// compositor blurs behind it. Nothing shows through an opaque or
    /// invisible panel, so those aren't blurred.
    fn frost(&mut self, area: Rect, corner_radius: u8, opacity: f32) {
        let opacity = opacity * f32::from(self.theme.background.a()) / 255.0;
        if self.look.blur == 0 || opacity <= 0.0 || opacity >= 1.0 {
            return;
        }
        let area = Geometry::new(
            (area.min.x.round() as i32, area.min.y.round() as i32).into(),
            (area.width().round() as i32, area.height().round() as i32).into(),
        );
        self.blurs.push(BlurArea {
            area,
            corner_radius,
            strength: self.look.blur,
        });
    }

    /// The assistant's last reply, if any.
    pub fn assistant_reply(&self) -> Option<&str> {
        self.reply.as_deref()
    }

    /// Paints server-side title bars, buttons on the left, bottom to top.
    pub fn paint_decorations(&self, painter: &Painter, shell: &Shell) {
        for p in shell.placements() {
            self.paint_decoration(painter, shell, &p);
        }
    }

    /// Paints one window's title bar and border.
    ///
    /// Hosts that interleave decorations with client surfaces (so a window
    /// above covers the title bar of one below) call this per placement.
    pub fn paint_decoration(&self, painter: &Painter, shell: &Shell, p: &WindowPlacement) {
        let bar = shell.profile().title_bar;
        // Phones have no title bars; a full-screen app needs no border.
        if bar.height == 0 {
            return;
        }
        let theme = &self.theme;
        // A window that fills the work area meets the screen edges and the
        // top bar: an outline, rounded corners or a shadow there would only
        // frame the screen and show slivers of wallpaper in the corners.
        let fills = p.frame == shell.work_area();
        // Client surfaces are drawn square by the compositor, so only the
        // title bar's top corners round.
        let r = if fills {
            0
        } else {
            shell.effects.windows.corner_radius.min(20)
        };
        let radius = CornerRadius {
            nw: r,
            ne: r,
            sw: 0,
            se: 0,
        };
        // A soft shadow separates overlapping windows.
        if shell.look().shadows && !fills {
            painter.add(
                egui::epaint::Shadow {
                    offset: [0, 6],
                    blur: if p.focused { 28 } else { 16 },
                    spread: 0,
                    color: Color32::from_black_alpha(if p.focused { 150 } else { 90 }),
                }
                .as_shape(to_rect(p.frame), radius),
            );
        }
        painter.rect_filled(
            to_rect(bar.bar(p.frame)),
            radius,
            if p.focused {
                theme.surface
            } else {
                theme.background
            },
        );
        if !fills {
            painter.rect_stroke(
                to_rect(p.frame),
                radius,
                Stroke::new(
                    1.0,
                    if p.focused {
                        theme.accent
                    } else {
                        theme.border
                    },
                ),
                StrokeKind::Inside,
            );
        }
        for (button, area) in bar.buttons(p.frame) {
            let r = to_rect(area);
            let name = match button {
                Button::Close => "window-close",
                Button::Minimize => "window-minimize",
                Button::Maximize => "window-maximize",
            };
            // Greyscale Papirus buttons on a faint disc.
            painter.circle_filled(
                r.center(),
                r.width() / 2.0,
                theme.border.gamma_multiply(0.35),
            );
            let icon_color = if p.focused {
                theme.foreground
            } else {
                theme.border
            };
            icons::paint(painter, r.shrink(r.width() * 0.15), name, icon_color);
        }
        let title_area = to_rect(bar.title(p.frame));
        let (app, title) = shell.window_label(p.window).unwrap_or_default();
        let look = shell.apps.look(app);
        let size = (bar.height as f32 * 0.42).max(11.0);
        self.paint_icon_label(
            painter,
            title_area.center(),
            title_area.width(),
            &look,
            if title.is_empty() { &look.name } else { title },
            size,
            theme.foreground,
        );
    }

    /// Rings the focused window in the accent, just outside its frame.
    ///
    /// The decoration's own border sits under the client surface, which
    /// covers all of it but the title bar, so the ring is chrome: drawn
    /// above every surface, under the bars and the palette. Phones and a
    /// window filling the work area have no frame to ring.
    ///
    /// When focus moves to another window its ring traces itself in, as
    /// the film's does: clockwise from the top-left corner, 40 frames,
    /// expo-out.
    fn focus_ring(&mut self, ui: &Ui, shell: &Shell) {
        let Some(p) = ringed(shell) else {
            return;
        };
        let ctx = ui.ctx();
        if self.ring.0 != Some(p.window) {
            self.ring = (Some(p.window), motion::now(ctx));
        }
        let traced = motion::since(ctx, self.ring.1, f(40), ease::OUT);
        if traced <= 0.0 {
            return;
        }
        let r = shell.effects.windows.corner_radius.min(20);
        let painter = ctx.layer_painter(egui::LayerId::new(
            Order::Background,
            Id::new("derisk-focus-ring"),
        ));
        let stroke = Stroke::new(FOCUS_RING, self.theme.accent);
        if traced < 1.0 {
            // The stroke's middle line, so it lands where the finished
            // ring is drawn.
            let half = FOCUS_RING / 2.0;
            let line = motion::outline(to_rect(p.frame).expand(half), f32::from(r) + half, traced);
            painter.add(egui::Shape::line(line, stroke));
            return;
        }
        painter.rect_stroke(
            to_rect(p.frame),
            CornerRadius {
                nw: r,
                ne: r,
                sw: 0,
                se: 0,
            },
            stroke,
            StrokeKind::Outside,
        );
    }

    /// The dot a finished request folded the palette into carries the
    /// result to the focused window, as the film's does: it waits where the
    /// palette was, flies along an arc to the window's top-left corner
    /// (24 to 90 frames after the fold began, settling at rest), and the
    /// window's ring traces in from there (see [`ShellUi::focus_ring`]).
    fn relay(&mut self, ui: &Ui, shell: &Shell) {
        let Some((at, from)) = self.relay else {
            return;
        };
        let Some(p) = ringed(shell) else {
            self.relay = None;
            return;
        };
        let ctx = ui.ctx();
        let arrives = at + f64::from(f(90));
        // The ring waits for the dot.
        self.ring = (Some(p.window), arrives);
        let flown = motion::since(ctx, at + f64::from(f(24)), f(66), ease::REST);
        if !motion::enabled(ctx) || motion::now(ctx) >= arrives {
            self.relay = None;
            return;
        }
        let corner = to_rect(p.frame).min + vec2(2.0, 2.0);
        let (x, y) = arc((from.x, from.y), (corner.x, corner.y), flown, 0.16);
        ctx.layer_painter(egui::LayerId::new(
            Order::Foreground,
            Id::new("derisk-relay"),
        ))
        .circle_filled(pos2(x, y), DOT / 2.0, self.theme.accent);
    }

    /// Shows the chrome above client surfaces and returns requested actions.
    ///
    /// `elapsed_ms` is time since the session started (drives the startup
    /// animation). Pass the root `Ui` from `Context::run_ui`.
    pub fn show(&mut self, ui: &mut Ui, shell: &Shell, elapsed_ms: u32) -> Vec<Action> {
        let mut actions = Vec::new();
        self.look = shell.look();
        self.blurs.clear();
        self.startup.reduced_motion = self.reduced_motion || !self.look.animate;
        motion::set_enabled(ui.ctx(), !self.startup.reduced_motion);
        let frame = self.startup_frame(elapsed_ms);
        let phone = shell.is_phone();
        if phone != self.touch_style {
            set_touch_style(ui.ctx(), phone);
            self.touch_style = phone;
        }
        // What the on-screen keyboard typed last frame reaches the chrome's
        // text fields as if typed on a hardware keyboard.
        if !self.chrome_input.is_empty() {
            let events = std::mem::take(&mut self.chrome_input);
            ui.ctx().input_mut(|i| i.events.extend(events));
        }
        if shell.keyboard_visible() != self.keyboard_shown
            || shell.palette_visible() != self.palette.open
        {
            self.keyboard_shown = shell.keyboard_visible();
            self.keyboard.reset();
        }
        self.relay(ui, shell);
        if !shell.overview_visible() {
            self.focus_ring(ui, shell);
        }
        self.drag_preview(ui, shell);
        self.snap_assist(ui, shell, &mut actions);
        if shell.overview_visible() {
            self.overview(ui, shell, &mut actions);
        }
        self.overview_open = shell.overview_visible();
        if !self.overview_open {
            self.overview_drag = None;
            self.pressed = None;
            self.grow(ui, shell);
        }
        if phone {
            self.status_bar(ui, shell, frame, &mut actions);
            self.nav_bar(ui, shell, frame, &mut actions);
        } else {
            self.top_bar(ui, shell, frame, &mut actions);
        }
        if shell.palette_visible() {
            self.palette(ui, shell, &mut actions);
        }
        self.palette.open = shell.palette_visible();
        if let Some(op) = shell.pending_confirmation() {
            self.confirmation(ui, shell, op, &mut actions);
        }
        self.on_screen_keyboard(ui, shell);
        if !frame.done {
            let painter = ui.ctx().layer_painter(egui::LayerId::new(
                Order::Foreground,
                Id::new("derisk-startup"),
            ));
            paint_startup(
                &painter,
                to_rect(shell.output()),
                frame,
                &self.theme,
                self.search_rect,
            );
            ui.ctx().request_repaint();
        }
        // Nobody used the session for a while: fade the screen, over the
        // chrome as well, until the next key or pointer movement.
        let dim = ui
            .ctx()
            .animate_bool_with_time(Id::new("derisk-idle-dim"), shell.dimmed, 1.0);
        if dim > 0.0 {
            ui.ctx()
                .layer_painter(egui::LayerId::new(Order::Debug, Id::new("derisk-idle-dim")))
                .rect_filled(
                    to_rect(shell.output()),
                    0,
                    Color32::from_black_alpha((dim * 170.0) as u8),
                );
        }
        actions
    }

    /// Asks the person to confirm a session operation an agent or injected
    /// input requested. Only real keyboard and pointer input can accept
    /// (the shell checks); Cancel has focus, so a stray Enter cancels.
    fn confirmation(
        &mut self,
        ui: &mut Ui,
        shell: &Shell,
        op: SessionOp,
        actions: &mut Vec<Action>,
    ) {
        let (verb, button) = match op {
            SessionOp::Logout => ("log out", "Log out"),
            SessionOp::Reboot => ("restart the computer", "Restart"),
            SessionOp::PowerOff => ("power off the computer", "Power off"),
            SessionOp::Lock | SessionOp::Suspend | SessionOp::Hibernate => {
                ("change the session", "Continue")
            }
        };
        let screen = to_rect(shell.output());
        ui.ctx()
            .layer_painter(egui::LayerId::new(
                Order::Foreground,
                Id::new("derisk-confirm-dim"),
            ))
            .rect_filled(screen, 0, Color32::from_black_alpha(140));
        // Above the dimming, which shares the palette's layer order.
        egui::Area::new(Id::new("derisk-confirm"))
            .order(Order::Tooltip)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ui.ctx(), |ui| {
                card(ui, &self.theme, |ui| {
                    ui.set_max_width(420.0);
                    let title = format!("An agent asked to {verb}");
                    ui.ctx().accesskit_node_builder(ui.unique_id(), |node| {
                        node.set_role(Role::AlertDialog);
                        node.set_label(title.clone());
                        node.set_modal();
                    });
                    ui.label(RichText::new(&title).size(18.0).strong());
                    ui.add_space(6.0);
                    ui.label(
                        "Unsaved work may be lost. Only you can allow this, with your own \
                         keyboard or mouse; clicks and keys from agents do not count.",
                    );
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        let cancel = ui.button("Cancel");
                        // Focus goes back to Cancel after anything an agent
                        // did, so it cannot leave the person's next Enter
                        // on the destructive button.
                        if shell.synthetic_input() || ui.ctx().memory(|m| m.focused().is_none()) {
                            cancel.request_focus();
                        }
                        if cancel.clicked() {
                            actions.push(Action::Confirm { accept: false });
                        }
                        let accept = ui.add(
                            egui::Button::new(RichText::new(button).color(Color32::WHITE))
                                .fill(Color32::from_rgb(185, 28, 28)),
                        );
                        if accept.clicked() {
                            actions.push(Action::Confirm { accept: true });
                        }
                    });
                });
            });
    }

    fn drag_preview(&self, ui: &Ui, shell: &Shell) {
        let preview = match shell.drag_target() {
            Some(DropTarget::Snap { preview, .. } | DropTarget::Tile { preview, .. }) => preview,
            _ => return,
        };
        let r = to_rect(preview);
        let painter = ui.painter();
        painter.rect_filled(r, 12, self.theme.accent.gamma_multiply(0.18));
        painter.rect_stroke(
            r,
            12,
            Stroke::new(2.0, self.theme.accent),
            StrokeKind::Inside,
        );
    }

    fn snap_assist(&mut self, ui: &mut Ui, shell: &Shell, actions: &mut Vec<Action>) {
        let Some(assist) = shell.snap_assist() else {
            return;
        };
        self.frost(to_rect(assist.frame), 12, self.look.snap_assist);
        ui.painter().rect_filled(
            to_rect(assist.frame),
            12,
            self.theme.background.gamma_multiply(self.look.snap_assist),
        );
        let cells = grid(assist.candidates.len(), assist.frame, 16);
        for (window, cell) in assist.candidates.iter().zip(cells) {
            let r = to_rect(inset(cell, 8));
            let response = ui.interact(r, Id::new(("derisk-assist", window.get())), Sense::click());
            let (app, title) = shell.window_label(*window).unwrap_or_default();
            name(
                ui,
                &response,
                Role::Button,
                format!("Snap {}", if title.is_empty() { app } else { title }),
            );
            let stroke = if response.hovered() {
                self.theme.accent
            } else {
                self.theme.border
            };
            ui.painter().rect_filled(r, 10, self.theme.surface);
            ui.painter()
                .rect_stroke(r, 10, Stroke::new(1.5, stroke), StrokeKind::Inside);
            let (app, title) = shell.window_label(*window).unwrap_or_default();
            self.paint_window_card(ui.painter(), r, &shell.apps.look(app), title);
            if response.clicked() {
                actions.push(Action::Snap {
                    window: Some(window.get()),
                    zone: assist.zone,
                });
            }
        }
    }

    fn top_bar(
        &mut self,
        ui: &mut Ui,
        shell: &Shell,
        frame: StartupFrame,
        actions: &mut Vec<Action>,
    ) {
        let prefs = shell.effects.top_bar;
        let home = to_rect(shell.bar_area());
        let height = home.height();
        let bottom = prefs.position == BarPosition::Bottom;
        // Auto-hide: reveal while the pointer touches the bar's edge or is
        // over the bar, and while the overview, palette or startup show.
        let target = if prefs.autohide && frame.done {
            let output = to_rect(shell.output());
            let pointer = ui.ctx().input(|i| i.pointer.latest_pos());
            let at_edge = pointer.is_some_and(|p| {
                if bottom {
                    p.y >= output.bottom() - 2.0
                } else {
                    p.y <= output.top() + 1.0
                }
            });
            let over = pointer.is_some_and(|p| self.bar_rect.is_some_and(|r| r.contains(p)));
            let open = shell.overview_visible() || shell.palette_visible();
            if at_edge || over || open { 1.0 } else { 0.0 }
        } else {
            1.0
        };
        self.bar_shown = if self.look.animate {
            let step = ui.ctx().input(|i| i.stable_dt).min(0.1) / 0.15;
            if target > self.bar_shown {
                (self.bar_shown + step).min(target)
            } else {
                (self.bar_shown - step).max(target)
            }
        } else {
            target
        };
        if self.bar_shown != target {
            ui.ctx().request_repaint();
        }
        if self.bar_shown <= 0.0 {
            self.bar_rect = None;
            return;
        }
        // Slide away from the edge: up for a top bar, down for a bottom one.
        let hidden = height * (1.0 - self.bar_shown);
        let offset = if bottom {
            hidden - frame.bar_offset
        } else {
            frame.bar_offset - hidden
        };
        let bar = home.translate(vec2(0.0, offset));
        self.bar_rect = Some(bar);
        let opacity = frame.shell_opacity.max(0.0);
        self.frost(bar, 0, self.look.top_bar * opacity);
        ui.painter().rect_filled(
            bar,
            0,
            self.theme
                .background
                .gamma_multiply(self.look.top_bar * opacity),
        );
        let focused = shell.focused();
        ui.scope_builder(
            UiBuilder::new()
                .id_salt("derisk-top-bar")
                .max_rect(bar.shrink2(vec2(10.0, 2.0)))
                .layout(Layout::left_to_right(Align::Center)),
            |ui| {
                ui.visuals_mut().override_text_color = Some(self.theme.foreground);
                ui.spacing_mut().item_spacing.x = 12.0;
                let overview = ui
                    .add(
                        icons::button(ui.ctx(), "view-app-grid", 16.0, self.theme.foreground)
                            .frame(false),
                    )
                    .on_hover_text("Overview");
                name(ui, &overview, Role::Button, "Overview");
                if std::mem::take(&mut self.focus_bar) {
                    overview.request_focus();
                }
                if overview.clicked() {
                    actions.push(Action::Overview { visible: None });
                }
                let search = icons::button_with_text(
                    ui.ctx(),
                    "system-search",
                    RichText::new("Search or ask…").color(self.theme.border),
                    14.0,
                    self.theme.border,
                );
                let search = prefs.search.then(|| {
                    ui.add(
                        search
                            .fill(self.theme.surface)
                            .stroke(Stroke::new(1.0, self.theme.border))
                            .corner_radius(8),
                    )
                    .on_hover_text("Command palette (Super+Space)")
                });
                if let Some(search) = &search {
                    name(ui, search, Role::Button, "Search or ask");
                }
                // Where the startup's dot lands; it stays the field's focus
                // while no window has the keyboard (typing then asks).
                self.search_rect = search.as_ref().map(|s| s.rect);
                if let Some(search) = search.as_ref().filter(|_| frame.open >= 1.0)
                    && focused.is_none()
                    && !shell.palette_visible()
                {
                    ui.painter().rect_stroke(
                        search.rect.expand(3.0),
                        11,
                        Stroke::new(2.5, self.theme.accent),
                        StrokeKind::Inside,
                    );
                }
                if search.is_some_and(|s| s.clicked()) {
                    actions.push(Action::Palette { visible: None });
                }
                if let Some(w) = focused.filter(|_| prefs.app_name) {
                    let (app, _) = shell.window_label(w).unwrap_or_default();
                    let look = shell.apps.look(app);
                    let side = (height * 0.6).round();
                    let (r, _) = ui.allocate_exact_size(vec2(side, side), Sense::hover());
                    ui.spacing_mut().item_spacing.x = 6.0;
                    self.paint_app_icon(ui.painter(), r, &look);
                    ui.spacing_mut().item_spacing.x = 12.0;
                    ui.label(RichText::new(look.name.as_ref()).strong());
                }
                for menu in shell.menus.bar(focused.map(|w| w.get())) {
                    menu_button(ui, &menu, focused.map(|w| w.get()), actions);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    // The overview's clock widget shows the time and date
                    // larger, so the bar's copy steps aside while it is open.
                    let big_clock =
                        shell.overview_visible() && crate::widgets::plugins().has("clock");
                    if !big_clock {
                        let time = if prefs.clock_24h {
                            shell.clock.time_label()
                        } else {
                            shell.clock.time_label_12h()
                        };
                        ui.label(if prefs.date {
                            format!("{}  {time}", shell.clock.date_label())
                        } else {
                            time
                        });
                    }
                    if let Some(b) = shell.battery.filter(|_| prefs.battery) {
                        let battery = battery(ui, b, self.theme.foreground);
                        name(
                            ui,
                            &battery,
                            Role::Label,
                            format!(
                                "Battery {}%{}",
                                b.percent,
                                if b.charging { ", charging" } else { "" }
                            ),
                        );
                    }
                    self.indicators(ui, shell, actions);
                    self.keyboard_button(ui, shell, actions);
                    self.tray_icons(ui, shell, height, actions);
                });
            },
        );
    }

    /// The phone's top bar: a status bar with the time, the focused app and
    /// the indicators. The overview button and search field move to the
    /// navigation bar within reach of the thumb, and the global menus, which
    /// have no room here, stay reachable as palette commands.
    fn status_bar(
        &mut self,
        ui: &mut Ui,
        shell: &Shell,
        frame: StartupFrame,
        actions: &mut Vec<Action>,
    ) {
        let output = to_rect(shell.output());
        let height = shell.profile().top_bar as f32;
        let bar = Rect::from_min_size(
            output.min + vec2(0.0, frame.bar_offset),
            vec2(output.width(), height),
        );
        // Never auto-hidden: the clock and indicators stay in reach.
        self.bar_rect = Some(bar);
        let opacity = frame.shell_opacity.max(0.0);
        self.frost(bar, 0, self.look.top_bar * opacity);
        ui.painter().rect_filled(
            bar,
            0,
            self.theme
                .background
                .gamma_multiply(self.look.top_bar * opacity),
        );
        ui.scope_builder(
            UiBuilder::new()
                .id_salt("derisk-status-bar")
                .max_rect(bar.shrink2(vec2(12.0, 0.0)))
                .layout(Layout::left_to_right(Align::Center)),
            |ui| {
                ui.visuals_mut().override_text_color = Some(self.theme.foreground);
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.label(RichText::new(shell.clock.time_label()).strong());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if let Some(b) = shell.battery {
                        battery(ui, b, self.theme.foreground);
                    }
                    self.indicators(ui, shell, actions);
                    self.tray_icons(ui, shell, height, actions);
                });
            },
        );
        // The app on screen, centred, as its title bar would name it on a
        // desktop: its icon and name (phones have no title bars).
        if let Some(w) = shell.focused() {
            let (app, title) = shell.window_label(w).unwrap_or_default();
            let look = shell.apps.look(app);
            let name = if !look.name.is_empty() {
                &look.name[..]
            } else if app.contains('.') && !title.is_empty() {
                title
            } else {
                app
            };
            self.paint_icon_label(
                ui.painter(),
                bar.center(),
                bar.width() * 0.4,
                &look,
                name,
                14.0,
                self.theme.foreground,
            );
        }
    }

    /// The phone's navigation bar along the bottom edge: Back, Home and
    /// Apps, each a third of the width, plus swipes that start on it (up for
    /// home, sideways to switch apps). See [`crate::mobile`].
    fn nav_bar(
        &mut self,
        ui: &mut Ui,
        shell: &Shell,
        frame: StartupFrame,
        actions: &mut Vec<Action>,
    ) {
        let nav = shell.nav_bar();
        let output = shell.output();
        let area = to_rect(nav.area(output));
        if area.height() <= 0.0 {
            return;
        }
        // The bar slides in from below as the top bar slides in from above.
        let area = area.translate(vec2(0.0, -frame.bar_offset));
        let opacity = frame.shell_opacity.max(0.0);
        self.frost(area, 0, self.look.top_bar * opacity);
        ui.painter().rect_filled(
            area,
            0,
            self.theme
                .background
                .gamma_multiply(self.look.top_bar * opacity),
        );
        let state = NavState {
            overview: shell.overview_visible(),
            palette: shell.palette_visible(),
        };
        // Added first, so the buttons on top keep their taps and this only
        // gets the drags they don't sense.
        let swipe = ui.interact(area, Id::new("derisk-nav-swipe"), Sense::drag());
        swipe.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Other,
                true,
                "Navigation bar: swipe up for home, sideways to switch apps",
            )
        });
        if swipe.drag_started() {
            self.swipe_from = ui.input(|i| i.pointer.press_origin());
        }
        if swipe.drag_stopped() {
            let end = ui.input(|i| i.pointer.latest_pos());
            if let (Some(from), Some(to)) = (self.swipe_from.take(), end) {
                let d = to - from;
                if let Some(s) = mobile::swipe(d.x, d.y) {
                    actions.extend(mobile::swiped(s, state));
                }
            }
        }
        for (button, cell) in nav.buttons(output) {
            let r = to_rect(cell).translate(vec2(0.0, -frame.bar_offset));
            // Named for screen readers, but no hover tooltip: a finger lifted
            // off the button would leave one hanging over the bar.
            let response = ui.interact(r, Id::new(("derisk-nav", button.label())), Sense::click());
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, button.label())
            });
            let active = match button {
                mobile::NavButton::Back => false,
                mobile::NavButton::Home => state.overview,
                mobile::NavButton::Apps => state.palette,
                mobile::NavButton::Keyboard => shell.keyboard_visible(),
            };
            if response.is_pointer_button_down_on() {
                ui.painter()
                    .rect_filled(r.shrink(4.0), 12, self.theme.accent.gamma_multiply(0.18));
            }
            icons::paint(
                ui.painter(),
                Rect::from_center_size(r.center(), vec2(22.0, 22.0)),
                button.icon(),
                if active {
                    self.theme.accent
                } else {
                    self.theme.foreground
                },
            );
            if response.clicked() {
                actions.extend(mobile::tap(button, state));
            }
        }
    }

    /// The on-screen keyboard, floating over the bottom of the screen (see
    /// [`Shell::keyboard_area`]): a strip of word suggestions and four rows of keys (see [`crate::keyboard`]). Output
    /// goes to the chrome while the palette or overview is up, and to the
    /// focused window otherwise.
    fn on_screen_keyboard(&mut self, ui: &mut Ui, shell: &Shell) {
        let Some(area) = shell.keyboard_area() else {
            self.backspace_repeat = None;
            return;
        };
        let r = to_rect(area);
        // A layer of its own in the tooltip order puts the keyboard above
        // everything else the chrome draws, the palette and dialogs
        // included, for drawing and for taps alike. The chrome itself is
        // drawn above every window, so nothing an app shows covers it.
        // A plain Ui rather than an Area: a new Area spends its first frame
        // measuring itself unseen, which would swallow the first tap.
        let id = Id::new("derisk-osk");
        let mut keys = Ui::new(
            ui.ctx().clone(),
            id,
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::new(Order::Tooltip, id))
                .max_rect(r),
        );
        // Taps between keys stop here instead of reaching what is drawn
        // under the keyboard.
        keys.interact(r, id.with("backdrop"), Sense::click());
        self.keyboard_keys(&mut keys, shell, area);
    }

    fn keyboard_keys(&mut self, ui: &mut Ui, shell: &Shell, area: Geometry) {
        let out = keyboard_keys(
            ui,
            &mut self.keyboard,
            area,
            &self.theme,
            &mut self.backspace_repeat,
        );
        if out.is_empty() {
            return;
        }
        if shell.palette_visible() || shell.overview_visible() {
            self.chrome_input.extend(out.into_iter().map(|o| match o {
                OskOutput::Text(text) => egui::Event::Text(text),
                OskOutput::Backspace => key_event(Key::Backspace),
                OskOutput::Enter => key_event(Key::Enter),
            }));
            ui.ctx().request_repaint();
        } else {
            self.window_input.extend(out);
        }
    }

    /// Failed units and agent activity, as buttons that open the palette on
    /// them. Shared by the desktop top bar and the phone status bar.
    /// The top bar's on-screen keyboard toggle, on touchscreens (the
    /// keyboard is in the palette everywhere else) and while it shows, so it
    /// can always be put away the way it came up. Phones have theirs on the
    /// navigation bar.
    fn keyboard_button(&mut self, ui: &mut Ui, shell: &Shell, actions: &mut Vec<Action>) {
        if !shell.profile().touch && !shell.keyboard_visible() {
            return;
        }
        let button = ui
            .add(
                icons::button(
                    ui.ctx(),
                    "input-keyboard",
                    16.0,
                    if shell.keyboard_visible() {
                        self.theme.accent
                    } else {
                        self.theme.foreground
                    },
                )
                .frame(false),
            )
            .on_hover_text("On-screen keyboard");
        name(ui, &button, Role::Button, "On-screen keyboard");
        if button.clicked() {
            actions.push(Action::Keyboard { visible: None });
        }
    }

    fn indicators(&mut self, ui: &mut Ui, shell: &Shell, actions: &mut Vec<Action>) {
        // Windows that rang the bell or asked to come forward while the
        // person was elsewhere: their app's icon, ringed, until focused.
        let side = 18.0;
        for window in shell.wanting_attention() {
            let (app, title) = shell.window_label(window).unwrap_or_default();
            let look = shell.apps.look(app);
            let (r, response) = ui.allocate_exact_size(vec2(side, side), Sense::click());
            ui.painter().circle_stroke(
                r.center(),
                side * 0.65,
                Stroke::new(2.0, self.theme.accent),
            );
            self.paint_app_icon(ui.painter(), r.shrink(1.0), &look);
            let label = format!("{} wants attention: {title}", look.name);
            name(ui, &response, Role::Button, label.clone());
            if response.on_hover_text(label).clicked() {
                actions.push(Action::Focus {
                    window: window.get(),
                });
            }
        }
        if shell.kept_awake {
            let awake = ui
                .add(
                    icons::button(ui.ctx(), "video-display", 14.0, self.theme.foreground)
                        .frame(false),
                )
                .on_hover_text("An app is keeping the screen on");
            name(ui, &awake, Role::Label, "An app is keeping the screen on");
        }
        let failed = (!shell.failed_units.is_empty()).then(|| {
            let n = shell.failed_units.len();
            let button = ui
                .add(
                    icons::button_with_text(
                        ui.ctx(),
                        "dialog-warning",
                        n.to_string(),
                        14.0,
                        self.theme.foreground,
                    )
                    .frame(false),
                )
                .on_hover_text("Failed user services: restart or dismiss them");
            let noun = if n == 1 { "service" } else { "services" };
            name(ui, &button, Role::Button, format!("{n} failed {noun}"));
            button
        });
        if failed.is_some_and(|b| b.clicked()) {
            self.palette.preset = Some("> failed".to_owned());
            actions.push(Action::Palette {
                visible: Some(true),
            });
        }
        // Agent activity the person has not looked at yet.
        let unseen = shell
            .conversation
            .turns()
            .filter(|t| t.id > self.palette.seen && t.source == Source::Agent)
            .count();
        let working = shell.conversation.turns().any(|t| !t.is_settled());
        let activity = (unseen > 0 || working).then(|| {
            let button = ui
                .add(
                    if unseen > 0 {
                        icons::button_with_text(
                            ui.ctx(),
                            "tool-magic",
                            RichText::new(unseen.to_string()).color(self.theme.accent),
                            14.0,
                            self.theme.accent,
                        )
                    } else {
                        icons::button(ui.ctx(), "tool-magic", 14.0, self.theme.accent)
                    }
                    .frame(false),
                )
                .on_hover_text(if working {
                    "derisk is working on a request"
                } else {
                    "Agent activity: open the conversation"
                });
            let label = match (working, unseen) {
                (true, _) => "Agent activity, working".to_owned(),
                (false, n) => format!("Agent activity, {n} unseen"),
            };
            name(ui, &button, Role::Button, label);
            button
        });
        if activity.is_some_and(|b| b.clicked()) {
            self.palette.chat_next = true;
            actions.push(Action::Palette {
                visible: Some(true),
            });
        }
    }

    fn tray_icons(&mut self, ui: &mut Ui, shell: &Shell, bar: f32, actions: &mut Vec<Action>) {
        let generation = shell.tray.generation();
        if self.tray.as_ref().is_none_or(|(g, _)| *g != generation) {
            let textures = shell
                .tray
                .items()
                .map(|item| {
                    let image = egui::ColorImage::from_rgba_unmultiplied(
                        [item.icon.width as usize, item.icon.height as usize],
                        &item.icon.rgba,
                    );
                    ui.ctx().load_texture(
                        format!("derisk-tray-{}", item.id),
                        image,
                        TextureOptions::LINEAR,
                    )
                })
                .collect();
            self.tray = Some((generation, textures));
        }
        let size = (bar * 0.6).round();
        let textures = self
            .tray
            .as_ref()
            .map(|(_, t)| t.as_slice())
            .unwrap_or_default();
        // Right-to-left layout: reverse so items read left to right.
        for (item, tex) in shell.tray.items().zip(textures).rev() {
            let response = ui
                .add(
                    egui::Button::image(egui::Image::new((tex.id(), vec2(size, size))))
                        .frame(false),
                )
                .on_hover_text(&item.title);
            name(ui, &response, Role::Button, &item.title);
            if response.clicked() {
                actions.push(Action::ActivateTray {
                    id: item.id.clone(),
                    item: None,
                });
            }
            if !item.menu.is_empty() {
                response.context_menu(|ui| {
                    entries(
                        ui,
                        &item.menu,
                        &mut |id| Action::ActivateTray {
                            id: item.id.clone(),
                            item: Some(id),
                        },
                        actions,
                    );
                });
            }
        }
    }

    fn overview(&mut self, ui: &mut Ui, shell: &Shell, actions: &mut Vec<Action>) {
        // Typing on the overview (outside the notes) asks or searches in the
        // palette, which is where the assistant lives.
        if !shell.palette_visible() && ui.ctx().memory(|m| m.focused().is_none()) {
            let typed: String = ui.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::Text(t) => Some(t.as_str()),
                        _ => None,
                    })
                    .collect()
            });
            if !typed.trim().is_empty() {
                self.palette.preset = Some(typed);
                actions.push(Action::Palette {
                    visible: Some(true),
                });
            }
        }
        // It comes in as the film's does: the backdrop fades up while the
        // search field's outline folds onto the current workspace (30
        // frames), and everything else rises in around it, one after
        // another (30 frames each, starting 4 to 28 frames in).
        let ctx = ui.ctx().clone();
        let now = motion::now(&ctx);
        if !self.overview_open {
            self.overview_since = now;
        }
        let since = self.overview_since;
        let backdrop = motion::since(&ctx, since, f(30), ease::SMOOTH);
        let area = shell.work_area();
        self.frost(to_rect(area), 0, self.look.overview * backdrop);
        ui.painter().rect_filled(
            to_rect(area),
            0,
            self.theme
                .background
                .gamma_multiply(self.look.overview * backdrop),
        );
        let layout = OverviewLayout::new(area, shell.profile().form_factor);

        // Workspace strip, Mission Control style: one cell per open workspace,
        // then a "+" slot that opens a new one. Windows dragged from the grid
        // drop onto either.
        let open = shell.workspaces();
        let active = shell.active_workspace();
        let slots = open.len() + usize::from(shell.can_add_workspace());
        let cells = row(slots, layout.workspaces, 10);
        let pointer = ui.input(|i| i.pointer.latest_pos());
        self.overview_drag = self
            .overview_drag
            .filter(|w| shell.workspace_of(*w) == Some(open[active as usize - 1]));
        let dragging = self.overview_drag;
        let drop_on = pointer.and_then(|p| cells.iter().position(|c| to_rect(*c).contains(p)));
        let windows = shell.windows_on(open[active as usize - 1]);
        let risers = slots - 1 + windows.len() + self.widget_cards;
        let mut riser = 0;
        let mut rise = || {
            let at = since + f64::from(f(4) + stagger(riser, risers, f(24)));
            riser += 1;
            (at, motion::since(&ctx, at, f(30), ease::OUT))
        };
        for (i, cell) in cells.iter().enumerate() {
            let n = i as u64 + 1;
            let mut painter = ui.painter().clone();
            let mut r = to_rect(*cell);
            if n == active {
                // Its contents come up under the outline.
                painter.multiply_opacity(motion::since(
                    &ctx,
                    since + f64::from(f(12)),
                    f(18),
                    ease::SMOOTH,
                ));
            } else {
                let (_, p) = rise();
                r = r.translate(vec2(0.0, RISE * (1.0 - p)));
                painter.multiply_opacity(p);
            }
            let response = ui.interact(r, Id::new(("derisk-ws", n)), Sense::click());
            name(
                ui,
                &response,
                Role::Button,
                if i < open.len() {
                    format!("Workspace {n}")
                } else {
                    "New workspace".to_owned()
                },
            );
            let hot = if dragging.is_some() {
                drop_on == Some(i)
            } else {
                response.hovered()
            };
            let Some(&ws) = open.get(i) else {
                // The "+" slot.
                painter.rect_filled(
                    r,
                    10,
                    if hot {
                        self.theme.surface
                    } else {
                        self.theme.background
                    },
                );
                painter.rect_stroke(
                    r,
                    10,
                    Stroke::new(
                        if hot { 2.0 } else { 1.0 },
                        if hot {
                            self.theme.accent
                        } else {
                            self.theme.border
                        },
                    ),
                    StrokeKind::Inside,
                );
                let plus = if hot {
                    self.theme.accent
                } else {
                    self.theme.foreground
                };
                icons::paint(
                    &painter,
                    Rect::from_center_size(r.center(), vec2(20.0, 20.0)),
                    "list-add",
                    plus,
                );
                if response.on_hover_text("New workspace").clicked() {
                    actions.push(Action::SwitchWorkspace { workspace: n });
                }
                continue;
            };
            painter.rect_filled(r, 10, self.theme.surface);
            if n == active {
                // The outline the top bar's search field folds into.
                let g = motion::since(&ctx, since, f(30), ease::OUT);
                let (from, radius) = self
                    .search_rect
                    .map_or((r, 10.0), |s| (s, s.height() / 2.0));
                ui.painter().rect_stroke(
                    motion::lerp_rect(from, r, g),
                    radius + (10.0 - radius) * g,
                    Stroke::new(2.0 * (5.0 / 3.5 + (1.0 - 5.0 / 3.5) * g), self.theme.accent),
                    StrokeKind::Inside,
                );
            } else {
                painter.rect_stroke(
                    r,
                    10,
                    Stroke::new(
                        if hot { 2.0 } else { 1.0 },
                        if hot {
                            self.theme.accent
                        } else {
                            self.theme.border
                        },
                    ),
                    StrokeKind::Inside,
                );
            }
            // The workspace in miniature: each window where it really is,
            // with its app's icon, so a workspace reads as what is open on
            // it and how it is arranged.
            let screen = fit(area, inset(*cell, 8));
            for p in shell.placements_on(ws) {
                let m = to_rect(miniature(p.frame, area, screen));
                painter.rect_filled(m, 3, self.theme.border.gamma_multiply(0.35));
                painter.rect_stroke(
                    m,
                    3,
                    Stroke::new(1.0, self.theme.border),
                    StrokeKind::Inside,
                );
                let (app, _) = shell.window_label(p.window).unwrap_or_default();
                let side = (m.height().min(m.width()) * 0.5).min(28.0);
                if side >= 10.0 {
                    let look = shell.apps.look(app);
                    self.paint_app_icon(
                        &painter,
                        Rect::from_center_size(m.center(), vec2(side, side)),
                        &look,
                    );
                }
            }
            // The workspace's number, in a corner clear of the windows.
            let badge = Rect::from_min_size(r.left_top() + vec2(6.0, 6.0), vec2(20.0, 20.0));
            painter.rect_filled(badge, 6, self.theme.background);
            painter.text(
                badge.center(),
                Align2::CENTER_CENTER,
                format!("{n}"),
                FontId::proportional(13.0),
                self.theme.foreground,
            );
            if response.clicked() {
                actions.push(Action::SwitchWorkspace { workspace: n });
            }
        }

        // Window grid (exposé), including minimized windows. Drag a window
        // onto a workspace to move it there.
        let placements = shell.placements();
        let mut ghost = None;
        for (w, cell) in windows.iter().zip(grid(windows.len(), layout.windows, 24)) {
            let frame = placements
                .iter()
                .find(|p| p.window == *w)
                .map_or(cell, |p| p.frame);
            let (_, risen) = rise();
            let mut painter = ui.painter().clone();
            painter.multiply_opacity(risen);
            let placed = to_rect(fit(frame, cell));
            let r = placed.translate(vec2(0.0, RISE * (1.0 - risen)));
            let response =
                ui.interact(r, Id::new(("derisk-win", w.get())), Sense::click_and_drag());
            // Pressed, the card dips to 97.5% and back (12 frames).
            if response.is_pointer_button_down_on() && self.pressed.is_none_or(|(p, _)| p != *w) {
                self.pressed = Some((*w, now));
            }
            let dip = self.pressed.filter(|(p, _)| p == w).map_or(0.0, |(_, at)| {
                let t = (now - at) as f32 / f(6);
                if t < 2.0 {
                    ctx.request_repaint();
                }
                (1.0 - (t - 1.0).abs()).max(0.0)
            });
            let r = if motion::enabled(&ctx) {
                Rect::from_center_size(r.center(), r.size() * (1.0 - 0.025 * dip))
            } else {
                r
            };
            let (app, title) = shell.window_label(*w).unwrap_or_default();
            name(
                ui,
                &response,
                Role::Button,
                if title.is_empty() { app } else { title },
            );
            if response.drag_started_by(egui::PointerButton::Primary) {
                self.overview_drag = Some(*w);
            }
            let lifted = self.overview_drag == Some(*w);
            if lifted {
                ghost = Some((*w, r.size()));
            }
            let minimized = shell.is_minimized(*w);
            let hovered = response.hovered() && dragging.is_none();
            // The window as it is, when nothing on screen covers it (it
            // would show in the window's place) and it isn't minimized.
            let picture = self
                .window_pictures
                .as_ref()
                .filter(|_| !minimized)
                .and_then(|pictures| {
                    let at = placements.iter().position(|p| p.window == *w)?;
                    let frame = to_rect(placements[at].frame);
                    let covered = placements[at + 1..]
                        .iter()
                        .any(|p| to_rect(p.frame).intersects(frame));
                    (!covered).then(|| (pictures.texture, (pictures.uv)(frame)))
                });
            let fill = match (picture, minimized) {
                (Some(_), _) => Color32::WHITE,
                (None, true) => self.theme.background,
                (None, false) => self.theme.surface,
            };
            let fill = if lifted {
                fill.gamma_multiply(0.4)
            } else {
                fill
            };
            let card = RectShape::filled(r, 12, fill);
            painter.add(match picture {
                Some((texture, uv)) => card.with_texture(texture, uv),
                None => card,
            });
            painter.rect_stroke(
                r,
                12,
                Stroke::new(
                    if hovered { 2.0 } else { 1.0 },
                    if hovered {
                        self.theme.accent
                    } else {
                        self.theme.border
                    },
                ),
                StrokeKind::Inside,
            );
            let (app, title) = shell.window_label(*w).unwrap_or_default();
            let look = shell.apps.look(app);
            if picture.is_some() {
                self.paint_picture_label(&painter, r, &look);
            } else {
                self.paint_window_card(&painter, r, &look, title);
            }
            // Without title bars, a phone closes apps from here: a close
            // button on each card, a full touch target in its corner.
            let mut closed = false;
            if shell.is_phone() {
                let size = mobile::TOUCH_TARGET as f32;
                let close = Rect::from_min_size(r.right_top() - vec2(size, 0.0), vec2(size, size));
                let hit = ui.interact(
                    close,
                    Id::new(("derisk-win-close", w.get())),
                    Sense::click(),
                );
                hit.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        true,
                        format!("Close {}", look.name),
                    )
                });
                painter.circle_filled(
                    close.center(),
                    14.0,
                    button_color(Button::Close).gamma_multiply(
                        if hit.is_pointer_button_down_on() {
                            0.7
                        } else {
                            1.0
                        },
                    ),
                );
                icons::paint(
                    &painter,
                    Rect::from_center_size(close.center(), vec2(16.0, 16.0)),
                    "window-close",
                    Color32::WHITE,
                );
                if hit.clicked() {
                    actions.push(Action::Close {
                        window: Some(w.get()),
                    });
                    closed = true;
                }
            }
            if response.clicked() && !closed {
                // It grows into the window (see `grow`).
                if !minimized {
                    self.picked = Some((*w, placed));
                    self.grown = Some((*w, placed, now));
                }
                actions.push(if minimized {
                    Action::Restore { window: w.get() }
                } else {
                    Action::Focus { window: w.get() }
                });
                actions.push(Action::Overview {
                    visible: Some(false),
                });
            }
        }

        // The lifted window follows the pointer, shrinking like a thumbnail.
        if let (Some((w, size)), Some(p)) = (ghost, pointer) {
            let scale = (180.0 / size.x.max(1.0)).min(1.0);
            let r = Rect::from_center_size(p, size * scale);
            let painter = ui.ctx().layer_painter(egui::LayerId::new(
                Order::Foreground,
                Id::new("derisk-overview-drag"),
            ));
            painter.rect_filled(r, 8, self.theme.surface.gamma_multiply(0.9));
            painter.rect_stroke(
                r,
                8,
                Stroke::new(2.0, self.theme.accent),
                StrokeKind::Inside,
            );
            let (app, _) = shell.window_label(w).unwrap_or_default();
            let look = shell.apps.look(app);
            self.paint_icon_label(
                &painter,
                r.center(),
                r.width() - 12.0,
                &look,
                &look.name,
                14.0,
                self.theme.foreground,
            );
        }
        if let Some(w) = self.overview_drag
            && ui.input(|i| i.pointer.primary_released())
        {
            self.overview_drag = None;
            if let Some(i) = drop_on {
                let n = i as u64 + 1;
                if n != active {
                    actions.push(Action::MoveToWorkspace {
                        window: Some(w.get()),
                        workspace: n,
                    });
                }
            }
        }

        // Widgets.
        ui.scope_builder(
            UiBuilder::new()
                .id_salt("derisk-widgets")
                .max_rect(to_rect(layout.widgets))
                .layout(Layout::top_down(Align::Min)),
            |ui| {
                ui.visuals_mut().override_text_color = Some(self.theme.foreground);
                let plugins = crate::widgets::plugins();
                let board = {
                    let mut board = std::mem::take(&mut self.board);
                    board.refresh(plugins, &crate::widgets::view(shell));
                    board
                };
                let names: Vec<&str> = plugins.iter().map(|p| p.name()).collect();
                let mut cards = 0;
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (plugin, card) in board.cards() {
                        cards += 1;
                        // Drawn lower while it rises, then the space it
                        // took back, so the cards below stay put.
                        let (at, p) = rise();
                        self.card_risen = at + f64::from(f(30));
                        let dy = (RISE * (1.0 - p)).round();
                        let top = ui.cursor().min + vec2(0.0, dy);
                        ui.scope_builder(
                            UiBuilder::new().max_rect(Rect::from_min_size(
                                top,
                                vec2(ui.available_width(), f32::INFINITY),
                            )),
                            |ui| {
                                ui.multiply_opacity(p);
                                self.widget_card(ui, shell, names[plugin], card, actions);
                            },
                        );
                        ui.add_space(-dy);
                    }
                });
                self.widget_cards = cards;
                self.board = board;
            },
        );
    }

    /// One widget plugin's card, every part drawn with the theme: the
    /// plugin says what the card holds, never how it looks.
    fn widget_card(
        &mut self,
        ui: &mut Ui,
        shell: &Shell,
        plugin: &str,
        card: &Card,
        actions: &mut Vec<Action>,
    ) {
        let theme = self.theme;
        self::card(ui, &theme, |ui| {
            for (index, part) in card.parts.iter().enumerate() {
                match part {
                    Part::Text(text) => {
                        ui.add(egui::Label::new(rich_text(&theme, text)).wrap());
                    }
                    Part::Progress(progress) => {
                        let mut bar = egui::ProgressBar::new(progress.value.clamp(0.0, 1.0));
                        if let Some(label) = &progress.label {
                            bar = bar.text(label.as_str());
                        }
                        ui.add(bar);
                    }
                    Part::Row(items) => {
                        ui.horizontal(|ui| self.widget_inline(ui, shell, items, actions));
                    }
                    Part::Flow(items) => {
                        ui.horizontal_wrapped(|ui| self.widget_inline(ui, shell, items, actions));
                    }
                    Part::Grid(grid) => {
                        // Columns that share the card's width: left to egui,
                        // each would take a full button's width and push the
                        // card past the overview's edge.
                        let columns = usize::from(grid.columns.max(1));
                        let gap = 4.0;
                        let col = ((ui.available_width() - (columns - 1) as f32 * gap)
                            / columns as f32)
                            .floor()
                            .max(1.0);
                        egui::Grid::new((plugin, &card.key, index))
                            .spacing(vec2(gap, 4.0))
                            .min_col_width(col)
                            .max_col_width(col)
                            .show(ui, |ui| {
                                for (i, cell) in grid.cells.iter().enumerate() {
                                    // Cells are centred in their column, and
                                    // the current one (today, in the
                                    // calendar) sits on an accent wash.
                                    let height = ui.spacing().interact_size.y;
                                    let wash = ui.painter().add(egui::Shape::Noop);
                                    let r = ui
                                        .add_sized(
                                            [col, height],
                                            egui::Label::new(rich_text(&theme, cell)),
                                        )
                                        .rect;
                                    if matches!(cell.tone, Tone::Accent) && !cell.text.is_empty() {
                                        ui.painter().set(
                                            wash,
                                            egui::Shape::rect_filled(
                                                r,
                                                6,
                                                // Washed in once its card is
                                                // up (8 frames).
                                                theme.accent.gamma_multiply(
                                                    0.2 * motion::since(
                                                        ui.ctx(),
                                                        self.card_risen,
                                                        f(8),
                                                        ease::OUT,
                                                    ),
                                                ),
                                            ),
                                        );
                                    }
                                    if (i + 1) % columns == 0 {
                                        ui.end_row();
                                    }
                                }
                            });
                    }
                    Part::Field(field) => {
                        // The field takes the card's own color and the
                        // card's width less its frame, instead of the
                        // theme's darkest input fill stretched past the
                        // card's edge.
                        let width = ui.available_width();
                        let key = (plugin.to_owned(), card.key.clone(), field.id.clone());
                        let text = self.fields.entry(key).or_default();
                        ui.add(
                            egui::TextEdit::multiline(text)
                                .id_salt((plugin, &card.key, &field.id))
                                .desired_rows(usize::from(field.lines.max(1)))
                                .desired_width(width)
                                .background_color(theme.surface)
                                .hint_text(RichText::new(&field.hint).color(theme.border)),
                        );
                    }
                }
            }
        });
        ui.add_space(10.0);
    }

    /// A widget card's row: text, icons, buttons and apps side by side. An
    /// icon takes the color and size of the row's first text.
    fn widget_inline(
        &self,
        ui: &mut Ui,
        shell: &Shell,
        items: &[Inline],
        actions: &mut Vec<Action>,
    ) {
        let theme = self.theme;
        let first_text = items.iter().find_map(|item| match item {
            Inline::Text(text) => Some(text),
            _ => None,
        });
        let icon_color = first_text.map_or(theme.foreground, |t| tone_color(&theme, t.tone));
        let icon_size = match first_text.map(|t| t.emphasis) {
            Some(Emphasis::Strong | Emphasis::Display) => 16.0,
            _ => 14.0,
        };
        // Buttons beside text are the small kind, as in a list of failed
        // services; a row of buttons alone gets full-size ones.
        let small = first_text.is_some();
        for item in items {
            match item {
                Inline::Text(text) => {
                    ui.label(rich_text(&theme, text));
                }
                Inline::Icon(name) => {
                    icons::show(ui, name, icon_size, icon_color);
                }
                Inline::Button(button) => {
                    let clicked = if small {
                        ui.small_button(button.label.as_str()).clicked()
                    } else {
                        ui.button(button.label.as_str()).clicked()
                    };
                    if clicked {
                        actions.extend(widget_actions(&button.actions));
                    }
                }
                Inline::App(app) => {
                    if self.app_chip(ui, shell, &app.app).clicked() {
                        actions.extend(widget_actions(&app.actions));
                    }
                }
            }
        }
    }

    /// An app by its icon and name, never its desktop file ID, as a button.
    fn app_chip(&self, ui: &mut Ui, shell: &Shell, app: &str) -> egui::Response {
        let look = shell.apps.look(app);
        let text = RichText::new(look.name.as_ref());
        let side = 18.0;
        let galley = egui::WidgetText::from(text).into_galley(
            ui,
            Some(egui::TextWrapMode::Truncate),
            ui.available_width() - side - 24.0,
            egui::TextStyle::Button,
        );
        let pad = ui.spacing().button_padding;
        let size = vec2(
            pad.x * 2.0 + side + 6.0 + galley.size().x,
            (pad.y * 2.0 + side.max(galley.size().y)).max(ui.spacing().interact_size.y),
        );
        let (r, response) = ui.allocate_exact_size(size, Sense::click());
        let visuals = ui.style().interact(&response);
        ui.painter().rect(
            r,
            visuals.corner_radius,
            visuals.weak_bg_fill,
            visuals.bg_stroke,
            StrokeKind::Inside,
        );
        let icon = Rect::from_min_size(
            pos2(r.left() + pad.x, r.center().y - side / 2.0),
            vec2(side, side),
        );
        self.paint_app_icon(ui.painter(), icon, &look);
        ui.painter().galley(
            pos2(icon.right() + 6.0, r.center().y - galley.size().y / 2.0),
            galley,
            visuals.text_color(),
        );
        name(ui, &response, Role::Button, look.name.to_string());
        response.on_hover_text(look.name.as_ref())
    }

    /// The command palette: a search box over [`palette::entries`] with the
    /// assistant as fallback, and the agent conversation (chat view).
    fn palette(&mut self, ui: &mut Ui, shell: &Shell, actions: &mut Vec<Action>) {
        let theme = self.theme;
        // A phone's palette runs down to the keyboard; elsewhere the keyboard
        // floats over the bottom of the screen, clear of the palette.
        let keyboard = shell
            .keyboard_area()
            .filter(|_| shell.is_phone())
            .map_or(0.0, |k| k.size.h as f32);
        let state = &mut self.palette;
        let mut cursor_to_end = false;
        if !state.open {
            state.query = state.preset.take().unwrap_or_default();
            // The field only gets focus next frame; keep what is typed now.
            let typed: String = ui.input_mut(|i| {
                let mut typed = String::new();
                i.events.retain(|e| match e {
                    egui::Event::Text(t) => {
                        typed.push_str(t);
                        false
                    }
                    _ => true,
                });
                typed
            });
            state.query.push_str(&typed);
            cursor_to_end = !state.query.is_empty();
            state.chat = std::mem::take(&mut state.chat_next);
            state.shown_query.clear();
            state.selected = 0;
            state.armed = None;
            state.message = None;
            state.opened = Some((motion::now(ui.ctx()), self.search_rect));
            state.typed = vec![0.0; state.query.chars().count()];
            state.asked_after = None;
            state.done_at = None;
            state.fold = None;
            state.opened_turn = shell.conversation.last_id().unwrap_or(0);
        }
        // `?` switches to the conversation; the rest is the request.
        if !state.chat
            && let Some(rest) = state.query.trim_start().strip_prefix('?')
        {
            state.query = rest.trim_start().to_owned();
            state.chat = true;
        }

        let plugins = palette::plugins();
        let view = palette::view(shell, &state.apps);
        let (entries, hits, query_rows) = if state.chat {
            (&[][..], Vec::new(), None)
        } else {
            let query_rows = state.catalog.rows(plugins, &view, &state.query).clone();
            let entries = state
                .catalog
                .entries(plugins, &view, &state.files, shell.can_hibernate);
            let hits = palette::search(entries, &state.query, &state.history);
            (
                entries,
                limit_palette_hits(entries, &hits, 60),
                Some(query_rows),
            )
        };
        let query_rows = query_rows.unwrap_or_default();
        let rows: Vec<&Entry> = palette::list(entries, &hits, &query_rows, &state.query);
        if state.query != state.shown_query {
            state.shown_query = state.query.clone();
            state.selected = 0;
            state.armed = None;
            if !state.chat {
                state.message = None;
            }
        }
        state.selected = state.selected.min(rows.len().saturating_sub(1));

        // Keys the text field would otherwise eat.
        let empty = state.query.is_empty();
        let (down, up, enter, escape, back) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown)
                    || i.consume_key(Modifiers::NONE, Key::Tab)
                    || i.consume_key(Modifiers::CTRL, Key::N),
                i.consume_key(Modifiers::NONE, Key::ArrowUp)
                    || i.consume_key(Modifiers::SHIFT, Key::Tab)
                    || i.consume_key(Modifiers::CTRL, Key::P),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
                state.chat && empty && i.consume_key(Modifiers::NONE, Key::Backspace),
            )
        });
        if back {
            state.chat = false;
            state.message = None;
        }
        if !rows.is_empty() {
            if down {
                state.selected = (state.selected + 1) % rows.len();
            }
            if up {
                state.selected = (state.selected + rows.len() - 1) % rows.len();
            }
        }
        if escape {
            actions.push(Action::Palette {
                visible: Some(false),
            });
            return;
        }

        let phone = shell.is_phone();
        // The navigation bar and the keyboard stay above the backdrop on
        // phones, so Back, Home and typing keep working with the palette open.
        let nav = shell.profile().nav_bar as f32 + keyboard;
        let screen = to_rect(shell.output());
        let dimmed = Rect::from_min_max(screen.min, screen.max - vec2(0.0, nav));
        // It grows out of the search field as the film's does: the card
        // morphs from the field to its own place (52 frames, expo-out) and
        // its contents fade in over the last 16.
        let (opened, from) = state.opened.unwrap_or_default();
        let ctx = ui.ctx().clone();
        let morph = motion::since(&ctx, opened, f(52), ease::OUT);
        let shown = motion::since(&ctx, opened + f64::from(f(36)), f(16), ease::SMOOTH);
        // A request sent from here that went through folds the palette into
        // the dot 30 frames after its "Done.", as the film's does (16
        // frames, expo-out): the contents go over the first 7 and the
        // accent floods the card from the 4th. One that failed keeps it
        // open to show why, and so does typing something else.
        if state.fold.is_none()
            && let Some(after) = state.asked_after
            && let Some(turn) = shell.conversation.turns().find(|t| t.id > after)
            && turn.is_settled()
        {
            let went_through = turn.error.is_none()
                && !turn.steps.is_empty()
                && turn.steps.iter().all(|s| s.status == StepStatus::Done);
            if !went_through {
                state.asked_after = None;
            } else if let Some(card) = state.rect {
                let now = motion::now(&ctx);
                let folds = *state.done_at.get_or_insert(now) + f64::from(f(30));
                if !state.query.is_empty() {
                    state.asked_after = None;
                } else if now >= folds {
                    state.fold = Some((folds.min(now), card));
                    // The window's ring waits for the dot to reach it.
                    if let Some(p) = ringed(shell) {
                        self.ring = (Some(p.window), f64::INFINITY);
                    }
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_secs_f64(
                        (folds - now) / 1000.0,
                    ));
                }
            }
        }
        let fold = state.fold.map(|(at, from)| {
            let t = motion::since(&ctx, at, f(16), ease::OUT);
            let dot = Rect::from_center_size(from.center(), vec2(DOT, DOT));
            (at, motion::lerp_rect(from, dot, t), t)
        });
        if let Some((at, card, t)) = fold
            && t >= 1.0
        {
            self.relay = Some((at, card.center()));
            actions.push(Action::Palette {
                visible: Some(false),
            });
            return;
        }
        let folded = fold.map_or(0.0, |(_, _, t)| t);
        // Dim the desktop; clicking it closes the palette.
        let backdrop = egui::Area::new(Id::new("derisk-palette-backdrop"))
            .order(Order::Middle)
            .fixed_pos(dimmed.min)
            .show(ui.ctx(), |ui| {
                let (r, response) = ui.allocate_exact_size(dimmed.size(), Sense::click());
                ui.painter().rect_filled(
                    r,
                    0,
                    Color32::from_black_alpha(110).gamma_multiply(shown * (1.0 - folded)),
                );
                response.clicked()
            })
            .inner;
        if backdrop {
            actions.push(Action::Palette {
                visible: Some(false),
            });
            return;
        }

        let width = (screen.width() - if phone { 16.0 } else { 32.0 }).clamp(240.0, 680.0);
        let bar = shell.profile().top_bar as f32;
        // A phone has no height to spare: the palette starts under the status
        // bar and its list runs down to just above the navigation bar.
        let top = if phone {
            screen.top() + bar + 8.0
        } else {
            screen.top() + (screen.height() * 0.14).max(bar + 16.0)
        };
        let row_height = if phone {
            mobile::TOUCH_TARGET as f32
        } else {
            PALETTE_ROW_HEIGHT
        };
        let list_height = if phone {
            (dimmed.bottom() - top - 96.0).max(row_height * 3.0)
        } else {
            PALETTE_ROWS as f32 * PALETTE_ROW_HEIGHT + 40.0
        };
        let mut chosen = (enter && !state.chat).then_some(state.selected);
        let mut new_conversation = false;
        let card = egui::Area::new(Id::new("derisk-palette"))
            .order(Order::Foreground)
            .fixed_pos(pos2(screen.center().x - width / 2.0, top))
            .show(ui.ctx(), |ui| {
                // The card is painted under the contents once their height
                // is known, easing to it (16 frames, expo-out) as rows come
                // and go.
                let painter = ui.painter().clone();
                let shadow_slot = painter.add(egui::Shape::Noop);
                let card_slot = painter.add(egui::Shape::Noop);
                ui.multiply_opacity(shown);
                if let Some((at, card, _)) = fold {
                    ui.multiply_opacity(1.0 - motion::since(ui.ctx(), at, f(7), ease::SMOOTH));
                    ui.set_clip_rect(card.intersect(ui.clip_rect()));
                }
                let inner = egui::Frame::new()
                    .inner_margin(10)
                    .show(ui, |ui| {
                        ui.set_width(width - 20.0);
                        ui.visuals_mut().override_text_color = Some(theme.foreground);
                        ui.horizontal(|ui| {
                            if state.chat {
                                icons::show(ui, "tool-magic", 18.0, theme.accent);
                            }
                            // Each character flashes in the accent as it is
                            // typed (4 frames), as the film's request does.
                            let now = motion::now(ui.ctx());
                            let flashes = motion::enabled(ui.ctx());
                            let typed = &state.typed;
                            let fresh = |i: usize| {
                                flashes
                                    && now - typed.get(i).copied().unwrap_or(now)
                                        < f64::from(f(4))
                            };
                            let mut layouter =
                                |ui: &Ui, text: &dyn egui::TextBuffer, wrap: f32| {
                                    let mut job = egui::text::LayoutJob::default();
                                    for (i, c) in text.as_str().chars().enumerate() {
                                        let color = if fresh(i) {
                                            theme.accent
                                        } else {
                                            theme.foreground
                                        };
                                        job.append(
                                            c.encode_utf8(&mut [0; 4]),
                                            0.0,
                                            egui::TextFormat::simple(
                                                FontId::proportional(18.0),
                                                color,
                                            ),
                                        );
                                    }
                                    job.wrap.max_width = wrap;
                                    ui.fonts_mut(|fonts| fonts.layout_job(job))
                                };
                            let input = ui.add(
                                egui::TextEdit::singleline(&mut state.query)
                                    .layouter(&mut layouter)
                                    .hint_text(if state.chat {
                                        "Ask derisk to do something…"
                                    } else {
                                        "Search apps, windows, commands, files… or ask derisk"
                                    })
                                    .font(FontId::proportional(18.0))
                                    .frame(egui::Frame::NONE)
                                    .margin(vec2(6.0, 8.0))
                                    .desired_width(f32::INFINITY),
                            );
                            input.request_focus();
                            let count = state.query.chars().count();
                            if count < state.typed.len() {
                                state.typed.truncate(count);
                            }
                            while state.typed.len() < count {
                                state.typed.push(now);
                            }
                            if state.typed.iter().any(|t| now - t < f64::from(f(4))) {
                                ui.ctx().request_repaint();
                            }
                            // Preset text (typed on the overview, or a
                            // filter) continues where it ends.
                            if cursor_to_end
                                && let Some(mut edit) =
                                    egui::text_edit::TextEditState::load(ui.ctx(), input.id)
                            {
                                let end = egui::text::CCursor::new(state.query.chars().count());
                                edit.cursor.set_char_range(Some(
                                    egui::text_selection::CCursorRange::one(end),
                                ));
                                edit.store(ui.ctx(), input.id);
                            }
                        });
                        ui.add_space(4.0);
                        ui.painter().hline(
                            ui.min_rect().x_range(),
                            ui.cursor().top(),
                            Stroke::new(1.0, theme.border),
                        );
                        ui.add_space(6.0);
                        if state.chat {
                            // The conversation takes the search's place
                            // (14 frames).
                            let o = motion::appear(
                                ui.ctx(),
                                Id::new("derisk-conversation"),
                                0.0,
                                f(14),
                                ease::SMOOTH,
                            );
                            ui.multiply_opacity(o);
                            new_conversation = conversation(
                                ui,
                                &theme,
                                shell,
                                state.cleared,
                                state.opened_turn,
                            );
                        } else {
                            if rows.is_empty() {
                                ui.label(
                                    RichText::new("Type to search, or ask in plain words.")
                                        .color(theme.border),
                                );
                            }
                            let selected = state.selected;
                            egui::ScrollArea::vertical()
                                .max_height(list_height)
                                .auto_shrink([false, true])
                                .show(ui, |ui| {
                                    let mut heading = None;
                                    for (i, entry) in rows.iter().enumerate() {
                                        // A row fades in when it first shows
                                        // (10 frames) and glides when rows
                                        // above it come or go (18, expo-out),
                                        // as the film's Apps rows make room
                                        // for the assistant's.
                                        let key = Id::new(("derisk-palette-row", entry.key()));
                                        if heading != Some(entry.category) {
                                            heading = Some(entry.category);
                                            let hid = key.with("heading");
                                            let (slot, _) = ui.allocate_exact_size(
                                                vec2(ui.available_width(), 18.0),
                                                Sense::hover(),
                                            );
                                            // Relative to the list, so
                                            // scrolling doesn't glide.
                                            let shift = motion::glide(
                                                ui.ctx(),
                                                Id::new((
                                                    "derisk-palette-heading",
                                                    entry.category.heading(),
                                                )),
                                                (slot.min - ui.max_rect().min).to_pos2(),
                                                f(18),
                                                ease::OUT,
                                            );
                                            let o = motion::appear(
                                                ui.ctx(),
                                                hid,
                                                0.0,
                                                f(10),
                                                ease::SMOOTH,
                                            );
                                            ui.painter().text(
                                                slot.left_center() + shift,
                                                Align2::LEFT_CENTER,
                                                entry.category.heading(),
                                                FontId::proportional(11.0),
                                                theme.border.gamma_multiply(o),
                                            );
                                        }
                                        let fade = motion::appear(
                                            ui.ctx(),
                                            key,
                                            0.0,
                                            f(10),
                                            ease::SMOOTH,
                                        );
                                        let at = (ui.cursor().min - ui.max_rect().min).to_pos2();
                                        let shift =
                                            motion::glide(ui.ctx(), key, at, f(18), ease::OUT);
                                        let response = ui
                                            .scope(|ui| {
                                                ui.multiply_opacity(fade);
                                                palette_row(
                                                    ui,
                                                    &theme,
                                                    &shell.apps,
                                                    &self.icons,
                                                    entry,
                                                    i == selected,
                                                    row_height,
                                                    shift,
                                                )
                                            })
                                            .inner;
                                        if i == selected && (up || down) {
                                            response.scroll_to_me(None);
                                        }
                                        if response.clicked() {
                                            chosen = Some(i);
                                        }
                                    }
                                });
                        }
                        if let Some(message) = &state.message {
                            ui.add_space(6.0);
                            ui.label(RichText::new(message).color(theme.accent));
                        }
                        // Keyboard hints mean nothing without a keyboard.
                        if phone {
                            return;
                        }
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(if state.chat {
                                "Enter ask   Backspace back to search   Esc close"
                            } else {
                                "⬆⬇ select   Enter run   Esc close   > commands   @ windows   / files   ? ask"
                            })
                            .size(11.0)
                            .color(theme.border),
                        );
                    });
                let laid = inner.response.rect;
                let height = motion::tween(
                    ui.ctx(),
                    Id::new("derisk-palette-height"),
                    laid.height(),
                    f(16),
                    ease::OUT,
                );
                let rest = Rect::from_min_size(laid.min, vec2(laid.width(), height));
                // From the field (or a dot in the card's middle) to the card.
                let start = from.unwrap_or_else(|| {
                    Rect::from_center_size(rest.center(), vec2(DOT, DOT))
                });
                let mut r = motion::lerp_rect(start, rest, morph);
                let mut radius = 8.0 + (14.0 - 8.0) * morph;
                if let Some((_, card, _)) = fold {
                    r = card;
                    radius += (DOT / 2.0 - radius) * folded;
                }
                let shadow = egui::epaint::Shadow {
                    offset: [0, 12],
                    blur: 40,
                    spread: 0,
                    color: Color32::from_black_alpha(140).gamma_multiply(morph * (1.0 - folded)),
                };
                painter.set(shadow_slot, shadow.as_shape(r, radius));
                painter.set(
                    card_slot,
                    egui::epaint::RectShape::new(
                        r,
                        radius,
                        theme.surface,
                        Stroke::new(1.0 - folded, theme.border),
                        StrokeKind::Inside,
                    ),
                );
                // Folding, the accent floods it until it is the dot.
                if let Some((at, _, _)) = fold {
                    let flood =
                        motion::since(ui.ctx(), at + f64::from(f(4)), f(12), ease::SMOOTH);
                    ui.ctx()
                        .layer_painter(ui.layer_id())
                        .with_clip_rect(ui.ctx().content_rect())
                        .rect_filled(r, radius, theme.accent.gamma_multiply(flood));
                }
                // While it morphs, the field's focus outline grows with it
                // into the input.
                if morph < 1.0 {
                    let field = Rect::from_min_size(
                        rest.min + vec2(10.0, 10.0),
                        vec2(rest.width() - 20.0, 36.0),
                    );
                    painter.rect_stroke(
                        motion::lerp_rect(start.expand(3.0), field, morph),
                        11.0 - 3.0 * morph,
                        Stroke::new(2.5, theme.accent.gamma_multiply(1.0 - shown)),
                        StrokeKind::Inside,
                    );
                }
                r
            });
        state.rect = Some(card.inner);

        if state.chat {
            state.seen = shell.conversation.last_id().unwrap_or(0).max(state.seen);
            if new_conversation {
                state.cleared = state.seen;
            }
            if enter && !state.query.trim().is_empty() {
                let text = state.query.trim().to_owned();
                let preview = palette::ask(&text);
                if preview.confirm && state.armed.as_deref() != Some(text.as_str()) {
                    state.armed = Some(text);
                    state.message =
                        Some("This ends your session. Press Enter again to go ahead.".into());
                    return;
                }
                let confirmed = preview.confirm;
                Self::send_ask(state, shell, &preview, text, confirmed, actions);
            }
            return;
        }

        let Some(entry) = chosen.and_then(|i| rows.get(i)).map(|e| (*e).clone()) else {
            return;
        };
        if entry.actions.is_empty() {
            // The assistant did not understand; say why and keep the text.
            state.message = Some(entry.detail.clone());
            return;
        }
        if entry.confirm && state.armed.as_deref() != Some(&entry.key()) {
            state.armed = Some(entry.key());
            state.message = Some(format!(
                "Press Enter again to {}.",
                entry.title.to_lowercase()
            ));
            return;
        }
        if entry.category == Category::Ask {
            // Requests run in the conversation, which stays open to show
            // their progress.
            let text = palette::scope(&state.query).1.to_owned();
            Self::send_ask(state, shell, &entry, text, entry.confirm, actions);
            state.chat = true;
            return;
        }
        state.history.record(&entry);
        let touches_overview = entry
            .actions
            .iter()
            .any(|a| matches!(a, Action::Overview { .. }));
        actions.extend(entry.actions);
        actions.push(Action::Palette {
            visible: Some(false),
        });
        if shell.overview_visible() && !touches_overview {
            actions.push(Action::Overview {
                visible: Some(false),
            });
        }
    }

    /// Queues a request for the assistant and clears the field.
    fn send_ask(
        state: &mut PaletteUi,
        shell: &Shell,
        preview: &Entry,
        text: String,
        confirmed: bool,
        actions: &mut Vec<Action>,
    ) {
        // Get the overview out of the way so results are visible, unless
        // the request is about the overview itself.
        if shell.overview_visible()
            && !preview
                .actions
                .iter()
                .any(|a| matches!(a, Action::Overview { .. }))
        {
            actions.push(Action::Overview {
                visible: Some(false),
            });
        }
        state.asks.push(Ask { text, confirmed });
        // Its turn is the next one; once it is done the palette folds away.
        state.asked_after = Some(shell.conversation.last_id().unwrap_or(0));
        state.done_at = None;
        state.query.clear();
        state.shown_query.clear();
        state.armed = None;
        state.message = None;
    }

    /// Requests typed in the palette since the last call, for the host to
    /// run with [`Shell::ask`] as [`Source::User`].
    pub fn take_asks(&mut self) -> Vec<Ask> {
        std::mem::take(&mut self.palette.asks)
    }
}

/// The focused window, when it has a frame to ring: not on a phone, and
/// not filling the work area.
fn ringed(shell: &Shell) -> Option<WindowPlacement> {
    if shell.profile().title_bar.height == 0 {
        return None;
    }
    shell
        .placements()
        .into_iter()
        .find(|p| p.focused)
        .filter(|p| p.frame != shell.work_area())
}

/// A natural-language request typed in the palette.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ask {
    /// The request.
    pub text: String,
    /// The person confirmed a destructive session operation in it.
    pub confirmed: bool,
}

/// The conversation transcript, newest at the bottom. Returns whether
/// "New conversation" was clicked.
///
/// Turns newer than `fresh_after` animate as the film's does: the steps
/// come in one after another, each check draws itself in when its step is
/// done, and the reply fades in once every step has finished.
fn conversation(ui: &mut Ui, theme: &Theme, shell: &Shell, cleared: u64, fresh_after: u64) -> bool {
    let turns: Vec<&Turn> = shell
        .conversation
        .turns()
        .filter(|t| t.id > cleared)
        .collect();
    let mut clear = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new("Conversation").size(11.0).color(theme.border));
        if !turns.is_empty() {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                clear = ui
                    .add(egui::Button::new(RichText::new("New").size(11.0)).frame(false))
                    .on_hover_text("Start a new conversation")
                    .clicked();
            });
        }
    });
    if turns.is_empty() {
        ui.label(
            RichText::new(
                "Ask in plain words, e.g. \"open calculator and snap it right, then go to workspace 2\". \
                 Requests from other agents show up here too.",
            )
            .color(theme.border),
        );
        return clear;
    }
    egui::ScrollArea::vertical()
        .max_height(PALETTE_ROWS as f32 * PALETTE_ROW_HEIGHT)
        .auto_shrink([false, true])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            for turn in turns {
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    let who = match turn.source {
                        Source::User => "You",
                        Source::Agent => "Agent",
                    };
                    ui.label(RichText::new(who).size(11.0).color(theme.accent));
                    ui.label(RichText::new(&turn.request).strong());
                });
                let fresh = turn.id > fresh_after;
                let turn_id = Id::new(("derisk-turn", turn.id));
                for (i, step) in turn.steps.iter().enumerate() {
                    let step_id = turn_id.with(i);
                    let shown = if fresh {
                        motion::appear(ui.ctx(), step_id, f(4) + f(3) * i as f32, f(12), ease::OUT)
                    } else {
                        1.0
                    };
                    let (icon, color) = match &step.status {
                        StepStatus::Done => (Some("object-select"), theme.accent),
                        StepStatus::Waiting => (Some("view-refresh"), theme.border),
                        StepStatus::Failed(_) => {
                            (Some("dialog-error"), button_color(Button::Close))
                        }
                        StepStatus::Skipped => (None, theme.border),
                    };
                    ui.horizontal(|ui| {
                        ui.multiply_opacity(shown);
                        ui.add_space(12.0);
                        match icon {
                            // The check draws itself in, left to right
                            // (8 frames).
                            Some(icon) if step.status == StepStatus::Done && fresh => {
                                let drawn = motion::appear(
                                    ui.ctx(),
                                    step_id.with("done"),
                                    0.0,
                                    f(8),
                                    ease::OUT,
                                );
                                let (r, _) =
                                    ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                                let mut clip = r;
                                clip.max.x = r.min.x + r.width() * drawn;
                                icons::paint(&ui.painter().with_clip_rect(clip), r, icon, color);
                            }
                            Some(icon) => {
                                icons::show(ui, icon, 14.0, color);
                            }
                            None => {
                                ui.label(RichText::new("·").color(color));
                            }
                        }
                        ui.label(RichText::new(&step.label).color(
                            if step.status == StepStatus::Skipped {
                                theme.border
                            } else {
                                theme.foreground
                            },
                        ));
                    });
                }
                ui.horizontal_wrapped(|ui| {
                    if fresh && turn.is_settled() {
                        ui.multiply_opacity(motion::appear(
                            ui.ctx(),
                            turn_id.with("reply"),
                            0.0,
                            f(11),
                            ease::SMOOTH,
                        ));
                    }
                    ui.add_space(12.0);
                    let failed = turn.error.is_some()
                        || turn
                            .steps
                            .iter()
                            .any(|s| matches!(s.status, StepStatus::Failed(_)));
                    ui.label(RichText::new(turn.reply()).color(if failed {
                        button_color(Button::Close)
                    } else {
                        theme.border
                    }));
                });
            }
        });
    clear
}

/// One palette row: icon, title, detail and shortcut hint, drawn `shift`
/// from where it is laid out while it glides.
#[allow(clippy::too_many_arguments)]
fn palette_row(
    ui: &mut Ui,
    theme: &Theme,
    apps: &Apps,
    app_icons: &AppIcons,
    entry: &Entry,
    selected: bool,
    height: f32,
    shift: egui::Vec2,
) -> egui::Response {
    let (slot, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    // Drawn where it is gliding from, while its place moves.
    let r = slot.translate(shift);
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_role(Role::ListBoxOption);
        node.set_label(entry.title.clone());
        if !entry.detail.is_empty() {
            node.set_description(entry.detail.clone());
        }
        node.set_selected(selected);
    });
    let painter = ui.painter_at(r);
    if selected || response.hovered() {
        painter.rect_filled(
            r,
            8,
            theme
                .accent
                .gamma_multiply(if selected { 0.22 } else { 0.1 }),
        );
    }
    let mid = r.center().y;
    // Apps show their own icon in color, as in title bars; every other row
    // is an action with a greyscale Papirus icon.
    let app = entry.actions.iter().find_map(|a| match a {
        Action::Launch { app } | Action::LaunchAction { app, .. } => Some(app),
        _ => None,
    });
    match app.filter(|_| matches!(entry.category, Category::App | Category::AppAction)) {
        Some(app) => {
            let mut look = apps.look(app);
            look.glyph = &entry.icon;
            let icon_area = Rect::from_center_size(pos2(r.left() + 18.0, mid), vec2(20.0, 20.0));
            paint_app_icon(app_icons, theme, &painter, icon_area, &look);
        }
        None => {
            let icon_area = Rect::from_center_size(pos2(r.left() + 18.0, mid), vec2(16.0, 16.0));
            icons::paint(&painter, icon_area, &entry.icon, theme.foreground);
        }
    }
    let shortcut_width = entry
        .shortcut
        .as_ref()
        .map_or(0.0, |s| s.len() as f32 * 7.0 + 16.0);
    let text_left = r.left() + 40.0;
    let title_galley = painter.layout_no_wrap(
        elide(
            &entry.title,
            (r.width() - 56.0 - shortcut_width) * 0.6,
            15.0,
        ),
        FontId::proportional(15.0),
        theme.foreground,
    );
    let title_width = title_galley.size().x;
    painter.galley(
        pos2(text_left, mid - title_galley.size().y / 2.0),
        title_galley,
        theme.foreground,
    );
    if !entry.detail.is_empty() {
        let room = r.right() - shortcut_width - (text_left + title_width + 12.0);
        painter.text(
            pos2(text_left + title_width + 12.0, mid),
            Align2::LEFT_CENTER,
            elide(&entry.detail, room, 13.0),
            FontId::proportional(13.0),
            theme.border,
        );
    }
    if let Some(shortcut) = &entry.shortcut {
        painter.text(
            pos2(r.right() - 10.0, mid),
            Align2::RIGHT_CENTER,
            shortcut,
            FontId::monospace(12.0),
            theme.border,
        );
    }
    response
}

/// The theme's color for `tone`.
fn tone_color(theme: &Theme, tone: Tone) -> Color32 {
    match tone {
        Tone::Normal => theme.foreground,
        Tone::Dim => theme.border,
        Tone::Accent => theme.accent,
        Tone::Danger => button_color(Button::Close),
    }
}

/// A widget plugin's text in the theme: its tone's color, and its emphasis
/// as weight or the overview's display size.
fn rich_text(theme: &Theme, text: &Text) -> RichText {
    let rich = RichText::new(&text.text).color(tone_color(theme, text.tone));
    match text.emphasis {
        Emphasis::Normal => rich,
        Emphasis::Strong => rich.strong(),
        Emphasis::Display => rich.size(44.0).strong(),
    }
}

/// A widget button's actions. `derisk-plugin` let through only JSON objects
/// whose kind the plugin's manifest lists; one that is not an [`Action`]
/// does nothing.
fn widget_actions(actions: &[String]) -> impl Iterator<Item = Action> + '_ {
    actions.iter().filter_map(|a| {
        serde_json::from_str(a)
            .inspect_err(|e| tracing::warn!(action = a, "widget action refused: {e}"))
            .ok()
    })
}

fn menu_button(ui: &mut Ui, menu: &Menu, window: Option<u64>, actions: &mut Vec<Action>) {
    ui.menu_button(menu.title.as_str(), |ui| {
        entries(
            ui,
            &menu.entries,
            &mut |item| Action::ActivateMenu { window, item },
            actions,
        );
    });
}

fn entries(
    ui: &mut Ui,
    list: &[MenuEntry],
    make: &mut dyn FnMut(String) -> Action,
    actions: &mut Vec<Action>,
) {
    for entry in list {
        match entry {
            MenuEntry::Item {
                id,
                label,
                shortcut,
                enabled,
            } => {
                let mut button = egui::Button::new(label.as_str());
                if let Some(hint) = shortcut {
                    button = button.shortcut_text(hint.as_str());
                }
                if ui.add_enabled(*enabled, button).clicked() {
                    actions.push(make(id.clone()));
                    ui.close();
                }
            }
            MenuEntry::Submenu { label, entries: e } => {
                ui.menu_button(label.as_str(), |ui| entries(ui, e, make, actions));
            }
            MenuEntry::Separator => {
                ui.separator();
            }
        }
    }
}

/// Draws the on-screen keyboard's suggestion strip and keys into `area` and
/// returns what was typed this frame. The shell's floating keyboard and the
/// setup screens (see [`crate::wizard`]) both draw it with this.
/// `backspace_repeat` holds when a held Backspace next repeats.
pub fn keyboard_keys(
    ui: &mut Ui,
    keyboard: &mut Keyboard,
    area: Geometry,
    theme: &Theme,
    backspace_repeat: &mut Option<f64>,
) -> Vec<OskOutput> {
    let r = to_rect(area);
    // Opaque, so nothing reads through the keys.
    ui.painter().rect_filled(r, 0, theme.surface);
    ui.painter()
        .hline(r.x_range(), r.top(), Stroke::new(1.0, theme.border));
    let mut out = Vec::new();

    let suggestions = keyboard.suggestions();
    for (i, cell) in Keyboard::suggestion_cells(area).into_iter().enumerate() {
        let cell = to_rect(cell);
        if i > 0 {
            ui.painter().vline(
                cell.left(),
                cell.y_range().shrink(10.0),
                Stroke::new(1.0, theme.border),
            );
        }
        let Some(word) = suggestions.get(i) else {
            continue;
        };
        let response = ui.interact(cell, Id::new(("derisk-osk-suggestion", i)), Sense::click());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                true,
                format!("Suggestion: {word}"),
            )
        });
        if response.is_pointer_button_down_on() {
            ui.painter()
                .rect_filled(cell.shrink(3.0), 8, theme.accent.gamma_multiply(0.2));
        }
        ui.painter().text(
            cell.center(),
            Align2::CENTER_CENTER,
            elide(word, cell.width() - 12.0, 16.0),
            FontId::proportional(16.0),
            theme.foreground,
        );
        if response.clicked() {
            out.extend(keyboard.choose(word));
        }
    }

    let now = ui.input(|i| i.time);
    let mut backspace_down = false;
    for (key, cell) in keyboard.keys(area) {
        let cell = to_rect(cell).shrink2(vec2(2.5, 4.0));
        let response = ui.interact(cell, Id::new(("derisk-osk-key", key)), Sense::click());
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, key.name()));
        let down = response.is_pointer_button_down_on();
        let modifier = !matches!(key, OskKey::Char(_) | OskKey::Space);
        let lit = matches!(key, OskKey::Shift) && keyboard.shift() != Shift::Off;
        let fill = if down {
            theme.accent.gamma_multiply(0.35)
        } else if lit {
            // Caps Lock reads stronger than a one-letter Shift.
            theme
                .accent
                .gamma_multiply(if keyboard.shift() == Shift::Lock {
                    0.55
                } else {
                    0.25
                })
        } else if modifier {
            theme.border.gamma_multiply(0.45)
        } else {
            theme.background
        };
        ui.painter().rect_filled(cell, 6, fill);
        let size = if matches!(key, OskKey::Char(_)) {
            20.0
        } else {
            15.0
        };
        let color = if lit { theme.accent } else { theme.foreground };
        match key.icon() {
            Some(icon) => icons::paint(
                ui.painter(),
                Rect::from_center_size(cell.center(), vec2(20.0, 20.0)),
                icon,
                color,
            ),
            None => {
                ui.painter().text(
                    cell.center(),
                    Align2::CENTER_CENTER,
                    key.label(keyboard),
                    FontId::proportional(size),
                    color,
                );
            }
        }
        // Holding Backspace repeats it, after a pause, like a hardware key.
        if key == OskKey::Backspace && down {
            backspace_down = true;
            match *backspace_repeat {
                None => *backspace_repeat = Some(now + 0.45),
                Some(at) if now >= at => {
                    out.extend(keyboard.press(key));
                    *backspace_repeat = Some(now + 0.06);
                }
                Some(_) => {}
            }
            ui.ctx().request_repaint();
        }
        if response.clicked() {
            out.extend(keyboard.press(key));
        }
    }
    if !backspace_down {
        *backspace_repeat = None;
    }
    out
}

/// Paints the desktop wallpaper: a vertical gradient with a soft accent glow
/// and the derisk mark in the lower right corner.
pub fn paint_wallpaper(painter: &Painter, screen: Rect, theme: &Theme) {
    let top = Color32::from_rgb(17, 24, 39);
    let bottom = Color32::from_rgb(30, 27, 75);
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(screen.left_top(), top);
    mesh.colored_vertex(screen.right_top(), top);
    mesh.colored_vertex(screen.left_bottom(), bottom);
    mesh.colored_vertex(screen.right_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
    let glow = screen.center() + vec2(screen.width() * 0.22, screen.height() * 0.18);
    for i in 0..12 {
        let r = screen.height() * (0.55 - i as f32 * 0.04);
        painter.circle_filled(glow, r, theme.accent.gamma_multiply(0.012));
    }
    let mark = screen.right_bottom() - vec2(64.0, 56.0);
    painter.circle_filled(mark, 18.0, theme.accent.gamma_multiply(0.35));
    painter.text(
        mark,
        Align2::CENTER_CENTER,
        "d",
        FontId::proportional(22.0),
        theme.background.gamma_multiply(0.9),
    );
}

/// The lock screen and the greeter share one look: the wallpaper over the
/// whole output, the clock, and a centered column below it that `add` fills.
/// Everything is opaque, so nothing behind shows through.
fn login_panel(ui: &mut Ui, screen: Rect, clock: &Clock, theme: &Theme, add: impl FnOnce(&mut Ui)) {
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        Order::Background,
        Id::new("derisk-lock-bg"),
    ));
    painter.rect_filled(screen, 0, theme.background);
    paint_wallpaper(&painter, screen, theme);
    egui::Area::new(Id::new("derisk-lock"))
        .order(Order::Foreground)
        .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
        .show(ui.ctx(), |ui| {
            ui.set_width(320.0);
            ui.visuals_mut().override_text_color = Some(theme.foreground);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new(clock.time_label()).size(64.0).strong());
                ui.label(RichText::new(clock.date_label()).size(18.0));
                ui.add_space(32.0);
                add(ui);
            });
        });
}

/// The one text field on the lock screen and the greeter. It is the only
/// thing that takes keys, so focus is taken back every frame: a single-line
/// field gives it up on Enter.
/// Returns where the field was drawn.
fn login_field(ui: &mut Ui, text: &mut String, enabled: bool, secret: bool, hint: &str) -> Rect {
    let field = ui.add_enabled(
        enabled,
        egui::TextEdit::singleline(text)
            .password(secret)
            .hint_text(hint)
            .font(FontId::proportional(18.0))
            .margin(vec2(10.0, 8.0))
            .desired_width(f32::INFINITY),
    );
    if enabled && !field.has_focus() {
        field.request_focus();
    }
    field.rect
}

/// The line under the field, red when it reports a failure.
fn login_status(ui: &mut Ui, text: &str, error: bool, theme: &Theme) {
    let color = if error {
        Color32::from_rgb(248, 113, 113)
    } else {
        theme.border
    };
    ui.label(RichText::new(text).color(color));
}

/// Draws the lock screen over the whole output: the wallpaper, the clock,
/// who is locked out, and the field answering whatever PAM asks -- the
/// password, a security key's PIN, a new password once the old one expired.
/// The line under it carries PAM's own messages, the fingerprint reader's
/// among them. Returns `true` when Enter was pressed in the field, to send
/// what was typed.
///
/// The host also stops drawing windows while locked, so nothing on the
/// desktop shows through even if this frame were skipped.
pub fn show_lock(
    ui: &mut Ui,
    lock: &mut LockScreen,
    shell: &Shell,
    theme: &Theme,
    user: &str,
) -> bool {
    let mut submit = false;
    login_panel(ui, to_rect(shell.output()), &shell.clock, theme, |ui| {
        ui.label(RichText::new(user).size(20.0).strong());
        ui.add_space(8.0);
        let login = &mut lock.login;
        let editable = login.editable();
        let hint = login.hint().to_owned();
        match login.phase().clone() {
            Phase::Prompt { secret, .. } => {
                login_field(ui, &mut login.answer, true, secret, &hint);
            }
            // Stopped (PAM could not ask at all) or waiting: an empty field
            // keeps the column still, and Enter on a stopped one starts over.
            _ => {
                let mut nothing = String::new();
                login_field(ui, &mut nothing, false, true, &hint);
            }
        }
        submit = editable && ui.input(|i| i.key_pressed(Key::Enter));
        ui.add_space(8.0);
        let (text, error) = lock.message();
        login_status(ui, text, error, theme);
    });
    // Both conversations answer between frames, the reader's whenever a
    // finger lands, so keep looking for them while locked.
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(100));
    submit
}

/// What the person did in polkit's dialog this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolkitInput {
    /// Nothing to act on.
    None,
    /// Enter or Authenticate: send what was typed.
    Submit,
    /// Escape or Cancel.
    Cancel,
    /// Authenticate as someone else instead, by index.
    Choose(usize),
}

/// Draws polkit's request to authenticate as mcsapi's [`NativeDialog`],
/// which in the compositor is a modal over the dimmed desktop: what the app
/// wants to do, who is asked (a choice when polkit accepts several
/// administrators), the field answering whatever PAM asks and the line under
/// it, where the fingerprint reader's prompt shows, then Cancel and
/// Authenticate, which Escape and Enter answer.
///
/// It is drawn by the shell rather than by an app, and the host routes
/// every key here while it is open, so no window can read what is typed.
///
/// [`NativeDialog`]: mcsapi_components::NativeDialog
pub fn show_polkit(
    ui: &mut Ui,
    dialog: &mut crate::polkit::AuthDialog,
    shell: &Shell,
    theme: &Theme,
) -> PolkitInput {
    use mcsapi_components::NativeDialog;
    use mcsapi_ui::dialog::{ActionRole, DialogAction};

    let mut input = PolkitInput::None;
    let mut open = true;
    let actions = [
        DialogAction::new("Cancel", ActionRole::Cancel),
        DialogAction::new("Authenticate", ActionRole::Default),
    ];
    // A phone is narrower than the dialog: its frame and 16 points of
    // margin either side.
    let width = 400.0_f32.min(to_rect(shell.output()).width() - 80.0);
    let answer = NativeDialog::new("derisk-polkit", &mut open, "Authentication required")
        .width(width)
        .show(ui.ctx(), |ui| {
            ui.label(RichText::new("Authentication required").size(18.0).strong());
            ui.add_space(6.0);
            ui.label(&dialog.message);
            ui.add_space(12.0);
            if dialog.identities.len() > 1 {
                let chosen = dialog.who().map(|w| w.name.clone()).unwrap_or_default();
                egui::ComboBox::from_id_salt("derisk-polkit-who")
                    .selected_text(chosen)
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        for (index, identity) in dialog.identities.iter().enumerate() {
                            if ui
                                .selectable_label(index == dialog.chosen, &identity.name)
                                .clicked()
                            {
                                input = PolkitInput::Choose(index);
                            }
                        }
                    });
            } else if let Some(who) = dialog.who() {
                ui.label(RichText::new(&who.name).strong());
            }
            ui.add_space(8.0);
            let secret = dialog.secret();
            let hint = dialog.hint().to_owned();
            if dialog.editable() {
                login_field(ui, &mut dialog.answer, true, secret, &hint);
            } else {
                let mut nothing = String::new();
                login_field(ui, &mut nothing, false, true, &hint);
            }
            ui.add_space(6.0);
            let (text, error) = dialog.status();
            login_status(ui, text, error, theme);
            ui.add_space(12.0);
            NativeDialog::actions(ui, &actions)
        })
        .flatten();
    match answer {
        Some(0) => input = PolkitInput::Cancel,
        Some(_) => input = PolkitInput::Submit,
        None if !open => input = PolkitInput::Cancel,
        None => {}
    }
    // The helper answers between frames, the reader's prompt among them.
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(100));
    input
}

/// What the person did in a choice dialog this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChoiceInput {
    /// Nothing to act on.
    None,
    /// Escape or Cancel.
    Cancel,
    /// Enter or the default button, on the option at this index.
    Pick(usize),
}

/// How an option of a choice reads on screen, and a second, quieter line.
/// The screen-cast portal names screens `Monitor: <name> <description>`
/// and windows `Window: <title> (<identifier>)`: those read as "Entire
/// screen" and the window's title.
pub fn choice_label(option: &str) -> (String, Option<String>) {
    if let Some(screen) = option.strip_prefix("Monitor: ") {
        return ("Entire screen".to_owned(), Some(screen.to_owned()));
    }
    if let Some(window) = option.strip_prefix("Window: ") {
        let title = window.rsplit_once(" (").map_or(window, |(title, _)| title);
        return (title.to_owned(), Some("Window".to_owned()));
    }
    (option.to_owned(), None)
}

/// Draws a choice an app asked the person to make, such as the screen to
/// share, as mcsapi's [`NativeDialog`] over the dimmed desktop: one row
/// per option, `picked` highlighted, and Cancel and Share. Double-clicking
/// a row picks it.
///
/// [`NativeDialog`]: mcsapi_components::NativeDialog
pub fn show_choice(
    ui: &mut Ui,
    title: &str,
    options: &[String],
    picked: &mut usize,
    shell: &Shell,
    theme: &Theme,
) -> ChoiceInput {
    use mcsapi_components::NativeDialog;
    use mcsapi_ui::dialog::{ActionRole, DialogAction};

    let mut input = ChoiceInput::None;
    let mut open = true;
    let actions = [
        DialogAction::new("Cancel", ActionRole::Cancel),
        DialogAction::new("Share", ActionRole::Default),
    ];
    let width = 420.0_f32.min(to_rect(shell.output()).width() - 80.0);
    let answer = NativeDialog::new("derisk-choice", &mut open, title)
        .width(width)
        .show(ui.ctx(), |ui| {
            ui.label(RichText::new(title).size(18.0).strong());
            ui.add_space(10.0);
            egui::ScrollArea::vertical()
                .max_height(320.0)
                .show(ui, |ui| {
                    for (index, option) in options.iter().enumerate() {
                        let (label, detail) = choice_label(option);
                        let (r, row) = ui
                            .allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
                        let fill = if *picked == index {
                            theme.accent.gamma_multiply(0.35)
                        } else if row.hovered() {
                            theme.surface
                        } else {
                            Color32::TRANSPARENT
                        };
                        let painter = ui.painter();
                        painter.rect_filled(r, 6, fill);
                        painter.text(
                            r.left_top() + vec2(12.0, 6.0),
                            Align2::LEFT_TOP,
                            &label,
                            egui::FontId::proportional(14.0),
                            theme.foreground,
                        );
                        if let Some(detail) = &detail {
                            painter.text(
                                r.left_top() + vec2(12.0, 25.0),
                                Align2::LEFT_TOP,
                                detail,
                                egui::FontId::proportional(12.0),
                                theme.foreground.gamma_multiply(0.6),
                            );
                        }
                        name(ui, &row, Role::Button, label);
                        if row.clicked() {
                            *picked = index;
                        }
                        if row.double_clicked() {
                            input = ChoiceInput::Pick(index);
                        }
                    }
                });
            ui.add_space(12.0);
            NativeDialog::actions(ui, &actions)
        })
        .flatten();
    match answer {
        Some(0) => ChoiceInput::Cancel,
        Some(_) => ChoiceInput::Pick(*picked),
        None if !open => ChoiceInput::Cancel,
        None => input,
    }
}

/// What the user did on the greeter this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GreeterInput {
    /// Nothing to act on.
    None,
    /// Enter: send what was typed.
    Submit,
    /// Escape: back to the user name.
    Back,
}

/// Draws `derisk greeter`: the lock screen's look, asking first who is
/// logging in and then whatever PAM asks through greetd. `users` are offered
/// as buttons above the field when there are several to pick from.
pub fn show_greeter(
    ui: &mut Ui,
    login: &mut Login,
    users: &[String],
    screen: Rect,
    clock: &Clock,
    theme: &Theme,
) -> (GreeterInput, Option<Rect>) {
    let mut input = GreeterInput::None;
    let mut field = None;
    login_panel(ui, screen, clock, theme, |ui| {
        let editable = login.editable();
        match login.phase().clone() {
            Phase::User => {
                if users.len() > 1 {
                    // One per line, centered under the clock like the rest.
                    ui.vertical_centered(|ui| {
                        for user in users {
                            if ui.selectable_label(login.username == *user, user).clicked() {
                                login.username = user.clone();
                                input = GreeterInput::Submit;
                            }
                        }
                    });
                    ui.add_space(8.0);
                }
                let hint = login.hint().to_owned();
                field = Some(login_field(ui, &mut login.username, editable, false, &hint));
            }
            Phase::Prompt { secret, .. } => {
                ui.label(RichText::new(&login.username).size(20.0).strong());
                ui.add_space(8.0);
                let hint = login.hint().to_owned();
                field = Some(login_field(ui, &mut login.answer, editable, secret, &hint));
            }
            _ => {
                ui.label(RichText::new(&login.username).size(20.0).strong());
                ui.add_space(8.0);
                // Keeps the column from jumping while greetd answers.
                let mut nothing = String::new();
                field = Some(login_field(ui, &mut nothing, false, true, login.hint()));
                ui.ctx().request_repaint();
            }
        }
        if editable {
            ui.input(|i| {
                if i.key_pressed(Key::Enter) {
                    input = GreeterInput::Submit;
                } else if i.key_pressed(Key::Escape) {
                    input = GreeterInput::Back;
                }
            });
        }
        ui.add_space(8.0);
        match login.notice() {
            Some(notice) => login_status(ui, &notice.text, notice.error, theme),
            None => login_status(ui, login.status(), false, theme),
        }
    });
    (input, field)
}

/// Paints one frame of the login screen's arrival over `screen`: Plymouth's
/// line closing into the dot, then the startup animation, its dot landing
/// on `field`.
pub fn paint_login(
    painter: &Painter,
    screen: Rect,
    frame: LoginFrame,
    theme: &Theme,
    field: Option<Rect>,
) {
    match frame {
        LoginFrame::Collapse(c) => {
            use crate::animation::{DOT, plymouth_line};
            painter.rect_filled(screen, 0, theme.background);
            let [x, y, w, h] = plymouth_line(screen.width(), screen.height());
            let line = Rect::from_min_size(screen.min + vec2(x, y), vec2(w, h));
            let at = line.center() + (screen.center() - line.center()) * c;
            // It closes on its length a little behind its height, so it is
            // round before it is small.
            let size = vec2(w + (DOT - w) * c, h + (DOT - h) * (c * 1.15).min(1.0));
            painter.rect_filled(
                Rect::from_center_size(at, size),
                size.min_elem() / 2.0,
                theme.accent,
            );
        }
        LoginFrame::Startup(frame) => paint_startup(painter, screen, frame, theme, field),
    }
}

/// Paints one frame of the startup animation over `screen`: the cover with
/// the iris the desktop shows through, the ring, and the dot on its way to
/// `field` (the search field's frame, when the top bar has one).
pub fn paint_startup(
    painter: &Painter,
    screen: Rect,
    frame: StartupFrame,
    theme: &Theme,
    field: Option<Rect>,
) {
    use crate::animation::{DOT, RING_RADIUS, RING_WIDTH, arc};
    let center = screen.center();
    let corner = screen.size().length() / 2.0;
    let iris = frame.reveal.radius(corner);
    if iris <= 0.0 {
        painter.rect_filled(screen, 0, theme.background);
    } else if iris.is_finite() {
        // Everything outside the iris: a ring out past the corners.
        let outer = corner + 2.0;
        if iris < outer {
            painter.circle_stroke(
                center,
                (iris + outer) / 2.0,
                Stroke::new(outer - iris, theme.background),
            );
        }
    }
    if frame.cover > 0.0 {
        painter.rect_filled(screen, 0, theme.background.gamma_multiply(frame.cover));
    }
    if frame.sweep > 0.0 {
        // Clockwise from twelve o'clock.
        let steps = 96;
        let sweep = frame.sweep * std::f32::consts::TAU;
        let points: Vec<Pos2> = (0..=steps)
            .map(|i| {
                let a = -std::f32::consts::FRAC_PI_2 + sweep * i as f32 / steps as f32;
                center + vec2(a.cos(), a.sin()) * RING_RADIUS
            })
            .collect();
        painter.line(points, Stroke::new(RING_WIDTH, theme.accent));
    }
    if frame.bloom > 0.0 {
        painter.circle_stroke(
            center,
            iris,
            Stroke::new(
                RING_WIDTH + 6.0 * frame.bloom,
                theme.accent.gamma_multiply(1.0 - 0.6 * frame.bloom),
            ),
        );
    }
    if frame.open < 1.0 {
        let target = field.map_or(center, |f| f.center());
        let (x, y) = arc(
            (center.x, center.y),
            (target.x, target.y),
            frame.flight,
            -0.18,
        );
        // Landed, it opens into the field's focus outline as it fades.
        let to = field.map_or(vec2(DOT, DOT), |f| f.size() + vec2(6.0, 6.0));
        let size = vec2(DOT, DOT) + (to - vec2(DOT, DOT)) * frame.open;
        painter.rect_filled(
            Rect::from_center_size(pos2(x, y), size * frame.dot_scale),
            DOT,
            theme.accent.gamma_multiply(1.0 - frame.open),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_result_limit_keeps_each_matching_category() {
        let mut entries: Vec<_> = (0..61)
            .map(|n| Entry::new(Category::App, "", format!("App {n}"), vec![]))
            .collect();
        entries.push(Entry::new(Category::Window, "", "Window", vec![]));
        entries.push(Entry::new(Category::Command, "", "Command", vec![]));

        let hits: Vec<_> = (0..entries.len()).collect();
        let limited = limit_palette_hits(&entries, &hits, 60);

        assert_eq!(limited.len(), 60);
        for category in [Category::App, Category::Window, Category::Command] {
            assert!(
                limited.iter().any(|&hit| entries[hit].category == category),
                "{category:?} results should remain available"
            );
        }
    }

    #[test]
    fn share_choices_read_as_screens_and_windows() {
        assert_eq!(
            choice_label("Monitor: Virtual-1 Unknown Unknown"),
            (
                "Entire screen".to_owned(),
                Some("Virtual-1 Unknown Unknown".to_owned())
            )
        );
        assert_eq!(
            choice_label("Window: Notes (a (b) c) (12)"),
            ("Notes (a (b) c)".to_owned(), Some("Window".to_owned()))
        );
        assert_eq!(
            choice_label("Something else"),
            ("Something else".to_owned(), None)
        );
    }
}
