//! Syncing titles into the books folder, importing ACSMs, returning loans and
//! caching the cloud list.

use std::io::Read;
use std::path::{Path, PathBuf};

use super::{
    Format, Item, Library, LibraryError, Loan, Metadata, Source, acsm_summary, decrypt_publication,
    epub_loan_token, extract_cover, folder_name, full_id_suffix, id_suffix, now_rfc3339, src_title,
    write_atomic,
};
use crate::cloud::{self, CloudError, DownloadRecord, PRODUCTION_BASE};
use crate::fulfillment::{
    self, Acsm, AcsmError, ActivationToken, DeviceDescriptor, DeviceRole, MAX_ACSM_BYTES,
};
use crate::service::Transport;
use crate::store::ActivationState;

/// Device type sent with fulfillment requests.
const DEVICE_TYPE: &str = "standalone";

/// Result of syncing one title into the books folder.
#[derive(Clone, Debug)]
pub struct SyncOutcome {
    pub folder: PathBuf,
    pub metadata: Metadata,
    pub warning: Option<String>,
}

/// The target device descriptor sent with fulfillment requests.
pub fn target_descriptor(activation: &ActivationState) -> DeviceDescriptor {
    // Device identity from the stored activation: the persisted device id and
    // fingerprint, plus the device's activation token (user + device). A live
    // operator may still reject the fingerprint.
    DeviceDescriptor {
        role: DeviceRole::Target,
        software_version: None,
        client_os: Some(std::env::consts::OS.to_string()),
        client_locale: None,
        client_version: None,
        device_type: DEVICE_TYPE.into(),
        product_name: Some("leafport".into()),
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
        activation: &ActivationState,
        transport: &dyn Transport,
    ) -> Result<SyncOutcome, LibraryError> {
        self.sync_with_source(acsm, Source::Cloud, activation, transport)
    }

    /// Uploads an ACSM file to the cloud, then syncs it. A failed upload is a warning.
    pub fn import_acsm(
        &self,
        path: &Path,
        activation: &ActivationState,
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
        activation: &ActivationState,
        transport: &dyn Transport,
    ) -> Result<SyncOutcome, LibraryError> {
        let summary = acsm_summary(acsm_bytes)?;
        let acsm = Acsm::parse(String::from_utf8_lossy(acsm_bytes).into_owned())?;
        let fulfilled = fulfillment::perform_fulfillment(
            transport,
            &acsm,
            &activation.user,
            &activation.device,
            DEVICE_TYPE,
            std::slice::from_ref(&target_descriptor(activation)),
            &activation.keys.authentication,
        )?;
        let f = fulfilled.fulfillment;
        let body = fulfilled.body;

        let format = match Format::sniff(&body) {
            Format::Unknown => match f.download_type.as_deref().map(Format::from_mime) {
                Some(Format::Unknown) | None => summary.format,
                Some(format) => format,
            },
            format => format,
        };
        let title = f
            .title
            .clone()
            .or_else(|| summary.title.clone())
            .or_else(|| summary.src.as_deref().and_then(src_title))
            .unwrap_or_else(|| summary.key.clone());
        let stem = folder_name(&title);
        let folder = self.title_folder(&stem, &summary.key)?;

        let temp = self.root.join(temp_name());
        std::fs::create_dir(&temp)?;
        let write = || -> Result<Metadata, LibraryError> {
            write_atomic(&temp.join(format!("{stem}.acsm")), acsm_bytes)?;
            let publication = decrypt_publication(
                &body,
                format,
                f.license_token_xml.as_deref(),
                &activation.keys.license,
            )?;
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
                synced_at: now_rfc3339(),
                returned: false,
                loan,
            };
            metadata.write(&temp)?;
            replace_folder(&self.root, &temp, &folder)?;
            Ok(metadata)
        };
        match write() {
            Ok(metadata) => Ok(SyncOutcome {
                folder,
                metadata,
                warning: None,
            }),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&temp);
                Err(e)
            }
        }
    }
}

/// Moves `temp` to `folder`, swapping out an existing folder so a failure keeps it.
fn replace_folder(root: &Path, temp: &Path, folder: &Path) -> Result<(), LibraryError> {
    if !folder.exists() {
        std::fs::rename(temp, folder)?;
        return Ok(());
    }
    let name = folder
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Not a `.tmp-` name: `Library::open` restores this folder instead of sweeping it.
    let old = root.join(format!(".old-{:016x}-{name}", rand::random::<u64>()));
    std::fs::rename(folder, &old)?;
    if let Err(e) = std::fs::rename(temp, folder) {
        if let Err(restore) = std::fs::rename(&old, folder) {
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
    returned.write(folder)?;
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
