#!/bin/bash
set -e

# Ferrite Installer
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

echo "Installing Ferrite binaries to $BIN_DIR..."

if [ -f "$SCRIPT_DIR/ferrite-gui" ]; then
    install -m 755 "$SCRIPT_DIR/ferrite-gui" "$BIN_DIR/ferrite-gui" 2>/dev/null || cp "$SCRIPT_DIR/ferrite-gui" "$BIN_DIR/ferrite-gui"
    chmod +x "$BIN_DIR/ferrite-gui" 2>/dev/null || true
fi

if [ -f "$SCRIPT_DIR/ferrite" ]; then
    install -m 755 "$SCRIPT_DIR/ferrite" "$BIN_DIR/ferrite" 2>/dev/null || cp "$SCRIPT_DIR/ferrite" "$BIN_DIR/ferrite"
    chmod +x "$BIN_DIR/ferrite" 2>/dev/null || true
fi

if [ -f "$SCRIPT_DIR/ferrite-native-host" ]; then
    install -m 755 "$SCRIPT_DIR/ferrite-native-host" "$BIN_DIR/ferrite-native-host" 2>/dev/null || cp "$SCRIPT_DIR/ferrite-native-host" "$BIN_DIR/ferrite-native-host"
    chmod +x "$BIN_DIR/ferrite-native-host" 2>/dev/null || true
fi

# Optional Linux desktop integration
RES_DIR=""
for loc in "$SCRIPT_DIR/resources/linux" "$ROOT_DIR/resources/linux" "$SCRIPT_DIR"; do
    if [ -f "$loc/ferrite.desktop" ]; then
        RES_DIR="$loc"
        break
    fi
done

if [ -n "$RES_DIR" ]; then
    DATA_DIR="$PREFIX/share"
    mkdir -p "$DATA_DIR/applications" "$DATA_DIR/metainfo"

    if [ -f "$RES_DIR/ferrite.desktop" ]; then
        sed "s|^Exec=ferrite-gui|Exec=$BIN_DIR/ferrite-gui|" "$RES_DIR/ferrite.desktop" > "$DATA_DIR/applications/ferrite.desktop" 2>/dev/null || \
            install -m 644 "$RES_DIR/ferrite.desktop" "$DATA_DIR/applications/ferrite.desktop"
    fi

    if [ -f "$RES_DIR/io.ferrite.Ferrite.metainfo.xml" ]; then
        install -m 644 "$RES_DIR/io.ferrite.Ferrite.metainfo.xml" "$DATA_DIR/metainfo/io.ferrite.Ferrite.metainfo.xml" 2>/dev/null || true
    fi

    if [ -d "$RES_DIR/icons/hicolor" ]; then
        for size in 16 32 48 64 128 256 512 1024; do
            SRC_ICON="$RES_DIR/icons/hicolor/${size}x${size}/apps/ferrite.png"
            if [ -f "$SRC_ICON" ]; then
                mkdir -p "$DATA_DIR/icons/hicolor/${size}x${size}/apps"
                install -m 644 "$SRC_ICON" "$DATA_DIR/icons/hicolor/${size}x${size}/apps/ferrite.png" 2>/dev/null || true
            fi
        done
        if [ -f "$RES_DIR/icons/hicolor/scalable/apps/ferrite.svg" ]; then
            mkdir -p "$DATA_DIR/icons/hicolor/scalable/apps"
            install -m 644 "$RES_DIR/icons/hicolor/scalable/apps/ferrite.svg" "$DATA_DIR/icons/hicolor/scalable/apps/ferrite.svg" 2>/dev/null || true
        fi
    fi

    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "$DATA_DIR/applications" 2>/dev/null || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        gtk-update-icon-cache -q -t -f "$DATA_DIR/icons/hicolor" 2>/dev/null || true
    fi
fi

echo "Ferrite installed successfully!"
if [ -f "$BIN_DIR/ferrite-gui" ]; then
    echo "  GUI:         $BIN_DIR/ferrite-gui"
fi
echo "  CLI/Daemon:  $BIN_DIR/ferrite"
echo "  Native Host: $BIN_DIR/ferrite-native-host"
echo ""
echo "To register the browser native messaging host, run:"
echo "  $BIN_DIR/ferrite native-host-install"
