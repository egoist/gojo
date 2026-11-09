# simd

Fixed-width vectors of numbers, for code that works on several values at
once: `Simd[f32, 8]` holds 8 lanes. The operations go lane by lane over
a fixed-size array, which the C compiler turns into vector instructions
(SSE/AVX, NEON) in release builds; the API is the same on every target.

```gojo
use std/simd.{Simd}
let a = Simd[f32, 4].load(xs, 0)
let b = Simd[f32, 4].splat(2.0)
println(a.mul(b).sum())
```

Arithmetic is methods (`a.add(b)`), not operators: Gojo keeps `+` for
numbers. Comparisons give a `Mask`, used with `select`.

**Types:** [`struct Simd`](#struct-simd), [`struct Mask`](#struct-mask)

## Types

### struct Simd

```gojo
pub copy struct Simd[T: Number, N: int] {
    pub lanes: [N]T
}
```

`N` lanes of `T`.

#### Simd.splat

```gojo
pub fn splat(x: T) -> Simd[T, N]
```

Every lane `x`.

#### Simd.load

```gojo
pub fn load(xs: []T, at: int) -> Simd[T, N]
```

Lanes `xs[at]`, `xs[at + 1]`... `xs[at + N - 1]`; panics when they
aren't all in `xs`.

#### Simd.store

```gojo
pub fn store(self, xs: inout []T, at: int)
```

Writes the lanes to `xs[at..at + N]`; panics when they don't fit.

#### Simd.len

```gojo
pub fn len(self) -> int
```

The number of lanes.

#### Simd.get

```gojo
pub fn get(self, i: int) -> T
```

Lane `i`.

#### Simd.set

```gojo
pub fn set(inout self, i: int, x: T)
```

Sets lane `i`.

#### Simd.add

```gojo
pub fn add(self, o: Simd[T, N]) -> Simd[T, N]
```

Lane-wise `+`, `-`, `*`, `/` (integer lanes: `/` panics on a zero
lane, and every lane is checked for overflow).

#### Simd.sub

```gojo
pub fn sub(self, o: Simd[T, N]) -> Simd[T, N]
```

#### Simd.mul

```gojo
pub fn mul(self, o: Simd[T, N]) -> Simd[T, N]
```

#### Simd.div

```gojo
pub fn div(self, o: Simd[T, N]) -> Simd[T, N]
```

#### Simd.min

```gojo
pub fn min(self, o: Simd[T, N]) -> Simd[T, N]
```

Lane-wise smaller and larger.

#### Simd.max

```gojo
pub fn max(self, o: Simd[T, N]) -> Simd[T, N]
```

#### Simd.mulAdd

```gojo
pub fn mulAdd(self, a: Simd[T, N], b: Simd[T, N]) -> Simd[T, N]
```

`self * a + b`, lane by lane.

#### Simd.sum

```gojo
pub fn sum(self) -> T
```

The sum of the lanes.

#### Simd.hmin

```gojo
pub fn hmin(self) -> T
```

The smallest and largest lane.

#### Simd.hmax

```gojo
pub fn hmax(self) -> T
```

#### Simd.dot

```gojo
pub fn dot(self, o: Simd[T, N]) -> T
```

The sum of the lane-wise products.

#### Simd.lt

```gojo
pub fn lt(self, o: Simd[T, N]) -> Mask[N]
```

Lane-wise comparisons.

#### Simd.le

```gojo
pub fn le(self, o: Simd[T, N]) -> Mask[N]
```

#### Simd.eq

```gojo
pub fn eq(self, o: Simd[T, N]) -> Mask[N]
```

#### Simd.select

```gojo
pub fn select(m: Mask[N], a: Simd[T, N], b: Simd[T, N]) -> Simd[T, N]
```

`a`'s lane where the mask is set, `b`'s where it isn't.

### struct Mask

```gojo
pub copy struct Mask[N: int] {
    pub lanes: [N]bool
}
```

`N` yes/no lanes: what lane-wise comparisons give.

#### Mask.any

```gojo
pub fn any(self) -> bool
```

Whether any lane, every lane is set.

#### Mask.all

```gojo
pub fn all(self) -> bool
```

#### Mask.count

```gojo
pub fn count(self) -> int
```

The number of lanes set.

#### Mask.bits

```gojo
pub fn bits(self) -> u64
```

The lanes as bits: lane 0 is the lowest. `N` must be at most 64.

