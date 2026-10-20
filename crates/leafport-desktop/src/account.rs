//! Account handling: provider discovery, SignInDirect, sign-out and the sign-in window.

use eframe::egui;
use leafport_core::{
    activation::{self, ActivationKeys},
    service::ServiceClient,
    store::{ActivationState, CredentialStore, DEFAULT_ACCOUNT, KeyringStore},
    vendors::VendorCatalog,
};
use std::sync::mpsc::Sender;

use crate::jobs::{Event, catch_panic};

pub struct Account {
    catalog: VendorCatalog,
    vendor: usize,
    discovering: bool,
    discovered: bool,
    provider_status: String,
    username: String,
    password: String,
    signing: bool,
    /// The activated account label from the credential store, when signed in.
    pub signed_in: Option<String>,
    pub status: String,
    /// Whether the sign-in window is shown.
    pub open: bool,
}

impl Account {
    /// Load the signed-in account label from the OS credential store, if present.
    /// A backend failure is surfaced in the status rather than hidden.
    pub fn new() -> Self {
        let (signed_in, status) = match ActivationState::load(&KeyringStore::new(), DEFAULT_ACCOUNT)
        {
            Ok(Some(state)) => (Some(state.user), String::new()),
            Ok(None) => (None, "Not signed in.".into()),
            Err(error) => (None, format!("Credential store unavailable: {error}")),
        };
        Self {
            catalog: VendorCatalog::new(),
            vendor: 0,
            discovering: false,
            discovered: false,
            provider_status: String::new(),
            username: String::new(),
            password: String::new(),
            signing: false,
            signed_in,
            status,
            open: false,
        }
    }

    /// Show the sign-in window, loading the providers on first open.
    pub fn open_window(&mut self, tx: &Sender<Event>, ctx: &egui::Context) {
        self.open = true;
        if !self.discovered {
            self.discover(tx, ctx);
        }
    }

    fn discover(&mut self, tx: &Sender<Event>, ctx: &egui::Context) {
        if self.discovering {
            return;
        }
        self.discovering = true;
        self.provider_status = "Loading sign-in providers…".into();
        let tx = tx.clone();
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

    pub fn on_vendors(&mut self, result: Result<VendorCatalog, String>) {
        self.discovering = false;
        match result {
            Ok(catalog) => {
                self.provider_status =
                    format!("{} sign-in choices loaded", catalog.entries().len());
                self.catalog = catalog;
                self.vendor = 0;
                self.discovered = true;
            }
            Err(e) => {
                self.provider_status = format!("Discovery failed: {e}. Existing choices retained.")
            }
        }
    }

    /// Start a SignInDirect exchange on a worker thread. Credentials are moved into the
    /// thread, encrypted by the core, and cleared from the UI immediately. On success the
    /// resulting activation state is written to the OS credential store.
    fn sign_in(&mut self, tx: &Sender<Event>, ctx: &egui::Context) {
        if self.signing {
            return;
        }
        let info = self.catalog.service_info();
        let (auth_url, certificate) = match (info.auth_url.clone(), info.certificate.clone()) {
            (Some(auth_url), Some(certificate)) if !certificate.is_empty() => {
                (auth_url, certificate)
            }
            _ => {
                self.status = "Discovery has no authentication URL or certificate yet.".into();
                return;
            }
        };
        if self.username.is_empty() || self.password.is_empty() {
            self.status = "Enter a username and password.".into();
            return;
        }
        let method = self.catalog.entries()[self.vendor].method.clone();
        // Move the credentials out of the UI so they are not retained after submission.
        let username = std::mem::take(&mut self.username);
        let password = std::mem::take(&mut self.password);
        self.signing = true;
        self.status = "Signing in…".into();
        let tx = tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = catch_panic(|| {
                let client = ServiceClient::new().map_err(|e| e.to_string())?;
                // Device key and activation keys are generated locally for this device.
                // The 16-byte device key is random and persisted with the activation state.
                // Reuse an existing activation so books fulfilled under it stay readable.
                let (device_key, keys, device, fingerprint) = match ActivationState::load(
                    &KeyringStore::new(),
                    DEFAULT_ACCOUNT,
                ) {
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
                // Adopt the account's license and signing keys when the service returns
                // them; the generated keys only apply to a brand-new account.
                let mut keys = keys;
                if let Some(license) =
                    activation::account_license_key(&signed.credentials_xml, &device_key)
                {
                    keys.license = license;
                }
                if let Some(authentication) =
                    activation::account_auth_key(&signed.credentials_xml, &device_key)
                {
                    keys.authentication = authentication;
                }
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

    /// Handle a finished sign-in. Returns true when an account is now active.
    pub fn on_sign_in(&mut self, result: Result<String, String>) -> bool {
        self.signing = false;
        match result {
            Ok(user) => {
                self.status = String::new();
                self.signed_in = Some(user);
                self.open = false;
                true
            }
            Err(error) => {
                self.status = format!("Sign-in failed: {error}");
                false
            }
        }
    }

    /// Remove the stored activation state. Title folders are untouched.
    pub fn sign_out(&mut self) -> Result<(), String> {
        match KeyringStore::new().delete(DEFAULT_ACCOUNT) {
            Ok(()) => {
                self.signed_in = None;
                self.status = "Signed out.".into();
                Ok(())
            }
            Err(error) => {
                self.status = format!("Sign-out failed: {error}");
                Err(self.status.clone())
            }
        }
    }

    /// The provider/username/password form, as a window.
    pub fn window(&mut self, tx: &Sender<Event>, ctx: &egui::Context) {
        let mut open = self.open;
        egui::Window::new("Sign in").open(&mut open).default_width(520.0).show(ctx, |ui| {
            ui.label(&self.provider_status);
            if ui.add_enabled(!self.discovering, egui::Button::new("Refresh providers")).clicked() {
                self.discover(tx, ctx);
            }
            if self.discovering {
                ui.spinner();
            }
            egui::ComboBox::from_id_salt("vendor")
                .selected_text(&self.catalog.entries()[self.vendor].name)
                .show_ui(ui, |ui| {
                    for (i, vendor) in self.catalog.entries().iter().enumerate() {
                        ui.selectable_value(&mut self.vendor, i, &vendor.name);
                    }
                });
            let vendor = &self.catalog.entries()[self.vendor];
            ui.label(format!("Method: {}", vendor.method));
            ui.label(format!("Node ID: {}", vendor.node_id.as_deref().unwrap_or("(built-in)")));
            ui.label(format!(
                "Sign-in type: {}",
                vendor.sign_in_type.as_deref().unwrap_or("(unspecified)")
            ));
            ui.separator();
            let has_certificate = self
                .catalog
                .service_info()
                .certificate
                .as_deref()
                .is_some_and(|certificate| !certificate.is_empty());
            egui::Grid::new("sign-in").num_columns(2).show(ui, |ui| {
                ui.label("Username");
                ui.add_enabled(!self.signing, egui::TextEdit::singleline(&mut self.username));
                ui.end_row();
                ui.label("Password");
                ui.add_enabled(
                    !self.signing,
                    egui::TextEdit::singleline(&mut self.password).password(true),
                );
                ui.end_row();
            });
            if ui
                .add_enabled(!self.signing && has_certificate, egui::Button::new("Sign in"))
                .clicked()
            {
                self.sign_in(tx, ctx);
            }
            if self.signing {
                ui.spinner();
            }
            if !has_certificate {
                ui.label("Waiting for the discovery certificate before sign-in.");
            }
            ui.label(&self.status);
            ui.separator();
            ui.label("Credentials are encrypted against the service certificate before sending. Activation is stored in the OS credential store.");
        });
        self.open = open;
    }
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
