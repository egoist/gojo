# url

URLs (RFC 3986): parsing them into their parts and back, query strings,
and percent-encoding.

```gojo
let u = try url.parse("https://ann@example.com:8080/a%20b?q=1&q=2#top")
// u.scheme "https", u.user some("ann"), u.host "example.com",
// u.port some(8080), u.path "/a b", u.fragment "top"
u.query().getAll("q")                 // ["1", "2"]

var q = url.Query{}
q.add("name", "Ann Lee")
q.encode()                            // "name=Ann+Lee"
```

**Types:** [`struct Error`](#struct-error), [`struct Url`](#struct-url), [`struct Query`](#struct-query)

**Functions:** [`parse`](#parse), [`parseQuery`](#parsequery), [`escape`](#escape), [`pathEscape`](#pathescape), [`unescape`](#unescape), [`pathUnescape`](#pathunescape)

## Types

### struct Error

```gojo
pub struct Error {
    pub msg: String
}
```

A URL that can't be parsed.

#### Error.message

```gojo
pub fn message(self) -> String
```

### struct Url

```gojo
pub struct Url {
    // in lower case: "https"
    pub scheme: String = ""
    // "user" or "user:password"
    pub user: ?String
    // a name or an address; an IPv6 address without its brackets
    pub host: String = ""
    pub port: ?int
    pub path: String = ""
    // after the `?`
    pub rawQuery: String = ""
    // after the `#`
    pub fragment: String = ""
}
```

A URL taken apart. `path`, `user` and `fragment` are decoded; `rawQuery`
is kept as written (decode it with `query()`).

#### Url.query

```gojo
pub fn query(self) -> Query
```

The query string's parameters.

#### Url.toString

```gojo
pub fn toString(self) -> String
```

The URL as text again, with its parts escaped as needed.

#### Url.resolve

```gojo
pub fn resolve(self, ref: str) -> Url ! Error
```

`ref` resolved against this URL, as a browser follows a link:
`"https://a.com/x/y".resolve("../z")` is `https://a.com/z`.

### struct Query

```gojo
pub struct Query {
    // and private fields
}
```

A query string's parameters, in the order they were added or appeared. A
name can have several values (`?tag=a&tag=b`).

#### Query.get

```gojo
pub fn get(self, name: str) -> ?String
```

The first value for `name`.

#### Query.getAll

```gojo
pub fn getAll(self, name: str) -> Vec[String]
```

Every value for `name`, in order.

#### Query.has

```gojo
pub fn has(self, name: str) -> bool
```

Whether there is a value for `name`.

#### Query.add

```gojo
pub fn add(inout self, name: str, value: str)
```

Adds a value for `name`, after any others.

#### Query.set

```gojo
pub fn set(inout self, name: str, value: str)
```

Makes `value` the only value for `name` (in the place of the first).

#### Query.remove

```gojo
pub fn remove(inout self, name: str)
```

Removes every value for `name`.

#### Query.len

```gojo
pub fn len(self) -> int
```

The number of (name, value) pairs.

#### Query.entries

```gojo
pub fn entries(self) -> Vec[(String, String)]
```

Copies of the (name, value) pairs, in order.

#### Query.encode

```gojo
pub fn encode(self) -> String
```

The query string: `a=1&b=two+words` (without the `?`).

#### Query.toString

```gojo
pub fn toString(self) -> String
```

## Functions

### parse

```gojo
pub fn parse(s: str) -> Url ! Error
```

The parts of `s`: an absolute URL (`https://host/path`), or a reference
relative to one (`/path?q`, `../x`, `#top`).

### parseQuery

```gojo
pub fn parseQuery(q: str) -> Query
```

The parameters of a query string (with or without its `?`). Lenient, as
browsers are: `+` is a space, and a `%` without two hex digits is kept.

### escape

```gojo
pub fn escape(s: str) -> String
```

`s` escaped for a query string's name or value: letters, digits and `-._~`
stay, a space becomes `+`, everything else `%XX`.

### pathEscape

```gojo
pub fn pathEscape(s: str) -> String
```

`s` escaped as one segment of a path: like `escape`, but a space is `%20`,
`/` is escaped, and `:@!$&'()*+,;=` stay.

### unescape

```gojo
pub fn unescape(s: str) -> ?String
```

The text of a query-string value: `%XX` decoded and `+` a space; none for
a `%` without two hex digits.

### pathUnescape

```gojo
pub fn pathUnescape(s: str) -> ?String
```

Like `unescape`, for a path: `+` stays a `+`.

