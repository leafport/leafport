#!/bin/sh
# Changes Leafport's version, the one under [workspace.package] in the root Cargo.toml.
#
#   dist/version.sh release    X.Y.Z-dev -> X.Y.Z
#   dist/version.sh prepare    X.Y.Z     -> X.(Y+1).0-dev
#   dist/version.sh set V      any       -> V exactly as given, X.Y.Z or X.Y.Z-dev
#
# Updates Cargo.toml and the workspace crates in Cargo.lock. Switching to a release version (no
# -dev) also adds a release entry dated today to the AppStream metadata, and moves the AUR package
# (dist/aur, pkgrel 1) to that release; between releases it stays at the last one. Commit the
# result yourself.
set -eu

usage() {
    sed -n '4,6s/^#   /usage: /p' "$0" >&2
    exit 2
}

root="$(cd "$(dirname "$0")/.." && pwd)"
manifest="$root/Cargo.toml"
metainfo="$root/crates/leafport-desktop/data/linux/dev.leafport.Leafport.metainfo.xml"
pkgbuild="$root/dist/aur/PKGBUILD"
srcinfo="$root/dist/aur/.SRCINFO"
version_pattern='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-dev)?$'

current="$(awk '
    /^\[/ { section = $0 }
    section == "[workspace.package]" && /^version[ \t]*=/ {
        sub(/^version[ \t]*=[ \t]*"/, ""); sub(/".*$/, ""); print; exit
    }
' "$manifest")"
if ! printf '%s\n' "$current" | grep -Eq "$version_pattern"; then
    echo "Can't read the version under [workspace.package] in $manifest: '$current'" >&2
    exit 1
fi
release="${current%-dev}"
major="${release%%.*}"
minor="${release#*.}"
minor="${minor%%.*}"

case "${1:-}" in
release)
    [ $# -eq 1 ] || usage
    if [ "$current" = "$release" ]; then
        echo "$current is already a release version" >&2
        exit 1
    fi
    new="$release"
    ;;
prepare)
    [ $# -eq 1 ] || usage
    if [ "$current" != "$release" ]; then
        echo "$current is already a development version" >&2
        exit 1
    fi
    new="$major.$((minor + 1)).0-dev"
    ;;
set)
    [ $# -eq 2 ] || usage
    new="$2"
    if ! printf '%s\n' "$new" | grep -Eq "$version_pattern"; then
        echo "'$new' is not X.Y.Z or X.Y.Z-dev" >&2
        exit 1
    fi
    ;;
*)
    usage
    ;;
esac

case "$new" in
*-dev) ;;
*)
    if grep -q "<release version=\"$new\"" "$metainfo"; then
        echo "$metainfo already lists release $new" >&2
        exit 1
    fi
    ;;
esac

tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT

awk -v version="$new" '
    /^\[/ { section = $0 }
    section == "[workspace.package]" && /^version[ \t]*=/ && !done {
        print "version = \"" version "\""
        done = 1
        next
    }
    { print }
' "$manifest" > "$tmp"
cat "$tmp" > "$manifest"

cargo update --workspace --manifest-path "$manifest"

case "$new" in
*-dev) ;;
*)
    entry="    <release version=\"$new\" date=\"$(date +%Y-%m-%d)\"/>"
    # Newest release first; create the <releases> element before </component> if missing.
    awk -v entry="$entry" '
        /<releases>/ { print; print entry; done = 1; next }
        /<\/component>/ && !done { print "  <releases>"; print entry; print "  </releases>" }
        { print }
    ' "$metainfo" > "$tmp"
    cat "$tmp" > "$metainfo"

    # The AUR package builds the release's tag, so the version is all that changes.
    old="$(sed -n 's/^pkgver=//p' "$pkgbuild")"
    sed -e "s/^pkgver=.*/pkgver=$new/" -e 's/^pkgrel=.*/pkgrel=1/' "$pkgbuild" > "$tmp"
    cat "$tmp" > "$pkgbuild"
    sed -e "s/^\tpkgver = .*/\tpkgver = $new/" -e 's/^\tpkgrel = .*/\tpkgrel = 1/' \
        -e "/^\tsource = /s/#tag=v$old\$/#tag=v$new/" "$srcinfo" > "$tmp"
    cat "$tmp" > "$srcinfo"
    ;;
esac

echo "Leafport: $current -> $new. Review and commit the changes."
