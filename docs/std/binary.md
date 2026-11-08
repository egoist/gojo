# binary

Fixed-size numbers as bytes, in either byte order, and variable-length
integers (varints, as in Protocol Buffers and Go's encoding/binary).

```gojo
var out = Vec[byte]{}
binary.big.appendU32(&out, 0xCAFEBABE)
binary.appendUvarint(&out, 300)
let magic = binary.big.u32(out)               // 0xCAFEBABE
let (n, size) = binary.uvarint(out[4..]) ?? (0, 0)
```

Readers take the first bytes of a slice (slice it to read further on) and
panic when it is too short, like indexing.

**Constants:** [`let big`](#let-big), [`let little`](#let-little), [`maxVarintLen`](#maxvarintlen)

**Types:** [`enum Order`](#enum-order)

**Functions:** [`appendUvarint`](#appenduvarint), [`appendVarint`](#appendvarint), [`uvarint`](#uvarint), [`varint`](#varint)

## Constants

### let big

```gojo
pub let big = Order.Big
```

Shorthands: `binary.big.u32(b)`, `binary.little.appendU16(&out, v)`.

### let little

```gojo
pub let little = Order.Little
```

### maxVarintLen

```gojo
pub const maxVarintLen = 10
```

The most bytes a 64-bit varint takes.

## Types

### enum Order

```gojo
pub copy enum Order {
    Big
    Little
}
```

The order of a number's bytes: most significant first (network order) or
least significant first (x86, ARM, most file formats).

#### Order.u16

```gojo
pub fn u16(self, b: []byte) -> u16
```

The u16 in the first 2 bytes of `b`.

#### Order.u32

```gojo
pub fn u32(self, b: []byte) -> u32
```

The u32 in the first 4 bytes of `b`.

#### Order.u64

```gojo
pub fn u64(self, b: []byte) -> u64
```

The u64 in the first 8 bytes of `b`.

#### Order.i16

```gojo
pub fn i16(self, b: []byte) -> i16
```

The i16 in the first 2 bytes of `b`.

#### Order.i32

```gojo
pub fn i32(self, b: []byte) -> i32
```

The i32 in the first 4 bytes of `b`.

#### Order.i64

```gojo
pub fn i64(self, b: []byte) -> i64
```

The i64 in the first 8 bytes of `b`.

#### Order.f32

```gojo
pub fn f32(self, b: []byte) -> f32
```

The f32 in the first 4 bytes of `b`.

#### Order.f64

```gojo
pub fn f64(self, b: []byte) -> f64
```

The f64 in the first 8 bytes of `b`.

#### Order.putU16

```gojo
pub fn putU16(self, b: inout []byte, v: u16)
```

Writes `v` into the first 2 bytes of `b`.

#### Order.putU32

```gojo
pub fn putU32(self, b: inout []byte, v: u32)
```

Writes `v` into the first 4 bytes of `b`.

#### Order.putU64

```gojo
pub fn putU64(self, b: inout []byte, v: u64)
```

Writes `v` into the first 8 bytes of `b`.

#### Order.putI16

```gojo
pub fn putI16(self, b: inout []byte, v: i16)
```

Writes `v` into the first 2 bytes of `b`.

#### Order.putI32

```gojo
pub fn putI32(self, b: inout []byte, v: i32)
```

Writes `v` into the first 4 bytes of `b`.

#### Order.putI64

```gojo
pub fn putI64(self, b: inout []byte, v: i64)
```

Writes `v` into the first 8 bytes of `b`.

#### Order.putF32

```gojo
pub fn putF32(self, b: inout []byte, v: f32)
```

Writes `v` into the first 4 bytes of `b`.

#### Order.putF64

```gojo
pub fn putF64(self, b: inout []byte, v: f64)
```

Writes `v` into the first 8 bytes of `b`.

#### Order.appendU16

```gojo
pub fn appendU16(self, out: inout Vec[byte], v: u16)
```

Appends the 2 bytes of `v` to `out`.

#### Order.appendU32

```gojo
pub fn appendU32(self, out: inout Vec[byte], v: u32)
```

Appends the 4 bytes of `v` to `out`.

#### Order.appendU64

```gojo
pub fn appendU64(self, out: inout Vec[byte], v: u64)
```

Appends the 8 bytes of `v` to `out`.

#### Order.appendI16

```gojo
pub fn appendI16(self, out: inout Vec[byte], v: i16)
```

Appends the 2 bytes of `v` to `out`.

#### Order.appendI32

```gojo
pub fn appendI32(self, out: inout Vec[byte], v: i32)
```

Appends the 4 bytes of `v` to `out`.

#### Order.appendI64

```gojo
pub fn appendI64(self, out: inout Vec[byte], v: i64)
```

Appends the 8 bytes of `v` to `out`.

#### Order.appendF32

```gojo
pub fn appendF32(self, out: inout Vec[byte], v: f32)
```

Appends the 4 bytes of `v` to `out`.

#### Order.appendF64

```gojo
pub fn appendF64(self, out: inout Vec[byte], v: f64)
```

Appends the 8 bytes of `v` to `out`.

## Functions

### appendUvarint

```gojo
pub fn appendUvarint(out: inout Vec[byte], v: u64)
```

Appends `v` as an unsigned varint: 7 bits per byte, low bits first, the
top bit set on every byte but the last. 1 byte below 128, 10 at most.

### appendVarint

```gojo
pub fn appendVarint(out: inout Vec[byte], v: i64)
```

Appends `v` as a signed varint, zig-zag encoded so that numbers near zero
either side are short: 0, -1, 1, -2 become 0, 1, 2, 3.

### uvarint

```gojo
pub fn uvarint(b: []byte) -> ?(u64, int)
```

The unsigned varint at the start of `b` and how many bytes it took; none
if `b` ends inside it or it doesn't fit in 64 bits.

### varint

```gojo
pub fn varint(b: []byte) -> ?(i64, int)
```

The signed (zig-zag) varint at the start of `b` and how many bytes it
took; none as for `uvarint`.

