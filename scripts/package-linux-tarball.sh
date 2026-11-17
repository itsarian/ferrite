#!/bin/bash
set -euo pipefail

# Ferrite DM Linux Standalone Tarball Packaging Script
# Bundles binaries, desktop integration assets, icons, and installers into .tar.gz.

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

if [ "$CLEAN" = true ]; then
    echo "==> Cleaning linux stage..."
    rm -rf "$DIST_DIR/linux-stage" "$DIST_DIR"/ferrite-dm-*-linux-*.tar.gz
fi

mkdir -p "$DIST_DIR"

# 1. Build release binaries if requested
if [ "$SKIP_BUILD" = false ]; then
    echo "==> Building release binaries for Linux ($ARCH)..."
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

# 2. Stage files
STAGE_DIR="$DIST_DIR/linux-stage/ferrite-dm-${VERSION}"
rm -rf "$STAGE_DIR"
mkdir -p "$STAGE_DIR/resources/linux/icons"

# Copy binaries
if [ -f "$BIN_SRC_DIR/fdm-gui" ]; then
    cp "$BIN_SRC_DIR/fdm-gui" "$STAGE_DIR/"
fi
cp "$BIN_SRC_DIR/ferrite-dm" "$STAGE_DIR/"
cp "$BIN_SRC_DIR/ferrite-native-host" "$STAGE_DIR/"

# Copy installers
cp "$SCRIPT_DIR/install-linux.sh" "$STAGE_DIR/install.sh"
cp "$SCRIPT_DIR/uninstall-linux.sh" "$STAGE_DIR/uninstall.sh"
cp "$SCRIPT_DIR/install-linux.sh" "$STAGE_DIR/install-linux.sh"
cp "$SCRIPT_DIR/uninstall-linux.sh" "$STAGE_DIR/uninstall-linux.sh"
cp "$SCRIPT_DIR/install-cli.sh" "$STAGE_DIR/install-cli.sh"
cp "$SCRIPT_DIR/uninstall-cli.sh" "$STAGE_DIR/uninstall-cli.sh"

# Copy desktop and icon assets
cp "$RESOURCES_DIR/linux/ferrite-dm.desktop" "$STAGE_DIR/resources/linux/"
cp "$RESOURCES_DIR/linux/io.ferrite_dm.FerriteDM.metainfo.xml" "$STAGE_DIR/resources/linux/"
cp -r "$RESOURCES_DIR/linux/icons/hicolor" "$STAGE_DIR/resources/linux/icons/"

# Copy documentation and licenses
cp "$ROOT_DIR/README.md" "$STAGE_DIR/" 2>/dev/null || true
cp "$ROOT_DIR/LICENSE-MIT" "$STAGE_DIR/" 2>/dev/null || true
cp "$ROOT_DIR/LICENSE-APACHE" "$STAGE_DIR/" 2>/dev/null || true

# Set permissions
chmod +x "$STAGE_DIR"/install*.sh "$STAGE_DIR"/uninstall*.sh "$STAGE_DIR"/ferrite-dm "$STAGE_DIR"/ferrite-native-host
if [ -f "$STAGE_DIR/fdm-gui" ]; then
    chmod +x "$STAGE_DIR/fdm-gui"
fi

# 3. Create tarball
TAR_NAME="ferrite-dm-${VERSION}-linux-${ARCH}.tar.gz"
echo "==> Creating tarball $DIST_DIR/$TAR_NAME..."
tar -czf "$DIST_DIR/$TAR_NAME" -C "$DIST_DIR/linux-stage" "ferrite-dm-${VERSION}"

echo "==> Successfully created Linux standalone tarball: $DIST_DIR/$TAR_NAME"
ls -lh "$DIST_DIR/$TAR_NAME"
