#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod account;
mod jobs;
mod list;

use eframe::egui;
use jobs::Event;
use leafport_core::{
    cloud::DownloadRecord,
    library::{Item, ItemState, Library, Settings, Theme, load_cache, merge, paths},
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::mpsc::{self, Receiver, Sender},
};

pub struct App {
    pub library: Library,
    pub cloud: Vec<DownloadRecord>,
    pub rows: Vec<Item>,
    /// Last sync error per row key, held in memory only.
    pub failures: HashMap<String, String>,
    /// The row whose sync worker runs (at most one).
    pub syncing: HashSet<String>,
    /// Row keys waiting to sync, in the order requested.
    pub queue: VecDeque<String>,
    pub banner: Option<String>,
    pub settings: Settings,
    account: account::Account,
    refreshing: bool,
    /// Bumped on sign-in and sign-out; a refresh started under an older value is stale.
    generation: u64,
    covers: list::Covers,
    tx: Sender<Event>,
    rx: Receiver<Event>,
}

/// Append a message to the banner, keeping earlier ones.
fn add_banner(banner: &mut Option<String>, message: String) {
    match banner {
        Some(text) => {
            text.push('\n');
            text.push_str(&message);
        }
        None => *banner = Some(message),
    }
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (tx, rx) = mpsc::channel();
        let mut banner = None;
        let settings = match paths::settings_path() {
            Some(path) => Settings::load(&path).unwrap_or_else(|e| {
                add_banner(&mut banner, format!("Could not read settings: {e}"));
                Settings::default()
            }),
            None => Settings::default(),
        };
        cc.egui_ctx.set_theme(match settings.theme {
            Theme::System => egui::ThemePreference::System,
            Theme::Light => egui::ThemePreference::Light,
            Theme::Dark => egui::ThemePreference::Dark,
        });
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
            syncing: HashSet::new(),
            queue: VecDeque::new(),
            banner,
            settings,
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
        self.remerge();
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
                self.syncing.remove(&key);
                match result {
                    Ok(outcome) => {
                        self.failures.remove(&key);
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
        self.sidebar(ctx);
        if let Some(text) = self.banner.clone() {
            egui::TopBottomPanel::top("banner").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.small_button("✕").clicked() {
                        self.banner = None;
                    }
                    ui.colored_label(ui.visuals().warn_fg_color, text);
                });
            });
        }
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
                syncing: &self.syncing,
                queue: &self.queue,
            };
            clicked = list::show(ui, &self.rows, busy, &mut self.covers, &self.tx);
        });
        if let Some(key) = clicked {
            self.enqueue(key, ctx);
        }
        if self.account.open {
            self.account.window(&self.tx, ctx);
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
