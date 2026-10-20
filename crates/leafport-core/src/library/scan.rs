//! Scanning the books folder and merging it with cloud records.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::{AcsmSummary, Format, LibraryError, METADATA_FILE, Metadata, acsm_summary, src_title};
use crate::cloud::DownloadRecord;

#[derive(Clone, Debug)]
pub struct LocalItem {
    pub folder: PathBuf,
    pub metadata: Option<Metadata>,
    pub publication: Option<PathBuf>,
    pub cover: Option<PathBuf>,
    pub warning: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Library {
    pub root: PathBuf,
    pub local: Vec<LocalItem>,
}

impl Library {
    /// Creates `root` if needed, sweeps leftover sync temp folders and reads every title folder.
    pub fn open(root: &Path) -> Result<Library, LibraryError> {
        std::fs::create_dir_all(root)?;
        Self::scan(root, true)
    }

    /// Like `open`, but leaves sync temp folders alone. Callers rescan while a sync may be
    /// running, because its live temp folder must not be deleted.
    pub fn rescan(root: &Path) -> Result<Library, LibraryError> {
        std::fs::create_dir_all(root)?;
        Self::scan(root, false)
    }

    fn scan(root: &Path, sweep: bool) -> Result<Library, LibraryError> {
        let mut local = Vec::new();
        // Snapshot the entries so a folder restored below is not listed twice.
        let entries = std::fs::read_dir(root)?.collect::<Result<Vec<_>, _>>()?;
        for entry in entries {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let mut path = entry.path();
            let mut hidden = name.starts_with('.');
            if let Some(original) = set_aside_name(&name) {
                // A previous copy set aside by a sync: restore it unless its folder exists.
                if !sweep {
                    continue;
                }
                let target = root.join(original);
                if target.exists() {
                    let _ = std::fs::remove_dir_all(&path);
                    continue;
                }
                if std::fs::rename(&path, &target).is_err() {
                    continue;
                }
                path = target;
                hidden = false;
            } else if is_temp_name(&name) && path.is_dir() {
                // Best effort: a stale temp that cannot be removed must not fail the scan.
                if sweep {
                    let _ = std::fs::remove_dir_all(&path);
                }
                continue;
            }
            if hidden || !path.is_dir() || !path.join(METADATA_FILE).is_file() {
                continue;
            }
            local.push(read_folder(path));
        }
        local.sort_by(|a, b| a.folder.cmp(&b.folder));
        Ok(Library {
            root: root.to_owned(),
            local,
        })
    }
}

/// Whether `name` is a sync temp folder name: `.tmp-` and 16 lowercase hex digits.
/// The books folder is user-chosen, so any other `.tmp-` entry is left alone.
fn is_temp_name(name: &str) -> bool {
    name.strip_prefix(".tmp-").is_some_and(|hex| {
        hex.len() == 16 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// The folder name in a `.old-<16 hex>-<name>` entry.
fn set_aside_name(name: &str) -> Option<&str> {
    let rest = name.strip_prefix(".old-")?;
    let (hex, original) = rest.split_at_checked(16)?;
    let original = original.strip_prefix('-')?;
    (hex.bytes().all(|b| b.is_ascii_hexdigit()) && !original.is_empty()).then_some(original)
}

fn read_folder(folder: PathBuf) -> LocalItem {
    let (metadata, warning) = match Metadata::read(&folder) {
        Ok(m) => (Some(m), None),
        Err(e) => (None, Some(format!("metadata.yaml is malformed: {e}"))),
    };
    let mut publications = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&folder) {
        for entry in entries.flatten() {
            let path = entry.path();
            let is_pub = path.is_file()
                && path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e == "epub" || e == "pdf");
            if is_pub {
                publications.push(path);
            }
        }
    }
    publications.sort();
    let cover = ["cover.jpg", "cover.png"]
        .iter()
        .map(|n| folder.join(n))
        .find(|p| p.is_file());
    LocalItem {
        folder,
        metadata,
        publication: publications.into_iter().next(),
        cover,
        warning,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ItemState {
    Available,
    Synced,
    OnLoan { expires: Option<String> },
    Returned,
    Local,
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct Item {
    pub key: String,
    pub title: String,
    pub author: Option<String>,
    pub format: Format,
    pub state: ItemState,
    pub acsm: Option<Vec<u8>>,
    pub folder: Option<PathBuf>,
    pub publication: Option<PathBuf>,
    pub cover: Option<PathBuf>,
    pub metadata: Option<Metadata>,
    pub warning: Option<String>,
}

fn matched_state(m: &Metadata) -> ItemState {
    if let Some(loan) = &m.loan {
        ItemState::OnLoan {
            expires: loan.expires.clone(),
        }
    } else if m.returned {
        ItemState::Returned
    } else {
        ItemState::Synced
    }
}

fn folder_label(folder: &Path) -> String {
    folder
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Joins cloud records and local folders into display rows, sorted by title then key.
pub fn merge(cloud: &[DownloadRecord], local: &[LocalItem]) -> Vec<Item> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    let mut used = vec![false; local.len()];
    let mut claimed = HashSet::new();
    let duplicate: Vec<bool> = local
        .iter()
        .map(|l| {
            l.metadata
                .as_ref()
                .is_some_and(|m| !claimed.insert(m.resource.clone()))
        })
        .collect();

    for record in cloud {
        let Ok(summary) = acsm_summary(&record.acsm) else {
            continue;
        };
        if !seen.insert(summary.key.clone()) {
            continue;
        }
        let hit = local.iter().position(|l| {
            l.metadata
                .as_ref()
                .is_some_and(|m| m.resource == summary.key)
        });
        let AcsmSummary {
            key,
            title,
            author,
            format,
            src,
            ..
        } = summary;
        let acsm_title = title.or_else(|| src.as_deref().and_then(src_title));
        let mut item = Item {
            title: acsm_title.clone().unwrap_or_else(|| key.clone()),
            key,
            author,
            format,
            state: ItemState::Available,
            acsm: Some(record.acsm.clone()),
            folder: None,
            publication: None,
            cover: None,
            metadata: None,
            warning: None,
        };
        if let Some(i) = hit {
            used[i] = true;
            let l = &local[i];
            let m = l.metadata.as_ref().expect("matched local has metadata");
            item.title = if m.title.is_empty() {
                acsm_title.unwrap_or_else(|| item.key.clone())
            } else {
                m.title.clone()
            };
            item.author = m.author.clone().or(item.author);
            item.format = match Format::from_mime(&m.format) {
                Format::Unknown => item.format,
                f => f,
            };
            item.state = matched_state(m);
            item.folder = Some(l.folder.clone());
            item.publication = l.publication.clone();
            item.cover = l.cover.clone();
            item.metadata = Some(m.clone());
            item.warning = l.warning.clone();
        }
        items.push(item);
    }

    for (i, l) in local.iter().enumerate().filter(|(i, _)| !used[*i]) {
        let (key, title, author, format, warning) = match &l.metadata {
            Some(m) if duplicate[i] => (
                format!("local:{}", folder_label(&l.folder)),
                m.title.clone(),
                m.author.clone(),
                Format::from_mime(&m.format),
                Some("Another folder holds the same resource".to_owned()),
            ),
            Some(m) => (
                m.resource.clone(),
                m.title.clone(),
                m.author.clone(),
                Format::from_mime(&m.format),
                l.warning.clone(),
            ),
            None => {
                let name = folder_label(&l.folder);
                (
                    format!("local:{name}"),
                    name,
                    None,
                    Format::Unknown,
                    l.warning.clone(),
                )
            }
        };
        items.push(Item {
            key,
            title,
            author,
            format,
            state: ItemState::Local,
            acsm: None,
            folder: Some(l.folder.clone()),
            publication: l.publication.clone(),
            cover: l.cover.clone(),
            metadata: l.metadata.clone(),
            warning,
        });
    }

    items.sort_by(|a, b| {
        a.title
            .to_lowercase()
            .cmp(&b.title.to_lowercase())
            .then_with(|| a.key.cmp(&b.key))
    });
    items
}
