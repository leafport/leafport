//! Durable activation state behind an OS credential-store platform adapter.
//!
//! The device secret and the two RSA private keys are long-lived authentication
//! material. The design keeps them in the operating system credential store, never
//! in a plaintext file, so this module exposes a `CredentialStore` adapter trait and
//! serializes the activation state to one opaque secret blob with a local schema.

use std::{error::Error, fmt};

use rsa::RsaPrivateKey;
use rsa::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use zeroize::Zeroize;

use crate::activation::ActivationKeys;

/// Account label under which the single active account's state is stored.
pub const DEFAULT_ACCOUNT: &str = "default";

/// Blob format revision. Byte 0 of every stored secret. Version 2 adds the device
/// identifier and fingerprint; version 1 blobs are still read and their device identity
/// is derived from the device key on load.
const BLOB_VERSION: u8 = 2;

/// Complete activation state for one signed-in account.
pub struct ActivationState {
    /// The device's 16-byte secret.
    pub device_key: [u8; 16],
    /// The generated activation key pair (authentication and license).
    pub keys: ActivationKeys,
    /// The account identifier returned by SignInDirect.
    pub user: String,
    /// The device identifier sent in activation and fulfillment (activation://device/UUID).
    pub device: String,
    /// The device fingerprint sent in the fulfill request's device descriptor.
    pub fingerprint: String,
    /// The credential record returned by the service, retained verbatim.
    pub credentials_xml: String,
}

#[derive(Debug)]
pub enum StoreError {
    /// A stored blob is truncated, over-long, or uses an unknown version.
    Corrupt(&'static str),
    /// Serializing or deserializing an RSA private key failed.
    Key(String),
    /// The underlying credential store failed.
    Backend(Box<dyn Error + Send + Sync>),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Corrupt(reason) => write!(f, "stored activation state is unreadable: {reason}"),
            Self::Key(error) => write!(f, "activation key (de)serialization failed: {error}"),
            Self::Backend(error) => write!(f, "credential store failed: {error}"),
        }
    }
}
impl Error for StoreError {}

/// Platform adapter over the OS credential store. Secrets are opaque bytes; the
/// adapter never interprets them. `load` returns `None` when no record exists.
pub trait CredentialStore {
    fn load(&self, account: &str) -> Result<Option<Vec<u8>>, StoreError>;
    fn save(&self, account: &str, secret: &[u8]) -> Result<(), StoreError>;
    fn delete(&self, account: &str) -> Result<(), StoreError>;
}

fn push_field(blob: &mut Vec<u8>, bytes: &[u8]) {
    blob.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    blob.extend_from_slice(bytes);
}

fn take_field<'a>(cursor: &mut &'a [u8]) -> Result<&'a [u8], StoreError> {
    if cursor.len() < 4 {
        return Err(StoreError::Corrupt("missing length prefix"));
    }
    let length = u32::from_le_bytes([cursor[0], cursor[1], cursor[2], cursor[3]]) as usize;
    let body = &cursor[4..];
    if body.len() < length {
        return Err(StoreError::Corrupt("field exceeds blob"));
    }
    let (field, rest) = body.split_at(length);
    *cursor = rest;
    Ok(field)
}

impl ActivationState {
    /// Serialize to the opaque secret blob. The caller stores the result only in the
    /// credential store. The returned Vec is sensitive; callers should zeroize it.
    fn to_blob(&self) -> Result<Vec<u8>, StoreError> {
        let mut auth = self
            .keys
            .authentication
            .to_pkcs8_der()
            .map_err(|error| StoreError::Key(error.to_string()))?
            .as_bytes()
            .to_vec();
        let mut license = self
            .keys
            .license
            .to_pkcs8_der()
            .map_err(|error| StoreError::Key(error.to_string()))?
            .as_bytes()
            .to_vec();
        let mut blob = Vec::with_capacity(1 + 16 + auth.len() + license.len() + 64);
        blob.push(BLOB_VERSION);
        blob.extend_from_slice(&self.device_key);
        push_field(&mut blob, &auth);
        push_field(&mut blob, &license);
        push_field(&mut blob, self.user.as_bytes());
        push_field(&mut blob, self.device.as_bytes());
        push_field(&mut blob, self.fingerprint.as_bytes());
        push_field(&mut blob, self.credentials_xml.as_bytes());
        auth.zeroize();
        license.zeroize();
        Ok(blob)
    }

    /// Restore activation state from the opaque secret blob. Version 1 blobs lack the
    /// device identity, which is derived from the device key for backward compatibility.
    fn from_blob(blob: &[u8]) -> Result<Self, StoreError> {
        let version = blob.first().copied();
        if !matches!(version, Some(1) | Some(2)) {
            return Err(StoreError::Corrupt("unknown blob version"));
        }
        if blob.len() < 1 + 16 {
            return Err(StoreError::Corrupt("missing device key"));
        }
        let mut device_key = [0u8; 16];
        device_key.copy_from_slice(&blob[1..17]);
        let mut cursor = &blob[17..];
        let authentication = RsaPrivateKey::from_pkcs8_der(take_field(&mut cursor)?)
            .map_err(|error| StoreError::Key(error.to_string()))?;
        let license = RsaPrivateKey::from_pkcs8_der(take_field(&mut cursor)?)
            .map_err(|error| StoreError::Key(error.to_string()))?;
        let user = String::from_utf8(take_field(&mut cursor)?.to_vec())
            .map_err(|_| StoreError::Corrupt("user is not UTF-8"))?;
        let (device, fingerprint) = if version == Some(2) {
            let device = String::from_utf8(take_field(&mut cursor)?.to_vec())
                .map_err(|_| StoreError::Corrupt("device is not UTF-8"))?;
            let fingerprint = String::from_utf8(take_field(&mut cursor)?.to_vec())
                .map_err(|_| StoreError::Corrupt("fingerprint is not UTF-8"))?;
            (device, fingerprint)
        } else {
            (
                crate::activation::device_id(&device_key),
                crate::activation::device_fingerprint(&device_key),
            )
        };
        let credentials_xml = String::from_utf8(take_field(&mut cursor)?.to_vec())
            .map_err(|_| StoreError::Corrupt("credentials are not UTF-8"))?;
        Ok(Self {
            device_key,
            keys: ActivationKeys {
                authentication,
                license,
            },
            user,
            device,
            fingerprint,
            credentials_xml,
        })
    }

    /// Persist this state under `account` in the given credential store.
    pub fn save(&self, store: &dyn CredentialStore, account: &str) -> Result<(), StoreError> {
        let mut blob = self.to_blob()?;
        let result = store.save(account, &blob);
        blob.zeroize();
        result
    }

    /// Load state for `account`, or `None` when no record exists.
    pub fn load(store: &dyn CredentialStore, account: &str) -> Result<Option<Self>, StoreError> {
        match store.load(account)? {
            Some(mut blob) => {
                let state = Self::from_blob(&blob);
                blob.zeroize();
                state.map(Some)
            }
            None => Ok(None),
        }
    }
}

#[cfg(feature = "keyring")]
pub use keyring_store::KeyringStore;

#[cfg(feature = "keyring")]
mod keyring_store {
    use super::{CredentialStore, StoreError};

    /// Default service name for the OS credential store entry.
    pub const DEFAULT_SERVICE: &str = "net.leafport.activation";

    /// OS credential-store adapter: Secret Service / keyutils on Linux, Keychain on
    /// macOS, Credential Manager on Windows, as provided by the `keyring` crate.
    pub struct KeyringStore {
        service: String,
    }

    impl KeyringStore {
        pub fn new() -> Self {
            Self {
                service: DEFAULT_SERVICE.to_owned(),
            }
        }

        pub fn with_service(service: impl Into<String>) -> Self {
            Self {
                service: service.into(),
            }
        }

        fn entry(&self, account: &str) -> Result<keyring::Entry, StoreError> {
            keyring::Entry::new(&self.service, account)
                .map_err(|error| StoreError::Backend(Box::new(error)))
        }
    }

    impl Default for KeyringStore {
        fn default() -> Self {
            Self::new()
        }
    }

    impl CredentialStore for KeyringStore {
        fn load(&self, account: &str) -> Result<Option<Vec<u8>>, StoreError> {
            match self.entry(account)?.get_secret() {
                Ok(secret) => Ok(Some(secret)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(error) => Err(StoreError::Backend(Box::new(error))),
            }
        }

        fn save(&self, account: &str, secret: &[u8]) -> Result<(), StoreError> {
            self.entry(account)?
                .set_secret(secret)
                .map_err(|error| StoreError::Backend(Box::new(error)))
        }

        fn delete(&self, account: &str) -> Result<(), StoreError> {
            match self.entry(account)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(error) => Err(StoreError::Backend(Box::new(error))),
            }
        }
    }
}
