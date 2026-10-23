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

    fn send(&self, msg: Msg) {
        // The reactor only stops once the app is gone, so this cannot fail while `self` lives.
        let _ = self.tx.send(msg);
    }
}
