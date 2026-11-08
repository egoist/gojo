# tls

TLS: encrypted, authenticated connections (what HTTPS runs on), through
the system's OpenSSL 1.1 or 3. It is loaded the first time TLS is used, so
programs without TLS don't need it; `GOJO_LIBSSL` names the library when
it isn't in a standard place.

```gojo
var c = try tls.dial("example.com:443")       // checks the certificate
try c.writeText("GET / HTTP/1.1\r\nHost: example.com\r\n\r\n")

let ln = try tls.listen(":8443", "cert.pem", "key.pem")
let c = try ln.accept()
```

Like `net`, calls block only the calling task. One task may read from a
connection while another writes to it. TLS 1.2 is the oldest version
accepted.

**Types:** [`type Error`](#type-error), [`struct ClientConfig`](#struct-clientconfig), [`struct Conn`](#struct-conn), [`struct Listener`](#struct-listener)

**Functions:** [`client`](#client), [`dial`](#dial), [`dialWith`](#dialwith), [`listen`](#listen)

## Types

### type Error

```gojo
pub type Error = io.Error
```

Errors here are io.Error: TLS failures (a certificate that doesn't verify,
a failed handshake) are of kind `Other`, with OpenSSL's reason.

### struct ClientConfig

```gojo
pub struct ClientConfig {
    // the name the server's certificate must be for (and the one sent in the
    // handshake, SNI); by default the host of the address
    pub serverName: ?String
    // a PEM file of CA certificates to trust instead of the system's
    pub caFile: ?String
    // don't check the certificate at all, which lets anyone in the middle
    // read and change the traffic: only for tests
    pub insecureSkipVerify: bool = false
}
```

How a client checks the server it connects to.

### struct Conn

```gojo
pub struct Conn {
    // and private fields
}
```

A TLS connection, from `dial` or `Listener.accept`. Closed when dropped
(with a close_notify). It is an `io.Reader` and an `io.Writer`.

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

#### Conn.handshake

```gojo
pub fn handshake(self) ! Error
```

Completes the handshake now. A server connection does it on its first
read or write; call this to see a client's handshake errors there.

#### Conn.rawFd

```gojo
pub fn rawFd(self) -> int
```

The underlying TCP connection's file descriptor.

### struct Listener

```gojo
pub struct Listener {
    // and private fields
}
```

A listening socket that serves TLS, from `listen`.

#### Listener.accept

```gojo
pub fn accept(self) -> Conn ! Error
```

Waits for the next connection. The handshake happens on the
connection's first read or write, in whichever task uses it, so a slow
client doesn't hold up the others.

#### Listener.port

```gojo
pub fn port(self) -> int
```

The port it listens on; useful after `listen(":0")`.

## Functions

### client

```gojo
pub fn client(tcp: sink net.Conn, host: str, cfg: ClientConfig) -> Conn ! Error
```

TLS over an established TCP connection `tcp` (for protocols that start in
plain text, like SMTP's STARTTLS); `host` is the name to check.

### dial

```gojo
pub fn dial(addr: str) -> Conn ! Error
```

Connects to `addr` ("host:port") and checks that its certificate is valid
for the host, against the system's root certificates.

### dialWith

```gojo
pub fn dialWith(addr: str, cfg: ClientConfig) -> Conn ! Error
```

Like `dial`, checking the server as `cfg` says.

### listen

```gojo
pub fn listen(addr: str, certFile: str, keyFile: str) -> Listener ! Error
```

Listens on `addr` (as `net.listen`) and serves TLS with the certificate
chain in the PEM file `certFile` and the private key in `keyFile`.

