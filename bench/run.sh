#!/bin/sh
# Builds every benchmark in Kite, Go and Zig, checks that all versions print the
# same output, then measures them with hyperfine. Results go to results.md.
set -e
cd "$(dirname "$0")"
OUT=${OUT:-/tmp/kite-bench}
RUNS=${RUNS:-10}
mkdir -p "$OUT"
KITE=../target/release/kite
(cd .. && cargo build --release -q)
BENCHES="fib binarytrees nbody spectralnorm fannkuch mandelbrot hashmap sort"

for b in $BENCHES; do
    $KITE build --release kite/$b.kite -o "$OUT/kite_$b"
    (cd go && go build -o "$OUT/go_$b" ./$b)
    (cd zig && zig build-exe -O ReleaseFast -lc $b.zig -femit-bin="$OUT/zig_fast_$b" \
            && zig build-exe -O ReleaseSafe -lc $b.zig -femit-bin="$OUT/zig_safe_$b")
    want=$("$OUT/kite_$b")
    for v in go zig_fast zig_safe; do
        if [ "$("$OUT/${v}_$b")" != "$want" ]; then echo "output mismatch: $v $b"; exit 1; fi
    done
done
echo "all outputs match"

echo "| benchmark | Kite (--release) | Go | Zig ReleaseFast | Zig ReleaseSafe |" > results.md
echo "| --- | --- | --- | --- | --- |" >> results.md
for b in $BENCHES; do
    hyperfine -N -w 1 -r "$RUNS" --export-json "$OUT/$b.json" \
        -n kite "$OUT/kite_$b" -n go "$OUT/go_$b" -n zig_fast "$OUT/zig_fast_$b" -n zig_safe "$OUT/zig_safe_$b" > /dev/null
    python3 - "$OUT/$b.json" "$b" >> results.md <<'PY'
import json, sys
r = {x["command"]: x for x in json.load(open(sys.argv[1]))["results"]}
cell = lambda k: "%.0f ms ± %.0f" % (r[k]["mean"] * 1e3, r[k]["stddev"] * 1e3)
print("| %s | %s | %s | %s | %s |" % (sys.argv[2], cell("kite"), cell("go"), cell("zig_fast"), cell("zig_safe")))
PY
done
cat results.md
