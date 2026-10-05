//! Source bar of the receiver: sound card or recording, the channel carrying the signal,
//! where received pictures are saved, and Start / Stop.

use super::enabled;
use crate::settings::{RxChannel, Settings, SourceKind};
use eframe::egui::{self, ComboBox, RichText, Ui};
use std::path::Path;

/// What the user asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceAction {
    Start,
    Stop,
}

/// Sound-card names, enumerated on first use (WASAPI/ALSA enumeration takes a moment,
/// so it is not done every frame) and on "refresh".
#[derive(Debug, Default)]
pub struct DeviceLists {
    inputs: Option<Vec<String>>,
    outputs: Option<Vec<String>>,
    pub error: Option<String>,
}

impl DeviceLists {
    pub fn inputs(&mut self) -> &[String] {
        if self.inputs.is_none() {
            self.inputs = Some(names(cofdmtv_engine::cofdmtv_io::list_input_devices(), &mut self.error));
        }
        self.inputs.as_deref().unwrap_or_default()
    }

    pub fn outputs(&mut self) -> &[String] {
        if self.outputs.is_none() {
            self.outputs = Some(names(cofdmtv_engine::cofdmtv_io::list_output_devices(), &mut self.error));
        }
        self.outputs.as_deref().unwrap_or_default()
    }

    pub fn refresh(&mut self) {
        *self = Self::default();
    }
}

fn names(list: cofdmtv_engine::cofdmtv_io::Result<Vec<cofdmtv_engine::cofdmtv_io::DeviceInfo>>, error: &mut Option<String>) -> Vec<String> {
    match list {
        Ok(devices) => devices.into_iter().map(|d| d.name).collect(),
        Err(e) => {
            *error = Some(format!("sound-card enumeration failed: {e}"));
            Vec::new()
        }
    }
}

/// A device picker: "System default" or one of `names`.
pub fn device_combo(ui: &mut Ui, id: &str, current: &mut Option<String>, names: &[String], default_label: &str, width: f32) {
    let text = current.clone().unwrap_or_else(|| default_label.to_string());
    ComboBox::from_id_salt(id).width(width).selected_text(text).show_ui(ui, |ui| {
        ui.selectable_value(current, None, default_label);
        for name in names {
            ui.selectable_value(current, Some(name.clone()), name);
        }
    });
}

/// Draw the bar. The source is locked while the receiver runs.
pub fn show(ui: &mut Ui, settings: &mut Settings, devices: &mut DeviceLists, running: bool, stopping: bool) -> Option<SourceAction> {
    let mut action = None;
    let free = !running;
    ui.horizontal_wrapped(|ui| {
        enabled(ui, free, |ui| {
            ui.selectable_value(&mut settings.source, SourceKind::Device, "Sound card");
            ui.selectable_value(&mut settings.source, SourceKind::File, "Recording");
            ui.separator();
            match settings.source {
                SourceKind::Device => {
                    let names = devices.inputs().to_vec();
                    device_combo(ui, "input_device", &mut settings.input_device, &names, "System default", 260.0);
                    if ui.small_button("⟳").on_hover_text("Refresh the device list").clicked() {
                        devices.refresh();
                    }
                }
                SourceKind::File => file_picker(ui, settings),
            }
            ui.separator();
            ComboBox::from_id_salt("rx_channel")
                .selected_text(settings.channel.label())
                .show_ui(ui, |ui| {
                    for c in RxChannel::ALL {
                        ui.selectable_value(&mut settings.channel, c, c.label());
                    }
                })
                .response
                .on_hover_text("Where the signal is: the mean of the channels (also for mono), one of them, or I/Q (Shredpix's analytic output)");
            if settings.source == SourceKind::File {
                ui.checkbox(&mut settings.realtime, "Real time")
                    .on_hover_text("Pace the recording to real time instead of decoding it as fast as possible.");
            }
        });
        ui.separator();
        if running {
            if ui.add_enabled(!stopping, egui::Button::new("■ Stop")).clicked() {
                action = Some(SourceAction::Stop);
            }
        } else if ui.button(RichText::new("▶ Start").strong()).clicked() {
            action = Some(SourceAction::Start);
        }
        ui.separator();
        save_picker(ui, settings, free);
    });
    if let Some(e) = &devices.error {
        ui.colored_label(ui.visuals().warn_fg_color, e);
    }
    action
}

fn file_picker(ui: &mut Ui, settings: &mut Settings) {
    let name = settings
        .file
        .as_deref()
        .and_then(Path::file_name)
        .map_or_else(|| "no recording selected".to_string(), |n| n.to_string_lossy().into_owned());
    let label = ui.add(egui::Label::new(RichText::new(name).monospace()).truncate());
    if let Some(path) = &settings.file {
        label.on_hover_text(path.display().to_string());
    }
    if ui.button("Open…").clicked() {
        let mut dialog = rfd::FileDialog::new()
            .set_title("Open a recording")
            .add_filter("Recordings (WAV, FLAC)", &["wav", "flac"])
            .add_filter("All files", &["*"]);
        if let Some(dir) = settings.file.as_deref().and_then(Path::parent).filter(|d| d.is_dir()) {
            dialog = dialog.set_directory(dir);
        }
        // Rust note: `pick_file` blocks this (UI) thread while the native dialog is open;
        // the engine keeps running on its own thread meanwhile.
        if let Some(path) = dialog.pick_file() {
            settings.file = Some(path);
        }
    }
}

/// Where received pictures go: a folder, or nowhere.
fn save_picker(ui: &mut Ui, settings: &mut Settings, free: bool) {
    let mut save = settings.save_dir.is_some();
    let toggled = enabled(ui, free, |ui| {
        ui.checkbox(&mut save, "Save to").on_hover_text("Save received pictures (named like Assempix does: date_time_CALL) and a log of the messages").changed()
    });
    if toggled {
        settings.save_dir = if save { crate::settings::default_save_dir() } else { None };
    }
    if let Some(dir) = settings.save_dir.clone() {
        let short = dir.file_name().map_or_else(|| dir.display().to_string(), |n| n.to_string_lossy().into_owned());
        let r = enabled(ui, free, |ui| ui.button(format!("{short}…")));
        if r.on_hover_text(format!("{} — click to choose another folder", dir.display())).clicked()
            && let Some(d) = rfd::FileDialog::new().set_title("Save received pictures in").set_directory(&dir).pick_folder()
        {
            settings.save_dir = Some(d);
        }
        if ui.small_button("Open").on_hover_text("Open the folder").clicked() {
            let _ = std::fs::create_dir_all(&dir);
            let _ = open::that_detached(&dir);
        }
    }
}
