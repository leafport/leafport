mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{
    Call, FakeOps, Recorder, Script, SyncStep, TempDir, cache_xml, record, settings_in, tempdir,
    write_book,
};
use leafport_app::{Activity, App, Event, Row, RowState, Status};
use leafport_core::vendors::VendorCatalog;

const A: &str = "urn:uuid:1";
const B: &str = "urn:uuid:2";

struct Setup {
    dir: TempDir,
    app: Arc<App>,
    script: Script,
    recorder: Recorder,
}

impl Setup {
    fn books(&self) -> PathBuf {
        self.dir.path().join("Books")
    }

    fn cache(&self) -> PathBuf {
        self.dir.path().join("cache.xml")
    }

    fn rows(&self) -> Vec<Row> {
        self.app.snapshot().rows
    }

    fn row(&self, key: &str) -> Option<Row> {
        self.rows().into_iter().find(|r| r.key == key)
    }

    fn status(&self) -> Status {
        self.app.snapshot().status
    }

    /// Block until the status satisfies `pred`.
    fn wait_status(&self, pred: impl Fn(&Status) -> bool) {
        self.recorder
            .wait_for(|_| pred(&self.app.snapshot().status));
    }

    /// Load providers and reply with `catalog()`.
    fn load(&self) {
        self.app.load_providers();
        assert_eq!(self.script.next_call(), Call::LoadProviders);
        self.script.providers.send(Ok(catalog())).unwrap();
        self.recorder
            .wait_for(|e| e.iter().any(|e| matches!(e, Event::Providers { .. })));
        self.wait_status(|s| !s.loading_providers);
    }
}

/// Poll until `pred` holds, for changes that raise no event. Panics after 5 s.
fn until(pred: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !pred() {
        assert!(std::time::Instant::now() < deadline, "timed out");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// A catalog with a service URL, a certificate and one discovered vendor.
fn catalog() -> VendorCatalog {
    let mut catalog = VendorCatalog::new();
    catalog
        .apply_service_info(
            r#"<authenticationServiceInfo xmlns="http://ns.adobe.com/adept"><authURL>https://auth.example/auth</authURL><certificate>MIIB</certificate><signInMethods><signInMethod method="vendor1" nodeId="n1" type="web">Library One</signInMethod></signInMethods></authenticationServiceInfo>"#,
        )
        .unwrap();
    catalog
}

/// Settings, a cache listing `cached`, and an app on a fake signed in as `user`.
fn start_with(user: Option<&str>, cached: &[(&str, &str)], prepare: impl Fn(&Path)) -> Setup {
    let dir = tempdir();
    let paths = settings_in(dir.path());
    std::fs::write(paths.cache.as_ref().unwrap(), cache_xml(cached)).unwrap();
    prepare(&dir.path().join("Books"));
    let (ops, script) = FakeOps::new(user);
    let recorder = Recorder::new();
    let app = App::with_ops(ops, paths, Box::new(recorder.clone()));
    if user.is_some() {
        recorder.settle_startup();
    }
    Setup {
        dir,
        app,
        script,
        recorder,
    }
}

fn signed_out() -> Setup {
    start_with(None, &[], |_| {})
}

#[test]
fn providers_loaded() {
    let s = signed_out();
    s.app.load_providers();
    assert_eq!(s.script.next_call(), Call::LoadProviders);
    s.wait_status(|st| st.loading_providers);
    s.script.providers.send(Ok(catalog())).unwrap();
    let events = s
        .recorder
        .wait_for(|e| e.iter().any(|e| matches!(e, Event::Providers { .. })));
    let providers = events
        .iter()
        .find_map(|e| match e {
            Event::Providers { result: Ok(v) } => Some(v.clone()),
            _ => None,
        })
        .expect("providers ok");
    assert_eq!(providers[0].method, "AdobeID");
    assert_eq!(providers[1].method, "vendor1");
    assert_eq!(providers[1].name, "Library One");
    assert_eq!(providers[1].node_id.as_deref(), Some("n1"));
    assert_eq!(providers[1].sign_in_type.as_deref(), Some("web"));
    s.wait_status(|st| !st.loading_providers);
}

#[test]
fn providers_error_keeps_catalog() {
    let s = signed_out();
    s.load();
    s.app.load_providers();
    assert_eq!(s.script.next_call(), Call::LoadProviders);
    s.script.providers.send(Err("dns".into())).unwrap();
    s.recorder.wait_for(|e| {
        e.contains(&Event::Providers {
            result: Err("dns".into()),
        })
    });
    s.wait_status(|st| !st.loading_providers);
    s.app
        .sign_in("vendor1".into(), "me".into(), "secret".into());
    assert_eq!(
        s.script.next_call(),
        Call::SignIn {
            method: "vendor1".into(),
            username: "me".into()
        }
    );
}

#[test]
fn sign_in_ready_follows_catalog() {
    let s = signed_out();
    assert!(!s.status().sign_in_ready);
    s.load();
    s.wait_status(|st| st.sign_in_ready);
    s.app.load_providers();
    assert_eq!(s.script.next_call(), Call::LoadProviders);
    s.script.providers.send(Err("dns".into())).unwrap();
    s.recorder.wait_for(|e| {
        e.contains(&Event::Providers {
            result: Err("dns".into()),
        })
    });
    s.wait_status(|st| !st.loading_providers);
    assert!(s.status().sign_in_ready);
}

#[test]
fn sign_in_needs_certificate() {
    let s = signed_out();
    s.app
        .sign_in("AdobeID".into(), "me".into(), "secret".into());
    s.wait_status(|st| {
        st.sign_in_error.as_deref()
            == Some("Discovery has no authentication URL or certificate yet.")
    });
    s.script.no_call_within(200);
    assert!(!s.status().signing_in);
}

#[test]
fn sign_in_needs_credentials() {
    let s = signed_out();
    s.load();
    s.app.sign_in("vendor1".into(), "".into(), "secret".into());
    s.wait_status(|st| st.sign_in_error.as_deref() == Some("Enter a username and password."));
    s.app.sign_in("vendor1".into(), "me".into(), "".into());
    s.script.no_call_within(200);
    assert!(!s.status().signing_in);
}

#[test]
fn sign_in_failure() {
    let s = signed_out();
    s.load();
    s.app
        .sign_in("vendor1".into(), "me".into(), "secret".into());
    assert_eq!(
        s.script.next_call(),
        Call::SignIn {
            method: "vendor1".into(),
            username: "me".into()
        }
    );
    s.wait_status(|st| st.signing_in);
    s.script.sign_in.send(Err("bad password".into())).unwrap();
    s.wait_status(|st| {
        !st.signing_in && st.sign_in_error.as_deref() == Some("Sign-in failed: bad password")
    });
    assert_eq!(s.status().user, None);

    // A new attempt clears the error, and success keeps it clear.
    s.app
        .sign_in("vendor1".into(), "me".into(), "secret".into());
    s.script.next_call();
    s.wait_status(|st| st.signing_in && st.sign_in_error.is_none());
    s.script.sign_in.send(Ok("user@x".into())).unwrap();
    s.wait_status(|st| st.user.as_deref() == Some("user@x") && !st.signing_in);
    assert_eq!(s.status().sign_in_error, None);
}

#[test]
fn sign_in_success_bumps_generation() {
    let s = start_with(Some("old@x"), &[(A, "A")], |_| {});
    s.app.refresh();
    assert_eq!(s.script.next_call(), Call::Refresh);
    s.wait_status(|st| st.refreshing);
    // Sign-in is ignored while signed in, so switch accounts through a sign-out.
    s.app.sign_out();
    assert_eq!(s.script.next_call(), Call::SignOut);
    s.script.sign_out.send(Ok(())).unwrap();
    s.wait_status(|st| st.user.is_none());
    s.load();
    s.app
        .sign_in("vendor1".into(), "me".into(), "secret".into());
    s.script.next_call();
    s.script.sign_in.send(Ok("user@x".into())).unwrap();
    // The new generation resets `refreshing`, so a new refresh may start.
    s.wait_status(|st| st.user.as_deref() == Some("user@x") && !st.refreshing);

    // The refresh that started under the old account finishes: its result and the file
    // it wrote are discarded, and the cache is not written.
    let part = s.dir.path().join("cache.xml.0.part");
    std::fs::write(&part, cache_xml(&[(B, "B")])).unwrap();
    s.script.refresh.send(Ok(vec![record(B, "B")])).unwrap();
    until(|| !part.exists());
    assert!(!s.cache().exists());
    assert!(s.row(B).is_none());

    // A fresh refresh works again.
    s.app.refresh();
    assert_eq!(s.script.next_call(), Call::Refresh);
    s.script.refresh.send(Ok(vec![record(B, "B")])).unwrap();
    s.recorder.wait_for(|_| s.row(B).is_some());
}

#[test]
fn current_refresh_moves_its_file_to_the_cache() {
    let s = start_with(Some("user@x"), &[(A, "A")], |_| {});
    s.app.refresh();
    assert_eq!(s.script.next_call(), Call::Refresh);
    let part = s.dir.path().join("cache.xml.0.part");
    let listed = cache_xml(&[(B, "B")]);
    std::fs::write(&part, &listed).unwrap();
    s.script.refresh.send(Ok(vec![record(B, "B")])).unwrap();
    s.recorder.wait_for(|_| s.row(B).is_some());
    assert!(!part.exists());
    assert_eq!(std::fs::read_to_string(s.cache()).unwrap(), listed);
}

#[test]
fn sign_out_clears_cloud() {
    let s = start_with(Some("user@x"), &[(A, "A"), (B, "B")], |books| {
        write_book(books, A, "A");
    });
    // A failed sync marks A Failed; sign-out must clear that.
    s.app.sync(A.into());
    assert_eq!(s.script.next_call(), Call::Sync(A.into()));
    s.script
        .sync
        .send(SyncStep::Done(Err("boom".into())))
        .unwrap();
    s.recorder
        .wait_for(|_| s.row(A).is_some_and(|r| r.state == RowState::Failed));

    s.app.sign_out();
    assert_eq!(s.script.next_call(), Call::SignOut);
    s.script.sign_out.send(Ok(())).unwrap();
    s.wait_status(|st| st.user.is_none());
    s.recorder.wait_for(|_| s.row(B).is_none());

    assert!(!s.cache().exists());
    let a = s.row(A).expect("synced folder stays");
    assert_eq!(a.state, RowState::Local);
    assert_eq!(a.error, None);
    assert!(!s.status().can_sync_all);
    assert!(s.books().join("A").exists());
}

#[test]
fn sign_out_during_sync() {
    let s = start_with(Some("user@x"), &[(A, "A"), (B, "B")], |_| {});
    s.app.sync(A.into());
    s.app.sync(B.into());
    assert_eq!(s.script.next_call(), Call::Sync(A.into()));
    s.recorder
        .wait_for(|_| s.row(B).is_some_and(|r| r.activity == Activity::Queued));

    s.app.sign_out();
    assert_eq!(s.script.next_call(), Call::SignOut);
    s.script.sign_out.send(Ok(())).unwrap();
    s.wait_status(|st| st.user.is_none());

    let outcome = write_book(&s.books(), A, "A");
    s.script.sync.send(SyncStep::Done(Ok(outcome))).unwrap();
    s.recorder
        .wait_for(|_| s.row(A).is_some_and(|r| r.activity == Activity::Idle));
    s.script.no_call_within(200);

    // The reactor still works.
    s.app.add_message("still alive".into());
    s.wait_status(|st| st.banner.contains(&"still alive".to_string()));
}

#[test]
fn sign_out_failure() {
    let s = start_with(Some("user@x"), &[(A, "A")], |_| {});
    s.app.sign_out();
    assert_eq!(s.script.next_call(), Call::SignOut);
    s.script.sign_out.send(Err("locked".into())).unwrap();
    s.wait_status(|st| st.banner.contains(&"Sign-out failed: locked".to_string()));
    assert_eq!(s.status().user.as_deref(), Some("user@x"));
    assert!(s.cache().exists());
    assert!(s.row(A).is_some());
}

#[test]
fn sign_out_during_sign_in_ignored() {
    let s = signed_out();
    s.load();
    s.app
        .sign_in("vendor1".into(), "me".into(), "secret".into());
    s.script.next_call();
    s.wait_status(|st| st.signing_in);
    s.app.sign_out();
    s.script.no_call_within(200);
    s.script.sign_in.send(Ok("user@x".into())).unwrap();
    s.wait_status(|st| st.user.as_deref() == Some("user@x") && !st.signing_in);
    s.script.no_call_within(200);
    assert_eq!(s.status().user.as_deref(), Some("user@x"));
}

#[test]
fn sign_in_while_signed_in_ignored() {
    let s = start_with(Some("user@x"), &[], |_| {});
    s.load();
    s.app
        .sign_in("vendor1".into(), "me".into(), "secret".into());
    s.script.no_call_within(200);
    assert!(!s.status().signing_in);
}

#[test]
fn second_sign_out_ignored() {
    let s = start_with(Some("user@x"), &[], |_| {});
    s.app.sign_out();
    assert_eq!(s.script.next_call(), Call::SignOut);
    s.app.sign_out();
    s.script.no_call_within(200);
    s.script.sign_out.send(Ok(())).unwrap();
    s.wait_status(|st| st.user.is_none());
}
