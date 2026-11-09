# hex

Hexadecimal: bytes as pairs of hex digits, and back.

```gojo
hex.encode("hi".bytes())          // "6869"
let key = try hex.decode("DEADbeef")
```

**Types:** [`struct Error`](#struct-error)

**Functions:** [`encode`](#encode), [`encodeUpper`](#encodeupper), [`decode`](#decode)

## Types

### struct Error

```gojo
pub struct Error {
    pub msg: String
    // the byte offset of the problem in the text
    pub offset: int
}
```

Text that isn't hex: a character that isn't a hex digit, or an odd number
of digits.

#### Error.message

```gojo
pub fn message(self) -> String
```

## Functions

### encode

```gojo
pub fn encode(data: []byte) -> String
```

The bytes as lower-case hex digits, two per byte.

### encodeUpper

```gojo
pub fn encodeUpper(data: []byte) -> String
```

The bytes as upper-case hex digits.

### decode

```gojo
pub fn decode(s: str) -> Vec[byte] ! Error
```

The bytes the hex digits stand for (in either case).

