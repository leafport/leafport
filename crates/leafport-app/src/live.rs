//! The real side effects: the network, the OS credential store and the activation lock.

use std::path::Path;

use leafport_core::activation::{self, ActivationKeys};
use leafport_core::cloud::DownloadRecord;
use leafport_core::library::{self, Item, Library, Metadata, SyncOutcome};
use leafport_core::service::{AdeptResponse, CancelToken, ServiceClient, ServiceError, Transport};
use leafport_core::store::{ActivationState, CredentialStore, DEFAULT_ACCOUNT, KeyringStore};
use leafport_core::vendors::VendorCatalog;

use crate::ops::{Ops, SyncHooks};

/// Serializes every load, device activation and save of the stored activation state, so
/// a sign-in and a sync never both register the device (each /Activate uses a slot).
static ACTIVATION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
        password: String,
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
        let _guard = ACTIVATION_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        activation::ensure_device_activation(&client, &mut state).map_err(|e| e.to_string())?;
        state
            .save(&KeyringStore::new(), DEFAULT_ACCOUNT)
            .map_err(|e| e.to_string())?;
        Ok(signed.user)
    }

    fn sign_out(&self) -> Result<(), String> {
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
    let registration = ACTIVATION_LOCK
        .lock()
        .map_err(|_| "device activation lock is unavailable".to_string())?;
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
        let _registration = ACTIVATION_LOCK
            .lock()
            .map_err(|_| "device activation lock is unavailable".to_string())?;
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
