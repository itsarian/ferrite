#!/bin/bash
set -euo pipefail

# Ferrite DM Debian / Ubuntu (.deb) Packaging Script
# Creates compliant .deb package with desktop integration, MIME types, and icons.

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

# Map architecture name to canonical asset arch & Debian conventions
case "$ARCH" in
    x86_64|amd64)
        ASSET_ARCH="x86_64"
        DEB_ARCH="amd64"
        RUST_TARGET="x86_64-unknown-linux-gnu"
        ;;
    aarch64|arm64)
        ASSET_ARCH="arm64"
        DEB_ARCH="arm64"
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
    echo "==> Cleaning deb build artifacts..."
    rm -rf "$DIST_DIR/deb-staging" "$DIST_DIR"/*.deb
fi

mkdir -p "$DIST_DIR"

# 1. Build release binaries if requested
if [ "$SKIP_BUILD" = false ]; then
    echo "==> Building release binaries for Linux (${ASSET_ARCH} / ${DEB_ARCH})..."
    if [ "$(uname -s)" = "Linux" ] && [ "$HOST_ASSET_ARCH" = "$ASSET_ARCH" ]; then
        cargo build --release --locked -p fdm-gui -p fdm-cli -p fdm-native-host
    else
        cargo build --release --locked --target "$RUST_TARGET" -p fdm-gui -p fdm-cli -p fdm-native-host
    fi
fi

# Locate binaries in target directory (prefer target/<RUST_TARGET>/release over target/release)
BIN_SRC_DIR=""
for candidate in "target/${RUST_TARGET}/release" "target/release"; do
    if [ -f "$ROOT_DIR/$candidate/ferrite-dm" ]; then
        BIN_SRC_DIR="$ROOT_DIR/$candidate"
        break
    fi
done

if [ -z "$BIN_SRC_DIR" ]; then
    echo "Error: Release binaries not found in target/${RUST_TARGET}/release or target/release. Run without --skip-build." >&2
    exit 1
fi

echo "==> Using binaries from $BIN_SRC_DIR"

# 2. Setup staging tree
PKG_NAME="ferrite-dm"
STAGE_DIR="$DIST_DIR/deb-staging/${PKG_NAME}-${ASSET_ARCH}"
rm -rf "$STAGE_DIR"
mkdir -p "$STAGE_DIR/usr/bin"
mkdir -p "$STAGE_DIR/usr/share/applications"
mkdir -p "$STAGE_DIR/usr/share/metainfo"
mkdir -p "$STAGE_DIR/usr/share/doc/$PKG_NAME"
mkdir -p "$STAGE_DIR/DEBIAN"

# Copy binaries
if [ -f "$BIN_SRC_DIR/fdm-gui" ]; then
    install -m 755 "$BIN_SRC_DIR/fdm-gui" "$STAGE_DIR/usr/bin/fdm-gui"
fi
install -m 755 "$BIN_SRC_DIR/ferrite-dm" "$STAGE_DIR/usr/bin/ferrite-dm"
install -m 755 "$BIN_SRC_DIR/ferrite-native-host" "$STAGE_DIR/usr/bin/ferrite-native-host"

# Copy Freedesktop .desktop file & AppStream metainfo
install -m 644 "$RESOURCES_DIR/linux/ferrite-dm.desktop" "$STAGE_DIR/usr/share/applications/ferrite-dm.desktop"
install -m 644 "$RESOURCES_DIR/linux/io.ferrite_dm.FerriteDM.metainfo.xml" "$STAGE_DIR/usr/share/metainfo/io.ferrite_dm.FerriteDM.metainfo.xml"

# Copy hicolor icon hierarchy
ICONS_SRC_DIR="$RESOURCES_DIR/linux/icons/hicolor"
for size in 16 32 48 64 128 256 512 1024; do
    DEST_ICON_DIR="$STAGE_DIR/usr/share/icons/hicolor/${size}x${size}/apps"
    mkdir -p "$DEST_ICON_DIR"
    if [ -f "$ICONS_SRC_DIR/${size}x${size}/apps/ferrite-dm.png" ]; then
        install -m 644 "$ICONS_SRC_DIR/${size}x${size}/apps/ferrite-dm.png" "$DEST_ICON_DIR/ferrite-dm.png"
    fi
done

if [ -f "$ICONS_SRC_DIR/scalable/apps/ferrite-dm.svg" ]; then
    mkdir -p "$STAGE_DIR/usr/share/icons/hicolor/scalable/apps"
    install -m 644 "$ICONS_SRC_DIR/scalable/apps/ferrite-dm.svg" "$STAGE_DIR/usr/share/icons/hicolor/scalable/apps/ferrite-dm.svg"
fi

# Copy licenses and documentation
install -m 644 "$ROOT_DIR/LICENSE-MIT" "$STAGE_DIR/usr/share/doc/$PKG_NAME/copyright" 2>/dev/null || true
install -m 644 "$ROOT_DIR/README.md" "$STAGE_DIR/usr/share/doc/$PKG_NAME/README" 2>/dev/null || true

# Calculate installed size in KB
INSTALLED_SIZE=$(du -sk "$STAGE_DIR/usr" | cut -f1)

# 3. Create DEBIAN/control
cat << EOF > "$STAGE_DIR/DEBIAN/control"
Package: $PKG_NAME
Version: $VERSION
Section: net
Priority: optional
Architecture: $DEB_ARCH
Essential: no
Installed-Size: $INSTALLED_SIZE
Maintainer: Ferrite DM Maintainers <https://github.com/itsarian/ferrite-dm>
Depends: libc6 (>= 2.31), libssl3 | libssl1.1, libx11-6, libwayland-client0, libxkbcommon0, libxkbcommon-x11-0, libfontconfig1
Recommends: ffmpeg, yt-dlp, xdg-utils
Homepage: https://github.com/itsarian/ferrite-dm
Description: High-performance, multi-protocol download manager and accelerator
 Ferrite DM is a fast, telemetry-free download accelerator written in Rust.
 It combines an ultra-fast asynchronous core with a modern, GPU-accelerated
 graphical interface designed to replace legacy download managers.
 Features:
  - High-throughput multi-connection HTTP/HTTPS downloads
  - BitTorrent client with sequential downloading & magnet support
  - Native HLS/DASH video stream capture and segment downloading
  - Dynamic wildcard batch downloads and URL expansion
  - Native browser messaging integration for Chrome, Firefox, Edge
EOF

chmod 644 "$STAGE_DIR/DEBIAN/control"

# 4. Create DEBIAN/postinst
cat << 'EOF' > "$STAGE_DIR/DEBIAN/postinst"
#!/bin/sh
set -e

if [ "$1" = "configure" ]; then
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database -q /usr/share/applications || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor || true
    fi
fi

exit 0
EOF

chmod 755 "$STAGE_DIR/DEBIAN/postinst"

# 5. Create DEBIAN/postrm
cat << 'EOF' > "$STAGE_DIR/DEBIAN/postrm"
#!/bin/sh
set -e

if [ "$1" = "remove" ] || [ "$1" = "purge" ]; then
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database -q /usr/share/applications || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor || true
    fi
fi

exit 0
EOF

chmod 755 "$STAGE_DIR/DEBIAN/postrm"

# 6. Build the .deb package
OUTPUT_DEB="$DIST_DIR/${PKG_NAME}-${VERSION}-linux-${ASSET_ARCH}.deb"
echo "==> Packaging $OUTPUT_DEB (DEBIAN Architecture: $DEB_ARCH)..."

if command -v dpkg-deb >/dev/null 2>&1; then
    dpkg-deb --build --root-owner-group "$STAGE_DIR" "$OUTPUT_DEB"
else
    # Portable fallback using tar and ar
    echo "==> dpkg-deb not found, using portable tar+ar packaging fallback..."
    BUILD_TMP="$(mktemp -d)"
    echo "2.0" > "$BUILD_TMP/debian-binary"

    # Pack control archive
    (cd "$STAGE_DIR/DEBIAN" && tar -czf "$BUILD_TMP/control.tar.gz" --owner=0 --group=0 . 2>/dev/null || tar -czf "$BUILD_TMP/control.tar.gz" .)

    # Pack data archive
    (cd "$STAGE_DIR" && tar -czf "$BUILD_TMP/data.tar.gz" --owner=0 --group=0 usr 2>/dev/null || tar -czf "$BUILD_TMP/data.tar.gz" usr)

    # Archive into .deb using ar (use -crS to avoid ranlib on macOS)
    rm -f "$OUTPUT_DEB"
    ar -crS "$OUTPUT_DEB" "$BUILD_TMP/debian-binary" "$BUILD_TMP/control.tar.gz" "$BUILD_TMP/data.tar.gz"
    rm -rf "$BUILD_TMP"
fi

echo "==> Successfully created Debian package: $OUTPUT_DEB"
ls -lh "$OUTPUT_DEB"
