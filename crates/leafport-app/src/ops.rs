//! The side effects behind the app: network, keyring and file paths.

use std::path::{Path, PathBuf};

use leafport_core::cloud::DownloadRecord;
use leafport_core::library::{Item, Library, Metadata, SyncOutcome, paths};
use leafport_core::service::CancelToken;
use leafport_core::vendors::VendorCatalog;
use zeroize::Zeroizing;

/// Where the app keeps its settings and the cloud list cache.
#[derive(Clone, Debug, PartialEq)]
pub struct Paths {
    /// `settings.yaml`; `None` when no config folder could be determined.
    pub settings: Option<PathBuf>,
    /// The cached SyncToDevice response; `None` when no data folder could be determined.
    pub cache: Option<PathBuf>,
}

impl Paths {
    /// The platform's settings and cache locations.
    pub fn platform() -> Paths {
        Paths {
            settings: paths::settings_path(),
            cache: paths::cache_path(),
        }
    }
}

/// Callbacks a running sync reports through.
pub struct SyncHooks<'a> {
    /// Download progress in whole percents, as a fraction in `0.0..=1.0`.
    pub progress: &'a (dyn Fn(f32) + Sync),
    /// Fulfill is about to be sent; the sync can no longer be cancelled.
    pub committed: &'a (dyn Fn() + Sync),
}

/// Operations with side effects. The app calls them on worker threads; each may block.
pub trait Ops: Send + Sync + 'static {
    /// The activated account label from the credential store, when signed in.
    fn signed_in_user(&self) -> Result<Option<String>, String>;

    /// The sign-in provider catalog.
    fn load_providers(&self) -> Result<VendorCatalog, String>;

    /// Sign in with `method` from `catalog`; returns the activated account label. The
    /// password is wiped from memory when dropped.
    fn sign_in(
        &self,
        catalog: &VendorCatalog,
        method: &str,
        username: String,
        password: Zeroizing<String>,
    ) -> Result<String, String>;

    /// Forget the stored activation.
    fn sign_out(&self) -> Result<(), String>;

    /// Fetch the cloud list and store it at `cache`.
    fn refresh(&self, cache: &Path) -> Result<Vec<DownloadRecord>, String>;

    /// Fulfill `acsm` into `library`.
    fn sync(
        &self,
        library: &Library,
        acsm: &[u8],
        cancel: CancelToken,
        hooks: SyncHooks,
    ) -> Result<SyncOutcome, String>;

    /// Fulfill the ACSM file at `path` into `library`.
    fn import(&self, library: &Library, path: &Path) -> Result<SyncOutcome, String>;

    /// Return the loaned `item` to its operator.
    fn return_loan(&self, item: &Item) -> Result<Metadata, String>;
}
