use derisk::{
    action::{Action, Effect},
    decorations::{Button, Hit, TitleBar},
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
fn dragging_a_title_bar_to_the_left_edge_snaps_and_offers_snap_assist() {
    let mut shell = desktop();
    let (a, _) = shell.map_window("a", "A");
    let (b, _) = shell.map_window("b", "B");
    let f = frame_of(&shell, b);
    let grab = (f.loc.x + f.size.w / 2, f.loc.y + 10);
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
    let f = frame_of(&shell, a);
    let grab = (f.loc.x + 300, f.loc.y + 10);
    shell.pointer_down(grab, 0).unwrap();
    shell.pointer_motion((900, 500));
    shell.pointer_up();
    assert!(matches!(shell.mode(a), Some(Mode::Floating { .. })));
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
    let grab = (f.loc.x + 100, f.loc.y + 10);
    shell.pointer_down(grab, 1000).unwrap();
    shell.pointer_motion((grab.0 + 2, grab.1 + 2));
    shell.pointer_up();
    assert_eq!(shell.mode(a), Some(Mode::Tiled));
}

#[test]
fn title_bar_buttons_sit_on_the_left() {
    let bar = TitleBar::default();
    let frame = rect(100, 100, 800, 600);
    let buttons: Vec<_> = bar.buttons(frame).collect();
    assert_eq!(
        buttons.iter().map(|(b, _)| *b).collect::<Vec<_>>(),
        Button::ORDER
    );
    for (_, g) in &buttons {
        assert!(g.loc.x < frame.loc.x + frame.size.w / 4);
    }
    let close = buttons[0].1;
    let center = (
        close.loc.x + close.size.w / 2,
        close.loc.y + close.size.h / 2,
    );
    assert_eq!(bar.hit(frame, center), Some(Hit::Button(Button::Close)));
    assert_eq!(bar.hit(frame, (850, 110)), Some(Hit::Title));
    assert_eq!(bar.hit(frame, (850, 500)), Some(Hit::Client));
}

#[test]
fn close_button_click_emits_close_effect() {
    let mut shell = desktop();
    let (a, _) = shell.map_window("a", "A");
    let f = frame_of(&shell, a);
    let (_, close) = TitleBar::default().buttons(f).next().unwrap();
    let outcome = shell
        .pointer_down((close.loc.x + 7, close.loc.y + 7), 0)
        .unwrap();
    assert_eq!(
        outcome,
        PointerOutcome::Handled {
            effects: vec![Effect::Close { window: a.get() }]
        }
    );
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
fn requests_for_sonnes_agent_open_its_panel() {
    let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
    let outcome = shell.run([Action::AskAgent {
        text: "tidy my downloads & sort by date".into(),
    }]);
    assert_eq!(
        outcome.effects,
        [Effect::Open {
            path: "zed://agent?prompt=tidy+my+downloads+%26+sort+by+date".into()
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
