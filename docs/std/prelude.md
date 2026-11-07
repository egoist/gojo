# prelude

The prelude: available in every Kite file without an import. It holds the
methods of the built-in types (`str`, `String`, slices, `Vec`, `Map`, `?T`,
`T ! E`, `rc`, `Mutex`, channels...), the `error` interface, `panic` and
`assert`, and a few general types: `Set`, `Arena`, `Hasher`, `Span`.
Functions without a body are implemented by the compiler or runtime.

**Types:** [`enum Ordering`](#enum-ordering), [`enum Flow`](#enum-flow), [`interface error`](#interface-error), [`interface Number`](#interface-number), [`struct Hasher`](#struct-hasher), [`struct Id`](#struct-id), [`struct Arena`](#struct-arena), [`struct Span`](#struct-span), [`struct Set`](#struct-set)

**Functions:** [`panic`](#panic), [`assert`](#assert), [`assertEq`](#asserteq), [`unreachable`](#unreachable), [`drop`](#drop), [`swap`](#swap), [`replace`](#replace), [`take`](#take), [`min`](#min), [`max`](#max), [`setOf`](#setof)

**Methods on other types:** [`extend ?T`](#extend-t), [`extend T ! E`](#extend-t--e), [`extend str`](#extend-str), [`extend String`](#extend-string), [`extend []T`](#extend-t-1), [`extend []String`](#extend-string-1), [`extend Vec[T]`](#extend-vect), [`extend Map[K, V]`](#extend-mapk-v), [`extend rc[T]`](#extend-rct), [`extend weak[T]`](#extend-weakt), [`extend arc[T]`](#extend-arct), [`extend Cell[T]`](#extend-cellt), [`extend Mutex[T]`](#extend-mutext), [`extend chan T`](#extend-chan-t), [`extend MutexRef[T]`](#extend-mutexreft)

## Types

### enum Ordering

```kite
pub enum Ordering { Less, Equal, Greater }
```

The result of comparing two values: `a.compare(b)`.

### enum Flow

```kite
pub enum Flow { Continue, Break }
```

Control signal for push-style iteration.

### interface error

```kite
pub interface error {
    fn message(self) String
}
```

The built-in error interface, for application-level error handling.

### interface Number

```kite
pub interface Number {
    ~int | ~i8 | ~i16 | ~i32 | ~u8 | ~u16 | ~u32 | ~u64 | ~f32 | ~f64
}
```

Numeric types, for generic arithmetic.

### struct Hasher

```kite
pub struct Hasher {
    // and private fields
}
```

Computes the hash of values (FNV-1a over `u64` words), for writing a
custom `hash` method.

#### Hasher.writeU64

```kite
pub fn writeU64(self inout, x u64)
```

Mixes `x` into the hash.

#### Hasher.write

```kite
pub fn write[T hash](self inout, x T)
```

Mixes in the hash of any hashable value.

### struct Id

```kite
pub copy struct Id[T any] {
    // and private fields
}
```

A handle to a value in an `Arena[T]`.

### struct Arena

```kite
pub struct Arena[T any] {
    // and private fields
}
```

Storage for graphs, trees and other structures whose nodes refer to each
other: nodes are values in the arena, references are `Id`s. An `Id` carries
a generation, so one whose value was removed finds nothing, rather than a
different value that took its slot.

```kite
var a = Arena[Node]{}
let root = a.alloc(Node{name: "root"})
with n = a.getMut(root) { ... }
```

#### Arena.alloc

```kite
pub fn alloc(self inout, v sink T) Id[T]
```

Stores `v` and returns its `Id`.

#### Arena.get

```kite
pub fn get(self, id Id[T]) yield ?&T
```

The value for `id`, if it is still there.

#### Arena.getMut

```kite
pub fn getMut(self inout, id Id[T]) yield ?inout T
```

The value for `id`, to change in place.

#### Arena.remove

```kite
pub fn remove(self inout, id Id[T]) ?T
```

Removes and returns the value for `id`; its slot is reused, with a new
generation.

#### Arena.len

```kite
pub fn len(self) int
```

The number of values stored.

### struct Span

```kite
pub copy struct Span {
    pub lo int
    pub hi int
}
```

A range of byte offsets into a string. A zero-copy parser keeps the owned
input and `Span`s into it, and borrows views on demand: no reference is
ever stored in a struct.

#### Span.of

```kite
pub fn of(self, s str) str
```

The part of `s` the span covers.

#### Span.len

```kite
pub fn len(self) int
```

The length in bytes.

### struct Set

```kite
pub struct Set[T hash] {
    // and private fields
}
```

A set of values, kept in the order they were first inserted.

```kite
var seen = Set[String].new()
if seen.insert(name.clone()) { ... first time ... }
```

#### Set.new

```kite
pub fn new() Set[T]
```

An empty set.

#### Set.insert

```kite
pub fn insert(self inout, x sink T) bool
```

Adds `x`; false if it was there already.

#### Set.remove

```kite
pub fn remove(self inout, x T) bool
```

Takes `x` out; false if it wasn't there.

#### Set.contains

```kite
pub fn contains(self, x T) bool
```

Whether `x` is in the set.

#### Set.len

```kite
pub fn len(self) int
```

The number of values.

#### Set.isEmpty

```kite
pub fn isEmpty(self) bool
```

Whether the set is empty.

#### Set.clear

```kite
pub fn clear(self inout)
```

Removes every value.

#### Set.each

```kite
pub fn each(self, body fn(&T) Flow)
```

`for x in set`

#### Set.toVec

```kite
pub fn toVec(self) Vec[T] where T: clone
```

Copies of the values, in insertion order.

#### Set.union

```kite
pub fn union(self, other Set[T]) Set[T] where T: clone
```

What is in either.

#### Set.intersection

```kite
pub fn intersection(self, other Set[T]) Set[T] where T: clone
```

What is in both.

#### Set.difference

```kite
pub fn difference(self, other Set[T]) Set[T] where T: clone
```

What is in this one but not in `other`.

## Functions

### panic

```kite
pub fn panic(msg str) never
```

Ends the program with `msg` and the caller's location (exit code 2). Panics
cannot be caught; deferred code does not run.

### assert

```kite
pub fn assert(cond bool)
```

Panics unless `cond` holds.

### assertEq

```kite
pub fn assertEq[T comparable](a T, b T)
```

Panics unless `a == b`, showing both values.

### unreachable

```kite
pub fn unreachable() never
```

Marks code that can't run; panics if it does.

### drop

```kite
pub fn drop[T any](x sink T)
```

Explicitly end the lifetime of a value.

### swap

```kite
pub fn swap[T any](a inout T, b inout T)
```

Exchanges two places.

### replace

```kite
pub fn replace[T any](dst inout T, value sink T) T
```

Puts `value` into `dst` and returns what was there.

### take

```kite
pub fn take[T default](dst inout T) T
```

Takes the value out of `dst`, leaving the type's default (`""`, `0`, `none`,
an empty `Vec`...).

### min

```kite
pub fn min[T copy + ordered](a T, b T) T
```

The smaller of two values (`a` when they are equal).

### max

```kite
pub fn max[T copy + ordered](a T, b T) T
```

The larger of two values (`a` when they are equal).

### setOf

```kite
pub fn setOf[T hash](xs sink Vec[T]) Set[T]
```

A set of the values in `xs` (duplicates once, in the order first seen).

## Methods on other types

### extend ?T

#### ?T.isSome

```kite
pub fn isSome(self) bool
```

Whether there is a value.

#### ?T.isNone

```kite
pub fn isNone(self) bool
```

Whether there is no value.

#### ?T.unwrap

```kite
pub fn unwrap(self sink) T
```

The value; panics on none. Prefer `??`, `if let` or `try` where none can
happen.

#### ?T.expect

```kite
pub fn expect(self sink, msg str) T
```

The value; panics with `msg` on none.

#### ?T.unwrapOr

```kite
pub fn unwrapOr(self sink, d sink T) T
```

The value, or `d` (which is evaluated either way; `x ?? d` evaluates `d` only
when needed).

#### ?T.map

```kite
pub fn map[U any](self, f fn(T) U) ?U
```

`f` applied to the value, if there is one.

### extend T ! E

#### T ! E.isOk

```kite
pub fn isOk(self) bool
```

Whether this is a value rather than an error.

#### T ! E.isErr

```kite
pub fn isErr(self) bool
```

Whether this is an error.

#### T ! E.unwrap

```kite
pub fn unwrap(self sink) T
```

The value; panics with the error. Prefer `try` or `catch`.

#### T ! E.expect

```kite
pub fn expect(self sink, msg str) T
```

The value; panics with `msg` and the error.

#### T ! E.unwrapOr

```kite
pub fn unwrapOr(self sink, d sink T) T
```

The value, or `d` on an error (which is dropped).

#### T ! E.ok

```kite
pub fn ok(self sink) ?T
```

The value, or none on an error.

#### T ! E.err

```kite
pub fn err(self sink) ?E
```

The error, or none.

### extend str

#### str.len

```kite
pub fn len(self) int
```

The length in bytes (UTF-8). Indexes and slices count bytes too.

#### str.ptr

```kite
pub fn ptr(self) *u8
```

raw pointer to the bytes, for FFI

#### str.isEmpty

```kite
pub fn isEmpty(self) bool
```

Whether the length is 0.

#### str.contains

```kite
pub fn contains(self, sub str) bool
```

Whether `sub` occurs in the string (the empty string always does).

#### str.startsWith

```kite
pub fn startsWith(self, prefix str) bool
```

Whether the string begins with `prefix`.

#### str.endsWith

```kite
pub fn endsWith(self, suffix str) bool
```

Whether the string ends with `suffix`.

#### str.find

```kite
pub fn find(self, sub str) ?int
```

The byte index of the first occurrence of `sub`.

#### str.trim

```kite
pub fn trim(self) str
```

The string without leading and trailing ASCII whitespace (space, tab,
newline, carriage return, vertical tab, form feed). A view: no copy.

#### str.trimStart

```kite
pub fn trimStart(self) str
```

The string without leading ASCII whitespace.

#### str.trimEnd

```kite
pub fn trimEnd(self) str
```

The string without trailing ASCII whitespace.

#### str.split

```kite
pub fn split(self, sep str) Vec[String]
```

The pieces between occurrences of `sep`: `"a,,b".split(",")` is
`["a", "", "b"]`, and `"".split(",")` is `[""]`. An empty `sep` splits into
characters.

#### str.lines

```kite
pub fn lines(self) Vec[String]
```

The lines, without their `\n` or `\r\n`; no empty last line for a trailing
newline.

#### str.fields

```kite
pub fn fields(self) Vec[String]
```

The words between runs of ASCII whitespace: `"  a b\tc ".fields()` is
`["a", "b", "c"]`.

#### str.toUpper

```kite
pub fn toUpper(self) String
```

A copy with ASCII letters in upper case (other characters unchanged).

#### str.toLower

```kite
pub fn toLower(self) String
```

A copy with ASCII letters in lower case (other characters unchanged).

#### str.toString

```kite
pub fn toString(self) String
```

An owned copy.

#### str.repeat

```kite
pub fn repeat(self, n int) String
```

The string `n` times (empty for `n` <= 0).

#### str.replace

```kite
pub fn replace(self, from str, to str) String
```

A copy with every occurrence of `from` replaced by `to` (unchanged when
`from` is empty).

#### str.bytes

```kite
pub fn bytes(self) []byte
```

The bytes, as a view.

#### str.runes

```kite
pub fn runes(self) Vec[rune]
```

The Unicode code points. A byte that isn't part of valid UTF-8 (strings
made with `String(bytes)` or read from files aren't checked) reads as
U+FFFD.

#### str.compare

```kite
pub fn compare(self, other str) Ordering
```

Byte-wise order, which is also code-point order.

#### str.parseInt

```kite
pub fn parseInt(self) ?int
```

The decimal integer that is the whole string (an optional `+` or `-`, then
digits), or none: no spaces, no other characters, nothing out of range.

#### str.parseFloat

```kite
pub fn parseFloat(self) ?f64
```

The floating-point number that is the whole string, or none. Accepts what C's
`strtod` does: `1.5`, `-2e10`, `inf`, `nan`, `0x1p3`.

### extend String

#### String.push

```kite
pub fn push(self inout, t str)
```

Appends `t`. (`s += t` does the same.)

#### String.pushRune

```kite
pub fn pushRune(self inout, r rune)
```

Appends the UTF-8 encoding of `r`; U+FFFD for a value that isn't a Unicode
scalar value (a surrogate, negative, or above U+10FFFF).

#### String.clear

```kite
pub fn clear(self inout)
```

Makes the string empty, keeping its buffer.

#### String.asStr

```kite
pub fn asStr(self) str
```

The string as a view.

### extend []T

#### []T.len

```kite
pub fn len(self) int
```

The number of elements.

#### []T.ptr

```kite
pub fn ptr(self) *T
```

raw pointer to the first element, for FFI

#### []T.isEmpty

```kite
pub fn isEmpty(self) bool
```

Whether there are no elements.

#### []T.get

```kite
pub fn get(self, i int) yield ?&T
```

Element `i`, or none when `i` is out of range (indexing with `xs[i]`
panics instead).

#### []T.first

```kite
pub fn first(self) yield ?&T
```

The first element, if any.

#### []T.last

```kite
pub fn last(self) yield ?&T
```

The last element, if any.

#### []T.contains

```kite
pub fn contains(self, x T) bool where T: comparable
```

Whether some element equals `x`.

#### []T.indexOf

```kite
pub fn indexOf(self, x T) ?int where T: comparable
```

The index of the first element equal to `x`.

#### []T.map

```kite
pub fn map[U any](self, f fn(T) U) Vec[U]
```

`f` applied to each element, in order.

#### []T.filter

```kite
pub fn filter(self, keep fn(T) bool) Vec[T] where T: clone
```

Copies of the elements for which `keep` is true.

#### []T.each

```kite
pub fn each(self, f fn(T))
```

Calls `f` on each element, in order.

#### []T.any

```kite
pub fn any(self, f fn(T) bool) bool
```

Whether `f` is true for some element (stops at the first).

#### []T.all

```kite
pub fn all(self, f fn(T) bool) bool
```

Whether `f` is true for every element (stops at the first false).

#### []T.count

```kite
pub fn count(self, f fn(T) bool) int
```

How many elements `f` is true for.

#### []T.fold

```kite
pub fn fold[A any](self, init sink A, f fn(sink A, T) A) A
```

Combines the elements from the left: `f(f(f(init, x0), x1), x2)`.

#### []T.sum

```kite
pub fn sum(self) T where T: Number
```

The sum (0 for no elements). Integer overflow panics, as with `+`.

#### []T.min

```kite
pub fn min(self) ?T where T: copy + ordered
```

The smallest element, or none for no elements.

#### []T.max

```kite
pub fn max(self) ?T where T: copy + ordered
```

The largest element, or none for no elements.

#### []T.toVec

```kite
pub fn toVec(self) Vec[T] where T: clone
```

An owned copy.

### extend []String

#### []String.join

```kite
pub fn join(self, sep str) String
```

The strings with `sep` between them: `["a", "b"].join(", ")` is `"a, b"`.

### extend Vec[T]

#### Vec[T].push

```kite
pub fn push(self inout, x sink T)
```

Appends `x`.

#### Vec[T].pop

```kite
pub fn pop(self inout) ?T
```

Removes and returns the last element, or none when empty.

#### Vec[T].insert

```kite
pub fn insert(self inout, i int, x sink T)
```

Inserts `x` at index `i` (0 <= `i` <= `len()`), moving the later elements
up; panics otherwise.

#### Vec[T].remove

```kite
pub fn remove(self inout, i int) T
```

Removes and returns element `i`, moving the later elements down; panics when
`i` is out of range.

#### Vec[T].swapRemove

```kite
pub fn swapRemove(self inout, i int) T
```

Removes and returns element `i`, putting the last element in its place:
O(1), but changes the order. Panics when `i` is out of range.

#### Vec[T].clear

```kite
pub fn clear(self inout)
```

Removes (and drops) every element, keeping the buffer.

#### Vec[T].truncate

```kite
pub fn truncate(self inout, n int)
```

Keeps the first `n` elements and drops the rest; nothing happens when `n`
>= `len()`. Panics when `n` is negative.

#### Vec[T].extend

```kite
pub fn extend(self inout, other sink Vec[T])
```

Moves the elements of `other` to the end.

#### Vec[T].reverse

```kite
pub fn reverse(self inout)
```

Reverses the order in place.

#### Vec[T].swap

```kite
pub fn swap(self inout, i int, j int)
```

Exchanges elements `i` and `j`; panics when either is out of range.

#### Vec[T].sort

```kite
pub fn sort(self inout) where T: ordered
```

Sorts in ascending order. The sort is stable (equal elements keep their
order) and takes O(n log n) time.

#### Vec[T].sortBy

```kite
pub fn sortBy(self inout, less fn(T, T) bool)
```

Sorts by `less(a, b)`: true when `a` goes before `b`. Stable.

#### Vec[T].reserve

```kite
pub fn reserve(self inout, n int)
```

Makes room for `n` more elements without reallocating; panics ("capacity
overflow") for an impossible size.

#### Vec[T].cap

```kite
pub fn cap(self) int
```

The number of elements there is room for.

### extend Map[K, V]

#### Map[K, V].len

```kite
pub fn len(self) int
```

The number of entries.

#### Map[K, V].isEmpty

```kite
pub fn isEmpty(self) bool
```

Whether there are no entries.

#### Map[K, V].get

```kite
pub fn get(self, k K) yield ?&V
```

The value for `k`, if any. (`m[k]` panics when there is none.)

#### Map[K, V].getMut

```kite
pub fn getMut(self inout, k K) yield ?inout V
```

The value for `k`, to change in place.

#### Map[K, V].contains

```kite
pub fn contains(self, k K) bool
```

Whether there is an entry for `k`.

#### Map[K, V].insert

```kite
pub fn insert(self inout, k sink K, v sink V) ?V
```

Sets the value for `k`, returning the old one. A new key goes at the end of
the iteration order; replacing a value keeps the key's place.

#### Map[K, V].remove

```kite
pub fn remove(self inout, k K) ?V
```

Removes the entry for `k`, returning its value. The others keep their order.

#### Map[K, V].entry

```kite
pub fn entry(self inout, k sink K) yield inout V where V: default
```

The value for `k`, inserting the default first if there is none:
`with n = counts.entry(word) { n += 1 }`.

#### Map[K, V].clear

```kite
pub fn clear(self inout)
```

Removes every entry.

#### Map[K, V].keys

```kite
pub fn keys(self) Vec[K] where K: clone
```

Copies of the keys, in insertion order.

#### Map[K, V].values

```kite
pub fn values(self) Vec[V] where V: clone
```

Copies of the values, in insertion order.

### extend rc[T]

#### rc[T].count

```kite
pub fn count(self) int
```

How many `rc` handles share the value.

#### rc[T].downgrade

```kite
pub fn downgrade(self) weak[T]
```

A weak handle: it doesn't keep the value alive (for back references that
would otherwise make a cycle).

### extend weak[T]

#### weak[T].upgrade

```kite
pub fn upgrade(self) ?rc[T]
```

A strong handle again, or none when the value is gone.

### extend arc[T]

#### arc[T].count

```kite
pub fn count(self) int
```

How many `arc` handles share the value (across threads).

### extend Cell[T]

#### Cell[T].get

```kite
pub fn get(self) T
```

A copy of the value. A `Cell` lets copy values inside an `rc` change.

#### Cell[T].set

```kite
pub fn set(self, v T)
```

Replaces the value.

### extend Mutex[T]

#### Mutex[T].lock

```kite
pub fn lock(self) yield inout T
```

Locking is a projection: the guarded value can't escape the block.

### extend chan T

#### chan T.send

```kite
pub fn send(self, x sink T)
```

Sends `x` (`ch <- x`): waits while the channel is full, or, unbuffered,
until a receiver takes it. Panics when the channel is closed.

#### chan T.recv

```kite
pub fn recv(self) ?T
```

Receives a value (`<-ch`), waiting for one; none once the channel is closed
and empty.

#### chan T.close

```kite
pub fn close(self)
```

Closes the channel: receivers get what is left, then none. Panics when it
is already closed.

#### chan T.len

```kite
pub fn len(self) int
```

The number of values waiting in the buffer.

### extend MutexRef[T]

#### MutexRef[T].lock

```kite
pub fn lock(self) yield inout T
```

`Mutex(&x)`: locking yields the borrowed place

