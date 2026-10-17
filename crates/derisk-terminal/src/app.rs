//! The window: winit's event loop, feeding the emulator and asking for
//! frames.

use std::{cell::RefCell, fs::File, rc::Rc, time::Instant};

use alacritty_terminal::{
    event::{Event, EventListener, WindowSize},
    grid::Scroll,
    index::{Column, Point, Side},
    selection::{Selection, SelectionType},
    term::{
        ClipboardType, Config, Term, TermMode, color::COUNT, test::TermSize, viewport_to_point,
    },
    vte::ansi::{Processor, Rgb, StdSyncHandler},
};
use crossbeam_channel::{Receiver, Sender, TrySendError};
use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalPosition},
    event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, StartCause, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{UserAttentionType, Window, WindowAttributes, WindowId},
};

use crate::{
    Renderer as Choice,
    atlas::Glyphs,
    cpu,
    frame::{self, Frame, Layout},
    gpu,
    input::{self, Button},
    theme,
};

/// What the pty threads tell the event loop.
#[derive(Debug)]
pub enum Wake {
    /// The shell printed something; it is waiting in the channel.
    Output,
    /// The shell is gone.
    Exited,
}

/// The emulator's events, queued while it parses and handled after.
#[derive(Clone, Default)]
struct Listener(Rc<RefCell<Vec<Event>>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        self.0.borrow_mut().push(event);
    }
}

enum Renderer {
    // Boxed: its GL function table is kilobytes, the software one a pointer.
    Gpu(Box<gpu::Renderer>),
    Cpu(cpu::Renderer),
}

/// What the event loop holds.
pub struct App<'a> {
    // Dropped before the window it draws into.
    renderer: Option<Renderer>,
    clipboard: Option<smithay_clipboard::Clipboard>,
    window: Option<Rc<Window>>,
    choice: Choice,
    name: crate::Name,
    pty: &'a File,
    output: Receiver<Vec<u8>>,
    input: Sender<Vec<u8>>,
    term: Term<Listener>,
    events: Listener,
    parser: Processor<StdSyncHandler>,
    glyphs: Glyphs,
    look: theme::Look,
    palette: [Rgb; COUNT],
    watch: theme::Watch,
    frame: Frame,
    layout: Layout,
    modifiers: ModifiersState,
    focused: bool,
    /// Ctrl+plus and Ctrl+minus, as a factor of the theme's size.
    zoom: f32,
    scale: f64,
    pointer: PhysicalPosition<f64>,
    /// The mouse button held down, for selections and drag reports.
    held: Option<Button>,
    /// The last left click, for double and triple clicks.
    last_click: Option<(Instant, Point, u8)>,
    /// Wheel movement smaller than a line, kept for the next event.
    scrolled: f64,
}

/// Width of the empty border around the grid, in logical pixels.
const PADDING: f64 = 4.0;
/// Bytes of output parsed before a frame is let through. A shell printing
/// without pause would otherwise keep the parser busy and the window frozen.
const BUDGET: usize = 4 << 20;

impl<'a> App<'a> {
    pub fn new(
        choice: Choice,
        name: crate::Name,
        pty: &'a File,
        output: Receiver<Vec<u8>>,
        input: Sender<Vec<u8>>,
        glyphs: Glyphs,
        // The theme the watch read first, and the watch.
        (look, watch): (theme::Look, theme::Watch),
    ) -> Self {
        let events = Listener::default();
        let term = Term::new(Config::default(), &TermSize::new(80, 24), events.clone());
        let palette = look.palette();
        Self {
            renderer: None,
            clipboard: None,
            window: None,
            choice,
            name,
            pty,
            output,
            input,
            term,
            events,
            parser: Processor::new(),
            glyphs,
            look,
            palette,
            watch,
            frame: Frame::default(),
            layout: Layout {
                width: 0,
                height: 0,
                padding: 0,
                columns: 80,
                lines: 24,
            },
            modifiers: ModifiersState::empty(),
            focused: true,
            zoom: 1.0,
            scale: 1.0,
            pointer: PhysicalPosition::new(0.0, 0.0),
            held: None,
            last_click: None,
            scrolled: 0.0,
        }
    }

    fn send(&self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        // The writer is a channel away; when the shell has stopped reading
        // and its queue is full, what is typed now is dropped rather than
        // freezing the window until it reads again.
        if let Err(TrySendError::Full(_)) = self.input.try_send(bytes) {
            tracing::warn!("the shell is not reading its input; dropped a key");
        }
    }

    fn redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    /// The font size in physical pixels.
    fn font_size(&self) -> f32 {
        self.look.size * self.zoom * self.scale as f32
    }

    /// Fits the grid to the window, telling the emulator and the shell if
    /// it changed.
    fn relayout(&mut self) {
        let Some(window) = &self.window else { return };
        let size = window.inner_size();
        let padding = (PADDING * self.scale).round() as u32;
        let cell = self.glyphs.fonts.cell();
        let layout = Layout::fit(size.width, size.height, padding, cell);
        if (layout.columns, layout.lines) != (self.layout.columns, self.layout.lines)
            || self.layout.width == 0
        {
            self.term
                .resize(TermSize::new(layout.columns, layout.lines));
            crate::pty::resize(
                self.pty,
                WindowSize {
                    num_lines: layout.lines as u16,
                    num_cols: layout.columns as u16,
                    cell_width: cell.width as u16,
                    cell_height: cell.height as u16,
                },
            );
        }
        self.layout = layout;
        if let Some(Renderer::Gpu(gpu)) = &self.renderer {
            gpu.resize(window);
        }
        self.redraw();
    }

    /// Takes the published theme again if it changed.
    fn follow_theme(&mut self) {
        let Some(look) = self.watch.poll() else {
            return;
        };
        if look == self.look {
            return;
        }
        let refont = look.family != self.look.family;
        let resize = look.size != self.look.size;
        self.palette = look.palette();
        self.look = look;
        if refont {
            match crate::font::Fonts::new(&self.look.family, self.font_size()) {
                Ok(fonts) => self.glyphs = Glyphs::new(fonts),
                Err(error) => tracing::warn!("{}: {error}", self.look.family),
            }
        } else if resize {
            self.glyphs.resize(self.font_size());
        }
        self.relayout();
    }

    /// Parses what the shell printed, up to the budget.
    fn take_output(&mut self) {
        let mut parsed = 0;
        while parsed < BUDGET {
            let Ok(bytes) = self.output.try_recv() else {
                break;
            };
            self.parser.advance(&mut self.term, &bytes);
            parsed += bytes.len();
        }
        if parsed > 0 {
            self.handle_events();
            // Inside a synchronised update (the program asked, with mode
            // 2026, for its frame to appear whole) the parser holds the
            // bytes back; the frame comes when the update ends or times out.
            if self.parser.sync_timeout().sync_timeout().is_none() {
                self.redraw();
            }
        }
    }

    fn handle_events(&mut self) {
        let events = std::mem::take(&mut *self.events.0.borrow_mut());
        for event in events {
            match event {
                Event::Title(title) => {
                    if let Some(window) = &self.window {
                        window.set_title(&title);
                    }
                }
                Event::ResetTitle => {
                    if let Some(window) = &self.window {
                        window.set_title(&self.name.title);
                    }
                }
                Event::PtyWrite(text) => self.send(text.into_bytes()),
                Event::ClipboardStore(kind, text) => {
                    if let Some(clipboard) = &self.clipboard {
                        match kind {
                            ClipboardType::Clipboard => clipboard.store(text),
                            ClipboardType::Selection => clipboard.store_primary(text),
                        }
                    }
                }
                Event::ClipboardLoad(kind, format) => {
                    let text = self.clipboard.as_ref().and_then(|c| match kind {
                        ClipboardType::Clipboard => c.load().ok(),
                        ClipboardType::Selection => c.load_primary().ok(),
                    });
                    self.send(format(&text.unwrap_or_default()).into_bytes());
                }
                Event::ColorRequest(index, format) => {
                    let color = self.term.colors()[index].unwrap_or(self.palette[index]);
                    self.send(format(color).into_bytes());
                }
                Event::TextAreaSizeRequest(format) => {
                    let cell = self.glyphs.fonts.cell();
                    self.send(
                        format(WindowSize {
                            num_lines: self.layout.lines as u16,
                            num_cols: self.layout.columns as u16,
                            cell_width: cell.width as u16,
                            cell_height: cell.height as u16,
                        })
                        .into_bytes(),
                    );
                }
                Event::Bell => {
                    if let Some(window) = &self.window
                        && !self.focused
                    {
                        window.request_user_attention(Some(UserAttentionType::Informational));
                    }
                }
                _ => {}
            }
        }
    }

    fn draw(&mut self) {
        let Some(window) = self.window.clone() else {
            return;
        };
        let look = frame::Look {
            palette: &self.palette,
            selection: self.look.selection,
            focused: self.focused,
        };
        frame::build(
            &mut self.frame,
            self.term.renderable_content(),
            &look,
            &mut self.glyphs,
            self.layout,
        );
        self.term.reset_damage();
        match &mut self.renderer {
            Some(Renderer::Gpu(gpu)) => gpu.draw(&window, &self.frame, &mut self.glyphs.atlas),
            Some(Renderer::Cpu(cpu)) => {
                window.pre_present_notify();
                if let Err(error) = cpu.draw(&self.frame, &self.glyphs.atlas) {
                    tracing::warn!("{error}");
                }
            }
            None => {}
        }
    }

    /// The cell under the pointer, in grid coordinates, and which half of it.
    fn cell_at(&self, position: PhysicalPosition<f64>) -> (Point, Side) {
        let cell = self.glyphs.fonts.cell();
        let pad = f64::from(self.layout.padding);
        let x = (position.x - pad).max(0.0);
        let y = (position.y - pad).max(0.0);
        let column = ((x / f64::from(cell.width)) as usize).min(self.layout.columns - 1);
        let line = ((y / f64::from(cell.height)) as usize).min(self.layout.lines - 1);
        let side = if x % f64::from(cell.width) < f64::from(cell.width) / 2.0 {
            Side::Left
        } else {
            Side::Right
        };
        let offset = self.term.grid().display_offset();
        (
            viewport_to_point(offset, Point::new(line, Column(column))),
            side,
        )
    }

    /// The pointer's cell as a mouse report wants it: viewport line and
    /// column.
    fn report_at(&self) -> (usize, usize) {
        let (point, _) = self.cell_at(self.pointer);
        let offset = self.term.grid().display_offset() as i32;
        (point.column.0, (point.line.0 + offset).max(0) as usize)
    }

    /// Whether the program wants the mouse; Shift keeps it for selecting.
    fn reporting(&self) -> bool {
        self.term.mode().intersects(TermMode::MOUSE_MODE) && !self.modifiers.shift_key()
    }

    fn mouse_button(&mut self, state: ElementState, button: MouseButton) {
        let button = match button {
            MouseButton::Left => Button::Left,
            MouseButton::Middle => Button::Middle,
            MouseButton::Right => Button::Right,
            _ => return,
        };
        let pressed = state == ElementState::Pressed;
        self.held = pressed.then_some(button);
        if self.reporting() {
            let (column, line) = self.report_at();
            if let Some(bytes) = input::mouse(
                button,
                pressed,
                false,
                column,
                line,
                self.modifiers,
                *self.term.mode(),
            ) {
                self.send(bytes);
            }
            return;
        }
        match (button, pressed) {
            (Button::Left, true) => {
                let (point, side) = self.cell_at(self.pointer);
                let now = Instant::now();
                let count = match self.last_click {
                    Some((at, p, n)) if p == point && now.duration_since(at).as_millis() < 400 => {
                        n % 3 + 1
                    }
                    _ => 1,
                };
                self.last_click = Some((now, point, count));
                let kind = match count {
                    1 => SelectionType::Simple,
                    2 => SelectionType::Semantic,
                    _ => SelectionType::Lines,
                };
                self.term.selection = Some(Selection::new(kind, point, side));
                self.redraw();
            }
            (Button::Left, false) => {
                // What is selected becomes the primary selection, which a
                // middle click pastes, here or in any other app.
                if let (Some(text), Some(clipboard)) =
                    (self.term.selection_to_string(), &self.clipboard)
                    && !text.is_empty()
                {
                    clipboard.store_primary(text);
                }
            }
            (Button::Middle, true) => {
                if let Some(text) = self.clipboard.as_ref().and_then(|c| c.load_primary().ok()) {
                    self.send(input::paste(&text, *self.term.mode()));
                }
            }
            _ => {}
        }
    }

    fn pointer_moved(&mut self, position: PhysicalPosition<f64>) {
        let before = self.cell_at(self.pointer).0;
        self.pointer = position;
        let (point, side) = self.cell_at(position);
        if self.reporting() {
            let mode = *self.term.mode();
            let moved = point != before;
            let button = self.held.unwrap_or(Button::Left);
            // Motion with no button held is only for programs asking for
            // every motion (1003); with one held, for drag reports too.
            let wanted = self.held.is_some() || mode.contains(TermMode::MOUSE_MOTION);
            if moved && wanted {
                let (column, line) = self.report_at();
                let code = if self.held.is_some() {
                    button
                } else {
                    Button::Left
                };
                if let Some(mut bytes) =
                    input::mouse(code, true, true, column, line, self.modifiers, mode)
                {
                    if self.held.is_none() {
                        // Motion without a button reports button 3.
                        patch_no_button(&mut bytes);
                    }
                    self.send(bytes);
                }
            }
            return;
        }
        if self.held == Some(Button::Left)
            && let Some(selection) = &mut self.term.selection
        {
            selection.update(point, side);
            self.redraw();
        }
    }

    fn wheel(&mut self, delta: MouseScrollDelta) {
        let cell = f64::from(self.glyphs.fonts.cell().height);
        self.scrolled += match delta {
            MouseScrollDelta::LineDelta(_, y) => f64::from(y) * 3.0,
            MouseScrollDelta::PixelDelta(p) => p.y / cell,
        };
        let lines = self.scrolled.trunc() as i32;
        self.scrolled -= f64::from(lines);
        if lines == 0 {
            return;
        }
        let mode = *self.term.mode();
        if self.reporting() {
            let (column, line) = self.report_at();
            let button = if lines > 0 {
                Button::WheelUp
            } else {
                Button::WheelDown
            };
            for _ in 0..lines.unsigned_abs() {
                if let Some(bytes) =
                    input::mouse(button, true, false, column, line, self.modifiers, mode)
                {
                    self.send(bytes);
                }
            }
        } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
            // A full-screen program without mouse reporting (less, man)
            // scrolls with the cursor keys.
            let key = Key::Named(if lines > 0 {
                NamedKey::ArrowUp
            } else {
                NamedKey::ArrowDown
            });
            for _ in 0..lines.unsigned_abs() {
                if let Some(bytes) = input::encode(&key, None, ModifiersState::empty(), mode) {
                    self.send(bytes);
                }
            }
        } else {
            self.term.scroll_display(Scroll::Delta(lines));
            self.redraw();
        }
    }

    /// The window's own keys: copy, paste, scrolling back, and the font
    /// size. `true` when `event` was one of them.
    fn shortcut(&mut self, event: &KeyEvent) -> bool {
        let mods = self.modifiers;
        let ctrl_shift = mods.control_key() && mods.shift_key();
        let character = match &event.logical_key {
            Key::Character(c) => c.to_lowercase(),
            _ => String::new(),
        };
        match (&event.logical_key, character.as_str()) {
            (_, "c") if ctrl_shift => {
                if let (Some(text), Some(clipboard)) =
                    (self.term.selection_to_string(), &self.clipboard)
                {
                    clipboard.store(text);
                }
            }
            (_, "v") if ctrl_shift => {
                if let Some(text) = self.clipboard.as_ref().and_then(|c| c.load().ok()) {
                    self.send(input::paste(&text, *self.term.mode()));
                }
            }
            (Key::Named(NamedKey::Insert), _) if mods.shift_key() => {
                if let Some(text) = self.clipboard.as_ref().and_then(|c| c.load_primary().ok()) {
                    self.send(input::paste(&text, *self.term.mode()));
                }
            }
            (
                Key::Named(
                    key @ (NamedKey::PageUp | NamedKey::PageDown | NamedKey::Home | NamedKey::End),
                ),
                _,
            ) if mods.shift_key() && !self.term.mode().contains(TermMode::ALT_SCREEN) => {
                self.term.scroll_display(match key {
                    NamedKey::PageUp => Scroll::PageUp,
                    NamedKey::PageDown => Scroll::PageDown,
                    NamedKey::Home => Scroll::Top,
                    _ => Scroll::Bottom,
                });
                self.redraw();
            }
            // foot's bindings, which the terminal it replaces taught.
            (_, "+" | "=") if mods.control_key() => self.set_zoom(self.zoom * 1.1),
            (_, "-") if mods.control_key() => self.set_zoom(self.zoom / 1.1),
            (_, "0") if mods.control_key() => self.set_zoom(1.0),
            _ => return false,
        }
        true
    }

    fn set_zoom(&mut self, zoom: f32) {
        self.zoom = zoom.clamp(0.3, 4.0);
        self.glyphs.resize(self.font_size());
        self.relayout();
    }

    fn key(&mut self, event: KeyEvent) {
        if event.state != ElementState::Pressed || self.shortcut(&event) {
            return;
        }
        let Some(bytes) = input::encode(
            &event.logical_key,
            event.text.as_deref(),
            self.modifiers,
            *self.term.mode(),
        ) else {
            return;
        };
        // Typing brings the view back to the prompt and ends a selection,
        // as every terminal does.
        if self.term.grid().display_offset() != 0 {
            self.term.scroll_display(Scroll::Bottom);
            self.redraw();
        }
        if self.term.selection.take().is_some() {
            self.redraw();
        }
        self.send(bytes);
    }

    fn open(&mut self, event_loop: &ActiveEventLoop) {
        // The first window is sized for 80×24 at the theme's size; the cell
        // is measured at the logical size, before the scale factor is known.
        let cell = self.glyphs.fonts.cell();
        let size = LogicalSize::new(
            f64::from(80 * cell.width) + 2.0 * PADDING,
            f64::from(24 * cell.height) + 2.0 * PADDING,
        );
        let attributes = with_app_id(
            WindowAttributes::default()
                .with_title(&self.name.title)
                .with_inner_size(size),
            &self.name.app_id,
        );
        let (window, renderer) = match self.choice {
            Choice::Software => match event_loop.create_window(attributes) {
                Ok(window) => (Rc::new(window), None),
                Err(error) => return fail(event_loop, &error),
            },
            choice => {
                match gpu::Renderer::new(event_loop, attributes.clone(), choice == Choice::Gpu) {
                    Ok((window, gpu)) => {
                        tracing::info!("drawing on the GPU with {}", gpu.name);
                        (window, Some(Renderer::Gpu(Box::new(gpu))))
                    }
                    Err((window, error)) => {
                        tracing::info!("{error}; drawing in software");
                        match window
                            .map(Ok)
                            .unwrap_or_else(|| event_loop.create_window(attributes).map(Rc::new))
                        {
                            Ok(window) => (window, None),
                            Err(error) => return fail(event_loop, &error),
                        }
                    }
                }
            }
        };
        let renderer = match renderer {
            Some(renderer) => renderer,
            None => match cpu::Renderer::new(window.clone()) {
                Ok(cpu) => Renderer::Cpu(cpu),
                Err(error) => return fail(event_loop, &error),
            },
        };
        self.clipboard = clipboard(&window);
        self.scale = window.scale_factor();
        self.glyphs.resize(self.font_size());
        self.renderer = Some(renderer);
        self.window = Some(window);
        self.relayout();
    }
}

/// Logs why there is no window and ends the event loop.
fn fail(event_loop: &ActiveEventLoop, error: &dyn std::fmt::Display) {
    tracing::error!("no window: {error}");
    event_loop.exit();
}

/// Turns a motion report's button into "none held" (3), as xterm sends it.
fn patch_no_button(bytes: &mut Vec<u8>) {
    if bytes.starts_with(b"\x1b[<") {
        // SGR: the code is the first number; 32 (motion) + 0 → 35.
        let text = String::from_utf8_lossy(&bytes[3..]).into_owned();
        if let Some((code, rest)) = text.split_once(';')
            && let Ok(code) = code.parse::<u8>()
        {
            *bytes = format!("\x1b[<{};{rest}", (code & !3) | 3).into_bytes();
        }
    } else if bytes.len() == 6 {
        bytes[3] = ((bytes[3] - 32) & !3 | 3) + 32;
    }
}

/// The window's app ID on Wayland (its WM_CLASS on X11), which the
/// desktop entry names, so the shell shows the entry's name and icon.
fn with_app_id(attributes: WindowAttributes, app_id: &str) -> WindowAttributes {
    use winit::platform::{wayland::WindowAttributesExtWayland, x11::WindowAttributesExtX11};
    let attributes = WindowAttributesExtWayland::with_name(attributes, app_id, "");
    WindowAttributesExtX11::with_name(attributes, app_id, app_id)
}

/// The Wayland clipboard; none on X11, where derisk does not run.
fn clipboard(window: &Window) -> Option<smithay_clipboard::Clipboard> {
    match window.display_handle().ok()?.as_raw() {
        RawDisplayHandle::Wayland(handle) => {
            // SAFETY: the display is winit's connection, which lives as long
            // as the event loop; the clipboard is dropped with the App,
            // before the event loop is.
            Some(unsafe { smithay_clipboard::Clipboard::new(handle.display.as_ptr()) })
        }
        _ => None,
    }
}

impl ApplicationHandler<Wake> for App<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            self.open(event_loop);
        }
    }

    fn new_events(&mut self, _: &ActiveEventLoop, cause: StartCause) {
        // A synchronised update that never ended: draw what there is.
        if let StartCause::ResumeTimeReached { .. } = cause
            && self
                .parser
                .sync_timeout()
                .sync_timeout()
                .is_some_and(|at| at <= Instant::now())
        {
            self.parser.stop_sync(&mut self.term);
            self.handle_events();
            self.redraw();
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, wake: Wake) {
        match wake {
            Wake::Output => self.take_output(),
            Wake::Exited => {
                self.take_output();
                event_loop.exit();
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(match self.parser.sync_timeout().sync_timeout() {
            Some(at) => ControlFlow::WaitUntil(at),
            None => ControlFlow::Wait,
        });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::Resized(_) => self.relayout(),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.scale = scale_factor;
                self.glyphs.resize(self.font_size());
                self.relayout();
            }
            WindowEvent::Focused(focused) => {
                self.focused = focused;
                self.term.is_focused = focused;
                if self.term.mode().contains(TermMode::FOCUS_IN_OUT) {
                    self.send(if focused {
                        b"\x1b[I".to_vec()
                    } else {
                        b"\x1b[O".to_vec()
                    });
                }
                // Settings is used in another window; coming back is when a
                // changed theme is worth looking for.
                if focused {
                    self.follow_theme();
                }
                self.redraw();
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::KeyboardInput { event, .. } => self.key(event),
            WindowEvent::CursorMoved { position, .. } => self.pointer_moved(position),
            WindowEvent::MouseInput { state, button, .. } => self.mouse_button(state, button),
            WindowEvent::MouseWheel { delta, .. } => self.wheel(delta),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::grid::Dimensions;

    use super::*;

    #[test]
    fn motion_without_a_button_reports_button_three() {
        let mut sgr = b"\x1b[<32;4;2M".to_vec();
        patch_no_button(&mut sgr);
        assert_eq!(sgr, b"\x1b[<35;4;2M".to_vec());
        let mut normal = vec![0x1b, b'[', b'M', 32 + 32, 40, 40];
        patch_no_button(&mut normal);
        assert_eq!(normal[3], 32 + 35);
    }

    #[test]
    fn dimensions_follow_resizes() {
        let mut term = Term::new(
            Config::default(),
            &TermSize::new(80, 24),
            Listener::default(),
        );
        term.resize(TermSize::new(100, 30));
        assert_eq!((term.columns(), term.screen_lines()), (100, 30));
    }
}
