# Leafport

Leafport keeps the ebooks you buy or borrow through Adobe-protected stores and libraries in sync on Linux, macOS, and Windows. Adobe makes no client for Linux, and without one a book bought there can't be opened at all. Leafport signs in to your account, downloads every book in your cloud library to a books folder, and keeps that folder up to date. Each book is saved as a plain EPUB or PDF file that any reader opens.

## Features

- **Sign-in** with an Adobe ID or one of the providers the service offers (libraries, stores). The activation is kept in the operating system's credential store: Secret Service on Linux, Keychain on macOS, and Credential Manager on Windows.
- **Cloud sync.** Leafport lists the books in your account and syncs one or all of them. It shows progress and can cancel queued or running syncs. Books already in the folder are skipped.
- **ACSM import.** Open an `.acsm` file from a store or library, through the menu or as a launch argument. Leafport fulfills it and adds it to the account's cloud library, so other devices see it too.
- **Loans.** A loaned book can be returned early from its row menu.
- **Exclusions.** A book can be excluded from Sync All. Leafport never deletes book folders.
- **Books folder.** Each title gets its own folder, `<Title> [<id>]/`, holding the book, its ACSM, and a `metadata.yaml`. The folder can be changed in the settings.

## Front ends

| Platform | App | Toolkit |
| --- | --- | --- |
| Linux | `leafport-gtk` | GTK 4 and libadwaita, follows the system style |
| macOS, Windows | `leafport-desktop` | egui |

Both share the application logic in `leafport-app` and the protocol and library code in `leafport-core`. `leafport-cli` is a small command-line harness for the core.

## Building

Leafport needs a current stable Rust toolchain.

```sh
# Linux
cargo run -p leafport-gtk
# Import an ACSM file on launch; a second launch hands it to the running window
cargo run -p leafport-gtk -- /path/to/file.acsm

# macOS and Windows
cargo run -p leafport-desktop
```

The GTK app needs a C toolchain and the GTK 4.10 and libadwaita 1.5 development packages: `gtk4` and `libadwaita` on Arch, `libgtk-4-dev` and `libadwaita-1-dev` on Debian and Ubuntu. On Linux, `leafport-desktop` prints a pointer to `leafport-gtk` and exits.

The CLI inspects the service and ACSM files:

```sh
cargo run -p leafport-cli -- vendors
cargo run -p leafport-cli -- service-info
cargo run -p leafport-cli -- acsm-info /path/to/file.acsm
```

## Where things live

| | Linux | macOS | Windows |
| --- | --- | --- | --- |
| Books folder (default) | `~/.local/share/leafport/Books` | `~/Library/Application Support/leafport/Books` | `%APPDATA%\leafport\Books` |
| Settings | `~/.config/leafport/settings.yaml` | `~/Library/Application Support/leafport/settings.yaml` | `%APPDATA%\leafport\settings.yaml` |

Signing in activates this computer as one of your account's devices. The account's device limit applies, so signing out and in again uses another slot.

## Roadmap

- **Distribution:** a Flatpak for Linux, signed app bundles for macOS and Windows, and CI-built releases. macOS and Windows check for updates; the Flatpak gets them from its repository.
- **macOS and Windows builds:** `leafport-desktop` has only been type-checked on Linux so far. Build and test it on both platforms.
- **Live verification:** verify fulfillment with more stores and libraries, including Google Play Books.
- **Several accounts:** the activation is a single record today.
