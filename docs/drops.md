# Freeing memory at last use

Status: implemented (Nov 2026), in `src/codegen/drops.rs`; the rules are in
the language reference (spec 5.3). This file keeps the reasons, the
measurements, and what is left.

## In one page

A value whose drop **only frees memory** is freed right after the last
statement that uses it, instead of at the end of its scope. Everything else
is unchanged: values whose drop can be observed are still dropped at the end
of their scope, in reverse order of declaration.

```gojo
fn main() {
    let raw = load()              // 200 MB
    let total = summarize(raw)
    // `raw` is freed here, after its last use, not at the end of `main`
    let more = load()             // reuses those pages
    println(total, summarize(more))
}
```

- **Which values.** Those whose type reaches no `drop` method and no `Rc`,
  `Arc`, `Weak`, `Mutex`, channel end, task handle, function value,
  interface value or type parameter: `String`, `Vec[int]`, `Map[String,
  Vec[String]]`, structs and enums made of those (`Program::drop_silent`).
  Freeing them earlier is invisible to every program that doesn't use
  `unsafe`: nothing runs, nothing closes, no count changes.
- **What counts as a use.** Any mention of the variable, and any mention of
  a variable that may hold a borrow of it: a view (`let s = text.asStr()`),
  a reference (`for x in v`, `with`, `if let`), a value holding one. A value
  that a closure or task captures, that a `defer` mentions, or that `unsafe`
  code or a raw pointer expression (`s.ptr()`) mentions is never freed early.
- **Owned parameters too.** A `sink` parameter is the function's own value,
  freed after its last use like a `let`.
- **No new syntax, no change for code that observes drops.** `drop(x)` still
  drops early on request, and `defer`, `errdefer`, scopes and tasks work as
  before.
- **Measured:** halves peak memory for programs that load data, summarize
  it, and go on (383 MB to 192 MB below); see "Measurements".

## Background: Mojo's ASAP destruction

Mojo destroys every value right after its last use, not at the end of its
scope. The analysis follows control flow and treats each field separately.
A reference that is still live (tracked by Mojo's "origins") keeps its
owner alive. The gains:

- It removes Rust's main friction between borrows and destructors: in Rust,
  a value with a destructor holds its borrows until its scope ends, because
  the destructor might use them. Under ASAP the destructor runs right after
  the last use, so the borrows end there too.
- Memory comes back without anyone writing `drop(x)`.
- Values moved on only some branches need no runtime flags: they are
  destroyed at the end of the branches that didn't move them.

The costs fall on values whose destruction does something:

- RAII guards break. A lock guard that is never mentioned again is
  destroyed immediately; Mojo's answers are `with` blocks and the
  `_ = guard^` idiom that keeps a value alive by using it once more.
- Raw pointers dangle. A pointer into a buffer is not a use of the buffer,
  so the buffer can be freed while the pointer is still in use; the fix is
  the same keep-alive idiom, which the programmer has to remember.
- When a destructor runs depends on where the last use is, not where the
  value was declared, so adding a `print(x)` while debugging moves it.

## Why only memory

Most of what ASAP buys Mojo, Gojo gets from other rules, and the costs
would fall on features Gojo depends on.

- **No drop-check problem.** Struct fields of ordinary structs can't be
  views (spec 5.4), so a value with a `drop` method never holds a borrow.
- **Views already end at their last use** (spec 5.4, "Last use").
- **Guards are blocks already.** Locks are `with mu.lock() { }`, so there
  are no guard values to destroy too early, and none to hold too long.
- **Drop timing matters in Gojo.** Dropping the last `Sender` closes the
  channel, dropping a task handle cancels the task, `drop` methods run
  shielded from cancellation and may block, and files close on drop. Moving
  those drops to the last use would change what programs do.

So Gojo frees memory at the last use, where no program can tell, and keeps
scope-end drops for everything else.

## How it works

The drop-state analysis (`src/codegen/drops.rs`) runs on each function, and
each task or closure body, before its C is generated:

- **Drop flags only where needed.** A value moved somewhere in its function
  used to get a runtime flag, tested at every drop. The analysis works out
  where the flag's value is known (loops are solved to a fixpoint), so the
  drop is emitted plainly or not at all. In the C for `tests/run`, declared
  flags went from 654 to 34, and flag tests from 1201 to 43.
- **Freed at the end of a branch.** When some branches of an `if` or `match`
  move a value that only frees memory, and nothing uses it or a view of it
  afterwards, the other branches free it at their end. After the branches it
  is moved on every path, so it needs no flag.
- **Freed after the last use.** After each statement, a value its block
  owns (its `let`s, and the function's owned parameters) is freed when it
  drops silently, holds a value on every path (never moved, or known to be
  there), and no later statement or the block's value mentions it or
  anything that may borrow it. Its drop at the end of the scope goes away, so
  a `return` or `break` after that point doesn't drop it again, while one
  before still does.

What may borrow a value is worked out transitively: a variable bound or
assigned from an expression that mentions the value, when its type can hold
a borrow (views, references, raw pointers, function and interface values,
and anything containing one). Variables that something the analysis can't
follow may reach are pinned, never freed early: captured by a closure or
task, mentioned in a `defer` or in `unsafe` code, or mentioned by an
expression of raw-pointer type.

### Raw pointers

Spec 11.1 says a pointer from `.ptr()` stays valid as long as a view of the
same data would: for a `String`, "until it is changed or dropped". Freeing
earlier would shorten that, and a pointer can escape where no analysis
follows it (into C, through a safe function into storage elsewhere). So any
variable that an expression of raw-pointer type mentions (`s.ptr()`,
`buf[..].ptr()`) is dropped where its scope ends, never earlier. Unsafe
code needs no keep-alive idiom.

## Measurements

macOS, Apple M5, `--release`.

- **How often.** In the C for `tests/run`, `examples` and `bench`, 455 of
  3,970 drops (11%) happen at the last use, and 30 more at the end of a
  branch.
- **Peak memory.** The program at the top, with 25 million `int`s per
  phase: peak RSS **383 MB → 192 MB**, time 0.56 s → 0.51 s (the second
  phase reuses the first one's pages).
- **Benchmarks.** BENCH_RESULT
- **Tail calls.** In the same C, 239 `return f(...)` statements, 37 with
  drops between the call and the `return`. In 32 of those, a value dropped
  there is one the call itself uses (an argument temporary, or a variable it
  borrows), which no drop timing can move. The other 5 drop a value that is
  dead on the returning branch but used on another, which statement-level
  frees don't reach (see "What is left").
- **Correctness.** Every program in `tests/run` and `examples` prints its
  `.out`; all of `tests/run` runs clean under AddressSanitizer and UBSan; the
  leak counts (macOS `leaks --atExit`) match the compiler without early
  frees for every program; `tests/run/drop_states.gojo` covers each pinning
  rule with a case that would read freed memory if it were wrong, and
  `drop_placement` in the test suite checks where its drops land in the C.

## Costs

- **Debugging.** In a debugger, a variable whose memory was freed early
  shows freed memory until its scope ends. Debug and release builds free at
  the same points, so they use memory the same way.
- **Compile time.** One more check per statement over the analysis already
  running; not measurable.

## What is left

- **Free on the paths where a value is dead:** at the start of a branch that
  doesn't use a value its sibling branches still need (Mojo frees there).
  Statement-level frees don't. It would unblock at most 5 of the 239
  `return f(...)` above.
- **Expression granularity** (freeing in the middle of a statement, as
  Mojo does): statements are where temporaries end anyway, and the
  measurements show no case where finer would matter.
- **ASAP for values whose drop can be observed** (`drop` methods, channel
  ends, task handles, `Rc`): not planned. It would change what programs do,
  and Gojo's channels, tasks and cancellation rely on scope-end drops.
  `drop(x)` is the explicit way.
