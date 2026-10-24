//! The app state, the messages that change it, and the reactor thread that applies them.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use leafport_core::cloud::DownloadRecord;
use leafport_core::fulfillment::{Acsm, MAX_ACSM_BYTES};
use leafport_core::library::{
    self, Item, ItemState, Library, Metadata, SyncOutcome, load_cache, merge,
};
use leafport_core::service::CancelToken;
use leafport_core::vendors::VendorCatalog;

use crate::ops::{Ops, Paths, SyncHooks};
use crate::rows::{self, Busy, Job, StatusInputs};
use crate::{AppError, Event, Listener, Provider, Row, Settings, Snapshot, Status, Theme};

/// The most banner messages kept; older ones drop off.
const BANNER_LINES: usize = 5;

/// Add a message to the banner, keeping the most recent unique ones.
pub(crate) fn add_banner(banner: &mut Vec<String>, message: String) {
    banner.retain(|line| *line != message);
    banner.push(message);
    if banner.len() > BANNER_LINES {
        banner.drain(..banner.len() - BANNER_LINES);
    }
}

/// Run a worker body, converting a panic into an Err so the completion message is always sent.
pub(crate) fn catch_panic<T>(body: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).unwrap_or_else(|payload| {
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".into());
        Err(format!("internal error: {message}"))
    })
}

/// Lock `state`, recovering it if a panic poisoned the lock.
pub(crate) fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A command from `App` or a result from a worker thread.
pub(crate) enum Msg {
    /// Fetch the cloud list, unless signed out or a refresh already runs.
    Refresh,
    /// A completed list refresh. `generation` is the account generation it started under.
    Refreshed {
        generation: u64,
        result: Result<Vec<DownloadRecord>, String>,
    },
    /// Queue a sync of the row with this key.
    Sync(String),
    /// Queue a sync of every row `unsynced` lists.
    SyncAll,
    /// Drop the sync of this key from the queue, or cancel it while it runs.
    Cancel(String),
    /// Drop every queued sync and cancel the running one. Imports stay.
    CancelAll,
    /// Queue an import of the ACSM file at this path.
    Import(PathBuf),
    /// Download progress of the sync of `key`.
    Progress { key: String, fraction: f32 },
    /// The sync of `key` is about to send Fulfill and can no longer be cancelled.
    Committed { key: String },
    /// A finished sync.
    Synced {
        key: String,
        result: Result<SyncOutcome, String>,
    },
    /// A finished import.
    Imported(Result<SyncOutcome, String>),
    /// Return the loan of this key.
    ReturnLoan(String),
    /// Delete the title folder of this key, returning its loan first when asked.
    Remove {
        key: String,
        return_loan_first: bool,
    },
    /// A finished loan return.
    Returned {
        key: String,
        result: Result<Metadata, String>,
    },
    /// Add a message to the banner.
    AddMessage(String),
    /// Empty the banner.
    ClearBanner,
    /// Settings changed under the lock; publish what changed.
    Publish,
    /// Load the sign-in provider catalog, unless that already runs.
    LoadProviders,
    /// The loaded provider catalog.
    Providers(Result<VendorCatalog, String>),
    /// Sign in. The password lives only in this message and the worker that gets it.
    SignIn {
        method: String,
        username: String,
        password: zeroize::Zeroizing<String>,
    },
    /// A finished sign-in: the account label.
    SignedIn(Result<String, String>),
    /// Forget the stored activation and the cloud list.
    SignOut,
    /// A finished sign-out.
    SignedOut(Result<(), String>),
}

/// Everything the reactor owns. `App` reads only the `emitted_*` fields and the settings.
pub(crate) struct State {
    pub paths: Paths,
    pub settings: library::Settings,
    pub library: Library,
    pub cloud: Vec<DownloadRecord>,
    /// The merged list; `remerge` rebuilds it.
    pub items: Vec<Item>,
    /// Last sync error per row key, held in memory only.
    pub failures: HashMap<String, String>,
    /// The sync or import whose worker runs (at most one).
    pub running: Option<Job>,
    /// Syncs and imports waiting to run, in the order requested.
    pub queue: VecDeque<Job>,
    /// The cancel token of the running sync.
    pub cancel: Option<CancelToken>,
    /// Whether the running sync reported that its Fulfill is about to be sent.
    pub committed: bool,
    /// Rows whose loan return runs.
    pub returning: HashSet<String>,
    /// Last return error per row key. Kept apart from `failures`, so the row stays OnLoan.
    pub row_errors: HashMap<String, String>,
    /// Rows whose title folder is deleted once their running return succeeds, with the
    /// folder the user confirmed when asking.
    pub remove_after_return: HashMap<String, PathBuf>,
    /// Recent unique messages, oldest first.
    pub banner: Vec<String>,
    /// The signed-in account label.
    pub user: Option<String>,
    pub refreshing: bool,
    /// The sign-in providers; kept when a later load fails and across sign-out.
    pub catalog: Arc<VendorCatalog>,
    pub loading_providers: bool,
    pub signing_in: bool,
    pub signing_out: bool,
    pub sign_in_error: Option<String>,
    /// Bumped on sign-in and sign-out; a refresh started under an older value is stale.
    pub generation: u64,
    /// The rows the listener has last been told about.
    pub emitted_rows: Vec<Row>,
    /// The status the listener has last been told about.
    pub emitted_status: Status,
}

impl State {
    /// Read the settings, books folder, cloud list cache and signed-in account.
    /// Failures go to the banner.
    pub(crate) fn load(ops: &dyn Ops, paths: Paths) -> State {
        let mut banner = Vec::new();
        let settings = match &paths.settings {
            Some(path) => library::Settings::load(path).unwrap_or_else(|e| {
                add_banner(&mut banner, format!("Could not read settings: {e}"));
                library::Settings::default()
            }),
            None => library::Settings::default(),
        };
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
        let cloud = match paths.cache.as_deref().map(load_cache) {
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
        let user = ops.signed_in_user().unwrap_or_else(|e| {
            add_banner(&mut banner, format!("Credential store unavailable: {e}"));
            None
        });
        let mut state = State {
            paths,
            settings,
            library,
            cloud,
            items: vec![],
            failures: HashMap::new(),
            running: None,
            queue: VecDeque::new(),
            cancel: None,
            committed: false,
            returning: HashSet::new(),
            row_errors: HashMap::new(),
            remove_after_return: HashMap::new(),
            banner,
            user,
            refreshing: false,
            catalog: Arc::new(VendorCatalog::new()),
            loading_providers: false,
            signing_in: false,
            signing_out: false,
            sign_in_error: None,
            generation: 0,
            emitted_rows: vec![],
            emitted_status: rows::status(&StatusInputs {
                user: None,
                refreshing: false,
                banner: &[],
                books_dir: None,
            }),
        };
        // The cloud list is refreshed only when the user asks; until then the cache shows.
        state.remerge();
        state.emitted_rows = state.rows();
        state.emitted_status = state.status();
        state
    }

    /// Rebuild `items` from the cloud list and local folders, then overlay sync failures.
    pub(crate) fn remerge(&mut self) {
        self.items = merge(&self.cloud, &self.library.local);
        for item in &mut self.items {
            if let Some(message) = self.failures.get(&item.key) {
                item.state = ItemState::Failed(message.clone());
            }
        }
    }

    /// Re-read the books folder without sweeping temp folders, so a running sync is safe.
    pub(crate) fn rescan(&mut self) {
        match Library::rescan(&self.library.root) {
            Ok(library) => self.library = library,
            Err(e) => self.add_banner(format!("Could not read the books folder: {e}")),
        }
        self.remerge();
    }

    /// Whether the running sync can still be cancelled: its Fulfill was not sent.
    pub(crate) fn running_cancellable(&self) -> bool {
        matches!(self.running, Some(Job::Sync(_)))
            && !self.committed
            && self
                .cancel
                .as_ref()
                .is_some_and(|token| !token.is_committed())
    }

    /// Keys of the rows Sync all queues: Available or Failed, with an ACSM and no book
    /// file on disk yet.
    pub(crate) fn unsynced(&self) -> Vec<String> {
        self.items
            .iter()
            .filter(|item| matches!(item.state, ItemState::Available | ItemState::Failed(_)))
            .filter(|item| item.publication.is_none() && item.acsm.is_some())
            .map(|item| item.key.clone())
            .collect()
    }

    /// Delete the title folder of the row with `key` and rescan. The book stays in the
    /// cloud list.
    pub(crate) fn remove_folder(&mut self, key: &str) {
        let Some(folder) = self.folder_of(key) else {
            return;
        };
        self.remove_path(key, &folder);
    }

    /// The title folder of the row with `key`.
    pub(crate) fn folder_of(&self, key: &str) -> Option<PathBuf> {
        self.items
            .iter()
            .find(|item| item.key == key)
            .and_then(|item| item.folder.clone())
    }

    /// Delete `folder`, the title folder of the row with `key`, and rescan.
    fn remove_path(&mut self, key: &str, folder: &std::path::Path) {
        // Only a title folder directly inside the books folder is ever deleted.
        if folder.parent() != Some(self.library.root.as_path()) {
            self.add_banner(format!(
                "Not removing {}: outside the books folder",
                folder.display()
            ));
            return;
        }
        match std::fs::remove_dir_all(folder) {
            Ok(()) => {
                self.failures.remove(key);
                self.row_errors.remove(key);
            }
            Err(e) => self.add_banner(format!("Could not remove {}: {e}", folder.display())),
        }
        self.rescan();
    }

    /// Save `new`, and on a books folder change open the new folder. The theme is applied
    /// by the UI. A folder change while a sync, import or loan return runs is refused
    /// before anything is saved. A failed save is reported but the settings still apply in memory, as in
    /// the egui app.
    pub(crate) fn apply_settings(&mut self, new: Settings) -> Result<(), AppError> {
        let mut next = self.settings.clone();
        next.books_dir = match new.books_dir.filter(|dir| !dir.is_empty()) {
            // The public form is lossy; an unchanged folder keeps its exact stored path.
            Some(dir)
                if self
                    .settings
                    .books_dir
                    .as_deref()
                    .is_some_and(|stored| stored.to_string_lossy() == dir) =>
            {
                self.settings.books_dir.clone()
            }
            dir => dir.map(PathBuf::from),
        };
        next.theme = match new.theme {
            Theme::System => library::Theme::System,
            Theme::Light => library::Theme::Light,
            Theme::Dark => library::Theme::Dark,
        };
        let old_root = self.settings.books_dir_or_default();
        let new_root = next.books_dir_or_default();
        let folder_changes = new_root != old_root;
        if folder_changes && self.job_running() {
            return Err(AppError::Busy);
        }
        let saved = match &self.paths.settings {
            Some(path) => next.save(path).map_err(|e| e.to_string()).map_err(|e| {
                self.add_banner(format!("Could not save settings: {e}"));
                e
            }),
            None => {
                let message = "No settings folder could be determined.".to_string();
                self.add_banner(message.clone());
                Err(message)
            }
        };
        self.settings = next;
        if folder_changes && let Some(root) = new_root {
            self.library = Library::open(&root).unwrap_or_else(|e| {
                self.add_banner(format!("Could not open {}: {e}", root.display()));
                Library {
                    root,
                    local: vec![],
                }
            });
            self.remerge();
        }
        saved.map_err(|message| AppError::Save { message })
    }

    pub(crate) fn add_banner(&mut self, message: String) {
        add_banner(&mut self.banner, message);
    }

    /// Whether a sync, import or loan return runs; the books folder cannot change then.
    pub(crate) fn job_running(&self) -> bool {
        self.running.is_some() || !self.returning.is_empty()
    }

    /// Start a new account generation: in-flight refreshes become stale and a new one may start.
    pub(crate) fn new_generation(&mut self) {
        self.generation += 1;
        self.refreshing = false;
    }

    /// Where the refresh started under `generation` writes the cloud list. Only a
    /// refresh that is still current when it finishes moves it to the cache, so a
    /// refresh of a signed-out account never writes the cache itself.
    fn refresh_file(&self, generation: u64) -> Option<PathBuf> {
        self.paths.cache.as_ref().map(|cache| {
            let mut name = cache.as_os_str().to_owned();
            name.push(format!(".{generation}.part"));
            PathBuf::from(name)
        })
    }

    fn remove_refresh_file(&mut self, generation: u64) {
        if let Some(path) = self.refresh_file(generation) {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => self.add_banner(format!("Could not remove {}: {e}", path.display())),
            }
        }
    }

    /// Make the list the current refresh wrote the cache.
    fn adopt_refresh_file(&mut self, generation: u64) {
        let (Some(part), Some(cache)) = (self.refresh_file(generation), self.paths.cache.clone())
        else {
            return;
        };
        match std::fs::rename(&part, &cache) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => self.add_banner(format!("Could not save the cloud list cache: {e}")),
        }
    }

    pub(crate) fn remove_cache(&mut self) {
        if let Some(path) = &self.paths.cache {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => self.add_banner(format!("Could not remove the cloud list cache: {e}")),
            }
        }
    }

    fn rows(&self) -> Vec<Row> {
        let busy = Busy {
            running: self.running.as_ref(),
            cancellable: self.running_cancellable(),
            queue: &self.queue,
            returning: &self.returning,
            row_errors: &self.row_errors,
        };
        self.items
            .iter()
            .map(|item| rows::row(item, &busy))
            .collect()
    }

    fn status(&self) -> Status {
        let root = &self.library.root;
        let mut status = rows::status(&StatusInputs {
            user: self.user.as_deref(),
            refreshing: self.refreshing,
            banner: &self.banner,
            books_dir: (!root.as_os_str().is_empty()).then_some(root.as_path()),
        });
        status.loading_providers = self.loading_providers;
        status.signing_in = self.signing_in;
        status.sign_in_error = self.sign_in_error.clone();
        let info = self.catalog.service_info();
        status.sign_in_ready =
            info.auth_url.is_some() && info.certificate.as_deref().is_some_and(|c| !c.is_empty());
        status.importing = matches!(self.running, Some(Job::Import(_)));
        status.job_running = self.job_running();
        status.can_sync_all = self.user.is_some() && !self.unsynced().is_empty();
        status.can_cancel_all =
            self.running_cancellable() || self.queue.iter().any(|job| matches!(job, Job::Sync(_)));
        status
    }

    /// Recompute rows and status, append what changed since the last emit to `events`,
    /// and remember them as emitted.
    fn publish(&mut self, events: &mut Vec<Event>) {
        let rows = self.rows();
        let status = self.status();
        events.extend(crate::diff::diff(&self.emitted_rows, &rows));
        if status != self.emitted_status {
            events.push(Event::Status {
                status: status.clone(),
            });
        }
        self.emitted_rows = rows;
        self.emitted_status = status;
    }

    /// What the listener has last been told, plus the settings.
    pub(crate) fn snapshot(&self) -> Snapshot {
        Snapshot {
            rows: self.emitted_rows.clone(),
            status: self.emitted_status.clone(),
            settings: settings(&self.settings),
        }
    }
}

/// The public form of the stored settings.
fn settings(settings: &library::Settings) -> Settings {
    Settings {
        books_dir: settings
            .books_dir
            .as_deref()
            .map(|dir| dir.to_string_lossy().into_owned()),
        theme: match settings.theme {
            library::Theme::System => Theme::System,
            library::Theme::Light => Theme::Light,
            library::Theme::Dark => Theme::Dark,
        },
    }
}

/// Ask the running sync to stop; no effect once its Fulfill was sent.
fn request_cancel(state: &State) {
    if !state.running_cancellable() {
        return;
    }
    if let Some(token) = &state.cancel {
        token.cancel();
    }
}

/// The reactor: applies every `Msg` to the state and tells the listener what changed.
pub(crate) struct Reactor {
    pub state: Arc<Mutex<State>>,
    pub ops: Arc<dyn Ops>,
    /// The sender `App` owns. Weak, so the channel closes once `App` and all workers are gone.
    pub tx: Weak<Sender<Msg>>,
    pub listener: Box<dyn Listener>,
}

impl Reactor {
    /// Handle messages until the channel closes. Only this loop calls the listener, and
    /// never while holding the state lock.
    pub(crate) fn run(self, rx: Receiver<Msg>) {
        for msg in rx {
            let events = {
                let mut state = lock(&self.state);
                let mut events = Vec::new();
                self.apply(&mut state, msg, &mut events);
                state.publish(&mut events);
                events
            };
            for event in events {
                // A panicking listener must not stop the reactor; the next event still goes out.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.listener.on_event(event)
                }));
            }
        }
    }

    /// Apply `msg` to `state`. Handlers may push events that are not row or status
    /// changes, such as progress, to `events`.
    fn apply(&self, state: &mut State, msg: Msg, events: &mut Vec<Event>) {
        match msg {
            Msg::Refresh => self.refresh(state),
            Msg::Refreshed { generation, .. }
                if generation != state.generation || state.user.is_none() =>
            {
                // Started before a sign-in or sign-out: drop its list, and the file it
                // wrote, without touching the cache. The user refreshes again if wanted.
                state.remove_refresh_file(generation);
            }
            Msg::Refreshed { generation, result } => {
                state.refreshing = false;
                match result {
                    Ok(cloud) => {
                        state.adopt_refresh_file(generation);
                        state.cloud = cloud;
                        state.remerge();
                    }
                    Err(e) => {
                        state.remove_refresh_file(generation);
                        state.add_banner(format!("Refresh failed: {e}"));
                    }
                }
            }
            Msg::Sync(key) => self.enqueue_sync(state, key),
            Msg::SyncAll => {
                for key in state.unsynced() {
                    self.enqueue_sync(state, key);
                }
            }
            Msg::Cancel(key) => {
                state
                    .queue
                    .retain(|job| !matches!(job, Job::Sync(k) if *k == key));
                if matches!(&state.running, Some(Job::Sync(k)) if *k == key) {
                    request_cancel(state);
                }
            }
            Msg::CancelAll => {
                state.queue.retain(|job| !matches!(job, Job::Sync(_)));
                if matches!(state.running, Some(Job::Sync(_))) {
                    request_cancel(state);
                }
            }
            Msg::Import(_) if state.user.is_none() => {
                state.add_banner("Sign in before importing an ACSM.".into());
            }
            Msg::Import(path) => self.enqueue(state, Job::Import(path)),
            Msg::Progress { key, fraction } => {
                // Only the running sync reports; anything else is stale.
                if matches!(&state.running, Some(Job::Sync(k)) if *k == key) {
                    events.push(Event::Progress { key, fraction });
                }
            }
            Msg::Committed { key } => {
                if matches!(&state.running, Some(Job::Sync(k)) if *k == key) {
                    state.committed = true;
                }
            }
            Msg::Synced { key, result } => {
                state.running = None;
                state.committed = false;
                let cancelled = state.cancel.take().is_some_and(|t| t.is_cancelled());
                match result {
                    Ok(outcome) => {
                        state.failures.remove(&key);
                        if let Some(warning) = outcome.warning {
                            state.add_banner(warning);
                        }
                    }
                    // A cancelled sync leaves the row as it was, not Failed.
                    Err(_) if cancelled => {}
                    Err(e) => {
                        state.failures.insert(key, e);
                    }
                }
                state.rescan();
                self.start_next(state);
            }
            Msg::ReturnLoan(key) => self.start_return(state, key),
            Msg::Remove {
                key,
                return_loan_first,
            } => {
                if return_loan_first {
                    if state.items.iter().any(|item| item.key == key) {
                        // Only the folder confirmed now is deleted after the return.
                        if let Some(folder) = state.folder_of(&key) {
                            state.remove_after_return.insert(key.clone(), folder);
                        }
                        self.start_return(state, key);
                    }
                } else {
                    state.remove_folder(&key);
                }
            }
            Msg::Returned { key, result } => {
                state.returning.remove(&key);
                let remove = state.remove_after_return.remove(&key);
                match result {
                    Ok(_) if let Some(folder) = remove => state.remove_path(&key, &folder),
                    Ok(_) => {
                        state.row_errors.remove(&key);
                    }
                    Err(e) => {
                        state.row_errors.insert(key, e);
                    }
                }
                state.rescan();
            }
            Msg::AddMessage(message) => state.add_banner(message),
            Msg::ClearBanner => state.banner.clear(),
            Msg::Publish => {}
            Msg::LoadProviders => {
                if state.loading_providers {
                    return;
                }
                state.loading_providers = true;
                let ops = self.ops.clone();
                self.spawn(move || ops.load_providers(), Msg::Providers);
            }
            Msg::Providers(result) => {
                state.loading_providers = false;
                events.push(Event::Providers {
                    result: match result {
                        Ok(catalog) => {
                            let providers = catalog
                                .entries()
                                .iter()
                                .map(|vendor| Provider {
                                    name: vendor.name.clone(),
                                    method: vendor.method.clone(),
                                    node_id: vendor.node_id.clone(),
                                    sign_in_type: vendor.sign_in_type.clone(),
                                })
                                .collect();
                            state.catalog = Arc::new(catalog);
                            Ok(providers)
                        }
                        // The previous catalog stays.
                        Err(e) => Err(e),
                    },
                });
            }
            Msg::SignIn {
                method,
                username,
                password,
            } => {
                if state.signing_in || state.signing_out || state.user.is_some() {
                    return;
                }
                state.sign_in_error = None;
                let info = state.catalog.service_info();
                if info.auth_url.is_none() || info.certificate.as_deref().is_none_or(str::is_empty)
                {
                    state.sign_in_error =
                        Some("Discovery has no authentication URL or certificate yet.".into());
                    return;
                }
                if username.is_empty() || password.is_empty() {
                    state.sign_in_error = Some("Enter a username and password.".into());
                    return;
                }
                state.signing_in = true;
                let catalog = state.catalog.clone();
                let ops = self.ops.clone();
                // The credentials move into the worker and are dropped there.
                self.spawn(
                    move || ops.sign_in(&catalog, &method, username, password),
                    Msg::SignedIn,
                );
            }
            Msg::SignedIn(result) => {
                state.signing_in = false;
                match result {
                    Ok(user) => {
                        state.sign_in_error = None;
                        state.user = Some(user);
                        state.new_generation();
                    }
                    Err(e) => state.sign_in_error = Some(format!("Sign-in failed: {e}")),
                }
            }
            Msg::SignOut => {
                if state.signing_in || state.signing_out {
                    return;
                }
                state.signing_out = true;
                let ops = self.ops.clone();
                self.spawn(move || ops.sign_out(), Msg::SignedOut);
            }
            Msg::SignedOut(result) => {
                state.signing_out = false;
                match result {
                    Ok(()) => {
                        state.user = None;
                        state.new_generation();
                        state.remove_cache();
                        state.cloud.clear();
                        // Queued cloud rows have no ACSM once the list is gone, and old
                        // failures would mark the remaining Local rows Failed.
                        state.queue.clear();
                        state.failures.clear();
                        state.row_errors.clear();
                        state.remerge();
                    }
                    Err(e) => state.add_banner(format!("Sign-out failed: {e}")),
                }
            }
            Msg::Imported(result) => {
                state.running = None;
                match result {
                    Ok(outcome) => {
                        state.failures.remove(&outcome.metadata.resource);
                        if let Some(warning) = outcome.warning {
                            state.add_banner(warning);
                        }
                    }
                    Err(e) => state.add_banner(format!("Import failed: {e}")),
                }
                state.rescan();
                self.start_next(state);
            }
        }
    }

    /// Queue a sync of the row with `key`. Keys without a row are ignored.
    fn enqueue_sync(&self, state: &mut State, key: String) {
        if state.items.iter().any(|item| item.key == key) {
            self.enqueue(state, Job::Sync(key));
        }
    }

    /// Queue `job`, unless it is already queued or running, and start it when idle.
    fn enqueue(&self, state: &mut State, job: Job) {
        if state.running.as_ref() == Some(&job) || state.queue.contains(&job) {
            return;
        }
        state.queue.push_back(job);
        self.start_next(state);
    }

    /// Start the front queued job when no sync or import worker runs.
    fn start_next(&self, state: &mut State) {
        if state.running.is_some() {
            return;
        }
        let Some(job) = state.queue.pop_front() else {
            return;
        };
        let library = state.library.clone();
        let ops = self.ops.clone();
        state.running = Some(job.clone());
        state.cancel = None;
        state.committed = false;
        match job {
            Job::Sync(key) => {
                let acsm = state
                    .items
                    .iter()
                    .find(|item| item.key == key)
                    .and_then(|item| item.acsm.clone());
                let cancel = CancelToken::new();
                state.cancel = Some(cancel.clone());
                let tx = self.tx.clone();
                let hook_key = key.clone();
                self.spawn(
                    move || {
                        let acsm = acsm.ok_or_else(|| "no ACSM for this item".to_string())?;
                        if library.root.as_os_str().is_empty() {
                            return Err("no books folder configured".into());
                        }
                        // The library parses the ACSM before any request goes out.
                        let send = |msg: Msg| {
                            if let Some(tx) = tx.upgrade() {
                                let _ = tx.send(msg);
                            }
                        };
                        let progress = |fraction: f32| {
                            send(Msg::Progress {
                                key: hook_key.clone(),
                                fraction,
                            })
                        };
                        let committed = || {
                            send(Msg::Committed {
                                key: hook_key.clone(),
                            })
                        };
                        let hooks = SyncHooks {
                            progress: &progress,
                            committed: &committed,
                        };
                        ops.sync(&library, &acsm, cancel, hooks)
                    },
                    move |result| Msg::Synced { key, result },
                );
            }
            Job::Import(path) => self.spawn(
                move || {
                    if library.root.as_os_str().is_empty() {
                        return Err("no books folder configured".into());
                    }
                    // Reject unreadable or malformed imports before touching the
                    // activation. Decoded as the library decodes it.
                    use std::io::Read;
                    let mut acsm = Vec::new();
                    std::fs::File::open(&path)
                        .map_err(|e| e.to_string())?
                        .take(MAX_ACSM_BYTES as u64 + 1)
                        .read_to_end(&mut acsm)
                        .map_err(|e| e.to_string())?;
                    Acsm::parse(String::from_utf8_lossy(&acsm).into_owned())
                        .map_err(|e| e.to_string())?;
                    ops.import(&library, &path)
                },
                Msg::Imported,
            ),
        }
    }

    /// Start returning the loan of the row with `key`, unless a return already runs.
    fn start_return(&self, state: &mut State, key: String) {
        if state.returning.contains(&key) {
            return;
        }
        let Some(item) = state.items.iter().find(|item| item.key == key).cloned() else {
            return;
        };
        state.row_errors.remove(&key);
        state.returning.insert(key.clone());
        let ops = self.ops.clone();
        self.spawn(
            move || ops.return_loan(&item),
            move |result| Msg::Returned { key, result },
        );
    }

    fn refresh(&self, state: &mut State) {
        if state.refreshing || state.user.is_none() {
            return;
        }
        state.refreshing = true;
        let generation = state.generation;
        let file = state.refresh_file(generation);
        let ops = self.ops.clone();
        self.spawn(
            move || {
                let file = file.ok_or_else(|| "no data directory".to_string())?;
                ops.refresh(&file)
            },
            move |result| Msg::Refreshed { generation, result },
        );
    }

    /// Run `body` on a worker thread and send its result, wrapped by `msg`.
    /// Does nothing once `App` is gone.
    fn spawn<T: Send + 'static>(
        &self,
        body: impl FnOnce() -> Result<T, String> + Send + 'static,
        msg: impl FnOnce(Result<T, String>) -> Msg + Send + 'static,
    ) {
        let Some(tx) = self.tx.upgrade().map(|tx| (*tx).clone()) else {
            return;
        };
        std::thread::spawn(move || {
            let _ = tx.send(msg(catch_panic(body)));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leafport_core::library::{Item, Metadata};
    use leafport_core::vendors::VendorCatalog;
    use std::path::Path;

    /// No side effects; the tests below never reach them.
    struct NoOps;

    impl Ops for NoOps {
        fn signed_in_user(&self) -> Result<Option<String>, String> {
            Ok(Some("user@x".into()))
        }
        fn load_providers(&self) -> Result<VendorCatalog, String> {
            unreachable!()
        }
        fn sign_in(
            &self,
            _: &VendorCatalog,
            _: &str,
            _: String,
            _: zeroize::Zeroizing<String>,
        ) -> Result<String, String> {
            unreachable!()
        }
        fn sign_out(&self) -> Result<(), String> {
            unreachable!()
        }
        fn refresh(&self, _: &Path) -> Result<Vec<DownloadRecord>, String> {
            unreachable!()
        }
        fn sync(
            &self,
            _: &Library,
            _: &[u8],
            _: CancelToken,
            _: SyncHooks,
        ) -> Result<SyncOutcome, String> {
            unreachable!()
        }
        fn import(&self, _: &Library, _: &Path) -> Result<SyncOutcome, String> {
            unreachable!()
        }
        fn return_loan(&self, _: &Item) -> Result<Metadata, String> {
            unreachable!()
        }
    }

    struct Silent;

    impl Listener for Silent {
        fn on_event(&self, _: Event) {}
    }

    /// A reactor and its state, with the books folder in a fresh temp folder.
    fn reactor() -> (PathBuf, Reactor) {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "leafport-app-state-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let settings = dir.join("settings.yaml");
        library::Settings {
            books_dir: Some(dir.join("Books")),
            ..library::Settings::default()
        }
        .save(&settings)
        .unwrap();
        let paths = Paths {
            settings: Some(settings),
            cache: None,
        };
        let state = State::load(&NoOps, paths);
        let reactor = Reactor {
            state: Arc::new(Mutex::new(state)),
            ops: Arc::new(NoOps),
            tx: Weak::new(),
            listener: Box::new(Silent),
        };
        (dir, reactor)
    }

    fn progress(key: &str) -> Msg {
        Msg::Progress {
            key: key.into(),
            fraction: 0.5,
        }
    }

    #[test]
    fn progress_only_for_running_sync() {
        let (dir, reactor) = reactor();
        let mut state = lock(&reactor.state);
        let mut events = Vec::new();

        reactor.apply(&mut state, progress("a"), &mut events);
        assert!(events.is_empty(), "nothing runs");

        state.running = Some(Job::Sync("a".into()));
        reactor.apply(&mut state, progress("b"), &mut events);
        assert!(events.is_empty(), "another key");
        reactor.apply(&mut state, progress("a"), &mut events);
        assert_eq!(
            events,
            [Event::Progress {
                key: "a".into(),
                fraction: 0.5
            }]
        );

        // A late report after the sync finished is dropped.
        events.clear();
        state.running = None;
        reactor.apply(&mut state, progress("a"), &mut events);
        assert!(events.is_empty(), "finished");
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn committed_only_for_running_sync() {
        let (dir, reactor) = reactor();
        let mut state = lock(&reactor.state);
        let mut events = Vec::new();
        state.running = Some(Job::Sync("a".into()));
        state.cancel = Some(CancelToken::new());
        reactor.apply(&mut state, Msg::Committed { key: "b".into() }, &mut events);
        assert!(state.running_cancellable());
        reactor.apply(&mut state, Msg::Committed { key: "a".into() }, &mut events);
        assert!(!state.running_cancellable());
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn remove_outside_root_refused() {
        let (dir, reactor) = reactor();
        let mut state = lock(&reactor.state);
        let nested = state.library.root.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        state.items.push(Item {
            key: "k".into(),
            title: "T".into(),
            author: None,
            format: leafport_core::library::Format::Epub,
            state: ItemState::Synced,
            acsm: None,
            folder: Some(nested.clone()),
            publication: None,
            cover: None,
            metadata: None,
            warning: None,
        });
        state.remove_folder("k");
        assert_eq!(
            state.banner,
            [format!(
                "Not removing {}: outside the books folder",
                nested.display()
            )]
        );
        assert!(nested.exists());
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A theme-only change keeps a books folder whose path is not valid UTF-8.
    #[cfg(unix)]
    #[test]
    fn non_utf8_books_dir_survives_theme_change() {
        use std::os::unix::ffi::OsStrExt;
        let (dir, reactor) = reactor();
        let mut state = lock(&reactor.state);
        let books = dir.join(std::ffi::OsStr::from_bytes(b"B\xffooks"));
        std::fs::create_dir_all(&books).unwrap();
        state.settings.books_dir = Some(books.clone());
        state.library = Library::open(&books).unwrap();
        let shown = settings(&state.settings);
        let _ = state.apply_settings(Settings {
            theme: Theme::Dark,
            ..shown
        });
        assert_eq!(state.settings.books_dir, Some(books.clone()));
        assert_eq!(state.library.root, books);
        assert_eq!(state.settings.theme, library::Theme::Dark);
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }
}
