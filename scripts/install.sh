#!/bin/sh
# Installs Gojo into ~/.gojo (or $GOJO_HOME): the compiler in ~/.gojo/bin,
# the docs and examples next to it.
#
#   curl -fsSL https://github.com/egoist/gojo/releases/latest/download/install.sh | sh
#   sh install.sh --version 0.1.0                          a given release
#   ... | sh -s -- --nightly                               the latest nightly build
#   sh install.sh --archive dist/gojo-0.1.0-macos-arm64.tar.gz   a local archive
#
# Downloads come from the GitHub releases of egoist/gojo ($GOJO_RELEASES
# overrides it, for a mirror), and are checked against the release's
# SHA256SUMS.
set -eu

RELEASES=${GOJO_RELEASES:-https://github.com/egoist/gojo/releases}
home=${GOJO_HOME:-$HOME/.gojo}
version=
nightly=
archive=

while [ $# -gt 0 ]; do
    case $1 in
        --version) version=$2; shift 2 ;;
        --nightly) nightly=1; shift ;;
        --archive) archive=$2; shift 2 ;;
        -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
        *) echo "install.sh: unknown argument $1" >&2; exit 2 ;;
    esac
done

case $(uname -s)-$(uname -m) in
    Darwin-arm64) platform=macos-arm64 ;;
    Darwin-x86_64) platform=macos-x86_64 ;;
    Linux-x86_64|Linux-amd64) platform=linux-x86_64 ;;
    Linux-aarch64|Linux-arm64) platform=linux-arm64 ;;
    *) echo "install.sh: Gojo runs on macOS and Linux (x86-64 and arm64), not on $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

fetch() {
    if command -v curl >/dev/null 2>&1; then curl -fsSL --retry 3 "$1" -o "$2"
    elif command -v wget >/dev/null 2>&1; then wget -q --tries=3 -O "$2" "$1"
    else echo "install.sh: needs curl or wget" >&2; exit 1
    fi
}

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
    else shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

if [ -z "$archive" ]; then
    if [ -z "$RELEASES" ]; then
        echo "install.sh: set GOJO_RELEASES to a releases page, or pass --archive FILE" >&2
        exit 1
    fi
    # nightly builds live in one release, tagged `nightly`, replaced by each build
    if [ -n "$nightly" ]; then
        if [ -n "$version" ]; then
            echo "install.sh: --nightly and --version exclude each other" >&2
            exit 2
        fi
        tag=nightly
        fetch "$RELEASES/download/nightly/VERSION" "$tmp/VERSION"
        version=$(tr -d ' \n' < "$tmp/VERSION")
    else
        if [ -z "$version" ]; then
            if ! fetch "$RELEASES/latest/download/VERSION" "$tmp/VERSION"; then
                echo "install.sh: cannot find the latest release at $RELEASES (there may be none yet: --nightly installs the latest nightly build)" >&2
                exit 1
            fi
            version=$(tr -d ' \n' < "$tmp/VERSION")
        fi
        tag=v$version
    fi
    name=gojo-$version-$platform.tar.gz
    echo "downloading $name"
    fetch "$RELEASES/download/$tag/$name" "$tmp/$name"
    fetch "$RELEASES/download/$tag/SHA256SUMS" "$tmp/SHA256SUMS"
    want=$(grep " $name\$" "$tmp/SHA256SUMS" | cut -d' ' -f1)
    if [ -z "$want" ] || [ "$want" != "$(sha256 "$tmp/$name")" ]; then
        echo "install.sh: checksum mismatch for $name" >&2
        exit 1
    fi
    archive=$tmp/$name
fi

tar -C "$tmp" -xzf "$archive"
dir=$(find "$tmp" -maxdepth 1 -type d -name 'gojo-*' | head -n 1)
if [ -z "$dir" ] || [ ! -x "$dir/gojo" ]; then
    echo "install.sh: $archive is not a Gojo release archive" >&2
    exit 1
fi
case $dir in
    *-"$platform") ;;
    *) echo "install.sh: $archive is not built for $platform" >&2; exit 1 ;;
esac

mkdir -p "$home/bin"
rm -rf "$home/docs" "$home/examples"
cp "$dir/gojo" "$home/bin/gojo.new" && mv "$home/bin/gojo.new" "$home/bin/gojo"
cp -R "$dir/docs" "$dir/examples" "$home/"
cp "$dir/README.md" "$home/"
[ -f "$dir/LICENSE" ] && cp "$dir/LICENSE" "$home/"
echo "installed $("$home/bin/gojo" version) in $home"

case :$PATH: in
    *:"$home/bin":*) ;;
    *) echo "add it to your PATH:  export PATH=\"$home/bin:\$PATH\"" ;;
esac

# Gojo compiles to C
if ! command -v cc >/dev/null 2>&1 && ! command -v clang >/dev/null 2>&1 && ! command -v gcc >/dev/null 2>&1 && ! command -v zig >/dev/null 2>&1; then
    echo
    echo "Gojo needs a C compiler, and none was found:"
    case $platform in
        macos-*) echo "  xcode-select --install" ;;
        *) echo "  install gcc or clang (e.g. apt install gcc, dnf install gcc, apk add gcc musl-dev)" ;;
    esac
    echo "  or install zig (https://ziglang.org) and set GOJO_CC=\"zig cc\""
fi
