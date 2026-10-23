//! The app state, the messages that change it, and the reactor thread that applies them.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use leafport_core::cloud::DownloadRecord;
use leafport_core::library::{self, Item, ItemState, Library, load_cache, merge};

use crate::ops::{Ops, Paths};
use crate::rows::{self, Busy, Job, StatusInputs};
use crate::{Event, Listener, Row, Settings, Snapshot, Status, Theme};

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
    /// Rows whose loan return runs.
    pub returning: HashSet<String>,
    /// Last return error per row key. Kept apart from `failures`, so the row stays OnLoan.
    pub row_errors: HashMap<String, String>,
    /// Recent unique messages, oldest first.
    pub banner: Vec<String>,
    /// The signed-in account label.
    pub user: Option<String>,
    pub refreshing: bool,
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
            returning: HashSet::new(),
            row_errors: HashMap::new(),
            banner,
            user,
            refreshing: false,
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

    pub(crate) fn add_banner(&mut self, message: String) {
        add_banner(&mut self.banner, message);
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
            cancellable: false,
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
        rows::status(&StatusInputs {
            user: self.user.as_deref(),
            refreshing: self.refreshing,
            banner: &self.banner,
            books_dir: (!root.as_os_str().is_empty()).then_some(root.as_path()),
        })
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
                self.listener.on_event(event);
            }
        }
    }

    /// Apply `msg` to `state`. Handlers may push events that are not row or status
    /// changes, such as progress, to `events`.
    fn apply(&self, state: &mut State, msg: Msg, _events: &mut Vec<Event>) {
        match msg {
            Msg::Refresh => self.refresh(state),
            Msg::Refreshed { generation, .. }
                if generation != state.generation || state.user.is_none() =>
            {
                // Started before a sign-in or sign-out: it may have rewritten the cache
                // after sign-out removed it. Drop it; the user refreshes again if wanted.
                state.remove_cache();
            }
            Msg::Refreshed { result, .. } => {
                state.refreshing = false;
                match result {
                    Ok(cloud) => {
                        state.cloud = cloud;
                        state.remerge();
                    }
                    Err(e) => state.add_banner(format!("Refresh failed: {e}")),
                }
            }
        }
    }

    fn refresh(&self, state: &mut State) {
        if state.refreshing || state.user.is_none() {
            return;
        }
        state.refreshing = true;
        let generation = state.generation;
        let cache: Option<PathBuf> = state.paths.cache.clone();
        let ops = self.ops.clone();
        self.spawn(
            move || {
                let cache = cache.ok_or_else(|| "no data directory".to_string())?;
                ops.refresh(&cache)
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
