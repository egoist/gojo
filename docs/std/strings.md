# strings

Building strings piece by piece.

**Types:** [`struct Builder`](#struct-builder)

## Types

### struct Builder

```kite
pub struct Builder {
    // and private fields
}
```

A growable string buffer: `write` pieces, then `finish` to take the
`String`.

#### Builder.write

```kite
pub fn write(self inout, s str)
```

Appends `s`.

#### Builder.writeRune

```kite
pub fn writeRune(self inout, r rune)
```

Appends the UTF-8 encoding of `r`.

#### Builder.len

```kite
pub fn len(self) int
```

The length so far, in bytes.

#### Builder.finish

```kite
pub fn finish(self sink) String
```

The string built (the builder is used up).

