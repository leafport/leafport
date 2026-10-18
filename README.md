# Leafport

Leafport syncs and reads the ebooks you buy or borrow through Adobe-protected stores and libraries, on Linux, macOS, and Windows. It covers Adobe ID sign-in, ACSM fulfillment, cloud sync, a local library, and a reader.

This is early implementation. The core loads the sign-in providers, including distinct vendor IDs and the built-in Adobe ID choice. Activation, fulfillment, and cloud sync are in development. A desktop shell provides EPUB text reading and live provider discovery.

The diagnostic harness loads the live vendor list, or reads a local response:

```sh
cargo run -p leafport-cli -- vendors
cargo run -p leafport-cli -- vendors crates/leafport-core/tests/data/authentication-service-info.xml
```

The live command successfully loaded 42 choices on 2026-10-17, including Adobe ID, CloudLibrary, OverDrive, Google, and PocketBook. This verifies discovery only, not vendor login, fulfillment, or sync. The fixture is synthetic.

## Native desktop

```sh
cargo run -p leafport-desktop
# Optional: open a local EPUB on launch
cargo run -p leafport-desktop -- /path/to/book.epub
```

The eframe/egui frontend uses native windows. Use **Import EPUB…**, drag a file into the window, or pass a path on launch. The library remembers links to original files using the platform application-data directory; it does not copy books. Removing an entry leaves the original file intact. EPUB metadata, spine order, EPUB 2/3 navigation labels, selectable text, chapter controls, and text size are supported. The file dialog uses platform-native services (an XDG desktop portal on Linux).

The **Providers** window loads the real service asynchronously and displays each choice's method and node ID. A failed refresh preserves existing choices and reports the error. There is no simulated login, fulfillment, or sync action.

Reader limitations: text only, no images/CSS layout, PDF, annotations, bookmarks, or protected-book rendering. EPUBs declaring rights/encryption metadata are conservatively marked unreadable, including font-obfuscated publications. Library links persist; reading position does not yet persist. Linux builds need a C toolchain and working Wayland/X11/OpenGL desktop. macOS and Windows support is intended through portable dependencies but has not been built or run on those platforms.

## Roadmap

- **Sign-in:** sign in with an Adobe ID or a library provider from the app, and keep the activation in the operating system's credential store.
- **Cloud sync:** download the account's ACSM list and fulfill each book, and upload newly imported ACSMs so other devices see them.
- **Books folder:** one folder per title with the book, its ACSM and its metadata, so syncing again skips what is already there.
- **Readable books:** save decrypted EPUB and PDF files that any reader opens.
- **Loans:** record loans so they can be returned early.
- **Clear errors:** describe service error codes in plain words, and count a sync as done only once its book is written.
- **Releases:** packages for Linux, macOS and Windows, each built and run on its platform before a release.
