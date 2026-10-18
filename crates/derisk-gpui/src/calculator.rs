//! The Calculator as a GPUI view.
//!
//! The arithmetic, history and keypad come from `derisk-calculator`, so this
//! version and the egui one compute the same thing; only the drawing differs.

use derisk_calculator::{CalculatorApp, KEYS, evaluate, format_number};
use gpui::{
    Context, Entity, FontWeight, IntoElement, ParentElement, Render, Styled, Subscription, Window,
    div, prelude::*, px,
};
use mcsapi_components_gpui::{Button, ButtonSize, ButtonVariant, Input, TextInput, Tokens};

use crate::run::DesktopTheme;

/// The Calculator window's root view.
pub struct Calculator {
    app: CalculatorApp,
    input: Entity<TextInput>,
    _edits: Subscription,
    _enter: Subscription,
}

impl Calculator {
    /// A calculator with an empty expression, focused for typing.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new(cx).placeholder("2 × (3 + 4)"));
        // Typing goes to the field; the model follows it so the preview and
        // the keypad see what was typed.
        let edits = cx.observe(&input, |this: &mut Self, input, cx| {
            let text = input.read(cx).text();
            if text != this.app.input {
                this.app.input = text.to_owned();
                cx.notify();
            }
        });
        // Enter submits. The field binds Enter to its own action, which
        // gpui dispatches instead of key listeners, so catch the keystroke
        // before bindings are matched. This process has only this window.
        let this = cx.entity().downgrade();
        let enter = cx.intercept_keystrokes(move |event, _, cx| {
            if event.keystroke.key == "enter" && !event.keystroke.modifiers.modified() {
                let _ = this.update(cx, |this, cx| {
                    this.app.submit();
                    this.sync(cx);
                });
                cx.stop_propagation();
            }
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        // Wayland's xdg_toplevel title, which the shell shows in its top bar.
        window.set_window_title("Calculator");
        Self {
            app: CalculatorApp::default(),
            input,
            _edits: edits,
            _enter: enter,
        }
    }

    /// Applies a keypad key, then shows the model's expression in the field.
    fn press(&mut self, key: &str, cx: &mut Context<Self>) {
        self.app.press(key);
        self.sync(cx);
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        let text = self.app.input.clone();
        self.input.update(cx, |input, cx| {
            if input.text() != text {
                input.set_text(text, cx);
            }
        });
        cx.notify();
    }

    fn preview(&self) -> (String, bool) {
        match self.app.error() {
            Some(error) => (error.to_string(), true),
            None => match evaluate(&self.app.input, self.app.ans()) {
                Ok(value) => (format!("= {}", format_number(value)), false),
                Err(_) => (String::new(), false),
            },
        }
    }
}

/// Below this width the history moves from beside the keypad to under it,
/// as the egui Calculator's does.
const NARROW: f32 = 560.0;

/// One keypad key, drawn from the tokens by what it does, as the egui
/// Calculator draws it: digits on the secondary fill, operators on the
/// field's, C in the destructive colour, = the one key in the accent.
fn key(t: &Tokens, index: usize, key: &'static str) -> gpui::Stateful<gpui::Div> {
    let digit = key == "." || key == "ans" || key.chars().all(|c| c.is_ascii_digit());
    let (fill, ink) = match key {
        "=" => (t.primary, t.primary_foreground),
        "C" => (t.field, t.destructive),
        _ if digit => (t.secondary, t.foreground),
        _ => (t.field, t.foreground),
    };
    let hover = if key == "=" {
        t.primary.opacity(0.85)
    } else {
        t.hover
    };
    div()
        .id(("key", index))
        .flex_1()
        .h_full()
        .min_h(px(44.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(t.radius)
        .bg(fill)
        .text_color(ink)
        .text_size(px(20.0))
        .hover(move |style| style.bg(hover))
        .cursor_pointer()
        .child(if key == "⬅" { "⌫" } else { key })
}

impl Render for Calculator {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Tokens::get(cx);
        let fonts = cx
            .try_global::<DesktopTheme>()
            .map(|theme| theme.0.fonts.clone())
            .unwrap_or_default();
        let (preview, is_error) = self.preview();
        let narrow = window.viewport_size().width < px(NARROW);
        let card = || {
            div()
                .bg(t.card)
                .border_1()
                .border_color(t.border)
                .rounded(t.card_radius())
        };

        let display = card()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .px(px(20.0))
            .py(px(16.0))
            .child(Input::new(&self.input))
            .child(
                div()
                    .h(px(26.0))
                    .flex()
                    .justify_end()
                    .text_size(px(18.0))
                    .text_color(if is_error {
                        t.destructive
                    } else {
                        t.muted_foreground
                    })
                    .child(preview),
            );

        let mut keypad = div().flex().flex_col().flex_1().gap(px(8.0));
        for (r, row) in KEYS.iter().enumerate() {
            let mut line = div().flex().flex_1().gap(px(8.0));
            for (c, &k) in row.iter().enumerate() {
                line = line.child(
                    key(&t, r * row.len() + c, k)
                        .on_click(cx.listener(move |this, _, _, cx| this.press(k, cx))),
                );
            }
            keypad = keypad.child(line);
        }

        let mut entries = div()
            .id("history")
            .flex()
            .flex_col()
            .gap(px(2.0))
            .flex_1()
            .overflow_y_scroll();
        for (index, (expression, value)) in self.app.history.iter().enumerate() {
            let reuse = expression.clone();
            entries = entries.child(
                div()
                    .id(("entry", index))
                    .flex()
                    .flex_col()
                    .items_end()
                    .px(px(10.0))
                    .py(px(6.0))
                    .rounded(t.radius)
                    .hover(|style| style.bg(t.hover))
                    .cursor_pointer()
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(t.muted_foreground)
                            .child(expression.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(17.0))
                            .child(format!("= {}", format_number(*value))),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.app.input = reuse.clone();
                        this.sync(cx);
                    })),
            );
        }
        if self.app.history.is_empty() {
            entries = entries.child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .py(px(24.0))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Nothing yet"))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(t.muted_foreground)
                            .child("Results show up here."),
                    ),
            );
        }
        let history = card()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .p(px(6.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .pl(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(16.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("History"),
                    )
                    .child(
                        Button::new("Clear")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Sm)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.app.history.clear();
                                cx.notify();
                            })),
                    ),
            )
            .child(entries);

        let page = div()
            .size_full()
            .flex()
            .gap(px(14.0))
            .bg(t.background)
            .text_color(t.foreground)
            .font_family(fonts.sans.clone())
            .text_size(px(fonts.size));
        let calculator = div()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(display)
            .child(keypad);
        if narrow {
            // A phone has no width for a side column; the history goes
            // under the keypad instead, a few lines tall.
            page.flex_col()
                .p(px(12.0))
                .child(calculator.flex_1())
                .child(history.h(px(168.0)))
        } else {
            page.px(px(32.0))
                .py(px(28.0))
                .child(calculator.flex_1().max_w(px(560.0)))
                .child(history.w(px(260.0)).flex_none())
        }
    }
}
