//! UI-free per-title books library.

pub mod acsm;
pub mod metadata;
pub mod naming;
pub mod paths;
pub mod publication;
pub mod scan;
pub mod settings;
pub mod sync;

use std::fmt;

use crate::cloud::CloudError;
use crate::fulfillment::{AcsmError, FulfillError};

pub use acsm::{AcsmSummary, Format, acsm_summary, src_title};
pub use metadata::{Loan, METADATA_FILE, Metadata, Source, now_rfc3339};
pub use naming::{folder_name, full_id_suffix, id_suffix};
pub use publication::{
    Cover, book_title, decrypt_publication, epub_loan_token, extract_cover, pdfium_available,
};
pub use scan::{Item, ItemState, Library, LocalItem, merge};
pub use settings::{Settings, Theme};
pub use sync::{SyncOutcome, load_cache, refresh, return_loan, target_descriptor};

#[derive(Debug)]
pub enum LibraryError {
    Io(std::io::Error),
    Yaml(String),
    Acsm(AcsmError),
    Fulfill(FulfillError),
    Cloud(CloudError),
    Drm(String),
    UnsupportedFormat,
    NotOnLoan,
    NoFolder,
}

impl fmt::Display for LibraryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => e.fmt(f),
            Self::Yaml(e) => write!(f, "invalid YAML: {e}"),
            Self::Acsm(e) => e.fmt(f),
            Self::Fulfill(e) => e.fmt(f),
            Self::Cloud(e) => e.fmt(f),
            Self::Drm(e) => write!(f, "DRM error: {e}"),
            Self::UnsupportedFormat => f.write_str("unsupported book format"),
            Self::NotOnLoan => f.write_str("item is not on loan"),
            Self::NoFolder => f.write_str("no books folder configured"),
        }
    }
}

impl std::error::Error for LibraryError {}

impl From<std::io::Error> for LibraryError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<AcsmError> for LibraryError {
    fn from(e: AcsmError) -> Self {
        Self::Acsm(e)
    }
}
impl From<FulfillError> for LibraryError {
    fn from(e: FulfillError) -> Self {
        Self::Fulfill(e)
    }
}
impl From<CloudError> for LibraryError {
    fn from(e: CloudError) -> Self {
        Self::Cloud(e)
    }
}

/// Writes `bytes` to `path` via a sibling temp file and rename. The temp file is flushed
/// to disk before the rename, so a crash leaves the old or the new file, never an empty
/// one.
pub(crate) fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<(), LibraryError> {
    use std::io::Write;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    let written = std::fs::File::create(&tmp).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    if let Err(e) = written.and_then(|()| std::fs::rename(&tmp, path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    // Make the rename itself durable. Best effort: not every platform can open a folder.
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        let _ = std::fs::File::open(parent).and_then(|dir| dir.sync_all());
    }
    Ok(())
}
