//! Per-title `metadata.yaml`.

use std::path::Path;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use super::{LibraryError, write_atomic};

pub const METADATA_FILE: &str = "metadata.yaml";

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Cloud,
    Import,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Loan {
    pub operator_url: String,
    #[serde(default)]
    pub expires: Option<String>,
    pub token: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Metadata {
    pub resource: String,
    pub title: String,
    #[serde(default)]
    pub author: Option<String>,
    pub format: String,
    #[serde(default)]
    pub fulfillment: Option<String>,
    pub source: Source,
    pub synced_at: String,
    #[serde(default)]
    pub returned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loan: Option<Loan>,
}

impl Metadata {
    pub fn read(folder: &Path) -> Result<Metadata, LibraryError> {
        let text = std::fs::read_to_string(folder.join(METADATA_FILE))?;
        serde_yaml_ng::from_str(&text).map_err(|e| LibraryError::Yaml(e.to_string()))
    }

    pub fn write(&self, folder: &Path) -> Result<(), LibraryError> {
        let text = serde_yaml_ng::to_string(self).map_err(|e| LibraryError::Yaml(e.to_string()))?;
        write_atomic(&folder.join(METADATA_FILE), text.as_bytes())
    }
}

pub fn now_rfc3339() -> String {
    humantime::format_rfc3339_seconds(SystemTime::now()).to_string()
}
