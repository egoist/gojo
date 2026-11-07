# A tour of Kite

Kite is a compiled systems language with Go's simplicity, no garbage
collector and no nil. Memory is managed by ownership, like Rust, but there are
no lifetime annotations: references can't be stored, only used, so the
compiler checks them without help. Concurrency is Go's: goroutines and
channels, plus structured task groups with cancellation.

This tour shows the language by example. Every program here is compiled and
run by the test suite, and the output shown is what it prints. The complete
rules are in the [language reference](spec.md) (in Chinese) and the standard
library is documented in [docs/std](std/README.md).

## Getting started

Kite runs on macOS and Linux (x86-64 and arm64). It compiles to C, so it needs
a C compiler: `cc`, `clang`, `gcc` or `zig` on the PATH (on macOS,
`xcode-select --install`). Windows is not supported.

```sh
sh install.sh                     # from a release; or build with `cargo build --release`
kite run hello.kite               # compile and run
kite build hello.kite -o hello    # a native binary
kite build --release --target x86_64-linux-musl hello.kite   # a static Linux binary (with zig)
kite test .                       # run `test` blocks
kite fmt .                        # format every .kite file below `.`
kite doc os                       # the API of a standard module
```

A program is a `main` function:

```kite
fn main() {
    println("hello, world")
}
```

```text
hello, world
```

A directory is a package; `kite run dir` runs the package in `dir`, whose
`main` is the entry point.

## Basics

`let` binds a value, `var` a variable. Types are inferred, or written after
the name. A local variable that is never used is an error, and a block can't
declare a name that hides one from an outer block.

```kite
fn main() {
    let name = "Kite"
    var count = 0
    count += 3
    let ratio f64 = 0.5
    let big u64 = 1 << 40
    println(name, count, ratio, big)

    // conversions are explicit
    let total = f64(count) * ratio
    println(total, int(total * 10.0))

    // overflow is a panic, not a wrap: `+%`, `-%`, `*%` wrap on purpose
    let b u8 = 250
    println(b +% 10)
}
```

```text
Kite 3 0.5 1099511627776
1.5 15
4
```

Statements end at the end of a line (inside parentheses and brackets, lines
continue). `if`, `match` and blocks are expressions:

```kite
fn classify(n int) str {
    return match n {
        0 => "zero"
        1..=9 => "small"
        _ => if n < 0 { "negative" } else { "big" }
    }
}

fn main() {
    for n in [0, 7, 42, -1] {
        println(n, classify(n))
    }
    var i = 0
    for i < 3 {
        i += 1
    }
    let parity = if i % 2 == 0 { "even" } else { "odd" }
    println(i, parity)
}
```

```text
0 zero
7 small
42 big
-1 negative
3 odd
```

`for` is the only loop: `for x in xs`, `for i, x in xs`, `for i in 0..n`,
`for k, v in map`, `for cond { }` and `for { }`. Loops can have labels:
`outer: for ...` and `break outer`.

## Structs, methods and enums

Methods are written inside the type. `self` borrows the value; `self inout`
may change it.

```kite
struct Account {
    owner String
    balance int = 0
    // `?T` fields default to none
    note ?String

    fn deposit(self inout, amount int) {
        self.balance += amount
    }

    fn describe(self) String {
        if let n = self.note {
            return self.owner + ": " + n
        }
        return self.owner + ": no note"
    }
}

enum Shape {
    Circle(r f64)
    Rect(w f64, h f64)

    fn area(self) f64 {
        return match self {
            .Circle(r) => 3.14159 * r * r
            .Rect(w, h) => w * h
        }
    }
}

fn main() {
    var a = Account{owner: "ada"}
    a.deposit(50)
    println(a.balance, a.describe())
    println(a)

    let shapes = [Shape.Circle(1.0), .Rect(2.0, 3.0)]
    for s in shapes {
        println(s.area())
    }
}
```

```text
50 ada: no note
Account{owner: "ada", balance: 50, note: none}
3.14159
6
```

`match` must cover every case. Where the type is known, a variant can be
written `.Rect(...)`.

## No nil

There is no null pointer. A value that may be missing has type `?T`:

```kite
fn find(names []String, want str) ?int {
    for i, n in names {
        if n == want {
            return i
        }
    }
    return none
}

fn main() {
    let names = ["ann", "bob"]
    if let i = find(names, "bob") {
        println("found at", i)
    }
    println(find(names, "eve") ?? -1)
    let first ?int = find(names, "ann")
    println(first, first.isSome())
}
```

```text
found at 1
-1
some(0) true
```

`if let`, `??`, `match` and `try` take a `?T` apart; `unwrap()` panics on
none.

## Errors

A function that can fail returns `T ! E`. `try` passes an error up to the
caller; `catch` handles it where it happens. An error can't be ignored by
accident: discarding one takes `_ = f()`.

```kite
enum ConfigError {
    Missing(key String)
    BadNumber(key String, text String)

    fn message(self) String {
        return match self {
            .Missing(k) => "missing " + k
            .BadNumber(k, t) => k + " is not a number: " + t
        }
    }
}

fn lookup(lines []String, key str) String ! ConfigError {
    for l in lines {
        let parts = l.split("=")
        if parts.len() == 2 && parts[0].trim() == key {
            return parts[1].trim().toString()
        }
    }
    return err(.Missing(key.toString()))
}

fn port(lines []String) int ! ConfigError {
    let text = try lookup(lines, "port")
    return text.parseInt() ?? return err(.BadNumber("port".toString(), text))
}

fn main() {
    let good = ["name = web", "port = 8080"]
    let bad = ["port = eighty"]
    println(port(good).unwrap())
    let p = port(bad) catch e {
        println("error:", e.message())
        80
    }
    println(p)
    match port(["name = x"]) {
        ok(n) => println(n)
        err(.Missing(k)) => println("no", k)
        err(e) => println(e.message())
    }
}
```

```text
8080
error: port is not a number: eighty
80
no port
```

Each module has its own error type: `io.Error` (also `os.Error` and
`net.Error`) for the operating system, `json.Error` with a line and column,
and so on. They all satisfy the built-in `error` interface (a `message`
method), so a function returning `! error` can `try` any of them.

A bug, such as an index out of range, an overflow or `unwrap()` on none, is a
*panic*: the program prints the message and the location and exits with
code 2. Panics can't be caught.

## Ownership

Every value has one owner, and is freed when its owner goes out of scope.
Assigning a value, passing it to a `sink` parameter, returning it or storing
it *moves* it; using the old variable afterwards is a compile error. Numbers
and other `copy` types are copied instead; everything else is copied with
`.clone()`.

Parameters borrow by default. `inout` borrows to change (the caller writes
`&`), `sink` takes ownership:

```kite
struct Doc {
    title String
    words Vec[String] = []

    fn drop(self inout) {
        println("freeing", self.title)
    }
}

fn wordCount(d Doc) int {
    return d.words.len()
}

fn addWord(d inout Doc, w str) {
    d.words.push(w.toString())
}

fn archive(d sink Doc) {
    println("archiving", d.title)
}

fn main() {
    var d = Doc{title: "notes"}
    addWord(&d, "hello")
    addWord(&d, "world")
    println(wordCount(d))
    let dup = Doc{title: d.title + " (copy)", words: d.words.clone()}
    archive(d)
    // `d` was moved: using it here would not compile
    println(dup.words)
    defer println("leaving main")
}
```

```text
2
archiving notes
freeing notes
["hello", "world"]
leaving main
freeing notes (copy)
```

`defer` runs code when the block ends; `errdefer` only when the function
returns an error. A value can be borrowed many times for reading or once for
writing, never both at once.

## Views

`str` and `[]T` are *views*: borrowed slices of a `String` or a `Vec`.
Views can't be stored in structs or collections, or outlive what they borrow,
and while one exists the owner can't change. That is the whole lifetime
system: no annotations, and no dangling references.

```kite
fn firstWord(s str) str {
    let i = s.find(" ") ?? s.len()
    return s[0..i]
}

fn main() {
    var text = "hello brave new world".toString()
    let w = firstWord(text)
    println(w, w.len())
    // `w` is not used after this line, so `text` may change again
    text.push("!")
    println(text)

    let nums = [5, 3, 8, 1]
    let middle = nums[1..3]
    println(middle, middle.len())
}
```

```text
hello 5
hello brave new world!
[3, 8] 2
```

Containers lend their elements through *projections*: `m.get(k)` and
`xs[i]` give a reference that lives in a block, and `with` binds one to change
it in place:

```kite
fn main() {
    var counts = Map[String, int]{}
    for w in "the cat and the hat".fields() {
        // the map keeps its keys: it gets a copy of the word
        with n = counts.entry(w.clone()) {
            n += 1
        }
    }
    println(counts)
    if let n = counts.get("the") {
        println("the:", n)
    }
    var stack = [1, 2, 3]
    with top = stack[2] {
        top *= 10
    }
    println(stack)
}
```

```text
{"the": 2, "cat": 1, "and": 1, "hat": 1}
the: 2
[1, 2, 30]
```

Maps keep their insertion order. To keep something you borrowed, clone it.

## Collections and closures

`Vec[T]`, `Map[K, V]` and `Set[T]` are in the prelude, with the usual methods.
Closures are written `(x) => expr` or `(x) => { ... }`:

```kite
fn main() {
    var xs = [4, 1, 3, 9, 2]
    xs.sort()
    let evens = xs.filter((x) => x % 2 == 0)
    let squares = xs.map((x) => x * x)
    println(xs, evens, squares, xs.sum())

    var total = 0
    xs.each((x) => { total += x })
    println(total)

    var names = ["eve", "bob", "ann"]
    names.sortBy((a, b) => a.len() < b.len() || (a.len() == b.len() && a < b))
    println(names.join(", "))

    let seen = setOf(["a", "b", "a"])
    println(seen.len(), seen.contains("a"))
}
```

```text
[1, 2, 3, 4, 9] [2, 4] [1, 4, 9, 16, 81] 19
19
ann, bob, eve
2 true
```

A closure that is only called during a call (like `each`'s) borrows the
variables it uses and may change them. One that is kept (stored, or given to
`go`) takes them over instead.

## Generics and interfaces

Type parameters go in brackets, with constraints:

```kite
fn largest[T copy + ordered](xs []T) ?T {
    if xs.len() == 0 {
        return none
    }
    var m = xs[0]
    for x in xs {
        if x > m {
            m = x
        }
    }
    return m
}

struct Stack[T any] {
    items Vec[T] = []

    fn push(self inout, x sink T) {
        self.items.push(x)
    }

    fn pop(self inout) ?T {
        return self.items.pop()
    }
}

fn main() {
    println(largest([3, 9, 2]), largest([2.5, 1.5]), largest(Vec[f64]{}))
    var s = Stack[String]{}
    s.push("x".toString())
    s.push("y".toString())
    println(s.pop(), s.pop(), s.pop())
}
```

```text
some(9) some(2.5) none
some("y") some("x") none
```

Interfaces are satisfied implicitly: a type with the methods satisfies the
interface. An interface parameter borrows; an interface value (in a variable
or a collection) owns what it holds.

```kite
interface Shape {
    fn area(self) f64
    fn name(self) String
}

struct Square {
    side f64

    fn area(self) f64 { return self.side * self.side }

    fn name(self) String { return "square".toString() }
}

struct Circle {
    r f64

    fn area(self) f64 { return 3.0 * self.r * self.r }

    fn name(self) String { return "circle".toString() }
}

fn report(s Shape) {
    println(s.name(), s.area())
}

fn main() {
    report(Square{side: 2.0})
    var all = Vec[Shape]{}
    all.push(Square{side: 1.0})
    all.push(Circle{r: 1.0})
    var total = 0.0
    for s in all {
        total += s.area()
    }
    println(total)
}
```

```text
square 4
4
```

## Concurrency

`go` starts a goroutine; channels carry values between them (and move them:
the sender can't touch what it sent).

```kite
fn worker(jobs chan int, results chan int) {
    for j in jobs {
        results <- j * j
    }
}

fn main() {
    let jobs = chan[int](10)
    let results = chan[int](10)
    for _ in 0..3 {
        go worker(jobs.clone(), results.clone())
    }
    for i in 1..=5 {
        jobs <- i
    }
    jobs.close()
    var sum = 0
    for _ in 0..5 {
        sum += <-results ?? 0
    }
    println(sum)
}
```

```text
55
```

A *task group* runs tasks that may borrow the caller's data, and waits for all
of them; the first error cancels the rest and becomes the group's result.
Shared mutable data goes in a `Mutex`, whose `lock` is a projection: the data
can't escape the `with` block, so it can't be used without the lock.

```kite
use std/task

fn main() {
    let words = ["alpha", "beta", "gamma", "delta"]
    let total = Mutex(0)
    let r = task.group((g) => {
        for w in words {
            g.go(() => {
                with t = total.lock() {
                    t += w.len()
                }
            })
        }
    })
    println(r.isOk())
    with t = total.lock() {
        println(t)
    }
}
```

```text
true
19
```

Cancellation flows down the tree of tasks. In a cancelled task, blocking
operations return early: a receive gives none, `time.sleep` wakes up, and
network calls fail with an error whose `isCancelled()` is true. `task.timeout`
runs code under a deadline:

```kite
use std/{task, time}

fn main() {
    let quick = task.timeout(time.second, () => 42)
    let slow = task.timeout(10 * time.millisecond, () => {
        time.sleep(time.second)
        7
    })
    println(quick, slow)
}
```

```text
some(42) none
```

Goroutines are scheduled M:N on a thread per CPU, and preempted, so a long
computation doesn't starve the others. A data race is a compile error: a
task can't change what it captured, and values with `rc` or raw pointers
can't cross to another thread.

## Packages and the standard library

A directory is a package. `use ./geom` imports the package in the directory
`geom` next to the current one. The standard library lives under `std`:
`use std/os` imports one module and `use std/{os, json}` several, and each is
then used by its own name (`os.readText`). Only
`pub` declarations are visible outside their package, and an unused import is
an error.

```kite
use std/{fmt, json, os, time}

fn main() {
    var doc = json.object()
    doc.set("tool", json.string("kite"))
    doc.set("args", json.integer(os.args().len()))
    println(doc.encode())

    let d = time.date(2026, 11, 6, 9, 30, 0)
    println(d.iso(), time.dayName(d.weekday()))
    println(fmt.format("{:>6}|{:<6}|{:06.2}", "right", "left", 3.14159))
}
```

```text
{"tool":"kite","args":1}
2026-11-06T09:30:00Z Friday
 right|left  |003.14
```

A directory with a `kite.toml` is a package, and it can depend on other
packages. `kite init` writes one; dependencies are named in it and imported by
that name:

```toml
[package]
name = "notes"

[dependencies]
yaml = { path = "../yaml" }
```

`use yaml` then imports the package in `../yaml`, and `kite doc yaml` shows
its API. For now dependencies are local directories; versions from git and a
registry are coming (see [packages.md](packages.md)).

The standard library has files and processes (`os`), streams (`io`, `bufio`),
TCP (`net`), an HTTP/1.1 server and client (`http`), `json`, `time` and dates,
and formatting (`fmt`). See [docs/std](std/README.md), or `kite doc <module>`.

## Tests

`test` blocks live next to the code; `kite test` runs each one in a process of
its own, so a panicking test fails alone:

```kite
fn double(x int) int {
    return x * 2
}

test "doubling" {
    assertEq(double(21), 42)
}

fn main() {
    println(double(4))
}
```

```text
8
```

## Talking to C

`extern "C"` declares C functions; calling them, and dereferencing raw
pointers (`*T`), takes an `unsafe` block. No header files are needed: the
compiler generates the calls from the Kite declarations.

```kite
extern "C" {
    fn abs(x i32) i32
}

fn main() {
    let v = unsafe { abs(-5) }
    println(v)
}
```

```text
5
```

## Where next

- [Examples](../examples): a JSON API server (`todo`), `grep`, an HTTP server
  with cancellation (`http`).
- [The standard library](std/README.md).
- [The language reference](spec.md), and the [design notes](design.md), in
  Chinese.

### Known limits of the beta

- macOS and Linux only; no Windows.
- No package manager yet: a program is the directories it uses.
- No language server. Debuggers (lldb, gdb) know Kite source lines, so
  breakpoints and backtraces work, but they show variables by their names in
  the generated C (`p_x` for a parameter `x`).
- HTTP is HTTP/1.1 without TLS, one request per connection; no UDP.
- JSON doesn't convert to and from structs automatically.
- Kite functions can't yet be passed to C as callbacks.
- Interface values and function values aren't `send` (closures given to `go`
  are checked by what they capture).
