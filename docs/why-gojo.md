# Why Gojo

Gojo is for the programs people write in Rust, Go or Zig today: command-line tools, network services, and libraries that should start fast, use little memory and not crash. It gives you Rust's guarantees (no use after free, no nil, no data races) without lifetime annotations, Go's simplicity and `go` statement without a garbage collector, and Zig's error handling with memory that frees itself. This page compares it with the major languages, says where each one is the better choice, and shows the numbers.

Gojo is a beta. The language, the compiler and the standard library work, are tested and are fuzzed, but the ecosystem is new. The last section says when to pick something else.

## At a glance

| | Gojo | Rust | Go | Zig | Swift | C++ |
| --- | --- | --- | --- | --- | --- | --- |
| Memory | Ownership: memory freed after its last use, other resources when the owner goes out of scope | Ownership, with lifetime annotations | Tracing GC | Manual, with explicit allocators | Reference counting (ARC) for classes | Manual, with RAII and smart pointers |
| Use after free | Ruled out at compile time | Ruled out at compile time | Ruled out by the GC | Possible (debug allocators catch some) | Ruled out by ARC | Possible |
| Null | Only `?T` can be empty | Only `Option<T>` | Pointers, maps, interfaces, channels and functions can be `nil` | Only `?T` | Only `Optional` | Any pointer |
| Data races | Ruled out at compile time | Ruled out at compile time | Runtime detector (`-race`) | Not checked | Ruled out at compile time (Swift 6 mode) | Not checked |
| Errors | `T ! E` and `try`; an unhandled error doesn't compile | `Result` and `?` | `(T, error)` and `if err != nil` | Error unions and `try` | `throws` and `try` | Exceptions, `std::expected` |
| Concurrency | Structured tasks: `scope`, `go`, channels, `select`; no `async` | `async`/`await` or threads | Goroutines (unstructured), channels | Threads, `std.Io` | `async`/`await`, actors | Threads, coroutines |
| Integer overflow (release) | Panics | Wraps | Wraps | Undefined (ReleaseFast) or panics (ReleaseSafe) | Traps | Undefined for signed integers |
| Compile-time code | Static reflection over fields and type shapes; no macros | Macros, procedural macros | `go generate` | `comptime` | Macros | Templates, `constexpr`, macros |
| Platforms | macOS, Linux | Nearly everything | Nearly everything | Nearly everything | Apple platforms, Linux, Windows | Everything |
| Maturity | Beta (2026) | Stable since 2015 | Stable since 2012 | Before 1.0 | Stable since 2014 | Stable since 1985 |

## Compared with Rust

Gojo takes ownership from Rust: every value has one owner, assignment moves, and a value is freed when its owner goes out of scope. What it leaves out is the part that makes Rust hard to learn: **references are second-class**. A reference can be a parameter, a returned view of one, or lent for a block by a projection, and it can sit in variables (with structs and vectors of views that sit in variables too), but it can't be stored in an ordinary struct, a box or an escaping closure, except as a view of another field of the same struct. So the compiler never needs to know how long a reference lives beyond the function it is in, and there is nothing to annotate.

A function that returns one of two strings needs a lifetime in Rust:

```rust
fn longer<'a>(a: &'a str, b: &'a str) -> &'a str {
    if a.len() >= b.len() { a } else { b }
}
```

In Gojo it is just the function:

```gojo
fn longer(a: str, b: str) -> str {
    return if a.len() >= b.len() { a } else { b }
}
```

The compiler works out from a function's `return`s which arguments its result points into, so a call borrows only those: a `pick(a, b)` that returns part of `a` leaves `b` free to change. `-> str of a` says the same in the signature, where callers can rely on it.

A struct that holds borrowed data is a **view struct**: it is a view itself, so it lives only in variables, like `str`, under the same rules. What Rust writes as `struct Lexer<'a> { src: &'a str, pos: usize }`:

```gojo
view struct Lexer {
    rest: str // borrowed: the lexer is a view itself

    // the next word: a view into the input, not into the lexer
    fn next(inout self) -> ?str {
        self.rest = self.rest.trimStart()
        if self.rest.isEmpty() {
            return none
        }
        let n = self.rest.find(" ") ?? self.rest.len()
        let word = self.rest[..n]
        self.rest = self.rest[n..]
        return word
    }
}
```

The words it returns point into the input, so the lexer can move on while earlier words are in use. Often no struct is needed at all: a generator yields the words to a `for` loop, and `src.fieldViews()` gives them all at once, as views.

```gojo
gen fn words(src: str) -> str {
    var rest = src.trimStart()
    while !rest.isEmpty() {
        let n = rest.find(" ") ?? rest.len()
        yield rest[..n]
        rest = rest[n..].trimStart()
    }
}
```

Views can also be collected, as long as the collection sits in a variable too: a `Vec[str]` or `Vec[Token]` is a view itself, borrowing what its elements view, so a tokenizer can return its tokens (`fn tokenize(src: str) -> Vec[Token]`) or push them into a vector its caller passes `inout`. A view struct may hold such collections and owned data alongside its views.

To keep views longer, keep them with what they view. A struct can hold views of one of its own fields, which then can't change; the struct is an ordinary value that can be returned, stored in other structs and maps, and sent to tasks:

```gojo
struct Doc {
    text: String // owns the input
    words: Vec[str] of text // views into it, which move with it

    fn word(self, i: int) -> str {
        return self.words[i]
    }
}

fn load(text: sink String) -> Doc {
    return Doc{text, words: text.splitViews(" ")}
}
```

What a view can't do is point into data owned by some *other* long-lived value: there are no lifetime names to say which. A structure like that stores ranges (`Span`) into its input instead.

Graphs, trees and ASTs use an `Arena[T]` and `Id[T]` handles (an `Id` carries a generation, so a stale one gives `none` instead of a dangling pointer), or `Rc` and `Weak`.

Other differences:

- **Interfaces instead of traits.** A type satisfies an interface by having its methods; there are no `impl` blocks, orphan rules or coherence puzzles. Generics are monomorphized, as in Rust, with built-in capability constraints (`copy`, `clone`, `send`, `hash`, …).
- **No `async`.** Tasks are stackful, so any function can block on a channel or a socket without being marked, and there is one ecosystem instead of a sync one and an async one. A cancelled task stops at its next blocking call, not at an arbitrary `.await` with half its state dropped.
- **Fast checks.** Type and borrow checking look at one function body and the signatures it calls; the one thing carried across functions is which arguments a returned view points into, worked out once per function (or written with `of`). The front end checks a 40,000-line program in about 66 ms. There are no macros, so nothing has to expand before checking starts.
- **Release builds keep overflow checks.** `+` panics on overflow in every build; `+%` wraps when you mean it.
- **Panics abort.** There is no unwinding: a panic is a bug, and it ends the process with the source location.

**Choose Rust** when you need what second-class references rule out (long-lived structures that borrow from each other, fine control over allocation), when you need Windows, embedded targets or WebAssembly today, or when you need its ecosystem.

## Compared with Go

Gojo's syntax is not Go's, but its spirit is close: a directory is a package, interfaces are structural, `go`, channels and `select` are built in, and there is one formatter and one test runner. What changes is everything Go handles at run time:

- **No garbage collector.** Memory is freed right after its last use, and files, sockets and locks are released when their owner goes out of scope, deterministically, with no pauses. `defer f.Close()` is unnecessary; `defer` remains for restoring state.
- **No nil.** Only `?T` can be empty, and it must be unwrapped before use. An interface value is never a "typed nil", and a map is never nil.
- **Errors must be handled.** `T ! E` replaces `(T, error)`, `try` replaces `if err != nil { return err }`, and a result you forget to look at is a compile error.

```go
func wordCount(path string) (int, error) {
    data, err := os.ReadFile(path)
    if err != nil {
        return 0, err
    }
    return len(strings.Split(string(data), " ")), nil
}
```

```gojo
use std/os

fn wordCount(path: str) -> int ! os.Error {
    let text = try os.readText(path)
    return text.split(" ").len()
}
```

- **Data races don't compile.** A value crosses tasks only if its type is `send`, which the compiler derives; shared mutable state goes through `Mutex`, `RwLock` or `Atomic`. Two tasks that write the same variable are an error, not a `-race` report:

```text
error: cannot modify `hits`: a task modifies it until its scope ends
  --> main.gojo:5:12
  5 |         go () => { hits += 1 }
    |            ^^^^^^^^^^^^^^^^^^^
```

- **Goroutines are structured.** Every task belongs to a `scope`, which waits for it, so tasks can't leak, can borrow the caller's data (even disjoint halves of one slice), and give results back with `t.join()`. Cancellation flows down the task tree without a `context.Context` in every signature; Ctrl-C cancels `main`.
- **Channels can't panic.** A channel has a `Sender` and a `Receiver` end and closes when the last sender is dropped. There is no `close`, and no send on a closed channel.
- **Slices don't alias by surprise.** `Vec[T]` owns, `[]T` borrows, and a `Vec` can't grow while a view of it exists.
- **Enums are sum types** with exhaustive `match`, and methods can have type parameters.
- **C without cgo.** `extern "C"` calls C directly, with no glue code or headers; libraries can be loaded at run time, so cross-compiling needs no C toolchain for the target.

**Choose Go** when a garbage collector is the simplest answer (lots of shared, cyclic, long-lived data), when you need its huge ecosystem and its tooling, or when you need Windows. Go's runtime is also still ahead on very large numbers of tasks (see the numbers below).

## Compared with Zig

Gojo takes error unions, `try` and `errdefer` from Zig, compiles through C (and uses `zig cc` for cross-compiling), and shares Zig's dislike of hidden control flow: no exceptions, no operator overloading beyond `==`, comparison and hashing.

The difference is safety. Zig gives you control and leaves memory to you: you pass allocators, call `deinit`, and nothing stops a use after free or a data race. Gojo frees memory itself, and rules out both at compile time. Overflow and bounds checks stay on in release builds (Zig's ReleaseSafe level), and the compiler removes the bounds checks it can prove can't fail.

**Choose Zig** when you want explicit control of every allocation, `comptime` metaprogramming, freestanding or embedded targets, or a drop-in C and C++ toolchain.

## Compared with Swift

Swift and Gojo agree on a lot: value types, `?T`-style optionals, `try`, structured concurrency with task groups, and a compiler that checks what crosses tasks (`Sendable` in Swift, `send` in Gojo). The differences:

- **No reference counting by default.** Swift classes are reference-counted, and every copy of a reference updates a count. Gojo values have one owner; `Rc` and `Arc` exist, but only where you write them.
- **No function coloring.** Swift's `async` functions can only be awaited from other `async` code, and actor reentrancy is a subtle default. Gojo tasks are ordinary functions on a scheduler, and locks are scoped blocks.
- **Simpler generics and fewer features.** No protocols with associated types, no result builders, no property wrappers, no macros.

**Choose Swift** for apps on Apple platforms, and for its UI frameworks.

## Compared with C and C++

Gojo compiles to C, so it runs wherever its runtime does (macOS and Linux today) at C-like speed, and calls C with `extern "C"` and no wrappers. What it adds is the safety C and C++ can't give: no null, no dangling pointers, no data races, no undefined behavior on overflow, and `unsafe` blocks that mark the few places where those guarantees are suspended. `gojo unsafe` lists every such place in a program, and a dependency with unsafe code builds only when your `gojo.toml` allows it.

**Choose C or C++** when you need a platform Gojo doesn't support, an existing large codebase, or full control of memory layout everywhere.

## Built for coding agents

Most Gojo code is expected to be written with a coding agent, and the language is designed for that, as well as for the person reviewing the diff:

- **Syntax you can read without context.** Types follow `:`, results follow `->`, `&` at a call site marks the arguments that change, and no token's meaning depends on the spaces around it. There is one way to write each thing, and `gojo fmt` decides layout.
- **Errors you can fix locally.** Every check looks at one function body and the signatures it calls, so every error is about code in front of you. Where a call borrows only some of its arguments, the error says which and why (`` `pick` returns a view of only `a` ``). Errors say what to do next: the data race above ends with "give each task its own data, collect results with `let t = go f()` and `t.join()`, or share it through a `Mutex`".
- **No lifetimes to juggle, and no clones to hide behind.** Agents tend to `.clone()` whatever the borrow checker complains about; Gojo warns about a clone of a value nothing uses afterwards and says to move it instead.
- **Concurrency bugs you can replay.** `gojo test` runs tasks on a seeded scheduler with a virtual clock; `--explore n` tries n interleavings and prints the seed of the one that fails, so a flaky test becomes a reproducible one.
- **A short loop.** The front end takes milliseconds, and an unchanged program reruns from the cache in about 10 ms.
- **Fences around `unsafe`.** `forbid = true` in `gojo.toml` keeps a package's code safe, so an agent can't reach for `unsafe` to get past the borrow checker.
- **Docs as Markdown.** `gojo doc` prints the API of any module or package in Markdown, ready to read in a terminal or paste into a prompt.

## The numbers

The [benchmarks](../bench/README.md) implement the same 8 programs in Gojo, Go and Zig, check that the outputs match byte for byte, and time them on an Apple M5. Gojo keeps overflow and bounds checks on:

| Gojo's run time, relative to | Geometric mean | Without the binary-trees allocator win |
| --- | --- | --- |
| Go | 0.78× | 0.87× |
| Zig ReleaseSafe (checks on) | 0.80× | 0.94× |
| Zig ReleaseFast (no checks) | 0.87× | 1.03× |

Gojo is fastest on binary-trees (its small-object allocator), mandelbrot, hashmap and sort; on par with Zig ReleaseSafe on fib, where the overflow checks are the cost; and on par with Go but slower than Zig on nbody.

Building nbody takes about 50 ms with Gojo (3 ms of it in the front end), 97 ms with `go build` and 924 ms with a Zig debug build.

Concurrency is where Go is still ahead: 100,000 suspended tasks passing a value along a chain take 0.38 s and 441 MB in Gojo against 0.11 s and 284 MB in Go, and a million short tasks take 0.43 s against 0.26 s. Gojo's task stacks can't move, because other tasks may borrow data on them; parked tasks are hibernated instead (100,000 idle tasks take 0.2 GB on macOS).

## When to choose something else

- **You need Windows, WebAssembly, or a bare-metal target.** Gojo runs on macOS and Linux (x86-64 and arm64); [WebAssembly](wasm.md) is a proposal.
- **You need a mature ecosystem.** There is no package registry yet (dependencies are local paths), no language server, and the standard library is the main source of libraries.
- **Your data is a web of shared, mutable, cyclic objects.** `Rc`, `Weak` and arenas handle it, but a garbage-collected language may be simpler.
- **You need long-lived structures that borrow from each other**, as in a cache of tokens that point into documents owned elsewhere. Views, and vectors and maps of them, live in variables, and a struct can keep views of its own data (`words: Vec[str] of text`), but one that points into data another long-lived value owns keeps `Span` ranges, where Rust stores the references with lifetimes.
- **You need compile-time code generation.** Gojo has no macros and no `comptime`, on purpose. Static reflection (`static for f in fields(T)`) covers serializers and decoders, but not arbitrary generated code.
- **You need the language to be finished.** Gojo is a beta, and it still changes: see the [known limits](tour.md#known-limits-of-the-beta).

Otherwise, [take the tour](tour.md): it shows the whole language by example, and every program in it is compiled and run by the test suite.
