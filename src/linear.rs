// Linear types (spec 5.7): values that must be consumed and are never dropped
// implicitly.
//
// A value of a `linear` type (or of a type that owns one, or of a type
// parameter declared `any`) has to end in one of a few ways: moved into a
// `sink` parameter, returned, stored in something that is itself consumed,
// or taken apart in its own package. This pass rejects every place where
// it would otherwise be dropped:
//
// - a variable or `sink` parameter still holding one where its block ends,
//   at a `return`, `break` or `continue` that leaves it, or where a `try`
//   returns an error;
// - a temporary nobody takes (`begin()` as a statement, `begin().id`);
// - assigning over one, which drops the old value;
// - a type that would drop one: `Rc[Tx]`, `Map[K, Tx]`, a box or array of
//   them, an interface value holding one, a generic type or function whose
//   type parameter isn't declared `any`, a closure capturing one.
//
// The analysis is per function, like the borrow checker: a forward dataflow
// over the paths that hold a value still to be consumed. Panics abort, so no
// path is unwound and every exit is one of the above.
//
// A `defer` or `errdefer` whose body consumes a linear variable is guarded
// by it: it runs only where the variable still holds its value (codegen tests
// the variable's drop flag). So `defer tx.rollback()` rolls back on every exit
// before `tx.commit()`, and does nothing after it.

use crate::diag::{Diag, Span};
use crate::tir::*;
use crate::types::*;
use crate::ast::Mode;
use std::collections::{HashMap, HashSet};

type Path = (LocalId, Vec<usize>);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum S {
    Live,
    /// Live on some paths only.
    Maybe,
}

#[derive(Clone, PartialEq, Debug)]
struct St {
    reachable: bool,
    /// The paths holding a value still to be consumed.
    m: HashMap<Path, S>,
}

impl St {
    fn unreachable() -> St {
        St { reachable: false, m: HashMap::new() }
    }
    fn join(&mut self, o: &St) {
        if !o.reachable {
            return;
        }
        if !self.reachable {
            *self = o.clone();
            return;
        }
        let keys: HashSet<Path> = self.m.keys().chain(o.m.keys()).cloned().collect();
        for k in keys {
            let v = match (self.m.get(&k), o.m.get(&k)) {
                (Some(S::Live), Some(S::Live)) => S::Live,
                _ => S::Maybe,
            };
            self.m.insert(k, v);
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Ctx {
    /// The value is taken: moved into a variable, a `sink` parameter, a
    /// literal, the result...
    Consumed,
    /// Only looked at; a temporary is dropped afterwards.
    Borrowed,
    /// Thrown away (an expression statement, `_ = e`).
    Discarded,
}

#[derive(Default)]
struct Scope<'a> {
    declared: Vec<LocalId>,
    defers: Vec<(&'a TBlock, bool)>,
}

struct LoopCx {
    id: u32,
    /// `scopes.len()` outside the loop body.
    level: usize,
    breaks: Vec<St>,
    conts: Vec<St>,
}

pub fn check(prog: &Program, tfuncs: &[TFunc]) -> Vec<Diag> {
    let mut diags = vec![];
    let mut seen = HashSet::new();
    for (i, g) in prog.globals.iter().enumerate() {
        let _ = i;
        if prog.is_linear(&g.ty) {
            diags.push(Diag::new(g.span, format!("package-level `{}` holds the linear `{}`, which nothing could ever consume", g.name, prog.show(&g.ty))));
        }
    }
    for tf in tfuncs {
        let mut l = Lin { prog, tf, diags: vec![], seen: &mut seen, scopes: vec![], loops: vec![], suppress: 0, exempt: None };
        l.wf_func();
        l.func();
        diags.extend(l.diags);
    }
    diags
}

/// The linear locals a `defer` body consumes that it doesn't declare itself:
/// the defer runs only while they hold their value (at most one is allowed).
pub fn defer_guards(prog: &Program, tf: &TFunc, b: &TBlock) -> Vec<LocalId> {
    let mut declared = HashSet::new();
    walk_block(b, &mut |n| {
        if let Node::Stmt(TStmt::Let { local, .. } | TStmt::LetRef { local, .. }) = n {
            declared.insert(*local);
        }
    });
    let mut out = vec![];
    visit_block(b, &mut |e| {
        if let TK::Move(p) = &e.kind {
            if let Some((l, _)) = place_path(p) {
                if !declared.contains(&l) && prog.is_linear(&tf.locals[l as usize].ty) && !out.contains(&l) {
                    out.push(l);
                }
            }
        }
    });
    out
}

/// A place rooted at a local, through struct fields only.
fn place_path(e: &TExpr) -> Option<Path> {
    match &e.kind {
        TK::Local(l) => Some((*l, vec![])),
        TK::Field { base, index } => {
            let (l, mut p) = place_path(base)?;
            p.push(*index);
            Some((l, p))
        }
        _ => None,
    }
}

fn is_error_value(e: &TExpr) -> bool {
    match &e.kind {
        TK::ErrLit(_) | TK::NoneLit => true,
        TK::Coerce(_, x) | TK::Wrap(x) => is_error_value(x),
        _ => false,
    }
}

struct Lin<'a, 's> {
    prog: &'a Program,
    tf: &'a TFunc,
    diags: Vec<Diag>,
    seen: &'s mut HashSet<(Span, String)>,
    scopes: Vec<Scope<'a>>,
    loops: Vec<LoopCx>,
    /// Inside a loop being iterated to its fixpoint: report nothing.
    suppress: u32,
    /// The `sink self` of a linear enum's own method: it is taken apart
    /// here, so matching it on a variant with no linear payload ends it.
    exempt: Option<LocalId>,
}

impl<'a, 's> Lin<'a, 's> {
    fn err(&mut self, d: Diag) {
        if self.suppress > 0 {
            return;
        }
        if self.seen.insert((d.span, d.msg.clone())) {
            self.diags.push(d);
        }
    }

    fn lin(&self, t: &Ty) -> bool {
        self.prog.is_linear(t)
    }

    fn show(&self, t: &Ty) -> String {
        self.prog.show(t)
    }

    /// How values of `t` are consumed, for the notes of errors.
    fn how(&self, t: &Ty) -> String {
        let core = match t {
            Ty::Option(a) | Ty::Vec(a) => (**a).clone(),
            t => t.clone(),
        };
        if let Ty::Adt(d, _) = &core {
            let mut ms: Vec<FuncId> = self.prog.methods.iter().filter(|((k, _), _)| *k == TyKey::Adt(*d)).flat_map(|(_, fs)| fs.iter().copied()).filter(|&f| self.prog.func(f).recv.as_ref().map(|r| r.mode == Mode::Sink).unwrap_or(false)).collect();
            ms.sort();
            ms.dedup();
            let names: Vec<String> = ms.iter().map(|&f| format!("`{}`", self.prog.func(f).name)).collect();
            if !names.is_empty() && self.prog.def(*d).linear {
                let which = match names.len() {
                    1 => names[0].clone(),
                    n => format!("{} or {}", names[..n - 1].join(", "), names[n - 1]),
                };
                let lead = match t {
                    Ty::Option(_) => "unwrap it (`if let`, `match`) and call ",
                    Ty::Vec(_) => "pop each element and call ",
                    _ => "call ",
                };
                return format!("`{}` is linear: {}{}", self.prog.def(*d).name, lead, which);
            }
        }
        match t {
            Ty::Vec(_) => format!("`{}` holds linear values: `pop` each one and consume it, then end the empty vector with `finish()`", self.show(t)),
            _ => format!("`{}` is linear: move it into a `sink` parameter, return it, or store it in something that is consumed", self.show(t)),
        }
    }

    fn path_name(&self, p: &Path) -> String {
        let mut s = self.tf.locals[p.0 as usize].name.clone();
        let mut t = self.tf.locals[p.0 as usize].ty.clone();
        for &i in &p.1 {
            if let Ty::Adt(d, args) = &t {
                let fs = self.prog.field_tys(*d, args);
                if let Some((n, ft)) = fs.get(i) {
                    if self.prog.def(*d).tuple {
                        s = format!("{}.{}", s, i);
                    } else {
                        s = format!("{}.{}", s, n);
                    }
                    t = ft.clone();
                    continue;
                }
            }
            s = format!("{}.{}", s, i);
        }
        s
    }

    fn path_ty(&self, p: &Path) -> Ty {
        let mut t = self.tf.locals[p.0 as usize].ty.clone();
        for &i in &p.1 {
            t = match &t {
                Ty::Adt(d, args) => self.prog.field_tys(*d, args).get(i).map(|f| f.1.clone()).unwrap_or(Ty::Error),
                _ => Ty::Error,
            };
        }
        t
    }

    // ------------------------------------------------------------------
    // Types that would drop a linear value
    // ------------------------------------------------------------------

    fn wf_func(&mut self) {
        let tf = self.tf;
        for l in &tf.locals {
            self.wf_ty(&l.ty, l.span);
        }
        let mut exprs: Vec<&'a TExpr> = vec![];
        fn collect<'b>(b: &'b TBlock, out: &mut Vec<&'b TExpr>) {
            // `visit_block` can't hand out references that outlive the call,
            // so walk by hand
            for s in &b.stmts {
                collect_stmt(s, out);
            }
            if let Some(v) = &b.value {
                collect_expr(v, out);
            }
        }
        fn collect_stmt<'b>(s: &'b TStmt, out: &mut Vec<&'b TExpr>) {
            match s {
                TStmt::Let { init: e, .. } | TStmt::LetRef { place: e, .. } | TStmt::Expr(e) => collect_expr(e, out),
                TStmt::Assign { place, value, .. } => {
                    collect_expr(place, out);
                    collect_expr(value, out)
                }
                TStmt::MapInsert { map, key, value } => {
                    collect_expr(map, out);
                    collect_expr(key, out);
                    collect_expr(value, out)
                }
                TStmt::Return(Some(e), _) | TStmt::Yield(Some(e), _) => collect_expr(e, out),
                TStmt::Defer(b) | TStmt::ErrDefer(b) => collect(b, out),
                TStmt::While { cond, body, .. } => {
                    if let Some(c) = cond {
                        collect_expr(c, out)
                    }
                    collect(body, out)
                }
                TStmt::For { iter, hi, body, .. } => {
                    collect_expr(iter, out);
                    if let Some(h) = hi {
                        collect_expr(h, out)
                    }
                    collect(body, out)
                }
                TStmt::ForEach { call, .. } => collect_expr(call, out),
                TStmt::WithLocks { places, body, .. } => {
                    for p in places {
                        collect_expr(p, out)
                    }
                    collect(body, out)
                }
                TStmt::ParFor { iter, hi, body, .. } => {
                    collect_expr(iter, out);
                    if let Some(h) = hi {
                        collect_expr(h, out)
                    }
                    collect_expr(body, out)
                }
                TStmt::Send { chan, value } => {
                    collect_expr(chan, out);
                    collect_expr(value, out)
                }
                TStmt::With { place, body, .. } => {
                    collect_expr(place, out);
                    collect(body, out)
                }
                TStmt::Select { arms, .. } => {
                    for a in arms {
                        match &a.kind {
                            TSelectKind::Recv { chan, .. } => collect_expr(chan, out),
                            TSelectKind::Send { chan, value } => {
                                collect_expr(chan, out);
                                collect_expr(value, out)
                            }
                            TSelectKind::Task { task, .. } => collect_expr(task, out),
                            TSelectKind::Timer { at, .. } => collect_expr(at, out),
                            TSelectKind::Cancelled | TSelectKind::Default => {}
                        }
                        if let Some(g) = &a.guard {
                            collect_expr(g, out)
                        }
                        collect(&a.body, out)
                    }
                }
                _ => {}
            }
        }
        fn collect_expr<'b>(e: &'b TExpr, out: &mut Vec<&'b TExpr>) {
            out.push(e);
            match &e.kind {
                TK::Field { base, .. } | TK::Deref(base) | TK::OptField { base, .. } => collect_expr(base, out),
                TK::Index { base, index } | TK::StrIndex { base, index } => {
                    collect_expr(base, out);
                    collect_expr(index, out)
                }
                TK::MapIndex { base, key } => {
                    collect_expr(base, out);
                    collect_expr(key, out)
                }
                TK::SliceOf { base, lo, hi } => {
                    collect_expr(base, out);
                    for x in [lo, hi].into_iter().flatten() {
                        collect_expr(x, out)
                    }
                }
                TK::Call { args, .. } | TK::Intrinsic { args, .. } => args.iter().for_each(|a| collect_expr(&a.e, out)),
                TK::Proj { call, .. } => collect_expr(call, out),
                TK::ProjUnwrap { proj, msg } => {
                    collect_expr(proj, out);
                    if let Some(m) = msg {
                        collect_expr(m, out)
                    }
                }
                TK::Unary { e: x, .. } | TK::Cast(x) | TK::SomeLit(x) | TK::OkLit(x) | TK::ErrLit(x) | TK::Move(x) | TK::Copy(x) | TK::Coerce(_, x) | TK::Recv(x) | TK::Wrap(x) | TK::MakeChan(x) | TK::Unbox(x) | TK::MutexRef(x) => collect_expr(x, out),
                TK::Binary { l, r, .. } | TK::Coalesce { l, r } => {
                    collect_expr(l, out);
                    collect_expr(r, out)
                }
                TK::StructLit { fields: xs, .. } | TK::EnumLit { args: xs, .. } | TK::VecLit(xs) | TK::ArrayLit(xs) => xs.iter().for_each(|x| collect_expr(x, out)),
                TK::MapLit(ps) => ps.iter().for_each(|(k, v)| {
                    collect_expr(k, out);
                    collect_expr(v, out)
                }),
                TK::Closure(c) => collect(&c.body, out),
                TK::If { cond, then, else_ } => {
                    collect_expr(cond, out);
                    collect(then, out);
                    if let Some(b) = else_ {
                        collect(b, out)
                    }
                }
                TK::Match { scrut, arms, .. } => {
                    collect_expr(scrut, out);
                    for a in arms {
                        if let Some(g) = &a.guard {
                            collect_expr(g, out)
                        }
                        collect_expr(&a.body, out)
                    }
                }
                TK::Block(b) | TK::Unsafe(b) | TK::Scope { body: b, .. } | TK::Isolate(b) => collect(b, out),
                TK::Spawn { target, task, .. } => {
                    if let SpawnTarget::Value(v) = target {
                        collect_expr(v, out)
                    }
                    collect_expr(task, out)
                }
                TK::Try { e: x, wrap } => {
                    collect_expr(x, out);
                    if let Some(TryWrap::Call(_, w)) = wrap {
                        collect_expr(w, out)
                    }
                }
                TK::Catch { e: x, handler, .. } => {
                    collect_expr(x, out);
                    collect(handler, out)
                }
                _ => {}
            }
        }
        collect(&tf.body, &mut exprs);
        for e in exprs {
            self.wf_ty(&e.ty, e.span);
            match &e.kind {
                TK::Call { callee: Callee::Func(f, targs), .. } | TK::FuncRef(f, targs) => self.wf_call(*f, targs, e.span),
                TK::StructLit { def, targs, .. } | TK::EnumLit { def, targs, .. } => self.wf_ty(&Ty::Adt(*def, std::rc::Rc::new(targs.clone())), e.span),
                TK::Coerce(Coercion::ToIface { owned: true }, x) if self.lin(&x.ty) => {
                    let d = Diag::new(e.span, format!("an interface value can't hold the linear `{}`: dropping the interface value would drop it", self.show(&x.ty))).note(None, "pass it where a borrowed interface is expected instead (a parameter), or consume it first");
                    self.err(d);
                }
                TK::Closure(c) if !c.loop_body.is_some() => {
                    for &(l, u) in &c.captures {
                        let lt = self.tf.locals[l as usize].ty.clone();
                        // borrowing it (a closure passed down, not kept) is fine
                        if self.lin(&lt) && (c.owned || u == CapUse::Move) && !self.spawned(c) {
                            let name = self.tf.locals[l as usize].name.clone();
                            let d = Diag::new(c.span, format!("a closure can't capture the linear `{}`: the closure could be dropped, or called twice", name)).note(Some(self.tf.locals[l as usize].span), format!("`{}` is declared here", name)).note(None, "pass it to a function as a `sink` argument instead (`go finish(tx)` hands it to a task)");
                            self.err(d);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Whether a closure is the body of a `go` (run exactly once).
    fn spawned(&self, c: &TClosure) -> bool {
        let mut found = false;
        let id = c.id;
        visit_block(&self.tf.body, &mut |e| {
            if let TK::Spawn { task, .. } = &e.kind {
                if let TK::Closure(t) = &task.kind {
                    if t.id == id {
                        found = true;
                    }
                }
            }
        });
        found
    }

    fn wf_call(&mut self, f: FuncId, targs: &[Ty], sp: Span) {
        let fd = self.prog.func(f);
        for (tp, ta) in fd.tparams.iter().zip(targs) {
            if !self.prog.tparam_any(*tp) && self.lin(ta) {
                let tn = self.prog.tparam(*tp).name.clone();
                let d = Diag::new(sp, format!("`{}` may drop its `{}` values, and `{}` is linear", fd.name, tn, self.show(ta))).note(Some(fd.span), format!("only a type parameter declared `any` (`[{}: any]` or `where {}: any`) takes linear types", tn, tn));
                self.err(d);
            }
        }
    }

    fn wf_ty(&mut self, t: &Ty, sp: Span) {
        let mut todo = vec![t.clone()];
        let mut seen = HashSet::new();
        while let Some(t) = todo.pop() {
            if !seen.insert(t.clone()) || seen.len() > 256 {
                continue;
            }
            match &t {
                Ty::Rc(a) | Ty::Arc(a) | Ty::Weak(a) | Ty::Cell(a) | Ty::Mutex(a) | Ty::Sender(a) | Ty::Receiver(a) | Ty::Task(a) | Ty::Iso(a) | Ty::Box(a) | Ty::Array(a, _) | Ty::ArrayT(a, _) => {
                    if self.lin(a) {
                        self.bad_holder(&t, a, sp);
                    }
                    todo.push((**a).clone());
                }
                Ty::Map(a, b) => {
                    for x in [a, b] {
                        if self.lin(x) {
                            self.bad_holder(&t, x, sp);
                        }
                        todo.push((**x).clone());
                    }
                }
                Ty::Vec(a) | Ty::Option(a) | Ty::Slice(a) | Ty::Ptr(a) | Ty::MutexRef(a) => todo.push((**a).clone()),
                Ty::Result(a, b) => {
                    todo.push((**a).clone());
                    todo.push((**b).clone());
                }
                Ty::Adt(d, args) | Ty::Iface(d, args) => {
                    let def = self.prog.def(*d);
                    if !def.tuple {
                        for (tp, a) in def.tparams.iter().zip(args.iter()) {
                            if !self.prog.tparam_any(*tp) && self.lin(a) {
                                let tn = self.prog.tparam(*tp).name.clone();
                                let d = Diag::new(sp, format!("`{}` may drop its `{}` values, and `{}` is linear", self.show(&t), tn, self.show(a))).note(Some(def.span), format!("only a type parameter declared `any` (`{}[{}: any]`) takes linear types", def.name, tn));
                                self.err(d);
                            }
                        }
                    }
                    todo.extend(args.iter().cloned());
                }
                _ => {}
            }
        }
    }

    fn bad_holder(&mut self, holder: &Ty, inner: &Ty, sp: Span) {
        let d = Diag::new(sp, format!("`{}` would drop the linear `{}` it holds", self.show(holder), self.show(inner))).note(None, "linear values can be held by structs and enums declared `linear`, `Vec`, `?T`, results and tuples, which are consumed in turn");
        self.err(d);
    }

    // ------------------------------------------------------------------
    // Consumption
    // ------------------------------------------------------------------

    fn func(&mut self) {
        let tf = self.tf;
        let fd = self.prog.func(tf.func);
        let mut st = St { reachable: true, m: HashMap::new() };
        let mut params = vec![];
        let mut decl = vec![];
        if let Some(r) = tf.recv {
            params.push(r);
        }
        params.extend(tf.params.iter().copied());
        for &p in &params {
            decl.push(p);
            if !matches!(tf.locals[p as usize].kind, LocalKind::Param(Mode::Sink)) || !self.lin(&tf.locals[p as usize].ty) {
                continue;
            }
            // a linear type's own `sink self` methods take the value apart:
            // what is left to consume are its linear fields
            if Some(p) == tf.recv {
                if let (Ty::Adt(d, args), Some(TyKey::Adt(k))) = (&tf.locals[p as usize].ty, fd.recv_key) {
                    if *d == k && self.prog.def(*d).linear {
                        match &self.prog.def(*d).kind {
                            TypeDefKind::Struct { .. } => {
                                for (i, (_, ft)) in self.prog.field_tys(*d, args).iter().enumerate() {
                                    if self.lin(ft) {
                                        st.m.insert((p, vec![i]), S::Live);
                                    }
                                }
                            }
                            _ => {
                                let payload = (0..self.prog.def(*d).variants().len()).any(|v| self.prog.variant_tys(*d, args, v).iter().any(|(_, t)| self.lin(t)));
                                if payload {
                                    st.m.insert((p, vec![]), S::Live);
                                    self.exempt = Some(p);
                                }
                            }
                        }
                        continue;
                    }
                }
            }
            st.m.insert((p, vec![]), S::Live);
        }
        self.scopes.push(Scope { declared: decl, defers: vec![] });
        let body: &'a TBlock = &tf.body;
        self.block(&mut st, body, Ctx::Consumed);
        if st.reachable {
            let end = Span { file: body.span.file, lo: body.span.hi.saturating_sub(1), hi: body.span.hi };
            self.exit(&mut st, 0, end, "where the function ends", true);
        }
        self.scopes.pop();
    }

    /// Reports the values still to be consumed of the locals declared in
    /// scopes `from..`, which an exit at `sp` leaves, and forgets them.
    fn exit(&mut self, st: &mut St, from: usize, sp: Span, wher: &str, at_decl: bool) {
        if !st.reachable {
            return;
        }
        let locals: Vec<LocalId> = self.scopes[from..].iter().flat_map(|s| s.declared.iter().copied()).collect();
        let mut left: Vec<(Path, S)> = st.m.iter().filter(|(p, _)| locals.contains(&p.0)).map(|(p, s)| (p.clone(), *s)).collect();
        left.sort_by(|a, b| a.0.cmp(&b.0));
        for (p, s) in left {
            st.m.remove(&p);
            let l = &self.tf.locals[p.0 as usize];
            let ty = self.path_ty(&p);
            let name = self.path_name(&p);
            let state = if s == S::Maybe { "may not be consumed on every path" } else { "is not consumed" };
            let msg = format!("`{}` {}: the linear `{}` would be dropped {}", name, state, self.show(&ty), wher);
            let d = if at_decl {
                Diag::new(l.span, msg).note(Some(sp), wher.to_string())
            } else {
                Diag::new(sp, msg).note(Some(l.span), format!("`{}` is declared here", l.name))
            };
            let d = d.note(None, self.how(&ty));
            self.err(d);
        }
    }

    /// Removes the obligations a move of `p` ends. Moving part of a value
    /// ends the whole, leaving its other linear parts to be consumed.
    fn consume(&mut self, st: &mut St, p: &Path) {
        let keys: Vec<Path> = st.m.keys().cloned().collect();
        for q in keys {
            if q.0 != p.0 {
                continue;
            }
            if q.1.len() >= p.1.len() && q.1[..p.1.len()] == p.1[..] {
                st.m.remove(&q);
            } else if p.1.len() > q.1.len() && p.1[..q.1.len()] == q.1[..] {
                // a part of `q` moves out: its sibling parts remain
                let s = st.m.remove(&q).unwrap();
                let mut cur = q.1.clone();
                for &i in &p.1[q.1.len()..] {
                    let t = self.path_ty(&(p.0, cur.clone()));
                    if let Ty::Adt(d, args) = &t {
                        for (j, (_, ft)) in self.prog.field_tys(*d, args).iter().enumerate() {
                            if j != i && self.lin(ft) {
                                let mut sib = cur.clone();
                                sib.push(j);
                                st.m.insert((p.0, sib), s);
                            }
                        }
                    }
                    cur.push(i);
                }
            }
        }
    }

    /// The value at `p` (of a linear type) is there again: assigned, or
    /// changed through `inout` (a vector that had no elements may have some
    /// now).
    fn refill(&mut self, st: &mut St, p: &Path) {
        // only what this function owns: an `inout` parameter or a reference
        // is its caller's to consume
        if !st.reachable || !matches!(self.tf.locals[p.0 as usize].kind, LocalKind::Let | LocalKind::Var | LocalKind::Param(Mode::Sink)) {
            return;
        }
        if st.m.keys().any(|q| q.0 == p.0 && (q.1.len() <= p.1.len() && p.1[..q.1.len()] == q.1[..])) {
            return;
        }
        let keys: Vec<Path> = st.m.keys().filter(|q| q.0 == p.0 && q.1.len() > p.1.len() && q.1[..p.1.len()] == p.1[..]).cloned().collect();
        for k in keys {
            st.m.remove(&k);
        }
        st.m.insert(p.clone(), S::Live);
    }

    fn declare(&mut self, l: LocalId) {
        if let Some(s) = self.scopes.last_mut() {
            s.declared.push(l);
        }
    }

    fn block(&mut self, st: &mut St, b: &'a TBlock, ctx: Ctx) {
        self.scopes.push(Scope::default());
        let level = self.scopes.len() - 1;
        for s in &b.stmts {
            self.stmt(st, s);
        }
        if let Some(v) = &b.value {
            self.expr(st, v, ctx);
        }
        if st.reachable {
            self.run_defers(st, level, false);
            let end = Span { file: b.span.file, lo: b.span.hi.saturating_sub(1), hi: b.span.hi };
            self.exit(st, level, end, "where its block ends", true);
        }
        let sc = self.scopes.pop().unwrap();
        st.m.retain(|p, _| !sc.declared.contains(&p.0));
    }

    fn run_defers(&mut self, st: &mut St, from: usize, with_err: bool) {
        let levels: Vec<Vec<(&'a TBlock, bool)>> = self.scopes[from..].iter().map(|s| s.defers.clone()).collect();
        for level in levels.iter().rev() {
            for (b, err_only) in level.iter().rev() {
                if !*err_only || with_err {
                    self.run_defer(st, b);
                }
            }
        }
    }

    fn run_defer(&mut self, st: &mut St, b: &'a TBlock) {
        let guards = defer_guards(self.prog, self.tf, b);
        if guards.len() == 1 {
            let g = guards[0];
            let has: Vec<S> = st.m.iter().filter(|(p, _)| p.0 == g).map(|(_, s)| *s).collect();
            if has.is_empty() {
                return; // consumed already: the defer doesn't run
            }
            if has.len() == 1 && has[0] == S::Live && st.m.contains_key(&(g, vec![])) {
                self.block(st, b, Ctx::Discarded);
                return;
            }
            let mut run = st.clone();
            run.m.retain(|p, _| p.0 != g);
            run.m.insert((g, vec![]), S::Live);
            self.block(&mut run, b, Ctx::Discarded);
            let mut skip = st.clone();
            skip.m.retain(|p, _| p.0 != g);
            skip.join(&run);
            *st = skip;
            return;
        }
        self.block(st, b, Ctx::Discarded);
    }

    fn stmt(&mut self, st: &mut St, s: &'a TStmt) {
        if !st.reachable {
            return;
        }
        match s {
            TStmt::Let { local, init } => {
                self.expr(st, init, Ctx::Consumed);
                self.declare(*local);
                if st.reachable && self.lin(&self.tf.locals[*local as usize].ty) && !self.linear_free(init) {
                    st.m.insert((*local, vec![]), S::Live);
                }
            }
            TStmt::LetRef { local, place } => {
                self.expr(st, place, Ctx::Borrowed);
                self.declare(*local);
            }
            TStmt::Expr(e) => self.expr(st, e, Ctx::Discarded),
            TStmt::Assign { place, value, span, .. } => {
                self.expr(st, value, Ctx::Consumed);
                self.place_subexprs(st, place);
                if !st.reachable || !self.lin(&place.ty) {
                    return;
                }
                match place_path(place) {
                    Some(p) => {
                        let old: Vec<Path> = st.m.keys().filter(|q| q.0 == p.0 && (q.1.starts_with(&p.1) || p.1.starts_with(&q.1))).cloned().collect();
                        if !old.is_empty() {
                            let d = Diag::new(*span, format!("assigning to `{}` drops the linear `{}` it still holds", self.path_name(&p), self.show(&place.ty))).note(None, self.how(&place.ty));
                            self.err(d);
                            for q in old {
                                st.m.remove(&q);
                            }
                        }
                        if !self.linear_free(value) {
                            self.refill(st, &p);
                        }
                    }
                    None => {
                        let d = Diag::new(*span, format!("assigning here drops the linear `{}` that was there", self.show(&place.ty))).note(None, "take the old value out first (`replace(&v[i], x)` gives it back)");
                        self.err(d);
                    }
                }
            }
            TStmt::MapInsert { map, key, value } => {
                self.expr(st, map, Ctx::Borrowed);
                self.expr(st, key, Ctx::Consumed);
                self.expr(st, value, Ctx::Consumed);
            }
            TStmt::Return(e, sp) => {
                if let Some(e) = e {
                    self.expr(st, e, Ctx::Consumed);
                }
                if st.reachable {
                    let with_err = e.as_ref().map(is_error_value).unwrap_or(false);
                    self.run_defers(st, 0, with_err);
                    self.exit(st, 0, *sp, "by this `return`", false);
                }
                *st = St::unreachable();
            }
            TStmt::Break(id, sp) | TStmt::Continue(id, sp) => {
                let is_break = matches!(s, TStmt::Break(..));
                if let Some(li) = self.loops.iter().rposition(|l| l.id == *id) {
                    let level = self.loops[li].level;
                    self.run_defers(st, level, false);
                    self.exit(st, level, *sp, if is_break { "by this `break`" } else { "by this `continue`" }, false);
                    let s2 = st.clone();
                    if is_break {
                        self.loops[li].breaks.push(s2);
                    } else {
                        self.loops[li].conts.push(s2);
                    }
                }
                *st = St::unreachable();
            }
            TStmt::Defer(b) | TStmt::ErrDefer(b) => {
                let guards = defer_guards(self.prog, self.tf, b);
                if guards.len() > 1 {
                    let names: Vec<String> = guards.iter().map(|&g| format!("`{}`", self.tf.locals[g as usize].name)).collect();
                    let d = Diag::new(b.span, format!("this defer consumes several linear values ({}): it runs only while the one it consumes still holds its value, so it may consume one", names.join(", "))).note(None, "write one defer per value");
                    self.err(d);
                }
                if let Some(sc) = self.scopes.last_mut() {
                    sc.defers.push((b, matches!(s, TStmt::ErrDefer(_))));
                }
            }
            TStmt::While { id, cond, body } => {
                let body: &'a TBlock = body;
                let cond = cond.as_ref();
                self.run_loop(st, *id, |me, s| {
                    let mut exit = None;
                    if let Some(c) = cond {
                        me.expr(s, c, Ctx::Borrowed);
                        exit = Some(s.clone());
                    }
                    me.block(s, body, Ctx::Discarded);
                    exit
                });
            }
            TStmt::For { id, iter, hi, body, .. } => {
                self.expr(st, iter, Ctx::Borrowed);
                if let Some(h) = hi {
                    self.expr(st, h, Ctx::Borrowed);
                }
                let body: &'a TBlock = body;
                self.run_loop(st, *id, |me, s| {
                    let exit = s.clone();
                    me.block(s, body, Ctx::Discarded);
                    Some(exit)
                });
            }
            TStmt::ForEach { id, call } => {
                // the body is a closure, run once per element: a loop
                if let TK::Call { args, .. } = &call.kind {
                    let n = args.len();
                    for (i, a) in args.iter().enumerate() {
                        if i + 1 < n {
                            self.expr(st, &a.e, Ctx::Borrowed);
                        }
                    }
                    if let Some(TArg { e: TExpr { kind: TK::Closure(c), .. }, .. }) = args.last() {
                        let body: &'a TBlock = &c.body;
                        self.run_loop(st, *id, |me, s| {
                            let exit = s.clone();
                            me.block(s, body, Ctx::Discarded);
                            Some(exit)
                        });
                    }
                } else {
                    self.expr(st, call, Ctx::Discarded);
                }
            }
            TStmt::ParFor { iter, hi, body, .. } => {
                self.expr(st, iter, Ctx::Borrowed);
                if let Some(h) = hi {
                    self.expr(st, h, Ctx::Borrowed);
                }
                self.expr(st, body, Ctx::Borrowed);
            }
            TStmt::Send { chan, value } => {
                self.expr(st, chan, Ctx::Borrowed);
                self.expr(st, value, Ctx::Consumed);
            }
            TStmt::With { local, place, body, .. } => {
                self.expr(st, place, Ctx::Borrowed);
                self.scopes.push(Scope { declared: vec![*local], defers: vec![] });
                self.block(st, body, Ctx::Discarded);
                self.scopes.pop();
            }
            TStmt::WithLocks { places, body, .. } => {
                for p in places {
                    self.expr(st, p, Ctx::Borrowed);
                }
                self.block(st, body, Ctx::Discarded);
            }
            TStmt::Yield(e, _) => {
                if let Some(e) = e {
                    self.expr(st, e, Ctx::Borrowed);
                }
            }
            TStmt::Select { arms, .. } => {
                let start = st.clone();
                let mut out = St::unreachable();
                for a in arms {
                    let mut s = start.clone();
                    let mut binds = vec![];
                    match &a.kind {
                        TSelectKind::Recv { chan, bind } => {
                            self.expr(&mut s, chan, Ctx::Borrowed);
                            binds.extend(bind.iter().copied());
                        }
                        TSelectKind::Send { chan, value } => {
                            self.expr(&mut s, chan, Ctx::Borrowed);
                            self.expr(&mut s, value, Ctx::Consumed);
                        }
                        TSelectKind::Task { task, bind } => {
                            self.expr(&mut s, task, Ctx::Borrowed);
                            binds.extend(bind.iter().copied());
                        }
                        TSelectKind::Timer { at, .. } => self.expr(&mut s, at, Ctx::Borrowed),
                        TSelectKind::Cancelled | TSelectKind::Default => {}
                    }
                    if let Some(g) = &a.guard {
                        self.expr(&mut s, g, Ctx::Borrowed);
                    }
                    self.scopes.push(Scope { declared: binds.clone(), defers: vec![] });
                    for b in binds {
                        if self.lin(&self.tf.locals[b as usize].ty) {
                            s.m.insert((b, vec![]), S::Live);
                        }
                    }
                    self.block(&mut s, &a.body, Ctx::Discarded);
                    let level = self.scopes.len() - 1;
                    self.exit(&mut s, level, a.body.span, "where the arm ends", true);
                    self.scopes.pop();
                    out.join(&s);
                }
                *st = out;
            }
        }
    }

    /// Runs a loop body to a fixpoint. `body` returns the state in which the
    /// loop ends by its own test (none for `loop { }`).
    fn run_loop(&mut self, st: &mut St, id: u32, mut body: impl FnMut(&mut Self, &mut St) -> Option<St>) {
        let entry = st.clone();
        let mut head = entry.clone();
        self.suppress += 1;
        for _ in 0..8 {
            let (end, _, _) = self.loop_once(&head, id, &mut body);
            let mut next = entry.clone();
            next.join(&end);
            if next == head {
                break;
            }
            head = next;
        }
        self.suppress -= 1;
        let (_, exit, breaks) = self.loop_once(&head, id, &mut body);
        let mut out = exit.unwrap_or_else(St::unreachable);
        for b in &breaks {
            out.join(b);
        }
        *st = out;
    }

    /// One pass over a loop body from `head`: the state where the iteration
    /// ends (or continues), where the loop's own test ends it, and at its
    /// `break`s.
    fn loop_once(&mut self, head: &St, id: u32, body: &mut impl FnMut(&mut Self, &mut St) -> Option<St>) -> (St, Option<St>, Vec<St>) {
        let level = self.scopes.len();
        self.loops.push(LoopCx { id, level, breaks: vec![], conts: vec![] });
        let mut s = head.clone();
        let exit = body(self, &mut s);
        let cx = self.loops.pop().unwrap();
        for c in &cx.conts {
            s.join(c);
        }
        (s, exit, cx.breaks)
    }

    fn place_subexprs(&mut self, st: &mut St, e: &'a TExpr) {
        match &e.kind {
            TK::Field { base, .. } => self.place_subexprs(st, base),
            TK::Index { base, index } => {
                self.place_subexprs(st, base);
                self.expr(st, index, Ctx::Borrowed);
            }
            TK::MapIndex { base, key } => {
                self.place_subexprs(st, base);
                self.expr(st, key, Ctx::Borrowed);
            }
            TK::Deref(b) => self.place_subexprs(st, b),
            TK::Local(_) | TK::Global(_) => {}
            _ => self.expr(st, e, Ctx::Borrowed),
        }
    }

    /// The value certainly holds nothing linear: `none`, an empty vector, a
    /// variant without linear payload of a type that isn't linear itself.
    fn linear_free(&self, e: &TExpr) -> bool {
        match &e.kind {
            TK::NoneLit => true,
            TK::VecLit(xs) => xs.is_empty(),
            TK::StructLit { def, fields, .. } if self.prog.def(*def).tuple => fields.iter().all(|f| self.linear_free(f) || !self.lin(&f.ty)),
            TK::EnumLit { def, args, .. } => !self.prog.def(*def).linear && args.iter().all(|a| self.linear_free(a) || !self.lin(&a.ty)),
            TK::OkLit(x) | TK::ErrLit(x) | TK::SomeLit(x) => match &e.ty {
                Ty::Result(a, b) => {
                    let other = if matches!(e.kind, TK::OkLit(_)) { b } else { a };
                    let _ = other;
                    self.linear_free(x) || !self.lin(&x.ty)
                }
                _ => self.linear_free(x) || !self.lin(&x.ty),
            },
            TK::Call { callee: Callee::Func(f, _), args } if args.is_empty() => {
                // `Vec[T]()`, `Vec[T].new()`
                let fd = self.prog.func(*f);
                fd.name == "new" && matches!(fd.ret, Ty::Vec(_))
            }
            TK::DefaultOf => matches!(e.ty, Ty::Vec(_) | Ty::Option(_)),
            TK::Coerce(_, x) => self.linear_free(x),
            _ => false,
        }
    }

    /// A temporary of a linear type that nobody takes.
    fn rvalue(&mut self, e: &TExpr, ctx: Ctx) {
        if ctx == Ctx::Consumed || e.ty == Ty::Never || !self.lin(&e.ty) || self.linear_free(e) {
            return;
        }
        let msg = if ctx == Ctx::Discarded { format!("this linear `{}` is thrown away", self.show(&e.ty)) } else { format!("this linear `{}` is dropped at the end of the statement: it is only borrowed here", self.show(&e.ty)) };
        let d = Diag::new(e.span, msg).note(None, self.how(&e.ty));
        self.err(d);
    }

    fn expr(&mut self, st: &mut St, e: &'a TExpr, ctx: Ctx) {
        if !st.reachable {
            return;
        }
        match &e.kind {
            TK::Local(_) | TK::Global(_) | TK::Const(_) | TK::ConstParam(_) | TK::FuncRef(..) | TK::Unit | TK::Int(_) | TK::Float(_) | TK::Bool(_) | TK::Str(_) | TK::NoneLit | TK::DefaultOf | TK::Error => {
                if !e.is_place() {
                    self.rvalue(e, ctx);
                }
            }
            TK::Field { base, .. } | TK::OptField { base, .. } => {
                if base.is_place() {
                    self.place_subexprs(st, base);
                } else {
                    self.expr(st, base, Ctx::Borrowed);
                }
                if !e.is_place() {
                    self.rvalue(e, ctx);
                }
            }
            TK::Deref(base) => self.expr(st, base, Ctx::Borrowed),
            TK::Index { base, index } | TK::StrIndex { base, index } => {
                self.expr(st, base, Ctx::Borrowed);
                self.expr(st, index, Ctx::Borrowed);
            }
            TK::MapIndex { base, key } => {
                self.expr(st, base, Ctx::Borrowed);
                self.expr(st, key, Ctx::Borrowed);
            }
            TK::SliceOf { base, lo, hi } => {
                self.expr(st, base, Ctx::Borrowed);
                for x in [lo, hi].into_iter().flatten() {
                    self.expr(st, x, Ctx::Borrowed);
                }
            }
            TK::Call { args, .. } | TK::Intrinsic { args, .. } => {
                for a in args {
                    let c = if a.mode == ArgMode::Value { Ctx::Consumed } else { Ctx::Borrowed };
                    self.expr(st, &a.e, c);
                }
                // what an `inout` argument holds may have changed
                for a in args {
                    if a.mode == ArgMode::Inout && self.lin(&a.e.ty) {
                        if let Some(p) = place_path(&a.e) {
                            self.refill(st, &p);
                        }
                    }
                }
                self.rvalue(e, ctx);
            }
            TK::Proj { call, .. } => self.expr(st, call, Ctx::Borrowed),
            TK::ProjUnwrap { proj, msg } => {
                self.expr(st, proj, Ctx::Borrowed);
                if let Some(m) = msg {
                    self.expr(st, m, Ctx::Borrowed);
                }
            }
            TK::Unary { e: x, .. } | TK::Cast(x) | TK::Copy(x) | TK::MakeChan(x) | TK::MutexRef(x) => {
                self.expr(st, x, Ctx::Borrowed);
                self.rvalue(e, ctx);
            }
            TK::Recv(x) => {
                self.expr(st, x, Ctx::Borrowed);
                self.rvalue(e, ctx);
            }
            TK::Binary { l, r, .. } => {
                self.expr(st, l, Ctx::Borrowed);
                self.expr(st, r, Ctx::Borrowed);
            }
            TK::Move(p) => {
                self.place_subexprs(st, p);
                if let Some(path) = place_path(p) {
                    self.consume(st, &path);
                }
                self.rvalue(e, ctx);
            }
            TK::Coerce(c, x) => {
                let cx = match c {
                    Coercion::ToIface { owned: true } | Coercion::FuncItem => Ctx::Consumed,
                    _ => Ctx::Borrowed,
                };
                self.expr(st, x, cx);
                self.rvalue(e, ctx);
            }
            TK::SomeLit(x) | TK::OkLit(x) | TK::ErrLit(x) | TK::Wrap(x) | TK::Unbox(x) => {
                self.expr(st, x, Ctx::Consumed);
                self.rvalue(e, ctx);
            }
            TK::StructLit { fields: xs, .. } | TK::EnumLit { args: xs, .. } | TK::VecLit(xs) | TK::ArrayLit(xs) => {
                for x in xs {
                    self.expr(st, x, Ctx::Consumed);
                }
                self.rvalue(e, ctx);
            }
            TK::MapLit(ps) => {
                for (k, v) in ps {
                    self.expr(st, k, Ctx::Consumed);
                    self.expr(st, v, Ctx::Consumed);
                }
            }
            TK::Closure(c) => self.closure(st, c),
            TK::If { cond, then, else_ } => {
                self.expr(st, cond, Ctx::Borrowed);
                let mut s1 = st.clone();
                self.block(&mut s1, then, ctx);
                if let Some(b) = else_ {
                    self.block(st, b, ctx);
                }
                st.join(&s1);
            }
            TK::Match { scrut, arms, .. } => self.match_expr(st, scrut, arms, ctx),
            TK::Block(b) | TK::Unsafe(b) => self.block(st, b, ctx),
            TK::Scope { body, .. } => self.block(st, body, ctx),
            TK::Isolate(b) => {
                self.block(st, b, Ctx::Consumed);
                self.rvalue(e, ctx);
            }
            TK::Spawn { target, task, .. } => {
                if let SpawnTarget::Value(v) = target {
                    self.expr(st, v, Ctx::Borrowed);
                }
                if let TK::Closure(c) = &task.kind {
                    self.closure(st, c);
                } else {
                    self.expr(st, task, Ctx::Consumed);
                }
            }
            TK::Try { e: x, wrap } => {
                self.expr(st, x, Ctx::Consumed);
                if !st.reachable {
                    return;
                }
                let mut err = st.clone();
                if let Some(TryWrap::Call(_, w)) = wrap {
                    self.expr(&mut err, w, Ctx::Consumed);
                }
                self.run_defers(&mut err, 0, true);
                self.exit(&mut err, 0, e.span, "when this `try` returns the error", false);
                self.rvalue(e, ctx);
            }
            TK::Catch { e: x, bind, handler } => {
                self.expr(st, x, Ctx::Consumed);
                let mut h = st.clone();
                let binds: Vec<LocalId> = bind.iter().copied().collect();
                self.scopes.push(Scope { declared: binds.clone(), defers: vec![] });
                for b in binds {
                    if self.lin(&self.tf.locals[b as usize].ty) {
                        h.m.insert((b, vec![]), S::Live);
                    }
                }
                // the handler's value stands in for the success value, which
                // is judged once below
                self.block(&mut h, handler, Ctx::Consumed);
                let level = self.scopes.len() - 1;
                self.exit(&mut h, level, handler.span, "where the `catch` block ends", true);
                self.scopes.pop();
                st.join(&h);
                self.rvalue(e, ctx);
            }
            TK::Coalesce { l, r } => {
                self.expr(st, l, Ctx::Consumed);
                let mut s2 = st.clone();
                self.expr(&mut s2, r, Ctx::Consumed);
                st.join(&s2);
                self.rvalue(e, ctx);
            }
        }
        if e.ty == Ty::Never {
            *st = St::unreachable();
        }
    }

    fn closure(&mut self, st: &mut St, c: &'a TClosure) {
        // what it captures by move is consumed here (a `go` task's body
        // consumes it in turn; any other closure can't capture one)
        let spawned = self.spawned(c);
        let mut inner = St { reachable: true, m: HashMap::new() };
        let mut decl = vec![];
        for &(l, u) in &c.captures {
            if u == CapUse::Move || c.owned {
                if self.lin(&self.tf.locals[l as usize].ty) {
                    let keys: Vec<Path> = st.m.keys().filter(|p| p.0 == l).cloned().collect();
                    for k in keys {
                        let s = st.m.remove(&k).unwrap();
                        if spawned {
                            inner.m.insert(k, s);
                        }
                    }
                    decl.push(l);
                }
            }
        }
        decl.extend(c.params.iter().copied());
        // the closure body is checked on its own: its exits are its own
        let scopes = std::mem::take(&mut self.scopes);
        let loops = std::mem::take(&mut self.loops);
        self.scopes.push(Scope { declared: decl, defers: vec![] });
        self.block(&mut inner, &c.body, Ctx::Consumed);
        if inner.reachable {
            let end = Span { file: c.body.span.file, lo: c.body.span.hi.saturating_sub(1), hi: c.body.span.hi };
            self.exit(&mut inner, 0, end, "where the closure ends", true);
        }
        self.scopes = scopes;
        self.loops = loops;
    }

    fn match_expr(&mut self, st: &mut St, scrut: &'a TExpr, arms: &'a [TArm], ctx: Ctx) {
        let spath = if scrut.is_place() { place_path(scrut) } else { None };
        if scrut.is_place() {
            self.place_subexprs(st, scrut);
        } else {
            // judged arm by arm below
            self.expr(st, scrut, Ctx::Consumed);
        }
        if !st.reachable {
            return;
        }
        let lin = self.lin(&scrut.ty);
        let start = st.clone();
        let mut out = St::unreachable();
        // the variants each arm can still see: earlier unguarded arms that
        // match a whole variant take it away from the later ones
        let mut remaining: Option<Vec<usize>> = self.ctors(&scrut.ty).map(|n| (0..n).collect());
        let exempt = spath.as_ref().map(|p| p.1.is_empty() && Some(p.0) == self.exempt).unwrap_or(false);
        for a in arms {
            let mut s = start.clone();
            let seen_ctors = remaining.as_ref().map(|r| {
                let mine = self.pat_ctors(&a.pat, &scrut.ty);
                r.iter().copied().filter(|c| mine.as_ref().map(|m| m.contains(c)).unwrap_or(true)).collect::<Vec<usize>>()
            });
            let free = lin && seen_ctors.as_ref().map(|cs| cs.iter().all(|&c| self.ctor_linear_free(&scrut.ty, c, exempt))).unwrap_or(false);
            if a.guard.is_none() {
                if let (Some(r), Some(mine)) = (remaining.as_mut(), self.full_ctors(&a.pat, &scrut.ty)) {
                    r.retain(|c| !mine.contains(c));
                }
            }
            // on a temporary, the arm's bindings own what they match
            let owns = !scrut.is_place() && a.binds.iter().any(|&b| matches!(self.tf.locals[b as usize].kind, LocalKind::Let | LocalKind::Var));
            if lin {
                if a.consuming || owns || free {
                    if let Some(p) = &spath {
                        self.consume(&mut s, p);
                    }
                    if a.consuming || owns {
                        self.wild_parts(&a.pat, &scrut.ty, a.body.span);
                    }
                } else if !scrut.is_place() {
                    let d = Diag::new(a.body.span, format!("this arm drops the matched linear `{}`", self.show(&scrut.ty))).note(None, "bind the value to a variable first, or move what the arm matches out of it");
                    self.err(d);
                }
            }
            let mut decl = vec![];
            for &b in &a.binds {
                let l = &self.tf.locals[b as usize];
                if matches!(l.kind, LocalKind::Let | LocalKind::Var) && self.lin(&l.ty) {
                    s.m.insert((b, vec![]), S::Live);
                }
                decl.push(b);
            }
            self.scopes.push(Scope { declared: decl, defers: vec![] });
            if let Some(g) = &a.guard {
                self.expr(&mut s, g, Ctx::Borrowed);
            }
            self.expr(&mut s, &a.body, ctx);
            let level = self.scopes.len() - 1;
            self.exit(&mut s, level, a.body.span, "where the arm ends", true);
            self.scopes.pop();
            out.join(&s);
        }
        *st = out;
    }

    /// The number of top-level constructors of an enum-like type.
    fn ctors(&self, t: &Ty) -> Option<usize> {
        match t {
            Ty::Option(_) | Ty::Result(..) => Some(2),
            Ty::Adt(d, _) if !self.prog.def(*d).is_struct() && !matches!(self.prog.def(*d).kind, TypeDefKind::Iface { .. }) => Some(self.prog.def(*d).variants().len()),
            _ => None,
        }
    }

    /// The constructors a pattern may match (none: all of them).
    fn pat_ctors(&self, p: &TPat, t: &Ty) -> Option<Vec<usize>> {
        match p {
            TPat::Some(_) => Some(vec![0]),
            TPat::None => Some(vec![1]),
            TPat::Ok(_) => Some(vec![0]),
            TPat::Err(_) => Some(vec![1]),
            TPat::Variant { variant, .. } => Some(vec![*variant]),
            TPat::Or(ps) => {
                let mut out = vec![];
                for q in ps {
                    out.extend(self.pat_ctors(q, t)?);
                }
                Some(out)
            }
            _ => None,
        }
    }

    /// The constructors a pattern matches whatever their payload holds.
    fn full_ctors(&self, p: &TPat, t: &Ty) -> Option<Vec<usize>> {
        let irrefutable = |q: &TPat| matches!(q, TPat::Wild | TPat::Bind(_));
        match p {
            TPat::Wild | TPat::Bind(_) => self.ctors(t).map(|n| (0..n).collect()),
            TPat::Some(q) | TPat::Ok(q) if irrefutable(q) => self.pat_ctors(p, t),
            TPat::Err(q) if irrefutable(q) => self.pat_ctors(p, t),
            TPat::None => Some(vec![1]),
            TPat::Variant { variant, subs, .. } if subs.iter().all(irrefutable) => Some(vec![*variant]),
            TPat::Or(ps) => {
                let mut out = vec![];
                for q in ps {
                    out.extend(self.full_ctors(q, t).unwrap_or_default());
                }
                Some(out)
            }
            _ => Some(vec![]),
        }
    }

    /// Whether a value built with constructor `c` of `t` holds nothing linear.
    fn ctor_linear_free(&self, t: &Ty, c: usize, exempt: bool) -> bool {
        match t {
            Ty::Option(a) => c == 1 || !self.lin(a),
            Ty::Result(a, b) => !self.lin(if c == 0 { a } else { b }),
            Ty::Adt(d, args) => (exempt || !self.prog.def(*d).linear) && self.prog.variant_tys(*d, args, c).iter().all(|(_, t)| !self.lin(t)),
            _ => false,
        }
    }

    /// In an arm that moves out of the matched value, a `_` (or literal) in
    /// place of a linear part would drop it.
    fn wild_parts(&mut self, p: &TPat, t: &Ty, sp: Span) {
        let subs: Vec<(&TPat, Ty)> = match (p, t) {
            (TPat::Some(q), Ty::Option(a)) => vec![(&**q, (**a).clone())],
            (TPat::Ok(q), Ty::Result(a, _)) => vec![(&**q, (**a).clone())],
            (TPat::Err(q), Ty::Result(_, b)) => vec![(&**q, (**b).clone())],
            (TPat::Variant { variant, subs, .. }, Ty::Adt(d, args)) => {
                let tys = self.prog.variant_tys(*d, args, *variant);
                subs.iter().zip(tys.into_iter().map(|x| x.1)).collect()
            }
            (TPat::Tuple(qs), Ty::Adt(d, args)) => {
                let tys = self.prog.field_tys(*d, args);
                qs.iter().zip(tys.into_iter().map(|x| x.1)).collect()
            }
            (TPat::Or(ps), _) => {
                for q in ps {
                    self.wild_parts(q, t, sp);
                }
                return;
            }
            _ => return,
        };
        for (q, qt) in subs {
            match q {
                TPat::Bind(_) => {}
                TPat::Wild if self.lin(&qt) => {
                    let d = Diag::new(sp, format!("this arm moves out of the matched value, and its `_` drops the linear `{}`", self.show(&qt))).note(None, "bind it to a name and consume it");
                    self.err(d);
                }
                _ => self.wild_parts(q, &qt, sp),
            }
        }
    }
}
