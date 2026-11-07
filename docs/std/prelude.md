# prelude

The prelude: available in every Gojo file without an import. It holds the
methods of the built-in types (`str`, `String`, slices, `Vec`, `Map`, `?T`,
`T ! E`, `rc`, `Mutex`, channels...), the `error` interface, `panic` and
`assert`, and a few general types: `Set`, `Arena`, `Hasher`, `Span`.
Functions without a body are implemented by the compiler or runtime.

**Types:** [`enum Ordering`](#enum-ordering), [`enum Flow`](#enum-flow), [`interface error`](#interface-error), [`interface Number`](#interface-number), [`interface Integer`](#interface-integer), [`struct Atomic`](#struct-atomic), [`struct RwLock`](#struct-rwlock), [`struct Chan`](#struct-chan), [`struct Hasher`](#struct-hasher), [`struct Id`](#struct-id), [`struct Arena`](#struct-arena), [`struct Span`](#struct-span), [`struct Set`](#struct-set)

**Functions:** [`panic`](#panic), [`assert`](#assert), [`assertEq`](#asserteq), [`unreachable`](#unreachable), [`drop`](#drop), [`swap`](#swap), [`replace`](#replace), [`take`](#take), [`min`](#min), [`max`](#max), [`setOf`](#setof)

**Methods on other types:** [`extend ?T`](#extend-t), [`extend T ! E`](#extend-t--e), [`extend str`](#extend-str), [`extend String`](#extend-string), [`extend []T`](#extend-t-1), [`extend []String`](#extend-string-1), [`extend Vec[T]`](#extend-vect), [`extend Map[K, V]`](#extend-mapk-v), [`extend rc[T]`](#extend-rct), [`extend weak[T]`](#extend-weakt), [`extend arc[T]`](#extend-arct), [`extend Cell[T]`](#extend-cellt), [`extend Mutex[T]`](#extend-mutext), [`extend Sender[T]`](#extend-sendert), [`extend Receiver[T]`](#extend-receivert), [`extend Task[T]`](#extend-taskt), [`extend Iso[T]`](#extend-isot), [`extend Scope`](#extend-scope), [`extend MutexRef[T]`](#extend-mutexreft)

## Types

### enum Ordering

```zig
pub enum Ordering { Less, Equal, Greater }
```

The result of comparing two values: `a.compare(b)`.

### enum Flow

```zig
pub enum Flow { Continue, Break }
```

Control signal for push-style iteration.

### interface error

```zig
pub interface error {
    fn message(self) String
}
```

The built-in error interface, for application-level error handling.

### interface Number

```zig
pub interface Number {
    ~int | ~i8 | ~i16 | ~i32 | ~u8 | ~u16 | ~u32 | ~u64 | ~f32 | ~f64
}
```

Numeric types, for generic arithmetic.

### interface Integer

```zig
pub interface Integer {
    ~int | ~i8 | ~i16 | ~i32 | ~u8 | ~u16 | ~u32 | ~u64
}
```

Integer types, for `Atomic`.

### struct Atomic

```zig
pub unsafe send struct Atomic[T Integer] {
    // and private fields
}
```

An integer that tasks share and change without a lock: each operation is
atomic and sequentially consistent. Tasks borrow it and call its methods at
the same time:

```zig
let hits = Atomic.new(0)
par for r in requests { if r.ok { _ = hits.add(1) } }
```

#### Atomic.new

```zig
pub fn new(x T) Atomic[T]
```

#### Atomic.load

```zig
pub fn load(self) T
```

The value now.

#### Atomic.store

```zig
pub fn store(self, x T)
```

Replaces the value.

#### Atomic.add

```zig
pub fn add(self, d T) T
```

Adds `d`, wrapping around on overflow, and gives the new value.

#### Atomic.sub

```zig
pub fn sub(self, d T) T
```

Subtracts `d`, wrapping around on overflow, and gives the new value.

#### Atomic.swap

```zig
pub fn swap(self, x T) T
```

Replaces the value and gives the old one.

#### Atomic.compareSwap

```zig
pub fn compareSwap(self, old T, new T) bool
```

Replaces the value with `new` if it is `old`; true if it did.

#### Atomic.clone

```zig
pub fn clone(self) Atomic[T]
```

A new `Atomic` with the value now.

### struct RwLock

```zig
pub unsafe send struct RwLock[T send] {
    // and private fields
}
```

A lock that many readers may hold at once, or one writer:

```zig
let cache = RwLock.new(Map[String, String]{})
with c = cache.read() { ... }     // shared: other readers too
with c = cache.write() { ... }    // exclusive
```

Like `Mutex.lock`, `read` and `write` are projections: the reference can't
leave the block. A writer waiting keeps new readers out, so writers don't
starve.

#### RwLock.new

```zig
pub fn new(x sink T) RwLock[T]
```

#### RwLock.read

```zig
pub fn read(self) yield &T
```

Shared access.

#### RwLock.write

```zig
pub fn write(self) yield inout T
```

Exclusive access.

#### RwLock.toString

```zig
pub fn toString(self) String
```

"RwLock" (the value can't be read without the lock).

### struct Chan

```zig
pub struct Chan[T] {
    // The sending end; `.clone()` it for more senders.
    pub tx Sender[T]
    // The receiving end; `.clone()` it for more receivers.
    pub rx Receiver[T]
}
```

`chan[T](n)` makes a channel with room for `n` values (`chan[T]()`: none, so
a send waits for a receiver). Its two ends are handed out separately:

```zig
let ch = chan[Job](16)
go produce(ch.tx)          // the channel closes when the last sender is dropped
for job in ch.rx { run(job) }
```

### struct Hasher

```zig
pub struct Hasher {
    // and private fields
}
```

Computes the hash of values (FNV-1a over `u64` words), for writing a
custom `hash` method.

#### Hasher.writeU64

```zig
pub fn writeU64(self inout, x u64)
```

Mixes `x` into the hash.

#### Hasher.write

```zig
pub fn write[T hash](self inout, x T)
```

Mixes in the hash of any hashable value.

### struct Id

```zig
pub copy struct Id[T any] {
    // and private fields
}
```

A handle to a value in an `Arena[T]`.

### struct Arena

```zig
pub struct Arena[T any] {
    // and private fields
}
```

Storage for graphs, trees and other structures whose nodes refer to each
other: nodes are values in the arena, references are `Id`s. An `Id` carries
a generation, so one whose value was removed finds nothing, rather than a
different value that took its slot.

```zig
var a = Arena[Node]{}
let root = a.alloc(Node{name: "root"})
with n = a.getMut(root) { ... }
```

#### Arena.alloc

```zig
pub fn alloc(self inout, v sink T) Id[T]
```

Stores `v` and returns its `Id`.

#### Arena.get

```zig
pub fn get(self, id Id[T]) yield ?&T
```

The value for `id`, if it is still there.

#### Arena.getMut

```zig
pub fn getMut(self inout, id Id[T]) yield ?inout T
```

The value for `id`, to change in place.

#### Arena.remove

```zig
pub fn remove(self inout, id Id[T]) ?T
```

Removes and returns the value for `id`; its slot is reused, with a new
generation.

#### Arena.len

```zig
pub fn len(self) int
```

The number of values stored.

### struct Span

```zig
pub copy struct Span {
    pub lo int
    pub hi int
}
```

A range of byte offsets into a string. A zero-copy parser keeps the owned
input and `Span`s into it, and borrows views on demand: no reference is
ever stored in a struct.

#### Span.of

```zig
pub fn of(self, s str) str
```

The part of `s` the span covers.

#### Span.len

```zig
pub fn len(self) int
```

The length in bytes.

### struct Set

```zig
pub struct Set[T hash] {
    // and private fields
}
```

A set of values, kept in the order they were first inserted.

```zig
var seen = Set[String].new()
if seen.insert(name.clone()) { ... first time ... }
```

#### Set.new

```zig
pub fn new() Set[T]
```

An empty set.

#### Set.insert

```zig
pub fn insert(self inout, x sink T) bool
```

Adds `x`; false if it was there already.

#### Set.remove

```zig
pub fn remove(self inout, x T) bool
```

Takes `x` out; false if it wasn't there.

#### Set.contains

```zig
pub fn contains(self, x T) bool
```

Whether `x` is in the set.

#### Set.len

```zig
pub fn len(self) int
```

The number of values.

#### Set.isEmpty

```zig
pub fn isEmpty(self) bool
```

Whether the set is empty.

#### Set.clear

```zig
pub fn clear(self inout)
```

Removes every value.

#### Set.each

```zig
pub fn each(self, body fn(&T) Flow)
```

`for x in set`

#### Set.toVec

```zig
pub fn toVec(self) Vec[T] where T: clone
```

Copies of the values, in insertion order.

#### Set.union

```zig
pub fn union(self, other Set[T]) Set[T] where T: clone
```

What is in either.

#### Set.intersection

```zig
pub fn intersection(self, other Set[T]) Set[T] where T: clone
```

What is in both.

#### Set.difference

```zig
pub fn difference(self, other Set[T]) Set[T] where T: clone
```

What is in this one but not in `other`.

## Functions

### panic

```zig
pub fn panic(msg str) never
```

Ends the program with `msg` and the caller's location (exit code 2). Panics
cannot be caught; deferred code does not run.

### assert

```zig
pub fn assert(cond bool)
```

Panics unless `cond` holds.

### assertEq

```zig
pub fn assertEq[T comparable](a T, b T)
```

Panics unless `a == b`, showing both values.

### unreachable

```zig
pub fn unreachable() never
```

Marks code that can't run; panics if it does.

### drop

```zig
pub fn drop[T any](x sink T)
```

Explicitly end the lifetime of a value.

### swap

```zig
pub fn swap[T any](a inout T, b inout T)
```

Exchanges two places.

### replace

```zig
pub fn replace[T any](dst inout T, value sink T) T
```

Puts `value` into `dst` and returns what was there.

### take

```zig
pub fn take[T default](dst inout T) T
```

Takes the value out of `dst`, leaving the type's default (`""`, `0`, `none`,
an empty `Vec`...).

### min

```zig
pub fn min[T copy + ordered](a T, b T) T
```

The smaller of two values (`a` when they are equal).

### max

```zig
pub fn max[T copy + ordered](a T, b T) T
```

The larger of two values (`a` when they are equal).

### setOf

```zig
pub fn setOf[T hash](xs sink Vec[T]) Set[T]
```

A set of the values in `xs` (duplicates once, in the order first seen).

## Methods on other types

### extend ?T

#### ?T.isSome

```zig
pub fn isSome(self) bool
```

Whether there is a value.

#### ?T.isNone

```zig
pub fn isNone(self) bool
```

Whether there is no value.

#### ?T.unwrap

```zig
pub fn unwrap(self sink) T
```

The value; panics on none. Prefer `??`, `if let` or `try` where none can
happen.

#### ?T.expect

```zig
pub fn expect(self sink, msg str) T
```

The value; panics with `msg` on none.

#### ?T.unwrapOr

```zig
pub fn unwrapOr(self sink, d sink T) T
```

The value, or `d` (which is evaluated either way; `x ?? d` evaluates `d` only
when needed).

#### ?T.map

```zig
pub fn map[U any](self, f fn(T) U) ?U
```

`f` applied to the value, if there is one.

### extend T ! E

#### T ! E.isOk

```zig
pub fn isOk(self) bool
```

Whether this is a value rather than an error.

#### T ! E.isErr

```zig
pub fn isErr(self) bool
```

Whether this is an error.

#### T ! E.unwrap

```zig
pub fn unwrap(self sink) T
```

The value; panics with the error. Prefer `try` or `catch`.

#### T ! E.expect

```zig
pub fn expect(self sink, msg str) T
```

The value; panics with `msg` and the error.

#### T ! E.unwrapOr

```zig
pub fn unwrapOr(self sink, d sink T) T
```

The value, or `d` on an error (which is dropped).

#### T ! E.ok

```zig
pub fn ok(self sink) ?T
```

The value, or none on an error.

#### T ! E.err

```zig
pub fn err(self sink) ?E
```

The error, or none.

### extend str

#### str.len

```zig
pub fn len(self) int
```

The length in bytes (UTF-8). Indexes and slices count bytes too.

#### str.ptr

```zig
pub fn ptr(self) *u8
```

raw pointer to the bytes, for FFI

#### str.isEmpty

```zig
pub fn isEmpty(self) bool
```

Whether the length is 0.

#### str.contains

```zig
pub fn contains(self, sub str) bool
```

Whether `sub` occurs in the string (the empty string always does).

#### str.startsWith

```zig
pub fn startsWith(self, prefix str) bool
```

Whether the string begins with `prefix`.

#### str.endsWith

```zig
pub fn endsWith(self, suffix str) bool
```

Whether the string ends with `suffix`.

#### str.find

```zig
pub fn find(self, sub str) ?int
```

The byte index of the first occurrence of `sub`.

#### str.trim

```zig
pub fn trim(self) str
```

The string without leading and trailing ASCII whitespace (space, tab,
newline, carriage return, vertical tab, form feed). A view: no copy.

#### str.trimStart

```zig
pub fn trimStart(self) str
```

The string without leading ASCII whitespace.

#### str.trimEnd

```zig
pub fn trimEnd(self) str
```

The string without trailing ASCII whitespace.

#### str.split

```zig
pub fn split(self, sep str) Vec[String]
```

The pieces between occurrences of `sep`: `"a,,b".split(",")` is
`["a", "", "b"]`, and `"".split(",")` is `[""]`. An empty `sep` splits into
characters.

#### str.lines

```zig
pub fn lines(self) Vec[String]
```

The lines, without their `\n` or `\r\n`; no empty last line for a trailing
newline.

#### str.fields

```zig
pub fn fields(self) Vec[String]
```

The words between runs of ASCII whitespace: `"  a b\tc ".fields()` is
`["a", "b", "c"]`.

#### str.toUpper

```zig
pub fn toUpper(self) String
```

A copy with ASCII letters in upper case (other characters unchanged).

#### str.toLower

```zig
pub fn toLower(self) String
```

A copy with ASCII letters in lower case (other characters unchanged).

#### str.toString

```zig
pub fn toString(self) String
```

An owned copy.

#### str.repeat

```zig
pub fn repeat(self, n int) String
```

The string `n` times (empty for `n` <= 0).

#### str.replace

```zig
pub fn replace(self, from str, to str) String
```

A copy with every occurrence of `from` replaced by `to` (unchanged when
`from` is empty).

#### str.bytes

```zig
pub fn bytes(self) []byte
```

The bytes, as a view.

#### str.runes

```zig
pub fn runes(self) Vec[rune]
```

The Unicode code points. A byte that isn't part of valid UTF-8 (strings
made with `String(bytes)` or read from files aren't checked) reads as
U+FFFD.

#### str.compare

```zig
pub fn compare(self, other str) Ordering
```

Byte-wise order, which is also code-point order.

#### str.parseInt

```zig
pub fn parseInt(self) ?int
```

The decimal integer that is the whole string (an optional `+` or `-`, then
digits), or none: no spaces, no other characters, nothing out of range.

#### str.parseFloat

```zig
pub fn parseFloat(self) ?f64
```

The floating-point number that is the whole string, or none. Accepts what C's
`strtod` does: `1.5`, `-2e10`, `inf`, `nan`, `0x1p3`.

### extend String

#### String.push

```zig
pub fn push(self inout, t str)
```

Appends `t`. (`s += t` does the same.)

#### String.pushRune

```zig
pub fn pushRune(self inout, r rune)
```

Appends the UTF-8 encoding of `r`; U+FFFD for a value that isn't a Unicode
scalar value (a surrogate, negative, or above U+10FFFF).

#### String.clear

```zig
pub fn clear(self inout)
```

Makes the string empty, keeping its buffer.

#### String.asStr

```zig
pub fn asStr(self) str
```

The string as a view.

### extend []T

#### []T.len

```zig
pub fn len(self) int
```

The number of elements.

#### []T.ptr

```zig
pub fn ptr(self) *T
```

raw pointer to the first element, for FFI

#### []T.isEmpty

```zig
pub fn isEmpty(self) bool
```

Whether there are no elements.

#### []T.get

```zig
pub fn get(self, i int) yield ?&T
```

Element `i`, or none when `i` is out of range (indexing with `xs[i]`
panics instead).

#### []T.first

```zig
pub fn first(self) yield ?&T
```

The first element, if any.

#### []T.last

```zig
pub fn last(self) yield ?&T
```

The last element, if any.

#### []T.contains

```zig
pub fn contains(self, x T) bool where T: comparable
```

Whether some element equals `x`.

#### []T.indexOf

```zig
pub fn indexOf(self, x T) ?int where T: comparable
```

The index of the first element equal to `x`.

#### []T.map

```zig
pub fn map[U any](self, f fn(T) U) Vec[U]
```

`f` applied to each element, in order.

#### []T.filter

```zig
pub fn filter(self, keep fn(T) bool) Vec[T] where T: clone
```

Copies of the elements for which `keep` is true.

#### []T.each

```zig
pub fn each(self, f fn(T))
```

Calls `f` on each element, in order.

#### []T.any

```zig
pub fn any(self, f fn(T) bool) bool
```

Whether `f` is true for some element (stops at the first).

#### []T.all

```zig
pub fn all(self, f fn(T) bool) bool
```

Whether `f` is true for every element (stops at the first false).

#### []T.count

```zig
pub fn count(self, f fn(T) bool) int
```

How many elements `f` is true for.

#### []T.fold

```zig
pub fn fold[A any](self, init sink A, f fn(sink A, T) A) A
```

Combines the elements from the left: `f(f(f(init, x0), x1), x2)`.

#### []T.sum

```zig
pub fn sum(self) T where T: Number
```

The sum (0 for no elements). Integer overflow panics, as with `+`.

#### []T.min

```zig
pub fn min(self) ?T where T: copy + ordered
```

The smallest element, or none for no elements.

#### []T.max

```zig
pub fn max(self) ?T where T: copy + ordered
```

The largest element, or none for no elements.

#### []T.toVec

```zig
pub fn toVec(self) Vec[T] where T: clone
```

An owned copy.

### extend []String

#### []String.join

```zig
pub fn join(self, sep str) String
```

The strings with `sep` between them: `["a", "b"].join(", ")` is `"a, b"`.

### extend Vec[T]

#### Vec[T].push

```zig
pub fn push(self inout, x sink T)
```

Appends `x`.

#### Vec[T].pop

```zig
pub fn pop(self inout) ?T
```

Removes and returns the last element, or none when empty.

#### Vec[T].insert

```zig
pub fn insert(self inout, i int, x sink T)
```

Inserts `x` at index `i` (0 <= `i` <= `len()`), moving the later elements
up; panics otherwise.

#### Vec[T].remove

```zig
pub fn remove(self inout, i int) T
```

Removes and returns element `i`, moving the later elements down; panics when
`i` is out of range.

#### Vec[T].swapRemove

```zig
pub fn swapRemove(self inout, i int) T
```

Removes and returns element `i`, putting the last element in its place:
O(1), but changes the order. Panics when `i` is out of range.

#### Vec[T].clear

```zig
pub fn clear(self inout)
```

Removes (and drops) every element, keeping the buffer.

#### Vec[T].truncate

```zig
pub fn truncate(self inout, n int)
```

Keeps the first `n` elements and drops the rest; nothing happens when `n`
>= `len()`. Panics when `n` is negative.

#### Vec[T].extend

```zig
pub fn extend(self inout, other sink Vec[T])
```

Moves the elements of `other` to the end.

#### Vec[T].reverse

```zig
pub fn reverse(self inout)
```

Reverses the order in place.

#### Vec[T].swap

```zig
pub fn swap(self inout, i int, j int)
```

Exchanges elements `i` and `j`; panics when either is out of range.

#### Vec[T].sort

```zig
pub fn sort(self inout) where T: ordered
```

Sorts in ascending order. The sort is stable (equal elements keep their
order) and takes O(n log n) time.

#### Vec[T].sortBy

```zig
pub fn sortBy(self inout, less fn(T, T) bool)
```

Sorts by `less(a, b)`: true when `a` goes before `b`. Stable.

#### Vec[T].reserve

```zig
pub fn reserve(self inout, n int)
```

Makes room for `n` more elements without reallocating; panics ("capacity
overflow") for an impossible size.

#### Vec[T].cap

```zig
pub fn cap(self) int
```

The number of elements there is room for.

### extend Map[K, V]

#### Map[K, V].len

```zig
pub fn len(self) int
```

The number of entries.

#### Map[K, V].isEmpty

```zig
pub fn isEmpty(self) bool
```

Whether there are no entries.

#### Map[K, V].get

```zig
pub fn get(self, k K) yield ?&V
```

The value for `k`, if any. (`m[k]` panics when there is none.)

#### Map[K, V].getMut

```zig
pub fn getMut(self inout, k K) yield ?inout V
```

The value for `k`, to change in place.

#### Map[K, V].contains

```zig
pub fn contains(self, k K) bool
```

Whether there is an entry for `k`.

#### Map[K, V].insert

```zig
pub fn insert(self inout, k sink K, v sink V) ?V
```

Sets the value for `k`, returning the old one. A new key goes at the end of
the iteration order; replacing a value keeps the key's place.

#### Map[K, V].remove

```zig
pub fn remove(self inout, k K) ?V
```

Removes the entry for `k`, returning its value. The others keep their order.

#### Map[K, V].entry

```zig
pub fn entry(self inout, k sink K) yield inout V where V: default
```

The value for `k`, inserting the default first if there is none:
`with n = counts.entry(word) { n += 1 }`.

#### Map[K, V].clear

```zig
pub fn clear(self inout)
```

Removes every entry.

#### Map[K, V].keys

```zig
pub fn keys(self) Vec[K] where K: clone
```

Copies of the keys, in insertion order.

#### Map[K, V].values

```zig
pub fn values(self) Vec[V] where V: clone
```

Copies of the values, in insertion order.

### extend rc[T]

#### rc[T].count

```zig
pub fn count(self) int
```

How many `rc` handles share the value.

#### rc[T].downgrade

```zig
pub fn downgrade(self) weak[T]
```

A weak handle: it doesn't keep the value alive (for back references that
would otherwise make a cycle).

### extend weak[T]

#### weak[T].upgrade

```zig
pub fn upgrade(self) ?rc[T]
```

A strong handle again, or none when the value is gone.

### extend arc[T]

#### arc[T].count

```zig
pub fn count(self) int
```

How many `arc` handles share the value (across threads).

### extend Cell[T]

#### Cell[T].get

```zig
pub fn get(self) T
```

A copy of the value. A `Cell` lets copy values inside an `rc` change.

#### Cell[T].set

```zig
pub fn set(self, v T)
```

Replaces the value.

### extend Mutex[T]

#### Mutex[T].lock

```zig
pub fn lock(self) yield inout T
```

Locking is a projection: the guarded value can't escape the block.

### extend Sender[T]

#### Sender[T].send

```zig
pub fn send(self, x sink T) bool
```

Sends `x` (`tx <- x`): waits while the channel is full, or, unbuffered,
until a receiver takes it. False, and `x` is dropped, when every
receiver is gone or the task is cancelled.

#### Sender[T].len

```zig
pub fn len(self) int
```

The number of values waiting in the buffer.

### extend Receiver[T]

#### Receiver[T].recv

```zig
pub fn recv(self) ?T
```

Receives a value (`<-rx`), waiting for one; none once every sender is
gone and the buffer is empty (or the task is cancelled).

#### Receiver[T].len

```zig
pub fn len(self) int
```

The number of values waiting in the buffer.

### extend Task[T]

#### Task[T].join

```zig
pub fn join(self sink) T
```

Waits for the task to finish and gives its result.

#### Task[T].cancel

```zig
pub fn cancel(self sink)
```

Cancels the task; the scope still waits for it, and its result is
dropped.

### extend Iso[T]

#### Iso[T].take

```zig
pub fn take(self sink) T
```

The value, to use in this task.

### extend Scope

#### Scope.cancel

```zig
pub fn cancel(self)
```

Cancels the scope: every task in it, and its body.

### extend MutexRef[T]

#### MutexRef[T].lock

```zig
pub fn lock(self) yield inout T
```

`Mutex(&x)`: locking yields the borrowed place

