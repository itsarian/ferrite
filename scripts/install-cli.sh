#!/bin/bash
set -euo pipefail

# Ferrite Installer
# Installs local binaries when run from an extracted release tarball,
# or downloads and verifies the latest (or FERRITE_VERSION) release from GitHub.

DEFAULT_REPO="itsarian/ferrite"
REPO="${FERRITE_REPO:-$DEFAULT_REPO}"

usage() {
    cat <<EOF
Ferrite Installer
Usage: $0 [--user | --system | --prefix <path>]

Options:
  --user         Install for the current user to ~/.local
  --system       Install system-wide to /usr/local (requires root/sudo)
  --prefix PATH  Install into specified directory prefix
  -h, --help     Show this help message

Environment Variables:
  PREFIX           Installation prefix (overridden by flags above)
  FERRITE_VERSION  Specific release tag or version to download (e.g. v0.4.0)
  FERRITE_REPO     GitHub repository owner/name (default: $DEFAULT_REPO)
EOF
}

PREFIX="${PREFIX:-}"
EXPLICIT_PREFIX=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --prefix)
            if [[ $# -lt 2 || -z "$2" ]]; then
                echo "Error: --prefix requires a directory path" >&2
                exit 1
            fi
            PREFIX="$2"
            EXPLICIT_PREFIX=true
            shift 2
            ;;
        --prefix=*)
            PREFIX="${1#*=}"
            if [[ -z "$PREFIX" ]]; then
                echo "Error: --prefix requires a directory path" >&2
                exit 1
            fi
            EXPLICIT_PREFIX=true
            shift
            ;;
        --user)
            PREFIX="$HOME/.local"
            EXPLICIT_PREFIX=true
            shift
            ;;
        --system)
            PREFIX="/usr/local"
            EXPLICIT_PREFIX=true
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "Error: Unknown option: $1" >&2
            usage >&2
            exit 1
            ;;
    esac
done

if [ -z "$PREFIX" ]; then
    PREFIX="/usr/local"
fi

if [ "$EXPLICIT_PREFIX" = false ]; then
    if [ -d "$PREFIX/bin" ]; then
        CHECK_DIR="$PREFIX/bin"
    elif [ -d "$PREFIX" ]; then
        CHECK_DIR="$PREFIX"
    else
        CHECK_DIR="$(dirname "$PREFIX")"
    fi

    if [ ! -w "$CHECK_DIR" ] && [ "${EUID:-$(id -u)}" -ne 0 ]; then
        PREFIX="$HOME/.local"
    fi
fi

BIN_DIR="$PREFIX/bin"
if ! mkdir -p "$BIN_DIR" 2>/dev/null; then
    echo "Error: Cannot create installation directory $BIN_DIR (try --user or run with sudo)." >&2
    exit 1
fi
if [ ! -w "$BIN_DIR" ]; then
    echo "Error: Installation directory $BIN_DIR is not writable (try --user or run with sudo)." >&2
    exit 1
fi

SCRIPT_SOURCE="${BASH_SOURCE[0]:-}"
SCRIPT_DIR=""
ROOT_DIR=""
if [ -n "$SCRIPT_SOURCE" ] && [ -f "$SCRIPT_SOURCE" ]; then
    SCRIPT_DIR="$(cd "$(dirname "$SCRIPT_SOURCE")" && pwd)"
    ROOT_DIR="$(cd "$SCRIPT_DIR/.." 2>/dev/null && pwd || echo "$SCRIPT_DIR")"
fi

TMP_DIR=""
cleanup() {
    if [ -n "$TMP_DIR" ] && [ -d "$TMP_DIR" ]; then
        rm -rf "$TMP_DIR"
    fi
}
trap cleanup EXIT

detect_os() {
    case "$(uname -s)" in
        Darwin) echo "macos" ;;
        Linux)  echo "linux" ;;
        *)
            echo "Error: Unsupported operating system '$(uname -s)'. Supported: macOS (Darwin), Linux." >&2
            exit 1
            ;;
    esac
}

detect_arch() {
    case "$(uname -m)" in
        arm64|aarch64) echo "arm64" ;;
        x86_64|amd64)  echo "x86_64" ;;
        *)
            echo "Error: Unsupported architecture '$(uname -m)'. Supported: arm64, x86_64." >&2
            exit 1
            ;;
    esac
}

verify_sha256() {
    local checksum_file="$1"
    local work_dir="$2"
    if command -v sha256sum >/dev/null 2>&1; then
        (cd "$work_dir" && sha256sum -c "$checksum_file")
    elif command -v shasum >/dev/null 2>&1; then
        (cd "$work_dir" && shasum -a 256 -c "$checksum_file")
    else
        echo "Error: Neither sha256sum nor shasum is installed to verify SHA-256 checksums." >&2
        return 1
    fi
}

install_binary() {
    local src="$1"
    local name="$2"
    install -m 755 "$src" "$BIN_DIR/$name" 2>/dev/null || {
        cp "$src" "$BIN_DIR/$name"
        chmod 755 "$BIN_DIR/$name"
    }
}

OS_NAME="$(detect_os)"
ARCH_NAME="$(detect_arch)"
SRC_DIR=""
RES_ROOT=""
REMOTE_MODE=false

if [ -n "$SCRIPT_DIR" ] && [ -f "$SCRIPT_DIR/ferrite" ]; then
    SRC_DIR="$SCRIPT_DIR"
    RES_ROOT="$ROOT_DIR"
else
    REMOTE_MODE=true

    for tool in curl tar awk; do
        if ! command -v "$tool" >/dev/null 2>&1; then
            echo "Error: Required tool '$tool' is not installed." >&2
            exit 1
        fi
    done
    if ! command -v sha256sum >/dev/null 2>&1 && ! command -v shasum >/dev/null 2>&1; then
        echo "Error: Neither sha256sum nor shasum is available to verify release checksums." >&2
        exit 1
    fi

    if [ -n "${FERRITE_VERSION:-}" ]; then
        VERSION="${FERRITE_VERSION#v}"
        TAG_NAME="v${VERSION}"
    else
        echo "==> Querying latest Ferrite release from GitHub (${REPO})..."
        RELEASE_JSON="$(curl -fsSL -H 'Accept: application/vnd.github+json' "https://api.github.com/repos/${REPO}/releases/latest")" || {
            echo "Error: Failed to query https://api.github.com/repos/${REPO}/releases/latest" >&2
            exit 1
        }
        TAG_NAME="$(printf '%s\n' "$RELEASE_JSON" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
        if [ -z "$TAG_NAME" ]; then
            echo "Error: Could not parse latest release tag from GitHub API response." >&2
            exit 1
        fi
        VERSION="${TAG_NAME#v}"
    fi

    TARBALL="ferrite-${VERSION}-${OS_NAME}-${ARCH_NAME}.tar.gz"
    BASE_URL="https://github.com/${REPO}/releases/download/${TAG_NAME}"
    TMP_DIR="$(mktemp -d)"

    echo "==> Downloading ${TARBALL} (${TAG_NAME})..."
    if ! curl -fsSL -o "$TMP_DIR/$TARBALL" "${BASE_URL}/${TARBALL}"; then
        echo "Error: Failed to download ${BASE_URL}/${TARBALL}" >&2
        exit 1
    fi

    echo "==> Downloading SHA256SUMS.txt..."
    if ! curl -fsSL -o "$TMP_DIR/SHA256SUMS.txt" "${BASE_URL}/SHA256SUMS.txt"; then
        echo "Error: Failed to download ${BASE_URL}/SHA256SUMS.txt" >&2
        exit 1
    fi

    echo "==> Verifying SHA-256 checksum for ${TARBALL}..."
    EXPECTED_ENTRY="$(awk -v target="$TARBALL" '$2 == target || $2 == ("./" target) { print; exit }' "$TMP_DIR/SHA256SUMS.txt")"
    if [ -z "$EXPECTED_ENTRY" ]; then
        echo "Error: No SHA-256 entry found for ${TARBALL} in SHA256SUMS.txt." >&2
        exit 1
    fi
    printf '%s\n' "$EXPECTED_ENTRY" > "$TMP_DIR/expected.sha256"
    if ! verify_sha256 "expected.sha256" "$TMP_DIR"; then
        echo "Error: SHA-256 checksum verification failed for ${TARBALL}. Aborting installation." >&2
        exit 1
    fi

    echo "==> Extracting ${TARBALL}..."
    tar -xzf "$TMP_DIR/$TARBALL" -C "$TMP_DIR"

    EXPECTED_DIR="$TMP_DIR/ferrite-${VERSION}-${OS_NAME}-${ARCH_NAME}"
    if [ -d "$EXPECTED_DIR" ] && [ -f "$EXPECTED_DIR/ferrite" ]; then
        SRC_DIR="$EXPECTED_DIR"
        RES_ROOT="$EXPECTED_DIR"
    else
        FOUND_CLI="$(find "$TMP_DIR" -maxdepth 3 -type f -name ferrite | head -n 1 || true)"
        if [ -z "$FOUND_CLI" ]; then
            echo "Error: Extracted archive did not contain the 'ferrite' binary." >&2
            exit 1
        fi
        SRC_DIR="$(dirname "$FOUND_CLI")"
        RES_ROOT="$SRC_DIR"
    fi
fi

if [ ! -f "$SRC_DIR/ferrite" ]; then
    echo "Error: Required binary 'ferrite' not found in $SRC_DIR." >&2
    exit 1
fi

if [ "$REMOTE_MODE" = true ]; then
    for required_bin in ferrite ferrite-gui ferrite-native-host; do
        if [ ! -f "$SRC_DIR/$required_bin" ]; then
            echo "Error: Expected binary '$required_bin' not found in extracted release archive ($SRC_DIR)." >&2
            exit 1
        fi
    done
fi

echo "==> Installing Ferrite binaries to $BIN_DIR..."

install_binary "$SRC_DIR/ferrite" "ferrite"

if [ -f "$SRC_DIR/ferrite-gui" ]; then
    install_binary "$SRC_DIR/ferrite-gui" "ferrite-gui"
fi

if [ -f "$SRC_DIR/ferrite-native-host" ]; then
    install_binary "$SRC_DIR/ferrite-native-host" "ferrite-native-host"
elif [ "$REMOTE_MODE" = true ]; then
    echo "Error: Required binary 'ferrite-native-host' not found in $SRC_DIR." >&2
    exit 1
fi

# Optional Linux desktop integration
RES_DIR=""
if [ "$OS_NAME" = "linux" ]; then
    for loc in "$SRC_DIR/resources/linux" "$RES_ROOT/resources/linux" "$SRC_DIR"; do
        if [ -f "$loc/ferrite.desktop" ]; then
            RES_DIR="$loc"
            break
        fi
    done
fi

if [ -n "$RES_DIR" ]; then
    DATA_DIR="$PREFIX/share"
    mkdir -p "$DATA_DIR/applications" "$DATA_DIR/metainfo"

    if [ -f "$RES_DIR/ferrite.desktop" ]; then
        sed -e "s|^Exec=ferrite-gui|Exec=$BIN_DIR/ferrite-gui|" \
            -e "s|^Exec=ferrite |Exec=$BIN_DIR/ferrite |" \
            "$RES_DIR/ferrite.desktop" > "$DATA_DIR/applications/ferrite.desktop"
        chmod 644 "$DATA_DIR/applications/ferrite.desktop"
    fi

    if [ -f "$RES_DIR/io.ferrite.Ferrite.metainfo.xml" ]; then
        install -m 644 "$RES_DIR/io.ferrite.Ferrite.metainfo.xml" "$DATA_DIR/metainfo/io.ferrite.Ferrite.metainfo.xml"
    fi

    if [ -d "$RES_DIR/icons/hicolor" ]; then
        for size in 16 32 48 64 128 256 512 1024; do
            SRC_ICON="$RES_DIR/icons/hicolor/${size}x${size}/apps/ferrite.png"
            if [ -f "$SRC_ICON" ]; then
                mkdir -p "$DATA_DIR/icons/hicolor/${size}x${size}/apps"
                install -m 644 "$SRC_ICON" "$DATA_DIR/icons/hicolor/${size}x${size}/apps/ferrite.png"
            fi
        done
        if [ -f "$RES_DIR/icons/hicolor/scalable/apps/ferrite.svg" ]; then
            mkdir -p "$DATA_DIR/icons/hicolor/scalable/apps"
            install -m 644 "$RES_DIR/icons/hicolor/scalable/apps/ferrite.svg" "$DATA_DIR/icons/hicolor/scalable/apps/ferrite.svg"
        fi
    fi

    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "$DATA_DIR/applications" 2>/dev/null || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        gtk-update-icon-cache -q -t -f "$DATA_DIR/icons/hicolor" 2>/dev/null || true
    fi
fi

if [ ! -x "$BIN_DIR/ferrite" ]; then
    echo "Error: Installation failed — $BIN_DIR/ferrite is missing or not executable." >&2
    exit 1
fi

echo ""
echo "Ferrite installed successfully!"
if [ -x "$BIN_DIR/ferrite-gui" ]; then
    echo "  GUI:         $BIN_DIR/ferrite-gui"
fi
echo "  CLI/Daemon:  $BIN_DIR/ferrite"
if [ -x "$BIN_DIR/ferrite-native-host" ]; then
    echo "  Native Host: $BIN_DIR/ferrite-native-host"
fi
echo ""
echo "To register the browser native messaging host, run:"
echo "  $BIN_DIR/ferrite native-host-install"
if [[ ":$PATH:" != *":$BIN_DIR:"* ]]; then
    echo ""
    echo "Notice: $BIN_DIR is not in your PATH. Add it to your shell profile:"
    echo "  export PATH=\"$BIN_DIR:\$PATH\""
fi
