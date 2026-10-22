#!/usr/bin/env bash
# Installs the Sketch desktop entry and app icon into the user's home, so the launcher and the
# task manager show the app with its icon. The window's app id is "sketch-rs", which is what ties
# it to the entry, so the icon shows for any build of the app.
#
# Usage: scripts/install-linux-desktop.sh [path-to-sketch-rs]
#
# Without an argument the script takes the binary from the PATH, then from the cargo target
# directory.
set -euo pipefail

cd "$(dirname "$0")/.."

bin=${1:-}
if [ -z "$bin" ]; then
    for candidate in "$(command -v sketch-rs || true)" target/release/sketch-rs target/debug/sketch-rs; do
        if [ -n "$candidate" ] && [ -x "$candidate" ]; then
            bin=$candidate
            break
        fi
    done
fi

if [ -z "$bin" ]; then
    echo "no sketch-rs binary found, pass its path as the first argument" >&2
    exit 1
fi
if [ ! -f "$bin" ]; then
    echo "$bin is not a file" >&2
    exit 1
fi
bin=$(readlink -f "$bin")

data=${XDG_DATA_HOME:-$HOME/.local/share}
apps=$data/applications
icons=$data/icons/hicolor

install -Dm644 packaging/linux/sketch-rs.desktop "$apps/sketch-rs.desktop"
# Point the entry at this build.
sed -i "s|^Exec=.*|Exec=$bin %f|" "$apps/sketch-rs.desktop"

for dir in packaging/linux/icons/hicolor/*/apps; do
    size=$(basename "$(dirname "$dir")")
    install -Dm644 "$dir"/* "$icons/$size/apps/"
done

# Refresh the caches the desktop shell reads.
if command -v update-desktop-database >/dev/null; then
    update-desktop-database "$apps"
fi
if command -v gtk-update-icon-cache >/dev/null; then
    gtk-update-icon-cache -f -t "$data/icons" || true
fi
if command -v kbuildsycoca6 >/dev/null; then
    kbuildsycoca6 --noincremental
fi

echo "installed $apps/sketch-rs.desktop"
echo "installed the icons under $icons"
echo "the launcher entry runs $bin"
