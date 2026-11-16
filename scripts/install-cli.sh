#!/bin/bash
set -e

# Ferrite DM CLI & Daemon Installer
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

echo "Ferrite DM installed successfully!"
if [ -f "$BIN_DIR/fdm-gui" ]; then
    echo "  GUI:         $BIN_DIR/fdm-gui"
fi
echo "  CLI/Daemon:  $BIN_DIR/ferrite-dm"
echo "  Native Host: $BIN_DIR/ferrite-native-host"
echo ""
echo "To register the browser native messaging host, run:"
echo "  $BIN_DIR/ferrite-dm native-host-install"
