#!/bin/bash
set -e

PREFIX="${PREFIX:-/usr/local}"
if [ ! -w "$PREFIX/bin" ] && [ "$EUID" -ne 0 ]; then
    PREFIX="$HOME/.local"
fi

BIN_DIR="$PREFIX/bin"
DATA_DIR="$PREFIX/share"

echo "Removing Ferrite DM from $BIN_DIR..."
rm -f "$BIN_DIR/fdm-gui" "$BIN_DIR/ferrite-dm" "$BIN_DIR/ferrite-native-host"

# Remove desktop file and icons if present
rm -f "$DATA_DIR/applications/ferrite-dm.desktop"
rm -f "$DATA_DIR/metainfo/io.ferrite_dm.FerriteDM.metainfo.xml"
for size in 16 32 48 64 128 256 512 1024; do
    rm -f "$DATA_DIR/icons/hicolor/${size}x${size}/apps/ferrite-dm.png"
done
rm -f "$DATA_DIR/icons/hicolor/scalable/apps/ferrite-dm.svg"

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$DATA_DIR/applications" 2>/dev/null || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -t -f "$DATA_DIR/icons/hicolor" 2>/dev/null || true
fi

echo "Uninstalled."
