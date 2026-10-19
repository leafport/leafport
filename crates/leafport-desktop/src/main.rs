#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod epub;
mod pdf;
use leafport_core::{
    activation::{self, ActivationKeys},
    cloud::{self, CloudClient},
    fulfillment::{self, Acsm, DeviceDescriptor, DeviceRole},
    service::ServiceClient,
    store::{ActivationState, CredentialStore, DEFAULT_ACCOUNT, KeyringStore},
    vendors::VendorCatalog,
};
use eframe::egui;
use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
};

/// Load the signed-in account label from the OS credential store, if present.
/// A backend failure is surfaced to the UI rather than hidden.
fn load_signed_in() -> Result<Option<String>, String> {
    ActivationState::load(&KeyringStore::new(), DEFAULT_ACCOUNT)
        .map(|state| state.map(|state| state.user))
        .map_err(|error| error.to_string())
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct SavedLibrary {
    paths: Vec<PathBuf>,
    /// Last-read chapter index per book path, restored on import.
    #[serde(default)]
    positions: std::collections::HashMap<PathBuf, usize>,
    /// Bookmarked chapter indices per book path.
    #[serde(default)]
    bookmarks: std::collections::HashMap<PathBuf, Vec<usize>>,
    /// Freeform note per (book path, chapter index).
    #[serde(default)]
    notes: std::collections::HashMap<String, String>,
}
enum Event {
    Vendors(Result<VendorCatalog, String>),
    Book(Result<epub::Book, String>),
    /// A completed sign-in: the activated account label, or an error message.
    SignIn(Result<String, String>),
    /// A completed cloud sync: the number of ACSM records retrieved, or an error.
    Sync(Result<usize, String>),
    /// A completed cloud upload (SyncToCloud): unit on success, or an error message.
    Upload(Result<(), String>),
    /// A completed fulfillment: the saved book path and an optional sidecar warning, or an
    /// error message.
    Fulfilled(Result<(PathBuf, Option<String>), String>),
    /// A completed loan return: the returned book path, or an error message.
    LoanReturned(Result<PathBuf, String>),
    /// A completed full-layout extraction: (book path, extraction dir, section href), or
    /// (book path, error message).
    LayoutReady(Result<(PathBuf, PathBuf, String), (PathBuf, String)>),
}

/// Run a worker body, converting a panic into an Err so the completion event is always sent.
fn catch_panic<T>(body: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).unwrap_or_else(|payload| {
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".into());
        Err(format!("internal error: {message}"))
    })
}

/// Remove a sidecar file; returns a warning if removal failed for a reason other than absence.
fn remove_sidecar(path: &std::path::Path) -> Option<String> {
    match std::fs::remove_file(path) {
        Ok(()) => None,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => Some(format!("could not remove stale {}: {e}", path.display())),
    }
}

/// Remove `leafport-layout-<pid>-*` temp directories left by dead processes (Linux only).
#[cfg(target_os = "linux")]
fn sweep_stale_layouts() {
    use std::os::unix::fs::MetadataExt;
    let Ok(uid) = std::fs::metadata("/proc/self").map(|m| m.uid()) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(rest) = name
            .to_str()
            .and_then(|n| n.strip_prefix("leafport-layout-"))
        else {
            continue;
        };
        let Some(pid) = rest.split('-').next().and_then(|p| p.parse::<u32>().ok()) else {
            continue;
        };
        if pid == std::process::id() || std::path::Path::new(&format!("/proc/{pid}")).exists() {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if meta.is_dir() && meta.uid() == uid {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn sweep_stale_layouts() {}

/// Directory where fulfilled publications are written, under the platform data dir.
fn books_dir() -> Result<PathBuf, String> {
    let base = dirs_next_data()
        .ok_or_else(|| "no platform data directory".to_string())?
        .join("leafport")
        .join("books");
    std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    Ok(base)
}

/// Platform application-data directory without extra dependencies.
fn dirs_next_data() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA").map(PathBuf::from)
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
        })
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            })
    }
}

/// Loan metadata saved beside a fulfilled book so it can later be returned. Present only
/// for borrowed titles (those whose rights carry a loanToken).
#[derive(serde::Serialize, serde::Deserialize)]
struct LoanInfo {
    operator_url: String,
    loan_token_xml: String,
}

/// Path of the loan sidecar for a given book file.
fn loan_sidecar_path(book: &std::path::Path) -> PathBuf {
    book.with_extension("leafport-loan.json")
}

/// Path of the license sidecar (fulfillment licenseToken) for a given book file. Used to
/// decrypt Adobe-DRM PDFs, whose content key is wrapped inside the licenseToken.
fn license_sidecar_path(book: &std::path::Path) -> PathBuf {
    book.with_extension("leafport-license.xml")
}

/// Extract the licenseToken subtree from an EPUB's META-INF/rights.xml, if it is a loan.
/// Returns None for a non-loan book or when rights.xml is absent or has no loanToken.
fn loan_token_from_epub(bytes: &[u8]) -> Option<String> {
    use std::io::Read;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    let mut rights = String::new();
    zip.by_name("META-INF/rights.xml")
        .ok()?
        .take(4 * 1024 * 1024)
        .read_to_string(&mut rights)
        .ok()?;
    // A loan is marked by a loanToken element in the rights; retain it verbatim.
    let document = roxmltree::Document::parse_with_options(
        &rights,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 50_000,
            entity_resolver: None,
        },
    )
    .ok()?;
    let node = document
        .descendants()
        .find(|node| node.has_tag_name(("http://ns.adobe.com/adept", "loanToken")))?;
    Some(fulfillment::standalone_element(&rights, node))
}

/// Reduce an identifier to a safe filename stem (alphanumeric, dash, underscore).
fn sanitize_stem(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(80)
        .collect()
}

/// Write bytes to `path` atomically: full write to a temp file, then rename.
fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("part");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}
struct Desktop {
    catalog: VendorCatalog,
    vendor: usize,
    discovering: bool,
    provider_status: String,
    books: Vec<epub::Book>,
    selected: Option<usize>,
    chapter: usize,
    font_size: f32,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    importing: usize,
    status: String,
    saved: SavedLibrary,
    show_providers: bool,
    username: String,
    password: String,
    signing: bool,
    /// The activated account label from the credential store, when signed in.
    signed_in: Option<String>,
    sign_in_status: String,
    syncing: bool,
    sync_status: String,
    fulfilling: bool,
    /// Editable note buffer for the current (book, chapter), synced to saved.notes.
    note_buffer: String,
    /// The notes key the buffer currently holds, or None when nothing is selected.
    note_key: Option<String>,
    /// Decoded image textures, keyed by "<book path>#<image index>".
    textures: std::collections::HashMap<String, egui::TextureHandle>,
    /// Per-book extraction directory for the full-layout (webview) view.
    extract_dirs: std::collections::HashMap<PathBuf, PathBuf>,
    /// Private per-run directory holding extracted books, removed on exit.
    layout_root: Option<PathBuf>,
    /// Books whose full-layout extraction is in flight.
    pending_layouts: std::collections::HashSet<PathBuf>,
}

/// Locate the leafport-reader webview helper binary: next to this executable, or in the same
/// target profile directory during development.
fn reader_helper() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let name = if cfg!(windows) {
        "leafport-reader.exe"
    } else {
        "leafport-reader"
    };
    let candidate = dir.join(name);
    candidate.exists().then_some(candidate)
}

/// Texture cache key for one book image.
fn texture_key(path: &std::path::Path, image_index: usize) -> String {
    format!("{}#{image_index}", path.display())
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

/// Stable key for a per-chapter note.
fn note_key(path: &std::path::Path, chapter: usize) -> String {
    format!("{}\u{0}{chapter}", path.display())
}
impl Desktop {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::light());
        let (tx, rx) = mpsc::channel();
        let saved = cc
            .storage
            .and_then(|s| eframe::get_value(s, "library"))
            .unwrap_or_default();
        let mut app = Self {
            catalog: VendorCatalog::new(),
            vendor: 0,
            discovering: false,
            provider_status: String::new(),
            books: vec![],
            selected: None,
            chapter: 0,
            font_size: 19.0,
            tx,
            rx,
            importing: 0,
            status: "Import an EPUB to begin reading.".into(),
            saved,
            show_providers: false,
            username: String::new(),
            password: String::new(),
            signing: false,
            signed_in: None,
            sign_in_status: String::new(),
            syncing: false,
            sync_status: String::new(),
            fulfilling: false,
            note_buffer: String::new(),
            note_key: None,
            textures: std::collections::HashMap::new(),
            extract_dirs: std::collections::HashMap::new(),
            layout_root: None,
            pending_layouts: std::collections::HashSet::new(),
        };
        sweep_stale_layouts();
        match load_signed_in() {
            Ok(Some(user)) => {
                app.sign_in_status = format!("Signed in as {user}.");
                app.signed_in = Some(user);
            }
            Ok(None) => app.sign_in_status = "Not signed in.".into(),
            Err(error) => app.sign_in_status = format!("Credential store unavailable: {error}"),
        }
        app.discover(&cc.egui_ctx);
        for path in app.saved.paths.clone() {
            app.import(path, &cc.egui_ctx);
        }
        for path in std::env::args_os().skip(1) {
            app.import(path.into(), &cc.egui_ctx);
        }
        app
    }
    fn discover(&mut self, ctx: &egui::Context) {
        if self.discovering {
            return;
        }
        self.discovering = true;
        self.provider_status = "Loading sign-in providers…".into();
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = catch_panic(|| {
                let mut catalog = VendorCatalog::new();
                ServiceClient::new()
                    .and_then(|client| client.load_vendors(&mut catalog))
                    .map_err(|e| e.to_string())?;
                Ok(catalog)
            });
            let _ = tx.send(Event::Vendors(result));
            ctx.request_repaint();
        });
    }
    /// Start a SignInDirect exchange on a worker thread. Credentials are moved into the
    /// thread, encrypted by the core, and cleared from the UI immediately. On success the
    /// resulting activation state is written to the OS credential store.
    fn sign_in(&mut self, ctx: &egui::Context) {
        if self.signing {
            return;
        }
        let info = self.catalog.service_info();
        let (auth_url, certificate) = match (info.auth_url.clone(), info.certificate.clone()) {
            (Some(auth_url), Some(certificate)) if !certificate.is_empty() => {
                (auth_url, certificate)
            }
            _ => {
                self.sign_in_status =
                    "Discovery has no authentication URL or certificate yet.".into();
                return;
            }
        };
        if self.username.is_empty() || self.password.is_empty() {
            self.sign_in_status = "Enter a username and password.".into();
            return;
        }
        let method = self.catalog.entries()[self.vendor].method.clone();
        // Move the credentials out of the UI so they are not retained after submission.
        let username = std::mem::take(&mut self.username);
        let password = std::mem::take(&mut self.password);
        self.signing = true;
        self.sign_in_status = "Signing in…".into();
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = catch_panic(|| {
                let client = ServiceClient::new().map_err(|e| e.to_string())?;
                // Device key and activation keys are generated locally for this device.
                // The 16-byte device key is random and persisted with the activation state.
                // Reuse an existing activation so books fulfilled under it stay readable.
                let (device_key, keys, device, fingerprint) =
                    match ActivationState::load(&KeyringStore::new(), DEFAULT_ACCOUNT) {
                        Ok(Some(existing)) => (
                            existing.device_key,
                            existing.keys,
                            existing.device,
                            existing.fingerprint,
                        ),
                        Ok(None) => {
                            let device_key = activation::generate_device_key();
                            let keys = ActivationKeys::generate().map_err(|e| e.to_string())?;
                            let device = activation::device_id(&device_key);
                            let fingerprint = machine_fingerprint(&device_key);
                            (device_key, keys, device, fingerprint)
                        }
                        Err(e) => {
                            return Err(format!(
                                "credential store unreadable: {e} — not replacing existing activation"
                            ));
                        }
                    };
                let signed = activation::perform_sign_in(
                    &client,
                    &auth_url,
                    &method,
                    &username,
                    &password,
                    &certificate,
                    &device_key,
                    &keys,
                )
                .map_err(|e| e.to_string())?;
                let state = ActivationState {
                    device,
                    fingerprint,
                    device_key,
                    keys,
                    user: signed.user.clone(),
                    credentials_xml: signed.credentials_xml,
                };
                state
                    .save(&KeyringStore::new(), DEFAULT_ACCOUNT)
                    .map_err(|e| e.to_string())?;
                Ok(signed.user)
            });
            // password/username were moved in and dropped here; nothing is returned.
            let _ = tx.send(Event::SignIn(result));
            ctx.request_repaint();
        });
    }
    /// Remove the stored activation state. Original book files are untouched.
    fn sign_out(&mut self) {
        match KeyringStore::new().delete(DEFAULT_ACCOUNT) {
            Ok(()) => {
                self.signed_in = None;
                self.sign_in_status =
                    "Signed out. Activation removed from credential store.".into();
            }
            Err(error) => self.sign_in_status = format!("Sign-out failed: {error}"),
        }
    }
    /// Pull the account's cloud locker (SyncToDevice) on a worker thread, using the
    /// stored activation. This retrieves ACSM records; it does not advance a sync cursor
    /// or mutate the library.
    fn sync_cloud(&mut self, ctx: &egui::Context) {
        if self.syncing {
            return;
        }
        self.syncing = true;
        self.sync_status = "Syncing from cloud…".into();
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = catch_panic(|| {
                let state = ActivationState::load(&KeyringStore::new(), DEFAULT_ACCOUNT)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "not signed in".to_string())?;
                let client = CloudClient::new(cloud::PRODUCTION_BASE).map_err(|e| e.to_string())?;
                // ADEPT requests are signed with the device's private license key.
                let records = client
                    .download(&state.user, None, None, &state.keys.license)
                    .map_err(|e| e.to_string())?;
                Ok(records.len())
            });
            let _ = tx.send(Event::Sync(result));
            ctx.request_repaint();
        });
    }
    /// Upload a local ACSM to the account's cloud locker (SyncToCloud) on a worker thread.
    fn upload_acsm(&mut self, acsm_path: PathBuf, ctx: &egui::Context) {
        if self.syncing {
            return;
        }
        self.syncing = true;
        self.sync_status = format!("Uploading {}…", acsm_path.display());
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = catch_panic(|| {
                let state = ActivationState::load(&KeyringStore::new(), DEFAULT_ACCOUNT)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "not signed in".to_string())?;
                let bytes = std::fs::read(&acsm_path).map_err(|e| e.to_string())?;
                let client = CloudClient::new(cloud::PRODUCTION_BASE).map_err(|e| e.to_string())?;
                client
                    .upload(&state.user, &bytes, &state.keys.license)
                    .map_err(|e| e.to_string())
            });
            let _ = tx.send(Event::Upload(result));
            ctx.request_repaint();
        });
    }
    /// Fulfill a local ACSM on a worker thread: build and POST the signed fulfill
    /// request with the stored activation, download the publication, and write it to the
    /// books directory. The resulting file is imported into the library on success.
    fn fulfill(&mut self, acsm_path: PathBuf, ctx: &egui::Context) {
        if self.fulfilling {
            return;
        }
        self.fulfilling = true;
        self.status = format!("Fulfilling {}…", acsm_path.display());
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = catch_panic(|| {
                let state = ActivationState::load(&KeyringStore::new(), DEFAULT_ACCOUNT)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "not signed in".to_string())?;
                let mut xml = String::new();
                use std::io::Read;
                std::fs::File::open(&acsm_path)
                    .map_err(|e| e.to_string())?
                    .take(fulfillment::MAX_ACSM_BYTES as u64 + 1)
                    .read_to_string(&mut xml)
                    .map_err(|e| e.to_string())?;
                let acsm = Acsm::parse(xml).map_err(|e| e.to_string())?;
                let client = ServiceClient::new().map_err(|e| e.to_string())?;
                // Device identity from the stored activation: the persisted device id and
                // fingerprint, plus the device's activation token (user + device). A live
                // operator may still reject the fingerprint. Signing uses the
                // private license key, as elsewhere.
                let descriptor = DeviceDescriptor {
                    role: DeviceRole::Target,
                    software_version: None,
                    client_os: Some(std::env::consts::OS.to_string()),
                    client_locale: None,
                    client_version: None,
                    device_type: "standalone".into(),
                    product_name: Some("leafport".into()),
                    device_name: None,
                    fingerprint: state.fingerprint.clone(),
                    activations: vec![fulfillment::ActivationToken {
                        user: state.user.clone(),
                        device: state.device.clone(),
                        until: None,
                        auth: None,
                        auth_distributor: None,
                    }],
                };
                let fulfilled = fulfillment::perform_fulfillment(
                    &client,
                    &acsm,
                    &state.user,
                    &state.device,
                    "standalone",
                    std::slice::from_ref(&descriptor),
                    &state.keys.license,
                )
                .map_err(|e| e.to_string())?;
                // Name the file by resource id when present, else by ACSM stem.
                let stem = fulfilled
                    .fulfillment
                    .resource
                    .as_deref()
                    .map(sanitize_stem)
                    .filter(|s| !s.is_empty())
                    .or_else(|| {
                        acsm_path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .map(sanitize_stem)
                    })
                    .unwrap_or_else(|| "book".into());
                let extension = match fulfilled.fulfillment.download_type.as_deref() {
                    Some(t) if t.contains("pdf") => "pdf",
                    _ => "epub",
                };
                let path = books_dir()
                    .map_err(|e| e)?
                    .join(format!("{stem}.{extension}"));
                write_atomic(&path, &fulfilled.body).map_err(|e| e.to_string())?;
                // Save the licenseToken beside a PDF so its EBX DRM can be decrypted on
                // open (EPUBs carry their rights inside the archive and need no sidecar).
                let mut warnings: Vec<String> = Vec::new();
                let license_sidecar = license_sidecar_path(&path);
                match fulfilled.fulfillment.license_token_xml.as_deref() {
                    Some(token) if extension == "pdf" => {
                        if let Err(e) = write_atomic(&license_sidecar, token.as_bytes()) {
                            warnings.push(format!("could not save license sidecar: {e}"));
                        }
                    }
                    _ => warnings.extend(remove_sidecar(&license_sidecar)),
                }
                // If this is a borrowed title, save a loan sidecar so it can be returned.
                let loan_sidecar = loan_sidecar_path(&path);
                if let Some(loan_token_xml) = fulfilled
                    .fulfillment
                    .loan_token_xml
                    .clone()
                    .or_else(|| loan_token_from_epub(&fulfilled.body))
                {
                    let info = LoanInfo {
                        operator_url: acsm.operator_url().to_owned(),
                        loan_token_xml,
                    };
                    match serde_json::to_vec_pretty(&info) {
                        Ok(json) => {
                            if let Err(e) = write_atomic(&loan_sidecar, &json) {
                                warnings.push(format!("could not save loan sidecar: {e}"));
                            }
                        }
                        Err(e) => warnings.push(format!("could not encode loan sidecar: {e}")),
                    }
                } else {
                    warnings.extend(remove_sidecar(&loan_sidecar));
                }
                let warning = (!warnings.is_empty()).then(|| warnings.join("; "));
                Ok((path, warning))
            });
            let _ = tx.send(Event::Fulfilled(result));
            ctx.request_repaint();
        });
    }
    /// Return a borrowed book to its operator on a worker thread, then delete the local
    /// file and its loan sidecar. Requires the loan sidecar written at fulfillment time.
    fn return_loan(&mut self, book_path: PathBuf, ctx: &egui::Context) {
        if self.fulfilling {
            return;
        }
        self.fulfilling = true;
        self.status = format!("Returning {}…", book_path.display());
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = catch_panic(|| {
                let state = ActivationState::load(&KeyringStore::new(), DEFAULT_ACCOUNT)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "not signed in".to_string())?;
                let sidecar = loan_sidecar_path(&book_path);
                let info: LoanInfo =
                    serde_json::from_slice(&std::fs::read(&sidecar).map_err(|e| e.to_string())?)
                        .map_err(|e| e.to_string())?;
                let client = ServiceClient::new().map_err(|e| e.to_string())?;
                fulfillment::perform_loan_return(
                    &client,
                    &info.operator_url,
                    &state.user,
                    &state.device,
                    &info.loan_token_xml,
                    &state.keys.license,
                )
                .map_err(|e| e.to_string())?;
                // The loan is returned; remove the local copy and its sidecars.
                let _ = std::fs::remove_file(&book_path);
                let _ = std::fs::remove_file(&sidecar);
                let _ = std::fs::remove_file(license_sidecar_path(&book_path));
                Ok(book_path)
            });
            let _ = tx.send(Event::LoanReturned(result));
            ctx.request_repaint();
        });
    }
    /// Open the current EPUB section in a webview window for full HTML/CSS layout. The
    /// book's resources are extracted (decrypted) to a per-book temp directory once, then
    /// the leafport-reader helper renders the section file with its real styling.
    fn open_full_layout(&mut self, ctx: &egui::Context) {
        let Some(index) = self.selected else {
            return;
        };
        let book_path = self.books[index].path.clone();
        let Some(href) = self.books[index]
            .chapters
            .get(self.chapter)
            .map(|c| c.href.clone())
        else {
            return;
        };
        if href.is_empty() {
            self.status = "Full layout is available for EPUB sections only.".into();
            return;
        }
        if reader_helper().is_none() {
            self.status =
                "Full-layout viewer (leafport-reader) is not installed next to the app.".into();
            return;
        }
        // Already extracted: launch directly.
        if let Some(dir) = self.extract_dirs.get(&book_path).cloned() {
            self.launch_layout(&dir, &href);
            return;
        }
        let root = match self.layout_root() {
            Ok(root) => root,
            Err(error) => {
                self.status = format!("Full layout unavailable: {error}");
                return;
            }
        };
        if !self.pending_layouts.insert(book_path.clone()) {
            self.status = "Already preparing the full-layout view for this book.".into();
            return;
        }
        // Extract the book once (decrypting with the stored license key if protected) on a
        // worker thread, since the credential store may block on an unlock prompt.
        self.status = "Preparing the full-layout view…".into();
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let error_path = book_path.clone();
            let result = catch_panic(|| {
                let license_key = ActivationState::load(&KeyringStore::new(), DEFAULT_ACCOUNT)
                    .ok()
                    .flatten()
                    .map(|state| state.keys.license);
                use sha1::{Digest, Sha1};
                let digest = Sha1::digest(book_path.as_os_str().as_encoded_bytes());
                let name: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
                let dir = root.join(name);
                std::fs::create_dir_all(&dir)
                    .map_err(|e| e.to_string())
                    .and_then(|_| epub::extract_to_dir(&book_path, license_key.as_ref(), &dir))
                    .map(|_| (book_path, dir, href))
            })
            .map_err(|e| (error_path, e));
            let _ = tx.send(Event::LayoutReady(result));
            ctx.request_repaint();
        });
    }
    /// The private per-run directory holding extracted books, created on first use with
    /// owner-only permissions. Creation fails rather than reusing an existing path.
    fn layout_root(&mut self) -> Result<PathBuf, String> {
        if let Some(root) = &self.layout_root {
            return Ok(root.clone());
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let root = std::env::temp_dir().join(format!(
            "leafport-layout-{}-{nanos:x}",
            std::process::id()
        ));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&root).map_err(|e| e.to_string())?;
        self.layout_root = Some(root.clone());
        Ok(root)
    }
    /// Launch the leafport-reader helper on one extracted section file.
    fn launch_layout(&mut self, dir: &std::path::Path, href: &str) {
        let Some(helper) = reader_helper() else {
            self.status =
                "Full-layout viewer (leafport-reader) is not installed next to the app.".into();
            return;
        };
        match std::process::Command::new(&helper)
            .arg(dir.join(href))
            .spawn()
        {
            Ok(_) => self.status = "Opened the section in the full-layout viewer.".into(),
            Err(error) => self.status = format!("Could not launch the full-layout viewer: {error}"),
        }
    }
    /// Drop a book's decrypted extraction directory and cached textures.
    fn discard_extracted(&mut self, path: &std::path::Path) {
        if let Some(dir) = self.extract_dirs.remove(path) {
            let _ = std::fs::remove_dir_all(dir);
        }
        let prefix = texture_key(path, 0);
        let prefix = &prefix[..prefix.len() - 1];
        self.textures.retain(|key, _| !key.starts_with(prefix));
    }
    fn import(&mut self, path: PathBuf, ctx: &egui::Context) {
        let path = path.canonicalize().unwrap_or(path);
        self.importing += 1;
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = catch_panic(|| {
                // Load the device license key so ADEPT-protected books can be decrypted.
                // Absent or signed-out: protected books stay blocked, plain EPUBs still open.
                let license_key = ActivationState::load(&KeyringStore::new(), DEFAULT_ACCOUNT)
                    .ok()
                    .flatten()
                    .map(|state| state.keys.license);
                let is_pdf = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("pdf"));
                let opened = if is_pdf {
                    // Load the licenseToken sidecar (if present) to decrypt an EBX-DRM PDF.
                    let token = std::fs::read_to_string(license_sidecar_path(&path)).ok();
                    pdf::open(&path, license_key.as_ref(), token.as_deref())
                } else {
                    epub::open(&path, license_key.as_ref())
                };
                opened.map_err(|e| format!("{}: {e}", path.display()))
            });
            let _ = tx.send(Event::Book(result));
            ctx.request_repaint();
        });
    }
}
impl eframe::App for Desktop {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "library", &self.saved);
    }
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(root) = self.layout_root.take() {
            let _ = std::fs::remove_dir_all(root);
        }
    }
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Vendors(result) => {
                    self.discovering = false;
                    match result {
                        Ok(catalog) => {
                            self.provider_status =
                                format!("{} sign-in choices loaded", catalog.entries().len());
                            self.catalog = catalog;
                            self.vendor = 0;
                        }
                        Err(e) => {
                            self.provider_status =
                                format!("Discovery failed: {e}. Existing choices retained.")
                        }
                    }
                }
                Event::SignIn(result) => {
                    self.signing = false;
                    match result {
                        Ok(user) => {
                            self.sign_in_status = format!("Signed in as {user}.");
                            self.signed_in = Some(user);
                        }
                        Err(error) => self.sign_in_status = format!("Sign-in failed: {error}"),
                    }
                }
                Event::Sync(result) => {
                    self.syncing = false;
                    self.sync_status = match result {
                        Ok(count) => format!("Cloud sync retrieved {count} ACSM record(s)."),
                        Err(error) => format!("Cloud sync failed: {error}"),
                    };
                }
                Event::Upload(result) => {
                    self.syncing = false;
                    self.sync_status = match result {
                        Ok(()) => "Uploaded ACSM to the cloud locker.".into(),
                        Err(error) => format!("Cloud upload failed: {error}"),
                    };
                }
                Event::Fulfilled(result) => {
                    self.fulfilling = false;
                    match result {
                        Ok((path, warning)) => {
                            self.status = format!("Fulfilled and saved {}.", path.display());
                            if let Some(warning) = warning {
                                self.status.push_str(&format!(" Warning: {warning}."));
                            }
                            self.import(path, ctx);
                        }
                        Err(error) => self.status = format!("Fulfillment failed: {error}"),
                    }
                }
                Event::LoanReturned(result) => {
                    self.fulfilling = false;
                    match result {
                        Ok(path) => {
                            self.note_key = None;
                            self.note_buffer.clear();
                            self.discard_extracted(&path);
                            if let Some(index) = self.books.iter().position(|b| b.path == path) {
                                self.books.remove(index);
                            }
                            self.saved.paths.retain(|p| *p != path);
                            self.saved.positions.remove(&path);
                            self.saved.bookmarks.remove(&path);
                            let prefix = note_key(&path, 0);
                            let prefix = &prefix[..prefix.len() - 1];
                            self.saved.notes.retain(|key, _| !key.starts_with(prefix));
                            self.selected = None;
                            self.chapter = 0;
                            self.status = "Loan returned. Local copy removed.".into();
                        }
                        Err(error) => self.status = format!("Loan return failed: {error}"),
                    }
                }
                Event::LayoutReady(result) => match result {
                    Ok((book_path, dir, href)) => {
                        self.pending_layouts.remove(&book_path);
                        if self.books.iter().any(|b| b.path == book_path) {
                            self.extract_dirs.insert(book_path, dir.clone());
                            self.launch_layout(&dir, &href);
                        } else {
                            // The book was removed while extracting; drop the copy.
                            let _ = std::fs::remove_dir_all(dir);
                        }
                    }
                    Err((book_path, error)) => {
                        self.pending_layouts.remove(&book_path);
                        self.status = format!("Full layout unavailable: {error}");
                    }
                },
                Event::Book(result) => {
                    self.importing = self.importing.saturating_sub(1);
                    match result {
                        Ok(book) => {
                            // Restore the last-read chapter, clamped to this book's length.
                            let saved_chapter = self
                                .saved
                                .positions
                                .get(&book.path)
                                .copied()
                                .unwrap_or(0)
                                .min(book.chapters.len().saturating_sub(1));
                            if let Some(index) = self.books.iter().position(|b| b.path == book.path)
                            {
                                // Replace the stale entry so re-fulfilled or newly
                                // unblocked books show their current content.
                                let path = book.path.clone();
                                self.discard_extracted(&path);
                                self.books[index] = book;
                                self.selected = Some(index);
                            } else {
                                if !self.saved.paths.contains(&book.path) {
                                    self.saved.paths.push(book.path.clone());
                                }
                                self.selected = Some(self.books.len());
                                self.books.push(book);
                            }
                            self.chapter = saved_chapter;
                            self.status =
                                "EPUB opened. Library stores links to original files.".into();
                        }
                        Err(e) => self.status = format!("Cannot open book: {e}"),
                    }
                }
            }
        }
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        for file in dropped {
            if let Some(path) = file.path {
                self.import(path, ctx);
            }
        }
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Leafport");
                ui.separator();
                if ui.button("Import book…").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("Books", &["epub", "pdf"])
                        .add_filter("EPUB books", &["epub"])
                        .add_filter("PDF books", &["pdf"])
                        .pick_file()
                    {
                        self.import(path, ctx);
                    }
                }
                if ui
                    .add_enabled(
                        !self.fulfilling && self.signed_in.is_some(),
                        egui::Button::new("Fulfill ACSM…"),
                    )
                    .on_hover_text(if self.signed_in.is_some() {
                        "Download a purchased or borrowed book from an .acsm file"
                    } else {
                        "Sign in (Providers) to fulfill an ACSM"
                    })
                    .clicked()
                {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("ACSM files", &["acsm"])
                        .pick_file()
                    {
                        self.fulfill(path, ctx);
                    }
                }
                if self.fulfilling {
                    ui.spinner();
                }
                ui.toggle_value(&mut self.show_providers, "Providers");
                if self.importing > 0 {
                    ui.spinner();
                    ui.label(format!("Opening {} file(s)", self.importing));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(egui::Slider::new(&mut self.font_size, 12.0..=32.0).text("Text size"));
                });
            });
        });
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.label(&self.status);
        });
        egui::SidePanel::left("library")
            .default_width(245.0)
            .show(ctx, |ui| {
                ui.heading("Library");
                ui.label(format!("{} books", self.books.len()));
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt("books")
                    .max_height(220.0)
                    .show(ui, |ui| {
                        for (i, book) in self.books.iter().enumerate() {
                            if ui
                                .selectable_label(self.selected == Some(i), &book.title)
                                .on_hover_text(book.path.display().to_string())
                                .clicked()
                            {
                                self.selected = Some(i);
                                // Restore the saved chapter for the book being opened.
                                let path = self.books[i].path.clone();
                                let chapters = self.books[i].chapters.len();
                                self.chapter = self
                                    .saved
                                    .positions
                                    .get(&path)
                                    .copied()
                                    .unwrap_or(0)
                                    .min(chapters.saturating_sub(1));
                            }
                        }
                    });
                if let Some(index) = self.selected {
                    if ui.small_button("Remove from library").clicked() {
                        self.note_key = None;
                        self.note_buffer.clear();
                        let book = self.books.remove(index);
                        self.discard_extracted(&book.path);
                        self.saved.paths.retain(|p| *p != book.path);
                        self.saved.positions.remove(&book.path);
                        self.saved.bookmarks.remove(&book.path);
                        let prefix = note_key(&book.path, 0);
                        let prefix = &prefix[..prefix.len() - 1];
                        self.saved.notes.retain(|key, _| !key.starts_with(prefix));
                        self.selected = None;
                        self.chapter = 0;
                        self.status = "Removed library link. Original file retained.".into();
                    } else if let Some(book) = self.books.get(index) {
                        // A borrowed book has a loan sidecar; offer to return it.
                        let book_path = book.path.clone();
                        if loan_sidecar_path(&book_path).exists()
                            && ui
                                .add_enabled(
                                    !self.fulfilling && self.signed_in.is_some(),
                                    egui::Button::new("Return loan").small(),
                                )
                                .clicked()
                        {
                            self.return_loan(book_path, ctx);
                        }
                    }
                }
                if let Some(book) = self.selected.and_then(|i| self.books.get(i)) {
                    ui.separator();
                    ui.heading("Contents");
                    egui::ScrollArea::vertical()
                        .id_salt("contents")
                        .show(ui, |ui| {
                            for (i, chapter) in book.chapters.iter().enumerate() {
                                if ui
                                    .selectable_label(self.chapter == i, &chapter.title)
                                    .clicked()
                                {
                                    self.chapter = i;
                                }
                            }
                        });
                    let marks = self
                        .saved
                        .bookmarks
                        .get(&book.path)
                        .cloned()
                        .unwrap_or_default();
                    if !marks.is_empty() {
                        ui.separator();
                        ui.heading("Bookmarks");
                        for chapter_index in marks {
                            let title = book
                                .chapters
                                .get(chapter_index)
                                .map(|chapter| chapter.title.as_str())
                                .unwrap_or("(section)");
                            if ui
                                .selectable_label(
                                    self.chapter == chapter_index,
                                    format!("★ {title}"),
                                )
                                .clicked()
                            {
                                self.chapter = chapter_index;
                            }
                        }
                    }
                }
            });
        // Sync the note buffer to the current (book, chapter) before rendering, so edits
        // persist and switching sections saves the previous note. Done outside the book
        // borrow to keep saved.notes mutations disjoint from the books borrow.
        let current_note_key = self
            .selected
            .and_then(|i| self.books.get(i))
            .filter(|book| book.blocked.is_none())
            .map(|book| note_key(&book.path, self.chapter));
        if self.note_key != current_note_key {
            if let Some(previous) = self.note_key.take() {
                if self.note_buffer.trim().is_empty() {
                    self.saved.notes.remove(&previous);
                } else {
                    self.saved
                        .notes
                        .insert(previous, std::mem::take(&mut self.note_buffer));
                }
            }
            self.note_buffer = current_note_key
                .as_ref()
                .and_then(|key| self.saved.notes.get(key).cloned())
                .unwrap_or_default();
            self.note_key = current_note_key;
        }
        // Decode any not-yet-cached images for the current section (disjoint field access:
        // reads self.books, writes self.textures). Decoding happens once per image.
        if let Some(book) = self.selected.and_then(|i| self.books.get(i)) {
            if let Some(chapter) = book.chapters.get(self.chapter) {
                for &image_index in &chapter.images {
                    let key = texture_key(&book.path, image_index);
                    if !self.textures.contains_key(&key) {
                        // Bound decoded dimensions and allocation against decode bombs.
                        let mut limits = image::Limits::default();
                        limits.max_image_width = Some(8192);
                        limits.max_image_height = Some(8192);
                        limits.max_alloc = Some(256 * 1024 * 1024);
                        let decoded = image::ImageReader::new(std::io::Cursor::new(
                            &book.images[image_index].bytes,
                        ))
                        .with_guessed_format()
                        .map_err(image::ImageError::IoError)
                        .and_then(|mut reader| {
                            reader.limits(limits);
                            reader.decode()
                        });
                        if let Ok(decoded) = decoded {
                            let rgba = decoded.to_rgba8();
                            let size = [rgba.width() as usize, rgba.height() as usize];
                            let color = egui::ColorImage::from_rgba_unmultiplied(size, &rgba);
                            let handle =
                                ctx.load_texture(&key, color, egui::TextureOptions::LINEAR);
                            self.textures.insert(key, handle);
                        }
                    }
                }
            }
        }
        egui::CentralPanel::default().show(ctx, |ui| {
            // Deferred mutations: applied after the immutable book borrow is released.
            let mut set_chapter: Option<usize> = None;
            let mut toggle_bookmark = false;
            let mut open_layout = false;
            if let Some(book) = self.selected.and_then(|i| self.books.get(i)) {
                ui.heading(&book.title); ui.label(&book.author);
                if let Some(reason) = &book.blocked { ui.separator(); ui.colored_label(egui::Color32::DARK_RED, "Protected EPUB — reader unavailable"); ui.label(reason); }
                else {
                    let bookmarked = self
                        .saved
                        .bookmarks
                        .get(&book.path)
                        .is_some_and(|chapters| chapters.contains(&self.chapter));
                    ui.horizontal(|ui| {
                        if ui.add_enabled(self.chapter > 0, egui::Button::new("Previous")).clicked() { set_chapter = Some(self.chapter - 1); }
                        ui.label(format!("{} / {}", self.chapter + 1, book.chapters.len()));
                        if ui.add_enabled(self.chapter + 1 < book.chapters.len(), egui::Button::new("Next")).clicked() { set_chapter = Some(self.chapter + 1); }
                        ui.separator();
                        if ui.selectable_label(bookmarked, if bookmarked { "★ Bookmarked" } else { "☆ Bookmark" }).clicked() { toggle_bookmark = true; }
                        if !book.chapters.get(self.chapter).map(|c| c.href.is_empty()).unwrap_or(true)
                            && ui.button("Full layout").on_hover_text("Open this section with full HTML/CSS layout in a webview").clicked() { open_layout = true; }
                    });
                    ui.separator();
                    if let Some(chapter) = book.chapters.get(self.chapter) { egui::ScrollArea::vertical().id_salt(("reading", self.selected, self.chapter)).show(ui, |ui| { ui.set_max_width(850.0); ui.heading(&chapter.title); ui.add_space(16.0); ui.add(egui::Label::new(egui::RichText::new(&chapter.text).size(self.font_size)).wrap().selectable(true));
                        // Render this section's images below the text.
                        for &image_index in &chapter.images {
                            if let Some(texture) = self.textures.get(&texture_key(&book.path, image_index)) {
                                ui.add_space(12.0);
                                ui.add(egui::Image::new(texture).max_width(820.0).fit_to_original_size(1.0));
                            }
                        }
                    }); }
                }
            } else {
                ui.add_space(45.0); ui.heading("Your reading library"); ui.label("Import an EPUB or drop it into this window."); ui.add_space(14.0); ui.label("This reader displays EPUB text, chapter navigation, bookmarks, per-section notes, and decrypts ADEPT-protected books when signed in. PDF and full image/CSS layout rendering are still being implemented.");
            }
            // Per-section notes editor (outside the book borrow above).
            if self.note_key.is_some() {
                ui.separator();
                egui::CollapsingHeader::new("Notes for this section").show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.note_buffer)
                            .desired_width(f32::INFINITY)
                            .hint_text("Your notes for this section (saved automatically)"),
                    );
                });
                if let Some(key) = self.note_key.clone() {
                    if self.note_buffer.trim().is_empty() {
                        self.saved.notes.remove(&key);
                    } else {
                        self.saved.notes.insert(key, self.note_buffer.clone());
                    }
                }
            }
            // Apply deferred reader mutations.
            if let Some(chapter) = set_chapter {
                self.chapter = chapter;
            }
            if open_layout {
                self.open_full_layout(ctx);
            }
            if toggle_bookmark {
                if let Some(index) = self.selected {
                    let path = self.books[index].path.clone();
                    let marks = self.saved.bookmarks.entry(path).or_default();
                    if let Some(position) = marks.iter().position(|c| *c == self.chapter) {
                        marks.remove(position);
                    } else {
                        marks.push(self.chapter);
                        marks.sort_unstable();
                    }
                }
            }
        });
        // Record the current reading position so it is restored on next launch.
        if let Some(book) = self.selected.and_then(|i| self.books.get(i)) {
            self.saved.positions.insert(book.path.clone(), self.chapter);
        }
        if self.show_providers {
            let mut open = true;
            egui::Window::new("Sign-in providers").open(&mut open).default_width(520.0).show(ctx, |ui| {
                ui.label(&self.provider_status);
                if ui.add_enabled(!self.discovering, egui::Button::new("Refresh providers")).clicked() { self.discover(ctx); }
                if self.discovering { ui.spinner(); }
                egui::ComboBox::from_id_salt("vendor").selected_text(&self.catalog.entries()[self.vendor].name).show_ui(ui, |ui| { for (i, vendor) in self.catalog.entries().iter().enumerate() { ui.selectable_value(&mut self.vendor, i, &vendor.name); } });
                let vendor = &self.catalog.entries()[self.vendor];
                ui.label(format!("Method: {}", vendor.method));
                ui.label(format!("Node ID: {}", vendor.node_id.as_deref().unwrap_or("(built-in)")));
                ui.label(format!("Sign-in type: {}", vendor.sign_in_type.as_deref().unwrap_or("(unspecified)")));
                ui.separator();
                ui.heading("Account");
                ui.label(&self.sign_in_status);
                if let Some(user) = self.signed_in.clone() {
                    ui.label(format!("Active account: {user}"));
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(!self.syncing, egui::Button::new("Sync from cloud"))
                            .clicked()
                        {
                            self.sync_cloud(ctx);
                        }
                        if ui
                            .add_enabled(!self.syncing, egui::Button::new("Upload ACSM to cloud"))
                            .clicked()
                        {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("ACSM files", &["acsm"])
                                .pick_file()
                            {
                                self.upload_acsm(path, ctx);
                            }
                        }
                        if self.syncing {
                            ui.spinner();
                        }
                        if ui.button("Sign out").clicked() {
                            self.sign_out();
                        }
                    });
                    if !self.sync_status.is_empty() {
                        ui.label(&self.sync_status);
                    }
                } else {
                    let has_certificate = self
                        .catalog
                        .service_info()
                        .certificate
                        .as_deref()
                        .is_some_and(|certificate| !certificate.is_empty());
                    egui::Grid::new("sign-in").num_columns(2).show(ui, |ui| {
                        ui.label("Username");
                        ui.add_enabled(
                            !self.signing,
                            egui::TextEdit::singleline(&mut self.username),
                        );
                        ui.end_row();
                        ui.label("Password");
                        ui.add_enabled(
                            !self.signing,
                            egui::TextEdit::singleline(&mut self.password).password(true),
                        );
                        ui.end_row();
                    });
                    if ui
                        .add_enabled(
                            !self.signing && has_certificate,
                            egui::Button::new("Sign in"),
                        )
                        .clicked()
                    {
                        self.sign_in(ctx);
                    }
                    if self.signing {
                        ui.spinner();
                    }
                    if !has_certificate {
                        ui.label("Waiting for the discovery certificate before sign-in.");
                    }
                }
                ui.separator();
                ui.label("Credentials are encrypted against the service certificate before sending. Activation is stored in the OS credential store. Fulfillment and cloud sync are not yet available in this application.");
            });
            self.show_providers = open;
        }
    }
}
fn main() -> eframe::Result {
    eframe::run_native(
        "Leafport",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1120.0, 780.0])
                .with_min_inner_size([720.0, 480.0]),
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(Desktop::new(cc)))),
    )
}
