# bufio

Buffered reading and writing on top of io.Reader / io.Writer.

```gojo
var input = bufio.reader(os.stdin())
for {
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

```gojo
pub struct Reader[R io.Reader] {
    // and private fields
}
```

Reads from `R` in large chunks, and line by line.

#### Reader.readByte

```gojo
pub fn readByte(self inout) ?byte ! io.Error
```

The next byte, or none at the end.

#### Reader.readUntil

```gojo
pub fn readUntil(self inout, delim byte) ?Vec[byte] ! io.Error
```

Everything up to and including `delim` (or to the end), or none when
nothing is left.

#### Reader.readLine

```gojo
pub fn readLine(self inout) ?String ! io.Error
```

The next line without its "\n" (or "\r\n"), or none at the end.

#### Reader.read

```gojo
pub fn read(self inout, buf inout Vec[byte], max int) int ! io.Error
```

As an io.Reader: what is buffered first.

#### Reader.readAll

```gojo
pub fn readAll(self inout) Vec[byte] ! io.Error
```

Everything that is left.

### struct Writer

```gojo
pub struct Writer[W io.Writer] {
    // and private fields
}
```

Collects small writes and passes them to `W` in large ones (64 KB). What is
still buffered is written when the writer is dropped, but call `flush` to
see errors.

#### Writer.write

```gojo
pub fn write(self inout, data []byte) ! io.Error
```

Buffers `data` (large writes go straight through).

#### Writer.writeText

```gojo
pub fn writeText(self inout, s str) ! io.Error
```

Buffers the bytes of `s`.

#### Writer.flush

```gojo
pub fn flush(self inout) ! io.Error
```

Writes out everything buffered.

## Functions

### reader

```gojo
pub fn reader[R io.Reader](src sink R) Reader[R]
```

A buffered reader on `src` (which it owns).

### writer

```gojo
pub fn writer[W io.Writer](dst sink W) Writer[W]
```

A buffered writer on `dst` (which it owns).

