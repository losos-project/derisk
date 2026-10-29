//! The Network page: whether the machine is online, each wired port, the
//! Wi-Fi networks in range and the ones joined before.
//!
//! First-boot setup joins one network through NetworkManager and saves it;
//! this page is where every one after it is joined, and where a saved one
//! is forgotten. It draws from the same thread the setup's Network page does
//! ([`derisk_install::wifi`]), started when the page is first shown, and
//! applies at once, like the Sign-in page, since none of it is derisk's own
//! setting: NetworkManager keeps the connections itself. So is encrypted
//! DNS, LosOS's ad blocker's setting ([`crate::encrypted_dns`]).

use derisk_install::{
    network::{Network, WiredState},
    wifi::{Status, Wifi},
};
use mcsapi_ui::{Theme, egui};

/// The page's state: the network thread, the network picked, and its
/// passphrase.
#[derive(Default)]
pub(crate) struct NetworkUi {
    schizo: crate::schizo::SchizoUi,
    wifi: Option<Wifi>,
    picked: Option<Vec<u8>>,
    passphrase: String,
    encrypted_dns: crate::encrypted_dns::EncryptedDnsUi,
}

impl std::fmt::Debug for NetworkUi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // No passphrase in debug output.
        f.debug_struct("NetworkUi")
            .field("started", &self.wifi.is_some())
            .finish_non_exhaustive()
    }
}

/// The line at the top of the page.
pub fn headline(status: &Status) -> String {
    if let Some(name) = &status.joining {
        format!("Joining {name}…")
    } else if status.online {
        match &status.connected {
            Some(name) => format!("Online, through {name}"),
            None => "Online".to_owned(),
        }
    } else {
        "Not connected".to_owned()
    }
}

/// What a network's row says under its name.
pub fn detail(network: &Network, status: &Status) -> String {
    let mut parts = vec![signal(network.signal), network.security.label()];
    if status.connected.as_deref() == Some(network.name().as_str()) {
        parts.insert(0, "Connected");
    } else if status.saved(&network.ssid).is_some() {
        parts.insert(0, "Saved");
    }
    parts.join(" · ")
}

/// Signal strength, 0 to 100, in words, at the setup pages' bars. Those
/// are block characters, which the Settings app's font does not have.
fn signal(signal: u8) -> &'static str {
    match signal {
        81.. => "Strong signal",
        56.. => "Good signal",
        31.. => "Fair signal",
        _ => "Weak signal",
    }
}

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(crate::layout::note(
        &mcsapi_components::Tokens::current(ui.ctx()),
        text,
    ));
}

pub(crate) fn page(ui: &mut egui::Ui, state: &mut NetworkUi, theme: &Theme) {
    let wifi = state.wifi.get_or_insert_with(|| Wifi::start(true));
    let status = wifi.status();
    // The thread publishes twice a second; the page follows it while shown.
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(500));

    crate::schizo::section(ui, &mut state.schizo, theme);
    ui.label(
        egui::RichText::new(headline(&status))
            .strong()
            .color(theme.foreground),
    );
    for port in &status.wired {
        let line = format!("Cable ({}): {}", port.name, port.state.label());
        if port.state == WiredState::Online {
            ui.label(line);
        } else {
            hint(ui, &line);
        }
    }
    ui.add_space(8.0);
    if let Some(message) = &status.message {
        ui.label(egui::RichText::new(message).color(egui::Color32::from_rgb(248, 113, 113)));
        ui.add_space(8.0);
    }

    // Above Wi-Fi, which returns early on a computer without it.
    crate::encrypted_dns::section(ui, &mut state.encrypted_dns, theme);

    if status.interface.is_none() {
        hint(ui, "No Wi-Fi adapter was found.");
        return;
    }
    if !status.wifi_ready {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Starting Wi-Fi…");
        });
        return;
    }

    let picked = state
        .picked
        .as_ref()
        .and_then(|ssid| status.networks.iter().find(|n| &n.ssid == ssid))
        .cloned();
    if let Some(network) = &picked {
        picked_ui(ui, state, &status, network);
        ui.add_space(12.0);
    }

    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Wi-Fi networks")
                .strong()
                .color(theme.foreground),
        );
        if status.scanning {
            ui.spinner();
        } else if ui.button("Scan again").clicked()
            && let Some(wifi) = &state.wifi
        {
            wifi.scan();
        }
    });
    if status.networks.is_empty() && !status.scanning {
        hint(ui, "No networks in range.");
    }
    for network in &status.networks {
        let selected = picked.as_ref().is_some_and(|p| p.ssid == network.ssid);
        let text = format!("{}\n{}", network.name(), detail(network, &status));
        if ui.selectable_label(selected, text).clicked() {
            state.passphrase.clear();
            state.picked = Some(network.ssid.clone());
        }
    }

    // Saved networks out of range can still be forgotten here; the ones in
    // range are forgotten from their row above.
    let away: Vec<_> = status
        .saved
        .iter()
        .filter(|s| !status.networks.iter().any(|n| n.ssid == s.ssid))
        .collect();
    if !away.is_empty() {
        ui.add_space(12.0);
        ui.label(
            egui::RichText::new("Saved, not in range")
                .strong()
                .color(theme.foreground),
        );
        for saved in away {
            ui.horizontal(|ui| {
                ui.label(saved.name());
                if ui.button("Forget").clicked()
                    && let Some(wifi) = &state.wifi
                {
                    wifi.forget(saved.id.clone());
                }
            });
        }
    }
}

/// The passphrase and buttons for the network picked, above the list so an
/// on-screen keyboard does not cover them.
fn picked_ui(ui: &mut egui::Ui, state: &mut NetworkUi, status: &Status, network: &Network) {
    let Some(wifi) = &state.wifi else {
        return;
    };
    let saved = status.saved(&network.ssid);
    let connected = status.connected.as_deref() == Some(network.name().as_str());
    let joining = status.joining.is_some();
    // A saved network is joined with the passphrase it was saved with.
    let ask = network.security.needs_passphrase() && saved.is_none();
    let mut join = false;
    ui.label(egui::RichText::new(network.name()).strong());
    if ask {
        let field = ui.add(
            egui::TextEdit::singleline(&mut state.passphrase)
                .password(true)
                .hint_text("Passphrase"),
        );
        join = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    }
    ui.horizontal(|ui| {
        if !connected {
            join |= ui
                .add_enabled(!joining, egui::Button::new("Join"))
                .clicked();
        }
        if let Some(saved) = saved
            && ui.button("Forget").clicked()
        {
            wifi.forget(saved.id.clone());
        }
        if joining {
            ui.spinner();
        }
    });
    if join {
        wifi.join(network.clone(), std::mem::take(&mut state.passphrase));
    }
}
