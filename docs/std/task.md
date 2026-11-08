# task

Tasks and cancellation.

```zig
scope {
    let a = go fetch(x)      // a task; it may borrow data from outside
    go log(y)                // a task whose result isn't needed
    use(a.join())
}                            // waits for every task started inside
```

Cancellation flows down the task tree: when a task fails, a scope is
cancelled, or a deadline set with `timeout` passes, every task below it is
cancelled. Blocking operations in a cancelled task return early: a receive
gives none, a send is dropped, `time.sleep` wakes up, and network calls
fail with an error whose `isCancelled()` is true.

**Types:** [`enum Reason`](#enum-reason), [`struct Cancelled`](#struct-cancelled), [`struct Local`](#struct-local), [`struct Stats`](#struct-stats)

**Functions:** [`cancelled`](#cancelled), [`reason`](#reason), [`check`](#check), [`deadline`](#deadline), [`shield`](#shield), [`yieldNow`](#yieldnow), [`local`](#local), [`with`](#with), [`dump`](#dump), [`stats`](#stats), [`setMaxProcs`](#setmaxprocs), [`timeout`](#timeout), [`map`](#map), [`tryMap`](#trymap), [`each`](#each), [`race`](#race)

## Types

### enum Reason

```zig
pub copy enum Reason {
    // a task in its scope failed
    Failed
    // a deadline (`timeout`, `shield`) passed
    Deadline
    // `s.cancel()`, `t.cancel()`, or the scope was left early
    Explicit
    // the program was asked to stop: Ctrl-C or SIGTERM cancels `main`
    Shutdown
}
```

Why a task was cancelled.

### struct Cancelled

```zig
pub copy struct Cancelled {
    pub reason: Reason
}
```

The error of an operation that stopped because its task was cancelled.

#### Cancelled.message

```zig
pub fn message(self) -> String
```

"cancelled: a task failed", ...

### struct Local

```zig
pub copy struct Local[T: send] {
    // and private fields
}
```

A task-local value: code bound with `with` sees it, and so does every task
started inside, however deep. For what a whole request shares (an id, a
deadline budget, a logger) without passing it through every call:

```zig
let requestId = task.local[String]()

fn handle(req: Request) {
    task.with(requestId, req.id.clone(), () => serve(req))
}
fn log(msg: str) {
    println(requestId.get() ?? String("-"), msg)
}
```

#### Local.get

```zig
pub fn get(self) -> ?T where T: clone
```

The value bound by the nearest enclosing `with` (a copy), or none.

### struct Stats

```zig
pub struct Stats {
    // goroutines alive, `main` included
    pub goroutines: int
    // threads started to run goroutines (more than maxProcs only while some
    // goroutines are in long C calls)
    pub threads: int
    // goroutines running Gojo code at the same time; defaults to the number of
    // CPUs, or GOJO_MAXPROCS
    pub maxProcs: int
    // parked goroutines whose stacks are put away (copied out, memory given
    // back) until they run again; GOJO_HIBERNATE=0 turns this off
    pub hibernated: int
}
```

A snapshot of the scheduler, from `stats`.

## Functions

### cancelled

```zig
pub fn cancelled() -> bool
```

Has the current task been cancelled?

### reason

```zig
pub fn reason() -> ?Reason
```

Why the current task was cancelled, or none.

### check

```zig
pub fn check() ! Cancelled
```

A checkpoint for long computations: `try task.check()`.

### deadline

```zig
pub fn deadline() -> ?int
```

The nearest deadline of the current task (`time.now()` scale), or none.
Pass it on: a server can give a request's remaining time to the calls it
makes.

### shield

```zig
pub fn shield[T](d: int, body: fn() -> T) -> T
```

Runs `body` shielded from cancellation for at most `d`: cancelling the
caller doesn't affect it, only its own deadline does. For cleanup that has
to block (a goodbye message, a flush). `drop` methods run shielded like
this already, for up to a second, when their task is cancelled.

### yieldNow

```zig
pub fn yieldNow()
```

Let other tasks run.

### local

```zig
pub fn local[T: send]() -> Local[T]
```

A new task-local key; declare it at package level: `let k = task.local[T]()`.

### with

```zig
pub fn with[T: send, R](key: Local[T], value: sink T, body: fn() -> R) -> R
```

Runs `body` with `key` bound to `value`, for it and the tasks it starts.

### dump

```zig
pub fn dump()
```

Prints every task to stderr: where it was started, what it waits for and
for how long, and the locks it holds. A program also prints this, and
stops, on SIGQUIT (Ctrl-\ in a terminal).

### stats

```zig
pub fn stats() -> Stats
```

How many goroutines and threads there are now.

### setMaxProcs

```zig
pub fn setMaxProcs(n: int) -> int
```

Changes how many goroutines run Gojo code at the same time; returns the old value.

### timeout

```zig
pub fn timeout[T](d: int, body: fn() -> T) -> ?T
```

Runs `body` with a deadline. When it passes, everything `body` started is
cancelled, and the result is none. Also none when the caller is cancelled.

### map

```zig
pub fn map[T: send, R: send](xs: []T, limit: int, f: send fn(T) -> R) -> Vec[R]
```

Calls `f` on every element, in up to `limit` tasks at once (0: no limit),
and gives the results in the order of `xs`.

```zig
let sizes = task.map(paths, 8, (p) => fileSize(p))
```

### tryMap

```zig
pub fn tryMap[T: send, R: send, E: send](xs: []T, limit: int, f: send fn(T) -> R ! E) -> Vec[R] ! E
```

Like `map` for a function that can fail: the first error cancels the
calls still running, and is the result.

### each

```zig
pub fn each[T: send](xs: []T, limit: int, f: send fn(T))
```

Calls `f` on every element, in up to `limit` tasks at once (0: no limit).

### race

```zig
pub fn race[R: send, E: send](fs: []send fn() -> R ! E) -> R ! E
```

Runs every function at once; the first to succeed wins and the others are
cancelled. When all fail, the last error is the result.

```zig
let page = try task.race([() => fetch(primary), () => fetch(mirror)])
```

