#!/bin/sh
# Builds glft-<version>-x86_64-linux.tar.gz into dist/ for distributions without a .deb:
# unpack it anywhere and run ./glft setup, which installs into ~/.local.
set -eu
. "$(dirname "$0")/common.sh"
cargo build --release --locked --target-dir "$target" -p glft-cli -p glft-engine -p glft-gtk
name="glft-${version}-x86_64-linux"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
install -d -m 755 "$stage/$name"
for p in glft glftd glft-window; do
    install -m 755 -s "$target/release/$p" "$stage/$name/$p"
done
install -m 644 packaging/README.txt "$stage/$name/README.txt"
(cd "$stage/$name" && sha256sum glft glftd glft-window README.txt > SHA256SUMS)
mkdir -p dist
tar -C "$stage" --sort=name --mtime="@$SOURCE_DATE_EPOCH" --owner=0 --group=0 --numeric-owner -cf - "$name" | gzip -9n > "dist/$name.tar.gz"
(cd dist && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
echo "dist/$name.tar.gz"
