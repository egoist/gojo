// Patterns, `match`, `if let` and exhaustiveness.

use super::body::{Expect, FnCx, PathRes};
use super::Checker;
use crate::ast::{self, Expr, ExprKind, Pattern, PatternKind};
use crate::diag::{Diag, Span};
use crate::tir::*;
use crate::types::*;

#[derive(Clone, Copy)]
pub struct BindMode {
    pub mutable: bool,
    pub arm: Option<u32>,
    pub root: Option<LocalId>,
}

impl Checker {
    fn scrutinee(&mut self, cx: &mut FnCx, scrut: &Expr) -> (TExpr, bool, bool) {
        let s = self.expr(cx, scrut, None);
        if let TK::Proj { opt: true, mutable, .. } = &s.kind {
            let m = *mutable;
            return (s, false, m);
        }
        if self.is_borrowed_rvalue(cx, &s) {
            return (s, false, false);
        }
        if s.is_place() {
            let root_info = self.place_owned_mut(cx, &s);
            let s = self.borrow(cx, s);
            (s, root_info.0, root_info.1)
        } else {
            (s, true, false)
        }
    }

    fn place_owned_mut(&self, cx: &FnCx, e: &TExpr) -> (bool, bool) {
        // reuse value()'s logic without side effects
        fn walk(c: &Checker, cx: &FnCx, e: &TExpr) -> Option<(bool, bool)> {
            match &e.kind {
                TK::Local(id) => {
                    let l = &cx.locals[*id as usize];
                    Some(match l.kind {
                        LocalKind::Let => (true, false),
                        LocalKind::Var => (true, true),
                        LocalKind::Param(ast::Mode::Borrow) => (false, false),
                        LocalKind::Param(ast::Mode::Inout) => (false, true),
                        LocalKind::Param(ast::Mode::Sink) => (true, true),
                        LocalKind::Ref { mutable } => (false, mutable),
                    })
                }
                TK::Field { base, .. } => {
                    let (o, m) = walk(c, cx, base)?;
                    let has_drop = match &base.ty {
                        Ty::Adt(d, _) => c.prog.def(*d).has_drop,
                        _ => false,
                    };
                    Some((o && !has_drop, m))
                }
                TK::Deref(base) => {
                    let m = match &base.ty {
                        Ty::Box(_) => walk(c, cx, base).map(|x| x.1).unwrap_or(true),
                        Ty::Ptr(_) => true,
                        _ => false,
                    };
                    Some((false, m))
                }
                TK::Index { base, .. } | TK::MapIndex { base, .. } => Some((false, walk(c, cx, base).map(|x| x.1).unwrap_or(true))),
                TK::Proj { mutable, .. } => Some((false, *mutable)),
                TK::ProjUnwrap { proj, .. } => walk(c, cx, proj),
                _ => None,
            }
        }
        walk(self, cx, e).unwrap_or((true, false))
    }

    fn scrut_root(&self, cx: &FnCx, s: &TExpr) -> Option<LocalId> {
        let r = match &s.kind {
            TK::Proj { call, .. } => match &call.kind {
                TK::Call { args, .. } => args.first().and_then(|a| self.place_root(cx, &a.e)),
                _ => None,
            },
            _ => self.place_root(cx, s),
        }?;
        Some(cx.locals[r as usize].ref_root.unwrap_or(r))
    }

    pub fn match_expr(&mut self, cx: &mut FnCx, scrut: &Expr, arms: &[ast::Arm], ex: Expect, sp: Span) -> TExpr {
        let (s, owned, mutable) = self.scrutinee(cx, scrut);
        let sty = self.resolve(cx, &s.ty);
        let mut tarms = vec![];
        let mut result_ty: Option<Ty> = match &ex {
            Expect::Ty(t) => Some(t.clone()),
            _ => None,
        };
        for arm in arms {
            let aid = cx.n_arms;
            cx.n_arms += 1;
            cx.arm_consuming.push(false);
            self.push_scope(cx);
            let mut binds = vec![];
            let bm = BindMode { mutable, arm: if owned && !self.is_copy(&sty) { Some(aid) } else { None }, root: self.scrut_root(cx, &s) };
            let pat = self.check_pat(cx, &arm.pat, &sty, bm, &mut binds);
            let guard = arm.guard.as_ref().map(|g| self.expr_value(cx, g, Some(&Ty::Bool)));
            let body = match &ex {
                Expect::NoValue => {
                    let b = self.expr_x_pub(cx, &arm.body, Expect::NoValue);
                    let t = self.resolve(cx, &b.ty);
                    if matches!(t, Ty::Result(..)) && !matches!(b.kind, TK::If { .. } | TK::Match { .. } | TK::Block(_)) {
                        self.diags.push(Diag::new(b.span, format!("result of type `{}` is not handled", self.prog.show(&t))).note(None, "handle it with `try`, `catch` or `match`, or discard it with `_ = ...`"));
                    }
                    self.borrow(cx, b)
                }
                _ => match &result_ty {
                    Some(t) => {
                        let t = t.clone();
                        let b = self.expr(cx, &arm.body, Some(&t));
                        self.coerce(cx, b, &t, super::body::Use::Value)
                    }
                    None => {
                        let b = self.expr(cx, &arm.body, None);
                        let b = self.value(cx, b);
                        let bt = self.resolve(cx, &b.ty);
                        if bt != Ty::Never {
                            result_ty = Some(bt);
                        }
                        b
                    }
                },
            };
            self.pop_scope(cx);
            let consuming = cx.arm_consuming[aid as usize];
            if consuming {
                for &b in &binds {
                    cx.locals[b as usize].kind = LocalKind::Let;
                }
            }
            for &b in &binds {
                cx.locals[b as usize].match_arm = None;
            }
            tarms.push(TArm { id: aid, pat, guard, body, consuming, binds });
        }
        // exhaustiveness
        if !sty.is_error() {
            let rows: Vec<Vec<TPat>> = tarms.iter().filter(|a| a.guard.is_none()).map(|a| vec![a.pat.clone()]).collect();
            if let Some(w) = self.missing(&rows, &[sty.clone()]) {
                self.diags.push(Diag::new(sp, format!("`match` is not exhaustive: `{}` is not covered", w.join(", "))).note(None, "add an arm for it, or a catch-all `_ => ...`"));
            }
        }
        let ty = match ex {
            Expect::NoValue => Ty::Unit,
            _ => result_ty.unwrap_or(Ty::Never),
        };
        let ty = if tarms.is_empty() { Ty::Never } else { ty };
        TExpr::new(TK::Match { scrut: Box::new(s), owned, arms: tarms }, ty, sp)
    }

    pub fn if_let(&mut self, cx: &mut FnCx, pat: &Pattern, scrut: &Expr, then: &ast::Block, else_: Option<&Expr>, ex: Expect, sp: Span) -> TExpr {
        let (s, owned, mutable) = self.scrutinee(cx, scrut);
        let sty = self.resolve(cx, &s.ty);
        let orig_kind = pat.kind.clone();
        // `if let v = opt` means `if let some(v) = opt`
        let pat = match (&pat.kind, &sty) {
            (PatternKind::Ident(_) | PatternKind::Tuple(_), Ty::Option(_)) => Pattern { kind: PatternKind::Path(vec!["some".into()], Some(vec![pat.clone()])), span: pat.span },
            _ => pat.clone(),
        };
        let aid = cx.n_arms;
        cx.n_arms += 1;
        cx.arm_consuming.push(false);
        self.push_scope(cx);
        let mut binds = vec![];
        let bm = BindMode { mutable, arm: if owned && !self.is_copy(&sty) { Some(aid) } else { None }, root: self.scrut_root(cx, &s) };
        if let (PatternKind::Ident(n), ExprKind::Ident(m)) = (&orig_kind, &scrut.kind) {
            if n == m {
                cx.shadow_ok = Some(n.clone());
            }
        }
        let tp = self.check_pat(cx, &pat, &sty, bm, &mut binds);
        cx.shadow_ok = None;
        let then_b = self.block(cx, then, ex.clone());
        self.pop_scope(cx);
        let consuming = cx.arm_consuming[aid as usize];
        if consuming {
            for &b in &binds {
                cx.locals[b as usize].kind = LocalKind::Let;
            }
        }
        for &b in &binds {
            cx.locals[b as usize].match_arm = None;
        }
        let tt = self.block_ty(&then_b);
        let then_ty = then_b.value.as_ref().map(|v| v.ty.clone());
        let arm1 = TArm { id: aid, pat: tp, guard: None, body: TExpr::new(TK::Block(then_b), tt.clone(), then.span), consuming, binds };
        let aid2 = cx.n_arms;
        cx.n_arms += 1;
        cx.arm_consuming.push(false);
        let (else_e, ty) = match else_ {
            Some(e) => {
                let ex2 = match &ex {
                    Expect::Infer if tt != Ty::Never => Expect::Ty(tt.clone()),
                    o => o.clone(),
                };
                let ee = self.expr_x_pub(cx, e, ex2);
                let et = self.resolve(cx, &ee.ty);
                let ty = if matches!(ex, Expect::NoValue) {
                    Ty::Unit
                } else if tt == Ty::Never {
                    et
                } else {
                    tt
                };
                (ee, ty)
            }
            None => {
                if !matches!(ex, Expect::NoValue) {
                    if let Some(t) = then_ty {
                        if t != Ty::Unit && t != Ty::Never {
                            self.err(sp, "`if let` without `else` cannot produce a value");
                        }
                    }
                }
                (TExpr::unit(sp), Ty::Unit)
            }
        };
        let arm2 = TArm { id: aid2, pat: TPat::Wild, guard: None, body: else_e, consuming: false, binds: vec![] };
        TExpr::new(TK::Match { scrut: Box::new(s), owned, arms: vec![arm1, arm2] }, ty, sp)
    }

    pub fn expr_x_pub(&mut self, cx: &mut FnCx, e: &Expr, ex: Expect) -> TExpr {
        self.expr_x(cx, e, ex)
    }

    fn bind(&mut self, cx: &mut FnCx, name: &str, ty: Ty, bm: BindMode, span: Span, binds: &mut Vec<LocalId>) -> TPat {
        let id = self.declare(cx, name, ty, LocalKind::Ref { mutable: bm.mutable }, span, false);
        cx.locals[id as usize].match_arm = bm.arm;
        cx.locals[id as usize].ref_root = bm.root;
        binds.push(id);
        TPat::Bind(id)
    }

    pub fn check_pat(&mut self, cx: &mut FnCx, p: &Pattern, ty: &Ty, bm: BindMode, binds: &mut Vec<LocalId>) -> TPat {
        let ty = self.resolve(cx, ty);
        if ty.is_error() {
            // still declare bindings to avoid cascading errors
            self.declare_pat_names(cx, p, binds);
            return TPat::Wild;
        }
        let sp = p.span;
        match &p.kind {
            PatternKind::Wild => TPat::Wild,
            PatternKind::Tuple(ps) => {
                let fts = match &ty {
                    Ty::Adt(d, args) if self.prog.def(*d).tuple => self.prog.field_tys(*d, args),
                    _ => vec![],
                };
                if fts.len() != ps.len() {
                    let s = self.prog.show(&ty);
                    self.err(sp, format!("this pattern has {} elements, but the value is a `{}`", ps.len(), s));
                    self.declare_pat_names(cx, p, binds);
                    return TPat::Wild;
                }
                TPat::Tuple(ps.iter().zip(fts).map(|(q, (_, t))| self.check_pat(cx, q, &t, bm, binds)).collect())
            }
            PatternKind::Ident(n) => {
                if let Ty::Adt(d, _) = &ty {
                    if let Some(vi) = self.prog.def(*d).variants().iter().position(|v| v.name == *n) {
                        let nfields = self.prog.def(*d).variants()[vi].fields.len();
                        if nfields > 0 {
                            self.err(sp, format!("variant `{}` has values; match them with `{}(...)`", n, n));
                        }
                        // one sub-pattern per field, as the exhaustiveness check expects
                        return TPat::Variant { def: *d, variant: vi, subs: vec![TPat::Wild; nfields] };
                    }
                }
                self.bind(cx, n, ty, bm, sp, binds)
            }
            PatternKind::DotVariant(n, subs) => self.variant_pat(cx, &ty, n, subs.as_deref(), bm, binds, sp),
            PatternKind::Path(path, subs) => {
                let last = path.last().unwrap().clone();
                if path.len() == 1 {
                    match (last.as_str(), &ty) {
                        ("some", Ty::Option(inner)) => {
                            let inner = (**inner).clone();
                            let sub = self.single_sub(cx, subs.as_deref(), &inner, bm, binds, sp);
                            return TPat::Some(Box::new(sub));
                        }
                        ("ok", Ty::Result(a, _)) => {
                            let a = (**a).clone();
                            let sub = self.single_sub(cx, subs.as_deref(), &a, bm, binds, sp);
                            return TPat::Ok(Box::new(sub));
                        }
                        ("err", Ty::Result(_, b)) => {
                            let b = (**b).clone();
                            let sub = self.single_sub(cx, subs.as_deref(), &b, bm, binds, sp);
                            return TPat::Err(Box::new(sub));
                        }
                        ("some" | "ok" | "err", _) => {
                            let s = self.prog.show(&ty);
                            self.err(sp, format!("pattern `{}(..)` does not match a value of type `{}`", last, s));
                            self.declare_pat_names(cx, p, binds);
                            return TPat::Wild;
                        }
                        _ => return self.variant_pat(cx, &ty, &last, subs.as_deref(), bm, binds, sp),
                    }
                }
                // Type.Variant or pkg.Type.Variant
                let mut e = Expr { kind: ExprKind::Ident(path[0].clone()), span: sp };
                for seg in &path[1..path.len() - 1] {
                    e = Expr { kind: ExprKind::Field(Box::new(e), seg.clone(), sp), span: sp };
                }
                match self.resolve_path(cx, &e) {
                    Some(PathRes::TypeCon(t, gd)) => {
                        let d = match (&t, gd) {
                            (Ty::Adt(d, _), _) => Some(*d),
                            (_, Some(d)) => Some(d),
                            _ => None,
                        };
                        let ok = matches!((&ty, d), (Ty::Adt(d1, _), Some(d2)) if *d1 == d2);
                        if !ok {
                            let s = self.prog.show(&ty);
                            self.err(sp, format!("pattern type does not match the value's type `{}`", s));
                            self.declare_pat_names(cx, p, binds);
                            return TPat::Wild;
                        }
                        self.variant_pat(cx, &ty, &last, subs.as_deref(), bm, binds, sp)
                    }
                    _ => {
                        self.err(sp, "unknown pattern path");
                        self.declare_pat_names(cx, p, binds);
                        TPat::Wild
                    }
                }
            }
            PatternKind::Int(v) => {
                match &ty {
                    Ty::Int(k) => {
                        if *v < k.min() || *v > k.max() {
                            self.err(sp, format!("pattern {} does not fit in `{}`", v, k.name()));
                        }
                    }
                    _ => {
                        let s = self.prog.show(&ty);
                        self.err(sp, format!("integer pattern does not match a value of type `{}`", s));
                    }
                }
                TPat::Int(*v)
            }
            PatternKind::Char(c) => {
                if !ty.is_int() {
                    let s = self.prog.show(&ty);
                    self.err(sp, format!("rune pattern does not match a value of type `{}`", s));
                }
                TPat::Int(*c as i128)
            }
            PatternKind::Range(lo, hi, inc) => {
                match &ty {
                    Ty::Int(k) => {
                        for v in [*lo, *hi] {
                            if v < k.min() || v > k.max() {
                                self.err(sp, format!("pattern bound {} does not fit in `{}`", v, k.name()));
                                return TPat::Wild;
                            }
                        }
                    }
                    t if t.is_int() => {}
                    _ => {
                        let s = self.prog.show(&ty);
                        self.err(sp, format!("range pattern does not match a value of type `{}`", s));
                    }
                }
                TPat::Range(*lo, *hi, *inc)
            }
            PatternKind::Str(s) => {
                if !ty.is_stringish() {
                    let t = self.prog.show(&ty);
                    self.err(sp, format!("string pattern does not match a value of type `{}`", t));
                }
                TPat::Str(s.clone())
            }
            PatternKind::Bool(b) => {
                if ty != Ty::Bool {
                    let t = self.prog.show(&ty);
                    self.err(sp, format!("bool pattern does not match a value of type `{}`", t));
                }
                TPat::Bool(*b)
            }
            PatternKind::None => {
                if !matches!(ty, Ty::Option(_)) {
                    let t = self.prog.show(&ty);
                    self.err(sp, format!("`none` does not match a value of type `{}`", t));
                }
                TPat::None
            }
            PatternKind::Typed(n, te) => {
                let (d, args) = match &ty {
                    Ty::Iface(d, a) => (*d, a.clone()),
                    _ => {
                        let t = self.prog.show(&ty);
                        self.err(sp, format!("type patterns match interface values, not `{}`", t));
                        return TPat::Wild;
                    }
                };
                let env = cx.env.clone();
                let ct = self.resolve_type(te, &env);
                if !ct.is_error() {
                    if let Err(m) = self.implements(&ct, d, &args) {
                        self.err(te.span, format!("this arm can never match: {}", m));
                    }
                }
                let bind = if n == "_" {
                    None
                } else {
                    let id = self.declare(cx, n, ct.clone(), LocalKind::Ref { mutable: bm.mutable }, sp, false);
                    binds.push(id);
                    Some(id)
                };
                TPat::TypeIs { ty: ct, bind }
            }
            PatternKind::Or(alts) => {
                let n = binds.len();
                let ps: Vec<TPat> = alts.iter().map(|a| self.check_pat(cx, a, &ty, bm, binds)).collect();
                if binds.len() > n {
                    self.err(sp, "alternatives (`|`) cannot bind variables");
                }
                TPat::Or(ps)
            }
        }
    }

    fn declare_pat_names(&mut self, cx: &mut FnCx, p: &Pattern, binds: &mut Vec<LocalId>) {
        match &p.kind {
            PatternKind::Ident(n) => {
                let id = self.declare(cx, n, Ty::Error, LocalKind::Let, p.span, false);
                cx.used[id as usize] = true;
                binds.push(id);
            }
            PatternKind::Path(_, Some(s)) | PatternKind::DotVariant(_, Some(s)) | PatternKind::Or(s) | PatternKind::Tuple(s) => {
                for x in s {
                    self.declare_pat_names(cx, x, binds)
                }
            }
            PatternKind::Typed(n, _) => {
                let id = self.declare(cx, n, Ty::Error, LocalKind::Let, p.span, false);
                cx.used[id as usize] = true;
            }
            _ => {}
        }
    }

    fn single_sub(&mut self, cx: &mut FnCx, subs: Option<&[Pattern]>, ty: &Ty, bm: BindMode, binds: &mut Vec<LocalId>, sp: Span) -> TPat {
        match subs {
            Some([one]) => self.check_pat(cx, one, ty, bm, binds),
            _ => {
                self.err(sp, "expected exactly one sub-pattern");
                TPat::Wild
            }
        }
    }

    fn variant_pat(&mut self, cx: &mut FnCx, ty: &Ty, name: &str, subs: Option<&[Pattern]>, bm: BindMode, binds: &mut Vec<LocalId>, sp: Span) -> TPat {
        let (d, args) = match ty {
            Ty::Adt(d, a) if !self.prog.def(*d).is_struct() => (*d, a.clone()),
            _ => {
                let s = self.prog.show(ty);
                self.err(sp, format!("variant pattern `{}` does not match a value of type `{}`", name, s));
                if let Some(ss) = subs {
                    for x in ss {
                        let p = x.clone();
                        self.declare_pat_names(cx, &p, binds);
                    }
                }
                return TPat::Wild;
            }
        };
        let vi = match self.prog.def(d).variants().iter().position(|v| v.name == name) {
            Some(v) => v,
            None => {
                let s = self.prog.show(ty);
                self.err(sp, format!("`{}` has no variant `{}`", s, name));
                if let Some(ss) = subs {
                    for x in ss {
                        let p = x.clone();
                        self.declare_pat_names(cx, &p, binds);
                    }
                }
                return TPat::Wild;
            }
        };
        let ftys = self.prog.variant_tys(d, &args, vi);
        let subs = subs.unwrap_or(&[]);
        if subs.len() != ftys.len() {
            self.err(sp, format!("variant `{}` has {} value(s), the pattern lists {}", name, ftys.len(), subs.len()));
            for x in subs {
                let p = x.clone();
                self.declare_pat_names(cx, &p, binds);
            }
            return TPat::Wild;
        }
        let mut out = vec![];
        for (s, (_, ft)) in subs.iter().zip(ftys.iter()) {
            out.push(self.check_pat(cx, s, ft, bm, binds));
        }
        TPat::Variant { def: d, variant: vi, subs: out }
    }

    // ------------------------------------------------------------------
    // Exhaustiveness (usefulness algorithm)
    // ------------------------------------------------------------------

    fn ctors(&self, ty: &Ty) -> Option<Vec<(Ctor, Vec<Ty>, String)>> {
        match ty {
            Ty::Bool => Some(vec![(Ctor::Bool(true), vec![], "true".into()), (Ctor::Bool(false), vec![], "false".into())]),
            Ty::Option(t) => Some(vec![(Ctor::Some, vec![(**t).clone()], "some".into()), (Ctor::None, vec![], "none".into())]),
            Ty::Result(a, b) => Some(vec![(Ctor::Ok, vec![(**a).clone()], "ok".into()), (Ctor::Err, vec![(**b).clone()], "err".into())]),
            Ty::Adt(d, args) if self.prog.def(*d).tuple => Some(vec![(Ctor::Variant(0), self.prog.field_tys(*d, args).into_iter().map(|f| f.1).collect(), String::new())]),
            Ty::Adt(d, args) if !self.prog.def(*d).is_struct() => {
                let def = self.prog.def(*d);
                Some(def.variants().iter().enumerate().map(|(i, v)| (Ctor::Variant(i), self.prog.variant_tys(*d, args, i).into_iter().map(|f| f.1).collect(), v.name.clone())).collect())
            }
            _ => None,
        }
    }

    pub fn missing(&self, rows: &[Vec<TPat>], tys: &[Ty]) -> Option<Vec<String>> {
        if tys.is_empty() {
            return if rows.is_empty() { Some(vec![]) } else { None };
        }
        // expand or-patterns in the first column
        let mut rows2: Vec<Vec<TPat>> = vec![];
        for r in rows {
            match &r[0] {
                TPat::Or(alts) => {
                    for a in alts {
                        let mut nr = vec![a.clone()];
                        nr.extend(r[1..].iter().cloned());
                        rows2.push(nr);
                    }
                }
                _ => rows2.push(r.clone()),
            }
        }
        let rows = rows2;
        match self.ctors(&tys[0]) {
            Some(all) => {
                let present: Vec<Ctor> = rows.iter().filter_map(|r| head_ctor(&r[0])).collect();
                let missing_c: Vec<&(Ctor, Vec<Ty>, String)> = all.iter().filter(|c| !present.contains(&c.0)).collect();
                if !missing_c.is_empty() {
                    let def: Vec<Vec<TPat>> = rows.iter().filter(|r| head_ctor(&r[0]).is_none()).map(|r| r[1..].to_vec()).collect();
                    let w = self.missing(&def, &tys[1..])?;
                    let c = missing_c[0];
                    let s = if c.1.is_empty() { c.2.clone() } else { format!("{}({})", c.2, vec!["_"; c.1.len()].join(", ")) };
                    let mut out = vec![s];
                    out.extend(w);
                    return Some(out);
                }
                for (c, args, name) in &all {
                    let mut sub = vec![];
                    for r in &rows {
                        match spec(&r[0], c, args.len()) {
                            Some(mut a) => {
                                a.extend(r[1..].iter().cloned());
                                sub.push(a);
                            }
                            None => {}
                        }
                    }
                    let mut stys = args.clone();
                    stys.extend(tys[1..].iter().cloned());
                    if let Some(w) = self.missing(&sub, &stys) {
                        let (a, rest) = w.split_at(args.len());
                        let s = if args.is_empty() { name.clone() } else { format!("{}({})", name, a.join(", ")) };
                        let mut out = vec![s];
                        out.extend(rest.iter().cloned());
                        return Some(out);
                    }
                }
                None
            }
            None => {
                let def: Vec<Vec<TPat>> = rows.iter().filter(|r| matches!(r[0], TPat::Wild | TPat::Bind(_))).map(|r| r[1..].to_vec()).collect();
                let w = self.missing(&def, &tys[1..])?;
                let mut out = vec!["_".to_string()];
                out.extend(w);
                Some(out)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Ctor {
    Bool(bool),
    Some,
    None,
    Ok,
    Err,
    Variant(usize),
}

fn head_ctor(p: &TPat) -> Option<Ctor> {
    match p {
        TPat::Bool(b) => Some(Ctor::Bool(*b)),
        TPat::Some(_) => Some(Ctor::Some),
        TPat::None => Some(Ctor::None),
        TPat::Ok(_) => Some(Ctor::Ok),
        TPat::Err(_) => Some(Ctor::Err),
        TPat::Variant { variant, .. } => Some(Ctor::Variant(*variant)),
        TPat::Tuple(_) => Some(Ctor::Variant(0)),
        _ => None,
    }
}

fn spec(p: &TPat, c: &Ctor, arity: usize) -> Option<Vec<TPat>> {
    match (p, c) {
        (TPat::Wild | TPat::Bind(_), _) => Some(vec![TPat::Wild; arity]),
        (TPat::Bool(a), Ctor::Bool(b)) if a == b => Some(vec![]),
        (TPat::Some(x), Ctor::Some) | (TPat::Ok(x), Ctor::Ok) | (TPat::Err(x), Ctor::Err) => Some(vec![(**x).clone()]),
        (TPat::None, Ctor::None) => Some(vec![]),
        (TPat::Variant { variant, subs, .. }, Ctor::Variant(v)) if variant == v => Some(subs.clone()),
        (TPat::Tuple(subs), Ctor::Variant(0)) => Some(subs.clone()),
        _ => None,
    }
}
