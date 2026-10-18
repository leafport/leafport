# Leafport design

## Goal

Leafport keeps the ebooks you buy or borrow through Adobe-protected stores and libraries in sync on Linux, macOS, and Windows. Adobe makes no client for Linux, and a book bought there cannot be read without one. Leafport signs in with an Adobe ID or a library provider, fulfills ACSM files, syncs the account's cloud library, and keeps every book as a readable file in a local books folder.

## Scope

- Sign-in providers are loaded from the authentication service, with Adobe ID built in. Providers are never hardcoded into one backend.
- One activation per device. It is kept in the operating system's credential store, never in a plain file.
- Fulfillment: parse ACSM files, request the book from the store or library named in it, download it, and record loans so they can be returned.
- Cloud sync in both directions: download the account's ACSM list, and upload newly imported ACSMs so other devices see them.
- A local library: one folder per title, with the book file, its ACSM, and metadata, so syncing again skips what is already there.
- Readable books: decrypt EPUB and PDF books with the device's license key so any reader opens them.
- Desktop front ends for Linux, macOS, and Windows.

## Architecture

A Rust workspace:

- `leafport-core`: platform-independent protocol and library code. Provider discovery, request signing, activation, fulfillment, cloud sync, decryption, and the credential-store adapter.
- `leafport-cli`: a small command-line harness for the core, for inspecting providers, service info, and ACSM files.
- `leafport-desktop`: the desktop front end. It shows the account, the library, and sync progress, and dispatches user actions to the core.

Platform adapters provide credentials, data directories, and file opening. Network access sits behind the core's service client so protocol code can be tested against recorded responses.

## Delivery order

1. Provider discovery and the CLI harness.
2. Request signing.
3. Cloud library requests.
4. Sign-in and activation, stored in the OS credential store.
5. Fulfillment and loan return.
6. EPUB and PDF decryption.
7. The desktop app on top of the core.
8. Packaging and verification on Linux, macOS, and Windows.

## Errors and persistence

Show service error codes with a readable description, and keep enough context to tell authentication, service, transfer, parsing, and local storage failures apart. Never report fulfillment or sync as successful only because an HTTP request succeeded. Write book files and operation state through staged writes and atomic renames. Never write credentials into fixtures or logs.

## Verification

- Byte-level tests for every request and response format, separate from state-machine tests.
- Live service runs only with the user's own account and books.
- Build and run checks on each platform before a release.
