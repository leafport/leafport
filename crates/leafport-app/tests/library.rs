mod common;

use std::sync::{Arc, OnceLock, Weak};

use common::{FakeOps, Recorder, Script, TempDir, cache_xml, record, settings_in, tempdir};
use leafport_app::{App, Event, Listener, RowState, Status};

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
