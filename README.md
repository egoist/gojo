# Gojo

**Documentation:** [A tour of Gojo](docs/tour.md) · [Standard library reference](docs/std/README.md) · [Language reference](docs/spec.md) · [Design notes](docs/design.md) · [Packages](docs/packages.md)

Gojo is a systems programming language. It keeps Go's simplicity and compile speed, uses Rust-style ownership to remove the GC and nil, and takes Zig's error unions, `errdefer`, explicit wrapping arithmetic and zero-dependency cross-compilation.

This repository is a prototype compiler built from the [design notes](docs/design.md). The compiler is written in Rust (about 19,000 lines, no third-party dependencies). The C runtime is about 4,000 lines, and the Gojo standard library about 3,400 lines.

```zig
use std/os

struct Config {
    name String // required
    port int = 8080 // default value
    proxy ?String // optional, defaults to none
}

fn load(path str) Config ! os.Error {
    let text = try os.readText(path)
    return Config{name: text.trim().toString()}
}

fn main() {
    let cfg = load("app.toml") catch e {
        eprintln("using defaults:", e)
        Config{name: "app"}
    }
    var hits = 0
    for i in 0..10 { hits += i }
    println(cfg.name, cfg.port, hits)
}
```

## Packages and imports

There is no `package` declaration. A directory is a package, and the package name is the directory name. The entry package of `gojo run` holds the `main` function. Imports use `use`, and paths are not quoted:

```zig
use std/{fmt, os, time}        // standard library: all under std, several per line
use ./geom                     // a package in this project: path relative to the current package directory
use ../shared/util as u        // nested path, renamed with as
use ./geom.{Vec2, Shape}       // bring names directly into scope; geom itself is not bound
```

The start of the path tells you whether it is the standard library (`std/`) or a package in the project (`./`), so the two never clash. `use std/os` binds the name `os`. `print`/`println`/`eprint`/`eprintln` are in the prelude, so hello world needs no imports; `fmt` only holds formatting functions such as `fmt.sprint` and `fmt.format`. An unused import (including a name inside `.{...}`) is a compile error.

## Methods

Methods are written together with their type. The receiver is an explicit `self`:

```zig
struct Counter {
    n int = 0

    fn new(start int) Counter {        // no self: associated function, Counter.new(10)
        return Counter{n: start}
    }

    fn value(self) int {               // borrow
        return self.n
    }

    fn bump(self inout) {              // mutable borrow
        self.n += 1
    }
}

extend Counter {                         // add methods, e.g. in another file of the same package
    fn reset(self inout) { self.n = 0 }
}

interface Bumper {
    fn bump(self inout)                // interface methods use the same syntax
}
```

## Installation

Gojo supports macOS and Linux (x86-64, arm64). It **does not support Windows** (the runtime depends on POSIX). Gojo compiles to C, so you need a C compiler: `cc`, `clang`, `gcc` or `zig` on `PATH` (searched in that order; on macOS run `xcode-select --install`; cross-compilation uses `zig`).

A release archive contains a statically linked `gojo` executable (with the standard library and runtime compiled in), plus the docs and examples:

```sh
curl -fsSL https://github.com/egoist/gojo/releases/latest/download/install.sh | sh
sh scripts/install.sh --archive dist/gojo-0.1.0-macos-arm64.tar.gz    # or install a local archive
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
| `gojo check <file-or-dir>` | Type check and borrow check only |
| `gojo test <file-or-dir>` | Run `test` blocks; each test runs in its own process, so a panic does not affect other tests |
| `gojo fmt [--check] [file-or-dir...]` | Format in place (current directory by default); `--check` only lists unformatted files and exits with code 1 if there are any |
| `gojo doc [module-or-path] [--out dir]` | API docs in Markdown: the standard library index, one standard library module, or one package; `--out` writes the whole standard library to a directory (that is, [`docs/std`](docs/std/README.md)) |
| `gojo emit-c <file-or-dir>` | Print the generated C code |
| `gojo init [name]` | Write a `gojo.toml` (package manifest, see [`docs/packages.md`](docs/packages.md)) in the current directory |
| `gojo version` | Version, commit and target platform |

Options: `--release` (`-O2`; overflow checks stay on), `--cc "<compiler flags>"`, `--target <triple>` (cross-compile with `zig cc`; for example `--target x86_64-linux-musl` gives a static Linux binary directly), `-v` (print the time of each phase).

## Safety guarantees, and how they are enforced

Every check looks only at function signatures and a single function body. There is no cross-function inference and there are no lifetime annotations. Each guarantee has compile-error tests (`tests/errors/`, 83 in total).

| Guarantee | Mechanism | Typical error |
| --- | --- | --- |
| No nil | Only `?T` can be empty, and it must be unwrapped before use | `` undefined: `nil` `` / `` `none` is not a value of type `int` `` |
| No use after free | Move semantics + a "maybe moved" data-flow analysis; loops run twice to find moves from the previous iteration | `` use of `s` after it was moved `` / `moved here in a previous iteration of the loop` |
| References do not dangle | References can only be parameters, projections (`yield`) or block-scoped bindings. Views (`str`, `[]T`) live only in variables; they cannot go into fields, collections, boxes, interface values, generic types or escaping closures (inferred types included). When a view is assigned to an outer variable, the borrowed value must live at least as long. A view of data inside a lock cannot leave the `with` block | `this temporary value is dropped at the end of the statement` / `` returns a view of `s` `` / `` `s` does not live long enough `` |
| Iterators are not invalidated | `for`/`with`/`if let` bindings hold a borrow inside the block | `` cannot modify `v` while it is borrowed by `x` `` |
| Exclusive mutable borrows | Arguments of the same call are checked for overlap; `inout` must be written with `&` at the call site | `` `a` is used by two arguments of the same call `` |
| No data races at compile time | `send` is derived from the structure of the type; `rc`/`Cell`/raw pointers cannot cross goroutines; shared mutable state must go through a `Mutex` | `` argument of type `rc[int]` cannot be sent to another goroutine `` / `a task cannot modify the captured variable` |
| Errors must be handled | An unhandled `T ! E` is a compile error; to discard it, write `_ = f()` | `` result of type `! String` is not handled `` |
| Exhaustive matching | Based on the usefulness algorithm; reports the exact missing pattern | `` `match` is not exhaustive: `Rect(_, _)` is not covered `` |
| Runtime checks | Integer overflow, out-of-bounds index, division by zero, out-of-range shift, out-of-range numeric conversion and a failed `unwrap` all panic and report the call site | `panic: integer overflow in addition` |

Also: `let` is immutable; shadowing within one function is not allowed; unused variables and imports are errors; a `copy` type cannot contain fields that need to be freed; recursive types of infinite size are errors; calling an `extern` function and dereferencing a raw pointer must happen inside an `unsafe` block.

The memory safety of the generated code is verified separately. All test programs run clean under AddressSanitizer + UndefinedBehaviorSanitizer (`cargo test -- --ignored`), and macOS `leaks` finds no leaks. The checks themselves have also been attacked systematically: `gojo-fuzz` runs mutation fuzzing and differential testing against the compiler (see "Testing" below), and a separate set of hand-built programs tries to get around the borrow checker. Every hole found has been fixed and added to `tests/errors/`.

## Implemented language features

- **Basics**: `let`/`var`, `for x in xs`, `for i in 0..n`, `for cond {}`, `for {}`, loop labels (`outer: for ...`, `break outer`), `if`/`match`/blocks as expressions, `defer` (block-scoped), `errdefer`, `const`, type aliases and newtypes (`type Celsius f64`; operators are kept when the underlying type is a number), `(x) => x * 2` short closures.
- **Types**: all scalars, `String`/`str`, `Vec[T]`/`[]T`, `[N]T`, `Map[K, V]`, `?T`, `T ! E`, `Box`, `rc`/`weak`, `arc`, `Cell`, `Mutex`, `chan T`, function types, `*T`.
- **Methods**: written inside the braces of a `struct`/`enum`; the first parameter, `self`, carries the receiver mode. Associated functions: `Stack[String].new()` (generic arguments can be inferred from the expected type). `extend T { ... }` adds methods; the standard library also uses it to add methods to built-in types and to specific instances of them (such as `extend []String`).
- **Structs and enums**: field defaults (a missing required field is a compile error), `copy struct`, automatic copy derivation for package-private types, sum-type enums, `.Variant` shorthand, struct embedding (fields and methods are promoted).
- **Ownership**: three parameter modes (borrow / `inout` / `sink`), deterministic destruction (reverse scope order, `drop` method, `drop(x)`), partial moves, conditional moves (flags are inserted only when needed), `.clone()`.
- **Projections**: `yield &T` / `yield inout T` / `yield ?&T` functions. `v[i]`, `m.get(k)`, `m.entry(k)` and `mu.lock()` are all projections, and the reference they lend cannot leave the block. References and views bound with `let` stop holding the borrowed value after their last use. Structures such as parsers store ranges as `Span` and do not need self-references.
- **Custom iteration**: a cursor protocol (`cursor` + `next`/`nextMut`; `zip` and merging just advance several cursors together) and a push protocol (`each`/`eachMut`, suited to recursive structures such as trees). In both protocols `break`, `continue`, `return`, `try` and labeled jumps work as usual.
- **Error handling**: `try`, `try x else Wrap`, `catch e { ... }`, `match` destructuring of `ok`/`err`, `try` on `?T`, `??`, `?.`.
- **Interfaces**: structural, implicit satisfaction; interface values (heap box + vtable); borrowed interface parameters (fat pointer, no allocation); mutable-receiver methods such as `fn bump(self inout) int`; `match` branching on the concrete type; embedded interfaces; constraint interfaces with type sets (`~int | ~f64`).
- **Generics**: generic functions/structs/enums, methods with their own type parameters, `where`-conditional methods, built-in capability constraints (`copy`, `clone`, `default`, `send`, `comparable`, `ordered`, `hash`), local bidirectional inference (including closure return types), integer const generics (`Mat[R, C]`; mismatched dimensions are a compile error). A type with custom `equal`/`hash`/`compare` methods can be a Map key or be sorted.
- **Concurrency**: stackful coroutines with M:N scheduling (100,000 goroutines take about 440 MB on Linux). Blocking channels, locks, `time.sleep` and network operations suspend only the goroutine. Preemption: when other goroutines are waiting, a goroutine gives up the CPU after running for 10 ms (the geometric mean of cycle counts over the 8 benchmarks did not change; the largest single change is +5%). During C calls and file I/O the CPU is handed to other threads. `task.setMaxProcs`, `task.stats()`; `go`, buffered/unbuffered channels (sending transfers ownership), `close`, `for x in ch`, `select` (branches are written like `match`: `let x = <-ch => ...`, `ch <- v => ...`, `_ => ...`), `arc(Mutex(x))`, `Mutex(&x)` (a lock that borrows outside data, for use inside `task.group`), deadlock detection.
- **Structured concurrency and cancellation**: `task.group` (tasks borrow data from outside the group and take values from the group body; the first error becomes the group's result). Cancellation propagates along the task tree (`g.cancel()`, a failed child task, the deadline of `task.timeout(d, body)`). Blocking operations in a cancelled task return immediately. `try task.check()` is a checkpoint.
- **Networking**: `net.listen`/`accept`/`dial`. Reads and writes on a `Conn` do not block the thread and can be cancelled. [`examples/http`](examples/http) is an HTTP/1.1 server with request timeouts, cancellation when the client disconnects, and cancellation of in-flight requests on shutdown.
- **unsafe and FFI**: `extern "C" lib "libz" { ... }` uses `dlopen` at run time, so the build needs no C headers. Without `lib`, calls go directly to libc. `extern "C" static` links statically (this also works in static musl binaries). `extern "C" fn` exports a Gojo function. With `unsafe send struct` the author vouches for thread safety. Types with no C representation (`String`, function values, etc.) cannot appear in `extern` signatures.
- **Standard library**: prelude (Option/Result/string/slice/Vec/Map/`Set` methods, `Arena[T]` + `Id[T]` with generations, `Hasher`), `fmt` (including `fmt.format` with compile-time-checked templates), `io` (a common error type and `Reader`/`Writer`), `bufio`, `os` (files, directories, environment variables, child processes), `time` (monotonic clock, dates, formatting and parsing), `json`, `net`, `http` (server and client), `task`, `math`, `strings`. The method signatures of built-in types are written in Gojo in `std/prelude.gojo`; `map`/`filter`/`fold`/`join`/`Arena`/`Set` there, and modules such as `bufio`, `json` and `http`, are themselves Gojo code. The module APIs are in section 12 of [`docs/spec.md`](docs/spec.md). [`examples/todo`](examples/todo) (a JSON API service) and [`examples/grep`](examples/grep) (a command-line tool) are written using only the standard library.

The programs under `tests/run/` cover all of the features above and can be read as language examples. The exact language rules are in [`docs/spec.md`](docs/spec.md); each section lists the tests that cover it.

## Implementation layout

```
src/lexer.rs      lexing (Go-style automatic semicolons)
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
                  M:N scheduler, context switching, channels, select, mutex, task groups and cancellation,
                  timers, kqueue/epoll network polling (gojo_sched.c)
std/              standard library written in Gojo (compiled into the binary)
```

The back end generates C and then calls the system C compiler. The runtime library is compiled once per (compiler, flags) pair and cached in `~/.cache/gojo`. Generated binaries are cached by the hash of the C source; when the source is unchanged, a repeated `run` takes about 10 ms.

## Performance

[`bench/`](bench/README.md) has equivalent Gojo / Go / Zig implementations of 8 common benchmarks (fib, binary-trees, nbody, spectral-norm, fannkuch-redux, mandelbrot, hashmap, sort). `bench/run.sh` first checks that the outputs of all three agree, then measures. Results on an Apple M5:

- Run time (geometric mean): Gojo takes 0.78× the time of Go, 0.80× of Zig ReleaseSafe and 0.87× of Zig ReleaseFast, with overflow and index checks on. binary-trees benefits from Gojo's small-object allocator; without it the figures are 0.87×, 0.94× and 1.03×.
- Of the 8 benchmarks, Gojo is fastest on binary-trees, mandelbrot, hashmap and sort. The gap on fib is the cost of the overflow checks themselves (on par with Zig ReleaseSafe). nbody is on par with Go and slower than Zig.
- Compile time: `gojo build` of nbody takes about 50 ms (about 3 ms of it in the front end), `go build` 97 ms, `zig build-exe` Debug 924 ms.

On a 40,000-line synthetic program, the Gojo front end takes about 66 ms (parsing 23 ms, type checking 34 ms, borrow checking 3 ms, C generation 6 ms).

Concurrency (see [bench/README.md](bench/README.md#goroutine)): 100,000 simultaneously suspended goroutines passing a value along a chain take 0.38 s / 441 MB with Gojo and 0.11 s / 284 MB with Go on Linux. 1,000,000 short-lived goroutines (in batches of 1,000) take 382 ms with Gojo and 278 ms with Go.

## Not yet implemented

- Windows; a language server (LSP); a package manager (for now a program is the directories it uses). The debugger can set breakpoints on Gojo lines and show the call stack, but variable names are the names in the generated C (`p_x`, `l0_total`).
- Networking has only TCP and plain-text HTTP/1.1 (one request per connection); no UDP, Unix sockets or TLS. JSON cannot yet be converted to and from structs automatically.
- On Linux kernels before 6.13, each goroutine uses two memory mappings. The default `vm.max_map_count` (262144) supports about 130,000 live goroutines; beyond that you get an error suggesting you raise it.
- `#specialize`, PGO.
- `RwLock[T]`, `Atomic[T]`, `saturatingAdd`, `unsafe.cast` and `unsafe.forget` from the design notes, and passing Gojo functions to C as callbacks (`extern "C" fn` can currently only be exported and called directly).
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
