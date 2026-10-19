mod common;

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
};

use leafport_core::activation::{ActivationKeys, device_fingerprint, device_id};
use leafport_core::store::{
    ActivationState, ChunkedStore, CredentialStore, DEFAULT_ACCOUNT, StoreError,
};

#[derive(Default)]
struct MemoryStore(RefCell<HashMap<String, Vec<u8>>>);

impl CredentialStore for MemoryStore {
    fn load(&self, account: &str) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.0.borrow().get(account).cloned())
    }
    fn save(&self, account: &str, secret: &[u8]) -> Result<(), StoreError> {
        self.0.borrow_mut().insert(account.into(), secret.to_vec());
        Ok(())
    }
    fn delete(&self, account: &str) -> Result<(), StoreError> {
        self.0.borrow_mut().remove(account);
        Ok(())
    }
}

fn state() -> ActivationState {
    ActivationState {
        device_key: [9; 16],
        keys: ActivationKeys {
            authentication: common::key().clone(),
            license: common::other_key().clone(),
        },
        user: "urn:uuid:ü".into(),
        device: "activation://device/x".into(),
        fingerprint: "FP==".into(),
        credentials_xml: "<credentials/>".into(),
    }
}

#[test]
fn state_round_trips_through_the_store() {
    let store = MemoryStore::default();
    state().save(&store, DEFAULT_ACCOUNT).unwrap();
    let loaded = ActivationState::load(&store, DEFAULT_ACCOUNT)
        .unwrap()
        .unwrap();
    let original = state();
    assert_eq!(loaded.device_key, original.device_key);
    assert_eq!(loaded.keys.authentication, original.keys.authentication);
    assert_eq!(loaded.keys.license, original.keys.license);
    assert_eq!(loaded.user, original.user);
    assert_eq!(loaded.device, original.device);
    assert_eq!(loaded.fingerprint, original.fingerprint);
    assert_eq!(loaded.credentials_xml, original.credentials_xml);
    assert_eq!(store.0.borrow()[DEFAULT_ACCOUNT][0], 2);
}

#[test]
fn missing_account_loads_as_none() {
    assert!(
        ActivationState::load(&MemoryStore::default(), "nobody")
            .unwrap()
            .is_none()
    );
}

#[test]
fn version_one_blob_derives_device_identity_from_key() {
    let store = MemoryStore::default();
    state().save(&store, "a").unwrap();
    // Rewrite as v1: drop the device and fingerprint fields after the user field.
    let v2 = store.0.borrow()["a"].clone();
    let mut cursor = 17;
    let mut fields = Vec::new();
    while cursor < v2.len() {
        let length = u32::from_le_bytes(v2[cursor..cursor + 4].try_into().unwrap()) as usize;
        fields.push(&v2[cursor..cursor + 4 + length]);
        cursor += 4 + length;
    }
    assert_eq!(fields.len(), 6);
    let mut v1 = vec![1];
    v1.extend_from_slice(&v2[1..17]);
    for index in [0, 1, 2, 5] {
        v1.extend_from_slice(fields[index]);
    }
    store.save("a", &v1).unwrap();

    let loaded = ActivationState::load(&store, "a").unwrap().unwrap();
    assert_eq!(loaded.device, device_id(&[9; 16]));
    assert_eq!(loaded.fingerprint, device_fingerprint(&[9; 16]));
    assert_eq!(loaded.credentials_xml, "<credentials/>");
}

#[test]
fn corrupt_blobs_are_rejected() {
    let store = MemoryStore::default();
    state().save(&store, "a").unwrap();
    let good = store.0.borrow()["a"].clone();
    for (blob, expected) in [
        (vec![], "unknown blob version"),
        (vec![3; 40], "unknown blob version"),
        (vec![2; 10], "missing device key"),
        (good[..19].to_vec(), "missing length prefix"),
        (good[..good.len() - 1].to_vec(), "field exceeds blob"),
    ] {
        store.save("a", &blob).unwrap();
        match ActivationState::load(&store, "a") {
            Err(StoreError::Corrupt(reason)) => assert_eq!(reason, expected),
            Err(other) => panic!("unexpected {other}"),
            Ok(_) => panic!("loaded corrupt blob for {expected}"),
        }
    }
}

fn large_secret(length: usize) -> Vec<u8> {
    (0..length).map(|index| (index % 251) as u8).collect()
}

#[test]
fn large_secret_round_trips_in_bounded_chunks() {
    let store = ChunkedStore(MemoryStore::default());
    let secret = large_secret(7000);
    store.save("a", &secret).unwrap();
    assert_eq!(store.load("a").unwrap().unwrap(), secret);
    let entries = store.0.0.borrow();
    assert_eq!(entries.len(), 5);
    assert!(entries.values().all(|entry| entry.len() <= 2560));
    assert_eq!(&entries["a"][..4], b"RCH2");
    assert!(entries.contains_key("a#0.3"));
}

#[test]
fn small_secret_is_stored_unchunked() {
    let store = ChunkedStore(MemoryStore::default());
    store.save("a", b"small").unwrap();
    assert_eq!(store.0.0.borrow().len(), 1);
    assert_eq!(store.0.0.borrow()["a"], b"small");
    assert_eq!(store.load("a").unwrap().unwrap(), b"small");
}

#[test]
fn activation_state_round_trips_through_chunked_store() {
    let store = ChunkedStore(MemoryStore::default());
    let mut original = state();
    original.credentials_xml = "x".repeat(4000);
    original.save(&store, DEFAULT_ACCOUNT).unwrap();
    let loaded = ActivationState::load(&store, DEFAULT_ACCOUNT)
        .unwrap()
        .unwrap();
    assert_eq!(loaded.credentials_xml, original.credentials_xml);
    assert_eq!(loaded.keys.license, original.keys.license);
}

#[test]
fn shrinking_save_removes_stale_chunks() {
    let store = ChunkedStore(MemoryStore::default());
    store.save("a", &large_secret(7000)).unwrap();
    let smaller = large_secret(3000);
    store.save("a", &smaller).unwrap();
    assert_eq!(store.0.0.borrow().len(), 3);
    assert!(store.0.0.borrow().contains_key("a#1.1"));
    assert!(!store.0.0.borrow().contains_key("a#0.2"));
    assert_eq!(store.load("a").unwrap().unwrap(), smaller);
    store.save("a", b"tiny").unwrap();
    assert_eq!(store.0.0.borrow().len(), 1);
    assert_eq!(store.load("a").unwrap().unwrap(), b"tiny");
}

#[test]
fn missing_chunk_is_corrupt() {
    let store = ChunkedStore(MemoryStore::default());
    store.save("a", &large_secret(7000)).unwrap();
    store.0.delete("a#0.1").unwrap();
    match store.load("a") {
        Err(StoreError::Corrupt(_)) => {}
        Err(other) => panic!("unexpected {other}"),
        Ok(_) => panic!("loaded secret with a missing chunk"),
    }
}

#[test]
fn delete_removes_header_and_chunks() {
    let store = ChunkedStore(MemoryStore::default());
    store.save("a", &large_secret(7000)).unwrap();
    store.delete("a").unwrap();
    assert!(store.0.0.borrow().is_empty());
    assert!(store.load("a").unwrap().is_none());
}

/// Wraps a `MemoryStore` and simulates a crash: every save/delete call from the
/// `fail_at`-th onward fails without taking effect.
#[derive(Default)]
struct FailingStore {
    inner: MemoryStore,
    calls: Cell<usize>,
    fail_at: Cell<Option<usize>>,
}

impl FailingStore {
    fn crash_after(&self, writes: usize) {
        self.fail_at.set(Some(self.calls.get() + writes));
    }
    fn recover(&self) {
        self.fail_at.set(None);
    }
    fn write(&self) -> Result<(), StoreError> {
        let call = self.calls.get();
        self.calls.set(call + 1);
        match self.fail_at.get() {
            Some(limit) if call >= limit => Err(StoreError::Backend("simulated crash".into())),
            _ => Ok(()),
        }
    }
}

impl CredentialStore for FailingStore {
    fn load(&self, account: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.inner.load(account)
    }
    fn save(&self, account: &str, secret: &[u8]) -> Result<(), StoreError> {
        self.write()?;
        self.inner.save(account, secret)
    }
    fn delete(&self, account: &str) -> Result<(), StoreError> {
        self.write()?;
        self.inner.delete(account)
    }
}

fn reversed_secret(length: usize) -> Vec<u8> {
    let mut secret = large_secret(length);
    secret.reverse();
    secret
}

#[test]
fn interrupted_chunked_save_keeps_a_loadable_secret() {
    let old = large_secret(7000);
    let new = reversed_secret(6000);
    for writes in 0.. {
        let store = ChunkedStore(FailingStore::default());
        store.save("a", &old).unwrap();
        store.0.crash_after(writes);
        let saved = store.save("a", &new).is_ok();
        store.0.recover();
        let loaded = store.load("a").unwrap().unwrap();
        // Three new chunks are written before the header flips.
        if writes <= 3 {
            assert!(!saved);
            assert_eq!(loaded, old, "crash after {writes} writes");
        } else {
            assert!(
                loaded == old || loaded == new,
                "crash after {writes} writes"
            );
        }
        store.save("a", &new).unwrap();
        assert_eq!(store.load("a").unwrap().unwrap(), new);
        assert_eq!(store.0.inner.0.borrow().len(), 4);
        store.delete("a").unwrap();
        assert!(store.0.inner.0.borrow().is_empty());
        if saved {
            break;
        }
    }
}

#[test]
fn interrupted_delete_is_resumable() {
    for writes in 0.. {
        let store = ChunkedStore(FailingStore::default());
        store.save("a", &large_secret(7000)).unwrap();
        store.0.crash_after(writes);
        let deleted = store.delete("a").is_ok();
        store.0.recover();
        if !deleted {
            assert!(store.0.inner.0.borrow().contains_key("a"));
        }
        store.delete("a").unwrap();
        assert!(
            store.0.inner.0.borrow().is_empty(),
            "crash after {writes} writes"
        );
        if deleted {
            break;
        }
    }
}

#[test]
fn chunk_gap_is_fully_cleaned() {
    let store = ChunkedStore(MemoryStore::default());
    store.save("a", &large_secret(7000)).unwrap();
    store.0.delete("a#0.1").unwrap();
    store.delete("a").unwrap();
    assert!(store.0.0.borrow().is_empty());

    store.save("a", &large_secret(7000)).unwrap();
    store.0.delete("a#0.0").unwrap();
    store.save("a", b"tiny").unwrap();
    assert_eq!(store.0.0.borrow().len(), 1);
}

#[test]
fn legacy_chunked_blob_loads_and_migrates() {
    let store = ChunkedStore(MemoryStore::default());
    let legacy = large_secret(3000);
    {
        let mut entries = store.0.0.borrow_mut();
        let mut header = b"RCH1".to_vec();
        header.extend_from_slice(&2u32.to_le_bytes());
        entries.insert("a".into(), header);
        entries.insert("a#0".into(), legacy[..2048].to_vec());
        entries.insert("a#1".into(), legacy[2048..].to_vec());
    }
    assert_eq!(store.load("a").unwrap().unwrap(), legacy);

    let secret = large_secret(7000);
    store.save("a", &secret).unwrap();
    assert_eq!(store.load("a").unwrap().unwrap(), secret);
    let entries = store.0.0.borrow();
    assert_eq!(&entries["a"][..4], b"RCH2");
    assert_eq!(entries.len(), 5);
    assert!(!entries.contains_key("a#0") && !entries.contains_key("a#1"));
}

#[test]
fn small_secret_with_magic_prefix_round_trips() {
    let store = ChunkedStore(MemoryStore::default());
    for secret in [&b"RCH1abcd"[..], &b"RCH2"[..]] {
        store.save("a", secret).unwrap();
        assert_eq!(store.load("a").unwrap().unwrap(), secret);
    }
}

#[test]
fn chunk_digest_mismatch_is_corrupt() {
    let store = ChunkedStore(MemoryStore::default());
    store.save("a", &large_secret(7000)).unwrap();
    store.0.0.borrow_mut().get_mut("a#0.2").unwrap()[5] ^= 1;
    match store.load("a") {
        Err(StoreError::Corrupt(reason)) => assert_eq!(reason, "chunk digest mismatch"),
        Err(other) => panic!("unexpected {other}"),
        Ok(_) => panic!("loaded tampered secret"),
    }
}
