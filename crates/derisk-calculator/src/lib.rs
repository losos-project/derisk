//! derisk Calculator: a keypad and an expression line with history.
//!
//! [`evaluate`] supports `+ - * / % ^`, parentheses, unary minus, the
//! constants `pi` and `e`, `ans` (the previous result), and the functions
//! `sqrt abs ln log sin cos tan asin acos atan floor ceil round`. Angles are
//! in radians.
//!
//! ```
//! assert_eq!(derisk_calculator::evaluate("2 + 3 * 4 ^ 2", 0.0), Ok(50.0));
//! assert_eq!(derisk_calculator::evaluate("-(ans - 1) / 2", 7.0), Ok(-3.0));
//! assert!(derisk_calculator::evaluate("1 / 0", 0.0).is_err());
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::fmt;

use mcsapi_components::{Button, ButtonSize, ButtonVariant, Tokens};
use mcsapi_ui::{App, Theme, egui};

/// Why an expression could not be evaluated.
#[derive(Clone, Debug, PartialEq, thiserror::Error, miette::Diagnostic)]
pub enum Error {
    /// A character that is not part of any token.
    #[error("unexpected `{0}`")]
    #[diagnostic(code(derisk_calculator::unexpected_char))]
    UnexpectedChar(char),
    /// The expression ended too early.
    #[error("incomplete expression")]
    #[diagnostic(code(derisk_calculator::unexpected_end))]
    UnexpectedEnd,
    /// A token in the wrong place.
    #[error("unexpected `{0}`")]
    #[diagnostic(code(derisk_calculator::unexpected_token))]
    UnexpectedToken(String),
    /// A name that is neither a constant nor a function.
    #[error("unknown name `{0}`")]
    #[diagnostic(code(derisk_calculator::unknown_name))]
    UnknownName(String),
    /// Division or remainder by zero.
    #[error("division by zero")]
    #[diagnostic(code(derisk_calculator::division_by_zero))]
    DivisionByZero,
    /// The result is not a finite number.
    #[error("result is not a finite number")]
    #[diagnostic(code(derisk_calculator::not_finite))]
    NotFinite,
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(f64),
    Name(String),
    Op(char),
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(n) => n.fmt(f),
            Self::Name(n) => f.write_str(n),
            Self::Op(c) => write!(f, "{c}"),
        }
    }
}

fn tokenize(text: &str) -> Result<Vec<Token>, Error> {
    let mut tokens = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some(&(start, c)) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c.is_ascii_digit() || c == '.' {
            let mut end = start;
            while let Some(&(i, c)) = chars.peek() {
                if c.is_ascii_digit() || c == '.' {
                    end = i + c.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
            let number = text[start..end]
                .parse()
                .map_err(|_| Error::UnexpectedToken(text[start..end].into()))?;
            tokens.push(Token::Number(number));
        } else if c.is_alphabetic() {
            let mut end = start;
            while let Some(&(i, c)) = chars.peek() {
                if c.is_alphanumeric() {
                    end = i + c.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
            tokens.push(Token::Name(text[start..end].to_lowercase()));
        } else {
            let op = match c {
                '×' => '*',
                '÷' => '/',
                '−' => '-',
                c if "+-*/%^()".contains(c) => c,
                c => return Err(Error::UnexpectedChar(c)),
            };
            tokens.push(Token::Op(op));
            chars.next();
        }
    }
    Ok(tokens)
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    ans: f64,
}

impl Parser<'_> {
    fn peek_op(&self) -> Option<char> {
        match self.tokens.get(self.pos) {
            Some(Token::Op(c)) => Some(*c),
            _ => None,
        }
    }

    fn next(&mut self) -> Result<&Token, Error> {
        let token = self.tokens.get(self.pos).ok_or(Error::UnexpectedEnd)?;
        self.pos += 1;
        Ok(token)
    }

    // sum := product (('+' | '-') product)*
    fn sum(&mut self) -> Result<f64, Error> {
        let mut value = self.product()?;
        while let Some(op @ ('+' | '-')) = self.peek_op() {
            self.pos += 1;
            let rhs = self.product()?;
            value = if op == '+' { value + rhs } else { value - rhs };
        }
        Ok(value)
    }

    // product := unary (('*' | '/' | '%') unary)*
    fn product(&mut self) -> Result<f64, Error> {
        let mut value = self.unary()?;
        while let Some(op @ ('*' | '/' | '%')) = self.peek_op() {
            self.pos += 1;
            let rhs = self.unary()?;
            value = match op {
                '*' => value * rhs,
                _ if rhs == 0.0 => return Err(Error::DivisionByZero),
                '/' => value / rhs,
                _ => value % rhs,
            };
        }
        Ok(value)
    }

    // unary := '-' unary | power
    fn unary(&mut self) -> Result<f64, Error> {
        match self.peek_op() {
            Some('-') => {
                self.pos += 1;
                Ok(-self.unary()?)
            }
            Some('+') => {
                self.pos += 1;
                self.unary()
            }
            _ => self.power(),
        }
    }

    // power := atom ('^' unary)?   (right-associative; -2^2 = -4)
    fn power(&mut self) -> Result<f64, Error> {
        let base = self.atom()?;
        if self.peek_op() == Some('^') {
            self.pos += 1;
            return Ok(base.powf(self.unary()?));
        }
        Ok(base)
    }

    fn atom(&mut self) -> Result<f64, Error> {
        match self.next()?.clone() {
            Token::Number(n) => Ok(n),
            Token::Op('(') => {
                let value = self.sum()?;
                self.expect(')')?;
                Ok(value)
            }
            Token::Name(name) => match name.as_str() {
                "pi" | "π" => Ok(std::f64::consts::PI),
                "e" => Ok(std::f64::consts::E),
                "ans" => Ok(self.ans),
                _ => {
                    let function = function(&name).ok_or(Error::UnknownName(name))?;
                    self.expect('(')?;
                    let argument = self.sum()?;
                    self.expect(')')?;
                    Ok(function(argument))
                }
            },
            token => Err(Error::UnexpectedToken(token.to_string())),
        }
    }

    fn expect(&mut self, op: char) -> Result<(), Error> {
        match self.next()? {
            Token::Op(c) if *c == op => Ok(()),
            token => Err(Error::UnexpectedToken(token.to_string())),
        }
    }
}

fn function(name: &str) -> Option<fn(f64) -> f64> {
    Some(match name {
        "sqrt" => f64::sqrt,
        "abs" => f64::abs,
        "ln" => f64::ln,
        "log" => f64::log10,
        "sin" => f64::sin,
        "cos" => f64::cos,
        "tan" => f64::tan,
        "asin" => f64::asin,
        "acos" => f64::acos,
        "atan" => f64::atan,
        "floor" => f64::floor,
        "ceil" => f64::ceil,
        "round" => f64::round,
        _ => return None,
    })
}

/// Evaluates an expression; `ans` refers to `previous`.
pub fn evaluate(expression: &str, previous: f64) -> Result<f64, Error> {
    let tokens = tokenize(expression)?;
    let mut parser = Parser {
        tokens: &tokens,
        pos: 0,
        ans: previous,
    };
    let value = parser.sum()?;
    if let Some(token) = tokens.get(parser.pos) {
        return Err(Error::UnexpectedToken(token.to_string()));
    }
    if value.is_finite() {
        // Avoid showing "-0".
        Ok(if value == 0.0 { 0.0 } else { value })
    } else {
        Err(Error::NotFinite)
    }
}

/// Formats a result with up to 12 significant digits and no trailing zeros.
pub fn format_number(value: f64) -> String {
    if value != 0.0 && !(1e-6..1e15).contains(&value.abs()) {
        return format!("{value:.6e}");
    }
    let integer_digits = if value.abs() < 1.0 {
        0
    } else {
        value.abs().log10().floor() as usize + 1
    };
    let digits = 12 - integer_digits.min(12);
    let text = format!("{value:.digits$}");
    let text = if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.')
    } else {
        &text
    };
    if text == "-0" {
        "0".into()
    } else {
        text.into()
    }
}

/// The keypad, row by row, shared by every toolkit's Calculator.
pub const KEYS: [[&str; 5]; 5] = [
    ["C", "(", ")", "%", "⬅"],
    ["7", "8", "9", "÷", "√"],
    ["4", "5", "6", "×", "^"],
    ["1", "2", "3", "−", "pi"],
    ["0", ".", "ans", "+", "="],
];

/// The Calculator app.
#[derive(Debug, Default)]
pub struct CalculatorApp {
    /// The expression being typed.
    pub input: String,
    /// Evaluated expressions and results, oldest first.
    pub history: Vec<(String, f64)>,
    error: Option<Error>,
}

impl CalculatorApp {
    /// The previous result, or zero.
    pub fn ans(&self) -> f64 {
        self.history.last().map_or(0.0, |(_, value)| *value)
    }

    /// The error from the last [`CalculatorApp::submit`], if any.
    pub fn error(&self) -> Option<&Error> {
        self.error.as_ref()
    }

    /// Evaluates the input, records it, and replaces it with the result.
    pub fn submit(&mut self) {
        if self.input.trim().is_empty() {
            return;
        }
        match evaluate(&self.input, self.ans()) {
            Ok(value) => {
                self.history.push((std::mem::take(&mut self.input), value));
                self.input = format_number(value);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }

    /// Applies one keypad key from [`KEYS`]: `C` clears, `⬅` deletes,
    /// `=` submits, `√` opens `sqrt(`, anything else is typed.
    pub fn press(&mut self, key: &str) {
        match key {
            "C" => {
                self.input.clear();
                self.error = None;
            }
            "⬅" => {
                self.input.pop();
            }
            "=" => self.submit(),
            "√" => self.input.push_str("sqrt("),
            key => self.input.push_str(key),
        }
    }
}

/// What a key does, which sets how it is drawn.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    /// A digit, the point or `ans`: what the number is made of.
    Digit,
    /// An operator, a bracket or a function.
    Operator,
    /// `C`, which throws the input away.
    Clear,
    /// `=`, the one key in the accent.
    Equals,
}

impl Kind {
    fn of(key: &str) -> Self {
        match key {
            "=" => Self::Equals,
            "C" => Self::Clear,
            "ans" | "." => Self::Digit,
            key if key.chars().all(|c| c.is_ascii_digit()) => Self::Digit,
            _ => Self::Operator,
        }
    }
}

/// Width of the history beside the keypad.
const HISTORY: f32 = 260.0;

/// Gap between keys.
const GAP: f32 = 8.0;

/// One keypad key: a rounded face in the theme's colours for its
/// [`Kind`], with the key's text or, for backspace, an icon.
fn key(ui: &mut egui::Ui, tokens: &Tokens, size: egui::Vec2, key: &str) -> egui::Response {
    let kind = Kind::of(key);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let name = if key == "⬅" { "Backspace" } else { key };
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name));
    if ui.is_rect_visible(rect) {
        let hovered = response.hovered();
        let (fill, ink) = match kind {
            Kind::Equals => (
                if hovered {
                    tokens.primary.gamma_multiply(0.85)
                } else {
                    tokens.primary
                },
                tokens.primary_foreground,
            ),
            Kind::Digit => (
                if hovered {
                    tokens.hover
                } else {
                    tokens.secondary
                },
                tokens.foreground,
            ),
            Kind::Operator => (
                if hovered { tokens.hover } else { tokens.field },
                tokens.foreground,
            ),
            Kind::Clear => (
                if hovered { tokens.hover } else { tokens.field },
                tokens.destructive,
            ),
        };
        let painter = ui.painter();
        painter.rect_filled(rect, tokens.control_radius(), fill);
        if response.has_focus() {
            painter.rect_stroke(
                rect,
                tokens.control_radius(),
                tokens.ring_stroke(),
                egui::StrokeKind::Outside,
            );
        }
        if key == "⬅" {
            let icon = egui::Rect::from_center_size(rect.center(), egui::vec2(20.0, 20.0));
            derisk_icons::paint(painter, icon, "edit-clear", ink);
        } else {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                key,
                egui::FontId::proportional(20.0),
                ink,
            );
        }
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

impl CalculatorApp {
    /// The expression being typed and, under it, what it comes to or why
    /// it does not.
    fn display(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        derisk_frame::card(ui, tokens, |ui| {
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.input)
                    .font(egui::FontId::proportional(30.0))
                    .hint_text("2 × (3 + 4)")
                    .horizontal_align(egui::Align::RIGHT)
                    .frame(egui::Frame::NONE)
                    .desired_width(f32::INFINITY),
            );
            if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                self.submit();
                field.request_focus();
            }
            let preview = match &self.error {
                Some(error) => egui::RichText::new(error.to_string()).color(tokens.destructive),
                None => match evaluate(&self.input, self.ans()) {
                    Ok(value) => egui::RichText::new(format!("= {}", format_number(value)))
                        .color(tokens.muted_foreground),
                    Err(_) => egui::RichText::new(" "),
                },
            };
            // Top down rather than right to left, which would centre the
            // line in all the height below it.
            ui.with_layout(egui::Layout::top_down(egui::Align::Max), |ui| {
                ui.label(preview.size(18.0));
            });
        });
        ui.add_space(4.0);
    }

    /// The keypad, its keys sharing the room left in the column.
    fn keypad(&mut self, ui: &mut egui::Ui, tokens: &Tokens, height: f32) {
        let columns = KEYS[0].len() as f32;
        let rows = KEYS.len() as f32;
        let size = egui::vec2(
            ((ui.available_width() - (columns - 1.0) * GAP) / columns).max(44.0),
            ((height - (rows - 1.0) * GAP) / rows).clamp(44.0, 100.0),
        );
        let mut pressed = None;
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(GAP, GAP);
            for row in KEYS {
                ui.horizontal(|ui| {
                    for k in row {
                        if key(ui, tokens, size, k).clicked() {
                            pressed = Some(k);
                        }
                    }
                });
            }
        });
        if let Some(k) = pressed {
            self.press(k);
        }
    }

    /// Evaluated expressions, newest last; a click puts one back in the
    /// input.
    fn history(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        derisk_frame::Card::new()
            .tight()
            .fill()
            .show(ui, tokens, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new("History")
                            .font(egui::FontId::proportional(16.0))
                            .strong(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                Button::new("Clear")
                                    .variant(ButtonVariant::Ghost)
                                    .size(ButtonSize::Sm)
                                    .enabled(!self.history.is_empty()),
                            )
                            .clicked()
                        {
                            self.history.clear();
                        }
                    });
                });
                if self.history.is_empty() {
                    derisk_frame::empty(ui, tokens, "Nothing yet", "Results show up here.");
                    return;
                }
                let mut reuse = None;
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        for (expression, value) in &self.history {
                            let (rect, row) = derisk_frame::list_row(ui, tokens, 52.0, false);
                            let row = row.on_hover_text("Use again");
                            let inner = rect.shrink2(egui::vec2(10.0, 6.0));
                            let painter = ui.painter();
                            painter.text(
                                inner.right_top(),
                                egui::Align2::RIGHT_TOP,
                                expression,
                                tokens.small_font(),
                                tokens.muted_foreground,
                            );
                            painter.text(
                                inner.right_bottom(),
                                egui::Align2::RIGHT_BOTTOM,
                                format!("= {}", format_number(*value)),
                                egui::FontId::proportional(17.0),
                                tokens.foreground,
                            );
                            if row.clicked() {
                                reuse = Some(expression.clone());
                            }
                        }
                    });
                if let Some(expression) = reuse {
                    self.input = expression;
                }
            });
    }
}

impl App for CalculatorApp {
    fn title(&self) -> &str {
        "Calculator"
    }

    fn ui(&mut self, ui: &mut egui::Ui, theme: &Theme) {
        let tokens = derisk_frame::begin(ui, theme);
        derisk_frame::Page::new()
            .wide()
            .fixed()
            .show(ui, &tokens, |ui| {
                let width = ui.available_width();
                if width < derisk_frame::NARROW {
                    // A phone has no width for a side column; the history
                    // goes under the keypad instead, a few lines tall.
                    self.display(ui, &tokens);
                    let keys = (ui.available_height() - 180.0).max(5.0 * 44.0 + 4.0 * GAP);
                    self.keypad(ui, &tokens, keys);
                    ui.add_space(12.0);
                    self.history(ui, &tokens);
                    return;
                }
                let height = ui.available_height();
                let keypad = (width - HISTORY - derisk_frame::GUTTER).min(560.0);
                ui.horizontal_top(|ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(keypad, height),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            self.display(ui, &tokens);
                            let keys = ui.available_height();
                            self.keypad(ui, &tokens, keys);
                        },
                    );
                    ui.add_space(derisk_frame::GUTTER);
                    ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width(), height),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| self.history(ui, &tokens),
                    );
                });
            });
    }
}
