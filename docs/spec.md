# Gojo language reference

This document describes the language that the current compiler implements. It is the precise version of [design.md](design.md): the design document explains why; this one states the rules. The "Tests" line at the end of each section lists the test files that cover its rules:

- `run/`: runnable programs; the output must match the `.out` file of the same name;
- `errors/`: must fail to compile with the message written after `// error:` in the file;
- `panic/`: must terminate at run time with exit code 2 and the message written after `// panic:`;
- `projects/`: multi-package projects; `testblocks/`: `gojo test`; `examples/` (at the repository root): example programs, whose output is compared too.

All tests live in `tests/` and run with `cargo test`. `cargo test -- --include-ignored` also reruns every `run/` program under AddressSanitizer / UBSan, and reruns the concurrent ones under ThreadSanitizer. Two other kinds of tests:

- `fuzz/`: inputs (reduced) that fuzzing found and that once crashed the compiler. Each must get a diagnostic or compile successfully;
- `unsafe/`: programs written specifically to get around the safety checks (dangling views, views escaping locks and loops, moves hidden in `defer` and closures, data races…). Each must be rejected. Many of them compiled when they were written, and really fail under AddressSanitizer;
- `nesting_limits` in `cargo test`: programs nested close to the various limits must compile and run; programs far beyond the limits must get an error, not a crash (section 15).

The fuzzing tool is `gojo-fuzz` (section 15).

## 1. Lexical structure

- Source files are UTF-8. Comments are `// line comments` and `/* block comments */`; block comments can span lines.
- **Automatic semicolons**: a statement terminator is inserted when the last token on a line is an identifier, a literal, `true`, `false`, `none`, `)`, `]`, `}`, `return`, `break` or `continue`. So a line that ends with an operator continues on the next line. Newlines inside parentheses and brackets do not end a statement. `;` puts several statements on one line.
- **Leading-dot lines**: a line that starts with `.name` continues the expression on the line before (a method chain written one step per line), unless it is a `match` arm, that is, it has a `=>` outside brackets (`.Circle(r) => ...`). Comments and blank lines may sit between the steps. A block whose last line is a bare `.Variant` after another expression statement therefore needs the variant's type: `Color.Red`.
- Keywords: `use extend pub fn struct enum interface type copy let var if else for in match return break continue defer errdefer go try catch with yield unsafe extern none true false inout sink where test const select scope isolate`. `self`, `as` and `lib` are contextual keywords, and so are `view` and `linear` before `struct` or `enum` and `of` after a function's result type.
- Integer literals: decimal, `0x`, `0b`, `0o`, optionally separated with `_`. Floats: `1.5`, `2.5e3`, `1e-3`.
- Strings `"..."` support `\n \t \\ \" \$ \xNN \u{...}`. Backquoted strings are raw and process no escapes (and no interpolation).
- **Interpolation**: `"x = ${expr}"` embeds the value of `expr`, shown as `print` shows it; `${expr:spec}` formats it with a `fmt.format` spec (`${n:>8}`, `${f:.2}`, `${b:08b}`), checked at compile time. A string with an interpolation is a new `String`; one without stays a literal. The expression stays on the string's line, and `\${` is a literal `${`. A character literal `'é'` is a `rune` (an `i32` code point); when the expected type is `u8` it is converted to a byte.
- An untyped integer literal defaults to `int` and a float literal to `f64`. When there is an expected type, the literal takes it; a value out of range is a compile error. In a binary operation, a side whose value can only be a literal (`5`, `-1.5`, `(1 + 2)`, an `if`/`match` whose branches are all literals, `{ 3 }`) takes the type of the other side: in `(if c { 5 } else { 7 }) +% v` the literals have the type of `v`.

Tests: `run/lexical`, `run/patterns`, `run/modern`, `errors/int_mismatch`, `errors/literals`, `errors/interp_empty`, `errors/interp_unclosed`, `errors/modern`, `errors/chan_renamed`.

## 2. Packages and imports

- A directory is a package. The package name is the directory name; files have no package declaration. The `main` function in the entry package is the program entry point and does not need `pub`.
- `use std/os` imports a standard library module; `use std/{fmt, os}` imports several at once. `use ./geom` and `use ../util/text` import packages of this project, with paths relative to the directory of the current package. The start of the path names the source: `std/` is the standard library; `./` and `../` are this project. The standard library takes only the name `std`, so modules added later will never clash with any package name.
- `use std/os` binds the name `os`: code still writes `os.readText(...)`. Writing just `use os` is a compile error that suggests `use std/os` instead.
- The default bound name is the last segment of the path; `use ./util/text as t` picks another name.
- `use ./geom.{Vec2, Shape}` brings the names directly into scope and does not bind `geom`. The imported names must exist and must be `pub`.
- An unused import (including an unused name inside `.{...}`) is a compile error. Imports between packages cannot form a cycle.
- Only `pub` declarations can be used by other packages. Functions, types, methods, associated functions and struct fields are all invisible outside the package by default.
- `print`, `println`, `eprint`, `eprintln`, `assert`, `assertEq`, `drop`, `ok`, `err`, `some` and `none` are in the prelude and need no import.

### 2.1 Package manifest `gojo.toml`

A directory with a `gojo.toml` is a **package**. Its subdirectories are its subpackages (except subdirectories that have their own `gojo.toml`; those are separate packages). Programs without a `gojo.toml` still run, but they cannot have dependencies. See [packages.md](packages.md) for the design and rationale.

```toml
[package]
name = "@ann/notes"        # lowercase letters, digits, `-`, `_`; may have a scope `@scope/name`
version = "0.3.0"          # MAJOR.MINOR.PATCH, default 0.1.0
edition = "2026"           # default: the latest edition
exports = [".", "schema"]  # subpackages other packages can import; default: only the root "."

[dependencies]
yaml = { path = "../yaml" }                     # the key is the name code uses in `use`
uuid = { path = "../uuid", package = "@carol/uuid" }

[unsafe]                                        # optional: see below
allow = ["@c/zlib"]                             # dependencies that may have unsafe code
forbid = true                                   # this package may not

[workspace]                                     # optional: members checked and tested together
members = ["app", "libs/*"]
```

- The form of an import names its source: `std/...` is the standard library, `./...` and `../...` are subpackages of this package, and a name without a prefix is a dependency from `[dependencies]` (`use yaml`, `use yaml/schema`). A dependency can have the same name as a standard library module; only `std` is reserved.
- A relative path cannot leave this package (the directory that holds `gojo.toml`), and cannot point into another package that has a `gojo.toml`: such a package must go in `[dependencies]`. A subpackage of a dependency must be listed in that dependency's `exports`.
- Only `path` dependencies are supported for now. `version` (registry) and `git` dependencies are recognized, but report an error saying they are not available yet.
- A directory that several packages depend on is compiled once (diamond dependencies).
- `gojo.toml` is a strict subset of TOML (tables, strings, integers, booleans, arrays, inline tables). Unknown keys, misspelled names and paths that don't exist are errors with a location; they are never ignored.
- **`[unsafe]`.** A dependency, direct or not, that has `unsafe` blocks, `unsafe fn`s, `unsafe send` types or `extern` blocks only builds when the manifest of the package being built lists it in `allow`, by the name in its own `gojo.toml`; the error says where its unsafe code is, and `gojo unsafe` lists all of it. Only the manifest being built decides, so a program sees and approves every package in its tree that has unsafe code (the dependencies' own `allow` lists don't count). A name in `allow` that isn't in the dependency tree is an error. `forbid = true` makes any unsafe code in the package's own files an error, which keeps a coding agent from reaching for `unsafe`. The standard library is not subject to it. A workspace's members each have their own `[unsafe]`.
- `gojo init [name]` writes a `gojo.toml` in the current directory. In a workspace root without `[package]`, `gojo check` and `gojo test` process each member in turn, and `gojo run`/`build` require a member to be named. `gojo doc yaml/schema` shows the API of a dependency.

Tests: `projects/app`, `projects/cycle`, `projects/private`, `projects/groups`, `packages/*` (`packages/unsafe_*` for `[unsafe]`), `errors/bad_use`, `errors/bad_use_names`, `errors/unused`, `errors/use_groups`.

## 3. Declarations

### 3.1 Variables and constants

- `let x = e` is immutable; `var x = e` is mutable. A type annotation follows a colon: `let n: u8 = 1`. A string literal is a `str` view unless a `String` is expected, so a string to build on is declared `var s: String = ""`. `var x: T` requires `T` to satisfy `default` (section 4.6); its value is the default value.
- Assigning to a `let` binding is a compile error.
- **No shadowing within a function**: an inner block cannot declare a variable with the same name as a local variable of an outer block.
- A local variable that is declared but never used is a compile error. Names that start with `_`, and `_` itself, are exempt.
- `const Name = constant expression`. It can refer to other constants. Integer constants can be used as array lengths.
- The order of package-level declarations (functions, types, aliases, constants, values) does not matter; a declaration can use declarations written after it. An alias or constant that refers to itself, directly or indirectly, is a compile error (recursive types need a struct or enum).
- **Package-level values**: `let name = e` (or `let name: T = e`) at the top level of a file, `pub let` to export it. `e` is any expression; it is computed once, on first use, by whichever task gets there first (the others wait), so a program pays only for the values it uses. The type must be `send` and can't be or contain a view: every task can see the value. It is read-only: no assignment, no `inout` borrow, no move out of it; state that changes goes inside, in a `Mutex`, `RwLock` or `Atomic` (`let hits = Atomic(0)`). A value whose computation needs itself panics: `the value of x depends on itself`. There is no top-level `var`.

Tests: `run/basics`, `run/decls`, `run/forward_decls`, `run/globals`, `errors/let_immutable`, `errors/shadowing`, `errors/unused`, `errors/constraints`, `errors/decl_cycles`, `errors/const_cycle`, `errors/globals`, `errors/global_move`, `panic/global_cycle`.

### 3.2 Type declarations

- `struct`, `enum` and `interface` declarations start with the kind, optionally preceded by `pub` and `copy`.
- `type A = B` is an alias, identical to `B`. `type Celsius f64` is a newtype: it is incompatible with its underlying type; convert explicitly with `Celsius(x)` and `f64(c)`.
- A newtype whose underlying type is a number keeps the number's operators (`+ - * / %`, `+% -% *%`, bitwise operators for integers, unary `-`, comparisons). Both sides must be the same newtype, and the result is that newtype. A newtype satisfies `ordered`, `default` and `hash` when its underlying type does. Other newtypes (such as `type Name String`) have only comparisons, no operators.
- Struct embedding: a field written as a bare type name (`struct Admin { Named }`) promotes the fields and methods of the embedded type to the outer type.
- A recursive type must go through an indirection (`Box`, `Vec`, `Rc`, etc.); otherwise its size is infinite, which is a compile error.

Tests: `run/decls`, `run/constraints`, `errors/infinite_type`, `errors/constraints`.

### 3.3 Functions and methods

- `fn name(a: A, b: B) -> R`. Every parameter has its own `name: Type`; a mode goes before the type (`u: inout User`, `x: sink T`, section 5.2). Without `->` the function returns unit; `fn save(c: Config) ! E` returns nothing but may fail. Function types are written the same way: `fn(int, inout Buf) -> bool`.
- Methods are written inside the braces of a `struct`/`enum`. Their first parameter is `self`, which can have a mode: `self`, `inout self`, `sink self` (section 5.2). A function without `self` is an associated function, called through the type: `Point.new()`, `Stack[String].new()`; type parameters can be inferred from the expected type.
- `extend T { ... }` adds methods to a type, and can target a specific instance (`extend []String`). Methods can only be defined in the package that declares the type.
- A field and a method, or a method and an associated function, cannot have the same name. `self` can only appear in methods.
- Receivers take no `&`: `v.push(x)` takes a mutable borrow automatically for a `inout self` method.
- **Constructors**: when a struct or enum has an associated function `new`, the type can be called like a function: `T(args)` is `T.new(args)` (`RwLock(m)`, `Atomic(0)`, `Stack[int]()`, or `Stack()` when the type is expected). For a newtype, `T(x)` is the conversion (3.2).
- `unsafe fn` declares a function every call of which needs an `unsafe` block (11).
- A function body, like any block, has the value of its last expression: `fn area(self) -> f64 { self.w * self.h }`. `return` is for leaving early.

Tests: `run/basics`, `run/generics`, `run/interfaces`, `errors/methods`, `errors/assoc_calls`, `errors/self_outside_method`, `errors/dup_members`.

## 4. Types

### 4.1 Built-in types

| Type | Description |
| --- | --- |
| `bool`, `int`/`i8`…`i64`, `uint`/`u8`…`u64`, `f32`, `f64`, `byte` (= `u8`), `rune` (= `i32`) | scalars, copy |
| `String` / `str` | owned UTF-8 string / borrowed view (`String(bytes)` and file reads don't check the encoding; invalid bytes decode as U+FFFD) |
| `Vec[T]` / `[]T` | owned dynamic array / borrowed slice view (read-only unless `inout`, 5.2) |
| `[N]T` | fixed-length array; `N` is an integer constant or an integer type parameter |
| `Map[K, V]` | hash table; iteration order is insertion order |
| `(A, B)` … | a tuple of 2 to 6 values (section 4.7) |
| `?T` | `none` or a `T` |
| `T ! E`, `! E` | a success value `T` or an error `E`; `! E` has no success value |
| `Box[T]` | exclusive ownership on the heap |
| `Rc[T]`, `Arc[T]`, `Weak[T]` | non-atomic / atomic reference counting, weak reference |
| `Cell[T]`, `Mutex[T]`, `MutexRef[T]` | single-threaded interior mutability; lock; lock that borrows outside data (section 10.1) |
| `Chan[T]`, `Sender[T]`, `Receiver[T]` | a channel and its two ends (section 10.3) |
| `Task[T]`, `Scope` | a task handle and a scope handle; views (section 10.1) |
| `Atomic[T]`, `Iso[T]` | an atomic integer; an isolated value (section 10.5) |
| `fn(A) -> R` | functions and closures, never null |
| `*T` | raw pointer, can be null |
| `Span` | `copy struct { lo int, hi int }`, a range within a string |

- There is no `nil`. Only `?T` can be empty.
- There are no implicit conversions between integer types, or between integers and floats; convert explicitly with `i64(x)`, `f64(n)`. A narrowing conversion panics when the value is out of range.
- `?Box[T]`, `?Rc[T]`, `?Arc[T]` and `?Weak[T]` are the size of one pointer; `none` is the null pointer.

Tests: `run/basics`, `run/niche`, `run/spans`, `errors/no_nil`, `errors/int_mismatch`, `panic/conversion`.

### 4.2 Structs

- Fields can have default values: `port int = 8080`. A `?T` field defaults to `none`. Leaving out a field without a default in a literal is a compile error.
- `Point{x, y}` sets each field from the variable of the same name (`Point{x: x, y: y}`); it can be mixed with `name: value`.
- `.{x: 1, y: 2}` is a literal of the struct the context expects (a parameter, a field, a `return`, a typed `let`, an element pushed into a `Vec[T]`, `?T` of a struct), as `.Variant` is for enums: `serve(ln, h, .{deadline: 5 * time.second})`. Without an expected struct type it is an error.
- `copy struct` declares a bitwise-copied type. All its fields must be copy, and the type cannot have a `drop` method.
- **copy inference**: structs and enums that are private to the package, not generic, have only copy fields and have no `drop` method are copy automatically. A `pub` type must write `copy` explicitly.
- A type gets `==` automatically when all its fields are comparable; it can be a `Map` key when all its fields are hashable; it gets `.clone()` automatically when all its fields can be cloned. Defining `equal`, `hash`, `compare` or `clone` methods replaces the generated implementations. A type that defines `equal` but not `hash` cannot be a key. These methods have fixed signatures, and a wrong signature is a compile error: `fn equal(self, other: T) -> bool`, `fn compare(self, other: T) -> Ordering`, `fn hash(self, h: inout Hasher)`, `fn clone(self) -> T`, `fn drop(inout self)`. `fn toString(self) -> String` (or `fn message(self) -> String`) decides how `print` shows the type; a method of the same name with a different signature is just an ordinary method.
- Fields of structs and payloads of enums cannot have view types (`str`, `[]T`, `MutexRef`), except in a `view struct` or `view enum` (5.4).

Tests: `run/basics`, `run/copyinfer`, `run/interfaces`, `errors/bad_copy`, `errors/copy_exported`, `errors/missing_field`, `errors/view_field`, `errors/protocol_methods`.

### 4.3 Enums

- Variants can have named fields, `Circle(r f64)`, or none, `enum Level { Debug, Info }`.
- When the expected type is known, you can write `.Warn`, `.Circle(1.0)`.
- Enums without data satisfy `ordered` (in declaration order) and can be converted to integers.

Tests: `run/basics`, `run/generics`, `run/errors`, `run/patterns`, `errors/non_exhaustive`.

### 4.4 Function types and closures

- `fn(int) -> int`. Closure literals are `x => x * 2`, `(x, y) => { ... }` and `(x int) => ...`; parameter and return types are inferred from the expected type. You can also write `fn(x: int) -> int { ... }`. In a `match` guard and the head of a `select` arm, `name =>` is the arm's arrow; a closure there goes in parentheses.
- Section 5.6 specifies how closures capture variables.

Tests: `run/basics`, `run/generics`, `run/edge`, `run/patterns`.

### 4.5 Interfaces

- Structural and implicitly satisfied: a type satisfies an interface when it has all the methods the interface requires (with the same names, parameters and `self` modes). Exception: when the interface requires `inout self`, a read-only `self` method also satisfies it (whatever can be borrowed for writing can be borrowed for reading). So the read-only `read` of `os.File` and `net.Conn` satisfies `io.Reader`.
- A borrowed interface parameter (`r Reader`, `r: inout Reader`) is a data pointer plus a method table and allocates no memory.
- An interface value (a variable, field or collection element) is an exclusive box on the heap plus a method table; dropping it drops the value inside. An interface value is never empty; write `?Reader` for an optional one.
- `match r { f File => ..., _ => ... }` branches on the concrete type.
- Interfaces can embed other interfaces. Methods with type parameters and conditional methods do not count toward the method set.
- **Type sets**: `interface Num { ~int | ~f64 }`. `~T` also accepts newtypes whose underlying type is `T`. An interface with a type set can only be used as a constraint, not as the type of a value.
- `error` is a built-in interface with the single method `fn message(self) -> String`.

Tests: `run/interfaces`, `run/iface_modes`, `run/generics`, `run/constraints`, `errors/constraints`, `errors/rules`.

### 4.6 Capabilities

Capabilities are built-in constraints that the compiler decides:

| Capability | Types that satisfy it |
| --- | --- |
| `any` | every type, linear ones included (5.7); a type parameter without it takes only types that can be dropped |
| `copy` | scalars, `Span`, copy structs and enums, fixed-length arrays of copy elements |
| `clone` | copy types; `String`, `Rc`, `Arc`, `Chan`; containers whose elements can be cloned; types with a `clone` method or whose fields can all be cloned |
| `default` | scalars, `String`, `Vec`, `Map`, `?T`; fixed-length arrays whose elements have a default; structs whose fields all have defaults; newtypes whose underlying type has a default |
| `send` | types that contain no `Rc`, `Weak`, `Cell`, raw pointer or function value (derived from the structure); interface values don't satisfy it for now (conservative) |
| `comparable` | built-in comparable types; types with an `equal` method or whose fields are all comparable |
| `ordered` | numbers, strings, `bool`, enums without data, types with a `compare` method, newtypes whose underlying type is ordered |
| `hash` | integers, strings, `bool`; types with a `hash` method or whose fields are all hashable |

Tests: `run/generics`, `run/interfaces`, `run/constraints`, `errors/unordered_sort`, `errors/send_rc`, `errors/bad_copy`, `errors/rules`.

### 4.7 Tuples

- `(A, B)` is the type of a pair, up to six elements: `(int, String, bool)`. `(a, b)` builds one; the element types come from the expected type, or from the elements (a string literal element is a `String`, as in a list). `(x)` is just `x` in parentheses, and `()` is unit.
- `t.0`, `t.1` read and assign elements (`p.1.0 = 2`), and can be moved out of an owned tuple like struct fields.
- A tuple is copy when all its elements are, comparable and hashable when they are (so it can be a `Map` key), and `clone` when they are. It prints as `(1, "a")`. Like a struct, it can't hold a view.
- Patterns: `(p, q)` matches the elements (`match pair { (0, _) => ..., (x, y) if x == y => ... }`), counts toward exhaustiveness, and works in `if let`, also through an optional: `if let (k, v) = parse(s)` when `parse` returns `?(String, int)`.
- `let (a, b) = e` and `var (a, b) = e` bind the elements (nested tuples and `_` too). The tuple is kept in a hidden variable, and each name takes its element as `let a = t.0` would: moved, copied, or, when `e` is a borrowed place, borrowed.
- A tuple is a prelude struct (`Tuple2[A, B]` with fields `_0`, `_1`, …), so it costs the same as a struct with those fields.

Tests: `run/tuples`, `errors/tuples`, `errors/tuples_views`, `errors/tuples_moves`.

## 5. Ownership

### 5.1 move and copy

- Every value has exactly one owner. Assigning, passing to a `sink` parameter, returning, storing in a collection and sending on a channel all transfer ownership; using the original variable afterwards is a compile error.
- Copy types are copied bitwise and never moved. Other types are copied explicitly with `.clone()`.
- `x.clone()` of an owned variable that nothing uses afterwards is a **warning** (the program still builds and runs): moving `x` does the same without the copy. It is given only where the move would be accepted, and where the copy can't be told apart from the original: no `clone` or `drop` method of a type in it runs, and it shares nothing (`Rc`, `Arc`, channel ends, function and interface values). It is the compiler's only warning; everything else it reports is an error.
- A variable that is moved on only some branches cannot be used afterwards: that is a "may have been moved" compile error. Where it is dropped on the other branches is in 5.3.
- A loop body cannot move a variable from outside the loop (the next iteration would move it again). Likewise, a view assigned inside a loop to a view variable from outside the loop is still there when the next iteration starts: if the next iteration may read it before reassigning it (or the loop may exit), the borrowed value cannot be modified in the next iteration.
- A single field of a struct can be moved (a partial move). After that the struct cannot be used as a whole; the fields that were not moved are dropped as usual.
- You cannot move out of a borrowed place: `return v[0]` (where `v` is a borrowed parameter) is a compile error; write `.clone()`.

Tests: `run/ownership`, `errors/use_after_move`, `errors/maybe_moved`, `errors/move_in_loop`, `errors/partial_move`, `errors/move_out_of_borrow`, `warnings/clones`.

### 5.2 Parameter modes

| Mode | Signature | Call site | Rules |
| --- | --- | --- | --- |
| read-only borrow (default) | `fn show(u: User)` | `show(u)` | the function cannot modify it or move it away |
| mutable borrow | `fn rename(u: inout User)` | `rename(&u)` | the argument must be a `var` place; the call site must write `&` |
| transfer | `fn store(u: sink User)` | `store(u)` | the argument is moved by the call |

- **Exclusivity**: at any moment a value has either any number of read-only borrows or exactly one mutable borrow. `both(&a, &a)` and `mixed(a, &a)` are compile errors.
- **Slices**: a `[]T` is a read-only view of elements, even in a `var` (other views of the same elements may exist). Only an `inout []T` parameter can change them: `v[i] = x`, `for x in &v`, `v.sort()`, `v.swap(i, j)`, `v.reverse()`, `v.sortBy(less)`. Its argument is `&xs` or `&xs[a..b]` of a `var` `Vec` or array, or another `inout` slice (`&v`, `&v[a..b]`); `xs[a..b].sort()` works the same way. An `inout` slice can be re-sliced (`v = v[1..]`) but not pointed at other elements (5.4).
- **Ranges**: a mutable borrow of `xs[a..b]` covers only those elements when the checker can compare its bounds: integer constants, and immutable locals (`let`, read-only parameters) plus or minus a constant, with an omitted bound meaning the start or the end. `split(&xs[..m], &xs[m..])` is allowed, and so are two tasks holding `&v[..p]` and `&v[p + 1..]` (10.1). Bounds that are `var`s, other expressions, or locals of a different loop iteration count as the whole value.

Tests: `run/ownership`, `run/slices_mut`, `errors/borrowed_param_mutation`, `errors/inout_needs_amp`, `errors/exclusivity`, `errors/inout_param_invalidation`, `errors/slices_mut`, `errors/slices_tasks`, `unsafe/w7_*`.

### 5.3 Dropping

- A value is dropped when its owner goes out of scope. Variables in the same scope are dropped in reverse order of declaration; struct fields are dropped in declaration order.
- A value that was moved is not dropped. A value moved on only some paths is dropped, on the others, where its owner goes out of scope, like any other (but see the next point).
- **Memory comes back early.** A value whose drop only frees memory (no type in it has a `drop` method, and it holds no `Rc`, `Arc`, `Weak`, `Mutex`, channel end, task, function or interface value) may be freed at any point after its last use and before its owner goes out of scope: no program can tell when. The compiler frees it right after the last statement of its block that uses it or anything that may borrow it (a view, a reference, a value holding one); and when some branches of an `if` or `match` moved it and nothing uses it afterwards, at the end of the others. A value that a closure or task captures, that a `defer` uses, or that `unsafe` code or a raw pointer expression (`.ptr()`) mentions is freed where its owner goes out of scope.
- A `fn drop(inout self)` method is called when the value is dropped. A type with a `drop` method cannot be copy.
- `drop(x)` drops early. It is an ordinary `sink` function.
- `defer stmt` runs when the **current block** ends; several defers run in reverse order. `errdefer stmt` runs only when the function returns an error, and can only be used in functions that return a result or an optional value.
- `break`, `continue`, `return` and `try` also run drops and defers when they leave a scope. A move inside deferred code happens where that code runs (at the end of the block, or at the `break`/`continue`/`return` that leaves the block), and is evaluated in execution order (reverse order): two defers that both move the same value, and a use after the block of a value moved in a defer, are compile errors. `errdefer` runs only when returning an error (or `none`), or when `try` propagates an error.
- Deferred code cannot leave itself: it cannot contain `return`, `try` or `yield`, and cannot `break`/`continue` an outer loop. Its own loops and closures are not affected.
- Temporaries in a statement (for example the `Vec` in `String(try readBytes())`) live until the statement is done with them.

Tests: `run/ownership`, `run/errdefer`, `run/labels`, `run/push_flow`, `run/temporaries`, `run/defer_moves`, `run/drop_states`, `errors/defer_exit`, `errors/rules`, `errors/defer_moves`.

### 5.4 Views and projections

- The view types `str`, `[]T` and `MutexRef[T]`, and references lent by projections, **cannot be stored**: they cannot be struct fields, collection elements or captures of escaping closures, and cannot be returned from functions (except through the `yield` of a projection).
- A projection function lends a reference with `yield`: `yield &T`, `yield inout T`, `yield ?&T`. `v[i]`, `m.get(k)`, `m.entry(k)` and `mu.lock()` are all projections.
- `let x = v[i]`: when the element is copy it is copied out; otherwise `x` is a reference. To own the value, write `.clone()`.
- `if let u = m.get(k) { }` and `with e = m.entry(k) { }` bind the reference to the block; the reference cannot be taken out of the block. The block cannot modify the borrowed container. A `with` body runs exactly once, so a function can end with a `with` whose body returns. A `with` is an expression with its body's value (`let n = with v = m.lock() { v.len() }`, or the last expression of a function); that value can't be a view (what the block borrows, such as locked data, is reachable only inside it), and moving out of the borrowed value is an error as anywhere else.
- **Last use**: a view or reference bound with `let` stops holding the borrowed value after its last use in the block; after that the original value can be modified.
- A view cannot outlive the value it borrows. You cannot take a view of a temporary and keep it (`let s: str = make()`).
- While a view exists, the borrowed `Vec`/`String` cannot be modified, so `push` never leaves a view dangling.
- **Views live only in variables** (possibly inside a variable's `?T` / `T ! E`). No value can exist that puts a view into any other storage, whether its type is written or inferred: `Rc(s)`, `Box(s)`, `[2]str`, `Vec[MutexRef[T]]`, a struct field (other than a view struct's), and putting a view into an interface value (`xs.push(s)` where `xs` is a `Vec[Len]`).
- **View collections.** A `Vec` or `Map` whose elements (keys or values) are views (`str`, `[]T`, a view struct, or a view collection in turn) is a view itself: `Vec[str]`, `Map[str, int]`, `Vec[Token]`. It lives only in variables under every rule here, owns its buffer (it is moved, not copied, and its memory is freed after its last use), and borrows what its elements view: `[s]`, `ws.push(w)`, `m[w] = 1` and `m.entry(w)` add what `w` views to what the collection borrows, which can't change or go away while the collection is in use. A view read out of it (`ws[i]`, `for w in ws`) views the same data, and so may outlive the collection; a reference into it (`ws.get(i)`, a slice `ws[1..]`) also borrows the collection itself, which can't change meanwhile. A function returning one says what it views like any returned view (`-> Vec[str] of text`, or inferred). `s.splitViews(sep)`, `s.lineViews()` and `s.fieldViews()` are the copy-free versions of `split`, `lines` and `fields`.
- **Views left in `inout` parameters.** A function may store views of its other parameters in an `inout` parameter that can hold views (`fn tokenize(src: str, out: inout Vec[Token])`); the compiler works out which parameters those are, as for a returned view, and the caller's variable then borrows those arguments. Storing a view of anything of the function's own there is an error.
- **Loops over a temporary.** The elements of `for x in f()`, when `f()` is a temporary that owns them (`text.split(" ")` gives a `Vec[String]`), are gone when the loop ends: a view of one can't be kept past the loop (`best = w`). Bind the temporary to a variable first. When the temporary is a view collection, the views read from it view what it does.
- **View types.** `view struct Token { kind: Kind, text: str }` (or `view enum`) may hold views, and is a view itself: it lives only in variables, like `str`, under every rule above. It borrows what its views point into: `Token{text: src[0..3], kind: k}` borrows `src`, a field read from it (`t.text`) borrows the same, and assigning a view to a field (`t.text = s`) adds to what the variable borrows. Its other fields must be copy, so it is copied like `str` and never dropped; owned data lives beside it. It can't have a `drop` method or hold task handles, scopes or lock guards, and isn't `send`. A method with `inout self` can only store subviews of `self` in it (`self.rest = self.rest[1..]`), as for an `inout` slice. A view returned by its methods borrows what the struct does, not the variable holding it: a lexer's tokens point into the input, so the lexer can move on while earlier tokens are in use.
- **Bindings into a view variable.** A `match` (or `if let`) binding on a variable that holds a view points into that variable, so the variable can't be given another value, directly, through `inout` or in a closure, while the binding is in use. Assigning to such a binding assigns the variable: the variable then views what was assigned too, under the rules for assigning to it. A binding of data the variable holds inline (an array payload) borrows the variable itself, as does a view of an inline field (`v.arr[0..2]`); a task that reads a view variable in place keeps it from being given another value until the task's scope ends.
- **Copies are passed by value.** A parameter of a copy type (a `copy struct`, an array, a view struct) is the function's own copy, so a view into data it holds inline (`self.arr[0..2]`) can't be returned; a view it holds (`self.text`) can, since it points into the caller's data.
- **Generic `inout` arguments.** A call that may store any of its arguments in an `inout` argument the borrow checker can't see into (a generic `inout T`, such as `swap` and `replace`, or a call through a function value or interface) leaves that argument viewing what the other arguments do.
- **Errors leave as returns.** An error that `try` passes up is returned: if it holds a view, it borrows what it points into like a returned view (and counts for `of`). A task started with a plain `go` whose error may hold a view can't use anything declared inside its scope: its error becomes the scope's, used after the scope's body is gone (`err("stop")` is fine).
- **Assigning to a view variable**: `v = s.asStr()` requires the borrowed value (`s`, or the final owner that `s` borrows from) to live at least as long as `v`: it must be declared in an outer block, or in the same block before `v` (values in the same block are dropped in reverse order of declaration, so the later one is dropped first, and a `defer` in between would see it already gone). After that, the borrowed value cannot be modified anywhere in `v`'s scope (up to its last use). `let v = { let s = ...; s.asStr() }` is rejected for the same reason.
- An `inout` view parameter can only be assigned a subview of itself (`rest = rest[1..]`) or a literal: the caller cannot see a new borrow. A closure cannot assign a view to a captured outer variable.
- A view returned by a call borrows the arguments the function's returned view may point into, including variables captured by function-value arguments (`run(() => s.asStr())` borrows `s`; `f()` borrows the closure `f` itself) and values passed as interfaces. For a function written in Gojo, the compiler works out which parameters those are from what its `return`s point into (through calls of other functions, recursive ones included); `fn pick(a: str, b: str) -> str { return a[1..] }` returns a view of `a` only, so `pick(x, y)` leaves `y` free to change. Calls through function values, interfaces and type parameters, and functions without a body, borrow all their arguments. A reference lent by a projection function written in Gojo is treated as borrowing the parameters that its `yield` actually points into.
- `-> T of a, b` states which parameters (`self` included) the returned view may point into. Callers rely on it instead of the body, so the body can change without changing what callers may do; returning a view of anything else is an error. It may name more than the body uses. A `sink` parameter, or one passed by value (a number, `bool`), can't be named: a view can't point into it.
- A reference into a temporary bound by `if let x = f()` or `match f() { }` is valid only inside that `match`; views of it cannot be taken out.
- A function's `defer` runs after the return value has been computed, so it cannot modify data that a returned view points to.
- **Data inside a lock**: a view obtained inside `with s = mu.lock() { }` (including locks from `Mutex(&x)`) cannot leave the block, cannot be assigned to a variable outside the block, and cannot be returned: the lock is released when the block ends, and other tasks may then modify the data.
- **Generics and views**: if a generic function keeps a value of its type parameter `T` after the call ends (captured by an escaping closure or by a task started through a `Scope` value, or passed to another generic function that keeps it), then `T` cannot be a view type at the call.

Tests: `run/lastuse`, `run/generics`, `run/spans`, `run/mutexref`, `errors/view_field`, `errors/view_return`, `errors/view_temporary`, `errors/view_outlives_owner`, `errors/views_in_containers`, `errors/escaping_view`, `errors/projection_borrow`, `errors/mutable_ref_conflict`, `errors/iterator_invalidation`, `errors/move_while_borrowed`, `errors/view_assign_scope`, `errors/view_lock_escape`, `errors/stored_views`, `errors/generic_keeps_view`, `errors/closure_view`, `errors/defer_returned_view`, `errors/temp_and_iface_views`, `run/view_structs`, `run/view_sources`, `errors/view_types`, `errors/view_types_syntax`, `errors/view_types_borrow`, `errors/view_types_capture`, `errors/match_view_storage`, `errors/view_sources`, `errors/ret_of`, `errors/view_holes`, `errors/task_error_view`, `run/view_collections`, `run/vec_literal_view`, `errors/view_collections`, `errors/view_collections_borrow`, `errors/view_collections_inout`, `errors/loop_temp_view`.

### 5.5 Shared ownership

- `Rc(x)` and `Arc(x)` create reference-counted values that are read-only inside; to modify them, wrap the contents in `Cell` or `Mutex`. `.clone()` increments the count.
- `Weak[T]` comes from `Rc.downgrade()`; `upgrade()` returns `?Rc[T]`. The programmer breaks cycles with `Weak`.
- `Arena[T]` with generational `Id[T]`: accessing through a deleted Id returns `none`.

Tests: `run/misc`, `run/interfaces`, `run/niche`, `run/arena`.

### 5.6 Closure capture

- A non-escaping closure (passed as an ordinary parameter) borrows outer variables and can modify `var` variables.
- An escaping closure (a `sink fn()` parameter, a stored closure, one given to `go`) owns its captured variables: copy types are copied, other types are moved into the closure. Views cannot be captured by escaping closures.
- A closure given to `go` cannot modify outer variables.

Tests: `run/basics`, `run/concurrency`, `errors/escaping_view`, `errors/go_closure_mutation`.

### 5.7 Linear types

A value of a **linear** type is never dropped implicitly: every value must be consumed. `linear struct` and `linear enum` declare one:

```gojo
linear struct Tx {
    id: int

    fn commit(sink self) ! DbError { ... }
    fn rollback(sink self) { ... }
}
```

- **Consuming** a value means moving it: into a `sink` parameter (its own `sink self` methods above all), out of the function with `return`, or into something that is consumed in turn (a field of a linear struct, a `Vec`, a `?T`, a result or a tuple). Panics abort, so no value is ever dropped on the way out of a panic.
- **What is rejected**: a variable or `sink` parameter (of a function or a closure) that still holds the value where its block ends, at a `return`, `break` or `continue` that leaves it, or where a `try` returns an error; a temporary nobody takes (`begin()` as a statement, `_ = begin()`, `begin().id`, the result of a plain `go`); assigning over a variable that holds one, and assigning to any place the function doesn't own (an `inout` parameter or `self`, an element, a reference, a variable a closure captures), since what was there would be dropped: `let old = replace(&x, new)` gives it back instead; moving it into a closure other than the body of a `go` (a closure could be dropped, or called twice); and a `match` arm that moves out of the value but drops a linear part with `_`.
- **Linear errors**: an error type can be linear, and is then consumed like any other value. A `catch` must bind it, `try` can't turn it into an `error` interface value, a `try ... else wrap` hands it to `wrap` (which must consume it), and the tasks of a `scope` can't fail with one (when several fail, only the first error is kept). Errors name the path (`pool.tx`), say whether it is unconsumed on every path or only some, and list the type's `sink self` methods.
- **Variables that may be empty**: `none`, `[]` and variants without linear payload hold nothing to consume, so `var o: ?Tx = none` needs nothing until a value is put in. A `match` (or `if let`) arm that can only see such variants consumes the value too: after `if let t = o { t.commit() }` there is nothing left. A vector of linear values is consumed by popping each one and ending the empty vector with `v.finish()`, which panics if it still holds values.
- **Taking it apart**: only the type's own package can move fields out of a linear value (elsewhere, `cannot move out of field`), or move a payload out of a variant of a linear enum. In its own methods with a `sink self` receiver, `self` is being ended: when the method returns, its fields are dropped as usual, except its linear fields, which must be consumed. In a linear enum's own `sink self` method, a `match` on `self` ends it in arms whose variant has no linear payload.
- **Guarded defers**: a `defer` or `errdefer` whose body consumes a linear place declared outside it (`tx`, `pool.conn`, or a value it matches with `if let`) runs only while all of that place is still there: neither it, nor part of it, nor what it is part of has been moved. `defer tx.rollback()` rolls back on every way out before `tx.commit()`, and does nothing after it; `errdefer tx.rollback()` does the same on error exits only. A defer may consume at most one linear place.
- **Types that hold linear values**: a struct or enum with a linear field or payload must be declared `linear` itself. Linear values can be held by linear structs and enums, `Vec`, `?T`, results and tuples, which are consumed in turn. `Rc`, `Arc`, `Weak`, `Cell`, `Mutex`, channels, tasks, `Iso`, `Box`, `Map`, arrays and interface values (a borrowed interface parameter is fine) would drop them, so `Rc[Tx]` and `let r: Reader = tx` are errors.
- **Generics**: a type parameter takes linear types only if declared `any` (`[T: any]`, or `where T: any` on a method): a generic body may drop a `T` otherwise. In a body where `T` is `any`, values of type `T` are linear, and checked as such. `Stack[T: any]` holding a `Vec[T]` is linear exactly when its argument is. In the standard library, `swap`, `replace`, `take`, the `Vec` methods that move elements (`push`, `pop`, `insert`, `remove`, `swapRemove`, `extend`, `reverse`, `swap`, `drain`, `splitOff`, `reserve`, `cap`, `finish`), the read-only slice methods (`len`, `get`, `first`, `last`, `find`, `each`, `map`, `sortBy`...), `?T`'s `isSome`, `isNone`, `unwrap` and `expect`, results' `isOk` and `isErr`, and `json.encode` are `any`; `drop`, `clear` and `truncate` are not.
- A linear type is not `copy`, has no derived `clone` (a `clone` method of its own still works), and can't have a `drop` method. A package-level value can't be linear: nothing could consume it.
- The check is per function: a forward analysis over the paths that hold a value still to be consumed, like the borrow checker's. It needs nothing from other functions' bodies.

Tests: `run/linear`, `run/linear_review`, `errors/linear_leaks`, `errors/linear_generic`, `errors/linear_decl`, `errors/linear_copy`, `errors/linear_paths`, `errors/linear_hole_*`, `projects/linear`, `projects/linear_enum`.

## 6. Expressions

- Operator precedence from lowest to highest: `??` (right-associative), `||`, `&&`, comparisons, `|`, `^`, `&`, `<< >>`, `+ - +% -%`, `* / % *%`, then unary `- ! ~ & <- *` and postfix `.f`, `?.f`, `(...)`, `[...]`.
- Both sides must have the same type; there is no implicit promotion. `+` concatenates strings.
- `+ - *` panic on integer overflow (in both debug and release); `+% -% *%` wrap. `/` and `%` panic on division by zero. Out-of-bounds indexing panics. `<<` and `>>` panic when the shift amount is not between `0` and the bit width minus one.
- An index or slice bound that is a constant and can only panic is a compile error: a negative one, one past a fixed array's length (`a[4]` of a `[4]int`; a slice may end at the length), or slice bounds whose start is after their end (`v[3..1]`).
- A numeric conversion `T(x)` uses the target type only to decide the type of literals (`u64(18446744073709551615)`, `u64(1 << 63)`); other arguments are converted from their own type (when `b` is a `u8`, `int(~b)` first complements on `u8`).
- `if`, `match` and blocks are expressions; their value is the last expression.
- Operations on `?T`: `if let v = x { }` (and `if let x { }`, which unwraps the variable `x` under the same name inside the block), `x ?? d`, `x?.field` (the result is still optional, and `field` must be a copy type; otherwise use `if let`), `x?.method(args)` (`none` when `x` is `none`; a method that returns `?U` gives `?U`, not `??U`; the method may only read its receiver, and a view it returns borrows `x`, so `x` must be a variable to keep it), `x.unwrap()` (panics on `none`), `match`.
- `[a, b]` is a `Vec` literal, or a `Set` where a `Set` is expected (`let s: Set[String] = ["a", "b"]`, a field, a parameter). `["a": 1, "b": 2]` is a `Map` literal and `[:]` an empty one, typed by the context or by the entries. `Map[K, V]{k: v}` and `Vec[T]{}` are typed literals. String literals in a list or map are `String`s.
- **List elements**: besides values, a list literal can hold `if cond { x }` (no `else`: `x` is added when `cond` holds; also `if let`), `for x in xs { f(x) }` (one element per iteration) and `..v` (every element of the `Vec` `v`, which is moved; `..v.clone()` keeps it). The block of an `if` / `for` element may have statements before its value, `continue` skips an iteration, and the value may itself be an `if` without `else` or a `for`: `[for r in rows { for c in r.cells { if c.shown { c.label.clone() } } }]`. It costs what the pushes would: the list is built in a hidden `Vec`. `if c { a } else { b }` is an ordinary element.
- `a..b` is a half-open range and `a..=b` a closed range; `s[a..b]` takes a view of a string or slice.
- A `T ! E` result must be handled; discard it with `_ = f()`.

Tests: `run/basics`, `run/edge`, `run/misc`, `run/constraints`, `run/patterns`, `panic/overflow`, `panic/div_zero`, `panic/bounds`, `panic/bounds_loop_*`, `panic/shift`, `panic/unwrap_none`, `errors/unhandled_result`, `errors/rules`, `errors/const_index`.

## 7. Statements and control flow

- `if c { } else if d { } else { }`; the condition must be a `bool`.
- `for x in xs`, `for i, x in xs` (index and element), `for i in 0..n`, `for k, v in m`, `for x in ch`, `while cond { }`, `loop { }`.
- `for x in v` borrows each element; `for x in &v` iterates mutably, and `v` must be a `var`. The collection being iterated cannot be modified during the iteration.
- **Labels**: `outer: for ...` (or `outer: while`, `outer: loop`); `break outer` and `continue outer` jump to that loop. A label cannot have the same name as the label of an enclosing loop; the label of a `break` must belong to an enclosing loop. A `break` inside a `select` arm applies to the enclosing loop.
- `match` is exhaustive. Patterns: literals (including negative ones), ranges `1..=9`, alternatives with `|`, `_`, bindings, enum variants (can nest; every field must be written), `ok(x)`/`err(e)`, `some(x)`/`none`, tuples `(a, b)`, and the concrete type of an interface value, `f: File`. An arm can have a guard `if cond`; guarded arms do not count toward the exhaustiveness check.
- **`let ... else`**: `let pattern = e else { ... }` matches `e` against a pattern that may fail (`let .Circle(r) = s else { return 0 }`, `let ok(v) = r else { ... }`, `let (k, 0) = pair else { ... }`); `let x = opt else { ... }` unwraps an optional. The `else` block must leave (`return`, `break`, `continue`, a panic); the names it binds are in scope for the rest of the block. It is the same as `match e { pattern => { rest of the block }, _ => { else } }`, with the same binding rules.
- When the matched value is a place, bindings are references by default. When an arm moves a binding away, that arm consumes the matched value.
- There is no `switch`.

Tests: `run/basics`, `run/labels`, `run/edge`, `run/misc`, `run/patterns`, `run/loop_views`, `errors/labels`, `errors/non_exhaustive`, `errors/iterator_invalidation`, `errors/iter_mutation`, `errors/iter_let`, `errors/rules`, `errors/loop_exits`, `errors/loop_carried_view`.

### 7.1 Custom iteration

For a custom type, `for x in t` looks for a protocol in this order:

1. **Cursor (pull)**: `fn cursor(self) -> C` and `fn next(self, c: inout C) yield ?&T` (or one that returns an owned `?T`, used as a generator). `for x in &t` uses `nextMut`. The `for` is translated into "get a cursor, then call `next` in a loop", with no closure.
2. **Push**: `fn each(self, body: fn(&T) -> Flow)`; `for x in &t` uses `eachMut(inout self, body: fn(inout T) -> Flow)`. The loop body becomes a non-escaping closure; the `break`, `continue`, `return`, `try` and labeled jumps in it are rewritten into control signals. When `each` receives `Flow.Break` it should return as soon as possible; even if it keeps calling, the loop body does not run again, so a wrong `each` only runs a few extra rounds and cannot change the program's result. The loop body runs many times, so it cannot move outer variables.

When a type provides both protocols, the cursor is used. A type that satisfies neither protocol cannot be used in `for`. The value being iterated cannot be modified during the iteration.

Whether a view of an element can outlive the current iteration: with the cursor protocol, it can when `next` lends a part of `self` (as when iterating a `Vec`; the iterated value then cannot be modified); it cannot when `next` lends data in the cursor, because the next step changes the cursor. With the push protocol it cannot, because `each` may pass the body a value that is valid only for one call.

Tests: `run/iterators`, `run/iterator_views`, `run/constraints`, `run/push_flow`, `errors/iter_misuse`, `errors/iter_mutation`, `errors/iterator_views`.

### 7.2 Lazy chains

`xs.iter()` followed by adapters is a lazy chain, for anything `for` can iterate over by value: lists, slices, arrays, sets, maps, ranges (`(0..n).iter()`), strings (their runes) and types with an iteration protocol. A type with its own `iter` method keeps it.

- **Adapters**: `map(f)`, `filter(p)`, `filterMap(f)` (`f` gives `?U`), `flatMap(f)` (`f` gives something iterable), `take(n)`, `skip(n)`, `takeWhile(p)`, `skipWhile(p)`, `stepBy(n)` (elements 0, n, 2n...; panics unless n > 0).
- **Consumers**: `for x in chain { }` and `for i, x in chain { }` (`i` counts the elements the chain yields), or a terminal method: `collect()` (a `Vec`), `count()`, `sum()`, `min()`/`max()` (`?T`), `any(p)`, `all(p)`, `find(p)`/`first()`/`last()` (`?T`), `fold(init, f)`, `join(sep)` (for strings).

A chain is not a value: it can't be stored, returned or passed (`let it = xs.iter()` is an error). It is compiled into one loop over `xs`, with each adapter a few statements inside it, so it allocates nothing and runs as fast as the loop written out; only the elements needed are computed: `find` stops at the first match, and `take(n)` ends the loop once it has its nth element, so chains over endless sources end. The closures are ordinary arguments: they borrow what they use, views included, and may modify `var` variables. `xs` stays borrowed for the whole loop. Elements that come from `xs` are borrowed; `collect`, `find`, `first`, `last`, `min` and `max` clone the ones they keep, and take values the chain computed (`map`) as they are. In `flatMap`'s body, `break` leaves the whole chain and `continue label` goes on with the next inner element.

Over a `Map`, the elements are entries, a key and a value: the closures take both (`(k, v) => ...`, as in `Map.retain`; `fold`'s takes `(acc, k, v)`), and `for k, v in m.iter()...` binds both. They stay pairs until `map`, `filterMap` or `flatMap` makes one value of each. A chain of pairs `collect`s into a `Map` (keeping the order), `find`/`first`/`last` give `?(K, V)`, and `sum`, `min`, `max` and `join` need one value per element: `m.iter().map((k, v) => v).sum()`.

```gojo
let names = people.iter().filter(p => p.age >= 18).map(p => p.name.asStr()).join(", ")
for w in lines.iter().flatMap(l => l.split(" ")).take(10) { }
let total = orders.iter().map(o => o.price * o.qty).sum()
```

Tests: `run/lazy`, `errors/lazy`, `errors/iter_mutation`.

## 8. Error handling

- `fn f() -> T ! E`; `return err(e)`, `return x` (wrapped as ok automatically).
- `try e`: on error, returns the error to the caller. The error types must be the same, or the outer error type must be `error`; otherwise write `try e else Wrap`, where `Wrap` is a function or enum variant that converts the inner error to the outer error. `try` can only be used in functions that return a result or an optional value; on a `?T` it propagates `none`.
- `e catch err { block }`: handles the error in place; the block's value is the value of the whole expression.
- `match r { ok(v) => ..., err(.Kind(x)) => ... }`.
- `x.unwrap()` panics on error.
- **Functions that can't fail**: `T ! never` is a result that is never an error, and a call returning one gives its `T` directly (no `try`, nothing to handle). A generic function whose error type parameter appears only as the error type of results (`fn map[T, R, E](xs: []T, f: fn(T) -> R ! E) -> Vec[R] ! E`) takes it as `never` when nothing at the call decides it: given `x => x * 2`, `map` gives a `Vec[int]`; given a function that returns `int ! ParseError`, a `Vec[int] ! ParseError`; an expected type (`let r: Vec[int] ! MyError = map(...)`) decides it too. A named function that returns a plain value is accepted where one returning `R ! E` is expected (it is called through a closure that wraps its result). So one function serves both: `task.map` and `task.each` take a function that can fail or one that can't.
- In a function that returns `?T ! E`, `return none` is a successful `none` (wrapped as ok). When the expected type of a `.Variant` shorthand is `E ! Err` or `?E`, it resolves against `E`.
- **Error types**: each module uses its own concrete error type, and they all satisfy the built-in interface `error` (`fn message(self) -> String`), so a function written with `! error` can `try` errors from any module. Errors from the operating system, files, processes and the network are all `io.Error`: `kind` (an `io.ErrorKind`: `NotFound`, `PermissionDenied`, `AlreadyExists`, `Cancelled`, etc., independent of the platform), `message()`, `code` (the system error number). `os.Error` and `net.Error` are aliases of it. Errors of other modules carry their own information, such as the line and column of `json.Error`, or `time.ParseError`.

**Error return traces**: in a debug build, an error that leaves `main` prints, after the error itself, where it was made (the `err(...)`) and each `try` that passed it up, in order. The trace belongs to the task that made the error; making a new error starts a new trace. Release builds don't keep it.

A **panic** signals a bug in the program. A panic prints a message and a source location (pointing into user code, not the standard library) and terminates the whole process with exit code 2. It does not unwind the stack and does not run defers. There is no `recover`.

Tests: `run/errors`, `run/errdefer`, `run/patterns`, `run/never_errors`, `errors/try_error_mismatch`, `errors/try_outside_result`, `errors/unhandled_result`, `panic/unwrap_err`, `panic/unwrap_none`, `panic/no_defer`, `testblocks/blocks`, `traces/basic`.

## 9. Generics

- Generic functions, structs and enums: `fn max[T: ordered](xs: []T) -> ?T`, `struct Stack[T]`. Constraints are written next to the parameter, or with `where T: copy + ordered`.
- Methods can have their own type parameters. A `where` on a method makes it a conditional method, which exists only when the condition holds. Calling a method whose condition does not hold is a compile error.
- Type parameters are inferred locally at the call site (including closure return types). When they cannot be inferred, write them out: `fresh[int]()`.
- A generic body can only use operations its constraints guarantee: call methods declared by an interface constraint, copy only with `copy`, compare only with `ordered`, do arithmetic only when the type set is all numbers.
- **Integer const generics**: `struct Mat[R: int, C: int] { data: [R][C]f64 }`. Only integer parameters are allowed, and there is no arithmetic at the type level. Mismatched dimensions are a compile error.
- Implementations are monomorphized per type argument.

Tests: `run/generics`, `run/constgen`, `run/constraints`, `errors/constgen`, `errors/constraints`, `errors/unordered_sort`.

## 10. Concurrency

### 10.1 Scopes and tasks

- `scope { body }` is an expression; its value is the value of `body`. `go f(x)` or `go () => { ... }` inside it starts a **task** of the innermost `scope` of the same function or closure. `go` outside any scope is an error, and so is a `go` inside a closure whose scope is outside the closure (the closure needs its own `scope`, or a `Scope` value, below). The bodies of `main` and of `test` blocks are scopes.
- A `scope` ends by waiting for every task started in it, before the values declared in its body are dropped. Leaving the body early (`return`, `try`, `break`, `continue`) first cancels its tasks (10.6), then waits.
- `go` as a statement starts a task whose result is not used. `let t = go f()` gives a handle of type `Task[R]`, where `R` is what the task returns; `R` can't be a view. A handle is a view of its scope: it can't be stored, sent, returned or assigned to a variable outside the scope, or be the scope's own value. On every path that reaches the end of its block it must be consumed, by `t.join()` (waits, then gives the result), `t.cancel()` (cancels just that task; the scope still waits for it, and drops its result), or a `select` arm (10.4); otherwise `task t is never joined`. A handle dropped when its block is left early cancels its task.
- **Arguments.** In `go f(a, b)`, an argument that is a place (`x`, `x.f`, `x[i]`, `&x`) is used by the task where it is: borrowed, or, for a `sink` parameter, moved into the task right away. Any other argument is evaluated before the task starts, and the task owns the result.
- **Captures.** A task borrows the variables it uses that live until the scope ends: those declared outside the scope, and those declared directly in the scope's body (not in a block inside it). They stay borrowed until the scope ends, so the body can't modify them (or, if the task modifies them, use them) or move them meanwhile. A task may modify a variable that nothing else uses until then; two tasks can't both use a variable one of them modifies, and neither can the tasks a loop starts in different iterations. A task borrows only the fields it uses (`ch.rx` leaves `ch.tx` free), and only the range of elements it uses when that range is one the checker can compare (5.2): `go psort(&v[..p])` and `go psort(&v[p + 1..])` run at once. Variables declared in blocks inside the body are moved (or copied) into the task, which then owns them; it can't modify a copied one. Everything a task captures must be `send`.
- **Failure.** A task whose result type is `T ! E` fails when it returns `err`. A failure cancels the scope (its other tasks and its body). The first error of a task started with a plain `go` becomes the scope's value: the scope then has type `B ! E` (`B` is the body's type), and is usually written `try scope { }`; all plain tasks of one scope must fail with the same `E`. The error of a task with a handle goes to whoever joins it. In `main` and in tests, an error of a plain task is the function's error (it must return `! E`).
- `scope s { }` binds `s`, of type `Scope` (a view of the scope; copy, and like a handle it can't leave the scope). `s.cancel()` cancels every task of the scope and its body. A function can take a `Scope` parameter and start tasks with `s.go(() => ...)` (or `s.go(f(x))`); such a task may outlive the function, so it must own everything it captures, can't fail, and keeps any type parameter it captures (which then can't be a view at the call, 5.4).

Tests: `run/scopes`, `run/task_move`, `run/concurrency`, `run/mutexref`, `run/cancel`, `errors/task_capture`, `errors/task_moved`, `errors/task_race`, `errors/task_shared_mutation`, `errors/task_never_joined`, `errors/task_results`, `errors/go_needs_scope`, `errors/scope_handle_escape`, `errors/scope_handle_outlives`, `errors/scope_handle_chan`, `errors/scope_handle_field`, `errors/scope_value_go`, `errors/generic_keeps_view`, `unsafe/w1_*`, `unsafe/w3_*`, `unsafe/w6_*`, `unsafe/w8_*`.

### 10.2 Data parallelism

- `par for x in &v { }` (a `var` `Vec` or array, an `inout` slice, or a range of one), `par for x in v` (also any slice), `par for i, x in &v` and `par for i in a..b` run the iterations on up to `maxProcs` tasks, in chunks handed out as tasks finish them. With `&v` the body may modify its element. It may only read what it captures, which must be `send`. `continue` ends an iteration; `break`, `return` and `try` are errors. The statement ends when every iteration has.
- `send: fn(A) -> R` is the type of a function that any number of tasks may call at once. A closure is a `send fn` when everything it captures is `send` and it only reads it; functions declared with `fn` are `send fn`. A `send fn` can be used where a `fn` is expected, not the other way round. A `send fn` is `send`.
- `task.map(xs, limit, f)` calls `f` on each element in up to `limit` tasks at once (0: no limit) and gives the results in order; when `f` can fail (`R ! E`), the first error cancels the calls still running and is the result (section 8: for an `f` that can't fail, the result is just the vector); `task.each` gives no results, and fails the same way; `task.race(fs)` runs the functions at once, gives the first success and cancels the rest (all fail: the last error).

Tests: `run/parallel`, `errors/par_for`, `errors/send_fn`.

### 10.3 Channels

- `Chan[T](n)` makes a channel with room for `n` values (`Chan[T]()` and `Chan[T](0)`: none, so a send waits for a receiver). Its value is a `Chan[T]` with two fields, `tx Sender[T]` and `rx Receiver[T]`. `T` must be `send` and can't be a view.
- `tx <- v` sends `v` (moving it), waiting while the buffer is full. `tx.send(v)` does the same and gives `false` when the value was not sent: every receiver is gone, or the task was cancelled; the value is dropped then. `<-rx` gives `?T`: `none` once every sender is gone and the buffer is empty (or when the task is cancelled). `for x in rx` receives until then. Both ends can be cloned for more senders or receivers; `.len()` is the number of values in the buffer.
- **Closing.** A channel closes when its last `Sender` is dropped (`drop(ch.tx)` closes it early). When its last `Receiver` is dropped, the values in the buffer are dropped, and sends fail. There is no `close` method; nothing about channels panics.

Tests: `run/concurrency`, `run/sched`, `run/niche`, `run/labels`, `errors/send_rc`, `unsafe/w1_chan_view`, `unsafe/w6_chan_of_rc`, `unsafe/w1_send_then_use`.

### 10.4 `select`

- Arms: `let x = <-rx => ...` (`x` is `?T`), `<-rx => ...`, `tx <- v => ...`, `let r = <-t => ...` (a task handle in a variable: the task has finished; the arm takes its result and the handle), `time.after(d) => ...` (`d` after the `select` starts), `time.at(t) => ...` (at the instant `t`), `cancelled => ...` (the task has been cancelled) and `_ => ...` (nothing is ready: don't wait). Any arm can have a guard, `... if cond =>`; guards, channels, values and times are evaluated once, before waiting, and an arm whose guard is false takes no part. At most one `_` and one `cancelled` arm. `time.after` and `time.at` exist only as arms.
- When several arms are ready, one is chosen at random; a `cancelled` arm wins as soon as the task is cancelled. A send arm whose receivers are all gone is never chosen. The value of a send arm that is not chosen is dropped. Without a `cancelled` arm, in a cancelled task the receive arms see their channels as closed, and with only send arms nothing runs. `break` / `continue` in an arm apply to the enclosing loop.

Tests: `run/select`, `run/concurrency`, `run/sched`, `errors/select_arms`, `errors/select_v2`, `errors/select_task_join`, `errors/loop_exits`, `unsafe/w1_move_in_select`, `unsafe/w2_select_arm_view_escape`, `unsafe/w3_select_send_view_chan`, `unsafe/w5_move_in_select_loop`.

### 10.5 Sharing

- `Mutex(x)` owns its data; `Arc(Mutex(x))` is for long-term sharing. `with v = mu.lock() { }` locks; the lock is released when the block ends, and the reference can't leave the block. `Mutex(&x)` makes a `MutexRef[T]`, a lock that borrows `x` (a `var`); while it exists `x` is reachable only through it.
- `with a, b = lock(m1, m2) { }` takes two or more locks at once, in one order everywhere (by address), so code that always takes several locks this way can't deadlock on them. The same mutex twice is an error, or a panic when only known at run time.
- **No waiting under a lock.** A send, receive, `select`, `time.sleep`, `join` or `scope` written directly inside a `with ... lock()` body is an error: the tasks waiting for the lock would wait too. (Waits inside called functions are found at run time, 10.7.)
- `RwLock(x)` lets many tasks read at once, or one write: `with v = rw.read() { }` gives a read-only reference, `with v = rw.write() { }` a mutable one; neither can leave its block, and the rules for waiting under a lock apply to both. Waiters are served in order, a run of readers together, so a stream of readers can't starve a writer.
- `Atomic[T]`, for integer types: `Atomic(x)`, `load`, `store`, `add` and `sub` (wrapping; they give the new value), `swap` (gives the old one), `compareSwap(old, new)` and `clone`. Every operation is atomic and sequentially consistent; tasks share an `Atomic` by borrowing it.
- `send interface Name { }` is an interface that only `send` types implement, so its values are `send`; a type parameter bounded by it is `send`.
- `isolate { body }` has type `Iso[T]` (`T` is the type of `body`, not a view). The body may use variables from outside only if their types are `send`, so nothing outside points into what it builds. `Iso[T]` is `send` whatever `T` is (it may hold `Rc` and `Cell`), can't be cloned, and `iso.take()` gives back the `T`.
- `task.yieldNow()` lets other tasks run.

- **Task-local values**: `let key = task.local[T]()` (at package level) makes a key; `task.with(key, value, () => body)` binds it while `body` runs, and `key.get()` gives a copy (`?T`; `T` must be `send` and cloneable) in that code and in every task started inside it, through any number of scopes and shields. An inner `task.with` of the same key hides the outer one until it ends. Nothing else can change a binding, so tasks never race on one.

Tests: `run/sharing`, `run/isolate`, `run/mutexref`, `run/rwlock`, `run/task_locals`, `errors/rwlock`, `errors/wait_under_lock`, `errors/send_interface`, `errors/isolate`, `errors/isolate_clone`, `errors/mutexref`, `errors/mutexref_borrow`, `errors/mutexref_immutable`, `panic/deadlock_locks`.

### 10.6 Cancellation

- Cancellation propagates down the task tree: when a task fails, `s.cancel()` or `t.cancel()` is called, a scope is left early, or the deadline of a `task.timeout` passes, every task below that scope, nested scopes included, is cancelled. A scope still waits for all its tasks, because they may be borrowing data from outside it.
- In a cancelled task, blocking operations return early: a receive gets `none`; a send does not happen and the value is dropped; `select` behaves as in 10.4; `time.sleep` wakes early; `net` reads and writes, `accept` and `dial` return a `net.Error` whose `isCancelled()` is true. Locking and `join` are not cancelled.
- `task.cancelled()` reports whether the current task has been cancelled, and `task.reason()` why: `?task.Reason`, one of `.Failed` (a task of the scope failed), `.Deadline`, `.Explicit` (`cancel()`, or a scope left early) and `.Shutdown`. `try task.check()` returns `task.Cancelled{reason}` when it has, as a checkpoint in long computations. Cancellation does not interrupt code that doesn't block.
- `task.timeout(d, () => body)` returns `?T`: `some` if the body finishes within the deadline, `none` if the deadline passes or the caller is cancelled. Deadlines nest; `task.deadline()` gives the nearest one of the current task (`?int`, on the `time.now()` scale).
- `task.shield(d, () => body)` runs the body shielded: cancelling the caller doesn't affect it, only the shield's own deadline `d` does. A type's `drop` method runs shielded the same way when its task is cancelled, for up to a second (`GOJO_DROP_GRACE`, in milliseconds), so cleanup that has to block still happens.
- Ctrl-C (SIGINT) and SIGTERM cancel the scope of `main`, with reason `.Shutdown`; the program then ends as usual (tasks wind down, values are dropped) with exit code 130. A second signal ends it at once. Code that belongs to no scope, like `main`'s own statements, is cancelled only this way.

Tests: `run/cancel`, `run/cancel_reasons`, `run/http`, `run/net`, `examples/http`, the `shutdown_signal` test (`tests/signals`).

### 10.7 Runtime

- Tasks are stackful coroutines, assigned to worker threads by an M:N scheduler. The number of tasks running Gojo code at the same time (`maxProcs`) defaults to the number of CPU cores, and can be changed with the environment variable `GOJO_MAXPROCS` or with `task.setMaxProcs(n)`. Channels, locks, `time.sleep` and network operations suspend only the current task.
- **File I/O**: an `os.File` read or write of a pipe, terminal or socket that has to wait parks the task on the poller, as a network operation does: it takes no thread, and cancelling the task cancels it. On Linux, a read or write of a regular file first tries without waiting (`RWF_NOWAIT`); one that has to wait for the disk goes through an io_uring, where the task parks the same way (and cancelling it cancels the operation). Without io_uring (kernels before 5.6, seccomp profiles that block it such as Docker's default, `GOJO_IO_URING=0`), on filesystems without `RWF_NOWAIT`, and on macOS, regular-file I/O blocks its thread, which gives its CPU slot away first (below).
- **Preemption**: when other tasks are waiting, a task that has run for about 10 ms in a row is asked to give up the CPU. It responds at its next checkpoint: the entry of a function that may recurse, each iteration of a loop that contains nested loops or calls, and every 65536 iterations of a purely computational counting loop. So computation that doesn't block cannot starve other tasks either. Other purely computational loops (`while cond {}`) don't check; the enclosing loop takes care of it. If such a loop still doesn't respond, it is treated as a C call after 20 ms. What counts is the CPU time the thread actually used: when the machine is overloaded and the thread gets no CPU, it is not treated as unresponsive, so no extra threads are started to add to the overload.
- **C calls**: while an `extern` function, a blocking file operation, or a DNS lookup runs, its thread gives its CPU slot to another thread (file I/O and DNS give it up immediately; other C calls give it up after about 1 ms). So the number of threads is `maxProcs` plus the number of tasks inside C calls. A long loop without checkpoints (for example a large sort inside the runtime) is also treated as a C call after 20 ms.
- **Stacks**: each task's stack stays at a fixed address (tasks can borrow data on other tasks' stacks). The default size is 2 MB (`GOJO_GOROUTINE_STACK`); `main` has 1 GB. Only the pages that are used take memory. Stack overflow is caught by a guard page and ends with a panic. A task parked for more than 200 ms, among more than 256, whose scopes have no running tasks (that might be reading its stack), is **hibernated**: the used part of its stack is copied out and its pages are given back, and both come back at the same address before it runs again or before a channel writes into it. A task whose stack C was given a pointer into (with `.ptr()`) is never hibernated, since C may keep the pointer (11). `GOJO_HIBERNATE=0` turns hibernation off, `GOJO_HIBERNATE=n` waits `n` ms. It is off under sanitizers.
- `task.stats()` returns the number of live tasks, threads, `maxProcs` and hibernated tasks.
- **Diagnostics.** When every task is blocked and no timer or network wait can wake any of them, the program ends with a panic, `all tasks are blocked: deadlock`, followed by every task: where it was started, what it waits for (with where that channel, mutex or scope was made, and which task holds the mutex) and the locks it holds, and, when tasks wait for each other's locks, a line saying so. `task.dump()` prints the same list to stderr; SIGQUIT (`Ctrl-\`) prints it and ends the program. Debug builds also report, once per pair, a mutex taken while holding another one when somewhere else they are taken the other way round (`warning: possible deadlock`), and note a scope that has waited more than 10 s for its tasks, with what they wait for.
- A panic in any task terminates the process.

Tests: `run/sched`, `run/preempt`, `run/long_loops`, `run/ffi_block`, `run/file_io`, `panic/deadlock`, `panic/deadlock_locks`, the `stack_hibernation` test (`tests/hibernate`), `run/ffi_retained` and the `ffi_pinned_stacks` test (`tests/ffi_pin`), the `stdin_pipe_cancel` test (`tests/stdin`); on Linux, `scripts/linux-test.sh` runs them with and without io_uring.

### 10.8 Simulation

- `gojo test` runs each test on a **simulated scheduler**: one thread; which runnable task goes next, and whether a task gives way at a channel operation, lock, spawn, join or atomic operation (and, now and then, at a preemption checkpoint), comes from a seed. Tasks affect each other only through these operations (there are no data races), so the seed decides the interleaving. The clock is virtual: monotonic time starts at 0 and wall-clock time at 2000-01-01 UTC; it advances 1 µs per scheduling step, and when every task waits it jumps to the next timer. While a task waits for the network the clock follows real time, and network events arrive when they do, so programs that use the network aren't fully repeatable.
- The seed of a test comes from its name, so a test does the same thing every run. `--seed n` uses seed `n`; `--explore n` runs each test under `n` seeds, stops at the first failure and prints its seed (`FAIL name (seed 966961; rerun it with gojo test --seed 966961)`); `--real` uses the real scheduler. `gojo run --seed n` (or `GOJO_SEED=n`) runs a program the same way.

Tests: the `simulated_scheduler` test (`tests/testblocks/sched.gojo`).

### 10.9 Networking

- `net.listen("host:port")` (`":0"` picks a free port) returns a `Listener`; `accept()` returns a `Conn`; `net.dial(addr)` connects.
- `Conn`: `read(&buf, max)` appends up to `max` bytes to `buf` and returns the number of bytes read; 0 means the peer has closed. `write(data)` and `writeText(s)` write all the data. `readAll()` reads until the peer closes. `closeWrite()` closes the write direction. `Listener` and `Conn` are closed when they go out of scope.
- Errors are `io.Error` (`net.Error` is an alias); for a cancelled call, `isCancelled()` is true. `Conn` satisfies `io.Reader` and `io.Writer`.

Tests: `run/net`, `run/http`, `examples/http`.

## 11. unsafe and FFI

A program whose packages contain no `unsafe` code has no undefined behaviour (as long as the compiler and the standard library are correct): no null or dangling pointers, no data races, no reads of uninitialized memory, no out-of-bounds accesses. `unsafe` is where code takes over checks the compiler can't do, and says so where it does.

- **What needs `unsafe { }`**, and nothing else does: dereferencing a raw pointer `*T` (`*p`, `p.field`, reading or writing), calling a function in an `extern` block, and calling an `unsafe fn`. Raw pointers can be made (`s.ptr()` of a `str` or slice, from C), compared, copied and stored in structs in safe code. Inside an `unsafe` block every other rule still holds: the borrow checker, overflow and bounds checks, `send`.
- An `unsafe` block with nothing in it that needs one is an error, so every block points at real unchecked code.
- **`unsafe fn`** (`unsafe fn f()`, `pub unsafe fn`, and as a method or associated function, also in `extend`) declares a function whose callers must uphold conditions the compiler can't check; say which in a comment above it. Every call needs an `unsafe` block, including `go f()`, `defer f()`, `T(args)` for an unsafe `new`, and calls inside closures and string interpolation. The body of an `unsafe fn` is not an `unsafe` block: unsafe operations inside it need their own, as in Rust 2024.
- Nothing calls an `unsafe fn` or an `extern` function without an `unsafe` block:
  - they can't be used as function values (`let f = abs`, `[free]`, passing them to `task.map`); wrap the call in a closure that upholds the conditions instead: `(x) => unsafe { abs(x) }`;
  - an `unsafe` method can't satisfy an interface or a bound;
  - methods the compiler calls itself can't be `unsafe`: `equal`, `compare`, `hash`, `clone`, `drop`, `toString`, `message`, and the iteration methods `cursor`, `next`, `nextMut`, `each`, `eachMut`;
  - interface methods, `main`, functions exported to C (`extern "C" fn`) and functions in an `extern` block (already unsafe) can't be marked `unsafe`.
- `extern "C" { ... }` (with no library) calls libc functions as ordinary direct calls. `extern "C" static "m" { ... }` links that library. `extern "C" lib "libz" { ... }` loads the library at run time with `dlopen`, so neither the library nor its headers are needed at build time (statically linked binaries cannot use `lib`). None of the three needs C headers: the compiler generates the calls from the Gojo declarations.
- `extern "C" fn name(...) { ... }` is a Gojo function exported under a C symbol name. Calling it from Gojo does not need `unsafe`.
- Parameters and return values of `extern "C"` functions can only be numbers, `bool`, raw pointers and structs made only of these. `String`, `str`, `Vec`, `?T`, function values and the like have no C representation, and declaring them is a compile error. Gojo functions cannot yet be passed to C as callbacks.
- A raw pointer makes a type fail `send`; with `unsafe send struct Handle { ... }` the author vouches that it can cross threads. That covers the type's own fields: a generic `unsafe send` type is `send` only when its type arguments are (`Wrap[Rc[int]]` is not).
- **Pointers into task stacks.** C may keep a pointer it is given and use it after the call returns, while the task that owns the memory is parked. A task's stack can be put away while it waits (10.7), so `.ptr()` pins the task whose stack the pointer points into: that stack is never put away, and the pointer keeps reaching the live memory. Nothing else is needed for this.
- **Accounting and policy.** `gojo unsafe` lists every `unsafe` block, `unsafe fn`, `unsafe send` type and `extern` block of the program, package by package, with locations and line counts (`--json` for tools). A dependency with any of them builds only when the `[unsafe]` table of the manifest being built allows it by name; `forbid = true` there keeps the package's own code free of them (2.1).

Tests: `run/ffi`, `run/unsafe_decls`, `run/unsafe_fn`, `run/ffi_block`, `run/ffi_retained`, `run/ptr_alias`, `errors/unsafe_extern`, `errors/unsafe_fn`, `errors/unsafe_fn_decls`, `errors/unsafe_fn_protocol`, `errors/unsafe_main`, `errors/unsafe_extern_fn`, `errors/ffi_types`, `errors/send_ptr`, `unsafe/w9_*`, the `ffi_pinned_stacks` test (`tests/ffi_pin`), `packages/unsafe_*`.

### 11.1 Undefined behaviour

`unsafe` code must never do any of the following, and C code it calls must not either. When it does, the program has no meaning at all: anything may happen, not just a crash at that line.

1. **Bad pointers.** Dereferencing a pointer that is null, dangling (into memory that was freed, or into a value that was dropped, moved or went out of scope), misaligned, or outside the allocation it was made from. A pointer from `.ptr()` stays valid only as long as a view of the same data would: for the elements of a `Vec`, until the `Vec` is next changed (a `push` may move them); for a `String`, until it is changed or dropped; for a temporary, until the end of the statement. (A variable whose `.ptr()` is taken is dropped where its scope ends, never earlier: 5.3.)
2. **Writing where Gojo doesn't expect it.** Writing through a raw pointer into Gojo memory unless safe code could write the same memory at that point: the value is held in a `var` or an `inout` parameter, nothing else borrows it, and it isn't the bytes of a string (string literals are in read-only memory). Compiled code may assume that something borrowed read-only doesn't change while the borrow lasts.
3. **Data races.** Two threads accessing the same memory at the same time, at least one of them writing, without a lock or atomic operation in between: through raw pointers, from C threads, or through an `unsafe send` type that isn't actually safe to share.
4. **Invalid values.** Making a value that isn't one of its type's values: a `bool` other than 0 or 1, an enum with an unknown variant, a string that isn't UTF-8, an optional, `Box`, `Rc`, `Vec`, `Map` or interface value whose insides were written through a pointer. Memory may be read through a pointer of another type (a C buffer as `*u8` and as `*u32`) as long as each value read is valid for its type: numbers, raw pointers and structs of these.
5. **Reading memory that was never written** (fresh `malloc` memory, for example).
6. **Breaking ownership.** Freeing, reallocating or dropping memory a Gojo value owns (C must not `free` a `String`'s bytes), dropping or using a value after Gojo has dropped it, or freeing C memory with something other than what allocated it.
7. **Wrong declarations.** Calling an `extern` function whose Gojo declaration doesn't match its C definition: the number or the types of the parameters, or the return type. Variadic C functions (`printf`) can't be declared: Gojo declarations aren't variadic, and on some platforms (Apple's arm64) variadic arguments are passed differently.
8. **Jumping over Gojo code.** `longjmp`, C++ exceptions or any other unwinding through Gojo frames. A C function may end the process (`exit`, `abort`); the values Gojo owns are then not dropped, which is a leak, not undefined behaviour.
9. **Calling Gojo from foreign threads.** C may call an exported `extern "C" fn` only on a thread that is running Gojo code, that is, from inside an `extern` call that Gojo made.
10. **Breaking an `unsafe fn`'s conditions**, whatever its comment says they are.

Not undefined behaviour, even in `unsafe` code: leaking memory, deadlocks, integer overflow, out-of-bounds indexing and failed `unwrap`s (these still panic), and a panic in an `extern "C" fn` called from C (it aborts the process).

**The C the compiler generates** is compiled, along with the runtime, with `-fwrapv` (signed arithmetic wraps; Gojo checks overflow itself) and `-fno-delete-null-pointer-checks` (a null check is never removed because the pointer was used before). Strict aliasing stays on, so the C compiler may assume that memory is only accessed through the type it holds, and the generated code and the runtime keep to that: the allocator's free lists link blocks with `memcpy`, interface vtables embed their common part as a real member, and a raw pointer is dereferenced through a `may_alias` version of its type, so that `unsafe` code may view the same memory as different types (item 4 above; `run/ptr_alias`). The `type_sanitizer` test runs every program in `tests/run` under clang's TypeSanitizer to check this.

## 12. Standard library

Except for the prelude, every module needs a `use`. Standard library modules can import each other; the compiler loads them in dependency order.

| Module | Contents |
| --- | --- |
| prelude | `print`/`println` (with spaces between arguments), `assert`, `panic`, `drop`, `swap`, `replace`, `take`, `min`/`max`; methods of `?T`, `T ! E`, strings, slices, `Vec`, `Map`; `Set[T]` (insertion order, `setOf`, `union`/`intersection`/`difference`); `Arena[T]` |
| `fmt` | `sprint`; `format(template, args...)` and `printf`: `{}`, `{:8}`, `{:<8}` `{:>8}` `{:^8}`, `{:*^8}` (fill character), `{:08}`, `{:.2}`, `{:x}` `{:X}` `{:o}` `{:b}`, `{{` `}}`. The template must be a string literal; the number of placeholders, and whether each format matches its argument's type, are checked at compile time. Numbers are right-aligned by default, everything else left-aligned |
| `io` | `Error`, `ErrorKind`, `fromCode`, `newError`; interfaces `Reader` (`read(inout self, buf: inout Vec[byte], max: int) -> int ! Error`, 0 means end) and `Writer` (`write(inout self, data: []byte) ! Error`); `readAll`, `readText`, `copy`, `writeText`; in-memory `BytesReader` (`textReader`, `bytesReader`) and `Buffer` |
| `bufio` | `reader(r)`: `readLine()` (strips `\n` / `\r\n`, `none` at the end), `readUntil(byte)`, `readByte()`, `readAll()`; it is itself an `io.Reader`. `writer(w)`: `write`, `writeText`, `flush`; flushes automatically when it goes out of scope (errors are lost; call `flush` explicitly when they matter) |
| `os` | `args`, `exit`; `File` (`open`, `create`, `append`, `stdin`/`stdout`/`stderr`; `read`, `write`, `writeText`, `readAll`, `readText`, `flush`; closed when it goes out of scope; satisfies `io.Reader`/`io.Writer`); `readFile`, `readText`, `writeFile`, `writeText`; `stat`, `lstat` (`Info`: `kind`, `size`, `modified`, `mode`), `exists`, `readDir` (sorted names), `mkdir`, `mkdirAll`, `remove`, `removeAll` (does not follow symbolic links), `rename`, `cwd`, `chdir`, `tempDir`, `joinPath`, `dirName`, `baseName`; `getenv`, `setenv`, `unsetenv`; `run(prog, args)`, `runWithInput` (capture output and return `Output{status, stdout, stderr}`; `status` is the exit code or 128 + the signal), `call` (shares this process's standard streams); `command(prog, args)` → `Command` (`arg`, `env`, `dir`, `stdin`/`stdout`/`stderr` as `Stdio`) and `start` → `Process` (`wait`, cancellable; `tryWait`, `signal`, `kill`); `notify([signals])` → `Signals` (`next()`); `walk(root, f)`; `pid`, `environ`. During file and process operations the thread gives up the CPU and other tasks keep running. Writes to standard output and `print` share one buffer, so their order is consistent |
| `time` | monotonic clock `now`, `since`, `sleep`; wall-clock time `unixNanos`, `unix`; `Date` (`year`…`nanosecond`, `offset`; `unixNanos()`, `weekday()`, `yearDay()`, `iso()` (RFC 3339), `format("%Y-%m-%d %H:%M:%S")` and other strftime-style directives); `utc`, `local`, `at(ns, offset)`, `date(...)`, `parseIso` (`! ParseError`); `formatDuration` (`1.5s`, `250ms`, `2h5m0s`) and its inverse `parseDuration`; `ticker(period)` (`wait`, `advance`, `deadline`, `reset`; missed ticks are skipped); `dayName`, `monthName`. Nanoseconds are held in an `int`, which covers about 292 years either side of 1970 |
| `json` | `Value` (`Null`, `Bool`, `Int`, `Num`, `Str`, `Arr`, `Obj`; objects keep the insertion order of keys, and integers that fit in an `int` stay exact); `parse(text) Value ! json.Error` (with line and column; at most `maxDepth` = 500 levels of nesting); projections `get(key)`, `at(i)`; `str()`, `int()`, `num()`, `boolean()`, `isNull()`, `isObject()`, `isArray()`, `len()`, `keys()`, `set`, `push`; `encode()`, `pretty()`; constructors `object()`, `array()`, `string`, `integer`, `number`, `boolean`, `null`; `quoted(s)`; for your own types `encode[T]`, `encodePretty`, `decode[T](text) -> T ! json.Error` (errors give the path, as in `(in .shapes[0].Circle.r)`), `toValue`, `fromValue`: structs are objects, enums without data are strings, other enums `{"Variant":{...}}`, tuples, sets and arrays are arrays; a type that can't be converted is a compile error at the call |
| `net` | TCP (see 10.9); UDP: `listenUdp`, `dialUdp`, `UdpSocket` (`sendTo`/`recvFrom`, `send`/`recv`); `lookup(host)` (IPv4 first) |
| `http` | HTTP/1.1 server and client, over TCP or TLS (`serveTls`, `https://` URLs). `Handler` interface (`fn serve(self, req: Request) -> Response`); `serve(ln, h, Options{deadline, maxBody, keepAlive, idleTimeout})`, `serveCounting` (also gives `Stats`): each connection is a task in the server's task group; each request has a deadline (504 when it passes); a client disconnect cancels the request; cancelling the task that runs `serve` cancels in-flight requests. `Request`: `method`, `path`, `rawQuery`, `headers`, `body`, `header(name)` (case-insensitive), `param(name)` (a decoded query parameter), `segments()`, `text()`, `route(method, "/todos/:id")` (`Params` with `get`, `int`; `*rest` takes the rest of the path); request bodies support `Content-Length` and chunked; over `maxBody` returns 413, headers over 64 KB return 431. `Response`: `text(status, s)`, `json(status, s)`, `setHeader`. Client: `get`, `post`, `request(method, url, headers, body)`. `encode`/`decode` do percent-encoding. Connections are kept alive on both sides (the client reuses them per server for a few seconds); a request dropped at shutdown gets 503 |
| `task`, `strings`, `math` | see section 10; `strings.Builder`; math functions |
| `rand` | `Rng` (seeded, or from the OS): `u64`, `range`, `below`, `float`, `bool`, `shuffle`, `choice`; `secureBytes`, `secureU64` |
| `path` | `join`, `clean`, `base`, `dir`, `ext`, `stem`, `withExt`, `isAbsolute`, `components`, `abs`, `relative`, `match` (globs) |
| `hex`, `base64`, `url`, `csv`, `binary` | encodings: hex and base64 (standard and URL-safe); `url.parse` (`Url`), `Query`, escaping; CSV `parse`, a streaming `Reader`, `format`; `binary.big`/`binary.little` (`u16`…`f64`, `put…`, `append…`) and varints |
| `collections` | `Deque`, `Heap`, `SortedMap`, `SortedSet`, `BitSet` |
| `crypto`, `crc32`, `tls` | SHA-2, SHA-1, MD5 (`hash`, streaming `Digest`), `hmac`, `pbkdf2`, constant-time `equal`; CRC-32; TLS clients and servers (`dial`, `dialWith`, `listen`) through the system's OpenSSL, loaded on first use |
| `regex`, `flag`, `log` | RE2-syntax regular expressions, matched in linear time; command-line parsing with help, subcommands and suggestions; leveled logging as text or JSON, with fields |

The full API of each module is in [`docs/std`](std/README.md) (generated by `gojo doc`, section 17).

Examples: [`examples/todo`](../examples/todo) (a service with a JSON API that stores its data as a JSON file) and [`examples/grep`](../examples/grep) (a command-line tool; the tests run each command line in `runs.txt` and compare the output and exit code). Both use only the standard library and no `unsafe`.

Tests: `run/files`, `run/buffered`, `run/format`, `run/dates`, `run/json`, `run/json_types`, `run/http`, `run/net`, `run/tls`, `run/processes`, `run/sets`, `run/collections`, `run/collections_more`, `run/encodings`, `run/binary`, `run/hashing`, `run/regex`, `run/flags`, `run/logging`, `run/math`, `run/iface_modes`, `errors/format`, `errors/json_types`, `examples/todo`, `examples/grep`, `examples/http`.

## 13. Test blocks

- `test "name" { ... }` can be written in any file. `gojo test` runs them; `gojo build`/`run` ignore them.
- Each test runs in a separate process: a panic in one test fails only that test. When any test fails, the exit code is 1.

Tests: `testblocks/blocks`.

## 14. Execution model

- The compiler generates C and compiles it with `cc` or `zig cc`. Debug and release (`-O2`) differ in optimization level and debug info: overflow checks and index checks stay in both.
- An index or slice the compiler proves in range has no check, in both builds (`src/codegen/bce.rs`). The proofs use loop counters (`for i in 0..v.len`, `for i, _ in v`), immutable locals (`let n = v.len`), `var` integers that only grow from a non-negative start (`var i = 0` with `i += 1`), conditions where they hold (`if i < v.len { v[i] }`, the rest of a block after `if i >= v.len { return }`, the body of `while i < v.len`, the right side of `i < v.len && v[i] != 0`, after `assert(i < v.len)`; `if parts.len() >= 7 { parts[5] }`, `if !xs.isEmpty() { xs[0] }`), constants, integer types and `&`/`%` masks. A bound relative to `v.len` counts only where nothing can change `v`'s length, and what a condition says about a place ends at the first statement that may change it. `gojo build --verify-bce` keeps the removed checks and reports a failed one as a compiler bug; the tests (`run_programs`, `runtime_panics`) and `gojo-fuzz` build that way, and `bounds_check_elimination` checks which indexes of `run/bce` are proven.
- Debug builds have debug info (`-g`), and the generated C uses `#line` to point at lines of the Gojo source, so lldb/gdb can set breakpoints like `main.gojo:12`, and stack traces and sanitizer reports show Gojo lines. On macOS the debug info is in a `.dSYM` next to the binary; `gojo build -o prog` also writes `prog.dSYM`. `gojo emit-c` and `--keep-c` output C without `#line`, which makes it easier to debug the compiler itself.
- Generics are monomorphized. Functions with at most 60 IR nodes get an `inline` hint.
- `Box`, `Rc`/`Arc` and interface boxes are allocated from thread-local free lists in size classes; `Vec`/`String` buffers use `malloc`.
- Tasks are stackful coroutines with M:N scheduling; see 10.7.
- `Map` keeps insertion order. Floats print in the shortest round-trip representation, without scientific notation when the exponent is in [-4, 21).
- If a function that returns a value runs off its end (type checking proves this cannot happen), the program panics with an internal error instead of returning an undefined value.
- The program and the runtime are compiled with `-fwrapv -fno-delete-null-pointer-checks`, the C semantics the generated code is written for, and with strict aliasing (11.1).
- Linux targets are compiled with `-fstack-clash-protection`: stack frames larger than the guard page (such as a very large local array) are touched page by page, so a stack overflow always lands on the guard page and panics, and never writes past it into other memory. Compilers on Darwin do this by default.
- The C compiler runs with a 64 MB stack limit (clang also gets `-fbracket-depth`), so the C generated for programs nested up to the limits also compiles.

## 15. Limits and fuzzing

**Limits** (exceeding them is a compile error, never a compiler crash):

- Nesting depth of expressions, blocks, types and patterns: at most 1000 parser levels (`parser::MAX_NEST`). Each level of source nesting takes a few parser levels, so the practical limit is a few hundred parentheses or a few hundred `else if`s. Long chains like `a + b + c ...` also count toward the depth (one level per operator). The compiler runs on a 1 GB stack (only the used part takes memory); at this depth no phase overflows the stack, and no analysis grows exponentially with depth.
- Array lengths are at most 2³² − 1; any value (after generics are instantiated) is at most 2⁴⁰ bytes. Use `Vec` for larger data.
- Integer literals, integer patterns and integer constant arithmetic that exceed the range of every integer type are errors. Float literals that exceed the range of `f64` (or of `f32` when that is the expected type) are errors rather than becoming infinity.
- Types after generic instantiation nest at most 400 levels. A generic function or type that instantiates itself with an ever larger type each time (`grow[T]` calling `grow[Vec[T]]`, `Node[T]` with a `Node[Vec[T]]` field) never terminates; the compiler reports an error when it reaches this depth.

**`gojo-fuzz`** (`cargo build --profile fuzz` produces `target/fuzz/gojo-fuzz`; the `fuzz` profile is an optimized build with debug assertions and overflow checks):

- `gojo-fuzz [--jobs N] [--time SECS]`: uses every Gojo program in the repository as the corpus and mutates them by token (delete, duplicate, replace, splice in pieces of other programs, rename, swap in boundary integers), running the front end and code generation in process. Compiler panics, crashes (stack overflows) and timeouts are recorded. Accepted mutated programs are compiled to C and run under AddressSanitizer / UBSan (except programs with `unsafe`, C calls, files or the network): if the generated C fails to compile, or a sanitizer reports an error at run time, that is a compiler bug or a hole in the safety checks. Mutations that produce a new error message or a new program are added to the corpus.
- `gojo-fuzz diff`: generates random programs that are well-typed, always terminate and have deterministic results, and builds and runs each with debug, release and the two sanitizers. All four must produce exactly the same output and end the same way. The programs also contain integer expressions (wrapping arithmetic, shifts, conversions, division) whose results the generator computes itself under Gojo semantics, and the program checks that the computed values agree: this also finds problems that are equally wrong in all four builds (literal types, conversions, integer promotion in the generated C). When the compiler rejects a generated program, that is recorded too (a generator bug, or a false positive in the compiler).
- `gojo-fuzz borrow`: generates small programs that perform random operations on a few owned values and their views (create, reassign, modify and move the owners, `defer`, nested blocks, loops, closures, locks, `Map` projections, and task handles and `Scope` values kept where they could outlive their scope), without guaranteeing they are valid. Most are rejected; each accepted one runs under AddressSanitizer, and an error there means the borrow checker let an unsafe program through.
- `gojo-fuzz reduce <file>`: deletes tokens and lines from the input while keeping the same failure signature, to get a minimal reproduction.
- `gojo-fuzz fmt [N]`: runs the formatter on generated programs, the corpus and mutated programs. For every input that parses, formatting must keep the tokens unchanged, the result must still parse, the type-checking result must be unchanged, formatting again must change nothing, and the result must not depend on the input's original indentation and trailing whitespace.
- `gojo-fuzz toml [N]`: mutates the `gojo.toml` files in `tests/packages`. Reading a manifest must never panic, and the location of every error must be displayable.
- `gojo-fuzz msgs N`: counts the most common first error messages over N mutated programs and gives examples, for reviewing the errors users see.

Findings go in `fuzz-out/findings/`; reduced regression cases go in `tests/fuzz/`.

## 16. Formatting

`gojo fmt [--check] [path...]` formats `.gojo` files in place (directories recursively, skipping hidden directories). `--check` only lists the files that need formatting, and exits with code 1 if there are any. Only files that parse are formatted: a file with a syntax error is reported and left unchanged. Rules:

- Indentation is 4 spaces, determined by bracket nesting: brackets opened on a line (however many) indent the next line by one level; after a line that ends with a binary operator, the next line is indented one more level.
- Existing line breaks are kept: lines are never joined or split. Runs of blank lines collapse to one; blank lines at the start of the file and at the start and end of blocks are removed; trailing whitespace is removed; the file ends with one newline.
- One space after binary operators, `=`, `=>`, `,` and `:`; no spaces around `..`, `..=`, `.`, unary operators, or the brackets of calls and indexing. Block braces have spaces inside (`if c { x }`); struct and map literals don't (`P{x: 1}`).
- One space before a trailing comment. Comments are kept as written; only the indentation of their line changes. The inside of multi-line block comments is unchanged.
- The layout never depends on the spaces in the source: types follow `:` or `->`, so whether `*`, `-` and `[` are prefixes, operators or indexing is clear from the tokens.
- The formatted result has exactly the same tokens as the original file (otherwise it reports an internal error and does not write the file). All Gojo files in the repository are formatted (except the regression cases in `tests/fuzz`), and a test checks this.

## 17. Documentation

`gojo doc` generates API documentation in Markdown from source: `gojo doc` is the index of the standard library, `gojo doc os` is one standard library module, `gojo doc path` is a package (directory) or file, and `gojo doc --out dir` writes the whole standard library into a directory (the [`docs/std`](std/README.md) in the repository is generated this way, and a test checks that it is up to date).

- Only `pub` declarations are listed: constants, types (`pub` fields and variants; private fields are only noted as present), functions, and the `pub` methods of types. Methods added to types from elsewhere (`extend str`) are listed separately.
- A declaration's documentation is the comment lines directly above it (with no blank line in between). A function or constant without its own comment that directly follows the previous declaration shares that one's comment (`floor`, `ceil` and `round` share one description).
- A comment block at the start of a file, followed by a blank line or `use`, is the package description.
- Lines in a comment indented by two or more spaces are code examples.
