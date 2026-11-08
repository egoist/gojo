# log

Leveled, structured logging: one line per event, with key=value fields,
to standard error.

```gojo
log.info("server started")
log.fields([log.string("addr", addr), log.integer("workers", 8)]).info("listening")
// 2026-11-08T16:40:00.123Z INFO  listening addr=:8080 workers=8

let req = log.fields([log.string("id", id)])   // a logger that adds fields
req.warn("slow")
req.fields([log.cause(e)]).error("failed")

log.setLevel(.Debug)        // also: log.setLevel(try log.Level.parse(s))
log.setFormat(.Json)        // {"time":"...","level":"info","msg":"...",...}
```

A line is written with a single write, so lines from different tasks never
mix. A message below the level costs one atomic load. A `Logger` is a
plain value: set it up once (`var l = log.Logger{}`; `l.level = .Debug`)
and pass it around, or change the one the package functions use with
`log.setDefault`.

**Types:** [`enum Level`](#enum-level), [`struct ParseError`](#struct-parseerror), [`enum Format`](#enum-format), [`enum Output`](#enum-output), [`interface Message`](#interface-message), [`struct Field`](#struct-field), [`struct Logger`](#struct-logger)

**Functions:** [`string`](#string), [`integer`](#integer), [`number`](#number), [`boolean`](#boolean), [`duration`](#duration), [`cause`](#cause), [`default`](#default), [`setDefault`](#setdefault), [`setLevel`](#setlevel), [`setFormat`](#setformat), [`setOutput`](#setoutput), [`fields`](#fields), [`enabled`](#enabled), [`debug`](#debug), [`info`](#info), [`warn`](#warn), [`error`](#error)

## Types

### enum Level

```gojo
pub copy enum Level {
    Debug
    Info
    Warn
    Error
}
```

How severe an event is. Events below a logger's level are dropped.

#### Level.toString

```gojo
pub fn toString(self) -> String
```

"debug", "info", "warn" or "error".

#### Level.parse

```gojo
pub fn parse(s: str) -> Level ! ParseError
```

The level named `s`, in any case; "warning" and "err" work too.

### struct ParseError

```gojo
pub struct ParseError {
    pub text: String
}
```

A level name that `Level.parse` doesn't know.

#### ParseError.message

```gojo
pub fn message(self) -> String
```

### enum Format

```gojo
pub copy enum Format {
    // `2026-11-08T16:40:00.123Z INFO  message key=value`, for people
    Text
    // one JSON object per line, for programs
    Json
}
```

How lines are written.

### enum Output

```gojo
pub copy enum Output {
    Stderr
    Stdout
}
```

Where lines go.

### interface Message

```gojo
pub interface Message {
    fn message(self) -> String
}
```

Anything with a `message`, as every error has; for `cause`.

### struct Field

```gojo
pub struct Field {
    // and private fields
}
```

A key and a value attached to an event. Make one with `string`,
`integer`, `number`, `boolean`, `duration` or `cause`.

### struct Logger

```gojo
pub struct Logger {
    pub level: Level = .Info
    pub format: Format = .Text
    pub out: Output = .Stderr
    // whether lines start with the time (UTC, to the millisecond)
    pub time: bool = true
    // and private fields
}
```

Writes events at or above `level`, with `fields` on every line.

#### Logger.fields

```gojo
pub fn fields(self, fields: []Field) -> Logger
```

A logger that also adds `fields` to every line.

#### Logger.enabled

```gojo
pub fn enabled(self, level: Level) -> bool
```

Whether events at `level` are written: to skip work that only builds
a log message.

#### Logger.log

```gojo
pub fn log(self, level: Level, msg: str)
```

Writes `msg` at `level`.

#### Logger.debug

```gojo
pub fn debug(self, msg: str)
```

#### Logger.info

```gojo
pub fn info(self, msg: str)
```

#### Logger.warn

```gojo
pub fn warn(self, msg: str)
```

#### Logger.error

```gojo
pub fn error(self, msg: str)
```

#### Logger.line

```gojo
pub fn line(self, level: Level, msg: str, ns: int) -> String
```

The line, with its `\n`, that an event at `level` would be at the
instant `ns` (Unix nanoseconds): to write logs somewhere else.

## Functions

### string

```gojo
pub fn string(key: str, value: str) -> Field
```

A text field.

### integer

```gojo
pub fn integer(key: str, value: int) -> Field
```

An integer field.

### number

```gojo
pub fn number(key: str, value: f64) -> Field
```

A number field.

### boolean

```gojo
pub fn boolean(key: str, value: bool) -> Field
```

A true or false field.

### duration

```gojo
pub fn duration(key: str, ns: int) -> Field
```

A duration in nanoseconds, shown as `time.formatDuration` shows it
(`1.5s`); in JSON, a number of seconds.

### cause

```gojo
pub fn cause(e: Message) -> Field
```

The message of an error, under the key "error".

### default

```gojo
pub fn default() -> Logger
```

A copy of the logger the package functions use.

### setDefault

```gojo
pub fn setDefault(l: sink Logger)
```

Makes `l` the logger the package functions use.

### setLevel

```gojo
pub fn setLevel(level: Level)
```

Changes the level of the default logger.

### setFormat

```gojo
pub fn setFormat(format: Format)
```

Changes the format of the default logger.

### setOutput

```gojo
pub fn setOutput(out: Output)
```

Changes where the default logger writes.

### fields

```gojo
pub fn fields(fields: []Field) -> Logger
```

The default logger with `fields` added to every line it writes.

### enabled

```gojo
pub fn enabled(level: Level) -> bool
```

Whether the default logger writes events at `level`.

### debug

```gojo
pub fn debug(msg: str)
```

### info

```gojo
pub fn info(msg: str)
```

### warn

```gojo
pub fn warn(msg: str)
```

### error

```gojo
pub fn error(msg: str)
```

