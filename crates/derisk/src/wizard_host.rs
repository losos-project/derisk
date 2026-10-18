//! The compositor under `derisk setup` and `derisk installer`: a shell with
//! no windows that draws one flow of pages (see [`derisk::wizard`]) over the
//! wallpaper, and the on-screen keyboard under it.
//!
//! It runs straight on the seat, like the login screen, with nothing to nest
//! in: the installer before any system exists, the setup before anyone can
//! log in. Clients that connect anyway are never placed, so never drawn.

use std::time::Duration;

use derisk::{
    ui::set_touch_style,
    wizard::{self, ScreenKeyboard, ScreenLayout},
};
use mcsapi::{WindowId, widgets::Theme};
use mcsapi_compositor::{
    self as compositor, Command, Compositor, KeyInput, KeyRoute, OutputTiming, Placement, Press,
    egui,
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// One flow of pages.
pub trait Flow: 'static {
    /// Draws the current page and queues anything for the compositor
    /// (`Command::Quit` when the flow is over, `Command::Keymap`).
    fn show(
        &mut self,
        ui: &mut egui::Ui,
        layout: &ScreenLayout,
        theme: &Theme,
        out: &mut Vec<Command>,
    );
}

struct Host<F> {
    flow: F,
    output: (i32, i32),
    theme: Theme,
    keyboard: ScreenKeyboard,
    /// Whether the keyboard was asked for by hand, on a screen that is not
    /// a phone's; a phone's comes up by itself for a focused field.
    keyboard_toggled: bool,
    phone_style: Option<bool>,
    next_window: u64,
    commands: Vec<Command>,
}

impl<F: Flow> compositor::Shell for Host<F> {
    fn map_window(&mut self, _app_id: &str, _title: &str) -> WindowId {
        self.next_window += 1;
        WindowId::new(self.next_window).expect("window IDs start at 1")
    }

    fn unmap_window(&mut self, _window: WindowId) {}

    fn set_output(&mut self, size: (i32, i32)) {
        self.output = size;
    }

    fn focused(&self) -> Option<WindowId> {
        None
    }

    fn placements(&self) -> Vec<Placement> {
        Vec::new()
    }

    fn chrome_wants_pointer(&self, _at: (i32, i32)) -> bool {
        true
    }

    fn pointer_down(&mut self, _at: (i32, i32), _time_ms: u64) -> Press {
        Press::Handled
    }

    /// Every key goes to the pages; there is no client to give one to.
    fn key(&mut self, _key: &KeyInput) -> KeyRoute {
        KeyRoute::Chrome
    }

    fn theme(&self) -> Theme {
        self.theme
    }

    /// Spinners and a typing cursor are the only animation; a few frames a
    /// second would do, but a list being scrolled by a finger wants all of
    /// them.
    fn frame_interval(&self, timing: &OutputTiming) -> Duration {
        timing.refresh_interval()
    }

    /// Its chrome asks egui for every frame it needs (below), and nothing
    /// changes between them on its own.
    fn needs_frame(&mut self) -> bool {
        false
    }

    fn chrome(&mut self, ui: &mut egui::Ui, _elapsed_ms: u32) {
        let screen = egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(self.output.0 as f32, self.output.1 as f32),
        );
        let ctx = ui.ctx().clone();
        let phone = wizard::screen_layout(self.output, false).phone;
        if self.phone_style != Some(phone) {
            set_touch_style(&ctx, phone);
            self.phone_style = Some(phone);
        }
        self.keyboard.deliver(&ctx);
        // A phone's keyboard follows the focused field, as on any phone;
        // elsewhere the hardware keyboard is the one in use, and the button
        // in the corner brings this one up for a touchscreen without one.
        let typing = self.keyboard.follow_focus(&ctx);
        let show_keyboard = (phone && typing) || self.keyboard_toggled;
        let layout = wizard::screen_layout(self.output, show_keyboard);

        wizard::background(ui, screen, &self.theme);
        self.flow.show(ui, &layout, &self.theme, &mut self.commands);
        if !phone {
            self.keyboard_button(ui, screen);
        }
        if let Some(area) = layout.keyboard {
            self.keyboard.show(ui, area, &self.theme);
        } else {
            self.keyboard.reset();
        }
        // Workers report between frames; keep drawing so their news shows.
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    fn take_commands(&mut self) -> Vec<Command> {
        std::mem::take(&mut self.commands)
    }
}

impl<F: Flow> Host<F> {
    fn keyboard_button(&mut self, ui: &mut egui::Ui, screen: egui::Rect) {
        let at = egui::Rect::from_min_size(
            screen.right_top() + egui::vec2(-56.0, 16.0),
            egui::vec2(40.0, 40.0),
        );
        let mut corner = ui.new_child(egui::UiBuilder::new().max_rect(at));
        let color = if self.keyboard_toggled {
            self.theme.accent
        } else {
            self.theme.foreground
        };
        let button = corner
            .add(derisk::icons::button(corner.ctx(), "input-keyboard", 20.0, color).frame(false))
            .on_hover_text("On-screen keyboard");
        if button.clicked() {
            self.keyboard_toggled = !self.keyboard_toggled;
        }
    }
}

/// Runs `flow` until it queues `Command::Quit`. `size` is the window's when
/// nested in another session, for trying the pages at a phone's size.
pub fn run(flow: impl Flow, title: &str, size: (i32, i32)) -> Result {
    let host = Host {
        flow,
        output: size,
        theme: Theme::default(),
        keyboard: ScreenKeyboard::new(),
        keyboard_toggled: false,
        phone_style: None,
        next_window: 0,
        commands: Vec::new(),
    };
    Compositor::new(host)
        .title(title)
        .size(size.0, size.1)
        .run()?;
    Ok(())
}
