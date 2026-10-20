//! The title list: one fixed-height row per item, with cover thumbnails.

use eframe::egui;
use leafport_core::library::{Item, ItemState};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::mpsc::Sender,
};

use crate::jobs::{self, Event};

pub const ROW_HEIGHT: f32 = 72.0;
const COVER: egui::Vec2 = egui::vec2(48.0, 64.0);

/// Cover textures by file path. A path is requested once; None means pending or unreadable.
#[derive(Default)]
pub struct Covers {
    textures: HashMap<PathBuf, Option<egui::TextureHandle>>,
}

impl Covers {
    pub fn insert(&mut self, ctx: &egui::Context, path: PathBuf, image: Option<egui::ColorImage>) {
        let texture = image.map(|image| {
            ctx.load_texture(
                path.display().to_string(),
                image,
                egui::TextureOptions::LINEAR,
            )
        });
        self.textures.insert(path, texture);
    }

    fn get(
        &mut self,
        path: &Path,
        tx: &Sender<Event>,
        ctx: &egui::Context,
    ) -> Option<&egui::TextureHandle> {
        if !self.textures.contains_key(path) {
            self.textures.insert(path.to_path_buf(), None);
            jobs::cover(tx, ctx, path.to_path_buf());
        }
        self.textures.get(path).and_then(Option::as_ref)
    }
}

/// Which rows are queued or syncing.
pub struct Busy<'a> {
    pub syncing: &'a HashSet<String>,
    pub queue: &'a VecDeque<String>,
}

fn state_label(state: &ItemState) -> &'static str {
    match state {
        ItemState::Available => "Available",
        ItemState::Synced => "Synced",
        ItemState::OnLoan { .. } => "On loan",
        ItemState::Returned => "Returned",
        ItemState::Local => "Local",
        ItemState::Failed(_) => "Failed",
    }
}

/// Draw the visible rows. Returns the key of a row whose Sync or Retry was clicked.
pub fn show(
    ui: &mut egui::Ui,
    rows: &[Item],
    busy: Busy<'_>,
    covers: &mut Covers,
    tx: &Sender<Event>,
) -> Option<String> {
    let mut clicked = None;
    let ctx = ui.ctx().clone();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_rows(ui, ROW_HEIGHT, rows.len(), |ui, range| {
            for item in &rows[range] {
                let size = egui::vec2(ui.available_width(), ROW_HEIGHT);
                ui.allocate_ui_with_layout(
                    size,
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.set_height(ROW_HEIGHT);
                        let texture = item
                            .cover
                            .as_deref()
                            .and_then(|path| covers.get(path, tx, &ctx));
                        cover(ui, texture, item.format.label());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if action(ui, item, &busy) {
                                clicked = Some(item.key.clone());
                            }
                            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                                text(ui, item);
                            });
                        });
                    },
                );
            }
        });
    clicked
}

fn cover(ui: &mut egui::Ui, texture: Option<&egui::TextureHandle>, format: &str) {
    let (rect, _) = ui.allocate_exact_size(COVER, egui::Sense::hover());
    if let Some(texture) = texture {
        let fitted = egui::Rect::from_center_size(
            rect.center(),
            texture.size_vec2() * (COVER / texture.size_vec2()).min_elem(),
        );
        egui::Image::new(texture).paint_at(ui, fitted);
    } else {
        let visuals = ui.visuals();
        ui.painter().rect_stroke(
            rect,
            2.0,
            visuals.widgets.noninteractive.bg_stroke,
            egui::StrokeKind::Inside,
        );
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            format,
            egui::TextStyle::Small.resolve(ui.style()),
            visuals.weak_text_color(),
        );
    }
}

fn text(ui: &mut egui::Ui, item: &Item) {
    ui.add(egui::Label::new(egui::RichText::new(&item.title).strong()).truncate());
    let mut info = format!("{} · {}", item.format.label(), state_label(&item.state));
    if let ItemState::OnLoan {
        expires: Some(expires),
    } = &item.state
    {
        info.push_str(&format!(
            " · until {}",
            expires.get(..10).unwrap_or(expires)
        ));
    }
    ui.add(egui::Label::new(info).truncate());
    if let ItemState::Failed(message) = &item.state {
        ui.add(
            egui::Label::new(egui::RichText::new(message).color(ui.visuals().error_fg_color))
                .truncate(),
        );
    } else if let Some(warning) = &item.warning {
        ui.add(egui::Label::new(egui::RichText::new(warning).weak()).truncate());
    }
}

/// Draw the row's action. Returns true when Sync or Retry was clicked.
fn action(ui: &mut egui::Ui, item: &Item, busy: &Busy<'_>) -> bool {
    if busy.syncing.contains(&item.key) {
        ui.spinner();
        return false;
    }
    if busy.queue.contains(&item.key) {
        ui.weak("Queued");
        return false;
    }
    match item.state {
        ItemState::Available => ui.button("Sync").clicked(),
        ItemState::Failed(_) => ui.button("Retry").clicked(),
        // Return and Show in folder are wired in a later change.
        ItemState::OnLoan { .. } => {
            ui.add_enabled(false, egui::Button::new("Return"));
            false
        }
        ItemState::Synced | ItemState::Returned | ItemState::Local => {
            ui.add_enabled(false, egui::Button::new("Show in folder"));
            false
        }
    }
}
