# WebAssembly (design proposal)

Status: proposal (Nov 2026). Nothing here is implemented. The numbers come
from a prototype built for this proposal (see "What the prototype measured").
Once the design is agreed, the rules move into the language reference
(`spec.md`) and this file keeps the reasons.

## The design in one page

```sh
gojo build --target wasm32-wasi app.gojo           # app.wasm: a WASI 0.1 command, runs anywhere
gojo run --target wasm32-wasi --dir . app.gojo     # runs it under wasmtime (or node), sandboxed
gojo build --target wasm32-js app.gojo             # app.wasm + app.js: browsers and JS runtimes
gojo build --target wasm32-wasip3 --lib svc.gojo   # svc.wasm: a WASI 0.3 component (later)
```

| | `wasm32-wasi` | `wasm32-js` | `wasm32-wasip3` |
| --- | --- | --- | --- |
| Runs on | Wasmtime, Wasmer, WAMR, wazero, Node's `node:wasi`, browsers with a shim | Chrome, Safari, Node; Firefox once it turns on JSPI | Wasmtime 46+, jco, and other WASI 0.3 hosts |
| Output | core module | core module + ES module glue | component |
| Tasks switch with | Asyncify (Binaryen) | JSPI | Component Model cooperative threads, or Asyncify until hosts enable them |
| Waiting for I/O | `poll_oneoff` | the JS event loop | waitable sets |
| C toolchain | `zig cc` (bundles wasi-libc) or clang + wasi-sdk | same | wasi-sdk |
| Phase | 1–2 | 3 | 4 |

The main decisions:

- **Everything Gojo already promises holds on Wasm,** including tasks that
  borrow from other tasks' stacks, structured cancellation, `select` and the
  simulated scheduler. Programs run on **one thread**: the M:N scheduler with
  one worker. `par for` is correct but not parallel.
- **The compiler still emits C.** The generated C compiles for wasm32
  unchanged; what changes is the runtime's platform layer: stacks, context
  switches, the poller, and the OS calls behind `std`.
- **Task switching has one seam with several implementations,** because the
  Wasm platform has no single answer yet: Asyncify works everywhere today,
  JSPI is cheaper where there is JavaScript, and native stack switching
  (Component Model threads, then core stack switching) replaces both as hosts
  ship it.
- **The compiler knows which functions can switch** (whole program, including
  closures and interface calls), so Asyncify instruments only those. That is
  the difference between +2–6% and +40–50% code size.
- **No garbage collector, so no WasmGC.** Linear memory and ownership fit
  each other; the prototype's binaries are 25–30 KB.
- **`extern "C" lib` becomes a Wasm import, and `extern "C" fn` an export.**
  The host provides what `dlopen` would have.

## Why Wasm, for Gojo

- **A sandbox for agent-written code.** Gojo's main users are coding agents.
  A Wasm build sees only the directories it is given, has no network and no
  processes unless the host grants them, and can have its memory capped.
  `gojo run --target wasm32-wasi` lets an agent run code it has not reviewed.
- **Plugins and extensions.** Many hosts load Wasm plugins; a Gojo plugin is
  small and has no GC pauses.
- **Server-side components.** WASI 0.3 hosts run `wasi:http` services, and Gojo
  tasks map well onto its async model (see "I/O").
- **The browser.** The website can run the tour's programs; libraries can
  ship to JavaScript users.

Gojo fits Wasm better than most languages: no GC to ship or tune, a small
runtime, errors as values (no exceptions to unwind), panics that end the
program, and an ownership model that matches the Component Model's `own` and
`borrow` handles.

## Where Wasm is (Nov 2026)

| Feature | Standard | Engines | What it means here |
| --- | --- | --- | --- |
| Wasm 3.0 (memory64, GC, exceptions, tail calls, relaxed SIMD) | Final, Sept 2025 | Shipping in browsers and Wasmtime | Nothing required; memory64 later |
| Threads (shared memory, atomics) | Phase 4 | Browsers (needs cross-origin isolation, workers made by JS); Wasmtime tier 2 | No way to start a thread from Wasm itself |
| wasi-threads | Legacy, replaced by shared-everything threads | Some WASI 0.1 hosts | Not worth building on |
| Shared-everything threads | Phase 1 | Unimplemented in Wasmtime | Parallelism on WASI is years away |
| JS Promise Integration (JSPI) | Phase 5 | Chrome 137+, Safari 27, Node 26 (no flag, checked), Firefox: intent to ship in 153 | Cheap task switching wherever there is JS |
| Stack switching (`cont.new`, `resume`, `suspend`, `switch`) | Phase 3 | V8 behind `--experimental-wasm-wasmfx`; Wasmtime tier 3, x86-64 Linux only; no LLVM support | The eventual answer; not usable from C yet |
| Wide arithmetic (`i64.mul_wide_s`) | Phase 4 | Wasmtime tier 1; rejected by Node 26's V8 | Would make checked multiplication cheap |
| Memory control (`memory.discard`) | Phase 1 | Unimplemented | Needed for stack hibernation |
| WASI 0.3 (async in the Component Model) | Ratified June 2026 | Wasmtime 46+ by default, jco | Async I/O with streams, futures, cancellation |
| Component Model cooperative threads (🧵) | In the CM proposal | Wasmtime: implemented, off by default; wasi-libc: `ENABLE_COOP_THREADS`, wasip3 only, experimental | Native task switching for components |

## What other languages did

| Language | How it runs concurrent code on Wasm | Lesson |
| --- | --- | --- |
| Go | Single-threaded: all goroutines on one thread. `go:wasmexport` (1.24) runs each export in a new goroutine, and goroutines still running when it returns pause until the next call. For WASI 0.3, `componentize-go` parks a goroutine at the ABI boundary until its stream is ready | One thread is acceptable; paused background work is a trap Gojo's scopes avoid |
| TinyGo | Goroutines via Binaryen's Asyncify; behaves like `GOMAXPROCS=1`, scheduling cooperatively. Dropping the scheduler (`-scheduler=none`) is its main size advice on Wasm | Asyncify works for stackful languages, at a size cost |
| Ruby (ruby.wasm) | Asyncify for `Fiber`, `setjmp`/`longjmp` and GC root scanning; notes that the cost grows with stack depth and that stack switching will replace it | Same approach, same cost model |
| Pyodide | JSPI to let synchronous Python call async JS | JSPI is ready for real use |
| Rust, C#, Python, JS | Stackless async (`async fn`, coroutines): nothing to switch | Their WASI 0.3 bindings don't need a second stack; Gojo, like Go, needs the stackful path |
| Kotlin, Dart, Java (J2CL) | WasmGC | Irrelevant: Gojo has no GC |

## What the prototype measured

The prototype compiled the generated C of the 8 benchmarks (`bench/`) and of
`tests/run/` with `zig cc -target wasm32-wasi`, against the existing
`gojo_rt.c` (with `dlopen` removed) and a 10-line scheduler that runs `main`
without tasks. Then it measured Asyncify and JSPI switching separately, in C
and in hand-written Wasm. Apple M5, Node 26.10 (V8), Wasmtime 50.0.1
(precompiled), Binaryen 133, zig 0.17. Best of 3 runs on a loaded machine
(load average about 4.5 on 10 cores), so differences under about 10% are noise.

**It works.** 30 of the 65 `tests/run` programs pass unchanged; the other 34
only fail to link because the stub lacks tasks, files or sockets, and one
(`ffi`) uses `dlopen`. All 8 benchmarks print exactly what they print natively.
No pointer-size problem showed up, though `std`'s unsafe code still needs an
audit.

**Speed and size** (ms; Wasm builds include the multiply fix below):

| Benchmark | Native | Node (V8) | Wasmtime | `.wasm` size |
| --- | --- | --- | --- | --- |
| fib | 253 | 223 | 496 | 25.6 KB |
| binary-trees | 252 | 277 | 499 | 29.1 KB |
| nbody | 163 | 330 | 178 | 28.0 KB |
| spectral-norm | 112 | 212 | 313 | 29.3 KB |
| fannkuch-redux | 135 | 258 | 148 | 30.0 KB |
| mandelbrot | 112 | 257 | 141 | 25.8 KB |
| hashmap | 104 | 176 | 143 | 29.4 KB |
| sort | 265 | 304 | 283 | 27.9 KB |
| Geometric mean vs native | 1.00× | 1.54× | 1.49× | |

The two engines are slow in different places: V8 on floating-point loops,
Wasmtime on calls. Part of the native lead is FMA and vectorization: nbody
built natively without them takes 306 ms, as on Wasm.

**Checked multiplication was the biggest gap.** `__builtin_mul_overflow` on
`int64_t` becomes a call to `__multi3` (a software 128-bit multiply), because
core Wasm has no 64×64→128 multiply. spectral-norm took 375 ms on V8 and 407 ms
on Wasmtime. Multiplying directly when both operands fit in 32 bits (the product then
cannot overflow) brought that to 212 and 313 ms, at no cost elsewhere. The wide-arithmetic proposal fixes this
properly, but Node 26 rejects it. Worse, the one build that used it
(spectral-norm) panicked with a false "integer overflow in multiplication" under
Wasmtime 50, even at `opt-level=0`, while a minimal C reproduction ran
correctly. Until that is explained and reported upstream, Gojo doesn't use it.

**Asyncify's cost depends on what can switch.** Here every preemption
checkpoint could switch (the checkpoints were compiled in but never fired).
Times are Node / Wasmtime in ms:

| Benchmark | No switching | Checkpoints at loop heads and function entries | Loop heads only |
| --- | --- | --- | --- |
| fib | 243 / 484 | 652 / 858 | 202 / 336 |
| binary-trees | 303 / 508 | 466 / 619 | 267 / 457 |
| nbody | 338 / 180 | 358 / 236 | 361 / 237 |
| spectral-norm | 196 / 306 | 200 / 289 | 203 / 303 |
| fannkuch-redux | 289 / 151 | 293 / 176 | 289 / 176 |
| mandelbrot | 254 / 138 | 255 / 138 | 255 / 137 |
| hashmap | 139 / 110 | 142 / 122 | 150 / 126 |
| sort | 302 / 276 | 309 / 277 | 313 / 276 |

- With checkpoints at the entry of recursive functions, every recursive call
  is instrumented: fib is 2.7× slower on V8. The geometric mean is +21% (V8)
  and +17% (Wasmtime).
- With checkpoints at loop heads only (none are left in fib, nor in
  binary-trees' hot recursion), the other six are +4% on V8 and +10% on Wasmtime, with nbody at +32%. That
  is over the 5% Gojo accepted for preemption natively.
- Size: +2–6% when indirect calls are known not to switch, +40–50% when
  Binaryen has to assume that every indirect call may switch.
- Running Binaryen's `-O2` again after LLVM made fib 2× slower on V8, by
  inlining. `-O1` after Asyncify is enough.

**A switch** between two tasks, net of the calls themselves, at three stack
depths (ns, 1,000,000 switches):

| Mechanism | Depth 0 | Depth 10 | Depth 50 |
| --- | --- | --- | --- |
| Asyncify, Node | 15 | 125 | 780 |
| Asyncify, Wasmtime | 25 | 133 | 448 |
| JSPI, Node | 83 | 71 | 194 |
| Core stack switching, V8 (experimental flag) | 69 | 77 | 188 |

Asyncify unwinds and rewinds every frame (9–16 ns each), so it wins only for
shallow stacks. A task parked in `net.read` under an HTTP handler is likely
10–20 frames deep. JSPI is as fast as the experimental native stack switching.

## Memory and stacks

- **wasm32, 4 GiB of linear memory.** `int` stays 64 bits; pointers and
  `size_t` are 32. The capacity checks the generated C already does
  (`PTRDIFF_MAX / sizeof`) keep their meaning. memory64 lifts the limit but
  costs explicit bounds checks; it can come later as a separate target.
- **Two stacks per task.** Clang keeps most locals in Wasm locals, on the
  engine's own stack, where nothing can point to them. Locals whose address is
  taken, and large values, live on a "shadow stack" in linear memory, which
  the runtime allocates per task. It needs far less than a native stack, so
  the default is **64 KiB** (`GOJO_STACK` still overrides it). That allows at
  most about 60,000 tasks in 4 GiB. Untouched pages should cost address space
  rather than memory in engines that map linear memory lazily; to be measured.
- **Stacks still never move, so borrowing across tasks stays sound.** Asyncify
  copies Wasm locals out and back, but nothing can point at a Wasm local. A
  frame's shadow-stack address is itself held in a Wasm local, which Asyncify
  saves and restores, so every frame comes back at the same address with its
  contents untouched. The runtime saves `__stack_pointer` when a task unwinds
  (frames skip their epilogues, so it is at its deepest) and restores it
  before rewinding, so calls made after resuming push below the task's frames.
  In the prototype, two tasks each wrote into a frame on the other's shadow
  stack while it was switched out, 100,000 times at depths 0, 10 and 50, and
  every frame came back at its address with the right contents. The other
  mechanisms keep each task's stack in place by construction.
- **Overflow must be checked.** Linear memory has no guard pages, so a deep
  recursion would silently write over the next task's stack or the heap.
  Binaryen's `--stack-check` pass compares every `__stack_pointer` update with
  limits that the runtime sets at each switch, and a failure panics with
  "stack overflow" as natively. Running out of the engine's own stack traps
  ("call stack exhausted"); the runtime cannot catch that, so the host
  reports it.
- **No hibernation.** Linear memory can grow but never give pages back, so
  putting a stack away saves nothing. Hibernation returns with memory control.
  Freed heap memory is reused, but never returned to the host.

## Tasks on one thread

`gojo_sched.c` already keeps scheduling separate from the platform: run
queues, channels, `select`, mutexes, scopes, cancellation, timers and
task-locals don't change, and the simulated scheduler is already a
one-thread scheduler. The Wasm build keeps all of that and replaces only:

| Native | Wasm |
| --- | --- |
| Assembly context switch | The switch backends below |
| `mmap`'d stacks with guard pages | Heap-allocated shadow stacks with limits |
| Workers, the monitor thread, CPU-slot handoff | One worker; `task.stats().maxProcs` is 1 |
| Signals: SIGINT cancels `main`, SIGQUIT dumps tasks | None on WASI; `task.dump()` still works |
| kqueue / epoll / io_uring | `poll_oneoff`, the JS event loop, or waitable sets |
| Hibernation | Off |

**The switch backends**, all behind one `gojo_switch(from, to)`:

1. **Asyncify** (`wasm32-wasi`). After linking, Binaryen rewrites the
   functions that can switch so they save their locals and return, and later
   resume where they were. The compiler computes that set: a function can
   switch if it parks (channel operations, locks, `join`, `sleep`, I/O), or if
   it calls one that can. Indirect calls count by type: a call through a
   closure, function value or interface method can switch if any function of
   that type that is used as a value can. Runtime functions that call back
   into Gojo (sorting with a comparator) are included the same way. The
   result goes to Binaryen as `asyncify-onlylist`, so nothing else is
   instrumented.
2. **JSPI** (`wasm32-js`). Each task is a `WebAssembly.promising` call.
   Parking calls a `WebAssembly.Suspending` import that resolves the next
   task's promise and returns its own. Nothing is instrumented, and a switch
   costs about 80 ns at any depth. The runtime saves and restores
   `__stack_pointer` around the switch.
3. **Component Model cooperative threads** (`wasm32-wasip3`).
   `thread.new-indirect` makes a task, `thread.suspend-then-resume` and
   `thread.yield-then-resume` switch, and `context.get`/`context.set` hold the
   shadow-stack pointer and the task. This is native switching, specified for
   exactly this use. Until hosts enable it by default, components use Asyncify.
4. **Core stack switching**, later. It would serve every target, but C can't
   express continuation types, so it needs LLVM support or a small
   post-link step that adds the switch function.

**Preemption.** Natively, generated code polls a flag that a monitor thread
sets. On Wasm no other thread exists, so the checkpoint becomes a countdown
(`if (--budget < 0)`), and the slow path reads the clock and switches when the
task has run 10 ms and others are waiting. On `wasm32-js` that switch goes
through the event loop, so a long computation doesn't freeze the page. Where
switching is free until it happens (JSPI, native switching), checkpoints work
as natively. Under Asyncify they are the expensive part (table above), so
**Asyncify builds switch only where tasks block**, as TinyGo does; `--preempt`
turns checkpoints back on for loop heads. A task that computes without
blocking then delays the others, and sees cancellation only when it next
blocks. That is the main behaviour difference from native Gojo, and the reason
to move off Asyncify as soon as hosts allow.

## I/O and waiting

When no task can run, the scheduler waits for the host:

- **WASI 0.1:** `poll_oneoff` with a clock subscription for the next timer,
  and fd subscriptions for stdin and pipes where the host supports them.
  Regular files are read synchronously, as WASI 0.1 offers nothing else.
- **JS:** the scheduler returns to the event loop. Timers are `setTimeout`, and
  every awaited host promise (fetch, stdin) wakes its task when it settles.
  `main` is itself a promising export: the glue's `run()` returns a promise
  that settles when `main`'s scope ends.
- **WASI 0.3:** async imports return subtasks, streams and futures; a parked
  task waits on them, and the idle scheduler waits on a waitable set. The
  model is completion-based, with explicit cancellation (`stream.cancel-read`,
  `subtask.cancel`), which is the io_uring model the runtime already
  implements: a cancelled task waits until the host is done with its buffer
  before the buffer is freed. The ownership rules that made io_uring safe in
  Gojo carry over unchanged.

## The standard library per target

| Module | `wasm32-wasi` | `wasm32-js` | `wasm32-wasip3` |
| --- | --- | --- | --- |
| `os`: files, directories | Preopened directories only (`--dir`) | No (stdout and stderr only) | `wasi:filesystem` |
| `os`: args, env | Yes | Passed to `run()` | Yes |
| `os.run` | No | No | No |
| `time` | Yes | Yes | Yes |
| `net` | No (WASI 0.1 can't create sockets) | No (browsers have no TCP) | `wasi:sockets` |
| `http` | No | Client via `fetch`, later | `wasi:http` client and server |
| `task` | One worker | One worker | One worker |

A program that reaches an unavailable function gets a compile error that
names the target and the reason, at the call in the user's code: ``error:
`os.run` is not available on wasm32-wasi: WASI has no processes``. The
compiler knows which runtime functions each target has, and checks
reachability from `main` (or from the exports). How a program provides
something else on Wasm is decision 4 below.

## FFI, imports and exports

```gojo
extern "C" lib "host" {                 // on Wasm: imports from module "host"
    fn progress(done: int, total: int)
}

extern "C" fn simulate(steps: int) -> f64 {   // on Wasm: the export "simulate"
    var e = 0.0
    for i in 0..steps {
        e += 1.0 / f64(i + 1)
        unsafe { progress(i + 1, steps) }
    }
    return e
}
```

- **`extern "C" lib "x"`** loads `x` at run time natively. On Wasm, `dlopen`
  doesn't exist, and the equivalent is an import: module `x`, the function's
  name. The host provides it: a JS object, a Wasmtime linker, or another
  module. Pointers are offsets into linear memory.
- **`extern "C" { }`** (libc) links against wasi-libc. A function wasi-libc
  doesn't have is reported against the Gojo declaration, not as a `wasm-ld`
  message.
- **`extern "C" static "z"`** links a static library built for wasm32 (for
  example zlib, built with `zig cc -target wasm32-wasi`).
- **`extern "C" fn`** functions of the root package become exports. A program
  with `main` is a command (`_start`). `gojo build --lib` builds a reactor:
  no `main`, the exports, and `_initialize`.
- **Each export call is a scope.** It runs as the root task of a new scope and
  returns when the scope ends, so every task it started has finished. Go's
  `go:wasmexport` leaves goroutines paused between calls; scopes rule that out.
  On WASI, an export that waits blocks the host's call, as any call would. On
  `wasm32-js`, an export that can park returns a promise.
- **Buffers.** Passing strings or byte buffers across needs a way to view
  memory at a pointer as a slice, which Gojo doesn't have yet. It would be an
  `unsafe` operation (`unsafe.slice(ptr, len)`), and the JS glue and `gojo
  bindgen` would build on it.

## Components and WIT (phase 4)

`gojo bindgen world.wit` generates Gojo declarations for a world's imports,
and the signatures that its exports must implement. The type mapping is
direct:

| WIT | Gojo |
| --- | --- |
| `string`, `list<T>` | `String` / `str`, `Vec[T]` / `[]T` |
| `option<T>`, `result<T, E>` | `?T`, `T ! E` |
| `record`, `variant`, `enum`, `tuple` | `struct`, `enum`, `enum`, tuple |
| `own<R>` | An owned value whose `drop` calls `resource.drop` |
| `borrow<R>` | A borrowed parameter |
| `stream<T>`, `future<T>` (WASI 0.3) | A `Receiver[T]`-like reader, a `Task[T]`-like handle |

Ownership is what makes this mapping safe. The Component Model requires that
a `borrow` handle not outlive the call that received it, which is exactly
what Gojo's borrow checker enforces for borrowed parameters. An owned handle
is released exactly once because it is a value with a `drop`. Languages with a
GC need finalizers or manual `close` calls for the same guarantees.

## Code generation details

- **Checked `i64` multiplication** gets a 32-bit fast path on Wasm (above), until
  wide arithmetic is safe to use.
- **Floating point.** Core Wasm has no fused multiply-add, so C compilers don't
  contract `a * b + c` on Wasm, while natively they may (clang does on arm64).
  The benchmarks printed identical results, but in general a program can
  print different floats natively and on Wasm. See decision 6.
- **Atomics** compile to plain loads and stores on one thread (clang does
  this without `+atomics`).
- **The thread-pointer assembly** (`gojo_box_head`, the preemption trampoline)
  already has portable fallbacks; the prototype used them.
- **Debugging.** Panics already carry Gojo source locations, so their messages
  are the same on Wasm. Debug builds keep the name section, so traps show
  function names. The `#line` directives give DWARF with Gojo lines, for
  debuggers that read DWARF in Wasm.

## For coding agents

- `gojo run --target wasm32-wasi` is a sandbox: the program gets only what
  the command line grants (`--dir`, `--env`), and an error says which flag
  grants what was missing ("`os.readText` failed: no directory is
  preopened; pass `--dir .`").
- `gojo test --target wasm32-wasi` runs tests on the simulated scheduler, as
  natively. Since one seed fixes the interleaving, **a seed must print the
  same output natively and on Wasm**: a differential test for the runtime
  port and for codegen assumptions about pointer size. `gojo-fuzz` gains a
  mode that runs generated programs both ways.
- Unavailable APIs fail at compile time, with the target named (above).
- `gojo build --target ... --json` reports the output files, the imports the
  module needs, and its exports, so an agent wiring a host can read them
  instead of disassembling the binary.

## What we deliberately don't take

- **WasmGC.** Gojo has nothing to collect, and its data structures live in linear
  memory.
- **Emscripten.** Its JS runtime emulates far more than Gojo needs; the glue
  for `wasm32-js` is a few hundred lines.
- **Instrumenting everything with Asyncify.** The compiler knows the switch
  set; Binaryen's default (any import or indirect call may switch) costs
  +40–50% in size for nothing.
- **wasi-threads.** It is legacy, and its successor is years away.
- **Preemption under Asyncify by default.** It costs too much; native switching
  makes it free.
- **A DOM or JS-object binding layer** in the first version. Imports and exports
  of numbers and memory come first, and generated bindings later.

## Decisions to confirm

1. **Three targets:** `wasm32-wasi` (WASI 0.1 core module), `wasm32-js`, and
   `wasm32-wasip3`. WASI 0.2 is skipped: 0.3 is ratified, Wasmtime and jco
   run it by default, and its async model is the one that fits tasks. A 0.2
   target can come later if hosts that matter stay on 0.2.
2. **External tools:** the C toolchain as today (`zig cc` already bundles
   wasi-libc; the prototype used nothing else) plus Binaryen's `wasm-opt`
   for Asyncify and `--stack-check`. If `wasm-opt` isn't on `PATH`, `gojo`
   downloads a pinned release into its cache and checks its hash. The
   alternative is for the compiler to emit Asyncify-style resumable C itself
   (save live locals, return, `goto` the resume point). That would drop the
   dependency and could do better with Gojo's knowledge of live values, but
   it is 1,500–2,500 lines of codegen, so I would start with Binaryen.
3. **Preemption** is off under Asyncify (`--preempt` turns it on) and on
   everywhere else.
4. **Code for one target only.** With compile errors for unavailable APIs,
   a library that works natively and on Wasm needs a way to provide both
   versions. Options: Go-style file suffixes (`net_wasm.gojo`,
   `net_native.gojo`), which compose with nothing and so add no feature
   matrix; or no mechanism, with `std` covering the differences. I propose
   file suffixes, with a fixed set of target names.
5. **64 KiB task stacks** on Wasm, configurable as natively.
6. **Floats:** turn off FMA contraction natively (`-ffp-contract=off`), so
   programs print the same numbers on every target, at some cost in
   numeric code such as nbody. Or accept the difference and document it.

## Implementation plan

1. **`wasm32-wasi` without tasks.**
   - `--target wasm32-wasi` in the driver; the runtime's platform layer, with
     `#ifdef __wasm__` around the parts listed above.
   - `os` on WASI 0.1 (args, env, preopened files, clocks); panics exit with
     code 2 through `proc_exit`.
   - Shadow stacks with `--stack-check`; the checked-multiply fast path.
   - `gojo run --target` runs the module under `wasmtime`, else `node`.
   - Compile errors for unavailable APIs.
   - `scripts/wasm-test.sh` runs `tests/run`, `tests/panic` and the examples
     under Wasmtime; CI runs it.
2. **Tasks on Asyncify.**
   - The switch-set analysis; `asyncify-onlylist`; the one-worker scheduler.
   - `poll_oneoff` waits, timers, `select`, cancellation.
   - The simulated scheduler, with the native-vs-Wasm seed comparison.
   - Measure memory with 100,000 parked tasks, and recheck the cost tables.
3. **`wasm32-js`.**
   - The JSPI backend and the ES module glue (`run()`, stdio, clocks, imports).
   - Preemption through the event loop.
   - The website runs the tour's programs in the browser.
4. **`wasm32-wasip3`.**
   - Components with wasi-sdk; the cooperative-threads backend, with Asyncify as
     the fallback.
   - `std/net` on `wasi:sockets`, and `std/http` on `wasi:http` (client, and
     server as a `service`).
   - `gojo bindgen`.
5. **Later,** as engines ship:
   - parallelism in browsers (workers and shared memory);
   - core stack switching;
   - wide arithmetic;
   - memory64;
   - memory control, and with it hibernation;
   - shared-everything threads.

## Sources

- Proposal phases: [WebAssembly/proposals](https://github.com/WebAssembly/proposals); [Wasm 3.0](https://webassembly.org/news/2025-09-17-wasm-3.0)
- Wasmtime support: [Wasm proposals in Wasmtime](https://docs.wasmtime.dev/stability-wasm-proposals.html); [stack switching tracking issue](https://github.com/bytecodealliance/wasmtime/issues/10248); [shared-everything threads issue](https://github.com/bytecodealliance/wasmtime/issues/9466)
- WASI 0.3: [WASI 0.3 launched](https://bytecodealliance.org/articles/WASI-0.3); [WASI 0.3 release notes](https://wasi.dev/releases/wasi-p3)
- Component Model threads: [Concurrency.md](https://github.com/WebAssembly/component-model/blob/main/design/mvp/Concurrency.md); [wasi-libc](https://github.com/webassembly/wasi-libc); [wasi-sdk threading ABI](https://github.com/WebAssembly/wasi-sdk/issues/589)
- Threads: [wasi-threads](https://github.com/WebAssembly/wasi-threads); [shared-everything threads overview](https://github.com/WebAssembly/shared-everything-threads/blob/main/proposals/shared-everything-threads/Overview.md)
- JSPI: [Chrome intent to ship](https://groups.google.com/a/chromium.org/g/blink-dev/c/w_jCD4gf7Bc); [Safari 27](https://webkit.org/blog/18325/webkit-features-for-safari-27-0/); [Firefox intent to ship](https://groups.google.com/a/mozilla.org/g/dev-platform/c/8cOxM-dWaNs); [JSPI in Pyodide](https://blog.pyodide.org/posts/jspi/)
- Stack switching: [explainer](https://github.com/WebAssembly/stack-switching/blob/main/proposals/stack-switching/Explainer.md); [Continuing stack switching in Wasmtime](https://dhil.net/research/papers/wasmfxtime-waw2025.pdf)
- Asyncify: [Pause and resume WebAssembly with Binaryen's Asyncify](https://kripken.github.io/blog/wasm/2019/07/16/asyncify.html)
- Go: [go:wasmexport](https://go.dev/blog/wasmexport). TinyGo: [optimizing binaries](https://tinygo.org/docs/guides/optimizing-binaries/)
- Ruby: [WebAssembly/WASI support in Ruby](https://itnext.io/final-report-webassembly-wasi-support-in-ruby-4aface7d90c9); [`wasm/setjmp.c`](https://git.ruby-lang.org/ruby.git/tree/wasm/setjmp.c?h=v4.0.4)
