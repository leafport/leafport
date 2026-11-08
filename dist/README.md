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

## Releasing

1. Set the version in the root `Cargo.toml` (`[workspace.package]`) and add a `<release>` entry to
   `crates/leafport-desktop/data/linux/dev.leafport.Leafport.metainfo.xml`. Commit.
2. Tag and push: `git tag v<version> && git push origin main v<version>`.
3. The Release workflow builds the installers and drafts a GitHub release with them. Review the
   draft and publish it.
4. Update the AUR package from `aur/`:

   ```sh
   cd dist/aur
   # set pkgver (pkgrel=1), then:
   updpkgsums
   makepkg --printsrcinfo > .SRCINFO
   makepkg -f   # test build
   ```

   Commit the change here, then copy `PKGBUILD` and `.SRCINFO` into a clone of
   `ssh://aur@aur.archlinux.org/leafport.git` and push it.

To build the installers without releasing, run the Release workflow by hand from the Actions tab.
The installers are attached to the run as artifacts.

## Icons

`crates/leafport-desktop/data/icons/dev.leafport.Leafport.svg` is the source. After changing it,
run `render-icons.sh` to update the PNG and ICO files.
