#!/bin/sh
# Builds the release archives into dist/:
#
#   kite-VERSION-macos-arm64.tar.gz    kite-VERSION-linux-x86_64.tar.gz
#   kite-VERSION-macos-x86_64.tar.gz   kite-VERSION-linux-arm64.tar.gz
#   SHA256SUMS, VERSION, install.sh   (upload all of dist/ to the release)
#
# Run it on a Mac with zig, cargo-zigbuild (`cargo install cargo-zigbuild`)
# and the Rust targets (`rustup target add aarch64-apple-darwin
# x86_64-apple-darwin x86_64-unknown-linux-musl aarch64-unknown-linux-musl`,
# for the toolchain `cargo` runs). The Linux binaries are static (musl) and run on any
# distribution. The standard library and the runtime are compiled into the
# binary: an archive holds `kite`, the docs and the examples.
#
# usage: scripts/release.sh [target...]   (default: all four)
#        JOBS=N limits the build's parallelism (default 4)
set -eu

cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
jobs=${JOBS:-4}
targets=${*:-"macos-arm64 macos-x86_64 linux-x86_64 linux-arm64"}

triple() {
    case $1 in
        macos-arm64) echo aarch64-apple-darwin ;;
        macos-x86_64) echo x86_64-apple-darwin ;;
        linux-x86_64) echo x86_64-unknown-linux-musl ;;
        linux-arm64) echo aarch64-unknown-linux-musl ;;
        *) echo "unknown target $1 (macos-arm64, macos-x86_64, linux-x86_64, linux-arm64)" >&2; exit 2 ;;
    esac
}

if [ -n "$(git status --porcelain 2>/dev/null)" ]; then
    echo "warning: the working tree has uncommitted changes" >&2
fi

tarflags=
tar --version 2>/dev/null | grep -q bsdtar && tarflags="--no-xattrs --no-mac-metadata"

mkdir -p dist
for t in $targets; do
    tr=$(triple "$t")
    echo "==> $t ($tr)"
    case $t in
        macos-*) nice cargo build --release --locked -j "$jobs" --target "$tr" --bin kite ;;
        linux-*) nice cargo zigbuild --release --locked -j "$jobs" --target "$tr" --bin kite ;;
    esac
    name=kite-$version-$t
    stage=dist/$name
    rm -rf "$stage"
    mkdir -p "$stage/docs"
    cp "target/$tr/release/kite" "$stage/kite"
    cp README.md "$stage/"
    [ -f LICENSE ] && cp LICENSE "$stage/"
    cp docs/*.md "$stage/docs/"
    [ -d docs/std ] && cp -R docs/std "$stage/docs/"
    # the examples, without build products
    mkdir -p "$stage/examples"
    (cd examples && find . -type f \( -name '*.kite' -o -name '*.txt' -o -name '*.out' \) -print | tar -cf - -T -) | (cd "$stage/examples" && tar -xf -)
    # (no macOS metadata: GNU tar warns about it)
    COPYFILE_DISABLE=1 tar $tarflags -C dist -czf "dist/$name.tar.gz" "$name"
    rm -rf "$stage"
done

(cd dist && shasum -a 256 kite-"$version"-*.tar.gz > SHA256SUMS)
echo "$version" > dist/VERSION
cp scripts/install.sh dist/
cat dist/SHA256SUMS

# a smoke test with the binary for this machine
host=$(uname -s)-$(uname -m)
case $host in
    Darwin-arm64) h=macos-arm64 ;;
    Darwin-x86_64) h=macos-x86_64 ;;
    Linux-x86_64) h=linux-x86_64 ;;
    Linux-aarch64) h=linux-arm64 ;;
    *) h= ;;
esac
if [ -n "$h" ] && [ -f "dist/kite-$version-$h.tar.gz" ]; then
    tmp=$(mktemp -d)
    tar -C "$tmp" -xzf "dist/kite-$version-$h.tar.gz"
    "$tmp/kite-$version-$h/kite" version
    "$tmp/kite-$version-$h/kite" run "$tmp/kite-$version-$h/examples/hello.kite"
    rm -rf "$tmp"
fi
