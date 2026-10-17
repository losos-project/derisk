use derisk::{
    action::{Action, LayoutKind},
    keys::{Key, Mods, SuperTap, binding},
    snap::Direction,
};

const SUPER: Mods = Mods {
    logo: true,
    shift: false,
    ctrl: false,
    alt: false,
};

#[test]
fn super_chords_map_to_actions() {
    assert_eq!(
        binding(SUPER, Key::Arrow(Direction::Left)),
        Some(Action::Nudge {
            window: None,
            direction: Direction::Left
        })
    );
    assert_eq!(
        binding(SUPER, Key::Digit(3)),
        Some(Action::SwitchWorkspace { workspace: 3 })
    );
    let shifted = Mods {
        shift: true,
        ..SUPER
    };
    assert_eq!(
        binding(shifted, Key::Digit(2)),
        Some(Action::MoveToWorkspace {
            window: None,
            workspace: 2
        })
    );
    assert_eq!(
        binding(SUPER, Key::Letter('m')),
        Some(Action::SetLayout {
            layout: LayoutKind::Monocle
        })
    );
    assert_eq!(
        binding(shifted, Key::Letter('m')),
        Some(Action::SetLayout {
            layout: LayoutKind::Tall
        })
    );
    assert_eq!(binding(SUPER, Key::Digit(0)), None);
}

#[test]
fn plain_keys_reach_clients() {
    assert_eq!(binding(Mods::default(), Key::Letter('q')), None);
    assert_eq!(binding(Mods::default(), Key::Arrow(Direction::Up)), None);
    let ctrl_super = Mods {
        ctrl: true,
        ..SUPER
    };
    assert_eq!(binding(ctrl_super, Key::Letter('q')), None);
}

#[test]
fn alt_tab_cycles_focus() {
    let alt = Mods {
        alt: true,
        ..Mods::default()
    };
    assert_eq!(binding(alt, Key::Tab), Some(Action::FocusNext));
    let alt_shift = Mods { shift: true, ..alt };
    assert_eq!(binding(alt_shift, Key::Tab), Some(Action::FocusPrevious));
}

#[test]
fn super_tap_only_without_other_keys() {
    let mut tap = SuperTap::default();
    assert!(!tap.key(true, true));
    assert!(tap.key(true, false));

    assert!(!tap.key(true, true));
    assert!(!tap.key(false, true));
    assert!(!tap.key(true, false));

    assert!(!tap.key(true, true));
    tap.cancel();
    assert!(!tap.key(true, false));
}

#[test]
fn rebound_shortcuts_move_and_free_their_chord() {
    use derisk::keys::binding_with;
    use derisk_settings::{Keys, Shortcut, Shortcuts};
    let mut shortcuts = Shortcuts::default();
    shortcuts.set(Shortcut::Close, Keys::parse("Ctrl+Alt+W"));
    shortcuts.set(Shortcut::Float, None);
    let ctrl_alt = Mods {
        ctrl: true,
        alt: true,
        ..Mods::default()
    };
    assert_eq!(
        binding_with(&shortcuts, ctrl_alt, Key::Letter('w')),
        Some(Action::Close { window: None })
    );
    assert_eq!(binding_with(&shortcuts, SUPER, Key::Letter('q')), None);
    assert_eq!(binding_with(&shortcuts, SUPER, Key::Letter('f')), None);
    // The fixed families still work.
    assert_eq!(
        binding_with(&shortcuts, SUPER, Key::Digit(4)),
        Some(Action::SwitchWorkspace { workspace: 4 })
    );
}

#[test]
fn a_sequence_waits_for_its_second_key_and_holds_the_keyboard() {
    use derisk::keys::{Sequence, Step};
    use derisk::snap::SnapZone;
    use derisk_settings::Shortcuts;
    let shortcuts = Shortcuts::default();
    let mut seq = Sequence::default();
    assert_eq!(
        seq.press(&shortcuts, SUPER, Key::Letter('w')),
        Step::Pending
    );
    assert!(seq.is_pending());
    assert_eq!(
        seq.press(&shortcuts, Mods::default(), Key::Arrow(Direction::Left)),
        Step::Action(Action::Snap {
            window: None,
            zone: SnapZone::Left
        })
    );
    assert!(!seq.is_pending());
    // A key that continues nothing ends the sequence, and is not typed.
    seq.press(&shortcuts, SUPER, Key::Letter('w'));
    assert_eq!(
        seq.press(&shortcuts, Mods::default(), Key::Letter('y')),
        Step::Cancelled
    );
    // Escape takes a sequence back.
    seq.press(&shortcuts, SUPER, Key::Letter('x'));
    assert_eq!(
        seq.press(&shortcuts, Mods::default(), Key::Escape),
        Step::Cancelled
    );
    // Plain typing is the window's.
    assert_eq!(
        seq.press(&shortcuts, Mods::default(), Key::Letter('w')),
        Step::Unbound
    );
}

#[test]
fn session_keys_ask_before_ending_the_session() {
    use derisk::keys::{Sequence, Step};
    use derisk::systemd::SessionOp;
    use derisk_settings::Shortcuts;
    let shortcuts = Shortcuts::default();
    let mut seq = Sequence::default();
    seq.press(&shortcuts, SUPER, Key::Letter('x'));
    assert_eq!(
        seq.press(&shortcuts, Mods::default(), Key::Letter('p')),
        Step::Action(Action::ConfirmSession {
            op: SessionOp::PowerOff
        })
    );
    assert_eq!(
        binding(SUPER, Key::Letter('l')),
        Some(Action::Session {
            op: SessionOp::Lock,
            confirmed: false
        })
    );
}

#[test]
fn new_single_chords_and_families() {
    use derisk_settings::KeyName;
    let alt = Mods {
        alt: true,
        ..Mods::default()
    };
    assert_eq!(
        binding(alt, Key::Other(KeyName::Function(4))),
        Some(Action::Close { window: None })
    );
    assert_eq!(
        binding(Mods::default(), Key::Other(KeyName::Print)),
        Some(Action::Screenshot)
    );
    assert_eq!(binding(SUPER, Key::Letter('d')), Some(Action::ShowDesktop));
    assert_eq!(
        binding(SUPER, Key::Other(KeyName::Slash)),
        Some(Action::ShortcutHelp { visible: None })
    );
    let super_ctrl = Mods {
        ctrl: true,
        ..SUPER
    };
    assert_eq!(
        binding(super_ctrl, Key::Arrow(Direction::Right)),
        Some(Action::StepWorkspace {
            by: 1,
            carry_window: false
        })
    );
    let carry = Mods {
        shift: true,
        ..super_ctrl
    };
    assert_eq!(
        binding(carry, Key::Arrow(Direction::Left)),
        Some(Action::StepWorkspace {
            by: -1,
            carry_window: true
        })
    );
}
