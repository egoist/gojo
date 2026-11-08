# math

Floating-point math and integer limits.

**Constants:** [`Pi`](#pi), [`E`](#e), [`MaxInt, MinInt`](#maxint-minint), [`MaxI8, MinI8, MaxI16, MinI16, MaxI32, MinI32, MaxU8, MaxU16, MaxU32, MaxU64`](#maxi8-mini8-maxi16-mini16-maxi32-mini32-maxu8-maxu16-maxu32-maxu64), [`MaxF64, MinPositiveF64, EpsilonF64`](#maxf64-minpositivef64-epsilonf64), [`Sqrt2, Ln2, Ln10`](#sqrt2-ln2-ln10)

**Functions:** [`sqrt`](#sqrt), [`pow`](#pow), [`abs`](#abs), [`floor, ceil, round`](#floor-ceil-round), [`trunc`](#trunc), [`cbrt`](#cbrt), [`hypot`](#hypot), [`fmod`](#fmod), [`sin, cos, tan`](#sin-cos-tan), [`asin, acos, atan`](#asin-acos-atan), [`atan2`](#atan2), [`sinh, cosh, tanh`](#sinh-cosh-tanh), [`log, exp`](#log-exp), [`log2, log10`](#log2-log10), [`log1p, expm1`](#log1p-expm1), [`isNaN`](#isnan), [`isInf, isFinite`](#isinf-isfinite), [`inf, nan`](#inf-nan), [`toRadians`](#toradians), [`toDegrees`](#todegrees), [`lerp`](#lerp), [`gcd`](#gcd), [`lcm`](#lcm)

## Constants

### Pi

```gojo
pub const Pi = 3.141592653589793
```

π, the ratio of a circle's circumference to its diameter.

### E

```gojo
pub const E = 2.718281828459045
```

e, the base of natural logarithms.

### MaxInt, MinInt

```gojo
pub const MaxInt = 9223372036854775807
pub const MinInt = -9223372036854775808
```

The largest and smallest `int`.

### MaxI8, MinI8, MaxI16, MinI16, MaxI32, MinI32, MaxU8, MaxU16, MaxU32, MaxU64

```gojo
pub const MaxI8: i8 = 127
pub const MinI8: i8 = -128
pub const MaxI16: i16 = 32767
pub const MinI16: i16 = -32768
pub const MaxI32: i32 = 2147483647
pub const MinI32: i32 = -2147483648
pub const MaxU8: u8 = 255
pub const MaxU16: u16 = 65535
pub const MaxU32: u32 = 4294967295
pub const MaxU64: u64 = 18446744073709551615
```

The limits of the sized integer types.

### MaxF64, MinPositiveF64, EpsilonF64

```gojo
pub const MaxF64 = 1.7976931348623157e308
pub const MinPositiveF64 = 2.2250738585072014e-308
pub const EpsilonF64 = 2.220446049250313e-16
```

The largest finite `f64`, the smallest positive normal one, and the gap
between 1.0 and the next `f64` up.

### Sqrt2, Ln2, Ln10

```gojo
pub const Sqrt2 = 1.4142135623730951
pub const Ln2 = 0.6931471805599453
pub const Ln10 = 2.302585092994046
```

The square root of 2, and the natural logarithms of 2 and 10.

## Functions

### sqrt

```gojo
pub fn sqrt(x: f64) -> f64
```

The square root; NaN for negative numbers.

### pow

```gojo
pub fn pow(x: f64, y: f64) -> f64
```

`x` to the power `y`.

### abs

```gojo
pub fn abs(x: f64) -> f64
```

The absolute value.

### floor, ceil, round

```gojo
pub fn floor(x: f64) -> f64
pub fn ceil(x: f64) -> f64
pub fn round(x: f64) -> f64
```

Rounds down, up, and to the nearest integer (halves away from zero).

### trunc

```gojo
pub fn trunc(x: f64) -> f64
```

Rounds towards zero.

### cbrt

```gojo
pub fn cbrt(x: f64) -> f64
```

The cube root.

### hypot

```gojo
pub fn hypot(x: f64, y: f64) -> f64
```

`sqrt(x*x + y*y)`, without overflow or underflow in between.

### fmod

```gojo
pub fn fmod(x: f64, y: f64) -> f64
```

The remainder of `x / y`, with the sign of `x` (C's `fmod`).

### sin, cos, tan

```gojo
pub fn sin(x: f64) -> f64
pub fn cos(x: f64) -> f64
pub fn tan(x: f64) -> f64
```

Trigonometric functions, in radians.

### asin, acos, atan

```gojo
pub fn asin(x: f64) -> f64
pub fn acos(x: f64) -> f64
pub fn atan(x: f64) -> f64
```

Their inverses: `asin` and `acos` take [-1, 1], `atan` gives (-π/2, π/2).

### atan2

```gojo
pub fn atan2(y: f64, x: f64) -> f64
```

The angle of the point (x, y) from the x axis, in (-π, π].

### sinh, cosh, tanh

```gojo
pub fn sinh(x: f64) -> f64
pub fn cosh(x: f64) -> f64
pub fn tanh(x: f64) -> f64
```

Hyperbolic functions.

### log, exp

```gojo
pub fn log(x: f64) -> f64
pub fn exp(x: f64) -> f64
```

The natural logarithm, and e to the power `x`.

### log2, log10

```gojo
pub fn log2(x: f64) -> f64
pub fn log10(x: f64) -> f64
```

Base-2 and base-10 logarithms.

### log1p, expm1

```gojo
pub fn log1p(x: f64) -> f64
pub fn expm1(x: f64) -> f64
```

`log(1 + x)` and `exp(x) - 1`, accurate for `x` near 0.

### isNaN

```gojo
pub fn isNaN(x: f64) -> bool
```

Whether `x` is not a number (the result of `0.0 / 0.0`, `sqrt(-1.0)`).

### isInf, isFinite

```gojo
pub fn isInf(x: f64) -> bool
pub fn isFinite(x: f64) -> bool
```

Whether `x` is +Inf or -Inf; whether it is neither infinite nor NaN.

### inf, nan

```gojo
pub fn inf() -> f64
pub fn nan() -> f64
```

Positive infinity, and a NaN.

### toRadians

```gojo
pub fn toRadians(deg: f64) -> f64
```

Converts degrees to radians, and radians to degrees.

### toDegrees

```gojo
pub fn toDegrees(rad: f64) -> f64
```

### lerp

```gojo
pub fn lerp(a: f64, b: f64, t: f64) -> f64
```

`a + (b - a) * t`: the point a fraction `t` of the way from `a` to `b`.

### gcd

```gojo
pub fn gcd(a: int, b: int) -> int
```

The greatest common divisor of `a` and `b` (never negative; 0 when both are 0).

### lcm

```gojo
pub fn lcm(a: int, b: int) -> int
```

The least common multiple of `a` and `b` (never negative; 0 when either is 0).
Panics when it doesn't fit in an `int`.

