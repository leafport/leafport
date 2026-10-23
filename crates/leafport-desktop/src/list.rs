//! The title list: one fixed-height row per item, with cover thumbnails.

use eframe::egui;
use leafport_app::{Activity, MenuAction, Primary, Row, RowState};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
};

pub const ROW_HEIGHT: f32 = 72.0;
const COVER: egui::Vec2 = egui::vec2(48.0, 64.0);
/// Wide enough for every main button label, so a spinner does not resize the button.
const MAIN_BUTTON_WIDTH: f32 = 64.0;

/// A decoded cover thumbnail, or None when it could not be read.
type Decoded = (PathBuf, Option<egui::ColorImage>);

/// Cover textures by file path. A path is requested once; None means pending or unreadable.
pub struct Covers {
    textures: HashMap<PathBuf, Option<egui::TextureHandle>>,
    tx: Sender<Decoded>,
    rx: Receiver<Decoded>,
}

impl Default for Covers {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            textures: HashMap::new(),
            tx,
            rx,
        }
    }
}

impl Covers {
    /// Turn the decoded covers that arrived into textures.
    pub fn receive(&mut self, ctx: &egui::Context) {
        while let Ok((path, image)) = self.rx.try_recv() {
            let texture = image.map(|image| {
                ctx.load_texture(
                    path.display().to_string(),
                    image,
                    egui::TextureOptions::LINEAR,
                )
            });
            self.textures.insert(path, texture);
        }
    }

    /// Drop the textures of covers inside `folder`, so a replaced cover is read again.
    pub fn forget(&mut self, folder: &Path) {
        self.textures.retain(|path, _| !path.starts_with(folder));
    }

    /// Drop every texture, so each cover is read again.
    pub fn clear(&mut self) {
        self.textures.clear();
    }

    fn get(&mut self, path: &Path, ctx: &egui::Context) -> Option<&egui::TextureHandle> {
        if !self.textures.contains_key(path) {
            self.textures.insert(path.to_path_buf(), None);
            self.load(path.to_path_buf(), ctx);
        }
        self.textures.get(path).and_then(Option::as_ref)
    }

    /// Decode a cover file on a worker thread and scale it to fit 96×128.
    fn load(&self, path: PathBuf, ctx: &egui::Context) {
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let decode = || -> Result<egui::ColorImage, String> {
                let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
                let rgba = image::load_from_memory(&bytes)
                    .map_err(|e| e.to_string())?
                    .thumbnail(96, 128)
                    .to_rgba8();
                let size = [rgba.width() as usize, rgba.height() as usize];
                Ok(egui::ColorImage::from_rgba_unmultiplied(size, &rgba))
            };
            // A panicking decoder counts as unreadable, like a failed decode.
            let image = std::panic::catch_unwind(std::panic::AssertUnwindSafe(decode))
                .ok()
                .and_then(Result::ok);
            let _ = tx.send((path, image));
            ctx.request_repaint();
        });
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

fn state_label(state: &RowState) -> &'static str {
    match state {
        RowState::Available => "Available",
        RowState::Synced => "Synced",
        RowState::OnLoan => "On loan",
        RowState::Returned => "Returned",
        RowState::Local => "Local",
        RowState::Failed => "Failed",
    }
}

/// Draw the visible rows. `progress` holds the download fraction of syncing rows.
/// Returns the row button that was clicked, if any.
pub fn show(
    ui: &mut egui::Ui,
    rows: &[Row],
    progress: &HashMap<String, f32>,
    covers: &mut Covers,
) -> Option<Action> {
    let mut clicked = None;
    let ctx = ui.ctx().clone();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_rows(ui, ROW_HEIGHT, rows.len(), |ui, range| {
            for row in &rows[range] {
                let size = egui::vec2(ui.available_width(), ROW_HEIGHT);
                ui.allocate_ui_with_layout(
                    size,
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.set_height(ROW_HEIGHT);
                        let texture = row
                            .cover_path
                            .as_deref()
                            .and_then(|path| covers.get(Path::new(path), &ctx));
                        cover(ui, texture, &row.format);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if let Some(action) = action(ui, row, progress.get(&row.key).copied()) {
                                clicked = Some(action);
                            }
                            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                                text(ui, row);
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

fn text(ui: &mut egui::Ui, row: &Row) {
    ui.add(egui::Label::new(egui::RichText::new(&row.title).strong()).truncate());
    let mut info = format!("{} · {}", row.format, state_label(&row.state));
    if row.state == RowState::OnLoan
        && let Some(expires) = &row.expires
    {
        info.push_str(&format!(
            " · until {}",
            expires.get(..10).unwrap_or(expires)
        ));
    }
    ui.add(egui::Label::new(info).truncate());
    if let Some(message) = &row.error {
        ui.add(
            egui::Label::new(egui::RichText::new(message).color(ui.visuals().error_fg_color))
                .truncate(),
        );
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
fn action(ui: &mut egui::Ui, row: &Row, progress: Option<f32>) -> Option<Action> {
    let key = || row.key.clone();
    let mut clicked = None;

    // A queued or running sync offers only Cancel, until the running sync's Fulfill is
    // sent; a loan return offers nothing.
    ui.add_enabled_ui(!row.menu.is_empty(), |ui| {
        let button = egui::Button::new(more_icon(ui)).frame_when_inactive(false);
        egui::containers::menu::MenuButton::from_button(button).ui(ui, |ui| {
            for entry in &row.menu {
                match entry {
                    MenuAction::CancelSync { enabled } => {
                        if ui
                            .add_enabled(*enabled, egui::Button::new("Cancel sync"))
                            .clicked()
                        {
                            clicked = Some(Action::Cancel(key()));
                        }
                    }
                    MenuAction::SyncAgain => {
                        if ui.button("Sync again").clicked() {
                            clicked = Some(Action::Sync(key()));
                        }
                    }
                    MenuAction::ReturnLoan => {
                        if ui.button("Return loan").clicked() {
                            clicked = Some(Action::Return(key()));
                        }
                    }
                    MenuAction::ShowInFolder => {
                        if let Some(folder) = &row.folder
                            && ui.button("Show in folder").clicked()
                        {
                            clicked = Some(Action::ShowInFolder(PathBuf::from(folder)));
                        }
                    }
                    MenuAction::RemoveFromDisk => {
                        if ui.button("Remove from disk…").clicked() {
                            clicked = Some(Action::Remove(key()));
                        }
                    }
                }
            }
        });
    });

    let (label, main) = match &row.primary {
        Some(Primary::Open) => (
            "Open",
            row.publication
                .as_ref()
                .map(|path| Action::Open(PathBuf::from(path))),
        ),
        Some(Primary::Retry) => ("Retry", Some(Action::Sync(key()))),
        Some(Primary::Sync) => ("Sync", Some(Action::Sync(key()))),
        // A row without an ACSM keeps a disabled button, labelled as it would be.
        None if row.state == RowState::Failed => ("Retry", None),
        None => ("Sync", None),
    };
    let working = row.activity != Activity::Idle;
    let button = if matches!(row.activity, Activity::Syncing { .. }) {
        // The active sync shows a progress bar in the button's place and size.
        progress_button(ui, progress);
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
