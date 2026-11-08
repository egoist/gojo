# base64

Base64 (RFC 4648): bytes as text, in the standard alphabet (with `+`, `/`
and `=` padding) or the URL-safe one (with `-`, `_` and no padding, as in
JWTs).

```gojo
let s = base64.encode("hello".bytes())   // "aGVsbG8="
let b = try base64.decode(s)
```

**Types:** [`struct Error`](#struct-error)

**Functions:** [`encode`](#encode), [`encodeUrl`](#encodeurl), [`decode`](#decode), [`decodeUrl`](#decodeurl)

## Types

### struct Error

```gojo
pub struct Error {
    // the byte offset of the problem in the text
    pub offset: int
}
```

Text that isn't valid base64.

#### Error.message

```gojo
pub fn message(self) -> String
```

## Functions

### encode

```gojo
pub fn encode(data: []byte) -> String
```

The bytes in the standard alphabet, padded with `=`.

### encodeUrl

```gojo
pub fn encodeUrl(data: []byte) -> String
```

The bytes in the URL-safe alphabet, without padding.

### decode

```gojo
pub fn decode(s: str) -> Vec[byte] ! Error
```

The bytes of standard base64 text. Padding may be left out; if it is
there it must be right. Line breaks and other characters are errors.

### decodeUrl

```gojo
pub fn decodeUrl(s: str) -> Vec[byte] ! Error
```

The bytes of URL-safe base64 text (padded or not).

