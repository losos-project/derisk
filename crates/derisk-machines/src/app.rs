//! The Machines app: the machines on this computer, with their labels and
//! states, and the buttons to make, start, stop and remove them.

use derisk_sys::time::Instant;
use mcsapi_ui::{App, Theme, egui};

use crate::{
    Label, Machine,
    setup::{self, Plugin, Template},
};

#[cfg(not(target_arch = "wasm32"))]
use crate::worker::{Create, Event, Order, Worker};

/// The app's ID.
pub const APP_ID: &str = "org.derisk.machines";

/// How many of pm's lines the app keeps to show while it works.
#[cfg(not(target_arch = "wasm32"))]
const LOG_LINES: usize = 200;

/// The Machines app.
pub struct MachinesApp {
    #[cfg(not(target_arch = "wasm32"))]
    worker: Worker,
    machines: Vec<Machine>,
    /// Why libvirt cannot be reached, while it cannot.
    unavailable: Option<String>,
    selected: Option<String>,
    /// What the worker is doing for the person, while it is.
    busy: Option<String>,
    log: Vec<String>,
    status: Option<Result<String, String>>,
    confirm: Option<Confirm>,
    form: Option<NewMachine>,
    templates: Vec<Template>,
    plugin: Plugin,
    looked: Option<Instant>,
    /// The machine apps open in by default, as the settings file says;
    /// empty for this computer.
    open_apps_in: String,
    /// Each running machine's apps, as it listed them, and when it was
    /// asked; forgotten when the machine stops.
    #[cfg(not(target_arch = "wasm32"))]
    apps: std::collections::HashMap<String, (Instant, Option<AppList>)>,
}

/// A machine's answer to which apps it has.
#[cfg(not(target_arch = "wasm32"))]
type AppList = Result<Vec<crate::GuestApp>, String>;

/// An action that loses something, waiting for the person to say yes.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Confirm {
    ForceOff(String),
    Remove(String),
}

/// The new-machine form.
#[derive(Clone, Debug)]
struct NewMachine {
    name: String,
    template: usize,
    label: Label,
    memory_mib: u32,
    cpus: u32,
    disk_gib: u32,
}

impl std::fmt::Debug for MachinesApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MachinesApp")
            .field("machines", &self.machines)
            .field("busy", &self.busy)
            .finish_non_exhaustive()
    }
}

impl Default for MachinesApp {
    fn default() -> Self {
        Self::new()
    }
}

impl MachinesApp {
    /// Starts watching libvirt's machines.
    pub fn new() -> Self {
        let mut app = Self {
            #[cfg(not(target_arch = "wasm32"))]
            worker: Worker::start(),
            machines: Vec::new(),
            #[cfg(not(target_arch = "wasm32"))]
            unavailable: None,
            #[cfg(target_arch = "wasm32")]
            unavailable: Some(
                "Machines run under libvirt on the computer itself, which this demo in a \
                 browser does not have."
                    .into(),
            ),
            selected: None,
            busy: None,
            log: Vec::new(),
            status: None,
            confirm: None,
            form: None,
            templates: Vec::new(),
            plugin: Plugin::NotShipped,
            looked: None,
            open_apps_in: String::new(),
            #[cfg(not(target_arch = "wasm32"))]
            apps: std::collections::HashMap::new(),
        };
        app.look();
        app
    }

    /// The machines as last read.
    pub fn machines(&self) -> &[Machine] {
        &self.machines
    }

    /// Reads the templates and the plugin's state again: both change only
    /// when the OS updates or the person sets pm up, so not every frame.
    fn look(&mut self) {
        let dirs = setup::data_dirs();
        self.templates = setup::templates(&dirs);
        self.plugin =
            setup::plugin_dir().map_or(Plugin::NotShipped, |mine| Plugin::find(&dirs, &mine));
        self.open_apps_in = derisk_settings::default_path()
            .and_then(|path| derisk_settings::Settings::load(&path).ok())
            .map(|(settings, _)| settings.machines.open_apps_in)
            .unwrap_or_default();
        self.looked = Some(Instant::now());
    }

    /// For the selected machine, whether apps opened from the desktop open
    /// in it: derisk's `machines.open_apps_in` setting, which the session
    /// reads when it launches an app. Only a machine pm made has the guest
    /// side that opens apps.
    fn default_machine(&mut self, ui: &mut egui::Ui) {
        let Some(name) = self
            .selected
            .as_ref()
            .and_then(|name| self.machines.iter().find(|m| &m.name == name))
            .filter(|m| m.from_pm())
            .map(|m| m.name.clone())
        else {
            return;
        };
        let mut here = self.open_apps_in == name;
        let changed = ui
            .checkbox(&mut here, format!("Open apps in {name}"))
            .on_hover_text(
                "Apps you open from the desktop open in this machine instead of on this \
                 computer, starting it if it is off. derisk's own apps stay here.",
            )
            .changed();
        if changed {
            let value = if here { name } else { String::new() };
            match save_open_apps_in(&value) {
                Ok(()) => self.open_apps_in = value,
                Err(error) => self.status = Some(Err(error)),
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn receive(&mut self) {
        let events: Vec<Event> = self.worker.events().collect();
        for event in events {
            match event {
                Event::Machines(machines) => {
                    self.unavailable = None;
                    if let Some(name) = &self.selected
                        && machines.iter().all(|m| &m.name != name)
                    {
                        self.selected = None;
                    }
                    self.machines = machines;
                }
                Event::Unavailable(why) => self.unavailable = Some(why),
                Event::Line(line) => {
                    if self.log.len() == LOG_LINES {
                        self.log.remove(0);
                    }
                    self.log.push(line);
                }
                Event::Done(done) => {
                    self.busy = None;
                    self.status = Some(done);
                    self.look();
                }
                Event::Apps { machine, apps } => {
                    if let Some(entry) = self.apps.get_mut(&machine) {
                        entry.1 = Some(apps);
                    }
                }
            }
        }
        // A stopped machine's list may be different when it starts again
        // (the person installed something in it).
        let machines = &self.machines;
        self.apps.retain(|name, _| {
            machines
                .iter()
                .any(|m| &m.name == name && m.state.is_active())
        });
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn order(&mut self, doing: String, order: Order) {
        if self.worker.send(order) {
            self.busy = Some(doing);
            self.status = None;
            self.log.clear();
        } else {
            self.status = Some(Err("The machines thread is not answering".into()));
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn receive(&mut self) {}

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let idle = self.busy.is_none() && self.unavailable.is_none();
        let can_create = idle
            && !self.templates.is_empty()
            && matches!(self.plugin, Plugin::Ready | Plugin::NotShipped);
        let selected = self
            .selected
            .as_ref()
            .and_then(|name| self.machines.iter().find(|m| &m.name == name))
            .cloned();
        // Wraps on a phone rather than pushing buttons off the window.
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    can_create && self.form.is_none(),
                    egui::Button::new("New machine"),
                )
                .on_disabled_hover_text(if self.templates.is_empty() {
                    "This system has no templates to make a machine from"
                } else {
                    "Set pm up for machines first"
                })
                .clicked()
            {
                self.form = Some(self.new_form());
            }
            ui.separator();
            let active = selected.as_ref().is_some_and(|m| m.state.is_active());
            let chosen = selected.is_some();
            if ui
                .add_enabled(idle && chosen && !active, egui::Button::new("Start"))
                .clicked()
                && let Some(machine) = &selected
            {
                self.act(format!("Starting {}", machine.name), Act::Start, machine);
            }
            if ui
                .add_enabled(idle && active, egui::Button::new("Shut down"))
                .clicked()
                && let Some(machine) = &selected
            {
                self.act(
                    format!("Shutting {} down", machine.name),
                    Act::ShutDown,
                    machine,
                );
            }
            if ui
                .add_enabled(idle && active, egui::Button::new("Force off"))
                .clicked()
                && let Some(machine) = &selected
            {
                self.confirm = Some(Confirm::ForceOff(machine.name.clone()));
            }
            // pm removes a machine with its disk, so only a machine pm made,
            // whose disk is pm's to remove, and only while it is off.
            let removable = selected
                .as_ref()
                .is_some_and(|m| m.from_pm() && !m.state.is_active());
            if ui
                .add_enabled(idle && removable, egui::Button::new("Remove"))
                .on_disabled_hover_text("Only a machine pm made, while it is off")
                .clicked()
                && let Some(machine) = &selected
            {
                self.confirm = Some(Confirm::Remove(machine.name.clone()));
            }
        });
    }

    /// The selected machine's apps, as buttons that open them on the
    /// desktop. A machine is asked for its list once it runs; one that
    /// does not answer (it is still booting) is asked again a little later.
    #[cfg(not(target_arch = "wasm32"))]
    fn open_in(&mut self, ui: &mut egui::Ui) {
        let Some(machine) = self
            .selected
            .as_ref()
            .and_then(|name| self.machines.iter().find(|m| &m.name == name))
            .filter(|m| m.state == crate::State::Running && m.from_pm())
            .cloned()
        else {
            return;
        };
        let stale = |asked: &Instant, answer: &Option<Result<_, _>>| match answer {
            None => false,
            Some(Ok(_)) => false,
            Some(Err(_)) => asked.elapsed() >= std::time::Duration::from_secs(5),
        };
        let ask = self
            .apps
            .get(&machine.name)
            .is_none_or(|(asked, answer)| stale(asked, answer));
        if ask && self.worker.send(Order::Apps(machine.name.clone())) {
            self.apps
                .insert(machine.name.clone(), (Instant::now(), None));
        }
        let idle = self.busy.is_none();
        let mut open = None;
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("Open in {}:", machine.name));
            match self.apps.get(&machine.name).and_then(|(_, a)| a.as_ref()) {
                None => {
                    ui.spinner();
                }
                Some(Err(_)) => {
                    ui.weak("waiting for the machine to answer");
                    ui.spinner();
                }
                Some(Ok(apps)) if apps.is_empty() => {
                    ui.weak("it has no apps to show here");
                }
                Some(Ok(apps)) => {
                    for app in apps {
                        if ui.add_enabled(idle, egui::Button::new(&app.name)).clicked() {
                            open = Some(app.clone());
                        }
                    }
                }
            }
        });
        if let Some(app) = open {
            self.order(
                format!("Opening {} in {}", app.name, machine.name),
                Order::Open {
                    machine: machine.name,
                    app,
                },
            );
        }
    }

    fn new_form(&self) -> NewMachine {
        // The first label nobody uses yet, so two machines made one after
        // the other do not look alike.
        let label = Label::ALL
            .into_iter()
            .find(|label| self.machines.iter().all(|m| m.label != *label))
            .unwrap_or_default();
        let name = (1..)
            .map(|n| {
                if n == 1 {
                    "work".to_owned()
                } else {
                    format!("work-{n}")
                }
            })
            .find(|name| self.machines.iter().all(|m| &m.name != name))
            .unwrap_or_default();
        NewMachine {
            name,
            template: 0,
            label,
            // pm's own defaults.
            memory_mib: 2048,
            cpus: 2,
            disk_gib: 32,
        }
    }

    fn form(&mut self, ui: &mut egui::Ui) {
        let Some(form) = &mut self.form else {
            return;
        };
        let mut close = false;
        let mut create = None;
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.strong("New machine");
            egui::Grid::new("new-machine")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Name");
                    ui.add(egui::TextEdit::singleline(&mut form.name).desired_width(200.0));
                    ui.end_row();

                    ui.label("Template");
                    let current = self
                        .templates
                        .get(form.template)
                        .map_or("", |t| t.name.as_str());
                    egui::ComboBox::from_id_salt("template")
                        .selected_text(current)
                        .show_ui(ui, |ui| {
                            for (i, template) in self.templates.iter().enumerate() {
                                ui.selectable_value(&mut form.template, i, &template.name);
                            }
                        });
                    ui.end_row();

                    ui.label("Label");
                    ui.horizontal_wrapped(|ui| {
                        for label in Label::ALL {
                            let picked = form.label == label;
                            if swatch_button(ui, label, picked).clicked() {
                                form.label = label;
                            }
                        }
                    });
                    ui.end_row();

                    ui.label("Memory");
                    ui.add(
                        egui::DragValue::new(&mut form.memory_mib)
                            .range(256..=262_144)
                            .speed(64)
                            .suffix(" MiB"),
                    );
                    ui.end_row();

                    ui.label("Processors");
                    ui.add(egui::DragValue::new(&mut form.cpus).range(1..=256));
                    ui.end_row();

                    ui.label("Disk");
                    ui.add(
                        egui::DragValue::new(&mut form.disk_gib)
                            .range(1..=65_536)
                            .suffix(" GiB"),
                    );
                    ui.end_row();
                });
            let taken = self.machines.iter().any(|m| m.name == form.name);
            let problem = if !setup::valid_name(&form.name) {
                Some(
                    "A name is a lowercase letter, then up to 31 lowercase letters, digits \
                     and hyphens",
                )
            } else if taken {
                Some("There is a machine with that name already")
            } else {
                None
            };
            if let Some(problem) = problem {
                ui.colored_label(ui.visuals().error_fg_color, problem);
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(problem.is_none(), egui::Button::new("Create"))
                    .clicked()
                {
                    create = self
                        .templates
                        .get(form.template)
                        .map(|t| (t.path.clone(), form.clone()));
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        if close {
            self.form = None;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some((image, form)) = create {
            self.form = None;
            self.selected = Some(form.name.clone());
            self.order(
                format!("Making {}", form.name),
                Order::Create(Create {
                    name: form.name,
                    image,
                    label: form.label,
                    memory_mib: form.memory_mib,
                    cpus: form.cpus,
                    disk_gib: form.disk_gib,
                }),
            );
        }
        #[cfg(target_arch = "wasm32")]
        drop(create);
    }

    fn act(&mut self, doing: String, act: Act, machine: &Machine) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let name = machine.name.clone();
            let order = match act {
                Act::Start => Order::Start(name),
                Act::ShutDown => Order::ShutDown(name),
                Act::ForceOff => Order::ForceOff(name),
                Act::Remove => Order::Remove(name),
            };
            self.order(doing, order);
        }
        #[cfg(target_arch = "wasm32")]
        drop((doing, act, machine));
    }

    fn confirmation(&mut self, ui: &mut egui::Ui) {
        let Some(confirm) = self.confirm.clone() else {
            return;
        };
        let (question, button, act, name) = match &confirm {
            Confirm::ForceOff(name) => (
                format!("Force {name} off? Whatever it has not saved is lost."),
                "Force off",
                Act::ForceOff,
                name,
            ),
            Confirm::Remove(name) => (
                format!("Remove {name} and its disk? Its files cannot be brought back."),
                "Remove",
                Act::Remove,
                name,
            ),
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(question);
            if ui.button(button).clicked() {
                self.confirm = None;
                if let Some(machine) = self.machines.iter().find(|m| &m.name == name).cloned() {
                    let doing = match act {
                        Act::Remove => format!("Removing {name}"),
                        _ => format!("Forcing {name} off"),
                    };
                    self.act(doing, act, &machine);
                }
            }
            if ui.button("Cancel").clicked() {
                self.confirm = None;
            }
        });
    }

    fn plugin_banner(&mut self, ui: &mut egui::Ui) {
        let Some((shipped, installed)) = self.plugin.paths() else {
            return;
        };
        let (shipped, installed) = (shipped.to_owned(), installed.to_owned());
        let text = match self.plugin {
            Plugin::Outdated { .. } => {
                "An update brought a new version of pm's libvirt plugin. Set it up again to \
                 sign the new one with your key."
            }
            _ => {
                "pm makes and removes machines through its libvirt plugin, which it runs only \
                 once you have signed it with your own key. Setting it up copies the plugin \
                 into your pm plugins and signs it, making you a key if you have none."
            }
        };
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.label(text);
            let idle = self.busy.is_none();
            if ui
                .add_enabled(idle, egui::Button::new("Set up pm for machines"))
                .clicked()
            {
                #[cfg(not(target_arch = "wasm32"))]
                self.order(
                    "Setting pm up".into(),
                    Order::SetUpPlugin { shipped, installed },
                );
                #[cfg(target_arch = "wasm32")]
                drop((shipped, installed));
            }
        });
    }

    fn list(&mut self, ui: &mut egui::Ui) {
        if self.machines.is_empty() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.heading("No machines yet");
                ui.label(
                    "A machine is a computer of its own inside this one. Apps you open in it \
                     cannot see your files or other machines' windows.",
                );
            });
            return;
        }
        let row_height = 2.0 * ui.spacing().interact_size.y;
        let mut select = None;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                let width = ui.available_width();
                for machine in &self.machines {
                    let selected = self.selected.as_ref() == Some(&machine.name);
                    let (rect, row) =
                        ui.allocate_exact_size(egui::vec2(width, row_height), egui::Sense::click());
                    row.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::WidgetType::SelectableLabel,
                            true,
                            selected,
                            format!("{}, {}", machine.name, machine.state.describe()),
                        )
                    });
                    let visuals = ui.visuals();
                    if selected {
                        ui.painter()
                            .rect_filled(rect, 4.0, visuals.selection.bg_fill);
                    } else if row.hovered() {
                        ui.painter()
                            .rect_filled(rect, 4.0, visuals.widgets.hovered.weak_bg_fill);
                    }
                    // The label as a bar down the row's left edge: the same
                    // colour as the machine's window borders.
                    let bar = egui::Rect::from_min_size(rect.min, egui::vec2(6.0, rect.height()));
                    ui.painter().rect_filled(bar, 2.0, colour(machine.label));
                    let text = rect.shrink2(egui::vec2(16.0, 4.0));
                    let details = details(machine);
                    ui.scope_builder(egui::UiBuilder::new().max_rect(text), |ui| {
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::Label::new(egui::RichText::new(&machine.name).strong())
                                    .selectable(false),
                            );
                            ui.add(egui::Label::new(machine.state.describe()).selectable(false));
                        });
                        ui.add(
                            egui::Label::new(egui::RichText::new(details).weak())
                                .truncate()
                                .selectable(false),
                        );
                    });
                    if row.clicked() {
                        select = Some(machine.name.clone());
                    }
                }
            });
        if select.is_some() {
            self.selected = select;
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Act {
    Start,
    ShutDown,
    ForceOff,
    Remove,
}

/// Sets which machine apps open in, keeping the rest of the settings file.
fn save_open_apps_in(machine: &str) -> Result<(), String> {
    let path = derisk_settings::default_path()
        .ok_or_else(|| "There is no config directory to save the setting in".to_owned())?;
    let (mut settings, _) = derisk_settings::Settings::load(&path)
        .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    settings.machines.open_apps_in = machine.to_owned();
    settings
        .save(&path)
        .map_err(|e| format!("Cannot save {}: {e}", path.display()))
}

/// The second line of a machine's row.
fn details(machine: &Machine) -> String {
    let template = machine.template.as_ref().map_or_else(
        || "not made by pm".to_owned(),
        |t| format!("from {}", t.name),
    );
    format!(
        "{} · {} · {} MiB · {} {}",
        machine.label,
        template,
        machine.memory_mib,
        machine.cpus,
        if machine.cpus == 1 {
            "processor"
        } else {
            "processors"
        }
    )
}

/// A label's colour as egui draws it.
pub fn colour(label: Label) -> egui::Color32 {
    let [r, g, b] = label.rgb();
    egui::Color32::from_rgb(r, g, b)
}

fn swatch_button(ui: &mut egui::Ui, label: Label, picked: bool) -> egui::Response {
    let size = egui::vec2(24.0, 24.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, picked, label.name())
    });
    let painter = ui.painter();
    painter.circle_filled(rect.center(), 9.0, colour(label));
    // A hairline in the text colour, so black stays visible on a dark
    // theme and yellow on a light one.
    painter.circle_stroke(
        rect.center(),
        9.0,
        egui::Stroke::new(1.0, ui.visuals().weak_text_color()),
    );
    if picked || response.hovered() {
        let stroke = ui.visuals().widgets.active.fg_stroke;
        painter.circle_stroke(rect.center(), 11.0, stroke);
    }
    response.on_hover_text(label.name())
}

impl App for MachinesApp {
    fn title(&self) -> &str {
        "Machines"
    }

    fn ui(&mut self, ui: &mut egui::Ui, _theme: &Theme) {
        self.receive();
        // While pm works the log moves; otherwise the list changes at most
        // as often as the worker reads it.
        let every = if self.busy.is_some() { 250 } else { 1000 };
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(every));
        if self
            .looked
            .is_none_or(|looked| looked.elapsed() >= std::time::Duration::from_secs(10))
        {
            self.look();
        }

        egui::Panel::top("machines-toolbar").show(ui, |ui| {
            ui.add_space(4.0);
            if let Some(why) = &self.unavailable {
                ui.colored_label(ui.visuals().warn_fg_color, why);
                return;
            }
            self.plugin_banner(ui);
            self.toolbar(ui);
            self.confirmation(ui);
            self.default_machine(ui);
            #[cfg(not(target_arch = "wasm32"))]
            self.open_in(ui);
            self.form(ui);
            if let Some(busy) = &self.busy {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(busy);
                });
                if let Some(line) = self.log.last() {
                    ui.add(
                        egui::Label::new(egui::RichText::new(line).weak().monospace()).truncate(),
                    );
                }
            }
            match &self.status {
                Some(Ok(done)) => {
                    ui.label(done);
                }
                Some(Err(error)) => {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                None => {}
            }
            ui.add_space(4.0);
        });
        egui::CentralPanel::default_margins().show(ui, |ui| self.list(ui));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{State, TemplateRef};

    fn machine(name: &str, label: Label) -> Machine {
        Machine {
            name: name.into(),
            uuid: format!("{name}-uuid"),
            label,
            template: Some(TemplateRef {
                name: "losos-guest".into(),
                digest: "d".into(),
            }),
            system: None,
            screen: false,
            memory_mib: 2048,
            cpus: 2,
            state: State::Off,
            cid: None,
        }
    }

    #[test]
    fn a_new_machine_gets_a_free_name_and_label() {
        let mut app = MachinesApp::new();
        app.machines = vec![
            machine("work", Label::Red),
            machine("work-2", Label::Orange),
        ];
        let form = app.new_form();
        assert_eq!(form.name, "work-3");
        assert_eq!(form.label, Label::Yellow);
        assert_eq!((form.memory_mib, form.cpus, form.disk_gib), (2048, 2, 32));
    }

    #[test]
    fn rows_say_where_a_machine_came_from() {
        let mut odd = machine("win", Label::Gray);
        odd.template = None;
        odd.cpus = 1;
        assert_eq!(
            details(&odd),
            "gray · not made by pm · 2048 MiB · 1 processor"
        );
        assert_eq!(
            details(&machine("work", Label::Blue)),
            "blue · from losos-guest · 2048 MiB · 2 processors"
        );
    }

    #[test]
    fn draws_without_libvirt() {
        let context = egui::Context::default();
        let mut app = MachinesApp::new();
        app.machines = vec![machine("work", Label::Blue)];
        app.selected = Some("work".into());
        app.form = Some(app.new_form());
        for _ in 0..2 {
            let mut output = mcsapi_ui::run_frame(
                &mut app,
                &context,
                egui::RawInput::default(),
                &Theme::default(),
            );
            assert!(!output.shapes.is_empty());
            output.textures_delta.clear();
        }
        assert_eq!(app.title(), "Machines");
    }
}
