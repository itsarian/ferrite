#!/bin/bash
set -euo pipefail

# Ferrite Linux Uninstaller
# Removes binaries, desktop launchers, AppStream metadata, and icons.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

PREFIX="${PREFIX:-}"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --prefix)
            if [[ $# -lt 2 || -z "$2" ]]; then
                echo "Error: --prefix requires a directory path" >&2
                exit 1
            fi
            PREFIX="$2"
            shift 2
            ;;
        --prefix=*)
            PREFIX="${1#*=}"
            if [[ -z "$PREFIX" ]]; then
                echo "Error: --prefix requires a directory path" >&2
                exit 1
            fi
            shift
            ;;
        --user)
            PREFIX="$HOME/.local"
            shift
            ;;
        --system)
            PREFIX="/usr/local"
            shift
            ;;
        -h|--help)
            echo "Usage: $0 [--user | --system | --prefix <path>]"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            echo "Usage: $0 [--user | --system | --prefix <path>]"
            exit 1
            ;;
    esac
done

if [ -z "$PREFIX" ]; then
    if [ "${EUID:-$(id -u)}" -eq 0 ]; then
        PREFIX="/usr/local"
    elif [ -w "/usr/local/bin" ]; then
        PREFIX="/usr/local"
    else
        PREFIX="$HOME/.local"
    fi
fi

BIN_DIR="$PREFIX/bin"
DATA_DIR="$PREFIX/share"
APP_DIR="$DATA_DIR/applications"
META_DIR="$DATA_DIR/metainfo"
ICON_BASE="$DATA_DIR/icons/hicolor"

echo "==> Uninstalling Ferrite from prefix: $PREFIX"

# Remove binaries
rm -f "$BIN_DIR/ferrite-gui" "$BIN_DIR/ferrite" "$BIN_DIR/ferrite-native-host"

# Remove desktop entry and AppStream metadata
rm -f "$APP_DIR/ferrite.desktop"
rm -f "$META_DIR/io.ferrite.Ferrite.metainfo.xml"

# Remove icons
for size in 16 32 48 64 128 256 512 1024; do
    rm -f "$ICON_BASE/${size}x${size}/apps/ferrite.png"
done
rm -f "$ICON_BASE/scalable/apps/ferrite.svg"

# Update caches
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$APP_DIR" 2>/dev/null || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -t -f "$ICON_BASE" 2>/dev/null || true
fi

echo "==> Ferrite uninstalled cleanly."
