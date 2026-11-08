//! Syncing titles into the books folder, importing ACSMs, returning loans and
//! caching the cloud list.

use std::io::Read;
use std::path::{Path, PathBuf};

use super::{
    Format, Item, Library, LibraryError, Loan, Metadata, Source, acsm_summary, book_title,
    decrypt_publication, epub_loan_token, extract_cover, folder_name, full_id_suffix, id_suffix,
    now_rfc3339, src_title, write_atomic,
};
use crate::cloud::{self, CloudError, DownloadRecord, PRODUCTION_BASE};
use crate::fulfillment::{
    self, Acsm, AcsmError, ActivationToken, DeviceDescriptor, DeviceRole, MAX_ACSM_BYTES,
};
use crate::service::Transport;
use crate::store::ActivationState;

use crate::activation::{CLIENT_LOCALE, DEVICE_TYPE, PRODUCT_NAME};

/// Result of syncing one title into the books folder.
#[derive(Clone, Debug)]
pub struct SyncOutcome {
    pub folder: PathBuf,
    pub metadata: Metadata,
    pub warning: Option<String>,
}

/// The target device descriptor sent with fulfillment requests.
pub fn target_descriptor(activation: &ActivationState) -> DeviceDescriptor {
    // Use the same device profile that was registered by /Activate. In particular,
    // activation.device is the server-issued UUID, not the provisional local ID.
    DeviceDescriptor {
        role: DeviceRole::Target,
        software_version: Some(crate::VERSION.into()),
        client_os: Some(std::env::consts::OS.to_string()),
        client_locale: Some(CLIENT_LOCALE.into()),
        client_version: Some(crate::VERSION.into()),
        device_type: DEVICE_TYPE.into(),
        product_name: Some(PRODUCT_NAME.into()),
        device_name: None,
        fingerprint: activation.fingerprint.clone(),
        activations: vec![ActivationToken {
            user: activation.user.clone(),
            device: activation.device.clone(),
            until: None,
            auth: None,
            auth_distributor: None,
        }],
    }
}

fn temp_name() -> String {
    format!(".tmp-{:016x}", rand::random::<u64>())
}

impl Library {
    /// Fulfills `acsm`, decrypts the publication and writes its title folder.
    pub fn sync_item(
        &self,
        acsm: &[u8],
        activation: &mut ActivationState,
        transport: &dyn Transport,
    ) -> Result<SyncOutcome, LibraryError> {
        self.sync_with_source(acsm, Source::Cloud, activation, transport)
    }

    /// Uploads an ACSM file to the cloud, then syncs it. A failed upload is a warning.
    pub fn import_acsm(
        &self,
        path: &Path,
        activation: &mut ActivationState,
        transport: &dyn Transport,
    ) -> Result<SyncOutcome, LibraryError> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(MAX_ACSM_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_ACSM_BYTES {
            return Err(AcsmError::TooLarge.into());
        }
        Acsm::parse(String::from_utf8_lossy(&bytes).into_owned())?;
        let warning = cloud::sync_to_cloud(
            transport,
            PRODUCTION_BASE,
            &activation.user,
            &bytes,
            &activation.keys.authentication,
        )
        .err()
        .map(|e| format!("Upload to cloud failed: {e}"));
        let mut outcome = self.sync_with_source(&bytes, Source::Import, activation, transport)?;
        outcome.warning = warning;
        Ok(outcome)
    }

    /// Mark `item` excluded from Sync All, or include it again. Excluding a title without
    /// a folder writes one holding its ACSM and `metadata.yaml`; nothing is deleted.
    pub fn set_excluded(&self, item: &Item, excluded: bool) -> Result<(), LibraryError> {
        if let (Some(folder), Some(metadata)) = (&item.folder, &item.metadata) {
            let mut metadata = metadata.clone();
            metadata.excluded = excluded;
            return metadata.write(folder);
        }
        if !excluded {
            return Ok(());
        }
        let acsm = item
            .acsm
            .as_deref()
            .ok_or_else(|| std::io::Error::other("no ACSM for this title"))?;
        let stem = folder_name(&item.title);
        let folder = self.title_folder(&stem, &item.key)?;
        let metadata = Metadata {
            resource: item.key.clone(),
            title: item.title.clone(),
            author: item.author.clone(),
            format: item.format.mime().to_owned(),
            fulfillment: None,
            source: Source::Cloud,
            synced_at: None,
            returned: false,
            excluded: true,
            loan: None,
        };
        let write = |temp: &Path| -> Result<(), LibraryError> {
            write_atomic(&temp.join(format!("{stem}.acsm")), acsm)?;
            metadata.write(temp)
        };
        stage_folder(&self.root, &folder, write, &|from, to| {
            std::fs::rename(from, to)
        })
    }

    /// The folder for `key`: a scanned folder holding it, else `<stem> [<id>]`, else
    /// `<stem> [<full id>]` when the short name holds another resource.
    fn title_folder(&self, stem: &str, key: &str) -> Result<PathBuf, LibraryError> {
        let scanned = self
            .local
            .iter()
            .find(|l| l.metadata.as_ref().is_some_and(|m| m.resource == key));
        if let Some(local) = scanned {
            return Ok(local.folder.clone());
        }
        // Folders on disk are checked too, since `local` may predate an earlier sync.
        let holds_key = |folder: &Path| {
            !folder.exists() || Metadata::read(folder).is_ok_and(|m| m.resource == key)
        };
        for suffix in [id_suffix(key), full_id_suffix(key)] {
            let folder = self.root.join(format!("{stem}{suffix}"));
            if holds_key(&folder) {
                return Ok(folder);
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "folder {} belongs to another title",
                self.root
                    .join(format!("{stem}{}", full_id_suffix(key)))
                    .display()
            ),
        )
        .into())
    }

    fn sync_with_source(
        &self,
        acsm_bytes: &[u8],
        source: Source,
        activation: &mut ActivationState,
        transport: &dyn Transport,
    ) -> Result<SyncOutcome, LibraryError> {
        let summary = acsm_summary(acsm_bytes)?;
        let acsm = Acsm::parse(String::from_utf8_lossy(acsm_bytes).into_owned())?;
        fulfillment::prepare_operator(transport, activation, &acsm)?;
        let result = fulfillment::perform_fulfillment(
            transport,
            &acsm,
            &activation.user,
            &activation.device,
            DEVICE_TYPE,
            std::slice::from_ref(&target_descriptor(activation)),
            &activation.keys.authentication,
        );
        // This rejection means the operator needs reinitializing. Leafport does not
        // retry: it forgets the operator, so the next sync the user starts initializes
        // it again.
        let unknown_user = matches!(
            &result,
            Err(fulfillment::FulfillError::Response(
                fulfillment::FulfillResponseError::Service { xml }
            )) if cloud::adept_error_data(xml).is_some_and(|data| data.starts_with("E_LIC_USER_UNKNOWN"))
        );
        if unknown_user {
            activation.operator_urls.retain(|url| {
                url.trim_end_matches('/') != acsm.operator_url().trim_end_matches('/')
            });
        }
        let fulfilled = result?;
        let f = fulfilled.fulfillment;
        let body = fulfilled.body;

        let format = match Format::sniff(&body) {
            Format::Unknown => match f.download_type.as_deref().map(Format::from_mime) {
                Some(Format::Unknown) | None => summary.format,
                Some(format) => format,
            },
            format => format,
        };
        let publication = decrypt_publication(
            &body,
            format,
            f.license_token_xml.as_deref(),
            &activation.keys.license,
        )?;
        // The book's own title wins. An EPUB's OPF may be encrypted, so
        // read it decrypted. A PDF is read from the original download: a decrypted PDF
        // has lost the /Encrypt dictionary that can carry the title.
        let title_source = match format {
            Format::Pdf => &body,
            _ => &publication,
        };
        let title = book_title(title_source, format)
            .or_else(|| f.title.clone())
            .or_else(|| summary.title.clone())
            .or_else(|| summary.src.as_deref().and_then(src_title))
            .unwrap_or_else(|| summary.key.clone());
        let stem = folder_name(&title);
        let folder = self.title_folder(&stem, &summary.key)?;

        let write = |temp: &Path| -> Result<Metadata, LibraryError> {
            write_atomic(&temp.join(format!("{stem}.acsm")), acsm_bytes)?;
            let extension = format.extension().ok_or(LibraryError::UnsupportedFormat)?;
            write_atomic(&temp.join(format!("{stem}.{extension}")), &publication)?;
            if let Some(cover) = extract_cover(&publication, format) {
                write_atomic(
                    &temp.join(format!("cover.{}", cover.extension)),
                    &cover.bytes,
                )?;
            }
            let loan = f
                .loan_token_xml
                .clone()
                .or_else(|| epub_loan_token(&body))
                .map(|token| Loan {
                    operator_url: acsm.operator_url().to_owned(),
                    expires: f.loan_expires.clone(),
                    token,
                });
            let metadata = Metadata {
                resource: summary.key.clone(),
                title: title.clone(),
                author: f.author.clone().or_else(|| summary.author.clone()),
                format: format.mime().to_owned(),
                fulfillment: f.fulfillment_id.clone(),
                source,
                synced_at: Some(now_rfc3339()),
                returned: false,
                excluded: false,
                loan,
            };
            metadata.write(temp)?;
            Ok(metadata)
        };
        let metadata = stage_folder(&self.root, &folder, write, &|from, to| {
            std::fs::rename(from, to)
        })?;
        Ok(SyncOutcome {
            folder,
            metadata,
            warning: None,
        })
    }
}

type Rename<'a> = &'a dyn Fn(&Path, &Path) -> std::io::Result<()>;

/// Runs `write` in a fresh temp folder under `root`, then moves that folder to
/// `folder`. A failure at any point removes the temp folder and keeps a previous
/// `folder`. `rename` is a seam so tests can fail a move.
fn stage_folder<T>(
    root: &Path,
    folder: &Path,
    write: impl FnOnce(&Path) -> Result<T, LibraryError>,
    rename: Rename,
) -> Result<T, LibraryError> {
    let temp = root.join(temp_name());
    std::fs::create_dir(&temp)?;
    let result = write(&temp).and_then(|value| {
        replace_folder(root, &temp, folder, rename)?;
        Ok(value)
    });
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&temp);
    }
    result
}

/// Moves `temp` to `folder`, swapping out an existing folder so a failure keeps it.
fn replace_folder(
    root: &Path,
    temp: &Path,
    folder: &Path,
    rename: Rename,
) -> Result<(), LibraryError> {
    if !folder.exists() {
        rename(temp, folder)?;
        return Ok(());
    }
    let name = folder
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Not a `.tmp-` name: `Library::open` restores this folder instead of sweeping it.
    let old = root.join(format!(".old-{:016x}-{name}", rand::random::<u64>()));
    rename(folder, &old)?;
    if let Err(e) = rename(temp, folder) {
        if let Err(restore) = rename(&old, folder) {
            return Err(std::io::Error::new(
                e.kind(),
                format!(
                    "{e}; restoring failed ({restore}), previous copy kept at {}",
                    old.display()
                ),
            )
            .into());
        }
        return Err(e.into());
    }
    // Best effort: a leftover `.old-` folder is removed on the next open.
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

/// Returns a loaned item to its operator and marks it Returned, keeping the file.
pub fn return_loan(
    item: &Item,
    activation: &ActivationState,
    transport: &dyn Transport,
) -> Result<Metadata, LibraryError> {
    let metadata = item.metadata.as_ref().ok_or(LibraryError::NotOnLoan)?;
    let loan = metadata.loan.as_ref().ok_or(LibraryError::NotOnLoan)?;
    let folder = item.folder.as_ref().ok_or(LibraryError::NoFolder)?;
    fulfillment::perform_loan_return(
        transport,
        &loan.operator_url,
        &activation.user,
        &activation.device,
        &loan.token,
        &activation.keys.authentication,
    )?;
    let mut returned = metadata.clone();
    returned.loan = None;
    returned.returned = true;
    // The operator has the book back; returning again would only be refused.
    returned.write(folder).map_err(|e| {
        std::io::Error::other(format!(
            "the loan was returned, but {} could not be updated: {e}",
            folder.join(super::METADATA_FILE).display()
        ))
    })?;
    Ok(returned)
}

/// Fetches the cloud list and stores the raw response at `cache_path`.
/// On error the cache is untouched.
pub fn refresh(
    cache_path: &Path,
    activation: &ActivationState,
    transport: &dyn Transport,
) -> Result<Vec<DownloadRecord>, LibraryError> {
    let response = cloud::sync_to_device(
        transport,
        PRODUCTION_BASE,
        &activation.user,
        &activation.keys.authentication,
    )?;
    if let Some(parent) = cache_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_atomic(cache_path, response.xml.as_bytes())?;
    Ok(response.records)
}

/// Reads the cached cloud list. A missing cache is empty; an unparseable one is an error.
pub fn load_cache(cache_path: &Path) -> Result<Vec<DownloadRecord>, LibraryError> {
    let xml = match std::fs::read_to_string(cache_path) {
        Ok(xml) => xml,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    cloud::download_records(&xml).map_err(|e| CloudError::Response(e).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    fn root() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("leafport-stage-{:016x}", rand::random::<u64>()));
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    fn names(root: &Path) -> Vec<String> {
        let mut out: Vec<_> = std::fs::read_dir(root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }

    fn put(temp: &Path) -> Result<(), LibraryError> {
        std::fs::write(temp.join("book.epub"), b"new")?;
        Ok(())
    }

    #[test]
    fn write_failure_after_temp_exists_leaves_nothing() {
        let root = root();
        let folder = root.join("Title [a]");
        let result = stage_folder(
            &root,
            &folder,
            |temp| {
                put(temp)?;
                assert_eq!(names(&root).len(), 1, "temp folder exists");
                Err::<(), _>(LibraryError::Drm("boom".into()))
            },
            &|from, to| std::fs::rename(from, to),
        );
        assert!(matches!(result, Err(LibraryError::Drm(_))));
        assert!(names(&root).is_empty(), "{:?}", names(&root));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn failed_final_rename_without_previous_folder_leaves_nothing() {
        let root = root();
        let folder = root.join("Title [a]");
        let result = stage_folder(&root, &folder, put, &|_, _| {
            Err(io::Error::other("rename failed"))
        });
        assert!(result.is_err());
        assert!(names(&root).is_empty(), "{:?}", names(&root));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn failed_final_rename_restores_previous_folder() {
        let root = root();
        let folder = root.join("Title [a]");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("book.epub"), b"old").unwrap();
        // Fail only the move of the new folder into place.
        let result = stage_folder(&root, &folder, put, &|from, to| {
            if to == folder
                && from
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with(".tmp-"))
            {
                Err(io::Error::other("rename failed"))
            } else {
                std::fs::rename(from, to)
            }
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(folder.join("book.epub")).unwrap(), b"old");
        assert_eq!(names(&root), vec!["Title [a]".to_owned()]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn successful_swap_replaces_folder_and_removes_old() {
        let root = root();
        let folder = root.join("Title [a]");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("book.epub"), b"old").unwrap();
        stage_folder(&root, &folder, put, &|from, to| std::fs::rename(from, to)).unwrap();
        assert_eq!(std::fs::read(folder.join("book.epub")).unwrap(), b"new");
        assert_eq!(names(&root), vec!["Title [a]".to_owned()]);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
