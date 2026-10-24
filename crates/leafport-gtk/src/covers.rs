//! Cover thumbnails: decoded off the main thread, cached by path.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::{Rc, Weak};

use gtk::{gdk, gio, glib};

type ReadyFn = Box<dyn Fn(&str)>;

/// Cover textures by file path. A path is decoded once; `None` means loading or
/// unreadable.
pub struct Covers {
    textures: RefCell<HashMap<String, Option<gdk::Texture>>>,
    ready: RefCell<Vec<ReadyFn>>,
    this: Weak<Covers>,
}

impl Covers {
    /// An empty cache.
    pub fn new() -> Rc<Covers> {
        Rc::new_cyclic(|this| Covers {
            textures: RefCell::default(),
            ready: RefCell::default(),
            this: this.clone(),
        })
    }

    /// The cached texture of `path`, or `None`. A path never seen is decoded on a
    /// worker thread; once it is there, the `connect_ready` handlers get `path`.
    pub fn get(&self, path: &str) -> Option<gdk::Texture> {
        if let Some(texture) = self.textures.borrow().get(path) {
            return texture.clone();
        }
        self.textures.borrow_mut().insert(path.to_owned(), None);
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
            // A path forgotten while it was decoded stays forgotten; the next `get`
            // reads it again.
            match this.textures.borrow_mut().get_mut(&path) {
                Some(slot @ None) => *slot = Some(texture),
                _ => return,
            }
            for ready in this.ready.borrow().iter() {
                ready(&path);
            }
        });
        None
    }

    /// Drop every cached path equal to or inside one of `paths`, so it is read again.
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
        self.textures.borrow_mut().insert(path.to_owned(), None);
    }

    #[cfg(test)]
    fn keys(&self) -> Vec<String> {
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
}
