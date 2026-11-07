//! The sign-in window: provider choice, username and password.

use eframe::egui;
use leafport_app::{Provider, Status};
use leafport_core::vendors::VendorCatalog;

pub struct Account {
    /// The sign-in providers: the built-in ones until a load succeeds.
    providers: Vec<Provider>,
    provider: usize,
    /// Whether a provider load succeeded; until then opening the window loads them.
    loaded: bool,
    provider_status: String,
    username: String,
    password: String,
    /// Whether an account was signed in during this run, so signed out reads "Signed out.".
    was_signed_in: bool,
    /// Whether the sign-in window is shown.
    pub open: bool,
}

impl Account {
    pub fn new(status: &Status) -> Self {
        let providers = VendorCatalog::new()
            .entries()
            .iter()
            .map(|vendor| Provider {
                name: vendor.name.clone(),
                method: vendor.method.clone(),
                node_id: vendor.node_id.clone(),
                sign_in_type: vendor.sign_in_type.clone(),
            })
            .collect();
        Self {
            providers,
            provider: 0,
            loaded: false,
            provider_status: String::new(),
            username: String::new(),
            password: String::new(),
            was_signed_in: status.user.is_some(),
            open: false,
        }
    }

    /// Show the sign-in window, loading the providers until a load succeeds.
    pub fn open_window(&mut self, core: &leafport_app::App) {
        self.open = true;
        if !self.loaded {
            core.load_providers();
        }
    }

    /// Apply a finished provider load.
    pub fn on_providers(&mut self, result: Result<Vec<Provider>, String>) {
        match result {
            Ok(providers) => {
                self.provider_status = format!("{} sign-in choices loaded", providers.len());
                self.providers = providers;
                self.provider = 0;
                self.loaded = true;
            }
            Err(e) => {
                self.provider_status = format!("Discovery failed: {e}. Existing choices retained.")
            }
        }
    }

    /// Follow a status change: a finished sign-in closes the window.
    pub fn on_status(&mut self, old: &Status, new: &Status) {
        if new.user.is_some() {
            self.was_signed_in = true;
            if old.signing_in && !new.signing_in {
                self.open = false;
            }
        }
    }

    /// The account line shown in the sidebar and the sign-in window.
    pub fn note(&self, status: &Status) -> String {
        if status.signing_in {
            "Signing in…".into()
        } else if let Some(error) = &status.sign_in_error {
            error.clone()
        } else if status.user.is_some() {
            String::new()
        } else if self.was_signed_in {
            "Signed out.".into()
        } else {
            "Not signed in.".into()
        }
    }

    /// Submit the form. The password is moved out of the UI so it is not retained after
    /// submission; the username stays, so a failed sign-in needs only the password again.
    /// An incomplete form, or one sent before discovery is ready, keeps both.
    fn sign_in(&mut self, core: &leafport_app::App, status: &Status) {
        if !status.sign_in_ready {
            return;
        }
        let Some(provider) = self.providers.get(self.provider) else {
            return;
        };
        let method = provider.method.clone();
        if self.username.is_empty() || self.password.is_empty() {
            // The app answers "Enter a username and password."
            core.sign_in(method, self.username.clone(), self.password.clone());
            return;
        }
        core.sign_in(
            method,
            self.username.clone(),
            std::mem::take(&mut self.password),
        );
    }

    /// The provider/username/password form, as a window.
    pub fn window(&mut self, core: &leafport_app::App, status: &Status, ctx: &egui::Context) {
        let mut open = self.open;
        let signing = status.signing_in;
        let loading = status.loading_providers;
        let note = self.note(status);
        egui::Window::new("Sign in").open(&mut open).default_width(520.0).show(ctx, |ui| {
            if loading {
                ui.label("Loading sign-in providers…");
            } else {
                ui.label(&self.provider_status);
            }
            if ui.add_enabled(!loading, egui::Button::new("Refresh providers")).clicked() {
                core.load_providers();
            }
            if loading {
                ui.spinner();
            }
            if let Some(selected) = self.providers.get(self.provider) {
                egui::ComboBox::from_id_salt("vendor")
                    .selected_text(&selected.name)
                    .show_ui(ui, |ui| {
                        for (i, vendor) in self.providers.iter().enumerate() {
                            ui.selectable_value(&mut self.provider, i, &vendor.name);
                        }
                    });
            }
            if let Some(vendor) = self.providers.get(self.provider) {
                // Only the details the selected provider has are shown.
                let details = [
                    ("Method", Some(vendor.method.as_str())),
                    ("Node ID", vendor.node_id.as_deref()),
                    ("Sign-in type", vendor.sign_in_type.as_deref()),
                ];
                for (title, value) in details {
                    if let Some(value) = value.filter(|value| !value.is_empty()) {
                        ui.add(egui::Label::new(format!("{title}: {value}")).selectable(false));
                    }
                }
            }
            ui.separator();
            let has_certificate = status.sign_in_ready;
            egui::Grid::new("sign-in").num_columns(2).show(ui, |ui| {
                ui.label("Username");
                ui.add_enabled(!signing, egui::TextEdit::singleline(&mut self.username));
                ui.end_row();
                ui.label("Password");
                ui.add_enabled(
                    !signing,
                    egui::TextEdit::singleline(&mut self.password).password(true),
                );
                ui.end_row();
            });
            if ui
                .add_enabled(!signing && has_certificate, egui::Button::new("Sign in"))
                .clicked()
            {
                self.sign_in(core, status);
            }
            if signing {
                ui.spinner();
            }
            if !has_certificate {
                ui.label("Waiting for the discovery certificate before sign-in.");
            }
            ui.label(note);
            ui.separator();
            ui.label("Credentials are encrypted against the service certificate before sending. Activation is stored in the OS credential store.");
        });
        self.open = open;
    }
}
