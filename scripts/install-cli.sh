#!/bin/bash
set -e

# Ferrite DM Installer
PREFIX="${PREFIX:-/usr/local}"
if [ -d "$PREFIX/bin" ]; then
    CHECK_DIR="$PREFIX/bin"
elif [ -d "$PREFIX" ]; then
    CHECK_DIR="$PREFIX"
else
    CHECK_DIR="$(dirname "$PREFIX")"
fi

if [ ! -w "$CHECK_DIR" ] && [ "$EUID" -ne 0 ]; then
    PREFIX="$HOME/.local"
fi

BIN_DIR="$PREFIX/bin"
mkdir -p "$BIN_DIR"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." 2>/dev/null && pwd || echo "$SCRIPT_DIR")"

echo "Installing Ferrite DM binaries to $BIN_DIR..."

if [ -f "$SCRIPT_DIR/fdm-gui" ]; then
    install -m 755 "$SCRIPT_DIR/fdm-gui" "$BIN_DIR/fdm-gui" 2>/dev/null || cp "$SCRIPT_DIR/fdm-gui" "$BIN_DIR/fdm-gui"
    chmod +x "$BIN_DIR/fdm-gui" 2>/dev/null || true
fi

if [ -f "$SCRIPT_DIR/ferrite-dm" ]; then
    install -m 755 "$SCRIPT_DIR/ferrite-dm" "$BIN_DIR/ferrite-dm" 2>/dev/null || cp "$SCRIPT_DIR/ferrite-dm" "$BIN_DIR/ferrite-dm"
    chmod +x "$BIN_DIR/ferrite-dm" 2>/dev/null || true
fi

if [ -f "$SCRIPT_DIR/ferrite-native-host" ]; then
    install -m 755 "$SCRIPT_DIR/ferrite-native-host" "$BIN_DIR/ferrite-native-host" 2>/dev/null || cp "$SCRIPT_DIR/ferrite-native-host" "$BIN_DIR/ferrite-native-host"
    chmod +x "$BIN_DIR/ferrite-native-host" 2>/dev/null || true
fi

# Optional Linux desktop integration
RES_DIR=""
for loc in "$SCRIPT_DIR/resources/linux" "$ROOT_DIR/resources/linux" "$SCRIPT_DIR"; do
    if [ -f "$loc/ferrite-dm.desktop" ]; then
        RES_DIR="$loc"
        break
    fi
done

if [ -n "$RES_DIR" ]; then
    DATA_DIR="$PREFIX/share"
    mkdir -p "$DATA_DIR/applications" "$DATA_DIR/metainfo"

    if [ -f "$RES_DIR/ferrite-dm.desktop" ]; then
        sed "s|^Exec=fdm-gui|Exec=$BIN_DIR/fdm-gui|" "$RES_DIR/ferrite-dm.desktop" > "$DATA_DIR/applications/ferrite-dm.desktop" 2>/dev/null || \
            install -m 644 "$RES_DIR/ferrite-dm.desktop" "$DATA_DIR/applications/ferrite-dm.desktop"
    fi

    if [ -f "$RES_DIR/io.ferrite_dm.FerriteDM.metainfo.xml" ]; then
        install -m 644 "$RES_DIR/io.ferrite_dm.FerriteDM.metainfo.xml" "$DATA_DIR/metainfo/io.ferrite_dm.FerriteDM.metainfo.xml" 2>/dev/null || true
    fi

    if [ -d "$RES_DIR/icons/hicolor" ]; then
        for size in 16 32 48 64 128 256 512 1024; do
            SRC_ICON="$RES_DIR/icons/hicolor/${size}x${size}/apps/ferrite-dm.png"
            if [ -f "$SRC_ICON" ]; then
                mkdir -p "$DATA_DIR/icons/hicolor/${size}x${size}/apps"
                install -m 644 "$SRC_ICON" "$DATA_DIR/icons/hicolor/${size}x${size}/apps/ferrite-dm.png" 2>/dev/null || true
            fi
        done
        if [ -f "$RES_DIR/icons/hicolor/scalable/apps/ferrite-dm.svg" ]; then
            mkdir -p "$DATA_DIR/icons/hicolor/scalable/apps"
            install -m 644 "$RES_DIR/icons/hicolor/scalable/apps/ferrite-dm.svg" "$DATA_DIR/icons/hicolor/scalable/apps/ferrite-dm.svg" 2>/dev/null || true
        fi
    fi

    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "$DATA_DIR/applications" 2>/dev/null || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        gtk-update-icon-cache -q -t -f "$DATA_DIR/icons/hicolor" 2>/dev/null || true
    fi
fi

echo "Ferrite DM installed successfully!"
if [ -f "$BIN_DIR/fdm-gui" ]; then
    echo "  GUI:         $BIN_DIR/fdm-gui"
fi
echo "  CLI/Daemon:  $BIN_DIR/ferrite-dm"
echo "  Native Host: $BIN_DIR/ferrite-native-host"
echo ""
echo "To register the browser native messaging host, run:"
echo "  $BIN_DIR/ferrite-dm native-host-install"
