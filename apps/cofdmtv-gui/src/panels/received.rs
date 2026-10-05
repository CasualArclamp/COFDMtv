//! The side panel: the latest (or chosen) picture large with what is known about it, a
//! multi-frame picture in the making, and the gallery of pictures or the list of
//! messages and pings.

use super::{Palette, heading, placeholder};
use crate::receiver::{Message, RxSession};
use crate::settings::SideTab;
use eframe::egui::{self, RichText, Sense, Ui, vec2};

pub fn show(ui: &mut Ui, rx: &mut RxSession, tab: &mut SideTab) {
    let pal = Palette::for_ui(ui);
    heading(ui, "Received");
    ui.add_space(2.0);
    let ctx = ui.ctx().clone();
    let shown = rx.selected.filter(|&i| i < rx.pictures.len()).or(rx.pictures.len().checked_sub(1));
    match shown {
        None => {
            placeholder(ui, "No picture received yet. Pictures from Shredpix (or COFDMtv) appear here, and are saved if \"Save to\" is on.");
        }
        Some(i) => {
            let max_h = (ui.available_height() * 0.5).max(160.0);
            let width = ui.available_width();
            let pic = &mut rx.pictures[i];
            match pic.texture(&ctx, i) {
                Ok(tex) => {
                    let size = tex.size_vec2();
                    let scale = (width / size.x).min(max_h / size.y).min(4.0);
                    ui.vertical_centered(|ui| {
                        ui.add(egui::Image::new(tex).fit_to_exact_size(size * scale).corner_radius(3.0));
                    });
                }
                Err(e) => {
                    ui.add(egui::Label::new(RichText::new(e).italics().color(pal.error)).wrap());
                }
            }
            let p = &pic.picture;
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(&p.call).strong().size(15.0));
                ui.label(RichText::new(p.time.format("%Y-%m-%d %H:%M:%S").to_string()).weak());
                if rx.selected.is_some() && ui.small_button("Latest").on_hover_text("Show the newest picture").clicked() {
                    rx.selected = None;
                }
            });
            let mut facts = vec![p.kind.map_or("unknown data".to_string(), |k| k.name().to_string())];
            if let Some((w, h)) = pic.size {
                facts.push(format!("{w}×{h}"));
            }
            facts.push(format!("{} bytes", p.data.len()));
            if p.frames > 1 {
                facts.push(format!("{} frames", p.frames));
            }
            facts.push(format!("mode {}", p.mode));
            ui.label(RichText::new(facts.join(" · ")).weak());
            ui.label(RichText::new(format!("SNR {:.1} dB · {} bits corrected · carrier {:.0} Hz", p.snr_db, p.flips, p.cfo_hz)).weak().small());
            if let Some(path) = &p.saved {
                ui.horizontal(|ui| {
                    if ui.small_button("Open").on_hover_text(path.display().to_string()).clicked() {
                        let _ = open::that_detached(path);
                    }
                    if let Some(dir) = path.parent()
                        && ui.small_button("Folder").on_hover_text(dir.display().to_string()).clicked()
                    {
                        let _ = open::that_detached(dir);
                    }
                    let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                    ui.add(egui::Label::new(RichText::new(name).monospace().small().weak()).truncate());
                });
            }
        }
    }
    if let Some(m) = &rx.multiframe {
        ui.add_space(4.0);
        egui::Frame::group(ui.style()).corner_radius(egui::CornerRadius::same(4)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(format!("Multi-frame picture from {}", m.call)).strong());
            ui.add(
                egui::ProgressBar::new(m.have as f32 / m.need.max(1) as f32)
                    .desired_width(ui.available_width())
                    .text(format!("{} of {} frames · {} bytes", m.have, m.need, m.size)),
            );
        });
    }
    ui.add_space(6.0);
    ui.separator();
    let texts = rx.messages.len();
    ui.horizontal(|ui| {
        ui.selectable_value(tab, SideTab::Pictures, format!("Pictures ({})", rx.pictures.len()));
        ui.selectable_value(tab, SideTab::Messages, format!("Messages ({texts})"));
    });
    match tab {
        SideTab::Pictures => gallery(ui, rx, &ctx),
        SideTab::Messages => messages(ui, rx, &pal),
    }
}

/// Thumbnails, newest first; a click shows a picture large.
fn gallery(ui: &mut Ui, rx: &mut RxSession, ctx: &egui::Context) {
    if rx.pictures.is_empty() {
        placeholder(ui, "Pictures received in this session.");
        return;
    }
    let thumb = 84.0;
    let shown = rx.selected.or(rx.pictures.len().checked_sub(1));
    let mut clicked = None;
    egui::ScrollArea::vertical().id_salt("gallery").auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            for i in (0..rx.pictures.len()).rev() {
                let pic = &mut rx.pictures[i];
                let (rect, resp) = ui.allocate_exact_size(vec2(thumb, thumb), Sense::click());
                let selected = shown == Some(i);
                let visuals = ui.visuals();
                ui.painter().rect_filled(rect, 3.0, visuals.extreme_bg_color);
                match pic.texture(ctx, i) {
                    Ok(tex) => {
                        let size = tex.size_vec2();
                        let scale = (thumb - 6.0) / size.x.max(size.y);
                        let r = egui::Rect::from_center_size(rect.center(), size * scale);
                        egui::Image::new(tex).corner_radius(2.0).paint_at(ui, r);
                    }
                    Err(_) => {
                        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "?", egui::FontId::proportional(20.0), visuals.weak_text_color());
                    }
                }
                if selected {
                    ui.painter().rect_stroke(rect, 3.0, egui::Stroke::new(2.0, visuals.selection.stroke.color), egui::StrokeKind::Inside);
                }
                let p = &pic.picture;
                if resp.on_hover_text(format!("{} · {}", p.call, p.time.format("%H:%M:%S"))).clicked() {
                    clicked = Some(i);
                }
            }
        });
    });
    if let Some(i) = clicked {
        rx.selected = if Some(i) == rx.pictures.len().checked_sub(1) { None } else { Some(i) };
    }
}

/// Texts and pings, newest at the bottom.
fn messages(ui: &mut Ui, rx: &RxSession, pal: &Palette) {
    if rx.messages.is_empty() {
        placeholder(ui, "Text messages (Rattlegram) and pings received in this session.");
        return;
    }
    egui::ScrollArea::vertical().id_salt("messages").auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
        for m in &rx.messages {
            match m {
                Message::Text(t) => {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(t.time.format("%H:%M:%S").to_string()).monospace().weak());
                        ui.label(RichText::new(&t.call).strong().color(pal.text));
                        ui.label(RichText::new(format!("{:.0} dB", t.snr_db)).weak().small());
                    });
                    ui.add(egui::Label::new(RichText::new(&t.text).size(14.0)).wrap().selectable(true));
                    ui.add_space(4.0);
                }
                Message::Ping { time, call, cfo_hz } => {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(time.format("%H:%M:%S").to_string()).monospace().weak());
                        ui.label(RichText::new(call).strong());
                        ui.label(RichText::new(format!("ping at {cfo_hz:.0} Hz")).italics().weak());
                    });
                    ui.add_space(2.0);
                }
            }
        }
    });
}
