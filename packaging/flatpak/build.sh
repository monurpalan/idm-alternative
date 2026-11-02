#!/bin/sh
# Builds Shardload's Flatpak bundle from a .deb, on Ubuntu 24.04 (as CI does):
#
#   packaging/flatpak/build.sh target/release/bundle/deb/Shardload_*.deb out/Shardload.flatpak
#
# Needs flatpak and flatpak-builder; the GNOME runtime and SDK come from Flathub.
set -eu
deb=$(realpath "$1")
out=$(realpath -m "$2")
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cp "$here"/io.github.monurpalan.Shardload.* "$work"/
cp "$deb" "$work/shardload.deb"
# The tray icon's library and what it loads, which the GNOME runtime does not have.
mkdir -p "$work/indicator" "$work/debs"
(cd "$work/debs" && apt-get download libayatana-appindicator3-1 libayatana-indicator3-7 \
    libayatana-ido3-0.4-0 libdbusmenu-glib4 libdbusmenu-gtk3-4)
for d in "$work"/debs/*.deb; do dpkg-deb -x "$d" "$work/debs/root"; done
cp -a "$work"/debs/root/usr/lib/x86_64-linux-gnu/*.so.* "$work/indicator/"

flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
cd "$work"
flatpak-builder --user --install-deps-from=flathub --disable-rofiles-fuse --force-clean \
    --repo=repo build io.github.monurpalan.Shardload.yml
mkdir -p "$(dirname "$out")"
flatpak build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo \
    repo "$out" io.github.monurpalan.Shardload
echo "built $out"
