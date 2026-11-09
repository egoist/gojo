# rand

Random numbers: fast pseudo-random values for simulations, sampling and
shuffling, a seeded generator for reproducible sequences, and
cryptographically secure bytes from the operating system.

```gojo
let roll = rand.range(1, 7)            // 1 to 6
rand.shuffle(&deck)
let token = hex.encode(rand.secureBytes(16))

var r = rand.Rng.new(42)               // the same numbers every run
let x = r.float()
```

The functions without a generator use one per thread, seeded from the
operating system, so tasks don't contend for it. They are not for secrets
(keys, tokens, passwords): use `secureBytes` / `secureU64` for those.
Under `GOJO_SEED` (and in `gojo test`), they are seeded from it, so a
run can be replayed.

**Types:** [`interface Source`](#interface-source), [`struct Rng`](#struct-rng)

**Functions:** [`u64`](#u64), [`range`](#range), [`below`](#below), [`float`](#float), [`bool`](#bool), [`shuffle`](#shuffle), [`choice`](#choice), [`secureBytes`](#securebytes), [`secureU64`](#secureu64)

## Types

### interface Source

```gojo
pub interface Source {
    fn u64(inout self) -> u64
}
```

What the sampling functions draw from: 64 random bits at a time.

### struct Rng

```gojo
pub struct Rng {
    // and private fields
}
```

A seeded pseudo-random generator (xoshiro256**, 256 bits of state): fast,
and the same seed always gives the same sequence. Not for secrets.

#### Rng.new

```gojo
pub fn new(seed: u64) -> Rng
```

A generator seeded with `seed` (the state is spread from it with
SplitMix64, so nearby seeds give unrelated sequences).

#### Rng.u64

```gojo
pub fn u64(inout self) -> u64
```

64 random bits.

#### Rng.range

```gojo
pub fn range(inout self, lo: int, hi: int) -> int
```

A uniformly random integer in `lo` up to (not including) `hi`; panics
unless `lo` < `hi`.

#### Rng.below

```gojo
pub fn below(inout self, n: int) -> int
```

A uniformly random integer from 0 up to (not including) `n`; panics
unless `n` > 0.

#### Rng.float

```gojo
pub fn float(inout self) -> f64
```

A uniformly random float in [0, 1).

#### Rng.bool

```gojo
pub fn bool(inout self) -> bool
```

true or false, with equal chances.

#### Rng.shuffle

```gojo
pub fn shuffle[T](inout self, xs: inout []T)
```

Puts the elements in a uniformly random order (Fisher-Yates).

#### Rng.choice

```gojo
pub fn choice[T](inout self, xs: []T) yield ?&T
```

A uniformly random element, or none for no elements.

## Functions

### u64

```gojo
pub fn u64() -> u64
```

64 random bits.

### range

```gojo
pub fn range(lo: int, hi: int) -> int
```

A uniformly random integer in `lo` up to (not including) `hi`; panics
unless `lo` < `hi`.

### below

```gojo
pub fn below(n: int) -> int
```

A uniformly random integer from 0 up to (not including) `n`; panics unless
`n` > 0.

### float

```gojo
pub fn float() -> f64
```

A uniformly random float in [0, 1).

### bool

```gojo
pub fn bool() -> bool
```

true or false, with equal chances.

### shuffle

```gojo
pub fn shuffle[T](xs: inout []T)
```

Puts the elements in a uniformly random order (Fisher-Yates).

### choice

```gojo
pub fn choice[T](xs: []T) yield ?&T
```

A uniformly random element, or none for no elements.

### secureBytes

```gojo
pub fn secureBytes(n: int) -> Vec[byte]
```

`n` cryptographically secure random bytes from the operating system
(`getrandom` / `getentropy`): for keys, tokens, nonces and salts. Panics
if the system can't provide them, which modern systems always can.

### secureU64

```gojo
pub fn secureU64() -> u64
```

64 cryptographically secure random bits.

