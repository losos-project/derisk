//! Motion in the chrome: values that ease toward their targets, things
//! that fade in when they first appear and glide when they move, on the
//! launch film's curves ([`crate::animation::ease`]).
//!
//! egui draws from scratch every frame, so each helper keeps its own
//! state in the context's memory under the ID it is given, and asks for
//! another frame while it is moving. With motion off (reduced motion, or
//! the Look setting) every helper returns its end state at once.
//!
//! Every helper times itself on [`now`], which runs faster than egui's
//! clock as the person gets used to the desktop ([`set_speed`]), so all of
//! the chrome's motion quickens together, delays and chained steps too.

use mcsapi::toolkit::egui::{self, Context, Id, Pos2, Rect, pos2};

use crate::animation::{Ease, MotionClock};

/// Turns motion on or off for every helper in `ctx`, for this frame on.
pub fn set_enabled(ctx: &Context, enabled: bool) {
    ctx.data_mut(|d| d.insert_temp(Id::new("derisk-motion-off"), !enabled));
}

/// Whether motion is on in `ctx`.
pub fn enabled(ctx: &Context) -> bool {
    !ctx.data(|d| d.get_temp::<bool>(Id::new("derisk-motion-off")))
        .unwrap_or(false)
}

/// Runs motion in `ctx` `speed` times as fast as the film, for this frame
/// on: 1 on a new desktop, more on one the person knows.
pub fn set_speed(ctx: &Context, speed: f32) {
    clock(ctx, |c| c.set_speed(speed));
}

/// The motion clock, in milliseconds: egui's, sped up by [`set_speed`].
pub fn now(ctx: &Context) -> f64 {
    clock(ctx, |_| ())
}

/// Brings the context's motion clock up to egui's, then applies `change`;
/// returns the time before the change.
fn clock(ctx: &Context, change: impl FnOnce(&mut MotionClock)) -> f64 {
    let id = Id::new("derisk-motion-clock");
    let real = ctx.input(|i| i.time) * 1000.0;
    let mut clock = ctx
        .data(|d| d.get_temp::<MotionClock>(id))
        .unwrap_or_default();
    let at = clock.at(real);
    change(&mut clock);
    ctx.data_mut(|d| d.insert_temp(id, clock));
    at
}

/// Eased progress through `ms` since `since` (ms on [`now`]'s clock),
/// asking for frames until it is done.
pub fn since(ctx: &Context, since: f64, ms: f32, ease: Ease) -> f32 {
    if !enabled(ctx) {
        return 1.0;
    }
    let t = ((now(ctx) - since) as f32 / ms).clamp(0.0, 1.0);
    if t < 1.0 {
        ctx.request_repaint();
    }
    ease.at(t)
}

#[derive(Clone, Copy)]
struct Tween {
    from: f32,
    to: f32,
    start: f64,
}

/// A value that eases to `target` over `ms` each time the target changes,
/// starting from wherever it was. The first call starts at the target.
pub fn tween(ctx: &Context, id: Id, target: f32, ms: f32, ease: Ease) -> f32 {
    let now = now(ctx);
    let mut tw = ctx.data(|d| d.get_temp::<Tween>(id)).unwrap_or(Tween {
        from: target,
        to: target,
        start: now,
    });
    let value = |tw: &Tween| {
        let t = ((now - tw.start) as f32 / ms).clamp(0.0, 1.0);
        tw.from + (tw.to - tw.from) * ease.at(t)
    };
    if !enabled(ctx) {
        tw = Tween {
            from: target,
            to: target,
            start: now,
        };
    } else if tw.to != target {
        tw = Tween {
            from: value(&tw),
            to: target,
            start: now,
        };
    }
    ctx.data_mut(|d| d.insert_temp(id, tw));
    let v = value(&tw);
    if v != tw.to {
        ctx.request_repaint();
    }
    v
}

#[derive(Clone, Copy)]
struct Seen {
    first: f64,
    frame: u64,
}

/// When `id` was first asked about without a break of a frame: the time
/// it appeared, for fading it in. Something that goes away and comes back
/// appears again.
pub fn appeared(ctx: &Context, id: Id) -> f64 {
    let now = now(ctx);
    let frame = ctx.cumulative_frame_nr();
    let seen = ctx
        .data(|d| d.get_temp::<Seen>(id))
        .filter(|s| s.frame + 1 >= frame)
        .unwrap_or(Seen { first: now, frame });
    ctx.data_mut(|d| {
        d.insert_temp(
            id,
            Seen {
                first: seen.first,
                frame,
            },
        )
    });
    seen.first
}

/// Eased progress, over `ms`, of `id` appearing (see [`appeared`]), with
/// a `delay` first.
pub fn appear(ctx: &Context, id: Id, delay: f32, ms: f32, ease: Ease) -> f32 {
    let first = appeared(ctx, id);
    since(ctx, first + f64::from(delay), ms, ease)
}

#[derive(Clone, Copy)]
struct Glide {
    at: Pos2,
    from: egui::Vec2,
    start: f64,
}

/// How far from `at` to draw something laid out there, so that when its
/// place moves it glides to the new one over `ms` instead of jumping: an
/// offset that starts at the old place and eases to zero. Something seen
/// for the first time doesn't glide.
pub fn glide(ctx: &Context, id: Id, at: Pos2, ms: f32, ease: Ease) -> egui::Vec2 {
    let now = now(ctx);
    let frame = ctx.cumulative_frame_nr();
    let offset = |g: &Glide| {
        let t = ((now - g.start) as f32 / ms).clamp(0.0, 1.0);
        g.from * (1.0 - ease.at(t))
    };
    let seen_id = id.with("seen");
    let fresh = ctx
        .data(|d| d.get_temp::<u64>(seen_id))
        .is_none_or(|f| f + 1 < frame);
    ctx.data_mut(|d| d.insert_temp(seen_id, frame));
    let mut g = ctx.data(|d| d.get_temp::<Glide>(id)).unwrap_or(Glide {
        at,
        from: egui::Vec2::ZERO,
        start: now,
    });
    if fresh || !enabled(ctx) {
        g = Glide {
            at,
            from: egui::Vec2::ZERO,
            start: now,
        };
    } else if g.at != at {
        // From wherever it is drawn now.
        let drawn = g.at + offset(&g);
        g = Glide {
            at,
            from: drawn - at,
            start: now,
        };
    }
    ctx.data_mut(|d| d.insert_temp(id, g));
    let v = offset(&g);
    if v != egui::Vec2::ZERO {
        ctx.request_repaint();
    }
    v
}

/// `a` to `b` by `t`.
pub fn lerp_rect(a: Rect, b: Rect, t: f32) -> Rect {
    Rect::from_min_max(a.min + (b.min - a.min) * t, a.max + (b.max - a.max) * t)
}

/// The first `t` (0–1) of the outline of `r`, its top corners rounded by
/// `radius`, as a line: from the top-left corner's end, clockwise, the
/// way the film's relay traces a window's focus border.
pub fn outline(r: Rect, radius: f32, t: f32) -> Vec<Pos2> {
    use std::f32::consts::{FRAC_PI_2, PI};
    let radius = radius.min(r.width() / 2.0).min(r.height() / 2.0).max(0.0);
    let arc = |c: Pos2, from: f32, out: &mut Vec<Pos2>| {
        for i in 1..=8 {
            let a = from + FRAC_PI_2 * i as f32 / 8.0;
            out.push(c + egui::vec2(a.cos(), a.sin()) * radius);
        }
    };
    let mut path = vec![
        pos2(r.left() + radius, r.top()),
        pos2(r.right() - radius, r.top()),
    ];
    arc(
        pos2(r.right() - radius, r.top() + radius),
        -FRAC_PI_2,
        &mut path,
    );
    path.push(r.right_bottom());
    path.push(r.left_bottom());
    path.push(pos2(r.left(), r.top() + radius));
    arc(pos2(r.left() + radius, r.top() + radius), PI, &mut path);
    if t >= 1.0 {
        return path;
    }
    let total: f32 = path.windows(2).map(|w| w[0].distance(w[1])).sum();
    let mut left = total * t.max(0.0);
    let mut out = vec![path[0]];
    for w in path.windows(2) {
        let d = w[0].distance(w[1]);
        if d >= left {
            out.push(w[0] + (w[1] - w[0]) * (left / d.max(f32::EPSILON)));
            break;
        }
        left -= d;
        out.push(w[1]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::ease;

    fn frame(ctx: &Context, time: f64, run: impl FnMut(&Context)) {
        let mut run = run;
        let input = egui::RawInput {
            time: Some(time),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| run(ui.ctx()));
        out.textures_delta.clear();
    }

    #[test]
    fn a_tween_eases_from_where_it_was() {
        let ctx = Context::default();
        let id = Id::new("t");
        let mut v = 0.0;
        frame(&ctx, 0.0, |c| v = tween(c, id, 10.0, 100.0, ease::SMOOTH));
        assert_eq!(v, 10.0);
        frame(&ctx, 1.0, |c| v = tween(c, id, 20.0, 100.0, ease::SMOOTH));
        assert_eq!(v, 10.0);
        frame(&ctx, 1.05, |c| v = tween(c, id, 20.0, 100.0, ease::SMOOTH));
        assert!((v - 15.0).abs() < 0.1);
        frame(&ctx, 1.2, |c| v = tween(c, id, 20.0, 100.0, ease::SMOOTH));
        assert_eq!(v, 20.0);
    }

    #[test]
    fn motion_off_jumps_to_the_end() {
        let ctx = Context::default();
        let id = Id::new("t");
        let mut v = 0.0;
        frame(&ctx, 0.0, |c| {
            set_enabled(c, false);
            v = tween(c, id, 1.0, 100.0, ease::OUT);
        });
        frame(&ctx, 0.01, |c| v = tween(c, id, 5.0, 100.0, ease::OUT));
        assert_eq!(v, 5.0);
        frame(&ctx, 0.02, |c| {
            v = appear(c, Id::new("a"), 0.0, 100.0, ease::OUT)
        });
        assert_eq!(v, 1.0);
    }

    #[test]
    fn a_quicker_desktop_finishes_sooner() {
        let ctx = Context::default();
        let id = Id::new("t");
        let mut v = 0.0;
        frame(&ctx, 0.0, |c| {
            set_speed(c, 4.0);
            v = tween(c, id, 0.0, 100.0, ease::SMOOTH);
        });
        frame(&ctx, 1.0, |c| v = tween(c, id, 1.0, 100.0, ease::SMOOTH));
        assert_eq!(v, 0.0);
        // A quarter of the film's 100 ms.
        frame(&ctx, 1.026, |c| v = tween(c, id, 1.0, 100.0, ease::SMOOTH));
        assert_eq!(v, 1.0);
    }

    #[test]
    fn a_moved_thing_glides_and_a_new_one_does_not() {
        let ctx = Context::default();
        let id = Id::new("g");
        let mut off = egui::Vec2::ZERO;
        frame(&ctx, 0.0, |c| {
            off = glide(c, id, pos2(0.0, 0.0), 100.0, ease::OUT)
        });
        assert_eq!(off, egui::Vec2::ZERO);
        frame(&ctx, 0.01, |c| {
            off = glide(c, id, pos2(0.0, 50.0), 100.0, ease::OUT)
        });
        assert_eq!(off, egui::vec2(0.0, -50.0));
        frame(&ctx, 0.2, |c| {
            off = glide(c, id, pos2(0.0, 50.0), 100.0, ease::OUT)
        });
        assert_eq!(off, egui::Vec2::ZERO);
    }

    #[test]
    fn the_outline_runs_clockwise_from_the_top_left() {
        let r = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(100.0, 50.0));
        let full = outline(r, 10.0, 1.0);
        assert_eq!(full[0], pos2(10.0, 0.0));
        let half = outline(r, 10.0, 0.25);
        // A quarter of the way round is along the top or down the right.
        let end = *half.last().unwrap();
        assert!(end.x > 50.0);
    }
}
