#!/bin/bash
set -euo pipefail

# Ferrite Linux Standalone Tarball Packaging Script
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
        RUST_TARGET="x86_64-unknown-linux-gnu"
        ;;
    aarch64|arm64)
        ASSET_ARCH="arm64"
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

if [ "$CLEAN" = true ]; then
    echo "==> Cleaning linux stage..."
    rm -rf "$DIST_DIR/linux-stage" "$DIST_DIR"/ferrite-*-linux-*.tar.gz
fi

mkdir -p "$DIST_DIR"

# 1. Build release binaries if requested
if [ "$SKIP_BUILD" = false ]; then
    echo "==> Building release binaries for Linux (${ASSET_ARCH} / ${RUST_TARGET})..."
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

# 2. Stage files
STAGE_NAME="ferrite-${VERSION}-linux-${ASSET_ARCH}"
STAGE_DIR="$DIST_DIR/linux-stage/$STAGE_NAME"
rm -rf "$STAGE_DIR"
mkdir -p "$STAGE_DIR/resources/linux/icons"

# Copy binaries
if [ -f "$BIN_SRC_DIR/ferrite-gui" ]; then
    cp "$BIN_SRC_DIR/ferrite-gui" "$STAGE_DIR/"
fi
cp "$BIN_SRC_DIR/ferrite" "$STAGE_DIR/"
cp "$BIN_SRC_DIR/ferrite-native-host" "$STAGE_DIR/"

# Copy installers
cp "$SCRIPT_DIR/install-linux.sh" "$STAGE_DIR/install.sh"
cp "$SCRIPT_DIR/uninstall-linux.sh" "$STAGE_DIR/uninstall.sh"
cp "$SCRIPT_DIR/install-linux.sh" "$STAGE_DIR/install-linux.sh"
cp "$SCRIPT_DIR/uninstall-linux.sh" "$STAGE_DIR/uninstall-linux.sh"
cp "$SCRIPT_DIR/install-cli.sh" "$STAGE_DIR/install-cli.sh"
cp "$SCRIPT_DIR/uninstall-cli.sh" "$STAGE_DIR/uninstall-cli.sh"

# Copy desktop and icon assets
cp "$RESOURCES_DIR/linux/ferrite.desktop" "$STAGE_DIR/resources/linux/"
cp "$RESOURCES_DIR/linux/io.ferrite.Ferrite.metainfo.xml" "$STAGE_DIR/resources/linux/"
cp -r "$RESOURCES_DIR/linux/icons/hicolor" "$STAGE_DIR/resources/linux/icons/"

# Copy documentation and licenses
cp "$ROOT_DIR/README.md" "$STAGE_DIR/" 2>/dev/null || true
cp "$ROOT_DIR/LICENSE-MIT" "$STAGE_DIR/" 2>/dev/null || true
cp "$ROOT_DIR/LICENSE-APACHE" "$STAGE_DIR/" 2>/dev/null || true

# Set permissions
chmod +x "$STAGE_DIR"/install*.sh "$STAGE_DIR"/uninstall*.sh "$STAGE_DIR"/ferrite "$STAGE_DIR"/ferrite-native-host
if [ -f "$STAGE_DIR/ferrite-gui" ]; then
    chmod +x "$STAGE_DIR/ferrite-gui"
fi

# 3. Create tarball
TAR_NAME="ferrite-${VERSION}-linux-${ASSET_ARCH}.tar.gz"
echo "==> Creating tarball $DIST_DIR/$TAR_NAME..."
tar -czf "$DIST_DIR/$TAR_NAME" -C "$DIST_DIR/linux-stage" "$STAGE_NAME"

echo "==> Successfully created Linux standalone tarball: $DIST_DIR/$TAR_NAME"
ls -lh "$DIST_DIR/$TAR_NAME"
