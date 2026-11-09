# toml

TOML: read configuration files written in TOML 1.0 into your own types or
a `toml.Value` tree, and write them back.

```gojo
struct Server { host: String = "localhost", port: int }
struct Config { name: String, server: Server, tags: Vec[String] = [] }

let cfg = try toml.decode[Config](try os.readText("app.toml"))
println(cfg.server.host, cfg.server.port)
println(try toml.encode(cfg))
```

A TOML document is a table, so `decode` and `encode` take a struct, a
`Map` or a `toml.Value`; any other type is a compile error. A type's TOML
form is its JSON form (see `json`): a struct is a table with its fields'
names as keys; `Vec`, arrays, sets and tuples are arrays; an enum without
data is the variant's name, and a variant with fields is a table with one
key, `{ Circle = { r = 1.5 } }`. TOML has no null: `encode` leaves out
what is `none`, and decoding takes a field that is left out from its
default value (`none` for an optional field); any other missing field is
an error. Keys match fields, and strings variants, ignoring case, `-` and
`_`: `max-connections` fills `maxConnections` (a key written exactly as the
field wins; two that match it otherwise are an error). Errors say what, in which value and where: `expected an
integer, found a string (in .server.port) at line 3, column 8`.

The text is TOML 1.0, with what TOML 1.1 adds: inline tables over several
lines with a trailing comma, the `\e` and `\xHH` escapes, and times
without seconds. Dates and times are read as strings in RFC 3339 form
(`1979-05-27T07:32:00Z`, `1979-05-27`, `07:32:00`).

`encode` writes the keys that hold values first, then each table under a
`[a.b]` header and each array of tables as `[[a.b]]`. Strings with line
breaks are multi-line literal strings (`'''`), and floats keep a `.`
(`2.0`). It fails when TOML can't hold the value: a null inside an array,
an integer above `i64`'s range, or a `toml.Value` that isn't a table.

**Constants:** [`maxDepth`](#maxdepth)

**Types:** [`type Value`](#type-value), [`struct Error`](#struct-error)

**Functions:** [`decode`](#decode), [`encode`](#encode), [`parse`](#parse)

## Constants

### maxDepth

```gojo
pub const maxDepth = 500
```

Deeper nesting is refused, as in `json`.

## Types

### type Value

```gojo
pub type Value = json.Value
```

A TOML document as a tree. It is `json.Value` itself, so a tree read from
TOML can be written as JSON or YAML, and the other way round.

### struct Error

```gojo
pub struct Error {
    pub msg: String
    // where, from 1
    pub line: int
    pub column: int
}
```

Why decoding or encoding failed, and where (`line` is 0 for encoding).

#### Error.message

```gojo
pub fn message(self) -> String
```

The message, with the line and column.

## Functions

### decode

```gojo
pub fn decode[T](text: str) -> T ! Error
```

The value of type `T` that the TOML `text` describes:
`toml.decode[Config](text)`.

### encode

```gojo
pub fn encode[T: any](v: T) -> String ! Error
```

The TOML text of a struct, a `Map` or a `toml.Value`.

### parse

```gojo
pub fn parse(text: str) -> Value ! Error
```

Reads a document as a tree (a `Value.Obj`).

