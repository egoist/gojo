# time

Time: a monotonic clock for measuring, the wall clock and calendar dates.

```zig
let start = time.now()
...
println("took", time.formatDuration(time.since(start)))
let d = time.local(time.unixNanos())
println(d.format("%Y-%m-%d %H:%M"))
```

**Constants:** [`nanosecond, microsecond, millisecond, second, minute, hour`](#nanosecond-microsecond-millisecond-second-minute-hour)

**Types:** [`struct Date`](#struct-date), [`struct ParseError`](#struct-parseerror)

**Functions:** [`sleep`](#sleep), [`now`](#now), [`since`](#since), [`unixNanos`](#unixnanos), [`unix`](#unix), [`formatDuration`](#formatduration), [`dayName`](#dayname), [`monthName`](#monthname), [`at`](#at), [`utc`](#utc), [`local`](#local), [`date`](#date), [`parseIso`](#parseiso)

## Constants

### nanosecond, microsecond, millisecond, second, minute, hour

```zig
pub const nanosecond = 1
pub const microsecond = 1000
pub const millisecond = 1000000
pub const second = 1000000000
pub const minute = 60000000000
pub const hour = 3600000000000
```

Durations are integer nanoseconds.

## Types

### struct Date

```zig
pub copy struct Date {
    pub year: int
    // 1 to 12
    pub month: int
    // 1 to 31
    pub day: int
    pub hour: int
    pub minute: int
    pub second: int
    pub nanosecond: int
    // seconds east of UTC (+08:00 is 28800)
    pub offset: int = 0
}
```

A calendar date and time of day, at some offset from UTC.

#### Date.unixNanos

```zig
pub fn unixNanos(self) -> int
```

The moment this is, as nanoseconds since 1970 (see `unixNanos`).

#### Date.weekday

```zig
pub fn weekday(self) -> int
```

0 is Sunday.

#### Date.yearDay

```zig
pub fn yearDay(self) -> int
```

1 to 366.

#### Date.iso

```zig
pub fn iso(self) -> String
```

RFC 3339: "2026-11-06T14:41:31Z", "2026-11-06T14:41:31.592+08:00".

#### Date.format

```zig
pub fn format(self, layout: str) -> String
```

Formats with `strftime`-style directives:

```zig
%Y year   %m month   %d day     %H hour   %M minute   %S second
%L milliseconds   %f microseconds   %N nanoseconds
%z +0800   %Z +08:00 (Z for UTC)   %a Mon   %A Monday   %b Jan   %B January
%j day of the year   %e day without zero padding   %%
```

### struct ParseError

```zig
pub struct ParseError {
    pub msg: String
}
```

Why `parseIso` could not read a date.

#### ParseError.message

```zig
pub fn message(self) -> String
```

The message, which quotes the input.

## Functions

### sleep

```zig
pub fn sleep(d: int)
```

Parks the goroutine; other goroutines keep running on its thread. Returns
early when the task is cancelled (see task.cancelled).

### now

```zig
pub fn now() -> int
```

Monotonic clock in nanoseconds, for measuring; not related to the date.

### since

```zig
pub fn since(start: int) -> int
```

Nanoseconds since `start`, a value of `now()`.

### unixNanos

```zig
pub fn unixNanos() -> int
```

The wall clock: nanoseconds since 1970-01-01 00:00 UTC.

### unix

```zig
pub fn unix() -> int
```

Seconds since 1970-01-01 00:00 UTC.

### formatDuration

```zig
pub fn formatDuration(d: int) -> String
```

"1.5s", "250ms", "3µs", "2h5m0s".

### dayName

```zig
pub fn dayName(weekday: int) -> str
```

0 is Sunday.

### monthName

```zig
pub fn monthName(month: int) -> str
```

1 is January.

### at

```zig
pub fn at(ns: int, offset: int) -> Date
```

The date `ns` nanoseconds after 1970 is, at `offset` seconds east of UTC.

### utc

```zig
pub fn utc(ns: int) -> Date
```

The date in UTC.

### local

```zig
pub fn local(ns: int) -> Date
```

The date in the local time zone.

### date

```zig
pub fn date(year: int, month: int, day: int, hour: int, minute: int, sec: int) -> Date
```

A date in UTC.

### parseIso

```zig
pub fn parseIso(s: str) -> Date ! ParseError
```

Reads an RFC 3339 date: "2026-11-06T14:41:31Z", "2026-11-06 14:41:31.5+08:00".

