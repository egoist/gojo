// Inferred isolation at `go` (spec 10.5): a task may take a value that isn't
// `send` (an `Rc` graph, a `Cell`) when nothing else can still reach it, the
// way `isolate { }` proves it for a block (Swift's region-based isolation,
// SE-0414, in the small).
//
// The value is moved into the task, from a local `x` (directly, or through
// the hidden local `go f(x)` makes). It is accepted when, in the function:
//
// - `x` is a `let` or `var` of the function, not a parameter (the caller may
//   hold other references) nor a reference;
// - everything ever stored in `x` is fresh: it is built only from values that
//   can cross tasks, from locals that are themselves moved in whole and pass
//   these same rules, and from reads that give `send` values. Package-level
//   values are `send`, so a call whose arguments are fresh returns a fresh
//   value: nothing outside it can point into it;
// - every other use of `x` leaves no reference to what it holds: a read of a
//   `send` part (`x.value`, `x.items.len()`), or `x` passed by reference to a
//   call whose other arguments are fresh and whose result is `send` (the call
//   can only connect `x` to itself and to what it makes). No closure other
//   than the task's captures it.
//
// Anything else keeps the error: `isolate { }` remains for what this can't
// tell.

use super::Checker;
use crate::diag::{Diag, Span};
use crate::tir::*;
use std::collections::{HashMap, HashSet};

/// The address of an expression node (they don't move while this runs).
fn addr(e: &TExpr) -> usize {
    e as *const TExpr as usize
}

/// The local a place chain starts from, and the node naming it (through
/// view coercions: `xs.len()` lends `xs` as a slice).
fn place_root(e: &TExpr) -> Option<&TExpr> {
    match &e.kind {
        TK::Local(_) => Some(e),
        TK::Coerce(Coercion::VecToSlice | Coercion::StringToStr | Coercion::ArrayToSlice, x) => place_root(x),
        TK::Field { base, .. } | TK::StaticField { base, .. } | TK::Deref(base) | TK::OptField { base, .. } => place_root(base),
        TK::Index { base, .. } => place_root(base),
        _ => None,
    }
}

struct Facts<'a> {
    tf: &'a TFunc,
    /// `send` by local.
    send: Vec<bool>,
    /// Every `Local` node, by local.
    mentions: HashMap<LocalId, Vec<&'a TExpr>>,
    /// `Local` nodes whose use leaves no reference behind.
    harmless: HashSet<usize>,
    /// What is stored into each local: `let` initializers and whole
    /// assignments (whose target node is harmless).
    defs: HashMap<LocalId, Vec<&'a TExpr>>,
    /// Locals given values some other way (patterns, loops, `with`).
    bound: HashSet<LocalId>,
    /// The `Local` nodes inside each closure's body, by closure id.
    in_closure: HashMap<u32, HashSet<usize>>,
}

impl Checker {
    /// Checks the non-`send` values the tasks of `tf` take (`TClosure::sending`).
    pub(super) fn check_isolation(&mut self, tf: &TFunc) {
        let mut spawns: Vec<&TClosure> = vec![];
        walk_block(&tf.body, &mut |n| {
            if let Node::Expr(e) = n {
                if let TK::Spawn { task, .. } = &e.kind {
                    if let TK::Closure(c) = &task.kind {
                        if !c.sending.is_empty() {
                            spawns.push(c);
                        }
                    }
                }
            }
        });
        if spawns.is_empty() {
            return;
        }
        let send: Vec<bool> = tf.locals.iter().map(|l| l.ty.clone()).collect::<Vec<_>>().iter().map(|t| self.is_send(t)).collect();
        let mut f = Facts { tf, send, mentions: HashMap::new(), harmless: HashSet::new(), defs: HashMap::new(), bound: HashSet::new(), in_closure: HashMap::new() };
        f.collect(self);
        for c in spawns {
            let inside = f.in_closure.get(&c.id).cloned().unwrap_or_default();
            for &l in &c.sending {
                if let Err((why, at)) = f.isolated(self, l, &inside) {
                    let local = &tf.locals[l as usize];
                    // `go f(x)` moves `x` into a hidden local: name `x`
                    let (name, lsp) = match f.hidden_origin(l) {
                        Some(x) => (tf.locals[x as usize].name.clone(), tf.locals[x as usize].span),
                        None => (local.name.clone(), local.span),
                    };
                    let s = self.prog.show(&local.ty);
                    let mut d = Diag::new(c.span, format!("a task captures `{}` of type `{}`, which is not safe to share between threads", name, s)).note(Some(lsp), "declared here");
                    d = match at {
                        Some(sp) => d.note(Some(sp), why),
                        None => d.note(None, why),
                    };
                    d = d.note(None, "a task can take a value that isn't `send` only when nothing else can reach it: build it from `send` values and don't share it before the `go`, or build it in `isolate { }`; otherwise use `Arc`, `Mutex` or `send fn`");
                    self.diags.push(d);
                }
            }
        }
    }
}

impl<'a> Facts<'a> {
    fn collect(&mut self, ck: &mut Checker) {
        let tf = self.tf;
        let mut nodes: Vec<&'a TExpr> = vec![];
        let mut stmts: Vec<&'a TStmt> = vec![];
        walk_block(&tf.body, &mut |n| match n {
            Node::Expr(e) => nodes.push(e),
            Node::Stmt(s) => stmts.push(s),
        });
        for s in &stmts {
            match s {
                TStmt::Let { local, init } => self.defs.entry(*local).or_default().push(init),
                TStmt::Assign { place, op: None, value, .. } => {
                    if let TK::Local(l) = place.kind {
                        self.defs.entry(l).or_default().push(value);
                        self.harmless.insert(addr(place));
                    }
                }
                TStmt::LetRef { local, .. } | TStmt::With { local, .. } => {
                    self.bound.insert(*local);
                }
                TStmt::For { elem, index, .. } => {
                    self.bound.insert(*elem);
                    if let Some(i) = index {
                        self.bound.insert(*i);
                    }
                }
                TStmt::WithLocks { locals, .. } => self.bound.extend(locals.iter().copied()),
                _ => {}
            }
        }
        for &e in &nodes {
            match &e.kind {
                TK::Local(l) => self.mentions.entry(*l).or_default().push(e),
                TK::Closure(c) => {
                    let mut set = HashSet::new();
                    walk_block(&c.body, &mut |n| {
                        if let Node::Expr(x) = n {
                            if matches!(x.kind, TK::Local(_)) {
                                set.insert(addr(x));
                            }
                        }
                    });
                    self.in_closure.insert(c.id, set);
                    self.bound.extend(c.params.iter().copied());
                }
                TK::Match { arms, .. } => {
                    for a in arms {
                        self.bound.extend(a.binds.iter().copied());
                    }
                }
                TK::Catch { bind: Some(b), .. } => {
                    self.bound.insert(*b);
                }
                _ => {}
            }
        }
        // a read that gives a `send` value
        for &e in &nodes {
            if let Some(root) = place_root(e) {
                if !matches!(e.kind, TK::Local(_)) && ck.is_send(&e.ty) {
                    self.harmless.insert(addr(root));
                }
            }
        }
        // a borrow by a call that can't connect it to anything outside
        for &e in &nodes {
            let args: Vec<(&ArgMode, &TExpr)> = match &e.kind {
                TK::Call { args, .. } | TK::Intrinsic { args, .. } => args.iter().map(|a| (&a.mode, &a.e)).collect(),
                _ => continue,
            };
            if !ck.is_send(&e.ty) {
                continue;
            }
            for (i, (mode, a)) in args.iter().enumerate() {
                if matches!(mode, ArgMode::Value) {
                    continue;
                }
                let Some(root) = place_root(a) else { continue };
                if args.iter().enumerate().all(|(j, (_, b))| j == i || self.plain(b)) {
                    self.harmless.insert(addr(root));
                }
            }
        }
    }

    /// No local that isn't `send` in `e`, except in reads that give `send`
    /// values.
    fn plain(&self, e: &TExpr) -> bool {
        let mut ok = true;
        visit_expr(e, &mut |x| {
            if let TK::Local(l) = x.kind {
                if !self.send[l as usize] && !self.harmless.contains(&addr(x)) {
                    ok = false;
                }
            }
            if let TK::Closure(c) = &x.kind {
                // a closure value that captures what isn't `send` is itself
                // a reference to it
                if c.captures.iter().any(|(l, _)| !self.send[*l as usize]) {
                    ok = false;
                }
            }
        });
        ok
    }

    /// The local `go f(x)` moved into this hidden one.
    fn hidden_origin(&self, l: LocalId) -> Option<LocalId> {
        if !self.tf.locals[l as usize].name.starts_with("__go") {
            return None;
        }
        let defs = self.defs.get(&l)?;
        match defs.as_slice() {
            [d] => match &d.kind {
                TK::Move(x) | TK::Copy(x) => match x.kind {
                    TK::Local(o) => Some(o),
                    _ => None,
                },
                TK::Local(o) => Some(*o),
                _ => None,
            },
            _ => None,
        }
    }

    /// Whether the task (whose body holds the `Local` nodes `inside`) may take
    /// `l`; why not, and where.
    fn isolated(&self, ck: &mut Checker, l: LocalId, inside: &HashSet<usize>) -> Result<(), (String, Option<Span>)> {
        let mut seen = HashSet::new();
        match self.hidden_origin(l) {
            Some(x) => {
                // the hidden local is used only by the task
                let d = self.defs[&l][0];
                let moved = match &d.kind {
                    TK::Move(inner) | TK::Copy(inner) => addr(inner),
                    _ => addr(d),
                };
                self.only_task(l, inside)?;
                self.alone(ck, x, Some(moved), inside, &mut seen)
            }
            None => self.alone(ck, l, None, inside, &mut seen),
        }
    }

    fn only_task(&self, l: LocalId, inside: &HashSet<usize>) -> Result<(), (String, Option<Span>)> {
        for m in self.mentions.get(&l).map(|v| v.as_slice()).unwrap_or(&[]) {
            if !inside.contains(&addr(m)) {
                return Err((format!("`{}` is also used here", self.tf.locals[l as usize].name), Some(m.span)));
            }
        }
        Ok(())
    }

    /// Nothing but `moved` (the node that moves it out) and the task's body
    /// (`inside`) can take a reference to what `x` holds, and all of it is
    /// fresh.
    fn alone(&self, ck: &mut Checker, x: LocalId, moved: Option<usize>, inside: &HashSet<usize>, seen: &mut HashSet<LocalId>) -> Result<(), (String, Option<Span>)> {
        let loc = &self.tf.locals[x as usize];
        if !seen.insert(x) {
            return Err((format!("`{}` is built from itself", loc.name), None));
        }
        match loc.kind {
            LocalKind::Param(_) => return Err((format!("`{}` is a parameter: the caller may still reach what it holds", loc.name), Some(loc.span))),
            LocalKind::Ref { .. } => return Err((format!("`{}` is a reference to another value", loc.name), Some(loc.span))),
            LocalKind::Let | LocalKind::Var => {}
        }
        if self.bound.contains(&x) {
            return Err((format!("`{}` is bound by a pattern or a loop, to a part of another value", loc.name), Some(loc.span)));
        }
        let defs = self.defs.get(&x).cloned().unwrap_or_default();
        if defs.is_empty() {
            return Err((format!("`{}` gets its value where this can't follow", loc.name), Some(loc.span)));
        }
        for d in defs {
            self.fresh(ck, d, inside, seen)?;
        }
        for m in self.mentions.get(&x).map(|v| v.as_slice()).unwrap_or(&[]) {
            let a = addr(m);
            if Some(a) == moved || inside.contains(&a) || self.harmless.contains(&a) && !self.in_other_closure(a, inside) {
                continue;
            }
            if self.in_other_closure(a, inside) {
                return Err((format!("`{}` is used here, in a closure (the body of a loop over a generator or a type's `each` is one), which holds a reference to it", loc.name), Some(m.span)));
            }
            return Err((format!("`{}` is used here, which may leave another reference to what it holds", loc.name), Some(m.span)));
        }
        Ok(())
    }

    fn in_other_closure(&self, a: usize, inside: &HashSet<usize>) -> bool {
        !inside.contains(&a) && self.in_closure.values().any(|s| s.contains(&a))
    }

    /// `e` makes a value nothing outside points into.
    fn fresh(&self, ck: &mut Checker, e: &TExpr, inside: &HashSet<usize>, seen: &mut HashSet<LocalId>) -> Result<(), (String, Option<Span>)> {
        // locals moved in whole: fresh if they pass the same rules
        let mut moved: Vec<(LocalId, usize)> = vec![];
        let mut bad: Option<(String, Option<Span>)> = None;
        visit_expr(e, &mut |x| {
            if bad.is_some() {
                return;
            }
            match &x.kind {
                TK::Move(inner) => {
                    if let TK::Local(m) = inner.kind {
                        if !self.send[m as usize] {
                            moved.push((m, addr(inner)));
                        }
                    }
                }
                TK::Local(m) => {
                    let a = addr(x);
                    if !self.send[*m as usize] && !(self.harmless.contains(&a) && !self.in_other_closure(a, inside)) && !moved.iter().any(|(_, ma)| *ma == a) {
                        bad = Some((format!("it is made from `{}`, which other code may still reach", self.tf.locals[*m as usize].name), Some(x.span)));
                    }
                }
                TK::Closure(c) => {
                    if c.captures.iter().any(|(l, _)| !self.send[*l as usize]) {
                        bad = Some(("it holds a closure that captures a value that isn't `send`".to_string(), Some(x.span)));
                    }
                }
                _ => {}
            }
        });
        if let Some(b) = bad {
            return Err(b);
        }
        for (m, a) in moved {
            self.alone(ck, m, Some(a), inside, seen)?;
        }
        Ok(())
    }
}
