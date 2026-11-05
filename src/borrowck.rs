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
use std::collections::HashMap;

type Path = (LocalId, Vec<usize>);

pub fn check(prog: &Program, tfuncs: &[TFunc]) -> Vec<Diag> {
    let mut diags = vec![];
    for tf in tfuncs {
        let fd = prog.func(tf.func);
        if prog.packages[fd.pkg as usize].is_std {
            continue;
        }
        let mut b = Bc::new(prog, tf, fd.ret.is_view() && fd.yields.is_none());
        let mut st = St::new();
        b.block(&mut st, &tf.body);
        b.end_scope_defers(&mut st);
        diags.extend(b.diags);
    }
    diags
}

#[derive(Clone)]
struct St {
    moved: Vec<(Path, Span)>,
    reachable: bool,
}

impl St {
    fn new() -> St {
        St { moved: vec![], reachable: true }
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
    }
    fn unreachable() -> St {
        St { moved: vec![], reachable: false }
    }
}

#[derive(Clone)]
struct Loan {
    target: Path,
    mutable: bool,
    holder: Option<LocalId>,
    span: Span,
    depth: usize,
}

struct LoopCx {
    id: u32,
    breaks: Vec<St>,
    conts: Vec<St>,
}

struct Bc<'a> {
    prog: &'a Program,
    tf: &'a TFunc,
    diags: Vec<Diag>,
    loans: Vec<Loan>,
    /// Reference / view locals and the owned paths they borrow.
    holders: HashMap<LocalId, Vec<(Path, bool)>>,
    depth: usize,
    suppress: u32,
    loops: Vec<LoopCx>,
    view_ret: bool,
    defers: Vec<Vec<TBlock>>,
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
    fn new(prog: &'a Program, tf: &'a TFunc, view_ret: bool) -> Bc<'a> {
        Bc { prog, tf, diags: vec![], loans: vec![], holders: HashMap::new(), depth: 0, suppress: 0, loops: vec![], view_ret, defers: vec![vec![]] }
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
        match place_path(e) {
            Some(p) => {
                if self.is_holder(p.0) {
                    self.holders.get(&p.0).cloned().unwrap_or_default()
                } else {
                    vec![(p, false)]
                }
            }
            None => vec![],
        }
    }

    fn is_copy(&self, t: &Ty) -> bool {
        match t {
            Ty::Unit | Ty::Never | Ty::Bool | Ty::Int(_) | Ty::F32 | Ty::F64 | Ty::Str | Ty::Slice(_) | Ty::Ptr(_) | Ty::Error | Ty::ConstInt(_) => true,
            Ty::Option(a) | Ty::Array(a, _) => self.is_copy(a),
            Ty::Result(a, b) => self.is_copy(a) && self.is_copy(b),
            Ty::Adt(d, _) => self.prog.def(*d).is_copy,
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
        let conflict = self.loans.iter().find(|l| overlap(&l.target, p) && (acc == Acc::Mut || l.mutable) && (via.is_none() || l.holder != via)).cloned();
        if let Some(l) = conflict {
            let n = self.name(p);
            let by = match l.holder {
                Some(h) if !self.tf.locals[h as usize].name.starts_with("__") => format!(" by `{}`", self.tf.locals[h as usize].name),
                _ => String::new(),
            };
            let msg = match (acc, l.mutable) {
                (Acc::Mut, false) => format!("cannot modify `{}` while it is borrowed{}", n, by),
                (Acc::Mut, true) => format!("cannot modify `{}` while it is mutably borrowed{}", n, by),
                (Acc::Read, _) => format!("cannot use `{}` while it is mutably borrowed{}", n, by),
            };
            self.err(Diag::new(sp, msg).note(Some(l.span), "the borrow starts here and lasts until the end of its block"));
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
            if self.is_holder(p.0) {
                return;
            }
            self.check_moved(st, &p, e.span, "use of");
            if let Some(l) = self.loans.iter().find(|l| overlap(&l.target, &p)).cloned() {
                let n = self.name(&p);
                self.err(Diag::new(e.span, format!("cannot move `{}` while it is borrowed", n)).note(Some(l.span), "borrowed here"));
            }
            if st.reachable {
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
        for (t, _) in &targets {
            self.check_moved(st, t, sp, "borrow of");
            let acc = if mutable { Acc::Mut } else { Acc::Read };
            let saved: Vec<Loan> = self.loans.clone();
            self.loans.retain(|l| !l.holder.map(|h| vias.contains(&h)).unwrap_or(false));
            self.check_loans(t, acc, sp, None);
            self.loans = saved;
        }
        for (t, _) in &targets {
            self.loans.push(Loan { target: t.clone(), mutable, holder: Some(holder), span: sp, depth: self.depth });
        }
        self.holders.insert(holder, targets.into_iter().map(|(p, _)| (p, mutable)).collect());
    }

    /// Owned paths a view-producing expression borrows; Err if it borrows a temporary.
    fn view_sources(&self, e: &TExpr) -> Result<Vec<(Path, bool)>, Span> {
        match &e.kind {
            TK::Str(_) | TK::Unit | TK::NoneLit => Ok(vec![]),
            TK::Local(l) => Ok(self.holders.get(l).cloned().unwrap_or_default()),
            TK::Copy(x) | TK::Move(x) | TK::SomeLit(x) => self.view_sources(x),
            TK::Coerce(Coercion::StringToStr | Coercion::VecToSlice | Coercion::ArrayToSlice, x) => {
                if x.is_place() {
                    Ok(self.targets(x))
                } else {
                    Err(x.span)
                }
            }
            TK::SliceOf { base, .. } => {
                if base.ty.is_view() {
                    self.view_sources(base)
                } else if base.is_place() {
                    Ok(self.targets(base))
                } else {
                    Err(base.span)
                }
            }
            TK::Call { args, .. } | TK::Intrinsic { args, .. } => {
                let mut out = vec![];
                for a in args {
                    if a.mode == ArgMode::Value && !a.e.ty.is_view() {
                        continue;
                    }
                    if a.e.ty.is_view() {
                        out.extend(self.view_sources(&a.e)?);
                    } else if a.e.is_place() {
                        out.extend(self.targets(&a.e));
                    } else if matches!(a.e.ty, Ty::String | Ty::Vec(_)) {
                        return Err(a.e.span);
                    }
                }
                Ok(out)
            }
            TK::Proj { call, .. } => self.view_sources(call),
            TK::ProjUnwrap { proj, .. } => self.view_sources(proj),
            TK::MutexRef(p) => Ok(self.targets(p).into_iter().map(|(t, _)| (t, true)).collect()),
            TK::Block(b) | TK::Unsafe(b) => match &b.value {
                Some(v) => self.view_sources(v),
                None => Ok(vec![]),
            },
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
            _ => Ok(vec![]),
        }
    }

    fn end_depth(&mut self) {
        let d = self.depth;
        self.loans.retain(|l| l.depth < d);
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
        }
        self.end_scope_defers(st);
        self.defers.pop();
        self.end_depth();
        self.depth -= 1;
    }

    fn end_scope_defers(&mut self, st: &mut St) {
        if let Some(ds) = self.defers.last().cloned() {
            for d in ds.iter().rev() {
                let mut s2 = st.clone();
                s2.reachable = true;
                self.block(&mut s2, d);
            }
        }
    }

    fn stmt(&mut self, st: &mut St, s: &TStmt) {
        match s {
            TStmt::Let { local, init } => {
                self.expr(st, init);
                self.reinit(st, &(*local, vec![]));
                let lt = &self.tf.locals[*local as usize].ty;
                if lt.is_view() {
                    match self.view_sources(init) {
                        Ok(srcs) => {
                            if !srcs.is_empty() {
                                let mutable = matches!(init.kind, TK::MutexRef(_));
                                self.add_loans(*local, srcs, mutable, init.span, st, init);
                            }
                        }
                        Err(sp) => self.err(Diag::new(sp, "this temporary value is dropped at the end of the statement, but a view of it is kept").note(None, "bind the value to a variable first: `let s = ...` and borrow from that")),
                    }
                }
            }
            TStmt::LetRef { local, place } => {
                self.read(st, place);
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
                if let Some(e) = e {
                    self.expr(st, e);
                    if self.view_ret {
                        self.check_view_return(e, *sp);
                    }
                }
                self.end_scope_defers_all(st);
                *st = St::unreachable();
            }
            TStmt::Yield(e, _) => {
                if let Some(e) = e {
                    self.read(st, e);
                }
                *st = St::unreachable();
            }
            TStmt::Break(id, _) => {
                if let Some(l) = self.loops.iter_mut().rev().find(|l| l.id == *id) {
                    l.breaks.push(st.clone());
                }
                *st = St::unreachable();
            }
            TStmt::Continue(id, _) => {
                if let Some(l) = self.loops.iter_mut().rev().find(|l| l.id == *id) {
                    l.conts.push(st.clone());
                }
                *st = St::unreachable();
            }
            TStmt::ErrDefer(b) => {
                let mut s2 = st.clone();
                self.block(&mut s2, b);
            }
            TStmt::Defer(b) => {
                if let Some(d) = self.defers.last_mut() {
                    d.push(b.clone());
                }
            }
            TStmt::While { id, cond, body } => {
                self.loop_body(st, *id, |me, s| {
                    if let Some(c) = cond {
                        me.expr(s, c);
                    }
                    me.block(s, body);
                }, cond.is_none());
            }
            TStmt::For { id, kind, iter, hi, elem, index, mutable, body, .. } => {
                self.expr(st, iter);
                if let Some(h) = hi {
                    self.expr(st, h);
                }
                let holds = !matches!(kind, IterKind::Range { .. } | IterKind::Chan | IterKind::Str) || (*kind == IterKind::Str && iter.is_place());
                self.depth += 1;
                if holds && iter.is_place() {
                    let targets = self.targets(iter);
                    for (t, _) in &targets {
                        self.check_loans(t, if *mutable { Acc::Mut } else { Acc::Read }, iter.span, None);
                    }
                    for (t, _) in &targets {
                        self.loans.push(Loan { target: t.clone(), mutable: *mutable, holder: Some(*elem), span: iter.span, depth: self.depth });
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
                }, false);
                self.end_depth();
                self.depth -= 1;
            }
            TStmt::Go { call, .. } | TStmt::ForEach { call, .. } => self.expr(st, call),
            TStmt::Send { chan, value } => {
                self.expr(st, chan);
                self.expr(st, value);
            }
            TStmt::With { local, place, lock, body } => {
                self.read(st, place);
                self.depth += 1;
                let mutable = !*lock && matches!(self.tf.locals[*local as usize].kind, LocalKind::Ref { mutable: true });
                let targets = self.targets(place);
                self.add_loans(*local, targets, mutable, place.span, st, place);
                self.block(st, body);
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
                        _ => {}
                    }
                }
                let start = st.clone();
                let mut out = St::unreachable();
                for a in arms {
                    let mut s2 = start.clone();
                    if let TSelectKind::Recv { bind: Some(b), .. } = &a.kind {
                        self.reinit(&mut s2, &(*b, vec![]));
                    }
                    self.block(&mut s2, &a.body);
                    out.join(&s2);
                }
                *st = out;
            }
        }
    }

    fn end_scope_defers_all(&mut self, st: &mut St) {
        let all: Vec<TBlock> = self.defers.iter().rev().flat_map(|d| d.iter().rev().cloned()).collect();
        for d in all {
            let mut s2 = st.clone();
            self.block(&mut s2, &d);
        }
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

    fn check_view_return(&mut self, e: &TExpr, sp: Span) {
        match self.view_sources(e) {
            Ok(srcs) => {
                for (p, _) in srcs {
                    let l = &self.tf.locals[p.0 as usize];
                    if matches!(l.kind, LocalKind::Let | LocalKind::Var | LocalKind::Param(Mode::Sink)) {
                        let n = l.name.clone();
                        let lsp = l.span;
                        self.err(Diag::new(sp, format!("returns a view of `{}`, which is dropped when the function returns", n)).note(Some(lsp), "declared here"));
                    }
                }
            }
            Err(tsp) => self.err(Diag::new(tsp, "returns a view of a temporary value")),
        }
    }

    fn loop_body(&mut self, st: &mut St, id: u32, mut body: impl FnMut(&mut Self, &mut St), infinite: bool) {
        let entry = st.clone();
        // first pass discovers moves that happen inside the loop
        self.suppress += 1;
        self.loops.push(LoopCx { id, breaks: vec![], conts: vec![] });
        let mut s1 = entry.clone();
        let saved_loans = self.loans.clone();
        let saved_holders = self.holders.clone();
        body(self, &mut s1);
        let lc = self.loops.pop().unwrap();
        self.loans = saved_loans;
        self.holders = saved_holders;
        self.suppress -= 1;
        let mut head = entry.clone();
        head.join(&s1);
        for c in &lc.conts {
            head.join(c);
        }
        // second pass reports errors with moves from previous iterations
        self.loops.push(LoopCx { id, breaks: vec![], conts: vec![] });
        let mut s2 = head.clone();
        body(self, &mut s2);
        let lc = self.loops.pop().unwrap();
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
            TK::Call { .. } | TK::Intrinsic { .. } => self.call_args(st, e),
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
                                match &scrut.kind {
                                    TK::Proj { call, .. } => match &call.kind {
                                        TK::Call { args, .. } => args.first().map(|x| self.targets(&x.e)).unwrap_or_default(),
                                        _ => vec![],
                                    },
                                    _ => vec![],
                                }
                            } else {
                                self.targets(scrut)
                            };
                            let mutable = refs.iter().any(|b| matches!(self.tf.locals[*b as usize].kind, LocalKind::Ref { mutable: true }));
                            for (t, _) in &targets {
                                self.loans.push(Loan { target: t.clone(), mutable, holder: Some(refs[0]), span: scrut.span, depth: self.depth });
                            }
                            for r in refs {
                                self.holders.insert(r, targets.iter().map(|(p, _)| (p.clone(), mutable)).collect());
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
            TK::Try { e: x, wrap } => {
                self.expr(st, x);
                if let Some(TryWrap::Call(_, w)) = wrap {
                    let mut s2 = st.clone();
                    self.expr(&mut s2, w);
                }
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
            if c.owned && !self.is_copy(&t) {
                self.do_move(st, &le);
            } else if *u == CapUse::Mutate {
                self.write(st, &le);
            } else {
                self.read(st, &le);
            }
            if c.scoped {
                // tasks keep their borrows until the group finishes
                if let Some(p) = place_path(&le) {
                    if !self.is_holder(p.0) {
                        self.loans.push(Loan { target: p, mutable: false, holder: None, span: c.span, depth: 1 });
                    }
                }
            }
        }
        // the body is checked on its own
        let mut inner = Bc { prog: self.prog, tf: self.tf, diags: vec![], loans: vec![], holders: self.holders.clone(), depth: 0, suppress: self.suppress, loops: vec![], view_ret: false, defers: vec![vec![]] };
        let mut s = St::new();
        inner.block(&mut s, &c.body);
        inner.end_scope_defers(&mut s);
        for d in inner.diags {
            self.err(d);
        }
    }

    /// Arguments of a call, with exclusivity checking.
    fn call_args(&mut self, st: &mut St, e: &TExpr) {
        let args = match &e.kind {
            TK::Call { args, .. } | TK::Intrinsic { args, .. } => args,
            TK::Proj { call, .. } => return self.call_args(st, call),
            _ => return self.expr(st, e),
        };
        // (path, access, span, comes from a push-style loop body)
        let mut accesses: Vec<(Path, Acc, Span, bool)> = vec![];
        for a in args {
            match a.mode {
                ArgMode::Inout => {
                    self.write(st, &a.e);
                    for (p, _) in self.arg_targets(&a.e) {
                        accesses.push((p, Acc::Mut, a.e.span, false));
                    }
                }
                ArgMode::Borrow => {
                    if let TK::Closure(c) = &a.e.kind {
                        self.closure(st, c);
                        if !c.owned {
                            for (l, u) in &c.captures {
                                let le = TExpr::new(TK::Local(*l), self.tf.locals[*l as usize].ty.clone(), c.span);
                                for (p, _) in self.arg_targets(&le) {
                                    accesses.push((p, if *u == CapUse::Mutate { Acc::Mut } else { Acc::Read }, c.span, c.loop_body.is_some()));
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
                    if !self.is_copy(&a.e.ty) || a.e.ty.is_view() {
                        for (p, _) in self.arg_targets(&a.e) {
                            accesses.push((p, Acc::Read, a.e.span, false));
                        }
                    }
                }
                ArgMode::Value => {
                    self.expr(st, &a.e);
                    if let TK::Move(x) = &a.e.kind {
                        if let Some(p) = place_path(x) {
                            if !self.is_holder(p.0) {
                                accesses.push((p, Acc::Mut, a.e.span, false));
                            }
                        }
                    } else if a.e.ty.is_view() {
                        if let Ok(srcs) = self.view_sources(&a.e) {
                            for (p, _) in srcs {
                                accesses.push((p, Acc::Read, a.e.span, false));
                            }
                        }
                    }
                }
            }
        }
        for i in 0..accesses.len() {
            for j in 0..i {
                let (a, b) = (&accesses[i], &accesses[j]);
                if overlap(&a.0, &b.0) && (a.1 == Acc::Mut || b.1 == Acc::Mut) {
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
        if e.ty.is_view() && !e.is_place() {
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

fn place_is_direct(e: &TExpr) -> bool {
    match &e.kind {
        TK::Local(_) => true,
        TK::Field { base, .. } => place_is_direct(base),
        _ => false,
    }
}
