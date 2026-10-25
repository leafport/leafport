//! The main window: a collapsible account sidebar beside the header bar, banner and list.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gio, glib};
use leafport_app::{App, Event, Row, Settings, Snapshot, Status};

use crate::account::{Account, SignInDialog};
use crate::covers::Covers;
use crate::operations::Operations;
use crate::row_menu;
use crate::row_widget::RowWidget;
use crate::settings::PreferencesDialog;
use crate::store::{RowObject, RowStore};

const SIGNED_OUT_HINT: &str = "Sign in to see the books in your account.";
const SYNC_ALL: &str = "Sync All";
const ALL_SYNCED: &str = "All Books Synced";

/// The application window and the widgets that follow the app state.
pub struct Window {
    window: adw::ApplicationWindow,
    core: Arc<App>,
    store: RowStore,
    covers: Rc<Covers>,
    /// Shows `signed-in` or `signed-out` in the sidebar.
    account: gtk::Stack,
    /// The sign-in provider, above the user.
    provider: gtk::Label,
    user: gtk::Label,
    /// The account note below the account.
    note: gtk::Label,
    /// Shows `icon` or `spinner` in the refresh button.
    refresh_stack: gtk::Stack,
    refresh_spinner: gtk::Spinner,
    refresh: gio::SimpleAction,
    sync_all: gio::SimpleAction,
    /// The menu section holding "Sync All", whose label says when all books are synced.
    actions: gio::Menu,
    sync_all_label: Cell<&'static str>,
    operations: Operations,
    banner: adw::Banner,
    /// Shows the app's notices over the body.
    toasts: adw::ToastOverlay,
    /// Shows `signed-out` or `list`.
    body: gtk::Stack,
    /// The sign-in hint above the list while signed out.
    list_hint: gtk::Box,
    signed_out: Cell<bool>,
    sign_in: RefCell<Account>,
    /// The latest status from the app.
    status: RefCell<Status>,
    /// The open sign-in dialog, if any.
    dialog: RefCell<Option<Rc<SignInDialog>>>,
    /// The settings in effect.
    settings: RefCell<Settings>,
    /// The open preferences dialog, if any.
    preferences: RefCell<Option<Rc<PreferencesDialog>>>,
}

impl Window {
    /// Build the window for `app`, showing `snapshot`.
    pub fn new(app: &adw::Application, core: Arc<App>, snapshot: Snapshot) -> Rc<Window> {
        let store = RowStore::new();
        let covers = Covers::new();

        // Sidebar.
        let user = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["heading"])
            .build();
        let provider = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .build();
        let sign_out = gtk::Button::builder()
            .label("Sign Out")
            .halign(gtk::Align::Start)
            .build();
        let signed_in = vbox(6);
        signed_in.append(&provider);
        signed_in.append(&user);
        signed_in.append(&sign_out);

        let signed_out = vbox(6);
        signed_out.append(
            &gtk::Label::builder()
                .label("Not signed in")
                .xalign(0.0)
                .build(),
        );
        let sidebar_sign_in = sign_in_button();
        signed_out.append(&sidebar_sign_in);

        let account = gtk::Stack::new();
        account.add_named(&signed_in, Some("signed-in"));
        account.add_named(&signed_out, Some("signed-out"));

        let note = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .visible(false)
            .build();

        let sidebar_box = vbox(12);
        sidebar_box.set_margin_top(12);
        sidebar_box.set_margin_bottom(12);
        sidebar_box.set_margin_start(12);
        sidebar_box.set_margin_end(12);
        sidebar_box.append(&account);
        sidebar_box.append(&note);

        let sidebar_view = adw::ToolbarView::new();
        sidebar_view.add_top_bar(&adw::HeaderBar::new());
        sidebar_view.set_content(Some(&sidebar_box));
        let sidebar = adw::NavigationPage::new(&sidebar_view, "Leafport");

        // Header bar.
        let sidebar_toggle = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-symbolic")
            .action_name("win.show-sidebar")
            .tooltip_text("Toggle Sidebar")
            .build();
        let operations = Operations::new(core.clone());

        let actions = gio::Menu::new();
        actions.append(Some("Import ACSM…"), Some("win.import"));
        actions.append(Some(SYNC_ALL), Some("win.sync-all"));
        let app_section = gio::Menu::new();
        app_section.append(Some("Preferences"), Some("app.preferences"));
        let menu = gio::Menu::new();
        menu.append_section(None, &actions);
        menu.append_section(None, &app_section);
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .primary(true)
            .tooltip_text("Main Menu")
            .build();

        let refresh_spinner = gtk::Spinner::new();
        let refresh_stack = gtk::Stack::new();
        refresh_stack.add_named(
            &gtk::Image::from_icon_name("view-refresh-symbolic"),
            Some("icon"),
        );
        refresh_stack.add_named(&refresh_spinner, Some("spinner"));
        let refresh_button = gtk::Button::builder()
            .child(&refresh_stack)
            .action_name("win.refresh")
            .tooltip_text("Refresh List")
            .build();

        let header = adw::HeaderBar::new();
        header.pack_start(&sidebar_toggle);
        header.pack_start(operations.button());
        header.pack_end(&menu_button);
        header.pack_end(&refresh_button);

        let banner = adw::Banner::new("");
        banner.set_use_markup(false);
        banner.set_button_label(Some("Dismiss"));

        // Body.
        let status_page = adw::StatusPage::builder().title(SIGNED_OUT_HINT).build();
        let status_sign_in = sign_in_button();
        status_sign_in.set_halign(gtk::Align::Center);
        status_sign_in.add_css_class("pill");
        status_page.set_child(Some(&status_sign_in));

        let list_hint = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        list_hint.set_margin_top(6);
        list_hint.set_margin_bottom(6);
        list_hint.set_margin_start(12);
        list_hint.set_margin_end(12);
        list_hint.append(&gtk::Label::new(Some(SIGNED_OUT_HINT)));
        let hint_sign_in = sign_in_button();
        list_hint.append(&hint_sign_in);

        let list = gtk::ListView::new(
            Some(gtk::NoSelection::new(Some(store.model()))),
            Some(row_factory(&covers)),
        );
        let scrolled = gtk::ScrolledWindow::builder()
            .child(&list)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        let list_page = vbox(0);
        list_page.append(&list_hint);
        list_page.append(&scrolled);

        let body = gtk::Stack::new();
        body.add_named(&status_page, Some("signed-out"));
        body.add_named(&list_page, Some("list"));

        let content_view = adw::ToolbarView::new();
        content_view.add_top_bar(&header);
        content_view.add_top_bar(&banner);
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&body));
        content_view.set_content(Some(&toasts));
        let content = adw::NavigationPage::new(&content_view, "Books");

        let split = adw::OverlaySplitView::builder()
            .sidebar(&sidebar)
            .content(&content)
            .build();

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Leafport")
            .default_width(1120)
            .default_height(780)
            .width_request(720)
            .height_request(480)
            .content(&split)
            .build();

        // Handlers. The "Sign In" buttons are connected once the window is shared.
        sign_out.connect_clicked(glib::clone!(
            #[strong]
            core,
            move |_| core.sign_out()
        ));
        window.add_action(&gio::PropertyAction::new(
            "show-sidebar",
            &split,
            "show-sidebar",
        ));
        let refresh = gio::SimpleAction::new("refresh", None);
        refresh.connect_activate(glib::clone!(
            #[strong]
            core,
            move |_, _| core.refresh()
        ));
        window.add_action(&refresh);
        let sync_all = gio::SimpleAction::new("sync-all", None);
        sync_all.connect_activate(glib::clone!(
            #[strong]
            core,
            move |_, _| core.sync_all()
        ));
        window.add_action(&sync_all);
        let import = gio::SimpleAction::new("import", None);
        banner.connect_button_clicked(glib::clone!(
            #[strong]
            core,
            move |banner| {
                // Hide at once; the app confirms with a status update.
                banner.set_revealed(false);
                core.clear_banner();
            }
        ));
        import.connect_activate(glib::clone!(
            #[weak]
            window,
            #[strong]
            core,
            move |_, _| choose_acsm(&window, core.clone())
        ));
        window.add_action(&import);

        let this = Rc::new(Window {
            window,
            core,
            store,
            covers,
            account,
            provider,
            user,
            note,
            refresh_stack,
            refresh_spinner,
            refresh,
            sync_all,
            actions,
            sync_all_label: Cell::new(SYNC_ALL),
            operations,
            banner,
            toasts,
            body,
            list_hint,
            signed_out: Cell::new(true),
            sign_in: RefCell::new(Account::new(&snapshot.status)),
            status: RefCell::new(snapshot.status.clone()),
            dialog: RefCell::default(),
            settings: RefCell::new(snapshot.settings.clone()),
            preferences: RefCell::default(),
        });
        for button in [&sidebar_sign_in, &status_sign_in, &hint_sign_in] {
            let this = Rc::downgrade(&this);
            button.connect_clicked(move |_| {
                if let Some(this) = this.upgrade() {
                    SignInDialog::present(&this);
                }
            });
        }
        this.window
            .insert_action_group("row", Some(&row_menu::action_group(&this)));
        this.store.set_rows(snapshot.rows, &this.covers);
        this.apply_status(&snapshot.status);
        this
    }

    /// Show the window and bring it to the front.
    pub fn present(&self) {
        self.window.present();
    }

    /// Apply one event from the app.
    pub fn apply(&self, event: Event) {
        match event {
            Event::Rows { rows } => self.store.set_rows(rows, &self.covers),
            Event::Row { row } => self.store.set_row(row, &self.covers),
            Event::RowRemoved { key } => self.store.remove(&key, &self.covers),
            Event::Progress { key, fraction } => {
                self.store.progress(&key, fraction);
                self.operations.progress(&key, fraction);
                return;
            }
            // `apply_status` updates the body itself.
            Event::Status { status } => {
                let old = self.status.replace(status.clone());
                let finished = self.sign_in.borrow_mut().on_status(&old, &status);
                self.apply_status(&status);
                self.update_dialog();
                self.update_preferences();
                if finished {
                    let dialog = self.dialog.borrow().clone();
                    if let Some(dialog) = dialog {
                        dialog.close();
                    }
                }
                return;
            }
            Event::Notice { message } => {
                // Book titles are not markup.
                let toast = adw::Toast::builder()
                    .title(message.as_str())
                    .use_markup(false)
                    .build();
                self.toasts.add_toast(toast);
                return;
            }
            Event::Providers { result } => {
                self.sign_in.borrow_mut().on_providers(result);
                self.update_dialog();
                return;
            }
        }
        self.update_sync_all();
        self.update_body();
    }

    /// The window widget.
    pub fn widget(&self) -> adw::ApplicationWindow {
        self.window.clone()
    }

    /// The sign-in account state.
    pub fn account(&self) -> &RefCell<Account> {
        &self.sign_in
    }

    /// The latest status from the app.
    pub fn status(&self) -> Status {
        self.status.borrow().clone()
    }

    /// The open sign-in dialog, if any.
    pub fn dialog(&self) -> Option<Rc<SignInDialog>> {
        self.dialog.borrow().clone()
    }

    /// Remember the open sign-in dialog, or forget it once it is closed.
    pub fn set_dialog(&self, dialog: Option<Rc<SignInDialog>>) {
        *self.dialog.borrow_mut() = dialog;
    }

    /// The settings in effect.
    pub fn settings(&self) -> Settings {
        self.settings.borrow().clone()
    }

    /// Remember the settings now in effect.
    pub fn set_settings(&self, settings: Settings) {
        *self.settings.borrow_mut() = settings;
    }

    /// The open preferences dialog, if any.
    pub fn preferences(&self) -> Option<Rc<PreferencesDialog>> {
        self.preferences.borrow().clone()
    }

    /// Remember the open preferences dialog, or forget it once it is closed.
    pub fn set_preferences(&self, dialog: Option<Rc<PreferencesDialog>>) {
        *self.preferences.borrow_mut() = dialog;
    }

    /// Show the settings in effect in the open preferences dialog.
    pub fn update_preferences(&self) {
        let dialog = self.preferences.borrow().clone();
        if let Some(dialog) = dialog {
            dialog.update(&self.settings.borrow(), &self.status.borrow());
        }
    }

    /// The app core.
    pub fn core(&self) -> Arc<App> {
        self.core.clone()
    }

    /// The current row of `key`, if listed.
    pub fn row(&self, key: &str) -> Option<Row> {
        self.store.find(key).map(|object| object.row())
    }

    fn apply_status(&self, status: &Status) {
        match &status.user {
            Some(user) => {
                let provider = status
                    .provider
                    .as_ref()
                    .map(|provider| format!("Signed in with {provider}"));
                self.provider
                    .set_label(provider.as_deref().unwrap_or("Signed in as"));
                self.user
                    .set_label(status.username.as_deref().unwrap_or(user));
                self.account.set_visible_child_name("signed-in");
            }
            None => self.account.set_visible_child_name("signed-out"),
        }
        self.refresh
            .set_enabled(status.user.is_some() && !status.refreshing);
        self.refresh_stack
            .set_visible_child_name(if status.refreshing { "spinner" } else { "icon" });
        set_spinning(&self.refresh_spinner, status.refreshing);
        let note = self.sign_in.borrow().note(status);
        self.note.set_visible(!note.is_empty());
        self.note.set_label(&note);

        self.sync_all.set_enabled(status.can_sync_all);
        self.update_sync_all();
        self.operations
            .update(&status.operations, status.can_cancel_all);

        self.banner.set_title(&status.banner.join("\n"));
        self.banner.set_revealed(!status.banner.is_empty());

        self.signed_out.set(status.user.is_none());
        self.list_hint.set_visible(status.user.is_none());
        self.update_body();
    }

    fn update_dialog(&self) {
        let dialog = self.dialog.borrow().clone();
        if let Some(dialog) = dialog {
            dialog.update(&self.sign_in.borrow(), &self.status.borrow());
        }
    }

    /// Label the "Sync All" item "All Books Synced" while signed in with rows and nothing
    /// to sync. A menu item has no tooltip, so the label says why it is disabled.
    fn update_sync_all(&self) {
        let all_synced = self.status.borrow().user.is_some()
            && !self.status.borrow().can_sync_all
            && self.store.model().n_items() > 0;
        let label = if all_synced { ALL_SYNCED } else { SYNC_ALL };
        if self.sync_all_label.get() != label {
            self.actions.remove(1);
            self.actions.insert(1, Some(label), Some("win.sync-all"));
            self.sync_all_label.set(label);
        }
    }

    /// Show the status page while signed out with no rows, the list otherwise.
    fn update_body(&self) {
        let page = if self.signed_out.get() && self.store.model().n_items() == 0 {
            "signed-out"
        } else {
            "list"
        };
        self.body.set_visible_child_name(page);
    }
}

fn vbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}

/// A "Sign In" button. `Window::new` connects it to the sign-in dialog.
fn sign_in_button() -> gtk::Button {
    gtk::Button::builder()
        .label("Sign In")
        .halign(gtk::Align::Start)
        .css_classes(["suggested-action"])
        .build()
}

fn set_spinning(spinner: &gtk::Spinner, spinning: bool) {
    spinner.set_visible(spinning);
    spinner.set_spinning(spinning);
}

/// Row widgets over the store's `RowObject`s. Bound widgets reload their cover when
/// `covers` reports it ready.
fn row_factory(covers: &Rc<Covers>) -> gtk::SignalListItemFactory {
    let bound: Rc<RefCell<Vec<RowWidget>>> = Rc::default();
    covers.connect_ready(glib::clone!(
        #[strong]
        bound,
        move |path| {
            for widget in bound.borrow().iter() {
                widget.cover_ready(path);
            }
        }
    ));
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("a ListItem");
        item.set_activatable(false);
        item.set_child(Some(&RowWidget::new()));
    });
    factory.connect_bind(glib::clone!(
        #[strong]
        covers,
        #[strong]
        bound,
        move |_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("a ListItem");
            let (Some(object), Some(widget)) = (
                item.item().and_downcast::<RowObject>(),
                item.child().and_downcast::<RowWidget>(),
            ) else {
                return;
            };
            widget.bind(&object, &covers);
            let mut bound = bound.borrow_mut();
            if !bound.contains(&widget) {
                bound.push(widget);
            }
        }
    ));
    factory.connect_unbind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("a ListItem");
        if let Some(widget) = item.child().and_downcast::<RowWidget>() {
            widget.unbind();
            bound.borrow_mut().retain(|other| other != &widget);
        }
    });
    factory
}

/// Let the user pick an ACSM file and import it.
fn choose_acsm(window: &adw::ApplicationWindow, core: Arc<App>) {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("ACSM"));
    filter.add_suffix("acsm");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let dialog = gtk::FileDialog::builder()
        .filters(&filters)
        .default_filter(&filter)
        .modal(true)
        .build();
    dialog.open(Some(window), gio::Cancellable::NONE, move |result| {
        // A dismissed dialog reports an error; there is nothing to import then.
        let Some(path) = result.ok().and_then(|file| file.path()) else {
            return;
        };
        // The app takes the path as text; a lossy copy would name another file.
        match path.to_str() {
            Some(path) => core.import_acsm(path.to_owned()),
            None => core.add_message(format!(
                "{} cannot be used: its name is not valid Unicode.",
                path.display()
            )),
        }
    });
}
