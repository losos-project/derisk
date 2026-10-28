use derisk_settings::stuck::{AFTER, working};
use mcsapi_ui::egui;

/// Draws `label`'s line once at `time` and says whether it was late.
fn frame(ctx: &egui::Context, time: f64, label: &str) -> bool {
    let mut late = false;
    let input = egui::RawInput {
        time: Some(time),
        ..Default::default()
    };
    let mut output = ctx.run_ui(input, |ui| late = working(ui, label));
    output.textures_delta.clear();
    late
}

#[test]
fn work_is_late_only_after_running_that_long_without_a_break() {
    let ctx = egui::Context::default();
    let after = AFTER.as_secs_f64();
    let mut time = 0.0;
    while time < after {
        assert!(!frame(&ctx, time, "Pairing…"), "late at {time} s");
        time += 0.5;
    }
    assert!(frame(&ctx, time, "Pairing…"));
    // Not drawn for a while: that work finished, and the next one under
    // the same label starts from nothing.
    assert!(!frame(&ctx, time + 10.0, "Pairing…"));
    // Other work is timed apart.
    assert!(!frame(&ctx, time + 10.5, "Connecting…"));
}
