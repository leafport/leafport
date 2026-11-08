# Distribution

| Package | Built by | Where |
| --- | --- | --- |
| Arch Linux (AUR) | `aur/PKGBUILD`, from the release's source tarball | the `leafport` AUR package |
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

## Releasing

1. Make the release commit: drop `-dev` from the version, run `cargo update --workspace` to update
   `Cargo.lock`, and add a `<releases>` entry for the version and date to
   `crates/leafport-desktop/data/linux/dev.leafport.Leafport.metainfo.xml`. Commit and push.
2. In the Actions tab, run the Release workflow on `main`. It refuses a `-dev` version or one that
   already has a release or tag, runs the CI checks, builds the installers and drafts a release
   with them. The installers are also attached to the run as artifacts.
3. Review the draft in the Releases tab and publish it. Publishing creates the `v<version>` tag.
4. Start the next version: set it to the next `X.Y.Z-dev`, run `cargo update --workspace`, commit and
   push.
5. Update the AUR package from `aur/`:

   ```sh
   cd dist/aur
   # set pkgver (pkgrel=1), then:
   updpkgsums
   makepkg --printsrcinfo > .SRCINFO
   makepkg -f   # test build
   ```

   Commit the change here, then copy `PKGBUILD` and `.SRCINFO` into a clone of
   `ssh://aur@aur.archlinux.org/leafport.git` and push it.

## Icons

`crates/leafport-desktop/data/icons/dev.leafport.Leafport.svg` is the source. After changing it,
run `render-icons.sh` to update the PNG and ICO files.
