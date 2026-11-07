#!/bin/sh
# Runs the programs of tests/run and tests/stdin on Linux, in a container:
# cross-compiled with `gojo build --target <arch>-linux-musl` (needs zig) and
# compared with their .out files. Each runs three times: with Docker's
# default seccomp profile, which blocks io_uring (so file I/O takes the
# blocking path); with seccomp off, where regular files that would block go
# through io_uring; and with GOJO_IO_URING=always, where every file read and
# write does. tests/stdin programs get a stdin pipe that stays empty for two
# seconds.
#
# usage: scripts/linux-test.sh            (GOJO=path/to/gojo, IMAGE=alpine:3)
set -eu

cd "$(dirname "$0")/.."
gojo=${GOJO:-target/debug/gojo}
image=${IMAGE:-alpine:3}
arch=$(docker info --format '{{.Architecture}}' 2>/dev/null || uname -m)
case $arch in
    aarch64|arm64) target=aarch64-linux-musl ;;
    x86_64|amd64) target=x86_64-linux-musl ;;
    *) echo "linux-test.sh: unsupported architecture $arch" >&2; exit 2 ;;
esac
out=$(mktemp -d "${TMPDIR:-/tmp}/gojo-linux.XXXXXX")
trap 'rm -rf "$out"' EXIT
fail=0
for f in tests/run/*.gojo tests/stdin/*.gojo; do
    d=$(basename "$(dirname "$f")")
    n=$d-$(basename "$f" .gojo)
    if ! "$gojo" build --target "$target" "$f" -o "$out/$n" > "$out/$n.build" 2>&1; then
        echo "BUILD FAIL $f"; sed 's/^/    /' "$out/$n.build"; fail=1; continue
    fi
    cp "${f%.gojo}.out" "$out/$n.expected"
done
# ffi loads libz with dlopen: not in a static musl binary
rm -f "$out/run-ffi" "$out/run-ffi.expected"
for mode in default uring always; do
    if [ $mode = default ]; then opts=; else opts="--security-opt seccomp=unconfined"; fi
    # shellcheck disable=SC2086
    docker run --rm $opts -e MODE=$mode -v "$out:/t" "$image" sh -c '
        cd /t
        [ $MODE = always ] && export GOJO_IO_URING=always
        for e in *.expected; do
            n=${e%.expected}
            case $n in
                stdin-*) (sleep 2; echo hi) | timeout 60 ./$n > $n.got 2> $n.err ;;
                *) timeout 60 ./$n > $n.got 2> $n.err < /dev/null ;;
            esac
            cmp -s $n.got $e || { echo "DIFF ($MODE) $n"; diff $e $n.got | head -20; sed "s/^/    /" $n.err | head -20; }
        done
        echo "$MODE: done"' || fail=1
done
exit $fail
