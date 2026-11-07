# http

HTTP/1.1: a server built on goroutines, task groups and cancellation, and a
small client. Plain HTTP only (no TLS).

```zig
struct Hello {
    fn serve(self, req http.Request) http.Response {
        return http.text(200, "hello " + (req.param("name") ?? "world".toString()))
    }
}
fn main() ! io.Error {
    let ln = try net.listen(":8080")
    http.serve(ln, Hello{}, http.Options{})
}
```

Every connection is a task in the server's group, so cancelling the task
that runs `serve` cancels the requests in flight. Every request runs under a
deadline (`Options.deadline`): a handler still running then is cancelled and
the client gets 504. A client that hangs up cancels its request too: the
handler's blocking calls return early.

**Types:** [`struct Header`](#struct-header), [`struct Request`](#struct-request), [`struct Response`](#struct-response), [`interface Handler`](#interface-handler), [`struct Options`](#struct-options), [`struct Stats`](#struct-stats)

**Functions:** [`findHeader`](#findheader), [`text`](#text), [`json`](#json), [`statusText`](#statustext), [`serve`](#serve), [`serveCounting`](#servecounting), [`request`](#request), [`get`](#get), [`post`](#post), [`decode`](#decode), [`encode`](#encode)

## Types

### struct Header

```zig
pub struct Header {
    pub name String
    pub value String
}
```

A header line. Names are matched without regard to case.

### struct Request

```zig
pub struct Request {
    pub method String
    // the path, without the query
    pub path String
    // what follows `?` in the target, still percent-encoded
    pub rawQuery String = ""
    pub headers Vec[Header] = []
    pub body Vec[byte] = []
}
```

A request, as a handler gets it: the whole body has been read (up to
`Options.maxBody`).

#### Request.header

```zig
pub fn header(self, name str) ?String
```

The value of header `name` (any case).

#### Request.param

```zig
pub fn param(self, name str) ?String
```

A query parameter, decoded: "/search?q=a+b" has param("q") == "a b".

#### Request.segments

```zig
pub fn segments(self) Vec[String]
```

The path's parts: "/todos/7" -> ["todos", "7"].

#### Request.text

```zig
pub fn text(self) String
```

The body, as a string.

### struct Response

```zig
pub struct Response {
    pub status int = 200
    pub headers Vec[Header] = []
    pub body Vec[byte] = []
}
```

What a handler answers, and what the client gets back. Content-Length and
Connection are set by the server; line breaks in header names and values
become spaces.

#### Response.header

```zig
pub fn header(self, name str) ?String
```

The value of header `name` (any case).

#### Response.setHeader

```zig
pub fn setHeader(self inout, name str, value str)
```

Adds a header.

#### Response.text

```zig
pub fn text(self) String
```

The body, as a string.

### interface Handler

```zig
pub interface Handler {
    fn serve(self, req Request) Response
}
```

Answers requests. `serve` runs in a goroutine of its own for each
connection, under the deadline of `Options.deadline`; it is cancelled when
the deadline passes, the client hangs up or the server stops.

### struct Options

```zig
pub struct Options {
    // how long a handler may run
    pub deadline int = 30 * time.second
    // the largest request body accepted
    pub maxBody int = 8 * 1024 * 1024
}
```

How the server treats requests.

### struct Stats

```zig
pub struct Stats {
    pub served int = 0
    pub timedOut int = 0
    pub hungUp int = 0
    pub stopped int = 0
    pub rejected int = 0
}
```

What happened to the requests a server has handled (see `serveCounting`):
answered, stopped at the deadline (answered 504), abandoned by the client,
cancelled when the server stopped, or rejected as malformed or too large.

## Functions

### findHeader

```zig
pub fn findHeader(hs []Header, name str) ?String
```

The value of header `name` (any case) in `hs`.

### text

```zig
pub fn text(status int, body str) Response
```

A plain-text response.

### json

```zig
pub fn json(status int, body str) Response
```

A JSON response; `body` is JSON text (see json.Value.encode).

### statusText

```zig
pub fn statusText(status int) str
```

The reason phrase for a status code: 404 -> "Not Found".

### serve

```zig
pub fn serve[H Handler + send](ln net.Listener, h H, opts Options)
```

Serves until the calling task is cancelled; then cancels the requests in
flight and waits for them.

### serveCounting

```zig
pub fn serveCounting[H Handler + send](ln net.Listener, h H, opts Options, stats Mutex[Stats])
```

Like `serve`, counting what happened to the requests in `stats`.

### request

```zig
pub fn request(method str, url str, headers []Header, body []byte) Response ! io.Error
```

Sends a request and reads the whole response. `url` is
"http://host[:port]/path?query".

### get

```zig
pub fn get(url str) Response ! io.Error
```

A GET request.

### post

```zig
pub fn post(url str, contentType str, body []byte) Response ! io.Error
```

A POST request with a body of the given type.

### decode

```zig
pub fn decode(s str) ?String
```

Percent-decoding (and `+` as a space, as in query strings); none if malformed.

### encode

```zig
pub fn encode(s str) String
```

Percent-encoding of everything but letters, digits and `-._~`.

