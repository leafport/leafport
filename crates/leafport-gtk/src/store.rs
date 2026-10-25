//! The row list as a `gio::ListStore` of `RowObject`s, without any widgets.

use std::cell::RefCell;
use std::collections::HashMap;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use leafport_app::{Activity, Row};

use crate::covers::Covers;

mod imp {
    use std::cell::{Cell, RefCell};

    use super::*;

    #[derive(glib::Properties)]
    #[properties(wrapper_type = super::RowObject, ext_trait)]
    pub struct RowObject {
        pub row: RefCell<Option<Row>>,
        /// Counts `set_row` calls so widgets can rebind on `notify::version`.
        #[property(get)]
        pub version: Cell<u32>,
        /// Fraction in `0.0..=1.0` while a row syncs; negative means none.
        #[property(get, set)]
        pub progress: Cell<f64>,
    }

    impl Default for RowObject {
        fn default() -> Self {
            Self {
                row: RefCell::new(None),
                version: Cell::new(0),
                progress: Cell::new(-1.0),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RowObject {
        const NAME: &'static str = "LeafportRowObject";
        type Type = super::RowObject;
    }

    #[glib::derived_properties]
    impl ObjectImpl for RowObject {}
}

glib::wrapper! {
    /// One list entry: the latest `Row` plus its sync progress.
    pub struct RowObject(ObjectSubclass<imp::RowObject>);
}

impl RowObject {
    /// Wrap `row`.
    pub fn new(row: Row) -> Self {
        let object: Self = glib::Object::new();
        *object.imp().row.borrow_mut() = Some(row);
        object
    }

    /// A clone of the current row.
    pub fn row(&self) -> Row {
        self.imp()
            .row
            .borrow()
            .clone()
            .expect("RowObject is built with a row")
    }

    /// The row's key.
    pub fn key(&self) -> String {
        self.imp()
            .row
            .borrow()
            .as_ref()
            .map(|row| row.key.clone())
            .expect("RowObject is built with a row")
    }

    /// Replace the row and notify `version`. Progress is cleared when the row is no
    /// longer syncing.
    pub fn set_row(&self, row: Row) {
        let syncing = matches!(row.activity, Activity::Syncing { .. });
        *self.imp().row.borrow_mut() = Some(row);
        if !syncing {
            self.set_progress(None);
        }
        let imp = self.imp();
        imp.version.set(imp.version.get().wrapping_add(1));
        self.notify("version");
    }

    /// The sync progress, if known.
    pub fn progress(&self) -> Option<f32> {
        let value = self.imp().progress.get();
        (value >= 0.0).then_some(value as f32)
    }

    /// Set or clear the sync progress.
    pub fn set_progress(&self, progress: Option<f32>) {
        self.set_property("progress", progress.map_or(-1.0, f64::from));
    }
}

/// The rows as a list model, keeping one `RowObject` per key.
pub struct RowStore {
    model: gio::ListStore,
    /// The listed objects by key, so lookups do not walk the model.
    index: RefCell<HashMap<String, RowObject>>,
}

impl Default for RowStore {
    fn default() -> Self {
        Self::new()
    }
}

impl RowStore {
    /// An empty store.
    pub fn new() -> Self {
        Self {
            model: gio::ListStore::new::<RowObject>(),
            index: RefCell::default(),
        }
    }

    /// The model to hand to a list view.
    pub fn model(&self) -> gio::ListStore {
        self.model.clone()
    }

    /// Replace the list. Keeps the `RowObject` of every key that stays and calls
    /// `set_row` on it when its row changed. Stale cover paths (see `stale_cover_paths`)
    /// are forgotten in `covers` before any object changes, so a widget that rebinds
    /// requests the new cover after the old one is gone. Only the range between the
    /// unchanged start and end of the list is spliced, so a refresh that keeps the keys
    /// in place leaves the view, its scroll position and its focus alone.
    pub fn set_rows(&self, rows: Vec<Row>, covers: &Covers) {
        let current = self.objects();
        let mut old = self.index.take();
        let mut stale = Vec::new();
        let mut changed = Vec::new();
        let mut objects = Vec::with_capacity(rows.len());
        for row in rows {
            match old.remove(&row.key) {
                Some(object) => {
                    let before = object.row();
                    if before != row {
                        stale.extend(stale_cover_paths(&before, Some(&row)));
                        changed.push((object.clone(), row));
                    }
                    objects.push(object);
                }
                None => objects.push(RowObject::new(row)),
            }
        }
        for object in old.values() {
            stale.extend(stale_cover_paths(&object.row(), None));
        }
        covers.forget(&stale);
        for (object, row) in changed {
            object.set_row(row);
        }
        *self.index.borrow_mut() = objects
            .iter()
            .map(|object| (object.key(), object.clone()))
            .collect();

        let prefix = current
            .iter()
            .zip(&objects)
            .take_while(|(a, b)| a == b)
            .count();
        let suffix = current[prefix..]
            .iter()
            .rev()
            .zip(objects[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let removed = current.len() - prefix - suffix;
        let added = &objects[prefix..objects.len() - suffix];
        if removed > 0 || !added.is_empty() {
            self.model.splice(prefix as u32, removed as u32, added);
        }
    }

    /// Replace one row; unknown keys are ignored. Stale cover paths are forgotten in
    /// `covers` first, as in `set_rows`.
    pub fn set_row(&self, row: Row, covers: &Covers) {
        let Some(object) = self.find(&row.key) else {
            return;
        };
        let before = object.row();
        if before == row {
            return;
        }
        covers.forget(&stale_cover_paths(&before, Some(&row)));
        object.set_row(row);
    }

    /// Remove one row, forgetting its cover paths in `covers` first.
    pub fn remove(&self, key: &str, covers: &Covers) {
        let Some(position) = (0..self.model.n_items()).find(|&i| {
            self.model
                .item(i)
                .and_downcast::<RowObject>()
                .is_some_and(|object| object.key() == key)
        }) else {
            return;
        };
        if let Some(object) = self.model.item(position).and_downcast::<RowObject>() {
            covers.forget(&stale_cover_paths(&object.row(), None));
        }
        self.index.borrow_mut().remove(key);
        self.model.remove(position);
    }

    /// Record progress of a syncing row; ignored otherwise.
    pub fn progress(&self, key: &str, fraction: f32) {
        if let Some(object) = self.find(key)
            && matches!(object.row().activity, Activity::Syncing { .. })
        {
            object.set_progress(Some(fraction));
        }
    }

    /// The object of `key`, if listed.
    pub fn find(&self, key: &str) -> Option<RowObject> {
        self.index.borrow().get(key).cloned()
    }

    fn objects(&self) -> Vec<RowObject> {
        (0..self.model.n_items())
            .filter_map(|i| self.model.item(i).and_downcast::<RowObject>())
            .collect()
    }
}

/// `old.folder` and `old.cover_path` (and the same of `new`) when `new` is `None` or its
/// `cover_path` or `folder` differs from `old`; empty otherwise.
pub fn stale_cover_paths(old: &Row, new: Option<&Row>) -> Vec<String> {
    let changed =
        new.is_none_or(|new| new.cover_path != old.cover_path || new.folder != old.folder);
    if !changed {
        return Vec::new();
    }
    [Some(old), new]
        .into_iter()
        .flatten()
        .flat_map(|row| [row.folder.clone(), row.cover_path.clone()])
        .flatten()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use leafport_app::RowState;

    fn row(key: &str, activity: Activity) -> Row {
        Row {
            key: key.into(),
            title: key.into(),
            author: None,
            format: String::new(),
            state: RowState::Available,
            expires: None,
            cover_path: None,
            folder: None,
            publication: None,
            error: None,
            warning: None,
            on_loan: false,
            activity,
            primary: None,
            menu: Vec::new(),
        }
    }

    fn keys(store: &RowStore) -> Vec<String> {
        let model = store.model();
        (0..model.n_items())
            .map(|i| model.item(i).and_downcast::<RowObject>().unwrap().key())
            .collect()
    }

    #[test]
    fn set_rows_keeps_objects_of_kept_keys() {
        let store = RowStore::new();
        store.set_rows(
            vec![row("a", Activity::Idle), row("b", Activity::Idle)],
            &Covers::new(),
        );
        let obj = store.find("a").unwrap();
        store.set_rows(
            vec![row("b", Activity::Idle), row("a", Activity::Idle)],
            &Covers::new(),
        );
        assert_eq!(store.find("a").unwrap(), obj);
        assert_eq!(keys(&store), ["b", "a"]);
    }

    /// Records every `items-changed` as (position, removed, added).
    fn record_changes(store: &RowStore) -> std::rc::Rc<std::cell::RefCell<Vec<(u32, u32, u32)>>> {
        let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = changes.clone();
        store
            .model()
            .connect_items_changed(move |_, position, removed, added| {
                seen.borrow_mut().push((position, removed, added));
            });
        changes
    }

    #[test]
    fn set_rows_with_same_keys_changes_no_items() {
        let store = RowStore::new();
        store.set_rows(
            vec![row("a", Activity::Idle), row("b", Activity::Idle)],
            &Covers::new(),
        );
        let changes = record_changes(&store);
        let mut renamed = row("b", Activity::Idle);
        renamed.title = "renamed".into();
        store.set_rows(vec![row("a", Activity::Idle), renamed], &Covers::new());
        assert!(changes.borrow().is_empty());
        assert_eq!(store.find("b").unwrap().row().title, "renamed");
    }

    #[test]
    fn set_rows_splices_only_the_changed_range() {
        let store = RowStore::new();
        let rows = |keys: &[&str]| keys.iter().map(|k| row(k, Activity::Idle)).collect();
        store.set_rows(rows(&["a", "b", "c", "d"]), &Covers::new());
        let changes = record_changes(&store);
        // "b" leaves and "x" arrives between kept "a" and "c", "d".
        store.set_rows(rows(&["a", "x", "c", "d"]), &Covers::new());
        assert_eq!(*changes.borrow(), [(1, 1, 1)]);
        assert_eq!(keys(&store), ["a", "x", "c", "d"]);
        // Appending touches only the tail.
        changes.borrow_mut().clear();
        store.set_rows(rows(&["a", "x", "c", "d", "e"]), &Covers::new());
        assert_eq!(*changes.borrow(), [(4, 0, 1)]);
    }

    #[test]
    fn find_follows_set_rows_and_remove() {
        let store = RowStore::new();
        let covers = Covers::new();
        store.set_rows(
            vec![row("a", Activity::Idle), row("b", Activity::Idle)],
            &covers,
        );
        store.remove("a", &covers);
        assert!(store.find("a").is_none());
        store.set_rows(vec![row("c", Activity::Idle)], &covers);
        assert!(store.find("b").is_none());
        assert_eq!(store.find("c").unwrap().key(), "c");
    }

    #[test]
    fn set_rows_drops_left_rows() {
        let store = RowStore::new();
        store.set_rows(
            vec![row("a", Activity::Idle), row("b", Activity::Idle)],
            &Covers::new(),
        );
        store.set_rows(vec![row("b", Activity::Idle)], &Covers::new());
        assert!(store.find("a").is_none());
        assert_eq!(store.model().n_items(), 1);
    }

    #[test]
    fn set_rows_clears_progress_of_rows_that_stop_syncing() {
        let store = RowStore::new();
        let syncing = Activity::Syncing { cancellable: true };
        store.set_rows(vec![row("a", syncing)], &Covers::new());
        store.progress("a", 0.5);
        assert_eq!(store.find("a").unwrap().progress(), Some(0.5));
        store.set_rows(vec![row("a", Activity::Idle)], &Covers::new());
        assert_eq!(store.find("a").unwrap().progress(), None);
    }

    #[test]
    fn set_rows_keeps_progress_of_rows_still_syncing() {
        let store = RowStore::new();
        let syncing = Activity::Syncing { cancellable: true };
        store.set_rows(vec![row("a", syncing.clone())], &Covers::new());
        store.progress("a", 0.5);
        store.set_rows(vec![row("a", syncing)], &Covers::new());
        assert_eq!(store.find("a").unwrap().progress(), Some(0.5));
    }

    #[test]
    fn progress_ignores_unknown_and_idle_rows() {
        let store = RowStore::new();
        store.progress("x", 0.3);
        store.set_rows(vec![row("a", Activity::Idle)], &Covers::new());
        store.progress("a", 0.3);
        assert_eq!(store.find("a").unwrap().progress(), None);
    }

    #[test]
    fn set_row_ignores_unknown_key() {
        let store = RowStore::new();
        store.set_rows(vec![row("a", Activity::Idle)], &Covers::new());
        store.set_row(row("b", Activity::Idle), &Covers::new());
        assert_eq!(store.model().n_items(), 1);
        assert!(store.find("b").is_none());
    }

    #[test]
    fn set_row_bumps_version() {
        let store = RowStore::new();
        store.set_rows(vec![row("a", Activity::Idle)], &Covers::new());
        let obj = store.find("a").unwrap();
        let count = std::rc::Rc::new(std::cell::Cell::new(0));
        let seen = count.clone();
        obj.connect_notify_local(Some("version"), move |_, _| seen.set(seen.get() + 1));
        let mut changed = row("a", Activity::Idle);
        changed.title = "changed".into();
        store.set_row(changed, &Covers::new());
        assert_eq!(count.get(), 1);
    }

    /// A widget's `notify::version` handler requests the row's cover, as `RowWidget`
    /// does; the request must survive the update that triggered it.
    #[test]
    fn new_cover_stays_requested_after_update() {
        // The spawned decodes never run: this context is not iterated.
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let covers = Covers::new();
                let store = RowStore::new();
                store.set_rows(vec![row("a", Activity::Idle)], &covers);
                let object = store.find("a").unwrap();
                let requests = covers.clone();
                object.connect_notify_local(Some("version"), move |object, _| {
                    if let Some(path) = object.row().cover_path {
                        requests.get(&path);
                    }
                });

                // First sync: the cover appears.
                let mut synced = row("a", Activity::Idle);
                synced.folder = Some("/b/t".into());
                synced.cover_path = Some("/b/t/c.jpg".into());
                store.set_rows(vec![synced.clone()], &covers);
                assert_eq!(covers.keys(), ["/b/t/c.jpg"]);

                // The books folder changes.
                let mut moved = synced.clone();
                moved.folder = Some("/n/t".into());
                moved.cover_path = Some("/n/t/c.jpg".into());
                store.set_row(moved, &covers);
                assert_eq!(covers.keys(), ["/n/t/c.jpg"]);
            })
            .unwrap();
    }

    #[test]
    fn remove_forgets_cover() {
        let covers = Covers::new();
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let store = RowStore::new();
                let mut covered = row("a", Activity::Idle);
                covered.folder = Some("/b/t".into());
                covered.cover_path = Some("/b/t/c.jpg".into());
                store.set_rows(vec![covered], &covers);
                covers.get("/b/t/c.jpg");
                store.remove("a", &covers);
                assert!(covers.keys().is_empty());
                assert!(store.find("a").is_none());
            })
            .unwrap();
    }

    #[test]
    fn stale_cover_paths_on_change_and_removal() {
        let mut old = row("a", Activity::Idle);
        old.folder = Some("/f".into());
        old.cover_path = Some("/f/c.jpg".into());
        assert!(stale_cover_paths(&old, Some(&old.clone())).is_empty());

        let mut new = old.clone();
        new.folder = Some("/g".into());
        new.cover_path = Some("/g/c.jpg".into());
        assert_eq!(
            stale_cover_paths(&old, Some(&new)),
            ["/f", "/f/c.jpg", "/g", "/g/c.jpg"]
        );
        assert_eq!(stale_cover_paths(&old, None), ["/f", "/f/c.jpg"]);
    }
}
