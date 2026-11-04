//! The operations button: the running and queued syncs and imports and the running loan
//! returns, in a popover, as GNOME Files shows its file operations.

use std::cell::{Cell, RefCell};
use std::f64::consts::{FRAC_PI_2, TAU};
use std::rc::Rc;
use std::sync::Arc;

use gtk::glib;
use gtk::prelude::*;
use leafport_app::{App, Operation, OperationKind};

/// The icon's size, a symbolic icon's.
const ICON_SIZE: i32 = 16;

/// The header bar button and its popover.
pub struct Operations {
    button: gtk::MenuButton,
    list: gtk::ListBox,
    cancel_all: gtk::Button,
    icon: gtk::DrawingArea,
    core: Arc<App>,
    /// The operations the list shows.
    shown: RefCell<Vec<Operation>>,
    /// The running sync's key, with its progress bar in the list.
    running: RefCell<Option<(String, gtk::ProgressBar)>>,
    /// The fraction the icon fills: the running sync's progress, else 0.
    fraction: Rc<Cell<f64>>,
}

impl Operations {
    pub fn new(core: Arc<App>) -> Operations {
        let fraction = Rc::new(Cell::new(0.0));
        let icon = gtk::DrawingArea::builder()
            .content_width(ICON_SIZE)
            .content_height(ICON_SIZE)
            .build();
        let shown_fraction = fraction.clone();
        icon.set_draw_func(move |area, cr, width, height| {
            draw_pie(area, cr, width, height, shown_fraction.get());
        });

        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .show_separators(true)
            .build();
        let scrolled = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(400)
            .min_content_width(320)
            .build();
        let cancel_all = gtk::Button::builder()
            .label("Cancel All")
            .halign(gtk::Align::End)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.append(&scrolled);
        content.append(&cancel_all);
        let popover = gtk::Popover::builder().child(&content).build();

        let button = gtk::MenuButton::builder()
            .child(&icon)
            .popover(&popover)
            .tooltip_text("Show Operations")
            .visible(false)
            .build();

        cancel_all.connect_clicked(glib::clone!(
            #[strong]
            core,
            move |_| core.cancel_all()
        ));

        Operations {
            button,
            list,
            cancel_all,
            icon,
            core,
            shown: RefCell::default(),
            running: RefCell::default(),
            fraction,
        }
    }

    /// The header bar button.
    pub fn button(&self) -> &gtk::MenuButton {
        &self.button
    }

    /// Show `operations`. The button hides, and its popover closes, once none are left.
    pub fn update(&self, operations: &[Operation], can_cancel_all: bool) {
        self.button.set_visible(!operations.is_empty());
        if operations.is_empty() {
            self.button.popdown();
        }
        self.cancel_all.set_visible(can_cancel_all);
        if *self.shown.borrow() == operations {
            return;
        }
        let running_key = operations
            .iter()
            .find_map(|operation| match &operation.kind {
                OperationKind::Sync { key } if operation.running => Some(key.clone()),
                _ => None,
            });
        let same_sync = self.running.borrow().as_ref().map(|(key, _)| key) == running_key.as_ref();
        if !same_sync {
            self.fraction.set(0.0);
        }
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let mut running = None;
        for operation in operations {
            let (row, bar) = self.row(operation);
            if let Some(bar) = bar
                && let OperationKind::Sync { key } = &operation.kind
            {
                bar.set_fraction(self.fraction.get());
                running = Some((key.clone(), bar));
            }
            self.list.append(&row);
        }
        self.running.replace(running);
        self.shown.replace(operations.to_vec());
        self.icon.queue_draw();
    }

    /// Download progress of the sync of `key`.
    pub fn progress(&self, key: &str, fraction: f32) {
        let running = self.running.borrow();
        let Some((running_key, bar)) = running.as_ref() else {
            return;
        };
        if running_key != key {
            return;
        }
        let fraction = f64::from(fraction.clamp(0.0, 1.0));
        self.fraction.set(fraction);
        bar.set_fraction(fraction);
        self.icon.queue_draw();
    }

    /// The list row of `operation`, and its progress bar while it is the running sync.
    fn row(&self, operation: &Operation) -> (gtk::ListBoxRow, Option<gtk::ProgressBar>) {
        let title = gtk::Label::builder()
            .label(&operation.label)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let detail = match (&operation.kind, operation.running) {
            (OperationKind::Return { .. }, _) => "Returning loan",
            (_, false) => "Waiting",
            (OperationKind::Sync { .. }, true) => "Syncing",
            (OperationKind::Import { .. }, true) => "Importing",
        };
        let detail = gtk::Label::builder()
            .label(detail)
            .xalign(0.0)
            .css_classes(["caption", "dim-label"])
            .build();
        let text = gtk::Box::new(gtk::Orientation::Vertical, 4);
        text.set_hexpand(true);
        text.set_valign(gtk::Align::Center);
        text.append(&title);
        let bar = (operation.running && matches!(operation.kind, OperationKind::Sync { .. }))
            .then(gtk::ProgressBar::new);
        if let Some(bar) = &bar {
            text.append(bar);
        }
        text.append(&detail);

        let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        content.set_margin_top(6);
        content.set_margin_bottom(6);
        content.set_margin_start(6);
        content.set_margin_end(6);
        content.append(&text);
        if let Some(cancel) = self.cancel_button(operation) {
            content.append(&cancel);
        }
        let row = gtk::ListBoxRow::builder()
            .child(&content)
            .activatable(false)
            .build();
        (row, bar)
    }

    /// A cancel button for `operation`, insensitive once cancelling has no effect. A
    /// loan return gets none: it cannot be cancelled.
    fn cancel_button(&self, operation: &Operation) -> Option<gtk::Button> {
        let core = self.core.clone();
        let cancel: Box<dyn Fn()> = match operation.kind.clone() {
            OperationKind::Sync { key } => Box::new(move || core.cancel(key.clone())),
            OperationKind::Import { path } => Box::new(move || core.cancel_import(path.clone())),
            OperationKind::Return { .. } => return None,
        };
        let button = gtk::Button::builder()
            .icon_name("process-stop-symbolic")
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular"])
            .sensitive(operation.cancellable)
            .tooltip_text("Cancel")
            .build();
        button.connect_clicked(move |_| cancel());
        Some(button)
    }
}

/// A pie in the foreground color: a faint disc, filled clockwise from the top by
/// `fraction`.
fn draw_pie(
    area: &gtk::DrawingArea,
    cr: &gtk::cairo::Context,
    width: i32,
    height: i32,
    fraction: f64,
) {
    let color = area.color();
    let (x, y) = (f64::from(width) / 2.0, f64::from(height) / 2.0);
    let radius = x.min(y) - 1.0;
    let (red, green, blue, alpha) = (
        f64::from(color.red()),
        f64::from(color.green()),
        f64::from(color.blue()),
        f64::from(color.alpha()),
    );
    cr.set_source_rgba(red, green, blue, alpha * 0.25);
    cr.arc(x, y, radius, 0.0, TAU);
    let _ = cr.fill();
    if fraction > 0.0 {
        cr.set_source_rgba(red, green, blue, alpha);
        cr.move_to(x, y);
        cr.arc(x, y, radius, -FRAC_PI_2, -FRAC_PI_2 + TAU * fraction);
        cr.close_path();
        let _ = cr.fill();
    }
}
