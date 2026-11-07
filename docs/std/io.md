# io

Errors from the operating system, files, processes and the network, and the
interfaces for streams of bytes.

```gojo
fn copyFile(from str, to str) ! io.Error {
    var src = try os.open(from)
    var dst = try os.create(to)
    _ = try io.copy(&dst, &src)
}
```

**Types:** [`enum ErrorKind`](#enum-errorkind), [`struct Error`](#struct-error), [`interface Reader`](#interface-reader), [`interface Writer`](#interface-writer), [`struct BytesReader`](#struct-bytesreader), [`struct Buffer`](#struct-buffer)

**Functions:** [`fromCode`](#fromcode), [`newError`](#newerror), [`readAll`](#readall), [`readText`](#readtext), [`copy`](#copy), [`writeText`](#writetext), [`bytesReader`](#bytesreader), [`textReader`](#textreader)

## Types

### enum ErrorKind

```gojo
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

```gojo
pub struct Error {
    pub kind ErrorKind
    pub msg String
    // the operating system's error number, or 0
    pub code int = 0
}
```

An error from the operating system or a stream: what kind, and a message
that says what was being done (`open notes.txt: no such file or directory`).
`os.Error` and `net.Error` are this type.

#### Error.message

```gojo
pub fn message(self) String
```

The message, as `print` shows it.

#### Error.isCancelled

```gojo
pub fn isCancelled(self) bool
```

The call stopped because its task was cancelled.

#### Error.isNotFound

```gojo
pub fn isNotFound(self) bool
```

The file or directory does not exist.

### interface Reader

```gojo
pub interface Reader {
    // Appends at most `max` bytes to `buf` and returns how many; 0 means the
    // end of the stream. Waits until at least one byte is there.
    fn read(self inout, buf inout Vec[byte], max int) int ! Error
}
```

A source of bytes: files, connections, process output, `io.BytesReader`.

### interface Writer

```gojo
pub interface Writer {
    // Writes all of `data`.
    fn write(self inout, data []byte) ! Error
}
```

A sink for bytes: files, connections, `io.Buffer`.

### struct BytesReader

```gojo
pub struct BytesReader {
    // and private fields
}
```

Reads from bytes in memory.

#### BytesReader.read

```gojo
pub fn read(self inout, buf inout Vec[byte], max int) int ! Error
```

Appends up to `max` of the remaining bytes to `buf`.

### struct Buffer

```gojo
pub struct Buffer {
    pub data Vec[byte] = []
}
```

Collects what is written to it.

#### Buffer.write

```gojo
pub fn write(self inout, data []byte) ! Error
```

Appends `data`; never fails.

#### Buffer.text

```gojo
pub fn text(self) String
```

What was written, as a string.

## Functions

### fromCode

```gojo
pub fn fromCode(what str, code int) Error
```

An error from the operating system's error number `code`, for `what` ("open
data.txt"). Used by `os`, `net` and code calling C directly.

### newError

```gojo
pub fn newError(kind ErrorKind, msg str) Error
```

An error that doesn't come from the operating system.

### readAll

```gojo
pub fn readAll(r inout Reader) Vec[byte] ! Error
```

Reads `r` to its end.

### readText

```gojo
pub fn readText(r inout Reader) String ! Error
```

Reads `r` to its end as text.

### copy

```gojo
pub fn copy(dst inout Writer, src inout Reader) int ! Error
```

Copies `src` into `dst` until `src` ends; returns the number of bytes.

### writeText

```gojo
pub fn writeText(w inout Writer, s str) ! Error
```

Writes the bytes of `s` to `w`.

### bytesReader

```gojo
pub fn bytesReader(data sink Vec[byte]) BytesReader
```

A reader of `data`.

### textReader

```gojo
pub fn textReader(s str) BytesReader
```

A reader of the bytes of `s`.

