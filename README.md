# Leafport

Leafport syncs and reads the ebooks you buy or borrow through Adobe-protected stores and libraries, on Linux, macOS, and Windows. It covers Adobe ID sign-in, ACSM fulfillment, cloud sync, a local library, and a reader.

The diagnostic harness loads the live vendor list, or reads a local response:

```sh
cargo run -p leafport-cli -- vendors
cargo run -p leafport-cli -- vendors crates/leafport-core/tests/data/authentication-service-info.xml
```

The live command successfully loaded 42 choices on 2026-10-17, including Adobe ID, CloudLibrary, OverDrive, Google, and PocketBook. This verifies discovery only, not vendor login, fulfillment, or sync. The fixture is synthetic. The design is in `SPEC.md`.
