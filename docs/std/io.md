# io

Errors from the operating system, files, processes and the network, and the
interfaces for streams of bytes.

```zig
fn copyFile(from: str, to: str) ! io.Error {
    var src = try os.open(from)
    var dst = try os.create(to)
    _ = try io.copy(&dst, &src)
}
```

**Types:** [`enum ErrorKind`](#enum-errorkind), [`struct Error`](#struct-error), [`interface Reader`](#interface-reader), [`interface Writer`](#interface-writer), [`struct BytesReader`](#struct-bytesreader), [`struct Buffer`](#struct-buffer)

**Functions:** [`fromCode`](#fromcode), [`newError`](#newerror), [`readAll`](#readall), [`readText`](#readtext), [`copy`](#copy), [`writeText`](#writetext), [`bytesReader`](#bytesreader), [`textReader`](#textreader)

## Types

### enum ErrorKind

```zig
pub copy enum ErrorKind {
    Other
    NotFound
    PermissionDenied
    AlreadyExists
    NotADirectory
    IsADirectory
    DirectoryNotEmpty
    InvalidInput
    UnexpectedEof
    BrokenPipe
    ConnectionRefused
    ConnectionReset
    AddressInUse
    TimedOut
    Cancelled
    Interrupted
}
```

What went wrong, independent of the platform.

### struct Error

```zig
pub struct Error {
    pub kind: ErrorKind
    pub msg: String
    // the operating system's error number, or 0
    pub code: int = 0
}
```

An error from the operating system or a stream: what kind, and a message
that says what was being done (`open notes.txt: no such file or directory`).
`os.Error` and `net.Error` are this type.

#### Error.message

```zig
pub fn message(self) -> String
```

The message, as `print` shows it.

#### Error.isCancelled

```zig
pub fn isCancelled(self) -> bool
```

The call stopped because its task was cancelled.

#### Error.isNotFound

```zig
pub fn isNotFound(self) -> bool
```

The file or directory does not exist.

### interface Reader

```zig
pub interface Reader {
    // Appends at most `max` bytes to `buf` and returns how many; 0 means the
    // end of the stream. Waits until at least one byte is there.
    fn read(inout self, buf: inout Vec[byte], max: int) -> int ! Error
}
```

A source of bytes: files, connections, process output, `io.BytesReader`.

### interface Writer

```zig
pub interface Writer {
    // Writes all of `data`.
    fn write(inout self, data: []byte) ! Error
}
```

A sink for bytes: files, connections, `io.Buffer`.

### struct BytesReader

```zig
pub struct BytesReader {
    // and private fields
}
```

Reads from bytes in memory.

#### BytesReader.read

```zig
pub fn read(inout self, buf: inout Vec[byte], max: int) -> int ! Error
```

Appends up to `max` of the remaining bytes to `buf`.

### struct Buffer

```zig
pub struct Buffer {
    pub data: Vec[byte] = []
}
```

Collects what is written to it.

#### Buffer.write

```zig
pub fn write(inout self, data: []byte) ! Error
```

Appends `data`; never fails.

#### Buffer.text

```zig
pub fn text(self) -> String
```

What was written, as a string.

## Functions

### fromCode

```zig
pub fn fromCode(what: str, code: int) -> Error
```

An error from the operating system's error number `code`, for `what` ("open
data.txt"). Used by `os`, `net` and code calling C directly.

### newError

```zig
pub fn newError(kind: ErrorKind, msg: str) -> Error
```

An error that doesn't come from the operating system.

### readAll

```zig
pub fn readAll(r: inout Reader) -> Vec[byte] ! Error
```

Reads `r` to its end.

### readText

```zig
pub fn readText(r: inout Reader) -> String ! Error
```

Reads `r` to its end as text.

### copy

```zig
pub fn copy(dst: inout Writer, src: inout Reader) -> int ! Error
```

Copies `src` into `dst` until `src` ends; returns the number of bytes.

### writeText

```zig
pub fn writeText(w: inout Writer, s: str) ! Error
```

Writes the bytes of `s` to `w`.

### bytesReader

```zig
pub fn bytesReader(data: sink Vec[byte]) -> BytesReader
```

A reader of `data`.

### textReader

```zig
pub fn textReader(s: str) -> BytesReader
```

A reader of the bytes of `s`.

