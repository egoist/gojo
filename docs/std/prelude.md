# prelude

The prelude: available in every Gojo file without an import. It holds the
methods of the built-in types (`str`, `String`, slices, `Vec`, `Map`, `?T`,
`T ! E`, `rc`, `Mutex`, channels...), the `error` interface, `panic` and
`assert`, and a few general types: `Set`, `Arena`, `Hasher`, `Span`.
Functions without a body are implemented by the compiler or runtime.

**Types:** [`enum Ordering`](#enum-ordering), [`enum Flow`](#enum-flow), [`interface error`](#interface-error), [`interface Number`](#interface-number), [`interface Integer`](#interface-integer), [`interface Float`](#interface-float), [`struct Atomic`](#struct-atomic), [`struct RwLock`](#struct-rwlock), [`struct Chan`](#struct-chan), [`struct Hasher`](#struct-hasher), [`struct Id`](#struct-id), [`struct Arena`](#struct-arena), [`struct Span`](#struct-span), [`struct Set`](#struct-set), [`struct Tuple2`](#struct-tuple2), [`struct Tuple3`](#struct-tuple3), [`struct Tuple4`](#struct-tuple4), [`struct Tuple5`](#struct-tuple5), [`struct Tuple6`](#struct-tuple6)

**Functions:** [`panic`](#panic), [`assert`](#assert), [`assertEq`](#asserteq), [`unreachable`](#unreachable), [`drop`](#drop), [`swap`](#swap), [`replace`](#replace), [`take`](#take), [`min`](#min), [`max`](#max), [`setOf`](#setof)

**Methods on other types:** [`extend ?T`](#extend-t), [`extend T ! E`](#extend-t--e), [`extend T: Integer`](#extend-t-integer), [`extend T: Float`](#extend-t-float), [`extend f64`](#extend-f64), [`extend f32`](#extend-f32), [`extend rune`](#extend-rune), [`extend str`](#extend-str), [`extend String`](#extend-string), [`extend []T`](#extend-t-1), [`extend []Vec[T]`](#extend-vect), [`extend []String`](#extend-string-1), [`extend Vec[T]`](#extend-vect-1), [`extend Map[K, V]`](#extend-mapk-v), [`extend Rc[T]`](#extend-rct), [`extend Weak[T]`](#extend-weakt), [`extend Arc[T]`](#extend-arct), [`extend Cell[T]`](#extend-cellt), [`extend Mutex[T]`](#extend-mutext), [`extend Sender[T]`](#extend-sendert), [`extend Receiver[T]`](#extend-receivert), [`extend Task[T]`](#extend-taskt), [`extend Iso[T]`](#extend-isot), [`extend Scope`](#extend-scope), [`extend MutexRef[T]`](#extend-mutexreft)

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

Integer types: their methods (`abs`, `pow`, `checkedAdd`...) are below,
under `extend T: Integer`.

### interface Float

```gojo
pub interface Float {
    ~f32 | ~f64
}
```

Floating-point types: their methods (`sqrt`, `floor`, `isNaN`...) are
below, under `extend T: Float`.

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

Computes the hash of values, for writing a custom `hash` method. Hashes
are keyed with a random value chosen when the program starts, so keys
can't be chosen to collide (and hashes differ from run to run).

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

#### Set.symmetricDifference

```gojo
pub fn symmetricDifference(self, other: Set[T]) -> Set[T] where T: clone
```

What is in exactly one of the two.

#### Set.isSubset

```gojo
pub fn isSubset(self, other: Set[T]) -> bool
```

Whether every value is also in `other`.

#### Set.isSuperset

```gojo
pub fn isSuperset(self, other: Set[T]) -> bool
```

Whether every value of `other` is also in this one.

#### Set.isDisjoint

```gojo
pub fn isDisjoint(self, other: Set[T]) -> bool
```

Whether the two have no value in common.

#### Set.extend

```gojo
pub fn extend(inout self, xs: sink Vec[T])
```

Adds the values of `xs`.

#### Set.retain

```gojo
pub fn retain(inout self, keep: fn(T) -> bool) where T: clone
```

Keeps the values `keep` is true for and removes the rest.

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
pub fn swap[T: any](a: inout T, b: inout T)
```

Exchanges two places.

### replace

```gojo
pub fn replace[T: any](dst: inout T, value: sink T) -> T
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
pub fn isSome(self) -> bool where T: any
```

Whether there is a value.

#### ?T.isNone

```gojo
pub fn isNone(self) -> bool where T: any
```

Whether there is no value.

#### ?T.unwrap

```gojo
pub fn unwrap(sink self) -> T where T: any
```

The value; panics on none. Prefer `??`, `if let` or `try` where none can
happen.

#### ?T.expect

```gojo
pub fn expect(sink self, msg: str) -> T where T: any
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

#### ?T.unwrapOrElse

```gojo
pub fn unwrapOrElse(sink self, f: fn() -> T) -> T
```

The value, or what `f` makes (called only when there is none).

#### ?T.andThen

```gojo
pub fn andThen[U](self, f: fn(T) -> ?U) -> ?U
```

`f` applied to the value, which may itself give none: chains steps that
can each find nothing.

#### ?T.filter

```gojo
pub fn filter(sink self, keep: fn(T) -> bool) -> ?T
```

The value if `keep` is true for it, otherwise none.

#### ?T.or

```gojo
pub fn or(sink self, other: sink ?T) -> ?T
```

This one if it has a value, otherwise `other`.

#### ?T.orElse

```gojo
pub fn orElse(sink self, f: fn() -> ?T) -> ?T
```

This one if it has a value, otherwise what `f` gives (called only then).

#### ?T.okOr

```gojo
pub fn okOr[E](sink self, e: sink E) -> T ! E
```

The value, or the error `e`: turns a missing value into a failure that
`try` passes on.

```gojo
let port = try env.get("PORT").okOr(ConfigError.Missing("PORT"))
```

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
pub fn unwrap(sink self) -> T where T: any
```

The value; panics with the error. Prefer `try` or `catch`.

#### T ! E.expect

```gojo
pub fn expect(sink self, msg: str) -> T where T: any
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

#### T ! E.unwrapOrElse

```gojo
pub fn unwrapOrElse(sink self, f: fn(sink E) -> T) -> T
```

The value, or what `f` makes from the error.

#### T ! E.map

```gojo
pub fn map[U](sink self, f: fn(T) -> U) -> U ! E
```

`f` applied to the value; an error passes through.

#### T ! E.mapErr

```gojo
pub fn mapErr[F](sink self, f: fn(sink E) -> F) -> T ! F
```

`f` applied to the error, to turn it into another error type; a value
passes through.

```gojo
let n = try text.parse().mapErr(e => AppError.BadConfig(e))
```

#### T ! E.andThen

```gojo
pub fn andThen[U](sink self, f: fn(T) -> U ! E) -> U ! E
```

`f` applied to the value, which may itself fail: chains steps that can
each fail with the same error type.

#### T ! E.orElse

```gojo
pub fn orElse[F](sink self, f: fn(sink E) -> T ! F) -> T ! F
```

This one if it is a value, otherwise what `f` makes of the error: a
fallback that may fail too.

### extend T: Integer

#### T: Integer.abs

```gojo
pub fn abs(self) -> T
```

The absolute value. Panics for the smallest value of a signed type
(`i8(-128)`), which has no positive counterpart.

#### T: Integer.signum

```gojo
pub fn signum(self) -> T
```

-1, 0 or 1, by the sign of the value.

#### T: Integer.pow

```gojo
pub fn pow(self, exp: int) -> T
```

The value to the power `exp` (>= 0); panics when the result overflows,
as `*` does. O(log exp).

#### T: Integer.clamp

```gojo
pub fn clamp(self, lo: T, hi: T) -> T
```

The value kept within `lo`..=`hi`; panics when `lo` > `hi`.

#### T: Integer.checkedAdd

```gojo
pub fn checkedAdd(self, other: T) -> ?T
```

`self + other`, or none when it overflows.

#### T: Integer.checkedSub

```gojo
pub fn checkedSub(self, other: T) -> ?T
```

`self - other`, or none when it overflows.

#### T: Integer.checkedMul

```gojo
pub fn checkedMul(self, other: T) -> ?T
```

`self * other`, or none when it overflows.

#### T: Integer.checkedDiv

```gojo
pub fn checkedDiv(self, other: T) -> ?T
```

`self / other`, or none when `other` is 0 or the result overflows
(the smallest signed value divided by -1).

#### T: Integer.saturatingAdd

```gojo
pub fn saturatingAdd(self, other: T) -> T
```

`self + other`, stopping at the type's largest or smallest value instead
of overflowing.

#### T: Integer.saturatingSub

```gojo
pub fn saturatingSub(self, other: T) -> T
```

`self - other`, stopping at the type's largest or smallest value.

#### T: Integer.saturatingMul

```gojo
pub fn saturatingMul(self, other: T) -> T
```

`self * other`, stopping at the type's largest or smallest value.

#### T: Integer.divEuclid

```gojo
pub fn divEuclid(self, other: T) -> T
```

The quotient rounded so that `remEuclid` is never negative:
`(-7).divEuclid(2)` is -4. Panics when `other` is 0.

#### T: Integer.remEuclid

```gojo
pub fn remEuclid(self, other: T) -> T
```

The remainder that is never negative: `(-7).remEuclid(2)` is 1, where
`%` gives -1. For wrapping indexes and clock arithmetic. Panics when
`other` is 0.

#### T: Integer.isPowerOfTwo

```gojo
pub fn isPowerOfTwo(self) -> bool
```

Whether the value is a power of two (1, 2, 4...).

#### T: Integer.countOnes

```gojo
pub fn countOnes(self) -> int
```

The number of 1 bits.

#### T: Integer.leadingZeros

```gojo
pub fn leadingZeros(self) -> int
```

The number of 0 bits before the highest 1 bit (all of them for 0).

#### T: Integer.trailingZeros

```gojo
pub fn trailingZeros(self) -> int
```

The number of 0 bits after the lowest 1 bit (all of them for 0).

#### T: Integer.bitLen

```gojo
pub fn bitLen(self) -> int
```

The number of bits needed for the value: 0 for 0, 3 for 5. (For a
negative value, the type's width.)

#### T: Integer.rotateLeft

```gojo
pub fn rotateLeft(self, n: int) -> T
```

The bits shifted left by `n` (any `n`), those that fall off the top
coming back in at the bottom.

#### T: Integer.rotateRight

```gojo
pub fn rotateRight(self, n: int) -> T
```

The bits shifted right by `n`, those that fall off the bottom coming
back in at the top.

#### T: Integer.swapBytes

```gojo
pub fn swapBytes(self) -> T
```

The bytes in the opposite order: converts between little- and big-endian.

#### T: Integer.toString

```gojo
pub fn toString(self) -> String
```

The value in decimal: what `print` shows.

#### T: Integer.toStringRadix

```gojo
pub fn toStringRadix(self, radix: int) -> String
```

The value in base `radix` (2 to 36), with lower-case letters for digits
over 9 and a `-` for negative values: `255.toStringRadix(16)` is `"ff"`.

#### T: Integer.parse

```gojo
pub fn parse(s: str) -> ?T
```

The integer that is the whole of `s` in decimal (an optional `+` or `-`,
then digits), or none: no spaces, nothing out of the type's range.

```gojo
let port = u16.parse(text) ?? 8080
```

#### T: Integer.parseRadix

```gojo
pub fn parseRadix(s: str, radix: int) -> ?T
```

Like `parse`, in base `radix` (2 to 36; letters in either case for
digits over 9), without a prefix: `u32.parseRadix("ff", 16)` is 255.

### extend T: Float

#### T: Float.abs

```gojo
pub fn abs(self) -> T
```

The absolute value.

#### T: Float.sqrt

```gojo
pub fn sqrt(self) -> T
```

The square root (NaN for negative values).

#### T: Float.cbrt

```gojo
pub fn cbrt(self) -> T
```

The cube root.

#### T: Float.pow

```gojo
pub fn pow(self, y: T) -> T
```

The value to the power `y`.

#### T: Float.exp

```gojo
pub fn exp(self) -> T
```

e to the power of the value.

#### T: Float.ln

```gojo
pub fn ln(self) -> T
```

The natural logarithm.

#### T: Float.log2

```gojo
pub fn log2(self) -> T
```

The base-2 logarithm.

#### T: Float.log10

```gojo
pub fn log10(self) -> T
```

The base-10 logarithm.

#### T: Float.floor

```gojo
pub fn floor(self) -> T
```

The largest whole number not above the value.

#### T: Float.ceil

```gojo
pub fn ceil(self) -> T
```

The smallest whole number not below the value.

#### T: Float.round

```gojo
pub fn round(self) -> T
```

The nearest whole number, halves away from zero (2.5 to 3, -2.5 to -3).

#### T: Float.trunc

```gojo
pub fn trunc(self) -> T
```

The whole-number part, towards zero (-2.7 to -2).

#### T: Float.fract

```gojo
pub fn fract(self) -> T
```

The fractional part: `self - self.trunc()`, with the value's sign.

#### T: Float.signum

```gojo
pub fn signum(self) -> T
```

1 for positive values (and +0), -1 for negative ones (and -0), NaN for NaN.

#### T: Float.copySign

```gojo
pub fn copySign(self, sign: T) -> T
```

The value with the sign of `sign`.

#### T: Float.isNaN

```gojo
pub fn isNaN(self) -> bool
```

Whether the value is NaN ("not a number").

#### T: Float.isInf

```gojo
pub fn isInf(self) -> bool
```

Whether the value is +Inf or -Inf.

#### T: Float.isFinite

```gojo
pub fn isFinite(self) -> bool
```

Whether the value is neither infinite nor NaN.

#### T: Float.clamp

```gojo
pub fn clamp(self, lo: T, hi: T) -> T
```

The value kept within `lo`..=`hi` (NaN stays NaN); panics when `lo` > `hi`.

#### T: Float.toString

```gojo
pub fn toString(self) -> String
```

The value as `print` shows it: the shortest form that reads back the same.

#### T: Float.parse

```gojo
pub fn parse(s: str) -> ?T
```

The number that is the whole of `s`, or none. Accepts what C's `strtod`
does: `1.5`, `-2e10`, `inf`, `nan`, `0x1p3`.

### extend f64

#### f64.toBits

```gojo
pub fn toBits(self) -> u64
```

The IEEE 754 bits of the value.

#### f64.fromBits

```gojo
pub fn fromBits(b: u64) -> f64
```

The value with the IEEE 754 bits `b`.

### extend f32

#### f32.toBits

```gojo
pub fn toBits(self) -> u32
```

The IEEE 754 bits of the value.

#### f32.fromBits

```gojo
pub fn fromBits(b: u32) -> f32
```

The value with the IEEE 754 bits `b`.

### extend rune

#### rune.isLetter

```gojo
pub fn isLetter(self) -> bool
```

Whether it is a letter (Unicode category L).

#### rune.isUpper

```gojo
pub fn isUpper(self) -> bool
```

Whether it is an upper-case letter (Lu).

#### rune.isLower

```gojo
pub fn isLower(self) -> bool
```

Whether it is a lower-case letter (Ll).

#### rune.isDigit

```gojo
pub fn isDigit(self) -> bool
```

Whether it is a decimal digit in any script (Nd): `'7'`, `'٧'`. For the
value of an ASCII digit, `toDigit`.

#### rune.isNumber

```gojo
pub fn isNumber(self) -> bool
```

Whether it is a number of any kind (N: digits, `'½'`, `'Ⅻ'`...).

#### rune.isAlphanumeric

```gojo
pub fn isAlphanumeric(self) -> bool
```

Whether it is a letter or a number.

#### rune.isPunct

```gojo
pub fn isPunct(self) -> bool
```

Whether it is punctuation (P).

#### rune.isWhitespace

```gojo
pub fn isWhitespace(self) -> bool
```

Whether it is white space (the White_Space property: space, tab,
newlines, no-break space...).

#### rune.isControl

```gojo
pub fn isControl(self) -> bool
```

Whether it is a control character (Cc).

#### rune.isSymbol

```gojo
pub fn isSymbol(self) -> bool
```

Whether it is a symbol (S: `'+'`, `'$'`, `'©'`...).

#### rune.isAscii

```gojo
pub fn isAscii(self) -> bool
```

Whether it is ASCII (0 to 127).

#### rune.isValid

```gojo
pub fn isValid(self) -> bool
```

Whether it is a Unicode scalar value: 0 to 0x10FFFF, not a surrogate.

#### rune.toUpper

```gojo
pub fn toUpper(self) -> rune
```

The upper-case form (simple case mapping), or the rune itself.

#### rune.toLower

```gojo
pub fn toLower(self) -> rune
```

The lower-case form (simple case mapping), or the rune itself.

#### rune.toDigit

```gojo
pub fn toDigit(self, radix: int) -> ?int
```

The value of the rune as a digit in base `radix` (2 to 36: `'0'`-`'9'`,
then `'a'`-`'z'` in either case), or none: `'f'.toDigit(16)` is 15.

#### rune.utf8Len

```gojo
pub fn utf8Len(self) -> int
```

The number of bytes of its UTF-8 encoding (3 for one that isn't valid,
which encodes as U+FFFD).

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

A copy in upper case, by Unicode simple case mapping (one character to
one: `"straße"` becomes `"STRAßE"`).

#### str.toLower

```gojo
pub fn toLower(self) -> String
```

A copy in lower case, by Unicode simple case mapping.

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

#### str.findFrom

```gojo
pub fn findFrom(self, sub: str, from: int) -> ?int
```

The byte index of the first occurrence of `sub` at or after byte `from`;
none when `from` is past the end.

#### str.runeAt

```gojo
pub fn runeAt(self, i: int) -> (rune, int)
```

The rune that starts at byte `i` and its length in bytes, to walk a
string with byte offsets: an invalid byte reads as U+FFFD, 1 byte long.

```gojo
var i = 0
while i < s.len() { let (r, n) = s.runeAt(i); ...; i += n }
```

#### str.rfind

```gojo
pub fn rfind(self, sub: str) -> ?int
```

The byte index of the last occurrence of `sub`.

#### str.count

```gojo
pub fn count(self, sub: str) -> int
```

How many times `sub` occurs, without overlaps: `"aaaa".count("aa")` is 2.
For an empty `sub`, the number of characters plus one.

#### str.splitN

```gojo
pub fn splitN(self, sep: str, n: int) -> Vec[String]
```

Like `split`, but into at most `n` pieces, the last holding the rest:
`"a=b=c".splitN("=", 2)` is `["a", "b=c"]`. Nothing for `n` <= 0.

#### str.splitOnce

```gojo
pub fn splitOnce(self, sep: str) -> ?(String, String)
```

The parts before and after the first `sep`, or none when it doesn't
occur: `"key=value".splitOnce("=")` is `("key", "value")`. (For views
without copies, slice at `find`: `s[..i]`, `s[i + sep.len()..]`.)

#### str.trimPrefix

```gojo
pub fn trimPrefix(self, prefix: str) -> str
```

The string without `prefix` at the start (unchanged when it doesn't start
with it). A view: no copy.

#### str.trimSuffix

```gojo
pub fn trimSuffix(self, suffix: str) -> str
```

The string without `suffix` at the end (unchanged when it doesn't end
with it). A view: no copy.

#### str.trimChars

```gojo
pub fn trimChars(self, chars: str) -> str
```

The string without the characters in `chars` at either end:
`"--a-b--".trimChars("-")` is `"a-b"`. A view: no copy.

#### str.equalFold

```gojo
pub fn equalFold(self, other: str) -> bool
```

Whether the strings are equal ignoring case, under Unicode simple case
folding: `"Straße".equalFold("STRASSE")` is false (ß folds to itself),
`"Σίσυφος".equalFold("ΣΊΣΥΦΟΣ")` is true. No copies.

#### str.runeCount

```gojo
pub fn runeCount(self) -> int
```

The number of Unicode code points (`runes().len()` without the copy).

#### str.isAscii

```gojo
pub fn isAscii(self) -> bool
```

Whether every byte is ASCII (below 0x80).

#### str.isValidUtf8

```gojo
pub fn isValidUtf8(self) -> bool
```

Whether the bytes are valid UTF-8. Strings made with `String(bytes)` or
read from files aren't checked.

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

#### String.insert

```gojo
pub fn insert(inout self, at: int, t: str)
```

Inserts `t` at byte index `at`; panics when `at` is out of range or not
at a character boundary.

#### String.truncate

```gojo
pub fn truncate(inout self, n: int)
```

Keeps the first `n` bytes; nothing happens when `n` >= `len()`. Panics
when `n` is negative or not at a character boundary.

#### String.pop

```gojo
pub fn pop(inout self) -> ?rune
```

Removes and returns the last character, or none when empty.

#### String.reserve

```gojo
pub fn reserve(inout self, n: int)
```

Makes room for `n` more bytes without reallocating.

#### String.cap

```gojo
pub fn cap(self) -> int
```

The number of bytes there is room for.

### extend []T

#### []T.len

```gojo
pub fn len(self) -> int where T: any
```

The number of elements.

#### []T.reverse

```gojo
pub fn reverse(inout self) where T: any
```

Reverses the order in place. Only an `inout []T` (or a range of a
`var` Vec or array, `v[a..b]`) can be changed.

#### []T.swap

```gojo
pub fn swap(inout self, i: int, j: int) where T: any
```

Exchanges elements `i` and `j`; panics when either is out of range.

#### []T.sort

```gojo
pub fn sort(inout self) where T: ordered
```

Sorts in ascending order. Stable; O(n log n).

#### []T.sortBy

```gojo
pub fn sortBy(inout self, less: fn(T, T) -> bool) where T: any
```

Sorts by `less(a, b)`: true when `a` goes before `b`. Stable.

#### []T.ptr

```gojo
pub fn ptr(self) -> *T where T: any
```

raw pointer to the first element, for FFI

#### []T.isEmpty

```gojo
pub fn isEmpty(self) -> bool where T: any
```

Whether there are no elements.

#### []T.get

```gojo
pub fn get(self, i: int) yield ?&T where T: any
```

Element `i`, or none when `i` is out of range (indexing with `xs[i]`
panics instead).

#### []T.first

```gojo
pub fn first(self) yield ?&T where T: any
```

The first element, if any.

#### []T.last

```gojo
pub fn last(self) yield ?&T where T: any
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
pub fn each(self, f: fn(T)) where T: any
```

Calls `f` on each element, in order.

#### []T.any

```gojo
pub fn any(self, f: fn(T) -> bool) -> bool where T: any
```

Whether `f` is true for some element (stops at the first).

#### []T.all

```gojo
pub fn all(self, f: fn(T) -> bool) -> bool where T: any
```

Whether `f` is true for every element (stops at the first false).

#### []T.count

```gojo
pub fn count(self, f: fn(T) -> bool) -> int where T: any
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

#### []T.find

```gojo
pub fn find(self, pred: fn(T) -> bool) yield ?&T where T: any
```

The first element `pred` is true for.

#### []T.findIndex

```gojo
pub fn findIndex(self, pred: fn(T) -> bool) -> ?int where T: any
```

The index of the first element `pred` is true for.

#### []T.lastIndexOf

```gojo
pub fn lastIndexOf(self, x: T) -> ?int where T: comparable
```

The index of the last element equal to `x`.

#### []T.binarySearch

```gojo
pub fn binarySearch(self, x: T) -> ?int where T: ordered
```

The index of an element equal to `x` in a sorted slice, found in
O(log n), or none. (With duplicates, any of them.)

#### []T.partitionPoint

```gojo
pub fn partitionPoint(self, pred: fn(T) -> bool) -> int where T: any
```

The index of the first element `pred` is false for, in a slice where
`pred` is true for a prefix and false after it; O(log n). With
`x => x < v` on a sorted slice, where `v` would be inserted.

#### []T.isSorted

```gojo
pub fn isSorted(self) -> bool where T: ordered
```

Whether the elements are in ascending order.

#### []T.sortByKey

```gojo
pub fn sortByKey[K: ordered](inout self, key: fn(T) -> K) where T: any
```

Sorts by the key `key` gives for each element, ascending. Stable.

#### []T.minBy

```gojo
pub fn minBy[K: ordered](self, key: fn(T) -> K) yield ?&T where T: any
```

The element with the smallest key (the first of equal ones), or none
for no elements.

#### []T.maxBy

```gojo
pub fn maxBy[K: ordered](self, key: fn(T) -> K) yield ?&T where T: any
```

The element with the largest key (the first of equal ones), or none
for no elements.

#### []T.startsWith

```gojo
pub fn startsWith(self, prefix: []T) -> bool where T: comparable
```

Whether the slice begins with the elements of `prefix`.

#### []T.endsWith

```gojo
pub fn endsWith(self, suffix: []T) -> bool where T: comparable
```

Whether the slice ends with the elements of `suffix`.

#### []T.fill

```gojo
pub fn fill(inout self, x: T) where T: clone
```

Sets every element to a copy of `x`.

#### []T.rotateLeft

```gojo
pub fn rotateLeft(inout self, n: int) where T: any
```

Moves every element `n` places towards the front, the first `n` going
to the end: `[1, 2, 3, 4]` becomes `[3, 4, 1, 2]` for `n` = 2. Any `n`
works (negative rotates the other way).

#### []T.rotateRight

```gojo
pub fn rotateRight(inout self, n: int) where T: any
```

Moves every element `n` places towards the end, the last `n` going to
the front.

#### []T.filterMap

```gojo
pub fn filterMap[U](self, f: fn(T) -> ?U) -> Vec[U]
```

The results of `f` for each element that gives one, in order: a map and
a filter in one pass.

```gojo
let ports = fields.filterMap(f => f.parseInt())
```

#### []T.flatMap

```gojo
pub fn flatMap[U](self, f: fn(T) -> Vec[U]) -> Vec[U]
```

The elements of all the `Vec`s `f` gives, in order.

#### []T.partition

```gojo
pub fn partition(self, pred: fn(T) -> bool) -> (Vec[T], Vec[T]) where T: clone
```

Copies of the elements split in two: those `pred` is true for, and the
rest. Both keep their order.

#### []T.groupBy

```gojo
pub fn groupBy[K: hash](self, key: fn(T) -> K) -> Map[K, Vec[T]] where T: clone
```

Copies of the elements grouped by the key `key` gives; groups are in the
order their keys first appear, and keep the elements' order.

#### []T.zip

```gojo
pub fn zip[U](self, other: []U) -> Vec[(T, U)] where T: clone, U: clone
```

Pairs of copies of the elements at the same index, as many as the
shorter slice has.

#### []T.chunks

```gojo
pub fn chunks(self, size: int) -> Vec[Vec[T]] where T: clone
```

Copies of the elements in pieces of `size` (the last may be shorter):
`[1, 2, 3, 4, 5].chunks(2)` is `[[1, 2], [3, 4], [5]]`. Panics when
`size` < 1. (To work on the pieces in place, slice them:
`xs[i..min(i + size, xs.len())]`.)

#### []T.windows

```gojo
pub fn windows(self, size: int) -> Vec[Vec[T]] where T: clone
```

Copies of every run of `size` neighbouring elements:
`[1, 2, 3, 4].windows(3)` is `[[1, 2, 3], [2, 3, 4]]`. Panics when
`size` < 1.

### extend []Vec[T]

#### []Vec[T].flatten

```gojo
pub fn flatten(self) -> Vec[T] where T: clone
```

Copies of the elements of every `Vec`, one after the other.

### extend []String

#### []String.join

```gojo
pub fn join(self, sep: str) -> String
```

The strings with `sep` between them: `["a", "b"].join(", ")` is `"a, b"`.

### extend Vec[T]

#### Vec[T].push

```gojo
pub fn push(inout self, x: sink T) where T: any
```

Appends `x`.

#### Vec[T].pop

```gojo
pub fn pop(inout self) -> ?T where T: any
```

Removes and returns the last element, or none when empty.

#### Vec[T].insert

```gojo
pub fn insert(inout self, i: int, x: sink T) where T: any
```

Inserts `x` at index `i` (0 <= `i` <= `len()`), moving the later elements
up; panics otherwise.

#### Vec[T].remove

```gojo
pub fn remove(inout self, i: int) -> T where T: any
```

Removes and returns element `i`, moving the later elements down; panics when
`i` is out of range.

#### Vec[T].swapRemove

```gojo
pub fn swapRemove(inout self, i: int) -> T where T: any
```

Removes and returns element `i`, putting the last element in its place:
O(1), but changes the order. Panics when `i` is out of range.

#### Vec[T].finish

```gojo
pub fn finish(sink self) where T: any
```

Ends an empty vector of linear values (spec 5.7): a vector that holds
them must be consumed like they are, so pop each one and consume it,
then call `finish`. Panics when the vector still holds values.

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
pub fn extend(inout self, other: sink Vec[T]) where T: any
```

Moves the elements of `other` to the end.

#### Vec[T].reverse

```gojo
pub fn reverse(inout self) where T: any
```

Reverses the order in place.

#### Vec[T].swap

```gojo
pub fn swap(inout self, i: int, j: int) where T: any
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
pub fn reserve(inout self, n: int) where T: any
```

Makes room for `n` more elements without reallocating; panics ("capacity
overflow") for an impossible size.

#### Vec[T].cap

```gojo
pub fn cap(self) -> int where T: any
```

The number of elements there is room for.

#### Vec[T].retain

```gojo
pub fn retain(inout self, keep: fn(T) -> bool)
```

Keeps the elements `keep` is true for, in order, and drops the rest; in
place, O(n), no copies.

#### Vec[T].dedup

```gojo
pub fn dedup(inout self) where T: comparable
```

Removes consecutive repeated elements, keeping the first of each run:
`[1, 1, 2, 1, 1]` becomes `[1, 2, 1]` (sort first to remove every
duplicate).

#### Vec[T].drain

```gojo
pub fn drain(inout self, lo: int, hi: int) -> Vec[T] where T: any
```

Removes the elements from index `lo` up to (not including) `hi` and
returns them, in order; panics unless 0 <= `lo` <= `hi` <= `len()`.

#### Vec[T].splitOff

```gojo
pub fn splitOff(inout self, at: int) -> Vec[T] where T: any
```

Splits in two at index `at`: keeps the elements before it and returns
the rest. Panics unless 0 <= `at` <= `len()`.

#### Vec[T].resize

```gojo
pub fn resize(inout self, n: int, x: T) where T: clone
```

Grows to `n` elements with copies of `x`, or shrinks to `n`.

#### Vec[T].extendFromSlice

```gojo
pub fn extendFromSlice(inout self, xs: []T) where T: clone
```

Appends copies of the elements of `xs`.

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

#### Map[K, V].getOr

```gojo
pub fn getOr(self, k: K, d: sink V) -> V where V: clone
```

A copy of the value for `k`, or `d` when there is none.

#### Map[K, V].retain

```gojo
pub fn retain(inout self, keep: fn(K, V) -> bool) where K: clone
```

Keeps the entries `keep` is true for and removes the rest; the kept
ones keep their order.

#### Map[K, V].entries

```gojo
pub fn entries(self) -> Vec[(K, V)] where K: clone, V: clone
```

Copies of the entries as (key, value) pairs, in insertion order.

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

