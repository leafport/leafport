mod common;

use std::sync::{Arc, OnceLock, Weak};

use common::{
    Call, FakeOps, Recorder, Script, SyncStep, TempDir, cache_xml, record, settings_in, tempdir,
    write_book,
};
use leafport_app::{
    Activity, App, AppError, Event, Listener, Row, RowState, Settings, Status, Theme,
};
use leafport_core::library::{Loan, Metadata};

/// A temp folder with settings and a cache listing `cached`, and an app on a fake.
fn start(user: Option<&str>, cached: &[(&str, &str)]) -> (TempDir, Arc<App>, Script, Recorder) {
    let dir = tempdir();
    let paths = settings_in(dir.path());
    std::fs::write(paths.cache.as_ref().unwrap(), cache_xml(cached)).unwrap();
    let (ops, script) = FakeOps::new(user);
    let recorder = Recorder::new();
    let app = App::with_ops(ops, paths, Box::new(recorder.clone()));
    (dir, app, script, recorder)
}

fn titles(event: &Event) -> Option<Vec<String>> {
    match event {
        Event::Rows { rows } => Some(rows.iter().map(|r| r.title.clone()).collect()),
        _ => None,
    }
}

fn has_status(events: &[Event], pred: impl Fn(&Status) -> bool) -> bool {
    common::last_status(events).is_some_and(|s| pred(&s))
}

/// Run a refresh that replaces the list with A and B, checking `refreshing` around it.
fn refresh_to_a_b(app: &App, script: &Script, recorder: &Recorder) {
    app.refresh();
    assert_eq!(script.next_call(), common::Call::Refresh);
    recorder.wait_for(|e| has_status(e, |s| s.refreshing));
    script
        .refresh
        .send(Ok(vec![
            record("urn:uuid:1", "A"),
            record("urn:uuid:2", "B"),
        ]))
        .unwrap();
    let events = recorder.wait_for(|e| {
        e.iter().filter_map(titles).any(|t| t == ["A", "B"]) && has_status(e, |s| !s.refreshing)
    });
    assert!(!common::last_status(&events).unwrap().refreshing);
}

#[test]
fn startup_snapshot_reads_cache() {
    let (_dir, app, script, _recorder) = start(Some("user@x"), &[("urn:uuid:1", "A")]);
    let snapshot = app.snapshot();
    assert_eq!(snapshot.rows.len(), 1);
    assert_eq!(snapshot.rows[0].state, RowState::Available);
    assert_eq!(snapshot.status.user.as_deref(), Some("user@x"));
    script.no_call_within(200);
}

#[test]
fn startup_credential_error_goes_to_banner() {
    let dir = tempdir();
    let paths = settings_in(dir.path());
    let (ops, _script) = FakeOps::with_user(Err("locked".into()));
    let app = App::with_ops(ops, paths, Box::new(Recorder::new()));
    let status = app.snapshot().status;
    assert_eq!(status.user, None);
    assert_eq!(status.banner, ["Credential store unavailable: locked"]);
    assert_eq!(
        status.books_dir,
        Some(dir.path().join("Books").to_string_lossy().into_owned())
    );
}

#[test]
fn refresh_replaces_rows() {
    let (_dir, app, script, recorder) = start(Some("user@x"), &[]);
    refresh_to_a_b(&app, &script, &recorder);
    let titles: Vec<_> = app.snapshot().rows.into_iter().map(|r| r.title).collect();
    assert_eq!(titles, ["A", "B"]);
}

#[test]
fn refresh_error_goes_to_banner() {
    let (_dir, app, script, recorder) = start(Some("user@x"), &[("urn:uuid:1", "A")]);
    let before = app.snapshot().rows;
    app.refresh();
    assert_eq!(script.next_call(), common::Call::Refresh);
    script.refresh.send(Err("offline".into())).unwrap();
    let events = recorder.wait_for(|e| has_status(e, |s| !s.banner.is_empty()));
    let status = common::last_status(&events).unwrap();
    assert_eq!(status.banner, ["Refresh failed: offline"]);
    assert!(!status.refreshing);
    assert_eq!(app.snapshot().rows, before);
    assert!(events.iter().all(|e| matches!(e, Event::Status { .. })));
}

#[test]
fn refresh_twice_runs_once() {
    let (_dir, app, script, recorder) = start(Some("user@x"), &[]);
    app.refresh();
    app.refresh();
    assert_eq!(script.next_call(), common::Call::Refresh);
    script.no_call_within(200);
    script
        .refresh
        .send(Ok(vec![record("urn:uuid:1", "A")]))
        .unwrap();
    recorder
        .wait_for(|e| has_status(e, |s| !s.refreshing) && e.iter().any(|e| titles(e).is_some()));
    script.no_call_within(200);
}

#[test]
fn refresh_signed_out_does_nothing() {
    let (_dir, app, script, recorder) = start(None, &[]);
    app.refresh();
    script.no_call_within(200);
    assert!(recorder.events().is_empty());
}

/// Forwards events to a recorder after reading a snapshot from inside the callback.
struct Reentrant {
    app: Arc<OnceLock<Weak<App>>>,
    recorder: Recorder,
}

impl Listener for Reentrant {
    fn on_event(&self, event: Event) {
        let app = self.app.get().and_then(Weak::upgrade).expect("app is set");
        let _ = app.snapshot();
        self.recorder.on_event(event);
    }
}

#[test]
fn listener_may_call_snapshot() {
    let dir = tempdir();
    let paths = settings_in(dir.path());
    let (ops, script) = FakeOps::new(Some("user@x"));
    let recorder = Recorder::new();
    let cell = Arc::new(OnceLock::new());
    let listener = Reentrant {
        app: cell.clone(),
        recorder: recorder.clone(),
    };
    let app = App::with_ops(ops, paths, Box::new(listener));
    cell.set(Arc::downgrade(&app)).unwrap();
    refresh_to_a_b(&app, &script, &recorder);
}

const K: &str = "urn:uuid:1";

fn row_of(app: &App, key: &str) -> Option<Row> {
    app.snapshot().rows.into_iter().find(|r| r.key == key)
}

/// Write book `K` into the books folder as an active loan.
fn loaned_book(dir: &TempDir) -> (std::path::PathBuf, Metadata) {
    let books = dir.path().join("Books");
    let mut metadata = write_book(&books, K, "A").metadata;
    metadata.loan = Some(Loan {
        operator_url: "https://op.example/fulfillment".into(),
        expires: None,
        token: "t".into(),
    });
    let folder = books.join("A");
    metadata.write(&folder).unwrap();
    (folder, metadata)
}

/// The metadata a successful return leaves behind, written to `folder`.
fn returned(folder: &std::path::Path, mut metadata: Metadata) -> Metadata {
    metadata.loan = None;
    metadata.returned = true;
    metadata.write(folder).unwrap();
    metadata
}

#[test]
fn return_loan_success() {
    let (_dir, app, script, recorder, folder, metadata) = loan_app();
    assert_eq!(row_of(&app, K).unwrap().state, RowState::OnLoan);
    app.return_loan(K.into());
    assert_eq!(script.next_call(), Call::ReturnLoan(K.into()));
    recorder.wait_for(|_| row_of(&app, K).unwrap().activity == Activity::Returning);
    script
        .return_loan
        .send(Ok(returned(&folder, metadata)))
        .unwrap();
    recorder.wait_for(|_| row_of(&app, K).unwrap().state == RowState::Returned);
    let row = row_of(&app, K).unwrap();
    assert_eq!(row.activity, Activity::Idle);
    assert_eq!(row.error, None);
}

/// An app over a loaned book `K`.
fn loan_app() -> (
    TempDir,
    Arc<App>,
    Script,
    Recorder,
    std::path::PathBuf,
    Metadata,
) {
    let dir = tempdir();
    let paths = settings_in(dir.path());
    std::fs::write(paths.cache.as_ref().unwrap(), cache_xml(&[(K, "A")])).unwrap();
    let (folder, metadata) = loaned_book(&dir);
    let (ops, script) = FakeOps::new(Some("user@x"));
    let recorder = Recorder::new();
    let app = App::with_ops(ops, paths, Box::new(recorder.clone()));
    (dir, app, script, recorder, folder, metadata)
}

#[test]
fn return_loan_error_keeps_loan() {
    let (_dir, app, script, recorder, _folder, _m) = loan_app();
    app.return_loan(K.into());
    assert_eq!(script.next_call(), Call::ReturnLoan(K.into()));
    script.return_loan.send(Err("denied".into())).unwrap();
    recorder.wait_for(|_| row_of(&app, K).unwrap().error.is_some());
    let row = row_of(&app, K).unwrap();
    assert_eq!(row.state, RowState::OnLoan);
    assert_eq!(row.error.as_deref(), Some("denied"));
    assert_eq!(row.activity, Activity::Idle);
}

#[test]
fn remove_deletes_folder_cloud_row_stays() {
    let dir = tempdir();
    let paths = settings_in(dir.path());
    std::fs::write(paths.cache.as_ref().unwrap(), cache_xml(&[(K, "A")])).unwrap();
    let folder = write_book(&dir.path().join("Books"), K, "A").folder;
    let (ops, _script) = FakeOps::new(Some("user@x"));
    let recorder = Recorder::new();
    let app = App::with_ops(ops, paths, Box::new(recorder.clone()));
    assert_eq!(row_of(&app, K).unwrap().state, RowState::Synced);
    app.remove(K.into(), false);
    recorder.wait_for(|_| row_of(&app, K).is_some_and(|r| r.state == RowState::Available));
    assert!(!folder.exists());
}

#[test]
fn remove_deletes_folder_local_row_disappears() {
    let dir = tempdir();
    let paths = settings_in(dir.path());
    let folder = write_book(&dir.path().join("Books"), K, "A").folder;
    let (ops, _script) = FakeOps::new(Some("user@x"));
    let recorder = Recorder::new();
    let app = App::with_ops(ops, paths, Box::new(recorder.clone()));
    assert_eq!(row_of(&app, K).unwrap().state, RowState::Local);
    app.remove(K.into(), false);
    recorder.wait_for(|_| row_of(&app, K).is_none());
    assert!(!folder.exists());
}

#[test]
fn remove_loaned_returns_first() {
    let (_dir, app, script, recorder, folder, metadata) = loan_app();
    app.remove(K.into(), true);
    assert_eq!(script.next_call(), Call::ReturnLoan(K.into()));
    // A second request while the return runs only keeps the mark.
    app.remove(K.into(), true);
    script.no_call_within(200);
    assert!(folder.exists());
    let done = returned(&folder, metadata);
    script.return_loan.send(Ok(done)).unwrap();
    recorder.wait_for(|_| row_of(&app, K).is_some_and(|r| r.state == RowState::Available));
    assert!(!folder.exists());
}

#[test]
fn remove_loaned_return_error_keeps_folder() {
    let (_dir, app, script, recorder, folder, _m) = loan_app();
    app.remove(K.into(), true);
    assert_eq!(script.next_call(), Call::ReturnLoan(K.into()));
    assert!(folder.exists());
    script.return_loan.send(Err("denied".into())).unwrap();
    recorder.wait_for(|_| row_of(&app, K).unwrap().error.is_some());
    assert!(folder.exists());
    assert_eq!(row_of(&app, K).unwrap().error.as_deref(), Some("denied"));
    // The mark is gone: a later plain return does not delete the folder.
    app.return_loan(K.into());
    assert_eq!(script.next_call(), Call::ReturnLoan(K.into()));
    script.return_loan.send(Err("again".into())).unwrap();
    recorder.wait_for(|_| row_of(&app, K).unwrap().error.as_deref() == Some("again"));
    assert!(folder.exists());
}

#[test]
fn unknown_key_ignored() {
    let (_dir, app, script, recorder) = start(Some("user@x"), &[(K, "A")]);
    let before = app.snapshot();
    app.return_loan("nope".into());
    app.remove("nope".into(), false);
    app.remove("nope".into(), true);
    script.no_call_within(200);
    assert!(recorder.events().is_empty());
    assert_eq!(app.snapshot(), before);
}

#[test]
fn settings_change_rescans() {
    let (dir, app, _script, recorder) = start(Some("user@x"), &[]);
    let other = dir.path().join("Other");
    write_book(&other, "urn:uuid:9", "Nine");
    let other_s = other.to_string_lossy().into_owned();
    let new = Settings {
        books_dir: Some(other_s.clone()),
        theme: Theme::Dark,
    };
    assert_eq!(app.set_settings(new.clone()), Ok(()));
    let saved = leafport_core::library::Settings::load(&dir.path().join("settings.yaml")).unwrap();
    assert_eq!(saved.books_dir, Some(other));
    assert_eq!(saved.theme, leafport_core::library::Theme::Dark);
    recorder.wait_for(|_| app.snapshot().status.books_dir.as_deref() == Some(other_s.as_str()));
    let snapshot = app.snapshot();
    assert_eq!(snapshot.settings, new);
    let titles: Vec<_> = snapshot.rows.iter().map(|r| r.title.as_str()).collect();
    assert_eq!(titles, ["Nine"]);
}

#[test]
fn settings_busy_rejects_folder_change() {
    let (dir, app, script, recorder) = start(Some("user@x"), &[(K, "A")]);
    app.sync(K.into());
    assert_eq!(script.next_call(), Call::Sync(K.into()));
    recorder.wait_for(|_| app.snapshot().status.job_running);
    let before = app.snapshot().settings;
    let moved = Settings {
        books_dir: Some(dir.path().join("Other").to_string_lossy().into_owned()),
        theme: Theme::Dark,
    };
    assert_eq!(app.set_settings(moved), Err(AppError::Busy));
    assert_eq!(app.snapshot().settings, before);
    let saved = leafport_core::library::Settings::load(&dir.path().join("settings.yaml")).unwrap();
    assert_eq!(saved.books_dir, Some(dir.path().join("Books")));
    assert_eq!(saved.theme, leafport_core::library::Theme::System);
    let theme_only = Settings {
        theme: Theme::Light,
        ..before
    };
    assert_eq!(app.set_settings(theme_only.clone()), Ok(()));
    assert_eq!(app.snapshot().settings, theme_only);
    script
        .sync
        .send(SyncStep::Done(Err("stop".into())))
        .unwrap();
    recorder.wait_for(|_| !app.snapshot().status.job_running);
}

#[test]
fn settings_save_failure_reported() {
    let (dir, app, _script, recorder) = start(Some("user@x"), &[]);
    // A folder where the file belongs makes the save fail.
    let path = dir.path().join("settings.yaml");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let result = app.set_settings(Settings {
        books_dir: Some(dir.path().join("Books").to_string_lossy().into_owned()),
        theme: Theme::Dark,
    });
    assert!(matches!(result, Err(AppError::Save { .. })), "{result:?}");
    recorder.wait_for(|_| !app.snapshot().status.banner.is_empty());
    let banner = app.snapshot().status.banner;
    assert!(
        banner[0].starts_with("Could not save settings: "),
        "{banner:?}"
    );
}

#[test]
fn banner_capped_and_cleared() {
    let (_dir, app, _script, recorder) = start(Some("user@x"), &[]);
    for n in 1..=6 {
        app.add_message(format!("m{n}"));
    }
    recorder.wait_for(|_| {
        app.snapshot().status.banner.len() == 5 && app.snapshot().status.banner[4] == "m6"
    });
    assert_eq!(app.snapshot().status.banner, ["m2", "m3", "m4", "m5", "m6"]);
    app.add_message("m3".into());
    recorder.wait_for(|_| app.snapshot().status.banner[4] == "m3");
    assert_eq!(app.snapshot().status.banner, ["m2", "m4", "m5", "m6", "m3"]);
    app.clear_banner();
    recorder.wait_for(|_| app.snapshot().status.banner.is_empty());
}

#[test]
fn settings_busy_during_return() {
    let (dir, app, script, recorder, folder, metadata) = loan_app();
    app.return_loan(K.into());
    assert_eq!(script.next_call(), Call::ReturnLoan(K.into()));
    recorder.wait_for(|_| app.snapshot().status.job_running);
    let moved = Settings {
        books_dir: Some(dir.path().join("Other").to_string_lossy().into_owned()),
        theme: Theme::System,
    };
    assert_eq!(app.set_settings(moved), Err(AppError::Busy));
    let saved = leafport_core::library::Settings::load(&dir.path().join("settings.yaml")).unwrap();
    assert_eq!(saved.books_dir, Some(dir.path().join("Books")));
    script
        .return_loan
        .send(Ok(returned(&folder, metadata)))
        .unwrap();
    recorder.wait_for(|_| !app.snapshot().status.job_running);
}

#[test]
fn remove_after_return_deletes_only_captured_folder() {
    let (dir, app, script, recorder, folder, metadata) = loan_app();
    app.remove(K.into(), true);
    assert_eq!(script.next_call(), Call::ReturnLoan(K.into()));
    // While the return runs, the title folder moves and a rescan picks up its new place.
    let moved = dir.path().join("Books").join("A2");
    std::fs::rename(&folder, &moved).unwrap();
    app.import_acsm(
        dir.path()
            .join("missing.acsm")
            .to_string_lossy()
            .into_owned(),
    );
    recorder.wait_for(|_| {
        let snapshot = app.snapshot();
        snapshot
            .status
            .banner
            .iter()
            .any(|m| m.starts_with("Import failed: "))
            && snapshot.rows.iter().any(|r| {
                r.key == K && r.folder.as_deref() == Some(moved.to_string_lossy().as_ref())
            })
    });
    let done = returned(&moved, metadata);
    script.return_loan.send(Ok(done)).unwrap();
    recorder.wait_for(|_| {
        row_of(&app, K).is_some_and(|r| r.activity == Activity::Idle && r.state != RowState::OnLoan)
    });
    assert!(moved.exists(), "a folder the user did not confirm stays");
}

/// Panics on its first event, then forwards to a recorder.
struct PanicsOnce {
    panicked: std::sync::atomic::AtomicBool,
    recorder: Recorder,
}

impl Listener for PanicsOnce {
    fn on_event(&self, event: Event) {
        if !self
            .panicked
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            panic!("listener failed");
        }
        self.recorder.on_event(event);
    }
}

#[test]
fn panicking_listener_keeps_reactor() {
    let dir = tempdir();
    let paths = settings_in(dir.path());
    let (ops, _script) = FakeOps::new(Some("user@x"));
    let recorder = Recorder::new();
    let listener = PanicsOnce {
        panicked: Default::default(),
        recorder: recorder.clone(),
    };
    let app = App::with_ops(ops, paths, Box::new(listener));
    app.add_message("one".into());
    app.add_message("two".into());
    recorder.wait_for(|e| {
        common::last_status(e).is_some_and(|s| s.banner.contains(&"two".to_string()))
    });
}
