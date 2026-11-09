# Freeing memory at last use (design proposal)

Status: proposal (Nov 2026). The groundwork is implemented: the drop-state
analysis in `src/codegen/drops.rs` and the early frees at the end of
branches (see "What Gojo does today"). Freeing at last use is not. The
numbers below come from a prototype built for this proposal on top of that
analysis; it is not merged. Once the design is agreed, the rules move into
the language reference (`spec.md`) and this file keeps the reasons.

## The proposal in one page

A value whose drop **only frees memory** is freed right after the last
statement that uses it, instead of at the end of its scope. Everything else
is unchanged: values whose drop can be observed are still dropped at the end
of their scope, in reverse order of declaration, exactly as today.

```gojo
fn main() {
    let raw = load()              // 200 MB
    let total = summarize(raw)
    // today `raw` lives until the end of `main`; with this proposal its
    // memory is freed here, after its last use
    let more = load()             // reuses those pages
    println(total, summarize(more))
}
```

- **Which values.** Those whose type reaches no `drop` method and no `Rc`,
  `Arc`, `Weak`, `Mutex`, channel end, task handle, function value,
  interface value or type parameter: `String`, `Vec[int]`, `Map[String,
  Vec[String]]`, structs and enums made of those. Freeing them earlier is
  invisible to every program that doesn't use `unsafe`: nothing runs, nothing
  closes, no count changes. This is the predicate `Program::drop_silent` that
  the compiler already uses.
- **What counts as a use.** Any mention of the variable, and any mention of
  a variable that may hold a borrow of it: a view (`let s = text.asStr()`),
  a reference (`for x in v`, `with`, `if let`), a raw pointer from `.ptr()`,
  a value holding any of those. A value that a closure or task captures,
  that a `defer` mentions, or that `unsafe` code touches is never freed
  early.
- **No new syntax, no change for code that observes drops.** `drop(x)` still
  drops early on request, and `defer`, `errdefer`, scopes and tasks work as
  before.
- **Measured:** halves peak memory for programs that load data, summarize
  it, and go on (383 MB to 192 MB below), changes nothing measurable in the
  eight benchmarks, and does not unblock tail calls (see "Measurements").

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

## What Gojo already gets

Most of what ASAP buys Mojo, Gojo gets from other rules, and the costs
would fall on features Gojo depends on.

- **No drop-check problem.** Struct fields can't be views (spec 5.4), so a
  value with a `drop` method never holds a borrow. The problem ASAP solves in
  Rust can't arise.
- **Views already end at their last use** (spec 5.4, "Last use").
- **Guards are blocks already.** Locks are `with mu.lock() { }`, so there
  are no guard values to destroy too early.
- **Drop timing matters in Gojo.** Dropping the last `Sender` closes the
  channel, dropping a task handle cancels the task, `drop` methods run
  shielded from cancellation and may block, and files close on drop. Moving
  those drops to the last use would change what programs do.

So Gojo should not adopt ASAP as the language rule. What is left is the
part that can't be observed: freeing memory sooner.

## What Gojo does today

Implemented in `src/codegen/drops.rs`:

- **Drop flags only where needed.** A value moved somewhere in its function
  used to get a runtime flag, tested at every drop. A dataflow pass now
  works out where the flag's value is known (loops are solved to a
  fixpoint), so the drop is emitted plainly or not at all. In the C for
  `tests/run`, declared flags went from 654 to 34, and flag tests from 1201
  to 43.
- **Freed at the end of a branch.** When a value is moved on some branches
  of an `if` or `match`, its drop only frees memory, and nothing uses it or
  a view of it afterwards, it is freed at the end of the other branches
  (spec 5.3). After the branches it is moved on every path, so it needs no
  flag at all.

```gojo
let s: String = "early"
if c {
    n += take(s)
}                  // the else branch frees `s`: no flag, no drop at the end
```

The analysis also already computes everything this proposal needs: which
types drop silently, which variables may borrow which (transitively, through
views, references, raw pointers and values holding them), which are pinned
by closures, tasks, `defer` and `unsafe` code, and whether a variable is
mentioned after a given statement.

## The rule

A local variable `x` whose type drops silently is freed after statement `i`
of the block that declares it when:

1. `x` holds a value there on every path (it hasn't been moved on any; the
   analysis knows this statically, so no flag is involved);
2. no later statement of the block mentions `x` or any variable that may
   borrow it (including the block's value);
3. `x` is not captured by a closure or task, not mentioned in a `defer` or
   `errdefer`, and not mentioned in `unsafe` code, and neither is any
   variable that may borrow it.

The free then replaces the drop at the end of the scope: a `break`,
`continue`, `return` or `try` after the statement doesn't drop `x` again,
and one before it still does.

The language reference would say, in 5.3: *"A value whose drop only frees
memory may be freed at any point after its last use and before its owner
goes out of scope; no program can tell when."* That leaves the compiler free
to choose the point, now and later.

### Raw pointers

Spec 11.1 says a pointer from `.ptr()` stays valid as long as a view of the
same data would: for a `String`, "until it is changed or dropped". Freeing
earlier shortens that. The prototype keeps the owner alive while a pointer
derived from it sits in a local (that local is counted as borrowing it), and
pins anything `unsafe` code mentions. A pointer passed through a safe
function into storage elsewhere (`cache.add(v.ptr())`, used later in
`unsafe` code) escapes both. Two ways to close this:

- **Pin any variable whose `.ptr()` is taken** anywhere in the function.
  Simple and sound; the cost is that such variables are never freed early.
  This is the recommendation: unsafe code is rare, and nobody has to learn
  a keep-alive idiom.
- Make the rule part of 11.1 ("valid until the owner's last use") and leave
  it to the unsafe code. That is Mojo's position, and its footgun.

## Measurements

From the prototype, statement-granularity frees on top of the current
analysis (macOS, Apple M5, `--release`):

- **Correctness.** Every program in `tests/run` prints its `.out`, and all
  86 run clean under AddressSanitizer and UBSan.
- **How often.** In the C for `tests/run`, `examples` and `bench`, 430 of
  4,232 drops (10%) move earlier.
- **Benchmarks.** No measurable change: run times are within the timer's
  10 ms resolution of each other, and peak memory is identical. These
  programs are compute-bound and keep no dead data around.
- **Peak memory.** The program at the top, with 25 million `int`s per
  phase: peak RSS **383 MB → 192 MB**, time 0.17 s → 0.15 s (the second
  phase reuses the first one's pages).
- **Tail calls.** In the same C, 239 `return f(...)` statements, 37 with
  drops between the call and the `return`. In 32 of those, a value dropped
  there is one the call itself uses (an argument temporary, or a variable it
  borrows), which no drop timing can move. The prototype unblocks none of
  the other 5: there the value is dead on the returning branch but still
  used on another, which takes step 2 of the plan. Even then the gain is at
  most 5 of 239. The rest would take passing ownership into the callee, a
  separate question.

## Costs and risks

- **Debugging.** In a debugger, a variable whose memory was freed early
  shows freed memory until its scope ends. Debug builds could keep
  scope-end drops, at the price of debug and release builds using memory
  differently (a program could run out of memory only in debug builds).
  The recommendation is to free early in both: Gojo's users mostly debug
  through prints and tests, not debuggers.
- **Unsafe code,** as above.
- **Compile time.** One more check per statement over the analysis already
  running; not measurable in the prototype.
- **Nothing else changes.** Values with observable drops, task stacks and
  hibernation (which deal with stacks, not heap memory), scopes and
  cancellation are not involved.

## Not proposed

- **ASAP for values whose drop can be observed** (`drop` methods, channel
  ends, task handles, `Rc`): it would change what programs do, and Gojo's
  channels, tasks and cancellation rely on scope-end drops. `drop(x)` is
  the explicit way.
- **Expression granularity** (freeing in the middle of a statement, as
  Mojo does): statements are where temporaries end anyway, and the
  measurements show no case where finer would matter.

## Plan

1. Statement-granularity frees, as prototyped, with `.ptr()` pinning;
   extend `tests/run/drop_states.gojo`, run AddressSanitizer and the leak
   comparison (old and new compiler, macOS `leaks --atExit`) over the
   corpus.
2. Free on the paths where a value is dead: at the start of a branch that
   doesn't use a value its sibling branches still need (Mojo frees there).
   The prototype doesn't; measure whether it matters.
3. Measure larger programs (`examples/http`, `examples/todo`) for peak
   memory under load.

Decisions needed: whether the spec may leave the timing of silent drops to
the compiler (the sentence in "The rule"), and whether to pin variables
whose `.ptr()` is taken.
