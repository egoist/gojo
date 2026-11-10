# yaml

YAML: read configuration and data written in YAML 1.2 into your own types
or a `yaml.Value` tree, and write them back.

```gojo
struct Service { image: String, ports: Vec[int] = [], env: Map[String, String] = [:] }
struct Compose { services: Map[String, Service] }

let c = try yaml.decode[Compose](try os.readText("compose.yaml"))
for name, s in c.services {
    println(name, s.image, s.ports)
}
println(yaml.encode(c))
```

A type's YAML form is its JSON form (see `json`): a struct is a mapping
with its fields' names as keys; `Vec`, arrays, sets and tuples are
sequences; `?T` is null or the value; an enum without data is the
variant's name, and a variant with fields is a mapping with one key,
`Circle: {r: 1.5}`. Decoding skips keys that aren't fields, and takes a
field that is left out from its default value (`none` for an optional
field); any other missing field is an error. Keys match fields, and
strings variants, ignoring case, `-` and `_`: `depends_on` fills
`dependsOn`, and `on-failure` is the variant `OnFailure` (a key written
exactly as the field wins; two that match it otherwise are an error). An
empty document (nothing but comments) is null, or an empty mapping or
sequence when the type wants one, so a struct takes its defaults. Errors say what, in which
value and where: `expected an integer, found a string (in
.services.web.ports[0]) at line 4, column 9`. A type with no YAML form is
a compile error at the call that needs it.

The text is YAML 1.2: block and flow (`[a, b]`, `{a: 1}`) collections,
plain, quoted and block (`|`, `>`) scalars, comments, several documents
(`---`), anchors and aliases (`&base`, `*base`), and merge keys
(`<<: *base`). Plain scalars follow the core schema: `~`, `null` and an
empty value are null; only `true` and `false` are bools (`yes` and `on`
are strings); integers are decimal, `0o17` or `0x1F`; `.inf`, `-.inf` and
`.nan` are floats. A plain scalar also reads into a `String` as it is
written: `version: 1.10` gives "1.10". The core schema's tags (`!!str`,
`!!int`, `!!float`, `!!bool`, `!!null`, `!!map`, `!!seq`) are applied;
other tags are ignored. A key appearing twice in a mapping is an error;
an anchor is known in its own document; and aliases can't expand a
document beyond about ten times its size.

`encode` writes block style indented by two spaces. Strings are quoted
only when they would read back as something else (`"yes"`, `"1.10"`,
`"null"`, `"a: b"`), and strings with line breaks are literal blocks
(`|`); floats keep a `.` (`2.0`), so they stay floats.

**Constants:** [`maxDepth`](#maxdepth)

**Types:** [`type Value`](#type-value), [`struct Error`](#struct-error)

**Functions:** [`decode`](#decode), [`decodeAll`](#decodeall), [`encode`](#encode), [`parse`](#parse), [`parseAll`](#parseall)

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

A YAML document as a tree. It is `json.Value` itself, so a tree read from
YAML can be written as JSON or TOML, and the other way round.

### struct Error

```gojo
pub struct Error {
    pub msg: String
    // where, from 1
    pub line: int
    pub column: int
}
```

Why decoding failed, and where.

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

The value of type `T` that the YAML `text` describes:
`yaml.decode[Config](text)`. The text holds one document; an empty text is
null.

### decodeAll

```gojo
pub fn decodeAll[T](text: str) -> Vec[T] ! Error
```

Every document of the text (separated by `---`), each as a `T`.

### encode

```gojo
pub fn encode[T: any](v: T) -> String
```

The YAML text of any value. Panics when it is nested more than `maxDepth`
levels deep.

### parse

```gojo
pub fn parse(text: str) -> Value ! Error
```

Reads one document as a tree.

### parseAll

```gojo
pub fn parseAll(text: str) -> Vec[Value] ! Error
```

Reads every document of the text as a tree.

