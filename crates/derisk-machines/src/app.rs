//! The Machines app: the machines on this computer in a sidebar, each
//! marked in its label's colour, and a page per machine with its state,
//! what it is, the apps it can open and removing it, drawn as Settings is.

use derisk_sys::time::Instant;
use mcsapi_components::{Button, ButtonVariant, Tokens};
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
    /// The machines' apps' icons, by app ID, once looked up.
    #[cfg(not(target_arch = "wasm32"))]
    icons: std::collections::HashMap<String, Option<egui::TextureHandle>>,
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
            #[cfg(not(target_arch = "wasm32"))]
            icons: std::collections::HashMap::new(),
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

    /// The machine shown: the one picked, else the first.
    fn shown(&self) -> Option<Machine> {
        self.selected
            .as_ref()
            .and_then(|name| self.machines.iter().find(|m| &m.name == name))
            .or_else(|| self.machines.first())
            .cloned()
    }

    /// Whether a machine can be made now: nothing else under way, a
    /// template to make it from, and pm able to.
    fn can_create(&self) -> bool {
        self.busy.is_none()
            && self.unavailable.is_none()
            && !self.templates.is_empty()
            && matches!(self.plugin, Plugin::Ready | Plugin::NotShipped)
    }

    /// Whether the sidebar has anything to list: a machine, or New machine.
    fn has_sidebar(&self) -> bool {
        !self.machines.is_empty() || self.form.is_some() || self.can_create()
    }

    /// The machines, each marked in its label's colour, and New machine.
    fn sidebar(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        let shown = self.shown().map(|m| m.name);
        let mut picked = None;
        let mut new = false;
        let creating = self.form.is_some();
        let can_create = self.can_create();
        derisk_frame::navigation(ui, tokens, "machines-list", |nav| {
            if !self.machines.is_empty() {
                nav.section("Your machines");
            }
            for machine in &self.machines {
                let open = !creating && shown.as_ref() == Some(&machine.name);
                if nav
                    .entry_dot(colour(machine.label), &machine.name, open)
                    .clicked()
                {
                    picked = Some(machine.name.clone());
                }
            }
            if can_create || creating {
                nav.section("Add");
                new = nav
                    .entry_icon("list-add", "New machine", creating)
                    .clicked();
            }
        });
        if let Some(name) = picked {
            self.selected = Some(name);
            self.form = None;
        }
        if new && self.form.is_none() {
            self.form = Some(self.new_form());
        }
    }

    /// What the machines are doing along the bottom, in words rather than
    /// pm's lines, with New machine at its right.
    fn action_bar(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        let can_create = self.can_create() && self.form.is_none();
        let mut new = false;
        let mut dismiss = false;
        derisk_frame::action_bar(ui, tokens, "machines-actions", |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                new = ui
                    .add(Button::new("New machine").enabled(can_create))
                    .on_disabled_hover_text(if self.templates.is_empty() {
                        "This system has no templates to make a machine from"
                    } else {
                        "Set pm up for machines first, or wait for what is under way"
                    })
                    .clicked();
                if self.status.is_some() && self.busy.is_none() {
                    dismiss = ui
                        .add(Button::new("Dismiss").variant(ButtonVariant::Ghost))
                        .clicked();
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    if let Some(busy) = &self.busy {
                        ui.spinner();
                        ui.label(busy);
                        // pm's latest line, quietly, so a slow step shows
                        // it moves without the bar reading like a log.
                        if let Some(line) = self.log.last() {
                            ui.add(egui::Label::new(derisk_frame::note(tokens, line)).truncate());
                        }
                        return;
                    }
                    match &self.status {
                        Some(Ok(done)) => {
                            ui.add(egui::Label::new(done.as_str()).truncate());
                        }
                        Some(Err(error)) => {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(error.as_str()).color(tokens.destructive),
                                )
                                .truncate(),
                            );
                        }
                        None => {
                            let count = match self.machines.len() {
                                0 => "No machines yet".to_owned(),
                                1 => "1 machine".to_owned(),
                                n => format!("{n} machines"),
                            };
                            ui.label(derisk_frame::note(tokens, &count));
                        }
                    }
                });
            });
        });
        if dismiss {
            self.status = None;
        }
        if new {
            self.form = Some(self.new_form());
        }
    }

    /// Setting pm's libvirt plugin up, as a card over the page while it
    /// is not.
    fn plugin_card(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
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
        let idle = self.busy.is_none();
        let mut set_up = false;
        derisk_frame::card(ui, tokens, |ui| {
            ui.label(egui::RichText::new("Set pm up for machines").strong());
            ui.label(derisk_frame::note(tokens, text));
            set_up = ui
                .add(Button::new("Set up pm for machines").enabled(idle))
                .clicked();
        });
        ui.add_space(8.0);
        if set_up {
            #[cfg(not(target_arch = "wasm32"))]
            self.order(
                "Setting pm up".into(),
                Order::SetUpPlugin { shipped, installed },
            );
            #[cfg(target_arch = "wasm32")]
            drop((shipped, installed));
        }
    }

    /// Force off and Remove lose something, so they ask first, in a card
    /// at the top of the page.
    fn confirmation(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
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
        derisk_frame::Card::new()
            .stripe(tokens.destructive)
            .show(ui, tokens, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(question);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(Button::new(button).variant(ButtonVariant::Destructive))
                            .clicked()
                        {
                            self.confirm = None;
                            if let Some(machine) =
                                self.machines.iter().find(|m| &m.name == name).cloned()
                            {
                                let doing = match act {
                                    Act::Remove => format!("Removing {name}"),
                                    _ => format!("Forcing {name} off"),
                                };
                                self.act(doing, act, &machine);
                            }
                        }
                        if ui
                            .add(Button::new("Cancel").variant(ButtonVariant::Outline))
                            .clicked()
                        {
                            self.confirm = None;
                        }
                    });
                });
            });
        ui.add_space(8.0);
    }

    fn page(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        if let Some(why) = self.unavailable.clone() {
            derisk_frame::header(ui, tokens, "Machines", "");
            derisk_frame::card(ui, tokens, |ui| {
                derisk_frame::empty(ui, tokens, "Machines are not available", &why);
            });
            return;
        }
        self.plugin_card(ui, tokens);
        self.confirmation(ui, tokens);
        if self.form.is_some() {
            return self.form(ui, tokens);
        }
        let Some(machine) = self.shown() else {
            derisk_frame::header(
                ui,
                tokens,
                "Machines",
                "Computers of their own inside this one.",
            );
            let can_create = self.can_create();
            derisk_frame::card(ui, tokens, |ui| {
                derisk_frame::empty(
                    ui,
                    tokens,
                    "No machines yet",
                    "A machine is a computer of its own inside this one. Apps you open in it \
                     cannot see your files or other machines' windows.",
                );
                ui.vertical_centered(|ui| {
                    if ui
                        .add(Button::new("New machine").enabled(can_create))
                        .clicked()
                    {
                        self.form = Some(self.new_form());
                    }
                    ui.add_space(16.0);
                });
            });
            return;
        };
        self.machine_page(ui, tokens, &machine);
    }

    /// One machine: its state with Start or Shut down beside it, a card of
    /// what it is with its label down the edge, the apps it can open, and
    /// removing it.
    fn machine_page(&mut self, ui: &mut egui::Ui, tokens: &Tokens, machine: &Machine) {
        let idle = self.busy.is_none();
        let active = machine.state.is_active();
        let mut act = None;
        derisk_frame::header_with(ui, tokens, &machine.name, machine.state.describe(), |ui| {
            // From the right: the step that matters most is rightmost.
            if active {
                if ui.add(Button::new("Shut down").enabled(idle)).clicked() {
                    act = Some(Act::ShutDown);
                }
                if ui
                    .add(
                        Button::new("Force off")
                            .variant(ButtonVariant::Outline)
                            .enabled(idle),
                    )
                    .clicked()
                {
                    self.confirm = Some(Confirm::ForceOff(machine.name.clone()));
                }
            } else if ui.add(Button::new("Start").enabled(idle)).clicked() {
                act = Some(Act::Start);
            }
        });
        match act {
            Some(Act::Start) => self.act(format!("Starting {}", machine.name), Act::Start, machine),
            Some(Act::ShutDown) => self.act(
                format!("Shutting {} down", machine.name),
                Act::ShutDown,
                machine,
            ),
            _ => {}
        }
        derisk_frame::Card::new()
            .stripe(colour(machine.label))
            .show(ui, tokens, |ui| {
                derisk_frame::rows(ui, ("machine", &machine.name), |ui| {
                    ui.label("Label");
                    ui.horizontal(|ui| {
                        let (dot, _) =
                            ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                        ui.painter()
                            .circle_filled(dot.center(), 6.0, colour(machine.label));
                        ui.label(capitalised(machine.label.name()));
                    });
                    ui.end_row();
                    ui.label("Made from");
                    ui.label(made_from(machine));
                    ui.end_row();
                    ui.label("Memory");
                    ui.label(format!("{} MiB", machine.memory_mib));
                    ui.end_row();
                    ui.label("Processors");
                    ui.label(machine.cpus.to_string());
                    ui.end_row();
                });
            });
        if machine.from_pm() {
            derisk_frame::subheading(ui, tokens, "Apps");
            derisk_frame::card(ui, tokens, |ui| self.apps(ui, tokens, machine));
        }
        derisk_frame::subheading(ui, tokens, "Remove");
        derisk_frame::card(ui, tokens, |ui| {
            // pm removes a machine with its disk, so only a machine pm made,
            // whose disk is pm's to remove, and only while it is off.
            let removable = machine.from_pm() && !active;
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(format!("Remove {} and its disk", machine.name));
                    let why = if !machine.from_pm() {
                        "Only a machine pm made can be removed here."
                    } else if active {
                        "Shut it down first."
                    } else {
                        "Its files go with it and cannot be brought back."
                    };
                    ui.label(derisk_frame::note(tokens, why));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let remove = Button::new("Remove")
                        .variant(ButtonVariant::Destructive)
                        .enabled(idle && removable);
                    if ui.add(remove).clicked() {
                        self.confirm = Some(Confirm::Remove(machine.name.clone()));
                    }
                });
            });
        });
    }

    /// The apps `machine` can open, as icons that open them on the
    /// desktop, and whether apps opened from the desktop open in it.
    fn apps(&mut self, ui: &mut egui::Ui, tokens: &Tokens, machine: &Machine) {
        #[cfg(not(target_arch = "wasm32"))]
        self.app_icons(ui, tokens, machine);
        ui.add_space(8.0);
        ui.separator();
        // derisk's `machines.open_apps_in` setting, which the session reads
        // when it launches an app; only a machine pm made has the guest
        // side that opens apps.
        let mut here = self.open_apps_in == machine.name;
        let changed = ui
            .checkbox(
                &mut here,
                format!("Open apps from the desktop in {}", machine.name),
            )
            .changed();
        ui.label(derisk_frame::note(
            tokens,
            &format!(
                "Apps you open from the desktop or the palette open in {} instead of on this \
                 computer, starting it if it is off. derisk's own apps stay here.",
                machine.name
            ),
        ));
        if changed {
            let value = if here {
                machine.name.clone()
            } else {
                String::new()
            };
            match save_open_apps_in(&value) {
                Ok(()) => self.open_apps_in = value,
                Err(error) => self.status = Some(Err(error)),
            }
        }
    }

    /// The running machine's apps as icons. A machine is asked for its
    /// list once it runs; one that does not answer (it is still booting) is
    /// asked again a little later.
    #[cfg(not(target_arch = "wasm32"))]
    fn app_icons(&mut self, ui: &mut egui::Ui, tokens: &Tokens, machine: &Machine) {
        if machine.state != crate::State::Running {
            ui.label(derisk_frame::note(
                tokens,
                &format!("Start {} to open its apps from here.", machine.name),
            ));
            return;
        }
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
        match self.apps.get(&machine.name).and_then(|(_, a)| a.as_ref()) {
            None | Some(Err(_)) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(derisk_frame::note(
                        tokens,
                        &format!("Asking {} which apps it has", machine.name),
                    ));
                });
            }
            Some(Ok(apps)) if apps.is_empty() => {
                ui.label(derisk_frame::note(tokens, "It has no apps to show here."));
            }
            Some(Ok(apps)) => {
                let apps = apps.clone();
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
                    for app in &apps {
                        let texture = self.icon(ui.ctx(), &app.id);
                        let response = app_tile(ui, tokens, &app.name, texture.as_ref());
                        if idle && response.clicked() {
                            open = Some(app.clone());
                        }
                    }
                });
            }
        }
        if let Some(app) = open {
            self.order(
                format!("Opening {} in {}", app.name, machine.name),
                Order::Open {
                    machine: machine.name.clone(),
                    app,
                },
            );
        }
    }

    /// App `id`'s icon from the icon theme, looked up once.
    #[cfg(not(target_arch = "wasm32"))]
    fn icon(&mut self, ctx: &egui::Context, id: &str) -> Option<egui::TextureHandle> {
        self.icons
            .entry(id.to_owned())
            .or_insert_with(|| {
                let path = derisk_icons::find(id, 48)?;
                let image = derisk_icons::load(&path, 48)?;
                Some(ctx.load_texture(
                    format!("derisk-machines-app:{id}"),
                    image,
                    egui::TextureOptions::LINEAR,
                ))
            })
            .clone()
    }

    fn form(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        let Some(form) = &mut self.form else {
            return;
        };
        let mut close = false;
        let mut create = None;
        derisk_frame::header(
            ui,
            tokens,
            "New machine",
            "A computer of its own inside this one: apps you open in it cannot see your \
             files or other machines' windows.",
        );
        derisk_frame::card(ui, tokens, |ui| {
            derisk_frame::rows(ui, "new-machine", |ui| {
                ui.label("Name");
                ui.add(derisk_frame::field(&mut form.name).desired_width(220.0));
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
                        if swatch_button(ui, tokens, label, picked).clicked() {
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
            ui.horizontal(|ui| {
                if let Some(problem) = problem {
                    ui.label(egui::RichText::new(problem).color(tokens.destructive));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(Button::new("Create").enabled(problem.is_none()))
                        .clicked()
                    {
                        create = self
                            .templates
                            .get(form.template)
                            .map(|t| (t.path.clone(), form.clone()));
                    }
                    if ui
                        .add(Button::new("Cancel").variant(ButtonVariant::Outline))
                        .clicked()
                    {
                        close = true;
                    }
                });
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

/// What a machine was made from, for its card.
fn made_from(machine: &Machine) -> String {
    machine
        .template
        .as_ref()
        .map_or_else(|| "Not made by pm".to_owned(), |t| t.name.clone())
}

/// `name` with a capital first letter, as a label's name reads on its own.
fn capitalised(name: &str) -> String {
    let mut chars = name.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// One of a machine's apps: its icon over its name, a tile that opens it.
/// An app the host has no icon for shows its initial.
#[cfg(not(target_arch = "wasm32"))]
fn app_tile(
    ui: &mut egui::Ui,
    tokens: &Tokens,
    name: &str,
    icon: Option<&egui::TextureHandle>,
) -> egui::Response {
    derisk_frame::tile(ui, tokens, egui::vec2(96.0, 92.0), name, |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(4.0);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(40.0, 40.0), egui::Sense::hover());
            match icon {
                Some(texture) => {
                    egui::Image::new(texture)
                        .fit_to_exact_size(rect.size())
                        .paint_at(ui, rect);
                }
                None => {
                    ui.painter().rect_filled(rect, 8.0, tokens.field);
                    let initial: String =
                        name.chars().take(1).flat_map(char::to_uppercase).collect();
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        initial,
                        egui::FontId::proportional(20.0),
                        tokens.muted_foreground,
                    );
                }
            }
            ui.add(egui::Label::new(name).truncate().selectable(false));
        });
    })
}

/// A label's colour as egui draws it.
pub fn colour(label: Label) -> egui::Color32 {
    let [r, g, b] = label.rgb();
    egui::Color32::from_rgb(r, g, b)
}

fn swatch_button(ui: &mut egui::Ui, tokens: &Tokens, label: Label, picked: bool) -> egui::Response {
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
        egui::Stroke::new(1.0, tokens.muted_foreground),
    );
    // Ringed as Settings rings the accent it has picked.
    if picked {
        painter.circle_stroke(
            rect.center(),
            11.5,
            egui::Stroke::new(2.0, tokens.foreground),
        );
    } else if response.hovered() {
        painter.circle_stroke(
            rect.center(),
            11.5,
            egui::Stroke::new(1.0, tokens.muted_foreground),
        );
    }
    response.on_hover_text(label.name())
}

impl App for MachinesApp {
    fn title(&self) -> &str {
        "Machines"
    }

    fn ui(&mut self, ui: &mut egui::Ui, theme: &Theme) {
        self.receive();
        // While pm works its lines move; otherwise the list changes at most
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
        let tokens = derisk_frame::begin(ui, theme);
        // With no machine to list and none to make yet, the sidebar would
        // be an empty column; the page says why instead.
        if self.has_sidebar() {
            self.sidebar(ui, &tokens);
        }
        self.action_bar(ui, &tokens);
        derisk_frame::Page::new().show(ui, &tokens, |ui| self.page(ui, &tokens));
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
            service: false,
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
    fn cards_say_where_a_machine_came_from() {
        let mut odd = machine("win", Label::Gray);
        odd.template = None;
        assert_eq!(made_from(&odd), "Not made by pm");
        assert_eq!(made_from(&machine("work", Label::Blue)), "losos-guest");
        assert_eq!(capitalised(Label::Blue.name()), "Blue");
    }

    #[test]
    fn the_sidebar_shows_only_with_something_in_it() {
        let mut app = MachinesApp::new();
        app.unavailable = Some("no libvirt".into());
        assert!(!app.has_sidebar());
        app.machines = vec![machine("work", Label::Blue)];
        assert!(app.has_sidebar());
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
