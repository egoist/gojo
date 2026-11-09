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

Any type converts to and from JSON text directly, without a `Value` in
between:

```gojo
struct User { name: String, age: int, tags: Vec[String] = [], boss: ?String }
let text = json.encode(User{name: "Ann", age: 30})
// {"name":"Ann","age":30,"tags":[],"boss":null}
let u = try json.decode[User](`{"name": "Bo", "age": 7}`)
// tags and boss were left out: their defaults, [] and none
```

The JSON form of a type:

```gojo
struct                 an object with its fields' names as keys
Vec, array, Set, tuple an array
Map[K, V]              an object; keys that are integers or enums
                       without data are written as strings
?T                     null, or the value
enum without data      the variant's name as a string: "Red"
other enums            "Leaf" for a variant without fields,
                       {"Circle": {"r": 1.5}} for one with fields
numbers, bool, String  themselves (NaN and infinity become null)
json.Value             any JSON
Box, Rc, Arc, Cell     what they hold
```

Decoding skips keys that aren't fields, and takes a field that is left out
from its default value (`none` for an optional field); any other missing
field is an error. Errors say where, and in which value: `expected an
integer, found a string (in .users[2].age) at line 4, column 12`. A type
that has no JSON form (a function, an interface, a lock...) is a compile
error at the `encode` or `decode` that needs it.

**Constants:** [`maxDepth`](#maxdepth)

**Types:** [`enum Value`](#enum-value), [`struct Error`](#struct-error)

**Functions:** [`null, boolean, integer, number, string, array, object`](#null-boolean-integer-number-string-array-object), [`quoted`](#quoted), [`encode`](#encode), [`encodePretty`](#encodepretty), [`decode`](#decode), [`toValue`](#tovalue), [`fromValue`](#fromvalue), [`parse`](#parse)

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

### encode

```gojo
pub fn encode[T](v: T) -> String
```

The JSON text of any value, compact.

### encodePretty

```gojo
pub fn encodePretty[T](v: T) -> String
```

The JSON text of any value, indented by two spaces.

### decode

```gojo
pub fn decode[T](text: str) -> T ! Error
```

The value of type `T` that the JSON `text` describes: `json.decode[Config](text)`.

### toValue

```gojo
pub fn toValue[T](v: T) -> Value
```

Any value as a `Value` tree, to inspect or change before encoding.

### fromValue

```gojo
pub fn fromValue[T](v: Value) -> T ! Error
```

The value of type `T` that a `Value` tree describes.

### parse

```gojo
pub fn parse(text: str) -> Value ! Error
```

Parses one JSON value (with nothing but white space after it).

