# Shared by the build scripts: the version, and the settings that make two builds of the
# same commit identical wherever they run.
cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
# File times come from the commit, not the clock.
SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct)}
export SOURCE_DATE_EPOCH
# No path of the build machine in the programs: the checkout and Cargo's home are renamed.
RUSTFLAGS="--remap-path-prefix=$PWD=/glft --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo"
export RUSTFLAGS
# Kept apart from development builds, which the flags above would otherwise rebuild.
target=target/dist
