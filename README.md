# Gojo

**Documentation:** [Why Gojo](docs/why-gojo.md) · [A tour of Gojo](docs/tour.md) · [Standard library reference](docs/std/README.md) · [Language reference](docs/spec.md) · [Design notes](docs/design.md) · [Packages](docs/packages.md)

Gojo is a small, fast-compiling systems programming language with no garbage collector and no nil. Ownership (without lifetime annotations) frees memory and rules out data races; errors are values (`T ! E`, `try`, `errdefer`); concurrency is structured (`scope`, `go`, channels, cancellation). It takes ideas from Rust, Zig, Swift, Kotlin and Go without following any of them.

This repository is a prototype compiler built from the [design notes](docs/design.md). The compiler is written in Rust (about 19,000 lines, no third-party dependencies). The C runtime is about 4,000 lines, and the Gojo standard library about 3,400 lines.

```gojo
use std/os

struct Config {
    name: String // required
    port: int = 8080 // default value
    proxy: ?String // optional, defaults to none
}

fn load(path: str) -> Config ! os.Error {
    let text = try os.readText(path)
    Config{name: text.trim().toString()}
}

fn main() {
    let cfg = load("app.toml") catch e {
        eprintln("using defaults:", e)
        .{name: "app"}
    }
    let backups = [8081, 80, 9000]
        .filter(p => p > 1024)
        .map(p => ":${p}")
    println("${cfg.name} on :${cfg.port}, backups ${backups.join(", ")}, proxy: ${cfg.proxy ?? "none"}")
}
```

## Packages and imports

There is no `package` declaration. A directory is a package, and the package name is the directory name. The entry package of `gojo run` holds the `main` function. Imports use `use`, and paths are not quoted:

```gojo
use std/{fmt, os, time}        // standard library: all under std, several per line
use ./geom                     // a package in this project: path relative to the current package directory
use ../shared/util as u        // nested path, renamed with as
use ./geom.{Vec2, Shape}       // bring names directly into scope; geom itself is not bound
```

The start of the path tells you whether it is the standard library (`std/`) or a package in the project (`./`), so the two never clash. `use std/os` binds the name `os`. `print`/`println`/`eprint`/`eprintln` are in the prelude, so hello world needs no imports; `fmt` only holds formatting functions such as `fmt.sprint` and `fmt.format`. An unused import (including a name inside `.{...}`) is a compile error.

## Methods

Methods are written together with their type. The receiver is an explicit `self`:

```gojo
struct Counter {
    n: int = 0

    fn new(start: int) -> Counter {        // no self: associated function, Counter.new(10)
        return Counter{n: start}
    }

    fn value(self) -> int {               // borrow
        return self.n
    }

    fn bump(inout self) {              // mutable borrow
        self.n += 1
    }
}

extend Counter {                         // add methods, e.g. in another file of the same package
    fn reset(inout self) { self.n = 0 }
}

interface Bumper {
    fn bump(inout self)                // interface methods use the same syntax
}
```

## Installation

Gojo supports macOS and Linux (x86-64, arm64). It **does not support Windows** (the runtime depends on POSIX). Gojo compiles to C, so you need a C compiler: `cc`, `clang`, `gcc` or `zig` on `PATH` (searched in that order; on macOS run `xcode-select --install`; cross-compilation uses `zig`).

A release archive contains a statically linked `gojo` executable (with the standard library and runtime compiled in), plus the docs and examples:

```sh
curl -fsSL https://github.com/egoist/gojo/releases/latest/download/install.sh | sh
sh scripts/install.sh --archive dist/gojo-0.1.0-macos-arm64.tar.gz    # or install a local archive
curl -fsSL https://github.com/egoist/gojo/releases/download/nightly/install.sh | sh -s -- --nightly   # the latest nightly build
```

It installs to `~/.gojo` (change it with `GOJO_HOME`). `gojo version` shows the version. Building from source only needs the Rust toolchain: `cargo build --release`.

## Quick start

```sh
cargo build --release
./target/release/gojo run examples/hello.gojo
./target/release/gojo run examples/json.gojo
./target/release/gojo run examples/shapes        # one directory = one package, with the subpackage geom
./target/release/gojo test examples/shapes       # run test "..." { } blocks
```

| Command | What it does |
| --- | --- |
| `gojo run <file-or-dir> [args...]` | Compile and run |
| `gojo build <file-or-dir> [-o output]` | Build a native binary |
| `gojo check <file-or-dir> [--json]` | Type check and borrow check only; `--json` prints each diagnostic as a line of JSON, with the edits that fix it where there is one obvious fix (`&` for an `inout` argument, `_` for an unused variable, the move for a needless clone) |
| `gojo test <file-or-dir>` | Run `test` blocks; each test runs in its own process, so a panic does not affect other tests |
| `gojo fmt [--check] [file-or-dir...]` | Format in place (current directory by default); `--check` only lists unformatted files and exits with code 1 if there are any |
| `gojo doc [module-or-path] [--out dir]` | API docs in Markdown: the standard library index, one standard library module, or one package; `--out` writes the whole standard library to a directory (that is, [`docs/std`](docs/std/README.md)) |
| `gojo emit-c <file-or-dir>` | Print the generated C code |
| `gojo init [name]` | Write a `gojo.toml` (package manifest, see [`docs/packages.md`](docs/packages.md)) in the current directory |
| `gojo spec` | The whole language on one page ([`docs/brief.md`](docs/brief.md)), sized for a coding agent's context |
| `gojo version` | Version, commit and target platform |

Options: `--release` (`-O2`; overflow checks stay on), `--cc "<compiler flags>"`, `--target <triple>` (cross-compile with `zig cc`; for example `--target x86_64-linux-musl` gives a static Linux binary directly), `-v` (print the time of each phase).

## Safety guarantees, and how they are enforced

Every check looks only at function signatures and a single function body. There is no cross-function inference and there are no lifetime annotations. Each guarantee has compile-error tests (`tests/errors/`, 153 in total).

| Guarantee | Mechanism | Typical error |
| --- | --- | --- |
| No nil | Only `?T` can be empty, and it must be unwrapped before use | `` undefined: `nil` `` / `` `none` is not a value of type `int` `` |
| No use after free | Move semantics + a "maybe moved" data-flow analysis; loops run twice to find moves from the previous iteration | `` use of `s` after it was moved `` / `moved here in a previous iteration of the loop` |
| References do not dangle | References can only be parameters, projections (`yield`) or block-scoped bindings. Views (`str`, `[]T`) live only in variables; they cannot go into fields, collections, boxes, interface values, generic types or escaping closures (inferred types included). When a view is assigned to an outer variable, the borrowed value must live at least as long. A view of data inside a lock cannot leave the `with` block | `this temporary value is dropped at the end of the statement` / `` returns a view of `s` `` / `` `s` does not live long enough `` |
| Iterators are not invalidated | `for`/`with`/`if let` bindings hold a borrow inside the block | `` cannot modify `v` while it is borrowed by `x` `` |
| Exclusive mutable borrows | Arguments of the same call are checked for overlap; `inout` must be written with `&` at the call site | `` `a` is used by two arguments of the same call `` |
| No data races at compile time | `send` is derived from the structure of the type; `Rc`/`Cell`/raw pointers cannot cross tasks; a variable a task changes is borrowed by that task alone until its scope ends (or the range of it the task uses); shared mutable state goes through a `Mutex`, `RwLock` or `Atomic` | `` a task captures `r` of type `Rc[int]`, which is not safe to share between threads `` / `` cannot use `total`: a task modifies it until its scope ends `` |
| Linear values are consumed | A `linear struct` is never dropped implicitly: every path must hand it to a `sink` parameter (its `commit`/`rollback`), return it, or store it in something consumed in turn; a `defer` that consumes it runs only while it is still there | `` `tx` is not consumed: the linear `Tx` would be dropped by this `return` `` |
| Errors must be handled | An unhandled `T ! E` is a compile error; to discard it, write `_ = f()` | `` result of type `! String` is not handled `` |
| Exhaustive matching | Based on the usefulness algorithm; reports the exact missing pattern | `` `match` is not exhaustive: `Rect(_, _)` is not covered `` |
| Runtime checks | Integer overflow, out-of-bounds index, division by zero, out-of-range shift, out-of-range numeric conversion and a failed `unwrap` all panic and report the call site | `panic: integer overflow in addition` |

Also: `let` is immutable; shadowing within one function is not allowed; unused variables and imports are errors; a `copy` type cannot contain fields that need to be freed; recursive types of infinite size are errors; calling an `extern` function or an `unsafe fn` and dereferencing a raw pointer must happen inside an `unsafe` block, and an `unsafe` block that needs none of these is an error.

The memory safety of the generated code is verified separately. All test programs run clean under AddressSanitizer + UndefinedBehaviorSanitizer (`cargo test -- --ignored`), and macOS `leaks` finds no leaks. The checks themselves have also been attacked systematically: `gojo-fuzz` runs mutation fuzzing and differential testing against the compiler (see "Testing" below), and a separate set of hand-built programs tries to get around the borrow checker. Every hole found has been fixed and added to `tests/errors/`.

## Implemented language features

- **Basics**: `let`/`var`, package-level `let` values (computed once, on first use), `for x in xs`, `for i in 0..n`, `while cond {}`, `loop {}`, loop labels (`outer: for ...`, `break outer`), `if`/`match`/blocks as expressions (a function body's last expression is its result), `defer` (block-scoped), `errdefer`, `const`, type aliases and newtypes (`type Celsius f64`; operators are kept when the underlying type is a number), closures `x => x * 2` and `(a, b) => a + b`, string interpolation checked at compile time (`"${name}: ${total:.2}"`), method chains written one step per line (a line starting with `.name` continues the one before).
- **Types**: all scalars, tuples (`(int, String)`, `let (q, r) = divmod(a, b)`, `t.0`, tuple patterns), `String`/`str`, `Vec[T]`/`[]T` (a slice is read-only unless it is an `inout []T`), `[N]T`, `Map[K, V]`, `?T`, `T ! E`, `Box`, `Rc`/`Weak`, `Arc`, `Cell`, `Mutex`, `RwLock`, `Atomic`, channels (`Chan[T]`, `Sender[T]`, `Receiver[T]`), `Task[T]`, `Iso[T]`, function types (`fn`, `send fn`), `*T`.
- **Methods**: written inside the braces of a `struct`/`enum`; the first parameter, `self`, carries the receiver mode. Associated functions: `Stack[String].new()` (generic arguments can be inferred from the expected type); a type with a `new` function is called like one, `Stack[String]()`, `RwLock(x)`, `Atomic(0)`. `extend T { ... }` adds methods; the standard library also uses it to add methods to built-in types and to specific instances of them (such as `extend []String`).
- **Collections**: `[a, b]` lists (a `Set` where one is expected), `["k": v]` / `[:]` maps, and `if` / `for` / `..` elements inside list literals, so built data keeps its shape: `[header, if let e = err { Text{text: e.clone()} }, for t in todos { row(t) }, ..footer()]` (see [`examples/ui`](examples/ui)).
- **Structs and enums**: field defaults (a missing required field is a compile error), `copy struct`, automatic copy derivation for package-private types, sum-type enums, `.Variant` shorthand and `.{field: v}` literals typed by the context, `P{x, y}` field shorthand, struct embedding (fields and methods are promoted).
- **Ownership**: three parameter modes (borrow / `inout` / `sink`), deterministic destruction (reverse scope order, `drop` method, `drop(x)`), partial moves, conditional moves (flags are inserted only when needed), `.clone()`. Linear types (`linear struct Tx`) that must be consumed, never dropped, with guarded `defer tx.rollback()`.
- **Projections**: `yield &T` / `yield inout T` / `yield ?&T` functions. `v[i]`, `m.get(k)`, `m.entry(k)` and `mu.lock()` are all projections, and the reference they lend cannot leave the block. References and views bound with `let` stop holding the borrowed value after their last use. Structures such as parsers store ranges as `Span` and do not need self-references.
- **Generators**: `gen fn lines(src: str) -> str { ... yield src[a..b] ... }` consumed by `for l in lines(text)`; recursive and nested generators, `break`/`return` from the loop stop them; compiled to the push protocol (a call per value, no coroutine).
- **Custom iteration**: a cursor protocol (`cursor` + `next`/`nextMut`; `zip` and merging just advance several cursors together) and a push protocol (`each`/`eachMut`, suited to recursive structures such as trees). In both protocols `break`, `continue`, `return`, `try` and labeled jumps work as usual.
- **Error handling**: `try`, `try x else Wrap`, `catch e { ... }`, `match` destructuring of `ok`/`err`, `try` on `?T`, `??`, `?.` for fields and method calls, `if let x { }` to unwrap a variable in place.
- **Interfaces**: structural, implicit satisfaction; interface values (heap box + vtable); borrowed interface parameters (fat pointer, no allocation); mutable-receiver methods such as `fn bump(inout self) -> int`; `match` branching on the concrete type; embedded interfaces; constraint interfaces with type sets (`~int | ~f64`).
- **Static reflection**: `static for f in fields(T)` with `x.[f]` and `f.name`, `static if T: Integer`, `static if let v: Vec[_] = x`, `static if T is String`, and `static T{f => ...}`: serializers and decoders written once for every type, checked once like other generic code (no macros, no compile-time evaluation).
- **Generics**: generic functions/structs/enums, methods with their own type parameters, `where`-conditional methods, built-in capability constraints (`copy`, `clone`, `default`, `send`, `comparable`, `ordered`, `hash`), local bidirectional inference (including closure return types), integer const generics (`Mat[R, C]`; mismatched dimensions are a compile error). A type with custom `equal`/`hash`/`compare` methods can be a Map key or be sorted.
- **Concurrency**: tasks are stackful coroutines with M:N scheduling, started with `go` inside a `scope`, which waits for them; they borrow what outlives the scope (down to disjoint ranges of one slice: `go psort(&v[..p])` and `go psort(&v[p + 1..])`), `let t = go f()` gives a handle whose `t.join()` returns the result, and a failing task cancels the others (`try scope { }`). `par for`, `task.map`/`each`/`race`, `send fn`. Channels with two ends that close when the last sender is dropped; `select` over channels, tasks, timers (`time.after`) and cancellation, with guards. `Mutex`, `Mutex(&x)`, `RwLock`, `with a, b = lock(m1, m2)`, `Atomic`, `send interface`, `isolate { }`, task-local values (`task.local`, `task.with`); waiting inside a lock block is an error. Preemption after 10 ms of running while others wait; C calls hand the CPU to other threads. A file read that has to wait parks its task: pipes and terminals on the poller, disk reads through io_uring on Linux. Parked tasks' stacks are hibernated (100,000 idle tasks: 0.2 GB on macOS).
- **Cancellation**: propagates along the task tree (a failing task, `s.cancel()`, a deadline from `task.timeout`, Ctrl-C for the whole program); blocking operations in a cancelled task return immediately; `task.reason()`, `task.deadline()`, `task.shield`, `try task.check()`; `drop` methods run shielded so cleanup that blocks still happens.
- **Diagnosing and testing**: deadlocks are reported with every task, what it waits for and which task holds the lock; `task.dump()` and SIGQUIT print the same; debug builds warn about locks taken in opposite orders. `gojo test` runs on a seeded simulated scheduler with virtual time; `--explore n` finds an interleaving that fails and prints the seed that replays it.
- **Networking**: `net.listen`/`accept`/`dial`. Reads and writes on a `Conn` do not block the thread and can be cancelled. [`examples/http`](examples/http) is an HTTP/1.1 server with request timeouts, cancellation when the client disconnects, and cancellation of in-flight requests on shutdown.
- **unsafe and FFI**: `extern "C" lib "libz" { ... }` uses `dlopen` at run time, so the build needs no C headers. Without `lib`, calls go directly to libc. `extern "C" static` links statically (this also works in static musl binaries). `extern "C" fn` exports a Gojo function. With `unsafe send struct` the author vouches for thread safety. Types with no C representation (`String`, function values, etc.) cannot appear in `extern` signatures. `unsafe fn` marks a function whose callers uphold what the compiler can't check (its body is checked as usual). The reference lists what unsafe code must never do. `gojo unsafe` lists the unsafe code of every package, and a dependency with any builds only when `gojo.toml` allows it (`[unsafe] allow`; `forbid = true` for the package's own code). C may keep pointers into task stacks: `.ptr()` pins the stack it points into, so hibernation leaves it alone.
- **Standard library**: prelude (Option/Result/string/slice/Vec/Map/`Set` methods, `Arena[T]` + `Id[T]` with generations, `Hasher`), `fmt` (including `fmt.format` with compile-time-checked templates), `io` (a common error type and `Reader`/`Writer`), `bufio`, `os` (files, directories, environment variables, child processes), `time` (monotonic clock, dates, formatting and parsing), `json` (including struct conversion), `net` (TCP, UDP, DNS), `http` (server and client, keep-alive, TLS), `tls`, `task`, `math`, `strings`, `rand`, `path`, `hex`, `base64`, `url`, `csv`, `binary`, `collections`, `crypto`, `crc32`, `regex`, `flag`, `log`. The method signatures of built-in types are written in Gojo in `std/prelude.gojo`; `map`/`filter`/`fold`/`join`/`Arena`/`Set` there, and modules such as `bufio`, `json` and `http`, are themselves Gojo code. The module APIs are in section 12 of [`docs/spec.md`](docs/spec.md). [`examples/todo`](examples/todo) (a JSON API service) and [`examples/grep`](examples/grep) (a command-line tool) are written using only the standard library.

The programs under `tests/run/` cover all of the features above and can be read as language examples. The exact language rules are in [`docs/spec.md`](docs/spec.md); each section lists the tests that cover it.

## Implementation layout

```
src/lexer.rs      lexing (newlines end statements; string interpolation)
src/parser.rs     recursive-descent parsing
src/check/        declaration collection, type resolution and capability rules (mod.rs)
                  function body type checking → typed IR (body.rs)
                  patterns, match, exhaustiveness (pattern.rs)
src/borrowck.rs   ownership and borrow checking (per function, single pass + two passes over loops; plus two whole-program checks:
                  values that cannot store views, generic functions that keep type parameters)
src/codegen.rs    IR → C: monomorphization, destructor insertion, vtables, closures, built-in functions
src/lib.rs        library interface of the front end (shared by the driver and the fuzzer); src/main.rs is the `gojo` command
src/bin/gojo-fuzz mutation fuzzing, differential testing (progen.rs generates random programs), borrow checker attacks (borrowgen.rs), reduction
runtime/          C runtime: strings, Vec, insertion-ordered hash map, small-object allocator, panic, FFI (gojo_rt.c);
                  M:N scheduler, context switching, channels, select, mutex, scopes, task handles and
                  cancellation, timers, kqueue/epoll network polling, stack hibernation, task dumps and
                  deadlock reports, the simulated scheduler for tests (gojo_sched.c)
std/              standard library written in Gojo (compiled into the binary)
```

The back end generates C and then calls the system C compiler. The runtime library is compiled once per (compiler, flags) pair and cached in `~/.cache/gojo`. Generated binaries are cached by the hash of the C source; when the source is unchanged, a repeated `run` takes about 10 ms.

## Performance

[`bench/`](bench/README.md) has equivalent Gojo / Go / Zig implementations of 8 common benchmarks (fib, binary-trees, nbody, spectral-norm, fannkuch-redux, mandelbrot, hashmap, sort). `bench/run.sh` first checks that the outputs of all three agree, then measures. Results on an Apple M5:

- Run time (geometric mean): Gojo takes 0.78× the time of Go, 0.80× of Zig ReleaseSafe and 0.87× of Zig ReleaseFast, with overflow and index checks on. binary-trees benefits from Gojo's small-object allocator; without it the figures are 0.87×, 0.94× and 1.03×.
- Of the 8 benchmarks, Gojo is fastest on binary-trees, mandelbrot, hashmap and sort. The gap on fib is the cost of the overflow checks themselves (on par with Zig ReleaseSafe). nbody is on par with Go and slower than Zig.
- Compile time: `gojo build` of nbody takes about 50 ms (about 3 ms of it in the front end), `go build` 97 ms, `zig build-exe` Debug 924 ms.

On a 40,000-line synthetic program, the Gojo front end takes about 66 ms (parsing 23 ms, type checking 34 ms, borrow checking 3 ms, C generation 6 ms).

Concurrency (see [bench/README.md](bench/README.md#goroutine)): 100,000 simultaneously suspended tasks passing a value along a chain take 0.38 s / 441 MB with Gojo and 0.11 s / 284 MB with Go on Linux. 100,000 tasks parked for seconds take 0.2 GB on macOS (1.7 GB without hibernation). 1,000,000 short-lived tasks (in batches of 1,000) take 0.43 s with Gojo and 0.26 s with Go.

## Not yet implemented

- Windows; a language server (LSP); a package registry and version resolution (for now `gojo.toml` dependencies are local paths, and workspaces share one manifest). The debugger can set breakpoints on Gojo lines and show the call stack, but variable names are the names in the generated C (`p_x`, `l0_total`).
- Networking has TCP, UDP and DNS; HTTP/1.1 over TCP or TLS, with keep-alive on both sides. No HTTP/2 or HTTP/3, and no Unix-socket listeners.
- On Linux kernels before 6.13, each task uses two memory mappings. The default `vm.max_map_count` (262144) supports about 130,000 live tasks; beyond that you get an error suggesting you raise it.
- `#specialize`, PGO.
- `saturatingAdd`, pointer arithmetic, `unsafe.cast` and `unsafe.forget` from the design notes, and passing Gojo functions to C as callbacks (`extern "C" fn` can currently only be exported and called directly).
- Separate compilation per package and caching of export data (for now everything is compiled as a whole each time; the front-end speed and the binary cache make up for it).
- Interface values and function values do not satisfy `send` (conservative); a closure passed to `go` is checked separately based on what it captures.

## Testing

```sh
cargo test                  # run programs and compare output, compile-error cases, runtime panic cases
cargo test -- --ignored     # run all programs under ASan + UBSan, and concurrent programs under TSan
```

- `tests/run/*.gojo`: programs and their expected output `.out`
- `tests/errors/*.gojo`: must fail to compile; `// error: ...` comments list the expected errors
- `tests/panic/*.gojo`: must panic with exit code 2 and report the call site
- `tests/projects/*/`, `examples/*/`: multi-package projects, compared against `expected.out` or `expected.err`
- `tests/fuzz/*.gojo`: compiler crashes found by fuzzing (reduced); each must produce a diagnostic or compile successfully
- `tests/unsafe/*.gojo`: 151 hand-built programs that try to get around the safety checks; all must be rejected

### Fuzzing

```sh
cargo build --profile fuzz              # optimized build that keeps debug assertions and overflow checks
target/fuzz/gojo-fuzz --jobs 4          # mutation fuzzing: compiler crashes, timeouts, generated C that fails to compile, ASan errors
target/fuzz/gojo-fuzz diff --jobs 2     # differential testing: random programs must give the same result in debug / release / sanitizer builds
target/fuzz/gojo-fuzz borrow --jobs 2   # borrow checker: random programs that juggle views; accepted ones must be clean under ASan
target/fuzz/gojo-fuzz fmt 6000         # formatter: tokens unchanged, output stable, independent of the original indentation
target/fuzz/gojo-fuzz toml 200000      # gojo.toml reader: mutated manifests must not make it panic
target/fuzz/gojo-fuzz reduce fuzz-out/findings/xxx.gojo   # reduce a reproducer
```

Results go to `fuzz-out/findings/`. Rules and limits are in section 15 of [`docs/spec.md`](docs/spec.md).

## Releases

`scripts/release.sh` builds four release archives on a Mac (`macos-arm64`, `macos-x86_64`, `linux-x86_64`, `linux-arm64`; the Linux ones are statically linked against musl with `cargo zigbuild`), plus `SHA256SUMS`, `VERSION` and `install.sh`, in `dist/`. When a `vX.Y.Z` tag is pushed, `.github/workflows/release.yml` does the same and creates a GitHub release (versions containing `-` are marked as prerelease). `ci.yml` runs the tests on Linux (x86-64, arm64) and macOS, and also runs the sanitizers on Linux.

Every night (03:00 UTC), `.github/workflows/nightly.yml` takes the newest commit on `main` that passed `ci.yml` and, if the code (`src`, `runtime`, `std`, `examples`, the build files, `release.sh`, `install.sh`) changed since the previous nightly, builds it and replaces the `nightly` prerelease and its tag. A nightly's version is Cargo.toml's with `-nightly.YYYYMMDD` appended (`GOJO_VERSION` overrides the version for `release.sh` and `gojo version`). Running the workflow by hand publishes now; its `force` option publishes even without code changes.
