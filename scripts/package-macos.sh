#!/bin/bash
set -euo pipefail

# Ferrite DM macOS Packaging Script
# Builds Ferrite DM.app, .dmg installer, .pkg installer, .zip bundle, and CLI tarball
# for macOS arm64 (Apple Silicon) or x86_64 (Intel).

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
DIST_DIR="$ROOT_DIR/dist"
RESOURCES_DIR="$ROOT_DIR/resources"

if [ -n "${VERSION:-}" ]; then
    RAW_VERSION="$VERSION"
elif [[ "${GITHUB_REF_NAME:-}" == v* ]]; then
    RAW_VERSION="$GITHUB_REF_NAME"
else
    RAW_VERSION="$(grep -m1 '^version = ' "$ROOT_DIR/Cargo.toml" | cut -d '"' -f2 || true)"
    RAW_VERSION="${RAW_VERSION:-0.1.0}"
fi
VERSION="${RAW_VERSION#v}"

HOST_ARCH_RAW="$(uname -m)"
case "$HOST_ARCH_RAW" in
    arm64|aarch64)
        HOST_ARCH="arm64"
        ;;
    x86_64|amd64)
        HOST_ARCH="x86_64"
        ;;
    *)
        HOST_ARCH="$HOST_ARCH_RAW"
        ;;
esac

ARCH="${ARCH:-$HOST_ARCH}"
SKIP_BUILD=false
CLEAN=false

usage() {
    echo "Usage: $0 [--skip-build] [--clean] [--arch <arm64|x86_64>] [--version <version>]"
}

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
            if [[ $# -lt 2 ]]; then
                echo "Error: --arch requires an argument."
                usage
                exit 1
            fi
            ARCH="$2"
            shift 2
            ;;
        --arch=*)
            ARCH="${1#*=}"
            shift
            ;;
        --version)
            if [[ $# -lt 2 ]]; then
                echo "Error: --version requires an argument."
                usage
                exit 1
            fi
            VERSION="${2#v}"
            shift 2
            ;;
        --version=*)
            VERSION="${1#*=}"
            VERSION="${VERSION#v}"
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            usage
            exit 1
            ;;
    esac
done

case "$ARCH" in
    arm64|aarch64)
        ARCH_LABEL="arm64"
        RUST_TARGET="aarch64-apple-darwin"
        ;;
    x86_64|amd64)
        ARCH_LABEL="x86_64"
        RUST_TARGET="x86_64-apple-darwin"
        ;;
    *)
        echo "Error: Unsupported macOS architecture '$ARCH'. Expected arm64 or x86_64."
        exit 1
        ;;
esac

cd "$ROOT_DIR"

if [ "$CLEAN" = true ]; then
    echo "==> Cleaning dist/..."
    rm -rf "$DIST_DIR"
fi

mkdir -p "$DIST_DIR" "$RESOURCES_DIR"

# Remove any stale legacy-named macOS artifacts in dist/
rm -f "$DIST_DIR"/Ferrite-DM-*.dmg "$DIST_DIR"/Ferrite-DM-*.pkg "$DIST_DIR"/Ferrite-DM-*.zip

# 1. Build release binaries
if [ "$SKIP_BUILD" = false ]; then
    echo "==> Building macOS release binaries for $ARCH_LABEL ($RUST_TARGET)..."
    if command -v rustup >/dev/null 2>&1; then
        rustup target add "$RUST_TARGET"
    fi
    cargo build --release --target "$RUST_TARGET" -p fdm-gui -p fdm-cli -p fdm-native-host
else
    echo "==> Skipping cargo build (using existing release binaries for $ARCH_LABEL)..."
fi

# Locate and verify binaries
BIN_SRC_DIR=""
if [ -f "target/$RUST_TARGET/release/fdm-gui" ] && [ -f "target/$RUST_TARGET/release/ferrite-dm" ] && [ -f "target/$RUST_TARGET/release/ferrite-native-host" ]; then
    BIN_SRC_DIR="target/$RUST_TARGET/release"
elif [ "$ARCH_LABEL" = "$HOST_ARCH" ] && [ -f "target/release/fdm-gui" ] && [ -f "target/release/ferrite-dm" ] && [ -f "target/release/ferrite-native-host" ]; then
    BIN_SRC_DIR="target/release"
fi

if [ -z "$BIN_SRC_DIR" ]; then
    echo "Error: Release binaries for $ARCH_LABEL ($RUST_TARGET) not found."
    if [ "$ARCH_LABEL" = "$HOST_ARCH" ]; then
        echo "Expected binaries in target/$RUST_TARGET/release or target/release."
    else
        echo "Expected binaries in target/$RUST_TARGET/release."
    fi
    echo "Run without --skip-build or build with: cargo build --release --target $RUST_TARGET -p fdm-gui -p fdm-cli -p fdm-native-host"
    exit 1
fi

echo "==> Using release binaries from $BIN_SRC_DIR"

# 2. Generate Apple Icon (.icns) if needed
if [ ! -f "$RESOURCES_DIR/Ferrite.icns" ]; then
    echo "==> Generating Ferrite.icns from vector assets..."
    swift "$SCRIPT_DIR/generate-icon.swift" "$RESOURCES_DIR"
fi

# 3. Assemble Ferrite DM.app
APP_NAME="Ferrite DM.app"
APP_DIR="$DIST_DIR/$APP_NAME"
CONTENTS_DIR="$APP_DIR/Contents"
MACOS_DIR="$CONTENTS_DIR/MacOS"
RES_DIR="$CONTENTS_DIR/Resources"

echo "==> Assembling $APP_NAME ($ARCH_LABEL)..."
rm -rf "$APP_DIR"
mkdir -p "$MACOS_DIR" "$RES_DIR"

# Copy Info.plist and Icon
cp "$RESOURCES_DIR/Info.plist" "$CONTENTS_DIR/Info.plist"
cp "$RESOURCES_DIR/Ferrite.icns" "$RES_DIR/Ferrite.icns"

# Copy binaries
cp "$BIN_SRC_DIR/fdm-gui" "$MACOS_DIR/fdm-gui"
cp "$BIN_SRC_DIR/ferrite-dm" "$MACOS_DIR/ferrite-dm"
cp "$BIN_SRC_DIR/ferrite-native-host" "$MACOS_DIR/ferrite-native-host"

chmod +x "$MACOS_DIR/"*

# 4. Ad-hoc code sign the bundle
echo "==> Code signing $APP_NAME (ad-hoc)..."
codesign --force --deep --sign - "$APP_DIR"

# 5. Build .dmg disk image
DMG_NAME="ferrite-dm-${VERSION}-macos-${ARCH_LABEL}.dmg"
DMG_PATH="$DIST_DIR/$DMG_NAME"
DMG_STAGE="$DIST_DIR/dmg-stage"

echo "==> Building Disk Image ($DMG_NAME)..."
rm -rf "$DMG_STAGE" "$DMG_PATH"
mkdir -p "$DMG_STAGE"
cp -R "$APP_DIR" "$DMG_STAGE/"
ln -s /Applications "$DMG_STAGE/Applications"

hdiutil create \
    -volname "Ferrite DM" \
    -srcfolder "$DMG_STAGE" \
    -ov \
    -format UDZO \
    "$DMG_PATH" > /dev/null

rm -rf "$DMG_STAGE"

# 6. Build .pkg installer package
PKG_NAME="ferrite-dm-${VERSION}-macos-${ARCH_LABEL}.pkg"
PKG_PATH="$DIST_DIR/$PKG_NAME"
PKG_STAGE="$DIST_DIR/pkg-stage"

echo "==> Building Installer Package ($PKG_NAME)..."
rm -rf "$PKG_STAGE" "$PKG_PATH"
mkdir -p "$PKG_STAGE"
cp -R "$APP_DIR" "$PKG_STAGE/"

chmod +x "$RESOURCES_DIR/pkg-scripts/postinstall"

pkgbuild \
    --root "$PKG_STAGE" \
    --install-location "/Applications" \
    --scripts "$RESOURCES_DIR/pkg-scripts" \
    --identifier "org.ferritedm.FerriteDM" \
    --version "$VERSION" \
    "$PKG_PATH" > /dev/null

rm -rf "$PKG_STAGE"

# 7. Build standalone App Bundle Zip
ZIP_NAME="ferrite-dm-${VERSION}-macos-${ARCH_LABEL}.zip"
ZIP_PATH="$DIST_DIR/$ZIP_NAME"

echo "==> Building App Bundle Zip ($ZIP_NAME)..."
rm -f "$ZIP_PATH"
ditto -c -k --keepParent "$APP_DIR" "$ZIP_PATH"

# 8. Build standalone CLI / portable tarball
TAR_NAME="ferrite-dm-${VERSION}-macos-${ARCH_LABEL}.tar.gz"
TAR_PATH="$DIST_DIR/$TAR_NAME"
TAR_STAGE_NAME="ferrite-dm-${VERSION}-macos-${ARCH_LABEL}"
TAR_STAGE="$DIST_DIR/tar-stage/$TAR_STAGE_NAME"

echo "==> Building Standalone Tarball ($TAR_NAME)..."
rm -rf "$DIST_DIR/tar-stage" "$TAR_PATH"
mkdir -p "$TAR_STAGE"

cp "$BIN_SRC_DIR/fdm-gui" "$TAR_STAGE/"
cp "$BIN_SRC_DIR/ferrite-dm" "$TAR_STAGE/"
cp "$BIN_SRC_DIR/ferrite-native-host" "$TAR_STAGE/"
cp "$SCRIPT_DIR/install-cli.sh" "$TAR_STAGE/install.sh"
cp "$SCRIPT_DIR/uninstall-cli.sh" "$TAR_STAGE/uninstall.sh"
cp "$SCRIPT_DIR/install-cli.sh" "$TAR_STAGE/install-cli.sh"
cp "$SCRIPT_DIR/uninstall-cli.sh" "$TAR_STAGE/uninstall-cli.sh"
cp "$ROOT_DIR/README.md" "$TAR_STAGE/" 2>/dev/null || true
cp "$ROOT_DIR"/LICENSE* "$TAR_STAGE/" 2>/dev/null || true

chmod +x "$TAR_STAGE/install.sh" "$TAR_STAGE/uninstall.sh" "$TAR_STAGE/install-cli.sh" "$TAR_STAGE/uninstall-cli.sh" "$TAR_STAGE/fdm-gui" "$TAR_STAGE/ferrite-dm" "$TAR_STAGE/ferrite-native-host"

tar -czf "$TAR_PATH" -C "$DIST_DIR/tar-stage" "$TAR_STAGE_NAME"
rm -rf "$DIST_DIR/tar-stage"

# 9. Compute Checksums
echo "==> Generating SHA256 checksums..."
cd "$DIST_DIR"
LC_ALL=C shasum -a 256 "$DMG_NAME" "$PKG_NAME" "$TAR_NAME" "$ZIP_NAME" > SHA256SUMS.txt
cd "$ROOT_DIR"

echo ""
echo "=========================================================="
echo " Packaging Complete! Generated Install Binaries:"
echo "=========================================================="
ls -lh "$DIST_DIR"
echo "=========================================================="
cat "$DIST_DIR/SHA256SUMS.txt"
echo "=========================================================="
