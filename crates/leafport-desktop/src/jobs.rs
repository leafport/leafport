//! Worker threads, their completion events, and the one-at-a-time sync queue.

use eframe::egui;
use leafport_core::{
    cloud::DownloadRecord,
    library::{self, SyncOutcome},
    service::ServiceClient,
    store::{ActivationState, DEFAULT_ACCOUNT, KeyringStore},
    vendors::VendorCatalog,
};
use std::{path::PathBuf, sync::mpsc::Sender};

use crate::App;

// Events are sent once per completed job, so the large `Synced` variant costs nothing.
#[allow(clippy::large_enum_variant)]
pub enum Event {
    Vendors(Result<VendorCatalog, String>),
    /// A completed sign-in: the activated account label, or an error message.
    SignIn(Result<String, String>),
    /// A completed list refresh: the cloud records, or an error message. `generation` is
    /// the account generation the refresh was started under.
    Refreshed {
        generation: u64,
        result: Result<Vec<DownloadRecord>, String>,
    },
    /// A completed sync of the row with `key`.
    Synced {
        key: String,
        result: Result<SyncOutcome, String>,
    },
    /// A decoded cover thumbnail, or None when it could not be read.
    Cover {
        path: PathBuf,
        image: Option<egui::ColorImage>,
    },
}

/// Run a worker body, converting a panic into an Err so the completion event is always sent.
pub fn catch_panic<T>(body: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).unwrap_or_else(|payload| {
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".into());
        Err(format!("internal error: {message}"))
    })
}

/// Run `body` on a worker thread and send its result, wrapped by `event`.
fn spawn<T: Send + 'static>(
    tx: &Sender<Event>,
    ctx: &egui::Context,
    body: impl FnOnce() -> Result<T, String> + Send + 'static,
    event: impl FnOnce(Result<T, String>) -> Event + Send + 'static,
) {
    let tx = tx.clone();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let _ = tx.send(event(catch_panic(body)));
        ctx.request_repaint();
    });
}

fn load_activation() -> Result<ActivationState, String> {
    ActivationState::load(&KeyringStore::new(), DEFAULT_ACCOUNT)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "not signed in".to_string())
}

/// Fetch the cloud list and store it in the cache.
pub fn refresh(tx: &Sender<Event>, ctx: &egui::Context, cache: Option<PathBuf>, generation: u64) {
    spawn(
        tx,
        ctx,
        move || {
            let cache = cache.ok_or_else(|| "no data directory".to_string())?;
            let activation = load_activation()?;
            let client = ServiceClient::new().map_err(|e| e.to_string())?;
            library::refresh(&cache, &activation, &client).map_err(|e| e.to_string())
        },
        move |result| Event::Refreshed { generation, result },
    );
}

/// Decode a cover file and scale it to fit 96×128.
pub fn cover(tx: &Sender<Event>, ctx: &egui::Context, path: PathBuf) {
    let file = path.clone();
    spawn(
        tx,
        ctx,
        move || {
            let bytes = std::fs::read(&file).map_err(|e| e.to_string())?;
            let rgba = image::load_from_memory(&bytes)
                .map_err(|e| e.to_string())?
                .thumbnail(96, 128)
                .to_rgba8();
            let size = [rgba.width() as usize, rgba.height() as usize];
            Ok(egui::ColorImage::from_rgba_unmultiplied(size, &rgba))
        },
        move |result| Event::Cover {
            path,
            image: result.ok(),
        },
    );
}

impl App {
    /// Queue a sync of the row with `key`, unless it is already queued or syncing.
    pub fn enqueue(&mut self, key: String, ctx: &egui::Context) {
        if self.syncing.contains(&key) || self.queue.contains(&key) {
            return;
        }
        self.queue.push_back(key);
        self.start_next_sync(ctx);
    }

    /// Start the front queued sync when no sync worker runs.
    pub fn start_next_sync(&mut self, ctx: &egui::Context) {
        if !self.syncing.is_empty() {
            return;
        }
        let Some(key) = self.queue.pop_front() else {
            return;
        };
        let acsm = self
            .rows
            .iter()
            .find(|row| row.key == key)
            .and_then(|row| row.acsm.clone());
        let library = self.library.clone();
        self.syncing.insert(key.clone());
        spawn(
            &self.tx,
            ctx,
            move || {
                let acsm = acsm.ok_or_else(|| "no ACSM for this item".to_string())?;
                if library.root.as_os_str().is_empty() {
                    return Err("no books folder configured".into());
                }
                let activation = load_activation()?;
                let client = ServiceClient::new().map_err(|e| e.to_string())?;
                library
                    .sync_item(&acsm, &activation, &client)
                    .map_err(|e| e.to_string())
            },
            move |result| Event::Synced { key, result },
        );
    }
}
