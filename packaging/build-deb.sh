#!/bin/sh
# Builds glft_<version>_amd64.deb into dist/: the three programs, the engine's systemd
# user units, the menu entry, icon and Nemo action, and scripts that stop a running
# engine before files change.
set -eu
. "$(dirname "$0")/common.sh"
cargo build --release --locked --target-dir "$target" -p glft-cli -p glft-engine -p glft-gtk

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
root="$stage/glft_${version}_amd64"
install -d "$root/DEBIAN" "$root/usr/bin" "$root/usr/lib/systemd/user" \
    "$root/usr/share/applications" "$root/usr/share/icons/hicolor/scalable/apps" \
    "$root/usr/share/nemo/actions" "$root/usr/share/doc/glft"
for p in glft glftd glft-window; do
    install -m 755 -s "$target/release/$p" "$root/usr/bin/$p"
done
cat > "$root/usr/lib/systemd/user/glft.socket" <<'UNIT'
[Unit]
Description=glft engine socket (local control only)

[Socket]
ListenStream=%t/glft/engine.sock
SocketMode=0600
DirectoryMode=0700

[Install]
WantedBy=sockets.target
UNIT
cat > "$root/usr/lib/systemd/user/glft.service" <<'UNIT'
[Unit]
Description=glft engine: sends and receives files between your devices
Requires=glft.socket

[Service]
ExecStart=/usr/bin/glftd
UNIT
chmod 644 "$root/usr/lib/systemd/user/glft.socket" "$root/usr/lib/systemd/user/glft.service"
install -m 644 contrib/desktop/io.github.contentist.glft.desktop "$root/usr/share/applications/"
install -m 644 contrib/desktop/io.github.contentist.glft.svg "$root/usr/share/icons/hicolor/scalable/apps/"
install -m 644 contrib/nemo/glft-send.nemo_action "$root/usr/share/nemo/actions/"
install -m 644 LICENSE "$root/usr/share/doc/glft/copyright" 2>/dev/null || true
install -m 644 packaging/README.txt "$root/usr/share/doc/glft/README.txt"
install -m 755 packaging/deb/prerm packaging/deb/postinst packaging/deb/postrm "$root/DEBIAN/"

# Library versions the programs need, from their own symbols.
mkdir -p "$stage/debian" && echo "Source: glft" > "$stage/debian/control"
depends=$(cd "$stage" && dpkg-shlibdeps -O "glft_${version}_amd64/usr/bin/glftd" "glft_${version}_amd64/usr/bin/glft" "glft_${version}_amd64/usr/bin/glft-window" 2>/dev/null | sed -n 's/^shlibs:Depends=//p')
size=$(du -sk "$root/usr" | cut -f1)
cat > "$root/DEBIAN/control" <<CONTROL
Package: glft
Version: $version
Architecture: amd64
Maintainer: Mister Human <319569693+contentist@users.noreply.github.com>
Installed-Size: $size
Depends: $depends
Suggests: nemo
Section: net
Priority: optional
Homepage: https://github.com/contentist/glft
Description: send files directly between your phone and computer, verified
 glft moves files and whole folders between a GrapheneOS (or other Android) phone
 and this computer over the local network, or a mesh such as Yggdrasil, with no
 server in between. Every file is checked with BLAKE3 as it arrives, and every
 step is shown in plain words. Includes the terminal program (glft), the
 background engine (glftd), the window (glft-window) and a Nemo action.
CONTROL
find "$root" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +
mkdir -p dist
dpkg-deb --root-owner-group --build "$root" "dist/glft_${version}_amd64.deb" >/dev/null
echo "dist/glft_${version}_amd64.deb"
echo "Depends: $depends"
