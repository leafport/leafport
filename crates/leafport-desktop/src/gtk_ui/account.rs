//! Sign-in: the UI-free account state and the sign-in dialog.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use leafport_app::{Provider, Status};
use leafport_core::vendors::VendorCatalog;

use crate::gtk_ui::window::Window;

/// UI-free sign-in state, ported from leafport-desktop's account.rs.
pub struct Account {
    /// The sign-in providers: the built-in ones until a load succeeds.
    pub providers: Vec<Provider>,
    pub selected: u32,
    /// Whether a provider load succeeded; until then opening the dialog loads them.
    pub loaded: bool,
    pub provider_status: String,
    /// The username typed in the sign-in dialog, kept while it is closed. The password
    /// is never kept.
    pub username: String,
    /// Whether an account was signed in during this run, so signed out reads "Signed out.".
    was_signed_in: bool,
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
            selected: 0,
            loaded: false,
            provider_status: String::new(),
            username: String::new(),
            was_signed_in: status.user.is_some(),
        }
    }

    /// Apply a finished provider load.
    pub fn on_providers(&mut self, result: Result<Vec<Provider>, String>) {
        match result {
            Ok(providers) => {
                self.provider_status = format!("{} sign-in choices loaded", providers.len());
                self.providers = providers;
                self.selected = 0;
                self.loaded = true;
            }
            Err(e) => {
                self.provider_status = format!("Discovery failed: {e}. Existing choices retained.")
            }
        }
    }

    /// Follow a status change. Returns true when a sign-in just finished signed in: the
    /// dialog closes.
    pub fn on_status(&mut self, old: &Status, new: &Status) -> bool {
        if new.user.is_none() {
            return false;
        }
        self.was_signed_in = true;
        old.signing_in && !new.signing_in
    }

    /// The account line shown in the sidebar and the dialog.
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

    /// The selected provider.
    fn provider(&self) -> Option<&Provider> {
        self.providers.get(self.selected as usize)
    }
}

/// The sign-in dialog and the widgets that follow the account state.
pub struct SignInDialog {
    dialog: adw::Dialog,
    status: gtk::Label,
    refresh: gtk::Button,
    refresh_spinner: gtk::Spinner,
    provider: adw::ComboRow,
    names: gtk::StringList,
    method: adw::ActionRow,
    node_id: adw::ActionRow,
    sign_in_type: adw::ActionRow,
    username: adw::EntryRow,
    password: adw::PasswordEntryRow,
    sign_in: gtk::Button,
    sign_in_spinner: gtk::Spinner,
    waiting: gtk::Label,
    note: gtk::Label,
    /// Set while `update` changes widgets, so their handlers stay quiet.
    updating: Cell<bool>,
}

impl SignInDialog {
    /// Show the dialog over `window`, loading the providers until a load succeeds.
    pub fn present(window: &Rc<Window>) {
        if let Some(dialog) = window.dialog() {
            dialog.dialog.present(Some(&window.widget()));
            return;
        }
        if !window.account().borrow().loaded {
            window.core().load_providers();
        }
        let dialog = Self::build();
        dialog
            .username
            .set_text(&window.account().borrow().username);
        dialog.connect(window);
        dialog.update(&window.account().borrow(), &window.status());
        window.set_dialog(Some(dialog.clone()));
        dialog.dialog.present(Some(&window.widget()));
    }

    /// Follow the account state and the app status.
    pub fn update(&self, account: &Account, status: &Status) {
        self.updating.set(true);
        let loading = status.loading_providers;
        let provider_status = if loading {
            "Loading sign-in providers…"
        } else {
            &account.provider_status
        };
        self.status.set_label(provider_status);
        // An empty label would still take its row's spacing.
        self.status.set_visible(!provider_status.is_empty());
        self.refresh.set_sensitive(!loading);
        set_spinning(&self.refresh_spinner, loading);

        let names: Vec<&str> = account.providers.iter().map(|p| p.name.as_str()).collect();
        let shown: Vec<glib::GString> = (0..self.names.n_items())
            .filter_map(|i| self.names.string(i))
            .collect();
        if shown.iter().map(|s| s.as_str()).ne(names.iter().copied()) {
            self.names.splice(0, self.names.n_items(), &names);
        }
        if self.provider.selected() != account.selected {
            self.provider.set_selected(account.selected);
        }
        // Only the details the selected provider has are shown.
        let provider = account.provider();
        set_detail(&self.method, provider.map(|p| p.method.as_str()));
        set_detail(&self.node_id, provider.and_then(|p| p.node_id.as_deref()));
        set_detail(
            &self.sign_in_type,
            provider.and_then(|p| p.sign_in_type.as_deref()),
        );

        let signing = status.signing_in;
        self.username.set_sensitive(!signing);
        self.password.set_sensitive(!signing);
        self.sign_in.set_sensitive(!signing && status.sign_in_ready);
        set_spinning(&self.sign_in_spinner, signing);
        self.waiting.set_visible(!status.sign_in_ready);
        let note = account.note(status);
        self.note.set_visible(!note.is_empty());
        self.note.set_label(&note);
        self.updating.set(false);
    }

    /// Close the dialog.
    pub fn close(&self) {
        self.dialog.close();
    }

    fn build() -> Rc<Self> {
        let status = wrapped_label(&[]);
        let refresh = gtk::Button::with_label("Refresh Providers");
        let refresh_spinner = gtk::Spinner::new();
        let status_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        status.set_hexpand(true);
        status_box.append(&status);
        status_box.append(&refresh_spinner);
        status_box.append(&refresh);
        let status_group = adw::PreferencesGroup::new();
        status_group.add(&status_box);

        let names = gtk::StringList::new(&[]);
        let provider = adw::ComboRow::builder()
            .title("Provider")
            .model(&names)
            .build();
        let method = info_row("Method");
        let node_id = info_row("Node ID");
        let sign_in_type = info_row("Sign-in type");
        let provider_group = adw::PreferencesGroup::new();
        provider_group.add(&provider);
        provider_group.add(&method);
        provider_group.add(&node_id);
        provider_group.add(&sign_in_type);

        let username = adw::EntryRow::builder().title("Username").build();
        let password = adw::PasswordEntryRow::builder().title("Password").build();
        let credentials = adw::PreferencesGroup::new();
        credentials.add(&username);
        credentials.add(&password);

        let sign_in = gtk::Button::builder()
            .label("Sign In")
            .halign(gtk::Align::Start)
            .css_classes(["suggested-action"])
            .build();
        let sign_in_spinner = gtk::Spinner::new();
        let action = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        action.append(&sign_in);
        action.append(&sign_in_spinner);
        let waiting = wrapped_label(&[]);
        waiting.set_label("Waiting for the discovery certificate before sign-in.");
        let note = wrapped_label(&[]);
        let footer = wrapped_label(&["dim-label"]);
        footer.set_label(
            "Credentials are encrypted against the service certificate before sending. \
             Activation is stored in the OS credential store.",
        );
        let submit = adw::PreferencesGroup::new();
        for widget in [action.upcast_ref::<gtk::Widget>(), waiting.upcast_ref()] {
            submit.add(widget);
        }
        submit.add(&note);
        submit.add(&footer);

        let page = adw::PreferencesPage::new();
        page.add(&status_group);
        page.add(&provider_group);
        page.add(&credentials);
        page.add(&submit);
        let view = adw::ToolbarView::new();
        view.add_top_bar(&adw::HeaderBar::new());
        view.set_content(Some(&page));
        let dialog = adw::Dialog::builder()
            .title("Sign In")
            .content_width(520)
            .child(&view)
            .build();
        Rc::new(Self {
            dialog,
            status,
            refresh,
            refresh_spinner,
            provider,
            names,
            method,
            node_id,
            sign_in_type,
            username,
            password,
            sign_in,
            sign_in_spinner,
            waiting,
            note,
            updating: Cell::new(false),
        })
    }

    fn connect(self: &Rc<Self>, window: &Rc<Window>) {
        let this = Rc::downgrade(self);
        let win = Rc::downgrade(window);
        self.provider.connect_selected_notify(glib::clone!(
            #[strong]
            win,
            #[strong]
            this,
            move |row| {
                let (Some(win), Some(this)) = (win.upgrade(), this.upgrade()) else {
                    return;
                };
                if this.updating.get() {
                    return;
                }
                win.account().borrow_mut().selected = row.selected();
                this.update(&win.account().borrow(), &win.status());
            }
        ));
        self.refresh.connect_clicked(glib::clone!(
            #[strong]
            win,
            move |_| {
                if let Some(win) = win.upgrade() {
                    win.core().load_providers();
                }
            }
        ));
        let submit = glib::clone!(
            #[strong]
            win,
            #[strong]
            this,
            move || {
                if let (Some(win), Some(this)) = (win.upgrade(), this.upgrade()) {
                    this.submit(&win);
                }
            }
        );
        self.sign_in.connect_clicked({
            let submit = submit.clone();
            move |_| submit()
        });
        self.username.connect_changed(glib::clone!(
            #[strong]
            win,
            move |entry| {
                if let Some(win) = win.upgrade() {
                    win.account().borrow_mut().username = entry.text().to_string();
                }
            }
        ));
        // Enter in Username moves on to Password; Enter in Password submits.
        self.username.connect_entry_activated(glib::clone!(
            #[weak(rename_to = password)]
            self.password,
            move |_| {
                password.grab_focus();
            }
        ));
        self.password.connect_entry_activated(move |_| submit());
        self.dialog.connect_closed(glib::clone!(
            #[strong]
            win,
            move |_| {
                if let Some(win) = win.upgrade() {
                    win.set_dialog(None);
                }
            }
        ));
    }

    /// Submit the form. An incomplete form, or one sent before discovery is ready, keeps
    /// both fields; the app answers an incomplete one. A complete one clears the password
    /// so it is not retained; the username stays.
    fn submit(&self, window: &Window) {
        if !window.status().sign_in_ready {
            return;
        }
        let Some(method) = window
            .account()
            .borrow()
            .provider()
            .map(|p| p.method.clone())
        else {
            return;
        };
        let username = self.username.text().to_string();
        let password = self.password.text().to_string();
        let complete = !username.is_empty() && !password.is_empty();
        window.core().sign_in(method, username, password);
        if complete {
            self.password.set_text("");
        }
    }
}

/// A wrapping, left-aligned label with `classes`.
fn wrapped_label(classes: &[&str]) -> gtk::Label {
    gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .css_classes(classes.iter().map(|c| c.to_string()).collect::<Vec<_>>())
        .build()
}

/// A read-only row with a plain-text subtitle.
fn info_row(title: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .use_markup(false)
        .build();
    row.add_css_class("property");
    row
}

/// Show `value` in `row`, or hide the row when there is none.
fn set_detail(row: &adw::ActionRow, value: Option<&str>) {
    let value = value.filter(|value| !value.is_empty());
    row.set_visible(value.is_some());
    row.set_subtitle(value.unwrap_or_default());
}

fn set_spinning(spinner: &gtk::Spinner, spinning: bool) {
    spinner.set_visible(spinning);
    spinner.set_spinning(spinning);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(user: Option<&str>) -> Status {
        Status {
            user: user.map(Into::into),
            username: None,
            provider: None,
            signing_in: false,
            loading_providers: false,
            refreshing: false,
            operations: Vec::new(),
            job_running: false,
            can_sync_all: false,
            can_cancel_all: false,
            sign_in_error: None,
            sign_in_ready: true,
            banner: Vec::new(),
            books_dir: None,
        }
    }

    fn provider(name: &str) -> Provider {
        Provider {
            name: name.into(),
            method: "m".into(),
            node_id: None,
            sign_in_type: None,
        }
    }

    #[test]
    fn note_follows_status() {
        let account = Account::new(&status(None));
        let mut s = status(None);
        assert_eq!(account.note(&s), "Not signed in.");
        s.signing_in = true;
        assert_eq!(account.note(&s), "Signing in…");
        s.signing_in = false;
        s.sign_in_error = Some("x".into());
        assert_eq!(account.note(&s), "x");
        assert_eq!(account.note(&status(Some("u"))), "");
    }

    #[test]
    fn note_after_sign_out() {
        let account = Account::new(&status(Some("u")));
        assert_eq!(account.note(&status(None)), "Signed out.");
    }

    #[test]
    fn providers_loaded_and_failed() {
        let mut account = Account::new(&status(None));
        let before = account.providers.clone();
        account.selected = 3;
        account.on_providers(Ok(vec![provider("a"), provider("b")]));
        assert_eq!(account.provider_status, "2 sign-in choices loaded");
        assert!(account.loaded);
        assert_eq!(account.selected, 0);
        assert_eq!(account.providers.len(), 2);
        let kept = account.providers.clone();
        account.on_providers(Err("timeout".into()));
        assert_eq!(
            account.provider_status,
            "Discovery failed: timeout. Existing choices retained."
        );
        assert_eq!(account.providers, kept);
        assert_ne!(before, kept);
    }

    #[test]
    fn finished_sign_in_closes() {
        let mut account = Account::new(&status(None));
        let mut old = status(None);
        old.signing_in = true;
        assert!(account.on_status(&old, &status(Some("u"))));
        assert!(!account.on_status(&old, &status(None)));
    }
}
