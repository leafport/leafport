#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod account;
mod jobs;
mod list;
mod settings_view;

use eframe::egui;
use jobs::{Event, Job};
use leafport_core::{
    cloud::DownloadRecord,
    library::{Item, ItemState, Library, Settings, load_cache, merge, paths},
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
};

/// The most banner messages kept; older ones drop off.
const BANNER_LINES: usize = 5;

pub struct App {
    pub library: Library,
    pub cloud: Vec<DownloadRecord>,
    pub rows: Vec<Item>,
    /// Last sync error per row key, held in memory only.
    pub failures: HashMap<String, String>,
    /// The sync or import whose worker runs (at most one).
    pub running: Option<Job>,
    /// Syncs and imports waiting to run, in the order requested.
    pub queue: VecDeque<Job>,
    /// Rows whose loan return runs.
    pub returning: HashSet<String>,
    /// Last return error per row key. Kept apart from `failures`, so the row stays OnLoan.
    pub row_errors: HashMap<String, String>,
    /// Recent unique messages, oldest first.
    pub banner: Vec<String>,
    pub settings: Settings,
    settings_view: settings_view::SettingsView,
    account: account::Account,
    refreshing: bool,
    /// Bumped on sign-in and sign-out; a refresh started under an older value is stale.
    generation: u64,
    covers: list::Covers,
    tx: Sender<Event>,
    rx: Receiver<Event>,
}

/// Add a message to the banner, keeping the most recent unique ones.
pub fn add_banner(banner: &mut Vec<String>, message: String) {
    banner.retain(|line| *line != message);
    banner.push(message);
    if banner.len() > BANNER_LINES {
        banner.drain(..banner.len() - BANNER_LINES);
    }
}

/// The startup argument, when it names an ACSM file.
fn startup_acsm() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::args_os().nth(1)?);
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("acsm"))
        .then_some(path)
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (tx, rx) = mpsc::channel();
        let mut banner = Vec::new();
        let settings = match paths::settings_path() {
            Some(path) => Settings::load(&path).unwrap_or_else(|e| {
                add_banner(&mut banner, format!("Could not read settings: {e}"));
                Settings::default()
            }),
            None => Settings::default(),
        };
        cc.egui_ctx
            .set_theme(settings_view::theme_preference(settings.theme));
        let root = settings.books_dir_or_default().unwrap_or_default();
        let library = if root.as_os_str().is_empty() {
            add_banner(&mut banner, "No books folder could be determined.".into());
            Library {
                root,
                local: vec![],
            }
        } else {
            Library::open(&root).unwrap_or_else(|e| {
                add_banner(
                    &mut banner,
                    format!("Could not open {}: {e}", root.display()),
                );
                Library {
                    root,
                    local: vec![],
                }
            })
        };
        let cloud = match paths::cache_path().map(|path| load_cache(&path)) {
            Some(Err(e)) => {
                add_banner(
                    &mut banner,
                    format!("Could not read the cloud list cache: {e}"),
                );
                vec![]
            }
            Some(Ok(cloud)) => cloud,
            None => vec![],
        };
        let mut app = Self {
            library,
            cloud,
            rows: vec![],
            failures: HashMap::new(),
            running: None,
            queue: VecDeque::new(),
            returning: HashSet::new(),
            row_errors: HashMap::new(),
            banner,
            settings,
            settings_view: settings_view::SettingsView::default(),
            account: account::Account::new(),
            refreshing: false,
            generation: 0,
            covers: list::Covers::default(),
            tx,
            rx,
        };
        app.remerge();
        if app.account.signed_in.is_some() {
            app.refresh(&cc.egui_ctx);
        }
        if let Some(path) = startup_acsm() {
            app.enqueue_import(path, &cc.egui_ctx);
        }
        app
    }

    /// Rebuild `rows` from the cloud list and local folders, then overlay sync failures.
    pub fn remerge(&mut self) {
        self.rows = merge(&self.cloud, &self.library.local);
        for row in &mut self.rows {
            if let Some(message) = self.failures.get(&row.key) {
                row.state = ItemState::Failed(message.clone());
            }
        }
    }

    fn refresh(&mut self, ctx: &egui::Context) {
        if self.refreshing {
            return;
        }
        self.refreshing = true;
        jobs::refresh(&self.tx, ctx, paths::cache_path(), self.generation);
    }

    /// Start a new account generation: in-flight refreshes become stale and a new one may start.
    fn new_generation(&mut self) {
        self.generation += 1;
        self.refreshing = false;
    }

    fn remove_cache(&mut self) {
        if let Some(path) = paths::cache_path() {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => add_banner(
                    &mut self.banner,
                    format!("Could not remove the cloud list cache: {e}"),
                ),
            }
        }
    }

    /// Re-read the books folder without sweeping temp folders, so a running sync is safe.
    fn rescan(&mut self) {
        match Library::rescan(&self.library.root) {
            Ok(library) => self.library = library,
            Err(e) => add_banner(
                &mut self.banner,
                format!("Could not read the books folder: {e}"),
            ),
        }
        self.remerge();
    }

    /// Start returning the loan of the row with `key`, unless a return already runs.
    fn start_return(&mut self, key: String, ctx: &egui::Context) {
        if self.returning.contains(&key) {
            return;
        }
        let Some(item) = self.rows.iter().find(|row| row.key == key).cloned() else {
            return;
        };
        self.row_errors.remove(&key);
        self.returning.insert(key);
        jobs::return_loan(&self.tx, ctx, item);
    }

    fn sign_out(&mut self) {
        if self.account.sign_out().is_err() {
            return;
        }
        self.new_generation();
        self.remove_cache();
        self.cloud.clear();
        // Queued cloud rows have no ACSM once the list is gone, and old failures would
        // mark the remaining Local rows Failed.
        self.queue.clear();
        self.failures.clear();
        self.row_errors.clear();
        self.remerge();
    }

    /// Save edited settings, apply the theme, and on a books folder change open the new folder.
    fn apply_settings(&mut self, new: Settings, ctx: &egui::Context) {
        match paths::settings_path() {
            Some(path) => {
                if let Err(e) = new.save(&path) {
                    add_banner(&mut self.banner, format!("Could not save settings: {e}"));
                }
            }
            None => add_banner(
                &mut self.banner,
                "No settings folder could be determined.".into(),
            ),
        }
        ctx.set_theme(settings_view::theme_preference(new.theme));
        let old_root = self.settings.books_dir_or_default();
        let new_root = new.books_dir_or_default();
        self.settings = new;
        if new_root != old_root
            && let Some(root) = new_root
        {
            self.library = Library::open(&root).unwrap_or_else(|e| {
                add_banner(
                    &mut self.banner,
                    format!("Could not open {}: {e}", root.display()),
                );
                Library {
                    root,
                    local: vec![],
                }
            });
            self.remerge();
        }
    }

    fn toolbar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Import ACSM…").clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("ACSM", &["acsm"])
                        .pick_file()
                {
                    self.enqueue_import(path, ctx);
                }
                if ui.button("Settings").clicked() {
                    self.settings_view.open(&self.settings);
                }
                if matches!(self.running, Some(Job::Import(_))) {
                    ui.spinner();
                    ui.weak("Importing…");
                }
            });
        });
    }

    fn banner(&mut self, ctx: &egui::Context) {
        if self.banner.is_empty() {
            return;
        }
        egui::TopBottomPanel::top("banner").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.small_button("✕").clicked() {
                    self.banner.clear();
                }
                ui.vertical(|ui| {
                    for line in &self.banner {
                        ui.colored_label(ui.visuals().warn_fg_color, line);
                    }
                });
            });
        });
    }

    fn handle(&mut self, event: Event, ctx: &egui::Context) {
        match event {
            Event::Vendors(result) => self.account.on_vendors(result),
            Event::SignIn(result) => {
                if self.account.on_sign_in(result) {
                    self.new_generation();
                    self.refresh(ctx);
                }
            }
            Event::Refreshed { generation, .. }
                if generation != self.generation || self.account.signed_in.is_none() =>
            {
                // Started before a sign-in or sign-out: it may have rewritten the cache
                // after sign-out removed it. Drop it, and refetch for a current account.
                self.remove_cache();
                if self.account.signed_in.is_some() && !self.refreshing {
                    self.refresh(ctx);
                }
            }
            Event::Refreshed { result, .. } => {
                self.refreshing = false;
                match result {
                    Ok(cloud) => {
                        self.cloud = cloud;
                        self.remerge();
                    }
                    Err(e) => add_banner(&mut self.banner, format!("Refresh failed: {e}")),
                }
            }
            Event::Synced { key, result } => {
                self.running = None;
                match result {
                    Ok(outcome) => {
                        self.failures.remove(&key);
                        self.covers.forget(&outcome.folder);
                        if let Some(warning) = outcome.warning {
                            add_banner(&mut self.banner, warning);
                        }
                    }
                    Err(e) => {
                        self.failures.insert(key, e);
                    }
                }
                self.rescan();
                self.start_next_sync(ctx);
            }
            Event::Imported(result) => {
                self.running = None;
                match result {
                    Ok(outcome) => {
                        self.failures.remove(&outcome.metadata.resource);
                        self.covers.forget(&outcome.folder);
                        if let Some(warning) = outcome.warning {
                            add_banner(&mut self.banner, warning);
                        }
                    }
                    Err(e) => add_banner(&mut self.banner, format!("Import failed: {e}")),
                }
                self.rescan();
                self.start_next_sync(ctx);
            }
            Event::Returned { key, result } => {
                self.returning.remove(&key);
                match result {
                    Ok(_) => {
                        self.row_errors.remove(&key);
                        let folder = self.rows.iter().find(|row| row.key == key);
                        if let Some(folder) = folder.and_then(|row| row.folder.as_deref()) {
                            self.covers.forget(folder);
                        }
                    }
                    Err(e) => {
                        self.row_errors.insert(key, e);
                    }
                }
                self.rescan();
            }
            Event::Cover { path, image } => self.covers.insert(ctx, path, image),
        }
    }

    fn sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("account")
            .default_width(220.0)
            .show(ctx, |ui| {
                ui.heading("Leafport");
                ui.separator();
                match self.account.signed_in.clone() {
                    Some(user) => {
                        ui.label("Signed in as");
                        ui.strong(user);
                        if ui.button("Sign out").clicked() {
                            self.sign_out();
                        }
                    }
                    None => {
                        ui.label("Not signed in");
                        if ui.button("Sign in").clicked() {
                            self.account.open_window(&self.tx, ctx);
                        }
                    }
                }
                ui.separator();
                ui.horizontal(|ui| {
                    let enabled = self.account.signed_in.is_some() && !self.refreshing;
                    if ui
                        .add_enabled(enabled, egui::Button::new("Refresh list"))
                        .clicked()
                    {
                        self.refresh(ctx);
                    }
                    if self.refreshing {
                        ui.spinner();
                    }
                });
                if !self.account.status.is_empty() {
                    ui.weak(&self.account.status);
                }
            });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        while let Ok(event) = self.rx.try_recv() {
            self.handle(event, ctx);
        }
        self.toolbar(ctx);
        self.banner(ctx);
        self.sidebar(ctx);
        let mut clicked = None;
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.account.signed_in.is_none() {
                ui.horizontal(|ui| {
                    ui.label("Sign in to see the books in your account.");
                    if ui.button("Sign in").clicked() {
                        self.account.open_window(&self.tx, ctx);
                    }
                });
                ui.separator();
            }
            let busy = list::Busy {
                running: self.running.as_ref(),
                queue: &self.queue,
                returning: &self.returning,
                row_errors: &self.row_errors,
            };
            clicked = list::show(ui, &self.rows, busy, &mut self.covers, &self.tx);
        });
        match clicked {
            Some(list::Action::Sync(key)) => self.enqueue(key, ctx),
            Some(list::Action::Return(key)) => self.start_return(key, ctx),
            Some(list::Action::ShowInFolder(folder)) => {
                if let Err(e) = list::show_in_folder(&folder) {
                    add_banner(
                        &mut self.banner,
                        format!("Could not open {}: {e}", folder.display()),
                    );
                }
            }
            None => {}
        }
        if self.account.open {
            self.account.window(&self.tx, ctx);
        }
        if self.settings_view.open {
            let busy = self.running.is_some();
            if let Some(new) = self.settings_view.window(ctx, &self.settings, busy) {
                self.apply_settings(new, ctx);
            }
        }
    }
}

fn main() -> eframe::Result {
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
