// Lazy iterator chains: `xs.iter()` followed by adapters (`map`, `filter`,
// `filterMap`, `flatMap`, `take`, `skip`, `takeWhile`, `skipWhile`,
// `stepBy`), consumed by a `for` loop or ended by a terminal method
// (`collect`, `count`, `sum`, `min`, `max`, `any`, `all`, `find`, `first`,
// `last`, `fold`, `join`).
//
// A chain is never a value: it is rewritten into one loop over `xs`, each
// adapter a few statements inside it, before it is checked. Nothing is
// allocated and nothing is stored, so the closures are ordinary arguments
// (of `__lazyApply` and friends in the prelude) and may borrow views, as the
// closures given to `Vec.map` may.
//
//   for x in xs.iter().filter(p).map(f).take(3) { body }
//
// becomes
//
//   let __n = 3
//   var __t = 0
//   __lz: for __e in xs {
//       if __t >= __n { break __lz }
//       if !__lazyApply(__e, p) { continue }
//       let __v = __lazyApply(__e, f)
//       __t += 1
//       let x = __v
//       body
//   }
//
// `flatMap` nests a loop. A `break` in the body then leaves the outermost
// one, and `continue label` goes on with the innermost (`FnCx::lazy_loops`).
//
// A `Map`'s entries are pairs: the current element is then a key and a
// value, the closures take both (`(k, v) => ...`, as `Map.retain` does), and
// it stays a pair until `map`, `filterMap` or `flatMap` makes one value of it.
// `collect` builds a `Map` from pairs; `find`, `first` and `last` give
// `?(K, V)`.

use std::rc::Rc;

use super::body::{Expect, FnCx};
use super::Checker;
use crate::ast::{self, Block, Expr, ExprKind, ForLoop, Pattern, PatternKind, Stmt, StmtKind};
use crate::diag::Span;
use crate::tir::*;
use crate::types::*;

const ADAPTERS: &[&str] = &["map", "filter", "filterMap", "flatMap", "take", "skip", "takeWhile", "skipWhile", "stepBy"];
const TERMINALS: &[&str] = &["collect", "count", "sum", "min", "max", "any", "all", "find", "first", "last", "fold", "join"];

pub const CONSUME_NOTE: &str = "`.iter()` starts a lazy chain: loop over it (`for x in xs.iter().map(f) { }`) or end it with `collect()`, `count()`, `sum()`, `min()`, `max()`, `any(p)`, `all(p)`, `find(p)`, `first()`, `last()`, `fold(init, f)` or `join(sep)`";

struct Step<'a> {
    name: &'a str,
    args: &'a [Expr],
    span: Span,
}

struct Chain<'a> {
    src: &'a Expr,
    steps: Vec<Step<'a>>,
    /// the source is a `Map`: its elements are (key, value) pairs
    pair: bool,
}

impl Chain<'_> {
    /// Whether the chain still yields pairs at its end.
    fn ends_in_pairs(&self) -> bool {
        self.pair && !self.steps.iter().any(|s| matches!(s.name, "map" | "filterMap" | "flatMap"))
    }
}

/// `src.iter().a(..).b(..)`, outermost call last.
fn parse_chain(e: &Expr) -> Option<Chain<'_>> {
    let mut steps = vec![];
    let mut cur = e;
    loop {
        let ExprKind::Call(f, args) = &cur.kind else { return None };
        let ExprKind::Field(base, name, nsp) = &f.kind else { return None };
        if name == "iter" && args.is_empty() {
            steps.reverse();
            return Some(Chain { src: base, steps, pair: false });
        }
        if !ADAPTERS.contains(&name.as_str()) {
            return None;
        }
        steps.push(Step { name: name.as_str(), args: args.as_slice(), span: *nsp });
        cur = base;
    }
}

/// Builds the rewritten AST.
struct Gen {
    sp: Span,
    n: u32,
    /// statements before the outermost loop: counters, accumulators, values
    /// computed once
    hoisted: Vec<Stmt>,
    outer: String,
    inner: String,
    /// `take`'s stops (`if t >= n { break }`), in chain order
    stops: Vec<Stmt>,
    errors: Vec<(Span, String)>,
}

impl Gen {
    fn fresh(&mut self, what: &str) -> String {
        self.n += 1;
        format!("__lz{}{}", what, self.n)
    }
    fn e(&self, k: ExprKind) -> Expr {
        Expr { kind: k, span: self.sp }
    }
    fn s(&self, k: StmtKind) -> Stmt {
        Stmt { kind: k, span: self.sp }
    }
    fn ident(&self, n: &str) -> Expr {
        self.e(ExprKind::Ident(n.to_string()))
    }
    fn int(&self, v: u128) -> Expr {
        self.e(ExprKind::Int(v))
    }
    fn call(&self, f: &str, args: Vec<Expr>) -> Expr {
        self.e(ExprKind::Call(Box::new(self.ident(f)), args))
    }
    fn method(&self, recv: Expr, m: &str, args: Vec<Expr>) -> Expr {
        self.e(ExprKind::Call(Box::new(self.e(ExprKind::Field(Box::new(recv), m.to_string(), self.sp))), args))
    }
    fn bin(&self, op: &'static str, a: Expr, b: Expr) -> Expr {
        self.e(ExprKind::Binary(op, Box::new(a), Box::new(b)))
    }
    fn not(&self, a: Expr) -> Expr {
        self.e(ExprKind::Unary("!", Box::new(a)))
    }
    fn refm(&self, a: Expr) -> Expr {
        self.e(ExprKind::Ref(Box::new(a)))
    }
    fn let_(&self, name: &str, init: Expr, mutable: bool) -> Stmt {
        self.s(StmtKind::Let { name: name.to_string(), name_span: self.sp, ty: None, init: Some(init), mutable })
    }
    fn assign(&self, name: &str, op: &'static str, v: Expr) -> Stmt {
        self.s(StmtKind::Assign { place: self.ident(name), op, value: v })
    }
    fn if_(&self, cond: Expr, then: Vec<Stmt>) -> Stmt {
        self.s(StmtKind::Expr(self.e(ExprKind::If { cond: Box::new(cond), then: Block { stmts: then, span: self.sp }, else_: None })))
    }
    fn brk(&self) -> Stmt {
        self.s(StmtKind::Break(Some((self.outer.clone(), self.sp))))
    }
    fn cont(&self) -> Stmt {
        self.s(StmtKind::Continue(None))
    }
    /// A hoisted `let`: evaluated once, before the loop.
    fn hoist(&mut self, what: &str, init: Expr, mutable: bool) -> Expr {
        let n = self.fresh(what);
        let st = self.let_(&n, init, mutable);
        self.hoisted.push(st);
        self.ident(&n)
    }
    /// A function argument: closures and paths are written where they're
    /// used (so a closure borrows, rather than owns, what it captures);
    /// anything else is computed once.
    fn func_arg(&mut self, a: &Expr) -> Expr {
        fn path(x: &Expr) -> bool {
            match &x.kind {
                ExprKind::Ident(_) => true,
                ExprKind::Field(b, _, _) => path(b),
                _ => false,
            }
        }
        if matches!(a.kind, ExprKind::Closure { .. }) || path(a) {
            a.clone()
        } else {
            self.hoist("f", a.clone(), false)
        }
    }
    /// `f` applied to the current element: one value, or a key and a value.
    fn apply(&mut self, cur: &[Expr], f: &Expr, what: &str) -> Expr {
        if cur.len() == 2 {
            if let ExprKind::Closure { params, .. } = &f.kind {
                if params.len() != 2 {
                    self.errors.push((f.span, format!("a Map's entries are a key and a value: `{}((k, v) => ...)`", what)));
                }
            }
            let f = self.func_arg(f);
            return self.call("__lazyApply2", vec![cur[0].clone(), cur[1].clone(), f]);
        }
        let f = self.func_arg(f);
        self.call("__lazyApply", vec![cur[0].clone(), f])
    }

    /// `label: for <elem> in iter { stops; steps...; sink; stops }`. A
    /// `take` further on stops every loop around it before it takes its next
    /// element (so that nothing is computed for it), and again after the
    /// element that reached the count.
    fn loop_over(&mut self, iter: Expr, pair: bool, steps: &[Step], sink: &mut dyn FnMut(&mut Gen, Vec<Expr>) -> Vec<Stmt>, label: String) -> Stmt {
        self.inner = label.clone();
        let el = self.fresh("e");
        let mut cur = vec![self.ident(&el)];
        // `for k, v in m`
        let second = pair.then(|| {
            let v = self.fresh("e");
            cur.push(self.ident(&v));
            (v, self.sp)
        });
        let start = self.stops.len();
        let rest = self.steps(steps, cur, sink);
        let mut body: Vec<Stmt> = self.stops[start..].to_vec();
        body.extend(rest);
        let lp = ForLoop::In { first: (el, self.sp), second, iter, by_ref: false, body: Block { stmts: body, span: self.sp } };
        self.s(StmtKind::For(lp, Some((label, self.sp))))
    }

    /// The statements for `steps` applied to the current element `cur`, then
    /// `sink`.
    fn steps(&mut self, steps: &[Step], cur: Vec<Expr>, sink: &mut dyn FnMut(&mut Gen, Vec<Expr>) -> Vec<Stmt>) -> Vec<Stmt> {
        let Some((st, rest)) = steps.split_first() else {
            let mut out = sink(self, cur);
            out.extend(self.stops.iter().cloned());
            return out;
        };
        if st.args.len() != 1 {
            self.errors.push((st.span, format!("`{}` takes one argument, not {}", st.name, st.args.len())));
            return vec![];
        }
        let a = &st.args[0];
        let mut out = vec![];
        match st.name {
            "map" => {
                let v = self.fresh("v");
                let app = self.apply(&cur, a, "map");
                out.push(self.let_(&v, app, false));
                let next = vec![self.ident(&v)];
                out.extend(self.steps(rest, next, sink));
            }
            "filter" => {
                let app = self.apply(&cur, a, "filter");
                out.push(self.if_(self.not(app), vec![self.cont()]));
                out.extend(self.steps(rest, cur, sink));
            }
            "filterMap" => {
                let v = self.fresh("v");
                let app = self.apply(&cur, a, "filterMap");
                let next = vec![self.ident(&v)];
                let then = self.steps(rest, next, sink);
                out.push(self.s(StmtKind::Expr(self.e(ExprKind::IfLet { pat: Pattern { kind: PatternKind::Ident(v), span: self.sp }, expr: Box::new(app), then: Block { stmts: then, span: self.sp }, else_: None }))));
            }
            "flatMap" => {
                let app = self.apply(&cur, a, "flatMap");
                let label = self.fresh("in");
                out.push(self.loop_over(app, false, rest, sink, label));
            }
            "take" => {
                let n = self.hoist("n", a.clone(), false);
                let t = self.fresh("t");
                let zero = self.let_(&t, self.int(0), true);
                self.hoisted.push(zero);
                let stop = self.if_(self.bin(">=", self.ident(&t), n), vec![self.brk()]);
                self.stops.push(stop);
                out.push(self.assign(&t, "+=", self.int(1)));
                out.extend(self.steps(rest, cur, sink));
            }
            "skip" => {
                let n = self.hoist("n", a.clone(), false);
                let s = self.fresh("s");
                let zero = self.let_(&s, self.int(0), true);
                self.hoisted.push(zero);
                out.push(self.if_(self.bin("<", self.ident(&s), n), vec![self.assign(&s, "+=", self.int(1)), self.cont()]));
                out.extend(self.steps(rest, cur, sink));
            }
            "takeWhile" => {
                let app = self.apply(&cur, a, "takeWhile");
                out.push(self.if_(self.not(app), vec![self.brk()]));
                out.extend(self.steps(rest, cur, sink));
            }
            "skipWhile" => {
                let d = self.fresh("d");
                let st0 = self.let_(&d, self.e(ExprKind::Bool(false)), true);
                self.hoisted.push(st0);
                let app = self.apply(&cur, a, "skipWhile");
                let inner = vec![self.if_(app, vec![self.cont()]), self.assign(&d, "=", self.e(ExprKind::Bool(true)))];
                out.push(self.if_(self.not(self.ident(&d)), inner));
                out.extend(self.steps(rest, cur, sink));
            }
            "stepBy" => {
                let checked = self.call("__lazyStep", vec![a.clone()]);
                let n = self.hoist("n", checked, false);
                let c = self.fresh("c");
                let zero = self.let_(&c, self.int(0), true);
                self.hoisted.push(zero);
                // elements 0, n, 2n...: `c` counts them
                let off = self.bin("%", self.ident(&c), n);
                out.push(self.if_(self.bin("!=", off, self.int(0)), vec![self.assign(&c, "+=", self.int(1)), self.cont()]));
                out.push(self.assign(&c, "+=", self.int(1)));
                out.extend(self.steps(rest, cur, sink));
            }
            _ => unreachable!(),
        }
        out
    }
}

impl Checker {
    /// Whether `e` is a lazy chain: `.iter()` here is ours only when the
    /// source has no `iter` method of its own.
    fn lazy_chain<'a>(&mut self, cx: &mut FnCx, e: &'a Expr) -> Option<Chain<'a>> {
        let mut ch = parse_chain(e)?;
        let before = self.diags.len();
        let t = self.expr(cx, ch.src, None);
        let ty = self.resolve(cx, &t.ty);
        self.diags.truncate(before);
        ch.pair = matches!(ty, Ty::Map(..));
        if let Some(k) = TyKey::of(&ty) {
            if self.prog.methods.contains_key(&(k, "iter".to_string())) {
                return None;
            }
        }
        Some(ch)
    }

    fn lazy_gen(&mut self, cx: &mut FnCx, sp: Span) -> Gen {
        cx.hidden += 1;
        let base = cx.hidden * 1000;
        Gen { sp, n: base, hoisted: vec![], outer: String::new(), inner: String::new(), stops: vec![], errors: vec![] }
    }

    /// Checks the rewritten block; `user` is the label of the user's loop.
    fn lazy_check(&mut self, cx: &mut FnCx, g: Gen, lp: Stmt, tail: Option<Expr>, user: Option<String>, expect: Option<&Ty>, sp: Span) -> TExpr {
        for (s, m) in &g.errors {
            self.err(*s, m.clone());
        }
        if !g.errors.is_empty() {
            return TExpr::error(sp);
        }
        let mut stmts = g.hoisted;
        stmts.push(lp);
        let has_tail = tail.is_some();
        if let Some(t) = tail {
            stmts.push(Stmt { kind: StmtKind::Expr(t), span: sp });
        }
        let nested = g.inner != g.outer;
        if nested {
            cx.lazy_loops.push((g.inner.clone(), g.outer.clone(), user));
        }
        let blk = Block { stmts, span: sp };
        let r = if has_tail {
            self.expr(cx, &Expr { kind: ExprKind::Block(blk), span: sp }, expect)
        } else {
            let tb = self.block(cx, &blk, Expect::NoValue);
            TExpr::new(TK::Block(tb), Ty::Unit, sp)
        };
        if nested {
            cx.lazy_loops.pop();
        }
        r
    }

    /// `for x in chain { }` / `for i, x in chain { }`.
    #[allow(clippy::too_many_arguments)]
    pub fn lazy_for(&mut self, cx: &mut FnCx, first: &(String, Span), second: Option<&(String, Span)>, iter: &Expr, by_ref: bool, body: &ast::Block, sp: Span) -> Option<Option<TStmt>> {
        let ch = self.lazy_chain(cx, iter)?;
        let user = cx.pending_label.take();
        if by_ref {
            self.err(iter.span, "a lazy chain can't be iterated mutably: its elements may be computed");
            return Some(None);
        }
        let mut g = self.lazy_gen(cx, sp);
        g.outer = user.clone().unwrap_or_else(|| g.fresh("out"));
        let (index, elem) = match second {
            Some(s2) => (Some(first.clone()), s2.clone()),
            None => (None, first.clone()),
        };
        let counter = index.as_ref().filter(|_| !ch.ends_in_pairs()).map(|_| {
            let c = g.fresh("i");
            let st = g.let_(&c, g.int(0), true);
            g.hoisted.push(st);
            c
        });
        let body_stmts = body.stmts.clone();
        let pairs = ch.ends_in_pairs();
        if pairs && second.is_none() {
            self.err(first.1, "a Map's entries are a key and a value: `for k, v in ...`");
            return Some(None);
        }
        let mut sink = |g: &mut Gen, cur: Vec<Expr>| {
            let bind = |name: &(String, Span), init: Expr| Stmt { kind: StmtKind::Let { name: name.0.clone(), name_span: name.1, ty: None, init: Some(init), mutable: false }, span: name.1 };
            if cur.len() == 2 {
                let mut out = vec![bind(index.as_ref().unwrap(), cur[0].clone()), bind(&elem, cur[1].clone())];
                out.extend(body_stmts.iter().cloned());
                return out;
            }
            let mut out = vec![bind(&elem, cur[0].clone())];
            if let (Some((iv, isp)), Some(c)) = (&index, &counter) {
                out.push(Stmt { kind: StmtKind::Let { name: iv.clone(), name_span: *isp, ty: None, init: Some(g.ident(c)), mutable: false }, span: *isp });
                out.push(g.assign(c, "+=", g.int(1)));
            }
            out.extend(body_stmts.iter().cloned());
            out
        };
        let outer = g.outer.clone();
        let lp = g.loop_over(ch.src.clone(), ch.pair, &ch.steps, &mut sink, outer);
        let r = self.lazy_check(cx, g, lp, None, user, None, sp);
        Some(Some(TStmt::Expr(r)))
    }

    /// `chain.collect()` and the other terminal methods.
    pub fn lazy_terminal(&mut self, cx: &mut FnCx, base: &Expr, name: &str, args: &[Expr], sp: Span, expect: Option<&Ty>) -> Option<TExpr> {
        if !TERMINALS.contains(&name) {
            return None;
        }
        let ch = self.lazy_chain(cx, base)?;
        let mut g = self.lazy_gen(cx, sp);
        g.outer = g.fresh("out");
        let want = match name {
            "any" | "all" | "find" | "join" => 1,
            "fold" => 2,
            _ => 0,
        };
        if args.len() != want {
            self.err(sp, format!("`{}` takes {} argument(s), not {}", name, want, args.len()));
            return Some(TExpr::error(sp));
        }
        let pairs = ch.ends_in_pairs();
        if pairs && matches!(name, "sum" | "min" | "max" | "join") {
            self.err(sp, format!("`{}` needs one value per element, and a Map's entries are a key and a value: pick one first, as in `.map((k, v) => v).{}`", name, if name == "join" { "join(sep)".to_string() } else { format!("{}()", name) }));
            return Some(TExpr::error(sp));
        }
        let r = g.fresh("r");
        let init = match name {
            "collect" if pairs => g.call("__lazyMap", vec![]),
            "collect" => g.call("__lazyVec", vec![]),
            "sum" => g.call("__lazyZero", vec![]),
            "count" => g.int(0),
            "any" => g.e(ExprKind::Bool(false)),
            "all" => g.e(ExprKind::Bool(true)),
            "fold" => args[0].clone(),
            "join" => g.call("String", vec![g.e(ExprKind::Str(String::new()))]),
            _ => g.e(ExprKind::None),
        };
        let st = g.let_(&r, init, true);
        g.hoisted.push(st);
        let (sep, first) = if name == "join" {
            let s = g.hoist("sep", args[0].clone(), false);
            let f = g.hoist("first", g.e(ExprKind::Bool(true)), true);
            (Some(s), Some(f))
        } else {
            (None, None)
        };
        let name = name.to_string();
        let args = args.to_vec();
        let rn = r.clone();
        let mut sink = move |g: &mut Gen, cur: Vec<Expr>| -> Vec<Stmt> {
            // the element as a value of its own: a pair becomes a tuple
            let own = |g: &Gen, c: &[Expr]| {
                let o = |x: &Expr| g.call("__lazyOwn", vec![x.clone()]);
                if c.len() == 2 {
                    g.e(ExprKind::Tuple(vec![o(&c[0]), o(&c[1])]))
                } else {
                    o(&c[0])
                }
            };
            match name.as_str() {
                "collect" if cur.len() == 2 => {
                    let ins = g.method(g.ident(&rn), "insert", vec![g.call("__lazyOwn", vec![cur[0].clone()]), g.call("__lazyOwn", vec![cur[1].clone()])]);
                    vec![g.s(StmtKind::Discard(ins))]
                }
                "collect" => vec![g.s(StmtKind::Expr(g.method(g.ident(&rn), "push", vec![own(g, &cur)])))],
                "count" => vec![g.assign(&rn, "+=", g.int(1))],
                "sum" | "min" | "max" => {
                    let f = match name.as_str() {
                        "sum" => "__lazyAdd",
                        "min" => "__lazyMin",
                        _ => "__lazyMax",
                    };
                    vec![g.s(StmtKind::Expr(g.call(f, vec![g.refm(g.ident(&rn)), cur[0].clone()])))]
                }
                "any" => {
                    let app = g.apply(&cur, &args[0], "any");
                    vec![g.if_(app, vec![g.assign(&rn, "=", g.e(ExprKind::Bool(true))), g.brk()])]
                }
                "all" => {
                    let app = g.apply(&cur, &args[0], "all");
                    vec![g.if_(g.not(app), vec![g.assign(&rn, "=", g.e(ExprKind::Bool(false))), g.brk()])]
                }
                "find" => {
                    let app = g.apply(&cur, &args[0], "find");
                    let v = g.call("some", vec![own(g, &cur)]);
                    vec![g.if_(app, vec![g.assign(&rn, "=", v), g.brk()])]
                }
                "first" | "last" => {
                    let v = g.call("some", vec![own(g, &cur)]);
                    let mut out = vec![g.assign(&rn, "=", v)];
                    if name == "first" {
                        out.push(g.brk());
                    }
                    out
                }
                "fold" if cur.len() == 2 => {
                    if let ExprKind::Closure { params, .. } = &args[1].kind {
                        if params.len() != 3 {
                            g.errors.push((args[1].span, "a Map's entries are a key and a value: `fold(init, (acc, k, v) => ...)`".into()));
                        }
                    }
                    let f = g.func_arg(&args[1]);
                    vec![g.assign(&rn, "=", g.call("__lazyFold2", vec![g.ident(&rn), cur[0].clone(), cur[1].clone(), f]))]
                }
                "fold" => {
                    let f = g.func_arg(&args[1]);
                    vec![g.assign(&rn, "=", g.call("__lazyFold", vec![g.ident(&rn), cur[0].clone(), f]))]
                }
                "join" => vec![g.s(StmtKind::Expr(g.call("__lazyJoin", vec![g.refm(g.ident(&rn)), cur[0].clone(), sep.clone().unwrap(), g.refm(first.clone().unwrap())])))],
                _ => unreachable!(),
            }
        };
        let outer = g.outer.clone();
        let lp = g.loop_over(ch.src.clone(), ch.pair, &ch.steps, &mut sink, outer);
        let tail = g.ident(&r);
        Some(self.lazy_check(cx, g, lp, Some(tail), None, expect, sp))
    }

    /// `__lazyVec()`: an empty `Vec` whose element type is inferred.
    /// `__lazyMap()`: an empty `Map` whose key and value types are inferred.
    /// `__lazyZero()`: zero, of a number type that is inferred.
    /// `__lazyOwn(x)`: `x` itself, or a clone when it is only borrowed.
    pub fn lazy_intrinsic(&mut self, cx: &mut FnCx, name: &str, args: &[Expr], sp: Span, expect: Option<&Ty>) -> Option<TExpr> {
        match (name, args) {
            ("__lazyVec", []) => {
                let t = self.fresh(cx);
                Some(TExpr::new(TK::VecLit(vec![]), Ty::vec(t), sp))
            }
            ("__lazyMap", []) => {
                let (k, v) = (self.fresh(cx), self.fresh(cx));
                Some(TExpr::new(TK::MapLit(vec![]), Ty::Map(Rc::new(k), Rc::new(v)), sp))
            }
            ("__lazyZero", []) => {
                let t = self.fresh(cx);
                Some(TExpr::new(TK::DefaultOf, t, sp))
            }
            ("__lazyOwn", [a]) => {
                let e = self.expr(cx, a, expect);
                let ty = self.resolve(cx, &e.ty);
                if !self.is_copy(&ty) && self.place_info(cx, &e).is_some_and(|i| !i.owned) {
                    return Some(self.method_call_on(cx, e, "clone", sp, None, &[], sp, expect));
                }
                Some(self.value(cx, e))
            }
            _ => None,
        }
    }
}
