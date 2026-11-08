# A tour of Gojo

Gojo is a compiled systems language with Go's simplicity, no garbage
collector and no nil. Memory is managed by ownership, like Rust, but there are
no lifetime annotations: references can't be stored, only used, so the
compiler checks them without help. Concurrency is Go's `go`, channels and
`select`, made structured: every task belongs to a `scope` that waits for it.

This tour shows the language by example. Every program here is compiled and
run by the test suite, and the output shown is what it prints. The complete
rules are in the [language reference](spec.md) and the standard
library is documented in [docs/std](std/README.md).

## Getting started

Gojo runs on macOS and Linux (x86-64 and arm64). It compiles to C, so it needs
a C compiler: `cc`, `clang`, `gcc` or `zig` on the PATH (on macOS,
`xcode-select --install`). Windows is not supported.

```sh
sh install.sh                     # from a release; or build with `cargo build --release`
gojo run hello.gojo               # compile and run
gojo build hello.gojo -o hello    # a native binary
gojo build --release --target x86_64-linux-musl hello.gojo   # a static Linux binary (with zig)
gojo test .                       # run `test` blocks
gojo fmt .                        # format every .gojo file below `.`
gojo doc os                       # the API of a standard module
```

A program is a `main` function:

```zig
fn main() {
    println("hello, world")
}
```

```text
hello, world
```

A directory is a package; `gojo run dir` runs the package in `dir`, whose
`main` is the entry point.

## Basics

`let` binds a value, `var` a variable. Types are inferred, or written after
the name. A local variable that is never used is an error, and a block can't
declare a name that hides one from an outer block.

```zig
fn main() {
    let name = "Gojo"
    var count = 0
    count += 3
    let ratio: f64 = 0.5
    let big: u64 = 1 << 40
    println(name, count, ratio, big)

    // conversions are explicit
    let total = f64(count) * ratio
    println(total, int(total * 10.0))

    // overflow is a panic, not a wrap: `+%`, `-%`, `*%` wrap on purpose
    let b: u8 = 250
    println(b +% 10)
}
```

```text
Gojo 3 0.5 1099511627776
1.5 15
4
```

Statements end at the end of a line (inside parentheses and brackets, lines
continue). `if`, `match` and blocks are expressions, and a function body's
last expression is its result, so `return` is only for leaving early:

```zig
fn classify(n: int) -> str {
    match n {
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
    while i < 3 {
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

`for` iterates: `for x in xs`, `for i, x in xs`, `for i in 0..n`,
`for k, v in map`. `while cond { }` repeats while a condition holds and
`loop { }` until a `break`. Loops can have labels: `outer: for ...` and
`break outer`.

Strings interpolate with `${...}`; a format after a colon pads, aligns or
picks a base, checked at compile time:

```zig
fn main() {
    let name = "Gojo"
    let items = 3
    let price = 9.5
    println("${name}: ${items} items, total ${f64(items) * price:.2}")
    println("[${name:>8}] [${items:03}] [${255:x}]")
}
```

```text
Gojo: 3 items, total 28.50
[    Gojo] [003] [ff]
```

## Structs, methods and enums

Methods are written inside the type. `self` borrows the value; `inout self`
may change it.

```zig
struct Account {
    owner: String
    balance: int = 0
    // `?T` fields default to none
    note: ?String

    fn deposit(inout self, amount: int) {
        self.balance += amount
    }

    fn describe(self) -> String {
        if let n = self.note {
            return "${self.owner}: ${n}"
        }
        "${self.owner}: no note"
    }
}

enum Shape {
    Circle(r: f64)
    Rect(w: f64, h: f64)

    fn area(self) -> f64 {
        match self {
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
written `.Rect(...)`, and a struct literal `.{...}`. `Point{x, y}` fills
fields from variables of the same names, and a type with a `new` function can
be called directly: `Stack[int]()` is `Stack[int].new()`.

```zig
struct Options {
    port: int = 8080
    verbose: bool = false
}

struct Point {
    x: int
    y: int
}

fn start(o: Options) {
    println("port", o.port, "verbose", o.verbose)
}

fn main() {
    start(.{})
    start(.{port: 9000, verbose: true})
    let x = 1
    let y = 2
    println(Point{x, y})
}
```

```text
port 8080 verbose false
port 9000 verbose true
Point{x: 1, y: 2}
```

Tuples group a few values without declaring a struct, and `let` takes them
apart:

```zig
fn divmod(a: int, b: int) -> (int, int) {
    (a / b, a % b)
}

fn main() {
    let (q, r) = divmod(17, 5)
    println(q, r)
    let pair = (3, "three")
    println(pair, pair.0, pair.1)
    match divmod(9, 3) {
        (n, 0) => println("divides", n, "times")
        (_, rest) => println("remainder", rest)
    }
}
```

```text
3 2
(3, "three") 3 three
divides 3 times
```

## No nil

There is no null pointer. A value that may be missing has type `?T`:

```zig
fn find(names: []String, want: str) -> ?int {
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
    let first: ?int = find(names, "ann")
    println(first, first.isSome())
}
```

```text
found at 1
-1
some(0) true
```

`if let`, `??`, `match` and `try` take a `?T` apart; `unwrap()` panics on
none. `if let x { }` unwraps the variable `x` in place, `let v = x else {
return }` unwraps it or leaves (any pattern works: `let .Circle(r) = shape
else { ... }`), and `?.` reaches into an optional value, giving none when there
is nothing there:

```zig
fn main() {
    let nick: ?String = "ada"
    if let nick {
        println("hi,", nick)
    }
    let missing: ?String = none
    println(nick?.len(), missing?.len(), missing?.len() ?? 0)
    let n = nick else {
        return
    }
    println(n.toUpper())
}
```

```text
hi, ada
some(3) none 0
ADA
```

## Errors

A function that can fail returns `T ! E`. `try` passes an error up to the
caller; `catch` handles it where it happens. An error can't be ignored by
accident: discarding one takes `_ = f()`.

```zig
enum ConfigError {
    Missing(key: String)
    BadNumber(key: String, text: String)

    fn message(self) -> String {
        match self {
            .Missing(k) => "missing ${k}"
            .BadNumber(k, t) => "${k} is not a number: ${t}"
        }
    }
}

fn lookup(lines: []String, key: str) -> String ! ConfigError {
    for l in lines {
        let parts = l.split("=")
        if parts.len() == 2 && parts[0].trim() == key {
            return parts[1].trim().toString()
        }
    }
    return err(.Missing(key.toString()))
}

fn port(lines: []String) -> int ! ConfigError {
    let text = try lookup(lines, "port")
    return text.parseInt() ?? return err(.BadNumber("port", text))
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

```zig
struct Doc {
    title: String
    words: Vec[String] = []

    fn drop(inout self) {
        println("freeing", self.title)
    }
}

fn wordCount(d: Doc) -> int {
    return d.words.len()
}

fn addWord(d: inout Doc, w: str) {
    d.words.push(w.toString())
}

fn archive(d: sink Doc) {
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

```zig
fn firstWord(s: str) -> str {
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

```zig
fn main() {
    var counts: Map[String, int] = [:]
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

A slice is read-only, even in a `var`: other views of the same elements may
exist. A function changes elements through an `inout []T` parameter, called
with `&v` or with a range of it, `&v[a..b]`:

```zig
fn bump(v: inout []int) {
    for x in &v {
        x += 100
    }
    v.reverse()
}

fn main() {
    var nums = [5, 3, 8, 1, 9]
    bump(&nums[1..4])
    nums[..2].sort()
    println(nums)
}
```

```text
[5, 101, 108, 103, 9]
```

## Collections and closures

`Vec[T]`, `Map[K, V]` and `Set[T]` are in the prelude, with the usual methods.
A list literal builds a `Set` where one is expected, and `["a": 1]` (or `[:]`)
is a map.
Closures are written `x => expr`, `(a, b) => expr` or `x => { ... }`:

```zig
fn main() {
    var xs = [4, 1, 3, 9, 2]
    xs.sort()
    let evens = xs.filter(x => x % 2 == 0)
    let squares = xs.map(x => x * x)
    println(xs, evens, squares, xs.sum())

    var total = 0
    xs.each(x => { total += x })
    println(total)

    // a line starting with `.` continues the chain above it
    let big = xs
        .filter(x => x > 2)
        .map(x => x * 10)
    println(big)

    var names = ["eve", "bob", "ann"]
    names.sortBy((a, b) => a.len() < b.len() || (a.len() == b.len() && a < b))
    println(names.join(", "))

    let seen: Set[String] = ["a", "b", "a"]
    println(seen.len(), seen.contains("a"))
}
```

```text
[1, 2, 3, 4, 9] [2, 4] [1, 4, 9, 16, 81] 19
19
[30, 40, 90]
ann, bob, eve
2 true
```

A closure that is only called during a call (like `each`'s) borrows the
variables it uses and may change them. One that is kept (stored, or given to
`go`) takes them over instead.

## Generics and interfaces

Type parameters go in brackets, with constraints:

```zig
fn largest[T: copy + ordered](xs: []T) -> ?T {
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

struct Stack[T] {
    items: Vec[T] = []

    fn push(inout self, x: sink T) {
        self.items.push(x)
    }

    fn pop(inout self) -> ?T {
        return self.items.pop()
    }
}

fn main() {
    println(largest([3, 9, 2]), largest([2.5, 1.5]), largest(Vec[f64]{}))
    var s = Stack[String]{}
    s.push("x")
    s.push("y")
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

```zig
interface Shape {
    fn area(self) -> f64
    fn name(self) -> String
}

struct Square {
    side: f64

    fn area(self) -> f64 { self.side * self.side }

    fn name(self) -> String { "square" }
}

struct Circle {
    r: f64

    fn area(self) -> f64 { 3.0 * self.r * self.r }

    fn name(self) -> String { "circle" }
}

fn report(s: Shape) {
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

`go` starts a task. `Chan[T](n)` makes a channel with room for `n` values,
and two ends: `tx` sends and `rx` receives (each can be cloned). Sending moves
the value: the sender can't touch it afterwards. When the last sender is
dropped the channel closes, and a `for` over the receiver ends.

```zig
fn worker(jobs: Receiver[int], results: Sender[int]) {
    for j in jobs {
        results <- j * j
    }
}

fn main() {
    let jobs = Chan[int](10)
    let results = Chan[int](10)
    for _ in 0..3 {
        go worker(jobs.rx.clone(), results.tx.clone())
    }
    for i in 1..=5 {
        jobs.tx <- i
    }
    drop(jobs.tx)
    var sum = 0
    for _ in 0..5 {
        sum += <-results.rx ?? 0
    }
    println(sum)
}
```

```text
55
```

A `scope` block runs tasks and waits for all of them before it ends, so the
tasks may borrow anything that outlives it. `let t = go f()` gives a handle
whose `join` returns the task's result:

```zig
use std/time

fn lookup(id: int) -> int {
    time.sleep(time.millisecond)
    return id * 10
}

fn main() {
    let total = scope {
        let a = go lookup(1)
        let b = go lookup(2)
        a.join() + b.join()
    }
    println(total)
}
```

```text
30
```

A task may also write to a variable that nothing else touches until the
scope ends. Shared mutable data goes in a `Mutex`, whose `lock` is a
projection: the data can't escape the `with` block, so it can't be used
without the lock. Like `if`, a `with` block has the value of its body.

```zig
fn main() {
    let words = ["alpha", "beta", "gamma", "delta"]
    let total = Mutex(0)
    var longest = 0
    scope {
        for w in words {
            go () => {
                with t = total.lock() {
                    t += w.len()
                }
            }
        }
        go () => {
            for w in words {
                longest = max(longest, w.len())
            }
        }
    }
    let sum = with t = total.lock() { t }
    println(sum, longest)
}
```

```text
19 5
```

When a task fails (returns an error), the scope cancels the others, and
`try scope { }` passes the error on.

Cancellation flows down the tree of tasks. In a cancelled task, blocking
operations return early: a receive gives none, `time.sleep` wakes up, and
network calls fail with an error whose `isCancelled()` is true. `task.timeout`
runs code under a deadline:

```zig
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

Tasks are scheduled M:N on a thread per CPU, and preempted, so a long
computation doesn't starve the others. A data race is a compile error: two
tasks can't use a variable one of them changes, and values with `Rc` or raw
pointers can't cross to another thread. `par for x in &v { }` runs the
iterations of a loop on every CPU, each owning its element. Two tasks may
also change parts of one slice, when the borrow checker can see that the
ranges don't overlap:

```zig
fn fill(v: inout []int, x: int) {
    for e in &v {
        e = x
    }
}

fn main() {
    var cells = [0, 0, 0, 0, 0, 0]
    let mid = 3
    scope {
        go fill(&cells[..mid], 1)
        go fill(&cells[mid..], 2)
    }
    println(cells)
}
```

```text
[1, 1, 1, 2, 2, 2]
```

A package-level `let` is computed once, the first time it is used, and every
task sees it. It is read-only; what changes goes in a `Mutex`, an `RwLock`
(many readers or one writer) or an `Atomic` inside it:

```zig
let hits = Atomic(0)
let names = RwLock(Vec[String]{})

fn main() {
    scope {
        for i in 0..4 {
            go () => {
                _ = hits.add(1)
                with n = names.write() {
                    n.push("task ${i}")
                }
            }
        }
    }
    with n = names.read() {
        println(hits.load(), n.len())
    }
}
```

```text
4 4
```

Tests run on a simulated scheduler: one thread, a virtual clock, and an
interleaving chosen by a seed, so a test does the same thing every time.
`gojo test --explore 1000` tries a thousand interleavings and, if one
fails, prints the seed that replays it: `gojo test --seed 966961`.

## Packages and the standard library

A directory is a package. `use ./geom` imports the package in the directory
`geom` next to the current one. The standard library lives under `std`:
`use std/os` imports one module and `use std/{os, json}` several, and each is
then used by its own name (`os.readText`). Only
`pub` declarations are visible outside their package, and an unused import is
an error.

```zig
use std/{fmt, json, os, time}

fn main() {
    var doc = json.object()
    doc.set("tool", json.string("gojo"))
    doc.set("args", json.integer(os.args().len()))
    println(doc.encode())

    let d = time.date(2026, 11, 6, 9, 30, 0)
    println(d.iso(), time.dayName(d.weekday()))
    println(fmt.format("{:>6}|{:<6}|{:06.2}", "right", "left", 3.14159))
}
```

```text
{"tool":"gojo","args":1}
2026-11-06T09:30:00Z Friday
 right|left  |003.14
```

A directory with a `gojo.toml` is a package, and it can depend on other
packages. `gojo init` writes one; dependencies are named in it and imported by
that name:

```toml
[package]
name = "notes"

[dependencies]
yaml = { path = "../yaml" }
```

`use yaml` then imports the package in `../yaml`, and `gojo doc yaml` shows
its API. For now dependencies are local directories; versions from git and a
registry are coming (see [packages.md](packages.md)).

The standard library has files and processes (`os`), streams (`io`, `bufio`),
TCP (`net`), an HTTP/1.1 server and client (`http`), `json`, `time` and dates,
and formatting (`fmt`). See [docs/std](std/README.md), or `gojo doc <module>`.

## Tests

`test` blocks live next to the code; `gojo test` runs each one in a process of
its own, so a panicking test fails alone:

```zig
fn double(x: int) -> int {
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
compiler generates the calls from the Gojo declarations.

```zig
extern "C" {
    fn abs(x: i32) -> i32
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
- [The language reference](spec.md), and the [design notes](design.md).

### Known limits of the beta

- macOS and Linux only; no Windows.
- No package manager yet: a program is the directories it uses.
- No language server. Debuggers (lldb, gdb) know Gojo source lines, so
  breakpoints and backtraces work, but they show variables by their names in
  the generated C (`p_x` for a parameter `x`).
- HTTP is HTTP/1.1 without TLS, one request per connection; no UDP.
- JSON doesn't convert to and from structs automatically.
- Gojo functions can't yet be passed to C as callbacks.
- Only `send interface`s and `send fn`s cross tasks among interface and
  function values.
