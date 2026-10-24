mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{
    Call, FakeOps, Recorder, Script, SyncStep, TempDir, acsm_xml, cache_xml, settings_in, tempdir,
    write_book,
};
use leafport_app::{Activity, App, Event, Row, RowState, Status};

const A: &str = "urn:uuid:1";
const B: &str = "urn:uuid:2";
const C: &str = "urn:uuid:3";

struct Setup {
    dir: TempDir,
    app: Arc<App>,
    ops: Arc<FakeOps>,
    script: Script,
    recorder: Recorder,
}

impl Setup {
    fn books(&self) -> PathBuf {
        self.dir.path().join("Books")
    }

    fn row(&self, key: &str) -> Row {
        row_in(&self.app, key)
    }

    /// Block until the row with `key` satisfies `pred`.
    fn wait_row(&self, key: &str, pred: impl Fn(&Row) -> bool) {
        self.recorder.wait_for(|_| pred(&row_in(&self.app, key)));
    }

    /// Block until the status satisfies `pred`.
    fn wait_status(&self, pred: impl Fn(&Status) -> bool) {
        self.recorder
            .wait_for(|_| pred(&self.app.snapshot().status));
    }

    /// Finish the running sync of `key` successfully, with its book on disk.
    fn finish(&self, key: &str, title: &str) {
        let outcome = write_book(&self.books(), key, title);
        self.script.sync.send(SyncStep::Done(Ok(outcome))).unwrap();
        self.wait_row(key, |r| r.activity == Activity::Idle);
    }

    /// An ACSM file for `resource` inside the temp folder.
    fn acsm_file(&self, resource: &str) -> PathBuf {
        let path = self
            .dir
            .path()
            .join(format!("{}.acsm", resource.replace(':', "-")));
        std::fs::write(&path, acsm_xml(resource, "Imported")).unwrap();
        path
    }
}

fn row_in(app: &App, key: &str) -> Row {
    app.snapshot()
        .rows
        .into_iter()
        .find(|r| r.key == key)
        .unwrap_or_else(|| panic!("no row {key}"))
}

/// Settings, a cache listing `cached`, and an app on a fake signed in as `user`.
fn start_with(user: Option<&str>, cached: &[(&str, &str)], prepare: impl Fn(&Path)) -> Setup {
    let dir = tempdir();
    let paths = settings_in(dir.path());
    std::fs::write(paths.cache.as_ref().unwrap(), cache_xml(cached)).unwrap();
    prepare(&dir.path().join("Books"));
    let (ops, script) = FakeOps::new(user);
    let recorder = Recorder::new();
    let app = App::with_ops(ops.clone(), paths, Box::new(recorder.clone()));
    Setup {
        dir,
        app,
        ops,
        script,
        recorder,
    }
}

/// Signed in, with rows A and B available.
fn start() -> Setup {
    start_with(Some("user@x"), &[(A, "A"), (B, "B")], |_| {})
}

#[test]
fn syncs_run_one_at_a_time_in_order() {
    let s = start();
    s.app.sync(B.into());
    s.app.sync(A.into());
    assert_eq!(s.script.next_call(), Call::Sync(B.into()));
    s.wait_row(A, |r| r.activity == Activity::Queued);
    s.script.no_call_within(200);
    s.finish(B, "B");
    assert_eq!(s.script.next_call(), Call::Sync(A.into()));
}

#[test]
fn duplicate_sync_ignored() {
    let s = start();
    s.app.sync(A.into());
    s.app.sync(A.into());
    assert_eq!(s.script.next_call(), Call::Sync(A.into()));
    s.script.no_call_within(200);
    assert_eq!(s.row(A).activity, Activity::Syncing { cancellable: true });
    s.finish(A, "A");
    s.script.no_call_within(200);
}

#[test]
fn progress_reaches_listener() {
    let s = start();
    s.app.sync(A.into());
    assert_eq!(s.script.next_call(), Call::Sync(A.into()));
    s.script.sync.send(SyncStep::Progress(0.5)).unwrap();
    s.recorder.wait_for(|e| {
        e.contains(&Event::Progress {
            key: A.into(),
            fraction: 0.5,
        })
    });
}

#[test]
fn commit_disables_cancel() {
    let s = start();
    s.app.sync(A.into());
    assert_eq!(s.script.next_call(), Call::Sync(A.into()));
    s.wait_row(A, |r| r.activity == Activity::Syncing { cancellable: true });
    assert!(s.app.snapshot().status.can_cancel_all);
    s.script.sync.send(SyncStep::Commit).unwrap();
    s.wait_row(A, |r| {
        r.activity == Activity::Syncing { cancellable: false }
    });
    assert!(!s.app.snapshot().status.can_cancel_all);
    s.app.cancel(A.into());
    s.app.cancel_all();
    s.finish(A, "A");
    assert!(!s.ops.last_token().is_cancelled());
    assert_eq!(s.row(A).state, RowState::Synced);
}

#[test]
fn cancel_queued_removes_it() {
    let s = start();
    s.app.sync(A.into());
    assert_eq!(s.script.next_call(), Call::Sync(A.into()));
    s.app.sync(B.into());
    s.wait_row(B, |r| r.activity == Activity::Queued);
    s.app.cancel(B.into());
    s.wait_row(B, |r| r.activity == Activity::Idle);
    s.finish(A, "A");
    s.script.no_call_within(200);
    assert_eq!(s.row(B).activity, Activity::Idle);
}

#[test]
fn cancelled_sync_not_failed() {
    let s = start();
    s.app.sync(A.into());
    assert_eq!(s.script.next_call(), Call::Sync(A.into()));
    s.app.cancel(A.into());
    // The cancel reaches the reactor before the result the reply below produces.
    s.script
        .sync
        .send(SyncStep::Done(Err("cancelled".into())))
        .unwrap();
    s.wait_row(A, |r| r.activity == Activity::Idle);
    assert!(s.ops.last_token().is_cancelled());
    let row = s.row(A);
    assert_eq!(row.state, RowState::Available);
    assert_eq!(row.error, None);
}

#[test]
fn failed_sync_marks_row() {
    let s = start();
    s.app.sync(A.into());
    assert_eq!(s.script.next_call(), Call::Sync(A.into()));
    s.script
        .sync
        .send(SyncStep::Done(Err("boom".into())))
        .unwrap();
    s.wait_row(A, |r| r.activity == Activity::Idle);
    let row = s.row(A);
    assert_eq!(row.state, RowState::Failed);
    assert_eq!(row.error.as_deref(), Some("boom"));

    s.app.sync(A.into());
    assert_eq!(s.script.next_call(), Call::Sync(A.into()));
    s.finish(A, "A");
    let row = s.row(A);
    assert_eq!(row.state, RowState::Synced);
    assert_eq!(row.error, None);
}

#[test]
fn sync_all_queues_unsynced_only() {
    let s = start_with(Some("user@x"), &[(A, "A"), (B, "B"), (C, "C")], |books| {
        write_book(books, B, "B");
    });
    assert_eq!(s.row(B).state, RowState::Synced);
    // Make C Failed.
    s.app.sync(C.into());
    assert_eq!(s.script.next_call(), Call::Sync(C.into()));
    s.script
        .sync
        .send(SyncStep::Done(Err("boom".into())))
        .unwrap();
    s.wait_row(C, |r| {
        r.state == RowState::Failed && r.activity == Activity::Idle
    });
    assert!(s.app.snapshot().status.can_sync_all);

    s.app.sync_all();
    assert_eq!(s.script.next_call(), Call::Sync(A.into()));
    s.finish(A, "A");
    assert_eq!(s.script.next_call(), Call::Sync(C.into()));
    s.finish(C, "C");
    s.wait_status(|st| !st.job_running);
    assert!(!s.app.snapshot().status.can_sync_all);
    s.script.no_call_within(200);
}

#[test]
fn cancel_all_keeps_import() {
    let s = start();
    let path = s.acsm_file("urn:uuid:9");
    s.app.import_acsm(path.to_string_lossy().into_owned());
    assert_eq!(s.script.next_call(), Call::Import(path));
    s.app.sync(A.into());
    s.wait_row(A, |r| r.activity == Activity::Queued);
    assert!(s.app.snapshot().status.can_cancel_all);
    s.app.cancel_all();
    s.wait_row(A, |r| r.activity == Activity::Idle);
    assert!(!s.app.snapshot().status.can_cancel_all);
    let outcome = write_book(&s.books(), "urn:uuid:9", "Imported");
    s.script.import.send(Ok(outcome)).unwrap();
    s.wait_status(|st| !st.importing && !st.job_running);
    assert!(s.app.snapshot().status.banner.is_empty());
    s.script.no_call_within(200);
}

#[test]
fn unknown_key_ignored() {
    let s = start();
    s.app.sync("nope".into());
    s.app.cancel("nope".into());
    s.script.no_call_within(200);
    assert!(s.recorder.events().is_empty());
}

#[test]
fn import_signed_out_banner() {
    let s = start_with(None, &[(A, "A")], |_| {});
    let path = s.acsm_file("urn:uuid:9");
    s.app.import_acsm(path.to_string_lossy().into_owned());
    s.wait_status(|st| !st.banner.is_empty());
    assert_eq!(
        s.app.snapshot().status.banner,
        ["Sign in before importing an ACSM."]
    );
    s.script.no_call_within(200);
}

#[test]
fn import_reports_warning_and_errors() {
    let s = start();
    let path = s.acsm_file("urn:uuid:9");
    let path_string = path.to_string_lossy().into_owned();

    s.app.import_acsm(path_string.clone());
    assert_eq!(s.script.next_call(), Call::Import(path.clone()));
    s.wait_status(|st| st.importing && st.job_running);
    let mut outcome = write_book(&s.books(), "urn:uuid:9", "Imported");
    outcome.warning = Some("upload failed".into());
    s.script.import.send(Ok(outcome)).unwrap();
    s.wait_status(|st| !st.importing);
    assert_eq!(s.app.snapshot().status.banner, ["upload failed"]);

    s.app.import_acsm(path_string);
    assert_eq!(s.script.next_call(), Call::Import(path));
    s.script.import.send(Err("bad".into())).unwrap();
    s.wait_status(|st| st.banner.len() == 2);
    assert_eq!(
        s.app.snapshot().status.banner,
        ["upload failed", "Import failed: bad"]
    );
}

#[test]
fn import_rejects_malformed_acsm_before_calling_ops() {
    let s = start();
    let path = s.dir.path().join("bad.acsm");
    std::fs::write(&path, "not xml").unwrap();
    s.app.import_acsm(path.to_string_lossy().into_owned());
    s.wait_status(|st| !st.banner.is_empty() && !st.job_running);
    assert!(s.app.snapshot().status.banner[0].starts_with("Import failed: "));
    s.script.no_call_within(200);
}

#[test]
fn import_accepts_an_acsm_that_is_not_utf8() {
    let s = start();
    let path = s.dir.path().join("latin1.acsm");
    // A Latin-1 "é". The library reads ACSMs leniently, so the app does not reject it.
    let bytes: Vec<u8> = acsm_xml("urn:uuid:9", "Caf\u{1}")
        .bytes()
        .map(|b| if b == 1 { 0xE9 } else { b })
        .collect();
    std::fs::write(&path, bytes).unwrap();
    s.app.import_acsm(path.to_string_lossy().into_owned());
    assert_eq!(s.script.next_call(), Call::Import(path));
}
