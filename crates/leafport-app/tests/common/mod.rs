//! Shared test helpers: temp folders, cache fixtures, a recording listener and a
//! scripted `Ops`.

// Each test binary uses a different subset of these helpers.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use leafport_app::{Account, Event, Listener, Ops, Paths, Status, SyncHooks};
use leafport_core::cloud::DownloadRecord;
use leafport_core::library::{
    Item, Library, Metadata, Settings, Source, SyncOutcome, acsm_summary,
};
use leafport_core::service::CancelToken;
use leafport_core::vendors::VendorCatalog;

/// How long `wait_for` waits before failing the test.
const WAIT: Duration = Duration::from_secs(5);

/// A temporary folder, removed on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn tempdir() -> TempDir {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "leafport-app-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

/// An ACSM for `resource` with operatorURL `https://op.example/fulfillment`.
pub fn acsm_xml(resource: &str, title: &str) -> String {
    format!(
        r#"<fulfillmentToken xmlns="http://ns.adobe.com/adept" xmlns:dc="http://purl.org/dc/elements/1.1/"><distributor>urn:uuid:distributor</distributor><operatorURL>https://op.example/fulfillment</operatorURL><transaction>T-1</transaction><resourceItemInfo><resource>{resource}</resource><metadata><dc:title>{title}</dc:title><dc:format>application/epub+zip</dc:format></metadata></resourceItemInfo></fulfillmentToken>"#
    )
}

/// A cloud record whose ACSM names `resource` and `title`.
pub fn record(resource: &str, title: &str) -> DownloadRecord {
    DownloadRecord {
        user: "u".into(),
        acsm: acsm_xml(resource, title).into_bytes(),
    }
}

/// A SyncToDevice response listing one ACSM per `(resource, title)`.
pub fn cache_xml(items: &[(&str, &str)]) -> String {
    let entries: String = items
        .iter()
        .map(|(resource, title)| {
            format!(
                "<user2acsm><user>u</user><acsm>{}</acsm></user2acsm>",
                STANDARD.encode(acsm_xml(resource, title))
            )
        })
        .collect();
    format!(
        r#"<synctodeviceResult xmlns="http://ns.adobe.com/adept">{entries}</synctodeviceResult>"#
    )
}

/// Paths inside `dir`: `settings.yaml` with `books_dir = dir/Books`, and `cache.xml`.
pub fn settings_in(dir: &Path) -> Paths {
    let settings = dir.join("settings.yaml");
    Settings {
        books_dir: Some(dir.join("Books")),
        ..Settings::default()
    }
    .save(&settings)
    .unwrap();
    Paths {
        settings: Some(settings),
        cache: Some(dir.join("cache.xml")),
    }
}

/// Write a synced book folder `books/title` for `resource`, as a finished sync leaves it,
/// and return the outcome that sync reports.
pub fn write_book(books: &Path, resource: &str, title: &str) -> SyncOutcome {
    let folder = books.join(title);
    std::fs::create_dir_all(&folder).unwrap();
    let metadata = Metadata {
        resource: resource.into(),
        title: title.into(),
        author: None,
        format: "application/epub+zip".into(),
        fulfillment: None,
        source: Source::Cloud,
        synced_at: Some("2026-10-23T10:00:00Z".into()),
        excluded: false,
        returned: false,
        loan: None,
    };
    metadata.write(&folder).unwrap();
    std::fs::write(folder.join(format!("{title}.epub")), b"x").unwrap();
    SyncOutcome {
        folder,
        metadata,
        warning: None,
    }
}

/// A listener that keeps every event and lets tests wait for one.
#[derive(Clone, Default)]
pub struct Recorder(Arc<(Mutex<Vec<Event>>, Condvar)>);

impl Recorder {
    pub fn new() -> Recorder {
        Recorder::default()
    }

    /// Every event received so far.
    pub fn events(&self) -> Vec<Event> {
        self.0.0.lock().unwrap().clone()
    }

    /// Wait until the app has applied the provider load a signed-in app starts with,
    /// then forget the events so far, so a test sees only the events it causes.
    pub fn settle_startup(&self) {
        self.wait_for(|events| {
            events
                .iter()
                .any(|event| matches!(event, Event::Providers { .. }))
                && last_status(events).is_some_and(|status| !status.loading_providers)
        });
        self.0.0.lock().unwrap().clear();
    }

    /// Block until `pred` holds for the events received so far. Panics after 5 s.
    pub fn wait_for(&self, pred: impl Fn(&[Event]) -> bool) -> Vec<Event> {
        let (events, cond) = &*self.0;
        let deadline = Instant::now() + WAIT;
        let mut guard = events.lock().unwrap();
        while !pred(&guard) {
            let now = Instant::now();
            if now >= deadline {
                panic!("timed out waiting for events; got {:#?}", *guard);
            }
            guard = cond.wait_timeout(guard, deadline - now).unwrap().0;
        }
        guard.clone()
    }

    /// The most recent `Event::Status`. Panics when none was received.
    pub fn last_status(&self) -> Status {
        last_status(&self.events()).expect("no status event yet")
    }
}

/// The most recent status in `events`.
pub fn last_status(events: &[Event]) -> Option<Status> {
    events.iter().rev().find_map(|event| match event {
        Event::Status { status } => Some(status.clone()),
        _ => None,
    })
}

impl Listener for Recorder {
    fn on_event(&self, event: Event) {
        let (events, cond) = &*self.0;
        events.lock().unwrap().push(event);
        cond.notify_all();
    }
}

/// An operation `FakeOps` was asked to run.
#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    LoadProviders,
    SignIn {
        method: String,
        username: String,
    },
    SignOut,
    Refresh,
    /// The ACSM key (its resource) of the sync.
    Sync(String),
    Import(PathBuf),
    ReturnLoan(String),
}

/// One scripted step of a sync.
// One step is sent at a time, so the large `Done` variant costs nothing.
#[allow(clippy::large_enum_variant)]
pub enum SyncStep {
    /// Report download progress through the hooks.
    Progress(f32),
    /// Report that Fulfill is about to be sent.
    Commit,
    /// Finish the sync with this result.
    Done(Result<SyncOutcome, String>),
}

/// The error a fake operation returns once its test dropped the reply sender.
const CLOSED: &str = "fake ops closed";

/// A scripted `Ops`. Each operation reports a `Call` and blocks until the test replies
/// through the matching sender of its `Script`.
pub struct FakeOps {
    user: Result<Option<String>, String>,
    calls: Mutex<Sender<Call>>,
    providers: Mutex<Receiver<Result<VendorCatalog, String>>>,
    sign_in: Mutex<Receiver<Result<String, String>>>,
    sign_out: Mutex<Receiver<Result<(), String>>>,
    refresh: Mutex<Receiver<Result<Vec<DownloadRecord>, String>>>,
    sync: Mutex<Receiver<SyncStep>>,
    import: Mutex<Receiver<Result<SyncOutcome, String>>>,
    return_loan: Mutex<Receiver<Result<Metadata, String>>>,
    /// The cancel token of the most recent sync.
    token: Mutex<Option<CancelToken>>,
    /// Whether the provider load a signed-in app starts with is still to come.
    startup_load: AtomicBool,
}

/// The test side of a `FakeOps`: the calls it saw and the replies to send.
pub struct Script {
    pub calls: Receiver<Call>,
    pub providers: Sender<Result<VendorCatalog, String>>,
    pub sign_in: Sender<Result<String, String>>,
    pub sign_out: Sender<Result<(), String>>,
    pub refresh: Sender<Result<Vec<DownloadRecord>, String>>,
    pub sync: Sender<SyncStep>,
    pub import: Sender<Result<SyncOutcome, String>>,
    pub return_loan: Sender<Result<Metadata, String>>,
}

impl Script {
    /// The next call. Panics after 5 s.
    pub fn next_call(&self) -> Call {
        self.calls.recv_timeout(WAIT).expect("no call within 5 s")
    }

    /// Assert that no call arrives within `ms` milliseconds.
    pub fn no_call_within(&self, ms: u64) {
        if let Ok(call) = self.calls.recv_timeout(Duration::from_millis(ms)) {
            panic!("unexpected call {call:?}");
        }
    }
}

impl FakeOps {
    /// A fake whose `signed_in_user` returns `user`.
    pub fn new(user: Option<&str>) -> (Arc<FakeOps>, Script) {
        Self::with_user(Ok(user.map(str::to_owned)))
    }

    /// A fake whose `signed_in_user` returns `user`, which may be an error.
    pub fn with_user(user: Result<Option<String>, String>) -> (Arc<FakeOps>, Script) {
        let (calls_tx, calls) = mpsc::channel();
        let (providers, providers_rx) = mpsc::channel();
        let (sign_in, sign_in_rx) = mpsc::channel();
        let (sign_out, sign_out_rx) = mpsc::channel();
        let (refresh, refresh_rx) = mpsc::channel();
        let (sync, sync_rx) = mpsc::channel();
        let (import, import_rx) = mpsc::channel();
        let (return_loan, return_loan_rx) = mpsc::channel();
        let ops = FakeOps {
            startup_load: AtomicBool::new(matches!(user, Ok(Some(_)))),
            user,
            calls: Mutex::new(calls_tx),
            providers: Mutex::new(providers_rx),
            sign_in: Mutex::new(sign_in_rx),
            sign_out: Mutex::new(sign_out_rx),
            refresh: Mutex::new(refresh_rx),
            sync: Mutex::new(sync_rx),
            import: Mutex::new(import_rx),
            return_loan: Mutex::new(return_loan_rx),
            token: Mutex::new(None),
        };
        let script = Script {
            calls,
            providers,
            sign_in,
            sign_out,
            refresh,
            sync,
            import,
            return_loan,
        };
        (Arc::new(ops), script)
    }

    /// The cancel token the most recent sync was given. Panics when none ran.
    pub fn last_token(&self) -> CancelToken {
        self.token.lock().unwrap().clone().expect("no sync ran")
    }

    fn call(&self, call: Call) {
        let _ = self.calls.lock().unwrap().send(call);
    }
}

/// Report `call`, then wait for the reply on `replies`.
fn reply<T>(
    ops: &FakeOps,
    call: Call,
    replies: &Mutex<Receiver<Result<T, String>>>,
) -> Result<T, String> {
    ops.call(call);
    replies
        .lock()
        .unwrap()
        .recv()
        .unwrap_or_else(|_| Err(CLOSED.into()))
}

/// An account with only an ID, as the fake reports it.
fn account(id: String) -> Account {
    Account {
        id,
        username: None,
        method: None,
    }
}

impl Ops for FakeOps {
    fn signed_in_user(&self) -> Result<Option<Account>, String> {
        self.user.clone().map(|user| user.map(account))
    }

    fn load_providers(&self) -> Result<VendorCatalog, String> {
        // A signed-in app loads the providers at startup. That load gets the built-in
        // providers without a call, so tests script only the loads they start.
        if self.startup_load.swap(false, Ordering::SeqCst) {
            return Ok(VendorCatalog::new());
        }
        reply(self, Call::LoadProviders, &self.providers)
    }

    fn sign_in(
        &self,
        _catalog: &VendorCatalog,
        method: &str,
        username: String,
        _password: zeroize::Zeroizing<String>,
    ) -> Result<Account, String> {
        let call = Call::SignIn {
            method: method.into(),
            username,
        };
        reply(self, call, &self.sign_in).map(account)
    }

    fn sign_out(&self) -> Result<(), String> {
        reply(self, Call::SignOut, &self.sign_out)
    }

    fn refresh(&self, _cache: &Path) -> Result<Vec<DownloadRecord>, String> {
        reply(self, Call::Refresh, &self.refresh)
    }

    fn sync(
        &self,
        _library: &Library,
        acsm: &[u8],
        cancel: CancelToken,
        hooks: SyncHooks,
    ) -> Result<SyncOutcome, String> {
        let key = acsm_summary(acsm).map_err(|e| e.to_string())?.key;
        *self.token.lock().unwrap() = Some(cancel);
        self.call(Call::Sync(key));
        let steps = self.sync.lock().unwrap();
        loop {
            match steps.recv() {
                Ok(SyncStep::Progress(fraction)) => (hooks.progress)(fraction),
                Ok(SyncStep::Commit) => (hooks.committed)(),
                Ok(SyncStep::Done(result)) => return result,
                Err(_) => return Err(CLOSED.into()),
            }
        }
    }

    fn import(&self, _library: &Library, path: &Path) -> Result<SyncOutcome, String> {
        reply(self, Call::Import(path.to_owned()), &self.import)
    }

    fn return_loan(&self, item: &Item) -> Result<Metadata, String> {
        reply(self, Call::ReturnLoan(item.key.clone()), &self.return_loan)
    }
}
