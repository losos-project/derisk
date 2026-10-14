//! Windows move as the launch film's do. One that opens grows in from
//! 94% while it fades in (22 frames, expo-out), or, opened into a half of
//! the screen, slides in from that edge (26 frames, expo-out). One whose
//! frame changes glides from where it is drawn to its new frame (24
//! frames, the film's glide), and one snapped where it already is nudges
//! 6 pixels away from the edge and back, so the snap still shows. One
//! picked in the overview grows out of its card (54 frames, the film's
//! camera curve).
//!
//! Only the drawing moves: the host draws each window where
//! [`WindowMotion::update`] says this frame, while the client is already
//! configured to its frame and input goes there.

use std::collections::{HashMap, HashSet};

use mcsapi::{Geometry, WindowId};

use crate::{
    animation::{Ease, ease, f, hit_pulse},
    shell::{Mode, WindowPlacement},
    snap::SnapZone,
};

/// Where a window is drawn this frame, when that is not its frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shown {
    /// The rectangle its frame is drawn in.
    pub frame: Geometry,
    /// From 0 (not drawn) to 255 (opaque).
    pub opacity: u8,
}

/// How far an opening window starts scaled down.
const OPEN_SCALE: f32 = 0.94;
/// How far a snap nudges a window, in pixels.
const PULSE: f32 = 6.0;
/// How far past the screen's edge a window sliding in starts.
const SLIDE_FROM: f32 = 40.0;

#[derive(Clone, Copy, Debug, PartialEq)]
struct RectF {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl RectF {
    fn of(g: Geometry) -> Self {
        Self {
            x: g.loc.x as f32,
            y: g.loc.y as f32,
            w: g.size.w as f32,
            h: g.size.h as f32,
        }
    }

    fn lerp(self, to: Self, t: f32) -> Self {
        let l = |a: f32, b: f32| a + (b - a) * t;
        Self {
            x: l(self.x, to.x),
            y: l(self.y, to.y),
            w: l(self.w, to.w),
            h: l(self.h, to.h),
        }
    }

    fn scaled(self, k: f32) -> Self {
        Self {
            x: self.x + self.w * (1.0 - k) / 2.0,
            y: self.y + self.h * (1.0 - k) / 2.0,
            w: self.w * k,
            h: self.h * k,
        }
    }

    fn geometry(self) -> Geometry {
        let (x, y) = (self.x.round() as i32, self.y.round() as i32);
        Geometry::new(
            (x, y).into(),
            (
                (self.x + self.w).round() as i32 - x,
                (self.y + self.h).round() as i32 - y,
            )
                .into(),
        )
    }
}

/// One window's motion.
#[derive(Clone, Copy, Debug)]
struct Track {
    frame: Geometry,
    mode: Mode,
    /// Where it was drawn last frame.
    drawn: RectF,
    /// Moving from a rectangle to its frame: from where, since when (ms),
    /// over how long, on which curve.
    moving: Option<(RectF, f64, f32, Ease)>,
    /// Fading in since when (ms).
    fading: Option<f64>,
    /// Nudged by a snap since when (ms), and which way across.
    pulse: Option<(f64, f32)>,
}

/// The windows' motion, kept between frames.
#[derive(Default)]
pub struct WindowMotion {
    /// Every window the shell had last frame, on any workspace; `None`
    /// before the first frame, when nothing opens.
    known: Option<HashSet<WindowId>>,
    tracks: HashMap<WindowId, Track>,
    /// A window to grow out of a rectangle, and since when.
    grow: Option<(WindowId, Geometry, f64)>,
    /// Whether the on-screen keyboard showed last frame, and whether it
    /// just came up or went away.
    keyboard: bool,
    keyboard_moved: bool,
}

impl WindowMotion {
    /// Tells it whether the on-screen keyboard shows, before
    /// [`WindowMotion::update`]: windows making room for it, or taking the
    /// room back, move on the keyboard's own curve (24 frames, expo-out).
    pub fn keyboard(&mut self, visible: bool) {
        self.keyboard_moved = visible != self.keyboard;
        self.keyboard = visible;
    }

    /// Grows `window` out of `from`, where it was picked in the overview,
    /// into its frame, starting at `now` (ms): 54 frames on the film's
    /// camera curve.
    pub fn grow(&mut self, window: WindowId, from: Geometry, now: f64) {
        self.grow = Some((window, from, now));
    }

    /// Works out this frame's motion at `now` (ms): `placements` are the
    /// windows drawn, `windows` every window the shell has, `dragged` the
    /// one the pointer is moving (it follows the pointer exactly) and
    /// `area` the work area. Returns where to draw each window that is not
    /// simply at its frame; with motion off, nothing.
    pub fn update(
        &mut self,
        now: f64,
        placements: &[WindowPlacement],
        windows: impl IntoIterator<Item = WindowId>,
        dragged: Option<WindowId>,
        area: Geometry,
        enabled: bool,
    ) -> HashMap<WindowId, Shown> {
        let windows: HashSet<WindowId> = windows.into_iter().collect();
        let known = self.known.replace(windows.clone());
        let mut tracks = HashMap::with_capacity(placements.len());
        let mut out = HashMap::new();
        for p in placements {
            let frame = RectF::of(p.frame);
            let opened = known.as_ref().is_some_and(|k| !k.contains(&p.window));
            let mut track = match self.tracks.get(&p.window) {
                // Seen last frame.
                Some(t) => *t,
                // Opened just now, or back from another workspace (which
                // doesn't move).
                None => {
                    let mut t = Track {
                        frame: p.frame,
                        mode: p.mode,
                        drawn: frame,
                        moving: None,
                        fading: None,
                        pulse: None,
                    };
                    if opened {
                        t.moving = Some(match slide_from(p.frame, area) {
                            Some(from) => (from, now, f(26), ease::OUT),
                            None => {
                                t.fading = Some(now);
                                (frame.scaled(OPEN_SCALE), now, f(22), ease::OUT)
                            }
                        });
                    }
                    t
                }
            };
            if let Some((_, from, start)) = self.grow.take_if(|(w, ..)| *w == p.window) {
                track.moving = Some((RectF::of(from), start, f(54), ease::CAM));
                track.fading = None;
                track.pulse = None;
                track.frame = p.frame;
            }
            if dragged == Some(p.window) {
                track.moving = None;
                track.pulse = None;
            } else if track.frame != p.frame {
                let curve = if self.keyboard_moved {
                    ease::OUT
                } else {
                    ease::GLIDE
                };
                track.moving = Some((track.drawn, now, f(24), curve));
            } else if track.mode != p.mode
                && matches!(p.mode, Mode::Snapped { zone } if zone != SnapZone::Maximize)
            {
                let across = if frame.x + frame.w / 2.0 > (area.loc.x + area.size.w / 2) as f32 {
                    -1.0
                } else {
                    1.0
                };
                track.pulse = Some((now, across));
            }
            track.frame = p.frame;
            track.mode = p.mode;

            let mut drawn = frame;
            if let Some((from, start, ms, curve)) = track.moving {
                let t = progress(now, start, ms);
                if t >= 1.0 {
                    track.moving = None;
                } else {
                    drawn = from.lerp(frame, curve.at(t));
                }
            }
            let mut opacity = 255;
            if let Some(start) = track.fading {
                let t = progress(now, start, f(22));
                if t >= 1.0 {
                    track.fading = None;
                } else {
                    opacity = (ease::OUT.at(t) * 255.0).round() as u8;
                }
            }
            if let Some((start, across)) = track.pulse {
                let ms = (now - start) as f32;
                let nudge = hit_pulse(ms);
                if ms > 0.0 && nudge == 0.0 {
                    track.pulse = None;
                } else {
                    drawn.x += across * PULSE * nudge;
                }
            }
            track.drawn = drawn;
            tracks.insert(p.window, track);
            if !enabled {
                continue;
            }
            let shown = Shown {
                frame: drawn.geometry(),
                opacity,
            };
            if shown.frame != p.frame || opacity != 255 {
                out.insert(p.window, shown);
            }
        }
        self.tracks = tracks;
        self.keyboard_moved = false;
        out
    }
}

/// How far through `ms` since `start` it is at `now`, from 0 to 1.
fn progress(now: f64, start: f64, ms: f32) -> f32 {
    ((now - start) as f32 / ms).clamp(0.0, 1.0)
}

/// Where a window opened into one half of the work area slides in from:
/// just past that edge of the screen, as the film's editor comes in.
fn slide_from(frame: Geometry, area: Geometry) -> Option<RectF> {
    let r = RectF::of(frame);
    let right = frame.loc.x + frame.size.w >= area.loc.x + area.size.w;
    let left = frame.loc.x <= area.loc.x;
    match (left, right) {
        (false, true) => Some(RectF {
            x: (area.loc.x + area.size.w) as f32 + SLIDE_FROM,
            ..r
        }),
        (true, false) => Some(RectF {
            x: area.loc.x as f32 - SLIDE_FROM - r.w,
            ..r
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u64) -> WindowId {
        WindowId::new(n).unwrap()
    }

    fn placed(window: u64, frame: Geometry, mode: Mode) -> WindowPlacement {
        WindowPlacement {
            window: id(window),
            frame,
            client: frame,
            mode,
            focused: false,
        }
    }

    fn g(x: i32, y: i32, w: i32, h: i32) -> Geometry {
        Geometry::new((x, y).into(), (w, h).into())
    }

    fn area() -> Geometry {
        g(0, 0, 1000, 600)
    }

    #[test]
    fn nothing_opens_on_the_first_frame() {
        let mut m = WindowMotion::default();
        let p = [placed(1, area(), Mode::Tiled)];
        assert!(m.update(0.0, &p, [id(1)], None, area(), true).is_empty());
    }

    #[test]
    fn an_opening_window_grows_and_fades_in() {
        let mut m = WindowMotion::default();
        m.update(0.0, &[], [], None, area(), true);
        let p = [placed(1, area(), Mode::Tiled)];
        let first = m.update(100.0, &p, [id(1)], None, area(), true)[&id(1)];
        assert_eq!(first.opacity, 0);
        assert_eq!(first.frame, g(30, 18, 940, 564));
        let mid = m.update(100.0 + f64::from(f(11)), &p, [id(1)], None, area(), true)[&id(1)];
        assert!(mid.opacity > 200, "{mid:?}");
        assert!(
            m.update(100.0 + f64::from(f(22)), &p, [id(1)], None, area(), true)
                .is_empty()
        );
    }

    #[test]
    fn one_opened_into_the_right_half_slides_in_from_the_edge() {
        let mut m = WindowMotion::default();
        m.update(0.0, &[], [], None, area(), true);
        let right = g(500, 0, 500, 600);
        let p = [placed(1, right, Mode::Tiled)];
        let first = m.update(0.0, &p, [id(1)], None, area(), true)[&id(1)];
        assert_eq!(
            first,
            Shown {
                frame: g(1040, 0, 500, 600),
                opacity: 255
            }
        );
    }

    #[test]
    fn a_retiled_window_glides_and_a_dragged_one_does_not() {
        let mut m = WindowMotion::default();
        let p = [placed(1, area(), Mode::Tiled)];
        m.update(0.0, &p, [id(1)], None, area(), true);
        let left = [placed(
            1,
            g(0, 0, 500, 600),
            Mode::Snapped {
                zone: SnapZone::Left,
            },
        )];
        let start = m.update(10.0, &left, [id(1)], None, area(), true)[&id(1)];
        assert_eq!(start.frame, area());
        let mid = m.update(10.0 + f64::from(f(12)), &left, [id(1)], None, area(), true)[&id(1)];
        assert!(mid.frame.size.w < 1000 && mid.frame.size.w > 500, "{mid:?}");
        assert!(
            m.update(10.0 + f64::from(f(24)), &left, [id(1)], None, area(), true)
                .is_empty()
        );

        let moved = [placed(
            1,
            g(5, 0, 500, 600),
            Mode::Snapped {
                zone: SnapZone::Left,
            },
        )];
        assert!(
            m.update(500.0, &moved, [id(1)], Some(id(1)), area(), true)
                .is_empty()
        );
    }

    #[test]
    fn a_snap_in_place_nudges_away_from_the_edge() {
        let mut m = WindowMotion::default();
        let right = g(500, 0, 500, 600);
        m.update(
            0.0,
            &[placed(1, right, Mode::Tiled)],
            [id(1)],
            None,
            area(),
            true,
        );
        let snapped = [placed(
            1,
            right,
            Mode::Snapped {
                zone: SnapZone::Right,
            },
        )];
        m.update(10.0, &snapped, [id(1)], None, area(), true);
        let nudged = m.update(
            10.0 + f64::from(f(2)),
            &snapped,
            [id(1)],
            None,
            area(),
            true,
        )[&id(1)];
        assert_eq!(nudged.frame, g(494, 0, 500, 600));
        assert!(
            m.update(10.0 + 1000.0, &snapped, [id(1)], None, area(), true)
                .is_empty()
        );
    }

    #[test]
    fn a_picked_window_grows_out_of_its_card() {
        let mut m = WindowMotion::default();
        let p = [placed(1, area(), Mode::Tiled)];
        m.update(0.0, &p, [id(1)], None, area(), true);
        let card = g(400, 200, 200, 120);
        m.grow(id(1), card, 10.0);
        assert_eq!(
            m.update(10.0, &p, [id(1)], None, area(), true)[&id(1)].frame,
            card
        );
        let mid = m.update(10.0 + f64::from(f(27)), &p, [id(1)], None, area(), true)[&id(1)];
        assert!(mid.frame.size.w > 200 && mid.frame.size.w < 1000, "{mid:?}");
        assert!(
            m.update(10.0 + f64::from(f(54)), &p, [id(1)], None, area(), true)
                .is_empty()
        );
    }

    #[test]
    fn windows_make_room_for_the_keyboard_on_its_curve() {
        let mut m = WindowMotion::default();
        m.update(
            0.0,
            &[placed(1, area(), Mode::Tiled)],
            [id(1)],
            None,
            area(),
            true,
        );
        let short = [placed(1, g(0, 0, 1000, 300), Mode::Tiled)];
        m.keyboard(true);
        m.update(10.0, &short, [id(1)], None, area(), true);
        // A quarter of the way in, expo-out is most of the way there.
        let h = m.update(10.0 + f64::from(f(6)), &short, [id(1)], None, area(), true)[&id(1)]
            .frame
            .size
            .h;
        assert!(h < 360, "{h}");
    }

    #[test]
    fn motion_off_draws_every_window_at_its_frame() {
        let mut m = WindowMotion::default();
        m.update(0.0, &[], [], None, area(), false);
        let p = [placed(1, area(), Mode::Tiled)];
        assert!(m.update(0.0, &p, [id(1)], None, area(), false).is_empty());
    }
}
