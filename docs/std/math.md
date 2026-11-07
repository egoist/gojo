# math

Floating-point math and integer limits.

**Constants:** [`Pi`](#pi), [`E`](#e), [`MaxInt, MinInt`](#maxint-minint)

**Functions:** [`sqrt`](#sqrt), [`pow`](#pow), [`abs`](#abs), [`floor, ceil, round`](#floor-ceil-round), [`sin, cos, tan`](#sin-cos-tan), [`atan2`](#atan2), [`log, exp`](#log-exp), [`isNaN`](#isnan), [`inf`](#inf)

## Constants

### Pi

```zig
pub const Pi = 3.141592653589793
```

π, the ratio of a circle's circumference to its diameter.

### E

```zig
pub const E = 2.718281828459045
```

e, the base of natural logarithms.

### MaxInt, MinInt

```zig
pub const MaxInt = 9223372036854775807
pub const MinInt = -9223372036854775808
```

The largest and smallest `int`.

## Functions

### sqrt

```zig
pub fn sqrt(x f64) f64
```

The square root; NaN for negative numbers.

### pow

```zig
pub fn pow(x f64, y f64) f64
```

`x` to the power `y`.

### abs

```zig
pub fn abs(x f64) f64
```

The absolute value.

### floor, ceil, round

```zig
pub fn floor(x f64) f64
pub fn ceil(x f64) f64
pub fn round(x f64) f64
```

Rounds down, up, and to the nearest integer (halves away from zero).

### sin, cos, tan

```zig
pub fn sin(x f64) f64
pub fn cos(x f64) f64
pub fn tan(x f64) f64
```

Trigonometric functions, in radians.

### atan2

```zig
pub fn atan2(y f64, x f64) f64
```

The angle of the point (x, y) from the x axis, in (-π, π].

### log, exp

```zig
pub fn log(x f64) f64
pub fn exp(x f64) f64
```

The natural logarithm, and e to the power `x`.

### isNaN

```zig
pub fn isNaN(x f64) bool
```

Whether `x` is not a number (the result of `0.0 / 0.0`, `sqrt(-1.0)`).

### inf

```zig
pub fn inf() f64
```

Positive infinity.

