# bufio

Buffered reading and writing on top of io.Reader / io.Writer.

```zig
var input = bufio.reader(os.stdin())
loop {
    let line = try input.readLine() ?? break
    ...
}
var out = bufio.writer(os.stdout())
try out.writeText("many small writes, one system call\n")
try out.flush()
```

**Types:** [`struct Reader`](#struct-reader), [`struct Writer`](#struct-writer)

**Functions:** [`reader`](#reader), [`writer`](#writer)

## Types

### struct Reader

```zig
pub struct Reader[R: io.Reader] {
    // and private fields
}
```

Reads from `R` in large chunks, and line by line.

#### Reader.readByte

```zig
pub fn readByte(inout self) -> ?byte ! io.Error
```

The next byte, or none at the end.

#### Reader.readUntil

```zig
pub fn readUntil(inout self, delim: byte) -> ?Vec[byte] ! io.Error
```

Everything up to and including `delim` (or to the end), or none when
nothing is left.

#### Reader.readLine

```zig
pub fn readLine(inout self) -> ?String ! io.Error
```

The next line without its "\n" (or "\r\n"), or none at the end.

#### Reader.read

```zig
pub fn read(inout self, buf: inout Vec[byte], max: int) -> int ! io.Error
```

As an io.Reader: what is buffered first.

#### Reader.readAll

```zig
pub fn readAll(inout self) -> Vec[byte] ! io.Error
```

Everything that is left.

### struct Writer

```zig
pub struct Writer[W: io.Writer] {
    // and private fields
}
```

Collects small writes and passes them to `W` in large ones (64 KB). What is
still buffered is written when the writer is dropped, but call `flush` to
see errors.

#### Writer.write

```zig
pub fn write(inout self, data: []byte) ! io.Error
```

Buffers `data` (large writes go straight through).

#### Writer.writeText

```zig
pub fn writeText(inout self, s: str) ! io.Error
```

Buffers the bytes of `s`.

#### Writer.flush

```zig
pub fn flush(inout self) ! io.Error
```

Writes out everything buffered.

## Functions

### reader

```zig
pub fn reader[R: io.Reader](src: sink R) -> Reader[R]
```

A buffered reader on `src` (which it owns).

### writer

```zig
pub fn writer[W: io.Writer](dst: sink W) -> Writer[W]
```

A buffered writer on `dst` (which it owns).

