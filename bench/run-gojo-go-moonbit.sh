#!/bin/sh
# Native comparison: Gojo, Go, MoonBit. See compare-moonbit/README.md.
set -e
cd "$(dirname "$0")"
exec python3 ./run_gojo_go_moonbit.py "$@"
