//! The smallest preview guest: a counter. Builds natively and for
//! `wasm32-wasip2`:
//!
//! ```sh
//! cargo build -p derisk_agent_preview --example preview_counter
//! cargo build -p derisk_agent_preview --example preview_counter --target wasm32-wasip2
//! ```

fn main() -> std::io::Result<()> {
    let mut count = 0u32;
    let mut name = String::new();
    derisk_agent_preview::serve("Counter", move |ui, theme| {
        ui.heading(egui::RichText::new("Counter").color(theme.foreground));
        ui.horizontal(|ui| {
            if ui.button("-").clicked() {
                count = count.saturating_sub(1);
            }
            ui.label(
                egui::RichText::new(count.to_string())
                    .size(28.0)
                    .color(theme.accent),
            );
            if ui.button("+").clicked() {
                count += 1;
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Name");
            ui.text_edit_singleline(&mut name);
        });
        if !name.is_empty() {
            ui.label(format!("Hello, {name}!"));
        }
    })
}
