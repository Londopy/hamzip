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
    // the bytes as sent, which "Save text" writes even when they aren't UTF-8
    raw: Vec<u8>,
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
        u64,
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
    jobs: logic::Latest,
    // with the generation it was made for, to tell when the text has moved on
    compressed: Option<(u64, Result<Compressed, String>)>,
    rate: u32,
    incoming: String,
    incoming_file: Option<(PathBuf, Vec<u8>)>,
    decode_gen: u64,
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
            jobs: logic::Latest::default(),
            compressed: None,
            rate: 1200,
            incoming: String::new(),
            incoming_file: None,
            decode_gen: 0,
            decoding: false,
            decoded: None,
            redecode: None,
            update: Update::Unknown,
            note: None,
        };
        app.load(app.selected);
        let off = ["HAMZIP_NO_UPDATE_CHECK", "CI"]
            .iter()
            .any(|v| std::env::var_os(v).is_some());
        if !off {
            app.check_update();
        }
        app
    }

    fn check_update(&mut self) {
        self.update = Update::Unknown;
        self.spawn(|| {
            let found = net::latest_release(Duration::from_secs(10))
                .ok()
                .and_then(|r| net::newer(&r).map(|v| (r, v.to_string())));
            Done::Update(found)
        });
    }

    fn spawn(&self, job: impl FnOnce() -> Done + Send + 'static) {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(job());
            ctx.request_repaint();
        });
    }

    fn downloading(&self, name: &str) -> bool {
        matches!(self.status.get(name), Some(Status::Downloading(..)))
    }

    fn load(&mut self, name: &'static str) {
        // the file isn't there until the download finishes, which then loads it
        if self.models.contains_key(name)
            || self.downloading(name)
            || matches!(self.status.get(name), Some(Status::Loading))
        {
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
        // a second download would write the same .part file
        if self.downloading(name) {
            return;
        }
        self.status.insert(name, Status::Downloading(0, None));
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        self.spawn(move || {
            let mut last = Instant::now();
            // replace a damaged copy where it is, since that's where loading looks first
            let dir = store::find_installed(name)
                .and_then(|p| p.parent().map(PathBuf::from))
                .unwrap_or_else(store::data_dir);
            let got = net::download_model(name, &dir, &mut |n, total| {
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
        if self.text.is_empty() {
            self.compressed = None;
            return;
        }
        let Some(loaded) = self.models.get(self.selected).cloned() else {
            return;
        };
        let Some(generation) = self.jobs.start() else {
            return;
        };
        let text = self.text.clone();
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
        self.decode_gen += 1;
        let generation = self.decode_gen;
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
                let raw = hamzip::decompress(loaded.as_ref().map(|l| &l.model), &msg)?;
                Ok(Decoded {
                    text: String::from_utf8_lossy(&raw).into_owned(),
                    raw,
                    mode,
                    bytes: msg.len(),
                    model,
                })
            })();
            Done::Decoded(generation, result, fresh)
        });
    }

    /// Drops a decode on its way and its result, for input that has changed.
    fn forget_decode(&mut self) {
        self.decode_gen += 1;
        self.decoding = false;
        self.decoded = None;
        self.redecode = None;
    }

    fn install_update(&mut self) {
        let Update::Available(rel, version) =
            std::mem::replace(&mut self.update, Update::Installing)
        else {
            return;
        };
        self.spawn(move || {
            let done = std::env::current_exe()
                .map_err(|e| format!("finding this program: {e}"))
                .and_then(|exe| {
                    let programs = net::programs_beside(&exe, "hamzip-gui");
                    net::fetch(&rel, &programs)
                        .and_then(|f| net::replace_all(&f))
                        .map_err(|e| {
                            format!("{e}; install {version} from {} instead", rel.html_url)
                        })
                })
                .map(|_| version);
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
                    let (show, again) = self.jobs.finish(generation);
                    if show {
                        self.compressed = Some((generation, done));
                    }
                    if again {
                        self.compress();
                    }
                }
                Done::Decoded(generation, done, fresh) => {
                    if let Some((name, l)) = fresh {
                        self.models.insert(name, l);
                        self.status.insert(name, Status::Ready);
                    }
                    if generation != self.decode_gen {
                        continue;
                    }
                    self.decoding = false;
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
                self.jobs.changed();
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
                let e = e.clone();
                ui.horizontal_wrapped(|ui| {
                    error_label(ui, format!("Update failed: {e}"));
                    if ui.button("Try again").clicked() {
                        self.check_update();
                    }
                });
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
                if ui.button(download_label(name)).clicked() {
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
                let installed = store::find_installed(name).is_some();
                if installed && ui.button("Try again").clicked() {
                    self.status.remove(name);
                    self.load(name);
                }
                // a damaged or rebuilt file is replaced when the download finishes
                let again = if installed {
                    "Download again".to_string()
                } else {
                    download_label(name)
                };
                if ui.button(again).clicked() {
                    self.status.remove(name);
                    self.download(name);
                }
            }
        }
    }

    fn compress_tab(&mut self, ui: &mut egui::Ui) {
        let label = ui
            .horizontal(|ui| {
                let label = ui.label("Message to send");
                if ui.button("Open text file…").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("Text", &["txt"])
                        .pick_file()
                    {
                        match read_small(&path).and_then(|b| {
                            String::from_utf8(b)
                                .map_err(|_| format!("{} isn't UTF-8 text", path.display()))
                        }) {
                            Ok(t) => {
                                self.text = t;
                                self.jobs.changed();
                                self.compress();
                            }
                            Err(e) => self.say(e),
                        }
                    }
                }
                label
            })
            .inner;
        let edit = ui
            .add(
                egui::TextEdit::multiline(&mut self.text)
                    .desired_rows(8)
                    .desired_width(f32::INFINITY)
                    .hint_text("Type or paste the text you want to send."),
            )
            .labelled_by(label.id);
        if edit.changed() {
            self.jobs.changed();
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
        let stale = self
            .compressed
            .as_ref()
            .is_none_or(|(generation, _)| !self.jobs.is_current(*generation));
        if stale {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Compressing…");
            });
        }
        match &self.compressed {
            Some((_, Ok(c))) => {
                let (msg, stats) = (c.msg.clone(), c.stats.clone());
                // an older result stays visible but can't be copied or saved
                ui.add_enabled_ui(!stale, |ui| self.results(ui, &msg, &stats));
            }
            Some((_, Err(e))) if !stale => {
                error_label(ui, e.clone());
            }
            _ => {}
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
                if s.text_form_chars >= s.text_bytes {
                    ui.label(format!(
                        "{} characters, no shorter than the text itself: over a text \
                         channel, send the text as it is",
                        s.text_form_chars
                    ));
                } else {
                    ui.label(format!("{} characters", s.text_form_chars));
                }
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
        // taken before the text box sees it, which would add a line break
        let shortcut = ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
        let label = ui
            .horizontal(|ui| {
                let label = ui.label("Received message");
                if ui.button("Open message file…").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("hamzip message", &["hz", "txt"])
                        .pick_file()
                    {
                        match read_small(&path) {
                            Ok(bytes) => {
                                self.forget_decode();
                                self.incoming_file = Some((path, bytes));
                                self.decode();
                            }
                            Err(e) => self.say(e),
                        }
                    }
                }
                label
            })
            .inner;
        if let Some((path, bytes)) = &self.incoming_file {
            let shown = format!("{} ({} bytes)", path.display(), bytes.len());
            ui.horizontal(|ui| {
                ui.label(shown);
                if ui.button("Clear").clicked() {
                    self.incoming_file = None;
                    self.forget_decode();
                }
            });
        } else {
            let edit = ui
                .add(
                    egui::TextEdit::multiline(&mut self.incoming)
                        .desired_rows(4)
                        .desired_width(f32::INFINITY)
                        .font(egui::TextStyle::Monospace)
                        .hint_text(
                            "Paste an hz: line, or the whole received text around it. \
                             Ctrl+Enter decodes.",
                        ),
                )
                .labelled_by(label.id);
            if edit.changed() {
                self.forget_decode();
            }
        }
        let ready = self.incoming_file.is_some() || !self.incoming.trim().is_empty();
        if shortcut && ready && !self.decoding {
            self.decode();
        }
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
                let summary = ui.label(RichText::new(info).weak());
                if d.text.as_bytes() != d.raw {
                    ui.label(
                        RichText::new(
                            "Some of the bytes aren't text and show as \u{fffd}; \
                             Save text writes them as they were sent.",
                        )
                        .weak(),
                    );
                }
                // a &str buffer can be selected and copied but not edited
                ui.add(
                    egui::TextEdit::multiline(&mut d.text.as_str())
                        .desired_rows(8)
                        .desired_width(f32::INFINITY)
                        .background_color(ui.visuals().extreme_bg_color),
                )
                .labelled_by(summary.id);
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
                            let saved = std::fs::write(&path, &d.raw);
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

fn download_label(name: &str) -> String {
    match name {
        "smollm2-135m" => "Download (137 MB)".into(),
        "smollm2-360m" => "Download (365 MB)".into(),
        _ => "Download".into(),
    }
}

/// Reads a file the user picked, refusing anything bigger than a message can be.
fn read_small(path: &std::path::Path) -> Result<Vec<u8>, String> {
    let too_big = std::fs::metadata(path).is_ok_and(|m| m.len() > 1 << 20);
    if too_big {
        return Err(format!(
            "{} is over 1 MiB, too long to send as one message",
            path.display()
        ));
    }
    std::fs::read(path).map_err(|e| format!("Couldn't read {}: {e}", path.display()))
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

/// Without a console, a startup failure or panic would otherwise vanish.
fn show_fatal(message: &str) {
    // on Linux the dialog needs zenity and a display; the terminal may be all there is
    eprintln!("hamzip-gui: {message}");
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("hamzip")
        .set_description(message)
        .show();
}

fn main() {
    if std::env::args().any(|a| a == "--version") {
        println!("hamzip-gui {}", hamzip::VERSION);
        return;
    }
    std::panic::set_hook(Box::new(|info| {
        show_fatal(&format!("hamzip stopped because of a bug: {info}"));
    }));
    net::clean_old_exe("hamzip-gui");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("hamzip")
            .with_inner_size([820.0, 640.0])
            .with_min_inner_size([520.0, 420.0])
            .with_icon(Arc::new(icon())),
        ..Default::default()
    };
    let run = eframe::run_native(
        "hamzip",
        options,
        Box::new(|cc| {
            style(&cc.egui_ctx);
            Ok(Box::new(App::new(cc.egui_ctx.clone())))
        }),
    );
    if let Err(e) = run {
        show_fatal(&format!(
            "hamzip couldn't open its window: {e}. It needs OpenGL 2 or newer, which some \
             remote desktop sessions don't provide; the hamzip command line still works."
        ));
        std::process::exit(1);
    }
}
