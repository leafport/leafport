# Distribution

| Package | Built By | Where |
| --- | --- | --- |
| Arch Linux (AUR) | `aur/PKGBUILD`, from the release's git tag | the `leafport` AUR package |
| Debian and Ubuntu `.deb` | `cargo deb`, configured in `crates/leafport-desktop/Cargo.toml` | GitHub release |
| macOS `.dmg` (Apple silicon, Intel) | `cargo packager`, same file | GitHub release |
| Windows installer (`-setup.exe`) | `cargo packager`, same file | GitHub release |

The `.deb`, `.dmg` and Windows installer ship the pdfium library for PDF cover images. The AUR
package leaves it to the optional `libpdfium` dependency.

The `.deb` needs GTK 4.10 and libadwaita 1.5: Debian 13, Ubuntu 24.04, or newer.

The macOS and Windows builds aren't signed yet. macOS refuses to open the app until it's allowed
under System Settings › Privacy & Security, and Windows SmartScreen warns before the installer runs.

## Versions

The version lives in one place, `version` under `[workspace.package]` in the root `Cargo.toml`.
Every commit's version says whether it is a release:

- `X.Y.Z-dev` is work toward release X.Y.Z. Commits on `main` carry this between releases.
- `X.Y.Z` is release X.Y.Z. Only the commit that releases it carries it.

`version.sh` changes it and everything that follows from it:

| Command | Version |
| --- | --- |
| `dist/version.sh release` | `X.Y.Z-dev` to `X.Y.Z` |
| `dist/version.sh prepare` | `X.Y.Z` to `X.(Y+1).0-dev` |
| `dist/version.sh set V` | any version to `V` exactly as given: `X.Y.Z` is a release, `X.Y.Z-dev` a development version |

It updates `Cargo.toml` and `Cargo.lock`. On a switch to a release version it also adds the release
to the AppStream metadata and moves the AUR package to it. The AUR package can't carry a `-dev`
version, so between releases it stays at the last one.

## Releasing

1. Run `dist/version.sh release`, review the changes, commit and push.
2. In the Actions tab, run the Release workflow on `main`. It refuses a `-dev` version or one that
   already has a release or tag, runs the CI checks, builds the installers and drafts a release
   with them. The installers are also attached to the run as artifacts.
3. Review the draft in the Releases tab and publish it. Publishing creates the `v<version>` tag.
4. Publish the AUR package: copy `aur/PKGBUILD` and `aur/.SRCINFO` into a clone of
   `ssh://aur@aur.archlinux.org/leafport.git`, commit and push. It builds from the release's tag.
5. Run `dist/version.sh prepare`, commit and push.

## Icons

`crates/leafport-desktop/data/icons/dev.leafport.Leafport.svg` is the source. After changing it,
run `render-icons.sh` to update the PNG and ICO files.
