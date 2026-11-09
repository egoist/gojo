# time

Time: a monotonic clock for measuring, the wall clock and calendar dates.

```gojo
let start = time.now()
...
println("took", time.formatDuration(time.since(start)))
let d = time.local(time.unixNanos())
println(d.format("%Y-%m-%d %H:%M"))
```

**Constants:** [`nanosecond, microsecond, millisecond, second, minute, hour`](#nanosecond-microsecond-millisecond-second-minute-hour)

**Types:** [`struct Ticker`](#struct-ticker), [`struct Date`](#struct-date), [`struct ParseError`](#struct-parseerror)

**Functions:** [`sleep`](#sleep), [`now`](#now), [`since`](#since), [`unixNanos`](#unixnanos), [`unix`](#unix), [`formatDuration`](#formatduration), [`parseDuration`](#parseduration), [`ticker`](#ticker), [`dayName`](#dayname), [`monthName`](#monthname), [`at`](#at), [`utc`](#utc), [`local`](#local), [`date`](#date), [`parseIso`](#parseiso)

## Constants

### nanosecond, microsecond, millisecond, second, minute, hour

```gojo
pub const nanosecond = 1
pub const microsecond = 1000
pub const millisecond = 1000000
pub const second = 1000000000
pub const minute = 60000000000
pub const hour = 3600000000000
```

Durations are integer nanoseconds.

## Types

### struct Ticker

```gojo
pub struct Ticker {
    // and private fields
}
```

Ticks every `period` at fixed instants, `start + n * period`, so a slow
tick doesn't push the later ones back; ticks that are missed are skipped.

```gojo
var tick = time.ticker(time.second)
loop {
    tick.wait()                  // or, among other things to wait for:
    ...                          // select { time.at(tick.deadline()) => { tick.advance(); ... } }
}
```

#### Ticker.deadline

```gojo
pub fn deadline(self) -> int
```

The instant (on the `now()` scale) of the next tick.

#### Ticker.advance

```gojo
pub fn advance(inout self) -> int
```

Moves on to the first tick after now: how many ticks have passed (0
when the next one hasn't come yet, more than 1 when some were missed).

#### Ticker.wait

```gojo
pub fn wait(inout self) -> int
```

Waits for the next tick, then `advance`s: how many ticks passed. 0
when the task was cancelled while waiting.

#### Ticker.reset

```gojo
pub fn reset(inout self)
```

Starts again: the next tick is one period from now.

### struct Date

```gojo
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

```gojo
pub fn unixNanos(self) -> int
```

The moment this is, as nanoseconds since 1970 (see `unixNanos`).

#### Date.weekday

```gojo
pub fn weekday(self) -> int
```

0 is Sunday.

#### Date.yearDay

```gojo
pub fn yearDay(self) -> int
```

1 to 366.

#### Date.iso

```gojo
pub fn iso(self) -> String
```

RFC 3339: "2026-11-06T14:41:31Z", "2026-11-06T14:41:31.592+08:00".

#### Date.format

```gojo
pub fn format(self, layout: str) -> String
```

Formats with `strftime`-style directives:

```gojo
%Y year   %m month   %d day     %H hour   %M minute   %S second
%L milliseconds   %f microseconds   %N nanoseconds
%z +0800   %Z +08:00 (Z for UTC)   %a Mon   %A Monday   %b Jan   %B January
%j day of the year   %e day without zero padding   %%
```

### struct ParseError

```gojo
pub struct ParseError {
    pub msg: String
}
```

Why `parseIso` could not read a date.

#### ParseError.message

```gojo
pub fn message(self) -> String
```

The message, which quotes the input.

## Functions

### sleep

```gojo
pub fn sleep(d: int)
```

Parks the goroutine; other goroutines keep running on its thread. Returns
early when the task is cancelled (see task.cancelled).

### now

```gojo
pub fn now() -> int
```

Monotonic clock in nanoseconds, for measuring; not related to the date.

### since

```gojo
pub fn since(start: int) -> int
```

Nanoseconds since `start`, a value of `now()`.

### unixNanos

```gojo
pub fn unixNanos() -> int
```

The wall clock: nanoseconds since 1970-01-01 00:00 UTC.

### unix

```gojo
pub fn unix() -> int
```

Seconds since 1970-01-01 00:00 UTC.

### formatDuration

```gojo
pub fn formatDuration(d: int) -> String
```

"1.5s", "250ms", "3µs", "2h5m0s".

### parseDuration

```gojo
pub fn parseDuration(s: str) -> int ! ParseError
```

A duration written as `formatDuration` writes one, or as Go does: a
sequence of numbers (decimals allowed) each with a unit, ns, us (or µs),
ms, s, m or h, and an optional sign: "1.5s", "2h45m", "-250ms", "0".

### ticker

```gojo
pub fn ticker(period: int) -> Ticker
```

A ticker whose first tick is one `period` from now.

### dayName

```gojo
pub fn dayName(weekday: int) -> str
```

0 is Sunday.

### monthName

```gojo
pub fn monthName(month: int) -> str
```

1 is January.

### at

```gojo
pub fn at(ns: int, offset: int) -> Date
```

The date `ns` nanoseconds after 1970 is, at `offset` seconds east of UTC.

### utc

```gojo
pub fn utc(ns: int) -> Date
```

The date in UTC.

### local

```gojo
pub fn local(ns: int) -> Date
```

The date in the local time zone.

### date

```gojo
pub fn date(year: int, month: int, day: int, hour: int, minute: int, sec: int) -> Date
```

A date in UTC.

### parseIso

```gojo
pub fn parseIso(s: str) -> Date ! ParseError
```

Reads an RFC 3339 date: "2026-11-06T14:41:31Z", "2026-11-06 14:41:31.5+08:00".

