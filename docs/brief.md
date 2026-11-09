# Gojo on one page

A summary of the whole language for writing Gojo, sized to fit in a coding agent's context (`gojo spec` prints it). The details are in the [language reference](spec.md); every complete program here is compiled and run by the test suite.

## Shape of a program

A directory is a package; `main` is in the entry package. No semicolons; braces on the same line. Types follow `:`, results follow `->`.

```gojo
use std/os

struct Config {
    name: String // required field
    port: int = 8080 // default value
    proxy: ?String // optional: defaults to none
}

fn load(path: str) -> Config ! os.Error {
    let text = try os.readText(path)
    return Config{name: text.trim().toString()}
}

fn main() {
    let cfg = load("missing.toml") catch e {
        println("using defaults:", e.kind)
        .{name: "app"}
    }
    println("${cfg.name} on :${cfg.port}, proxy: ${cfg.proxy ?? "none"}")
}
```

```text
using defaults: NotFound
app on :8080, proxy: none
```

- `let` is immutable, `var` mutable; no shadowing in one function; unused variables and imports are errors (prefix `_` to keep one).
- `use std/os`, `use std/{fmt, os}`, `use ./geom`, `use ../util as u`, `use ./geom.{Vec2}`. `pub` exports.
- Scalars: `bool int i8..i64 uint u8..u64 f32 f64 byte rune`. `String` owns text, `str` borrows it; `Vec[T]` owns, `[]T` borrows; `[N]T` arrays; `Map[K, V]`, `Set[T]` (insertion order); tuples `(A, B)`, `t.0`; `?T`; `T ! E`.
- Literals: `[1, 2]`, `["k": v]`, `[:]`, `"x = ${x}"`, `"${f:.2}"`. `[a, if c { b }, for x in xs { f(x) }, ..rest]` builds lists.
- Functions: `fn f(a: int, b: str) -> int { a + 1 }` (the last expression is the value). Closures: `x => x * 2`, `(a, b) => { ... }`.
- Control: `if`/`match`/blocks are expressions; `for x in xs`, `for i, x in xs`, `for i in 0..n`, `while c {}`, `loop {}`, labels `outer: for ... { break outer }`, `let .Circle(r) = s else { return }`, `defer` (block end), `errdefer`.
- Integer overflow, out-of-bounds indexes and failed `unwrap` panic; `+%` wraps. A panic ends the process; there is no recovery.

## Types and methods

```gojo
struct Counter {
    n: int = 0

    fn new(start: int) -> Counter { Counter{n: start} } // Counter(5) calls it
    fn get(self) -> int { self.n } // borrow
    fn bump(inout self) { self.n += 1 } // modify
    fn into(sink self) -> int { self.n } // take ownership
}

enum Shape {
    Circle(r: f64)
    Rect(w: f64, h: f64)

    fn area(self) -> f64 {
        match self {
            Circle(r) => 3.14 * r * r
            Rect(w, h) => w * h
        }
    }
}

interface Area {
    fn area(self) -> f64
}
```

- Interfaces are structural: having the methods is enough. A borrowed interface parameter costs nothing; an interface value (`let a: Area = s`) is boxed.
- `match` is exhaustive. `.Circle(1.0)` and `.{field: v}` take their type from context. `extend T { }` adds methods.
- `copy struct P { x: f64 }` copies; other values move. `.clone()` copies explicitly (a needless clone is a warning).
- Generics: `fn max[T: ordered](xs: []T) -> ?T`, `struct Stack[T]`, `where T: clone` on methods. Built-in constraints: `copy clone default send comparable ordered hash`, and `any` (linear types too).

## Ownership: no lifetimes

- Parameters: borrow (default), `inout` (call with `&x`), `sink` (moves). Exclusive: `f(&a, a)` is an error.
- References are second-class: views (`str`, `[]T`, `view struct`) live only in local variables, never in fields, collections or escaping closures. Keep owned data and `Span` ranges instead; `-> str of a` says which parameter a returned view points into.
- `v[i]`, `m.get(k)`, `m.entry(k)`, `mu.lock()` are projections: they lend a reference for a statement or a block (`if let u = m.get(k) { }`, `with e = m.entry(k) { e += 1 }`).
- Values are dropped when their owner's scope ends (memory is freed right after its last use). `drop(x)` drops early; a `fn drop(inout self)` method runs on drop.
- Shared data: `Rc`/`Weak` (one task), `Arc`, `Cell`, `Mutex`, `RwLock`, `Atomic`, `Arena[T]` with `Id[T]`.
- `linear struct Tx { ... }` values must be consumed (moved into a `sink` parameter such as `fn commit(sink self)`), never dropped; `defer tx.rollback()` runs only if `tx` wasn't consumed.

## Errors

```gojo
struct ParseError {
    line: int
}

fn digit(c: byte, line: int) -> int ! ParseError {
    if c < 48 || c > 57 {
        return err(ParseError{line})
    }
    return int(c - 48)
}

fn total(lines: []String) -> int ! ParseError {
    var sum = 0
    for i, l in lines {
        sum += try digit(l.bytes()[0], i + 1)
    }
    return sum
}

fn main() {
    println(total(["1", "2"]) catch _e { -1 })
    match total(["1", "x"]) {
        ok(n) => println(n)
        err(e) => println("bad line", e.line)
    }
}
```

```text
3
bad line 2
```

- `T ! E` must be handled: `try` (pass up), `catch e { }`, `match`, `??` on `?T`, or `_ = f()` to discard. `try e else Wrap` converts errors; `! error` accepts any error type.
- A generic error parameter nothing decides is `never`, and `T ! never` is just `T`: `task.map(xs, 4, x => x * 2)` is a `Vec[int]`.
- Debug builds print an error return trace when an error leaves `main`.

## Concurrency

```gojo
use std/task

fn main() {
    let squares = task.map([1, 2, 3], 2, x => x * x)
    var total = 0
    let mu = Mutex(&total)
    scope {
        for x in squares {
            go () => {
                with t = mu.lock() {
                    t += x
                }
            }
        }
    }
    println(squares, total)
}
```

```text
[1, 4, 9] 14
```

- `go` starts a task in the innermost `scope`, which waits for all of them; tasks may borrow what outlives the scope. `let t = go f()` gives a handle: `t.join()`. A failing task cancels the others (`try scope { }`).
- Values crossing tasks must be `send` (derived; `Rc` and `Cell` aren't). Shared mutable state needs `Mutex`, `RwLock` or `Atomic`; data races don't compile.
- Channels: `let c = Chan[int](8)`, `c.tx <- v`, `<-c.rx` (gives `?T`), `for v in c.rx`; closes when the last sender is dropped. `select` waits on several. Cancellation flows down the task tree; `par for x in &v { }` runs iterations in parallel.

## Tools

`gojo run`, `build [--release]`, `check [--json]` (diagnostics as JSON lines with fixes), `test` (`test "name" { }` blocks, seeded scheduler, `--explore n`), `fmt`, `doc [module]` (API docs as Markdown), `unsafe` (lists unchecked code), `authority` (what each dependency does with files, environment, programs and network; `[authority] leftpad = []` in gojo.toml limits it). `unsafe { }` is needed to call `extern "C"` functions and dereference `*T`.
