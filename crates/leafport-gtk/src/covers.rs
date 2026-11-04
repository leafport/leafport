//! Cover thumbnails: decoded off the main thread, cached by path.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::rc::{Rc, Weak};

use gtk::{gdk, gio, glib};

type ReadyFn = Box<dyn Fn(&str)>;

/// One cached path: the decode that fills it and its texture, `None` while loading or
/// when unreadable.
struct Slot {
    generation: u64,
    texture: Option<gdk::Texture>,
}

/// Cover textures by file path. A path is decoded once until it is forgotten.
pub struct Covers {
    textures: RefCell<HashMap<String, Slot>>,
    /// The generation of the next decode.
    next: Cell<u64>,
    ready: RefCell<Vec<ReadyFn>>,
    this: Weak<Covers>,
}

impl Covers {
    /// An empty cache.
    pub fn new() -> Rc<Covers> {
        Rc::new_cyclic(|this| Covers {
            textures: RefCell::default(),
            next: Cell::new(0),
            ready: RefCell::default(),
            this: this.clone(),
        })
    }

    /// The cached texture of `path`, or `None`. A path never seen is decoded on a
    /// worker thread; once it is there, the `connect_ready` handlers get `path`.
    pub fn get(&self, path: &str) -> Option<gdk::Texture> {
        if let Some(slot) = self.textures.borrow().get(path) {
            return slot.texture.clone();
        }
        let generation = self.next.get();
        self.next.set(generation.wrapping_add(1));
        self.textures.borrow_mut().insert(
            path.to_owned(),
            Slot {
                generation,
                texture: None,
            },
        );
        let this = self.this.clone();
        let path = path.to_owned();
        glib::spawn_future_local(async move {
            let file = path.clone();
            let texture = gio::spawn_blocking(move || decode(&file))
                .await
                .ok()
                .flatten();
            let (Some(this), Some(texture)) = (this.upgrade(), texture) else {
                return;
            };
            if this.fill(&path, generation, texture) {
                for ready in this.ready.borrow().iter() {
                    ready(&path);
                }
            }
        });
        None
    }

    /// Store the texture of decode `generation` of `path`. Returns false, storing
    /// nothing, when `path` was forgotten since: a later `get` started a new decode, so
    /// this one may have read an older file.
    fn fill(&self, path: &str, generation: u64, texture: gdk::Texture) -> bool {
        match self.textures.borrow_mut().get_mut(path) {
            Some(slot) if slot.generation == generation && slot.texture.is_none() => {
                slot.texture = Some(texture);
                true
            }
            _ => false,
        }
    }

    /// Drop every cached path equal to or inside one of `paths`. The next `get` of such a
    /// path decodes it again; a decode still running for it is discarded.
    pub fn forget(&self, paths: &[String]) {
        self.textures
            .borrow_mut()
            .retain(|cached, _| !paths.iter().any(|path| Path::new(cached).starts_with(path)));
    }

    /// Call `f` with each path whose texture became available.
    pub fn connect_ready(&self, f: impl Fn(&str) + 'static) {
        self.ready.borrow_mut().push(Box::new(f));
    }

    #[cfg(test)]
    fn insert_pending(&self, path: &str) {
        self.textures.borrow_mut().insert(
            path.to_owned(),
            Slot {
                generation: u64::MAX,
                texture: None,
            },
        );
    }

    #[cfg(test)]
    fn generation(&self, path: &str) -> Option<u64> {
        self.textures.borrow().get(path).map(|slot| slot.generation)
    }

    /// The cached paths, sorted.
    #[cfg(test)]
    pub(crate) fn keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.textures.borrow().keys().cloned().collect();
        keys.sort();
        keys
    }
}

/// Read `path` and scale it to fit 96×128. `None` when it cannot be read or decoded.
fn decode(path: &str) -> Option<gdk::Texture> {
    let decode = || -> Option<gdk::Texture> {
        let bytes = std::fs::read(path).ok()?;
        let rgba = image::load_from_memory(&bytes)
            .ok()?
            .thumbnail(96, 128)
            .to_rgba8();
        let (width, height) = rgba.dimensions();
        let texture = gdk::MemoryTexture::new(
            width as i32,
            height as i32,
            gdk::MemoryFormat::R8g8b8a8,
            &glib::Bytes::from_owned(rgba.into_raw()),
            width as usize * 4,
        );
        Some(texture.into())
    };
    // A panicking decoder counts as unreadable, like a failed decode.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(decode))
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forget_drops_paths_inside_folder() {
        let covers = Covers::new();
        covers.insert_pending("/b/t1/cover.jpg");
        covers.insert_pending("/b/t2/cover.jpg");
        covers.forget(&["/b/t1".to_owned()]);
        assert_eq!(covers.keys(), ["/b/t2/cover.jpg"]);
    }

    fn texture() -> gdk::Texture {
        gdk::MemoryTexture::new(
            1,
            1,
            gdk::MemoryFormat::R8g8b8a8,
            &glib::Bytes::from_static(&[0, 0, 0, 255]),
            4,
        )
        .into()
    }

    #[test]
    fn decode_started_before_forget_does_not_fill_new_request() {
        // The spawned decodes never run: this context is not iterated.
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let covers = Covers::new();
                let path = "/b/t/cover.jpg";
                assert!(covers.get(path).is_none());
                let stale = covers.generation(path).unwrap();
                covers.forget(&["/b/t".to_owned()]);
                assert!(covers.get(path).is_none());
                let current = covers.generation(path).unwrap();
                assert!(!covers.fill(path, stale, texture()));
                assert!(covers.get(path).is_none());
                assert!(covers.fill(path, current, texture()));
                assert!(covers.get(path).is_some());
            })
            .unwrap();
    }
}
