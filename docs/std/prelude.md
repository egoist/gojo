# prelude

The prelude: available in every Gojo file without an import. It holds the
methods of the built-in types (`str`, `String`, slices, `Vec`, `Map`, `?T`,
`T ! E`, `rc`, `Mutex`, channels...), the `error` interface, `panic` and
`assert`, and a few general types: `Set`, `Arena`, `Hasher`, `Span`.
Functions without a body are implemented by the compiler or runtime.

**Types:** [`enum Ordering`](#enum-ordering), [`enum Flow`](#enum-flow), [`interface error`](#interface-error), [`interface Number`](#interface-number), [`interface Integer`](#interface-integer), [`struct Atomic`](#struct-atomic), [`struct RwLock`](#struct-rwlock), [`struct Chan`](#struct-chan), [`struct Hasher`](#struct-hasher), [`struct Id`](#struct-id), [`struct Arena`](#struct-arena), [`struct Span`](#struct-span), [`struct Set`](#struct-set), [`struct Tuple2`](#struct-tuple2), [`struct Tuple3`](#struct-tuple3), [`struct Tuple4`](#struct-tuple4), [`struct Tuple5`](#struct-tuple5), [`struct Tuple6`](#struct-tuple6)

**Functions:** [`panic`](#panic), [`assert`](#assert), [`assertEq`](#asserteq), [`unreachable`](#unreachable), [`drop`](#drop), [`swap`](#swap), [`replace`](#replace), [`take`](#take), [`min`](#min), [`max`](#max), [`setOf`](#setof)

**Methods on other types:** [`extend ?T`](#extend-t), [`extend T ! E`](#extend-t--e), [`extend str`](#extend-str), [`extend String`](#extend-string), [`extend []T`](#extend-t-1), [`extend []String`](#extend-string-1), [`extend Vec[T]`](#extend-vect), [`extend Map[K, V]`](#extend-mapk-v), [`extend Rc[T]`](#extend-rct), [`extend Weak[T]`](#extend-weakt), [`extend Arc[T]`](#extend-arct), [`extend Cell[T]`](#extend-cellt), [`extend Mutex[T]`](#extend-mutext), [`extend Sender[T]`](#extend-sendert), [`extend Receiver[T]`](#extend-receivert), [`extend Task[T]`](#extend-taskt), [`extend Iso[T]`](#extend-isot), [`extend Scope`](#extend-scope), [`extend MutexRef[T]`](#extend-mutexreft)

## Types

### enum Ordering

```gojo
pub enum Ordering { Less, Equal, Greater }
```

The result of comparing two values: `a.compare(b)`.

### enum Flow

```gojo
pub enum Flow { Continue, Break }
```

Control signal for push-style iteration.

### interface error

```gojo
pub interface error {
    fn message(self) -> String
}
```

The built-in error interface, for application-level error handling.

### interface Number

```gojo
pub interface Number {
    ~int | ~i8 | ~i16 | ~i32 | ~u8 | ~u16 | ~u32 | ~u64 | ~f32 | ~f64
}
```

Numeric types, for generic arithmetic.

### interface Integer

```gojo
pub interface Integer {
    ~int | ~i8 | ~i16 | ~i32 | ~u8 | ~u16 | ~u32 | ~u64
}
```

Integer types, for `Atomic`.

### struct Atomic

```gojo
pub unsafe send struct Atomic[T: Integer] {
    // and private fields
}
```

An integer that tasks share and change without a lock: each operation is
atomic and sequentially consistent. Tasks borrow it and call its methods at
the same time:

```gojo
let hits = Atomic(0)
par for r in requests { if r.ok { _ = hits.add(1) } }
```

#### Atomic.new

```gojo
pub fn new(x: T) -> Atomic[T]
```

#### Atomic.load

```gojo
pub fn load(self) -> T
```

The value now.

#### Atomic.store

```gojo
pub fn store(self, x: T)
```

Replaces the value.

#### Atomic.add

```gojo
pub fn add(self, d: T) -> T
```

Adds `d`, wrapping around on overflow, and gives the new value.

#### Atomic.sub

```gojo
pub fn sub(self, d: T) -> T
```

Subtracts `d`, wrapping around on overflow, and gives the new value.

#### Atomic.swap

```gojo
pub fn swap(self, x: T) -> T
```

Replaces the value and gives the old one.

#### Atomic.compareSwap

```gojo
pub fn compareSwap(self, old: T, new: T) -> bool
```

Replaces the value with `new` if it is `old`; true if it did.

#### Atomic.clone

```gojo
pub fn clone(self) -> Atomic[T]
```

A new `Atomic` with the value now.

### struct RwLock

```gojo
pub unsafe send struct RwLock[T: send] {
    // and private fields
}
```

A lock that many readers may hold at once, or one writer:

```gojo
let cache = RwLock(Map[String, String]{})
with c = cache.read() { ... }     // shared: other readers too
with c = cache.write() { ... }    // exclusive
```

Like `Mutex.lock`, `read` and `write` are projections: the reference can't
leave the block. A writer waiting keeps new readers out, so writers don't
starve.

#### RwLock.new

```gojo
pub fn new(x: sink T) -> RwLock[T]
```

#### RwLock.read

```gojo
pub fn read(self) yield &T
```

Shared access.

#### RwLock.write

```gojo
pub fn write(self) yield inout T
```

Exclusive access.

#### RwLock.toString

```gojo
pub fn toString(self) -> String
```

"RwLock" (the value can't be read without the lock).

### struct Chan

```gojo
pub struct Chan[T] {
    // The sending end; `.clone()` it for more senders.
    pub tx: Sender[T]
    // The receiving end; `.clone()` it for more receivers.
    pub rx: Receiver[T]
}
```

`Chan[T](n)` makes a channel with room for `n` values (`Chan[T]()`: none, so
a send waits for a receiver). Its two ends are handed out separately:

```gojo
let ch = Chan[Job](16)
go produce(ch.tx)          // the channel closes when the last sender is dropped
for job in ch.rx { run(job) }
```

### struct Hasher

```gojo
pub struct Hasher {
    // and private fields
}
```

Computes the hash of values (FNV-1a over `u64` words), for writing a
custom `hash` method.

#### Hasher.writeU64

```gojo
pub fn writeU64(inout self, x: u64)
```

Mixes `x` into the hash.

#### Hasher.write

```gojo
pub fn write[T: hash](inout self, x: T)
```

Mixes in the hash of any hashable value.

### struct Id

```gojo
pub copy struct Id[T] {
    // and private fields
}
```

A handle to a value in an `Arena[T]`.

### struct Arena

```gojo
pub struct Arena[T] {
    // and private fields
}
```

Storage for graphs, trees and other structures whose nodes refer to each
other: nodes are values in the arena, references are `Id`s. An `Id` carries
a generation, so one whose value was removed finds nothing, rather than a
different value that took its slot.

```gojo
var a = Arena[Node]{}
let root = a.alloc(Node{name: "root"})
with n = a.getMut(root) { ... }
```

#### Arena.alloc

```gojo
pub fn alloc(inout self, v: sink T) -> Id[T]
```

Stores `v` and returns its `Id`.

#### Arena.get

```gojo
pub fn get(self, id: Id[T]) yield ?&T
```

The value for `id`, if it is still there.

#### Arena.getMut

```gojo
pub fn getMut(inout self, id: Id[T]) yield ?inout T
```

The value for `id`, to change in place.

#### Arena.remove

```gojo
pub fn remove(inout self, id: Id[T]) -> ?T
```

Removes and returns the value for `id`; its slot is reused, with a new
generation.

#### Arena.len

```gojo
pub fn len(self) -> int
```

The number of values stored.

### struct Span

```gojo
pub copy struct Span {
    pub lo: int
    pub hi: int
}
```

A range of byte offsets into a string. A zero-copy parser keeps the owned
input and `Span`s into it, and borrows views on demand: no reference is
ever stored in a struct.

#### Span.of

```gojo
pub fn of(self, s: str) -> str
```

The part of `s` the span covers.

#### Span.len

```gojo
pub fn len(self) -> int
```

The length in bytes.

### struct Set

```gojo
pub struct Set[T: hash] {
    // and private fields
}
```

A set of values, kept in the order they were first inserted.

```gojo
var seen = Set[String].new()
if seen.insert(name.clone()) { ... first time ... }
```

#### Set.new

```gojo
pub fn new() -> Set[T]
```

An empty set.

#### Set.insert

```gojo
pub fn insert(inout self, x: sink T) -> bool
```

Adds `x`; false if it was there already.

#### Set.remove

```gojo
pub fn remove(inout self, x: T) -> bool
```

Takes `x` out; false if it wasn't there.

#### Set.contains

```gojo
pub fn contains(self, x: T) -> bool
```

Whether `x` is in the set.

#### Set.len

```gojo
pub fn len(self) -> int
```

The number of values.

#### Set.isEmpty

```gojo
pub fn isEmpty(self) -> bool
```

Whether the set is empty.

#### Set.clear

```gojo
pub fn clear(inout self)
```

Removes every value.

#### Set.each

```gojo
pub fn each(self, body: fn(&T) -> Flow)
```

`for x in set`

#### Set.toVec

```gojo
pub fn toVec(self) -> Vec[T] where T: clone
```

Copies of the values, in insertion order.

#### Set.union

```gojo
pub fn union(self, other: Set[T]) -> Set[T] where T: clone
```

What is in either.

#### Set.intersection

```gojo
pub fn intersection(self, other: Set[T]) -> Set[T] where T: clone
```

What is in both.

#### Set.difference

```gojo
pub fn difference(self, other: Set[T]) -> Set[T] where T: clone
```

What is in this one but not in `other`.

### struct Tuple2

```gojo
pub struct Tuple2[A, B] {
    pub _0: A
    pub _1: B
}
```

Tuples: `(A, B)` is a `Tuple2[A, B]`, `(a, b)` builds one and `t.0` reads
its first element. They are copy when their elements are.

### struct Tuple3

```gojo
pub struct Tuple3[A, B, C] {
    pub _0: A
    pub _1: B
    pub _2: C
}
```

### struct Tuple4

```gojo
pub struct Tuple4[A, B, C, D] {
    pub _0: A
    pub _1: B
    pub _2: C
    pub _3: D
}
```

### struct Tuple5

```gojo
pub struct Tuple5[A, B, C, D, E] {
    pub _0: A
    pub _1: B
    pub _2: C
    pub _3: D
    pub _4: E
}
```

### struct Tuple6

```gojo
pub struct Tuple6[A, B, C, D, E, F] {
    pub _0: A
    pub _1: B
    pub _2: C
    pub _3: D
    pub _4: E
    pub _5: F
}
```

## Functions

### panic

```gojo
pub fn panic(msg: str) -> never
```

Ends the program with `msg` and the caller's location (exit code 2). Panics
cannot be caught; deferred code does not run.

### assert

```gojo
pub fn assert(cond: bool)
```

Panics unless `cond` holds.

### assertEq

```gojo
pub fn assertEq[T: comparable](a: T, b: T)
```

Panics unless `a == b`, showing both values.

### unreachable

```gojo
pub fn unreachable() -> never
```

Marks code that can't run; panics if it does.

### drop

```gojo
pub fn drop[T](x: sink T)
```

Explicitly end the lifetime of a value.

### swap

```gojo
pub fn swap[T](a: inout T, b: inout T)
```

Exchanges two places.

### replace

```gojo
pub fn replace[T](dst: inout T, value: sink T) -> T
```

Puts `value` into `dst` and returns what was there.

### take

```gojo
pub fn take[T: default](dst: inout T) -> T
```

Takes the value out of `dst`, leaving the type's default (`""`, `0`, `none`,
an empty `Vec`...).

### min

```gojo
pub fn min[T: copy + ordered](a: T, b: T) -> T
```

The smaller of two values (`a` when they are equal).

### max

```gojo
pub fn max[T: copy + ordered](a: T, b: T) -> T
```

The larger of two values (`a` when they are equal).

### setOf

```gojo
pub fn setOf[T: hash](xs: sink Vec[T]) -> Set[T]
```

A set of the values in `xs` (duplicates once, in the order first seen).

## Methods on other types

### extend ?T

#### ?T.isSome

```gojo
pub fn isSome(self) -> bool
```

Whether there is a value.

#### ?T.isNone

```gojo
pub fn isNone(self) -> bool
```

Whether there is no value.

#### ?T.unwrap

```gojo
pub fn unwrap(sink self) -> T
```

The value; panics on none. Prefer `??`, `if let` or `try` where none can
happen.

#### ?T.expect

```gojo
pub fn expect(sink self, msg: str) -> T
```

The value; panics with `msg` on none.

#### ?T.unwrapOr

```gojo
pub fn unwrapOr(sink self, d: sink T) -> T
```

The value, or `d` (which is evaluated either way; `x ?? d` evaluates `d` only
when needed).

#### ?T.map

```gojo
pub fn map[U](self, f: fn(T) -> U) -> ?U
```

`f` applied to the value, if there is one.

### extend T ! E

#### T ! E.isOk

```gojo
pub fn isOk(self) -> bool
```

Whether this is a value rather than an error.

#### T ! E.isErr

```gojo
pub fn isErr(self) -> bool
```

Whether this is an error.

#### T ! E.unwrap

```gojo
pub fn unwrap(sink self) -> T
```

The value; panics with the error. Prefer `try` or `catch`.

#### T ! E.expect

```gojo
pub fn expect(sink self, msg: str) -> T
```

The value; panics with `msg` and the error.

#### T ! E.unwrapOr

```gojo
pub fn unwrapOr(sink self, d: sink T) -> T
```

The value, or `d` on an error (which is dropped).

#### T ! E.ok

```gojo
pub fn ok(sink self) -> ?T
```

The value, or none on an error.

#### T ! E.err

```gojo
pub fn err(sink self) -> ?E
```

The error, or none.

### extend str

#### str.len

```gojo
pub fn len(self) -> int
```

The length in bytes (UTF-8). Indexes and slices count bytes too.

#### str.ptr

```gojo
pub fn ptr(self) -> *u8
```

raw pointer to the bytes, for FFI

#### str.isEmpty

```gojo
pub fn isEmpty(self) -> bool
```

Whether the length is 0.

#### str.contains

```gojo
pub fn contains(self, sub: str) -> bool
```

Whether `sub` occurs in the string (the empty string always does).

#### str.startsWith

```gojo
pub fn startsWith(self, prefix: str) -> bool
```

Whether the string begins with `prefix`.

#### str.endsWith

```gojo
pub fn endsWith(self, suffix: str) -> bool
```

Whether the string ends with `suffix`.

#### str.find

```gojo
pub fn find(self, sub: str) -> ?int
```

The byte index of the first occurrence of `sub`.

#### str.trim

```gojo
pub fn trim(self) -> str
```

The string without leading and trailing ASCII whitespace (space, tab,
newline, carriage return, vertical tab, form feed). A view: no copy.

#### str.trimStart

```gojo
pub fn trimStart(self) -> str
```

The string without leading ASCII whitespace.

#### str.trimEnd

```gojo
pub fn trimEnd(self) -> str
```

The string without trailing ASCII whitespace.

#### str.split

```gojo
pub fn split(self, sep: str) -> Vec[String]
```

The pieces between occurrences of `sep`: `"a,,b".split(",")` is
`["a", "", "b"]`, and `"".split(",")` is `[""]`. An empty `sep` splits into
characters.

#### str.lines

```gojo
pub fn lines(self) -> Vec[String]
```

The lines, without their `\n` or `\r\n`; no empty last line for a trailing
newline.

#### str.fields

```gojo
pub fn fields(self) -> Vec[String]
```

The words between runs of ASCII whitespace: `"  a b\tc ".fields()` is
`["a", "b", "c"]`.

#### str.toUpper

```gojo
pub fn toUpper(self) -> String
```

A copy with ASCII letters in upper case (other characters unchanged).

#### str.toLower

```gojo
pub fn toLower(self) -> String
```

A copy with ASCII letters in lower case (other characters unchanged).

#### str.toString

```gojo
pub fn toString(self) -> String
```

An owned copy.

#### str.repeat

```gojo
pub fn repeat(self, n: int) -> String
```

The string `n` times (empty for `n` <= 0).

#### str.replace

```gojo
pub fn replace(self, from: str, to: str) -> String
```

A copy with every occurrence of `from` replaced by `to` (unchanged when
`from` is empty).

#### str.bytes

```gojo
pub fn bytes(self) -> []byte
```

The bytes, as a view.

#### str.runes

```gojo
pub fn runes(self) -> Vec[rune]
```

The Unicode code points. A byte that isn't part of valid UTF-8 (strings
made with `String(bytes)` or read from files aren't checked) reads as
U+FFFD.

#### str.compare

```gojo
pub fn compare(self, other: str) -> Ordering
```

Byte-wise order, which is also code-point order.

#### str.parseInt

```gojo
pub fn parseInt(self) -> ?int
```

The decimal integer that is the whole string (an optional `+` or `-`, then
digits), or none: no spaces, no other characters, nothing out of range.

#### str.parseFloat

```gojo
pub fn parseFloat(self) -> ?f64
```

The floating-point number that is the whole string, or none. Accepts what C's
`strtod` does: `1.5`, `-2e10`, `inf`, `nan`, `0x1p3`.

### extend String

#### String.push

```gojo
pub fn push(inout self, t: str)
```

Appends `t`. (`s += t` does the same.)

#### String.pushRune

```gojo
pub fn pushRune(inout self, r: rune)
```

Appends the UTF-8 encoding of `r`; U+FFFD for a value that isn't a Unicode
scalar value (a surrogate, negative, or above U+10FFFF).

#### String.clear

```gojo
pub fn clear(inout self)
```

Makes the string empty, keeping its buffer.

#### String.asStr

```gojo
pub fn asStr(self) -> str
```

The string as a view.

### extend []T

#### []T.len

```gojo
pub fn len(self) -> int
```

The number of elements.

#### []T.reverse

```gojo
pub fn reverse(inout self)
```

Reverses the order in place. Only an `inout []T` (or a range of a
`var` Vec or array, `v[a..b]`) can be changed.

#### []T.swap

```gojo
pub fn swap(inout self, i: int, j: int)
```

Exchanges elements `i` and `j`; panics when either is out of range.

#### []T.sort

```gojo
pub fn sort(inout self) where T: ordered
```

Sorts in ascending order. Stable; O(n log n).

#### []T.sortBy

```gojo
pub fn sortBy(inout self, less: fn(T, T) -> bool)
```

Sorts by `less(a, b)`: true when `a` goes before `b`. Stable.

#### []T.ptr

```gojo
pub fn ptr(self) -> *T
```

raw pointer to the first element, for FFI

#### []T.isEmpty

```gojo
pub fn isEmpty(self) -> bool
```

Whether there are no elements.

#### []T.get

```gojo
pub fn get(self, i: int) yield ?&T
```

Element `i`, or none when `i` is out of range (indexing with `xs[i]`
panics instead).

#### []T.first

```gojo
pub fn first(self) yield ?&T
```

The first element, if any.

#### []T.last

```gojo
pub fn last(self) yield ?&T
```

The last element, if any.

#### []T.contains

```gojo
pub fn contains(self, x: T) -> bool where T: comparable
```

Whether some element equals `x`.

#### []T.indexOf

```gojo
pub fn indexOf(self, x: T) -> ?int where T: comparable
```

The index of the first element equal to `x`.

#### []T.map

```gojo
pub fn map[U](self, f: fn(T) -> U) -> Vec[U]
```

`f` applied to each element, in order.

#### []T.filter

```gojo
pub fn filter(self, keep: fn(T) -> bool) -> Vec[T] where T: clone
```

Copies of the elements for which `keep` is true.

#### []T.each

```gojo
pub fn each(self, f: fn(T))
```

Calls `f` on each element, in order.

#### []T.any

```gojo
pub fn any(self, f: fn(T) -> bool) -> bool
```

Whether `f` is true for some element (stops at the first).

#### []T.all

```gojo
pub fn all(self, f: fn(T) -> bool) -> bool
```

Whether `f` is true for every element (stops at the first false).

#### []T.count

```gojo
pub fn count(self, f: fn(T) -> bool) -> int
```

How many elements `f` is true for.

#### []T.fold

```gojo
pub fn fold[A](self, init: sink A, f: fn(sink A, T) -> A) -> A
```

Combines the elements from the left: `f(f(f(init, x0), x1), x2)`.

#### []T.sum

```gojo
pub fn sum(self) -> T where T: Number
```

The sum (0 for no elements). Integer overflow panics, as with `+`.

#### []T.min

```gojo
pub fn min(self) -> ?T where T: copy + ordered
```

The smallest element, or none for no elements.

#### []T.max

```gojo
pub fn max(self) -> ?T where T: copy + ordered
```

The largest element, or none for no elements.

#### []T.toVec

```gojo
pub fn toVec(self) -> Vec[T] where T: clone
```

An owned copy.

### extend []String

#### []String.join

```gojo
pub fn join(self, sep: str) -> String
```

The strings with `sep` between them: `["a", "b"].join(", ")` is `"a, b"`.

### extend Vec[T]

#### Vec[T].push

```gojo
pub fn push(inout self, x: sink T)
```

Appends `x`.

#### Vec[T].pop

```gojo
pub fn pop(inout self) -> ?T
```

Removes and returns the last element, or none when empty.

#### Vec[T].insert

```gojo
pub fn insert(inout self, i: int, x: sink T)
```

Inserts `x` at index `i` (0 <= `i` <= `len()`), moving the later elements
up; panics otherwise.

#### Vec[T].remove

```gojo
pub fn remove(inout self, i: int) -> T
```

Removes and returns element `i`, moving the later elements down; panics when
`i` is out of range.

#### Vec[T].swapRemove

```gojo
pub fn swapRemove(inout self, i: int) -> T
```

Removes and returns element `i`, putting the last element in its place:
O(1), but changes the order. Panics when `i` is out of range.

#### Vec[T].clear

```gojo
pub fn clear(inout self)
```

Removes (and drops) every element, keeping the buffer.

#### Vec[T].truncate

```gojo
pub fn truncate(inout self, n: int)
```

Keeps the first `n` elements and drops the rest; nothing happens when `n`
>= `len()`. Panics when `n` is negative.

#### Vec[T].extend

```gojo
pub fn extend(inout self, other: sink Vec[T])
```

Moves the elements of `other` to the end.

#### Vec[T].reverse

```gojo
pub fn reverse(inout self)
```

Reverses the order in place.

#### Vec[T].swap

```gojo
pub fn swap(inout self, i: int, j: int)
```

Exchanges elements `i` and `j`; panics when either is out of range.

#### Vec[T].sort

```gojo
pub fn sort(inout self) where T: ordered
```

Sorts in ascending order. The sort is stable (equal elements keep their
order) and takes O(n log n) time.

#### Vec[T].sortBy

```gojo
pub fn sortBy(inout self, less: fn(T, T) -> bool)
```

Sorts by `less(a, b)`: true when `a` goes before `b`. Stable.

#### Vec[T].reserve

```gojo
pub fn reserve(inout self, n: int)
```

Makes room for `n` more elements without reallocating; panics ("capacity
overflow") for an impossible size.

#### Vec[T].cap

```gojo
pub fn cap(self) -> int
```

The number of elements there is room for.

### extend Map[K, V]

#### Map[K, V].len

```gojo
pub fn len(self) -> int
```

The number of entries.

#### Map[K, V].isEmpty

```gojo
pub fn isEmpty(self) -> bool
```

Whether there are no entries.

#### Map[K, V].get

```gojo
pub fn get(self, k: K) yield ?&V
```

The value for `k`, if any. (`m[k]` panics when there is none.)

#### Map[K, V].getMut

```gojo
pub fn getMut(inout self, k: K) yield ?inout V
```

The value for `k`, to change in place.

#### Map[K, V].contains

```gojo
pub fn contains(self, k: K) -> bool
```

Whether there is an entry for `k`.

#### Map[K, V].insert

```gojo
pub fn insert(inout self, k: sink K, v: sink V) -> ?V
```

Sets the value for `k`, returning the old one. A new key goes at the end of
the iteration order; replacing a value keeps the key's place.

#### Map[K, V].remove

```gojo
pub fn remove(inout self, k: K) -> ?V
```

Removes the entry for `k`, returning its value. The others keep their order.

#### Map[K, V].entry

```gojo
pub fn entry(inout self, k: sink K) yield inout V where V: default
```

The value for `k`, inserting the default first if there is none:
`with n = counts.entry(word) { n += 1 }`.

#### Map[K, V].clear

```gojo
pub fn clear(inout self)
```

Removes every entry.

#### Map[K, V].keys

```gojo
pub fn keys(self) -> Vec[K] where K: clone
```

Copies of the keys, in insertion order.

#### Map[K, V].values

```gojo
pub fn values(self) -> Vec[V] where V: clone
```

Copies of the values, in insertion order.

### extend Rc[T]

#### Rc[T].count

```gojo
pub fn count(self) -> int
```

How many `rc` handles share the value.

#### Rc[T].downgrade

```gojo
pub fn downgrade(self) -> Weak[T]
```

A weak handle: it doesn't keep the value alive (for back references that
would otherwise make a cycle).

### extend Weak[T]

#### Weak[T].upgrade

```gojo
pub fn upgrade(self) -> ?Rc[T]
```

A strong handle again, or none when the value is gone.

### extend Arc[T]

#### Arc[T].count

```gojo
pub fn count(self) -> int
```

How many `arc` handles share the value (across threads).

### extend Cell[T]

#### Cell[T].get

```gojo
pub fn get(self) -> T
```

A copy of the value. A `Cell` lets copy values inside an `rc` change.

#### Cell[T].set

```gojo
pub fn set(self, v: T)
```

Replaces the value.

### extend Mutex[T]

#### Mutex[T].lock

```gojo
pub fn lock(self) yield inout T
```

Locking is a projection: the guarded value can't escape the block.

### extend Sender[T]

#### Sender[T].send

```gojo
pub fn send(self, x: sink T) -> bool
```

Sends `x` (`tx <- x`): waits while the channel is full, or, unbuffered,
until a receiver takes it. False, and `x` is dropped, when every
receiver is gone or the task is cancelled.

#### Sender[T].len

```gojo
pub fn len(self) -> int
```

The number of values waiting in the buffer.

### extend Receiver[T]

#### Receiver[T].recv

```gojo
pub fn recv(self) -> ?T
```

Receives a value (`<-rx`), waiting for one; none once every sender is
gone and the buffer is empty (or the task is cancelled).

#### Receiver[T].len

```gojo
pub fn len(self) -> int
```

The number of values waiting in the buffer.

### extend Task[T]

#### Task[T].join

```gojo
pub fn join(sink self) -> T
```

Waits for the task to finish and gives its result.

#### Task[T].cancel

```gojo
pub fn cancel(sink self)
```

Cancels the task; the scope still waits for it, and its result is
dropped.

### extend Iso[T]

#### Iso[T].take

```gojo
pub fn take(sink self) -> T
```

The value, to use in this task.

### extend Scope

#### Scope.cancel

```gojo
pub fn cancel(self)
```

Cancels the scope: every task in it, and its body.

### extend MutexRef[T]

#### MutexRef[T].lock

```gojo
pub fn lock(self) yield inout T
```

`Mutex(&x)`: locking yields the borrowed place

