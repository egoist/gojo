# task

Structured concurrency.

```kite
try task.group((g) => {
    g.go(() => { ... })      // tasks may borrow data from outside the group
})                           // waits for all tasks; returns the first error
```

Cancellation flows down the task tree: when a task fails, or a deadline set
with `timeout` passes, every task below it is cancelled. Blocking operations
in a cancelled task return early: a receive gives none, a send is dropped,
`time.sleep` wakes up, and network calls fail with an error whose
`isCancelled()` is true.

**Types:** [`struct Group`](#struct-group), [`struct Cancelled`](#struct-cancelled), [`struct Stats`](#struct-stats)

**Functions:** [`cancelled`](#cancelled), [`check`](#check), [`yieldNow`](#yieldnow), [`stats`](#stats), [`setMaxProcs`](#setmaxprocs), [`timeout`](#timeout)

## Types

### struct Group

```kite
pub unsafe send copy struct Group {
    // and private fields
}
```

A handle to a running group. Tasks may use it to cancel the group (the
handle never outlives the group: `task.group` waits for every task).

#### Group.cancel

```kite
pub fn cancel(self)
```

Cancel every task in the group, and the group body itself.

### struct Cancelled

```kite
pub copy struct Cancelled {}
```

The error of an operation that stopped because its task was cancelled.

#### Cancelled.message

```kite
pub fn message(self) String
```

"cancelled"

### struct Stats

```kite
pub struct Stats {
    // goroutines alive, `main` included
    pub goroutines int
    // threads started to run goroutines (more than maxProcs only while some
    // goroutines are in long C calls)
    pub threads int
    // goroutines running Kite code at the same time; defaults to the number of
    // CPUs, or KITE_MAXPROCS
    pub maxProcs int
}
```

A snapshot of the scheduler, from `stats`.

## Functions

### cancelled

```kite
pub fn cancelled() bool
```

Has the current task been cancelled?

### check

```kite
pub fn check() ! Cancelled
```

A checkpoint for long computations: `try task.check()`.

### yieldNow

```kite
pub fn yieldNow()
```

Let other goroutines run.

### stats

```kite
pub fn stats() Stats
```

How many goroutines and threads there are now.

### setMaxProcs

```kite
pub fn setMaxProcs(n int) int
```

Changes how many goroutines run Kite code at the same time; returns the old value.

### timeout

```kite
pub fn timeout[T any](d int, body fn() T) ?T
```

Runs `body` with a deadline. When it passes, everything `body` started is
cancelled, and the result is none. Also none when the caller is cancelled.

