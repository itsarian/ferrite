Name:           ferrite-dm
Version:        0.1.1
Release:        1%{?dist}
Summary:        High-performance, multi-protocol download manager and accelerator

License:        MIT or Apache-2.0
URL:            https://github.com/itsarian/ferrite-dm
Source0:        https://github.com/itsarian/ferrite-dm/archive/refs/tags/v%{version}.tar.gz

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
Ferrite DM is a fast, telemetry-free download accelerator written in Rust.
It combines an ultra-fast asynchronous core with a sleek, GPU-accelerated
graphical interface designed to replace legacy tools like Free Download Manager.

%prep
%autosetup -n ferrite-dm-%{version}

%build
cargo build --release --locked -p fdm-gui -p fdm-cli -p fdm-native-host

%install
rm -rf %{buildroot}
install -d %{buildroot}%{_bindir}
install -m 755 target/release/fdm-gui %{buildroot}%{_bindir}/fdm-gui
install -m 755 target/release/ferrite-dm %{buildroot}%{_bindir}/ferrite-dm
install -m 755 target/release/ferrite-native-host %{buildroot}%{_bindir}/ferrite-native-host

install -d %{buildroot}%{_datadir}/applications
install -m 644 resources/linux/ferrite-dm.desktop %{buildroot}%{_datadir}/applications/ferrite-dm.desktop

install -d %{buildroot}%{_datadir}/metainfo
install -m 644 resources/linux/io.ferrite_dm.FerriteDM.metainfo.xml %{buildroot}%{_datadir}/metainfo/io.ferrite_dm.FerriteDM.metainfo.xml

for size in 16 32 48 64 128 256 512 1024; do
    install -d %{buildroot}%{_datadir}/icons/hicolor/${size}x${size}/apps
    install -m 644 resources/linux/icons/hicolor/${size}x${size}/apps/ferrite-dm.png %{buildroot}%{_datadir}/icons/hicolor/${size}x${size}/apps/ferrite-dm.png
done

install -d %{buildroot}%{_datadir}/icons/hicolor/scalable/apps
install -m 644 resources/linux/icons/hicolor/scalable/apps/ferrite-dm.svg %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/ferrite-dm.svg

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
%{_bindir}/fdm-gui
%{_bindir}/ferrite-dm
%{_bindir}/ferrite-native-host
%{_datadir}/applications/ferrite-dm.desktop
%{_datadir}/metainfo/io.ferrite_dm.FerriteDM.metainfo.xml
%{_datadir}/icons/hicolor/*/apps/ferrite-dm.*

%changelog
* Tue Nov 17 2026 Ferrite DM Maintainers <https://github.com/itsarian/ferrite-dm> - 0.1.1-1
- Release 0.1.1: Full Linux GUI and distribution packaging integration
