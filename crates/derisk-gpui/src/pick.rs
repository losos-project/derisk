//! The portal's pickers: a color, or an area, from a picture of the screen.
//!
//! The portal takes a screenshot through the session and runs
//! `derisk-gpui pick color` or `derisk-gpui pick area` on it. That opens one
//! fullscreen window showing the screenshot, frozen, so what the person
//! points at is exactly what the app will get. A click picks a color; a drag
//! picks an area, and Enter takes the whole screen. The answer is printed to
//! stdout in the screenshot's own pixels, `x y` or `x y width height`, and
//! the exit status says whether there is one: 0 picked, 1 cancelled with
//! Escape or by closing the window.

use std::{fmt, path::PathBuf, str::FromStr};

/// What to pick.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Mode {
    /// One pixel's color.
    #[default]
    Color,
    /// A rectangle.
    Area,
}

/// What `derisk-gpui pick` was asked.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Pick {
    /// Color or area.
    pub mode: Mode,
    /// The screenshot, a PNG.
    pub image: PathBuf,
    /// The asking app's name, said in the banner; empty for "An app".
    pub app: String,
}

impl Pick {
    /// Reads `color` or `area`, then `--image <path>` and `--app <name>`.
    pub fn from_args(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut args = args.into_iter();
        let mode = match args.next().as_deref() {
            Some("color") => Mode::Color,
            Some("area") => Mode::Area,
            Some(other) => return Err(format!("pick what? {other} is not color or area")),
            None => return Err("pick color or pick area".into()),
        };
        let mut pick = Self {
            mode,
            ..Self::default()
        };
        while let Some(flag) = args.next() {
            let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
            match flag.as_str() {
                "--image" => pick.image = value.into(),
                "--app" => pick.app = value,
                _ => return Err(format!("unknown option {flag}")),
            }
        }
        if pick.image.as_os_str().is_empty() {
            return Err("--image is required".into());
        }
        Ok(pick)
    }

    /// The line across the top of the screen.
    pub fn banner(&self) -> String {
        let app = if self.app.is_empty() {
            "An app"
        } else {
            &self.app
        };
        match self.mode {
            Mode::Color => format!("{app} wants a color from the screen. Click it, or press Esc."),
            Mode::Area => format!(
                "{app} wants a picture of the screen. Drag over the part to share, press Enter for all of it, or Esc."
            ),
        }
    }
}

/// What was picked, in the screenshot's pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Answer {
    /// A pixel, for a color.
    Point {
        /// Column.
        x: u32,
        /// Row.
        y: u32,
    },
    /// A rectangle, never empty.
    Area {
        /// Left column.
        x: u32,
        /// Top row.
        y: u32,
        /// Columns.
        width: u32,
        /// Rows.
        height: u32,
    },
}

impl fmt::Display for Answer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Point { x, y } => write!(f, "{x} {y}"),
            Self::Area {
                x,
                y,
                width,
                height,
            } => write!(f, "{x} {y} {width} {height}"),
        }
    }
}

impl FromStr for Answer {
    type Err = String;

    fn from_str(line: &str) -> Result<Self, String> {
        let numbers: Vec<u32> = line
            .split_whitespace()
            .map(|n| n.parse().map_err(|_| format!("not a number: {n}")))
            .collect::<Result<_, _>>()?;
        match numbers[..] {
            [x, y] => Ok(Self::Point { x, y }),
            [x, y, width, height] if width > 0 && height > 0 => Ok(Self::Area {
                x,
                y,
                width,
                height,
            }),
            _ => Err(format!("not a point or an area: {line}")),
        }
    }
}

/// The screenshot's pixel under `at`, a point in a window of `view` size
/// that shows the whole screenshot of `image` size stretched over it.
pub fn to_image(at: (f32, f32), view: (f32, f32), image: (u32, u32)) -> (u32, u32) {
    let axis = |at: f32, view: f32, size: u32| {
        if view <= 0.0 || size == 0 {
            return 0;
        }
        // Truncating picks the pixel the point falls inside.
        ((at / view * size as f32).max(0.0) as u32).min(size - 1)
    };
    (axis(at.0, view.0, image.0), axis(at.1, view.1, image.1))
}

/// The area between two corners a drag went from and to, in the
/// screenshot's pixels, including both corner pixels.
pub fn area(from: (u32, u32), to: (u32, u32)) -> Answer {
    let (x, y) = (from.0.min(to.0), from.1.min(to.1));
    Answer::Area {
        x,
        y,
        width: from.0.max(to.0) - x + 1,
        height: from.1.max(to.1) - y + 1,
    }
}

#[cfg(feature = "gpui")]
mod window {
    use std::{cell::Cell, rc::Rc};

    use gpui::{
        App, AppContext, Bounds, Context, CursorStyle, FocusHandle, Hsla, InteractiveElement,
        IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
        ObjectFit, ParentElement, Pixels, Point, Render, Styled, StyledImage, Window, WindowBounds,
        WindowKind, WindowOptions, div, img, point, px, rgb,
    };
    use mcsapi_components_gpui::Tokens;

    use super::{Answer, Mode, Pick, area, to_image};
    use crate::ThemeWatch;

    struct Picker {
        pick: Pick,
        /// The screenshot's pixels, for the color under the pointer.
        pixels: Option<image::RgbaImage>,
        size: (u32, u32),
        focus: FocusHandle,
        pointer: Option<Point<Pixels>>,
        pressed: Option<Point<Pixels>>,
        answer: Rc<Cell<Option<Answer>>>,
    }

    fn xy(p: Point<Pixels>) -> (f32, f32) {
        (f32::from(p.x), f32::from(p.y))
    }

    impl Picker {
        fn pixel(&self, at: Point<Pixels>, window: &Window) -> (u32, u32) {
            let view = window.viewport_size();
            to_image(
                xy(at),
                (f32::from(view.width), f32::from(view.height)),
                self.size,
            )
        }

        fn color(&self, at: Point<Pixels>, window: &Window) -> Option<[u8; 3]> {
            let (x, y) = self.pixel(at, window);
            let p = self.pixels.as_ref()?.get_pixel_checked(x, y)?;
            Some([p[0], p[1], p[2]])
        }

        fn finish(&self, answer: Option<Answer>, window: &mut Window) {
            self.answer.set(answer);
            window.remove_window();
        }
    }

    impl Render for Picker {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let t = Tokens::get(cx);
            let mut root = div()
                .id("picker")
                .size_full()
                .relative()
                .track_focus(&self.focus)
                .cursor(CursorStyle::Crosshair)
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, _| {
                    match event.keystroke.key.as_str() {
                        "escape" => this.finish(None, window),
                        "enter" if this.pick.mode == Mode::Area => {
                            let (w, h) = this.size;
                            this.finish(
                                Some(Answer::Area {
                                    x: 0,
                                    y: 0,
                                    width: w.max(1),
                                    height: h.max(1),
                                }),
                                window,
                            );
                        }
                        _ => {}
                    }
                }))
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                    this.pointer = Some(event.position);
                    cx.notify();
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, _, cx| {
                        this.pressed = Some(event.position);
                        this.pointer = Some(event.position);
                        cx.notify();
                    }),
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseUpEvent, window, cx| {
                        let to = this.pixel(event.position, window);
                        let answer = match this.pick.mode {
                            Mode::Color => Answer::Point { x: to.0, y: to.1 },
                            Mode::Area => {
                                let Some(pressed) = this.pressed.take() else {
                                    return;
                                };
                                // A click is not a drag: it would share a
                                // single pixel, which nobody means to.
                                let moved = event.position - pressed;
                                if f32::from(moved.x).abs() < 4.0 || f32::from(moved.y).abs() < 4.0
                                {
                                    cx.notify();
                                    return;
                                }
                                area(this.pixel(pressed, window), to)
                            }
                        };
                        this.finish(Some(answer), window);
                    }),
                )
                .child(
                    img(self.pick.image.as_path())
                        .absolute()
                        .size_full()
                        .object_fit(ObjectFit::Fill),
                );

            // The area being dragged over, outlined in the accent color.
            if self.pick.mode == Mode::Area
                && let (Some(from), Some(to)) = (self.pressed, self.pointer)
            {
                let left = from.x.min(to.x);
                let top = from.y.min(to.y);
                root = root.child(
                    div()
                        .absolute()
                        .left(left)
                        .top(top)
                        .w(from.x.max(to.x) - left)
                        .h(from.y.max(to.y) - top)
                        .border_2()
                        .border_color(t.primary)
                        .bg(Hsla {
                            a: 0.12,
                            ..t.primary
                        }),
                );
            }

            // The color under the pointer, beside it.
            if self.pick.mode == Mode::Color
                && let Some(at) = self.pointer
                && let Some([r, g, b]) = self.color(at, window)
            {
                let hex = format!("#{r:02x}{g:02x}{b:02x}");
                root = root.child(
                    div()
                        .absolute()
                        .left(at.x + px(16.0))
                        .top(at.y + px(16.0))
                        .flex()
                        .gap(px(8.0))
                        .items_center()
                        .p(px(6.0))
                        .rounded(t.radius)
                        .bg(t.card)
                        .border_1()
                        .border_color(t.border)
                        .text_color(t.foreground)
                        .child(
                            div()
                                .size(px(24.0))
                                .rounded(t.radius)
                                .border_1()
                                .border_color(t.border)
                                .bg(rgb(u32::from_be_bytes([0, r, g, b]))),
                        )
                        .child(hex),
                );
            }

            root.child(
                div()
                    .absolute()
                    .top(px(24.0))
                    .w_full()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .px(px(16.0))
                            .py(px(10.0))
                            .rounded(t.radius)
                            .bg(t.card)
                            .border_1()
                            .border_color(t.border)
                            .text_color(t.foreground)
                            .child(self.pick.banner()),
                    ),
            )
        }
    }

    /// Shows the picker until the person picks or cancels.
    pub fn pick(pick: Pick) -> Option<Answer> {
        let pixels = image::open(&pick.image).ok().map(|i| i.to_rgba8());
        let size = pixels.as_ref().map_or((0, 0), |p| p.dimensions());
        if size.0 == 0 || size.1 == 0 {
            tracing::error!("cannot read the screenshot {}", pick.image.display());
            return None;
        }
        let answer = Rc::new(Cell::new(None));
        let result = answer.clone();
        gpui_platform::application()
            .with_assets(mcsapi_components_gpui::Assets::new())
            .run(move |cx: &mut App| {
                crate::run::install(ThemeWatch::default().poll().unwrap_or_default(), cx);
                let display = cx.primary_display();
                let bounds = display.map_or_else(
                    || Bounds::new(point(px(0.0), px(0.0)), gpui::size(px(1280.0), px(800.0))),
                    |d| d.bounds(),
                );
                let options = WindowOptions {
                    window_bounds: Some(WindowBounds::Fullscreen(bounds)),
                    titlebar: None,
                    kind: WindowKind::Normal,
                    app_id: Some("org.derisk.picker".into()),
                    ..Default::default()
                };
                let opened = cx.open_window(options, |window, cx| {
                    cx.new(|cx| {
                        let focus = cx.focus_handle();
                        window.focus(&focus, cx);
                        Picker {
                            pick,
                            pixels,
                            size,
                            focus,
                            pointer: None,
                            pressed: None,
                            answer: result,
                        }
                    })
                });
                if let Err(error) = opened {
                    tracing::error!("opening the picker: {error}");
                    cx.quit();
                    return;
                }
                cx.on_window_closed(|cx, _| cx.quit()).detach();
                cx.activate(true);
            });
        answer.get()
    }
}

#[cfg(feature = "gpui")]
pub use window::pick;

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn reads_its_arguments() {
        let pick = Pick::from_args(args(&["area", "--image", "/tmp/s.png", "--app", "Krita"]));
        assert_eq!(
            pick,
            Ok(Pick {
                mode: Mode::Area,
                image: "/tmp/s.png".into(),
                app: "Krita".into(),
            })
        );
        assert!(pick.unwrap().banner().starts_with("Krita wants a picture"));
        assert!(Pick::from_args(args(&["color"])).is_err());
        assert!(Pick::from_args(args(&["window", "--image", "x"])).is_err());
        assert!(Pick::from_args(args(&["color", "--image"])).is_err());
        assert!(
            Pick::from_args(args(&["color", "--image", "x"]))
                .unwrap()
                .banner()
                .starts_with("An app wants a color")
        );
    }

    #[test]
    fn answers_read_back() {
        for answer in [
            Answer::Point { x: 3, y: 4 },
            Answer::Area {
                x: 1,
                y: 2,
                width: 30,
                height: 40,
            },
        ] {
            assert_eq!(answer.to_string().parse(), Ok(answer));
        }
        assert!("1 2 0 5".parse::<Answer>().is_err());
        assert!("1".parse::<Answer>().is_err());
        assert!("a b".parse::<Answer>().is_err());
    }

    #[test]
    fn points_land_on_the_screenshots_pixels() {
        // A 2560x1600 screenshot shown in a 1280x800 window: scale 2.
        assert_eq!(to_image((0.0, 0.0), (1280.0, 800.0), (2560, 1600)), (0, 0));
        assert_eq!(
            to_image((640.5, 400.2), (1280.0, 800.0), (2560, 1600)),
            (1281, 800)
        );
        // The far edge and beyond stay on the last pixel.
        assert_eq!(
            to_image((1280.0, 900.0), (1280.0, 800.0), (2560, 1600)),
            (2559, 1599)
        );
        assert_eq!(to_image((-5.0, 10.0), (0.0, 800.0), (2560, 1600)), (0, 20));
    }

    #[test]
    fn a_drag_makes_an_area_either_way() {
        let expected = Answer::Area {
            x: 10,
            y: 20,
            width: 91,
            height: 31,
        };
        assert_eq!(area((10, 20), (100, 50)), expected);
        assert_eq!(area((100, 50), (10, 20)), expected);
        assert_eq!(
            area((7, 7), (7, 7)),
            Answer::Area {
                x: 7,
                y: 7,
                width: 1,
                height: 1
            }
        );
    }
}
