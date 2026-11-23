// a window, not a console, when started from the Start menu
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod logic;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};
use hamzip::{net, store, Mode, Model, Tokenizer, MODELS};
use logic::Stats;

const TYPING_PAUSE: Duration = Duration::from_millis(400);

struct Loaded {
    model: Model,
    tok: Tokenizer,
}

enum Status {
    Loading,
    Ready,
    Missing,
    Downloading(u64, Option<u64>),
    Failed(String),
}

struct Compressed {
    msg: Vec<u8>,
    stats: Stats,
}

struct Decoded {
    text: String,
    mode: Mode,
    bytes: usize,
    model: Option<String>,
}

struct DecodeFailed {
    reason: String,
    // the published model the message needs, when it isn't downloaded
    missing: Option<&'static str>,
}

impl From<hamzip::Error> for DecodeFailed {
    fn from(e: hamzip::Error) -> Self {
        DecodeFailed {
            reason: e.to_string(),
            missing: None,
        }
    }
}

enum Update {
    Unknown,
    Available(net::Release, String),
    Installing,
    Installed(String),
    Failed(String),
}

/// What background work reports back to the window.
enum Done {
    Loaded(&'static str, Result<Arc<Loaded>, String>),
    Progress(&'static str, u64, Option<u64>),
    Downloaded(&'static str, Result<(), String>),
    Compressed(u64, Result<Compressed, String>),
    Decoded(
        Result<Decoded, DecodeFailed>,
        Option<(&'static str, Arc<Loaded>)>,
    ),
    Update(Option<(net::Release, String)>),
    Installed(Result<String, String>),
}

#[derive(PartialEq)]
enum Tab {
    Compress,
    Decode,
}

struct App {
    ctx: egui::Context,
    tx: Sender<Done>,
    rx: Receiver<Done>,
    models: HashMap<&'static str, Arc<Loaded>>,
    status: HashMap<&'static str, Status>,
    selected: &'static str,
    tab: Tab,
    text: String,
    edited: Option<Instant>,
    generation: u64,
    compressing: bool,
    compressed: Option<Result<Compressed, String>>,
    rate: u32,
    incoming: String,
    incoming_file: Option<(PathBuf, Vec<u8>)>,
    decoding: bool,
    decoded: Option<Result<Decoded, DecodeFailed>>,
    // decode again once this model has downloaded and loaded
    redecode: Option<&'static str>,
    update: Update,
    note: Option<(String, Instant)>,
}

fn load_job(name: &'static str) -> Result<Arc<Loaded>, String> {
    let model = store::load_published(name).map_err(|e| e.to_string())?;
    let tok = Tokenizer::new(&model);
    Ok(Arc::new(Loaded { model, tok }))
}

impl App {
    fn new(ctx: egui::Context) -> App {
        let (tx, rx) = channel();
        let mut app = App {
            ctx,
            tx,
            rx,
            models: HashMap::new(),
            status: HashMap::new(),
            selected: MODELS[0].0,
            tab: Tab::Compress,
            text: String::new(),
            edited: None,
            generation: 0,
            compressing: false,
            compressed: None,
            rate: 1200,
            incoming: String::new(),
            incoming_file: None,
            decoding: false,
            decoded: None,
            redecode: None,
            update: Update::Unknown,
            note: None,
        };
        app.load(app.selected);
        if std::env::var_os("HAMZIP_NO_UPDATE_CHECK").is_none() {
            app.spawn(|| {
                let found = net::latest_release(Duration::from_secs(10))
                    .ok()
                    .and_then(|r| net::newer(&r).map(|v| (r, v.to_string())));
                Done::Update(found)
            });
        }
        app
    }

    fn spawn(&self, job: impl FnOnce() -> Done + Send + 'static) {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(job());
            ctx.request_repaint();
        });
    }

    fn load(&mut self, name: &'static str) {
        if self.models.contains_key(name) {
            return;
        }
        if store::find_installed(name).is_none() {
            self.status.insert(name, Status::Missing);
            return;
        }
        self.status.insert(name, Status::Loading);
        self.spawn(move || Done::Loaded(name, load_job(name)));
    }

    fn download(&mut self, name: &'static str) {
        self.status.insert(name, Status::Downloading(0, None));
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        self.spawn(move || {
            let mut last = Instant::now();
            let got = net::download_model(name, &store::data_dir(), &mut |n, total| {
                if last.elapsed() > Duration::from_millis(100) {
                    last = Instant::now();
                    let _ = tx.send(Done::Progress(name, n, total));
                    ctx.request_repaint();
                }
            });
            Done::Downloaded(name, got.map(|_| ()).map_err(|e| e.to_string()))
        });
    }

    fn compress(&mut self) {
        self.edited = None;
        self.generation += 1;
        if self.text.is_empty() {
            self.compressed = None;
            return;
        }
        let Some(loaded) = self.models.get(self.selected).cloned() else {
            return;
        };
        self.compressing = true;
        let (generation, text) = (self.generation, self.text.clone());
        self.spawn(move || {
            let msg = hamzip::compress(Some(&loaded.model), Some(&loaded.tok), text.as_bytes());
            let done = Stats::new(&text, &msg)
                .map(|stats| Compressed { msg, stats })
                .map_err(|e| e.to_string());
            Done::Compressed(generation, done)
        });
    }

    fn decode(&mut self) {
        let raw = match &self.incoming_file {
            Some((_, bytes)) => bytes.clone(),
            None => self.incoming.clone().into_bytes(),
        };
        let have: HashMap<&'static str, Arc<Loaded>> = self.models.clone();
        self.decoding = true;
        self.spawn(move || {
            let mut fresh = None;
            let fail = |reason: String, missing| DecodeFailed { reason, missing };
            let result = (|| {
                let msg = hamzip::message_from(&raw)?;
                let mode = hamzip::mode_of(&msg)?;
                let mut model = None;
                let loaded = match msg.get(1..3).filter(|_| mode == Mode::Model) {
                    Some(id) => {
                        let id = [id[0], id[1]];
                        model = Some(hamzip::describe_model(id));
                        let name = hamzip::model_name(id).ok_or_else(|| {
                            fail(
                                format!(
                                    "This message was coded with model {}, which hamzip \
                                     doesn't know. Ask the sender which model they used.",
                                    hamzip::describe_model(id)
                                ),
                                None,
                            )
                        })?;
                        match have.get(name) {
                            Some(l) => Some(l.clone()),
                            None if store::find_installed(name).is_none() => {
                                return Err(fail(
                                    format!(
                                        "This message was coded with {name}, which isn't \
                                         downloaded on this computer yet."
                                    ),
                                    Some(name),
                                ));
                            }
                            None => {
                                let l = load_job(name).map_err(|e| fail(e, None))?;
                                fresh = Some((name, l.clone()));
                                Some(l)
                            }
                        }
                    }
                    None => None,
                };
                let text = hamzip::decompress(loaded.as_ref().map(|l| &l.model), &msg)?;
                Ok(Decoded {
                    text: String::from_utf8_lossy(&text).into_owned(),
                    mode,
                    bytes: msg.len(),
                    model,
                })
            })();
            Done::Decoded(result, fresh)
        });
    }

    fn install_update(&mut self) {
        let Update::Available(rel, version) =
            std::mem::replace(&mut self.update, Update::Installing)
        else {
            return;
        };
        self.spawn(move || {
            let programs = store::exe_dir()
                .map(|d| net::installed_programs(&d))
                .unwrap_or_default();
            let done = if programs.is_empty() {
                Err("couldn't find the installed hamzip programs".to_string())
            } else {
                net::install(&rel, &programs)
                    .map(|_| version)
                    .map_err(|e| e.to_string())
            };
            Done::Installed(done)
        });
    }

    fn receive(&mut self) {
        while let Ok(done) = self.rx.try_recv() {
            match done {
                Done::Loaded(name, Ok(l)) => {
                    self.models.insert(name, l);
                    self.status.insert(name, Status::Ready);
                    if name == self.selected && !self.text.is_empty() {
                        self.compress();
                    }
                    if self.redecode == Some(name) {
                        self.redecode = None;
                        self.decode();
                    }
                }
                Done::Loaded(name, Err(e)) => {
                    self.status.insert(name, Status::Failed(e));
                }
                Done::Progress(name, got, total) => {
                    self.status.insert(name, Status::Downloading(got, total));
                }
                Done::Downloaded(name, Ok(())) => {
                    self.status.remove(name);
                    self.load(name);
                }
                Done::Downloaded(name, Err(e)) => {
                    self.status.insert(name, Status::Failed(e));
                }
                Done::Compressed(generation, done) => {
                    if generation == self.generation {
                        self.compressing = false;
                        self.compressed = Some(done);
                    }
                }
                Done::Decoded(done, fresh) => {
                    self.decoding = false;
                    if let Some((name, l)) = fresh {
                        self.models.insert(name, l);
                        self.status.insert(name, Status::Ready);
                    }
                    if let Some(name) = done.as_ref().err().and_then(|f| f.missing) {
                        self.redecode = Some(name);
                        self.load(name);
                    }
                    self.decoded = Some(done);
                }
                Done::Update(found) => {
                    if let Some((rel, v)) = found {
                        self.update = Update::Available(rel, v);
                    }
                }
                Done::Installed(Ok(v)) => self.update = Update::Installed(v),
                Done::Installed(Err(e)) => self.update = Update::Failed(e),
            }
        }
    }

    fn say(&mut self, note: impl Into<String>) {
        self.note = Some((note.into(), Instant::now()));
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.heading("hamzip");
            ui.label(RichText::new(hamzip::VERSION).weak());
            ui.separator();
            let label = ui.label("Model for compressing:");
            let before = self.selected;
            egui::ComboBox::from_id_salt("model")
                .selected_text(self.selected)
                .show_ui(ui, |ui| {
                    for (name, _) in MODELS {
                        ui.selectable_value(&mut self.selected, name, *name);
                    }
                })
                .response
                .labelled_by(label.id);
            if self.selected != before {
                self.load(self.selected);
                self.compress();
            }
            self.model_status(ui, self.selected);
        });
        match &self.update {
            Update::Available(_, v) => {
                let v = v.clone();
                ui.horizontal(|ui| {
                    ui.label(format!("hamzip {v} is available."));
                    if ui.button("Install update").clicked() {
                        self.install_update();
                    }
                });
            }
            Update::Installing => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Downloading and checking the update…");
                });
            }
            Update::Installed(v) => {
                ui.label(format!(
                    "Installed hamzip {v}. Close and reopen hamzip to use it."
                ));
            }
            Update::Failed(e) => {
                error_label(ui, format!("Update failed: {e}"));
            }
            Update::Unknown => {}
        }
    }

    fn model_status(&mut self, ui: &mut egui::Ui, name: &'static str) {
        match self.status.get(name) {
            Some(Status::Ready) => {
                ui.label("ready");
            }
            Some(Status::Loading) | None => {
                ui.spinner();
                ui.label("loading the model…");
            }
            Some(Status::Missing) => {
                ui.label("not downloaded");
                let size = if name == MODELS[0].0 {
                    "137 MB"
                } else {
                    "365 MB"
                };
                if ui.button(format!("Download ({size})")).clicked() {
                    self.download(name);
                }
            }
            Some(Status::Downloading(got, total)) => {
                let (got, total) = (*got, *total);
                let bar = match total {
                    Some(t) if t > 0 => egui::ProgressBar::new(got as f32 / t as f32).text(
                        format!("{} of {}", logic::format_bytes(got), logic::format_bytes(t)),
                    ),
                    _ => egui::ProgressBar::new(0.0)
                        .text(logic::format_bytes(got))
                        .animate(true),
                };
                ui.add(bar.desired_width(220.0));
            }
            Some(Status::Failed(e)) => {
                error_label(ui, e.clone());
                if ui.button("Try again").clicked() {
                    self.status.remove(name);
                    if store::find_installed(name).is_some() {
                        self.load(name);
                    } else {
                        self.download(name);
                    }
                }
            }
        }
    }

    fn compress_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let label = ui.label("Message to send");
            if ui.button("Open text file…").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Text", &["txt"])
                    .pick_file()
                {
                    match std::fs::read_to_string(&path) {
                        Ok(t) => {
                            self.text = t;
                            self.compress();
                        }
                        Err(e) => self.say(format!("Couldn't read {}: {e}", path.display())),
                    }
                }
            }
            label
        });
        let edit = ui.add(
            egui::TextEdit::multiline(&mut self.text)
                .desired_rows(8)
                .desired_width(f32::INFINITY)
                .hint_text("Type or paste the text you want to send."),
        );
        if edit.changed() {
            self.edited = Some(Instant::now());
            self.ctx.request_repaint_after(TYPING_PAUSE);
        }
        if self.edited.is_some_and(|t| t.elapsed() >= TYPING_PAUSE) {
            self.compress();
        }

        ui.add_space(8.0);
        if self.text.is_empty() {
            ui.label(RichText::new("The compressed size shows up here as you type.").weak());
            return;
        }
        if !self.models.contains_key(self.selected) {
            let why = match self.status.get(self.selected) {
                Some(Status::Missing) => "Download the model above to compress with it.",
                Some(Status::Downloading(..)) => "Compressing as soon as the model has downloaded.",
                Some(Status::Failed(_)) => "The model isn't available; see the message above.",
                _ => "Compressing as soon as the model has loaded.",
            };
            ui.label(RichText::new(why).weak());
            return;
        }
        if self.compressing {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Compressing…");
            });
        }
        match &self.compressed {
            Some(Ok(c)) => {
                let (msg, stats) = (c.msg.clone(), c.stats.clone());
                self.results(ui, &msg, &stats);
            }
            Some(Err(e)) => {
                error_label(ui, e.clone());
            }
            None => {}
        }
    }

    fn results(&mut self, ui: &mut egui::Ui, msg: &[u8], s: &Stats) {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!("{} bytes", s.msg_bytes))
                    .family(heavy())
                    .size(30.0)
                    .strong(),
            );
            ui.label(
                RichText::new(format!(
                    "{:.0}% of the {}-byte text, {:.2} bits per character (plain text is 8)",
                    s.percent_of_text(),
                    s.text_bytes,
                    s.bits_per_char()
                ))
                .size(16.0),
            );
        });
        ui.add_space(10.0);
        egui::Grid::new("results")
            .num_columns(2)
            .spacing([24.0, 8.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Sent").weak());
                ui.label(format!(
                    "{} ({} of those bytes are header, checksum and model id)",
                    logic::mode_label(s.mode),
                    s.mode.framing()
                ));
                ui.end_row();
                ui.label(RichText::new("hz: line").weak());
                ui.label(format!("{} characters", s.text_form_chars));
                ui.end_row();
                let label = ui.label(RichText::new("Airtime at").weak());
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("rate")
                        .selected_text(format!("{} bps", self.rate))
                        .show_ui(ui, |ui| {
                            for r in logic::RATES {
                                ui.selectable_value(&mut self.rate, r, format!("{r} bps"));
                            }
                        })
                        .response
                        .labelled_by(label.id);
                    ui.label(format!(
                        "{} for the message, {} for the plain text (payload only)",
                        logic::format_seconds(logic::airtime(s.msg_bytes, self.rate)),
                        logic::format_seconds(logic::airtime(s.text_bytes, self.rate))
                    ));
                });
                ui.end_row();
            });
        ui.add_space(12.0);
        ui.label(
            RichText::new(
                "Save the .hz file for a link that carries files (VARA, ARDOP, FreeDATA, \
                 packet). Copy the hz: line for one that carries only text, such as a \
                 Winlink message body.",
            )
            .weak(),
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("Copy hz: line").clicked() {
                self.ctx.copy_text(hamzip::armor::to_text(msg));
                self.say("Copied the hz: line. Paste it into the text mode you're using.");
            }
            if ui.button("Save message file…").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("hamzip message", &["hz"])
                    .set_file_name("message.hz")
                    .save_file()
                {
                    match std::fs::write(&path, msg) {
                        Ok(()) => self.say(format!("Saved {}", path.display())),
                        Err(e) => self.say(format!("Couldn't save {}: {e}", path.display())),
                    }
                }
            }
        });
    }

    fn decode_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let label = ui.label("Received message");
            if ui.button("Open message file…").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("hamzip message", &["hz", "txt"])
                    .pick_file()
                {
                    match std::fs::read(&path) {
                        Ok(bytes) => {
                            self.incoming_file = Some((path, bytes));
                            self.decode();
                        }
                        Err(e) => self.say(format!("Couldn't read {}: {e}", path.display())),
                    }
                }
            }
            label
        });
        if let Some((path, bytes)) = &self.incoming_file {
            let shown = format!("{} ({} bytes)", path.display(), bytes.len());
            ui.horizontal(|ui| {
                ui.label(shown);
                if ui.button("Clear").clicked() {
                    self.incoming_file = None;
                    self.decoded = None;
                }
            });
        } else {
            // taken before the text box sees it, which would add a line break
            let shortcut =
                ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
            let edit = ui.add(
                egui::TextEdit::multiline(&mut self.incoming)
                    .desired_rows(4)
                    .desired_width(f32::INFINITY)
                    .font(egui::TextStyle::Monospace)
                    .hint_text(
                        "Paste an hz: line, or the whole received text around it. \
                         Ctrl+Enter decodes.",
                    ),
            );
            if edit.changed() {
                self.decoded = None;
                self.redecode = None;
            }
            if shortcut && edit.has_focus() && !self.incoming.trim().is_empty() && !self.decoding {
                self.decode();
            }
        }
        let ready = self.incoming_file.is_some() || !self.incoming.trim().is_empty();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(ready && !self.decoding, egui::Button::new("Decode"))
                .on_hover_text("Ctrl+Enter")
                .clicked()
            {
                self.decode();
            }
            if self.decoding {
                ui.spinner();
                ui.label("Decoding…");
            }
        });

        ui.add_space(8.0);
        match &self.decoded {
            Some(Ok(d)) => {
                let info = logic::received_summary(d.mode, d.bytes, d.model.as_deref());
                ui.label(RichText::new(info).weak());
                // a &str buffer can be selected and copied but not edited
                ui.add(
                    egui::TextEdit::multiline(&mut d.text.as_str())
                        .desired_rows(8)
                        .desired_width(f32::INFINITY)
                        .background_color(ui.visuals().extreme_bg_color),
                );
                ui.horizontal(|ui| {
                    if ui.button("Copy text").clicked() {
                        self.ctx.copy_text(d.text.clone());
                        self.note = Some(("Copied the text.".into(), Instant::now()));
                    }
                    if ui.button("Save text…").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("Text", &["txt"])
                            .set_file_name("message.txt")
                            .save_file()
                        {
                            let saved = std::fs::write(&path, d.text.as_bytes());
                            self.note = Some((
                                match saved {
                                    Ok(()) => format!("Saved {}", path.display()),
                                    Err(e) => format!("Couldn't save {}: {e}", path.display()),
                                },
                                Instant::now(),
                            ));
                        }
                    }
                });
            }
            Some(Err(e)) => {
                let (reason, missing) = (e.reason.clone(), e.missing);
                error_label(ui, reason);
                if let Some(name) = missing {
                    ui.horizontal(|ui| self.model_status(ui, name));
                    ui.label(RichText::new("It decodes as soon as the model is ready.").weak());
                }
            }
            None if !ready => {
                ui.label(RichText::new("The decoded text shows up here.").weak());
            }
            None => {}
        }
    }
}

fn error_label(ui: &mut egui::Ui, s: String) {
    let color = ui.visuals().error_fg_color;
    ui.label(RichText::new(s).color(color));
}

fn heavy() -> egui::FontFamily {
    egui::FontFamily::Name("bold".into())
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.receive();
        egui::Panel::top("header").show(ui, |ui| {
            ui.add_space(6.0);
            self.header(ui);
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Compress, "Compress");
                ui.selectable_value(&mut self.tab, Tab::Decode, "Decode");
            });
            ui.add_space(4.0);
        });
        if self
            .note
            .as_ref()
            .is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(6))
        {
            self.note = None;
        }
        if let Some((note, _)) = &self.note {
            let note = note.clone();
            egui::Panel::bottom("note").show(ui, |ui| {
                ui.add_space(4.0);
                ui.label(note);
                ui.add_space(4.0);
            });
            self.ctx.request_repaint_after(Duration::from_secs(1));
        }
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| match self.tab {
                Tab::Compress => self.compress_tab(ui),
                Tab::Decode => self.decode_tab(ui),
            });
        });
    }
}

/// Signal-strength bars over a dark amber square, so the window has its own icon.
fn icon() -> egui::IconData {
    const N: usize = 64;
    let mut rgba = vec![0u8; N * N * 4];
    for y in 0..N {
        for x in 0..N {
            let i = (y * N + x) * 4;
            let inside = (4..N - 4).contains(&x) && (4..N - 4).contains(&y);
            let bar =
                |left: usize, top: usize| x >= left && x < left + 10 && y >= top && y < N - 12;
            let px = if bar(13, 14) || bar(27, 24) || bar(41, 34) {
                [0xFF, 0xF4, 0xE0, 0xFF]
            } else if inside {
                [0xB8, 0x5C, 0x00, 0xFF]
            } else {
                [0, 0, 0, 0]
            };
            rgba[i..i + 4].copy_from_slice(&px);
        }
    }
    egui::IconData {
        rgba,
        width: N as u32,
        height: N as u32,
    }
}

fn fonts(ctx: &egui::Context) {
    // Atkinson Hyperlegible keeps 0/O, 1/l/I and B/8 apart, which matters in callsigns
    let mut fonts = egui::FontDefinitions::default();
    for (name, bytes) in [
        (
            "atkinson",
            &include_bytes!("../fonts/AtkinsonHyperlegible-Regular.ttf")[..],
        ),
        (
            "atkinson-bold",
            &include_bytes!("../fonts/AtkinsonHyperlegible-Bold.ttf")[..],
        ),
    ] {
        fonts
            .font_data
            .insert(name.into(), Arc::new(egui::FontData::from_static(bytes)));
    }
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "atkinson".into());
    let mut bold = vec!["atkinson-bold".to_string()];
    bold.extend(
        fonts.families[&egui::FontFamily::Proportional]
            .iter()
            .skip(1)
            .cloned(),
    );
    fonts.families.insert(heavy(), bold);
    ctx.set_fonts(fonts);
}

fn style(ctx: &egui::Context) {
    fonts(ctx);
    // amber for the accent rather than egui's blue; egui's pure red errors and faded
    // gray fall under 4.5:1 contrast on the light background
    for (theme, accent, error, weak) in [
        (
            egui::Theme::Light,
            Color32::from_rgb(0xB8, 0x5C, 0x00),
            Color32::from_rgb(0xB4, 0x23, 0x18),
            Color32::from_rgb(0x5C, 0x5C, 0x5C),
        ),
        (
            egui::Theme::Dark,
            Color32::from_rgb(0xF0, 0xA0, 0x40),
            Color32::from_rgb(0xFF, 0x8A, 0x70),
            Color32::from_rgb(0x9C, 0x9C, 0x9C),
        ),
    ] {
        ctx.style_mut_of(theme, |s| {
            s.visuals.error_fg_color = error;
            s.visuals.weak_text_color = Some(weak);
            s.visuals.selection.bg_fill = accent.linear_multiply(0.35);
            // text drawn over the accent, such as on the progress bar, keeps full contrast
            s.visuals.selection.stroke.color = s.visuals.strong_text_color();
            s.visuals.hyperlink_color = accent;
            s.spacing.button_padding = egui::vec2(10.0, 5.0);
            // as tall as a button at this text size, so labels beside buttons line up
            s.spacing.interact_size.y = 28.0;
            for (text, size) in [
                (egui::TextStyle::Body, 15.0),
                (egui::TextStyle::Button, 15.0),
                (egui::TextStyle::Monospace, 14.0),
                (egui::TextStyle::Heading, 22.0),
                (egui::TextStyle::Small, 12.0),
            ] {
                if let Some(font) = s.text_styles.get_mut(&text) {
                    font.size = size;
                }
            }
        });
    }
}

fn main() -> eframe::Result {
    if std::env::args().any(|a| a == "--version") {
        println!("hamzip-gui {}", hamzip::VERSION);
        return Ok(());
    }
    net::clean_old_exe();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("hamzip")
            .with_inner_size([820.0, 640.0])
            .with_min_inner_size([520.0, 420.0])
            .with_icon(Arc::new(icon())),
        ..Default::default()
    };
    eframe::run_native(
        "hamzip",
        options,
        Box::new(|cc| {
            style(&cc.egui_ctx);
            Ok(Box::new(App::new(cc.egui_ctx.clone())))
        }),
    )
}
