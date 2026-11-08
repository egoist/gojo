# crypto

Cryptographic hashes (SHA-2, and SHA-1 and MD5 for old formats), HMAC,
PBKDF2, and comparison in constant time.

```gojo
let sum = hex.encode(crypto.sha256("hello".bytes()))
let mac = crypto.hmac(.Sha256, key, body)
if !crypto.equal(mac, sent) { ... }       // never `==` for secrets

var d = crypto.Digest.new(.Sha256)       // streamed, e.g. a whole file:
_ = try io.copy(&d, &file)                 // a Digest is an io.Writer
let sum = d.finish()
```

Random keys and nonces: `rand.secureBytes`.

**Types:** [`enum Hash`](#enum-hash), [`struct Digest`](#struct-digest), [`struct Hmac`](#struct-hmac)

**Functions:** [`size`](#size), [`blockSize`](#blocksize), [`hash`](#hash), [`sha256`](#sha256), [`sha512`](#sha512), [`sha1`](#sha1), [`md5`](#md5), [`hmac`](#hmac), [`pbkdf2`](#pbkdf2), [`equal`](#equal)

## Types

### enum Hash

```gojo
pub copy enum Hash { Md5, Sha1, Sha256, Sha384, Sha512 }
```

The hash functions. MD5 and SHA-1 are broken for security: use them only
where a format requires them.

### struct Digest

```gojo
pub struct Digest {
    // and private fields
}
```

A hash computed piece by piece. It is an `io.Writer`, so `io.copy` can
stream a file or a connection into it.

#### Digest.new

```gojo
pub fn new(h: Hash) -> Digest
```

A digest with nothing hashed yet.

#### Digest.update

```gojo
pub fn update(inout self, data: []byte)
```

Adds `data` to what is hashed.

#### Digest.updateText

```gojo
pub fn updateText(inout self, s: str)
```

Adds the bytes of `s`.

#### Digest.write

```gojo
pub fn write(inout self, data: []byte) ! io.Error
```

For `io.Writer`: the same as `update`, and never fails.

#### Digest.finish

```gojo
pub fn finish(sink self) -> Vec[byte]
```

The hash of everything added (the digest is used up).

#### Digest.sum

```gojo
pub fn sum(self) -> Vec[byte]
```

The hash of everything added so far; more can be added afterwards.

### struct Hmac

```gojo
pub struct Hmac {
    // and private fields
}
```

An HMAC (RFC 2104) computed piece by piece: the hash keyed with `key`.

#### Hmac.new

```gojo
pub fn new(h: Hash, key: []byte) -> Hmac
```

An HMAC with hash `h` and secret `key` (any length).

#### Hmac.update

```gojo
pub fn update(inout self, data: []byte)
```

Adds `data` to what is authenticated.

#### Hmac.write

```gojo
pub fn write(inout self, data: []byte) ! io.Error
```

For `io.Writer`: the same as `update`.

#### Hmac.finish

```gojo
pub fn finish(sink self) -> Vec[byte]
```

The MAC of everything added (the HMAC is used up).

## Functions

### size

```gojo
pub fn size(h: Hash) -> int
```

The length of a hash's digest, in bytes: 16, 20, 32, 48 or 64.

### blockSize

```gojo
pub fn blockSize(h: Hash) -> int
```

The input block size of a hash, in bytes (64, or 128 for SHA-384/512).

### hash

```gojo
pub fn hash(h: Hash, data: []byte) -> Vec[byte]
```

The hash of `data`.

### sha256

```gojo
pub fn sha256(data: []byte) -> Vec[byte]
```

SHA-256 of `data`, the usual choice.

### sha512

```gojo
pub fn sha512(data: []byte) -> Vec[byte]
```

SHA-512 of `data`.

### sha1

```gojo
pub fn sha1(data: []byte) -> Vec[byte]
```

SHA-1 of `data` (broken for security; for formats like WebSocket handshakes
and git).

### md5

```gojo
pub fn md5(data: []byte) -> Vec[byte]
```

MD5 of `data` (broken for security; for checksums old formats require).

### hmac

```gojo
pub fn hmac(h: Hash, key: []byte, data: []byte) -> Vec[byte]
```

The HMAC of `data` with hash `h` and `key`. Compare MACs with `equal`.

### pbkdf2

```gojo
pub fn pbkdf2(h: Hash, password: []byte, salt: []byte, iterations: int, keyLen: int) -> Vec[byte]
```

A key of `keyLen` bytes derived from a password (PBKDF2, RFC 8018, with
HMAC-`h`). Use a random `salt` of 16 bytes or more, and as many
`iterations` as you can afford (600000 for SHA-256, as of 2023).

### equal

```gojo
pub fn equal(a: []byte, b: []byte) -> bool
```

Whether `a` and `b` are equal, taking the same time wherever they differ:
for comparing MACs, tokens and password hashes, where `==` would leak how
much of a guess was right.

