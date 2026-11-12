#!/bin/sh
# Adds rCAD to your desktop's app menu (for the current user, no root needed).
# Run it from the unpacked release folder: ./install.sh   Undo with: ./install.sh --uninstall
set -e
here=$(cd "$(dirname "$0")" && pwd)
data=${XDG_DATA_HOME:-$HOME/.local/share}
apps="$data/applications"
icons="$data/icons/hicolor"
if [ "$1" = "--uninstall" ]; then
    rm -f "$apps/rcad.desktop" "$icons/scalable/apps/rcad.svg"
    for s in 16 24 32 48 64 128 256 512; do rm -f "$icons/${s}x${s}/apps/rcad.png"; done
    echo "rCAD removed from the app menu"
    exit 0
fi
mkdir -p "$apps" "$icons/scalable/apps"
cp "$here/icon/rcad.svg" "$icons/scalable/apps/rcad.svg"
for s in 16 24 32 48 64 128 256 512; do
    mkdir -p "$icons/${s}x${s}/apps"
    cp "$here/icon/rcad-$s.png" "$icons/${s}x${s}/apps/rcad.png"
done
# point Exec at this copy of the binary
sed "s|^Exec=rcad|Exec=\"$here/rcad\"|" "$here/rcad.desktop" > "$apps/rcad.desktop"
command -v update-desktop-database >/dev/null && update-desktop-database "$apps" 2>/dev/null || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q "$icons" 2>/dev/null || true
echo "rCAD added to the app menu"
