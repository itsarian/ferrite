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
APPIMAGETOOL_VERSION="1.9.1"
case "$APPIMAGE_ARCH" in
    x86_64)
        APPIMAGETOOL_SHA256="ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0"
        ;;
    aarch64)
        APPIMAGETOOL_SHA256="f0837e7448a0c1e4e650a93bb3e85802546e60654ef287576f46c71c126a9158"
        ;;
    *)
        echo "Error: No pinned appimagetool SHA-256 configured for ${APPIMAGE_ARCH}." >&2
        exit 1
        ;;
esac

verify_tool_sha256() {
    local file_path="$1"
    local expected="$2"
    local actual=""
    if command -v sha256sum >/dev/null 2>&1; then
        actual="$(sha256sum "$file_path" | awk '{print $1}')"
    elif command -v shasum >/dev/null 2>&1; then
        actual="$(shasum -a 256 "$file_path" | awk '{print $1}')"
    else
        echo "Error: Neither sha256sum nor shasum is available to verify appimagetool." >&2
        return 1
    fi
    [ "$actual" = "$expected" ]
}

TOOL=""
TOOL_CACHE="$DIST_DIR/tools/appimagetool-${APPIMAGETOOL_VERSION}-${APPIMAGE_ARCH}.AppImage"
if command -v appimagetool >/dev/null 2>&1; then
    TOOL="appimagetool"
else
    if [ -x "$TOOL_CACHE" ]; then
        if verify_tool_sha256 "$TOOL_CACHE" "$APPIMAGETOOL_SHA256"; then
            TOOL="$TOOL_CACHE"
        else
            echo "==> Cached appimagetool failed SHA-256 check; removing stale cache..." >&2
            rm -f "$TOOL_CACHE"
        fi
    fi

    if [ -z "$TOOL" ] && [ "$(uname -s)" = "Linux" ]; then
        mkdir -p "$DIST_DIR/tools"
        echo "==> Downloading pinned appimagetool ${APPIMAGETOOL_VERSION} for ${APPIMAGE_ARCH}..."
        TOOL_URL="https://github.com/AppImage/appimagetool/releases/download/${APPIMAGETOOL_VERSION}/appimagetool-${APPIMAGE_ARCH}.AppImage"
        if ! curl -fsSL -o "${TOOL_CACHE}.tmp" "$TOOL_URL"; then
            rm -f "${TOOL_CACHE}.tmp"
            echo "Error: Failed to download appimagetool ${APPIMAGETOOL_VERSION} from $TOOL_URL" >&2
            exit 1
        fi
        if ! verify_tool_sha256 "${TOOL_CACHE}.tmp" "$APPIMAGETOOL_SHA256"; then
            rm -f "${TOOL_CACHE}.tmp"
            echo "Error: SHA-256 verification failed for downloaded appimagetool ${APPIMAGETOOL_VERSION} (${APPIMAGE_ARCH})." >&2
            exit 1
        fi
        mv -f "${TOOL_CACHE}.tmp" "$TOOL_CACHE"
        chmod +x "$TOOL_CACHE"
        TOOL="$TOOL_CACHE"
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
