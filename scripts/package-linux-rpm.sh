#!/bin/bash
set -euo pipefail

# Ferrite DM Fedora / RHEL / openSUSE (.rpm) Packaging Script
# Generates RPM spec and builds .rpm packages with full desktop integration.

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
    x86_64|amd64)
        RPM_ARCH="x86_64"
        RUST_TARGET="x86_64-unknown-linux-gnu"
        ;;
    aarch64|arm64)
        RPM_ARCH="aarch64"
        RUST_TARGET="aarch64-unknown-linux-gnu"
        ;;
    *)
        RPM_ARCH="$ARCH"
        RUST_TARGET=""
        ;;
esac

cd "$ROOT_DIR"

RPM_TOP_DIR="$DIST_DIR/rpmbuild"
if [ "$CLEAN" = true ]; then
    echo "==> Cleaning rpm build artifacts..."
    rm -rf "$RPM_TOP_DIR" "$DIST_DIR"/*.rpm
fi

mkdir -p "$DIST_DIR"

# 1. Build release binaries if requested
if [ "$SKIP_BUILD" = false ]; then
    echo "==> Building release binaries for Linux ($RPM_ARCH)..."
    if [ -n "$RUST_TARGET" ] && [ "$(uname -s)" = "Linux" ]; then
        cargo build --release --locked -p fdm-gui -p fdm-cli -p fdm-native-host
    else
        cargo build --release -p fdm-gui -p fdm-cli -p fdm-native-host
    fi
fi

# Locate binaries in target directory
BIN_SRC_DIR=""
for candidate in "target/release" "target/$RUST_TARGET/release"; do
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

# 2. Setup rpmbuild directory structure
rm -rf "$RPM_TOP_DIR"
mkdir -p "$RPM_TOP_DIR"/{BUILD,RPMS,SOURCES,SPECS,SRPMS,BUILDROOT,STAGE}

PKG_NAME="ferrite-dm"
RELEASE="1"
RPM_STAGE_DIR="$RPM_TOP_DIR/STAGE"
mkdir -p "$RPM_STAGE_DIR/usr/bin"
mkdir -p "$RPM_STAGE_DIR/usr/share/applications"
mkdir -p "$RPM_STAGE_DIR/usr/share/metainfo"
mkdir -p "$RPM_STAGE_DIR/usr/share/doc/$PKG_NAME"
mkdir -p "$RPM_STAGE_DIR/usr/share/licenses/$PKG_NAME"

# Copy binaries
if [ -f "$BIN_SRC_DIR/fdm-gui" ]; then
    install -m 755 "$BIN_SRC_DIR/fdm-gui" "$RPM_STAGE_DIR/usr/bin/fdm-gui"
fi
install -m 755 "$BIN_SRC_DIR/ferrite-dm" "$RPM_STAGE_DIR/usr/bin/ferrite-dm"
install -m 755 "$BIN_SRC_DIR/ferrite-native-host" "$RPM_STAGE_DIR/usr/bin/ferrite-native-host"

# Copy desktop and AppStream metadata
install -m 644 "$RESOURCES_DIR/linux/ferrite-dm.desktop" "$RPM_STAGE_DIR/usr/share/applications/ferrite-dm.desktop"
install -m 644 "$RESOURCES_DIR/linux/io.ferrite_dm.FerriteDM.metainfo.xml" "$RPM_STAGE_DIR/usr/share/metainfo/io.ferrite_dm.FerriteDM.metainfo.xml"

# Copy icons
ICONS_SRC_DIR="$RESOURCES_DIR/linux/icons/hicolor"
for size in 16 32 48 64 128 256 512 1024; do
    DEST_ICON_DIR="$RPM_STAGE_DIR/usr/share/icons/hicolor/${size}x${size}/apps"
    mkdir -p "$DEST_ICON_DIR"
    if [ -f "$ICONS_SRC_DIR/${size}x${size}/apps/ferrite-dm.png" ]; then
        install -m 644 "$ICONS_SRC_DIR/${size}x${size}/apps/ferrite-dm.png" "$DEST_ICON_DIR/ferrite-dm.png"
    fi
done

if [ -f "$ICONS_SRC_DIR/scalable/apps/ferrite-dm.svg" ]; then
    mkdir -p "$RPM_STAGE_DIR/usr/share/icons/hicolor/scalable/apps"
    install -m 644 "$ICONS_SRC_DIR/scalable/apps/ferrite-dm.svg" "$RPM_STAGE_DIR/usr/share/icons/hicolor/scalable/apps/ferrite-dm.svg"
fi

# Copy licenses and documentation
install -m 644 "$ROOT_DIR/LICENSE-MIT" "$RPM_STAGE_DIR/usr/share/licenses/$PKG_NAME/LICENSE-MIT" 2>/dev/null || true
install -m 644 "$ROOT_DIR/LICENSE-APACHE" "$RPM_STAGE_DIR/usr/share/licenses/$PKG_NAME/LICENSE-APACHE" 2>/dev/null || true
install -m 644 "$ROOT_DIR/README.md" "$RPM_STAGE_DIR/usr/share/doc/$PKG_NAME/README.md" 2>/dev/null || true

# 3. Create Binary RPM Spec file
RPM_SPEC_FILE="$RPM_TOP_DIR/SPECS/ferrite-dm.spec"
cat << EOF > "$RPM_SPEC_FILE"
%define _topdir $RPM_TOP_DIR
%define _buildrootdir $RPM_TOP_DIR/BUILDROOT
%define _rpmdir $DIST_DIR
%global debug_package %{nil}
%global _build_id_links none
%global __os_install_post %{nil}
%global __strip /bin/true

Name:           $PKG_NAME
Version:        $VERSION
Release:        $RELEASE%{?dist}
Summary:        High-performance, multi-protocol download manager and accelerator
License:        MIT or Apache-2.0
URL:            https://github.com/itsarian/ferrite-dm
BuildArch:      $RPM_ARCH

Requires:       openssl-libs
Requires:       fontconfig
Requires:       libX11
Requires:       libxkbcommon
Requires:       libxkbcommon-x11
Requires:       wayland-client
Recommends:     ffmpeg
Recommends:     yt-dlp
Recommends:     xdg-utils

%description
Ferrite DM is a fast, telemetry-free download accelerator written in Rust.
It combines an ultra-fast asynchronous core with a modern, GPU-accelerated
graphical interface designed to replace legacy tools like Free Download Manager.

%install
rm -rf %{buildroot}
mkdir -p %{buildroot}
cp -a "$RPM_STAGE_DIR/." "%{buildroot}/"

%post
if [ \$1 -eq 1 ]; then
    command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q /usr/share/applications || :
    command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor || :
fi

%postun
if [ \$1 -eq 0 ]; then
    command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q /usr/share/applications || :
    command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor || :
fi

%files
%license /usr/share/licenses/$PKG_NAME/*
%doc /usr/share/doc/$PKG_NAME/*
%{_bindir}/*
%{_datadir}/applications/ferrite-dm.desktop
%{_datadir}/metainfo/io.ferrite_dm.FerriteDM.metainfo.xml
%{_datadir}/icons/hicolor/*/apps/ferrite-dm.*

%changelog
* Tue Nov 17 2026 Ferrite DM Maintainers <https://github.com/itsarian/ferrite-dm> - $VERSION-1
- Release $VERSION: Linux GUI and multi-distribution packaging
EOF

# 4. Build RPM with rpmbuild if available
echo "==> Building RPM package..."
if command -v rpmbuild >/dev/null 2>&1; then
    rpmbuild -bb \
        --define "_topdir $RPM_TOP_DIR" \
        --define "_rpmdir $DIST_DIR" \
        "$RPM_SPEC_FILE"

    # Flatten rpm output if placed in architecture subdirectory
    if [ -d "$DIST_DIR/$RPM_ARCH" ]; then
        mv "$DIST_DIR/$RPM_ARCH"/*.rpm "$DIST_DIR/"
        rmdir "$DIST_DIR/$RPM_ARCH" 2>/dev/null || true
    fi

    echo "==> Successfully created RPM package in $DIST_DIR:"
    ls -lh "$DIST_DIR"/*.rpm
else
    echo "==> Note: rpmbuild not installed on current host."
    echo "==> Generated RPM spec at: $RPM_SPEC_FILE"
    echo "==> Staged payload at: $RPM_STAGE_DIR"
    echo "==> To build on a system with rpm-build installed: rpmbuild -bb $RPM_SPEC_FILE"
fi
