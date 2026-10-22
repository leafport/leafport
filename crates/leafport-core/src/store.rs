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
/// is derived from the device key on load. Version 3 adds the server-issued activation
/// token and the initialized operator URLs; older blobs load without them.
const BLOB_VERSION: u8 = 3;

/// Complete activation state for one signed-in account.
pub struct ActivationState {
    /// The device's 16-byte secret.
    pub device_key: [u8; 16],
    /// The generated activation key pair (authentication and license).
    pub keys: ActivationKeys,
    /// The account identifier returned by SignInDirect.
    pub user: String,
    /// The device identifier sent in fulfillment: the server-issued urn:uuid once the
    /// device is activated, else the provisional activation://device/UUID.
    pub device: String,
    /// The device fingerprint sent in the fulfill request's device descriptor.
    pub fingerprint: String,
    /// The credential record returned by the service, retained verbatim.
    pub credentials_xml: String,
    /// Server-issued activationToken XML from /Activate, verbatim. None until activated.
    pub activation_token_xml: Option<String>,
    /// Operator URLs whose license service was initialized.
    pub operator_urls: Vec<String>,
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
        push_field(
            &mut blob,
            self.activation_token_xml
                .as_deref()
                .unwrap_or("")
                .as_bytes(),
        );
        push_field(&mut blob, self.operator_urls.join("\n").as_bytes());
        auth.zeroize();
        license.zeroize();
        Ok(blob)
    }

    /// Restore activation state from the opaque secret blob. Version 1 blobs lack the
    /// device identity, which is derived from the device key for backward compatibility.
    fn from_blob(blob: &[u8]) -> Result<Self, StoreError> {
        let version = blob.first().copied();
        if !matches!(version, Some(1..=3)) {
            return Err(StoreError::Corrupt("unknown blob version"));
        }
        if blob.len() < 1 + 16 {
            return Err(StoreError::Corrupt("missing device key"));
        }
        let mut device_key = [0u8; 16];
        device_key.copy_from_slice(&blob[1..17]);
        let mut cursor = &blob[17..];
        let mut authentication = RsaPrivateKey::from_pkcs8_der(take_field(&mut cursor)?)
            .map_err(|error| StoreError::Key(error.to_string()))?;
        let mut license = RsaPrivateKey::from_pkcs8_der(take_field(&mut cursor)?)
            .map_err(|error| StoreError::Key(error.to_string()))?;
        let user = String::from_utf8(take_field(&mut cursor)?.to_vec())
            .map_err(|_| StoreError::Corrupt("user is not UTF-8"))?;
        let (device, fingerprint) = if version >= Some(2) {
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
        let (activation_token_xml, operator_urls) = if version >= Some(3) {
            let token = String::from_utf8(take_field(&mut cursor)?.to_vec())
                .map_err(|_| StoreError::Corrupt("activation token is not UTF-8"))?;
            let operators = String::from_utf8(take_field(&mut cursor)?.to_vec())
                .map_err(|_| StoreError::Corrupt("operator URLs are not UTF-8"))?;
            (
                Some(token).filter(|token| !token.is_empty()),
                operators
                    .split('\n')
                    .filter(|url| !url.is_empty())
                    .map(str::to_owned)
                    .collect(),
            )
        } else {
            (None, Vec::new())
        };
        // States saved before the account key was adopted hold the locally generated
        // key; the service's account license key in the credentials takes precedence.
        if let Some(account) = crate::activation::account_license_key(&credentials_xml, &device_key)
        {
            license = account;
        }
        // Likewise for the account signing key held in the credentials' pkcs12.
        if let Some(account) = crate::activation::account_auth_key(&credentials_xml, &device_key) {
            authentication = account;
        }
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
            activation_token_xml,
            operator_urls,
        })
    }

    /// The stored server-issued activation token, if the device has been activated.
    pub fn activation_token(&self) -> Option<crate::activation::ActivationToken> {
        self.activation_token_xml
            .as_deref()
            .and_then(|xml| crate::activation::parse_activation_token(xml).ok())
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

/// Largest secret stored in a single entry. Windows Credential Manager caps a blob at
/// 2560 bytes; larger secrets are split across `"{account}#{gen}.{i}"` entries.
pub const CHUNK_SIZE: usize = 2048;

/// Header magic of a legacy chunked secret: the magic followed by the u32 LE chunk
/// count, with chunks in `"{account}#{i}"`. Still read; never written.
pub const CHUNK_MAGIC_V1: &[u8; 4] = b"RCH1";

/// Header magic of a generational chunked secret: the magic, the u32 LE generation,
/// the u32 LE chunk count and the SHA-1 digest of the whole secret. Chunks live in
/// `"{account}#{gen}.{i}"`, so a save writes a fresh generation and flips the header.
pub const CHUNK_MAGIC: &[u8; 4] = b"RCH2";

/// Extra chunk indexes probed past the known count when sweeping leftovers.
const SWEEP_EXTRA: usize = 8;

const DIGEST_LEN: usize = 20;

/// Which chunk entries a header entry refers to.
#[derive(Clone, Copy)]
enum ChunkLayout {
    /// No chunks are known (absent, unchunked or unreadable header).
    Unknown,
    Legacy {
        count: usize,
    },
    Generation {
        generation: u32,
        count: usize,
    },
}

/// Adapter that splits secrets larger than `CHUNK_SIZE` across several entries of the
/// wrapped store. Secrets stored unchunked by an older version still load as-is.
pub struct ChunkedStore<S: CredentialStore>(pub S);

impl<S: CredentialStore> ChunkedStore<S> {
    fn chunk_account(account: &str, layout: ChunkLayout, index: usize) -> String {
        match layout {
            ChunkLayout::Generation { generation, .. } => format!("{account}#{generation}.{index}"),
            _ => format!("{account}#{index}"),
        }
    }

    fn parse_header(header: &[u8]) -> Result<ChunkLayout, StoreError> {
        let word = |at: usize| {
            u32::from_le_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]])
        };
        if header.starts_with(CHUNK_MAGIC_V1) {
            if header.len() != 8 {
                return Err(StoreError::Corrupt("malformed chunk header"));
            }
            Ok(ChunkLayout::Legacy {
                count: word(4) as usize,
            })
        } else if header.starts_with(CHUNK_MAGIC) {
            if header.len() != 12 + DIGEST_LEN {
                return Err(StoreError::Corrupt("malformed chunk header"));
            }
            Ok(ChunkLayout::Generation {
                generation: word(4),
                count: word(8) as usize,
            })
        } else {
            Ok(ChunkLayout::Unknown)
        }
    }

    /// The chunk layout currently referenced by `account`'s header entry.
    fn current_layout(&self, account: &str) -> Result<ChunkLayout, StoreError> {
        match self.0.load(account)? {
            Some(mut header) => {
                let layout = Self::parse_header(&header).unwrap_or(ChunkLayout::Unknown);
                header.zeroize();
                Ok(layout)
            }
            None => Ok(ChunkLayout::Unknown),
        }
    }

    /// Delete the `layout`'s chunks from `first`, then probe `SWEEP_EXTRA` further
    /// indexes. Gaps do not stop the sweep, so no fragment is orphaned.
    fn delete_chunks(
        &self,
        account: &str,
        layout: ChunkLayout,
        first: usize,
    ) -> Result<(), StoreError> {
        let count = match layout {
            ChunkLayout::Unknown => 0,
            ChunkLayout::Legacy { count } | ChunkLayout::Generation { count, .. } => count,
        };
        for index in first..count.max(first) + SWEEP_EXTRA {
            let name = Self::chunk_account(account, layout, index);
            if let Some(mut chunk) = self.0.load(&name)? {
                chunk.zeroize();
                self.0.delete(&name)?;
            }
        }
        Ok(())
    }

    /// Probe-only layout of a generation adjacent to `layout`'s, which an interrupted
    /// save may have left behind.
    fn neighbour(layout: ChunkLayout, step: i32) -> Option<ChunkLayout> {
        match layout {
            ChunkLayout::Generation { generation, .. } => Some(ChunkLayout::Generation {
                generation: generation.wrapping_add_signed(step),
                count: 0,
            }),
            _ => None,
        }
    }

    /// Delete every chunk `layout` refers to plus leftovers of the adjacent generations.
    fn delete_all_chunks(&self, account: &str, layout: ChunkLayout) -> Result<(), StoreError> {
        for stale in [Self::neighbour(layout, -1), Self::neighbour(layout, 1)]
            .into_iter()
            .flatten()
        {
            self.delete_chunks(account, stale, 0)?;
        }
        self.delete_chunks(account, layout, 0)
    }

    fn read_chunks(
        &self,
        account: &str,
        layout: ChunkLayout,
        count: usize,
    ) -> Result<Vec<u8>, StoreError> {
        let mut secret = Vec::new();
        for index in 0..count {
            match self.0.load(&Self::chunk_account(account, layout, index)) {
                Ok(Some(mut chunk)) => {
                    secret.extend_from_slice(&chunk);
                    chunk.zeroize();
                }
                Ok(None) => {
                    secret.zeroize();
                    return Err(StoreError::Corrupt("missing chunk"));
                }
                Err(error) => {
                    secret.zeroize();
                    return Err(error);
                }
            }
        }
        Ok(secret)
    }

    fn digest(secret: &[u8]) -> [u8; DIGEST_LEN] {
        use sha1::{Digest, Sha1};
        Sha1::digest(secret).into()
    }
}

impl<S: CredentialStore> CredentialStore for ChunkedStore<S> {
    fn load(&self, account: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let Some(header) = self.0.load(account)? else {
            return Ok(None);
        };
        let layout = Self::parse_header(&header)?;
        match layout {
            ChunkLayout::Unknown => Ok(Some(header)),
            ChunkLayout::Legacy { count } => self.read_chunks(account, layout, count).map(Some),
            ChunkLayout::Generation { count, .. } => {
                let mut secret = self.read_chunks(account, layout, count)?;
                if Self::digest(&secret)[..] != header[12..] {
                    secret.zeroize();
                    return Err(StoreError::Corrupt("chunk digest mismatch"));
                }
                Ok(Some(secret))
            }
        }
    }

    fn save(&self, account: &str, secret: &[u8]) -> Result<(), StoreError> {
        let old = self.current_layout(account)?;
        // A small secret that happens to begin with either magic is chunked too, so load
        // never misreads it as a header.
        if secret.len() <= CHUNK_SIZE
            && !secret.starts_with(CHUNK_MAGIC)
            && !secret.starts_with(CHUNK_MAGIC_V1)
        {
            self.0.save(account, secret)?;
            return self.delete_all_chunks(account, old);
        }
        // An interrupted cleanup may have left the generation before `old` behind.
        if let Some(stale) = Self::neighbour(old, -1) {
            self.delete_chunks(account, stale, 0)?;
        }
        let generation = match old {
            ChunkLayout::Generation { generation, .. } => generation.wrapping_add(1),
            _ => 0,
        };
        let mut layout = ChunkLayout::Generation {
            generation,
            count: 0,
        };
        // New chunks go to a fresh generation; the old header stays valid until the
        // header write below flips to it.
        let mut count = 0;
        for chunk in secret.chunks(CHUNK_SIZE) {
            self.0
                .save(&Self::chunk_account(account, layout, count), chunk)?;
            count += 1;
        }
        layout = ChunkLayout::Generation { generation, count };
        let mut header = CHUNK_MAGIC.to_vec();
        header.extend_from_slice(&generation.to_le_bytes());
        header.extend_from_slice(&(count as u32).to_le_bytes());
        header.extend_from_slice(&Self::digest(secret));
        self.0.save(account, &header)?;
        // Sweep leftovers of an earlier interrupted save into this generation.
        self.delete_chunks(account, layout, count)?;
        self.delete_chunks(account, old, 0)
    }

    fn delete(&self, account: &str) -> Result<(), StoreError> {
        // Chunks first, header last: an interrupted delete keeps the header, so a later
        // delete still knows which chunks to remove.
        let layout = self.current_layout(account)?;
        self.delete_all_chunks(account, layout)?;
        self.0.delete(account)
    }
}

pub use keyring_store::KeyringStore;

mod keyring_store {
    use super::{ChunkedStore, CredentialStore, StoreError};

    /// Default service name for the OS credential store entry.
    pub const DEFAULT_SERVICE: &str = "net.leafport.activation";

    /// OS credential-store adapter: Secret Service / keyutils on Linux, Keychain on
    /// macOS, Credential Manager on Windows, as provided by the `keyring` crate.
    /// Secrets are chunked to stay under Credential Manager's per-blob limit.
    pub struct KeyringStore(ChunkedStore<RawKeyringStore>);

    impl KeyringStore {
        pub fn new() -> Self {
            Self::with_service(DEFAULT_SERVICE)
        }

        pub fn with_service(service: impl Into<String>) -> Self {
            Self(ChunkedStore(RawKeyringStore {
                service: service.into(),
            }))
        }
    }

    impl Default for KeyringStore {
        fn default() -> Self {
            Self::new()
        }
    }

    impl CredentialStore for KeyringStore {
        fn load(&self, account: &str) -> Result<Option<Vec<u8>>, StoreError> {
            self.0.load(account)
        }

        fn save(&self, account: &str, secret: &[u8]) -> Result<(), StoreError> {
            self.0.save(account, secret)
        }

        fn delete(&self, account: &str) -> Result<(), StoreError> {
            self.0.delete(account)
        }
    }

    /// One keyring entry per account, without chunking.
    struct RawKeyringStore {
        service: String,
    }

    impl RawKeyringStore {
        fn entry(&self, account: &str) -> Result<keyring::Entry, StoreError> {
            keyring::Entry::new(&self.service, account)
                .map_err(|error| StoreError::Backend(Box::new(error)))
        }
    }

    impl CredentialStore for RawKeyringStore {
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
