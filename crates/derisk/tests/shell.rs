use derisk::{
    action::{Action, Effect},
    geom::rect,
    shell::{DropTarget, Error, MAX_WORKSPACES, Mode, PointerOutcome, Shell},
    snap::SnapZone,
    systemd::SessionOp,
};
use mcsapi::WindowId;

fn desktop() -> Shell {
    Shell::new(rect(0, 0, 1920, 1080), false)
}

fn frame_of(shell: &Shell, window: WindowId) -> mcsapi::Geometry {
    shell
        .placements()
        .into_iter()
        .find(|p| p.window == window)
        .expect("window is visible")
        .frame
}

#[test]
fn windows_tile_automatically_inside_the_work_area() {
    let mut shell = desktop();
    let (a, _) = shell.map_window("a", "A");
    let (b, _) = shell.map_window("b", "B");
    let area = shell.work_area();
    let (fa, fb) = (frame_of(&shell, a), frame_of(&shell, b));
    assert!(fa.loc.y >= area.loc.y && fb.loc.y >= area.loc.y);
    assert!(fa.loc.x + fa.size.w <= fb.loc.x, "side by side, no overlap");
    assert_eq!(shell.mode(a), Some(Mode::Tiled));
    assert_eq!(shell.focused(), Some(b));
}

#[test]
fn super_dragging_a_window_to_the_left_edge_snaps_and_offers_snap_assist() {
    let mut shell = desktop();
    let (a, _) = shell.map_window("a", "A");
    let (b, _) = shell.map_window("b", "B");
    let f = frame_of(&shell, b);
    let grab = (f.loc.x + f.size.w / 2, f.loc.y + f.size.h / 2);
    shell.set_move_modifier(true);
    assert!(matches!(
        shell.pointer_down(grab, 0).unwrap(),
        PointerOutcome::Handled { .. }
    ));
    shell.pointer_motion((grab.0 - 50, grab.1));
    assert!(matches!(
        shell.pointer_motion((1, 500)),
        Some(DropTarget::Snap {
            zone: SnapZone::Left,
            ..
        })
    ));
    shell.pointer_up();
    assert_eq!(
        shell.mode(b),
        Some(Mode::Snapped {
            zone: SnapZone::Left
        })
    );
    let assist = shell.snap_assist().expect("snap assist");
    assert_eq!(assist.zone, SnapZone::Right);
    assert!(assist.candidates.contains(&a));
}

#[test]
fn dragging_a_snapped_window_away_restores_a_floating_frame() {
    let mut shell = desktop();
    let (a, _) = shell.map_window("a", "A");
    shell
        .apply(Action::Snap {
            window: None,
            zone: SnapZone::Left,
        })
        .unwrap();
    // From its title in the top bar: above the window, and off to the
    // side of a window snapped left.
    shell.grab_title((1500, 10));
    shell.pointer_motion((900, 500));
    shell.pointer_up();
    let Some(Mode::Floating { frame }) = shell.mode(a) else {
        panic!("floats");
    };
    // It hangs below the pointer, centred on it.
    let frame: mcsapi::Geometry = frame.into();
    assert!(frame.loc.y > 500);
    assert_eq!(frame.loc.x + frame.size.w / 2, 900);
}

#[test]
fn dragging_a_floating_window_by_its_title_keeps_its_offset() {
    let mut shell = desktop();
    let (a, _) = shell.map_window("a", "A");
    shell.apply(Action::Float { window: None }).unwrap();
    let before = frame_of(&shell, a);
    shell.grab_title((100, 10));
    shell.pointer_motion((160, 50));
    shell.pointer_up();
    let after = frame_of(&shell, a);
    assert_eq!(
        (after.loc.x - before.loc.x, after.loc.y - before.loc.y),
        (60, 40)
    );
}

#[test]
fn a_dragged_window_stays_below_the_top_bar() {
    let mut shell = desktop();
    let (a, _) = shell.map_window("a", "A");
    shell.apply(Action::Float { window: None }).unwrap();
    let f = frame_of(&shell, a);
    let inside = (f.loc.x + 20, f.loc.y + f.size.h / 2);
    shell.set_move_modifier(true);
    shell.pointer_down(inside, 0).unwrap();
    shell.pointer_motion((inside.0, 0));
    shell.pointer_up();
    assert_eq!(frame_of(&shell, a).loc.y, shell.work_area().loc.y);
}

#[test]
fn phones_have_no_window_to_move() {
    let mut phone = Shell::new(rect(0, 0, 392, 872), true);
    let (a, _) = phone.map_window("a", "A");
    phone.set_move_modifier(true);
    let f = frame_of(&phone, a);
    assert!(matches!(
        phone.pointer_down((f.loc.x + 10, f.loc.y + 10), 0).unwrap(),
        PointerOutcome::Client { .. }
    ));
    phone.grab_title((100, 10));
    phone.pointer_motion((200, 400));
    assert_eq!(phone.pointer_up(), None);
}

#[test]
fn small_drags_and_client_clicks_do_not_move_windows() {
    let mut shell = desktop();
    let (a, _) = shell.map_window("a", "A");
    let f = frame_of(&shell, a);
    let inside = (f.loc.x + 100, f.loc.y + 200);
    assert!(matches!(
        shell.pointer_down(inside, 0).unwrap(),
        PointerOutcome::Client { window, .. } if window == a
    ));
    shell.pointer_up();
    shell.set_move_modifier(true);
    shell.pointer_down(inside, 1000).unwrap();
    shell.pointer_motion((inside.0 + 2, inside.1 + 2));
    shell.pointer_up();
    assert_eq!(shell.mode(a), Some(Mode::Tiled));
    // Letting go of Super gives clicks back to the window.
    shell.set_move_modifier(false);
    assert!(matches!(
        shell.pointer_down(inside, 2000).unwrap(),
        PointerOutcome::Client { .. }
    ));
}

#[test]
fn the_client_has_the_whole_frame() {
    let mut shell = desktop();
    shell.map_window("a", "A");
    shell.map_window("b", "B");
    for p in shell.placements() {
        assert_eq!(p.client, p.frame, "no title bar above the client");
    }
}

#[test]
fn a_lone_or_maximized_window_fills_the_work_area() {
    let mut shell = desktop();
    let (a, _) = shell.map_window("a", "A");
    // Gaps separate tiles; one tile has nothing to be separated from.
    assert_eq!(frame_of(&shell, a), shell.work_area());
    let (b, _) = shell.map_window("b", "B");
    assert_ne!(frame_of(&shell, b), shell.work_area());
    shell
        .apply(Action::ToggleMaximize { window: None })
        .unwrap();
    assert_eq!(frame_of(&shell, b), shell.work_area());
}

#[test]
fn maximize_toggles_and_minimize_hides() {
    let mut shell = desktop();
    let (a, _) = shell.map_window("a", "A");
    shell
        .apply(Action::ToggleMaximize { window: None })
        .unwrap();
    assert_eq!(
        shell.mode(a),
        Some(Mode::Snapped {
            zone: SnapZone::Maximize
        })
    );
    shell
        .apply(Action::ToggleMaximize { window: None })
        .unwrap();
    assert_ne!(
        shell.mode(a),
        Some(Mode::Snapped {
            zone: SnapZone::Maximize
        })
    );
    shell.apply(Action::Minimize { window: None }).unwrap();
    assert!(shell.is_minimized(a));
    assert!(shell.placements().iter().all(|p| p.window != a));
    shell.apply(Action::Restore { window: a.get() }).unwrap();
    assert!(!shell.is_minimized(a));
}

#[test]
fn an_app_id_set_after_mapping_still_gets_queued_actions() {
    // GPUI maps its window before it sets the app ID.
    let mut shell = desktop();
    shell
        .run([
            Action::Launch {
                app: "org.derisk.calculator".into(),
            },
            Action::Snap {
                window: None,
                zone: SnapZone::Left,
            },
        ])
        .result
        .unwrap();
    let (w, _) = shell.map_window("app", "");
    assert_eq!(shell.mode(w), Some(Mode::Tiled));
    shell.set_app_id(w, "org.derisk.calculator").unwrap();
    assert_eq!(shell.window_label(w), Some(("org.derisk.calculator", "")));
    assert_eq!(
        shell.mode(w),
        Some(Mode::Snapped {
            zone: SnapZone::Left
        })
    );
}

#[test]
fn actions_after_a_launch_wait_for_the_new_window() {
    let mut shell = desktop();
    let effects = shell
        .run([
            Action::Launch {
                app: "kitty".into(),
            },
            Action::Snap {
                window: None,
                zone: SnapZone::Right,
            },
        ])
        .into_result()
        .unwrap();
    assert_eq!(
        effects,
        vec![Effect::Launch {
            app: "kitty".into()
        }]
    );
    let (w, _) = shell.map_window("kitty", "kitty");
    assert_eq!(
        shell.mode(w),
        Some(Mode::Snapped {
            zone: SnapZone::Right
        })
    );
}

#[test]
fn launch_rejects_paths_and_arguments() {
    let mut shell = desktop();
    for app in ["/bin/sh", "rm -rf", "-x", "a;b", ""] {
        assert_eq!(
            shell.apply(Action::Launch { app: app.into() }),
            Err(Error::NotLaunchable(app.into())),
            "{app}"
        );
    }
}

#[test]
fn destructive_session_ops_need_confirmation() {
    let mut shell = desktop();
    assert_eq!(
        shell.apply(Action::Session {
            op: SessionOp::Reboot,
            confirmed: false
        }),
        Err(Error::NeedsConfirmation(SessionOp::Reboot))
    );
    assert_eq!(
        shell.apply(Action::Session {
            op: SessionOp::Reboot,
            confirmed: true
        }),
        Ok(vec![Effect::Session {
            op: SessionOp::Reboot
        }])
    );
    assert_eq!(
        shell.apply(Action::Session {
            op: SessionOp::Lock,
            confirmed: false
        }),
        Ok(vec![Effect::Session {
            op: SessionOp::Lock
        }])
    );
}

#[test]
fn unit_actions_only_touch_failed_units() {
    let mut shell = desktop();
    assert!(
        shell
            .apply(Action::RestartUnit {
                unit: "sshd.service".into()
            })
            .is_err()
    );
    shell.failed_units = vec!["foo.service".into()];
    assert_eq!(
        shell.apply(Action::RestartUnit {
            unit: "foo.service".into()
        }),
        Ok(vec![Effect::RestartUnit {
            unit: "foo.service".into()
        }])
    );
}

/// The space between two side-by-side tiles, and between each and the
/// work area's left and right edges.
fn gaps_around(shell: &Shell, a: WindowId, b: WindowId) -> (i32, i32, i32) {
    let area = shell.work_area();
    let (fa, fb) = (frame_of(shell, a), frame_of(shell, b));
    (
        fa.loc.x - area.loc.x,
        fb.loc.x - (fa.loc.x + fa.size.w),
        area.loc.x + area.size.w - (fb.loc.x + fb.size.w),
    )
}

#[test]
fn tiles_are_five_pixels_apart_by_default() {
    let mut shell = desktop();
    let (a, _) = shell.map_window("a", "A");
    let (b, _) = shell.map_window("b", "B");
    assert_eq!(gaps_around(&shell, a, b), (5, 5, 5));
    let area = shell.work_area();
    assert_eq!(frame_of(&shell, a).loc.y - area.loc.y, 5);
}

#[test]
fn the_gap_setting_moves_tiles_and_snapped_halves() {
    let mut shell = desktop();
    let (a, _) = shell.map_window("a", "A");
    let (b, _) = shell.map_window("b", "B");
    for gaps in [0, 7, 16] {
        shell.set_gaps(gaps);
        let g = i32::from(gaps);
        assert_eq!(gaps_around(&shell, a, b), (g, g, g), "gap {gaps}");
    }

    let area = shell.work_area();
    let left = SnapZone::Left.geometry(area, 7);
    let right = SnapZone::Right.geometry(area, 7);
    assert_eq!(left.loc.x - area.loc.x, 7);
    assert_eq!(right.loc.x - (left.loc.x + left.size.w), 7);
    assert_eq!(area.loc.x + area.size.w - (right.loc.x + right.size.w), 7);
}

#[test]
fn phones_keep_no_gaps_whatever_the_setting() {
    let mut shell = Shell::new(rect(0, 0, 400, 800), true);
    shell.set_gaps(16);
    let (a, _) = shell.map_window("a", "A");
    assert_eq!(frame_of(&shell, a), shell.work_area());
    // Turning the phone into a desktop picks the setting up.
    shell.set_output(rect(0, 0, 1920, 1080), false);
    assert_eq!(shell.profile().gap, 16);
}

#[test]
fn phones_use_monocle_without_gaps() {
    let mut shell = Shell::new(rect(0, 0, 400, 800), true);
    assert!(shell.is_phone());
    let (_, _) = shell.map_window("a", "A");
    let (b, _) = shell.map_window("b", "B");
    let visible = shell.placements();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].window, b);
}

#[test]
fn workspaces_switch_and_move() {
    let mut shell = desktop();
    assert_eq!(shell.workspaces().len(), 1);
    let (a, _) = shell.map_window("a", "A");
    shell.map_window("b", "B");
    // One past the last workspace opens a new one.
    shell
        .apply(Action::MoveToWorkspace {
            window: Some(a.get()),
            workspace: 2,
        })
        .unwrap();
    assert_eq!(shell.workspaces().len(), 2);
    assert_eq!(
        shell
            .workspace_of(a)
            .and_then(|w| shell.workspace_number(w)),
        Some(2)
    );
    assert_eq!(shell.placements().len(), 1);
    shell
        .apply(Action::SwitchWorkspace { workspace: 2 })
        .unwrap();
    assert_eq!(shell.active_workspace(), 2);
    assert_eq!(shell.placements().len(), 1);
    assert_eq!(shell.placements()[0].window, a);
    for workspace in [4, 42] {
        assert_eq!(
            shell.apply(Action::SwitchWorkspace { workspace }),
            Err(Error::UnknownWorkspace(workspace))
        );
    }
}

#[test]
fn empty_workspaces_close_and_the_rest_renumber() {
    let mut shell = desktop();
    let (a, _) = shell.map_window("a", "A");
    let (b, _) = shell.map_window("b", "B");
    let (c, _) = shell.map_window("c", "C");
    for (w, n) in [(b, 2), (c, 3)] {
        shell
            .apply(Action::MoveToWorkspace {
                window: Some(w.get()),
                workspace: n,
            })
            .unwrap();
    }
    assert_eq!(shell.workspaces().len(), 3);
    let number = |shell: &Shell, w: WindowId| {
        shell
            .workspace_of(w)
            .and_then(|ws| shell.workspace_number(ws))
    };
    // Emptying workspace 2 closes it; C's workspace becomes number 2.
    shell
        .apply(Action::MoveToWorkspace {
            window: Some(b.get()),
            workspace: 1,
        })
        .unwrap();
    assert_eq!(shell.workspaces().len(), 2);
    assert_eq!(number(&shell, c), Some(2));

    // The active workspace stays open while empty, and closes once left.
    for w in [a, b] {
        shell
            .apply(Action::MoveToWorkspace {
                window: Some(w.get()),
                workspace: 2,
            })
            .unwrap();
    }
    assert_eq!(shell.workspaces().len(), 2);
    assert!(shell.placements().is_empty());
    shell
        .apply(Action::SwitchWorkspace { workspace: 2 })
        .unwrap();
    assert_eq!(shell.workspaces().len(), 1);
    assert_eq!(shell.active_workspace(), 1);
    assert_eq!(shell.placements().len(), 3);

    // Closing a workspace's last window closes the workspace.
    shell
        .apply(Action::MoveToWorkspace {
            window: Some(c.get()),
            workspace: 2,
        })
        .unwrap();
    assert_eq!(shell.workspaces().len(), 2);
    shell.unmap_window(c).unwrap();
    assert_eq!(shell.workspaces().len(), 1);
}

#[test]
fn workspaces_are_capped() {
    let mut shell = desktop();
    for n in 2..=MAX_WORKSPACES {
        let (w, _) = shell.map_window("app", "x");
        shell
            .apply(Action::MoveToWorkspace {
                window: Some(w.get()),
                workspace: n,
            })
            .unwrap();
    }
    assert!(!shell.can_add_workspace());
    shell.map_window("app", "x");
    assert_eq!(
        shell.apply(Action::MoveToWorkspace {
            window: None,
            workspace: MAX_WORKSPACES + 1,
        }),
        Err(Error::UnknownWorkspace(MAX_WORKSPACES + 1))
    );
}

#[test]
fn the_bar_can_move_to_the_bottom_or_hide() {
    use derisk_settings::BarPosition;
    let mut shell = desktop();
    let bar = shell.profile().top_bar;
    assert_eq!(shell.work_area(), rect(0, bar, 1920, 1080 - bar));
    shell.effects.top_bar.position = BarPosition::Bottom;
    assert_eq!(shell.work_area(), rect(0, 0, 1920, 1080 - bar));
    assert_eq!(shell.bar_area(), rect(0, 1080 - bar, 1920, bar));
    let (a, _) = shell.map_window("editor", "notes");
    let f = frame_of(&shell, a);
    assert!(f.loc.y + f.size.h <= 1080 - bar, "{f:?}");
    shell.effects.top_bar.autohide = true;
    assert_eq!(shell.work_area(), rect(0, 0, 1920, 1080));
}

#[test]
fn requests_for_the_agent_open_a_new_chat() {
    let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
    let outcome = shell.run([Action::AskAgent {
        text: "tidy my downloads & sort by date".into(),
    }]);
    assert_eq!(
        outcome.effects,
        [Effect::Open {
            path: "derisk-agent:ask?prompt=tidy+my+downloads+%26+sort+by+date".into()
        }]
    );
}

#[test]
fn a_dialog_floats_centred_on_its_parent() {
    let mut shell = desktop();
    // The parent is in the stack beside the first window, where a third
    // tile would have halved it.
    let (_first, _) = shell.map_window("other", "Other");
    let (main, _) = shell.map_window("app", "Main");
    let (dialog, _) = shell.map_window("app", "Save as");
    shell.set_parent(dialog, Some(main)).unwrap();
    let (p, d) = (frame_of(&shell, main), frame_of(&shell, dialog));
    assert!(matches!(shell.mode(dialog), Some(Mode::Floating { .. })));
    // Twice the centre, so odd sizes compare exactly, give or take the
    // pixel halving loses.
    let centre = |g: mcsapi::Geometry| (2 * g.loc.x + g.size.w, 2 * g.loc.y + g.size.h);
    let ((dx, dy), (px, py)) = (centre(d), centre(p));
    assert!(
        (dx - px).abs() <= 1 && (dy - py).abs() <= 1,
        "centred on the parent"
    );
    assert!(d.size.w < p.size.w && d.size.h < p.size.h);
}

#[test]
fn a_floating_window_takes_the_size_it_draws_at() {
    let mut shell = desktop();
    let (_first, _) = shell.map_window("other", "Other");
    let (calculator, _) = shell.map_window("macos.com.apple.calculator", "Calculator");
    shell
        .apply(Action::Float {
            window: Some(calculator.get()),
        })
        .unwrap();
    let before = frame_of(&shell, calculator);
    shell.fit_floating(calculator, (230, 408)).unwrap();
    let after = frame_of(&shell, calculator);
    assert_eq!((after.size.w, after.size.h), (230, 408));
    let centre = |g: mcsapi::Geometry| (2 * g.loc.x + g.size.w, 2 * g.loc.y + g.size.h);
    let ((ax, ay), (bx, by)) = (centre(after), centre(before));
    assert!((ax - bx).abs() <= 1 && (ay - by).abs() <= 1, "same centre");

    // Larger than the work area, it is kept inside it.
    let area = shell.work_area();
    shell.fit_floating(calculator, (5000, 5000)).unwrap();
    let big = frame_of(&shell, calculator);
    assert_eq!((big.loc, big.size), (area.loc, area.size));

    // A tiled window keeps its tile.
    let (tiled, _) = shell.map_window("app", "Tiled");
    let tile = frame_of(&shell, tiled);
    shell.fit_floating(tiled, (100, 100)).unwrap();
    assert_eq!(frame_of(&shell, tiled), tile);
}

#[test]
fn a_modal_dialog_keeps_its_parents_input() {
    let mut shell = desktop();
    let (main, _) = shell.map_window("app", "Main");
    let (other, _) = shell.map_window("other", "Other");
    let (dialog, _) = shell.map_window("app", "Delete?");
    shell.set_parent(dialog, Some(main)).unwrap();
    shell.set_modal(dialog, true).unwrap();
    shell.apply(Action::Focus { window: main.get() }).unwrap();
    assert_eq!(
        shell.focused(),
        Some(dialog),
        "focusing the parent focuses the dialog"
    );
    shell
        .apply(Action::Focus {
            window: other.get(),
        })
        .unwrap();
    // A click in the parent's client area brings the dialog back and does
    // not reach the parent.
    let p = frame_of(&shell, main);
    let corner = (p.loc.x + 5, p.loc.y + p.size.h - 5);
    assert_eq!(
        shell.pointer_down(corner, 0).unwrap(),
        PointerOutcome::Handled { effects: vec![] }
    );
    assert_eq!(shell.focused(), Some(dialog));
    // Closed, it leaves the parent free.
    shell.unmap_window(dialog).unwrap();
    shell.apply(Action::Focus { window: main.get() }).unwrap();
    assert_eq!(shell.focused(), Some(main));
}

#[test]
fn attention_lasts_until_the_window_is_focused() {
    let mut shell = desktop();
    let (a, _) = shell.map_window("a", "A");
    let (b, _) = shell.map_window("b", "B");
    shell.set_attention(b, true).unwrap();
    assert_eq!(shell.wanting_attention().count(), 0, "b is focused already");
    shell.set_attention(a, true).unwrap();
    assert_eq!(shell.wanting_attention().collect::<Vec<_>>(), vec![a]);
    let ws = shell.workspace_of(a).unwrap();
    assert!(shell.workspace_wants_attention(ws));
    shell.apply(Action::Focus { window: a.get() }).unwrap();
    assert_eq!(shell.wanting_attention().count(), 0);
    assert!(!shell.workspace_wants_attention(ws));
}

#[test]
fn every_workspace_reports_where_its_windows_sit() {
    let mut shell = desktop();
    let (a, _) = shell.map_window("files", "Files");
    let (b, _) = shell.map_window("editor", "Untitled");
    let first = shell.workspaces()[0];
    let tiled = shell.placements();
    // A second workspace with one window, then back to the first.
    shell
        .apply(Action::SwitchWorkspace { workspace: 2 })
        .unwrap();
    let (c, _) = shell.map_window("terminal", "sh");
    let second = shell.workspaces()[1];
    shell
        .apply(Action::SwitchWorkspace { workspace: 1 })
        .unwrap();

    // The active workspace: its placements, focus and all.
    assert_eq!(shell.placements_on(first), shell.placements());
    let windows: Vec<_> = shell
        .placements_on(first)
        .iter()
        .map(|p| p.window)
        .collect();
    assert_eq!(windows, [a, b]);
    // Another one: laid out the same way, with nothing focused.
    let others = shell.placements_on(second);
    assert_eq!(others.len(), 1);
    assert_eq!(others[0].window, c);
    assert!(!others[0].focused);
    assert_eq!(others[0].frame, shell.work_area());
    assert_eq!(tiled.len(), 2);
}

#[test]
fn a_window_opened_over_snapped_halves_floats_on_top() {
    let mut shell = desktop();
    shell.map_window("org.derisk.files", "Files");
    shell
        .apply(Action::Snap {
            window: None,
            zone: SnapZone::Left,
        })
        .unwrap();
    shell.map_window("org.derisk.editor", "Text Editor");
    shell
        .apply(Action::Snap {
            window: None,
            zone: SnapZone::Right,
        })
        .unwrap();
    let (settings, _) = shell.map_window("org.derisk.settings", "Settings");

    assert!(matches!(shell.mode(settings), Some(Mode::Floating { .. })));
    // Placements are drawn bottom to top: the new window is last, over both
    // halves, at a usable size rather than the gap between them.
    let placements = shell.placements();
    assert_eq!(placements.last().map(|p| p.window), Some(settings));
    let frame = frame_of(&shell, settings);
    let area = shell.work_area();
    assert!(frame.size.w >= area.size.w / 2 && frame.size.h >= area.size.h / 2);
}

#[test]
fn a_window_kept_on_top_stays_above_the_focused_one() {
    let mut shell = desktop();
    let (pinned, _) = shell.map_window("clock", "Clock");
    shell.apply(Action::Float { window: None }).unwrap();
    shell
        .apply(Action::KeepOnTop {
            window: None,
            on: None,
        })
        .unwrap();
    assert!(shell.is_on_top(pinned));
    let (other, _) = shell.map_window("editor", "notes.md");
    shell.apply(Action::Float { window: None }).unwrap();
    assert_eq!(shell.focused(), Some(other));
    let order: Vec<WindowId> = shell.placements().iter().map(|p| p.window).collect();
    assert_eq!(order.last(), Some(&pinned), "{order:?}");
    // Where both are, a press reaches the one on top.
    let f = frame_of(&shell, pinned);
    let inside = (f.loc.x + f.size.w / 2, f.loc.y + f.size.h / 2);
    assert_eq!(shell.window_at(inside).map(|p| p.window), Some(pinned));

    // Flipped again, it is stacked like any other.
    shell
        .apply(Action::KeepOnTop {
            window: Some(pinned.get()),
            on: None,
        })
        .unwrap();
    assert!(!shell.is_on_top(pinned));
    shell
        .apply(Action::Focus {
            window: other.get(),
        })
        .unwrap();
    let order: Vec<WindowId> = shell.placements().iter().map(|p| p.window).collect();
    assert_eq!(order.last(), Some(&other), "{order:?}");
}

#[test]
fn an_untrusted_machines_window_cannot_be_kept_on_top() {
    use derisk::shell::WindowMachine;
    use derisk_machines::Label;

    let machine = |label| WindowMachine {
        name: "mail".into(),
        label,
    };
    let mut shell = desktop();
    let (window, _) = shell.map_window("foot", "Foot");
    shell
        .set_window_machine(window, machine(Label::Red))
        .unwrap();
    assert!(shell.is_untrusted(window));
    let refused = shell.apply(Action::KeepOnTop {
        window: None,
        on: Some(true),
    });
    assert!(matches!(refused, Err(Error::UntrustedOnTop(name)) if name == "mail"));
    assert!(!shell.is_on_top(window));

    // A trusted machine's window may be, until the compositor learns the
    // window is the untrusted machine's after all.
    let (other, _) = shell.map_window("foot", "Foot");
    shell
        .set_window_machine(other, machine(Label::Blue))
        .unwrap();
    shell
        .apply(Action::KeepOnTop {
            window: None,
            on: Some(true),
        })
        .unwrap();
    assert!(shell.is_on_top(other));
    shell
        .set_window_machine(other, machine(Label::Red))
        .unwrap();
    assert!(!shell.is_on_top(other));
}

#[test]
fn show_desktop_hides_every_window_and_brings_them_back() {
    let mut shell = desktop();
    let (a, _) = shell.map_window("a", "A");
    let (b, _) = shell.map_window("b", "B");
    shell.apply(Action::ShowDesktop).unwrap();
    assert!(shell.is_minimized(a) && shell.is_minimized(b));
    assert_eq!(shell.focused(), None);
    shell.apply(Action::ShowDesktop).unwrap();
    assert!(!shell.is_minimized(a) && !shell.is_minimized(b));
    assert_eq!(shell.focused(), Some(b));
}

#[test]
fn stepping_workspaces_and_carrying_a_window_along() {
    let mut shell = desktop();
    let (a, _) = shell.map_window("a", "A");
    let (b, _) = shell.map_window("b", "B");
    // Nothing past the last workspace without a window to take there.
    shell
        .apply(Action::StepWorkspace {
            by: 1,
            carry_window: false,
        })
        .unwrap();
    assert_eq!(shell.active_workspace(), 1);
    shell
        .apply(Action::StepWorkspace {
            by: 1,
            carry_window: true,
        })
        .unwrap();
    assert_eq!(shell.active_workspace(), 2);
    assert_eq!(shell.focused(), Some(b));
    shell
        .apply(Action::StepWorkspace {
            by: -1,
            carry_window: false,
        })
        .unwrap();
    assert_eq!(shell.active_workspace(), 1);
    assert_eq!(shell.focused(), Some(a));
}

#[test]
fn session_keys_ask_on_screen_as_the_person() {
    let mut shell = desktop();
    assert_eq!(
        shell
            .apply(Action::ConfirmSession {
                op: SessionOp::PowerOff
            })
            .unwrap(),
        []
    );
    assert_eq!(shell.pending_confirmation(), Some(SessionOp::PowerOff));
    assert!(!shell.confirmation_from_agent());
    assert_eq!(
        shell.apply(Action::Confirm { accept: true }).unwrap(),
        [Effect::Session {
            op: SessionOp::PowerOff
        }]
    );
}

#[test]
fn the_tour_covers_the_desktop_and_is_remembered_when_it_ends() {
    let mut shell = desktop();
    shell
        .apply(Action::Palette {
            visible: Some(true),
        })
        .unwrap();
    assert_eq!(
        shell
            .apply(Action::Tutorial {
                visible: Some(true)
            })
            .unwrap(),
        vec![]
    );
    assert!(shell.tutorial_visible());
    assert!(
        !shell.palette_visible(),
        "nothing covers what the tour shows"
    );
    assert_eq!(
        shell
            .apply(Action::Tutorial {
                visible: Some(false)
            })
            .unwrap(),
        vec![Effect::TutorialSeen]
    );
    // Closing it again remembers nothing new.
    assert_eq!(
        shell
            .apply(Action::Tutorial {
                visible: Some(false)
            })
            .unwrap(),
        vec![]
    );
}
