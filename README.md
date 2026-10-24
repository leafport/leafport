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
# Linux (GTK 4 and libadwaita)
cargo run -p leafport-gtk
# Optional: import an ACSM file on launch
cargo run -p leafport-gtk -- /path/to/file.acsm
```

On Linux the app is `leafport-gtk`, a GTK4 and libadwaita front end. Building it needs a C toolchain and the GTK 4.10 and libadwaita 1.5 development packages (`gtk4` and `libadwaita` on Arch, `libgtk-4-dev` and `libadwaita-1-dev` on Debian and Ubuntu).

`leafport-desktop` is the egui front end. It builds on macOS and Windows only, and is their UI until native apps exist. On Linux it prints a pointer to `leafport-gtk` and exits. The macOS and Windows builds have not been built or run.

Both front ends show the library list from your account, with a cover, sync status and a button per book. They sign in and out, sync one book or all of them, cancel a queued or running sync, return a loan, and remove a downloaded book from disk. They import an ACSM file through a button or from the launch argument; on Linux, a second launch with an ACSM file hands it to the running window. The settings set the theme and the books folder.

## Roadmap

- **Releases:** packages for Linux, macOS and Windows, each built and run on its platform before a release.
- **macOS and Windows builds:** the egui app has not been built or run on either platform yet.
- **Live verification:** verify fulfillment with more stores and libraries.
