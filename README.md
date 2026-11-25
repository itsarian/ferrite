<div align="center">
  <img src="resources/Ferrite-1024.png" width="100" height="100" alt="Ferrite DM Logo" />

  <h1>Ferrite DM</h1>

  <p><strong>A modern, native download manager built for speed, control, and privacy.</strong></p>
  <p>Rust-powered desktop interface (<a href="https://github.com/zed-industries/zed/tree/main/crates/gpui">GPUI</a>), scriptable CLI, and background daemon — built around one unified transfer engine.</p>

  [![Release](https://img.shields.io/github/v/release/itsarian/ferrite-dm?style=flat-square)](https://github.com/itsarian/ferrite-dm/releases)
  [![CI](https://img.shields.io/github/actions/workflow/status/itsarian/ferrite-dm/ci.yml?branch=main&label=CI&style=flat-square)](https://github.com/itsarian/ferrite-dm/actions/workflows/ci.yml)
  [![Security Audit](https://img.shields.io/github/actions/workflow/status/itsarian/ferrite-dm/audit.yml?branch=main&label=Security%20Audit&style=flat-square)](https://github.com/itsarian/ferrite-dm/actions/workflows/audit.yml)
  [![License](https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-blue?style=flat-square)](#license)
  <br />

  <a href="https://github.com/itsarian/ferrite-dm/releases">Download</a> •
  <a href="#features">Features</a> •
  <a href="#screenshots">Screenshots</a> •
  <a href="#quick-start">Quick Start</a> •
  <a href="#installation">Installation</a> •
  <a href="#architecture">Architecture</a> •
  <a href="#supported-protocols">Protocols</a> •
  <a href="#cli-reference">CLI Reference</a> •
  <a href="#roadmap">Roadmap</a>
</div>

<br />

<div align="center">
  <img src="resources/screenshot.png" width="100%" alt="Ferrite DM native GPUI desktop interface running on macOS" />
  <p><em>Native GPUI desktop interface with real-time transfer throughput and the 6-tab diagnostic inspector (<a href="resources/demo.mp4">watch 14s demo clip</a>).</em></p>
</div>

---

## Features

- **Zero Telemetry, Local-First:** All download state, chunk maps, and mirror metadata are stored locally in SQLite with Write-Ahead Logging (WAL). No analytics, no ads, no cloud relay.
- **Native GPUI Desktop Interface:** Sub-millisecond GPU rendering via Zed's GPUI framework — zero Electron or Chromium overhead. Includes a 6-tab diagnostic inspector (`GENERAL`, `DETAILS`, `FILES`, `PROGRESS`, `CONNECTIONS`, `SPEED`), menu bar item, Dock progress badges, sleep prevention (`caffeinate`), and system notifications.
- **Multi-Protocol Acceleration:** Segmented HTTP(S)/FTP with mirror fallback, full BitTorrent (`librqbit`) with sequential "Play ASAP" streaming, and HLS (`.m3u8`) / MPEG-DASH (`.mpd`) streams with AES-128 decryption.
- **Traffic Usage Modes (TUM):** Instant bandwidth throttling profiles (`snail`, `low`, `medium`, `high`).
- **Wildcard Batch Expansion:** Expand URL range patterns like `https://example.com/part-[01-20:2].zip` and brace lists like `{linux,macos}`.
- **Post-Processing & Utilities:** FFmpeg integration (`ferrite-dm mp3`, `ferrite-dm mp4`) and cryptographic hash verification (`ferrite-dm hash` for MD5, SHA-1, SHA-256, SHA-512).
- **Browser & LAN Remote Control:** WebExtension Native Messaging host (`ferrite-native-host`) for Chrome, Firefox, Edge, Brave, Opera, and Vivaldi, plus a built-in self-signed HTTPS + WebSocket server with QR-code pairing.

---

## Screenshots

<table>
  <tr>
    <td width="33%" align="center">
      <img src="resources/screenshot-progress.png" alt="Inspector Progress Chunk Map" />
      <br />
      <em>Inspector — Piece &amp; Segment Map</em>
    </td>
    <td width="33%" align="center">
      <img src="resources/screenshot-preferences.png" alt="Preferences and Traffic Limits" />
      <br />
      <em>Preferences — Appearance &amp; Limits</em>
    </td>
    <td width="33%" align="center">
      <img src="resources/screenshot-cli.png" alt="Ferrite DM CLI Output" />
      <br />
      <em>CLI — Queue &amp; Traffic Modes</em>
    </td>
  </tr>
</table>

---

## Quick Start

### 1. Install or run from terminal

```bash
# macOS installer via script (or download .dmg / .pkg from Releases)
curl -fsSL https://raw.githubusercontent.com/itsarian/ferrite-dm/main/scripts/install-cli.sh | bash
```

### 2. Download via CLI

```bash
# Direct download (runs standalone, or hands off to the GUI/daemon if open)
ferrite-dm https://example.com/archive.tar.gz

# Download with mirror fallback, priority, and wait until complete
ferrite-dm add "https://cdn1.example.com/installer.dmg" \
  --mirror "https://cdn2.example.com/installer.dmg" \
  --priority high \
  --wait
```

### 3. Connect your browser

Integrate one-click download capture with Chrome, Firefox, Edge, Brave, Opera, or Vivaldi:

```bash
ferrite-dm native-host-install --browser all
```

> [!NOTE]
> When you click a download link in your browser, the native host automatically spawns `ferrite-dm daemon` in the background if neither the GUI nor daemon is already running.

---

## Installation

### macOS (Apple Silicon & Intel)

Download pre-built installers from [GitHub Releases](https://github.com/itsarian/ferrite-dm/releases) (`arm64` for Apple Silicon or `x86_64` for Intel):

- **Disk Image (`.dmg`):** Open `ferrite-dm-<version>-macos-arm64.dmg` (Apple Silicon) or `ferrite-dm-<version>-macos-x86_64.dmg` (Intel) and drag **Ferrite DM.app** to `/Applications`.
- **Installer Package (`.pkg`):** Run `ferrite-dm-<version>-macos-arm64.pkg` or `ferrite-dm-<version>-macos-x86_64.pkg` to install the desktop app and configure CLI symlinks (`fdm-gui`, `ferrite-dm`, and `ferrite-native-host`) in `/usr/local/bin`.
- **Standalone Archives (`.zip` / `.tar.gz`):** Compressed `Ferrite DM.app` bundle (`ferrite-dm-<version>-macos-<arch>.zip`) and portable CLI & GUI bundle (`ferrite-dm-<version>-macos-<arch>.tar.gz`).

Or install via terminal script:
```bash
curl -fsSL https://raw.githubusercontent.com/itsarian/ferrite-dm/main/scripts/install-cli.sh | bash
```

### Linux (GUI & CLI — x86_64 & ARM64)

Download the appropriate package for your architecture (`x86_64` or `arm64`) from [GitHub Releases](https://github.com/itsarian/ferrite-dm/releases):

#### Debian / Ubuntu (`.deb`)
```bash
# x86_64 (amd64)
sudo apt install ./ferrite-dm-<version>-linux-x86_64.deb

# ARM64 (aarch64)
sudo apt install ./ferrite-dm-<version>-linux-arm64.deb
```

#### Fedora / RHEL / openSUSE (`.rpm`)
```bash
# Fedora / RHEL (dnf) — replace <arch> with x86_64 or arm64
sudo dnf install ./ferrite-dm-<version>-linux-<arch>.rpm

# openSUSE (zypper) — replace <arch> with x86_64 or arm64
sudo zypper install ./ferrite-dm-<version>-linux-<arch>.rpm
```

#### Portable Linux AppImage (`.AppImage`)
```bash
# x86_64
chmod +x ferrite-dm-<version>-linux-x86_64.AppImage
./ferrite-dm-<version>-linux-x86_64.AppImage

# ARM64 (aarch64)
chmod +x ferrite-dm-<version>-linux-arm64.AppImage
./ferrite-dm-<version>-linux-arm64.AppImage
```
No installation required. Runs on x86_64 and ARM64 (`aarch64`) Linux with glibc ≥ 2.31.

#### Arch Linux (AUR — `x86_64` & `aarch64`)
```bash
yay -S ferrite-dm
```

#### Standalone Tarball (`.tar.gz`)
```bash
# Replace <arch> with x86_64 or arm64
tar -xzf ferrite-dm-<version>-linux-<arch>.tar.gz
cd ferrite-dm-<version>-linux-<arch>/
./install.sh   # Installs fdm-gui, ferrite-dm, and ferrite-native-host into ~/.local/bin
```

#### Linux Runtime Dependencies

The GUI (`fdm-gui`) requires the following native libraries at runtime:

| Distro | Install Command |
| :--- | :--- |
| **Debian / Ubuntu** | `sudo apt install libx11-6 libxkbcommon0 libxkbcommon-x11-0 libwayland-client0 libfontconfig1 libasound2 libpulse0` |
| **Fedora / RHEL** | `sudo dnf install libX11 libxkbcommon libxkbcommon-x11 wayland-libs fontconfig alsa-lib pulseaudio-libs` |
| **Arch** | `sudo pacman -Sy libx11 libxkbcommon wayland fontconfig alsa-lib libpulse` |

The `.deb` and `.rpm` packages declare these as package-manager dependencies and install them automatically.

### Building from Source

Requires Rust Edition 2024 (`rustc >= 1.85`), a C compiler (`clang` or `gcc`), and `cmake`:

```bash
# Clone the repository
git clone https://github.com/itsarian/ferrite-dm.git
cd ferrite-dm

# Linux only: install GUI native libraries before building
# Debian / Ubuntu:
sudo apt-get install -y cmake pkg-config libssl-dev \
  libx11-dev libxkbcommon-dev libxkbcommon-x11-dev \
  libwayland-dev libfontconfig1-dev libasound2-dev libpulse-dev

# Build all workspace binaries in release mode
cargo build --release --workspace

# Run the offline loopback self-test suite
./target/release/ferrite-dm self-test
```

Compiled binaries in `target/release/`:
- `fdm-gui` — Native GPUI desktop application.
- `ferrite-dm` — CLI client and background engine daemon.
- `ferrite-native-host` — Browser Native Messaging host.

### Prerequisites

- **macOS:** macOS 11+ (Big Sur or newer). Native builds for Apple Silicon (`arm64`) and Intel (`x86_64`).
- **Linux:** `x86_64` or ARM64 (`aarch64`) Linux distribution with glibc 2.31+. The GUI (`fdm-gui`) requires an X11 or Wayland display server and native libraries (automatically handled by `.deb`/`.rpm` packages; see [Linux Runtime Dependencies](#linux-runtime-dependencies) for other install methods).
- **Optional Dependencies:** [FFmpeg](https://ffmpeg.org/) (for audio/video conversion) and [yt-dlp](https://github.com/yt-dlp/yt-dlp) (for extended media URL resolution).

---

## Architecture

Ferrite DM is structured as a modular 10-crate Rust workspace. The GPUI desktop app (`fdm-gui`) and background daemon (`ferrite-dm daemon`) expose a shared Unix domain socket (`ferrite.sock`) and optional LAN HTTPS/WebSocket server, allowing the CLI and browser native host to hand off and inspect transfers seamlessly. When no daemon or GUI is running, the CLI can also execute transfers directly against the core engine.

```mermaid
flowchart TD
    GUI["Desktop GUI · GPUI (fdm-gui)"] --> IPC["IPC & LAN Remote (fdm-ipc · ferrite.sock)"]
    CLI["CLI (ferrite-dm)"] --> IPC
    CLI -. "standalone mode" .-> CORE
    NH["Browser Native Host (ferrite-native-host)"] --> IPC
    IPC --> CORE["Core Engine & Persistence (fdm-core)"]
    GUI --> CORE
    CORE --> HTTP["HTTP(S) / FTP (fdm-http)"]
    CORE --> BT["BitTorrent · librqbit (fdm-bt)"]
    CORE --> HLS["HLS / DASH (fdm-hls)"]
    CORE --> BATCH["Batch & Media (fdm-batch · fdm-media)"]
    CORE --> DB[("SQLite · WAL (db.sqlite)")]
```

### Built with GPUI

Ferrite DM's desktop interface (`fdm-gui`) is built with [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui), Zed's GPU-accelerated UI framework.

Ferrite is an independent project exploring GPUI outside of a code editor — applying it to a native download manager with live transfer graphs, a 6-tab diagnostic inspector, custom chunk maps, background daemon IPC, and native macOS/Linux integrations.

> [!NOTE]
> Ferrite DM is an independent open-source project and is not affiliated with Zed Industries.

### Workspace Crates

| Crate | What it holds |
| :--- | :--- |
| [`crates/fdm-core`](crates/fdm-core) | SQLite WAL database persistence, data models, token bucket rate limiter, path routing, and OS sleep prevention |
| [`crates/fdm-http`](crates/fdm-http) | Multi-connection segmented HTTP/HTTPS and FTP engine with mirror management |
| [`crates/fdm-bt`](crates/fdm-bt) | BitTorrent engine wrapping `librqbit` with sequential "Play ASAP" streaming |
| [`crates/fdm-hls`](crates/fdm-hls) | HLS (`.m3u8`) and MPEG-DASH (`.mpd`) stream parser with AES-128 segment decryption |
| [`crates/fdm-batch`](crates/fdm-batch) | Wildcard pattern parser expanding numeric ranges and brace expansions |
| [`crates/fdm-media`](crates/fdm-media) | Embedded Boa JavaScript engine for media plugins and FFmpeg conversion helpers |
| [`crates/fdm-ipc`](crates/fdm-ipc) | Unix domain socket protocol (`ferrite.sock`) and LAN Remote Control web server |
| [`crates/fdm-native-host`](crates/fdm-native-host) | WebExtension Native Messaging protocol host executable for web browser capture |
| [`crates/fdm-cli`](crates/fdm-cli) | Command-line interface executable (`ferrite-dm`) and background daemon supervisor |
| [`crates/fdm-gui`](crates/fdm-gui) | Native GPUI desktop application interface (`fdm-gui` / `Ferrite DM.app`) |
| [`docs/CI_CD.md`](docs/CI_CD.md) | Continuous Integration, security audit, release pipelines, and contributor pre-flight guide |
| [`scripts/`](scripts/) | Packaging scripts (`package-macos.sh`, `package-linux-*.sh`), installers, `PKGBUILD`, and icon generation |
| [`resources/`](resources/) | Application icon (`Ferrite-1024.png`, `Ferrite.icns`), screenshots, demo video, and `Info.plist` |

---

## Supported Protocols

| Protocol | Crate | Capabilities |
| :--- | :--- | :--- |
| **HTTP / HTTPS / FTP** | `fdm-http` | Multi-connection segmented transfers, dynamic chunk splitting, mirror fallback, ETag/range resumption |
| **BitTorrent** | `fdm-bt` | Magnet links, `.torrent` files, DHT, PEX, LSD, uTP, selective file downloads, sequential "Play ASAP" streaming |
| **HLS & MPEG-DASH** | `fdm-hls` | `.m3u8` and `.mpd` playlist parsing, parallel segment downloads, AES-128 segment decryption |
| **Batch & Media** | `fdm-batch`, `fdm-media` | Numeric range & brace URL expansion, Boa JS media site plugins, FFmpeg MP3/MP4 post-processing |

---

## CLI Reference

When `fdm-gui` or `ferrite-dm daemon` is running, CLI commands automatically communicate through the Unix domain socket `ferrite.sock`. When run standalone, commands operate directly against the local SQLite database.

```bash
# ---------------------------------------------------------------------------
# Adding Downloads
# ---------------------------------------------------------------------------
# Direct download with mirror fallback and priority
ferrite-dm add "https://cdn1.example.com/installer.dmg" \
  --mirror "https://cdn2.example.com/installer.dmg" \
  --dir ~/Downloads/Installers \
  --priority high \
  --wait

# Expand batch pattern (numeric ranges with step sizes, brace lists)
ferrite-dm batch "https://example.com/parts/part-[01-20:2].zip" --dir ~/Downloads/Parts
ferrite-dm batch "https://example.com/builds/v[1-3]-{linux,macos}.tar.gz" --preview-only

# Inspect torrent metadata or stream sequential video files
ferrite-dm torrent ~/Downloads/sample.torrent --inspect
ferrite-dm torrent "magnet:?xt=urn:btih:..." --select-files 0,2 --play-asap

# ---------------------------------------------------------------------------
# Queue Management
# ---------------------------------------------------------------------------
ferrite-dm list                         # View active downloads table
ferrite-dm list --json                  # Output queue in JSON format
ferrite-dm pause 1                      # Pause download #1
ferrite-dm resume 1                     # Resume download #1
ferrite-dm cancel 1                     # Cancel download #1
ferrite-dm remove 1 --delete-files      # Remove download and delete files from disk
ferrite-dm start-all                    # Start all queued downloads
ferrite-dm pause-all                    # Pause all active downloads

# ---------------------------------------------------------------------------
# Bandwidth Throttling (TUM)
# ---------------------------------------------------------------------------
ferrite-dm tum snail                    # Throttled (64 KiB/s down, 32 KiB/s up)
ferrite-dm tum medium                   # Balanced (4 MiB/s down, 1 MiB/s up)
ferrite-dm tum high                     # Unlimited bandwidth and connections

# ---------------------------------------------------------------------------
# Utilities & Media
# ---------------------------------------------------------------------------
# Verify cryptographic checksum
ferrite-dm hash ~/Downloads/ubuntu.iso --algo sha256 --expect <HEX_DIGEST>

# Extract audio to MP3 via FFmpeg
ferrite-dm mp3 ~/Downloads/podcast.mkv ~/Music/podcast.mp3 --bitrate 320

# Remux or transcode video to MP4 via FFmpeg
ferrite-dm mp4 ~/Downloads/capture.ts ~/Videos/output.mp4 --transcode

# Run headless background daemon with optional LAN Remote Control
ferrite-dm daemon --remote-control
```

---

## Desktop Shortcuts

Launch the desktop client via `/Applications/Ferrite DM.app` or by running `fdm-gui`.

| Shortcut | Action | Shortcut | Action |
| :--- | :--- | :--- | :--- |
| `⌘N` | Open Add Download modal | `⌘⇧V` / `⌘V` | Paste URL into Add modal |
| `⌘B` | Open Batch Download modal | `Space` | Toggle Pause / Resume on selected item |
| `⌘F` | Focus search and filter bar | `Delete` | Remove selected download |
| `⌘I` | Toggle bottom Inspector pane | `⌘⇧A` | Select all downloads in view |
| `⌘,` | Open Preferences / Settings | `⌘=` / `⌘-` | Zoom in / Zoom out UI |
| `⌘M` | Minimize window | `⌘W` / `⌘Q` | Close window / Quit application |

### Inspector Panel Tabs (`⌘I`)

- **`GENERAL`:** File location, progress bar, total size, ETA, and download speed.
- **`DETAILS`:** HTTP response headers, MIME type, ETag, redirect hops, and mirror fallbacks.
- **`FILES`:** Interactive file tree for multi-file torrent archives and batch packages.
- **`PROGRESS`:** Visual completion map of segmented HTTP chunks and BitTorrent pieces.
- **`CONNECTIONS`:** Active remote IP addresses, connection protocol, and per-peer throughput.
- **`SPEED`:** Real-time download and upload transfer graphs.

---

## Configuration

### Data Paths

State, history, and configuration are stored in your operating system's standard data directory:

- **macOS:** `~/Library/Application Support/org.ferritedm.FerriteDM`
- **Linux:** `~/.local/share/ferritedm` (`$XDG_DATA_HOME/ferritedm`)
- **Portable Mode (`--portable`):** `./ferrite-data` adjacent to the executable binary.

Files stored inside the data folder:
- `db.sqlite` — SQLite database with WAL journaling (downloads, history, chunk maps, tags).
- `config.json` — Persistent settings and bandwidth profiles.
- `ferrite.sock` — Unix domain socket for IPC communication.
- `plugins/` — JavaScript media site parser add-ons.

### Traffic Usage Mode (TUM) Profiles

| Mode | Max Download Speed | Max Upload Speed | Max Conns | Conns / Server | Max Active Downloads | Upload Ratio |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **`snail`** | 64 KiB/s | 32 KiB/s | 4 | 2 | 1 | 1.0x |
| **`low`** | 512 KiB/s | 128 KiB/s | 12 | 4 | 2 | 1.5x |
| **`medium`** | 4 MiB/s | 1 MiB/s | 32 | 8 | 4 | 2.0x |
| **`high`** | Unlimited | Unlimited | 64 | 16 | 8 | Unlimited |

### Environment Variables

| Variable | Description |
| :--- | :--- |
| `FERRITE_THEME` | Overrides desktop interface theme (`dark`, `light`, or `system`). |
| `FERRITE_UPDATE_MANIFEST_URL` | Custom endpoint for application update manifests. |
| `HOSTNAME` | Hostname used for self-signed TLS certificates when generating LAN Remote Control links. |

---

## Roadmap

- **Shipped:**
  - Native GPUI desktop interface on macOS (`arm64` / `x86_64`) and Linux (`x86_64` / `arm64`)
  - Segmented HTTP(S)/FTP engine, BitTorrent (`librqbit`), HLS/DASH, and wildcard batch expansion
  - Unified CLI, background daemon (`ferrite.sock`), browser Native Messaging host, and LAN Remote Control
- **In Progress:**
  - Windows desktop GUI, file allocation, and named-pipe IPC support

---

## Contributing

Contributions from the open-source community are welcome!

1. Fork and clone the repository.
2. Create a feature branch: `git checkout -b feature/my-feature`.
3. Check code formatting, lints, and run tests (see [CI/CD Guide](docs/CI_CD.md) for full pre-flight instructions):
   ```bash
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo check --workspace
   cargo test --workspace
   cargo doc --workspace --no-deps
   ```
4. Commit your changes following conventional commits (`docs:`, `feat:`, `fix:`).
5. Open a Pull Request on GitHub against `main`.

---

## License

Dual-licensed under either:

- **[MIT License](LICENSE-MIT)**
- **[Apache License, Version 2.0](LICENSE-APACHE)**

at your option.
