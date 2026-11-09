// Ownership and borrow checking.
//
// Runs on one function body at a time. Every reference is block-scoped
// (parameters, `for`/`with`/`if let` bindings, views bound by `let`), so a
// single forward pass with "maybe moved" sets and scoped loans is enough —
// no lifetime inference and nothing crosses function boundaries.

use crate::ast::Mode;
use crate::diag::{Diag, Span};
use crate::tir::*;
use crate::types::*;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};

type Path = (LocalId, Vec<usize>);

pub fn check(prog: &Program, tfuncs: &[TFunc]) -> Vec<Diag> {
    let mut diags = vec![];
    kept_type_params(prog, tfuncs, &mut diags);
    let proj = yield_sources(prog, tfuncs, &mut diags);
    let rsrc = return_sources(prog, tfuncs, &proj);
    for tf in tfuncs {
        let fd = prog.func(tf.func);
        if prog.packages[fd.pkg as usize].is_std {
            continue;
        }
        if stored_views(prog, tf, &mut diags) {
            continue;
        }
        let view_ret = prog.is_view(&fd.ret) && fd.yields.is_none();
        let mut b = Bc::new(prog, tf, &proj, &rsrc, view_ret);
        let mut st = St::new();
        b.block(&mut st, &tf.body);
        b.end_scope_defers(&mut st);
        if b.diags.is_empty() {
            needless_clones(prog, tf, &proj, &rsrc, view_ret, &mut diags);
        }
        diags.extend(b.diags);
    }
    diags
}

/// Warnings for `x.clone()` where `x` itself could be moved: `x` is an owned
/// local that nothing uses afterwards, and a copy of it can't be told apart
/// from the original (no `clone` or `drop` method of its own runs, nothing is
/// shared), so moving it does the same without copying. Coding agents clone
/// to get past the borrow checker; this says when they didn't need to.
///
/// Whether nothing uses `x` afterwards is the borrow checker's own answer:
/// the function is checked again with that clone as a move of `x`, and a
/// warning given only if that is accepted.
fn needless_clones(prog: &Program, tf: &TFunc, proj: &HashMap<FuncId, Vec<usize>>, rsrc: &HashMap<FuncId, Vec<usize>>, view_ret: bool, out: &mut Vec<Diag>) {
    let mut cands: Vec<(usize, LocalId, Span)> = vec![];
    visit_block(&tf.body, &mut |e| match &e.kind {
        TK::Intrinsic { name, args, .. } if name == "clone" && args.len() == 1 => {
            if let TK::Local(l) = args[0].e.kind {
                cands.push((e as *const TExpr as usize, l, e.span));
            }
        }
        _ => {}
    });
    if cands.is_empty() {
        return;
    }
    // Not in closures (moving a captured value changes what the closure
    // captures) or defers (checked on copies of their code).
    let mut captured: HashSet<LocalId> = HashSet::new();
    let mut inside: HashSet<usize> = HashSet::new();
    walk_block(&tf.body, &mut |n| match n {
        Node::Expr(e) => {
            if let TK::Closure(c) = &e.kind {
                captured.extend(c.captures.iter().map(|c| c.0));
                visit_block(&c.body, &mut |x| {
                    inside.insert(x as *const TExpr as usize);
                });
            }
        }
        Node::Stmt(TStmt::Defer(b) | TStmt::ErrDefer(b)) => visit_block(b, &mut |x| {
            inside.insert(x as *const TExpr as usize);
        }),
        _ => {}
    });
    let probe = Bc::new(prog, tf, proj, rsrc, view_ret);
    for (addr, l, sp) in cands {
        let loc = &tf.locals[l as usize];
        let owned = matches!(loc.kind, LocalKind::Let | LocalKind::Var | LocalKind::Param(Mode::Sink));
        if !owned || loc.name.starts_with("__") || captured.contains(&l) || inside.contains(&addr) || probe.is_copy(&loc.ty) || !prog.drop_silent(&loc.ty) || !prog.plain_clone(&loc.ty) {
            continue;
        }
        let mut b = Bc::new(prog, tf, proj, rsrc, view_ret);
        b.as_move = Some(addr);
        let mut st = St::new();
        b.block(&mut st, &tf.body);
        b.end_scope_defers(&mut st);
        if b.as_move_seen && b.diags.is_empty() {
            out.push(Diag::warning(sp, format!("`{0}.clone()` copies `{0}`, which isn't used after this", loc.name)).note(None, format!("write `{}` to move it instead of copying it", loc.name)));
        }
    }
}

/// For each function returning a view, the arguments (receiver first) the
/// view may point into: what its `of` says, or else what its body returns. A
/// call then borrows only those arguments (`call_view_sources`). Functions
/// that call each other are solved together: each starts at "none" and grows
/// to what its returns point into, given the others, until nothing changes.
/// That least fixpoint is exactly what the bodies can return. Functions with
/// a single argument are left out: they borrow it either way.
fn return_sources(prog: &Program, tfuncs: &[TFunc], proj: &HashMap<FuncId, Vec<usize>>) -> HashMap<FuncId, Vec<usize>> {
    let mut out: HashMap<FuncId, Vec<usize>> = HashMap::new();
    for (i, fd) in prog.funcs.iter().enumerate() {
        if let Some(d) = &fd.ret_of {
            out.insert(i as FuncId, d.clone());
        }
    }
    let cands: Vec<&TFunc> = tfuncs
        .iter()
        .filter(|tf| {
            let fd = prog.func(tf.func);
            prog.is_view(&fd.ret) && fd.yields.is_none() && fd.ret_of.is_none() && fd.recv.iter().count() + fd.params.len() > 1
        })
        .collect();
    for tf in &cands {
        out.insert(tf.func, vec![]);
    }
    for _ in 0..16 {
        let mut changed = false;
        for tf in &cands {
            let got = {
                let mut b = Bc::new(prog, tf, proj, &out, true);
                let mut st = St::new();
                b.block(&mut st, &tf.body);
                b.end_scope_defers(&mut st);
                b.ret_params
            };
            let cur = out.get_mut(&tf.func).unwrap();
            for i in got {
                if !cur.contains(&i) {
                    cur.push(i);
                    changed = true;
                }
            }
        }
        if !changed {
            for v in out.values_mut() {
                v.sort_unstable();
            }
            return out;
        }
    }
    // (it didn't settle: every argument, as for any other call)
    for tf in &cands {
        out.remove(&tf.func);
    }
    out
}

/// For each projection function written in Gojo, the arguments (receiver
/// first) that its `yield`s can point into: the parameter that owns each
/// yielded place, following references to what they borrow. A yield into the
/// function's own local is an error (reported here): it would be gone once the
/// caller's block ends, while the caller may keep views of it.
fn yield_sources(prog: &Program, tfuncs: &[TFunc], diags: &mut Vec<Diag>) -> HashMap<FuncId, Vec<usize>> {
    let mut out = HashMap::new();
    for tf in tfuncs {
        if prog.func(tf.func).yields.is_none() {
            continue;
        }
        let params: Vec<LocalId> = tf.recv.iter().copied().chain(tf.params.iter().copied()).collect();
        // references bound in the body, and the place each one borrows
        let mut bound: HashMap<LocalId, LocalId> = HashMap::new();
        crate::tir::walk_block(&tf.body, &mut |n| {
            if let crate::tir::Node::Stmt(TStmt::LetRef { local, place }) = n {
                if let Some(p) = place_path(place) {
                    bound.insert(*local, p.0);
                }
            }
        });
        let root = |mut l: LocalId| -> LocalId {
            for _ in 0..64 {
                match bound.get(&l).copied().or(tf.locals[l as usize].ref_root) {
                    Some(r) if r != l => l = r,
                    _ => break,
                }
            }
            l
        };
        let mut idx: Vec<usize> = vec![];
        let mut bad: Vec<(Span, String)> = vec![];
        crate::tir::walk_block(&tf.body, &mut |n| {
            if let crate::tir::Node::Stmt(TStmt::Yield(Some(e), sp)) = n {
                let Some(p) = place_path(e) else {
                    idx.extend(0..params.len());
                    return;
                };
                let r = root(p.0);
                match params.iter().position(|q| *q == r) {
                    Some(i) => idx.push(i),
                    None if matches!(tf.locals[r as usize].kind, LocalKind::Let | LocalKind::Var) => bad.push((*sp, tf.locals[r as usize].name.clone())),
                    None => idx.extend(0..params.len()),
                }
            }
        });
        if !prog.packages[prog.func(tf.func).pkg as usize].is_std {
            for (sp, name) in bad {
                diags.push(Diag::new(sp, format!("`yield` lends a reference into `{}`, a local of this function", name)).note(None, "the caller may keep views of what it gets, after this function has ended; yield a part of `self` or of a parameter, or return an owned value instead"));
            }
        }
        idx.sort_unstable();
        idx.dedup();
        out.insert(tf.func, idx);
    }
    out
}

/// Whether a value of this type keeps a view inside storage of its own: a
/// container, box, cell, channel or user type. Views are tracked only while they
/// sit in variables (or, one level down, in an option or result).
fn stores_view(prog: &Program, t: &Ty) -> bool {
    match t {
        Ty::Str | Ty::Slice(_) | Ty::MutexRef(_) | Ty::Func(_) | Ty::Iface(..) | Ty::Ptr(_) | Ty::Scope => false,
        // a view struct sits in a variable like `str`: its views are tracked
        // as the variable's
        Ty::Adt(d, _) if prog.def(*d).view => false,
        // a task's result is stored in the task
        Ty::Task(a) => prog.contains_view(a),
        Ty::Option(a) => stores_view(prog, a),
        Ty::Result(a, b) => stores_view(prog, a) || stores_view(prog, b),
        _ => prog.contains_view(t),
    }
}

/// Values that would store a view can't exist: the borrow checker couldn't
/// follow the view into them. Types written out (`Vec[str]`) are rejected when
/// they are resolved; this catches the ones inference produces (`[s]`, `rc(s)`,
/// `Box(s)`, `[2]str`) and views boxed as interface values. Returns whether it
/// reported anything.
fn stored_views(prog: &Program, tf: &TFunc, diags: &mut Vec<Diag>) -> bool {
    let help = "views can only be kept in variables, where the compiler checks that the data they point into outlives them; store an owned copy (`String`, `Vec[T]`) instead";
    let mut found = false;
    for l in &tf.locals {
        if stores_view(prog, &l.ty) {
            // a lazy chain's accumulator (check/lazy.rs): the chain is what the user wrote
            if l.name.starts_with("__lz") {
                diags.push(Diag::new(l.span, format!("this chain would build a `{}`, which would store borrowed views", prog.show(&l.ty))).note(None, "map the elements to owned values first: `.map(s => s.toString())`"));
            } else {
                diags.push(Diag::new(l.span, format!("`{}` has type `{}`, which would store a borrowed view", l.name, prog.show(&l.ty))).note(None, help));
            }
            found = true;
        }
    }
    if found {
        return true;
    }
    let mut bad: Vec<(Span, String)> = vec![];
    visit_block(&tf.body, &mut |e| {
        if stores_view(prog, &e.ty) {
            bad.push((e.span, format!("a value of type `{}` would store a borrowed view", prog.show(&e.ty))));
        } else if let TK::Coerce(Coercion::ToIface { owned: true }, x) = &e.kind {
            if prog.contains_view(&x.ty) {
                bad.push((x.span, format!("the borrowed view `{}` cannot be boxed into an interface value", prog.show(&x.ty))));
            }
        }
    });
    // report the innermost: the expressions around one repeat it
    let inner: Vec<&(Span, String)> = bad.iter().filter(|(sp, _)| !bad.iter().any(|(o, _)| o != sp && o.file == sp.file && o.lo >= sp.lo && o.hi <= sp.hi)).collect();
    let mut seen = std::collections::HashSet::new();
    for (sp, msg) in inner {
        if seen.insert(*sp) {
            diags.push(Diag::new(*sp, msg.clone()).note(None, help));
        }
    }
    !bad.is_empty()
}

/// Generic functions that keep values of a type parameter beyond the call (in
/// an escaping closure or a goroutine) can't be called with a view for it: the
/// view would outlive the data it points into, and nothing at the call site
/// shows that it is kept.
fn kept_type_params(prog: &Program, tfuncs: &[TFunc], diags: &mut Vec<Diag>) {
    use std::collections::HashSet;
    let mut kept: HashMap<FuncId, HashSet<TParamId>> = HashMap::new();
    // (caller, callee, type arguments, span)
    let mut calls: Vec<(FuncId, FuncId, Vec<Ty>, Span)> = vec![];
    fn params_in(t: &Ty, out: &mut HashSet<TParamId>) {
        t.walk(&mut |x| {
            if let Ty::Param(p) = x {
                out.insert(*p);
            }
        });
    }
    for tf in tfuncs {
        let mut k = HashSet::new();
        visit_block(&tf.body, &mut |e| match &e.kind {
            TK::Closure(c) if c.owned => {
                for (l, _) in &c.captures {
                    params_in(&tf.locals[*l as usize].ty, &mut k);
                }
            }
            TK::Call { callee: Callee::Func(f, targs), .. } if !targs.is_empty() => calls.push((tf.func, *f, targs.clone(), e.span)),
            _ => {}
        });
        // `s.go(...)` on a `Scope` value: the task may outlive the call
        visit_block(&tf.body, &mut |e| {
            if let TK::Spawn { target: SpawnTarget::Value(_), task, .. } = &e.kind {
                if let TK::Closure(c) = &task.kind {
                    for (l, _) in &c.captures {
                        params_in(&tf.locals[*l as usize].ty, &mut k);
                    }
                }
            }
        });
        if !k.is_empty() {
            kept.insert(tf.func, k);
        }
    }
    // a caller passing its own type parameter on to a kept one keeps it too
    loop {
        let mut changed = false;
        for (caller, callee, targs, _) in &calls {
            let Some(ks) = kept.get(callee).cloned() else { continue };
            let tps = &prog.func(*callee).tparams;
            let mut add = HashSet::new();
            for (tp, ta) in tps.iter().zip(targs) {
                if ks.contains(tp) {
                    params_in(ta, &mut add);
                }
            }
            let e = kept.entry(*caller).or_default();
            for p in add {
                changed |= e.insert(p);
            }
        }
        if !changed {
            break;
        }
    }
    let mut seen = std::collections::HashSet::new();
    for (caller, callee, targs, sp) in &calls {
        if prog.packages[prog.func(*caller).pkg as usize].is_std {
            continue;
        }
        let Some(ks) = kept.get(callee) else { continue };
        let fd = prog.func(*callee);
        for (tp, ta) in fd.tparams.iter().zip(targs) {
            if ks.contains(tp) && prog.contains_view(ta) && seen.insert(*sp) {
                let pn = prog.tparam(*tp).name.clone();
                diags.push(Diag::new(*sp, format!("`{}` cannot be called with the borrowed view `{}` for `{}`: it keeps `{}` values after it returns", fd.name, prog.show(ta), pn, pn)).note(Some(fd.span), "in a closure or task that can outlive the call").note(None, "pass an owned value (`.toString()` / `.toVec()`)"));
            }
        }
    }
}

#[derive(Clone)]
struct St {
    moved: Vec<(Path, Span)>,
    reachable: bool,
    /// Task handles moved (joined or cancelled) on every path so far.
    joined: Vec<LocalId>,
}

impl St {
    fn new() -> St {
        St { moved: vec![], reachable: true, joined: vec![] }
    }
    fn join(&mut self, o: &St) {
        if !o.reachable {
            return;
        }
        if !self.reachable {
            *self = o.clone();
            return;
        }
        for m in &o.moved {
            if !self.moved.iter().any(|x| x.0 == m.0) {
                self.moved.push(m.clone());
            }
        }
        self.joined.retain(|l| o.joined.contains(l));
    }
    fn unreachable() -> St {
        St { moved: vec![], reachable: false, joined: vec![] }
    }
}

#[derive(Clone)]
struct Loan {
    target: Path,
    mutable: bool,
    holder: Option<LocalId>,
    span: Span,
    depth: usize,
    /// Held by a returned view while the function's `defer`s run.
    returned: bool,
    /// Taken in an earlier iteration of the loop being checked, by a view
    /// declared outside it.
    carried: bool,
    /// Only these elements of the target (a range of a Vec, array or inout
    /// slice): loans of disjoint ranges of the same target don't conflict.
    range: Option<SymRange>,
    /// Held by a task until its scope ends.
    task: bool,
}

/// A bound of a slice range the checker can compare without knowing values:
/// a constant, an immutable local plus a constant, or the end.
#[derive(Clone, Debug, PartialEq)]
enum RBound {
    Const(i128),
    Var(LocalId, i128),
    End,
}

/// The elements `lo..hi` of a range expression.
#[derive(Clone, Debug, PartialEq)]
struct SymRange {
    lo: RBound,
    hi: RBound,
}

/// Whether `a <= b` for every value the bounds can take (indices are >= 0).
fn bound_le(a: &RBound, b: &RBound) -> bool {
    match (a, b) {
        (_, RBound::End) => true,
        (RBound::Const(0), _) => true,
        (RBound::Const(x), RBound::Const(y)) => x <= y,
        (RBound::Var(l, x), RBound::Var(m, y)) => l == m && x <= y,
        _ => false,
    }
}

fn disjoint(a: &SymRange, b: &SymRange) -> bool {
    bound_le(&a.hi, &b.lo) || bound_le(&b.hi, &a.lo)
}

/// Whether two loans of overlapping paths actually share elements.
fn ranges_meet(pa: &Path, ra: Option<&SymRange>, pb: &Path, rb: Option<&SymRange>) -> bool {
    match (ra, rb) {
        (Some(a), Some(b)) if pa == pb => !disjoint(a, b),
        _ => true,
    }
}

struct LoopCx {
    id: u32,
    /// `defers.len()` at the loop: `break`/`continue` run the ones above.
    defer_level: usize,
    breaks: Vec<St>,
    conts: Vec<St>,
}

struct Bc<'a> {
    prog: &'a Program,
    tf: &'a TFunc,
    /// For projection functions written in Gojo: the arguments their `yield`s
    /// can point into (see `yield_sources`).
    proj: &'a HashMap<FuncId, Vec<usize>>,
    diags: Vec<Diag>,
    loans: Vec<Loan>,
    /// Reference / view locals and the owned paths they borrow.
    holders: HashMap<LocalId, Vec<(Path, bool)>>,
    depth: usize,
    suppress: u32,
    loops: Vec<LoopCx>,
    view_ret: bool,
    /// Per enclosing block, its `defer`s and `errdefer`s (true) in order.
    defers: Vec<Vec<(TBlock, bool)>>,
    /// Block depth each local was declared at (parameters: 0).
    decl_depth: HashMap<LocalId, usize>,
    /// Locals already reported as viewed by the value of the block that drops them.
    escaped: Vec<LocalId>,
    /// Active `with x = mu.lock()` blocks: the body's depth, the locked paths
    /// and the `with` statement's span. Views into locked data can't leave them.
    locked: Vec<(usize, Vec<Path>, Span, String)>,
    /// Checking a closure body: locals not declared in it are captures.
    in_closure: bool,
    /// ... the body of a `for` over a type with `each`.
    in_each_body: bool,
    /// The holders each holder was borrowed through (`with s = mu.lock()`: `s`
    /// through `mu`); re-borrowing through `s` doesn't conflict with `mu`.
    via_parents: HashMap<LocalId, Vec<LocalId>>,
    /// `scope` blocks being checked: (id, depth of the body, anchor). What a
    /// task borrows stays borrowed until its scope's body ends; its handle is
    /// a view of the anchor (see `TK::Scope`).
    tscopes: Vec<(u32, usize, LocalId)>,
    /// While checking a task closure: the depth its borrows last until.
    spawn_depth: Option<usize>,
    /// The `x.clone()` (by address) to check as a move of `x` instead
    /// (`needless_clones`), and whether the check got to it.
    as_move: Option<usize>,
    as_move_seen: bool,
    /// For functions returning a view: the arguments (receiver first) the
    /// view may point into (`return_sources`). Calls of other functions
    /// borrow all their arguments.
    rsrc: &'a HashMap<FuncId, Vec<usize>>,
    /// The parameters (receiver first, as indices) the views this function
    /// returns point into, so far.
    ret_params: Vec<usize>,
    /// Set by `view_sources` when it met a view whose sources it doesn't
    /// know (then a returned view may point into any parameter).
    src_unknown: Cell<bool>,
    /// Variables holding a view that a `match` binding points into, while
    /// the binding lives: (the place, where it was bound, depth). The binding
    /// is a reference into the variable's own storage, so the variable can't
    /// be given another value meanwhile (its loans are on what it views).
    storage: Vec<(Path, Span, usize, &'static str)>,
    /// `match` bindings (references) into a variable holding a view: the
    /// variable's place. Assigning the binding assigns the variable.
    bind_into: HashMap<LocalId, Path>,
    /// Borrows (by where they start) that come from a call whose result
    /// views only some of its arguments: why, for the error that names them.
    why_borrow: HashMap<Span, String>,
}

#[derive(Clone, Copy, PartialEq)]
enum Acc {
    Read,
    Mut,
}

fn overlap(a: &Path, b: &Path) -> bool {
    a.0 == b.0 && (a.1.starts_with(&b.1) || b.1.starts_with(&a.1))
}

/// The local and field path a place refers to (indexing / deref widen to the base).
fn place_path(e: &TExpr) -> Option<Path> {
    match &e.kind {
        TK::Local(l) => Some((*l, vec![])),
        TK::Field { base, index } => {
            let (l, mut p) = place_path(base)?;
            if matches!(base.kind, TK::Local(_) | TK::Field { .. }) {
                p.push(*index);
            }
            Some((l, p))
        }
        TK::Deref(base) | TK::Index { base, .. } | TK::MapIndex { base, .. } | TK::Copy(base) | TK::Move(base) => place_path(base),
        TK::Coerce(Coercion::VecToSlice | Coercion::ArrayToSlice | Coercion::StringToStr, x) => place_path(x),
        TK::Proj { call, .. } => match &call.kind {
            TK::Call { args, .. } => args.first().and_then(|a| place_path(&a.e)),
            _ => None,
        },
        TK::ProjUnwrap { proj, .. } => place_path(proj),
        _ => None,
    }
}

impl<'a> Bc<'a> {
    fn new(prog: &'a Program, tf: &'a TFunc, proj: &'a HashMap<FuncId, Vec<usize>>, rsrc: &'a HashMap<FuncId, Vec<usize>>, view_ret: bool) -> Bc<'a> {
        Bc { prog, tf, proj, diags: vec![], loans: vec![], holders: HashMap::new(), depth: 0, suppress: 0, loops: vec![], view_ret, defers: vec![vec![]], decl_depth: HashMap::new(), escaped: vec![], locked: vec![], in_closure: false, in_each_body: false, via_parents: HashMap::new(), tscopes: vec![], spawn_depth: None, as_move: None, as_move_seen: false, rsrc, ret_params: vec![], src_unknown: Cell::new(false), storage: vec![], bind_into: HashMap::new(), why_borrow: HashMap::new() }
    }

    fn err(&mut self, d: Diag) {
        if self.suppress == 0 && !self.diags.iter().any(|x| x.span == d.span && x.msg == d.msg) {
            self.diags.push(d);
        }
    }

    fn name(&self, p: &Path) -> String {
        let l = &self.tf.locals[p.0 as usize];
        let mut s = l.name.clone();
        let mut t = l.ty.clone();
        for &i in &p.1 {
            if let Ty::Adt(d, args) = &t {
                let fs = self.prog.field_tys(*d, args);
                if let Some(f) = fs.get(i) {
                    s.push('.');
                    s.push_str(&f.0);
                    t = f.1.clone();
                }
            }
        }
        s
    }

    fn is_holder(&self, l: LocalId) -> bool {
        matches!(self.tf.locals[l as usize].kind, LocalKind::Ref { .. }) || self.holders.contains_key(&l)
    }

    /// Owned paths a place depends on (through reference locals).
    fn targets(&self, e: &TExpr) -> Vec<(Path, bool)> {
        if let Some(ts) = self.proj_targets(e) {
            return ts;
        }
        match place_path(e) {
            Some(p) => {
                // inline data of a view variable (`v.arr` of a view struct, an
                // array payload) is in the variable itself, not in what it views
                if self.is_holder(p.0) && !p.1.is_empty() && !matches!(self.tf.locals[p.0 as usize].kind, LocalKind::Ref { .. }) && !self.prog.contains_view(&e.ty) {
                    return vec![(p, false)];
                }
                if self.is_holder(p.0) {
                    self.holders.get(&p.0).cloned().unwrap_or_else(|| {
                        // a reference whose sources weren't recorded
                        self.src_unknown.set(true);
                        vec![]
                    })
                } else {
                    vec![(p, false)]
                }
            }
            None => {
                // (package-level values live as long as the program)
                if e.is_place() && !matches!(e.kind, TK::Global(_)) {
                    self.src_unknown.set(true);
                }
                vec![]
            }
        }
    }

    /// What the reference from a call of a projection written in Gojo points
    /// into: the arguments its `yield`s come from, not just the receiver (a
    /// cursor's `next` may yield into the cursor).
    fn proj_targets(&self, e: &TExpr) -> Option<Vec<(Path, bool)>> {
        let call = match &e.kind {
            TK::Proj { call, .. } => call,
            TK::ProjUnwrap { proj, .. } => match &proj.kind {
                TK::Proj { call, .. } => call,
                _ => return None,
            },
            _ => return None,
        };
        let TK::Call { callee: Callee::Func(f, _), args } = &call.kind else { return None };
        let idx = self.proj.get(f)?;
        let mut out = vec![];
        for &i in idx {
            if let Some(a) = args.get(i) {
                out.extend(self.targets(&a.e));
            }
        }
        Some(out)
    }

    fn is_copy(&self, t: &Ty) -> bool {
        match t {
            Ty::Unit | Ty::Never | Ty::Bool | Ty::Int(_) | Ty::F32 | Ty::F64 | Ty::Str | Ty::Slice(_) | Ty::Ptr(_) | Ty::Error | Ty::ConstInt(_) => true,
            Ty::Option(a) | Ty::Array(a, _) => self.is_copy(a),
            Ty::Result(a, b) => self.is_copy(a) && self.is_copy(b),
            Ty::Adt(d, args) => self.prog.def(*d).is_copy || (self.prog.def(*d).tuple && args.iter().all(|a| self.is_copy(a))),
            Ty::Param(p) => self.prog.tparam(*p).bounds.iter().any(|b| matches!(b, Bound::Cap(Cap::Copy))),
            _ => false,
        }
    }

    // ------------------------------------------------------------------
    // Accesses
    // ------------------------------------------------------------------

    fn check_moved(&mut self, st: &St, p: &Path, sp: Span, what: &str) {
        if !st.reachable {
            return;
        }
        if let Some((mp, msp)) = st.moved.iter().find(|(m, _)| overlap(m, p)).cloned() {
            let n = self.name(p);
            let partial = mp.1.len() > p.1.len();
            let msg = if partial { format!("{} `{}` after part of it (`{}`) was moved", what, n, self.name(&mp)) } else { format!("{} `{}` after it was moved", what, n) };
            let note = if msp == sp { "it was moved here in a previous iteration of the loop".to_string() } else { "moved here".to_string() };
            let mut d = Diag::new(sp, msg).note(Some(msp), note);
            let t = &self.tf.locals[p.0 as usize].ty;
            if !self.is_copy(t) {
                d = d.note(None, "use `.clone()` before moving if you need the value again");
            }
            self.err(d);
        }
    }

    fn check_loans(&mut self, p: &Path, acc: Acc, sp: Span, via: Option<LocalId>) {
        self.check_loans_r(p, None, acc, sp, via)
    }

    /// The range `lo..hi` of slicing `base` when its bounds can be compared
    /// symbolically. `fixed` says which locals keep their value for as long
    /// as the loan lasts.
    fn sym_range(&self, lo: Option<&TExpr>, hi: Option<&TExpr>, fixed: &dyn Fn(LocalId) -> bool) -> Option<SymRange> {
        fn bound(e: &TExpr, fixed: &dyn Fn(LocalId) -> bool) -> Option<RBound> {
            match &e.kind {
                TK::Int(n) => Some(RBound::Const(*n)),
                TK::Copy(x) | TK::Cast(x) => bound(x, fixed),
                TK::Local(l) if fixed(*l) => Some(RBound::Var(*l, 0)),
                TK::Binary { op: BinOp::Add, l, r } => match (bound(l, fixed)?, bound(r, fixed)?) {
                    (RBound::Var(v, a), RBound::Const(b)) | (RBound::Const(b), RBound::Var(v, a)) => Some(RBound::Var(v, a.checked_add(b)?)),
                    (RBound::Const(a), RBound::Const(b)) => Some(RBound::Const(a.checked_add(b)?)),
                    _ => None,
                },
                TK::Binary { op: BinOp::Sub, l, r } => match (bound(l, fixed)?, bound(r, fixed)?) {
                    (RBound::Var(v, a), RBound::Const(b)) => Some(RBound::Var(v, a.checked_sub(b)?)),
                    (RBound::Const(a), RBound::Const(b)) => Some(RBound::Const(a.checked_sub(b)?)),
                    _ => None,
                },
                _ => None,
            }
        }
        let lo = match lo {
            Some(e) => bound(e, fixed)?,
            None => RBound::Const(0),
        };
        let hi = match hi {
            Some(e) => bound(e, fixed)?,
            None => RBound::End,
        };
        Some(SymRange { lo, hi })
    }

    /// Locals that can't change: their value at a call (or a task's start)
    /// is their value for as long as it runs.
    fn immutable_local(&self, l: LocalId) -> bool {
        matches!(self.tf.locals[l as usize].kind, LocalKind::Let | LocalKind::Param(Mode::Borrow)) && self.is_copy(&self.tf.locals[l as usize].ty)
    }

    /// Evaluates what a loop iterates over. A range of a place is checked
    /// against loans by the loop itself, with its range, not as a use of the
    /// whole place.
    fn iter_expr(&mut self, st: &mut St, iter: &TExpr) {
        if let TK::SliceOf { base, lo, hi } = &iter.kind {
            if base.is_place() {
                self.subexprs_of_place(st, base);
                if let Some(p) = place_path(base) {
                    self.check_moved(st, &p, base.span, "use of");
                }
                for x in [lo, hi].into_iter().flatten() {
                    self.expr(st, x);
                }
                return;
            }
        }
        self.expr(st, iter);
    }

    /// What a loop over `iter` borrows while it runs: a place, or the
    /// elements a range of one (`xs[a..b]`, `v[i..]`) covers.
    fn iter_targets(&self, iter: &TExpr) -> Option<(Vec<(Path, bool)>, Option<SymRange>)> {
        if iter.is_place() {
            return Some((self.targets(iter), None));
        }
        match &iter.kind {
            TK::SliceOf { base, lo, hi } => {
                let ts = self.view_sources(iter).ok()?;
                // the range is relative to the base: only meaningful when the
                // base is the place itself, not a view of something else
                let direct = place_path(base).is_some_and(|p| !self.is_holder(p.0)) && base.is_place();
                let range = if direct { self.sym_range(lo.as_deref(), hi.as_deref(), &|l| self.immutable_local(l)) } else { None };
                Some((ts, range))
            }
            TK::Coerce(Coercion::VecToSlice | Coercion::ArrayToSlice | Coercion::StringToStr, x) if x.is_place() => Some((self.targets(x), None)),
            _ => None,
        }
    }

    fn check_loans_r(&mut self, p: &Path, range: Option<&SymRange>, acc: Acc, sp: Span, via: Option<LocalId>) {
        let conflict = self.loans.iter().find(|l| overlap(&l.target, p) && ranges_meet(&l.target, l.range.as_ref(), p, range) && (acc == Acc::Mut || l.mutable) && (via.is_none() || l.holder != via)).cloned();
        if let Some(l) = conflict {
            let n = self.name(p);
            if l.returned {
                self.err(Diag::new(sp, format!("a `defer` cannot modify `{}`: the view being returned points into it", n)).note(Some(l.span), "returned here; the `defer` runs after this value is computed"));
                return;
            }
            if l.task {
                let what = if l.mutable { "modifies" } else { "reads" };
                let msg = if l.carried {
                    format!("a task started in an earlier iteration of this loop {} `{}`, and may still be running", what, n)
                } else {
                    format!("cannot {} `{}`: a task {} it until its scope ends", if acc == Acc::Mut { "modify" } else { "use" }, n, what)
                };
                self.err(Diag::new(sp, msg).note(Some(l.span), "the task").note(None, "tasks keep what they borrow until their `scope` ends; give each task its own data, collect results with `let t = go f()` and `t.join()`, or share it through a `Mutex`"));
                return;
            }
            if l.carried {
                let h = l.holder.map(|h| self.tf.locals[h as usize].name.clone()).unwrap_or_default();
                self.err(Diag::new(sp, format!("cannot modify `{}` here: `{}` may still view it from the previous iteration of the loop", n, h)).note(Some(l.span), format!("`{}` is set here, and keeps the view into the next iteration", h)).note(None, format!("declare `{}` inside the loop if each iteration sets it before using it", h)));
                return;
            }
            let by = match l.holder {
                Some(h) if !self.tf.locals[h as usize].name.starts_with("__") => format!(" by `{}`", self.tf.locals[h as usize].name),
                _ => String::new(),
            };
            let msg = match (acc, l.mutable) {
                (Acc::Mut, false) => format!("cannot modify `{}` while it is borrowed{}", n, by),
                (Acc::Mut, true) => format!("cannot modify `{}` while it is mutably borrowed{}", n, by),
                (Acc::Read, _) => format!("cannot use `{}` while it is mutably borrowed{}", n, by),
            };
            let mut d = Diag::new(sp, msg).note(Some(l.span), "the borrow starts here and lasts until the end of its block");
            if let Some(w) = self.why_borrow.get(&l.span) {
                d = d.note(None, w.clone());
            }
            self.err(d);
        }
    }

    fn read(&mut self, st: &mut St, e: &TExpr) {
        self.subexprs_of_place(st, e);
        if let Some(p) = place_path(e) {
            if self.is_holder(p.0) {
                self.check_moved(st, &(p.0, vec![]), e.span, "use of");
                return;
            }
            self.check_moved(st, &p, e.span, "use of");
            self.check_loans(&p, Acc::Read, e.span, None);
        }
    }

    fn write(&mut self, st: &mut St, e: &TExpr) {
        self.subexprs_of_place(st, e);
        if let Some(p) = place_path(e) {
            self.check_storage(&p, e.span);
            self.check_own_loans(&p, e.span);
            if self.is_holder(p.0) {
                return;
            }
            self.check_moved(st, &p, e.span, "modification of");
            self.check_loans(&p, Acc::Mut, e.span, None);
        }
    }

    fn do_move(&mut self, st: &mut St, e: &TExpr) {
        self.subexprs_of_place(st, e);
        if let Some(p) = place_path(e) {
            // (views are copied, but a task handle is a view that is used up)
            if self.is_holder(p.0) && !matches!(self.tf.locals[p.0 as usize].ty, Ty::Task(_)) {
                return;
            }
            self.check_moved(st, &p, e.span, "use of");
            if let Some(l) = self.loans.iter().find(|l| overlap(&l.target, &p)).cloned() {
                let n = self.name(&p);
                self.err(Diag::new(e.span, format!("cannot move `{}` while it is borrowed", n)).note(Some(l.span), "borrowed here"));
            }
            if st.reachable {
                if p.1.is_empty() && matches!(self.tf.locals[p.0 as usize].ty, Ty::Task(_)) {
                    st.joined.push(p.0);
                }
                st.moved.push((p, e.span));
            }
        }
    }

    /// Index expressions and receivers inside a place still need checking.
    fn subexprs_of_place(&mut self, st: &mut St, e: &TExpr) {
        match &e.kind {
            TK::Field { base, .. } | TK::Deref(base) => {
                if !base.is_place() {
                    self.expr(st, base);
                } else {
                    self.subexprs_of_place(st, base)
                }
            }
            TK::Index { base, index } => {
                self.subexprs_of_place(st, base);
                self.expr(st, index);
            }
            TK::MapIndex { base, key } => {
                self.subexprs_of_place(st, base);
                self.expr(st, key);
            }
            TK::Proj { call, .. } => self.call_args(st, call),
            TK::ProjUnwrap { proj, msg } => {
                self.subexprs_of_place(st, proj);
                if let Some(m) = msg {
                    self.expr(st, m);
                }
            }
            _ => {}
        }
    }

    fn reinit(&mut self, st: &mut St, p: &Path) {
        st.moved.retain(|(m, _)| !(m.0 == p.0 && m.1.starts_with(&p.1)));
        if p.1.is_empty() {
            st.joined.retain(|l| *l != p.0);
        }
    }

    // ------------------------------------------------------------------
    // Loans
    // ------------------------------------------------------------------

    /// Record that `holder` borrows `targets`. `from` is the expression the
    /// borrow comes from: borrowing again through an existing reference (a
    /// re-borrow) does not conflict with that reference's own loan.
    fn add_loans(&mut self, holder: LocalId, targets: Vec<(Path, bool)>, mutable: bool, sp: Span, st: &St, from: &TExpr) {
        let mut vias: Vec<LocalId> = vec![];
        visit_expr(from, &mut |e| {
            if let TK::Local(l) = e.kind {
                vias.push(l)
            }
        });
        vias.retain(|l| self.holders.contains_key(l));
        let mut i = 0;
        while i < vias.len() {
            for p in self.via_parents.get(&vias[i]).cloned().unwrap_or_default() {
                if !vias.contains(&p) {
                    vias.push(p);
                }
            }
            i += 1;
        }
        self.via_parents.insert(holder, vias.clone());
        for (t, _) in &targets {
            self.check_moved(st, t, sp, "borrow of");
            let acc = if mutable { Acc::Mut } else { Acc::Read };
            let saved: Vec<Loan> = self.loans.clone();
            self.loans.retain(|l| !l.holder.map(|h| vias.contains(&h)).unwrap_or(false));
            self.check_loans(t, acc, sp, None);
            self.loans = saved;
        }
        for (t, _) in &targets {
            self.loans.push(Loan { target: t.clone(), mutable, holder: Some(holder), span: sp, depth: self.depth, returned: false, carried: false, range: None, task: false });
        }
        self.holders.insert(holder, targets.into_iter().map(|(p, _)| (p, mutable)).collect());
    }

    /// Owned paths a view-producing expression borrows; Err if it borrows a temporary.
    fn view_sources(&self, e: &TExpr) -> Result<Vec<(Path, bool)>, Span> {
        match &e.kind {
            TK::Str(_) | TK::Unit | TK::NoneLit => Ok(vec![]),
            TK::Local(l) => Ok(match self.holders.get(l) {
                Some(h) => h.clone(),
                // a view parameter borrows from the caller: it is its own source
                None if matches!(self.tf.locals[*l as usize].kind, LocalKind::Param(_)) && self.prog.is_view(&self.tf.locals[*l as usize].ty) => vec![((*l, vec![]), false)],
                // a scope's handle (`scope s { }`) is its own source too: it
                // lives in the scope's body
                None if self.tf.locals[*l as usize].ty == Ty::Scope && matches!(self.tf.locals[*l as usize].kind, LocalKind::Let) => vec![((*l, vec![]), false)],
                None => {
                    // a view or reference whose sources weren't recorded
                    // (none, or ones this checker doesn't follow)
                    let loc = &self.tf.locals[*l as usize];
                    if self.prog.is_view(&loc.ty) || matches!(loc.kind, LocalKind::Ref { .. }) {
                        self.src_unknown.set(true);
                    }
                    vec![]
                }
            }),
            TK::Copy(x) | TK::Move(x) | TK::SomeLit(x) | TK::OkLit(x) | TK::ErrLit(x) | TK::Wrap(x) | TK::Try { e: x, .. } => self.view_sources(x),
            TK::Coalesce { l, r } => {
                let mut out = self.view_sources(l)?;
                out.extend(self.view_sources(r)?);
                Ok(out)
            }
            TK::Catch { e: x, handler, .. } => {
                let mut out = self.view_sources(x)?;
                if let Some(v) = &handler.value {
                    out.extend(self.view_sources(v)?);
                }
                Ok(out)
            }
            TK::Coerce(Coercion::StringToStr | Coercion::VecToSlice | Coercion::ArrayToSlice, x) => {
                if x.is_place() {
                    Ok(self.targets(x))
                } else {
                    Err(x.span)
                }
            }
            TK::SliceOf { base, .. } => {
                if self.prog.is_view(&base.ty) {
                    self.view_sources(base)
                } else if base.is_place() {
                    Ok(self.targets(base))
                } else {
                    Err(base.span)
                }
            }
            TK::Call { args, callee } => self.call_view_sources(callee, args),
            TK::Intrinsic { args, .. } => self.call_view_sources(&Callee::Value, args),
            TK::Proj { call, .. } => self.view_sources(call),
            TK::ProjUnwrap { proj, .. } => self.view_sources(proj),
            TK::MutexRef(p) => Ok(self.targets(p).into_iter().map(|(t, _)| (t, true)).collect()),
            // a view struct or enum views what its view fields do
            TK::StructLit { fields: xs, .. } | TK::EnumLit { args: xs, .. } if self.prog.is_view(&e.ty) => {
                let mut out = vec![];
                for x in xs {
                    if self.prog.contains_view(&x.ty) {
                        out.extend(self.view_sources(x)?);
                    }
                }
                Ok(out)
            }
            // a field of a view struct views what the struct does; where the
            // struct is its own source (a parameter), just that field of it,
            // so the struct's other fields can still change (`self.pos += 1`
            // while `self.src` is viewed)
            TK::Field { base, .. } if self.prog.is_view(&base.ty) => {
                let srcs = self.view_sources(base)?;
                Ok(match place_path(e) {
                    Some(fp) => srcs.into_iter().map(|(p, m)| if p.0 == fp.0 && fp.1.starts_with(&p.1) { (fp.clone(), m) } else { (p, m) }).collect(),
                    None => srcs,
                })
            }
            TK::Block(b) | TK::Unsafe(b) | TK::Scope { body: b, .. } => match &b.value {
                Some(v) => self.view_sources(v),
                None => Ok(vec![]),
            },
            // A task handle is a view of its scope (the task's own borrows last
            // until the scope ends): of the anchor of a `scope` block, or of
            // the `Scope` value it was started through.
            TK::Spawn { handle: true, target: SpawnTarget::Lexical(id), .. } => Ok(self.tscopes.iter().rev().find(|t| t.0 == *id).map(|t| vec![((t.2, vec![]), false)]).unwrap_or_default()),
            TK::Spawn { handle: true, target: SpawnTarget::Value(v), .. } => self.view_sources(v),
            TK::Spawn { .. } | TK::Isolate(_) => Ok(vec![]),
            TK::If { then, else_, .. } => {
                let mut out = vec![];
                for b in [Some(then), else_.as_ref()].into_iter().flatten() {
                    if let Some(v) = &b.value {
                        out.extend(self.view_sources(v)?);
                    }
                }
                Ok(out)
            }
            TK::Match { arms, .. } => {
                let mut out = vec![];
                for a in arms {
                    out.extend(self.view_sources(&a.body)?);
                }
                Ok(out)
            }
            // Anything else that produces a view: conservatively, it views
            // every place and view inside it. (Kinds that can't produce views
            // borrow nothing.)
            _ if self.prog.is_view(&e.ty) => {
                let mut out = vec![];
                let mut inner: Vec<TExpr> = vec![];
                let mut first = true;
                visit_expr(e, &mut |x| {
                    if first {
                        first = false;
                        return;
                    }
                    if self.prog.is_view(&x.ty) || x.is_place() {
                        inner.push(x.clone());
                    }
                });
                for x in &inner {
                    if x.is_place() && !self.prog.is_view(&x.ty) {
                        out.extend(self.targets(x));
                    } else if let TK::Local(_) = x.kind {
                        // (a view parameter is its own source)
                        out.extend(self.view_sources(x).unwrap_or_default());
                    }
                }
                Ok(out)
            }
            _ => Ok(vec![]),
        }
    }

    /// What the view a call returns points into: the arguments the callee's
    /// view may point into (all of them when that isn't known), or, for a
    /// function value, what it captured.
    fn call_view_sources(&self, callee: &Callee, args: &[TArg]) -> Result<Vec<(Path, bool)>, Span> {
        let only: Option<&Vec<usize>> = match callee {
            Callee::Func(f, _) => self.rsrc.get(f).filter(|_| {
                let fd = self.prog.func(*f);
                fd.recv.iter().count() + fd.params.len() == args.len()
            }),
            _ => None,
        };
        let mut out = vec![];
        for (i, a) in args.iter().enumerate() {
            if only.is_some_and(|o| !o.contains(&i)) {
                continue;
            }
            // a function value can return a view of what it captured
            if matches!(a.e.ty, Ty::Func(_)) {
                match &a.e.kind {
                    TK::Closure(c) if c.owned => return Err(a.e.span),
                    TK::Closure(c) => {
                        for (l, _) in &c.captures {
                            let le = TExpr::new(TK::Local(*l), self.tf.locals[*l as usize].ty.clone(), c.span);
                            out.extend(self.targets(&le));
                        }
                    }
                    _ if a.e.is_place() => out.extend(self.targets(&a.e)),
                    _ => {}
                }
                continue;
            }
            // a borrowed interface argument is the value behind it
            if let TK::Coerce(Coercion::ToIface { owned: false }, x) = &a.e.kind {
                if x.is_place() {
                    out.extend(self.targets(x));
                }
                continue;
            }
            if a.mode == ArgMode::Value && !self.prog.is_view(&a.e.ty) {
                continue;
            }
            if self.prog.is_view(&a.e.ty) {
                out.extend(self.view_sources(&a.e)?);
            } else if a.e.is_place() {
                out.extend(self.targets(&a.e));
            } else if matches!(a.e.ty, Ty::String | Ty::Vec(_)) {
                return Err(a.e.span);
            }
        }
        Ok(out)
    }

    /// Views into data locked by a `with` block must not outlive the block
    /// (`outer`: the depth of the variable the view is stored in; None: it leaves
    /// the function).
    fn check_locked(&mut self, srcs: &[(Path, bool)], sp: Span, outer: Option<usize>) {
        let hit = self.locked.iter().find(|(d, ps, _, _)| outer.is_none_or(|o| o < *d) && srcs.iter().any(|(p, _)| ps.iter().any(|l| overlap(l, p)))).cloned();
        if let Some((_, _, wsp, n)) = hit {
            self.err(Diag::new(sp, format!("a view into data locked by `{}` cannot leave the `with` block", n)).note(Some(wsp), "the lock is released at the end of this block, and another task could then change the data").note(None, "copy what you need: `.toString()` / `.toVec()` / `.clone()`"));
        }
    }

    /// `holder` (declared at `depth`) now views `srcs`: they must live at least
    /// as long as it, and stay borrowed while it is in use.
    fn bind_view(&mut self, holder: LocalId, depth: usize, srcs: Vec<(Path, bool)>, mutable: bool, sp: Span, st: &St, from: &TExpr) {
        if let Some(w) = self.why_call_borrows(from) {
            self.why_borrow.insert(sp, w);
        }
        self.check_locked(&srcs, sp, Some(depth));
        for (p, _) in &srcs {
            let od = self.decl_depth.get(&p.0).copied().unwrap_or(0);
            // An owner in the same block must come first: values are dropped
            // in reverse order, and a `defer` between the two declarations
            // would run after the owner is gone (locals are numbered in
            // declaration order).
            if od > depth || (od == depth && od > 0 && p.0 > holder) {
                if self.escaped.contains(&p.0) {
                    // reported where its block's value took the view
                    return;
                }
                let (o, osp) = (self.tf.locals[p.0 as usize].name.clone(), self.tf.locals[p.0 as usize].span);
                let h = self.tf.locals[holder as usize].name.clone();
                if o.starts_with("__c") {
                    // the hidden cursor of a `for` loop over a type with `cursor` / `next`
                    self.err(Diag::new(sp, format!("`{}` would still view a loop element after the loop's cursor moved on", h)).note(Some(osp), "this type's `next` yields references into its cursor, which changes with every step").note(None, "store an owned copy (`.toString()` / `.clone()`), or have `next` yield parts of `self`"));
                    return;
                }
                if self.tf.locals[p.0 as usize].ty == Ty::Scope {
                    // a task handle or a `Scope` value, of a scope that ends first
                    let h = self.tf.locals[holder as usize].name.clone();
                    let what = if o.starts_with("__scope") { "its scope".to_string() } else { format!("the scope `{}`", o) };
                    self.err(Diag::new(sp, format!("`{}` would outlive {}: a task handle or `Scope` value can't leave the scope it belongs to", h, what)).note(Some(osp), "the scope ends here, after waiting for its tasks; a handle or `Scope` kept past that would point at a scope that is gone").note(None, "join the task (or use the scope) inside the scope, and keep its result instead"));
                    return;
                }
                if o.starts_with("__h") {
                    // a hidden binding, such as the unwrapped value of `x?.m()` on a temporary
                    self.err(Diag::new(sp, format!("`{}` would view a temporary value that is dropped at the end of this statement", h)).note(None, "keep the value in a variable first (`let x = ...`), or store an owned copy (`.toString()` / `.clone()`)"));
                    return;
                }
                let hsp = self.tf.locals[holder as usize].span;
                let same_block = od == depth;
                let mut d = Diag::new(sp, format!("`{}` does not live long enough: `{}` would still view it after it is dropped", o, h)).note(
                    Some(osp),
                    if same_block { format!("`{}` is declared after `{}`, so it is dropped first (and a `defer` in between would see it gone)", o, h) } else { format!("`{}` is dropped at the end of the block it is declared in", o) },
                );
                if hsp != osp && !same_block {
                    d = d.note(Some(hsp), format!("`{}` is declared here, outside that block", h));
                }
                self.err(d.note(None, "store an owned copy instead (`.toString()` / `.toVec()`)"));
                return;
            }
        }
        let saved = self.depth;
        self.depth = depth;
        let old = self.holders.get(&holder).cloned().unwrap_or_default();
        self.add_loans(holder, srcs, mutable, sp, st, from);
        self.depth = saved;
        // a reassigned view may still be the old one on some path
        if let Some(h) = self.holders.get_mut(&holder) {
            for o in old {
                if !h.contains(&o) {
                    h.push(o);
                }
            }
        }
    }

    /// For a call whose result views only some of its arguments: which
    /// ones, and where that comes from (the function's `of`, or its body).
    fn why_call_borrows(&self, e: &TExpr) -> Option<String> {
        let mut e = e;
        while let TK::Move(x) | TK::Copy(x) | TK::SomeLit(x) | TK::OkLit(x) | TK::Try { e: x, .. } = &e.kind {
            e = x;
        }
        let TK::Call { callee: Callee::Func(f, _), args } = &e.kind else { return None };
        let only = self.rsrc.get(f)?;
        let fd = self.prog.func(*f);
        let params: Vec<&str> = fd.recv.iter().chain(fd.params.iter()).map(|p| p.name.as_str()).collect();
        if params.len() != args.len() || only.len() >= params.len() {
            return None;
        }
        let names: Vec<String> = only.iter().filter_map(|&i| params.get(i)).map(|n| format!("`{}`", n)).collect();
        let what = if names.is_empty() { "none of its arguments".to_string() } else { format!("only {}", names.join(" and ")) };
        Some(match &fd.ret_of {
            Some(_) => format!("`{}` returns a view of {} (its signature says so with `of`)", fd.name, what),
            None if !names.is_empty() => {
                let plain: Vec<&str> = only.iter().filter_map(|&i| params.get(i).copied()).collect();
                format!("`{}` returns a view of {}, as its `return`s show (`-> {} of {}` would say so in its signature)", fd.name, what, self.prog.show(&fd.ret), plain.join(", "))
            }
            None => format!("`{}` returns a view of {}, as its `return`s show", fd.name, what),
        })
    }

    /// `v = value` for a view variable `v`.
    fn assign_view(&mut self, st: &mut St, v: LocalId, sp: Span, value: &TExpr) {
        self.assign_view_to(st, v, sp, value, true)
    }

    /// `v = value` for a view variable `v`, or a store into it through a
    /// binding (`storage`: whether bindings into `v` forbid it).
    fn assign_view_to(&mut self, st: &mut St, v: LocalId, sp: Span, value: &TExpr, storage: bool) {
        if storage {
            self.check_storage(&(v, vec![]), sp);
            self.check_own_loans(&(v, vec![]), sp);
        }
        let srcs = match self.view_sources(value) {
            Ok(s) => s,
            Err(tsp) => {
                self.err(Diag::new(tsp, "this temporary value is dropped at the end of the statement, but a view of it is kept").note(None, "bind the value to a variable first: `let s = ...` and borrow from that"));
                return;
            }
        };
        let l = &self.tf.locals[v as usize];
        let name = l.name.clone();
        if self.in_closure && !self.decl_depth.contains_key(&v) {
            if !srcs.is_empty() && self.in_each_body {
                self.err(Diag::new(sp, format!("`{}` can't keep a view of a loop element past the iteration", name)).note(None, "this loop runs through the type's `each` method, which may pass the body something that only lives for one call; store an owned copy (`.toString()` / `.clone()`), or iterate with `cursor` / `next`"));
            } else if !srcs.is_empty() {
                self.err(Diag::new(sp, format!("a closure cannot store a borrowed view in the captured variable `{}`", name)).note(None, "the function that owns the variable couldn't see what it borrows; return the view instead, or store an owned copy"));
            }
            return;
        }
        if matches!(l.kind, LocalKind::Param(Mode::Inout)) {
            // the caller can't see new borrows; re-slicing the view itself is fine
            if let Some((p, _)) = srcs.iter().find(|(p, _)| p.0 != v) {
                let o = self.name(p);
                self.err(Diag::new(sp, format!("cannot store a view of `{}` in the `inout` parameter `{}`", o, name)).note(None, "the caller couldn't see that its variable now borrows this; return the view instead, or use an owned type"));
            }
            return;
        }
        self.reinit(st, &(v, vec![]));
        if !srcs.is_empty() {
            let depth = self.decl_depth.get(&v).copied().unwrap_or(0);
            let mutable = matches!(value.kind, TK::MutexRef(_));
            self.bind_view(v, depth, srcs, mutable, value.span, st, value);
        }
    }

    fn end_depth(&mut self) {
        let d = self.depth;
        self.loans.retain(|l| l.depth < d);
        self.storage.retain(|s| s.2 < d);
    }

    /// Changing the view variable at `p` (a `let` or `var`) while something
    /// views the data it holds inline (`v.arr[0..2]`, a binding of an array
    /// payload). (What it views is borrowed as usual; a parameter's loans are
    /// on what the caller gave it.)
    fn check_own_loans(&mut self, p: &Path, sp: Span) {
        if matches!(self.tf.locals[p.0 as usize].kind, LocalKind::Let | LocalKind::Var) && self.prog.is_view(&self.tf.locals[p.0 as usize].ty) {
            self.check_loans(p, Acc::Mut, sp, None);
        }
    }

    /// Giving the view variable at `p` (or part of it) another value, while a
    /// `match` binding points into it.
    fn check_storage(&mut self, p: &Path, sp: Span) {
        if let Some((sp2, why)) = self.storage.iter().find(|s| overlap(&s.0, p)).map(|s| (s.1, s.3)) {
            let n = self.name(p);
            if why == "task" {
                self.err(Diag::new(sp, format!("cannot change `{}` while a task uses it", n)).note(Some(sp2), "the task is started here and may run until its scope ends").note(None, "give the task its own copy of the view (`let w = x` before `go`), or change the variable after the scope"));
            } else {
                self.err(Diag::new(sp, format!("cannot change `{}` while a `match` binding points into it", n)).note(Some(sp2), "bound here: the binding refers to the value inside this variable").note(None, "copy what the binding holds into its own variable first (`let w = x`), or change the variable after the `match`"));
            }
        }
    }

    // ------------------------------------------------------------------
    // Blocks & statements
    // ------------------------------------------------------------------

    fn block(&mut self, st: &mut St, b: &TBlock) {
        self.depth += 1;
        self.defers.push(vec![]);
        // A reference or view declared in this block stops borrowing after the
        // last statement that mentions it (defers count as the end of the block).
        let mut last_use: HashMap<LocalId, usize> = HashMap::new();
        let mut declared: Vec<LocalId> = vec![];
        for (i, s) in b.stmts.iter().enumerate() {
            match s {
                TStmt::Let { local, .. } | TStmt::LetRef { local, .. } => declared.push(*local),
                _ => {}
            }
            let mut ls = vec![];
            locals_in_stmt(s, &mut ls);
            let at = if matches!(s, TStmt::Defer(_) | TStmt::ErrDefer(_)) { usize::MAX } else { i };
            for l in ls {
                let e = last_use.entry(l).or_insert(at);
                *e = (*e).max(at);
            }
        }
        if let Some(v) = &b.value {
            let mut ls = vec![];
            visit_expr(v, &mut |e| {
                if let TK::Local(l) = e.kind {
                    ls.push(l)
                }
                if let TK::Closure(c) = &e.kind {
                    ls.extend(c.captures.iter().map(|c| c.0))
                }
            });
            for l in ls {
                last_use.insert(l, usize::MAX);
            }
        }
        for (i, s) in b.stmts.iter().enumerate() {
            self.stmt(st, s);
            for &l in &declared {
                if last_use.get(&l).copied().unwrap_or(0) == i && self.holders.contains_key(&l) {
                    self.loans.retain(|ln| ln.holder != Some(l));
                }
            }
        }
        if let Some(v) = &b.value {
            self.expr(st, v);
            // the function's own value
            if self.depth == 1 && self.view_ret && st.reachable {
                self.note_returned(v);
            }
            // the value is used after the block, where its own locals are gone
            // (`f(if c { let s = ...; s.asStr() } else { "" })`)
            if self.prog.is_view(&v.ty) {
                if let Ok(srcs) = self.view_sources(v) {
                    if let Some((p, _)) = srcs.iter().find(|(p, _)| declared.contains(&p.0) && !self.escaped.contains(&p.0)) {
                        let (o, osp) = (self.tf.locals[p.0 as usize].name.clone(), self.tf.locals[p.0 as usize].span);
                        if self.suppress == 0 {
                            self.escaped.push(p.0);
                        }
                        self.err(Diag::new(v.span, format!("`{}` does not live long enough: the block's value views it, but it is dropped where the block ends", o)).note(Some(osp), format!("`{}` is declared in the block", o)).note(None, format!("declare `{}` before the block, or make the value an owned copy (`.toString()` / `.toVec()`)", o)));
                    }
                }
            }
        }
        // a task handle must be joined or cancelled where the block ends
        // normally (leaving early cancels the scope's tasks anyway)
        if st.reachable {
            for &l in &declared {
                if matches!(self.tf.locals[l as usize].ty, Ty::Task(_)) && !st.joined.contains(&l) {
                    let loc = &self.tf.locals[l as usize];
                    let d = Diag::new(loc.span, format!("task `{}` is never joined", loc.name)).note(None, format!("use its result with `{}.join()`, stop it with `{}.cancel()`, or start it with a plain `go` statement if its result isn't needed", loc.name, loc.name));
                    self.err(d);
                }
            }
        }
        self.end_scope_defers(st);
        self.defers.pop();
        self.end_depth();
        self.depth -= 1;
    }

    /// The `defer`s of the innermost block, run where it ends. They run one
    /// after another, so what one moves the next and the code after the block
    /// see as moved. (When the end can't be reached, they ran, and were
    /// checked, on the paths that left the block.)
    fn end_scope_defers(&mut self, st: &mut St) {
        let level = self.defers.len().saturating_sub(1);
        if st.reachable {
            self.run_defers(st, level, false);
        } else {
            let mut s2 = st.clone();
            s2.reachable = true;
            self.run_defers(&mut s2, level, false);
        }
    }

    /// Runs the `defer`s of blocks `from..` (innermost first), and their
    /// `errdefer`s when leaving with an error.
    fn run_defers(&mut self, st: &mut St, from: usize, with_err: bool) {
        let levels: Vec<Vec<(TBlock, bool)>> = self.defers.iter().skip(from).cloned().collect();
        for level in levels.iter().rev() {
            for (b, err_only) in level.iter().rev() {
                if !*err_only || with_err {
                    self.block(st, b);
                }
            }
        }
    }

    fn stmt(&mut self, st: &mut St, s: &TStmt) {
        match s {
            TStmt::Let { local, init } => {
                self.expr(st, init);
                self.reinit(st, &(*local, vec![]));
                self.decl_depth.insert(*local, self.depth);
                let lt = &self.tf.locals[*local as usize].ty;
                if self.prog.is_view(&lt) {
                    match self.view_sources(init) {
                        Ok(srcs) => {
                            if !srcs.is_empty() {
                                let mutable = matches!(init.kind, TK::MutexRef(_));
                                self.bind_view(*local, self.depth, srcs, mutable, init.span, st, init);
                            }
                        }
                        Err(sp) => self.err(Diag::new(sp, "this temporary value is dropped at the end of the statement, but a view of it is kept").note(None, "bind the value to a variable first: `let s = ...` and borrow from that")),
                    }
                }
            }
            TStmt::LetRef { local, place } => {
                self.read(st, place);
                self.decl_depth.insert(*local, self.depth);
                let mutable = matches!(self.tf.locals[*local as usize].kind, LocalKind::Ref { mutable: true });
                let targets = self.targets(place);
                let targets = match self.view_sources_of_place_temp(place) {
                    Some(sp) => {
                        self.err(Diag::new(sp, "this temporary value is dropped at the end of the statement, but a reference to it is kept").note(None, "bind the value to a variable first"));
                        targets
                    }
                    None => targets,
                };
                self.add_loans(*local, targets, mutable, place.span, st, place);
            }
            TStmt::Expr(e) => self.expr(st, e),
            TStmt::Assign { place, op, value, .. } => {
                self.expr(st, value);
                if let (None, TK::Local(v)) = (op, &place.kind) {
                    if self.prog.is_view(&self.tf.locals[*v as usize].ty) {
                        // a `match` binding is a reference into the variable
                        // matched: that variable now views the value too
                        if let Some(p) = self.bind_into.get(v).cloned() {
                            self.assign_view_to(st, p.0, place.span, value, false);
                        }
                        self.assign_view(st, *v, place.span, value);
                        return;
                    }
                }
                // a part of a view variable (`tok.text = s`, `self.rest =
                // self.rest[1..]`): the variable now views what the value does
                // too, as when it is given a whole new value
                if let (None, TK::Field { .. }, Some(p)) = (op, &place.kind, place_path(place)) {
                    if self.prog.is_view(&self.tf.locals[p.0 as usize].ty) && place_is_direct(place) {
                        self.check_storage(&p, place.span);
                        self.check_own_loans(&p, place.span);
                        if self.prog.contains_view(&place.ty) {
                            self.assign_view(st, p.0, place.span, value);
                        }
                        return;
                    }
                }
                match (op, place_path(place)) {
                    (None, Some(p)) if matches!(place.kind, TK::Local(_) | TK::Field { .. }) && !self.is_holder(p.0) && place_is_direct(place) => {
                        // re-initialization: the old value must not be borrowed
                        self.check_loans(&p, Acc::Mut, place.span, None);
                        // the containing value must exist for a field assignment
                        if !p.1.is_empty() {
                            let base: Path = (p.0, p.1[..p.1.len() - 1].to_vec());
                            if let Some((mp, _)) = st.moved.iter().find(|(m, _)| m.0 == base.0 && base.1.starts_with(&m.1)).cloned() {
                                let n = self.name(&mp);
                                self.err(Diag::new(place.span, format!("cannot assign to a field of `{}` after it was moved", n)));
                            }
                        }
                        self.reinit(st, &p);
                    }
                    _ => self.write(st, place),
                }
            }
            TStmt::MapInsert { map, key, value } => {
                self.expr(st, key);
                self.expr(st, value);
                self.write(st, map);
            }
            TStmt::Return(e, sp) => {
                let mut kept = 0;
                if let Some(e) = e {
                    self.expr(st, e);
                    if self.view_ret {
                        self.check_view_return(e, *sp);
                        self.note_returned(e);
                        // the returned view still points into its sources
                        // while the `defer`s run, after it was computed
                        if let Ok(srcs) = self.view_sources(e) {
                            for (t, _) in srcs {
                                self.loans.push(Loan { target: t, mutable: false, holder: None, span: e.span, depth: 0, returned: true, carried: false, range: None, task: false });
                                kept += 1;
                            }
                        }
                    }
                }
                // an error return runs the `errdefer`s too
                let err = e.as_ref().is_some_and(|e| is_error_value(e));
                self.end_scope_defers_all(st, err);
                let n = self.loans.len();
                self.loans.truncate(n.saturating_sub(kept));
                *st = St::unreachable();
            }
            TStmt::Yield(e, _) => {
                if let Some(e) = e {
                    self.read(st, e);
                }
                *st = St::unreachable();
            }
            TStmt::Break(id, _) | TStmt::Continue(id, _) => {
                // the `defer`s of the blocks this leaves run first
                let is_break = matches!(s, TStmt::Break(..));
                if let Some(level) = self.loops.iter().rev().find(|l| l.id == *id).map(|l| l.defer_level) {
                    let mut s2 = st.clone();
                    if s2.reachable {
                        self.run_defers(&mut s2, level, false);
                    }
                    if let Some(l) = self.loops.iter_mut().rev().find(|l| l.id == *id) {
                        if is_break {
                            l.breaks.push(s2);
                        } else {
                            l.conts.push(s2);
                        }
                    }
                }
                *st = St::unreachable();
            }
            TStmt::ErrDefer(b) => {
                if let Some(d) = self.defers.last_mut() {
                    d.push((b.clone(), true));
                }
            }
            TStmt::Defer(b) => {
                if let Some(d) = self.defers.last_mut() {
                    d.push((b.clone(), false));
                }
            }
            TStmt::While { id, cond, body } => {
                self.loop_body(st, *id, |me, s| {
                    if let Some(c) = cond {
                        me.expr(s, c);
                    }
                    me.block(s, body);
                }, cond.is_none(), (cond.as_ref(), body));
            }
            TStmt::For { id, kind, iter, hi, elem, index, mutable, body, .. } => {
                self.iter_expr(st, iter);
                if let Some(h) = hi {
                    self.expr(st, h);
                }
                let holds = !matches!(kind, IterKind::Range { .. } | IterKind::Chan | IterKind::Str) || (*kind == IterKind::Str && iter.is_place());
                self.depth += 1;
                self.decl_depth.insert(*elem, self.depth);
                if let Some(i) = index {
                    self.decl_depth.insert(*i, self.depth);
                }
                if let Some((targets, range)) = self.iter_targets(iter).filter(|_| holds) {
                    for (t, _) in &targets {
                        self.check_loans_r(t, range.as_ref(), if *mutable { Acc::Mut } else { Acc::Read }, iter.span, None);
                    }
                    for (t, _) in &targets {
                        self.loans.push(Loan { target: t.clone(), mutable: *mutable, holder: Some(*elem), span: iter.span, depth: self.depth, returned: false, carried: false, range: range.clone(), task: false });
                    }
                    self.holders.insert(*elem, targets.iter().map(|(p, _)| (p.clone(), *mutable)).collect());
                    if let Some(i) = index {
                        if matches!(self.tf.locals[*i as usize].kind, LocalKind::Ref { .. }) {
                            self.holders.insert(*i, targets.iter().map(|(p, _)| (p.clone(), false)).collect());
                        }
                    }
                }
                let (el, ix) = (*elem, *index);
                self.loop_body(st, *id, |me, s| {
                    me.reinit(s, &(el, vec![]));
                    if let Some(i) = ix {
                        me.reinit(s, &(i, vec![]));
                    }
                    me.block(s, body)
                }, false, (None, body));
                self.end_depth();
                self.depth -= 1;
            }
            TStmt::ForEach { call, .. } => self.expr(st, call),
            TStmt::ParFor { iter, hi, mutable, body, .. } => {
                if iter.is_place() {
                    if *mutable {
                        self.write(st, iter);
                    } else {
                        self.read(st, iter);
                    }
                } else {
                    self.iter_expr(st, iter);
                }
                if let Some(h) = hi {
                    self.expr(st, h);
                }
                // the iterated value stays borrowed while the body runs
                let before = self.loans.len();
                if let Some((targets, range)) = self.iter_targets(iter).filter(|_| hi.is_none()) {
                    if !iter.is_place() {
                        for (t, _) in &targets {
                            self.check_loans_r(t, range.as_ref(), if *mutable { Acc::Mut } else { Acc::Read }, iter.span, None);
                        }
                    }
                    for (t, _) in targets {
                        self.loans.push(Loan { target: t, mutable: *mutable, holder: None, span: iter.span, depth: self.depth + 1, returned: false, carried: false, range: range.clone(), task: false });
                    }
                }
                if let TK::Closure(c) = &body.kind {
                    self.closure(st, c);
                }
                self.loans.truncate(before);
            }
            TStmt::Send { chan, value } => {
                self.expr(st, chan);
                self.expr(st, value);
            }
            TStmt::WithLocks { locals, places, body } => {
                for (l, place) in locals.iter().zip(places) {
                    self.read(st, place);
                    self.depth += 1;
                    self.decl_depth.insert(*l, self.depth);
                    let targets = self.targets(place);
                    let n = place_path(place).map(|p| self.name(&p)).unwrap_or_else(|| "the lock".into());
                    self.locked.push((self.depth, targets.iter().map(|t| t.0.clone()).collect(), place.span, n));
                    self.add_loans(*l, targets, false, place.span, st, place);
                }
                self.block(st, body);
                for _ in places {
                    self.locked.pop();
                    self.end_depth();
                    self.depth -= 1;
                }
            }
            TStmt::With { local, place, lock, body } => {
                self.read(st, place);
                self.depth += 1;
                self.decl_depth.insert(*local, self.depth);
                let mutable = !*lock && matches!(self.tf.locals[*local as usize].kind, LocalKind::Ref { mutable: true });
                let targets = self.targets(place);
                if *lock {
                    let n = place_path(place).map(|p| self.name(&p)).unwrap_or_else(|| "the lock".into());
                    self.locked.push((self.depth, targets.iter().map(|t| t.0.clone()).collect(), place.span, n));
                }
                self.add_loans(*local, targets, mutable, place.span, st, place);
                self.block(st, body);
                if *lock {
                    self.locked.pop();
                }
                self.end_depth();
                self.depth -= 1;
            }
            TStmt::Select { arms, .. } => {
                for a in arms {
                    match &a.kind {
                        TSelectKind::Recv { chan, .. } => self.expr(st, chan),
                        TSelectKind::Send { chan, value } => {
                            self.expr(st, chan);
                            self.expr(st, value);
                        }
                        TSelectKind::Task { task, .. } => self.read(st, task),
                        TSelectKind::Timer { at, .. } => self.expr(st, at),
                        _ => {}
                    }
                    if let Some(g) = &a.guard {
                        self.expr(st, g);
                    }
                }
                let start = st.clone();
                let mut out = St::unreachable();
                for a in arms {
                    let mut s2 = start.clone();
                    if let TSelectKind::Recv { bind: Some(b), .. } | TSelectKind::Task { bind: Some(b), .. } = &a.kind {
                        self.reinit(&mut s2, &(*b, vec![]));
                        self.decl_depth.insert(*b, self.depth + 1);
                    }
                    // the arm that takes a task's result takes its handle
                    if let TSelectKind::Task { task, .. } = &a.kind {
                        self.do_move(&mut s2, task);
                    }
                    self.block(&mut s2, &a.body);
                    out.join(&s2);
                }
                *st = out;
            }
        }
    }

    /// Leaving the function: every enclosing block's `defer`s run, in order.
    fn end_scope_defers_all(&mut self, st: &mut St, with_err: bool) {
        let mut s2 = st.clone();
        self.run_defers(&mut s2, 0, with_err);
    }

    fn view_sources_of_place_temp(&self, place: &TExpr) -> Option<Span> {
        // `let r = makeVec()[0]`: a reference into a temporary
        fn base_temp(e: &TExpr) -> Option<Span> {
            match &e.kind {
                TK::Field { base, .. } | TK::Index { base, .. } | TK::MapIndex { base, .. } | TK::Deref(base) => {
                    if base.is_place() {
                        base_temp(base)
                    } else if matches!(base.kind, TK::Proj { .. }) {
                        None
                    } else {
                        Some(base.span)
                    }
                }
                _ => None,
            }
        }
        base_temp(place)
    }

    /// Record which parameters a returned view points into (for callers,
    /// `return_sources`), and check them against the function's `of`.
    fn note_returned(&mut self, e: &TExpr) {
        self.src_unknown.set(false);
        // (a view of a temporary is an error reported elsewhere; until then,
        // and where nothing reports it, it may point into anything)
        let (srcs, temp) = match self.view_sources(e) {
            Ok(s) => (s, false),
            Err(_) => (vec![], true),
        };
        let params: Vec<LocalId> = self.tf.recv.iter().copied().chain(self.tf.params.iter().copied()).collect();
        let mut got: Vec<usize> = vec![];
        let mut all = temp || self.src_unknown.get();
        for (p, _) in &srcs {
            match params.iter().position(|q| *q == p.0) {
                Some(i) => got.push(i),
                None => match self.tf.locals[p.0 as usize].kind {
                    // a view of this function's own value: an error, reported
                    // by `check_view_return`
                    LocalKind::Let | LocalKind::Var => {}
                    _ => all = true,
                },
            }
        }
        if all {
            got = (0..params.len()).collect();
        }
        got.sort_unstable();
        got.dedup();
        let fd = self.prog.func(self.tf.func);
        if let Some(decl) = &fd.ret_of {
            let extra: Vec<String> = got.iter().filter(|i| !decl.contains(i)).map(|&i| format!("`{}`", self.tf.locals[params[i] as usize].name)).collect();
            if !extra.is_empty() {
                let mut names: Vec<String> = decl.iter().chain(got.iter()).map(|&i| self.tf.locals[params[i] as usize].name.clone()).collect();
                names.dedup();
                let mut seen = HashSet::new();
                names.retain(|n| seen.insert(n.clone()));
                let ret = self.prog.show(&fd.ret);
                let what = if all { format!("may point into {}", extra.join(", ")) } else { format!("points into {}", extra.join(", ")) };
                self.err(Diag::new(e.span, format!("this view {}, but the function's `of` doesn't name {}", what, if extra.len() == 1 { "it" } else { "them" })).note(None, format!("callers rely on `of`: name everything the view can point into (`-> {} of {}`)", ret, names.join(", "))));
            }
        }
        for i in got {
            if !self.ret_params.contains(&i) {
                self.ret_params.push(i);
            }
        }
    }

    /// The type of the place `p` (a local and fields), when it can be told.
    fn place_ty(&self, p: &Path) -> Option<Ty> {
        let mut t = self.tf.locals.get(p.0 as usize)?.ty.clone();
        for &i in &p.1 {
            t = match &t {
                Ty::Adt(d, args) if self.prog.def(*d).is_struct() => self.prog.field_tys(*d, args).get(i)?.1.clone(),
                _ => return None,
            };
        }
        Some(t)
    }

    fn check_view_return(&mut self, e: &TExpr, sp: Span) {
        match self.view_sources(e) {
            Ok(srcs) => {
                self.check_locked(&srcs, sp, None);
                for (p, _) in srcs {
                    let l = &self.tf.locals[p.0 as usize];
                    if l.ty == Ty::Scope && matches!(l.kind, LocalKind::Let) {
                        let lsp = l.span;
                        self.err(Diag::new(sp, "returns a task handle or `Scope` value of a scope in this function, which has ended by then").note(Some(lsp), "the scope").note(None, "join the task inside the scope and return its result instead"));
                        continue;
                    }
                    // a copy parameter is passed by value: a view into its own
                    // data (not into what it views) points into this call
                    if matches!(l.kind, LocalKind::Param(Mode::Borrow)) && self.is_copy(&l.ty) && self.place_ty(&p).is_some_and(|t| !self.prog.contains_view(&t)) && (!p.1.is_empty() || !self.prog.is_view(&l.ty)) {
                        let n = l.name.clone();
                        let lsp = l.span;
                        self.err(Diag::new(sp, format!("returns a view into `{}`, which this function got as a copy", n)).note(Some(lsp), "a copy type is passed by value: the view would point into this call's copy, gone when it returns").note(None, format!("take `{}` as `inout` to view the caller's value, or return an owned copy (`.toVec()`)", n)));
                        continue;
                    }
                    if matches!(l.kind, LocalKind::Let | LocalKind::Var | LocalKind::Param(Mode::Sink)) && !self.escaped.contains(&p.0) {
                        let n = l.name.clone();
                        let lsp = l.span;
                        self.err(Diag::new(sp, format!("returns a view of `{}`, which is dropped when the function returns", n)).note(Some(lsp), "declared here"));
                    }
                }
            }
            Err(tsp) => self.err(Diag::new(tsp, "returns a view of a temporary value")),
        }
    }

    fn loop_body(&mut self, st: &mut St, id: u32, mut body: impl FnMut(&mut Self, &mut St), infinite: bool, src: (Option<&TExpr>, &TBlock)) {
        let entry = st.clone();
        if self.suppress > 0 {
            // Inside an enclosing loop's discovery pass only the resulting state
            // matters, and one pass computes it: statements only add moved paths
            // (moves) or remove them (assignments), so an iteration that starts
            // with the moves of a previous one ends with nothing the first
            // iteration and its `continue`s didn't already produce. (Running
            // both passes here would cost 2^depth for nested loops.)
            self.loops.push(LoopCx { id, defer_level: self.defers.len(), breaks: vec![], conts: vec![] });
            let mut s1 = entry.clone();
            body(self, &mut s1);
            let lc = self.loops.pop().unwrap();
            let mut exit = if infinite { St::unreachable() } else { entry };
            exit.join(&s1);
            for c in lc.conts.iter().chain(&lc.breaks) {
                exit.join(c);
            }
            *st = exit;
            return;
        }
        // first pass discovers moves that happen inside the loop
        self.suppress += 1;
        self.loops.push(LoopCx { id, defer_level: self.defers.len(), breaks: vec![], conts: vec![] });
        let mut s1 = entry.clone();
        let saved_loans = self.loans.clone();
        let saved_holders = self.holders.clone();
        let outside: std::collections::HashSet<LocalId> = self.decl_depth.keys().copied().collect();
        body(self, &mut s1);
        let lc = self.loops.pop().unwrap();
        // ... and views assigned in it to variables declared outside: they
        // still point where they did when the next iteration starts
        let key = |l: &Loan| (l.target.clone(), l.holder, l.span);
        let before: std::collections::HashSet<_> = saved_loans.iter().map(key).collect();
        // ... and what tasks started in it borrow: they run on while the next
        // iteration starts (another task of the loop, or the loop itself)
        let outer_depth = self.depth;
        let carried: Vec<Loan> = self
            .loans
            .iter()
            .filter(|l| (l.holder.is_some_and(|h| outside.contains(&h) && !overwritten_first(h, src.0, src.1)) || (l.holder.is_none() && !l.returned && l.depth <= outer_depth)) && !before.contains(&key(l)))
            // (a range's bounds may differ in the next iteration)
            .map(|l| Loan { carried: true, range: None, ..l.clone() })
            .collect();
        let carried_holders: Vec<(LocalId, Vec<(Path, bool)>)> = carried.iter().filter_map(|l| l.holder).filter_map(|h| self.holders.get(&h).map(|t| (h, t.clone()))).collect();
        self.loans = saved_loans;
        self.holders = saved_holders;
        self.loans.extend(carried);
        for (h, ts) in carried_holders {
            let e = self.holders.entry(h).or_default();
            for t in ts {
                if !e.contains(&t) {
                    e.push(t);
                }
            }
        }
        self.suppress -= 1;
        let mut head = entry.clone();
        head.join(&s1);
        for c in &lc.conts {
            head.join(c);
        }
        // second pass reports errors with moves from previous iterations
        self.loops.push(LoopCx { id, defer_level: self.defers.len(), breaks: vec![], conts: vec![] });
        let mut s2 = head.clone();
        body(self, &mut s2);
        let lc = self.loops.pop().unwrap();
        // after the loop, the loans this pass took at the same assignments
        // stand for the views that are left
        self.loans.retain(|l| !l.carried);
        let mut exit = if infinite { St::unreachable() } else { head };
        exit.join(&s2);
        for b in &lc.breaks {
            exit.join(b);
        }
        *st = exit;
    }

    // ------------------------------------------------------------------
    // Expressions
    // ------------------------------------------------------------------

    fn expr(&mut self, st: &mut St, e: &TExpr) {
        if self.as_move == Some(e as *const TExpr as usize) {
            if let TK::Intrinsic { args, .. } = &e.kind {
                self.as_move_seen = true;
                return self.do_move(st, &args[0].e);
            }
        }
        match &e.kind {
            TK::Move(x) => self.do_move(st, x),
            TK::Copy(x) => self.read(st, x),
            TK::Local(_) | TK::Field { .. } | TK::Deref(_) | TK::Index { .. } | TK::MapIndex { .. } | TK::ProjUnwrap { .. } => {
                if e.is_place() {
                    self.read(st, e)
                } else if let TK::Field { base, .. } = &e.kind {
                    self.expr(st, base)
                }
            }
            TK::Proj { call, .. } => self.call_args(st, call),
            TK::Call { .. } | TK::Intrinsic { .. } => {
                self.call_args(st, e);
                self.widen_inout_views(st, e);
            }
            TK::StrIndex { base, index } => {
                self.expr(st, base);
                self.expr(st, index);
            }
            TK::SliceOf { base, lo, hi } => {
                self.expr(st, base);
                if let Some(l) = lo {
                    self.expr(st, l)
                }
                if let Some(h) = hi {
                    self.expr(st, h)
                }
            }
            TK::Unary { e: x, .. } | TK::Cast(x) | TK::SomeLit(x) | TK::OkLit(x) | TK::ErrLit(x) | TK::Coerce(_, x) | TK::Recv(x) | TK::Wrap(x) | TK::MakeChan(x) | TK::Unbox(x) => self.expr(st, x),
            TK::MutexRef(p) => self.write(st, p),
            TK::OptField { base, .. } => self.expr(st, base),
            TK::Binary { op, l, r } => {
                self.expr(st, l);
                if matches!(op, BinOp::And | BinOp::Or) {
                    let mut s2 = st.clone();
                    self.expr(&mut s2, r);
                    st.join(&s2);
                } else {
                    self.expr(st, r);
                }
            }
            TK::Coalesce { l, r } => {
                self.expr(st, l);
                let mut s2 = st.clone();
                self.expr(&mut s2, r);
                st.join(&s2);
            }
            TK::StructLit { fields, .. } => fields.iter().for_each(|f| self.expr(st, f)),
            TK::EnumLit { args, .. } => args.iter().for_each(|f| self.expr(st, f)),
            TK::VecLit(xs) | TK::ArrayLit(xs) => xs.iter().for_each(|x| self.expr(st, x)),
            TK::MapLit(ps) => ps.iter().for_each(|(k, v)| {
                self.expr(st, k);
                self.expr(st, v)
            }),
            TK::Closure(c) => self.closure(st, c),
            TK::If { cond, then, else_ } => {
                self.expr(st, cond);
                let mut a = st.clone();
                self.block(&mut a, then);
                let mut b = st.clone();
                if let Some(eb) = else_ {
                    self.block(&mut b, eb);
                }
                a.join(&b);
                *st = a;
            }
            TK::Match { scrut, owned, arms } => {
                if !matches!(scrut.kind, TK::Proj { .. }) && scrut.is_place() {
                    self.read(st, scrut);
                } else {
                    self.expr(st, scrut);
                }
                let start = st.clone();
                let mut out = St::unreachable();
                for a in arms {
                    let mut s2 = start.clone();
                    for b in &a.binds {
                        self.reinit(&mut s2, &(*b, vec![]));
                    }
                    self.depth += 1;
                    if a.consuming && *owned && scrut.is_place() {
                        self.do_move(&mut s2, scrut);
                    } else if !a.binds.is_empty() {
                        let refs: Vec<LocalId> = a.binds.iter().copied().filter(|b| matches!(self.tf.locals[*b as usize].kind, LocalKind::Ref { .. })).collect();
                        if !refs.is_empty() {
                            let targets = if matches!(scrut.kind, TK::Proj { .. }) {
                                // what the projection's yields point into; for
                                // built-in ones, the receiver
                                self.proj_targets(scrut).unwrap_or_else(|| match &scrut.kind {
                                    TK::Proj { call, .. } => match &call.kind {
                                        TK::Call { args, .. } => args.first().map(|x| self.targets(&x.e)).unwrap_or_default(),
                                        _ => vec![],
                                    },
                                    _ => vec![],
                                })
                            } else {
                                self.targets(scrut)
                            };
                            let mutable = refs.iter().any(|b| matches!(self.tf.locals[*b as usize].kind, LocalKind::Ref { mutable: true }));
                            for (t, _) in &targets {
                                self.loans.push(Loan { target: t.clone(), mutable, holder: Some(refs[0]), span: scrut.span, depth: self.depth, returned: false, carried: false, range: None, task: false });
                            }
                            // the bindings point into the view variable itself
                            if self.prog.is_view(&scrut.ty) {
                                if let Some(p) = place_path(scrut) {
                                    self.storage.push((p.clone(), scrut.span, self.depth, "match"));
                                    for &r in &refs {
                                        self.bind_into.insert(r, p.clone());
                                    }
                                }
                            }
                            // Bindings into a temporary (`if let x = w.upgrade()`) point
                            // into something that lives only for this `match`: they own
                            // what they point into, as far as views of them go.
                            let temp = targets.is_empty() && !scrut.is_place() && !matches!(scrut.kind, TK::Proj { .. });
                            // a binding of inline data (no view in it) inside a
                            // variable holding a view (`err(a)` of `str ! [4]int`)
                            // points into that variable, not into what it views
                            let own = if self.prog.is_view(&scrut.ty) { place_path(scrut) } else { None };
                            for r in refs {
                                self.decl_depth.insert(r, self.depth);
                                let ts: Vec<(Path, bool)> = match &own {
                                    Some(p) if !self.prog.contains_view(&self.tf.locals[r as usize].ty) => vec![(p.clone(), mutable)],
                                    _ if temp => vec![((r, vec![]), mutable)],
                                    _ => targets.iter().map(|(p, _)| (p.clone(), mutable)).collect(),
                                };
                                self.holders.insert(r, ts);
                            }
                        }
                    }
                    if let Some(g) = &a.guard {
                        self.expr(&mut s2, g);
                    }
                    self.expr(&mut s2, &a.body);
                    self.end_depth();
                    self.depth -= 1;
                    out.join(&s2);
                }
                *st = out;
            }
            TK::Block(b) | TK::Unsafe(b) => self.block(st, b),
            TK::Isolate(b) => self.block(st, b),
            TK::Scope { id, body, anchor, .. } => {
                // the anchor lives in the body: handles can't leave it
                self.decl_depth.insert(*anchor, self.depth + 1);
                self.tscopes.push((*id, self.depth + 1, *anchor));
                self.block(st, body);
                self.tscopes.pop();
            }
            TK::Spawn { target, task, handle } => {
                if let SpawnTarget::Value(v) = target {
                    self.expr(st, v);
                }
                let d = match target {
                    SpawnTarget::Lexical(id) => self.tscopes.iter().rev().find(|t| t.0 == *id).map(|t| t.1),
                    SpawnTarget::Value(_) => None,
                };
                // A plain `go` task's error becomes its scope's, used after the
                // scope's body is gone: a view in it can't point into
                // anything the task got from that body.
                if let (TK::Closure(c), false, Some(d)) = (&task.kind, *handle, d) {
                    if matches!(&c.ret, Ty::Result(_, e) if self.prog.contains_view(e)) {
                        for (l, _) in &c.captures {
                            if self.decl_depth.get(l).is_some_and(|&ld| ld >= d) {
                                let (n, lsp) = (self.tf.locals[*l as usize].name.clone(), self.tf.locals[*l as usize].span);
                                let es = match &c.ret {
                                    Ty::Result(_, e) => self.prog.show(e),
                                    _ => String::new(),
                                };
                                self.err(Diag::new(task.span, format!("this task's error (`{}`) may view `{}`, which is gone when the scope gives the error back", es, n)).note(Some(lsp), "declared inside the scope").note(None, "fail with an owned value (`String(s)`), or declare it before the scope"));
                            }
                        }
                    }
                }
                if let TK::Closure(c) = &task.kind {
                    let saved = self.spawn_depth.replace(d.unwrap_or(self.depth));
                    self.closure(st, c);
                    self.spawn_depth = saved;
                } else {
                    self.expr(st, task);
                }
            }
            TK::Try { e: x, wrap } => {
                self.expr(st, x);
                // the error (or what `wrap` makes of it) is returned: when it
                // may hold a view, it is checked and recorded like any
                // returned view
                let fd_ret = self.prog.func(self.tf.func).ret.clone();
                let err_view = self.view_ret && matches!(&fd_ret, Ty::Result(_, e) if self.prog.contains_view(e));
                let mut returned: Option<&TExpr> = None;
                if let Some(TryWrap::Call(h, w)) = wrap {
                    let mut s2 = st.clone();
                    if err_view {
                        // the error bound for `wrap` views what `x` does
                        if let Ok(srcs) = self.view_sources(x) {
                            self.holders.insert(*h, srcs.into_iter().map(|(p, _)| (p, false)).collect());
                        }
                    }
                    self.expr(&mut s2, w);
                    returned = Some(w);
                } else if matches!(&x.ty, Ty::Result(_, e) if self.prog.contains_view(e)) {
                    returned = Some(x);
                }
                let mut kept = 0;
                if let (true, Some(r)) = (err_view, returned) {
                    self.check_view_return(r, r.span);
                    self.note_returned(r);
                    if let Ok(srcs) = self.view_sources(r) {
                        for (t, _) in srcs {
                            self.loans.push(Loan { target: t, mutable: false, holder: None, span: r.span, depth: 0, returned: true, carried: false, range: None, task: false });
                            kept += 1;
                        }
                    }
                }
                // the error path leaves the function: `defer`s and `errdefer`s run
                self.end_scope_defers_all(st, true);
                let n = self.loans.len();
                self.loans.truncate(n.saturating_sub(kept));
            }
            TK::Catch { e: x, handler, bind } => {
                self.expr(st, x);
                let mut s2 = st.clone();
                if let Some(b) = bind {
                    self.reinit(&mut s2, &(*b, vec![]));
                }
                self.block(&mut s2, handler);
                st.join(&s2);
            }
            _ => {}
        }
    }

    fn closure(&mut self, st: &mut St, c: &TClosure) {
        // captures at the creation site
        for (l, u) in &c.captures {
            let le = TExpr::new(TK::Local(*l), self.tf.locals[*l as usize].ty.clone(), c.span);
            let t = self.tf.locals[*l as usize].ty.clone();
            let taken = c.owned || (c.scoped && c.by_value.contains(l));
            if taken && !self.is_copy(&t) {
                self.do_move(st, &le);
                continue;
            }
            // a task borrows only the parts of a variable it uses: `ch.rx`
            // leaves `ch.tx` free to move
            if c.scoped && !taken && !self.is_holder(*l) {
                // a range's bounds must keep their value while the task runs:
                // immutable locals it captures
                let fixed = |x: LocalId| c.captures.iter().any(|(y, _)| *y == x) && self.immutable_local(x);
                let rng = |lo: Option<&TExpr>, hi: Option<&TExpr>| self.sym_range(lo, hi, &fixed);
                if let Some(paths) = capture_paths(c, *l, &rng) {
                    let d = self.spawn_depth.unwrap_or(1);
                    let acc = if *u == CapUse::Mutate { Acc::Mut } else { Acc::Read };
                    for (p, range) in paths {
                        let path = (*l, p);
                        self.check_moved(st, &path, c.span, if acc == Acc::Mut { "modification of" } else { "use of" });
                        self.check_loans_r(&path, range.as_ref(), acc, c.span, None);
                        self.loans.push(Loan { target: path, mutable: acc == Acc::Mut, holder: None, span: c.span, depth: d, returned: false, carried: false, range, task: true });
                    }
                    continue;
                }
            }
            if *u == CapUse::Mutate {
                self.write(st, &le);
            } else {
                self.read(st, &le);
            }
            if c.scoped && (!c.by_value.contains(l) || self.prog.is_view(&t)) {
                // tasks keep their borrows (and copies of views keep theirs)
                // until the scope ends
                let d = self.spawn_depth.unwrap_or(1);
                // a view variable the task reads in place can't be given
                // another value meanwhile (its loans are on what it views)
                if !c.by_value.contains(l) && self.prog.is_view(&t) {
                    self.storage.push(((*l, vec![]), c.span, d, "task"));
                }
                // (a view that may write, like `Mutex(&x)`, keeps writing)
                let held: Vec<(Path, bool)> = place_path(&le).and_then(|p| self.holders.get(&p.0).cloned()).unwrap_or_default();
                for (p, m) in self.arg_targets(&le) {
                    let via_mut = m || held.iter().any(|(h, hm)| *hm && overlap(h, &p));
                    self.loans.push(Loan { target: p, mutable: *u == CapUse::Mutate || via_mut, holder: None, span: c.span, depth: d, returned: false, carried: false, range: None, task: true });
                }
            }
        }
        // the body is checked on its own
        // (`return` in a loop body run through `each` returns from this function)
        let mut inner = Bc { prog: self.prog, tf: self.tf, proj: self.proj, diags: vec![], loans: vec![], holders: self.holders.clone(), depth: 0, suppress: self.suppress, loops: vec![], view_ret: self.view_ret && c.loop_body.is_some(), defers: vec![vec![]], decl_depth: c.params.iter().map(|p| (*p, 0)).collect(), escaped: vec![], locked: self.locked.iter().map(|(_, ps, sp, n)| (0, ps.clone(), *sp, n.clone())).collect(), in_closure: true, in_each_body: c.loop_body.is_some(), via_parents: self.via_parents.clone(), tscopes: vec![], spawn_depth: None, as_move: None, as_move_seen: false, rsrc: self.rsrc, ret_params: vec![], src_unknown: Cell::new(false), storage: self.storage.iter().map(|(p, sp, _, w)| (p.clone(), *sp, 0, *w)).collect(), bind_into: self.bind_into.clone(), why_borrow: HashMap::new() };
        let mut s = St::new();
        inner.block(&mut s, &c.body);
        inner.end_scope_defers(&mut s);
        for i in inner.ret_params {
            if !self.ret_params.contains(&i) {
                self.ret_params.push(i);
            }
        }
        for d in inner.diags {
            self.err(d);
        }
    }

    /// A call that can't be seen into (a generic `inout T`, like `swap` and
    /// `replace`, or a call through a function value or interface) may store
    /// in an `inout` argument holding a view any view the call is given: the
    /// argument's variable now views those too.
    fn widen_inout_views(&mut self, st: &mut St, e: &TExpr) {
        let TK::Call { callee, args } = &e.kind else { return };
        let generic: Vec<bool> = match callee {
            Callee::Func(f, _) => {
                let fd = self.prog.func(*f);
                fd.recv.iter().chain(fd.params.iter()).map(|p| p.ty.has_params()).collect()
            }
            _ => vec![true; args.len()],
        };
        for (i, a) in args.iter().enumerate() {
            if a.mode != ArgMode::Inout || !generic.get(i).copied().unwrap_or(true) || !self.prog.contains_view(&a.e.ty) {
                continue;
            }
            let Some(root) = place_path(&a.e) else { continue };
            let mut srcs: Vec<(Path, bool)> = vec![];
            for (j, b) in args.iter().enumerate() {
                if j == i || !self.prog.contains_view(&b.e.ty) {
                    continue;
                }
                match self.view_sources(&b.e) {
                    Ok(s) => srcs.extend(s),
                    Err(tsp) => {
                        self.err(Diag::new(tsp, "this temporary value is dropped at the end of the statement, but a view of it may be kept").note(None, "bind the value to a variable first: `let s = ...` and borrow from that"));
                        return;
                    }
                }
            }
            if srcs.is_empty() {
                continue;
            }
            self.check_storage(&root, a.e.span);
            let v = root.0;
            let l = &self.tf.locals[v as usize];
            if matches!(l.kind, LocalKind::Param(Mode::Inout)) {
                if let Some((p, _)) = srcs.iter().find(|(p, _)| p.0 != v) {
                    let (o, name) = (self.name(p), l.name.clone());
                    self.err(Diag::new(a.e.span, format!("cannot store a view of `{}` in the `inout` parameter `{}`", o, name)).note(None, "the caller couldn't see that its variable now borrows this; return the view instead, or use an owned type"));
                }
                continue;
            }
            let depth = self.decl_depth.get(&v).copied().unwrap_or(0);
            self.bind_view(v, depth, srcs, false, a.e.span, st, &a.e);
        }
    }

    /// Arguments of a call, with exclusivity checking.
    fn call_args(&mut self, st: &mut St, e: &TExpr) {
        let args = match &e.kind {
            TK::Call { args, .. } | TK::Intrinsic { args, .. } => args,
            TK::Proj { call, .. } => return self.call_args(st, call),
            _ => return self.expr(st, e),
        };
        // (path, access, span, comes from a push-style loop body, range)
        let mut accesses: Vec<(Path, Acc, Span, bool, Option<SymRange>)> = vec![];
        for a in args {
            match a.mode {
                // `&v[a..b]`: only those elements
                ArgMode::Inout if matches!(&a.e.kind, TK::SliceOf { base, .. } if base.is_place()) => {
                    let TK::SliceOf { base, lo, hi } = &a.e.kind else { unreachable!() };
                    let range = self.sym_range(lo.as_deref(), hi.as_deref(), &|l| self.immutable_local(l));
                    self.subexprs_of_place(st, base);
                    for x in [lo, hi].into_iter().flatten() {
                        self.expr(st, x);
                    }
                    if let Some(p) = place_path(base) {
                        if !self.is_holder(p.0) {
                            self.check_moved(st, &p, base.span, "modification of");
                            self.check_loans_r(&p, range.as_ref(), Acc::Mut, base.span, None);
                        }
                    }
                    for (p, _) in self.arg_targets(base) {
                        accesses.push((p, Acc::Mut, a.e.span, false, range.clone()));
                    }
                }
                ArgMode::Inout => {
                    self.write(st, &a.e);
                    for (p, _) in self.arg_targets(&a.e) {
                        accesses.push((p, Acc::Mut, a.e.span, false, None));
                    }
                }
                ArgMode::Borrow => {
                    if let TK::Closure(c) = &a.e.kind {
                        self.closure(st, c);
                        if !c.owned {
                            // values copied into a task's environment aren't borrowed
                            for (l, u) in c.captures.iter().filter(|(l, _)| !c.by_value.contains(l)) {
                                let le = TExpr::new(TK::Local(*l), self.tf.locals[*l as usize].ty.clone(), c.span);
                                for (p, _) in self.arg_targets(&le) {
                                    accesses.push((p, if *u == CapUse::Mutate { Acc::Mut } else { Acc::Read }, c.span, c.loop_body.is_some(), None));
                                }
                            }
                        }
                        continue;
                    }
                    if a.e.is_place() {
                        self.read(st, &a.e);
                    } else {
                        self.expr(st, &a.e);
                    }
                    if !self.is_copy(&a.e.ty) || self.prog.is_view(&a.e.ty) {
                        for (p, _) in self.arg_targets(&a.e) {
                            accesses.push((p, Acc::Read, a.e.span, false, None));
                        }
                    }
                }
                ArgMode::Value => {
                    self.expr(st, &a.e);
                    if let TK::Move(x) = &a.e.kind {
                        if let Some(p) = place_path(x) {
                            if !self.is_holder(p.0) {
                                accesses.push((p, Acc::Mut, a.e.span, false, None));
                            }
                        }
                    } else if self.prog.is_view(&a.e.ty) {
                        if let Ok(srcs) = self.view_sources(&a.e) {
                            for (p, _) in srcs {
                                accesses.push((p, Acc::Read, a.e.span, false, None));
                            }
                        }
                    }
                }
            }
        }
        for i in 0..accesses.len() {
            for j in 0..i {
                let (a, b) = (&accesses[i], &accesses[j]);
                if overlap(&a.0, &b.0) && ranges_meet(&a.0, a.4.as_ref(), &b.0, b.4.as_ref()) && (a.1 == Acc::Mut || b.1 == Acc::Mut) {
                    let n = self.name(&a.0);
                    let (sp, other) = (a.2, b.2);
                    if a.3 || b.3 {
                        let (body, it) = if a.3 { (a, b) } else { (b, a) };
                        let msg = if body.1 == Acc::Mut { format!("cannot modify `{}` while iterating over it", n) } else { format!("cannot use `{}` while iterating over it mutably", n) };
                        self.err(Diag::new(it.2, msg).note(Some(body.2), "the loop body uses it here"));
                    } else {
                        self.err(Diag::new(sp, format!("`{}` is used by two arguments of the same call, and one of them may modify it", n)).note(Some(other), "the other use is here"));
                    }
                }
            }
        }
    }

    fn arg_targets(&self, e: &TExpr) -> Vec<(Path, bool)> {
        if self.prog.is_view(&e.ty) && !e.is_place() {
            return self.view_sources(e).unwrap_or_default();
        }
        if let TK::Coerce(Coercion::ToIface { .. }, x) = &e.kind {
            return self.arg_targets(x);
        }
        let ts = self.targets(e);
        if !ts.is_empty() {
            return ts;
        }
        // a view parameter / local passed along
        if let Some(p) = place_path(e) {
            return self.holders.get(&p.0).cloned().unwrap_or_default();
        }
        vec![]
    }
}

/// The parts of captured local `l` that closure `c` uses: field paths, or the
/// whole variable (an empty path). None when the variable isn't used through
/// a place at all (then the caller treats it as a whole).
fn capture_paths(c: &TClosure, l: LocalId, range: &dyn Fn(Option<&TExpr>, Option<&TExpr>) -> Option<SymRange>) -> Option<Vec<(Vec<usize>, Option<SymRange>)>> {
    use std::collections::HashSet;
    // the child a place goes through: the inner node is part of the outer place
    fn inner(e: &TExpr) -> Option<&TExpr> {
        match &e.kind {
            TK::Field { base, .. } | TK::Deref(base) | TK::Index { base, .. } | TK::MapIndex { base, .. } | TK::Copy(base) | TK::Move(base) => Some(base),
            TK::Coerce(Coercion::VecToSlice | Coercion::ArrayToSlice | Coercion::StringToStr, x) => Some(x),
            TK::ProjUnwrap { proj, .. } => Some(proj),
            _ => None,
        }
    }
    let mut found: Vec<(*const TExpr, Vec<usize>)> = vec![];
    let mut nested: HashSet<*const TExpr> = HashSet::new();
    // places used only through a range `v[a..b]`, with the range
    let mut ranged: HashMap<*const TExpr, SymRange> = HashMap::new();
    visit_block(&c.body, &mut |e| {
        if let TK::SliceOf { base, lo, hi } = &e.kind {
            if let Some(r) = range(lo.as_deref(), hi.as_deref()) {
                ranged.insert(&**base as *const TExpr, r);
            }
        }
        if let Some((r, p)) = place_path(e) {
            if r == l {
                found.push((e as *const TExpr, p));
                if let Some(i) = inner(e) {
                    nested.insert(i as *const TExpr);
                }
            }
        }
    });
    let mut out: Vec<(Vec<usize>, Option<SymRange>)> = vec![];
    for (ptr, p) in found {
        if nested.contains(&ptr) {
            continue;
        }
        let r = ranged.get(&ptr).cloned();
        if p.is_empty() && r.is_none() {
            return Some(vec![(vec![], None)]);
        }
        let x = (p, r);
        if !out.contains(&x) {
            out.push(x);
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// A returned value that makes the function end with an error (or `none`).
fn is_error_value(e: &TExpr) -> bool {
    match &e.kind {
        TK::ErrLit(_) | TK::NoneLit => true,
        TK::Coerce(_, x) | TK::Wrap(x) => is_error_value(x),
        _ => false,
    }
}

/// Whether each iteration sets `h` before anything could read the value it had
/// when the iteration started, or leave the loop with it: then the view it held
/// at the end of one iteration is dead in the next.
fn overwritten_first(h: LocalId, cond: Option<&TExpr>, body: &TBlock) -> bool {
    use crate::tir::Node;
    let mentions = |n: Node| -> bool {
        let mut found = false;
        let mut f = |x: Node| match x {
            Node::Expr(e) => match &e.kind {
                TK::Local(l) => found |= *l == h,
                TK::Closure(c) => found |= c.captures.iter().any(|c| c.0 == h),
                // leaving the loop or the function from inside the expression
                TK::Try { .. } => found = true,
                _ => {}
            },
            Node::Stmt(TStmt::Break(..) | TStmt::Continue(..) | TStmt::Return(..) | TStmt::Yield(..)) => found = true,
            Node::Stmt(_) => {}
        };
        match n {
            Node::Expr(e) => crate::tir::walk_expr(e, &mut f),
            Node::Stmt(s) => crate::tir::walk_stmt(s, &mut f),
        }
        found
    };
    if cond.is_some_and(|c| mentions(Node::Expr(c))) {
        return false;
    }
    for s in &body.stmts {
        if let TStmt::Assign { place, op: None, value, .. } = s {
            if matches!(place.kind, TK::Local(l) if l == h) {
                return !mentions(Node::Expr(value));
            }
        }
        // a read, a closure that could read it, or a way out first
        if mentions(Node::Stmt(s)) {
            return false;
        }
    }
    false
}

fn place_is_direct(e: &TExpr) -> bool {
    match &e.kind {
        TK::Local(_) => true,
        TK::Field { base, .. } => place_is_direct(base),
        _ => false,
    }
}
