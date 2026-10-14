use derisk::{
    action::Action,
    animation::StartupFrame,
    geom::rect,
    overview::{OverviewLayout, fit, grid, row},
    shell::Shell,
    ui::{ShellUi, to_rect},
};
use derisk_settings::LowPower;
use mcsapi::toolkit::egui;

fn frame(
    ctx: &egui::Context,
    ui: &mut ShellUi,
    shell: &Shell,
    size: (f32, f32),
    events: Vec<egui::Event>,
    ms: u32,
) -> Vec<Action> {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(size.0, size.1),
        )),
        events,
        ..Default::default()
    };
    let mut actions = Vec::new();
    let mut out = ctx.run_ui(input, |root| {
        ui.paint_decorations(root.painter(), shell);
        actions = ui.show(root, shell, ms);
    });
    assert!(!out.shapes.is_empty());
    out.textures_delta.clear();
    actions
}

#[test]
fn renders_every_form_factor_with_and_without_overview() {
    for (w, h, touch) in [(1920, 1080, false), (1280, 800, true), (400, 800, true)] {
        let mut shell = Shell::new(rect(0, 0, w, h), touch);
        shell.map_window("editor", "notes.md");
        shell.map_window("terminal", "sh");
        shell.failed_units = vec!["foo.service".into()];
        let mut ui = ShellUi::new(&shell, false);
        let ctx = egui::Context::default();
        for ms in [0, 900, 5000] {
            frame(&ctx, &mut ui, &shell, (w as f32, h as f32), vec![], ms);
        }
        shell
            .apply(Action::Overview {
                visible: Some(true),
            })
            .unwrap();
        frame(&ctx, &mut ui, &shell, (w as f32, h as f32), vec![], 5000);
    }
}

#[test]
fn startup_cover_swallows_clicks() {
    let shell = Shell::new(rect(0, 0, 1920, 1080), false);
    let mut ui = ShellUi::new(&shell, false);
    let ctx = egui::Context::default();
    let click = |pressed| egui::Event::PointerButton {
        pos: egui::pos2(14.0, 14.0),
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    frame(&ctx, &mut ui, &shell, (1920.0, 1080.0), vec![], 0);
    let actions = frame(
        &ctx,
        &mut ui,
        &shell,
        (1920.0, 1080.0),
        vec![click(true), click(false)],
        10,
    );
    assert!(actions.is_empty());
}

#[test]
fn overview_button_in_the_top_bar_toggles_the_overview() {
    let shell = Shell::new(rect(0, 0, 1920, 1080), false);
    let mut ui = ShellUi::new(&shell, false);
    let ctx = egui::Context::default();
    let click = |pressed| egui::Event::PointerButton {
        pos: egui::pos2(16.0, 14.0),
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    let size = (1920.0, 1080.0);
    frame(
        &ctx,
        &mut ui,
        &shell,
        size,
        vec![egui::Event::PointerMoved(egui::pos2(16.0, 14.0))],
        5000,
    );
    frame(&ctx, &mut ui, &shell, size, vec![click(true)], 5010);
    let actions = frame(&ctx, &mut ui, &shell, size, vec![click(false)], 5020);
    assert_eq!(actions, vec![Action::Overview { visible: None }]);
}

/// Opens workspace 2 with one window on it (workspaces are dynamic).
fn open_second_workspace(shell: &mut Shell) {
    let (w, _) = shell.map_window("foot", "sh");
    shell
        .apply(Action::MoveToWorkspace {
            window: Some(w.get()),
            workspace: 2,
        })
        .unwrap();
}

#[test]
fn palette_runs_the_selected_entry_and_confirms_destructive_ones() {
    let (w, h) = (1280.0, 800.0);
    let mut shell = Shell::new(rect(0, 0, 1280, 800), false);
    open_second_workspace(&mut shell);
    shell.map_window("editor", "notes.md");
    shell.apply(Action::Palette { visible: None }).unwrap();
    let mut ui = ShellUi::new(&shell, true);
    let ctx = egui::Context::default();
    let key = |key| egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Default::default(),
    };
    let text = |t: &str| egui::Event::Text(t.to_owned());

    // Opening focuses the search field, so typing goes straight in.
    frame(&ctx, &mut ui, &shell, (w, h), vec![], 5000);
    frame(
        &ctx,
        &mut ui,
        &shell,
        (w, h),
        vec![text("workspace 2")],
        5000,
    );
    let actions = frame(
        &ctx,
        &mut ui,
        &shell,
        (w, h),
        vec![key(egui::Key::Enter)],
        5000,
    );
    assert_eq!(
        actions,
        [
            Action::SwitchWorkspace { workspace: 2 },
            Action::Palette {
                visible: Some(false)
            }
        ]
    );

    // Arrow down picks the next match.
    let mut ui = ShellUi::new(&shell, true);
    let ctx = egui::Context::default();
    frame(&ctx, &mut ui, &shell, (w, h), vec![], 5000);
    frame(
        &ctx,
        &mut ui,
        &shell,
        (w, h),
        vec![text("workspace 2")],
        5000,
    );
    let actions = frame(
        &ctx,
        &mut ui,
        &shell,
        (w, h),
        vec![key(egui::Key::ArrowDown), key(egui::Key::Enter)],
        5000,
    );
    assert_eq!(
        actions[0],
        Action::MoveToWorkspace {
            window: None,
            workspace: 2
        }
    );

    // Shutting down takes a second Enter.
    let mut ui = ShellUi::new(&shell, true);
    let ctx = egui::Context::default();
    frame(&ctx, &mut ui, &shell, (w, h), vec![], 5000);
    frame(&ctx, &mut ui, &shell, (w, h), vec![text("shut down")], 5000);
    let first = frame(
        &ctx,
        &mut ui,
        &shell,
        (w, h),
        vec![key(egui::Key::Enter)],
        5000,
    );
    assert!(first.is_empty());
    let second = frame(
        &ctx,
        &mut ui,
        &shell,
        (w, h),
        vec![key(egui::Key::Enter)],
        5000,
    );
    assert_eq!(
        second[0],
        Action::Session {
            op: derisk::systemd::SessionOp::PowerOff,
            confirmed: true
        }
    );

    // Escape closes without running anything.
    let actions = frame(
        &ctx,
        &mut ui,
        &shell,
        (w, h),
        vec![key(egui::Key::Escape)],
        5000,
    );
    assert_eq!(
        actions,
        [Action::Palette {
            visible: Some(false)
        }]
    );
}

fn key(key: egui::Key) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Default::default(),
    }
}

#[test]
fn palette_hands_requests_to_the_assistant_and_shows_the_conversation() {
    use derisk::{conversation::Source, ui::Ask};

    let (w, h) = (1280.0, 800.0);
    let mut shell = Shell::new(rect(0, 0, 1280, 800), false);
    shell.apply(Action::Palette { visible: None }).unwrap();
    let mut ui = ShellUi::new(&shell, true);
    let ctx = egui::Context::default();
    frame(&ctx, &mut ui, &shell, (w, h), vec![], 5000);
    frame(
        &ctx,
        &mut ui,
        &shell,
        (w, h),
        vec![egui::Event::Text(
            "open kitty then go to workspace 2".into(),
        )],
        5000,
    );
    // The palette stays open: the request runs in the conversation.
    let actions = frame(
        &ctx,
        &mut ui,
        &shell,
        (w, h),
        vec![key(egui::Key::Enter)],
        5000,
    );
    assert!(actions.is_empty());
    let asks = ui.take_asks();
    assert_eq!(
        asks,
        [Ask {
            text: "open kitty then go to workspace 2".into(),
            confirmed: false
        }]
    );
    shell
        .ask(&asks[0].text, Source::User, false)
        .into_result()
        .unwrap();
    frame(&ctx, &mut ui, &shell, (w, h), vec![], 5000);

    // Follow-ups go straight to the assistant, and shutting down needs a
    // second Enter.
    frame(
        &ctx,
        &mut ui,
        &shell,
        (w, h),
        vec![egui::Event::Text("shut down".into())],
        5000,
    );
    frame(
        &ctx,
        &mut ui,
        &shell,
        (w, h),
        vec![key(egui::Key::Enter)],
        5000,
    );
    assert!(ui.take_asks().is_empty());
    frame(
        &ctx,
        &mut ui,
        &shell,
        (w, h),
        vec![key(egui::Key::Enter)],
        5000,
    );
    assert_eq!(
        ui.take_asks(),
        [Ask {
            text: "shut down".into(),
            confirmed: true
        }]
    );

    // Backspace on an empty field goes back to searching.
    frame(
        &ctx,
        &mut ui,
        &shell,
        (w, h),
        vec![key(egui::Key::Backspace)],
        5000,
    );
    frame(
        &ctx,
        &mut ui,
        &shell,
        (w, h),
        vec![egui::Event::Text("lock screen".into())],
        5000,
    );
    let actions = frame(
        &ctx,
        &mut ui,
        &shell,
        (w, h),
        vec![key(egui::Key::Enter)],
        5000,
    );
    assert!(
        matches!(
            actions[0],
            Action::Session {
                op: derisk::systemd::SessionOp::Lock,
                ..
            }
        ),
        "{actions:?}"
    );
}

#[test]
fn question_mark_opens_the_conversation() {
    let (w, h) = (1280.0, 800.0);
    let mut shell = Shell::new(rect(0, 0, 1280, 800), false);
    shell.apply(Action::Palette { visible: None }).unwrap();
    let mut ui = ShellUi::new(&shell, true);
    let ctx = egui::Context::default();
    frame(&ctx, &mut ui, &shell, (w, h), vec![], 5000);
    frame(
        &ctx,
        &mut ui,
        &shell,
        (w, h),
        vec![egui::Event::Text("?lock".into())],
        5000,
    );
    frame(
        &ctx,
        &mut ui,
        &shell,
        (w, h),
        vec![key(egui::Key::Enter)],
        5000,
    );
    assert_eq!(ui.take_asks()[0].text, "lock");
}

#[test]
fn typing_on_the_overview_opens_the_palette() {
    let (w, h) = (1280.0, 800.0);
    let mut shell = Shell::new(rect(0, 0, 1280, 800), false);
    shell
        .apply(Action::Overview {
            visible: Some(true),
        })
        .unwrap();
    let mut ui = ShellUi::new(&shell, true);
    let ctx = egui::Context::default();
    frame(&ctx, &mut ui, &shell, (w, h), vec![], 5000);
    let actions = frame(
        &ctx,
        &mut ui,
        &shell,
        (w, h),
        vec![egui::Event::Text("o".into())],
        5000,
    );
    assert_eq!(
        actions,
        [Action::Palette {
            visible: Some(true)
        }]
    );
    shell.run(actions).into_result().unwrap();
    frame(
        &ctx,
        &mut ui,
        &shell,
        (w, h),
        vec![egui::Event::Text("pen kitty and snap it left".into())],
        5000,
    );
    frame(
        &ctx,
        &mut ui,
        &shell,
        (w, h),
        vec![key(egui::Key::Enter)],
        5000,
    );
    assert_eq!(ui.take_asks()[0].text, "open kitty and snap it left");
}

#[test]
fn failed_units_open_the_palette_on_their_actions() {
    let (w, h) = (1280.0, 800.0);
    let mut shell = Shell::new(rect(0, 0, 1280, 800), false);
    shell.failed_units = vec!["foo.service".into()];
    let mut ui = ShellUi::new(&shell, true);
    ui.palette.preset = Some("> failed".into());
    shell.apply(Action::Palette { visible: None }).unwrap();
    let ctx = egui::Context::default();
    frame(&ctx, &mut ui, &shell, (w, h), vec![], 5000);
    let actions = frame(
        &ctx,
        &mut ui,
        &shell,
        (w, h),
        vec![egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }],
        5000,
    );
    assert_eq!(
        actions[0],
        Action::RestartUnit {
            unit: "foo.service".into()
        }
    );
}

#[test]
fn translucent_panels_report_blur_areas_unless_low_power() {
    let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
    shell.map_window("editor", "notes.md");
    shell
        .apply(Action::Overview {
            visible: Some(true),
        })
        .unwrap();
    let mut ui = ShellUi::new(&shell, false);
    let ctx = egui::Context::default();
    frame(&ctx, &mut ui, &shell, (1920.0, 1080.0), vec![], 5000);
    let bar = shell.profile().top_bar;
    let areas: Vec<_> = ui.blur_regions().iter().map(|b| b.area).collect();
    assert!(areas.contains(&rect(0, 0, 1920, bar)), "{areas:?}");
    assert!(areas.contains(&shell.work_area()), "{areas:?}");
    assert!(ui.blur_regions().iter().all(|b| b.strength > 0));

    shell.effects.low_power = LowPower::On;
    frame(&ctx, &mut ui, &shell, (1920.0, 1080.0), vec![], 5100);
    assert!(ui.blur_regions().is_empty());

    shell.effects.low_power = LowPower::Off;
    shell.effects.blur = 0;
    frame(&ctx, &mut ui, &shell, (1920.0, 1080.0), vec![], 5200);
    assert!(ui.blur_regions().is_empty());
}

#[test]
fn opaque_panels_are_not_blurred() {
    let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
    shell
        .apply(Action::Overview {
            visible: Some(true),
        })
        .unwrap();
    shell.effects.panels.top_bar = 100;
    let mut ui = ShellUi::new(&shell, false);
    let ctx = egui::Context::default();
    frame(&ctx, &mut ui, &shell, (1920.0, 1080.0), vec![], 5000);
    let bar = shell.profile().top_bar;
    let areas: Vec<_> = ui.blur_regions().iter().map(|b| b.area).collect();
    assert!(!areas.contains(&rect(0, 0, 1920, bar)), "{areas:?}");
    assert!(areas.contains(&shell.work_area()), "{areas:?}");
}

#[test]
fn low_power_skips_the_startup_animation() {
    let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
    shell.effects.low_power = LowPower::On;
    let ui = ShellUi::new(&shell, false);
    assert_eq!(ui.startup_frame(0), StartupFrame::DONE);
    shell.effects.low_power = LowPower::Off;
    shell.effects.reduce_motion = true;
    let ui = ShellUi::new(&shell, true);
    assert!(!ui.startup_frame(0).done);
}
/// Drags the first exposé window onto workspace strip slot `slot` (0-based).
fn drag_to_slot(shell: &mut Shell, slot: usize, slots: usize) -> Vec<Action> {
    drag_to_slot_with(shell, slot, slots, |_| {}, vec![])
}

/// Like [`drag_to_slot`], but once the drag is under way `mid_drag` changes
/// the shell and `extra` events are sent with the move onto the slot.
fn drag_to_slot_with(
    shell: &mut Shell,
    slot: usize,
    slots: usize,
    mid_drag: impl FnOnce(&mut Shell),
    extra: Vec<egui::Event>,
) -> Vec<Action> {
    let size = (1920.0, 1080.0);
    let mut ui = ShellUi::new(shell, false);
    let ctx = egui::Context::default();
    let layout = OverviewLayout::new(shell.work_area(), shell.profile().form_factor);
    let active = shell.workspaces()[shell.active_workspace() as usize - 1];
    let windows = shell.windows_on(active);
    let cell = grid(windows.len(), layout.windows, 24).next().unwrap();
    let frame0 = shell
        .placements()
        .into_iter()
        .find(|p| p.window == windows[0])
        .unwrap()
        .frame;
    let from = to_rect(fit(frame0, cell)).center();
    let to = to_rect(row(slots, layout.workspaces, 10)[slot]).center();
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    let mut steps = [
        vec![egui::Event::PointerMoved(from)],
        vec![button(from, true)],
        vec![egui::Event::PointerMoved(from + egui::vec2(30.0, -30.0))],
        vec![egui::Event::PointerMoved(to)],
        vec![button(to, false)],
    ];
    steps[3].extend(extra);
    let mut mid_drag = Some(mid_drag);
    let mut actions = Vec::new();
    for (i, events) in steps.into_iter().enumerate() {
        if i == 3 {
            (mid_drag.take().expect("runs once"))(shell);
        }
        actions.extend(frame(
            &ctx,
            &mut ui,
            shell,
            size,
            events,
            5000 + i as u32 * 16,
        ));
    }
    actions
}

fn overview_with_two_windows() -> (Shell, u64) {
    let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
    let (a, _) = shell.map_window("editor", "notes.md");
    shell.map_window("terminal", "sh");
    shell
        .apply(Action::Overview {
            visible: Some(true),
        })
        .unwrap();
    (shell, a.get())
}

#[test]
fn dragging_a_window_onto_plus_opens_a_new_workspace() {
    let (mut shell, a) = overview_with_two_windows();
    // Strip: workspace 1, then "+".
    let actions = drag_to_slot(&mut shell, 1, 2);
    assert_eq!(
        actions,
        vec![Action::MoveToWorkspace {
            window: Some(a),
            workspace: 2
        }]
    );
    shell.run(actions).into_result().unwrap();
    assert_eq!(shell.workspaces().len(), 2);
    assert!(shell.overview_visible());
}

#[test]
fn dragging_a_window_onto_a_workspace_moves_it_there() {
    let (mut shell, a) = overview_with_two_windows();
    let (c, _) = shell.map_window("browser", "web");
    shell
        .apply(Action::MoveToWorkspace {
            window: Some(c.get()),
            workspace: 2,
        })
        .unwrap();
    // Strip: workspaces 1 and 2, then "+".
    let actions = drag_to_slot(&mut shell, 1, 3);
    assert_eq!(
        actions,
        vec![Action::MoveToWorkspace {
            window: Some(a),
            workspace: 2
        }]
    );
    // Dropping back on the current workspace does nothing.
    assert!(drag_to_slot(&mut shell, 0, 3).is_empty());
}

#[test]
fn other_buttons_do_not_drop_a_dragged_window() {
    let (mut shell, a) = overview_with_two_windows();
    let secondary = |pressed| egui::Event::PointerButton {
        pos: egui::pos2(500.0, 60.0),
        button: egui::PointerButton::Secondary,
        pressed,
        modifiers: Default::default(),
    };
    // A right click mid-drag leaves the drag alive; the primary release drops it.
    let actions = drag_to_slot_with(
        &mut shell,
        1,
        2,
        |_| {},
        vec![secondary(true), secondary(false)],
    );
    assert_eq!(
        actions,
        vec![Action::MoveToWorkspace {
            window: Some(a),
            workspace: 2
        }]
    );
}

#[test]
fn switching_workspaces_mid_drag_cancels_it() {
    let (mut shell, _) = overview_with_two_windows();
    let (c, _) = shell.map_window("browser", "web");
    shell
        .apply(Action::MoveToWorkspace {
            window: Some(c.get()),
            workspace: 2,
        })
        .unwrap();
    // Super+2 during the drag: the dragged window is no longer on screen,
    // so releasing over the "+" slot must not move it.
    let actions = drag_to_slot_with(
        &mut shell,
        2,
        3,
        |shell| {
            shell
                .apply(Action::SwitchWorkspace { workspace: 2 })
                .unwrap();
        },
        vec![],
    );
    assert!(actions.is_empty(), "{actions:?}");
}

/// Presses at `from`, moves through `path`, and releases at the last point,
/// one frame per event, returning the actions of every frame.
fn touch(
    ctx: &egui::Context,
    ui: &mut ShellUi,
    shell: &Shell,
    size: (f32, f32),
    from: egui::Pos2,
    path: &[egui::Pos2],
) -> Vec<Action> {
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    let mut actions = Vec::new();
    let mut ms = 5000;
    let mut step = |events| {
        ms += 16;
        actions.extend(frame(ctx, ui, shell, size, events, ms));
    };
    step(vec![egui::Event::PointerMoved(from)]);
    step(vec![button(from, true)]);
    for p in path {
        step(vec![egui::Event::PointerMoved(*p)]);
    }
    let end = path.last().copied().unwrap_or(from);
    step(vec![button(end, false)]);
    step(vec![]);
    actions
}

#[test]
fn phones_get_a_navigation_bar_below_the_work_area() {
    let phone = Shell::new(rect(0, 0, 392, 872), true);
    let nav = phone.nav_bar().area(phone.output());
    assert!(nav.size.h >= derisk::mobile::TOUCH_TARGET);
    let area = phone.work_area();
    assert_eq!(area.loc.y + area.size.h, nav.loc.y);

    // The desktop layout is unchanged above the breakpoint.
    let desktop = Shell::new(rect(0, 0, 1920, 1080), false);
    assert_eq!(desktop.nav_bar().height, 0);
    let area = desktop.work_area();
    assert_eq!(area.loc.y + area.size.h, 1080);
}

#[test]
fn navigation_bar_buttons_and_swipes() {
    let size = (392.0, 872.0);
    let mut shell = Shell::new(rect(0, 0, 392, 872), true);
    shell.map_window("editor", "notes.md");
    shell.map_window("terminal", "sh");
    let mut ui = ShellUi::new(&shell, true);
    let ctx = egui::Context::default();
    frame(&ctx, &mut ui, &shell, size, vec![], 5000);
    let buttons = shell.nav_bar().buttons(shell.output());
    let center = |i: usize| to_rect(buttons[i].1).center();

    // Back with nothing open switches to the previous app.
    let actions = touch(&ctx, &mut ui, &shell, size, center(0), &[]);
    assert_eq!(actions, vec![Action::FocusPrevious]);
    // Home opens the overview, Apps the palette.
    let actions = touch(&ctx, &mut ui, &shell, size, center(1), &[]);
    assert_eq!(actions, vec![Action::Overview { visible: None }]);
    let actions = touch(&ctx, &mut ui, &shell, size, center(2), &[]);
    assert_eq!(actions, vec![Action::Palette { visible: None }]);

    // A swipe up that starts on a button goes home and is not a tap.
    let from = center(0);
    let path: Vec<_> = (1..=6)
        .map(|i| from - egui::vec2(0.0, 30.0 * i as f32))
        .collect();
    let actions = touch(&ctx, &mut ui, &shell, size, from, &path);
    assert_eq!(
        actions,
        vec![Action::Overview {
            visible: Some(true)
        }]
    );
    // Sideways along the bar switches apps.
    let from = center(1);
    let path: Vec<_> = (1..=5)
        .map(|i| from + egui::vec2(25.0 * i as f32, 0.0))
        .collect();
    let actions = touch(&ctx, &mut ui, &shell, size, from, &path);
    assert_eq!(actions, vec![Action::FocusNext]);
}

#[test]
fn navigation_bar_stays_usable_over_the_palette() {
    let size = (392.0, 872.0);
    let mut shell = Shell::new(rect(0, 0, 392, 872), true);
    shell.map_window("editor", "notes.md");
    shell.apply(Action::Palette { visible: None }).unwrap();
    let mut ui = ShellUi::new(&shell, true);
    let ctx = egui::Context::default();
    frame(&ctx, &mut ui, &shell, size, vec![], 5000);
    let home = to_rect(shell.nav_bar().buttons(shell.output())[1].1).center();
    let actions = touch(&ctx, &mut ui, &shell, size, home, &[]);
    assert_eq!(
        actions,
        vec![
            Action::Palette {
                visible: Some(false)
            },
            Action::Overview {
                visible: Some(true)
            }
        ]
    );
}

#[test]
fn phones_show_one_window_without_a_title_bar() {
    let mut phone = Shell::new(rect(0, 0, 392, 872), true);
    let (a, _) = phone.map_window("a", "A");
    let (b, _) = phone.map_window("b", "B");
    // Even a floating window, or a workspace switched to tall, fills the
    // work area alone.
    phone
        .apply(Action::Float {
            window: Some(a.get()),
        })
        .unwrap();
    phone
        .apply(Action::SetLayout {
            layout: derisk::action::LayoutKind::Tall,
        })
        .unwrap();
    phone.apply(Action::Focus { window: a.get() }).unwrap();
    let placements = phone.placements();
    assert_eq!(placements.len(), 1);
    assert_eq!(placements[0].window, a);
    assert_eq!(placements[0].frame, phone.work_area());
    assert_eq!(placements[0].client, phone.work_area());
    phone.apply(Action::Focus { window: b.get() }).unwrap();
    assert_eq!(phone.placements()[0].window, b);
}

#[test]
fn the_keyboard_types_into_the_palette_and_completes_words() {
    use derisk::keyboard::{self, Key, Keyboard};
    let size = (392.0, 872.0);
    let mut shell = Shell::new(rect(0, 0, 392, 872), true);
    shell.apply(Action::Palette { visible: None }).unwrap();
    let mut ui = ShellUi::new(&shell, true);
    let ctx = egui::Context::default();
    frame(&ctx, &mut ui, &shell, size, vec![], 5000);
    // Opening the palette brings the keyboard up above the navigation bar.
    assert!(shell.keyboard_visible());
    let nav = shell.nav_bar().area(shell.output());
    let area = rect(
        nav.loc.x,
        nav.loc.y - keyboard::HEIGHT,
        nav.size.w,
        keyboard::HEIGHT,
    );
    assert_eq!(shell.keyboard_area(), Some(area));
    let keys: Vec<_> = ui.keyboard.keys(area).collect();
    let at = |k: Key| to_rect(keys.iter().find(|(key, _)| *key == k).unwrap().1).center();
    for c in "tom".chars() {
        touch(&ctx, &mut ui, &shell, size, at(Key::Char(c)), &[]);
    }
    // The first letter was capitalized; tapping a suggestion finishes it.
    assert_eq!(ui.keyboard.word(), "Tom");
    let suggestions = ui.keyboard.suggestions();
    let first = Keyboard::suggestion_cells(area)[0];
    assert_eq!(suggestions[0], "Tomorrow");
    touch(&ctx, &mut ui, &shell, size, to_rect(first).center(), &[]);
    assert_eq!(ui.keyboard.word(), "");
    // Everything went to the palette's field, none to a window.
    assert!(ui.take_window_input().is_empty());
}

#[test]
fn the_keyboard_button_types_into_the_focused_window() {
    use derisk::keyboard::{self, Key, Output};
    let size = (392.0, 872.0);
    let mut shell = Shell::new(rect(0, 0, 392, 872), true);
    shell.map_window("editor", "notes.md");
    let mut ui = ShellUi::new(&shell, true);
    let ctx = egui::Context::default();
    frame(&ctx, &mut ui, &shell, size, vec![], 5000);
    assert!(!shell.keyboard_visible());
    let button = to_rect(shell.nav_bar().buttons(shell.output())[3].1).center();
    let outcome = shell.run(touch(&ctx, &mut ui, &shell, size, button, &[]));
    assert!(outcome.result.is_ok());
    assert!(shell.keyboard_visible());
    // It floats over the app, which shrinks to stay above it; the work area
    // itself is unchanged.
    let work = shell.work_area();
    assert_eq!(work.size.h, 872 - 32 - 56);
    let app = shell.placements()[0].frame;
    assert_eq!(app.size.h, work.size.h - keyboard::HEIGHT);
    let nav = shell.nav_bar().area(shell.output());
    let area = rect(
        nav.loc.x,
        nav.loc.y - keyboard::HEIGHT,
        nav.size.w,
        keyboard::HEIGHT,
    );
    let keys: Vec<_> = ui.keyboard.keys(area).collect();
    let at = |k: Key| to_rect(keys.iter().find(|(key, _)| *key == k).unwrap().1).center();
    touch(&ctx, &mut ui, &shell, size, at(Key::Char('h')), &[]);
    touch(&ctx, &mut ui, &shell, size, at(Key::Char('i')), &[]);
    touch(&ctx, &mut ui, &shell, size, at(Key::Backspace), &[]);
    assert_eq!(
        ui.take_window_input(),
        vec![
            Output::Text("H".into()),
            Output::Text("i".into()),
            Output::Backspace
        ]
    );
}

#[test]
fn the_keyboard_floats_over_desktop_windows_and_only_the_focused_one_moves() {
    use derisk::{keyboard, shell::KEYBOARD_MAX_WIDTH};
    let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
    let (a, _) = shell.map_window("files", "Files");
    let (b, _) = shell.map_window("editor", "notes.md");
    shell.apply(Action::Focus { window: b.get() }).unwrap();
    let before = shell.placements();
    assert_eq!(shell.keyboard_area(), None);
    shell.apply(Action::Keyboard { visible: None }).unwrap();
    let area = shell.keyboard_area().unwrap();
    // Phone-sized on a wide screen, centered along the bottom edge.
    assert_eq!(area.size.w, KEYBOARD_MAX_WIDTH);
    assert_eq!(area.size.h, keyboard::HEIGHT);
    assert_eq!(area.loc.x, (1920 - KEYBOARD_MAX_WIDTH) / 2);
    assert_eq!(area.loc.y + area.size.h, 1080);
    let after = shell.placements();
    let frame =
        |ps: &[derisk::shell::WindowPlacement], w| ps.iter().find(|p| p.window == w).unwrap().frame;
    // The unfocused window stays where it was, under the keyboard.
    assert_eq!(frame(&before, a), frame(&after, a));
    // The focused one ends at the keyboard's top edge.
    let focused = frame(&after, b);
    assert_eq!(focused.loc.y, frame(&before, b).loc.y);
    assert_eq!(focused.loc.y + focused.size.h, area.loc.y);
    shell
        .apply(Action::Keyboard {
            visible: Some(false),
        })
        .unwrap();
    assert_eq!(shell.placements(), before);
}

#[test]
fn a_short_window_slides_up_over_the_keyboard_instead_of_shrinking() {
    use derisk::shell::clear_of_keyboard;
    let area = rect(0, 28, 1280, 772);
    let keyboard = rect(160, 552, 960, 248);
    // Too short to lose 150 px: it moves up whole.
    let frame = rect(300, 500, 400, 200);
    assert_eq!(
        clear_of_keyboard(frame, keyboard, area),
        rect(300, 352, 400, 200)
    );
    // Beside the keyboard: untouched.
    let beside = rect(0, 500, 150, 300);
    assert_eq!(clear_of_keyboard(beside, keyboard, area), beside);
    // Taller than the room above the keyboard: pinned to the top and cut.
    let tall = rect(300, 100, 400, 680);
    assert_eq!(
        clear_of_keyboard(tall, keyboard, area),
        rect(300, 100, 400, 452)
    );
}

/// Where each piece of text a frame drew is, by its text.
fn texts(
    ctx: &egui::Context,
    ui: &mut ShellUi,
    shell: &Shell,
    size: (f32, f32),
) -> Vec<(String, egui::Rect)> {
    fn walk(shape: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
        match shape {
            egui::Shape::Text(text) => {
                out.push((text.galley.text().to_owned(), text.visual_bounding_rect()))
            }
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(size.0, size.1),
        )),
        ..Default::default()
    };
    let out = ctx.run_ui(input, |root| {
        ui.show(root, shell, 5000);
    });
    let mut found = Vec::new();
    for clipped in &out.shapes {
        walk(&clipped.shape, &mut found);
    }
    found
}

#[test]
fn the_overview_draws_the_widget_plugins_cards_and_runs_their_buttons() {
    let size = (1920.0, 1080.0);
    let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
    shell.failed_units = vec!["foo.service".into()];
    shell
        .widgets
        .register(
            serde_json::from_value(serde_json::json!({
                "id": "weather", "title": "Weather",
                "rows": [{"type": "text", "text": "18 °C, clear"}],
            }))
            .unwrap(),
            None,
        )
        .unwrap();
    shell
        .apply(Action::Overview {
            visible: Some(true),
        })
        .unwrap();
    let mut ui = ShellUi::new(&shell, true);
    let ctx = egui::Context::default();
    frame(&ctx, &mut ui, &shell, size, vec![], 5000);
    let drawn = texts(&ctx, &mut ui, &shell, size);
    let shown = |text: &str| drawn.iter().find(|(t, _)| t == text).map(|(_, r)| *r);
    for text in [
        "Suggested",
        "Services",
        "foo.service",
        "Notes",
        "Weather",
        "18 °C, clear",
    ] {
        assert!(shown(text).is_some(), "{text} not drawn: {drawn:?}");
    }
    // The clock's own copy in the top bar steps aside for the clock card.
    let time = shell.clock.time_label();
    assert_eq!(drawn.iter().filter(|(t, _)| *t == time).count(), 1);

    let restart = shown("Restart").expect("a Restart button").center();
    let click = |pressed| egui::Event::PointerButton {
        pos: restart,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    frame(
        &ctx,
        &mut ui,
        &shell,
        size,
        vec![egui::Event::PointerMoved(restart)],
        5000,
    );
    frame(&ctx, &mut ui, &shell, size, vec![click(true)], 5010);
    let actions = frame(&ctx, &mut ui, &shell, size, vec![click(false)], 5020);
    assert_eq!(
        actions,
        vec![Action::RestartUnit {
            unit: "foo.service".into()
        }]
    );
}

/// The accent rings drawn around windows in one frame.
fn focus_rings(shell: &Shell) -> Vec<egui::Rect> {
    let mut ui = ShellUi::new(shell, false);
    let accent = ui.theme.accent;
    let ctx = egui::Context::default();
    let input = egui::RawInput {
        screen_rect: Some(to_rect(shell.output())),
        ..Default::default()
    };
    let mut out = ctx.run_ui(input, |root| {
        ui.show(root, shell, 5000);
    });
    out.textures_delta.clear();
    out.shapes
        .into_iter()
        .filter_map(|clipped| match clipped.shape {
            egui::Shape::Rect(r)
                if r.stroke.color == accent && r.stroke.width == derisk::ui::FOCUS_RING =>
            {
                Some(r.rect)
            }
            _ => None,
        })
        .collect()
}

#[test]
fn the_focused_window_is_ringed_in_the_accent() {
    let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
    shell.map_window("files", "Files");
    let (editor, _) = shell.map_window("editor", "Untitled");
    let frame = shell
        .placements()
        .into_iter()
        .find(|p| p.window == editor)
        .unwrap()
        .frame;
    assert_eq!(focus_rings(&shell), vec![to_rect(frame)]);

    // A maximized window meets the screen edges and the top bar: no ring.
    shell
        .apply(Action::ToggleMaximize { window: None })
        .unwrap();
    assert!(focus_rings(&shell).is_empty());

    // Nor on a phone, where apps have no frame.
    let mut phone = Shell::new(rect(0, 0, 392, 872), true);
    phone.map_window("editor", "Untitled");
    assert!(focus_rings(&phone).is_empty());
}

/// How many of the overview's window cards are drawn from the windows'
/// pictures.
fn picture_cards(shell: &Shell) -> usize {
    // Without motion, so the cards are drawn where they settle.
    let mut ui = ShellUi::new(shell, true);
    ui.window_pictures = Some(derisk::ui::WindowPictures {
        texture: egui::TextureId::User(7),
        uv: Box::new(|area| area),
    });
    let ctx = egui::Context::default();
    let input = egui::RawInput {
        screen_rect: Some(to_rect(shell.output())),
        ..Default::default()
    };
    let mut out = ctx.run_ui(input, |root| {
        ui.show(root, shell, 5000);
    });
    out.textures_delta.clear();
    out.shapes
        .iter()
        .filter(|clipped| {
            matches!(&clipped.shape, egui::Shape::Rect(r)
                if r.fill_texture_id() == egui::TextureId::User(7))
        })
        .count()
}

#[test]
fn overview_cards_show_what_windows_hold() {
    let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
    shell.map_window("files", "Files");
    shell.map_window("editor", "Untitled");
    shell
        .apply(Action::Overview {
            visible: Some(true),
        })
        .unwrap();
    assert_eq!(picture_cards(&shell), 2);

    // A minimized window isn't on screen to copy: it keeps its icon.
    shell.apply(Action::Minimize { window: None }).unwrap();
    assert_eq!(picture_cards(&shell), 1);
}

/// One frame at `secs` on the context's clock: the actions, and the
/// shapes drawn.
fn timed(
    ctx: &egui::Context,
    ui: &mut ShellUi,
    shell: &Shell,
    events: Vec<egui::Event>,
    secs: f64,
) -> (Vec<Action>, Vec<egui::Shape>) {
    let input = egui::RawInput {
        screen_rect: Some(to_rect(shell.output())),
        time: Some(secs),
        events,
        ..Default::default()
    };
    let mut actions = Vec::new();
    let mut out = ctx.run_ui(input, |root| {
        actions = ui.show(root, shell, 60_000);
    });
    out.textures_delta.clear();
    (actions, out.shapes.into_iter().map(|c| c.shape).collect())
}

/// Whether the focus ring is drawn whole, and whether it is drawn at all.
fn ring_drawn(ui: &ShellUi, shapes: &[egui::Shape]) -> (bool, bool) {
    let accent = ui.theme.accent;
    let whole = shapes.iter().any(|s| {
        matches!(s, egui::Shape::Rect(r)
            if r.stroke.color == accent && r.stroke.width == derisk::ui::FOCUS_RING)
    });
    let tracing = shapes.iter().any(|s| {
        matches!(s, egui::Shape::Path(p)
            if p.stroke.width == derisk::ui::FOCUS_RING && !p.closed)
    });
    (whole, whole || tracing)
}

#[test]
fn a_newly_focused_windows_ring_traces_itself_in() {
    let mut shell = Shell::new(rect(0, 0, 1280, 800), false);
    shell.map_window("files", "Files");
    shell.map_window("terminal", "Terminal");
    let mut ui = ShellUi::new(&shell, false);
    let ctx = egui::Context::default();
    // What was focused from the start is ringed whole.
    let (_, shapes) = timed(&ctx, &mut ui, &shell, vec![], 1.0);
    assert_eq!(ring_drawn(&ui, &shapes), (true, true));

    shell.map_window("editor", "Untitled");
    let (_, shapes) = timed(&ctx, &mut ui, &shell, vec![], 2.0);
    assert_eq!(ring_drawn(&ui, &shapes), (false, false));
    let (_, shapes) = timed(&ctx, &mut ui, &shell, vec![], 2.2);
    assert_eq!(ring_drawn(&ui, &shapes), (false, true));
    // 40 frames.
    let (_, shapes) = timed(&ctx, &mut ui, &shell, vec![], 2.0 + 41.0 / 60.0);
    assert_eq!(ring_drawn(&ui, &shapes), (true, true));
}

#[test]
fn a_finished_request_folds_the_palette_into_the_dot_that_rings_the_window() {
    use derisk::{animation::DOT, conversation::Source};

    let mut shell = Shell::new(rect(0, 0, 1280, 800), false);
    shell.map_window("files", "Files");
    let mut ui = ShellUi::new(&shell, false);
    let ctx = egui::Context::default();
    timed(&ctx, &mut ui, &shell, vec![], 1.0);
    shell.apply(Action::Palette { visible: None }).unwrap();
    timed(&ctx, &mut ui, &shell, vec![], 2.0);
    timed(
        &ctx,
        &mut ui,
        &shell,
        vec![egui::Event::Text("?snap it left".into())],
        3.0,
    );
    timed(&ctx, &mut ui, &shell, vec![key(egui::Key::Enter)], 3.1);
    let asks = ui.take_asks();
    assert_eq!(asks.len(), 1);
    shell
        .ask(&asks[0].text, Source::User, false)
        .into_result()
        .unwrap();

    // "Done." holds for 30 frames, then the palette folds for 16.
    let (actions, _) = timed(&ctx, &mut ui, &shell, vec![], 4.0);
    assert!(actions.is_empty(), "{actions:?}");
    let (actions, _) = timed(&ctx, &mut ui, &shell, vec![], 4.49);
    assert!(actions.is_empty(), "{actions:?}");
    let (actions, _) = timed(&ctx, &mut ui, &shell, vec![], 4.5);
    assert!(actions.is_empty(), "{actions:?}");
    let (actions, _) = timed(&ctx, &mut ui, &shell, vec![], 4.5 + 17.0 / 60.0);
    assert_eq!(
        actions,
        [Action::Palette {
            visible: Some(false)
        }]
    );
    for a in actions {
        shell.apply(a).unwrap();
    }

    // The dot flies to the window while its ring waits for it.
    let accent = ui.theme.accent;
    let dot = |shapes: &[egui::Shape]| {
        shapes.iter().any(
            |s| matches!(s, egui::Shape::Circle(c) if c.fill == accent && c.radius == DOT / 2.0),
        )
    };
    let (_, shapes) = timed(&ctx, &mut ui, &shell, vec![], 5.0);
    assert!(dot(&shapes));
    assert_eq!(ring_drawn(&ui, &shapes), (false, false));
    // It lands 90 frames after the fold began, and the ring traces in.
    let landed = 4.5 + 1.5;
    let (_, shapes) = timed(&ctx, &mut ui, &shell, vec![], landed + 0.1);
    assert!(!dot(&shapes));
    assert_eq!(ring_drawn(&ui, &shapes), (false, true));
    let (_, shapes) = timed(&ctx, &mut ui, &shell, vec![], landed + 0.7);
    assert_eq!(ring_drawn(&ui, &shapes), (true, true));
}

#[test]
fn the_overview_rises_in_and_a_picked_card_grows_into_its_window() {
    let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
    shell.map_window("files", "Files");
    let (editor, _) = shell.map_window("editor", "Untitled");
    let mut ui = ShellUi::new(&shell, false);
    let ctx = egui::Context::default();
    timed(&ctx, &mut ui, &shell, vec![], 1.0);
    shell
        .apply(Action::Overview {
            visible: Some(true),
        })
        .unwrap();
    // A window card's name, and where it is drawn.
    let label = |shapes: &[egui::Shape]| {
        shapes.iter().find_map(|s| match s {
            egui::Shape::Text(t) if t.galley.text() == "Untitled" => Some(t.pos.y),
            _ => None,
        })
    };
    let (_, shapes) = timed(&ctx, &mut ui, &shell, vec![], 2.0);
    // Still below its place, and see-through: not drawn yet.
    assert_eq!(label(&shapes), None);
    let (_, shapes) = timed(&ctx, &mut ui, &shell, vec![], 2.3);
    let rising = label(&shapes).expect("the card rising in");
    let (_, shapes) = timed(&ctx, &mut ui, &shell, vec![], 3.0);
    let settled = label(&shapes).expect("the card in place");
    assert!(rising > settled, "{rising} {settled}");

    // Clicking the card picks the window, which grows out of the card.
    let card = shapes
        .iter()
        .find_map(|s| match s {
            egui::Shape::Text(t) if t.galley.text() == "Untitled" => Some(t.pos),
            _ => None,
        })
        .unwrap();
    let click = |pressed| egui::Event::PointerButton {
        pos: card,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    timed(
        &ctx,
        &mut ui,
        &shell,
        vec![egui::Event::PointerMoved(card)],
        3.1,
    );
    timed(&ctx, &mut ui, &shell, vec![click(true)], 3.13);
    let (actions, _) = timed(&ctx, &mut ui, &shell, vec![click(false)], 3.16);
    assert!(actions.contains(&Action::Focus {
        window: editor.get()
    }));
    let (picked, from) = ui.take_picked().expect("a picked window");
    assert_eq!(picked, editor);
    assert!(from.contains(card));
}

#[test]
fn the_keyboard_rises_and_a_typed_key_glows() {
    let mut shell = Shell::new(rect(0, 0, 392, 872), true);
    shell.map_window("editor", "Untitled");
    shell
        .apply(Action::Keyboard {
            visible: Some(true),
        })
        .unwrap();
    let mut ui = ShellUi::new(&shell, false);
    let ctx = egui::Context::default();
    let key_q = |shapes: &[egui::Shape]| {
        shapes.iter().find_map(|s| match s {
            egui::Shape::Text(t) if t.galley.text().eq_ignore_ascii_case("q") => Some(t.pos),
            _ => None,
        })
    };
    timed(&ctx, &mut ui, &shell, vec![], 1.0);
    let (_, shapes) = timed(&ctx, &mut ui, &shell, vec![], 1.1);
    let rising = key_q(&shapes).expect("the q key");
    let (_, shapes) = timed(&ctx, &mut ui, &shell, vec![], 2.0);
    let risen = key_q(&shapes).expect("the q key");
    assert!(rising.y > risen.y, "{rising:?} {risen:?}");

    // Typed, its label turns to the background colour on the accent.
    let at = risen + egui::vec2(4.0, 8.0);
    let tap = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    let background = ui.theme.background;
    let label_color = |shapes: &[egui::Shape]| {
        shapes.iter().find_map(|s| match s {
            egui::Shape::Text(t) if t.galley.text().eq_ignore_ascii_case("q") => {
                Some(t.fallback_color)
            }
            _ => None,
        })
    };
    timed(
        &ctx,
        &mut ui,
        &shell,
        vec![egui::Event::PointerMoved(at)],
        2.1,
    );
    timed(&ctx, &mut ui, &shell, vec![tap(true)], 2.11);
    timed(&ctx, &mut ui, &shell, vec![tap(false)], 2.12);
    let (_, shapes) = timed(&ctx, &mut ui, &shell, vec![], 2.13);
    assert_eq!(label_color(&shapes), Some(background));
    let (_, shapes) = timed(&ctx, &mut ui, &shell, vec![], 2.5);
    assert_ne!(label_color(&shapes), Some(background));
}

#[test]
fn the_screen_shuts_into_a_line() {
    let shell = Shell::new(rect(0, 0, 1280, 800), false);
    let mut ui = ShellUi::new(&shell, false);
    let ctx = egui::Context::default();
    timed(&ctx, &mut ui, &shell, vec![], 1.0);
    ui.shut_down();
    timed(&ctx, &mut ui, &shell, vec![], 1.1);
    assert!(!ui.has_shut());
    // 10 frames' wait, then 16 to narrow.
    let (_, shapes) = timed(&ctx, &mut ui, &shell, vec![], 1.1 + 27.0 / 60.0);
    assert!(ui.has_shut());
    let accent = ui.theme.accent;
    let line = shapes
        .iter()
        .find_map(|s| match s {
            egui::Shape::Rect(r) if r.fill == accent => Some(r.rect),
            _ => None,
        })
        .expect("the line");
    assert_eq!(line.size(), egui::vec2(300.0, 4.0));
    assert_eq!(line.center(), egui::pos2(640.0, 400.0));

    ui.open_up();
    let (_, shapes) = timed(&ctx, &mut ui, &shell, vec![], 2.0);
    assert!(
        !shapes.iter().any(
            |s| matches!(s, egui::Shape::Rect(r) if r.fill == accent && r.rect.height() == 4.0)
        )
    );
}
