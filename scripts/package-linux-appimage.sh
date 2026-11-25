#!/bin/bash
set -euo pipefail

# Ferrite Linux AppImage Packaging Script
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
            if [[ $# -lt 2 || -z "$2" ]]; then
                echo "Error: --arch requires a value (x86_64, amd64, arm64, aarch64)" >&2
                exit 1
            fi
            ARCH="$2"
            shift 2
            ;;
        --arch=*)
            ARCH="${1#*=}"
            if [[ -z "$ARCH" ]]; then
                echo "Error: --arch requires a value (x86_64, amd64, arm64, aarch64)" >&2
                exit 1
            fi
            shift
            ;;
        --version)
            if [[ $# -lt 2 || -z "$2" ]]; then
                echo "Error: --version requires a value" >&2
                exit 1
            fi
            VERSION="$2"
            VERSION="${VERSION#v}"
            shift 2
            ;;
        --version=*)
            VERSION="${1#*=}"
            VERSION="${VERSION#v}"
            if [[ -z "$VERSION" ]]; then
                echo "Error: --version requires a value" >&2
                exit 1
            fi
            shift
            ;;
        -h|--help)
            echo "Usage: $0 [--skip-build] [--clean] [--arch <x86_64|amd64|arm64|aarch64>] [--version <version>]"
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            echo "Usage: $0 [--skip-build] [--clean] [--arch <x86_64|amd64|arm64|aarch64>] [--version <version>]" >&2
            exit 1
            ;;
    esac
done

case "$ARCH" in
    x86_64|amd64)
        ASSET_ARCH="x86_64"
        APPIMAGE_ARCH="x86_64"
        RUST_TARGET="x86_64-unknown-linux-gnu"
        ;;
    aarch64|arm64)
        ASSET_ARCH="arm64"
        APPIMAGE_ARCH="aarch64"
        RUST_TARGET="aarch64-unknown-linux-gnu"
        ;;
    *)
        echo "Error: Unsupported architecture '$ARCH'. Supported: x86_64, amd64, arm64, aarch64" >&2
        exit 1
        ;;
esac

HOST_RAW_ARCH="$(uname -m)"
case "$HOST_RAW_ARCH" in
    x86_64|amd64) HOST_ASSET_ARCH="x86_64" ;;
    aarch64|arm64) HOST_ASSET_ARCH="arm64" ;;
    *) HOST_ASSET_ARCH="$HOST_RAW_ARCH" ;;
esac

cd "$ROOT_DIR"

APPDIR="$DIST_DIR/Ferrite-${ASSET_ARCH}.AppDir"
OUTPUT_APPIMAGE="$DIST_DIR/ferrite-${VERSION}-linux-${ASSET_ARCH}.AppImage"

if [ "$CLEAN" = true ]; then
    echo "==> Cleaning AppDir and existing AppImages..."
    rm -rf "$DIST_DIR"/Ferrite*.AppDir "$DIST_DIR"/*.AppImage
fi

mkdir -p "$DIST_DIR"

# 1. Build release binaries if requested
if [ "$SKIP_BUILD" = false ]; then
    echo "==> Building release binaries for AppImage (${ASSET_ARCH} / ${APPIMAGE_ARCH})..."
    if [ "$(uname -s)" = "Linux" ] && [ "$HOST_ASSET_ARCH" = "$ASSET_ARCH" ]; then
        cargo build --release --locked -p ferrite-gui -p ferrite-cli -p ferrite-native-host
    else
        cargo build --release --locked --target "$RUST_TARGET" -p ferrite-gui -p ferrite-cli -p ferrite-native-host
    fi
fi

# Locate binaries in target directory (prefer target/<RUST_TARGET>/release over target/release)
BIN_SRC_DIR=""
for candidate in "target/${RUST_TARGET}/release" "target/release"; do
    if [ -f "$ROOT_DIR/$candidate/ferrite" ]; then
        BIN_SRC_DIR="$ROOT_DIR/$candidate"
        break
    fi
done

if [ -z "$BIN_SRC_DIR" ]; then
    echo "Error: Release binaries not found in target/${RUST_TARGET}/release or target/release. Run without --skip-build." >&2
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
mkdir -p "$APPDIR/usr/share/doc/ferrite"

# Copy binaries
if [ -f "$BIN_SRC_DIR/ferrite-gui" ]; then
    install -m 755 "$BIN_SRC_DIR/ferrite-gui" "$APPDIR/usr/bin/ferrite-gui"
fi
install -m 755 "$BIN_SRC_DIR/ferrite" "$APPDIR/usr/bin/ferrite"
install -m 755 "$BIN_SRC_DIR/ferrite-native-host" "$APPDIR/usr/bin/ferrite-native-host"

# Copy desktop and metadata
install -m 644 "$RESOURCES_DIR/linux/ferrite.desktop" "$APPDIR/ferrite.desktop"
install -m 644 "$RESOURCES_DIR/linux/ferrite.desktop" "$APPDIR/usr/share/applications/ferrite.desktop"
install -m 644 "$RESOURCES_DIR/linux/io.ferrite.Ferrite.metainfo.xml" "$APPDIR/usr/share/metainfo/io.ferrite.Ferrite.metainfo.xml"

# Copy icons
ICONS_SRC_DIR="$RESOURCES_DIR/linux/icons/hicolor"
for size in 16 32 48 64 128 256 512 1024; do
    DEST_ICON_DIR="$APPDIR/usr/share/icons/hicolor/${size}x${size}/apps"
    mkdir -p "$DEST_ICON_DIR"
    if [ -f "$ICONS_SRC_DIR/${size}x${size}/apps/ferrite.png" ]; then
        install -m 644 "$ICONS_SRC_DIR/${size}x${size}/apps/ferrite.png" "$DEST_ICON_DIR/ferrite.png"
    fi
done

if [ -f "$ICONS_SRC_DIR/scalable/apps/ferrite.svg" ]; then
    mkdir -p "$APPDIR/usr/share/icons/hicolor/scalable/apps"
    install -m 644 "$ICONS_SRC_DIR/scalable/apps/ferrite.svg" "$APPDIR/usr/share/icons/hicolor/scalable/apps/ferrite.svg"
fi

# Root AppDir icon
if [ -f "$ICONS_SRC_DIR/256x256/apps/ferrite.png" ]; then
    install -m 644 "$ICONS_SRC_DIR/256x256/apps/ferrite.png" "$APPDIR/ferrite.png"
    install -m 644 "$ICONS_SRC_DIR/256x256/apps/ferrite.png" "$APPDIR/.DirIcon"
elif [ -f "$RESOURCES_DIR/Ferrite-1024.png" ]; then
    install -m 644 "$RESOURCES_DIR/Ferrite-1024.png" "$APPDIR/ferrite.png"
    install -m 644 "$RESOURCES_DIR/Ferrite-1024.png" "$APPDIR/.DirIcon"
fi

# Documentation and licenses
install -m 644 "$ROOT_DIR/LICENSE-MIT" "$APPDIR/usr/share/doc/ferrite/LICENSE-MIT" 2>/dev/null || true
install -m 644 "$ROOT_DIR/LICENSE-APACHE" "$APPDIR/usr/share/doc/ferrite/LICENSE-APACHE" 2>/dev/null || true
install -m 644 "$ROOT_DIR/README.md" "$APPDIR/usr/share/doc/ferrite/README.md" 2>/dev/null || true

# 3. Create AppRun entrypoint
cat << 'EOF' > "$APPDIR/AppRun"
#!/bin/sh
set -e

# Resolve directory of this script
HERE="${APPDIR:-$(cd "$(dirname "$0")" && pwd)}"

export PATH="${HERE}/usr/bin:${PATH}"
export LD_LIBRARY_PATH="${HERE}/usr/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export XDG_DATA_DIRS="${HERE}/usr/share:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"

# Route commands based on argument
case "${1:-}" in
    cli|--cli|ferrite)
        shift
        exec "${HERE}/usr/bin/ferrite" "$@"
        ;;
    native-host|--native-host|ferrite-native-host)
        shift
        exec "${HERE}/usr/bin/ferrite-native-host" "$@"
        ;;
    *)
        if [ -x "${HERE}/usr/bin/ferrite-gui" ]; then
            exec "${HERE}/usr/bin/ferrite-gui" "$@"
        else
            exec "${HERE}/usr/bin/ferrite" "$@"
        fi
        ;;
esac
EOF

chmod 755 "$APPDIR/AppRun"

# 4. Build .AppImage using appimagetool if available
TOOL=""
TOOL_CACHE="$DIST_DIR/tools/appimagetool-${APPIMAGE_ARCH}"
if command -v appimagetool >/dev/null 2>&1; then
    TOOL="appimagetool"
elif [ -x "$TOOL_CACHE" ]; then
    TOOL="$TOOL_CACHE"
elif [ "$(uname -s)" = "Linux" ]; then
    mkdir -p "$DIST_DIR/tools"
    echo "==> Downloading appimagetool for ${APPIMAGE_ARCH}..."
    TOOL_URL="https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-${APPIMAGE_ARCH}.AppImage"
    FALLBACK_URL="https://github.com/AppImage/AppImageKit/releases/download/continuous/appimagetool-${APPIMAGE_ARCH}.AppImage"
    if curl -fsSL -o "${TOOL_CACHE}.tmp" "$TOOL_URL" || curl -fsSL -o "${TOOL_CACHE}.tmp" "$FALLBACK_URL"; then
        mv -f "${TOOL_CACHE}.tmp" "$TOOL_CACHE"
        chmod +x "$TOOL_CACHE"
        TOOL="$TOOL_CACHE"
    else
        rm -f "${TOOL_CACHE}.tmp"
    fi
fi

if [ -n "$TOOL" ]; then
    echo "==> Packaging AppImage (${APPIMAGE_ARCH}) using $TOOL..."
    if ! APPIMAGE_EXTRACT_AND_RUN=1 ARCH="$APPIMAGE_ARCH" "$TOOL" "$APPDIR" "$OUTPUT_APPIMAGE"; then
        echo "==> Retrying AppImage build with --no-appstream..."
        APPIMAGE_EXTRACT_AND_RUN=1 ARCH="$APPIMAGE_ARCH" "$TOOL" --no-appstream "$APPDIR" "$OUTPUT_APPIMAGE"
    fi

    echo "==> Successfully created AppImage: $OUTPUT_APPIMAGE"
    ls -lh "$OUTPUT_APPIMAGE"
else
    if [ "$(uname -s)" = "Linux" ]; then
        echo "Error: Failed to acquire appimagetool for ${APPIMAGE_ARCH} on Linux." >&2
        exit 1
    fi
    echo "==> Notice: appimagetool not found on current host."
    echo "==> Fully structured AppDir prepared at: $APPDIR"
    echo "==> To generate final AppImage on Linux: ARCH=${APPIMAGE_ARCH} appimagetool $APPDIR $OUTPUT_APPIMAGE"
fi
