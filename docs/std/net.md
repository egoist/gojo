# net

TCP networking. Calls block only the calling task: while one waits for the
network, others keep running on its thread.

```gojo
let ln = try net.listen(":8080")
scope {
    loop {
        let c = try ln.accept()
        go serve(c)
    }
}
```

**Types:** [`type Error`](#type-error), [`struct Listener`](#struct-listener), [`struct Conn`](#struct-conn)

**Functions:** [`listen`](#listen), [`dial`](#dial)

## Types

### type Error

```gojo
pub type Error = io.Error
```

Errors here are io.Error; `isCancelled()` tells a cancelled call apart.

### struct Listener

```gojo
pub struct Listener {
    // and private fields
}
```

A listening TCP socket, from `listen`. Closed when dropped.

#### Listener.accept

```gojo
pub fn accept(self) -> Conn ! Error
```

Waits for the next connection.

#### Listener.port

```gojo
pub fn port(self) -> int
```

The port it listens on; useful after `listen(":0")`.

### struct Conn

```gojo
pub struct Conn {
    // and private fields
}
```

A TCP connection, from `dial` or `Listener.accept`. Closed when dropped. It
is an `io.Reader` and an `io.Writer`.

#### Conn.read

```gojo
pub fn read(self, buf: inout Vec[byte], max: int) -> int ! Error
```

Appends up to `max` bytes to `buf`, waiting until at least one arrives.
Returns how many were read; 0 means the peer closed the connection.

#### Conn.write

```gojo
pub fn write(self, data: []byte) ! Error
```

Writes all of `data`.

#### Conn.writeText

```gojo
pub fn writeText(self, s: str) ! Error
```

Writes the bytes of `s`.

#### Conn.readAll

```gojo
pub fn readAll(self) -> Vec[byte] ! Error
```

Reads until the peer closes the connection.

#### Conn.closeWrite

```gojo
pub fn closeWrite(self)
```

Tells the peer no more data is coming; reading still works.

## Functions

### listen

```gojo
pub fn listen(addr: str) -> Listener ! Error
```

Listens on "host:port"; ":port" listens on all interfaces, ":0" picks a free port.

### dial

```gojo
pub fn dial(addr: str) -> Conn ! Error
```

Connects to `addr` ("host:port"; the host may be a name, which is looked up).

