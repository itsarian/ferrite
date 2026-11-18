#!/bin/bash
set -euo pipefail

# Ferrite DM Linux Installer
# Installs GUI & CLI binaries, Freedesktop .desktop entry, AppStream metainfo, and hicolor icons.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." 2>/dev/null && pwd || echo "$SCRIPT_DIR")"

# Default prefix detection: /usr/local if writable/root, else ~/.local
PREFIX="${PREFIX:-}"
CUSTOM_PREFIX=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --prefix)
            if [[ $# -lt 2 || -z "$2" ]]; then
                echo "Error: --prefix requires a directory path" >&2
                exit 1
            fi
            PREFIX="$2"
            CUSTOM_PREFIX=true
            shift 2
            ;;
        --prefix=*)
            PREFIX="${1#*=}"
            if [[ -z "$PREFIX" ]]; then
                echo "Error: --prefix requires a directory path" >&2
                exit 1
            fi
            CUSTOM_PREFIX=true
            shift
            ;;
        --user)
            PREFIX="$HOME/.local"
            CUSTOM_PREFIX=true
            shift
            ;;
        --system)
            PREFIX="/usr/local"
            CUSTOM_PREFIX=true
            shift
            ;;
        -h|--help)
            echo "Ferrite DM Linux Installer"
            echo "Usage: $0 [--user | --system | --prefix <path>]"
            echo ""
            echo "Options:"
            echo "  --user       Install for the current user to ~/.local (default if non-root)"
            echo "  --system     Install system-wide to /usr/local (requires sudo/root)"
            echo "  --prefix DIR Install into specified directory prefix"
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

mkdir -p "$BIN_DIR" "$APP_DIR" "$META_DIR"

echo "==> Installing Ferrite DM to prefix: $PREFIX"

# Detect host Rust target triple for target/<triple>/release lookup
HOST_RUST_TARGET=""
case "$(uname -m)" in
    x86_64|amd64)
        HOST_RUST_TARGET="x86_64-unknown-linux-gnu"
        ;;
    aarch64|arm64)
        HOST_RUST_TARGET="aarch64-unknown-linux-gnu"
        ;;
esac

# Locate binaries (either in script dir, target/<triple>/release, target/release, or current directory)
install_bin() {
    local bin_name="$1"
    local found=""
    local candidates=("$SCRIPT_DIR/$bin_name")
    if [ -n "$HOST_RUST_TARGET" ]; then
        candidates+=("$ROOT_DIR/target/${HOST_RUST_TARGET}/release/$bin_name")
    fi
    candidates+=("$ROOT_DIR/target/release/$bin_name" "$PWD/$bin_name")

    for loc in "${candidates[@]}"; do
        if [ -f "$loc" ]; then
            found="$loc"
            break
        fi
    done

    if [ -n "$found" ]; then
        echo "  -> Installing $bin_name to $BIN_DIR"
        install -m 755 "$found" "$BIN_DIR/$bin_name" 2>/dev/null || cp "$found" "$BIN_DIR/$bin_name"
        chmod +x "$BIN_DIR/$bin_name" 2>/dev/null || true
    fi
}

install_bin "fdm-gui"
install_bin "ferrite-dm"
install_bin "ferrite-native-host"

# Locate resources
RES_DIR=""
for loc in "$SCRIPT_DIR/resources/linux" "$ROOT_DIR/resources/linux" "$SCRIPT_DIR"; do
    if [ -f "$loc/ferrite-dm.desktop" ]; then
        RES_DIR="$loc"
        break
    fi
done

if [ -n "$RES_DIR" ]; then
    echo "==> Installing desktop integration files..."

    # Install .desktop file
    if [ -f "$RES_DIR/ferrite-dm.desktop" ]; then
        echo "  -> Installing application launcher to $APP_DIR/ferrite-dm.desktop"
        sed "s|^Exec=fdm-gui|Exec=$BIN_DIR/fdm-gui|" "$RES_DIR/ferrite-dm.desktop" > "$APP_DIR/ferrite-dm.desktop" 2>/dev/null || \
            install -m 644 "$RES_DIR/ferrite-dm.desktop" "$APP_DIR/ferrite-dm.desktop"
        chmod 644 "$APP_DIR/ferrite-dm.desktop" 2>/dev/null || true
    fi

    # Install AppStream metadata
    if [ -f "$RES_DIR/io.ferrite_dm.FerriteDM.metainfo.xml" ]; then
        echo "  -> Installing AppStream metadata to $META_DIR"
        install -m 644 "$RES_DIR/io.ferrite_dm.FerriteDM.metainfo.xml" "$META_DIR/io.ferrite_dm.FerriteDM.metainfo.xml"
    fi

    # Install hicolor icons
    if [ -d "$RES_DIR/icons/hicolor" ]; then
        echo "  -> Installing hicolor theme icons to $ICON_BASE"
        for size in 16 32 48 64 128 256 512 1024; do
            SRC_ICON="$RES_DIR/icons/hicolor/${size}x${size}/apps/ferrite-dm.png"
            if [ -f "$SRC_ICON" ]; then
                mkdir -p "$ICON_BASE/${size}x${size}/apps"
                install -m 644 "$SRC_ICON" "$ICON_BASE/${size}x${size}/apps/ferrite-dm.png"
            fi
        done
        if [ -f "$RES_DIR/icons/hicolor/scalable/apps/ferrite-dm.svg" ]; then
            mkdir -p "$ICON_BASE/scalable/apps"
            install -m 644 "$RES_DIR/icons/hicolor/scalable/apps/ferrite-dm.svg" "$ICON_BASE/scalable/apps/ferrite-dm.svg"
        fi
    fi

    # Update desktop & icon caches
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "$APP_DIR" 2>/dev/null || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        gtk-update-icon-cache -q -t -f "$ICON_BASE" 2>/dev/null || true
    fi
fi

echo ""
echo "=========================================================="
echo " Ferrite DM installed successfully!"
echo "=========================================================="
if [ -f "$BIN_DIR/fdm-gui" ]; then
    echo "  GUI Executable:  $BIN_DIR/fdm-gui"
fi
echo "  CLI / Daemon:    $BIN_DIR/ferrite-dm"
echo "  Native Host:     $BIN_DIR/ferrite-native-host"
if [ -f "$APP_DIR/ferrite-dm.desktop" ]; then
    echo "  Desktop Entry:   $APP_DIR/ferrite-dm.desktop"
fi
echo ""
echo "To register browser integration (Chrome, Firefox, Edge, Brave):"
echo "  $BIN_DIR/ferrite-dm native-host-install"
echo ""
if [[ ":$PATH:" != *":$BIN_DIR:"* ]]; then
    echo "Notice: $BIN_DIR is not in your PATH. You may want to add:"
    echo "  export PATH=\"$BIN_DIR:\$PATH\""
    echo "to your ~/.bashrc or ~/.zshrc profile."
fi
