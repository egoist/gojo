# json

JSON: parse text into a `json.Value` tree and encode it back.

```gojo
let v = try json.parse(`{"name": "gojo", "tags": ["fast", "safe"]}`)
if let name = v.get("name") {
    println(name.str() ?? "not a string")
}
var out = json.object()
out.set("ok", json.boolean(true))
println(out.encode())          // {"ok":true}
```

Objects keep their keys in insertion order. Integers that fit in `int` stay
exact (`Int`); other numbers are `Num` (f64).

**Constants:** [`maxDepth`](#maxdepth)

**Types:** [`enum Value`](#enum-value), [`struct Error`](#struct-error)

**Functions:** [`null, boolean, integer, number, string, array, object`](#null-boolean-integer-number-string-array-object), [`quoted`](#quoted), [`parse`](#parse)

## Constants

### maxDepth

```gojo
pub const maxDepth = 500
```

Deeper nesting is refused: parsing is recursive, and goroutine stacks are small.

## Types

### enum Value

```gojo
pub enum Value {
    Null
    Bool(b: bool)
    Int(i: int)
    Num(f: f64)
    Str(s: String)
    Arr(items: Vec[Value])
    Obj(fields: Map[String, Value])
}
```

A JSON value. Build one with the functions below (`json.object()`,
`json.string(s)`...) or with the variants, and read one with the methods.

#### Value.isNull

```gojo
pub fn isNull(self) -> bool
```

Whether this is `null`.

#### Value.isObject

```gojo
pub fn isObject(self) -> bool
```

Whether this is an object.

#### Value.isArray

```gojo
pub fn isArray(self) -> bool
```

Whether this is an array.

#### Value.boolean

```gojo
pub fn boolean(self) -> ?bool
```

The value of `true` or `false`.

#### Value.int

```gojo
pub fn int(self) -> ?int
```

An integer (also a float with no fractional part that fits).

#### Value.num

```gojo
pub fn num(self) -> ?f64
```

A number, integer or not.

#### Value.str

```gojo
pub fn str(self) -> ?str
```

The text of a string.

#### Value.get

```gojo
pub fn get(self, key: str) yield ?&Value
```

The value at `key` of an object.

#### Value.at

```gojo
pub fn at(self, i: int) yield ?&Value
```

Element `i` of an array.

#### Value.len

```gojo
pub fn len(self) -> int
```

Elements of an array, members of an object, or 0.

#### Value.keys

```gojo
pub fn keys(self) -> Vec[String]
```

The keys of an object, in order.

#### Value.set

```gojo
pub fn set(inout self, key: str, v: sink Value)
```

Sets a member of an object (does nothing to other values).

#### Value.push

```gojo
pub fn push(inout self, v: sink Value)
```

Appends to an array (does nothing to other values).

#### Value.encode

```gojo
pub fn encode(self) -> String
```

Compact JSON text.

#### Value.pretty

```gojo
pub fn pretty(self) -> String
```

JSON text indented by two spaces.

### struct Error

```gojo
pub struct Error {
    pub msg: String
    // where, from 1
    pub line: int
    pub column: int
}
```

Why `parse` failed, and where.

#### Error.message

```gojo
pub fn message(self) -> String
```

The message, with the line and column.

## Functions

### null, boolean, integer, number, string, array, object

```gojo
pub fn null() -> Value
pub fn boolean(b: bool) -> Value
pub fn integer(i: int) -> Value
pub fn number(f: f64) -> Value
pub fn string(s: str) -> Value
pub fn array() -> Value
pub fn object() -> Value
```

Values to build documents with; `array()` and `object()` are empty (add to
them with `push` and `set`).

### quoted

```gojo
pub fn quoted(s: str) -> String
```

Quotes a string as JSON.

### parse

```gojo
pub fn parse(text: str) -> Value ! Error
```

Parses one JSON value (with nothing but white space after it).

