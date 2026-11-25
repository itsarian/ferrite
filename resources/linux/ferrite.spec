Name:           ferrite
Version:        0.4.0
Release:        1%{?dist}
Summary:        High-performance, multi-protocol download manager and accelerator

License:        MIT or Apache-2.0
URL:            https://github.com/itsarian/ferrite
Source0:        https://github.com/itsarian/ferrite/archive/refs/tags/v%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  gcc
BuildRequires:  cmake
BuildRequires:  pkgconfig
BuildRequires:  openssl-devel
BuildRequires:  fontconfig-devel
BuildRequires:  libX11-devel
BuildRequires:  libxkbcommon-devel
BuildRequires:  libxkbcommon-x11-devel
BuildRequires:  wayland-devel

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
Ferrite is a fast, telemetry-free download accelerator written in Rust.
It combines an ultra-fast asynchronous core with a sleek, GPU-accelerated
graphical interface built with GPUI.

%prep
%autosetup -n ferrite-%{version}

%build
cargo build --release --locked -p ferrite-gui -p ferrite-cli -p ferrite-native-host

%install
rm -rf %{buildroot}
install -d %{buildroot}%{_bindir}
install -m 755 target/release/ferrite-gui %{buildroot}%{_bindir}/ferrite-gui
install -m 755 target/release/ferrite %{buildroot}%{_bindir}/ferrite
install -m 755 target/release/ferrite-native-host %{buildroot}%{_bindir}/ferrite-native-host

install -d %{buildroot}%{_datadir}/applications
install -m 644 resources/linux/ferrite.desktop %{buildroot}%{_datadir}/applications/ferrite.desktop

install -d %{buildroot}%{_datadir}/metainfo
install -m 644 resources/linux/io.ferrite.Ferrite.metainfo.xml %{buildroot}%{_datadir}/metainfo/io.ferrite.Ferrite.metainfo.xml

for size in 16 32 48 64 128 256 512 1024; do
    install -d %{buildroot}%{_datadir}/icons/hicolor/${size}x${size}/apps
    install -m 644 resources/linux/icons/hicolor/${size}x${size}/apps/ferrite.png %{buildroot}%{_datadir}/icons/hicolor/${size}x${size}/apps/ferrite.png
done

install -d %{buildroot}%{_datadir}/icons/hicolor/scalable/apps
install -m 644 resources/linux/icons/hicolor/scalable/apps/ferrite.svg %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/ferrite.svg

%post
if [ $1 -eq 1 ]; then
    /usr/bin/update-desktop-database &> /dev/null || :
    /usr/bin/gtk-update-icon-cache %{_datadir}/icons/hicolor &> /dev/null || :
fi

%postun
if [ $1 -eq 0 ]; then
    /usr/bin/update-desktop-database &> /dev/null || :
    /usr/bin/gtk-update-icon-cache %{_datadir}/icons/hicolor &> /dev/null || :
fi

%files
%license LICENSE-MIT LICENSE-APACHE
%doc README.md
%{_bindir}/ferrite-gui
%{_bindir}/ferrite
%{_bindir}/ferrite-native-host
%{_datadir}/applications/ferrite.desktop
%{_datadir}/metainfo/io.ferrite.Ferrite.metainfo.xml
%{_datadir}/icons/hicolor/*/apps/ferrite.*

%changelog
* Thu Nov 26 2026 Ferrite Maintainers <https://github.com/itsarian/ferrite> - 0.4.0-1
- Release 0.4.0: Renamed project to Ferrite (ferrite, ferrite-gui, Ferrite.app) with automatic legacy data migration and refreshed documentation
* Wed Nov 18 2026 Ferrite Maintainers <https://github.com/itsarian/ferrite> - 0.3.0-1
- Release 0.3.0: Native Linux ARM64 (aarch64) support, unified release asset naming, Linux copy/paste fixes, and Linux HTTP 403 download fixes
* Tue Nov 17 2026 Ferrite Maintainers <https://github.com/itsarian/ferrite> - 0.2.0-1
- Release 0.2.0: Full Linux GUI (Wayland/X11), multi-distro packaging, OS integration, and comprehensive code audit fixes
* Tue Nov 17 2026 Ferrite Maintainers <https://github.com/itsarian/ferrite> - 0.1.1-1
- Release 0.1.1: Full Linux GUI and distribution packaging integration
