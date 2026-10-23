//! UI-free application logic shared by the native Leafport frontends.

mod diff;
mod ops;
mod rows;
mod state;
mod types;

use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

pub use ops::{Ops, Paths, SyncHooks};
pub use types::{
    Activity, AppError, Event, Listener, MenuAction, Primary, Provider, Row, RowState, Settings,
    Snapshot, Status, Theme,
};

use state::{Msg, Reactor, State};

/// The app. Every method returns immediately; changes reach the `Listener` as events.
pub struct App {
    state: Arc<Mutex<State>>,
    /// The reactor holds this weakly, so it exits once the app and its workers are gone.
    tx: Arc<Sender<Msg>>,
}

impl App {
    /// Load the settings, books folder, cloud list cache and account through `ops`, and
    /// start the reactor thread that reports changes to `listener`.
    pub fn with_ops(ops: Arc<dyn Ops>, paths: Paths, listener: Box<dyn Listener>) -> Arc<App> {
        let state = Arc::new(Mutex::new(State::load(ops.as_ref(), paths)));
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
        Arc::new(App { state, tx })
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

    /// Queue every Available or Failed row that has an ACSM and no book file yet.
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

    /// Queue an import of the ACSM file at `path`. Signed out, the banner asks to sign in.
    pub fn import_acsm(&self, path: String) {
        self.send(Msg::Import(path.into()));
    }

    fn send(&self, msg: Msg) {
        // The reactor only stops once the app is gone, so this cannot fail while `self` lives.
        let _ = self.tx.send(msg);
    }
}
