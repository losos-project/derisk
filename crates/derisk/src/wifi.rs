//! The Network page the installer and the first-boot setup share: whether
//! the machine is online, what each wired port is doing, and the Wi-Fi
//! networks in range, with a passphrase field for the one picked.
//!
//! The page reads the latest [`Status`] that [`Wifi`]'s thread published
//! ([`derisk_install::wifi`]), so a slow or restarting daemon never holds
//! up a frame. The Settings app's Network page reads the same thread.

use egui::{RichText, Ui};
use mcsapi::toolkit::egui;
use mcsapi_components::{Button, ButtonVariant, Tokens};

pub use derisk_install::wifi::{Status, Wifi};

use crate::{
    network::WiredState,
    wizard::{self, Row},
};

/// The Network page's own state: the network picked and its passphrase.
#[derive(Debug, Default)]
pub struct NetworkPage {
    picked: Option<Vec<u8>>,
    passphrase: String,
}

impl NetworkPage {
    /// Draws the page body from `status`, sending scans and joins to `wifi`.
    pub fn show(&mut self, ui: &mut Ui, wifi: &Wifi, status: &Status) {
        let tokens = Tokens::current(ui.ctx());
        let headline = if status.online {
            match &status.connected {
                Some(name) => format!("Online, through {name}."),
                None => "Online.".to_owned(),
            }
        } else if status.joining.is_some() {
            format!("Joining {}…", status.joining.as_deref().unwrap_or_default())
        } else {
            "Not connected.".to_owned()
        };
        ui.label(RichText::new(headline).strong());
        for port in &status.wired {
            let line = format!("Cable ({}): {}", port.name, port.state.label());
            let color = if port.state == WiredState::Online {
                tokens.foreground
            } else {
                tokens.muted_foreground
            };
            ui.label(RichText::new(line).color(color).font(tokens.small_font()));
        }
        ui.add_space(8.0);
        if let Some(message) = &status.message {
            wizard::notice(ui, message, true);
            ui.add_space(8.0);
        }

        let Some(_) = &status.interface else {
            wizard::notice(
                ui,
                if status.online {
                    "No Wi-Fi adapter was found."
                } else if status.wired.is_empty() {
                    "This machine has no network adapter that works yet."
                } else {
                    "No Wi-Fi adapter was found. Plug in a network cable."
                },
                false,
            );
            return;
        };
        if !status.wifi_ready {
            wizard::busy(ui, "Starting Wi-Fi…");
            return;
        }

        // The passphrase for the network picked, above the list so the
        // on-screen keyboard does not cover it.
        let picked = self
            .picked
            .as_ref()
            .and_then(|ssid| status.networks.iter().find(|n| &n.ssid == ssid))
            .cloned();
        if let Some(network) = &picked {
            // A network joined before is joined with the passphrase it was
            // saved with.
            if network.security.needs_passphrase() && status.saved(&network.ssid).is_none() {
                let field = wizard::field(
                    ui,
                    &format!("Passphrase for {}", network.name()),
                    &mut self.passphrase,
                    wizard::Entry::Secret,
                    "Passphrase",
                );
                if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    wifi.join(network.clone(), std::mem::take(&mut self.passphrase));
                }
            }
            ui.horizontal(|ui| {
                let joining = status.joining.is_some();
                if ui.add(Button::new("Join").enabled(!joining)).clicked() {
                    wifi.join(network.clone(), std::mem::take(&mut self.passphrase));
                }
                if joining {
                    wizard::busy(ui, "Joining…");
                }
            });
            ui.add_space(8.0);
        }

        ui.horizontal(|ui| {
            ui.label(RichText::new("Wi-Fi networks").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if status.scanning {
                    ui.add(mcsapi_components::Spinner::new());
                } else if ui
                    .add(Button::new("Scan again").variant(ButtonVariant::Ghost))
                    .clicked()
                {
                    wifi.scan();
                }
            });
        });
        ui.add_space(4.0);
        let selected = picked
            .as_ref()
            .and_then(|p| status.networks.iter().position(|n| n.ssid == p.ssid));
        let networks = &status.networks;
        if let Some(i) = wizard::list(ui, "wifi", networks.len(), selected, None, |i| {
            let n = &networks[i];
            let mut detail = format!("{} · {}", bars(n.signal), n.security.label());
            if status.connected.as_deref() == Some(n.name().as_str()) {
                detail = format!("Connected · {detail}");
            }
            Row {
                title: n.name(),
                detail,
            }
        }) {
            let network = networks[i].clone();
            self.passphrase.clear();
            self.picked = Some(network.ssid.clone());
            // An open or saved network needs nothing typed: join it on the
            // tap.
            if !network.security.needs_passphrase() || status.saved(&network.ssid).is_some() {
                wifi.join(network, String::new());
            }
        }
    }
}

/// Signal strength as one to four bars.
fn bars(dbm: i32) -> &'static str {
    match dbm {
        d if d >= -55 => "▂▄▆█",
        d if d >= -67 => "▂▄▆",
        d if d >= -78 => "▂▄",
        _ => "▂",
    }
}
