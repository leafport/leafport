//! The title list: one fixed-height row per item, with cover thumbnails.

use eframe::egui;
use leafport_core::library::{Item, ItemState};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::mpsc::Sender,
};

use crate::jobs::{self, Event, Job};

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

    /// Drop the textures of covers inside `folder`, so a replaced cover is read again.
    pub fn forget(&mut self, folder: &Path) {
        self.textures.retain(|path, _| !path.starts_with(folder));
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

/// Which rows are queued, syncing or returning, and the last return error per row.
pub struct Busy<'a> {
    pub running: Option<&'a Job>,
    pub queue: &'a VecDeque<Job>,
    pub returning: &'a HashSet<String>,
    pub row_errors: &'a HashMap<String, String>,
}

impl Busy<'_> {
    /// True while the row's sync or loan return runs.
    fn working(&self, key: &str) -> bool {
        matches!(self.running, Some(Job::Sync(k)) if k == key) || self.returning.contains(key)
    }

    fn queued(&self, key: &str) -> bool {
        self.queue
            .iter()
            .any(|job| matches!(job, Job::Sync(k) if k == key))
    }
}

/// A row button that was clicked.
pub enum Action {
    /// Sync or Retry the row with this key.
    Sync(String),
    /// Return the loan of the row with this key.
    Return(String),
    /// Open this title folder in the file manager.
    ShowInFolder(PathBuf),
}

/// Open `path` in the platform file manager.
pub fn show_in_folder(path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let program = "xdg-open";
    let mut child = std::process::Command::new(program).arg(path).spawn()?;
    // Reap the launcher when it exits, so it leaves no zombie process.
    std::thread::spawn(move || child.wait());
    Ok(())
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

/// Draw the visible rows. Returns the row button that was clicked, if any.
pub fn show(
    ui: &mut egui::Ui,
    rows: &[Item],
    busy: Busy<'_>,
    covers: &mut Covers,
    tx: &Sender<Event>,
) -> Option<Action> {
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
                            if let Some(action) = action(ui, item, &busy) {
                                clicked = Some(action);
                            }
                            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                                text(ui, item, busy.row_errors.get(&item.key));
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

fn text(ui: &mut egui::Ui, item: &Item, row_error: Option<&String>) {
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
    let error = match &item.state {
        ItemState::Failed(message) => Some(message),
        _ => row_error,
    };
    if let Some(message) = error {
        ui.add(
            egui::Label::new(egui::RichText::new(message).color(ui.visuals().error_fg_color))
                .truncate(),
        );
    } else if let Some(warning) = &item.warning {
        ui.add(egui::Label::new(egui::RichText::new(warning).weak()).truncate());
    }
}

/// Draw the row's action. Returns the action when its button was clicked.
fn action(ui: &mut egui::Ui, item: &Item, busy: &Busy<'_>) -> Option<Action> {
    if busy.working(&item.key) {
        ui.spinner();
        return None;
    }
    if busy.queued(&item.key) {
        ui.weak("Queued");
        return None;
    }
    let key = || item.key.clone();
    // A loan can be returned without a cloud record, so local loan folders offer it too.
    let on_loan = item.metadata.as_ref().is_some_and(|m| m.loan.is_some());
    match item.state {
        ItemState::Available => ui.button("Sync").clicked().then(|| Action::Sync(key())),
        ItemState::Failed(_) => ui.button("Retry").clicked().then(|| Action::Sync(key())),
        ItemState::OnLoan { .. } => ui.button("Return").clicked().then(|| Action::Return(key())),
        ItemState::Local if on_loan => ui.button("Return").clicked().then(|| Action::Return(key())),
        ItemState::Synced | ItemState::Returned | ItemState::Local => {
            let folder = item.folder.as_ref();
            ui.add_enabled(folder.is_some(), egui::Button::new("Show in folder"))
                .clicked()
                .then(|| folder.cloned().map(Action::ShowInFolder))
                .flatten()
        }
    }
}
