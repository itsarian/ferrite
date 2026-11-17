#!/bin/bash
set -euo pipefail

# Ferrite DM Linux AppImage Packaging Script
# Assembles AppDir and builds a self-contained portable .AppImage bundle.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
DIST_DIR="$ROOT_DIR/dist"
RESOURCES_DIR="$ROOT_DIR/resources"

# Extract version from Cargo.toml or environment
if [ -z "${VERSION:-}" ]; then
    VERSION=$(grep -m1 '^version = ' "$ROOT_DIR/Cargo.toml" | cut -d '"' -f2)
fi
VERSION="${VERSION#v}"

ARCH="${ARCH:-$(uname -m)}"
SKIP_BUILD=false
CLEAN=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --skip-build)
            SKIP_BUILD=true
            shift
            ;;
        --clean)
            CLEAN=true
            shift
            ;;
        --arch)
            ARCH="$2"
            shift 2
            ;;
        --arch=*)
            ARCH="${1#*=}"
            shift
            ;;
        --version)
            VERSION="$2"
            VERSION="${VERSION#v}"
            shift 2
            ;;
        --version=*)
            VERSION="${1#*=}"
            VERSION="${VERSION#v}"
            shift
            ;;
        -h|--help)
            echo "Usage: $0 [--skip-build] [--clean] [--arch <arch>] [--version <version>]"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            echo "Usage: $0 [--skip-build] [--clean] [--arch <arch>] [--version <version>]"
            exit 1
            ;;
    esac
done

case "$ARCH" in
    amd64)
        ARCH="x86_64"
        ;;
    arm64)
        ARCH="aarch64"
        ;;
esac

cd "$ROOT_DIR"

APPDIR="$DIST_DIR/Ferrite_DM.AppDir"
OUTPUT_APPIMAGE="$DIST_DIR/Ferrite-DM-${VERSION}-${ARCH}.AppImage"

if [ "$CLEAN" = true ]; then
    echo "==> Cleaning AppDir and existing AppImage..."
    rm -rf "$APPDIR" "$OUTPUT_APPIMAGE"
fi

mkdir -p "$DIST_DIR"

# 1. Build release binaries if requested
if [ "$SKIP_BUILD" = false ]; then
    echo "==> Building release binaries for AppImage ($ARCH)..."
    cargo build --release -p fdm-gui -p fdm-cli -p fdm-native-host
fi

# Locate binaries in target directory
BIN_SRC_DIR=""
for candidate in "target/release" "target/${ARCH}-unknown-linux-gnu/release"; do
    if [ -f "$ROOT_DIR/$candidate/ferrite-dm" ]; then
        BIN_SRC_DIR="$ROOT_DIR/$candidate"
        break
    fi
done

if [ -z "$BIN_SRC_DIR" ]; then
    echo "Error: Release binaries not found in target/. Run without --skip-build."
    exit 1
fi

echo "==> Using binaries from $BIN_SRC_DIR"

# 2. Assemble AppDir
echo "==> Assembling AppDir layout at $APPDIR..."
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin"
mkdir -p "$APPDIR/usr/lib"
mkdir -p "$APPDIR/usr/share/applications"
mkdir -p "$APPDIR/usr/share/metainfo"
mkdir -p "$APPDIR/usr/share/doc/ferrite-dm"

# Copy binaries
if [ -f "$BIN_SRC_DIR/fdm-gui" ]; then
    install -m 755 "$BIN_SRC_DIR/fdm-gui" "$APPDIR/usr/bin/fdm-gui"
fi
install -m 755 "$BIN_SRC_DIR/ferrite-dm" "$APPDIR/usr/bin/ferrite-dm"
install -m 755 "$BIN_SRC_DIR/ferrite-native-host" "$APPDIR/usr/bin/ferrite-native-host"

# Copy desktop and metadata
install -m 644 "$RESOURCES_DIR/linux/ferrite-dm.desktop" "$APPDIR/ferrite-dm.desktop"
install -m 644 "$RESOURCES_DIR/linux/ferrite-dm.desktop" "$APPDIR/usr/share/applications/ferrite-dm.desktop"
install -m 644 "$RESOURCES_DIR/linux/io.ferrite_dm.FerriteDM.metainfo.xml" "$APPDIR/usr/share/metainfo/io.ferrite_dm.FerriteDM.metainfo.xml"

# Copy icons
ICONS_SRC_DIR="$RESOURCES_DIR/linux/icons/hicolor"
for size in 16 32 48 64 128 256 512 1024; do
    DEST_ICON_DIR="$APPDIR/usr/share/icons/hicolor/${size}x${size}/apps"
    mkdir -p "$DEST_ICON_DIR"
    if [ -f "$ICONS_SRC_DIR/${size}x${size}/apps/ferrite-dm.png" ]; then
        install -m 644 "$ICONS_SRC_DIR/${size}x${size}/apps/ferrite-dm.png" "$DEST_ICON_DIR/ferrite-dm.png"
    fi
done

if [ -f "$ICONS_SRC_DIR/scalable/apps/ferrite-dm.svg" ]; then
    mkdir -p "$APPDIR/usr/share/icons/hicolor/scalable/apps"
    install -m 644 "$ICONS_SRC_DIR/scalable/apps/ferrite-dm.svg" "$APPDIR/usr/share/icons/hicolor/scalable/apps/ferrite-dm.svg"
fi

# Root AppDir icon
if [ -f "$ICONS_SRC_DIR/256x256/apps/ferrite-dm.png" ]; then
    install -m 644 "$ICONS_SRC_DIR/256x256/apps/ferrite-dm.png" "$APPDIR/ferrite-dm.png"
    install -m 644 "$ICONS_SRC_DIR/256x256/apps/ferrite-dm.png" "$APPDIR/.DirIcon"
elif [ -f "$RESOURCES_DIR/Ferrite-1024.png" ]; then
    install -m 644 "$RESOURCES_DIR/Ferrite-1024.png" "$APPDIR/ferrite-dm.png"
    install -m 644 "$RESOURCES_DIR/Ferrite-1024.png" "$APPDIR/.DirIcon"
fi

# Documentation and licenses
install -m 644 "$ROOT_DIR/LICENSE-MIT" "$APPDIR/usr/share/doc/ferrite-dm/LICENSE-MIT" 2>/dev/null || true
install -m 644 "$ROOT_DIR/LICENSE-APACHE" "$APPDIR/usr/share/doc/ferrite-dm/LICENSE-APACHE" 2>/dev/null || true
install -m 644 "$ROOT_DIR/README.md" "$APPDIR/usr/share/doc/ferrite-dm/README.md" 2>/dev/null || true

# 3. Create AppRun entrypoint
cat << 'EOF' > "$APPDIR/AppRun"
#!/bin/sh
set -e

# Resolve directory of this script
HERE="${APPDIR:-$(cd "$(dirname "$0")" && pwd)}"

export PATH="${HERE}/usr/bin:${PATH}"
export LD_LIBRARY_PATH="${HERE}/usr/lib:${LD_LIBRARY_PATH}"
export XDG_DATA_DIRS="${HERE}/usr/share:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"

# Route commands based on argument
case "${1:-}" in
    cli|--cli|ferrite-dm)
        shift
        exec "${HERE}/usr/bin/ferrite-dm" "$@"
        ;;
    native-host|--native-host|ferrite-native-host)
        shift
        exec "${HERE}/usr/bin/ferrite-native-host" "$@"
        ;;
    *)
        if [ -x "${HERE}/usr/bin/fdm-gui" ]; then
            exec "${HERE}/usr/bin/fdm-gui" "$@"
        else
            exec "${HERE}/usr/bin/ferrite-dm" "$@"
        fi
        ;;
esac
EOF

chmod 755 "$APPDIR/AppRun"

# 4. Build .AppImage using appimagetool if available
TOOL=""
if command -v appimagetool >/dev/null 2>&1; then
    TOOL="appimagetool"
elif [ -x "$ROOT_DIR/appimagetool" ]; then
    TOOL="$ROOT_DIR/appimagetool"
elif [ "$(uname -s)" = "Linux" ]; then
    echo "==> Downloading appimagetool for $ARCH..."
    TOOL_URL="https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-${ARCH}.AppImage"
    FALLBACK_URL="https://github.com/AppImage/AppImageKit/releases/download/continuous/appimagetool-${ARCH}.AppImage"
    if curl -fsSL -o "$ROOT_DIR/appimagetool" "$TOOL_URL" || curl -fsSL -o "$ROOT_DIR/appimagetool" "$FALLBACK_URL"; then
        chmod +x "$ROOT_DIR/appimagetool"
        TOOL="$ROOT_DIR/appimagetool"
    fi
fi

if [ -n "$TOOL" ]; then
    echo "==> Packaging AppImage using $TOOL..."
    if ! APPIMAGE_EXTRACT_AND_RUN=1 ARCH="$ARCH" "$TOOL" "$APPDIR" "$OUTPUT_APPIMAGE"; then
        echo "==> Retrying AppImage build with --no-appstream..."
        APPIMAGE_EXTRACT_AND_RUN=1 ARCH="$ARCH" "$TOOL" --no-appstream "$APPDIR" "$OUTPUT_APPIMAGE"
    fi

    echo "==> Successfully created AppImage: $OUTPUT_APPIMAGE"
    ls -lh "$OUTPUT_APPIMAGE"
else
    if [ "$(uname -s)" = "Linux" ]; then
        echo "Error: Failed to acquire appimagetool on Linux."
        exit 1
    fi
    echo "==> Notice: appimagetool not found on current host."
    echo "==> Fully structured AppDir prepared at: $APPDIR"
    echo "==> To generate final AppImage on Linux: appimagetool $APPDIR $OUTPUT_APPIMAGE"
fi
