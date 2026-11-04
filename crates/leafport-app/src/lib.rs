//! UI-free application logic shared by the native Leafport frontends.

mod diff;
mod live;
mod notice;
mod ops;
mod rows;
mod state;
mod types;

use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

pub use ops::{Account, Ops, Paths, SyncHooks};
pub use types::{
    Activity, AppError, Event, Listener, MenuAction, Operation, OperationKind, Primary, Provider,
    Row, RowState, Settings, Snapshot, Status, Theme,
};

use live::LiveOps;
use state::{Msg, Reactor, State};

/// The app. Every method returns immediately; changes reach the `Listener` as events.
pub struct App {
    state: Arc<Mutex<State>>,
    /// The reactor holds this weakly, so it exits once the app and its workers are gone.
    tx: Arc<Sender<Msg>>,
}

impl App {
    /// Start the app against the real network and credential store, with the platform's
    /// settings and cache locations.
    pub fn new(listener: Box<dyn Listener>) -> Arc<App> {
        App::with_ops(Arc::new(LiveOps), Paths::platform(), listener)
    }

    /// Load the settings, books folder, cloud list cache and account through `ops`, and
    /// start the reactor thread that reports changes to `listener`.
    pub fn with_ops(ops: Arc<dyn Ops>, paths: Paths, listener: Box<dyn Listener>) -> Arc<App> {
        let state = State::load(ops.as_ref(), paths);
        let signed_in = state.account.is_some();
        let state = Arc::new(Mutex::new(state));
        let (tx, rx) = mpsc::channel();
        let tx = Arc::new(tx);
        let reactor = Reactor {
            state: state.clone(),
            ops,
            tx: Arc::downgrade(&tx),
            listener,
        };
        std::thread::Builder::new()
            .name("leafport-reactor".into())
            .spawn(move || reactor.run(rx))
            .expect("spawn the reactor thread");
        let app = Arc::new(App { state, tx });
        // The provider list names the signed-in account's provider.
        if signed_in {
            app.load_providers();
        }
        app
    }

    /// The rows and status last sent to the listener, and the settings.
    pub fn snapshot(&self) -> Snapshot {
        state::lock(&self.state).snapshot()
    }

    /// Fetch the cloud list. Does nothing while signed out or while a refresh runs.
    pub fn refresh(&self) {
        self.send(Msg::Refresh);
    }

    /// Queue a sync of the row with `key`. Ignored when it is already queued or syncing,
    /// or no row has that key.
    pub fn sync(&self, key: String) {
        self.send(Msg::Sync(key));
    }

    /// Queue every Available or Failed row that has an ACSM and no book file yet. Excluded
    /// rows are skipped.
    pub fn sync_all(&self) {
        self.send(Msg::SyncAll);
    }

    /// Drop the sync of `key` from the queue, or cancel it while it runs, unless its
    /// Fulfill was sent.
    pub fn cancel(&self, key: String) {
        self.send(Msg::Cancel(key));
    }

    /// Drop every queued sync and cancel the running one, unless its Fulfill was sent.
    /// Imports are left alone.
    pub fn cancel_all(&self) {
        self.send(Msg::CancelAll);
    }

    /// Drop the queued import of the ACSM file at `path`. A running import cannot be
    /// cancelled.
    pub fn cancel_import(&self, path: String) {
        self.send(Msg::CancelImport(path));
    }

    /// Queue an import of the ACSM file at `path`. Signed out, the banner asks to sign in.
    pub fn import_acsm(&self, path: String) {
        self.send(Msg::Import(path.into()));
    }

    /// Return the loan of the row with `key`. Unknown keys are ignored.
    pub fn return_loan(&self, key: String) {
        self.send(Msg::ReturnLoan(key));
    }

    /// Exclude the row with `key` from Sync All, or include it again. The mark is kept
    /// in its title folder, which excluding creates when needed. Excluding also drops a
    /// queued sync of it; a running sync is left alone and the call does nothing.
    pub fn set_excluded(&self, key: String, excluded: bool) {
        self.send(Msg::SetExcluded { key, excluded });
    }

    /// Save `settings` and, when the books folder changed, open the new one. Applying the
    /// theme is up to the UI.
    ///
    /// Unlike the other methods this one returns its result, so it applies the change
    /// itself under the state lock instead of through the reactor. It then sends a
    /// message so the reactor publishes the changed rows and status: the reactor is still
    /// the only thread that emits events. It never waits for the reactor, so it may be
    /// called from `Listener::on_event`.
    ///
    /// Returns `AppError::Busy` and saves nothing when the books folder changes while a
    /// sync, import or loan return runs, and `AppError::Save` when the file cannot be written (the
    /// settings still apply for this session).
    pub fn set_settings(&self, settings: Settings) -> Result<(), AppError> {
        let result = state::lock(&self.state).apply_settings(settings);
        self.send(Msg::Publish);
        result
    }

    /// Load the sign-in providers; the result arrives as `Event::Providers`. Does nothing
    /// while a load runs. A failed load keeps the previous providers.
    pub fn load_providers(&self) {
        self.send(Msg::LoadProviders);
    }

    /// Sign in with the provider `method`. Needs loaded providers; otherwise
    /// `Status.sign_in_error` says so. Does nothing while signed in or while a sign-in or
    /// sign-out runs. The password is passed on to the worker and never kept; its memory
    /// is wiped once the worker drops it.
    pub fn sign_in(&self, method: String, username: String, password: String) {
        self.send(Msg::SignIn {
            method,
            username,
            password: zeroize::Zeroizing::new(password),
        });
    }

    /// Forget the stored activation. On success the cloud list, its cache, the queued
    /// syncs and the failures are cleared; synced folders stay as local rows. On failure
    /// the user stays signed in and the banner says why. Does nothing while a sign-in or
    /// sign-out runs.
    pub fn sign_out(&self) {
        self.send(Msg::SignOut);
    }

    /// Empty the banner.
    pub fn clear_banner(&self) {
        self.send(Msg::ClearBanner);
    }

    /// Add a message to the banner, for failures the UI hits itself.
    pub fn add_message(&self, message: String) {
        self.send(Msg::AddMessage(message));
    }

    fn send(&self, msg: Msg) {
        // The reactor only stops once the app is gone, so this cannot fail while `self` lives.
        let _ = self.tx.send(msg);
    }
}
