mod common;

use std::{cell::RefCell, collections::HashMap};

use leafport_core::activation::{ActivationKeys, device_fingerprint, device_id};
use leafport_core::store::{ActivationState, CredentialStore, DEFAULT_ACCOUNT, StoreError};

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
