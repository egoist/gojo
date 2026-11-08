# strings

Building strings piece by piece.

**Types:** [`struct Builder`](#struct-builder)

## Types

### struct Builder

```gojo
pub struct Builder {
    // and private fields
}
```

A growable string buffer: `write` pieces, then `finish` to take the
`String`.

#### Builder.write

```gojo
pub fn write(inout self, s: str)
```

Appends `s`.

#### Builder.writeRune

```gojo
pub fn writeRune(inout self, r: rune)
```

Appends the UTF-8 encoding of `r`.

#### Builder.len

```gojo
pub fn len(self) -> int
```

The length so far, in bytes.

#### Builder.finish

```gojo
pub fn finish(sink self) -> String
```

The string built (the builder is used up).

