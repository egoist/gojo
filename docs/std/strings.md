# strings

Building strings piece by piece.

**Types:** [`struct Builder`](#struct-builder)

## Types

### struct Builder

```zig
pub struct Builder {
    // and private fields
}
```

A growable string buffer: `write` pieces, then `finish` to take the
`String`.

#### Builder.write

```zig
pub fn write(inout self, s: str)
```

Appends `s`.

#### Builder.writeRune

```zig
pub fn writeRune(inout self, r: rune)
```

Appends the UTF-8 encoding of `r`.

#### Builder.len

```zig
pub fn len(self) -> int
```

The length so far, in bytes.

#### Builder.finish

```zig
pub fn finish(sink self) -> String
```

The string built (the builder is used up).

