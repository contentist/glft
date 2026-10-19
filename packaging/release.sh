#!/bin/sh
# Builds a release from a clean clone of its tag and drafts it on GitHub: the .deb, the
# tarball, the signed APK and SHA256SUMS, with notes from CHANGELOG.md.
#
#   packaging/release.sh 0.2.0           build, sign and draft the release on GitHub
#   packaging/release.sh 0.2.0 --local   build and sign only, into dist/release-0.2.0
#
# Run it in a terminal: it asks for the signing key's password first, then needs nothing
# more. The password is checked straight away and never written anywhere.
set -eu
cd "$(dirname "$0")/.."
version=${1:?usage: packaging/release.sh VERSION [--local]}
local_only=${2:-}
tag="v$version"
noreply=319569693+contentist@users.noreply.github.com
keystore=${GLFT_KEYSTORE:-$HOME/.glft-release/glft-release.jks}
export ANDROID_HOME=${ANDROID_HOME:-$HOME/android-sdk}
# From build-tools 35 on, apksigner makes APKs F-Droid's tools cannot verify.
apksigner=$ANDROID_HOME/build-tools/34.0.0/apksigner
zipalign=$ANDROID_HOME/build-tools/34.0.0/zipalign

stop() { echo "release: $*" >&2; exit 1; }

# Everything that could make this release wrong is checked before anything is built.
[ -z "$(git status --porcelain)" ] || stop "the working tree has changes; commit them first"
[ "$(git rev-parse "$tag^{commit}" 2>/dev/null)" = "$(git rev-parse HEAD)" ] || stop "$tag is not tagged at HEAD"
grep -q "^version = \"$version\"" Cargo.toml || stop "Cargo.toml's version is not $version"
authors=$(git log --format='%ae%n%ce' "$tag" | sort -u)
[ "$authors" = "$noreply" ] || stop "commits by other addresses than $noreply: $authors"
[ -f "$keystore" ] || stop "no signing key at $keystore"
[ -x "$apksigner" ] || stop "no apksigner at $apksigner (install build-tools 34.0.0)"
if [ -z "$local_only" ]; then
    grep -q "^## $version" CHANGELOG.md || stop "CHANGELOG.md has no \"## $version\" section"
    git ls-remote --exit-code --tags origin "$tag" >/dev/null || stop "$tag is not on GitHub yet: git push origin $tag"
fi
case $(stat -c %a "$keystore") in
    600 | 400) ;;
    *) echo "release: note: $keystore can be read by others on this computer (chmod 600 it)" >&2 ;;
esac

trap 'stty echo 2>/dev/null' EXIT INT TERM
printf 'Password of the signing key: '
stty -echo
read -r GLFT_KS_PASS
stty echo
echo
export GLFT_KS_PASS
keytool -list -keystore "$keystore" -storepass:env GLFT_KS_PASS >/dev/null 2>&1 || stop "that password doesn't open $keystore"

work=$(mktemp -d)
trap 'stty echo 2>/dev/null; rm -rf "$work"' EXIT INT TERM
echo "Building $tag from a clean clone (several minutes)"
git clone --quiet --branch "$tag" "$PWD" "$work/glft"
(
    cd "$work/glft"
    packaging/build-deb.sh >/dev/null
    packaging/build-tarball.sh >/dev/null
    cd android && ./gradlew -q --no-build-cache assembleRelease
)

out="dist/release-$version"
rm -rf "$out"
mkdir -p "$out"
cp "$work/glft/dist/glft_${version}_amd64.deb" "$work/glft/dist/glft-$version-x86_64-linux.tar.gz" "$out/"
unsigned="$work/glft/android/app/build/outputs/apk/release/app-release-unsigned.apk"
"$zipalign" -c -p 4 "$unsigned" || stop "the APK is not aligned"
"$apksigner" sign --ks "$keystore" --ks-key-alias glft --ks-pass env:GLFT_KS_PASS \
    --out "$out/glft-$version.apk" "$unsigned"
rm -f "$out/glft-$version.apk.idsig"
# Kept for comparing with other builds of the same commit, which must give this exact file.
cp "$unsigned" "$out/glft-$version-unsigned.apk"
(cd "$out" && sha256sum glft_${version}_amd64.deb glft-$version-x86_64-linux.tar.gz glft-$version.apk > SHA256SUMS)
echo "Signing certificate:"
"$apksigner" verify --print-certs "$out/glft-$version.apk" | grep -E "SHA-256"

if [ -n "$local_only" ]; then
    echo "Built into $out; nothing uploaded."
    exit 0
fi
notes="$work/notes.md"
awk -v v="## $version" 'index($0, v) == 1 { on = 1; next } /^## / { on = 0 } on' CHANGELOG.md > "$notes"
gh release create "$tag" --draft --verify-tag --title "glft $version" --notes-file "$notes" \
    "$out/glft_${version}_amd64.deb" "$out/glft-$version-x86_64-linux.tar.gz" "$out/glft-$version.apk" "$out/SHA256SUMS"
echo "Drafted $tag on GitHub; review it there and publish it."
