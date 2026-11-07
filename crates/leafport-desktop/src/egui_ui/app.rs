use eframe::egui;
use leafport_app::{Activity, AppError, Event, Listener, OperationKind, Row, Settings, Status};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
};

/// How long a notice stays on screen.
const NOTICE_TIME: std::time::Duration = std::time::Duration::from_secs(4);

/// Passes the app's events to the UI thread and wakes it.
struct Forward {
    tx: Sender<Event>,
    ctx: egui::Context,
}

impl Listener for Forward {
    fn on_event(&self, event: Event) {
        let _ = self.tx.send(event);
        self.ctx.request_repaint();
    }
}

/// A view of the `leafport_app::App` state, plus the state only this UI has.
pub struct App {
    core: Arc<leafport_app::App>,
    rows: Vec<Row>,
    status: Status,
    settings: Settings,
    /// Download progress of syncing rows, once their download reports one.
    progress: HashMap<String, f32>,
    rx: Receiver<Event>,
    /// The latest notice and when it arrived; shown for `NOTICE_TIME`.
    notice: Option<(String, std::time::Instant)>,
    settings_view: crate::egui_ui::settings_view::SettingsView,
    account: crate::egui_ui::account::Account,
    covers: crate::egui_ui::list::Covers,
}

/// The startup argument, when it names an ACSM file.
fn startup_acsm() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::args_os().nth(1)?);
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("acsm"))
        .then_some(path)
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (tx, rx) = mpsc::channel();
        let core = leafport_app::App::new(Box::new(Forward {
            tx,
            ctx: cc.egui_ctx.clone(),
        }));
        // The cloud list is refreshed only when the user asks; until then the cache shows.
        let snapshot = core.snapshot();
        cc.egui_ctx
            .set_theme(crate::egui_ui::settings_view::theme_preference(
                &snapshot.settings.theme,
            ));
        if let Some(path) = startup_acsm() {
            core.import_acsm(path_string(&path));
        }
        Self {
            account: crate::egui_ui::account::Account::new(&snapshot.status),
            core,
            rows: snapshot.rows,
            status: snapshot.status,
            settings: snapshot.settings,
            progress: HashMap::new(),
            rx,
            notice: None,
            settings_view: crate::egui_ui::settings_view::SettingsView::default(),
            covers: crate::egui_ui::list::Covers::default(),
        }
    }

    /// Apply one event from the app.
    fn apply(&mut self, event: Event) {
        match event {
            Event::Rows { rows } => {
                // Read a cover again only for rows whose cover may have changed or that left.
                for old in &self.rows {
                    match rows.iter().find(|row| row.key == old.key) {
                        Some(new) if !cover_changed(old, new) => {}
                        Some(new) => {
                            forget_cover(&mut self.covers, old);
                            forget_cover(&mut self.covers, new);
                        }
                        None => forget_cover(&mut self.covers, old),
                    }
                }
                self.rows = rows;
                let rows = &self.rows;
                self.progress
                    .retain(|key, _| rows.iter().any(|row| row.key == *key && is_syncing(row)));
            }
            Event::Row { row } => {
                let Some(slot) = self.rows.iter_mut().find(|old| old.key == row.key) else {
                    return;
                };
                // A row with a new cover file or folder has its cover read again.
                if cover_changed(slot, &row) {
                    forget_cover(&mut self.covers, slot);
                    forget_cover(&mut self.covers, &row);
                }
                if !is_syncing(&row) {
                    self.progress.remove(&row.key);
                }
                *slot = row;
            }
            Event::RowRemoved { key } => {
                if let Some(index) = self.rows.iter().position(|row| row.key == key) {
                    let row = self.rows.remove(index);
                    forget_cover(&mut self.covers, &row);
                }
                self.progress.remove(&key);
            }
            Event::Progress { key, fraction } => {
                if self
                    .rows
                    .iter()
                    .any(|row| row.key == key && is_syncing(row))
                {
                    self.progress.insert(key, fraction);
                }
            }
            Event::Status { status } => {
                self.account.on_status(&self.status, &status);
                self.status = status;
            }
            Event::Providers { result } => self.account.on_providers(result),
            Event::Notice { message } => self.notice = Some((message, std::time::Instant::now())),
        }
    }

    /// Save edited settings and apply the theme. The app opens a changed books folder.
    fn apply_settings(&mut self, new: Settings, ctx: &egui::Context) {
        match self.core.set_settings(new.clone()) {
            // A failed save is already in the banner; the settings still apply.
            Ok(()) | Err(AppError::Save { .. }) => {
                ctx.set_theme(crate::egui_ui::settings_view::theme_preference(&new.theme));
                self.settings = new;
            }
            Err(AppError::Busy) => {
                self.core.add_message(
                    "The books folder can change once the running job finishes.".into(),
                );
                // Nothing was saved; show the settings in effect again.
                self.settings_view.open(&self.settings);
            }
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Import ACSM…").clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("ACSM", &["acsm"])
                        .pick_file()
                {
                    self.core.import_acsm(path_string(&path));
                }
                if ui.button("Settings").clicked() {
                    self.settings_view.open(&self.settings);
                }
                if let Some(running) = self.status.operations.iter().find(|op| op.running) {
                    ui.spinner();
                    let verb = match running.kind {
                        OperationKind::Sync { .. } => "Syncing",
                        OperationKind::Import { .. } => "Importing",
                        OperationKind::Return { .. } => "Returning",
                    };
                    let waiting = self.status.operations.len() - 1;
                    let mut text = format!("{verb} {}…", running.label);
                    if waiting > 0 {
                        text.push_str(&format!(" ({waiting} more)"));
                    }
                    ui.weak(text);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // A disabled button gets no explanation, so the label says why.
                    let all_synced = self.status.user.is_some()
                        && !self.status.can_sync_all
                        && !self.rows.is_empty();
                    let label = if all_synced {
                        "All books synced"
                    } else {
                        "Sync all"
                    };
                    if ui
                        .add_enabled(self.status.can_sync_all, egui::Button::new(label))
                        .clicked()
                    {
                        self.core.sync_all();
                    }
                    if self.status.can_cancel_all && ui.button("Cancel all").clicked() {
                        self.core.cancel_all();
                    }
                });
            });
        });
    }

    fn banner(&mut self, ui: &mut egui::Ui) {
        if self.status.banner.is_empty() {
            return;
        }
        egui::Panel::top("banner").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .add(egui::Button::new(crate::egui_ui::list::close_icon(ui)).small())
                    .clicked()
                {
                    // Clear at once; the app confirms with a status update.
                    self.status.banner.clear();
                    self.core.clear_banner();
                }
                ui.vertical(|ui| {
                    for line in &self.status.banner {
                        ui.colored_label(ui.visuals().warn_fg_color, line);
                    }
                });
            });
        });
    }

    /// The latest notice at the bottom of the window, until `NOTICE_TIME` passed.
    fn notice(&mut self, ctx: &egui::Context) {
        let Some((message, since)) = &self.notice else {
            return;
        };
        let Some(left) = NOTICE_TIME.checked_sub(since.elapsed()) else {
            self.notice = None;
            return;
        };
        egui::Area::new(egui::Id::unique("notice"))
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -16.0))
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| ui.label(message.as_str()));
            });
        ctx.request_repaint_after(left);
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("account")
            .default_size(220.0)
            .show(ui, |ui| {
                ui.heading("Leafport");
                ui.separator();
                match self.status.user.clone() {
                    Some(user) => {
                        match &self.status.provider {
                            Some(provider) => ui.label(format!("Signed in with {provider}")),
                            None => ui.label("Signed in as"),
                        };
                        ui.strong(self.status.username.clone().unwrap_or(user));
                        if ui.button("Sign out").clicked() {
                            self.core.sign_out();
                        }
                    }
                    None => {
                        ui.label("Not signed in");
                        if ui.button("Sign in").clicked() {
                            self.account.open_window(&self.core);
                        }
                    }
                }
                ui.separator();
                ui.horizontal(|ui| {
                    let enabled = self.status.user.is_some() && !self.status.refreshing;
                    if ui
                        .add_enabled(enabled, egui::Button::new("Refresh list"))
                        .clicked()
                    {
                        self.core.refresh();
                    }
                    if self.status.refreshing {
                        ui.spinner();
                    }
                });
                let note = self.account.note(&self.status);
                if !note.is_empty() {
                    ui.weak(note);
                }
            });
    }
}

fn is_syncing(row: &Row) -> bool {
    matches!(row.activity, Activity::Syncing { .. })
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.covers.receive(ctx);
        while let Ok(event) = self.rx.try_recv() {
            self.apply(event);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.toolbar(ui);
        self.banner(ui);
        self.sidebar(ui);
        let mut clicked = None;
        egui::CentralPanel::default().show(ui, |ui| {
            if self.status.user.is_none() {
                ui.horizontal(|ui| {
                    ui.label("Sign in to see the books in your account.");
                    if ui.button("Sign in").clicked() {
                        self.account.open_window(&self.core);
                    }
                });
                ui.separator();
            }
            clicked = crate::egui_ui::list::show(ui, &self.rows, &self.progress, &mut self.covers);
        });
        match clicked {
            Some(crate::egui_ui::list::Action::Sync(key)) => self.core.sync(key),
            Some(crate::egui_ui::list::Action::Return(key)) => self.core.return_loan(key),
            Some(
                crate::egui_ui::list::Action::Open(path)
                | crate::egui_ui::list::Action::ShowInFolder(path),
            ) => {
                if let Err(e) = crate::egui_ui::list::open_path(&path) {
                    self.core
                        .add_message(format!("Could not open {}: {e}", path.display()));
                }
            }
            Some(crate::egui_ui::list::Action::Exclude(key)) => self.core.set_excluded(key, true),
            Some(crate::egui_ui::list::Action::Include(key)) => self.core.set_excluded(key, false),
            Some(crate::egui_ui::list::Action::Cancel(key)) => self.core.cancel(key),
            None => {}
        }
        self.notice(&ctx);
        if self.account.open {
            self.account.window(&self.core, &self.status, &ctx);
        }
        if self.settings_view.open {
            let busy = self.status.job_running;
            let books_dir = self.status.books_dir.clone();
            if let Some(new) =
                self.settings_view
                    .window(&ctx, &self.settings, books_dir.as_deref(), busy)
            {
                self.apply_settings(new, &ctx);
            }
        }
    }
}

/// Whether `new` may show a different cover than `old`.
fn cover_changed(old: &Row, new: &Row) -> bool {
    old.cover_path != new.cover_path || old.folder != new.folder
}

/// Drop the cached cover textures of `row`, so they are read again.
fn forget_cover(covers: &mut crate::egui_ui::list::Covers, row: &Row) {
    for path in [&row.folder, &row.cover_path].into_iter().flatten() {
        covers.forget(Path::new(path));
    }
}

pub fn run() -> eframe::Result {
    eframe::run_native(
        "Leafport",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1120.0, 780.0])
                .with_min_inner_size([720.0, 480.0]),
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
