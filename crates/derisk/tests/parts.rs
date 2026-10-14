use derisk::{
    adaptive::{FormFactor, Habits, Profile},
    animation::StartupAnimation,
    geom::rect,
    overview::{Battery, grid, miniature},
    snap::{Direction, Nudge, SnapConfig, SnapZone, zone_at},
    time::{Clock, utc_offset},
    tray::{Pixmap, monochrome},
};

#[test]
fn snap_zones_follow_windows_conventions() {
    let area = rect(0, 28, 1920, 1052);
    let cfg = SnapConfig::default();
    assert_eq!(zone_at((0, 500), area, cfg), Some(SnapZone::Left));
    assert_eq!(zone_at((1919, 500), area, cfg), Some(SnapZone::Right));
    assert_eq!(zone_at((900, 28), area, cfg), Some(SnapZone::Maximize));
    assert_eq!(zone_at((900, 5), area, cfg), Some(SnapZone::Maximize));
    assert_eq!(zone_at((2, 40), area, cfg), Some(SnapZone::TopLeft));
    assert_eq!(
        zone_at((1918, 1078), area, cfg),
        Some(SnapZone::BottomRight)
    );
    assert_eq!(zone_at((900, 1079), area, cfg), None);
    assert_eq!(zone_at((900, 500), area, cfg), None);

    let left = SnapZone::Left.geometry(area, 0);
    assert_eq!((left.loc.x, left.size.w, left.size.h), (0, 960, 1052));
    assert_eq!(SnapZone::Left.complement(), Some(SnapZone::Right));
}

#[test]
fn super_arrow_nudges_like_windows() {
    assert_eq!(
        SnapZone::nudge(None, Direction::Left),
        Nudge::Snap(SnapZone::Left)
    );
    assert_eq!(
        SnapZone::nudge(Some(SnapZone::Left), Direction::Up),
        Nudge::Snap(SnapZone::TopLeft)
    );
    assert_eq!(
        SnapZone::nudge(Some(SnapZone::Left), Direction::Right),
        Nudge::Restore
    );
    assert_eq!(
        SnapZone::nudge(None, Direction::Up),
        Nudge::Snap(SnapZone::Maximize)
    );
    assert_eq!(SnapZone::nudge(None, Direction::Down), Nudge::Minimize);
}

#[test]
fn profiles_adapt_to_the_output() {
    assert_eq!(
        Profile::detect(400, 800, true).form_factor,
        FormFactor::Phone
    );
    assert_eq!(
        Profile::detect(1280, 800, true).form_factor,
        FormFactor::Tablet
    );
    assert_eq!(
        Profile::detect(2560, 1440, false).form_factor,
        FormFactor::Desktop
    );
    // A small laptop screen without touch gets desktop chrome.
    assert_eq!(
        Profile::detect(1280, 800, false).form_factor,
        FormFactor::Desktop
    );
}

#[test]
fn habits_suggest_apps_used_at_this_time() {
    let mut habits = Habits::default();
    for _ in 0..3 {
        habits.record("mail", 9);
    }
    habits.record("Music", 21);
    habits.record("music", 21);
    assert_eq!(habits.suggestions(9, 1), ["mail"]);
    assert_eq!(habits.suggestions(21, 2), ["music", "mail"]);
}

#[test]
fn startup_animation_runs_to_completion() {
    let anim = StartupAnimation {
        reduced_motion: false,
        bar_height: 28.0,
    };
    let start = anim.frame(0);
    assert_eq!(start.cover, 1.0);
    assert!(!start.done);
    let end = anim.frame(anim.duration_ms());
    assert!(end.done);
    assert_eq!(end.cover, 0.0);
    assert_eq!(end.shell_opacity, 1.0);
    let reduced = StartupAnimation {
        reduced_motion: true,
        bar_height: 28.0,
    };
    assert!(reduced.duration_ms() < anim.duration_ms());
    assert_eq!(reduced.frame(150).logo_scale, 1.0);
}

#[test]
fn tray_icons_become_monochrome() {
    let icon = Pixmap::from_rgba(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 0]).unwrap();
    let mono = monochrome(&icon, [10, 20, 30]);
    assert_eq!(mono.rgba, vec![10, 20, 30, 255, 10, 20, 30, 0]);
    assert!(Pixmap::from_rgba(2, 2, vec![0; 3]).is_none());

    // Fully opaque icons are masked by luminance instead of alpha.
    let opaque = Pixmap::from_rgba(2, 1, vec![255, 255, 255, 255, 0, 0, 0, 255]).unwrap();
    let mono = monochrome(&opaque, [1, 2, 3]);
    assert_ne!(mono.rgba[3], mono.rgba[7]);
    assert!(mono.rgba.chunks(4).all(|p| p[..3] == [1, 2, 3]));
}

#[test]
fn clock_and_calendar() {
    let c = Clock::from_unix(1_791_018_000, 0); // 2026-10-03 09:00 UTC
    assert_eq!((c.year, c.month, c.day, c.hour), (2026, 10, 3, 9));
    assert_eq!(c.date_label(), "Sat 3 Oct");
    assert_eq!(c.time_label(), "09:00");
    assert_eq!(c.days_in_month(), 31);
    assert_eq!(c.first_weekday(), 3);
}

/// A version 2 TZif file with the given 64-bit transitions, each switching
/// to the type at the same index of `offsets`, and the POSIX rule `footer`.
fn tzif(transitions: &[(i64, u8)], offsets: &[i32], footer: &str) -> Vec<u8> {
    fn header(timecnt: usize, typecnt: usize) -> Vec<u8> {
        let mut h = b"TZif2".to_vec();
        h.resize(20, 0);
        // isutcnt, isstdcnt, leapcnt, timecnt, typecnt, charcnt (one NUL).
        for n in [0, 0, 0, timecnt, typecnt, 1] {
            h.extend((n as u32).to_be_bytes());
        }
        h
    }
    let types: Vec<u8> = offsets
        .iter()
        .flat_map(|o| o.to_be_bytes().into_iter().chain([0, 0]))
        .collect();
    // The version 1 block carries no transitions, as zic -b slim writes it.
    let mut file = header(0, offsets.len());
    file.extend(&types);
    file.push(0);
    file.extend(header(transitions.len(), offsets.len()));
    file.extend(transitions.iter().flat_map(|(t, _)| t.to_be_bytes()));
    file.extend(transitions.iter().map(|(_, i)| *i));
    file.extend(&types);
    file.push(0);
    file.extend(format!("\n{footer}\n").bytes());
    file
}

#[test]
fn clock_follows_the_time_zone_file() {
    // 2026-10-05 19:50 UTC, when the demo's top bar showed 19:56 in
    // Bratislava instead of the local 21:5x.
    let october = 1_791_229_800;
    let january = 1_799_000_000; // 2027-01-03
    let cet = tzif(&[], &[3600], "CET-1CEST,M3.5.0,M10.5.0/3");
    assert_eq!(utc_offset(&cet, october), Some(7200));
    assert_eq!(utc_offset(&cet, january), Some(3600));
    assert_eq!(Clock::from_unix(october, 7200).time_label(), "21:50");
    // The 2026 changes: 29 March 01:00 UTC and 25 October 01:00 UTC.
    assert_eq!(utc_offset(&cet, 1_774_745_999), Some(3600));
    assert_eq!(utc_offset(&cet, 1_774_746_000), Some(7200));
    assert_eq!(utc_offset(&cet, 1_792_890_000 - 1), Some(7200));
    assert_eq!(utc_offset(&cet, 1_792_890_000), Some(3600));
    // Southern hemisphere: Sydney's daylight time spans the new year.
    let sydney = tzif(&[], &[36000], "AEST-10AEDT,M10.1.0,M4.1.0/3");
    assert_eq!(utc_offset(&sydney, january), Some(39600));
    assert_eq!(utc_offset(&sydney, 1_784_000_000), Some(36000)); // July 2026
    // A fixed zone, a quoted name, and UTC itself.
    assert_eq!(
        utc_offset(&tzif(&[], &[19800], "IST-5:30"), october),
        Some(19800)
    );
    assert_eq!(
        utc_offset(&tzif(&[], &[-10800], "<-03>3"), october),
        Some(-10800)
    );
    assert_eq!(utc_offset(&tzif(&[], &[0], "UTC0"), october), Some(0));
    // Inside the table, transitions win over the footer.
    let table = tzif(&[(0, 0), (2_000_000_000, 1)], &[3600, 7200], "XYZ-3");
    assert_eq!(utc_offset(&table, october), Some(3600));
    assert_eq!(utc_offset(&table, 2_000_000_000), Some(10800));
    assert_eq!(utc_offset(b"not a zone file", october), None);
    // The real file, where this machine has one.
    if let Ok(real) = std::fs::read("/usr/share/zoneinfo/Europe/Bratislava") {
        assert_eq!(utc_offset(&real, october), Some(7200));
        assert_eq!(utc_offset(&real, january), Some(3600));
    }
}

#[test]
fn overview_grid_and_battery() {
    let cells: Vec<_> = grid(5, rect(0, 0, 1000, 600), 10).collect();
    assert_eq!(cells.len(), 5);
    for pair in cells.windows(2) {
        assert!(pair[0] != pair[1]);
    }
    let dir = std::env::temp_dir().join(format!("derisk-psu-{}", std::process::id()));
    let bat = dir.join("BAT0");
    std::fs::create_dir_all(&bat).unwrap();
    std::fs::write(bat.join("type"), "Battery\n").unwrap();
    std::fs::write(bat.join("capacity"), "77\n").unwrap();
    std::fs::write(bat.join("status"), "Charging\n").unwrap();
    assert_eq!(
        Battery::read(&dir),
        Some(Battery {
            percent: 77,
            charging: true,
            discharging: false,
        })
    );
    std::fs::write(bat.join("status"), "Not charging\n").unwrap();
    let held = Battery::read(&dir).unwrap();
    assert!(!held.charging && !held.discharging);
    std::fs::write(bat.join("status"), "Discharging\n").unwrap();
    assert!(Battery::read(&dir).unwrap().discharging);
    let ac = dir.join("AC");
    std::fs::create_dir_all(&ac).unwrap();
    std::fs::write(ac.join("type"), "Mains\n").unwrap();
    std::fs::write(ac.join("online"), "1\n").unwrap();
    assert!(!Battery::read(&dir).unwrap().discharging);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn workspace_miniatures_keep_windows_where_they_are() {
    // A 1920x1000 work area under a 80 px bar, drawn at a fifth of its size.
    let area = rect(0, 80, 1920, 1000);
    let screen = rect(100, 10, 384, 200);
    // The left half lands on the miniature's left half...
    assert_eq!(
        miniature(rect(0, 80, 960, 1000), area, screen),
        rect(100, 10, 192, 200)
    );
    // ...and a window at the bottom right, at its bottom right.
    assert_eq!(
        miniature(rect(1440, 580, 480, 500), area, screen),
        rect(388, 110, 96, 100)
    );
}
