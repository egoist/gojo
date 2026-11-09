# Ideas from other languages

Nov 9, 2026

This note surveys newer languages and recent language research. For each of Gojo's limits, it asks what Gojo could borrow to lift it. The limits include both those Gojo chose on purpose and those it simply hasn't solved. Gojo is an experiment, so no rule is protected here. Each proposal is still judged by the same tests Gojo's existing design was: checks stay within one function body and the signatures it calls, compiles stay fast, and the code stays readable by a coding agent.

The sources are:

- Goose ([aardappel/goose](https://github.com/aardappel/goose), read at commit `f4637bf`, Nov 8 2026).
- Mojo 1.2 ([mojolang.org](https://mojolang.org), Oct 2026).
- Swift 6.x ownership proposals; Hylo; OxCaml.
- Vale, Ante, Koka, Lean 4, Morphic, Effekt, Scala 3 capture checking, Austral.
- The Rust 2026 project goals.
- Zig 0.16 `std.Io`, Go 1.23–1.27, Java Loom, OCaml 5/Eio, Crystal.
- MoonBit, Gleam, C3.
- 2024–2026 studies of code written by language models.

"Status" lines say what the source says about how far each thing has got. Everything under **For Gojo** is a proposal, not a fact.

## Status (Nov 10, 2026)

What the round of work that followed this note did with each proposal, in the suggested order. Each language change got tests that try to get around it, and the borrow-checker ones got an adversarial review whose holes became `tests/errors/` cases.

| Proposal | Status | Where |
| --- | --- | --- |
| P1 must-consume types | Done: `linear struct`/`linear enum`, `[T: any]`, guarded `defer` | spec 5.7, `src/linear.rs` |
| P5 error return traces | Done (debug builds) | spec 8 |
| P5 `check --json` with fixes, `gojo spec` | Done; `docs/brief.md` is the page `gojo spec` prints | README (tools) |
| P6 error-polymorphic closures | Done: an unconstrained error type is `never`, `T ! never` is `T`; `task.tryMap` folded into `task.map` | spec 8 |
| P6 mode-polymorphic projections | Not done: a receiver is written without `&`, so `ref self` would have to infer the mode from how the result is used, and a reader (or agent) could no longer tell from a call whether it writes | — |
| P7 `restrict` on `inout` | Measured, not kept: the machine code of every benchmark was identical (after inlining the C compiler already knows) | — |
| P7 task stack offsets | Measured, not kept: no change on the 100k-chain benchmark | — |
| P2 view collections | Done, with two review rounds; view structs may hold view collections and owned data (moved, not copied), as sketched above | spec 5.4 |
| L1, the long-lived case | Lifted without lifetime names for its common shape: a struct keeps views of its own heap-owning field (`words: Vec[str] of text`), which is frozen, and is then an ordinary value: returned, stored, sent. Views into data some *other* value owns still can't be kept past the variables holding them | spec 5.4 |
| P3 field reflection | Done: `static for`, `x.[f]`, `static T{}`, `static if`. `json` stays compiler-generated: reflection has no enum variants yet, which a library encoder needs | spec 9.1 |
| P4 generators | Done: `gen fn`, `yield` to a `for` loop (the frame stays on the stack, so a generator is consumed by a loop, not stored) | spec 7.3 |
| P5 `requires`, `checkfree`, `gojo claims` | Done | spec 14 |
| P7 SIMD | Done as a library: `std/simd` (`Simd[T, N]`, `Mask[N]`), lane-wise loops the C compiler vectorizes | spec 12 |
| P9 `[authority]` | Done, checked against calls rather than imports (so `std/os` didn't need splitting); `gojo authority` reports the inferred set | spec 2.1 |
| P10 simulated network and disk | Done: in-memory TCP/UDP under `gojo test`, `--faults` for resets, refused dials, lost datagrams and disk errors. Losing writes that weren't `fsync`ed is not modelled: there is no `fsync` yet | spec 10.8 |
| P7 reuse on last-use frees | Done for boxes: a box emptied by a move and the next box of its type in the same block share an allocation (0.53 s → 0.39 s on a tree rewrite). `Rc` reset/reuse is not done | spec 14, `reuse_boxes` |
| Inferred isolation at `go` | Done, within one function: a non-`send` value moved into a task is accepted when it is built from `send` values and nothing else takes a reference to it | spec 10.5, `src/check/isolation.rs` |
| P8a branded ids | Not done: `g.Id` is a type that depends on a variable. Signatures (`fn bfs(g: Arena[Node], from: g.Id)`) and node types (`Vec[g.Id]` in a struct declared outside `g`'s scope) need path-dependent or rank-2 types, a type-system extension that reaches every signature. The measured gain elsewhere (1.08–1.17×) doesn't pay for that yet | — |
| P8b region references | Not done: depends on (a), and is the least proven item on the list | — |
| `inout` fields in view structs | Not done: view structs are copied, and a struct holding an exclusive borrow must not be. It needs move-only views and exclusive sources in the borrow checker, a change the size of view collections that should get its own review round | — |
| Edits that keep a view's target in place | Not done: research only, as noted below | — |
| Non-local `return` from closures | Rejected: `return` inside `x => { ... }` would mean the enclosing function for some calls and the closure for others, depending on whether the callee lets it escape, which a reader can't see at the call | — |
| Building values where they end up | Not done: the C compilers already construct returned structs in the caller's slot (sret), and `v.push(f())` copies one value; nothing measured large enough to justify a guarantee | — |
| A cheaper context switch | Not done: tasks park inside runtime functions (channel, lock and timer code), not at points in generated code, so the clobber-list trick has no live-register set to exploit there | — |
| `SmallString`, `PackedSeq` | Not done: a `copy struct` can't lend a `str` of bytes it holds inline (spec 5.4), so a library `SmallString` would need a non-copy representation; `PackedSeq[E]` needs enum variants in reflection | — |

## Gojo's limits, by kind

| # | Limit | By design? | Where it shows |
| --- | --- | --- | --- |
| L1 | Borrowed data can't go into collections or long-lived structs | Yes: second-class references | Parsers store `Span` ranges; a `Vec[str]` of words is an error |
| L2 | Shared, mutable, cyclic data is awkward | Partly | `Rc` + `Cell` + `Weak`, or `Arena` + `Id` with a run-time generation check |
| L3 | No must-use (linear) types | No, just not done | A transaction that is never committed or rolled back compiles |
| L4 | Values with an observable drop are dropped only at scope end | Yes | Only `drop(x)` releases a lock or file early |
| L5 | No reflection or compile-time code | Yes | Only the compiler can derive JSON (`json.encode` is an intrinsic); YAML, TOML, CSV rows, flags and SQL rows are written by hand |
| L6 | No generators | Partly | Pull iteration means writing a cursor struct; recursive `each` can't be turned into a cursor |
| L7 | Paired APIs | No | `at`/`atMut`, `each`/`eachMut`, `next`/`nextMut`, `task.map`/`task.tryMap` |
| L8 | Task stacks can't move | Yes: tasks borrow other tasks' stacks | Go is 3.5× faster on 100k chained tasks and 1.6× on 1M short ones |
| L9 | No allocator or region control; no SIMD | Partly | Every `Vec` and `String` uses the general allocator |
| L10 | What a function or package may do is not visible | No | Any dependency can open sockets; `[unsafe]` only gates C |
| L11 | Proof and debugging output for agents is thin | No | Errors are text only; an error that escapes `main` has no trace; BCE proofs are only checkable with `--verify-bce` |

## What the field is converging on

Four trends show up across unrelated projects. Each one bears on a Gojo decision.

**1. Non-escaping views are becoming the norm.** Gojo has bet on this.

- **Swift** shipped `~Escapable`, `Span`, `MutableSpan` and `InlineArray` in Swift 6.2. Its yielding accessors (`yielding borrow`/`yielding mutate`, SE-0474) are accepted.
- **Mojo** checks origins within one function ([origin-design.md](https://github.com/modular/modular/blob/main/Mojo/proposals/origin-design.md)).
- **Move** (Sui, Aptos) has run second-class references in production for years. Its borrow checker was formalised in "Tracking Borrows with Regular Expressions" (OOPSLA 2026, [doi](https://doi.org/10.1145/3839521)).

The difference is where each language puts the extra information:

- **Swift** still needs a dependency annotation language: `@lifetime(copy x)` is in its third pitch ([pitch #3](https://forums.swift.org/t/pitch-3-compile-time-lifetime-dependency-annotations/84968)).
- **Mojo** puts origins into types. For example, `StringSpan.split()` returns `List[StringSpan[origin_of(origin)]]`. Users report that signatures got heavy as a result ([forum](https://forum.modular.com/t/what-was-your-biggest-wait-what-moment-in-mojo/2774?page=2)).
- **Gojo's `of`** is the smallest of these designs. The next step is to put more *inference* behind `of`, not more annotation in front of it (see P2).

**2. Linear types are arriving in mainstream languages.**

- **Mojo 1.2**: `struct Transaction(not Deinitable else "call 'commit()' or 'rollback()'")` ([explicit-destroy](https://mojolang.org/docs/manual/lifecycle/explicit-destroy/)).
- **Vale**'s "higher RAII" ([post](https://verdagon.dev/blog/higher-raii-7drl)).
- **Rust**'s 2026–27 goal for `!Forget` types ([goal](https://goals.rust-lang.org/2026/move-trait.html)). Rust needs it because its destructors can be skipped, which is also why it can't have scoped async tasks.
- **Goose** has a written design for an `own` qualifier ([ownership.md](https://github.com/aardappel/goose/blob/main/docs/design/ownership.md)).

Gojo is in a strong position for this. Panics abort, so no path is unwound. `drops.rs` already knows the drop state of every value at every exit.

**3. Reflection is winning over macros.**

- **Rust** has a project goal, "Reflection and comptime", with a landed MVP (`TypeId::info()`). Its stated aim is one serializer for every type, with no derive ([goal](https://goals.rust-lang.org/2026/reflection-and-comptime.html)).
- **Mojo** gets derive-like behaviour from `reflect[T].field_names()` plus trait default methods, with no macros ([reflection](https://mojolang.org/docs/manual/metaprogramming/reflection/)).
- **MoonBit and Roc** derive encoders and decoders in the compiler.
- **Swift macros** show the cost of the alternative: one team's clean build went from 85 s to 152 s ([Tuist](https://tuist.dev/blog/2024/08/26/swift-macros)).
- **C3** says its new `$expand` is "hostile to LSP/IDE tooling" ([C3 0.8](https://c3-lang.org/blog/0_8_0_the_core_language_is_settling/)).

**4. Languages written for agents compete on claims, not syntax.**

- **Goose** was itself written almost entirely by a model. Its design note on that ([llm_frontier.md](https://github.com/aardappel/goose/blob/main/docs/design/llm_frontier.md)) argues that "the unit of review is the claim, not the line". The compiler should state what it proved about each function, such as `checkfree` (no bounds checks left), so a reviewer reads claims instead of loops.
- **Studies** point the same way:
  - Type-constrained decoding more than halves compile errors ([PLDI 2025](https://arxiv.org/abs/2504.09246)).
  - Compiler error text in the prompt fixes about 74% of Rust compile errors ([RustAssistant](https://www.microsoft.com/en-us/research/publication/rustassistant-using-llms-to-fix-compilation-errors-in-rust-code/)).
  - In Rust, 16.7% of model errors are about ownership and lifetimes ([arXiv 2608.00661](https://arxiv.org/pdf/2608.00661)).
  - An unfamiliar language costs more tokens and gives weaker results ([arXiv 2606.13763](https://arxiv.org/abs/2606.13763), [danluu.com/pl-tokens](http://danluu.com/pl-tokens/)).
- **Takeaway for Gojo:** Gojo's syntax is already familiar. What it can add is short, complete reference material in context ([Mog](https://moglang.org)'s whole spec is about 3,200 tokens), structured diagnostics, and claims an agent can check.

## Proposals

The proposals are ordered by value for cost. Each one names the limit it addresses, the precedent, a sketch, and how it fits Gojo's rules.

### P1. Must-consume types (L3, L4)

**Precedent:** Mojo `not Deinitable`, Vale higher RAII, Austral, Rust `!Forget`, Goose `own`.

```gojo
linear struct Tx {
    conn: Conn

    fn commit(sink self) ! DbError
    fn rollback(sink self)
}

fn transfer(db: inout Db) ! DbError {
    let tx = db.begin()
    errdefer tx.rollback()    // runs on error exits where tx is still live
    try tx.exec("...")
    try tx.commit()           // consumes tx, even when it fails
}
```

**The rule.** At every place `drops.rs` would insert an implicit drop of a `linear` value, the compiler reports an error instead. That covers a definite drop or a drop-flag test, at `return`, `try`, `break` or the end of a block. The only ways to consume the value are:

- passing it to a `sink` parameter, which includes the type's own `sink self` methods;
- destructuring it;
- consuming it in a `defer` or `errdefer` on that exit.

The error message can be generated rather than written by hand, because the consuming methods are exactly the type's `sink self` methods: "`tx` is not consumed on this path; call `commit` or `rollback`". Mojo needs a custom `else "..."` message for this; Gojo can derive it.

**Generics.**

- A type parameter stays droppable by default. A generic body that may receive a linear `T` says so (`[T: linear]`) and must consume every `T` it holds.
- Only containers that opt in pay for this. `Vec[Tx]` must be emptied with `v.drain(t => t.commit())`.
- Mojo went the other way: `AnyType` stopped implying droppable. Its 1.0 changelog then warned that generic code "may now need `& Deinitable`", so the bound spread everywhere.

**Why it is sound and cheap.**

- Panics abort, so there is no unwinding path on which a linear value would have to be destroyed.
- Tasks are scoped, so a linear value moved into a task is consumed by that task's body. The same exit rule applies there.
- The analysis already exists.

**What it gives.**

- Transactions and two-phase protocols.
- Goose's "token" pattern: `map.add(x)` returns a token that only `map.remove(token)` can consume.
- A clean answer to L4: a file whose `close` can fail becomes linear, so `try f.close()` is the only way to end it, and its error is no longer swallowed by a silent drop.

### P2. Collections of views that live in variables (L1)

**Precedent:** Goose's root sets, which allow views in arrays and dictionaries with no annotations ([spec §9](https://github.com/aardappel/goose/blob/main/docs/goose_spec.md)); Mojo's `List[StringSpan[origin]]`; the "freshness" marker of reachability types ([arXiv 2404.08217](https://arxiv.org/abs/2404.08217)).

Today `Vec[str]` is an error, because a `Vec` could carry the views anywhere. The proposal makes a collection whose element type is a view a *view type itself*, just as a `view struct` is one. It may live in local variables and view structs only, never in fields of ordinary types, boxes, interface values or escaping closures. That covers `Vec[str]`, `Map[str, int]`, `Vec[Token]` where `Token` is a view struct, and `?str`.

```gojo
view struct Token { kind: Kind, text: str }

view struct Ast {
    src: str
    toks: Vec[Token]      // a view collection: owns its buffer, borrows from src
    nodes: Vec[Node]
}

fn tokenize(src: str, out: inout Vec[Token]) {    // inferred: views in `out` come from `src`
    ...
}

fn count(text: str) -> Map[String, int] {
    var seen: Map[str, int] = [:]                  // borrows text
    for w in text.split(" ") {
        with c = seen.entry(w) { c += 1 }
    }
    return seen.iter().map((k, v) => (k.toString(), v)).collect()    // owns, so it can leave
}
```

What changes in the checker:

1. **Views may own memory.** View types are copy-only today. A view collection owns its buffer, so it is move-only and freed at its last use. That is the memory-only drop case `drops.rs` already handles.
2. **A source set per variable.** Each push adds the pushed view's sources to the collection's source set. While the collection is live, its sources are borrowed: no `inout`, no move, and no free at last use, so the free of a source moves to after the collection's last use. This is how `let` views work today, applied to a set that grows. Goose calls it the "store record".
3. **Signatures learn which `inout` parameters receive views.** For an `inout` view-collection parameter, the per-function source inference that already exists for returned views is extended. It answers "views in `out` come from `src`", or the signature writes it: `out: inout Vec[Token] of src`. The fixpoint and the error messages ("`tokenize` puts views of `src` into `out`") are unchanged in kind.

What this lifts:

- Tokenizers, splitters, and indexes over input that lives for one call tree.
- ASTs over source text, in the common shape where `main` reads the file, parses, checks and emits.

What it leaves:

- A long-lived structure that stores borrowed data still owns its input and uses `Span`s. Mojo covers that case with origin parameters on structs. That needs lifetime names in types, and Gojo should keep refusing it.

**Warning from Mojo.** Interior references invalidated by "any `mut self` call" turn out to be very conservative ([roadmap](https://mojolang.org/docs/roadmap/)). Gojo's existing rule (no `inout` of a borrowed value while the borrow is live) has the same limits. P2 doesn't make them worse.

### P3. Field reflection without an evaluator (L5)

**Precedent:** Mojo `reflect[T]` and `comptime for`; Zig `inline for` over `@typeInfo`; the Rust reflection goal; Roc and MoonBit derive.

The proposal adds two forms. Both are unrolled per monomorphized instance, so they need no compile-time interpreter:

- `static for f in fields(T)` gives `f.name` (a `str` constant), `f.type`, and `x.[f]`, a projection of field `f`.
- `static for v in variants(T)` is the same for enums.
- `static if T: cap` branches on a capability: `copy`, `send`, `hash`, an interface, `struct`, `enum`, `number`, `str`.

```gojo
pub fn encode[T, W: Writer](x: T, w: inout W) {
    static if T: has encode {
        x.encode(&w)                            // a type's own method wins, as with equal/hash today
    } else static if T: number {
        w.number(x)
    } else static if T: struct {
        w.beginObject()
        static for f in fields(T) {
            w.key(f.name)
            encode(x.[f], &w)
        }
        w.endObject()
    }
}
```

**Compile-time cost.** "Static" only ever means field lists, names, types and capability tests. The cost grows with fields × instantiations actually used, which is what `json.encode` already costs. There are no loops over values and no evaluator, so Mojo's documented cost does not apply (`comptime for` "can greatly expand ... compilation time"; its `nn` package took 98 s).

**Errors.** Generic bodies with `static if` are checked per instantiation, as with templates. That is a real cost. Errors must give the path: "`encode[User]`: field `created` (`Time`) has no case". Carbon's checked generics avoid this, but would need a constraint solver (Mojo has one, plus `rebind`). That isn't worth it for a feature used in a few library functions.

**What it lifts.**

- `json` becomes ordinary library code instead of an intrinsic.
- Packages for YAML, TOML, CSV, flags and SQL can be written without macros.
- A small `@key("user_id")` field attribute covers renaming. Go-style tags are the familiar form for agents.

**Also worth doing.** `gojo gen` (Gleam's "generate decoder" code action as a command): it writes plain source that an agent can then edit by hand, for the cases where generated code should be visible.

### P4. Generators whose frame lives in a variable (L6)

**Precedent:** Kotlin sequences, C# `yield`, Zig's planned stackless `Io`, Go `iter.Pull`. Rust's `gen` blocks are blocked by self-borrowing frames ([notes](https://hackmd.io/@rust-project-goals/H1v1bz69Je)).

```gojo
gen fn lines(src: str) -> str {            // yields views into src
    var start = 0
    for i, b in src.bytes() {
        if b == '\n' {
            yield src[start..i]
            start = i + 1
        }
    }
    if start < src.len { yield src[start..] }
}

for line in lines(text) { ... }
let a = lines(x)                         // a frame held in a variable: a view type
let b = lines(y)
while true { let l = a.next() ?? break; ... }
```

**How it compiles.**

- `yield` may appear only directly in the body. The frame is then a C struct plus a `switch`, with a size known at compile time. Zig's lesson is that anything more needs bounds on every indirect call.
- The frame is lowered to the cursor protocol Gojo already has.
- The frame is a view type: it lives in a variable, can't be stored, returned, sent or moved after the first `next`.

**Why it is safe without `Pin`.** Views the body holds across a `yield` stay valid, because nothing can move the frame. Rust needs `Pin` here; Gojo's second-class views make it unnecessary.

**What happens to an unfinished frame.** Its live locals are dropped according to the state it stopped in, using the same per-state drop flags as `drops.rs`.

For recursive `each` traversals, `task.pull(tree)` can run `each` on a pooled stack and switch to it directly. Go's `coroswitch` costs about 10 ns against a few hundred for a goroutine switch ([coro.go](https://go.dev/src/runtime/coro.go)). That turns any push iterator into a cursor without a channel.

### P5. Claims the compiler checks (L11)

**Precedent:** Goose `checkfree` and its proposed `goose claims`; Koka `fip`/`fbip` (functions guaranteed not to allocate); C++26 contracts; the RustAssistant and type-constrained decoding results.

This is a set of small additions, each building on analyses Gojo already runs:

**Function qualifiers.**

- `checkfree fn` is an error if `bce.rs` leaves any bounds check in the body.
- `noalloc fn` is an error on any allocation. That is the same idea as Koka's `fip`.
- `nopanic fn` is for overflow, bounds and unwrap.

```gojo
checkfree fn dot(a: []f32, b: []f32) -> f32
    requires a.len == b.len
{ ... }
```

**`requires` contracts.**

- Clauses are pure comparisons over parameters, `len` and arithmetic.
- In the body they are facts available to BCE.
- At each call site they are checked unless BCE proves them there. A failure panics at the caller's line.
- `gojo doc` prints them.
- No SMT solver. Goose insists on this too: the loop between the agent and the compiler must stay deterministic and fast.

**`gojo claims`.** It prints, per function and as JSON:

- the bounds checks that remain, each with the reason it couldn't be removed;
- the checks that were removed, with the facts used;
- whether the function allocates, may panic, or contains `unsafe`.

Reviewers and agents read this instead of reading loops.

**`gojo check --json` with fix-its.** Each diagnostic gains a span and, where the fix is unambiguous, a structured edit (rustc's "machine-applicable" suggestions). Examples: the `&` an `inout` argument is missing, the move the needless-clone warning suggests, the import to remove.

**Error return traces in debug and test builds.**

- Each `try` that propagates an error appends a function and line to a small per-task buffer. `catch` clears it.
- An error that escapes `main` or a test prints the chain.
- Zig's measurement: the cost scales with how many times an error is returned, not with how deep the stack is ([Kelley](https://andrewkelley.me/post/zig-january-2018-in-review.html)).

**`gojo spec --brief`.** The whole language on one page under 4k tokens, generated from the spec and tested like the tour. Agents pay most for an unfamiliar language, and Mog shows the language card is the cheapest fix.

### P6. Mode-polymorphic projections and error-polymorphic closures (L7)

**Precedent:** Mojo `ref self` with parametric mutability; Hylo's access-polymorphic types; Mojo's typed `raises E` with `E = Never`.

**Mode-polymorphic projections.** A projection written once with `ref self` lends `&T` when called on a read-only receiver and `inout T` when called on a mutable one:

```gojo
fn at(ref self, i: int) yield ref T { yield self.items[i] }
fn each(ref self, body: fn(ref T) -> Flow) { ... }
```

The mode is known at every call site, so checking stays local. This removes `atMut`, `eachMut` and `nextMut` from the standard library and from user types.

**Error-polymorphic closures.** An error type parameter becomes `never` when the closure can't fail:

```gojo
pub fn map[T: send, R: send, E: send](xs: []T, limit: int, f: send fn(T) -> R ! E) -> Vec[R] ! E
task.map(xs, 8, x => x * 2)          // E = never: the result is a plain Vec[R], no try
```

`R ! never` is the same type as `R`, with the same C layout. That folds `task.map`/`task.tryMap`, and every similar pair, into one function.

### P7. Code generation that the C compiler can't do on its own (L8, L9)

All of these are invisible to programs and measurable on `bench/`.

**`restrict` on `inout` parameters.**

- Exclusivity is a language rule, so an `inout` parameter that is not a view can't alias any other parameter. Gojo can say so in the C with `restrict`. It doesn't today.
- On one MSVC kernel, Goose measured code 3.3× slower without this information.
- Tree Borrows (PLDI 2025, [post](https://www.ralfj.de/blog/2025/07/07/tree-borrows-paper.html)) is the reference for what `unsafe` code must then not do. That list belongs in spec 11.1.

**Reuse on last-use frees.**

- When a value dies at its last use and an allocation of the same size follows in the same block, reuse the block.
- For `Rc`/`Arc` with a count of 1, do Lean's `reset`/`reuse`.
- This is the static half of Koka's Perceus (PLDI 2021). Gojo needs no run-time check for unique owners, because ownership already proves uniqueness.

**Building values where they end up.** `v.push(parse(line))` and `return S{...}` would pass the destination down through calls as a guarantee, not as something left to the C compiler. Goose guarantees it.

**Task stack offsets.**

- Every task stack starts at the same offset inside a 2 MB-aligned reservation (`runtime/gojo_sched.c`, `g_prepare`). So the hot top-of-stack lines of every task map to the same cache sets.
- Photon offset each stack's start by a random multiple of 64 bytes. One L1 miss rate fell from 15.83% to about 0 ([Photon](https://photonlibos.github.io/blog-20241014/stackful-coroutine-made-fast.html)).
- This is a five-line experiment on the 100k-chain benchmark.
- **A cheaper context switch.** Photon also used a switch whose clobber list lets the C compiler save only the live registers. Its yield went from 9 ns to 1.5 ns. Gojo generates the C at each park point, so it can do the same.

**SIMD: `Simd[T, N]`.**

- Built on integer const generics and lowered to `__attribute__((vector_size))`, which also covers Wasm SIMD128.
- Lane-wise comparisons return masks through methods (`a.lt(b)`), so `==` keeps meaning one `bool` and operator overloading stays closed.
- Ship reductions and mask→bits from the start. Go's portable `simd` package lacked them, and users went back to architecture-specific code ([issue](https://github.com/golang/go/issues/73787)).

### P8. Arenas whose references can form cycles (L2, L9)

**Precedent:** "When Lifetimes Liberate" (arenas with reachability tracking, [arXiv 2509.04253](https://arxiv.org/abs/2509.04253)); Vale region borrowing; Ante's places ([post](https://antelang.org/blog/safe_cyclic_mutability/)); Goose's index types and grow-only arrays.

There are two levels. The first is cheap; the second is research.

**(a) Branded ids.**

- `let g = Arena[Node]()` gives a type `g.Id`. Each `g.Id` is a `u32` proven to be less than `g.len`.
- `g` can't shrink while any `g.Id` is live, so `g[id]` needs no bounds or generation check.
- `g.Id` is plain data: it can be stored in nodes, giving cycles with no `Rc`.
- A function names the brand by its parameter: `fn bfs(g: Arena[Node], from: g.Id)`. The arena is borrowed, so it can't shrink during the call, and the check stays local.
- Goose measured 1.08–1.17× on graph benchmarks from its version of this.

**(b) Region references.** An `arena a { }` block allocates objects that may refer to each other with `&Node in a`:

```gojo
arena a {
    let x = a.new(Node{name: "x", next: none})
    let y = a.new(Node{name: "y", next: x})
    x.next = y                                  // a cycle, no Rc
}                                               // everything in `a` is freed at once
```

- A reference into `a` is a view whose only source is `a`. It can be stored in objects in `a`, in locals and in view structs.
- Chunks never move and nothing in `a` is freed early, so writing fields is safe in one task.
- Objects in `a` must have memory-only drops.
- When nothing outside points into `a`, the whole arena is `send`, which generalises `isolate { }`.

This is the largest expressiveness gain on the list, and it stays intra-procedural, because the arena is a lexical local like a `scope`. It is also the least proven: no shipping language has it in this form. Prototype it after (a).

### P9. What a package may do (L10)

**Precedent:** Austral's root capability, Eio's `env`, Mog's `requires fs`, Effekt's second-class capabilities.

Gojo already gates C behind `[unsafe]`. The only other ambient authority is the I/O modules of `std`. A sibling table in the manifest closes that:

```toml
[authority]
"example.org/leftpad" = []                 # may not import std/net, os fs/env/process, http, tls
"example.org/s3"      = ["net", "tls"]
```

- **How it is checked:** against the import graph only. It needs no type-system change and no run-time check.
- **Why it is enough:** a package without `net` can still write to an `io.Writer` its caller hands it, which is the object-capability pattern for free.
- **Agent fit:** `gojo add` would record the inferred set, the way the lock file is written.
- **Prerequisite:** `std/os` would split into finer modules.

This is Austral's supply-chain story without threading capability values through every signature. Effekt shows that capability *values* passed as parameters can't leak in a language with second-class parameters: Gojo already has that structure, should finer-grained authority be wanted later.

### P10. Testing with a simulated network and disk

**Precedent:** TigerBeetle VOPR ([docs](https://docs.tigerbeetle.com/about/vopr)), turmoil, `Eio_mock`, Go `synctest`.

- The spec admits that programs using the network are not fully repeatable under `gojo test`.
- All I/O goes through `std`, so the runtime can swap in an in-memory network and a filesystem overlay, both driven by the test seed:
  - latency, reordering, resets, short reads and writes;
  - `ENOSPC`, `EIO`, and losing writes that weren't `fsync`ed.
- It reuses the virtual clock and `--explore`, and needs no language change.

## Smaller ideas

- **Inferred isolation at `go`** (Swift SE-0414 region isolation): accept moving an `Rc` graph into a task when no other local still reaches it. `isolate { }` stays for the cases the inference can't prove.
- **`inout` fields in view structs** (Hylo's `remote inout`, Swift's `Inout<T>` pitch): `view struct Cursor { buf: inout []u8, pos: int }`. The struct holds an exclusive borrow for as long as it lives, under the same rule as an `inout` parameter.
- **Edits that keep a view's target in place** (Ante 2025–26, Vale group borrowing):
  - Today, a live view into a `Vec` blocks every change to the `Vec`.
  - The Ante/Vale split: shape-changing operations (`push`, replacing an enum variant) invalidate views into the buffer, while scalar writes to *other* elements don't.
  - It needs proof that the indices differ, and it weakens `restrict`. Research only.
- **A non-local `return` from a non-escaping closure** (Goose function blocks): `xs.each(x => { if bad(x) { return x } })` could return from the enclosing function. Gojo already compiles `for` over `each` this way; exposing it for plain calls is mostly a parser and checker question.
- **Inline-capacity strings and packed enum sequences** (Goose `u8[..16]` and `Shape..`). Goose's `records` benchmark runs 2.0–2.3× faster than Rust with 4× less memory from storing records inline. A library `SmallString` and an append-only `PackedSeq[E]` are worth measuring before any language change.
- **Effect polymorphism** (Flix, Koka, Scala capture sets): not recommended. Every higher-order function would need effect variables, and inference would have to cross functions.

## Not recommended

| Idea | Seen in | Why not |
| --- | --- | --- |
| Lifetime or origin parameters on structs | Mojo, Rust, Swift `@lifetime` | They lift the long-lived case of L1, at the price of the main thing Gojo removed. Mojo's users report this exact pain |
| A compile-time interpreter, or macros | Zig, Jai, C3, Swift, Rust | Generic bodies checked only per instance, tools must run the evaluator, and the measured build cost. P3 covers the main use |
| Bump-allocated "data stacks" as the memory model | Goose | Each task would need many multi-GB reservations. Goose itself went stackless for coroutines for this reason, and its recursion rules are severe |
| `return x from f` across frames | Goose | Failure becomes invisible in the signatures in between; `T ! E` already makes propagation cheap and visible |
| No exclusivity rules | Goose, Vale | Sound in Goose only because it has no shared-memory threads. Gojo's tasks share memory |
| Copying or segmented stacks | Go, Loom, old Rust | Copying breaks borrows across tasks; segmented stacks thrash at segment boundaries ("hot split"). P7's experiments come first |
| `int` as a one-lane SIMD alias | Mojo 1.0 | Churn and stricter conversions for no gain |
| Panics as values (contained tasks) | Goose `thread_spawn_contained`, Erlang | Gojo's panics are bugs and abort. Supervising them would need unwinding, or drops that run after a broken invariant |
| GPU kernels | Mojo | The C back end would have to emit CUDA or Metal for a subset; nothing in Gojo's target users needs it yet |

## Suggested order

1. **Small changes that unblock others:**
   - must-consume types (P1);
   - `restrict` on `inout` and the stack-offset experiment (P7);
   - error return traces, `check --json` with fix-its, and `spec --brief` (P5);
   - error-polymorphic closures (P6).
2. **The language changes with the biggest wins:**
   - view collections (P2);
   - field reflection, with `json` moved into the library (P3);
   - generators (P4);
   - `checkfree`/`requires`/`gojo claims` (P5);
   - mode-polymorphic projections (P6).
3. **Research prototypes, measured before they are kept:**
   - region references (P8b);
   - edits that keep a view's target in place;
   - inferred isolation;
   - `Simd[T, N]`;
   - reuse on last-use frees.

P2, P4 and P8 all move the borrow checker. Review them adversarially, as the views round was: build programs that try to get around the new rule, then keep the ones that fail as `tests/errors/` cases.

## Sources

- **Goose:**
  - [README](https://github.com/aardappel/goose)
  - [spec](https://github.com/aardappel/goose/blob/main/docs/goose_spec.md)
  - [implementation guide](https://github.com/aardappel/goose/blob/main/docs/implementation.md)
  - [design notes](https://github.com/aardappel/goose/tree/main/docs/design)
  - [benchmarks](https://github.com/aardappel/goose/blob/main/bench/summary.md)
  - [Lobster memory management](https://aardappel.github.io/lobster/memory_management.html)
- **Mojo:**
  - [manual](https://mojolang.org/docs/manual/)
  - [origins](https://mojolang.org/docs/manual/values/lifetimes/)
  - [explicit destruction](https://mojolang.org/docs/manual/lifecycle/explicit-destroy/)
  - [reflection](https://mojolang.org/docs/manual/metaprogramming/reflection/)
  - [1.0](https://mojolang.org/releases/v1.0.0/) and [1.2](https://mojolang.org/releases/v1.2.0/) release notes
  - [async design](https://github.com/modular/modular/blob/main/Mojo/proposals/async-design.md)
  - [roadmap](https://mojolang.org/docs/roadmap/)
- **Swift:**
  - [SE-0474 yielding accessors](https://forums.swift.org/t/accepted-se-0474-yielding-accessors/80273)
  - [SE-0507 borrow/mutate accessors](https://forums.swift.org/t/accepted-se-0507-borrow-and-mutate-accessors/85266)
  - [`Borrow`/`Inout` types](https://forums.swift.org/t/borrow-and-inout-types-for-safe-first-class-references/84490)
  - [lifetime dependencies](https://forums.swift.org/t/pitch-3-compile-time-lifetime-dependency-annotations/84968)
- **Hylo:**
  - [subscripts](https://docs.hylo-lang.org/language-tour/subscripts)
  - [remote parts](https://github.com/hylo-lang/hylo-old/blob/main/Docs/RemoteParts.md)
- **OxCaml:**
  - [modes](https://oxcaml.org/documentation/modes/intro/)
  - [Data Race Freedom à la Mode](https://iris-project.org/pdfs/2025-popl-drfcaml.pdf)
- **Vale:**
  - [higher RAII](https://verdagon.dev/blog/higher-raii-7drl)
  - [regions](https://verdagon.dev/blog/zero-cost-borrowing-regions-overview)
  - [group borrowing](https://verdagon.dev/blog/group-borrowing)
- **Ante:**
  - [stable mutable references](https://antelang.org/blog/stable_mutable_refs/)
  - [cyclic mutability](https://antelang.org/blog/safe_cyclic_mutability/)
- **Koka FP²:** [blog](https://www.microsoft.com/en-us/research/blog/fp2-fully-in-place-functional-programming-provides-memory-reuse-for-pure-functional-programs/)
- **Morphic:** [OOPSLA 2026](https://theory.stanford.edu/~aiken/publications/papers/oopsla26.pdf)
- **Effekt:** [Effects, Capabilities, and Boxes](https://pl.cs.uni-tuebingen.de/publications/brachthaeuser22effects/)
- **Scala:** [capture and separation checking](https://tanishiking.github.io/posts/introduction-to-scala-3s-capture-checking-and-separation-checking)
- **Rust:**
  - [Polonius alpha](https://blog.rust-lang.org/2026/08/04/enabling-polonius-alpha-on-nightly)
  - [Borrow checker within](https://goals.rust-lang.org/2026/roadmap-borrow-checker-within.html)
  - [`Move`/`Forget`](https://goals.rust-lang.org/2026/move-trait.html)
  - [reflection](https://goals.rust-lang.org/2026/reflection-and-comptime.html)
- **Papers:**
  - [Second-class values, ECOOP 2022](https://drops.dagstuhl.de/entities/document/10.4230/LIPIcs.ECOOP.2022.15)
  - [Reachability types](https://arxiv.org/abs/2404.08217)
  - [Arenas with reachability](https://arxiv.org/abs/2509.04253)
  - [Move borrows as regular expressions](https://doi.org/10.1145/3839521)
- **Zig, Go, Java, OCaml:**
  - [Zig `std.Io`](https://kristoff.it/blog/zig-new-async-io/)
  - [Go coroutines](https://research.swtch.com/coro.pdf)
  - [`testing/synctest`](https://pkg.go.dev/testing/synctest)
  - [Go SIMD](https://go.dev/blog/archsimd)
  - [Loom](https://wiki.openjdk.org/x/WwD5Aw)
  - [OCaml 5 effects](https://arxiv.org/abs/2104.00250)
  - [Photon stackful coroutines](https://photonlibos.github.io/blog-20241014/stackful-coroutine-made-fast.html)
  - [stack strategies](https://arxiv.org/abs/2512.03083)
- **Other languages:**
  - [Gleam 1.9](https://raw.githubusercontent.com/gleam-lang/gleam/main/changelog/v1.9.md)
  - [MoonBit derive](https://docs.moonbitlang.com/en/latest/language/derive.html)
  - [C3 0.8](https://c3-lang.org/blog/0_8_0_the_core_language_is_settling/)
  - [Swift macro build times](https://tuist.dev/blog/2024/08/26/swift-macros)
- **Models writing code:**
  - [type-constrained decoding](https://arxiv.org/abs/2504.09246)
  - [RustAssistant](https://www.microsoft.com/en-us/research/publication/rustassistant-using-llms-to-fix-compilation-errors-in-rust-code/)
  - [errors in model-written code](https://arxiv.org/pdf/2608.00661)
  - [34 chess engines](https://arxiv.org/abs/2606.13763)
  - [pl-tokens](http://danluu.com/pl-tokens/)
  - [Mog](https://moglang.org)
  - [Niko Matsakis on agents](https://smallcultfollowing.com/babysteps/blog/2026/10/14/agentic-dev-and-rust/)
