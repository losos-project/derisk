//! The shell's motion: easing curves, and the startup animation's timeline.
//!
//! The curves and timings are the launch film's (losos-desktop's launch
//! video), so the desktop moves the way the film shows it. The film runs at
//! 60 fps; a length written `f(n)` is n of its frames.
//!
//! Timelines are pure functions of elapsed time, so any renderer (egui, a
//! GLES pass in the compositor, a test) can sample them.

/// `n` frames of the film (60 fps) in milliseconds.
pub const fn f(n: u32) -> f32 {
    n as f32 * 1000.0 / 60.0
}

/// A CSS `cubic-bezier(x1, y1, x2, y2)` easing: progress through time in,
/// progress through the move out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ease {
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
}

impl Ease {
    /// The curve through (0, 0), (x1, y1), (x2, y2) and (1, 1).
    pub const fn new(x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        Self { x1, y1, x2, y2 }
    }

    /// Eased progress at `t` (clamped to 0–1).
    pub fn at(&self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        if t == 0.0 || t == 1.0 {
            return t;
        }
        let s = self.solve_x(t);
        bezier(s, self.y1, self.y2)
    }

    /// The curve parameter where x is `x`: Newton's method, with bisection
    /// where the slope is too flat for it.
    fn solve_x(&self, x: f32) -> f32 {
        let mut s = x;
        for _ in 0..8 {
            let err = bezier(s, self.x1, self.x2) - x;
            if err.abs() < 1e-6 {
                return s;
            }
            let slope = bezier_slope(s, self.x1, self.x2);
            if slope.abs() < 1e-6 {
                break;
            }
            s -= err / slope;
        }
        let (mut lo, mut hi) = (0.0, 1.0);
        s = x;
        for _ in 0..32 {
            let v = bezier(s, self.x1, self.x2);
            if (v - x).abs() < 1e-6 {
                break;
            }
            if v < x {
                lo = s;
            } else {
                hi = s;
            }
            s = (lo + hi) / 2.0;
        }
        s
    }
}

/// One axis of the cubic through 0, `p1`, `p2` and 1.
fn bezier(s: f32, p1: f32, p2: f32) -> f32 {
    let u = 1.0 - s;
    3.0 * u * u * s * p1 + 3.0 * u * s * s * p2 + s * s * s
}

fn bezier_slope(s: f32, p1: f32, p2: f32) -> f32 {
    let u = 1.0 - s;
    3.0 * u * u * p1 + 6.0 * u * s * (p2 - p1) + 3.0 * s * s * (1.0 - p2)
}

/// The film's easing tokens, named for how they feel. Entrances decay,
/// exits accelerate.
pub mod ease {
    use super::Ease;

    /// Expo-out: arrivals and reveals.
    pub const OUT: Ease = Ease::new(0.16, 1.0, 0.3, 1.0);
    /// Quint-out: gentle settles.
    pub const OUT_SOFT: Ease = Ease::new(0.22, 1.0, 0.36, 1.0);
    /// Expo-in: implosions and hard snaps out.
    pub const IN: Ease = Ease::new(0.7, 0.0, 0.84, 0.0);
    /// Cubic-in-out: drifts and short fades.
    pub const SMOOTH: Ease = Ease::new(0.65, 0.0, 0.35, 1.0);
    /// Small UI state changes.
    pub const UI: Ease = Ease::new(0.4, 0.0, 0.2, 1.0);
    /// Moves of a few hundred pixels with a soft landing.
    pub const GLIDE: Ease = Ease::new(0.47, 0.2, 0.15, 1.0);
    /// Starts from rest and ends at rest: chains after another move.
    pub const REST: Ease = Ease::new(0.45, 0.0, 0.1, 1.0);
    /// Expo-in-out: whips and slides.
    pub const IN_OUT: Ease = Ease::new(0.87, 0.0, 0.13, 1.0);
    /// The film's camera: a slow start and a long settle, for something
    /// growing to fill the screen.
    pub const CAM: Ease = Ease::new(0.48, 0.1, 0.0, 0.9);
}

/// Eased progress of `now` through `[from, to]` (milliseconds), 0 before and
/// 1 after.
pub fn progress(now: f32, from: f32, to: f32, ease: Ease) -> f32 {
    if to <= from {
        return if now >= to { 1.0 } else { 0.0 };
    }
    ease.at((now - from) / (to - from))
}

/// When the `i`th of `n` things starts, spread evenly over `span` (ms):
/// how the film staggers a group in.
pub fn stagger(i: usize, n: usize, span: f32) -> f32 {
    if n > 1 {
        span * (i as f32 / (n - 1) as f32).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// A hit's envelope `ms` after it: up to 1 in 2 frames, then an exponential
/// decay over about 30 (0 before the hit and once it has died away). Scales
/// a pop: `1.0 + 0.12 * hit_pulse(ms)`.
pub fn hit_pulse(ms: f32) -> f32 {
    let (attack, tau) = (f(2), f(5));
    if ms <= 0.0 || ms > attack + 6.0 * tau {
        0.0
    } else if ms < attack {
        (ms / attack * std::f32::consts::FRAC_PI_2).sin()
    } else {
        (-(ms - attack) / tau).exp()
    }
}

/// A point `t` of the way along a gentle arc from `a` to `b`, bowed to the
/// left of travel by `bow` times the distance (negative bows right): how
/// the dot flies.
pub fn arc(a: (f32, f32), b: (f32, f32), t: f32, bow: f32) -> (f32, f32) {
    let mid = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
    let c = (mid.0 - (b.1 - a.1) * bow, mid.1 + (b.0 - a.0) * bow);
    let lerp = |p: f32, q: f32| p + (q - p) * t;
    (
        lerp(lerp(a.0, c.0), lerp(c.0, b.0)),
        lerp(lerp(a.1, c.1), lerp(c.1, b.1)),
    )
}

/// The dot's diameter, as the boot left it.
pub const DOT: f32 = 40.0;
/// The radius of the ring the dot sweeps before the bloom.
pub const RING_RADIUS: f32 = 150.0;
/// The ring's stroke.
pub const RING_WIDTH: f32 = 5.0;

/// The startup timeline, in ms from the session's first frame (the film's
/// second act, from its drop).
mod at {
    use super::f;

    /// The ring sweeps round the dot.
    pub const SWEEP: (f32, f32) = (0.0, f(54));
    /// The ring blooms open into the desktop.
    pub const BLOOM: (f32, f32) = (f(60), f(90));
    /// The top bar slides in.
    pub const BAR: (f32, f32) = (f(90), f(114));
    /// The dot flies into the search button.
    pub const FLIGHT: (f32, f32) = (f(90), f(120));
    /// It opens into the button's focus ring.
    pub const OPEN: (f32, f32) = (f(118), f(132));
}

/// How much of the desktop the startup animation shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Reveal {
    /// None of it: only the dot and its ring.
    Hidden,
    /// A circle round the screen's centre, opening from the ring's radius
    /// past the corners as this goes from 0 to 1.
    Iris(f32),
    /// All of it.
    Shown,
}

impl Reveal {
    /// The radius the desktop shows within, when the screen's corners are
    /// `corner` from its centre.
    pub fn radius(self, corner: f32) -> f32 {
        match self {
            Self::Hidden => 0.0,
            Self::Iris(t) => RING_RADIUS + (corner + 40.0 - RING_RADIUS) * t,
            Self::Shown => f32::INFINITY,
        }
    }
}

/// What to draw at one instant of the startup animation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StartupFrame {
    /// How much of the desktop shows.
    pub reveal: Reveal,
    /// Opacity of a plain cover over the desktop (the reduced-motion fade).
    pub cover: f32,
    /// How far the ring has swept round the dot, 0–1 (0: none).
    pub sweep: f32,
    /// How far the bloom is, 0–1 (0: not blooming): its ring rides the
    /// iris's edge, widening and fading.
    pub bloom: f32,
    /// The dot's scale: 1 at rest, more on the hit that starts the sweep.
    pub dot_scale: f32,
    /// How far the dot has flown from the screen's centre to the search
    /// field, 0–1.
    pub flight: f32,
    /// How far the dot has opened into the field's focus outline, 0–1; at 1
    /// it is gone.
    pub open: f32,
    /// Opacity of the shell chrome (top bar, windows).
    pub shell_opacity: f32,
    /// Vertical offset, in logical pixels, of the top bar sliding in.
    pub bar_offset: f32,
    /// The animation has finished; the renderer can stop sampling.
    pub done: bool,
}

impl StartupFrame {
    /// The end of the animation: the desktop fully shown, the top bar in place.
    pub const DONE: Self = Self {
        reveal: Reveal::Shown,
        cover: 0.0,
        sweep: 0.0,
        bloom: 0.0,
        dot_scale: 1.0,
        flight: 1.0,
        open: 1.0,
        shell_opacity: 1.0,
        bar_offset: 0.0,
        done: true,
    };
}

/// The startup animation, as the film's desktop arrives: the dot the boot
/// left in the middle of the screen sweeps a ring, the ring blooms open into
/// the desktop, the top bar slides in, and the dot flies into the search
/// field and opens into its focus outline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StartupAnimation {
    /// Honor the user's reduced-motion preference with a short cross-fade.
    pub reduced_motion: bool,
    /// Top bar height, used for the slide distance.
    pub bar_height: f32,
}

impl StartupAnimation {
    /// Total length in milliseconds.
    pub fn duration_ms(&self) -> u32 {
        if self.reduced_motion {
            300
        } else {
            at::OPEN.1.ceil() as u32
        }
    }

    /// Samples the animation `elapsed_ms` after start.
    pub fn frame(&self, elapsed_ms: u32) -> StartupFrame {
        let now = elapsed_ms as f32;
        let done = elapsed_ms >= self.duration_ms();
        if self.reduced_motion {
            let t = progress(now, 0.0, 300.0, ease::SMOOTH);
            return StartupFrame {
                cover: 1.0 - t,
                shell_opacity: t,
                done,
                ..StartupFrame::DONE
            };
        }
        let bloom = progress(now, at::BLOOM.0, at::BLOOM.1, ease::IN);
        let blooming = now >= at::BLOOM.0;
        let reveal = if bloom >= 1.0 {
            Reveal::Shown
        } else if blooming {
            Reveal::Iris(bloom)
        } else {
            Reveal::Hidden
        };
        StartupFrame {
            reveal,
            cover: 0.0,
            sweep: if blooming {
                0.0
            } else {
                progress(now, at::SWEEP.0, at::SWEEP.1, ease::SMOOTH)
            },
            bloom: if bloom < 1.0 { bloom } else { 0.0 },
            dot_scale: 1.0 + 0.12 * hit_pulse(now),
            flight: progress(now, at::FLIGHT.0, at::FLIGHT.1, ease::REST),
            open: progress(now, at::OPEN.0, at::OPEN.1, ease::OUT),
            shell_opacity: 1.0,
            bar_offset: -self.bar_height * (1.0 - progress(now, at::BAR.0, at::BAR.1, ease::OUT)),
            done,
        }
    }
}

/// Where Plymouth's line lies on a `w`×`h` screen when it lets go of it
/// (`[x, y, width, height]`), as losos-desktop's splash.script lays it out:
/// type sized from the screen's height, the salmon a fifth of it, the
/// caption under the salmon and the line under the caption. The caption's
/// height is DejaVu Sans Bold's line height at the size the script asks for.
pub fn plymouth_line(w: f32, h: f32) -> [f32; 4] {
    let size = (h / 54.0).floor().max(11.0);
    let logo_h = (h * 0.2).floor();
    // The swimming salmon's frames are 1.6 times as wide as tall.
    let logo_w = (logo_h * 1.6).floor();
    let logo_y = ((h - logo_h - 4.0 * size) / 2.0).floor();
    let caption_y = logo_y + logo_h + 2.0 * size;
    // Pango's points at 96 dpi, and DejaVu's line height of 1.164 em.
    let caption_h = ((size * 1.2).floor() * 4.0 / 3.0 * 1.164).round();
    let width = (logo_w * 1.2).min(w - 2.0 * size).floor();
    let height = (size / 5.0).floor().max(3.0);
    [
        ((w - width) / 2.0).floor(),
        caption_y + caption_h + size,
        width,
        height,
    ]
}

/// The login screen's arrival, and its exit into the session.
///
/// Plymouth leaves its line on the ground; the line closes into the dot in
/// the middle of the screen (the film's collapse, into its drop), and from
/// there the login screen arrives as the desktop does: the ring, the bloom,
/// and the dot flying into the field. Once signed in, the field's outline
/// folds back into the dot, the screen closes round it as the dot flies
/// back to the middle, and the session starts from that dot.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LoginAnimation;

/// What to draw at one instant of the login screen's animation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LoginFrame {
    /// The line closing into the dot: 0 is Plymouth's line, 1 the dot.
    Collapse(f32),
    /// Everything after, drawn as the startup animation is.
    Startup(StartupFrame),
}

impl LoginAnimation {
    /// How long the line takes to close into the dot.
    pub const COLLAPSE_MS: f32 = f(30);
    /// How long the exit takes: the fold, then the close.
    pub const LEAVE_MS: f32 = f(46);

    /// The arrival, `elapsed_ms` after the login screen's first frame.
    pub fn frame(&self, elapsed_ms: u32) -> LoginFrame {
        let now = elapsed_ms as f32;
        if now < Self::COLLAPSE_MS {
            return LoginFrame::Collapse(progress(now, 0.0, Self::COLLAPSE_MS, ease::IN));
        }
        let startup = StartupAnimation {
            reduced_motion: false,
            // No top bar on the login screen.
            bar_height: 0.0,
        };
        LoginFrame::Startup(startup.frame((now - Self::COLLAPSE_MS) as u32))
    }

    /// Whether the arrival is over.
    pub fn arrived(&self, elapsed_ms: u32) -> bool {
        matches!(self.frame(elapsed_ms), LoginFrame::Startup(f) if f.done)
    }

    /// The exit, `ms` after the session was started: the field's outline
    /// folds into the dot (16 frames), then the screen closes round it as it
    /// flies back to the middle (30).
    pub fn leaving(&self, ms: f32) -> StartupFrame {
        let fold = progress(ms, 0.0, f(16), ease::OUT);
        let close = progress(ms, f(16), Self::LEAVE_MS, ease::IN);
        let back = progress(ms, f(16), Self::LEAVE_MS, ease::REST);
        StartupFrame {
            reveal: if close >= 1.0 {
                Reveal::Hidden
            } else {
                Reveal::Iris(1.0 - close)
            },
            open: 1.0 - fold,
            flight: 1.0 - back,
            done: ms >= Self::LEAVE_MS,
            ..StartupFrame::DONE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eases_run_from_zero_to_one() {
        for e in [ease::OUT, ease::IN, ease::SMOOTH, ease::GLIDE, ease::REST] {
            assert_eq!(e.at(0.0), 0.0);
            assert_eq!(e.at(1.0), 1.0);
            assert!((0.0..=1.0).contains(&e.at(0.5)));
        }
        // Symmetric curves cross the middle in the middle.
        assert!((ease::SMOOTH.at(0.5) - 0.5).abs() < 1e-4);
        // Expo-out is most of the way there early; expo-in barely started.
        assert!(ease::OUT.at(0.2) > 0.7);
        assert!(ease::IN.at(0.5) < 0.1);
    }

    #[test]
    fn a_hit_peaks_then_dies_away() {
        assert_eq!(hit_pulse(0.0), 0.0);
        assert!((hit_pulse(f(2)) - 1.0).abs() < 1e-3);
        assert!(hit_pulse(f(10)) < 0.3);
        assert_eq!(hit_pulse(f(40)), 0.0);
    }

    #[test]
    fn the_arc_ends_where_it_aims() {
        assert_eq!(arc((0.0, 0.0), (100.0, 50.0), 0.0, 0.2), (0.0, 0.0));
        let end = arc((0.0, 0.0), (100.0, 50.0), 1.0, 0.2);
        assert!((end.0 - 100.0).abs() < 1e-4 && (end.1 - 50.0).abs() < 1e-4);
    }

    #[test]
    fn plymouths_line_sits_under_the_caption() {
        let [x, y, w, h] = plymouth_line(1920.0, 1080.0);
        // 20 px type, a 216 px salmon 345 px wide, the line 1.2 times that.
        assert_eq!((w, h), (414.0, 4.0));
        assert_eq!(x, 753.0);
        assert!((690.0..720.0).contains(&y));
    }

    #[test]
    fn the_login_screen_arrives_from_the_line_and_leaves_into_the_dot() {
        let login = LoginAnimation;
        assert_eq!(login.frame(0), LoginFrame::Collapse(0.0));
        assert!(!login.arrived(1000));
        assert!(login.arrived(3000));
        let start = login.leaving(0.0);
        assert_eq!((start.open, start.flight), (1.0, 1.0));
        let end = login.leaving(LoginAnimation::LEAVE_MS);
        assert!(end.done);
        assert_eq!(end.reveal, Reveal::Hidden);
        assert_eq!((end.open, end.flight), (0.0, 0.0));
    }
}
