//! The eframe application: page layout, settings persistence and the repaint policy.
//!
//! eframe calls [`eframe::App::logic`] and then [`eframe::App::ui`] for every frame. egui
//! is an *immediate-mode* GUI: there are no widget objects to update; each frame the whole
//! window is described again from the application state, and a widget's return value
//! (e.g. `button(..).clicked()`) reports the interaction. Frames are only produced when
//! something happens (input, or a repaint request), so while the receiver runs we ask for
//! one about 30 times a second, while transmitting 10 times, and otherwise let the GUI
//! idle. Receiver and transmitter are independent and may run at the same time.

use crate::Args;
use crate::panels::plots::PlotTextures;
use crate::panels::source::{DeviceLists, SourceAction};
use crate::panels::tx_page::TxPage;
use crate::panels::{self};
use crate::receiver::RxSession;
use crate::settings::{Page, Settings, SettingsStore, SourceKind, ThemeChoice};
use crate::transmitter::TxSession;
use cofdmtv_engine::{InputSpec, RxConfig};
use eframe::egui::{self, RichText, Ui};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Unattended runs (`--exit-after`, `--screenshot`): for documentation screenshots and
/// smoke tests.
#[derive(Debug, Default)]
struct Automation {
    quit_at: Option<Instant>,
    screenshot: Option<PathBuf>,
    requested_at: Option<Instant>,
}

impl Automation {
    fn active(&self) -> bool {
        self.quit_at.is_some()
    }

    fn tick(&mut self, ctx: &egui::Context, now: Instant, log: &mut crate::receiver::LogBuffer) {
        let Some(quit_at) = self.quit_at else { return };
        if now < quit_at {
            return;
        }
        match (&self.screenshot, self.requested_at) {
            (Some(_), None) => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
                self.requested_at = Some(now);
            }
            (Some(_), Some(t)) if now.duration_since(t) < Duration::from_secs(3) => {}
            (Some(_), Some(_)) => {
                log.push("screenshot not delivered; quitting");
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            (None, _) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }

    /// Save a delivered screenshot and quit.
    fn handle_screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.screenshot.clone() else { return };
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        let Some(image) = image else { return };
        let rgba: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
        let (w, h) = (image.size[0] as u32, image.size[1] as u32);
        match image::save_buffer(&path, &rgba, w, h, image::ColorType::Rgba8) {
            Ok(()) => eprintln!("screenshot saved to {}", path.display()),
            Err(e) => eprintln!("cannot save screenshot {}: {e}", path.display()),
        }
        self.screenshot = None;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

pub struct CofdmtvApp {
    settings: Settings,
    store: SettingsStore,
    rx: RxSession,
    tx: TxSession,
    tx_page: TxPage,
    devices: DeviceLists,
    textures: PlotTextures,
    automation: Automation,
    /// No sound-card output in this run (`--no-audio`).
    mute: bool,
    applied_theme: Option<ThemeChoice>,
    fonts: crate::fonts::FontFallbacks,
    last_save: Instant,
    /// Transient message under the source bar (e.g. why Start did nothing).
    notice: Option<String>,
    /// `--transmit`: send once the transmitter page is ready.
    pending_transmit: bool,
}

const PICTURE_EXTENSIONS: [&str; 8] = ["jpg", "jpeg", "png", "webp", "bmp", "gif", "tif", "tiff"];

fn has_extension(path: &Path, list: &[&str]) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| list.iter().any(|l| e.eq_ignore_ascii_case(l)))
}

impl CofdmtvApp {
    pub fn new(cc: &eframe::CreationContext<'_>, args: Args) -> Self {
        let (store, mut settings, warning) = SettingsStore::load(args.config.clone());
        let mut rx = RxSession::default();
        if let Some(w) = warning {
            rx.log.push(w);
        }
        if let Some(path) = store.path() {
            rx.log.push(format!("settings: {}", path.display()));
        }
        let mut tx_page = TxPage::new(&mut settings);
        if let Some(file) = &args.file {
            if has_extension(file, &PICTURE_EXTENSIONS) {
                tx_page.open_picture(&mut settings, file);
                settings.page = Page::Transmitter;
            } else {
                settings.file = Some(file.clone());
                settings.source = SourceKind::File;
                settings.page = Page::Receiver;
            }
        }
        if let Some(page) = args.page {
            settings.page = page;
        }
        cc.egui_ctx.set_theme(settings.theme.preference());
        let now = Instant::now();
        let exit_after = args.exit_after.or(args.screenshot.as_ref().map(|_| 10.0));
        let mut app = Self {
            applied_theme: Some(settings.theme),
            settings,
            store,
            rx,
            tx: TxSession::default(),
            tx_page,
            devices: DeviceLists::default(),
            textures: PlotTextures::default(),
            automation: Automation {
                quit_at: exit_after.map(|s| now + Duration::from_secs_f64(s.clamp(0.0, 86_400.0))),
                screenshot: args.screenshot.clone(),
                requested_at: None,
            },
            mute: args.no_audio,
            fonts: crate::fonts::FontFallbacks::default(),
            last_save: now,
            notice: None,
            pending_transmit: args.transmit,
        };
        if args.start {
            app.start();
        }
        app
    }

    fn start(&mut self) {
        let input = match self.settings.source {
            SourceKind::File => match &self.settings.file {
                Some(path) => InputSpec::File { path: path.clone(), realtime: self.settings.realtime },
                None => {
                    self.notice = Some("Open a recording first.".into());
                    return;
                }
            },
            SourceKind::Device => InputSpec::Device { name: self.settings.input_device.clone() },
        };
        let label = match &input {
            InputSpec::File { path, .. } => path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned()),
            InputSpec::Device { name } => name.clone().unwrap_or_else(|| "default input".into()),
        };
        self.notice = None;
        let cfg = RxConfig { input, channel: self.settings.channel.sel(), save_dir: self.settings.save_dir.clone() };
        self.rx.start(cfg, label);
    }

    fn top_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("COFDMtv").strong().size(16.0));
            ui.separator();
            // A marker for a page whose engine is running (both may run at once). "▶" is in
            // egui's default fonts; "●" is not.
            let running = |on: bool| if on { " ▶" } else { "" };
            let rx_label = format!("Receiver{}", running(self.rx.is_running()));
            let tx_label = format!("Transmitter{}", running(self.tx.is_running()));
            ui.selectable_value(&mut self.settings.page, Page::Receiver, rx_label);
            ui.selectable_value(&mut self.settings.page, Page::Transmitter, tx_label);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::ComboBox::from_id_salt("theme").selected_text(self.settings.theme.label()).show_ui(ui, |ui| {
                    for t in ThemeChoice::ALL {
                        ui.selectable_value(&mut self.settings.theme, t, t.label());
                    }
                });
                if self.settings.page == Page::Receiver {
                    ui.toggle_value(&mut self.settings.show_log, "Log");
                }
            });
        });
    }

    fn receiver_page(&mut self, ui: &mut Ui) {
        egui::Panel::top("source_bar").show(ui, |ui| {
            let action = panels::source::show(ui, &mut self.settings, &mut self.devices, self.rx.is_running(), self.rx.is_stopping());
            if let Some(n) = &self.notice {
                ui.colored_label(ui.visuals().warn_fg_color, n);
            }
            match action {
                Some(SourceAction::Start) => self.start(),
                Some(SourceAction::Stop) => self.rx.stop(),
                None => {}
            }
        });
        egui::Panel::top("status_strip").show(ui, |ui| panels::status_strip::show(ui, &self.rx));
        if self.settings.show_log {
            egui::Panel::bottom("log").resizable(true).default_size(150.0).min_size(60.0).show(ui, |ui| panels::log::show(ui, &mut self.rx.log));
        }
        egui::Panel::right("side")
            .resizable(true)
            .default_size(420.0)
            .min_size(300.0)
            .show(ui, |ui| panels::received::show(ui, &mut self.rx, &mut self.settings.side_tab));
        egui::CentralPanel::default().show(ui, |ui| {
            panels::plots::show(ui, &mut self.settings.plot_tab, &mut self.settings.span, &self.rx, &mut self.textures);
        });
    }

    /// Files dropped on the window: recordings go to the receiver, pictures to the
    /// transmitter.
    fn dropped_files(&mut self, ctx: &egui::Context) {
        let files: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        for path in files {
            if has_extension(&path, &PICTURE_EXTENSIONS) {
                self.tx_page.open_picture(&mut self.settings, &path);
                self.settings.page = Page::Transmitter;
            } else if has_extension(&path, &["wav", "flac"]) {
                if self.rx.is_running() {
                    self.notice = Some("Stop the receiver to open another recording.".into());
                } else {
                    self.settings.file = Some(path);
                    self.settings.source = SourceKind::File;
                    self.settings.page = Page::Receiver;
                }
            }
        }
    }
}

impl eframe::App for CofdmtvApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = Instant::now();
        let span = self.settings.span;
        self.rx.poll(|rate, iq| span.range(rate, iq));
        self.tx.poll();
        self.dropped_files(ctx);
        self.tx_page.update(ctx, &self.settings);
        if self.pending_transmit && !self.tx_page.busy(&self.settings) {
            self.pending_transmit = false;
            match self.tx_page.problem(&self.settings, !self.mute) {
                None => self.tx_page.transmit(&mut self.settings, &mut self.tx, !self.mute),
                Some(p) => self.rx.log.push(format!("cannot transmit: {p}")),
            }
        }

        // Fonts for non-Latin scripts in what was received or is being written.
        self.fonts.note(&std::mem::take(&mut self.rx.text_seen));
        if self.settings.page == Page::Transmitter {
            self.fonts.note(self.tx_page.text(&self.settings));
        }
        if self.fonts.apply(ctx) {
            ctx.request_repaint();
        }
        for m in self.fonts.messages.drain(..) {
            self.rx.log.push(m);
        }
        if self.applied_theme != Some(self.settings.theme) {
            ctx.set_theme(self.settings.theme.preference());
            self.applied_theme = Some(self.settings.theme);
        }
        // Save changed settings at most once per second.
        if now.duration_since(self.last_save) >= Duration::from_secs(1) && self.store.is_dirty(&self.settings) {
            if let Err(e) = self.store.save_if_changed(&self.settings) {
                self.rx.log.push(format!("cannot save settings: {e}"));
            }
            self.last_save = now;
        }
        self.automation.tick(ctx, now, &mut self.rx.log);

        // Repaint policy: ~30 Hz while the receiver runs (the waterfall moves 11 rows a
        // second, the meters faster), 10 Hz while transmitting or an unattended run waits;
        // otherwise only on input.
        if self.rx.is_running() {
            ctx.request_repaint_after(Duration::from_millis(33));
        } else if self.tx.is_running() || self.automation.active() || self.pending_transmit {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.automation.handle_screenshot(ui.ctx());
        egui::Panel::top("top_bar").show(ui, |ui| self.top_bar(ui));
        match self.settings.page {
            Page::Receiver => self.receiver_page(ui),
            Page::Transmitter => self.tx_page.show(ui, &mut self.settings, &mut self.devices, &mut self.tx, !self.mute),
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Err(e) = self.store.save_if_changed(&self.settings) {
            eprintln!("cannot save settings: {e}");
        }
        self.rx.stop_now();
        self.tx.stop_now();
    }
}
