//! UI-free per-title books library.

pub mod acsm;
pub mod metadata;
pub mod naming;
pub mod paths;
pub mod publication;
pub mod scan;
pub mod settings;

use std::fmt;

use crate::cloud::CloudError;
use crate::fulfillment::{AcsmError, FulfillError};

pub use acsm::{AcsmSummary, Format, acsm_summary, src_title};
pub use metadata::{Loan, METADATA_FILE, Metadata, Source, now_rfc3339};
pub use naming::{clash_suffix, folder_name};
pub use publication::{
    Cover, decrypt_publication, epub_loan_token, extract_cover, pdfium_available,
};
pub use scan::{Item, ItemState, Library, LocalItem, merge};
pub use settings::{Settings, Theme};

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

/// Writes `bytes` to `path` via a sibling temp file and rename.
pub(crate) fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<(), LibraryError> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    if let Err(e) = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}
