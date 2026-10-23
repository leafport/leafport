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
/// Wide enough for every main button label, so a spinner does not resize the button.
const MAIN_BUTTON_WIDTH: f32 = 64.0;

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
    /// Download progress of the running sync; None until its download reports one.
    pub progress: Option<f32>,
    /// Whether the running sync can still be cancelled, i.e. its Fulfill was not sent.
    pub cancellable: bool,
    pub queue: &'a VecDeque<Job>,
    pub returning: &'a HashSet<String>,
    pub row_errors: &'a HashMap<String, String>,
}

impl Busy<'_> {
    /// True while the row's own sync runs.
    fn syncing(&self, key: &str) -> bool {
        matches!(self.running, Some(Job::Sync(k)) if k == key)
    }

    /// True while the row's sync or loan return runs.
    fn working(&self, key: &str) -> bool {
        matches!(self.running, Some(Job::Sync(k)) if k == key) || self.returning.contains(key)
    }

    fn queued(&self, key: &str) -> bool {
        self.queue
            .iter()
            .any(|job| matches!(job, Job::Sync(k) if k == key))
    }

    /// True while the row's sync or loan return runs or waits.
    fn busy(&self, key: &str) -> bool {
        self.working(key) || self.queued(key)
    }
}

/// A row button or menu entry that was clicked.
pub enum Action {
    /// Sync, Retry or Sync again the row with this key.
    Sync(String),
    /// Return the loan of the row with this key.
    Return(String),
    /// Open this publication in its default application.
    Open(PathBuf),
    /// Open this title folder in the file manager.
    ShowInFolder(PathBuf),
    /// Ask to remove the title folder of the row with this key.
    Remove(String),
    /// Cancel the queued or running sync of the row with this key.
    Cancel(String),
}

/// Open `path` with the platform's default application: a folder in the file
/// manager, a book in its reader.
pub fn open_path(path: &Path) -> std::io::Result<()> {
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

/// The running sync's main button: a disabled button in the progress color, filled to
/// `progress` once the download reports one, else with a segment sliding back and forth.
fn progress_button(ui: &mut egui::Ui, progress: Option<f32>) {
    // A blank label keeps the height of the labelled buttons.
    let button = egui::Button::new(" ").min_size(egui::vec2(MAIN_BUTTON_WIDTH, 0.0));
    let rect = ui.add_enabled(false, button).rect;
    let radius = ui.visuals().widgets.inactive.corner_radius;
    let fill = ui.visuals().selection.bg_fill;
    let painter = ui.painter();
    painter.rect_filled(rect, radius, fill.gamma_multiply(0.35));
    let bar = match progress {
        Some(fraction) => {
            let mut bar = rect;
            bar.set_width(rect.width() * fraction.clamp(0.0, 1.0));
            bar
        }
        None => {
            // A third of the width, moving back and forth once every two seconds.
            let width = rect.width() / 3.0;
            let phase = (ui.input(|i| i.time) * std::f64::consts::PI).sin() as f32 * 0.5 + 0.5;
            let left = rect.left() + (rect.width() - width) * phase;
            ui.ctx().request_repaint();
            egui::Rect::from_min_size(
                egui::pos2(left, rect.top()),
                egui::vec2(width, rect.height()),
            )
        }
    };
    painter.rect_filled(bar, radius, fill);
}

/// A cross for close buttons, painted because the default fonts have no "✕" glyph.
pub fn close_icon<'a>(ui: &egui::Ui) -> egui::Atom<'a> {
    let height = ui.text_style_height(&egui::TextStyle::Button);
    egui::Atom::paint(egui::vec2(height, height), |ui, args| {
        let rect = args.rect.shrink(args.rect.height() * 0.25);
        let stroke = egui::Stroke::new(1.5, args.fallback_text_color);
        ui.painter()
            .line_segment([rect.left_top(), rect.right_bottom()], stroke);
        ui.painter()
            .line_segment([rect.right_top(), rect.left_bottom()], stroke);
    })
}

/// Three vertical dots for the menu button, painted because the default fonts have no
/// "⋮" glyph.
fn more_icon<'a>(ui: &egui::Ui) -> egui::Atom<'a> {
    let height = ui.text_style_height(&egui::TextStyle::Button);
    egui::Atom::paint(egui::vec2(height * 0.5, height), |ui, args| {
        let radius = args.rect.height() / 12.0;
        let gap = args.rect.height() / 3.0;
        for i in -1..=1 {
            let center = args.rect.center() + egui::vec2(0.0, i as f32 * gap);
            ui.painter()
                .circle_filled(center, radius, args.fallback_text_color);
        }
    })
}

/// Draw the row's main button and its menu button, right to left. Returns the action
/// that was clicked.
fn action(ui: &mut egui::Ui, item: &Item, busy: &Busy<'_>) -> Option<Action> {
    let working = busy.busy(&item.key);
    let key = || item.key.clone();
    // A loan can be returned without a cloud record, so local loan folders offer it too.
    let on_loan = matches!(item.state, ItemState::OnLoan { .. })
        || item.metadata.as_ref().is_some_and(|m| m.loan.is_some());
    let resync = item.publication.is_some()
        && item.acsm.is_some()
        && !matches!(item.state, ItemState::Returned);
    let mut clicked = None;

    // A queued or running sync offers only Cancel, until the running sync's Fulfill is
    // sent; a loan return cannot be cancelled.
    let sync_pending = busy.syncing(&item.key) || busy.queued(&item.key);
    let cancellable = busy.queued(&item.key) || busy.cancellable;
    let has_entries = resync || on_loan || item.folder.is_some();
    ui.add_enabled_ui(sync_pending || (!working && has_entries), |ui| {
        let button = egui::Button::new(more_icon(ui)).frame_when_inactive(false);
        egui::containers::menu::MenuButton::from_button(button).ui(ui, |ui| {
            if sync_pending {
                if ui
                    .add_enabled(cancellable, egui::Button::new("Cancel sync"))
                    .clicked()
                {
                    clicked = Some(Action::Cancel(key()));
                }
                return;
            }
            if resync && ui.button("Sync again").clicked() {
                clicked = Some(Action::Sync(key()));
            }
            if on_loan && ui.button("Return loan").clicked() {
                clicked = Some(Action::Return(key()));
            }
            if let Some(folder) = &item.folder {
                if ui.button("Show in folder").clicked() {
                    clicked = Some(Action::ShowInFolder(folder.clone()));
                }
                if ui.button("Remove from disk…").clicked() {
                    clicked = Some(Action::Remove(key()));
                }
            }
        });
    });

    let (label, main) = match (&item.publication, &item.state) {
        (Some(path), _) => ("Open", Some(Action::Open(path.clone()))),
        (None, ItemState::Failed(_)) => ("Retry", item.acsm.as_ref().map(|_| Action::Sync(key()))),
        (None, _) => ("Sync", item.acsm.as_ref().map(|_| Action::Sync(key()))),
    };
    let button = if busy.syncing(&item.key) {
        // The active sync shows a progress bar in the button's place and size.
        progress_button(ui, busy.progress);
        return clicked;
    } else if working {
        // A waiting sync or a loan return shows a spinner in place of the label, so the
        // button keeps its position.
        let size = ui.text_style_height(&egui::TextStyle::Button);
        egui::Button::new(egui::Atom::paint(egui::Vec2::splat(size), |ui, args| {
            egui::Spinner::new()
                .color(args.fallback_text_color)
                .paint_at(ui, args.rect);
        }))
    } else {
        egui::Button::new(label)
    };
    let button = button.min_size(egui::vec2(MAIN_BUTTON_WIDTH, 0.0));
    if ui.add_enabled(!working && main.is_some(), button).clicked() {
        clicked = main;
    }
    clicked
}
