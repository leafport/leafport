//! The real side effects: the network, the OS credential store and the activation lock.

use std::path::Path;
use std::time::Duration;

use leafport_core::activation::{self, ActivationKeys};
use leafport_core::cloud::DownloadRecord;
use leafport_core::library::{self, Item, Library, Metadata, SyncOutcome};
use leafport_core::service::{AdeptResponse, CancelToken, ServiceClient, ServiceError, Transport};
use leafport_core::store::{
    ActivationState, CredentialStore, DEFAULT_ACCOUNT, KeyringStore, StoreError,
};
use leafport_core::vendors::VendorCatalog;

use crate::ops::{Ops, SyncHooks};

/// Serializes every load, device activation and save of the stored activation state, so
/// a sign-in and a sync never both register the device (each /Activate uses a slot).
static ACTIVATION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take `ACTIVATION_LOCK`. It guards no data, so a panic that poisoned it left nothing
/// inconsistent behind and the lock is recovered.
fn activation_lock() -> std::sync::MutexGuard<'static, ()> {
    ACTIVATION_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The operations against the real network and credential store.
pub(crate) struct LiveOps;

impl Ops for LiveOps {
    fn signed_in_user(&self) -> Result<Option<String>, String> {
        ActivationState::load(&KeyringStore::new(), DEFAULT_ACCOUNT)
            .map(|state| state.map(|state| state.user))
            .map_err(|e| e.to_string())
    }

    fn load_providers(&self) -> Result<VendorCatalog, String> {
        let mut catalog = VendorCatalog::new();
        ServiceClient::new()
            .and_then(|client| client.load_vendors(&mut catalog))
            .map_err(|e| e.to_string())?;
        Ok(catalog)
    }

    fn sign_in(
        &self,
        catalog: &VendorCatalog,
        method: &str,
        username: String,
        password: zeroize::Zeroizing<String>,
    ) -> Result<String, String> {
        let info = catalog.service_info();
        let (auth_url, certificate) = match (info.auth_url.clone(), info.certificate.clone()) {
            (Some(auth_url), Some(certificate)) if !certificate.is_empty() => {
                (auth_url, certificate)
            }
            _ => return Err("Discovery has no authentication URL or certificate yet.".into()),
        };
        let client = ServiceClient::new().map_err(|e| e.to_string())?;
        // Device key and activation keys are generated locally for this device.
        // The 16-byte device key is random and persisted with the activation state.
        // Reuse an existing activation so books fulfilled under it stay readable.
        let _guard = activation_lock();
        let existing = match ActivationState::load(&KeyringStore::new(), DEFAULT_ACCOUNT) {
            Ok(existing) => existing,
            Err(e) => {
                return Err(format!(
                    "credential store unreadable: {e} — not replacing existing activation"
                ));
            }
        };
        let (device_key, keys, device, fingerprint, token, operators) = match existing {
            Some(existing) => (
                existing.device_key,
                existing.keys,
                existing.device,
                existing.fingerprint,
                existing.activation_token_xml,
                existing.operator_urls,
            ),
            None => {
                let device_key = activation::generate_device_key();
                let keys = ActivationKeys::generate().map_err(|e| e.to_string())?;
                let device = activation::device_id(&device_key);
                let fingerprint = machine_fingerprint(&device_key);
                (device_key, keys, device, fingerprint, None, Vec::new())
            }
        };
        let signed = activation::perform_sign_in(
            &client,
            &auth_url,
            method,
            &username,
            &password,
            &certificate,
            &device_key,
            &keys,
        )
        .map_err(|e| e.to_string())?;
        // Adopt the account's license and signing keys when the service returns
        // them; the generated keys only apply to a brand-new account.
        let mut keys = keys;
        if let Some(license) = activation::account_license_key(&signed.credentials_xml, &device_key)
        {
            keys.license = license;
        }
        if let Some(authentication) =
            activation::account_auth_key(&signed.credentials_xml, &device_key)
        {
            keys.authentication = authentication;
        }
        let mut state = ActivationState {
            device,
            fingerprint,
            device_key,
            keys,
            user: signed.user.clone(),
            credentials_xml: signed.credentials_xml,
            activation_token_xml: token,
            operator_urls: operators,
        };
        // A token issued to another account, or one that no longer parses, does not
        // carry over. Signing in is an explicit request, so the device registers anew.
        if state.activation_token_xml.is_some()
            && state
                .activation_token()
                .is_none_or(|token| token.user != state.user)
        {
            state.activation_token_xml = None;
            state.operator_urls.clear();
        }
        // Register this device with the activation service (POST /Activate) after
        // signing in. Fulfillment requires the server-issued device,
        // and only sign-in activates: if this fails, the sign-in fails and nothing
        // new is saved, so the user signs in again.
        let activated =
            activation::ensure_device_activation(&client, &mut state).map_err(|e| e.to_string())?;
        save_retrying(&state, &KeyringStore::new(), &SAVE_RETRY_DELAYS).map_err(|e| {
            if activated {
                format!(
                    "this device was activated, but saving the activation failed: {e}. \
                     Signing in again uses another of the account's device slots."
                )
            } else {
                e.to_string()
            }
        })?;
        Ok(signed.user)
    }

    fn sign_out(&self) -> Result<(), String> {
        // A worker saving its operator setup holds the lock from its account check to
        // its save, so the deletion cannot land in between and be undone.
        let _guard = activation_lock();
        KeyringStore::new()
            .delete(DEFAULT_ACCOUNT)
            .map_err(|e| e.to_string())
    }

    fn refresh(&self, cache: &Path) -> Result<Vec<DownloadRecord>, String> {
        let activation = load_activation()?;
        let client = ServiceClient::new().map_err(|e| e.to_string())?;
        library::refresh(cache, &activation, &client).map_err(|e| e.to_string())
    }

    fn sync(
        &self,
        library: &Library,
        acsm: &[u8],
        cancel: CancelToken,
        hooks: SyncHooks,
    ) -> Result<SyncOutcome, String> {
        // Report whole percents only, so a large book sends at most 100 events.
        // 255 means nothing was reported yet.
        let percent = std::sync::atomic::AtomicU8::new(u8::MAX);
        let report = |received: u64, total: Option<u64>| {
            let Some(total) = total.filter(|&total| total > 0) else {
                return;
            };
            let now = (received.min(total) * 100 / total) as u8;
            if percent.swap(now, std::sync::atomic::Ordering::Relaxed) == now {
                return;
            }
            (hooks.progress)(f32::from(now) / 100.0);
        };
        with_fulfillment_activation(Some(cancel), |activation, client| {
            // Cancel turns off once Fulfill is committed; the hook tells the app.
            let transport = ProgressTransport {
                client,
                report: &report,
                on_fulfill: hooks.committed,
            };
            library
                .sync_item(acsm, activation, &transport)
                .map_err(|e| e.to_string())
        })
    }

    fn import(&self, library: &Library, path: &Path) -> Result<SyncOutcome, String> {
        with_fulfillment_activation(None, |activation, client| {
            library
                .import_acsm(path, activation, client)
                .map_err(|e| e.to_string())
        })
    }

    fn return_loan(&self, item: &Item) -> Result<Metadata, String> {
        let activation = load_activation()?;
        let client = ServiceClient::new().map_err(|e| e.to_string())?;
        library::return_loan(item, &activation, &client).map_err(|e| e.to_string())
    }
}

fn load_activation() -> Result<ActivationState, String> {
    ActivationState::load(&KeyringStore::new(), DEFAULT_ACCOUNT)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "not signed in".to_string())
}

/// Pauses before the second and the third attempt to save a signed-in activation. A new
/// device activation uses one of the account's device slots, so a failed save is not
/// given up at once.
const SAVE_RETRY_DELAYS: [Duration; 2] = [Duration::from_millis(500), Duration::from_secs(2)];

/// Save `state` to `store`, trying again after each of `delays` while it fails.
/// Returns the last error.
fn save_retrying(
    state: &ActivationState,
    store: &dyn CredentialStore,
    delays: &[Duration],
) -> Result<(), StoreError> {
    let mut result = state.save(store, DEFAULT_ACCOUNT);
    for delay in delays {
        if result.is_ok() {
            break;
        }
        std::thread::sleep(*delay);
        result = state.save(store, DEFAULT_ACCOUNT);
    }
    result
}

/// A transport whose book downloads report their progress; everything else is the
/// plain client.
struct ProgressTransport<'a> {
    client: &'a ServiceClient,
    report: &'a (dyn Fn(u64, Option<u64>) + Sync),
    on_fulfill: &'a (dyn Fn() + Sync),
}

impl Transport for ProgressTransport<'_> {
    fn post_adept(&self, url: &str, xml: String) -> Result<AdeptResponse, ServiceError> {
        self.client.post_adept(url, xml)
    }

    fn fetch(&self, url: &str, max_bytes: usize) -> Result<AdeptResponse, ServiceError> {
        self.client.fetch(url, max_bytes)
    }

    fn download(&self, url: &str, max_bytes: usize) -> Result<AdeptResponse, ServiceError> {
        self.client
            .download_with_progress(url, max_bytes, self.report)
    }

    fn before_fulfill(&self) -> Result<(), ServiceError> {
        self.client.before_fulfill()?;
        (self.on_fulfill)();
        Ok(())
    }
}

/// Require the device activation made at sign-in, and keep successful operator setup
/// even if the subsequent book download fails, so the next sync reuses it.
fn with_fulfillment_activation<T>(
    cancel: Option<CancelToken>,
    work: impl FnOnce(&mut ActivationState, &ServiceClient) -> Result<T, String>,
) -> Result<T, String> {
    let registration = activation_lock();
    let mut state = load_activation()?;
    let user = state.user.clone();
    let device_key = state.device_key;
    let credentials = state.credentials_xml.clone();
    let mut client = ServiceClient::new().map_err(|e| e.to_string())?;
    if let Some(token) = cancel {
        client = client.with_cancel(token);
    }
    let store = KeyringStore::new();
    let account_is_current = || {
        load_activation().is_ok_and(|current| {
            current.user == user
                && current.device_key == device_key
                && current.credentials_xml == credentials
        })
    };
    // Only sign-in activates a device; a sync without an activation fails.
    if leafport_core::activation::require_device_activation(&mut state)
        .map_err(|e| e.to_string())?
    {
        if !account_is_current() {
            return Err("account changed while checking the device activation".into());
        }
        state
            .save(&store, DEFAULT_ACCOUNT)
            .map_err(|e| format!("could not save device activation: {e}"))?;
    }
    drop(registration);
    let operators = state.operator_urls.clone();
    let result = work(&mut state, &client);
    if state.operator_urls != operators {
        let _registration = activation_lock();
        // Sign-out or a replacement sign-in must not be undone by a late worker.
        if account_is_current()
            && let Err(error) = state.save(&store, DEFAULT_ACCOUNT)
        {
            let error = format!("could not save provider authorization: {error}");
            return Err(match result {
                Ok(_) => error,
                Err(failure) => format!("{failure}; {error}"),
            });
        }
    }
    result
}

/// A machine-derived device fingerprint: Base64 SHA-1 over the host machine id plus the
/// OS user, the ADEPT fingerprint format. It is stable per machine+user. Falls back to
/// the device key when no machine id is available.
fn machine_fingerprint(device_key: &[u8; 16]) -> String {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use sha1::{Digest, Sha1};
    let machine = machine_uid::get().unwrap_or_default();
    if machine.is_empty() {
        return activation::device_fingerprint(device_key);
    }
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();
    let mut hasher = Sha1::new();
    hasher.update(machine.as_bytes());
    hasher.update(b"\0");
    hasher.update(user.as_bytes());
    STANDARD.encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A store whose first `failures` saves fail.
    struct FlakyStore {
        failures: Mutex<usize>,
        saves: Mutex<usize>,
    }

    impl FlakyStore {
        fn new(failures: usize) -> Self {
            Self {
                failures: Mutex::new(failures),
                saves: Mutex::new(0),
            }
        }

        fn saves(&self) -> usize {
            *self.saves.lock().unwrap()
        }
    }

    impl CredentialStore for FlakyStore {
        fn load(&self, _: &str) -> Result<Option<Vec<u8>>, StoreError> {
            Ok(None)
        }

        fn save(&self, _: &str, _: &[u8]) -> Result<(), StoreError> {
            *self.saves.lock().unwrap() += 1;
            let mut failures = self.failures.lock().unwrap();
            if *failures > 0 {
                *failures -= 1;
                return Err(StoreError::Corrupt("locked"));
            }
            Ok(())
        }

        fn delete(&self, _: &str) -> Result<(), StoreError> {
            Ok(())
        }
    }

    fn state() -> ActivationState {
        let device_key = [7; 16];
        ActivationState {
            device_key,
            keys: ActivationKeys::generate().unwrap(),
            user: "urn:uuid:user".into(),
            device: activation::device_id(&device_key),
            fingerprint: activation::device_fingerprint(&device_key),
            credentials_xml: String::new(),
            activation_token_xml: None,
            operator_urls: Vec::new(),
        }
    }

    const NO_WAIT: [Duration; 2] = [Duration::ZERO; 2];

    #[test]
    fn save_retry_waits_before_the_second_and_third_attempt() {
        assert_eq!(SAVE_RETRY_DELAYS.len(), 2);
        assert!(SAVE_RETRY_DELAYS[0] > Duration::ZERO);
        assert!(SAVE_RETRY_DELAYS[1] > SAVE_RETRY_DELAYS[0]);
    }

    #[test]
    fn save_retry_stops_at_the_first_success() {
        let state = state();
        let store = FlakyStore::new(0);
        save_retrying(&state, &store, &NO_WAIT).unwrap();
        assert_eq!(store.saves(), 1);
        let store = FlakyStore::new(2);
        save_retrying(&state, &store, &NO_WAIT).unwrap();
        assert_eq!(store.saves(), 3);
    }

    #[test]
    fn save_retry_gives_up_after_three_attempts() {
        let store = FlakyStore::new(usize::MAX);
        assert!(save_retrying(&state(), &store, &NO_WAIT).is_err());
        assert_eq!(store.saves(), 3);
    }
}
