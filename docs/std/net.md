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

**Types:** [`type Error`](#type-error), [`struct Listener`](#struct-listener), [`struct Conn`](#struct-conn), [`struct UdpSocket`](#struct-udpsocket)

**Functions:** [`listen`](#listen), [`dial`](#dial), [`listenUdp`](#listenudp), [`dialUdp`](#dialudp), [`lookup`](#lookup)

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

#### Listener.rawFd

```gojo
pub fn rawFd(self) -> int
```

The socket's file descriptor, for system calls this module doesn't
wrap. It stays owned by the listener.

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

#### Conn.rawFd

```gojo
pub fn rawFd(self) -> int
```

The socket's file descriptor, for system calls this module doesn't
wrap (and for `tls`). It stays owned by the connection.

### struct UdpSocket

```gojo
pub struct UdpSocket {
    // and private fields
}
```

A UDP socket, from `listenUdp` or `dialUdp`. Each send is one datagram and
each receive returns one. Closed when dropped.

```gojo
let sock = try net.listenUdp(":5353")
var buf = Vec[byte]{}
let (n, from) = try sock.recvFrom(&buf, 1500)
try sock.sendTo(buf[..n], from)
```

#### UdpSocket.sendTo

```gojo
pub fn sendTo(self, data: []byte, addr: str) ! Error
```

Sends `data` as one datagram to `addr` ("host:port"). Literal IP
addresses are used as they are; names are looked up on every call.

#### UdpSocket.recvFrom

```gojo
pub fn recvFrom(self, buf: inout Vec[byte], max: int) -> (int, String) ! Error
```

Waits for the next datagram, appends up to `max` of its bytes to `buf`
(the rest of a longer one is dropped), and returns how many with the
sender's address, which `sendTo` takes back.

#### UdpSocket.send

```gojo
pub fn send(self, data: []byte) ! Error
```

Sends `data` as one datagram to the peer given to `dialUdp`.

#### UdpSocket.recv

```gojo
pub fn recv(self, buf: inout Vec[byte], max: int) -> int ! Error
```

Like `recvFrom` without the sender; after `dialUdp` only the peer's
datagrams arrive.

#### UdpSocket.port

```gojo
pub fn port(self) -> int
```

The local port; useful after `listenUdp(":0")`.

#### UdpSocket.rawFd

```gojo
pub fn rawFd(self) -> int
```

The socket's file descriptor, for options this module doesn't wrap
(broadcast, multicast). It stays owned by the socket.

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

### listenUdp

```gojo
pub fn listenUdp(addr: str) -> UdpSocket ! Error
```

Binds a UDP socket to "host:port"; ":port" takes IPv4 and IPv6 on all
interfaces, ":0" picks a free port.

### dialUdp

```gojo
pub fn dialUdp(addr: str) -> UdpSocket ! Error
```

A UDP socket that talks to `addr` alone, through `send` and `recv`. Nothing
is sent until you send; errors such as an unreachable port show up on later
calls.

### lookup

```gojo
pub fn lookup(host: str) -> Vec[String] ! Error
```

The IP addresses of `host`, IPv4 ones first, from the system resolver
(/etc/hosts, then DNS). A literal address comes back as it is. An unknown
host is an error whose `isNotFound()` is true.

