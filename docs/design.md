# Gojo Language Design

Nov 4, 2026

## 1. Overview

Gojo (a code name; it may change) is a small, fast-compiling systems language without a garbage collector and without nil. Memory is managed through value semantics plus second-class references: the compiler rules out null pointers and data races, yet the language needs no lifetime annotations. It borrows from several languages (ownership from Rust, error unions and `errdefer` from Zig, structured concurrency from Trio and Swift, `go`/channels from Go, expression syntax from Swift and Kotlin) and follows none of them: each choice has to earn its place on its own.

**Design goals**

- **No GC**: memory and resources (files, locks, connections) are released deterministically when they go out of scope. There are no pauses, so Gojo can be embedded in other programs and used for low-level code.
- **No nil**: a value that may be empty must be written `?T` and must be unwrapped before use.
- **No data races at compile time**: mutable state shared across tasks can only go through types like `Mutex[T]` and `Atomic[T]`.
- **Fast compiles**: checks look only at function signatures and a single function body. They never infer across functions. The front end handles about 40,000 lines in under 70 ms.
- **Small and regular**: no lifetimes, no trait implementations, no macros; one way to write each thing, and syntax that a reader (or a coding agent) can parse without knowing the declarations around it.

**Non-goals**

- Gojo does not aim for all of Rust's expressive power. Code that stores references in structs (zero-copy parsers and the like) is left to `unsafe` or to dedicated view types.
- Gojo is not compatible with Go source and cannot call Go code directly: Go's GC heap and Gojo's ownership model cannot be mixed safely.
- No macros, compile-time evaluation, higher-kinded types or specialization.

**Kept because they work**: the package (a directory) as the unit of code, acyclic imports, structural implicit interfaces (no orphan rules, separate compilation stays possible), `go`, channels and `select` (inside structured scopes), composition instead of inheritance, struct embedding, one built-in format, a single static binary, built-in testing and toolchain. Several of these came from Go; none is kept *because* Go has it.

## 2. Lexical structure and basic syntax

The syntax aims to be read without context: annotations are `name: Type`, results come after `->`, and no token's meaning depends on the spaces around it (the formatter and parser decide everything from the tokens). Braces go on the same line and newlines end statements.

```gojo
use std/os

struct Config {
    name:  String            // required: no default value
    port:  int = 8080        // fields can have default values
    proxy: ?String           // optional, defaults to none
}

fn load(path: str) -> Config ! os.Error {
    let data = try os.readFile(path)   // data is a Vec[byte], freed when the function returns
    return parse(data)
}

fn main() {
    let cfg = load("app.toml") catch e {
        eprintln("using defaults:", e)
        Config{name: "app"}         // the last expression of a catch block is its value
    }
    var hits = 0                    // only var is mutable
    for i in 0..10 { hits += i }
    println(cfg.name, cfg.port, hits)
}
```

| Form | Meaning | Notes |
| --- | --- | --- |
| `let x = e`, `let n: u8 = 1` | Immutable binding; the type, when written, follows `:` | Most bindings are immutable, so the short keyword is the immutable one |
| `var x = e`, `var x: T` | Mutable binding; `var x: T` requires `T` to have a default value | Not every type has a zero value |
| No `package` declaration | A directory is a package; the package name is the directory name | Nothing to keep in sync with the directory |
| `use fmt, os`, `use ./geom` | Import a standard library module / a package of this project | Paths are not quoted; local packages start with `./` or `../`, so they are easy to tell apart from the standard library |
| `struct P { fn f(self) {} }` | Methods live inside the type; the receiver is `self`, `inout self` or `sink self` | One set of rules for receivers and parameters |
| `fn name(x: int) -> int` | Function; function types are written `fn(int) -> int` | `fn f() ! E` returns nothing but may fail |
| `(int, String)`, `let (q, r) = divmod(a, b)` | Tuples, destructuring | Several results without declaring a struct |
| `pub fn`, `pub struct` | Export | Names are free to use any case |
| `for x in xs`, `while c {}`, `loop {}` | Iterate; repeat while a condition holds; repeat until `break` | Each loop says what it is |
| `x => x * 2`, `(a, b) => a + b` | Short closure; parameter types are inferred from context | |
| `"${name}: ${n:>4}"` | String interpolation, with format specs checked at compile time | |
| `defer stmt` | Runs at the end of the **current block** | |
| `if let v = opt { }`, `if let v { }` | Unwraps a `?T`; `v` is valid only inside the block | |
| `a ?? b`, `a?.f()` | `b` if `a` is none; call through an optional | |
| `match x { ... }` | Exhaustive pattern matching | There is no `switch` |
| `a +% b`, `a *% b` | Explicit wrapping arithmetic | `+` panics on overflow |

### 2.1 Packages and imports

A directory is a package, the package name is the directory name, and files contain no package declaration. The entry package of `gojo run`/`gojo build` holds the `main` function (it does not need `pub`). Imports go at the top of the file:

```gojo
use std/{fmt, os, time}        // standard library: all under std; one line can import several
use ./geom                     // a package of this project: path relative to the current package's directory
use ../shared/util as u        // nested path; as picks a different binding name
use ./geom.{Vec2, Shape}       // brings the names directly into scope; geom is not bound in this case
```

- The default binding name is the last path segment (`use std/os` binds `os`). If the last segment is not a valid identifier, `as` is required.
- The standard library takes only the single name `std` (as in Rust, Zig and Gleam), so the start of a path tells you where it comes from: `std/` is the standard library, `./` is this project, and future dependency packages are names from `gojo.toml` (see [packages.md](packages.md)).
- Names brought in with `.{...}` must be `pub` and must not clash with declarations in the current package.
- Unused imports, including unused names inside `.{...}`, are compile errors. Imports must not form cycles.
- `print`, `println`, `eprint` and `eprintln` are in the prelude and need no import; `fmt` provides formatting functions such as `sprint`.

### 2.2 Declarations and statements

A type declaration starts with its kind: `struct Config {}`, `enum Shape {}`, `interface Reader {}`, optionally prefixed with `pub` or `copy`. `type` is used only for aliases, `type Id = u64`, and newtypes, `type Celsius f64`, a newtype is not compatible with its underlying type and needs an explicit conversion. When the underlying type is a number, the arithmetic and comparison operators are kept, and both operands must be the same newtype.

- **Shadowing** an outer variable **is forbidden within a function**: a name means one thing in a function body. `if let x { }` is the one exception, and it unwraps the same value.
- There is no `switch`: constant cases also use `match` (literals, ranges, `|` alternative patterns), and it is checked for exhaustiveness.
- Unused imports and variables are compile errors.

## 3. Type system

Every type states whether it owns its data, whether it can be empty and whether it has a default value. A type does not have a zero value by nature.

| Type | Description | Has a default value |
| --- | --- | --- |
| `bool`, `int`, `i8`…`i64`, `uint`, `u8`…`u64`, `f32`, `f64`, `byte`, `rune` | Scalars, bitwise copyable | Yes |
| `String` / `str` | Owned UTF-8 string / borrowed string view | `String` does (empty string) |
| `Vec[T]` / `[]T` | Owned dynamic array / borrowed slice view | `Vec` does |
| `[N]T` | Fixed-length array, on the stack or inline in a struct | If `T` does |
| `Map[K, V]`, `Set[T]` | Built-in hash tables; iteration follows insertion order, so it is deterministic | Yes |
| `?T` | Optional value: `none` or a `T` | Yes (`none`) |
| `T ! E` | Result: `T` on success, `E` on failure | No |
| `Box[T]` | Unique ownership on the heap | No |
| `Rc[T]`, `Arc[T]`, `Weak[T]` | Shared ownership (single-threaded / atomic / weak reference) | No |
| `Chan[T]` | A channel's two ends, `Sender[T]` and `Receiver[T]`; sending transfers ownership | No |
| `fn(A) -> R` | Functions and closures; never empty | No |
| `*T` | Raw pointer; may be null; can only be dereferenced in `unsafe` | No |

**Struct** fields can have default values. Leaving out a field without a default value in a literal is a compile error. A struct satisfies `default`, and you can write `var c: Config`, only when all its fields have default values.

Structs and enums get `==`, `hash` and `.clone()` automatically when all their fields support them. Writing an `equal`, `hash`, `compare` or `clone` method replaces the derived version. `print` shows a value by its structure (`Config{name: "app", port: 8080}`), unless the type has a `toString()` or `message()` method, which is used instead.

**Enums** are true sum types, and variants can carry data. Without data they are plain enums and replace `iota`.

```gojo
enum Shape {
    Circle(r f64)
    Rect(w f64, h f64)
}

enum Level { Debug, Info, Warn, Error }

fn area(s: Shape) -> f64 {
    return match s {
        Circle(r)  => math.Pi * r * r
        Rect(w, h) => w * h
    }                       // missing any variant is a compile error
}

log(.Warn, "disk almost full")   // the enum name can be omitted when the type is known
```

**Optional values `?T`** support these operations: `if let v = x { }` unwraps, `x ?? d` supplies a default, `x?.field` chains access (the result is still optional), and `x.unwrap()` panics on none. The memory layout of `?T` uses the niche of non-null types: `?Box[T]` is the size of one pointer.

**Integer overflow**: in debug builds, overflow in `+ - *` panics. Release builds also check (turning the checks off per package, in `gojo.toml`, is planned). Write `+%` explicitly when you want wrapping; saturating arithmetic (`a.saturatingAdd(b)`) is planned. Array and slice indexing is always bounds-checked; the compiler removes a check only where it proves the index in range (`for i in 0..v.len { v[i] }`, see spec 14), and a constant index that can only fail is a compile error.

## 4. Ownership and borrowing

Every value has exactly one owner, and the value is released when its owner goes out of scope. References are **second-class**: they can only be passed in as parameters or borrowed out of projections, and they cannot be stored in structs, collections or escaping closures. As a result, all checks happen within a single function, and no lifetime annotations are needed.

### 4.1 move and copy

- Assignment, passing to a `sink` parameter, returning and sending on a channel all **move** ownership. Using the original variable afterwards is a compile error.
- Scalars and `copy` types are copied bitwise: `copy struct Point { x, y f64 }`. A type that has fields needing release (`String`, `Vec`, `Box`, ...) or a `drop` method cannot be `copy`.
- Exported types must declare `copy` explicitly. A package-private, non-generic type becomes `copy` automatically when all its fields are copy and it has no `drop` method. This way, adding a field to a public type never silently changes the semantics of downstream code.
- To copy any other type, call `.clone()` explicitly, so the cost is visible in the code.
- `match` on a place (a variable, a field, an element) binds references by default. An arm that moves a binding out (`some(v) => v`) consumes the matched value instead, which needs an owned value. So there is no `ref` keyword, and `unwrap` can still be written with a `sink` receiver.

### 4.2 Parameter modes

| Mode | Signature | Call site | Meaning |
| --- | --- | --- | --- |
| Read-only borrow (default) | `fn show(u: User)` | `show(u)` | Cannot modify, cannot escape; `copy` types may be passed by value |
| Mutable borrow | `fn rename(u: inout User)` | `rename(&u)` | Exclusive: only one mutable borrow at a time |
| Ownership transfer | `fn store(u: sink User)` | `store(u)` | `u` cannot be used after the call |

A method's receiver is written as the first parameter `self` and has the same three modes: `fn show(self)`, `fn rename(inout self)`, `fn store(sink self)` (see 6.1). At the call site, `&` is used only for `inout` parameters, so a reader can see at a glance which arguments get modified. Receivers do not need `&`.

**Exclusivity rule**: at any moment, a value has either any number of read-only borrows or exactly one mutable borrow. `swap(&a, &a)` and `f(a, &a)` are both compile errors.

### 4.3 Projections: lending references

Containers must be able to lend out their elements; otherwise every access would need a copy. A projection function lends a reference with `yield`. A lent reference (and a view such as `str` or `[]T`) is valid within the current statement. When bound with `let`, it stops holding the borrowed value **after its last use** in the block, so the original value can be modified again once the borrow ends.

```gojo
extend Vec[T] {
    pub fn at(self, i: int) yield &T
    pub fn atMut(inout self, i: int) yield inout T
}
extend Map[K, V] {
    pub fn get(self, k: K) yield ?&V
    pub fn entry(inout self, k: sink K) yield inout V where V: default
}

let x = v[i]                 // v[i] is sugar for v.at(i); copied out when T is copy
v[i] += 1                 // mutable projection, valid only within this statement
if let u = users.get(id) {  // u is a borrow, valid only inside the block; users cannot be modified in the block
    greet(u)
}
with e = counts.entry(word) {   // with binds a projection to a block
    e += 1
}
```

**Views live only in variables.** Without lifetime annotations, the borrow checker can track only views held in local variables. So no value that would store a view elsewhere may exist: collections, `Box`, `Rc`, interface values, generic types, whether the type is written out or inferred (`[s]`, `Rc(s)`). When a view is assigned to an outer variable, the borrowed value must live at least as long. A view of data inside a lock cannot leave the `with` block, because the lock is released when the block ends. If a generic function keeps a value of `T` after the call returns (an escaping closure, a task started through a `Scope` value), `T` cannot be a view at the call. These rules were added during M2 while systematically constructing bypass cases; see `tests/errors/view_*`, `stored_views`, `generic_keeps_view`.

### 4.4 Release and defer

- A type that defines a `fn drop(inout self)` method has it called automatically when it goes out of scope. Variables in the same scope are released in **reverse** declaration order; struct fields are released in declaration order.
- A variable that has been moved out is not released. For a variable moved only on some branches, the compiler inserts a flag.
- `defer` runs at the end of the current block. Since resources are released automatically, code like `defer f.Close()` is mostly unnecessary; defer is mainly for restoring state and logging. Deferred code cannot change where the function goes: it cannot contain `return` or `try`, and it cannot jump out of an enclosing loop.
- To release early, call `drop(x)`, which is just an ordinary function that takes a `sink` parameter.

### 4.5 Shared ownership and cyclic structures

- `Rc[T]` uses a non-atomic count and cannot cross tasks (except inside an `isolate`d value); `Arc[T]` uses an atomic count. Both are read-only inside; to modify, wrap the value in `Cell[T]` (single-threaded) or `Mutex[T]`.
- The programmer breaks cycles with `Weak[T]`. The language does no cycle detection.
- For structures like graphs, ASTs and DOMs, the recommended approach is `arena[T]` plus indices: nodes refer to each other with `Id[T]`, and the whole arena is released at once. An `Id` carries a generation, so accessing through a deleted Id gives none instead of a dangling pointer.

### 4.6 Vec and slice views

`Vec[T]` owns its data; `[]T` is only a borrowed view. `v.push(x)` requires an `inout` borrow, so the Vec cannot grow while any view exists. The Go problem of "two slices unexpectedly sharing a backing array after append" therefore cannot happen. `String` and `str` relate the same way.

A list of string literals, `["a", "b"]`, is a `Vec[String]`, not a `Vec[str]`: views never end up inside collections.

### 4.7 Closure capture

- A closure passed in as a parameter that does not escape (the default mode) can borrow outer variables, for example `v.each((x) => total += x)`.
- An escaping closure (`sink fn()`, stored in a struct, handed to `go`) must **own** what it captures: `copy` types are copied automatically, and other types are moved into the closure. To keep the original variable, `.clone()` it first.

## 5. Error handling

Errors are still values, but the type system guarantees that they are handled. `T ! E` replaces `(T, error)`, and `try` replaces `if err != nil { return err }`.

```gojo
enum LoadError {
    Io(os.Error)
    Syntax(line: int, msg: String)
}

fn load(path: str) -> Config ! LoadError {
    let data = try os.readFile(path) else LoadError.Io   // wraps os.Error in a LoadError
    return try parse(data)                           // same type, propagated directly
}

fn save(c: Config) ! os.Error { ... }               // can only fail; no result value

let cfg = load(p) catch e { Config.default() }          // handle in place

match load(p) {                                      // handle case by case
    ok(c)                   => run(c)
    err(.Syntax(line, msg)) => eprintln("line", line, msg)
    err(.Io(e))             => eprintln(e)
}
```

- Calling a function that returns `T ! E` without handling the result is a compile error. To discard it on purpose, write `_ = f()` explicitly.
- `try x` propagates directly when the error types are the same or the outer error type is `error`; otherwise you must write `try x else wrapperFunction`. There is no implicit conversion.
- `error` is a built-in interface (with a single method, `fn message(self) -> String`). It is used at the top level of applications, like Go's `error`. Libraries should return concrete enum errors.
- `x.unwrap()` panics on error. Use it only in tests and in places that truly cannot fail.
- `errdefer` (from Zig) runs only when the function leaves with an error (or `none`, or through `try`), which suits rolling back partial work:

```gojo
fn createAccount(db: inout Db, name: str) -> Account ! DbError {
    let id = try db.insert(name)
    errdefer db.delete(id)            // only if a later step fails
    try db.grant(id, "default")
    return Account{id: id}
}
```

**panic** signals only program bugs, such as out-of-bounds access, overflow or a failed `unwrap`. A panic prints the message and the source location, then **terminates the whole process** (exit code 2). It does not unwind the stack.

- A panic in any task ends the process. There is no `recover` and no `task.Panic`.
- Rationale: no unwind tables are generated, binaries are smaller, and the normal path has zero overhead. After a bug, the state of memory and locks can no longer be trusted, and keeping the same process running is more dangerous than restarting it. Services that need fault isolation use process-level supervision (multiple processes or an external restart).
- Exception for tests: `gojo test` runs each test in a separate process, so one test's panic does not affect the others.

## 6. Interfaces and methods

Interfaces keep Go's structural, implicit satisfaction: a type satisfies an interface as long as it has the methods, with no declaration needed. What changes: interface values are never empty, and owned and borrowed interface values are distinguished.

```gojo
interface Reader {
    fn read(inout self, buf: inout []byte) -> int ! io.Error
}

fn copyAll(dst: inout Writer, src: inout Reader) -> int ! io.Error   // borrowed: fat pointer, no allocation

var r: Reader = try os.open("a.txt")   // owned: the File is moved into a unique box on the heap
let readers = Vec[Reader]{}            // heterogeneous collection
let shared = Arc[Logger](newLogger())  // sharing is written out explicitly

match r {                           // replaces type assertions and type switches
    f: File   => println("file", f.name)
    b: Buffer => println("buffer", b.len)
    _         => {}
}
```

| Form | Memory representation | Use |
| --- | --- | --- |
| Parameter `r: Reader` / `r: inout Reader` | Data pointer + method table, no allocation | The vast majority of function parameters |
| Value `Reader` (variable, field, collection element) | Unique box on the heap + method table; drops the inner value when released | Storing heterogeneous objects |
| `Rc[Reader]`, `Arc[Reader]` | Reference-counted box | Sharing |
| `?Reader` | Same as above; the niche represents none | Optional |

- Go's "a typed nil is not equal to nil" problem does not exist: interface values cannot be empty; write `?Reader` when a value may be empty.
- Method sets no longer distinguish value receivers from pointer receivers. Interface methods and ordinary methods are declared the same way, and the mode of `self` (borrow / `inout` / `sink`) in the implementation must match the interface.
- Methods with type parameters and conditional methods are **not part of** an interface's method set (see section 7), because no code can be generated for them under dynamic dispatch.
- Both struct embedding and interface embedding are kept: an embedded field is owned by the outer struct, and its methods are promoted to the outer struct.

### 6.1 Writing methods

Methods are written inside the braces of the type declaration, and the receiver is the explicit first parameter `self`. A function without `self` is an associated function and is called through the type.

```gojo
pub struct Stack[T] {
    items: Vec[T]

    pub fn new() -> Stack[T] {               // associated function: Stack[String].new(), or inferred from the expected type
        return Stack[T]{items: Vec[T]{}}
    }

    pub fn push(inout self, x: sink T) {    // mutable receiver
        self.items.push(x)
    }

    pub fn peek(self) yield ?&T {           // borrowed receiver; type parameter T comes from the enclosing type
        ...
    }

    pub fn into(sink self) -> Vec[T] {      // consuming receiver
        return self.items
    }
}

extend []String {                             // the standard library adds methods to built-in types, possibly for a specific instance
    pub fn join(self, sep: str) -> String { ... }
}
```

- Methods can be written inside `struct` and `enum` declarations. `extend T { ... }` is used to put methods in another file, to add methods to a specific instance (such as `[]String`), and for the standard library to add methods to built-in types.
- Methods and associated functions can only be defined in the package that declares the type. That way a type's method set depends only on its own package, and with separate compilation, downstream packages need no recheck.
- A field and a method, or a method and an associated function, cannot share a name.
- Compared with Go's `func (s *Stack) push(x int)`: methods sit with their type; the receiver is always called `self`, so you don't name it for every method; there are no longer two sets of rules for value and pointer receivers, and modes are written like ordinary parameters; constructors have a home (`Stack.new()`) instead of being package-level `newStack()`.

## 7. Generics

Generics fill the gaps Go users hit day to day: type parameters on methods, conditional methods, and the capability constraints that a language without GC requires. The type system itself is not made into a computation system. The compiler monomorphizes: each instantiation the program actually reaches is compiled on its own, so generic code runs as fast as hand-written code. Dictionary passing (a dictionary carrying the method table, `size`, `align` and a `drop` function pointer) is the plan for faster debug builds once Gojo has its own back end; see section 10.

```gojo
pub struct Vec[T] {
    ...

    // methods can have their own type parameters (not part of interface method sets)
    pub fn map[U](self, f: fn(T) -> U) -> Vec[U]
    pub fn filter(self, f: fn(T) -> bool) -> Vec[T] where T: clone

    // conditional methods: exist only when T satisfies the constraint
    pub fn sort(inout self) where T: ordered
    pub fn clone(self) -> Vec[T] where T: clone
}

// constraints go next to the type parameter or in a where clause
fn fill[T: copy](n: int, x: T) -> Vec[T]
fn max[T: ordered](xs: []T) -> ?T

let names = users
    .filter(u => u.active)
    .map(u => u.name.clone())
```

**Capability constraints** are built in; the compiler decides whether a type satisfies them:

| Constraint | Meaning | How it is satisfied |
| --- | --- | --- |
| `any` | Any type; can only be moved and borrowed | All types |
| `copy` | Copied bitwise; assignment does not move | Scalars; types declared as `copy struct` |
| `clone` | Can be explicitly `.clone()`d | A `clone()` method, or all fields clonable (derived); `copy` types satisfy it automatically |
| `default` | Has a default value; `var x: T` is allowed | All fields have default values |
| `send` | Can be transferred across tasks | Derived structurally by the compiler: contains no `Rc`, `Cell`, raw pointer, or function value that isn't a `send fn` |
| `comparable` | Supports `==` | Built-in types; an `equal(other T) bool` method; or all fields comparable (derived) |
| `ordered` | Supports `<` | Numbers and strings; has a `compare(other T) Ordering` method |
| `hash` | Can be a `Map` key | `comparable`, and a `hash(h: inout Hasher)` method or all fields hashable (derived) |

A user-defined type that implements `equal` and `hash` can be a map key, which Go cannot do. But Gojo opens this door only for `==` and comparison; there is no general operator overloading.

**User-defined constraints** are still interfaces and can contain type sets `~int | ~float64`. An interface with a type set can only be used as a constraint; for a value that is "one of several types", use an `enum`.

**Integer const generics** are for fixed-length arrays, matrices and SIMD. Only integer parameters are allowed, and no arithmetic happens at the type level. Array size determines memory layout, so this part is monomorphized per N.

```gojo
struct Mat[R: int, C: int] { data: [R][C]f32 }
extend Mat[R, K] {
    fn mul[C: int](self, b: Mat[K, C]) -> Mat[R, C]   // mismatched dimensions are a compile error
}
```

**Iterators**. An iterator object that holds a borrow breaks the "references cannot be stored" rule, so Gojo has two iteration protocols, neither of which stores references:

- **Cursor (pull)**: `fn cursor(self) -> C` returns a cursor of plain data, and `fn next(self, c: inout C) yield ?&T` lends one element at a time (`nextMut` is for `for x in &t`; `next` can also return an owned `?T` directly, acting as a generator). `for` is translated into "get a cursor + call `next` in a loop". The loop holds a borrow of `t`; `break`, `continue`, `return` and `try` all work as usual, with no overhead. `zip` and merging just advance two cursors together and need no coroutines.
- **Push**: `fn each(self, body: fn(T) -> Flow)` (`eachMut` for `&t`), suited to recursive structures (in-order tree traversal). The loop body is a non-escaping closure; the compiler rewrites `break`, `continue`, `return`, `try` and labeled jumps into control signals. The loop body may run many times, so it cannot move outer variables. After the loop exits, the body is never run again, even if `each` ignores the returned `Flow`.

When both are provided, the cursor protocol is used.

```gojo
extend Tree[T] {
    pub fn each(self, body: fn(&T) -> Flow)
}

for k, v in m { }            // Map's each yields two borrows
for x in &v { x += 1 }      // mutable iteration: calls eachMut
```

Loops can have labels: `outer: for row in grid { for x in row { if x == 0 { continue outer } } }`.

## 8. Concurrency

Tasks are Go's goroutines made structured: every task belongs to a `scope`, and a scope ends only when its tasks have. That one rule removes goroutine leaks, lets tasks borrow their caller's data (the borrow checker already reasons about blocks), gives results back as values, and makes cancellation a property of the tree. [concurrency.md](concurrency.md) explains the choices against other languages; the rules are in the reference (section 10).

```gojo
// tasks borrow `urls`; results come back with `join` or through a lock
fn fetchAll(urls: []String) -> Vec[Page] ! http.Error {
    var pages = Vec[Page]{}
    let mu = Mutex(&pages)              // a lock that borrows `pages`
    try scope {
        for u in urls {
            go () => {
                let p = try http.get(u)    // a failure cancels the other tasks
                with list = mu.lock() { list.push(p) }
            }
        }
    }                                   // waits for every task
    return pages
}

fn dashboard(id: int) -> Page ! Error {
    return scope {
        let user = go fetchUser(id)     // Task[User ! Error]
        let orders = go fetchOrders(id)
        Page{user: try user.join(), orders: try orders.join()}
    }
}

// channels: the last sender gone closes the channel
let jobs = Chan[Job](16)
go produce(jobs.tx)                     // moves the sender into the task
for j in jobs.rx { run(j) }             // ends once `produce` returns
```

- **Scopes.** `go` starts a task in the innermost `scope` (the bodies of `main` and tests are scopes). A task may borrow anything that lives until the scope ends, and one task may even modify a variable nothing else touches; values declared in blocks inside the scope move into the task. Leaving a scope early cancels its tasks; a failing task cancels its siblings. A function that starts long-lived tasks takes a `Scope` parameter, so where they live is visible in its signature.
- **Values crossing tasks satisfy `send`**: the compiler derives it from the type's structure; users don't write it. `send fn` is a function type safe to call from several tasks at once, `send interface` an interface whose values may cross tasks, and `isolate { }` builds a value (even a graph of `Rc`) that may cross as a whole because nothing outside points into it.
- **Shared mutable state** goes through `Mutex[T]` (locking is a projection, so a reference can't leave the critical section), `with a, b = lock(m1, m2)` for several locks in one global order, and `Atomic[T]`. Waiting (on a channel, a task, a timer) directly inside a lock block is an error.
- **Channels** have two ends, `Sender[T]` and `Receiver[T]`. Ownership knows when the last sender is gone, so that closes the channel: there is no `close`, and no channel operation panics. `select` waits on channels, task handles, timers (`time.after(d)`) and cancellation, with `if` guards on arms (Gojo has no nil channels to disable an arm with).
- **Cancellation propagates along the task tree**, without a `ctx` in every signature. In a cancelled task, blocking operations return immediately in the "no result" form they already have: a receive gets `none`, a send doesn't happen, `time.sleep` wakes early, network calls fail with an error for which `isCancelled()` holds. `task.reason()` says why, `task.deadline()` gives the deadline to pass on, `task.shield` protects cleanup, and `drop` methods run shielded for a moment so that cleanup which blocks still happens. Ctrl-C cancels `main`'s scope: programs shut down cleanly by default.
- **Data parallelism**: `par for x in &v { }` runs iterations on every CPU, each owning its element; `task.map`, `task.each` and `task.race` cover the rest.
- **Testing**: `gojo test` runs on a simulated scheduler with a seed and a virtual clock. Since there are no data races, tasks affect each other only at runtime operations, which the scheduler controls, so a seed reproduces an interleaving exactly; `--explore n` tries n of them and prints the seed of a failure.

Stacks differ from Go: tasks borrow data on other tasks' stacks, so stacks **cannot move**, and therefore cannot start at 2 KB and be copied wholesale when full, as Go does. Each task reserves a fixed range of virtual addresses (2 MB by default, with a guard page), and only pages actually used take memory: a suspended task takes one page (4 KB on Linux, 16 KB on Apple Silicon) while it runs, and, once it has been parked for a while, only the bytes it really uses: its stack is **hibernated**, copied out and brought back at the same address (100,000 idle tasks: 1.7 GB → 0.2 GB on macOS). Blocking C calls need no special annotation: during an `extern` call, the thread gives its CPU slot to another thread. Long computations that don't block are preempted: generated code checks a global flag in loops and at the entry of recursive functions (one load, one branch), and when tasks are waiting, a task that has run for more than 10 ms yields the CPU. Across the 8 benchmarks, the geometric mean of cycle counts did not change, and no single benchmark got more than 5% slower (`bench/README.md`).

## 9. unsafe and FFI

Safe code has no null pointers, dangling pointers or data races; every operation that cannot guarantee this must be written in an `unsafe` block. Below that line is the runtime, written in C (the scheduler, `Vec`, `Map`, reference counts), and the standard library's two `unsafe send` types (`Atomic`, `RwLock`).

```gojo
extern "C" lib "libz" {                       // dlopen at run time; no C toolchain needed at build time
    fn crc32(crc: u64, buf: *u8, len: u32) -> u64
}

pub fn checksum(data: []byte) -> u64 {
    return unsafe { crc32(0, data.ptr(), u32(data.len)) }
}

struct Node {
    next: *Node                              // raw pointers can be stored in structs and can be null
}

// `p` must point at a live `Node`.
unsafe fn nextOf(p: *Node) -> *Node {
    return unsafe { p.next }
}
```

| Allowed only in `unsafe` | Notes |
| --- | --- |
| Dereferencing `*T` | Raw pointers can be created and passed around; only dereferencing is unsafe |
| Calling `extern` functions | The compiler cannot verify that the other side respects ownership |
| Calling an `unsafe fn` | Its callers uphold conditions the compiler can't check; its body is checked like any other (Rust 2024's rule) |
| Manually declaring that a type satisfies `send` | `unsafe send struct Pool { ... }`; the author vouches for its own fields, and its type arguments must still be `send` |
| Planned: pointer arithmetic, `unsafe.cast`, `unsafe.forget(x)` | Not implemented yet; an `extern` declaration can view C memory as another type |

- An `unsafe` block that needs nothing is an error, and `unsafe fn`s and `extern` functions can't become function values, satisfy interfaces, or be called implicitly (`==`, `print`, `for`, drops): every unchecked call is under an `unsafe` the reader can find. The reference lists what unsafe code must never do (spec 11.1), as Rust's reference does, so "correct unsafe code" has a definition.
- Keep `unsafe` blocks as small as possible. `gojo unsafe` lists the unsafe code of every package of a program, with locations and line counts. A dependency with unsafe code builds only when the manifest being built allows it by name (`[unsafe] allow`), so adding a package that calls C takes a decision, the way pnpm makes install scripts opt-in. `forbid = true` keeps a package's own code safe (Rust's `forbid(unsafe_code)`), which also keeps coding agents from reaching for `unsafe` to get past the borrow checker. The registry will show the count on package pages.
- Libraries in `extern "C" lib "z"` are loaded at run time, like approaches that avoid cgo, so cross-compiling needs no C toolchain for the target platform. libc functions (no library given) and `extern "C" static "z"` are ordinary link-time calls.
- C may keep a pointer into a task's stack after the call returns; `.ptr()` pins that task, so its stack is never put away while it waits (section 8).
- External callbacks will be Gojo functions declared with `extern "C" fn`. They cannot capture variables; context is passed through a `*void` parameter. (Exporting works today; passing them to C as values doesn't yet.)
- A panic cannot cross an `extern` boundary: a panic inside a callback aborts the process immediately.
- The generated C is compiled with `-fwrapv -fno-delete-null-pointer-checks`, so the C compiler's own undefined behaviour (signed overflow, null-check deletion) can't change what the code means. Strict aliasing is left on (with `-fno-strict-aliasing`, `fannkuch` and `hashmap` executed 7.7% and 5.1% more instructions and took about 2% and 1.4% more cycles; the other benchmarks didn't change), so the generated code and the runtime are written to be clean under it: clang's TypeSanitizer had found free lists that reused blocks for other types and interface vtables read through another struct, and both now access memory only through the type it holds (`memcpy` for free-list links, `gojo_vt_base` embedded in every vtable). Raw pointers are dereferenced through `may_alias` types, since unsafe code may view one piece of memory as several types. Without that, `-O2` miscompiled a test of it (`run/ptr_alias`: a write through `*Pair` was not seen by a read through `*u64`). The `type_sanitizer` test (`cargo test -- --ignored`, needs an LLVM clang) keeps this checked.

## 10. Compilation model and performance

The goal is debug builds no slower than 1.3 times Go. The checks are built for it: type and borrow checking look only at function signatures and a single function body, never across functions, so they stay close to linear in program size.

**Today** the compiler works on the whole program at once:

- It loads the program, the packages it uses and the standard modules they import, checks everything, and writes one C file. `cc`, `clang`, `gcc` or `zig cc` compiles it. Debug builds use `-O0 -g` with `#line` directives, so debuggers show Gojo lines; release builds use `-O2`. Overflow and bounds checks stay on in both, except bounds checks the compiler proves can't fail: it removes those itself, which also speeds up `-O0` debug builds, and catches what the C compiler can't, since it knows that nothing outside a loop can change a vector the loop reads (`for i in 0..v.len { f(v[i]) }` with a call `f` that isn't inlined).
- The C runtime is compiled once per compiler and C toolchain, and binaries are cached by their C source and toolchain, so rebuilding an unchanged program costs only the front end.
- Cross-compilation comes from `zig cc -target`, which gives static Linux binaries.
- Generics are monomorphized (section 7).

| Phase | Work | Cost |
| --- | --- | --- |
| Parsing and type checking | Same as Go; only local bidirectional type inference | On par with Go |
| Borrow checking | One dataflow pass per function body | Low, close to linear |
| Drop insertion | Drops and flags at scope exits and conditional moves | Low, but the generated code grows |
| Escape analysis | **Not needed**: the type decides whether a value is on the stack or the heap | Saves this part of Go's work |
| Generics | Monomorphization of the reachable instantiations | Grows with the instantiations |
| Back end | The C compiler | Most of the build time |

Measured on an Apple M5 (see the README): building the nbody benchmark takes about 50 ms, of which about 3 ms is the front end (`go build`: 97 ms). On a 40,000-line program, the front end takes about 66 ms. The C compiler dominates.

**Next steps**, in order, as programs grow:

- **Separate compilation per package.** Downstream packages would read only upstream export data: signatures, parameter modes, type layouts and derived capabilities (`copy`, `send`, ...), so changing a function body would not recheck its dependents. The package manifest and lock file (packages.md) provide the cache keys.
- **A built-in fast back end for debug builds**, so that debug builds don't wait for the C compiler, with dictionary passing for generics there.
- Release builds keep monomorphizing, and C (or the C compiler's optimizer) stays the release back end.

Everything that would drag compile speed toward Rust stays excluded: inferring parameter modes from function bodies, procedural macros, compile-time evaluation and overlapping implementations.

## 11. Coming from Go

Gojo is not a dialect of Go, but many readers know Go, so this table maps the familiar forms.

| Topic | Go | Gojo |
| --- | --- | --- |
| Memory management | GC + escape analysis | Ownership + deterministic release; `Rc` / `Arc` for sharing |
| Empty values | Pointers, interfaces, maps, chans and functions can all be nil | Only `?T` can be empty |
| Bindings | `x := e`, `var x = e`, all mutable | `let x = e` immutable, `var x = e` mutable |
| Type declarations | `type Config struct {}` | `struct Config {}`, `enum`, `interface` |
| Zero values | Every type has one | Only types that satisfy `default` |
| Package declaration | Every file writes `package name`, which must match the directory | None; the directory is the package |
| Imports | `import "fmt"`, `import "example.com/m/geom"` | `use std/fmt`, `use ./geom`, `use yaml` (a dependency in gojo.toml), `use ./geom.{Vec2}` |
| Functions | `func f(x int) int` | `fn f(x: int) -> int` |
| Methods | `func (p *Point) scale(k int)`, value / pointer receivers | Inside the type: `fn scale(inout self, k: int)`; `extend T {}` for additions |
| Constructors | Package-level function `NewStack()` | Associated function `Stack.new()` |
| Export | Capitalized first letter | `pub` |
| Enums | `const` + `iota` | `enum` sum types + exhaustive `match` |
| Errors | `(T, error)` + `if err != nil` | `T ! E` + `try` / `catch` |
| panic | Can be `recover`ed | Cannot be recovered; terminates the process |
| Type tests | Type assertions, type switches | `match` |
| Slices | May share a backing array with other slices | `Vec[T]` owns, `[]T` borrows; no accidental sharing |
| Parameters | By value or by pointer | Read-only borrow / `inout` / `sink` |
| defer | Runs at function end | Runs at block end |
| Iteration | `for i, x := range xs`, `for cond {}`, `for {}` | `for i, x in xs`, `while cond {}`, `loop {}` |
| Several results | `func f() (int, int)` | `fn f() -> (int, int)`, `let (a, b) = f()`; errors go in `T ! E`, not in a second result |
| Closures | `func(x int) int { ... }` | Also `x => expr`, `(a, b) => expr` |
| Formatting strings | `fmt.Sprintf("%s: %d", name, n)` | `"${name}: ${n}"`, checked at compile time |
| Constructors with options | `NewServer(Options{Port: 80})` | `Server(.{port: 80})`: `T(...)` calls `T.new(...)`, `.{...}` takes its type from the parameter |
| `select` | `case v := <-ch:`, `default:` | `let v = <-ch => ...`, `_ => ...`, same form as `match` |
| Integer overflow | Wraps silently | panics; write `+%` to wrap |
| Data races | Runtime race detector | Ruled out at compile time |
| Goroutines | Detached; leaks need a profiler to find | Tasks belong to a `scope`, which waits for them; `let t = go f()` gives the result with `t.join()` |
| Channels | Closed by hand; send on a closed channel panics | `Sender` / `Receiver` ends; closes when the last sender is dropped; never panics |
| Cancellation | Pass `context.Context` by hand | Propagates automatically along the task tree; Ctrl-C cancels `main` |
| Testing concurrency | `-race`, `synctest` | A seeded, simulated scheduler with virtual time: `gojo test --explore n` |
| Generic methods | Not supported | Supported; not part of interface method sets |
| Map keys | Only built-in comparable types | Any type that satisfies `hash` |
| C interop | cgo | `extern "C"`: direct calls, no glue code or C headers; libraries linked (`static`) or loaded at run time (`lib`) |

## 12. Deliberately omitted features and settled decisions

**Deliberately omitted**

| Feature | Why it is omitted |
| --- | --- |
| Stored references and lifetime annotations | The main reason Rust is hard to learn; projections and arenas cover the vast majority of cases |
| Classes and inheritance | Composition and structural interfaces are enough |
| Trait-style explicit implementations | They bring orphan rules and overlap problems and break separate compilation |
| Higher-kinded types, specialization, variadic generics | Type checking would turn into a solving problem |
| Macros and compile-time evaluation | Slower compilation, and tools struggle to understand the code; code generation is left to external tools |
| General operator overloading | Only `==`, comparison and hashing are open |
| Exceptions and `recover` | Errors go through `T ! E`; panic signals only bugs |
| async / await | Goroutines have no "function coloring" problem |
| Global type inference | Hard-to-read error messages and slower compilation |

**Settled questions** (the open questions of the former section 12)

- [x] **Whether copy is derived automatically**: a compromise. Exported types must write `copy` explicitly; a package-private, non-generic type is derived as `copy` when all its fields are copy and it has no `drop` method.
- [x] **Pull iterators**: no coroutines. The cursor protocol (`cursor` + `next`) provides zero-overhead pull iteration, and `zip` and merging just advance several cursors together; recursive structures use the push protocol `each` (see section 7).
- [x] **Self-references and zero-copy views**: no `View[Owner, T]`. A parser stores owned `String` / `Vec` data and integer ranges `Span` in its struct, and borrows views through methods when needed (`span.of(self.src)`). Structs never store references, so the rules stay simple and no `unsafe` is needed.
- [x] **Cost of panic unwinding**: the default, and only, mode is abort; see section 5.
- [x] **Default integer overflow behavior in release**: checked by default. Measurements (`bench/`) show that once the small functions containing the check branches are inlined, most benchmarks match the unchecked versions; code dense with recursive calls is about 1.6 times slower, and hot spots use `+%` for explicit wrapping. Turning checks off per package waits until there is a package manifest.
- [x] **How cancellation shows up**: blocking operations return early in the "no result" form they already have (`none` for a receive, an error for the network, `none` for `timeout`), instead of uniformly returning `task.Cancelled`. Otherwise every receive and every `sleep` would have to become a result type, with `try` everywhere. Use `try task.check()` when an explicit checkpoint is needed.
- [x] **Task stacks**: fixed addresses, pages committed on demand, never moved, because tasks can borrow data on other stacks. A task parked for a while is hibernated: its used bytes are copied out and its pages given back, and both return at the same address before it runs.
- [x] **Interop with Go**: not part of the language. When needed, go through the C ABI and `extern`; tools that generate declarations are left to the ecosystem.
- [x] **Back end**: generate C, compiled by the system C compiler or `zig cc`. It gives portable, well-optimized code and cross-compilation for free; the cost is build time, which a built-in debug back end will address (section 10).
- [x] **Generics implementation**: monomorphization for now; dictionary passing comes with the built-in debug back end (sections 7 and 10).
- [x] **Packages**: a manifest (`gojo.toml`) and lock file, short dependency names in source, the standard library under `std/`; see [packages.md](packages.md).
- [x] **Name**: Gojo.

## 13. Syntax revision (Nov 2026)

Two rounds. The first adopted conveniences from current languages; the second dropped the forms Gojo had only because Go has them.

A review of the language against current languages (Swift, Kotlin, Rust, Zig, TypeScript) asked one question of each idea: does it make everyday code shorter or clearer *without* costing run time or compile time? Everything adopted is resolved in the parser or in local bidirectional checking, and lowers to constructs that already existed, so the generated C is unchanged.

**Adopted**

| Change | Before | After | Lowers to |
| --- | --- | --- | --- |
| String interpolation | `fmt.format("{}: {:.2}", name, x)`, `a + ": " + b` | `"${name}: ${x:.2}"` | the `fmt.format` intrinsic: one buffer, specs checked at compile time |
| One-parameter closures | `(x) => x * 2` | `x => x * 2` | the same closure |
| Method chains across lines | one long line, or temporaries | a line starting with `.name` continues the one before | parser only; `match` arms (`.A => ...`) are told apart by their `=>` |
| `if let x` | `if let v = x { }` (shadowing `x` is forbidden) | `if let x { }` | `if let x = x`, the one shadowing the rules allow |
| `.{...}` literals | `serve(ln, h, http.Options{deadline: d})` | `serve(ln, h, .{deadline: d})` | the struct literal of the expected type, like `.Variant` |
| Field shorthand | `Point{x: x, y: y}` | `Point{x, y}` | the same literal |
| `?.` calls | `if let p = x { some(p.len()) } else { none }` | `x?.len()` | a `match` on the optional, like `?.field`; `?U` results are not doubled |
| `T(args)` constructors | `RwLock.new(x)`, `Atomic.new(0)`, `Stack[int].new()` | `RwLock(x)`, `Atomic(0)`, `Stack[int]()` | a call of the associated function `new` |
| One spelling for built-in types | `rc`, `arc`, `weak`, `chan[T](n)`, `chan T` (a keyword) | `Rc`, `Arc`, `Weak`, `Chan[T](n)` | nothing: names only; `chan` is no longer a keyword |
| `with` bodies end functions | a dead `return ...` after a `with` whose body returns | none needed | the body runs exactly once, so its `return` ends the function |
| String literals as `String` | `"ok".toString()` where a `String` is expected | `"ok"` (it was already allowed; the code and docs now use it), also in `Box("x")`, `Rc("x")` | the same allocation the call made |

Old names give errors that say what to write now (`` undefined: `rc` `` with "built-in types are capitalized: write `Rc`").

**Second round: not following Go**

| Change | Before | After | Why |
| --- | --- | --- | --- |
| Annotations after `:` | `fn f(xs []Item, k int) int`, `port int = 80`, `let n u8 = 1`, `[T ordered]` | `fn f(xs: []Item, k: int) -> int`, `port: int = 80`, `let n: u8 = 1`, `[T: ordered]`, `[T]` | Without the colon, `t []int` and `xs [1]`, or `f() *u8` and `a * b`, differed only by a space: the parser and formatter had to read whitespace. Now every token means one thing; the formatter no longer looks at the source's spacing |
| Modes before the type | `u inout User`, `self inout` | `u: inout User`, `inout self` | The mode is part of how the value is passed, read left to right like the type |
| `while` / `loop` | `for cond {}`, `for {}` | `while cond {}`, `loop {}` | Each loop says what it is; `for` only iterates |
| Tuples | a named struct for every pair | `(A, B)`, `(a, b)`, `t.0`, `(x, _)` patterns, `let (q, r) = f()` | Several results without ceremony. A tuple is a prelude struct (`Tuple2[A, B]` …), so moves, borrows and code generation are the struct paths; it is copy when its elements are, `==`/hash when they are, holds no views (like any struct), and prints as `(1, "a")`. 2 to 6 elements |
| One parameter, one type | `fn f(x, y int)` | `fn f(x: int, y: int)` | One form, read the same everywhere |
| `with` has a value | `var n = 0; with v = m.lock() { n = v.len() }` | `let n = with v = m.lock() { v.len() }` | `if`, `match` and blocks were already expressions. Checked as a hidden `?T` set in the body and unwrapped after it, so the lock and borrow rules apply unchanged; the value can't be a view |
| `let ... else` | `let r = match s { .Circle(r) => r, _ => return 0 }` | `let .Circle(r) = s else { return 0 }` | Early exits without nesting; the parser folds the rest of the block into the match, and the `else` must leave |
| `if` / `for` / `..` in list literals | `var items = Vec[View]{}` then `items.push(...)` inside `if`s and loops | `[header, if let e = err { Text{...} }, for t in todos { row(t) }, ..footer()]` | Data (UI trees above all) keeps its shape in the code. Dart's collection-if/for, which Flutter relies on; lowered to the same pushes |
| Collection literals by context | `setOf(["a"])`, `Map[String, int]{"a": 1}` | `let s: Set[String] = ["a"]`, `["a": 1]`, `[:]` | One literal syntax for `Vec`, `Set` and `Map`; sets print as `{"a", "b"}` |

The whole repository was converted by a throwaway tool that used the old parser to find each annotation. Old forms now fail with an error that names the new one (`` expected `:` before the type of parameter `x` ``, `` expected `->` before the result type ``, `` the mode goes before `self`: `inout self` ``, `` a loop with a condition is `while cond { }` ``).

**Considered and not adopted** (each would still fit the syntax, if a need shows up):

- **Default parameter values** (`fn serve(ln: Listener, h: Handler, o: Options = .{})`). Options structs with `.{...}` cover most uses without adding overload-like call rules.
- **Patterns in more places**: `x is .Circle` as a boolean test, struct patterns `Point{x, y: 0}`. `if let` and `let ... else` cover most uses.
- **Trailing closures** (`task.timeout(time.second) { work() }`): conflicts with `if cond {` and struct literals without adding much over `() => { }`.
