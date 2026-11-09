// Type checking of function bodies into the typed IR.

use super::{bind_params, builtin_type, is_generic_builtin, Checker, TyEnv};
use crate::ast::{self, Expr, ExprKind, Mode, Stmt, StmtKind, YieldKind};
use crate::diag::{Diag, Span};
use crate::tir::*;
use crate::types::*;
use std::collections::HashMap;
use std::rc::Rc;

pub struct Frame {
    pub depth: u32,
    pub captures: Vec<(LocalId, CapUse)>,
    saved_ret: Ty,
    saved_loops: Vec<(Option<String>, u32)>,
    saved_yields: Option<(YieldKind, bool)>,
}

pub struct FnCx {
    pub locals: Vec<Local>,
    pub scopes: Vec<Vec<(String, LocalId)>>,
    pub used: Vec<bool>,
    pub ret: Ty,
    pub yields: Option<(YieldKind, bool)>,
    /// Enclosing loops (label, id), innermost last.
    pub loops: Vec<(Option<String>, u32)>,
    pub next_loop: u32,
    /// Label for the next loop being checked.
    pub pending_label: Option<String>,
    pub frames: Vec<Frame>,
    /// For each enclosing `unsafe` block, innermost last: whether anything in
    /// it needed it. A block that doesn't is an error.
    pub unsafe_used: Vec<bool>,
    pub env: TyEnv,
    pub n_arms: u32,
    pub arm_consuming: Vec<bool>,
    pub vars: Vec<Option<Ty>>,
    pub closure_ids: u32,
    pub hidden: u32,
    /// Enclosing `scope` blocks, innermost last.
    pub task_scopes: Vec<TaskScopeCx>,
    pub next_scope: u32,
    /// Closure depths of `par for` bodies being checked.
    pub par_frames: Vec<u32>,
    /// `with ... lock()` bodies being checked: (closure depth, the lock's name).
    pub locked: Vec<(u32, String)>,
    /// Owner type for the next associated-function call (`Stack[String].new()`).
    pub call_owner: Option<Ty>,
    /// The next closure checked is the body of a push-style `for` loop.
    pub loop_body_next: Option<(u32, Option<String>)>,
    /// Inside `defer` / `errdefer`: the deferred code can't leave the function.
    /// An ordinary closure starts over at 0; a loop body doesn't.
    pub defer_depth: u32,
    /// `if let x { }` / `if let x = x { }`: the binding may reuse the name of
    /// the optional it unwraps.
    pub shadow_ok: Option<String>,
    /// The value expected from the `with` body being checked (`let n = with ...`).
    pub with_value: Option<Expect>,
    /// Lazy chains with `flatMap` being checked (lazy.rs): the labels of
    /// their innermost and outermost loops, and the user's label. A `break`
    /// in the body leaves the outermost; `continue label`, the innermost.
    pub lazy_loops: Vec<(String, String, Option<String>)>,
}

/// A `scope` block being checked.
#[derive(Clone, Debug)]
pub struct TaskScopeCx {
    pub id: u32,
    /// Closure depth of the scope: `go` must be at the same depth.
    pub depth: u32,
    /// Block level of the scope's body: locals declared at this level or
    /// outside live until the scope has waited for its tasks.
    pub block: u32,
    /// The error plain `go` tasks can fail with, and where the first one is.
    pub errt: Option<(Ty, Span)>,
    pub handle: Option<LocalId>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Use {
    Value,
    Borrow,
}

#[derive(Clone, Debug)]
pub enum Expect {
    Infer,
    Ty(Ty),
    NoValue,
}

impl Expect {
    fn ty(&self) -> Option<&Ty> {
        match self {
            Expect::Ty(t) => Some(t),
            _ => None,
        }
    }
}

/// Result of resolving a path-like expression.
#[derive(Clone, Debug)]
pub enum PathRes {
    Pkg(PkgId, String),
    /// A type; for generic ADTs without arguments `args` is None.
    TypeCon(Ty, Option<DefId>),
    Func(FuncId, Option<Vec<Ty>>),
    Const(u32),
    Global(u32),
    Variant(Ty, usize),
    Value,
}

pub(super) struct PInfo {
    root: Option<LocalId>,
    pub(super) owned: bool,
    mutable: bool,
    desc: String,
}

impl FnCx {
    pub fn new(env: TyEnv, ret: Ty, _fn_name: String) -> FnCx {
        FnCx {
            locals: vec![],
            scopes: vec![vec![]],
            used: vec![],
            ret,
            yields: None,
            loops: vec![],
            next_loop: 0,
            pending_label: None,
            frames: vec![],
            unsafe_used: vec![],
            env,
            n_arms: 0,
            arm_consuming: vec![],
            vars: vec![],
            closure_ids: 0,
            hidden: 0,
            task_scopes: vec![],
            next_scope: 0,
            par_frames: vec![],
            locked: vec![],
            call_owner: None,
            loop_body_next: None,
            lazy_loops: vec![],
            defer_depth: 0,
            shadow_ok: None,
            with_value: None,
        }
    }
    pub fn depth(&self) -> u32 {
        self.frames.len() as u32
    }
}

/// The name of the lock in `with v = m.lock()`, for messages.
fn lock_name(e: &Expr) -> String {
    match &e.kind {
        ExprKind::Call(f, _) => match &f.kind {
            ExprKind::Field(b, _, _) => ast_text(b),
            _ => "the lock".into(),
        },
        _ => "the lock".into(),
    }
}

/// A short rendering of a path expression for messages (`jobs`, `s.out`).
fn ast_text(e: &Expr) -> String {
    match &e.kind {
        ExprKind::Ident(n) => n.clone(),
        ExprKind::Field(b, n, _) => format!("{}.{}", ast_text(b), n),
        _ => "ch".into(),
    }
}

/// A place written as a path: `x`, `x.f`, `x[i]`, `x.f[i].g`.
fn ast_is_place(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Ident(_) => true,
        ExprKind::Field(b, _, _) => ast_is_place(b),
        ExprKind::Index(b, i) => i.len() == 1 && ast_is_place(b),
        _ => false,
    }
}

fn is_closure_expr(e: &Expr) -> bool {
    matches!(e.kind, ExprKind::Closure { .. })
}

impl Checker {
    // ------------------------------------------------------------------
    // Driver
    // ------------------------------------------------------------------

    pub fn check_bodies(&mut self) {
        // constants first (types of consts may be inferred); one that uses
        // another is checked after it, in whatever order they are written
        self.const_state = vec![0; self.prog.consts.len()];
        for ci in 0..self.prog.consts.len() {
            self.ensure_const(ci, None);
        }
        // struct field defaults
        for d in 0..self.prog.types.len() {
            self.check_defaults(d as DefId);
        }
        // package-level values (their types may be inferred)
        self.global_state = vec![0; self.prog.globals.len()];
        for gi in 0..self.prog.globals.len() {
            self.ensure_global(gi, None);
        }
        for fid in 0..self.prog.funcs.len() as FuncId {
            let fd = self.prog.func(fid).clone();
            if let Some((fi, ii, ei)) = fd.ast {
                let item = &self.files[fi].items[ii];
                let body = match (&item.kind, ei) {
                    (ast::ItemKind::Func(f), None) => f.body.clone(),
                    (ast::ItemKind::Test { body, .. }, None) => Some(body.clone()),
                    _ => None,
                };
                let params_decl = match &item.kind {
                    ast::ItemKind::Func(f) => Some(f.clone()),
                    _ => None,
                };
                if let Some(b) = body {
                    if fd.kind == FuncKind::Normal || fd.kind == FuncKind::Test {
                        let tf = self.check_func(fid, fi, &b, params_decl.as_ref());
                        self.tfuncs.push(tf);
                    }
                }
            }
        }
        // main
        let main_pkg = self.prog.main_pkg;
        match self.prog.packages[main_pkg as usize].items.get("main") {
            Some(Sym::Func(f)) => {
                let fd = self.prog.func(*f).clone();
                if !fd.params.is_empty() || !(fd.ret == Ty::Unit || matches!(&fd.ret, Ty::Result(t, _) if **t == Ty::Unit)) {
                    self.err(fd.span, "`main` must take no parameters and return nothing (or `! E`)");
                }
            }
            Some(other) => {
                let sp = match other {
                    Sym::Type(d) => Some(self.prog.def(*d).span),
                    Sym::Const(c) => Some(self.prog.consts[*c as usize].span),
                    _ => None,
                };
                let sp = sp.or_else(|| {
                    // a type alias: find its declaration
                    (0..self.files.len()).filter(|&fi| self.file_cx[fi].pkg == main_pkg).flat_map(|fi| self.files[fi].items.iter()).find(|it| matches!(&it.kind, ast::ItemKind::TypeAlias { name, .. } if name == "main")).map(|it| it.span)
                });
                if let Some(sp) = sp {
                    self.err(sp, "`main` must be a function: the program starts by calling `fn main()`");
                }
            }
            None => {}
        }
    }

    /// A use of package-level value `g`.
    fn global_value(&mut self, g: u32, sp: Span) -> TExpr {
        self.ensure_global(g as usize, Some(sp));
        let t = self.prog.globals[g as usize].ty.clone();
        TExpr::new(TK::Global(g), t, sp)
    }

    /// Checks package-level value `gi` (its initializer becomes the body of
    /// its init function) unless it already is.
    fn ensure_global(&mut self, gi: usize, use_sp: Option<Span>) {
        match self.global_state.get(gi).copied() {
            None | Some(2) => {}
            Some(1) => {
                if self.prog.globals[gi].ty.is_error() {
                    let name = self.prog.globals[gi].name.clone();
                    let sp = use_sp.unwrap_or(self.prog.globals[gi].span);
                    self.diags.push(Diag::new(sp, format!("the type of `{}` depends on itself", name)).note(None, format!("write its type: `let {}: T = ...`", name)));
                }
            }
            _ => {
                self.global_state[gi] = 1;
                self.check_global(gi);
                self.global_state[gi] = 2;
            }
        }
    }

    fn check_global(&mut self, gi: usize) {
        let g = self.prog.globals[gi].clone();
        let mut found = None;
        for fi in 0..self.files.len() {
            if self.file_cx[fi].pkg != g.pkg {
                continue;
            }
            for item in &self.files[fi].items {
                if let ast::ItemKind::Global { name, value, .. } = &item.kind {
                    if *name == g.name {
                        found = Some((fi, value.clone()));
                    }
                }
            }
        }
        let Some((fi, value)) = found else { return };
        let env = self.file_env(fi);
        let expect = if g.ty.is_error() { None } else { Some(g.ty.clone()) };
        let mut cx = FnCx::new(env, expect.clone().unwrap_or(Ty::Error), g.name.clone());
        let e = self.expr(&mut cx, &value, expect.as_ref());
        let e = match &expect {
            Some(t) => self.coerce(&mut cx, e, t, Use::Value),
            None => self.value(&mut cx, e),
        };
        let t = self.resolve(&mut cx, &e.ty);
        let t = if t.has_vars() {
            self.diags.push(Diag::new(value.span, format!("cannot infer the type of `{}`", g.name)).note(None, format!("write it: `let {}: T = ...`", g.name)));
            Ty::Error
        } else {
            t
        };
        if !t.is_error() {
            if self.prog.contains_view(&t) {
                let s = self.prog.show(&t);
                self.err(g.span, format!("package-level value `{}` can't be the borrowed view `{}`", g.name, s));
            } else if !self.is_send(&t) {
                let s = self.prog.show(&t);
                self.diags.push(Diag::new(g.span, format!("package-level value `{}` has type `{}`, which can't be shared between tasks", g.name, s)).note(None, "every task can read a package-level value: it must be `send` (no `Rc`, `Cell` or raw pointers)"));
            }
        }
        self.check_pending_infer(&mut cx);
        let e = self.zonk_expr(&mut cx, e);
        self.prog.globals[gi].ty = t.clone();
        self.prog.funcs[g.init as usize].ret = t;
        let body = TBlock { stmts: vec![], value: Some(Box::new(e)), span: value.span };
        self.tfuncs.push(TFunc { func: g.init, locals: cx.locals, params: vec![], recv: None, body, n_arms: cx.n_arms });
    }

    /// Checks constant `ci` unless it already is. `use_sp`: where another
    /// constant (being checked) refers to it.
    fn ensure_const(&mut self, ci: usize, use_sp: Option<Span>) {
        match self.const_state.get(ci).copied() {
            // outside `check_bodies` (array lengths during collection)
            None | Some(2) => {}
            Some(1) => {
                let name = self.prog.consts[ci].name.clone();
                let sp = use_sp.unwrap_or(self.prog.consts[ci].span);
                self.err(sp, format!("the value of constant `{}` depends on itself", name));
                self.prog.consts[ci].ty = Ty::Error;
            }
            _ => {
                self.const_state[ci] = 1;
                self.check_const(ci);
                self.const_state[ci] = 2;
            }
        }
    }

    fn check_const(&mut self, ci: usize) {
        let c = self.prog.consts[ci].clone();
        // find the AST
        let mut found = None;
        for fi in 0..self.files.len() {
            if self.file_cx[fi].pkg != c.pkg {
                continue;
            }
            for item in &self.files[fi].items {
                if let ast::ItemKind::Const { name, value, .. } = &item.kind {
                    if *name == c.name {
                        found = Some((fi, value.clone()));
                    }
                }
            }
        }
        if let Some((fi, value)) = found {
            let env = self.file_env(fi);
            let mut cx = FnCx::new(env, Ty::Unit, "const".into());
            let expect = if c.ty.is_error() { None } else { Some(c.ty.clone()) };
            let e = self.expr(&mut cx, &value, expect.as_ref());
            let e = match &expect {
                Some(t) => self.coerce(&mut cx, e, t, Use::Value),
                None => self.value(&mut cx, e),
            };
            let e = self.zonk_expr(&mut cx, e);
            if !is_const_expr(&e) {
                // an error inside was reported where it happened
                if !has_error_ty(&e.ty) && !matches!(e.kind, TK::Error) {
                    self.err(value.span, "constant value must be a literal or a simple constant expression");
                }
                // its type may hold inference variables of this context only
                self.prog.consts[ci].ty = Ty::Error;
                return;
            }
            self.prog.consts[ci].ty = e.ty.clone();
            self.prog.consts[ci].value = Some(e);
        }
    }

    fn check_defaults(&mut self, d: DefId) {
        let def = self.prog.def(d).clone();
        let fields = match &def.kind {
            TypeDefKind::Struct { fields } => fields.clone(),
            _ => return,
        };
        // find AST
        let mut decl = None;
        for fi in 0..self.files.len() {
            if self.file_cx[fi].pkg != def.pkg {
                continue;
            }
            for item in &self.files[fi].items {
                if let ast::ItemKind::Struct(s) = &item.kind {
                    if s.name == def.name {
                        decl = Some((fi, s.clone()));
                    }
                }
            }
        }
        let (fi, s) = match decl {
            Some(x) => x,
            None => return,
        };
        for (i, f) in s.fields.iter().enumerate() {
            if let Some(dexpr) = &f.default {
                let mut env = self.file_env(fi);
                for (tp, &pid) in s.tparams.iter().zip(def.tparams.iter()) {
                    env.tparams.insert(tp.name.clone(), Ty::Param(pid));
                }
                let mut cx = FnCx::new(env, Ty::Unit, "default".into());
                let ty = fields[i].ty.clone();
                let e = self.expr(&mut cx, dexpr, Some(&ty));
                let e = self.coerce(&mut cx, e, &ty, Use::Value);
                let e = self.zonk_expr(&mut cx, e);
                if let TypeDefKind::Struct { fields } = &mut self.prog.types[d as usize].kind {
                    fields[i].default = Some(e);
                }
            }
        }
    }

    fn check_func(&mut self, fid: FuncId, fi: usize, body: &ast::Block, decl: Option<&ast::FuncDecl>) -> TFunc {
        let diags_before = self.diags.len();
        let fd = self.prog.func(fid).clone();
        let mut env = self.file_env(fi);
        // type params in scope by name
        for &tp in &fd.tparams {
            let name = self.prog.tparam(tp).name.clone();
            env.tparams.insert(name, Ty::Param(tp));
        }
        let mut cx = FnCx::new(env, fd.ret.clone(), fd.name.clone());
        if let Some(y) = fd.yields {
            cx.yields = Some((y, fd.yield_opt));
            cx.ret = Ty::Unit;
        }
        let mut recv = None;
        if let Some(r) = &fd.recv {
            let id = self.declare(&mut cx, &r.name, r.ty.clone(), LocalKind::Param(r.mode), r.span, true);
            recv = Some(id);
        }
        let mut params = vec![];
        for p in &fd.params {
            let id = self.declare(&mut cx, &p.name, p.ty.clone(), LocalKind::Param(p.mode), p.span, true);
            params.push(id);
        }
        let _ = decl;
        let unit_result = matches!(&fd.ret, Ty::Result(t, _) if **t == Ty::Unit);
        let expect = if fd.yields.is_some() || fd.ret == Ty::Unit || fd.ret == Ty::Never || unit_result { Expect::NoValue } else { Expect::Ty(fd.ret.clone()) };
        // `main` and test bodies are scopes: `go` works there directly
        let is_main = fd.kind == FuncKind::Normal && fd.name == "main" && fd.pkg == self.prog.main_pkg && fd.recv.is_none() && fd.owner_ty.is_none();
        let wrapped;
        let body = if is_main || fd.kind == FuncKind::Test {
            let sc = Expr { kind: ExprKind::Scope { name: None, body: body.clone(), implicit: true }, span: body.span };
            wrapped = ast::Block { stmts: vec![Stmt { kind: StmtKind::Expr(sc), span: body.span }], span: body.span };
            &wrapped
        } else {
            body
        };
        let mut tb = self.block_in_scope(&mut cx, body, expect.clone(), false);
        self.finish_fn_body(&mut cx, &mut tb, &fd.ret, body.span, fd.yields.is_some());
        self.pop_scope(&mut cx);
        self.check_pending_infer(&mut cx);
        if self.diags.len() == diags_before {
            self.report_uninferred(&cx, &tb);
        }
        let tb = self.zonk_block(&mut cx, tb);
        TFunc { func: fid, locals: cx.locals, params, recv, body: tb, n_arms: cx.n_arms }
    }

    /// Implicit `ok(())` for `! E` functions, missing-return errors.
    fn finish_fn_body(&mut self, cx: &mut FnCx, tb: &mut TBlock, ret: &Ty, span: Span, is_yield: bool) {
        if is_yield {
            return;
        }
        let diverges = block_diverges(tb);
        let rty = self.resolve(cx, ret);
        if tb.value.is_none() && !diverges {
            match &rty {
                Ty::Unit | Ty::Error => {}
                Ty::Result(t, _) if **t == Ty::Unit => {
                    tb.value = Some(Box::new(TExpr::new(TK::OkLit(Box::new(TExpr::unit(span))), rty.clone(), span)));
                }
                Ty::Option(t) if **t == Ty::Unit => {}
                Ty::Never => self.err(span, "function declared `never` may return"),
                _ => {
                    let s = self.prog.show(&rty);
                    self.diags.push(Diag::new(Span { lo: span.hi.saturating_sub(1), ..span }, format!("missing return value: function returns `{}`", s)).note(None, "the end of the function can be reached: add a `return` there, or end the body with the value"));
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Scopes & locals
    // ------------------------------------------------------------------

    pub fn push_scope(&mut self, cx: &mut FnCx) {
        cx.scopes.push(vec![]);
    }

    pub fn pop_scope(&mut self, cx: &mut FnCx) {
        if let Some(sc) = cx.scopes.pop() {
            for (name, id) in sc {
                if !cx.used[id as usize] && !name.starts_with('_') && !matches!(cx.locals[id as usize].kind, LocalKind::Param(_)) {
                    let sp = cx.locals[id as usize].span;
                    self.diags.push(Diag::new(sp, format!("variable `{}` is declared but never used", name)).note(None, "prefix it with `_` if this is intentional"));
                }
            }
        }
    }

    pub fn declare(&mut self, cx: &mut FnCx, name: &str, ty: Ty, kind: LocalKind, span: Span, is_param: bool) -> LocalId {
        if name != "_" && !name.starts_with("__") && cx.shadow_ok.take().as_deref() != Some(name) {
            for sc in cx.scopes.iter() {
                for (n, id) in sc {
                    if n == name {
                        let prev = cx.locals[*id as usize].span;
                        self.diags.push(Diag::new(span, format!("`{}` shadows an existing variable; Gojo does not allow shadowing inside a function", name)).note(Some(prev), "previously declared here"));
                        break;
                    }
                }
            }
        }
        let _ = is_param;
        let id = cx.locals.len() as LocalId;
        cx.locals.push(Local { name: name.to_string(), ty, kind, span, closure_depth: cx.depth(), block_level: cx.scopes.len() as u32, match_arm: None, ref_root: None });
        cx.used.push(name == "_");
        if let Some(sc) = cx.scopes.last_mut() {
            sc.push((name.to_string(), id));
        }
        id
    }

    fn hidden_local(&mut self, cx: &mut FnCx, ty: Ty, kind: LocalKind, span: Span) -> LocalId {
        cx.hidden += 1;
        let name = format!("__h{}", cx.hidden);
        let id = cx.locals.len() as LocalId;
        cx.locals.push(Local { name: name.clone(), ty, kind, span, closure_depth: cx.depth(), block_level: cx.scopes.len() as u32, match_arm: None, ref_root: None });
        cx.used.push(true);
        cx.scopes.last_mut().unwrap().push((name, id));
        id
    }

    fn lookup_local(&mut self, cx: &mut FnCx, name: &str) -> Option<LocalId> {
        for sc in cx.scopes.iter().rev() {
            for (n, id) in sc.iter().rev() {
                if n == name {
                    let id = *id;
                    cx.used[id as usize] = true;
                    // record captures
                    let ld = cx.locals[id as usize].closure_depth;
                    for fr in cx.frames.iter_mut() {
                        if fr.depth > ld && !fr.captures.iter().any(|c| c.0 == id) {
                            fr.captures.push((id, CapUse::Read));
                        }
                    }
                    return Some(id);
                }
            }
        }
        None
    }

    fn note_use(&mut self, cx: &mut FnCx, root: LocalId, u: CapUse) {
        let ld = cx.locals[root as usize].closure_depth;
        for fr in cx.frames.iter_mut() {
            if fr.depth > ld {
                for c in fr.captures.iter_mut() {
                    if c.0 == root {
                        let rank = |u: CapUse| match u {
                            CapUse::Read => 0,
                            CapUse::Mutate => 1,
                            CapUse::Move => 2,
                        };
                        if rank(u) > rank(c.1) {
                            c.1 = u;
                        }
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Inference
    // ------------------------------------------------------------------

    pub fn fresh(&mut self, cx: &mut FnCx) -> Ty {
        cx.vars.push(None);
        Ty::Var((cx.vars.len() - 1) as u32)
    }

    pub fn resolve(&self, cx: &FnCx, t: &Ty) -> Ty {
        if !t.has_vars() {
            return t.clone();
        }
        t.map(&mut |x| match x {
            Ty::Var(v) => Some(match &cx.vars[*v as usize] {
                Some(b) => self.resolve(cx, b),
                None => x.clone(),
            }),
            _ => None,
        })
    }

    fn shallow(&self, cx: &FnCx, t: &Ty) -> Ty {
        let mut t = t.clone();
        while let Ty::Var(v) = t {
            match &cx.vars[v as usize] {
                Some(b) => t = b.clone(),
                None => break,
            }
        }
        t
    }

    pub fn unify(&mut self, cx: &mut FnCx, a: &Ty, b: &Ty) -> bool {
        let a = self.shallow(cx, a);
        let b = self.shallow(cx, b);
        match (&a, &b) {
            (Ty::Var(x), Ty::Var(y)) if x == y => true,
            (Ty::Var(_), Ty::Error) | (Ty::Error, Ty::Var(_)) => true,
            (Ty::Error, _) | (_, Ty::Error) => true,
            (Ty::Var(x), t) | (t, Ty::Var(x)) => {
                let r = self.resolve(cx, t);
                let mut occurs = false;
                r.walk(&mut |y| {
                    if *y == Ty::Var(*x) {
                        occurs = true
                    }
                });
                if occurs {
                    return false;
                }
                cx.vars[*x as usize] = Some(r);
                true
            }
            (Ty::Vec(x), Ty::Vec(y)) | (Ty::Slice(x), Ty::Slice(y)) | (Ty::Option(x), Ty::Option(y)) | (Ty::Box(x), Ty::Box(y)) | (Ty::Rc(x), Ty::Rc(y)) | (Ty::Arc(x), Ty::Arc(y)) | (Ty::Weak(x), Ty::Weak(y)) | (Ty::Cell(x), Ty::Cell(y)) | (Ty::Mutex(x), Ty::Mutex(y)) | (Ty::MutexRef(x), Ty::MutexRef(y)) | (Ty::Sender(x), Ty::Sender(y)) | (Ty::Receiver(x), Ty::Receiver(y)) | (Ty::Task(x), Ty::Task(y)) | (Ty::Iso(x), Ty::Iso(y)) | (Ty::Ptr(x), Ty::Ptr(y)) => {
                let (x, y) = ((**x).clone(), (**y).clone());
                self.unify(cx, &x, &y)
            }
            (Ty::ArrayT(x, n), Ty::Array(y, m)) | (Ty::Array(y, m), Ty::ArrayT(x, n)) => {
                let (x, y, n) = ((**x).clone(), (**y).clone(), (**n).clone());
                self.unify(cx, &n, &Ty::ConstInt(*m)) && self.unify(cx, &x, &y)
            }
            (Ty::ArrayT(x, n), Ty::ArrayT(y, m)) => {
                let (x, y, n, m) = ((**x).clone(), (**y).clone(), (**n).clone(), (**m).clone());
                self.unify(cx, &n, &m) && self.unify(cx, &x, &y)
            }
            (Ty::Array(x, n), Ty::Array(y, m)) => {
                let (x, y) = ((**x).clone(), (**y).clone());
                n == m && self.unify(cx, &x, &y)
            }
            (Ty::Map(x1, y1), Ty::Map(x2, y2)) | (Ty::Result(x1, y1), Ty::Result(x2, y2)) => {
                let (x1, y1, x2, y2) = ((**x1).clone(), (**y1).clone(), (**x2).clone(), (**y2).clone());
                self.unify(cx, &x1, &x2) && self.unify(cx, &y1, &y2)
            }
            (Ty::Adt(d1, a1), Ty::Adt(d2, a2)) | (Ty::Iface(d1, a1), Ty::Iface(d2, a2)) => {
                if d1 != d2 || a1.len() != a2.len() {
                    return false;
                }
                let (a1, a2) = (a1.clone(), a2.clone());
                a1.iter().zip(a2.iter()).all(|(x, y)| self.unify(cx, x, y))
            }
            (Ty::Func(f1), Ty::Func(f2)) => {
                if f1.params.len() != f2.params.len() {
                    return false;
                }
                let (f1, f2) = (f1.clone(), f2.clone());
                for ((m1, x), (m2, y)) in f1.params.iter().zip(f2.params.iter()) {
                    if m1 != m2 || !self.unify(cx, x, y) {
                        return false;
                    }
                }
                self.unify(cx, &f1.ret, &f2.ret)
            }
            (x, y) => x == y,
        }
    }

    /// Unify without side effects on failure.
    fn try_unify(&mut self, cx: &mut FnCx, a: &Ty, b: &Ty) -> bool {
        let snap = cx.vars.clone();
        if self.unify(cx, a, b) {
            true
        } else {
            cx.vars = snap;
            false
        }
    }

    fn zonk_ty(&self, cx: &FnCx, t: &Ty) -> Ty {
        self.resolve(cx, t)
    }

    pub fn zonk_expr(&mut self, cx: &mut FnCx, mut e: TExpr) -> TExpr {
        zonk_walk_expr(&mut e, &mut |t| *t = self.zonk_ty(cx, t));
        e
    }

    /// A type still unknown at the end of a function that had no errors (which
    /// would explain it): `f` alone as a statement, for a generic `f`. Code
    /// generation must never see one.
    fn report_uninferred(&mut self, cx: &FnCx, b: &TBlock) {
        let mut found: Option<(Span, String)> = None;
        // an error type left by another item's error (a bad constant) is a
        // cascade of that error
        let no_errors = self.diags.is_empty();
        crate::tir::visit_block(b, &mut |e| {
            if found.is_some() {
                return;
            }
            let unknown = |t: &Ty| self.resolve(cx, t).has_vars();
            match &e.kind {
                TK::FuncRef(f, targs) if targs.iter().any(|t| unknown(t)) => {
                    let fd = self.prog.func(*f);
                    let names: Vec<String> = fd.tparams.iter().zip(targs).filter(|(_, t)| unknown(t)).map(|(p, _)| format!("`{}`", self.prog.tparam(*p).name)).collect();
                    found = Some((e.span, format!("cannot infer type parameter{} {} of `{}`; specify {}: `{}[T]`", if names.len() > 1 { "s" } else { "" }, names.join(", "), fd.name, if names.len() > 1 { "them" } else { "it" }, fd.name)));
                }
                TK::Call { callee: Callee::Func(f, targs), .. } if targs.iter().any(|t| unknown(t)) => {
                    let fd = self.prog.func(*f);
                    found = Some((e.span, format!("cannot infer the type arguments of `{}`; specify them: `{}[T](...)`", fd.name, fd.name)));
                }
                _ if unknown(&e.ty) => found = Some((e.span, "cannot infer the type of this expression; add a type annotation".to_string())),
                _ if no_errors && has_error_ty(&self.resolve(cx, &e.ty)) => found = Some((e.span, "internal error: the compiler could not type this expression (please report it)".to_string())),
                _ => {}
            }
        });
        if let Some((sp, msg)) = found {
            self.err(sp, msg);
        }
    }

    fn zonk_block(&mut self, cx: &mut FnCx, mut b: TBlock) -> TBlock {
        for l in cx.locals.iter_mut() {
            l.ty = self.resolve_static(&cx.vars, &l.ty);
        }
        zonk_walk_block(&mut b, &mut |t| *t = self.resolve_static(&cx.vars, t));
        // unresolved vars become errors (already reported)
        b
    }

    fn resolve_static(&self, vars: &[Option<Ty>], t: &Ty) -> Ty {
        if !t.has_vars() {
            return t.clone();
        }
        t.map(&mut |x| match x {
            Ty::Var(v) => Some(match &vars[*v as usize] {
                Some(b) => self.resolve_static(vars, b),
                None => Ty::Error,
            }),
            _ => None,
        })
    }

    pub fn show_ty(&self, cx: &FnCx, t: &Ty) -> String {
        self.prog.show(&self.resolve(cx, t))
    }

    // ------------------------------------------------------------------
    // Places and use modes
    // ------------------------------------------------------------------

    pub(super) fn place_info(&self, cx: &FnCx, e: &TExpr) -> Option<PInfo> {
        match &e.kind {
            TK::Global(g) => Some(PInfo { root: None, owned: false, mutable: false, desc: format!("package-level value `{}`", self.prog.globals[*g as usize].name) }),
            TK::Local(id) => {
                let l = &cx.locals[*id as usize];
                let (owned, mutable, what) = match l.kind {
                    LocalKind::Let => (true, false, "variable"),
                    LocalKind::Var => (true, true, "variable"),
                    LocalKind::Param(Mode::Borrow) => (false, false, "borrowed parameter"),
                    LocalKind::Param(Mode::Inout) => (false, true, "inout parameter"),
                    LocalKind::Param(Mode::Sink) => (true, true, "parameter"),
                    LocalKind::Ref { mutable } => (false, mutable, "reference"),
                };
                Some(PInfo { root: Some(*id), owned, mutable, desc: format!("{} `{}`", what, l.name) })
            }
            TK::Field { base, index } => {
                let b = self.place_info(cx, base)?;
                let bty = &base.ty;
                let (fname, has_drop) = match bty {
                    Ty::Adt(d, _) => (self.prog.def(*d).fields().get(*index).map(|f| f.name.clone()).unwrap_or_default(), self.prog.def(*d).has_drop),
                    _ => (String::new(), false),
                };
                Some(PInfo { root: b.root, owned: b.owned && !has_drop, mutable: b.mutable, desc: format!("field `{}` of {}", fname, b.desc) })
            }
            TK::Deref(base) => {
                let b = self.place_info(cx, base);
                let (mutable, desc) = match &base.ty {
                    Ty::Box(_) => (b.as_ref().map(|b| b.mutable).unwrap_or(true), "the contents of a Box"),
                    Ty::Rc(_) | Ty::Arc(_) => (false, "shared (Rc/Arc) data"),
                    Ty::Ptr(_) => (true, "data behind a raw pointer"),
                    _ => (false, "a reference"),
                };
                Some(PInfo { root: b.and_then(|b| b.root), owned: false, mutable, desc: desc.into() })
            }
            TK::Index { base, .. } | TK::MapIndex { base, .. } => {
                let b = self.place_info(cx, base);
                let mutable = match self.resolve(cx, &base.ty) {
                    // a slice is a read-only view unless it came in `inout`
                    Ty::Slice(_) => self.slice_writable(cx, base).is_ok(),
                    _ => b.as_ref().map(|b| b.mutable).unwrap_or(true),
                };
                Some(PInfo { root: b.and_then(|b| b.root), owned: false, mutable, desc: "an element of a collection".into() })
            }
            TK::ProjUnwrap { proj, .. } => self.place_info(cx, &TExpr::new(match &proj.kind {
                TK::Proj { call, mutable, .. } => TK::Proj { call: call.clone(), mutable: *mutable, opt: false },
                k => k.clone(),
            }, proj.ty.clone(), proj.span)),
            TK::Proj { call, mutable, .. } => {
                let root = match &call.kind {
                    TK::Call { args, .. } => args.first().and_then(|a| self.place_info(cx, &a.e)).and_then(|p| p.root),
                    _ => None,
                };
                Some(PInfo { root, owned: false, mutable: *mutable, desc: "a projected reference".into() })
            }
            TK::Coerce(Coercion::VecToSlice | Coercion::ArrayToSlice, inner) => {
                let b = self.place_info(cx, inner)?;
                Some(PInfo { root: b.root, owned: false, mutable: b.mutable, desc: b.desc })
            }
            _ => None,
        }
    }

    /// `x?.field` on a place with a non-copy field: a read-only view of the field.
    pub fn is_borrowed_rvalue(&self, cx: &FnCx, e: &TExpr) -> bool {
        match &e.kind {
            TK::OptField { base, .. } => {
                let t = self.resolve(cx, &e.ty);
                (base.is_place() || self.is_borrowed_rvalue(cx, base)) && !self.is_copy(&t)
            }
            TK::Proj { opt: true, .. } => !self.is_copy(&self.resolve(cx, &e.ty)),
            _ => false,
        }
    }

    pub fn place_root(&self, cx: &FnCx, e: &TExpr) -> Option<LocalId> {
        self.place_info(cx, e).and_then(|p| p.root)
    }

    /// The consumer takes ownership of the value.
    pub fn value(&mut self, cx: &mut FnCx, e: TExpr) -> TExpr {
        if matches!(e.ty, Ty::Never | Ty::Error) {
            return e;
        }
        if let TK::Closure(_) = &e.kind {
            let mut e = e;
            if let TK::Closure(c) = &mut e.kind {
                self.finalize_closure(cx, c, true);
            }
            return e;
        }
        if !e.is_place() {
            if self.is_borrowed_rvalue(cx, &e) {
                let t = self.resolve(cx, &e.ty);
                let s = self.prog.show(&t);
                let note = if matches!(e.kind, TK::Proj { .. }) { "this value is borrowed from a projection; use `if let`, `.unwrap()` or `.clone()`" } else { "`?.` on a variable borrows the field; use `.clone()` or `if let` to work with it" };
                self.diags.push(Diag::new(e.span, format!("cannot move a value of type `{}` out of a borrowed place", s)).note(None, note));
                return TExpr::error(e.span);
            }
            return e;
        }
        let ty = self.resolve(cx, &e.ty);
        let info = self.place_info(cx, &e);
        if self.is_copy(&ty) {
            if let Some(r) = info.and_then(|i| i.root) {
                self.note_use(cx, r, CapUse::Read);
            }
            let sp = e.span;
            return TExpr::new(TK::Copy(Box::new(e)), ty, sp);
        }
        let info = info.unwrap();
        if let TK::Deref(base) = &e.kind {
            if matches!(self.resolve(cx, &base.ty), Ty::Box(_)) {
                if let TK::Local(id) = &base.kind {
                    if let Some(arm) = cx.locals[*id as usize].match_arm {
                        cx.arm_consuming[arm as usize] = true;
                        cx.locals[*id as usize].kind = LocalKind::Let;
                    }
                }
                if let Some(bi) = self.place_info(cx, base) {
                    if bi.owned {
                        let sp = e.span;
                        let TK::Deref(base) = e.kind else { unreachable!() };
                        let b = self.value(cx, *base);
                        return TExpr::new(TK::Unbox(Box::new(b)), ty, sp);
                    }
                }
            }
        }
        if info.owned {
            if let Some(r) = info.root {
                self.note_use(cx, r, CapUse::Move);
            }
            let sp = e.span;
            return TExpr::new(TK::Move(Box::new(e)), ty, sp);
        }
        // a binding from a match on an owned value may be upgraded to a move
        if let TK::Local(id) = &e.kind {
            if let Some(arm) = cx.locals[*id as usize].match_arm {
                cx.arm_consuming[arm as usize] = true;
                cx.locals[*id as usize].kind = LocalKind::Let;
                let sp = e.span;
                return TExpr::new(TK::Move(Box::new(e)), ty, sp);
            }
        }
        let s = self.prog.show(&ty);
        self.diags.push(
            Diag::new(e.span, format!("cannot move out of {} (type `{}`)", info.desc, s)).note(None, if self.is_clone(&ty) { "use `.clone()` to make an explicit copy" } else { "the value is only borrowed here" }),
        );
        TExpr::error(e.span)
    }

    /// The consumer only reads the value.
    pub fn borrow(&mut self, cx: &mut FnCx, mut e: TExpr) -> TExpr {
        if let TK::Closure(c) = &mut e.kind {
            self.finalize_closure(cx, c, false);
            return e;
        }
        if let Some(r) = self.place_root(cx, &e) {
            self.note_use(cx, r, CapUse::Read);
        }
        e
    }

    /// Whether the elements of slice `e` may be written: it is an `inout []T`
    /// parameter, a range of one, or a range of a mutable `Vec` or array.
    /// Otherwise what it is, for the error message. A plain `[]T` is a
    /// read-only view, even in a `var`: other views of the same elements may
    /// exist, in this task or another.
    pub fn slice_writable(&self, cx: &FnCx, e: &TExpr) -> Result<Option<LocalId>, String> {
        match &e.kind {
            TK::Local(id) => {
                let l = &cx.locals[*id as usize];
                match l.kind {
                    LocalKind::Param(Mode::Inout) => Ok(Some(*id)),
                    LocalKind::Param(_) => Err(format!("parameter `{}` is a read-only view", l.name)),
                    _ => Err(format!("`{}` is a read-only view", l.name)),
                }
            }
            TK::Copy(x) => self.slice_writable(cx, x),
            TK::SliceOf { base, .. } => match self.resolve(cx, &base.ty) {
                Ty::Slice(_) => self.slice_writable(cx, base),
                Ty::Vec(_) | Ty::Array(..) | Ty::ArrayT(..) => match self.place_info(cx, base) {
                    Some(p) if p.mutable => Ok(p.root),
                    Some(p) => Err(format!("{} is not mutable", p.desc)),
                    None => Err("a range of a temporary value".into()),
                },
                _ => Err("the range is read-only".into()),
            },
            TK::Coerce(Coercion::VecToSlice | Coercion::ArrayToSlice, inner) => match self.place_info(cx, inner) {
                Some(p) if p.mutable => Ok(p.root),
                Some(p) => Err(format!("{} is not mutable", p.desc)),
                None => Err("a temporary value".into()),
            },
            _ => Err("a temporary view".into()),
        }
    }

    pub fn require_mut(&mut self, cx: &mut FnCx, e: &TExpr, what: &str) -> bool {
        if matches!(e.ty, Ty::Error) {
            return true;
        }
        // (assigning a slice variable points it elsewhere; an `inout` one may
        // only be re-sliced, which the borrow checker enforces)
        if let Ty::Slice(_) = self.resolve(cx, &e.ty) {
            if what != "assign to" {
                return match self.slice_writable(cx, e) {
                    Ok(root) => {
                        if let Some(r) = root {
                            self.note_use(cx, r, CapUse::Mutate);
                        }
                        true
                    }
                    Err(why) => {
                        self.diags.push(Diag::new(e.span, format!("cannot {} this slice: {}", what, why)).note(None, "only `inout []T` parameters may change elements; pass `&v` or `&v[a..b]` of a `var` Vec or array"));
                        false
                    }
                };
            }
        }
        match self.place_info(cx, e) {
            Some(p) => {
                if let Some(r) = p.root {
                    self.note_use(cx, r, CapUse::Mutate);
                }
                if let (false, TK::Index { base, .. }) = (p.mutable, &e.kind) {
                    if let (Ty::Slice(_), Err(why)) = (self.resolve(cx, &base.ty), self.slice_writable(cx, base)) {
                        self.diags.push(Diag::new(e.span, format!("cannot {} an element of a read-only slice: {}", what, why)).note(None, "only `inout []T` parameters may change elements; pass `&v` or `&v[a..b]` of a `var` Vec or array"));
                        return false;
                    }
                }
                if !p.mutable {
                    let mut d = Diag::new(e.span, format!("cannot {} {}: it is not mutable", what, p.desc));
                    if let Some(r) = p.root {
                        let l = &cx.locals[r as usize];
                        match l.kind {
                            LocalKind::Let => d = d.note(Some(l.span), "declare it with `var` to make it mutable"),
                            LocalKind::Param(Mode::Borrow) => d = d.note(Some(l.span), "declare the parameter `inout` to modify it"),
                            _ => {}
                        }
                    }
                    self.diags.push(d);
                    return false;
                }
                true
            }
            None => {
                self.err(e.span, format!("cannot {} a temporary value", what));
                false
            }
        }
    }

    // ------------------------------------------------------------------
    // Coercion
    // ------------------------------------------------------------------

    pub fn coerce(&mut self, cx: &mut FnCx, e: TExpr, target: &Ty, u: Use) -> TExpr {
        let actual = self.resolve(cx, &e.ty);
        let target = self.resolve(cx, target);
        let sp = e.span;
        if matches!(actual, Ty::Never | Ty::Error) || target.is_error() {
            return e;
        }
        if let (Ty::Func(a), Ty::Func(t)) = (&actual, &target) {
            if t.send && !a.send && !matches!(e.kind, TK::FuncRef(..)) {
                let s = self.prog.show(&actual);
                self.diags.push(Diag::new(sp, format!("expected a `send fn`, found `{}`", s)).note(None, "a `send fn` may run in many tasks at once: pass a function, or a closure that only reads `send` values it captures"));
                return TExpr::error(sp);
            }
        }
        if let (TK::FuncRef(..), Ty::Func(_)) = (&e.kind, &target) {
            let t = actual.clone();
            if self.unify(cx, &t, &target) {
                return TExpr::new(TK::Coerce(Coercion::FuncItem, Box::new(e)), target, sp);
            }
        }
        // interface -> interface (same) or owned iface into borrowed position
        if let (Ty::Iface(d1, _), Ty::Iface(d2, _)) = (&actual, &target) {
            if d1 == d2 && self.unify(cx, &actual, &target) {
                return match u {
                    Use::Value => self.value(cx, e),
                    Use::Borrow => self.borrow(cx, e),
                };
            }
        }
        if self.try_unify(cx, &actual, &target) {
            return match u {
                Use::Value => self.value(cx, e),
                Use::Borrow => self.borrow(cx, e),
            };
        }
        if let Ty::Box(inner) | Ty::Rc(inner) | Ty::Arc(inner) = &actual {
            let inner = (**inner).clone();
            if !matches!(target, Ty::Box(_) | Ty::Rc(_) | Ty::Arc(_) | Ty::Var(_)) && self.try_unify(cx, &inner, &target) {
                let is_box = matches!(actual, Ty::Box(_));
                if u == Use::Borrow || self.is_copy(&inner) || is_box {
                    let d = if e.is_place() || u == Use::Borrow {
                        TExpr::new(TK::Deref(Box::new(e)), inner.clone(), sp)
                    } else {
                        // rvalue box consumed by value
                        let v = self.value(cx, e);
                        return TExpr::new(TK::Unbox(Box::new(v)), inner, sp);
                    };
                    return match u {
                        Use::Value => self.value(cx, d),
                        Use::Borrow => self.borrow(cx, d),
                    };
                }
            }
        }
        match (&actual, &target) {
            (Ty::Str, Ty::String) if matches!(e.kind, TK::Str(_)) => return TExpr::new(TK::Coerce(Coercion::StrToString, Box::new(e)), Ty::String, sp),
            (Ty::String, Ty::Str) => {
                let e = self.borrow(cx, e);
                return TExpr::new(TK::Coerce(Coercion::StringToStr, Box::new(e)), Ty::Str, sp);
            }
            (Ty::Vec(a), Ty::Slice(b)) | (Ty::Array(a, _), Ty::Slice(b)) | (Ty::ArrayT(a, _), Ty::Slice(b)) => {
                let (a, b) = ((**a).clone(), (**b).clone());
                if self.unify(cx, &a, &b) {
                    let k = if matches!(actual, Ty::Vec(_)) { Coercion::VecToSlice } else { Coercion::ArrayToSlice };
                    let _ = &k;
                    let e = self.borrow(cx, e);
                    return TExpr::new(TK::Coerce(k, Box::new(e)), target.clone(), sp);
                }
            }
            _ => {}
        }
        if let Ty::Option(inner) = &target {
            let inner = (**inner).clone();
            if !matches!(actual, Ty::Option(_)) {
                let e2 = self.coerce_quiet(cx, e.clone(), &inner, Use::Value);
                if let Some(v) = e2 {
                    return TExpr::new(TK::SomeLit(Box::new(v)), target.clone(), sp);
                }
            }
        }
        if let Ty::Result(ok, _) = &target {
            let ok = (**ok).clone();
            if !matches!(actual, Ty::Result(..)) {
                if let Some(v) = self.coerce_quiet(cx, e.clone(), &ok, Use::Value) {
                    return TExpr::new(TK::OkLit(Box::new(v)), target.clone(), sp);
                }
            }
        }
        if let Ty::Iface(d, args) = &target {
            if !matches!(actual, Ty::Iface(..) | Ty::Var(_)) {
                let (d, args) = (*d, args.clone());
                match self.implements(&actual, d, &args) {
                    Ok(()) => {
                        let owned = u == Use::Value;
                        let inner = if owned { self.value(cx, e) } else { self.borrow(cx, e) };
                        return TExpr::new(TK::Coerce(Coercion::ToIface { owned }, Box::new(inner)), target.clone(), sp);
                    }
                    Err(msg) => {
                        self.err(sp, msg);
                        return TExpr::error(sp);
                    }
                }
            }
        }
        let (a, t) = (self.prog.show(&actual), self.prog.show(&target));
        let mut d = Diag::new(sp, format!("mismatched types: expected `{}`, found `{}`", t, a));
        if actual == Ty::Str && target == Ty::String {
            d = d.note(None, "use `.toString()` to allocate an owned String");
        }
        if actual.is_numeric() && target.is_numeric() {
            d = d.note(None, format!("use an explicit conversion: `{}(x)`", t));
        }
        match (&actual, &target) {
            (_, Ty::Option(inner)) if **inner == actual => d = d.note(None, "wrap the value: `some(x)`"),
            (Ty::Option(inner), _) if **inner == target => d = d.note(None, "the value may be `none`: use `x ?? fallback`, `if let v = x { ... }`, or `x.unwrap()` if it can't be"),
            (Ty::Result(inner, _), _) if **inner == target => d = d.note(None, "the value may be an error: use `try x`, `x catch e { ... }`, or `match`"),
            (Ty::Unit, _) if target != Ty::Unit => d = d.note(None, "this has no value: a block's value is its last expression, without `;` or `return`"),
            (Ty::String, Ty::Str) => d = d.note(None, "borrow it as a view: `s.asStr()`"),
            _ => {}
        }
        self.diags.push(d);
        TExpr::error(sp)
    }

    fn coerce_quiet(&mut self, cx: &mut FnCx, e: TExpr, target: &Ty, u: Use) -> Option<TExpr> {
        let n = self.diags.len();
        let snap = cx.vars.clone();
        let r = self.coerce(cx, e, target, u);
        if self.diags.len() > n || matches!(r.kind, TK::Error) {
            self.diags.truncate(n);
            cx.vars = snap;
            None
        } else {
            Some(r)
        }
    }

    pub fn expr_value(&mut self, cx: &mut FnCx, e: &Expr, expect: Option<&Ty>) -> TExpr {
        let te = self.expr(cx, e, expect);
        match expect {
            Some(t) => {
                let t = t.clone();
                self.coerce(cx, te, &t, Use::Value)
            }
            None => self.value(cx, te),
        }
    }

    pub fn expr_borrow(&mut self, cx: &mut FnCx, e: &Expr, expect: Option<&Ty>) -> TExpr {
        let te = self.expr(cx, e, expect);
        match expect {
            Some(t) => {
                let t = t.clone();
                self.coerce(cx, te, &t, Use::Borrow)
            }
            None => self.borrow(cx, te),
        }
    }

    // ------------------------------------------------------------------
    // Blocks and statements
    // ------------------------------------------------------------------

    pub fn block(&mut self, cx: &mut FnCx, b: &ast::Block, expect: Expect) -> TBlock {
        self.push_scope(cx);
        let tb = self.block_in_scope(cx, b, expect, true);
        self.pop_scope(cx);
        tb
    }

    fn block_in_scope(&mut self, cx: &mut FnCx, b: &ast::Block, expect: Expect, _own_scope: bool) -> TBlock {
        let mut stmts = vec![];
        let mut value = None;
        let n = b.stmts.len();
        for (i, s) in b.stmts.iter().enumerate() {
            let last = i + 1 == n;
            if last && !matches!(expect, Expect::NoValue) {
                if let StmtKind::Expr(e) = &s.kind {
                    // `go f()` written as a statement starts a plain task, even last
                    let no_else = matches!(&e.kind, ExprKind::If { else_: None, .. } | ExprKind::IfLet { else_: None, .. } | ExprKind::Go(_));
                    if no_else {
                        if let Some(ts) = self.stmt(cx, s) {
                            stmts.push(ts);
                        }
                        continue;
                    }
                    if let ExprKind::With(_) = &e.kind {
                        // a `with` whose body gives no value stays a statement
                        let te = self.expr_x(cx, e, expect.clone());
                        if self.resolve(cx, &te.ty) == Ty::Unit && matches!(&te.kind, TK::Block(b) if b.value.is_none()) {
                            if let TK::Block(b) = te.kind {
                                stmts.extend(b.stmts);
                            }
                            continue;
                        }
                        let te = match &expect {
                            Expect::Ty(t) => {
                                let t = t.clone();
                                self.coerce(cx, te, &t, Use::Value)
                            }
                            _ => self.value(cx, te),
                        };
                        value = Some(Box::new(te));
                        continue;
                    }
                    let te = match &expect {
                        Expect::Ty(t) => {
                            let t = t.clone();
                            self.expr_value(cx, e, Some(&t))
                        }
                        _ => self.expr_value(cx, e, None),
                    };
                    value = Some(Box::new(te));
                    continue;
                }
            }
            if let Some(ts) = self.stmt(cx, s) {
                stmts.push(ts);
            }
        }
        TBlock { stmts, value, span: b.span }
    }

    pub fn block_ty(&self, b: &TBlock) -> Ty {
        match &b.value {
            Some(v) => v.ty.clone(),
            None => {
                if block_diverges(b) {
                    Ty::Never
                } else {
                    Ty::Unit
                }
            }
        }
    }

    fn stmt(&mut self, cx: &mut FnCx, s: &Stmt) -> Option<TStmt> {
        if let StmtKind::Expr(Expr { kind: ExprKind::With(inner), .. }) = &s.kind {
            return self.stmt(cx, inner);
        }
        let sp = s.span;
        Some(match &s.kind {
            StmtKind::LetElse { .. } => {
                self.err(sp, "`let ... else` must be a statement of a block");
                return None;
            }
            StmtKind::Let { name, name_span, ty, init, mutable } => {
                let decl_ty = ty.as_ref().map(|t| {
                    let env = cx.env.clone();
                    self.resolve_type(t, &env)
                });

                match init {
                    Some(init) => {
                        let raw = self.expr(cx, init, decl_ty.as_ref());
                        let rty = self.resolve(cx, &raw.ty);
                        // binding a borrowed place makes a reference
                        if raw.is_place() && !self.is_copy(&rty) && decl_ty.is_none() {
                            let info = self.place_info(cx, &raw).unwrap();
                            if !info.owned && !(matches!(&raw.kind, TK::Local(id) if cx.locals[*id as usize].match_arm.is_some())) {
                                if *mutable && !info.mutable {
                                    self.err(init.span, format!("cannot bind {} mutably; it is not mutable (use `let`, or `.clone()` it)", info.desc));
                                }
                                let root = self.place_root(cx, &raw);
                                let raw = self.borrow(cx, raw);
                                let id = self.declare(cx, name, rty, LocalKind::Ref { mutable: *mutable }, *name_span, false);
                                cx.locals[id as usize].ref_root = root.map(|r| cx.locals[r as usize].ref_root.unwrap_or(r));
                                return Some(TStmt::LetRef { local: id, place: raw });
                            }
                        }
                        let te = match &decl_ty {
                            Some(t) => self.coerce(cx, raw, t, Use::Value),
                            None => self.value(cx, raw),
                        };
                        let ty = match &decl_ty {
                            Some(t) => t.clone(),
                            None => self.resolve(cx, &te.ty),
                        };
                        // (a lazy chain's accumulator gets its type from the loop after it)
                        if ty.has_vars() && !name.starts_with("__lz") {
                            self.err(*name_span, format!("cannot infer the type of `{}`; add a type annotation", name));
                        }
                        let ty = if ty == Ty::Never { Ty::Unit } else { ty };
                        let kind = if *mutable { LocalKind::Var } else { LocalKind::Let };
                        let id = self.declare(cx, name, ty, kind, *name_span, false);
                        TStmt::Let { local: id, init: te }
                    }
                    None => {
                        let ty = match decl_ty {
                            Some(t) => t,
                            None => {
                                self.err(*name_span, format!("`{}` needs a type or an initial value", name));
                                Ty::Error
                            }
                        };
                        if !*mutable {
                            self.err(*name_span, format!("`let {}` needs an initial value", name));
                        } else if !self.is_default(&ty) {
                            let s = self.prog.show(&ty);
                            self.diags.push(Diag::new(*name_span, format!("`var {}: {}` needs an initial value: `{}` has no default value", name, s, s)).note(None, format!("give it one: `var {}: {} = ...`", name, s)));
                        }
                        let id = self.declare(cx, name, ty.clone(), LocalKind::Var, *name_span, false);
                        TStmt::Let { local: id, init: TExpr::new(TK::DefaultOf, ty, sp) }
                    }
                }
            }
            StmtKind::Expr(e) => {
                if let ExprKind::Go(inner) = &e.kind {
                    let te = self.go_expr(cx, inner, false, e.span);
                    return Some(TStmt::Expr(te));
                }
                let te = self.expr_novalue(cx, e);
                self.check_must_use(cx, &te);
                let te = self.borrow(cx, te);
                TStmt::Expr(te)
            }
            StmtKind::Discard(e) => {
                let te = self.expr(cx, e, None);
                let te = self.value(cx, te);
                TStmt::Expr(te)
            }
            StmtKind::Assign { place, op, value } => return self.assign(cx, place, op, value, sp),
            StmtKind::Return(v) => {
                if cx.defer_depth > 0 {
                    self.err(sp, "cannot `return` from a defer");
                }
                if cx.par_frames.last() == Some(&cx.depth()) {
                    self.diags.push(Diag::new(sp, "`return` can't leave a `par for`: its iterations run at the same time").note(None, "`continue` ends this iteration; to find a value, collect results with `task.map`"));
                    return None;
                }
                if cx.yields.is_some() && cx.frames.is_empty() {
                    if v.is_some() {
                        self.err(sp, "projection functions `yield` a place instead of returning a value");
                    }
                    return Some(TStmt::Return(None, sp));
                }
                let ret = cx.ret.clone();
                let rr = self.resolve(cx, &ret);
                match v {
                    Some(v) => {
                        if rr == Ty::Unit {
                            self.err(v.span, "this function does not return a value");
                        }
                        let te = self.expr(cx, v, Some(&ret));
                        let te = self.coerce(cx, te, &ret, Use::Value);
                        TStmt::Return(Some(te), sp)
                    }
                    None => {
                        match &rr {
                            Ty::Unit | Ty::Error => TStmt::Return(None, sp),
                            Ty::Result(t, _) if **t == Ty::Unit => TStmt::Return(Some(TExpr::new(TK::OkLit(Box::new(TExpr::unit(sp))), rr.clone(), sp)), sp),
                            Ty::Var(_) => {
                                self.unify(cx, &ret, &Ty::Unit);
                                TStmt::Return(None, sp)
                            }
                            _ => {
                                let s = self.prog.show(&rr);
                                self.err(sp, format!("missing return value of type `{}`", s));
                                TStmt::Return(None, sp)
                            }
                        }
                    }
                }
            }
            StmtKind::Yield(v) => {
                if cx.defer_depth > 0 {
                    self.err(sp, "cannot `yield` from a defer");
                }
                let (yk, opt) = match cx.yields {
                    Some(y) if cx.frames.is_empty() => y,
                    _ => {
                        self.err(sp, "`yield` is only allowed in projection functions (declared with `yield &T`)");
                        return None;
                    }
                };
                let fty = match cx.env.tparams.get("\0") {
                    _ => None::<Ty>,
                };
                let _ = fty;
                match v {
                    None => {
                        self.err(sp, "`yield` needs a place");
                        None?
                    }
                    Some(v) => {
                        if matches!(v.kind, ExprKind::None) {
                            if !opt {
                                self.err(v.span, "only optional projections (`yield ?&T`) can yield `none`");
                            }
                            return Some(TStmt::Yield(None, sp));
                        }
                        let te = self.expr(cx, v, None);
                        match self.place_info(cx, &te) {
                            Some(p) => {
                                let root_ok = match p.root {
                                    Some(r) => matches!(cx.locals[r as usize].kind, LocalKind::Param(Mode::Borrow | Mode::Inout) | LocalKind::Ref { .. }),
                                    None => true,
                                };
                                if !root_ok {
                                    self.err(v.span, "a projection can only yield a place reachable from its parameters; local values die when the function returns");
                                }
                                if yk == YieldKind::Mut && !p.mutable {
                                    self.err(v.span, format!("cannot yield {} mutably; it is not mutable", p.desc));
                                }
                            }
                            None => self.err(v.span, "`yield` needs a place (a variable, field or element), not a temporary value"),
                        }
                        TStmt::Yield(Some(te), sp)
                    }
                }
            }
            StmtKind::Break(label) | StmtKind::Continue(label) => {
                let is_break = matches!(s.kind, StmtKind::Break(_));
                let word = if is_break { "break" } else { "continue" };
                // in a lazy chain's body: see FnCx::lazy_loops
                let redirect = match label {
                    None if is_break => cx.lazy_loops.iter().rev().find(|r| cx.loops.last().and_then(|l| l.0.as_deref()) == Some(r.0.as_str())).map(|r| r.1.clone()),
                    Some((l, _)) if !is_break => cx.lazy_loops.iter().rev().find(|r| r.2.as_deref() == Some(l.as_str())).map(|r| r.0.clone()),
                    _ => None,
                };
                if let Some(target) = redirect {
                    if let Some((_, id)) = cx.loops.iter().rev().find(|(n, _)| n.as_deref() == Some(target.as_str())) {
                        return Some(if is_break { TStmt::Break(*id, sp) } else { TStmt::Continue(*id, sp) });
                    }
                }
                let id = match label {
                    None => match cx.loops.last() {
                        Some((_, id)) => *id,
                        None if cx.par_frames.last() == Some(&cx.depth()) => {
                            if is_break {
                                self.diags.push(Diag::new(sp, "`break` can't stop a `par for`: its iterations run at the same time").note(None, "`continue` ends this iteration"));
                                return None;
                            }
                            // the body is a function per element
                            return Some(TStmt::Return(None, sp));
                        }
                        None => {
                            self.err(sp, format!("`{}` outside of a loop", word));
                            return None;
                        }
                    },
                    Some((l, lsp)) => match cx.loops.iter().rev().find(|(n, _)| n.as_deref() == Some(l.as_str())) {
                        Some((_, id)) => *id,
                        None => {
                            self.err(*lsp, format!("no enclosing loop is labeled `{}`", l));
                            return None;
                        }
                    },
                };
                if is_break {
                    TStmt::Break(id, sp)
                } else {
                    TStmt::Continue(id, sp)
                }
            }
            StmtKind::ErrDefer(inner) => {
                let rr = {
                    let r = cx.ret.clone();
                    self.resolve(cx, &r)
                };
                if !matches!(rr, Ty::Result(..) | Ty::Option(_) | Ty::Error) || !cx.frames.is_empty() {
                    self.err(sp, "`errdefer` is only allowed in functions that return a result (`T ! E`) or an optional value");
                }
                self.push_scope(cx);
                let saved = std::mem::take(&mut cx.loops);
                cx.defer_depth += 1;
                let t = self.stmt(cx, inner);
                cx.defer_depth -= 1;
                cx.loops = saved;
                self.pop_scope(cx);
                TStmt::ErrDefer(TBlock { stmts: t.into_iter().collect(), value: None, span: inner.span })
            }
            StmtKind::Defer(inner) => {
                self.push_scope(cx);
                let saved = std::mem::take(&mut cx.loops);
                cx.defer_depth += 1;
                let t = self.stmt(cx, inner);
                cx.defer_depth -= 1;
                cx.loops = saved;
                self.pop_scope(cx);
                TStmt::Defer(TBlock { stmts: t.into_iter().collect(), value: None, span: inner.span })
            }
            StmtKind::For(f, label) => {
                if let Some((l, lsp)) = label {
                    if cx.loops.iter().any(|(n, _)| n.as_deref() == Some(l.as_str())) {
                        self.err(*lsp, format!("label `{}` is already used by an enclosing loop", l));
                    }
                }
                cx.pending_label = label.as_ref().map(|l| l.0.clone());
                return self.for_loop(cx, f, sp);
            }
            StmtKind::Send { chan, value } => {
                self.no_wait_under_lock(cx, sp, "this send can wait");
                let ch = self.expr(cx, chan, None);
                let ch = self.borrow(cx, ch);
                let cty = self.resolve(cx, &ch.ty);
                let elem = self.sender_elem(&cty, chan.span, &ast_text(chan));
                let v = self.expr(cx, value, Some(&elem));
                let v = self.coerce(cx, v, &elem, Use::Value);
                TStmt::Send { chan: ch, value: v }
            }
            StmtKind::With { name, name_span, expr, body } => {
                let wx = cx.with_value.take().unwrap_or(Expect::NoValue);
                let te = self.expr(cx, expr, None);
                let info = self.place_info(cx, &te);
                let lock = is_lock_proj(&self.prog, &te);
                let lock_writes = lock_kind(&self.prog, &te) == Some(true);
                let mutable = match &info {
                    Some(p) => p.mutable || lock_writes,
                    None => {
                        self.err(expr.span, "`with` binds a reference to a place (a projection, field or element), not a temporary");
                        false
                    }
                };
                let ty = self.resolve(cx, &te.ty);
                let root = self.place_root(cx, &te);
                let te = self.borrow(cx, te);
                self.push_scope(cx);
                let id = self.declare(cx, name, ty, LocalKind::Ref { mutable }, *name_span, false);
                cx.locals[id as usize].ref_root = root.map(|r| cx.locals[r as usize].ref_root.unwrap_or(r));
                if lock {
                    cx.locked.push((cx.depth(), lock_name(expr)));
                }
                let b = self.block_in_scope(cx, body, wx.clone(), false);
                if lock {
                    cx.locked.pop();
                }
                self.pop_scope(cx);
                TStmt::With { local: id, place: te, lock, body: b }
            }
            StmtKind::Select(arms) => {
                self.no_wait_under_lock(cx, sp, "this `select` waits");
                return self.select_stmt(cx, arms, sp);
            }
            StmtKind::WithLocks { names, mutexes, body } => {
                let wx = cx.with_value.take().unwrap_or(Expect::NoValue);
                if names.len() != mutexes.len() {
                    self.err(sp, format!("`with` binds {} name(s) to {} lock(s)", names.len(), mutexes.len()));
                    return None;
                }
                if mutexes.len() < 2 {
                    self.err(sp, "`lock(...)` takes two or more mutexes; for one, write `with v = m.lock() { }`");
                }
                for (i, m) in mutexes.iter().enumerate() {
                    if ast_is_place(m) && mutexes[..i].iter().any(|o| ast_is_place(o) && ast_text(o) == ast_text(m) && ast_text(m) != "ch") {
                        self.err(m.span, format!("`lock(...)` is given `{}` twice: a task can't hold a lock twice", ast_text(m)));
                    }
                }
                self.push_scope(cx);
                let mut locals = vec![];
                let mut places = vec![];
                for ((n, nsp), m) in names.iter().zip(mutexes) {
                    let call = Expr { kind: ExprKind::Call(Box::new(Expr { kind: ExprKind::Field(Box::new(m.clone()), "lock".into(), m.span), span: m.span }), vec![]), span: m.span };
                    let te = self.expr(cx, &call, None);
                    if !matches!(te.kind, TK::Error) && !is_lock_proj(&self.prog, &te) {
                        self.err(m.span, "`lock(...)` takes mutexes (`Mutex[T]`, or `Mutex(&x)`)");
                    }
                    let ty = self.resolve(cx, &te.ty);
                    let root = self.place_root(cx, &te);
                    let te = self.borrow(cx, te);
                    let id = self.declare(cx, n, ty, LocalKind::Ref { mutable: true }, *nsp, false);
                    cx.locals[id as usize].ref_root = root.map(|r| cx.locals[r as usize].ref_root.unwrap_or(r));
                    locals.push(id);
                    places.push(te);
                }
                cx.locked.push((cx.depth(), names.iter().map(|n| n.0.clone()).collect::<Vec<_>>().join(", ")));
                let b = self.block_in_scope(cx, body, wx.clone(), false);
                cx.locked.pop();
                self.pop_scope(cx);
                TStmt::WithLocks { locals, places, body: b }
            }
            StmtKind::ParFor { first, second, iter, by_ref, body } => return self.par_for(cx, first, second.as_ref(), iter, *by_ref, body, sp),
        })
    }

    fn check_must_use(&mut self, cx: &mut FnCx, te: &TExpr) {
        let t = self.resolve(cx, &te.ty);
        if let Ty::Result(..) = t {
            if !matches!(te.kind, TK::If { .. } | TK::Match { .. } | TK::Block(_)) {
                let s = self.prog.show(&t);
                self.diags.push(
                    Diag::new(te.span, format!("result of type `{}` is not handled", s)).note(None, "handle it with `try`, `catch` or `match`, or discard it explicitly with `_ = ...`"),
                );
            }
        }
    }

    fn assign(&mut self, cx: &mut FnCx, place: &Expr, op: &str, value: &Expr, sp: Span) -> Option<TStmt> {
        // map insert `m[k] = v`
        if let ExprKind::Index(base, idx) = &place.kind {
            if idx.len() == 1 {
                let b = self.expr(cx, base, None);
                let bty = self.resolve(cx, &b.ty);
                if let Ty::Map(k, v) = &bty {
                    let (k, v) = ((**k).clone(), (**v).clone());
                    if op == "=" {
                        if !self.require_mut(cx, &b, "insert into") {
                            return None;
                        }
                        let key = self.expr(cx, &idx[0], Some(&k));
                        let key = self.coerce(cx, key, &k, Use::Value);
                        let val = self.expr(cx, value, Some(&v));
                        let val = self.coerce(cx, val, &v, Use::Value);
                        return Some(TStmt::MapInsert { map: b, key, value: val });
                    }
                }
            }
        }
        let p = self.expr(cx, place, None);
        if matches!(p.ty, Ty::Error) {
            let _ = self.expr(cx, value, None);
            return None;
        }
        if !self.require_mut(cx, &p, "assign to") {
            let _ = self.expr(cx, value, None);
            return None;
        }
        let pty = self.resolve(cx, &p.ty);
        if op == "=" {
            let v = self.expr(cx, value, Some(&pty));
            let v = self.coerce(cx, v, &pty, Use::Value);
            return Some(TStmt::Assign { place: p, op: None, value: v, span: sp });
        }
        let bop = BinOp::from_str(&op[..op.len() - 1]).unwrap();
        if pty == Ty::String && bop == BinOp::Add {
            let v = self.expr(cx, value, Some(&Ty::Str));
            let v = self.coerce(cx, v, &Ty::Str, Use::Borrow);
            return Some(TStmt::Assign { place: p, op: Some(bop), value: v, span: sp });
        }
        let v = self.expr(cx, value, Some(&pty));
        let v = self.coerce(cx, v, &pty, Use::Value);
        self.check_arith(cx, bop, &pty, &v.ty.clone(), sp);
        Some(TStmt::Assign { place: p, op: Some(bop), value: v, span: sp })
    }

    fn check_arith(&mut self, cx: &mut FnCx, op: BinOp, l: &Ty, r: &Ty, sp: Span) {
        let l = self.resolve(cx, l);
        let r = self.resolve(cx, r);
        if l.is_error() || r.is_error() {
            return;
        }
        let ok = match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => l.is_numeric() || self.param_numeric(&l) || self.numeric_newtype(&l).is_some(),
            BinOp::WAdd | BinOp::WSub | BinOp::WMul | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => l.is_int() || self.numeric_newtype(&l).map_or(false, |i| i.is_int()),
            _ => true,
        };
        if !ok {
            let s = self.prog.show(&l);
            self.err(sp, format!("operator not supported for type `{}`", s));
        }
    }

    fn param_numeric(&self, t: &Ty) -> bool {
        if let Ty::Param(p) = t {
            return self.prog.tparam(*p).bounds.iter().any(|b| {
                if let Bound::Iface(d, _) = b {
                    if let TypeDefKind::Iface { type_set: Some(ts), .. } = &self.prog.def(*d).kind {
                        return ts.iter().all(|(_, t)| t.is_numeric());
                    }
                }
                false
            });
        }
        false
    }

    fn enter_loop(&mut self, cx: &mut FnCx) -> u32 {
        let id = cx.next_loop;
        cx.next_loop += 1;
        let label = cx.pending_label.take();
        cx.loops.push((label, id));
        id
    }

    fn for_loop(&mut self, cx: &mut FnCx, f: &ast::ForLoop, sp: Span) -> Option<TStmt> {
        match f {
            ast::ForLoop::Infinite(b) => {
                let id = self.enter_loop(cx);
                let body = self.block(cx, b, Expect::NoValue);
                cx.loops.pop();
                Some(TStmt::While { id, cond: None, body })
            }
            ast::ForLoop::Cond(c, b) => {
                let label = cx.pending_label.take();
                let cond = self.expr_value(cx, c, Some(&Ty::Bool));
                cx.pending_label = label;
                let id = self.enter_loop(cx);
                let body = self.block(cx, b, Expect::NoValue);
                cx.loops.pop();
                Some(TStmt::While { id, cond: Some(cond), body })
            }
            ast::ForLoop::In { first, second, iter, by_ref, body } => {
                self.push_scope(cx);
                let r = self.for_in(cx, first, second.as_ref(), iter, *by_ref, body, sp);
                self.pop_scope(cx);
                r
            }
        }
    }

    fn for_in(&mut self, cx: &mut FnCx, first: &(String, Span), second: Option<&(String, Span)>, iter: &Expr, by_ref: bool, body: &ast::Block, sp: Span) -> Option<TStmt> {
        if let Some(r) = self.lazy_for(cx, first, second, iter, by_ref, body, sp) {
            return r;
        }
        if let ExprKind::Range { lo, hi, inclusive } = &iter.kind {
            let l = self.expr(cx, lo, None);
            let l = self.value(cx, l);
            let lt = self.resolve(cx, &l.ty);
            let it = if lt.is_int() { lt } else { Ty::int() };
            let l = self.coerce(cx, l, &it, Use::Value);
            let h = self.expr_value(cx, hi, Some(&it));
            if second.is_some() {
                self.err(iter.span, "a range yields one value per iteration");
            }
            let elem = self.declare(cx, &first.0, it, LocalKind::Let, first.1, false);
            let id = self.enter_loop(cx);
            let b = self.block(cx, body, Expect::NoValue);
            cx.loops.pop();
            return Some(TStmt::For { id, kind: IterKind::Range { inclusive: *inclusive }, iter: l, hi: Some(h), elem, index: None, mutable: false, body: b, span: sp });
        }
        let it = self.expr(cx, iter, None);
        let ity = self.resolve(cx, &it.ty);
        let iter_root = self.place_root(cx, &it).map(|r| cx.locals[r as usize].ref_root.unwrap_or(r));
        if by_ref {
            self.require_mut(cx, &it, "mutably iterate over");
        }
        let it = self.borrow(cx, it);
        let (kind, elem_ty, idx_ty, elem_kind) = match &ity {
            Ty::Vec(t) => (IterKind::Vec, (**t).clone(), Some(Ty::int()), LocalKind::Ref { mutable: by_ref }),
            Ty::Slice(t) => (IterKind::Slice, (**t).clone(), Some(Ty::int()), LocalKind::Ref { mutable: by_ref }),
            Ty::Array(t, _) | Ty::ArrayT(t, _) => (IterKind::Array, (**t).clone(), Some(Ty::int()), LocalKind::Ref { mutable: by_ref }),
            Ty::Map(k, v) => (IterKind::Map, (**v).clone(), Some((**k).clone()), LocalKind::Ref { mutable: by_ref }),
            Ty::Str | Ty::String => {
                if by_ref {
                    self.err(iter.span, "strings cannot be iterated mutably");
                }
                (IterKind::Str, Ty::Int(IntTy::I32), Some(Ty::int()), LocalKind::Let)
            }
            Ty::Receiver(t) => (IterKind::Chan, (**t).clone(), None, LocalKind::Let),
            Ty::Adt(d, _) if Some(*d) == self.prog.prelude_type("Chan") => {
                let w = ast_text(iter);
                self.diags.push(Diag::new(iter.span, format!("iterate over the channel's receiving end: `for x in {}.rx`", w)).note(None, format!("the loop ends once every sender (`{}.tx` and its clones) is gone and the buffer is empty", w)));
                return None;
            }
            Ty::Error => return None,
            _ => return self.for_custom(cx, first, second, iter, it, &ity, by_ref, body, sp),
        };
        let (index, elem) = if kind == IterKind::Map {
            let k = self.declare(cx, &first.0, idx_ty.clone().unwrap(), LocalKind::Ref { mutable: false }, first.1, false);
            let v = match second {
                Some(s) => self.declare(cx, &s.0, elem_ty, elem_kind, s.1, false),
                None => self.hidden_local(cx, elem_ty, elem_kind, first.1),
            };
            (Some(k), v)
        } else {
            match second {
                Some(s) => {
                    let it = match idx_ty {
                        Some(t) => t,
                        None => {
                            self.err(s.1, "channels yield one value per iteration");
                            Ty::Error
                        }
                    };
                    let i = self.declare(cx, &first.0, it, LocalKind::Let, first.1, false);
                    let e = self.declare(cx, &s.0, elem_ty, elem_kind, s.1, false);
                    (Some(i), e)
                }
                None => (None, self.declare(cx, &first.0, elem_ty, elem_kind, first.1, false)),
            }
        };
        for l in [Some(elem), index].into_iter().flatten() {
            if matches!(cx.locals[l as usize].kind, LocalKind::Ref { .. }) {
                cx.locals[l as usize].ref_root = iter_root;
            }
        }
        let id = self.enter_loop(cx);
        let b = self.block(cx, body, Expect::NoValue);
        cx.loops.pop();
        Some(TStmt::For { id, kind, iter: it, hi: None, elem, index, mutable: by_ref, body: b, span: sp })
    }

    /// `for x in t` over a user type:
    /// - pull: `cursor(self) C` + `next(self, c inout C) yield ?&T` (or returning `?T`);
    ///   `nextMut` for `for x in &t`. Rewritten into a plain loop, so every kind of
    ///   control flow works as usual.
    /// - push: `each(self, body fn(T) Flow)` (`eachMut` for `&t`). The body becomes a
    ///   closure; break / continue / return / try inside it are routed out by codegen.
    #[allow(clippy::too_many_arguments)]
    fn for_custom(&mut self, cx: &mut FnCx, first: &(String, Span), second: Option<&(String, Span)>, iter: &Expr, it: TExpr, ity: &Ty, by_ref: bool, body: &ast::Block, sp: Span) -> Option<TStmt> {
        let has = |c: &Checker, name: &str| TyKey::of(ity).map(|k| c.prog.methods.contains_key(&(k, name.to_string()))).unwrap_or(false);
        let next = if by_ref { "nextMut" } else { "next" };
        let each = if by_ref { "eachMut" } else { "each" };
        let label = cx.pending_label.take();
        if has(self, "cursor") && has(self, next) {
            cx.hidden += 1;
            let n = cx.hidden;
            let e = |k: ExprKind| Expr { kind: k, span: iter.span };
            let ident = |name: &str| Expr { kind: ExprKind::Ident(name.to_string()), span: iter.span };
            let is_path = {
                fn path(x: &Expr) -> bool {
                    match &x.kind {
                        ExprKind::Ident(_) => true,
                        ExprKind::Field(b, _, _) => path(b),
                        _ => false,
                    }
                }
                path(iter)
            };
            let mut stmts = vec![];
            let target = if is_path {
                iter.clone()
            } else {
                let name = format!("__it{}", n);
                stmts.push(Stmt { kind: StmtKind::Let { name: name.clone(), name_span: iter.span, ty: None, init: Some(iter.clone()), mutable: by_ref }, span: iter.span });
                ident(&name)
            };
            let cname = format!("__c{}", n);
            let method = |m: &str, args: Vec<Expr>| e(ExprKind::Call(Box::new(e(ExprKind::Field(Box::new(target.clone()), m.to_string(), iter.span))), args));
            stmts.push(Stmt { kind: StmtKind::Let { name: cname.clone(), name_span: iter.span, ty: None, init: Some(method("cursor", vec![])), mutable: true }, span: iter.span });
            let iname = format!("__i{}", n);
            let (elem, index) = match second {
                Some(s2) => (s2.clone(), Some(first.clone())),
                None => (first.clone(), None),
            };
            let mut inner = vec![];
            if let Some((iv, isp)) = &index {
                stmts.push(Stmt { kind: StmtKind::Let { name: iname.clone(), name_span: *isp, ty: None, init: Some(e(ExprKind::Int(0))), mutable: true }, span: *isp });
                inner.push(Stmt { kind: StmtKind::Let { name: iv.clone(), name_span: *isp, ty: None, init: Some(ident(&iname)), mutable: false }, span: *isp });
                inner.push(Stmt { kind: StmtKind::Assign { place: ident(&iname), op: "+=", value: e(ExprKind::Int(1)) }, span: *isp });
            }
            inner.extend(body.stmts.iter().cloned());
            let step = method(next, vec![e(ExprKind::Ref(Box::new(ident(&cname))))]);
            let if_let = e(ExprKind::IfLet {
                pat: ast::Pattern { kind: ast::PatternKind::Ident(elem.0.clone()), span: elem.1 },
                expr: Box::new(step),
                then: ast::Block { stmts: inner, span: body.span },
                else_: Some(Box::new(e(ExprKind::Block(ast::Block { stmts: vec![Stmt { kind: StmtKind::Break(None), span: sp }], span: sp })))),
            });
            let lp = ast::ForLoop::Infinite(ast::Block { stmts: vec![Stmt { kind: StmtKind::Expr(if_let), span: sp }], span: body.span });
            stmts.push(Stmt { kind: StmtKind::For(lp, label.map(|l| (l, sp))), span: sp });
            let tb = self.block(cx, &ast::Block { stmts, span: sp }, Expect::NoValue);
            return Some(TStmt::Expr(TExpr::new(TK::Block(tb), Ty::Unit, sp)));
        }
        if has(self, each) {
            let id = cx.next_loop;
            cx.next_loop += 1;
            let mut params = vec![ast::ClosureParam { name: first.0.clone(), mode: Mode::Borrow, ty: None, span: first.1 }];
            if let Some(s2) = second {
                params.push(ast::ClosureParam { name: s2.0.clone(), mode: Mode::Borrow, ty: None, span: s2.1 });
            }
            let closure = Expr { kind: ExprKind::Closure { params, ret: None, body: Box::new(Expr { kind: ExprKind::Block(body.clone()), span: body.span }) }, span: body.span };
            cx.loop_body_next = Some((id, label));
            let call = self.method_call_on(cx, it, each, iter.span, None, &[closure], sp, None);
            let took = cx.loop_body_next.take().is_none();
            if let TK::Call { args, .. } = &call.kind {
                let ok = took && matches!(args.last(), Some(TArg { e: TExpr { kind: TK::Closure(c), .. }, mode: ArgMode::Borrow }) if c.loop_body == Some(id));
                if !ok {
                    let s = self.prog.show(ity);
                    self.err(iter.span, format!("`{}.{}` must take the loop body as its last parameter: `fn {}(self, body: fn(T) -> Flow)`", s, each, each));
                    return None;
                }
                let rt = self.resolve(cx, match args.last() {
                    Some(a) => match &a.e.ty {
                        Ty::Func(f) => &f.ret,
                        _ => &a.e.ty,
                    },
                    None => &Ty::Error,
                });
                let flow = self.prog.prelude_type("Flow").map(|d| Ty::Adt(d, Rc::new(vec![])));
                if Some(&rt) != flow.as_ref() {
                    let s = self.prog.show(&rt);
                    self.err(iter.span, format!("the loop body passed to `{}` must return `Flow`, not `{}`", each, s));
                }
            } else {
                return None;
            }
            return Some(TStmt::ForEach { id, call });
        }
        let s = self.prog.show(ity);
        self.diags.push(
            Diag::new(iter.span, format!("cannot iterate over `{}`", s))
                .note(None, format!("make it iterable with `fn cursor(self) -> C` and `fn {}({}self, c: inout C) yield ?&T`, or with `fn {}({}self, body: fn(T) -> Flow)`", next, if by_ref { "inout " } else { "" }, each, if by_ref { "inout " } else { "" })),
        );
        None
    }

    /// `par for x in &v { }`: the body becomes a `send fn` called for each
    /// element, from several tasks at once.
    fn par_for(&mut self, cx: &mut FnCx, first: &(String, Span), second: Option<&(String, Span)>, iter: &Expr, by_ref: bool, body: &ast::Block, sp: Span) -> Option<TStmt> {
        let mut params = vec![];
        let mut ptys = vec![];
        let (kind, it, hi) = if let ExprKind::Range { lo, hi, inclusive } = &iter.kind {
            if second.is_some() || by_ref {
                self.err(iter.span, "a range yields one value per iteration: `par for i in 0..n { }`");
            }
            let l = self.expr(cx, lo, None);
            let l = self.value(cx, l);
            let lt = self.resolve(cx, &l.ty);
            let t = if lt.is_int() { lt } else { Ty::int() };
            let l = self.coerce(cx, l, &t, Use::Value);
            let h = self.expr_value(cx, hi, Some(&t));
            ptys.push((Mode::Borrow, t));
            (IterKind::Range { inclusive: *inclusive }, l, Some(h))
        } else {
            let it = self.expr(cx, iter, None);
            let ity = self.resolve(cx, &it.ty);
            if by_ref {
                self.require_mut(cx, &it, "mutably iterate over");
            }
            let it = self.borrow(cx, it);
            let (kind, et) = match &ity {
                Ty::Vec(t) => (IterKind::Vec, (**t).clone()),
                Ty::Slice(t) => (IterKind::Slice, (**t).clone()),
                Ty::Array(t, _) | Ty::ArrayT(t, _) => (IterKind::Array, (**t).clone()),
                Ty::Error => return None,
                _ => {
                    let s = self.prog.show(&ity);
                    self.err(iter.span, format!("`par for` runs over a range, a `Vec`, a slice or an array, not `{}`", s));
                    return None;
                }
            };
            if !self.is_send(&et) {
                let s = self.prog.show(&et);
                self.err(iter.span, format!("`par for` hands elements of type `{}` to other tasks, but it isn't `send`", s));
            }
            if second.is_some() {
                ptys.push((Mode::Borrow, Ty::int()));
            }
            ptys.push((if by_ref { Mode::Inout } else { Mode::Borrow }, et));
            (kind, it, None)
        };
        for (n, nsp) in [Some(first), second].into_iter().flatten() {
            params.push(ast::ClosureParam { name: n.clone(), mode: Mode::Borrow, ty: None, span: *nsp });
        }
        if params.len() != ptys.len() {
            return None;
        }
        let clo = Expr { kind: ExprKind::Closure { params, ret: None, body: Box::new(Expr { kind: ExprKind::Block(body.clone()), span: body.span }) }, span: body.span };
        let fty = Ty::Func(Rc::new(FuncTy { params: ptys, ret: Ty::Unit, send: true }));
        cx.par_frames.push(cx.depth() + 1);
        let b = self.expr(cx, &clo, Some(&fty));
        cx.par_frames.pop();
        let indexed = second.is_some();
        Some(TStmt::ParFor { kind, iter: it, hi, mutable: by_ref, indexed, body: b, span: sp })
    }

    /// `time.after(d)`, `time.at(t)` and `cancelled` as `select` arms.
    fn select_event(&mut self, cx: &mut FnCx, e: &Expr) -> Option<TSelectKind> {
        if let ExprKind::Ident(n) = &e.kind {
            if n == "cancelled" {
                return Some(TSelectKind::Cancelled);
            }
        }
        if let ExprKind::Call(f, args) = &e.kind {
            if let ExprKind::Field(base, name, _) = &f.kind {
                let is_time = matches!(self.resolve_path(cx, base), Some(PathRes::Pkg(p, _)) if p != u32::MAX && self.prog.packages[p as usize].name == "time" && self.prog.packages[p as usize].is_std);
                if is_time && (name == "after" || name == "at") && args.len() == 1 {
                    let at = self.expr_value(cx, &args[0], Some(&Ty::int()));
                    return Some(TSelectKind::Timer { at, relative: name == "after" });
                }
            }
        }
        self.diags.push(Diag::new(e.span, "this is not a `select` arm").note(None, "arms are `let x = <-rx`, `<-rx`, `tx <- v`, `let r = <-task`, `time.after(d)`, `time.at(t)`, `cancelled` and `_`, each optionally followed by `if cond`"));
        None
    }

    fn select_stmt(&mut self, cx: &mut FnCx, arms: &[ast::SelectArm], sp: Span) -> Option<TStmt> {
        let mut out = vec![];
        let mut defaults = 0;
        for a in arms {
            self.push_scope(cx);
            // evaluated before waiting; a false guard leaves the arm out
            let guard = a.guard.as_ref().map(|g| self.expr_value(cx, g, Some(&Ty::Bool)));
            let kind = match &a.kind {
                ast::SelectKind::Recv { bind, chan } => {
                    let ch = self.expr(cx, chan, None);
                    let cty = self.resolve(cx, &ch.ty);
                    if let Ty::Task(r) = &cty {
                        // `let r = <-t`: the task has finished
                        let r = (**r).clone();
                        if !matches!(ch.kind, TK::Local(_)) {
                            self.err(chan.span, "a `select` arm waits on a task handle held in a variable: `let r = <-t =>`");
                        }
                        let b = bind.as_ref().map(|(n, s)| self.declare(cx, n, r.clone(), LocalKind::Let, *s, false));
                        TSelectKind::Task { task: ch, bind: b }
                    } else {
                        let ch = self.borrow(cx, ch);
                        let elem = self.receiver_elem(&cty, chan.span, &ast_text(chan));
                        let b = bind.as_ref().map(|(n, s)| self.declare(cx, n, Ty::opt(elem.clone()), LocalKind::Let, *s, false));
                        TSelectKind::Recv { chan: ch, bind: b }
                    }
                }
                ast::SelectKind::Event(e) => match self.select_event(cx, e) {
                    Some(k) => k,
                    None => {
                        self.pop_scope(cx);
                        continue;
                    }
                },
                ast::SelectKind::Send { chan, value } => {
                    let ch = self.expr(cx, chan, None);
                    let ch = self.borrow(cx, ch);
                    let cty = self.resolve(cx, &ch.ty);
                    let elem = self.sender_elem(&cty, chan.span, &ast_text(chan));
                    let v = self.expr(cx, value, Some(&elem));
                    let v = self.coerce(cx, v, &elem, Use::Value);
                    TSelectKind::Send { chan: ch, value: v }
                }
                ast::SelectKind::Default => {
                    defaults += 1;
                    TSelectKind::Default
                }
            };
            let body = self.block_in_scope(cx, &a.body, Expect::NoValue, false);
            self.pop_scope(cx);
            out.push(TSelectArm { kind, guard, body });
        }
        if defaults > 1 {
            self.err(sp, "`select` has more than one `_` arm");
        }
        if out.iter().filter(|a| matches!(a.kind, TSelectKind::Cancelled)).count() > 1 {
            self.err(sp, "`select` has more than one `cancelled` arm");
        }
        Some(TStmt::Select { arms: out, span: sp })
    }

    // ------------------------------------------------------------------
    // Expressions
    // ------------------------------------------------------------------

    /// Expression in statement position: if/match/blocks don't produce values.
    fn expr_novalue(&mut self, cx: &mut FnCx, e: &Expr) -> TExpr {
        match &e.kind {
            ExprKind::If { .. } | ExprKind::IfLet { .. } | ExprKind::Match { .. } | ExprKind::Block(_) | ExprKind::Unsafe(_) | ExprKind::With(_) | ExprKind::LetElse { .. } => self.expr_x(cx, e, Expect::NoValue),
            ExprKind::Scope { name, body, implicit } => self.scope_expr(cx, name.as_ref(), body, *implicit, Expect::NoValue, e.span),
            _ => self.expr(cx, e, None),
        }
    }

    pub fn expr(&mut self, cx: &mut FnCx, e: &Expr, expect: Option<&Ty>) -> TExpr {
        let ex = match expect {
            Some(t) => Expect::Ty(self.resolve(cx, t)),
            None => Expect::Infer,
        };
        self.expr_x(cx, e, ex)
    }

    pub fn expr_x(&mut self, cx: &mut FnCx, e: &Expr, ex: Expect) -> TExpr {
        let sp = e.span;
        let expect = ex.ty().cloned();
        match &e.kind {
            ExprKind::Int(n) => match self.literal_value(*n, false, sp) {
                Some(v) => self.int_lit(cx, v, expect.as_ref(), sp),
                None => TExpr::error(sp),
            },
            ExprKind::Float(f) => {
                let t = match expect {
                    Some(Ty::F32) => Ty::F32,
                    _ => Ty::F64,
                };
                // rounding is fine; overflowing to infinity is not
                let max = if t == Ty::F32 { f32::MAX as f64 } else { f64::MAX };
                if !f.is_finite() || f.abs() > max {
                    self.err(sp, format!("float literal is out of range for `{}`", self.prog.show(&t)));
                    return TExpr::error(sp);
                }
                TExpr::new(TK::Float(*f), t, sp)
            }
            ExprKind::Str(s) => TExpr::new(TK::Str(s.clone()), Ty::Str, sp),
            ExprKind::Interp(parts) => self.interp(cx, parts, sp),
            ExprKind::With(inner) => self.with_expr(cx, inner, ex, sp),
            ExprKind::LetElse { pat, expr, else_, rest } => {
                // `match expr { pat => { rest }, _ => { else } }`, where the else
                // branch has to leave
                let arms = vec![
                    ast::Arm { pat: pat.clone(), guard: None, body: Expr { kind: ExprKind::Block(rest.clone()), span: rest.span }, span: pat.span },
                    ast::Arm { pat: ast::Pattern { kind: ast::PatternKind::Wild, span: else_.span }, guard: None, body: Expr { kind: ExprKind::Block(else_.clone()), span: else_.span }, span: else_.span },
                ];
                let m = Expr { kind: ExprKind::Match { scrut: expr.clone(), arms }, span: sp };
                let before = self.diags.len();
                let te = self.expr_x(cx, &m, ex);
                if let TK::Match { arms, .. } = &te.kind {
                    let et = self.resolve(cx, &arms[1].body.ty);
                    let inside = |d: &Diag| d.span.file == else_.span.file && d.span.lo >= else_.span.lo && d.span.hi <= else_.span.hi && d.msg.starts_with("mismatched types");
                    let mismatch = self.diags[before..].iter().any(inside);
                    if arms.len() == 2 && et != Ty::Never && (mismatch || !et.is_error()) {
                        let mut i = before;
                        while i < self.diags.len() {
                            if inside(&self.diags[i]) {
                                self.diags.remove(i);
                            } else {
                                i += 1;
                            }
                        }
                        self.diags.push(Diag::new(else_.span, "the `else` of `let ... else` must leave: end it with `return`, `break`, `continue` or a panic").note(None, "the names the pattern binds exist only after the `let`, so the code can't continue without them"));
                    }
                }
                te
            }
            ExprKind::Tuple(xs) => {
                let Some(d) = self.tuple_def(xs.len()) else {
                    self.err(sp, "tuples have 2 to 6 elements; use a struct for more");
                    return TExpr::error(sp);
                };
                // the tuple type expected here, or one to infer
                let ty = match expect.as_ref().map(|t| self.resolve(cx, t)) {
                    Some(t @ Ty::Adt(e, _)) if e == d => t,
                    Some(Ty::Option(t)) if matches!(&*t, Ty::Adt(e, _) if *e == d) => (*t).clone(),
                    Some(Ty::Result(t, _)) if matches!(&*t, Ty::Adt(e, _) if *e == d) => (*t).clone(),
                    _ => Ty::Adt(d, Rc::new(xs.iter().map(|_| self.fresh(cx)).collect())),
                };
                let fields: Vec<(String, Span, Expr)> = xs.iter().enumerate().map(|(i, x)| (format!("_{}", i), x.span, x.clone())).collect();
                let infer = Expr { kind: ExprKind::Ident("_".into()), span: sp };
                self.struct_lit(cx, &infer, &fields, &[], &[], sp, Some(&ty))
            }
            ExprKind::Char(c) => {
                let t = match &expect {
                    Some(Ty::Int(k)) => Ty::Int(*k),
                    _ => Ty::Int(IntTy::I32),
                };
                self.int_lit(cx, *c as i128, Some(&t), sp)
            }
            ExprKind::Bool(b) => TExpr::new(TK::Bool(*b), Ty::Bool, sp),
            ExprKind::None => match &expect {
                Some(t @ Ty::Option(_)) => TExpr::new(TK::NoneLit, t.clone(), sp),
                // `?T ! E`: the success value `none` (wrapped in `ok` like any other)
                Some(Ty::Result(ok, _)) if matches!(**ok, Ty::Option(_)) => TExpr::new(TK::NoneLit, (**ok).clone(), sp),
                Some(Ty::Var(_)) | None => {
                    let v = self.fresh(cx);
                    let t = Ty::opt(v);
                    if let Some(e) = &expect {
                        self.unify(cx, e, &t);
                    }
                    TExpr::new(TK::NoneLit, t, sp)
                }
                Some(t) => {
                    let s = self.prog.show(t);
                    self.err(sp, format!("`none` is not a value of type `{}`; only `?T` values can be none", s));
                    TExpr::error(sp)
                }
            },
            ExprKind::Ident(name) => self.ident(cx, name, sp, expect.as_ref()),
            ExprKind::Field(base, name, nsp) => self.field(cx, e, base, name, *nsp, expect.as_ref()),
            ExprKind::OptField(base, name) => {
                let b = self.expr(cx, base, None);
                let bty = self.resolve(cx, &b.ty);
                let inner = match &bty {
                    Ty::Option(t) => (**t).clone(),
                    Ty::Error => return TExpr::error(sp),
                    _ => {
                        let s = self.prog.show(&bty);
                        self.err(sp, format!("`?.` needs an optional value, found `{}`", s));
                        return TExpr::error(sp);
                    }
                };
                let (d, args) = match &inner {
                    Ty::Adt(d, a) if self.prog.def(*d).is_struct() && self.prog.field_tys(*d, a).iter().any(|f| f.0 == *name) => (*d, a.clone()),
                    _ => return self.opt_member(cx, b, inner, name, sp),
                };
                let fields = self.prog.field_tys(d, &args);
                let idx = match fields.iter().position(|f| f.0 == *name) {
                    Some(i) => i,
                    None => {
                        let s = self.prog.show(&inner);
                        self.err(sp, format!("`{}` has no field `{}`", s, name));
                        return TExpr::error(sp);
                    }
                };
                let fty = fields[idx].1.clone();
                let b = if b.is_place() { self.borrow(cx, b) } else { b };
                let rty = match &fty {
                    Ty::Option(_) => fty.clone(),
                    _ => Ty::opt(fty.clone()),
                };
                TExpr::new(TK::OptField { base: Box::new(b), index: idx }, rty, sp)
            }
            ExprKind::Index(base, idx) => self.index(cx, e, base, idx, sp),
            ExprKind::TypeArgs(..) => match self.resolve_path(cx, e) {
                Some(PathRes::Func(f, targs)) => self.func_value(cx, f, targs, expect.as_ref(), sp),
                _ => {
                    self.err(sp, "type used as a value");
                    TExpr::error(sp)
                }
            },
            ExprKind::Slice(base, lo, hi) => {
                let b = self.expr(cx, base, None);
                let bty = self.resolve(cx, &b.ty);
                let lo = lo.as_ref().map(|l| Box::new(self.expr_value(cx, l, Some(&Ty::int()))));
                let hi = hi.as_ref().map(|h| Box::new(self.expr_value(cx, h, Some(&Ty::int()))));
                let rty = match &bty {
                    Ty::Vec(t) | Ty::Slice(t) | Ty::Array(t, _) | Ty::ArrayT(t, _) => Ty::Slice(t.clone()),
                    Ty::Str | Ty::String => Ty::Str,
                    Ty::Error => return TExpr::error(sp),
                    _ => {
                        let s = self.prog.show(&bty);
                        self.err(sp, format!("cannot slice `{}`", s));
                        return TExpr::error(sp);
                    }
                };
                self.const_bounds(&bty, lo.as_deref(), hi.as_deref(), true);
                let b = self.borrow(cx, b);
                TExpr::new(TK::SliceOf { base: Box::new(b), lo, hi }, rty, sp)
            }
            ExprKind::Call(callee, args) => self.call(cx, callee, args, sp, expect.as_ref()),
            ExprKind::Unary(op, inner) => self.unary(cx, op, inner, sp, expect.as_ref()),
            ExprKind::Binary(op, l, r) => self.binary(cx, op, l, r, sp, expect.as_ref()),
            ExprKind::Ref(_) => {
                self.err(sp, "`&` marks an inout argument; it is only allowed as a call argument");
                TExpr::error(sp)
            }
            ExprKind::Closure { params, ret, body } => self.closure(cx, params, ret.as_ref(), body, sp, expect.as_ref()),
            ExprKind::If { cond, then, else_ } => {
                let c = self.expr_value(cx, cond, Some(&Ty::Bool));
                self.if_rest(cx, c, then, else_.as_deref(), ex, sp)
            }
            ExprKind::IfLet { pat, expr, then, else_ } => self.if_let(cx, pat, expr, then, else_.as_deref(), ex, sp),
            ExprKind::Match { scrut, arms } => self.match_expr(cx, scrut, arms, ex, sp),
            ExprKind::Block(b) => {
                let tb = self.block(cx, b, ex);
                let t = self.block_ty(&tb);
                TExpr::new(TK::Block(tb), t, sp)
            }
            ExprKind::Scope { name, body, implicit } => self.scope_expr(cx, name.as_ref(), body, *implicit, ex, sp),
            ExprKind::Go(inner) => self.go_expr(cx, inner, true, sp),
            ExprKind::Isolate(b) => self.isolate_expr(cx, b, sp),
            ExprKind::Unsafe(b) => {
                cx.unsafe_used.push(false);
                let tb = self.block(cx, b, ex);
                if cx.unsafe_used.pop() == Some(false) {
                    let lo = Span::new(sp.file, sp.lo, sp.lo + 6);
                    self.diags.push(Diag::new(lo, "this `unsafe` block has nothing that needs it").note(None, "only dereferencing a raw pointer and calling an `extern` or `unsafe fn` function need `unsafe`; remove the block"));
                }
                let t = self.block_ty(&tb);
                TExpr::new(TK::Unsafe(tb), t, sp)
            }
            ExprKind::Try { expr, wrap } => self.try_expr(cx, expr, wrap.as_deref(), sp),
            ExprKind::Catch { expr, bind, body } => {
                let inner = self.expr(cx, expr, None);
                let inner = self.value(cx, inner);
                let ity = self.resolve(cx, &inner.ty);
                let (ok, errt) = match &ity {
                    Ty::Result(a, b) => ((**a).clone(), Some((**b).clone())),
                    Ty::Option(a) => ((**a).clone(), None),
                    Ty::Error => return TExpr::error(sp),
                    _ => {
                        let s = self.prog.show(&ity);
                        self.err(expr.span, format!("`catch` needs a result (`T ! E`) or optional value, found `{}`", s));
                        return TExpr::error(sp);
                    }
                };
                self.push_scope(cx);
                let b = match (bind, &errt) {
                    (Some((n, s)), Some(et)) => Some(self.declare(cx, n, et.clone(), LocalKind::Let, *s, false)),
                    (Some((_, s)), None) => {
                        self.err(*s, "an optional value has no error to bind");
                        None
                    }
                    _ => None,
                };
                let expect_ok = match &ex {
                    Expect::NoValue if ok == Ty::Unit => Expect::NoValue,
                    _ => Expect::Ty(ok.clone()),
                };
                let h = self.block_in_scope(cx, body, expect_ok, false);
                self.pop_scope(cx);
                let hty = self.block_ty(&h);
                if !matches!(ex, Expect::NoValue) && hty != Ty::Never && ok != Ty::Unit && h.value.is_none() {
                    let s = self.prog.show(&ok);
                    self.err(body.span, format!("`catch` block must produce a value of type `{}` (or return / panic)", s));
                }
                TExpr::new(TK::Catch { e: Box::new(inner), bind: b, handler: h }, ok, sp)
            }
            ExprKind::Coalesce(l, r) => {
                let lt = self.expr(cx, l, expect.as_ref().map(|t| Ty::opt(t.clone())).as_ref());
                let before = self.diags.len();
                let lt = self.value(cx, lt);
                // `x ?? d` on a borrowed `x` (a field of `self`, an element):
                // say how to write it instead
                if self.diags.len() == before + 1 && self.diags[before].msg.starts_with("cannot move out of") {
                    let d = &mut self.diags[before];
                    let clone = d.notes.iter().any(|(_, n)| n.contains(".clone()"));
                    d.notes.clear();
                    d.notes.push((None, "`??` takes the value out of its left side, and this one is only borrowed".into()));
                    if clone {
                        d.notes.push((None, "copy it first: `x.clone() ?? default`, or borrow it: `if let v = x { ... } else { ... }`".into()));
                    } else {
                        d.notes.push((None, "borrow it instead: `if let v = x { ... } else { ... }`".into()));
                    }
                }
                let lty = self.resolve(cx, &lt.ty);
                let inner = match &lty {
                    Ty::Option(t) => (**t).clone(),
                    Ty::Error => Ty::Error,
                    _ => {
                        let s = self.prog.show(&lty);
                        self.err(l.span, format!("`??` needs an optional value on the left, found `{}`", s));
                        Ty::Error
                    }
                };
                let rt = self.expr(cx, r, Some(&inner));
                let rty = self.resolve(cx, &rt.ty);
                if matches!(rty, Ty::Option(_)) {
                    let rt = self.coerce(cx, rt, &lty, Use::Value);
                    return TExpr::new(TK::Coalesce { l: Box::new(lt), r: Box::new(rt) }, lty, sp);
                }
                let rt = self.coerce(cx, rt, &inner, Use::Value);
                TExpr::new(TK::Coalesce { l: Box::new(lt), r: Box::new(rt) }, inner, sp)
            }
            ExprKind::Range { .. } => {
                self.err(sp, "ranges are only allowed in `for` loops and slices");
                TExpr::error(sp)
            }
            ExprKind::Recv(ch) => {
                self.no_wait_under_lock(cx, sp, "this receive waits");
                let c = self.expr(cx, ch, None);
                let c = self.borrow(cx, c);
                let cty = self.resolve(cx, &c.ty);
                if cty == Ty::Error {
                    return TExpr::error(sp);
                }
                if let Ty::Task(_) = cty {
                    self.diags.push(Diag::new(sp, "`<-t` on a task handle only works as a `select` arm").note(None, "wait for the task with `t.join()`"));
                    return TExpr::error(sp);
                }
                let t = self.receiver_elem(&cty, ch.span, &ast_text(ch));
                if t == Ty::Error {
                    return TExpr::error(sp);
                }
                TExpr::new(TK::Recv(Box::new(c)), Ty::opt(t), sp)
            }
            ExprKind::DotVariant(name) => self.dot_variant(cx, name, &[], false, sp, expect.as_ref()),
            ExprKind::StructLit { ty, fields, positional, kv } => self.struct_lit(cx, ty, fields, positional, kv, sp, expect.as_ref()),
            ExprKind::VecLit(xs) => self.vec_lit(cx, xs, sp, expect.as_ref()),
            ExprKind::ListComp(elems) => self.list_comp(cx, elems, sp, expect.as_ref()),
            ExprKind::MapLit(kvs) => {
                let ty = match expect.as_ref().map(|t| self.resolve(cx, t)) {
                    Some(t @ Ty::Map(..)) => t,
                    _ => Ty::Map(Rc::new(self.fresh(cx)), Rc::new(self.fresh(cx))),
                };
                let infer = Expr { kind: ExprKind::Ident("_".into()), span: sp };
                let r = self.struct_lit(cx, &infer, &[], &[], kvs, sp, Some(&ty));
                if self.resolve(cx, &r.ty).has_vars() {
                    self.err(sp, "cannot infer the key and value types of `[:]`; write the type: `let m: Map[String, int] = [:]`");
                    return TExpr::error(sp);
                }
                r
            }
            ExprKind::TypeExpr(_) => {
                self.err(sp, "type used as a value");
                TExpr::error(sp)
            }
        }
    }

    /// `x?.member` for members other than struct fields (e.g. `.len`): a match on `x`.
    fn opt_member(&mut self, cx: &mut FnCx, b: TExpr, inner: Ty, name: &str, sp: Span) -> TExpr {
        let owned = !b.is_place();
        let b = if b.is_place() { self.borrow(cx, b) } else { b };
        self.push_scope(cx);
        let h = self.hidden_local(cx, inner.clone(), LocalKind::Ref { mutable: false }, sp);
        let fe = self.field_of(cx, TExpr::new(TK::Local(h), inner, sp), name, sp, sp);
        self.pop_scope(cx);
        let ft = self.resolve(cx, &fe.ty);
        if ft.is_error() {
            return TExpr::error(sp);
        }
        if !self.is_copy(&ft) {
            let s = self.prog.show(&ft);
            self.err(sp, format!("`?.` can only read copyable members here (`{}` is not copy); use `if let`", s));
            return TExpr::error(sp);
        }
        let fe = self.value(cx, fe);
        let rty = match &ft {
            Ty::Option(_) => ft.clone(),
            _ => Ty::opt(ft.clone()),
        };
        let some_body = if matches!(ft, Ty::Option(_)) { fe } else { TExpr::new(TK::SomeLit(Box::new(fe)), rty.clone(), sp) };
        let a1 = cx.n_arms;
        cx.n_arms += 2;
        cx.arm_consuming.push(false);
        cx.arm_consuming.push(false);
        let arms = vec![
            TArm { id: a1, pat: TPat::Some(Box::new(TPat::Bind(h))), guard: None, body: some_body, consuming: false, binds: vec![h] },
            TArm { id: a1 + 1, pat: TPat::None, guard: None, body: TExpr::new(TK::NoneLit, rty.clone(), sp), consuming: false, binds: vec![] },
        ];
        TExpr::new(TK::Match { scrut: Box::new(b), owned, arms }, rty, sp)
    }

    /// `x?.m(args)`: `none` when `x` is, else `some(x.m(args))`; a method that
    /// returns `?T` gives that `?T` (no `??T`).
    fn opt_call(&mut self, cx: &mut FnCx, base: &Expr, name: &str, nsp: Span, args: &[Expr], sp: Span) -> TExpr {
        let b = self.expr(cx, base, None);
        let bty = self.resolve(cx, &b.ty);
        let inner = match &bty {
            Ty::Option(t) => (**t).clone(),
            Ty::Error => return TExpr::error(sp),
            _ => {
                let s = self.prog.show(&bty);
                self.err(sp, format!("`?.` needs an optional value, found `{}`", s));
                return TExpr::error(sp);
            }
        };
        let owned = !b.is_place();
        let b = if b.is_place() { self.borrow(cx, b) } else { b };
        self.push_scope(cx);
        let h = self.hidden_local(cx, inner.clone(), LocalKind::Ref { mutable: false }, sp);
        let before = self.diags.len();
        let call = self.method_call_on(cx, TExpr::new(TK::Local(h), inner, sp), name, nsp, None, args, sp, None);
        // name the unwrapped value after what the user wrote
        let (hidden, shown) = (format!("`{}`", cx.locals[h as usize].name), format!("the value in `{}`", ast_text(base)));
        for d in &mut self.diags[before..] {
            d.msg = d.msg.replace(&format!("reference {}", hidden), &shown).replace(&hidden, &shown);
        }
        self.pop_scope(cx);
        let rt = self.resolve(cx, &call.ty);
        if rt.is_error() {
            return TExpr::error(sp);
        }
        if rt == Ty::Unit || rt == Ty::Never {
            self.err(sp, format!("`{}` gives no value, so `?.` has nothing to wrap; use `if let v = x {{ v.{}(...) }}`", name, name));
            return TExpr::error(sp);
        }
        let call = self.value(cx, call);
        let rty = match &rt {
            Ty::Option(_) => rt.clone(),
            _ => Ty::opt(rt.clone()),
        };
        let some_body = if matches!(rt, Ty::Option(_)) { call } else { TExpr::new(TK::SomeLit(Box::new(call)), rty.clone(), sp) };
        let a1 = cx.n_arms;
        cx.n_arms += 2;
        cx.arm_consuming.push(false);
        cx.arm_consuming.push(false);
        let arms = vec![
            TArm { id: a1, pat: TPat::Some(Box::new(TPat::Bind(h))), guard: None, body: some_body, consuming: false, binds: vec![h] },
            TArm { id: a1 + 1, pat: TPat::None, guard: None, body: TExpr::new(TK::NoneLit, rty.clone(), sp), consuming: false, binds: vec![] },
        ];
        TExpr::new(TK::Match { scrut: Box::new(b), owned, arms }, rty, sp)
    }

    /// A literal's value, exactly. Beyond 64 bits no type can hold it (and an
    /// `i128` conversion could wrap).
    fn literal_value(&mut self, n: u128, neg: bool, sp: Span) -> Option<i128> {
        if n > u64::MAX as u128 {
            self.err(sp, format!("integer literal {}{} is too large for any integer type", if neg { "-" } else { "" }, n));
            return None;
        }
        Some(if neg { -(n as i128) } else { n as i128 })
    }

    fn int_lit(&mut self, cx: &mut FnCx, v: i128, expect: Option<&Ty>, sp: Span) -> TExpr {
        let t = match expect.map(|t| self.shallow(cx, t)) {
            Some(Ty::Int(k)) => Ty::Int(k),
            Some(Ty::F32) => return TExpr::new(TK::Float(v as f64), Ty::F32, sp),
            Some(Ty::F64) => return TExpr::new(TK::Float(v as f64), Ty::F64, sp),
            Some(t @ Ty::Var(_)) => {
                self.unify(cx, &t, &Ty::int());
                Ty::int()
            }
            Some(Ty::Param(p)) if self.param_numeric(&Ty::Param(p)) => {
                // literal converted to the type parameter at runtime
                return TExpr::new(TK::Cast(Box::new(TExpr::new(TK::Int(v), Ty::int(), sp))), Ty::Param(p), sp);
            }
            _ => Ty::int(),
        };
        if let Ty::Int(k) = t {
            if v < k.min() || v > k.max() {
                self.err(sp, format!("integer literal {} does not fit in `{}`", v, k.name()));
            }
        }
        TExpr::new(TK::Int(v), t, sp)
    }

    fn ident(&mut self, cx: &mut FnCx, name: &str, sp: Span, expect: Option<&Ty>) -> TExpr {
        if let Some(id) = self.lookup_local(cx, name) {
            let t = cx.locals[id as usize].ty.clone();
            return TExpr::new(TK::Local(id), t, sp);
        }
        let e = Expr { kind: ExprKind::Ident(name.to_string()), span: sp };
        match self.resolve_path(cx, &e) {
            Some(PathRes::Func(f, targs)) => self.func_value(cx, f, targs, expect, sp),
            Some(PathRes::Const(c)) => {
                self.ensure_const(c as usize, Some(sp));
                let t = self.prog.consts[c as usize].ty.clone();
                TExpr::new(TK::Const(c), t, sp)
            }
            Some(PathRes::Global(g)) => self.global_value(g, sp),
            Some(PathRes::Pkg(..)) => {
                self.err(sp, format!("package `{}` used as a value", name));
                TExpr::error(sp)
            }
            Some(PathRes::TypeCon(Ty::Param(p), _)) if self.prog.tparam(p).is_int => {
                // an integer type parameter used as a value
                TExpr::new(TK::ConstParam(p), Ty::int(), sp)
            }
            Some(PathRes::TypeCon(..)) => {
                self.err(sp, format!("type `{}` used as a value", name));
                TExpr::error(sp)
            }
            _ => {
                let mut d = Diag::new(sp, format!("undefined: `{}`", name));
                if name == "nil" || name == "null" {
                    d = d.note(None, "Gojo has no nil; optional values are written `?T` and the empty value is `none`");
                }
                if name == "self" || name == "this" {
                    d = d.note(None, "only methods have `self`: declare it as the first parameter, `fn get(self) -> int`");
                }
                if let Some(h) = super::renamed(name) {
                    d = d.note(None, h);
                }
                self.diags.push(d);
                TExpr::error(sp)
            }
        }
    }

    /// An operation that needs an `unsafe` block: an error outside one, and
    /// otherwise a use of the innermost one.
    fn unsafe_op(&mut self, cx: &mut FnCx, sp: Span, msg: String) {
        match cx.unsafe_used.last_mut() {
            Some(u) => *u = true,
            None => self.err(sp, msg),
        }
    }

    fn func_value(&mut self, cx: &mut FnCx, f: FuncId, targs: Option<Vec<Ty>>, expect: Option<&Ty>, sp: Span) -> TExpr {
        let fd = self.prog.func(f).clone();
        if fd.recv.is_some() || fd.yields.is_some() || fd.kind == FuncKind::Intrinsic {
            self.err(sp, format!("`{}` cannot be used as a function value", fd.name));
            return TExpr::error(sp);
        }
        if fd.needs_unsafe() {
            // A function value is called without `unsafe`: wrapping it is the
            // only way to say who upholds its conditions.
            let what = if fd.is_unsafe { "an `unsafe fn`" } else { "an `extern` function" };
            let ps: Vec<String> = fd.params.iter().map(|p| p.name.clone()).collect();
            self.diags.push(Diag::new(sp, format!("`{}` is {}: it can't be used as a function value, since calling the value wouldn't need `unsafe`", fd.name, what)).note(None, format!("wrap the call in a closure that upholds its conditions: `({}) => unsafe {{ {}({}) }}`", ps.join(", "), fd.name, ps.join(", "))));
        }
        let targs: Vec<Ty> = match targs {
            Some(t) => t,
            None => fd.tparams.iter().map(|_| self.fresh(cx)).collect(),
        };
        if targs.len() != fd.tparams.len() {
            self.err(sp, format!("`{}` expects {} type argument(s)", fd.name, fd.tparams.len()));
            return TExpr::error(sp);
        }
        let s: HashMap<TParamId, Ty> = fd.tparams.iter().copied().zip(targs.iter().cloned()).collect();
        let fty = Ty::Func(Rc::new(FuncTy { params: fd.params.iter().map(|p| (p.mode, p.ty.subst(&s))).collect(), ret: fd.ret.subst(&s), send: true }));
        if let Some(e) = expect {
            let e = e.clone();
            self.unify(cx, &fty, &e);
        }
        TExpr::new(TK::FuncRef(f, targs), fty, sp)
    }

    /// Resolve a path-like expression (identifiers, package members, types, variants).
    pub fn resolve_path(&mut self, cx: &mut FnCx, e: &Expr) -> Option<PathRes> {
        match &e.kind {
            ExprKind::Ident(n) => {
                if cx.scopes.iter().any(|s| s.iter().any(|(m, _)| m == n)) {
                    return Some(PathRes::Value);
                }
                if let Some(t) = cx.env.tparams.get(n) {
                    return Some(PathRes::TypeCon(t.clone(), None));
                }
                if let Some(t) = builtin_type(n) {
                    return Some(PathRes::TypeCon(t, None));
                }
                if is_generic_builtin(n) {
                    return Some(PathRes::TypeCon(Ty::Error, None)).map(|_| PathRes::Pkg(u32::MAX, n.clone()));
                }
                let fi = cx.env.file;
                if let Some(sym) = self.lookup_unqualified(fi, cx.env.pkg, n) {
                    return Some(self.sym_res(sym));
                }
                if let Some((p, _)) = self.file_cx[fi].imports.get(n).cloned() {
                    self.file_cx[fi].used_imports.insert(n.clone());
                    return Some(PathRes::Pkg(p, n.clone()));
                }
                None
            }
            ExprKind::Field(base, name, _) => {
                let b = self.resolve_path(cx, base)?;
                match b {
                    PathRes::Pkg(p, pname) if p != u32::MAX => {
                        let sym = self.prog.packages[p as usize].items.get(name).cloned();
                        match sym {
                            Some(sym) => {
                                let public = match &sym {
                                    Sym::Type(d) => self.prog.def(*d).is_pub,
                                    Sym::Func(f) => self.prog.func(*f).is_pub,
                                    Sym::Const(c) => self.prog.consts[*c as usize].is_pub,
                                    Sym::Global(g) => self.prog.globals[*g as usize].is_pub,
                                    Sym::Alias(_) => true,
                                };
                                if !public {
                                    self.err(e.span, format!("`{}` is not public in package `{}`", name, pname));
                                }
                                Some(self.sym_res(sym))
                            }
                            None => {
                                self.err(e.span, format!("package `{}` has no member `{}`", pname, name));
                                Some(PathRes::Const(u32::MAX))
                            }
                        }
                    }
                    PathRes::TypeCon(t, gd) => {
                        let def = match (&t, gd) {
                            (Ty::Adt(d, _), _) => Some(*d),
                            (_, Some(d)) => Some(d),
                            _ => None,
                        };
                        if let Some(d) = def {
                            if let Some(vi) = self.prog.def(d).variants().iter().position(|v| v.name == *name) {
                                let ty = match &t {
                                    Ty::Adt(..) => t.clone(),
                                    _ => {
                                        let args: Vec<Ty> = self.prog.def(d).tparams.clone().iter().map(|_| self.fresh(cx)).collect();
                                        Ty::Adt(d, Rc::new(args))
                                    }
                                };
                                return Some(PathRes::Variant(ty, vi));
                            }
                        }
                        Some(PathRes::Value)
                    }
                    _ => Some(PathRes::Value),
                }
            }
            ExprKind::Index(base, args) => {
                let b = self.resolve_path(cx, base);
                match b {
                    Some(PathRes::TypeCon(..)) | Some(PathRes::Func(..)) | Some(PathRes::Pkg(u32::MAX, _)) => {
                        let mut tys = vec![];
                        for a in args {
                            match expr_to_type(a) {
                                Some(te) => {
                                    let env = cx.env.clone();
                                    tys.push(self.resolve_type(&te, &env))
                                }
                                None => return Some(PathRes::Value),
                            }
                        }
                        self.apply_targs(cx, b.unwrap(), tys, e)
                    }
                    _ => Some(PathRes::Value),
                }
            }
            ExprKind::TypeArgs(base, targs) => {
                let b = self.resolve_path(cx, base);
                let env = cx.env.clone();
                let tys: Vec<Ty> = targs.iter().map(|t| self.resolve_type(t, &env)).collect();
                match b {
                    Some(b @ (PathRes::TypeCon(..) | PathRes::Func(..) | PathRes::Pkg(u32::MAX, _))) => self.apply_targs(cx, b, tys, e),
                    _ => Some(PathRes::Value),
                }
            }
            ExprKind::TypeExpr(te) => {
                let env = cx.env.clone();
                Some(PathRes::TypeCon(self.resolve_type(te, &env), None))
            }
            _ => Some(PathRes::Value),
        }
    }

    fn apply_targs(&mut self, _cx: &mut FnCx, b: PathRes, tys: Vec<Ty>, e: &Expr) -> Option<PathRes> {
        match b {
            PathRes::TypeCon(_, Some(d)) => {
                if tys.iter().any(|t| self.prog.contains_view(&t)) && !matches!(self.prog.def(d).kind, TypeDefKind::Iface { .. }) {
                    self.err(e.span, format!("`{}` cannot be instantiated with a borrowed view type; use an owned type (String / Vec[T])", self.prog.def(d).name));
                }
                if self.prog.def(d).tparams.len() != tys.len() {
                    self.err(e.span, format!("`{}` expects {} type argument(s)", self.prog.def(d).name, self.prog.def(d).tparams.len()));
                    return Some(PathRes::TypeCon(Ty::Error, None));
                }
                let t = match self.prog.def(d).kind {
                    TypeDefKind::Iface { .. } => Ty::Iface(d, Rc::new(tys)),
                    _ => Ty::Adt(d, Rc::new(tys)),
                };
                Some(PathRes::TypeCon(t, None))
            }
            PathRes::Pkg(u32::MAX, name) => {
                let need = if name == "Map" { 2 } else { 1 };
                if tys.len() != need {
                    self.err(e.span, format!("`{}` expects {} type argument(s)", name, need));
                    return Some(PathRes::TypeCon(Ty::Error, None));
                }
                let a = Rc::new(tys[0].clone());
                let t = match name.as_str() {
                    "Vec" => Ty::Vec(a),
                    "Map" => Ty::Map(a, Rc::new(tys[1].clone())),
                    "Box" => Ty::Box(a),
                    "Rc" => Ty::Rc(a),
                    "Arc" => Ty::Arc(a),
                    "Weak" => Ty::Weak(a),
                    "Cell" => Ty::Cell(a),
                    "Mutex" => Ty::Mutex(a),
                    _ => Ty::Error,
                };
                Some(PathRes::TypeCon(t, None))
            }
            PathRes::Func(f, _) => Some(PathRes::Func(f, Some(tys))),
            _ => {
                self.err(e.span, "this type does not take type arguments");
                Some(PathRes::TypeCon(Ty::Error, None))
            }
        }
    }

    fn sym_res(&mut self, sym: Sym) -> PathRes {
        match sym {
            Sym::Type(d) => {
                let def = self.prog.def(d);
                if def.tparams.is_empty() {
                    let t = match def.kind {
                        TypeDefKind::Iface { .. } => Ty::Iface(d, Rc::new(vec![])),
                        _ => Ty::Adt(d, Rc::new(vec![])),
                    };
                    PathRes::TypeCon(t, None)
                } else {
                    PathRes::TypeCon(Ty::Error, Some(d))
                }
            }
            Sym::Alias(t) => PathRes::TypeCon(t, None),
            Sym::Func(f) => PathRes::Func(f, None),
            Sym::Const(c) => PathRes::Const(c),
            Sym::Global(g) => PathRes::Global(g),
        }
    }

    fn generic_type_instance(&mut self, cx: &mut FnCx, t: Ty, gd: Option<DefId>) -> Ty {
        match gd {
            Some(d) => {
                let args: Vec<Ty> = self.prog.def(d).tparams.clone().iter().map(|_| self.fresh(cx)).collect();
                Ty::Adt(d, Rc::new(args))
            }
            None => t,
        }
    }

    fn field(&mut self, cx: &mut FnCx, whole: &Expr, base: &Expr, name: &str, nsp: Span, expect: Option<&Ty>) -> TExpr {
        let sp = whole.span;
        // paths: package members, enum variants, constants
        if !matches!(self.resolve_path(cx, base), Some(PathRes::Value) | Some(PathRes::Global(_)) | None) {
            match self.resolve_path(cx, whole) {
                Some(PathRes::Variant(t, vi)) => return self.variant_value(cx, t, vi, &[], false, sp, expect),
                Some(PathRes::Func(f, targs)) => return self.func_value(cx, f, targs, expect, sp),
                Some(PathRes::Const(c)) => {
                    if c == u32::MAX {
                        return TExpr::error(sp);
                    }
                    self.ensure_const(c as usize, Some(sp));
                    let t = self.prog.consts[c as usize].ty.clone();
                    return TExpr::new(TK::Const(c), t, sp);
                }
                Some(PathRes::Global(g)) => return self.global_value(g, sp),
                Some(PathRes::TypeCon(..)) => {
                    self.err(sp, "type used as a value");
                    return TExpr::error(sp);
                }
                _ => {
                    self.err(nsp, format!("unknown member `{}`", name));
                    return TExpr::error(sp);
                }
            }
        }
        let b = self.expr(cx, base, None);
        self.field_of(cx, b, name, nsp, sp)
    }

    pub fn field_of(&mut self, cx: &mut FnCx, b: TExpr, name: &str, nsp: Span, sp: Span) -> TExpr {
        let mut b = b;
        loop {
            let bty = self.resolve(cx, &b.ty);
            match &bty {
                Ty::Adt(d, args) if self.prog.def(*d).is_struct() => {
                    let d = *d;
                    let fields = self.prog.field_tys(d, args);
                    if let Some(i) = fields.iter().position(|f| f.0 == name) {
                        let def = self.prog.def(d);
                        if !def.fields()[i].is_pub && def.pkg != cx.env.pkg {
                            self.err(nsp, format!("field `{}` of `{}` is private", name, def.name));
                        }
                        let ft = fields[i].1.clone();
                        return TExpr::new(TK::Field { base: Box::new(b), index: i }, ft, sp);
                    }
                    // promoted fields from embedded structs
                    let defc = self.prog.def(d).clone();
                    for (i, f) in defc.fields().iter().enumerate() {
                        if f.embedded {
                            let inner_ty = fields[i].1.clone();
                            if let Ty::Adt(d2, a2) = &inner_ty {
                                if self.prog.field_tys(*d2, a2).iter().any(|f2| f2.0 == name) {
                                    let inner = TExpr::new(TK::Field { base: Box::new(b), index: i }, inner_ty.clone(), sp);
                                    return self.field_of(cx, inner, name, nsp, sp);
                                }
                            }
                        }
                    }
                    if name == "len" {
                        break;
                    }
                    let s = self.prog.show(&bty);
                    let mut d = Diag::new(nsp, format!("`{}` has no field `{}`", s, name));
                    if self.prog.methods.contains_key(&(TyKey::Adt(d_id(&bty)), name.to_string())) {
                        d = d.note(None, format!("`{}` is a method; call it with `{}()`", name, name));
                    }
                    self.diags.push(d);
                    return TExpr::error(sp);
                }
                Ty::Box(inner) | Ty::Rc(inner) | Ty::Arc(inner) => {
                    let it = (**inner).clone();
                    b = TExpr::new(TK::Deref(Box::new(b)), it, sp);
                    continue;
                }
                Ty::Ptr(inner) => {
                    self.unsafe_op(cx, sp, "dereferencing a raw pointer requires an `unsafe` block".into());
                    let it = (**inner).clone();
                    b = TExpr::new(TK::Deref(Box::new(b)), it, sp);
                    continue;
                }
                Ty::Error => return TExpr::error(sp),
                _ => break,
            }
        }
        let bty = self.resolve(cx, &b.ty);
        if name == "len" && matches!(bty, Ty::Vec(_) | Ty::Slice(_) | Ty::Array(..) | Ty::ArrayT(..) | Ty::Str | Ty::String | Ty::Map(..)) {
            let b = self.borrow(cx, b);
            return TExpr::new(TK::Intrinsic { name: "len".into(), targs: vec![], args: vec![TArg { mode: ArgMode::Borrow, e: b }] }, Ty::int(), sp);
        }
        let s = self.prog.show(&bty);
        self.err(nsp, format!("type `{}` has no field `{}`", s, name));
        TExpr::error(sp)
    }

    fn index(&mut self, cx: &mut FnCx, whole: &Expr, base: &Expr, idx: &[Expr], sp: Span) -> TExpr {
        match self.resolve_path(cx, whole) {
            Some(PathRes::Func(f, targs)) => return self.func_value(cx, f, targs, None, sp),
            Some(PathRes::TypeCon(..)) => {
                self.err(sp, "type used as a value");
                return TExpr::error(sp);
            }
            _ => {}
        }
        let b = self.expr(cx, base, None);
        if idx.len() != 1 {
            self.err(sp, "expected a single index");
            return TExpr::error(sp);
        }
        let mut b = b;
        loop {
            let bty = self.resolve(cx, &b.ty);
            match &bty {
                Ty::Vec(t) | Ty::Slice(t) | Ty::Array(t, _) | Ty::ArrayT(t, _) => {
                    let t = (**t).clone();
                    let i = self.expr(cx, &idx[0], Some(&Ty::int()));
                    let ity = self.resolve(cx, &i.ty);
                    let i = if ity.is_int() { self.value(cx, i) } else { self.coerce(cx, i, &Ty::int(), Use::Value) };
                    self.const_bounds(&bty, Some(&i), None, false);
                    return TExpr::new(TK::Index { base: Box::new(b), index: Box::new(i) }, t, sp);
                }
                Ty::Str | Ty::String => {
                    let i = self.expr_value(cx, &idx[0], Some(&Ty::int()));
                    self.const_bounds(&bty, Some(&i), None, false);
                    let b = self.borrow(cx, b);
                    return TExpr::new(TK::StrIndex { base: Box::new(b), index: Box::new(i) }, Ty::Int(IntTy::U8), sp);
                }
                Ty::Map(k, v) => {
                    let (k, v) = ((**k).clone(), (**v).clone());
                    let key = self.expr_borrow(cx, &idx[0], Some(&k));
                    return TExpr::new(TK::MapIndex { base: Box::new(b), key: Box::new(key) }, v, sp);
                }
                Ty::Box(inner) | Ty::Rc(inner) | Ty::Arc(inner) => {
                    let it = (**inner).clone();
                    b = TExpr::new(TK::Deref(Box::new(b)), it, sp);
                }
                Ty::Error => return TExpr::error(sp),
                _ => {
                    let s = self.prog.show(&bty);
                    self.err(sp, format!("cannot index into a value of type `{}`", s));
                    return TExpr::error(sp);
                }
            }
        }
    }

    /// Rejects an index (`lo`, `slice` false) or slice bounds that are
    /// constants and can only panic: negative, past a fixed array's length, or
    /// a start after the end.
    fn const_bounds(&mut self, bty: &Ty, lo: Option<&TExpr>, hi: Option<&TExpr>, slice: bool) {
        let len = match bty {
            Ty::Array(_, n) => Some(*n as i128),
            _ => None,
        };
        let shown = self.prog.show(bty);
        let mut vals = vec![];
        for e in [lo, hi].into_iter().flatten() {
            let Some(v) = crate::tir::const_int(e, &self.prog) else {
                vals.push(None);
                continue;
            };
            vals.push(Some(v));
            let what = if slice { "slice bound" } else { "index" };
            if v < 0 {
                self.err(e.span, format!("{} {} is negative", what, v));
            } else if let Some(n) = len {
                // a slice may end at the length; an index must be below it
                if v > n || (!slice && v == n) {
                    self.err(e.span, format!("{} {} is out of range for `{}` (length {})", what, v, shown, n));
                }
            }
        }
        if let (Some(lo), Some(hi), [Some(a), Some(b)]) = (lo, hi, vals.as_slice()) {
            if a > b && *a >= 0 && *b >= 0 {
                self.err(lo.span.to(hi.span), format!("slice bounds are reversed: {} is after {}", a, b));
            }
        }
    }

    fn unary(&mut self, cx: &mut FnCx, op: &'static str, inner: &Expr, sp: Span, expect: Option<&Ty>) -> TExpr {
        match op {
            "-" => {
                if let ExprKind::Int(n) = inner.kind {
                    return match self.literal_value(n, true, sp) {
                        Some(v) => self.int_lit(cx, v, expect, sp),
                        None => TExpr::error(sp),
                    };
                }
                let e = self.expr_value(cx, inner, expect.filter(|t| t.is_numeric()));
                let t = self.resolve(cx, &e.ty);
                let signed = |t: &Ty| matches!(t, Ty::Int(k) if k.signed()) || t.is_float();
                if !(signed(&t) || t.is_error() || self.param_numeric(&t) || self.numeric_newtype(&t).map_or(false, |i| signed(&i))) {
                    let s = self.prog.show(&t);
                    self.err(sp, format!("cannot negate a value of type `{}`", s));
                }
                TExpr::new(TK::Unary { op, e: Box::new(e) }, t, sp)
            }
            "!" => {
                let e = self.expr_value(cx, inner, Some(&Ty::Bool));
                TExpr::new(TK::Unary { op, e: Box::new(e) }, Ty::Bool, sp)
            }
            "~" => {
                let e = self.expr_value(cx, inner, expect.filter(|t| t.is_int()));
                let t = self.resolve(cx, &e.ty);
                if !t.is_int() && !t.is_error() {
                    self.err(sp, "`~` (bitwise not) needs an integer");
                }
                TExpr::new(TK::Unary { op, e: Box::new(e) }, t, sp)
            }
            "*" => {
                let e = self.expr(cx, inner, None);
                let t = self.resolve(cx, &e.ty);
                match &t {
                    Ty::Ptr(p) => {
                        self.unsafe_op(cx, sp, "dereferencing a raw pointer requires an `unsafe` block".into());
                        let pt = (**p).clone();
                        let e = self.value(cx, e);
                        TExpr::new(TK::Deref(Box::new(e)), pt, sp)
                    }
                    Ty::Box(p) => {
                        let pt = (**p).clone();
                        TExpr::new(TK::Deref(Box::new(e)), pt, sp)
                    }
                    Ty::Error => TExpr::error(sp),
                    _ => {
                        let s = self.prog.show(&t);
                        self.err(sp, format!("cannot dereference a value of type `{}`", s));
                        TExpr::error(sp)
                    }
                }
            }
            _ => unreachable!(),
        }
    }

    fn binary(&mut self, cx: &mut FnCx, op: &'static str, l: &Expr, r: &Expr, sp: Span, expect: Option<&Ty>) -> TExpr {
        let bop = BinOp::from_str(op).unwrap();
        if matches!(bop, BinOp::And | BinOp::Or) {
            let a = self.expr_value(cx, l, Some(&Ty::Bool));
            let b = self.expr_value(cx, r, Some(&Ty::Bool));
            return TExpr::new(TK::Binary { op: bop, l: Box::new(a), r: Box::new(b) }, Ty::Bool, sp);
        }
        let is_lit = literal_like;
        let arith_expect = if bop.is_cmp() { None } else { expect.filter(|t| t.is_numeric()).cloned() };
        // check the non-literal side first so literals adopt its type
        let (a, b) = if is_lit(l) && !is_lit(r) {
            let b = self.expr(cx, r, arith_expect.as_ref());
            let bt = self.resolve(cx, &b.ty);
            let a = self.expr(cx, l, Some(&bt));
            (a, b)
        } else {
            let a = self.expr(cx, l, arith_expect.as_ref());
            let at = self.resolve(cx, &a.ty);
            let hint = if matches!(bop, BinOp::Shl | BinOp::Shr) { None } else { Some(at) };
            let b = self.expr(cx, r, hint.as_ref());
            (a, b)
        };
        let at = self.resolve(cx, &a.ty);
        let bt = self.resolve(cx, &b.ty);
        if at.is_error() || bt.is_error() {
            return TExpr::error(sp);
        }
        // strings
        if at.is_stringish() && bt.is_stringish() {
            let a = self.coerce(cx, a, &Ty::Str, Use::Borrow);
            let b = self.coerce(cx, b, &Ty::Str, Use::Borrow);
            if bop == BinOp::Add {
                return TExpr::new(TK::Intrinsic { name: "str_concat".into(), targs: vec![], args: vec![TArg { mode: ArgMode::Borrow, e: a }, TArg { mode: ArgMode::Borrow, e: b }] }, Ty::String, sp);
            }
            if bop.is_cmp() {
                return TExpr::new(TK::Binary { op: bop, l: Box::new(a), r: Box::new(b) }, Ty::Bool, sp);
            }
            self.err(sp, format!("operator `{}` is not supported for strings", op));
            return TExpr::error(sp);
        }
        if matches!(bop, BinOp::Shl | BinOp::Shr) {
            if !at.is_int() || !bt.is_int() {
                self.err(sp, "shifts need integer operands");
            }
            let a = self.value(cx, a);
            let b = self.value(cx, b);
            return TExpr::new(TK::Binary { op: bop, l: Box::new(a), r: Box::new(b) }, at, sp);
        }
        if !self.try_unify(cx, &at, &bt) {
            let (x, y) = (self.prog.show(&at), self.prog.show(&bt));
            let mut d = Diag::new(sp, format!("mismatched operand types: `{}` {} `{}`", x, op, y));
            if at.is_numeric() && bt.is_numeric() {
                d = d.note(None, format!("convert explicitly, e.g. `{}(x)`", x));
            }
            self.diags.push(d);
            return TExpr::error(sp);
        }
        if bop.is_cmp() {
            let needs_order = !matches!(bop, BinOp::Eq | BinOp::Ne);
            let ok = if needs_order { self.is_ordered(&at) } else { self.is_comparable(&at) };
            if !ok {
                let s = self.prog.show(&at);
                self.err(sp, format!("type `{}` does not support `{}`{}", s, op, if needs_order { " (it is not `ordered`)" } else { " (it is not `comparable`)" }));
            }
            let a = if self.is_copy(&at) { self.value(cx, a) } else { self.borrow(cx, a) };
            let b = if self.is_copy(&bt) { self.value(cx, b) } else { self.borrow(cx, b) };
            return TExpr::new(TK::Binary { op: bop, l: Box::new(a), r: Box::new(b) }, Ty::Bool, sp);
        }
        self.check_arith(cx, bop, &at, &bt, sp);
        let a = self.value(cx, a);
        let b = self.value(cx, b);
        TExpr::new(TK::Binary { op: bop, l: Box::new(a), r: Box::new(b) }, at, sp)
    }

    fn if_rest(&mut self, cx: &mut FnCx, c: TExpr, then: &ast::Block, else_: Option<&Expr>, ex: Expect, sp: Span) -> TExpr {
        let then_b = self.block(cx, then, ex.clone());
        let tt = self.block_ty(&then_b);
        match else_ {
            None => {
                if let Expect::Ty(t) = &ex {
                    if !matches!(t, Ty::Unit | Ty::Never) && !matches!(t, Ty::Var(_)) {
                        let s = self.prog.show(t);
                        self.err(sp, format!("`if` without `else` has no value, but `{}` is expected", s));
                    }
                }
                let ty = if matches!(ex, Expect::NoValue) { Ty::Unit } else if tt == Ty::Never { Ty::Unit } else { tt.clone() };
                if !matches!(ex, Expect::NoValue) && !matches!(ty, Ty::Unit | Ty::Error) {
                    let s = self.prog.show(&ty);
                    self.err(sp, format!("`if` without `else` cannot produce a value of type `{}`", s));
                }
                TExpr::new(TK::If { cond: Box::new(c), then: then_b, else_: None }, Ty::Unit, sp)
            }
            Some(e) => {
                let ex2 = match &ex {
                    Expect::Infer if tt != Ty::Never => Expect::Ty(tt.clone()),
                    other => other.clone(),
                };
                let else_e = self.expr_x(cx, e, ex2);
                let et = self.resolve(cx, &else_e.ty);
                let else_b = match else_e.kind {
                    TK::Block(b) => b,
                    _ => TBlock { stmts: vec![], value: Some(Box::new(else_e.clone())), span: else_e.span },
                };
                let ty = if matches!(ex, Expect::NoValue) {
                    Ty::Unit
                } else if tt == Ty::Never {
                    et
                } else {
                    tt
                };
                TExpr::new(TK::If { cond: Box::new(c), then: then_b, else_: Some(else_b) }, ty, sp)
            }
        }
    }

    fn try_expr(&mut self, cx: &mut FnCx, inner: &Expr, wrap: Option<&Expr>, sp: Span) -> TExpr {
        if cx.par_frames.last() == Some(&cx.depth()) {
            self.diags.push(Diag::new(sp, "`try` can't leave a `par for`: its iterations run at the same time").note(None, "handle the error in the body (`catch`), or use `task.tryMap`, which stops at the first error"));
            return TExpr::error(sp);
        }
        if cx.defer_depth > 0 {
            self.err(sp, "cannot use `try` in a defer: it would return from the function; handle the error with `catch` or `match`");
        }
        let e = self.expr(cx, inner, None);
        let e = self.value(cx, e);
        let ety = self.resolve(cx, &e.ty);
        let ret = cx.ret.clone();
        let mut rr = self.resolve(cx, &ret);
        if cx.yields.is_some() && cx.frames.is_empty() {
            self.err(sp, "`try` cannot be used in a projection function");
            return TExpr::error(sp);
        }
        match &ety {
            Ty::Result(ok, e1) => {
                let (ok, e1) = ((**ok).clone(), (**e1).clone());
                if let Ty::Var(_) = rr {
                    let v = self.fresh(cx);
                    let t = Ty::result(v, e1.clone());
                    self.unify(cx, &ret, &t);
                    rr = self.resolve(cx, &ret);
                }
                let e2 = match &rr {
                    Ty::Result(_, e2) => (**e2).clone(),
                    Ty::Error => return TExpr::error(sp),
                    _ => {
                        let s = self.prog.show(&rr);
                        self.diags.push(Diag::new(sp, format!("`try` needs the enclosing function to return a result (`T ! E`), but it returns `{}`", s)).note(None, "use `catch` or `match` to handle the error here"));
                        return TExpr::error(sp);
                    }
                };
                let w = if let Some(w) = wrap {
                    // check `w(err)` with a hidden local holding the error
                    self.push_scope(cx);
                    let hid = self.hidden_local(cx, e1.clone(), LocalKind::Let, sp);
                    let hname = cx.locals[hid as usize].name.clone();
                    let call = Expr { kind: ExprKind::Call(Box::new(w.clone()), vec![Expr { kind: ExprKind::Ident(hname), span: w.span }]), span: w.span };
                    let te = self.expr(cx, &call, Some(&e2));
                    let te = self.coerce(cx, te, &e2, Use::Value);
                    self.pop_scope(cx);
                    Some(TryWrap::Call(hid, Box::new(te)))
                } else if self.try_unify(cx, &e1, &e2) {
                    None
                } else if matches!(&e2, Ty::Iface(d, _) if Some(*d) == self.iface_error) {
                    let (d, args) = match &e2 {
                        Ty::Iface(d, a) => (*d, a.clone()),
                        _ => unreachable!(),
                    };
                    if let Err(m) = self.implements(&e1, d, &args) {
                        self.err(sp, m);
                    }
                    Some(TryWrap::ToError)
                } else {
                    let (a, b) = (self.prog.show(&e1), self.prog.show(&e2));
                    self.diags.push(
                        Diag::new(sp, format!("`try` would propagate an error of type `{}`, but the function returns errors of type `{}`", a, b)).note(None, "convert it explicitly: `try expr else Wrapper` (a function or enum variant taking the error)"),
                    );
                    None
                };
                TExpr::new(TK::Try { e: Box::new(e), wrap: w }, ok, sp)
            }
            Ty::Option(inner_t) => {
                let inner_t = (**inner_t).clone();
                if let Ty::Var(_) = rr {
                    let v = self.fresh(cx);
                    self.unify(cx, &ret, &Ty::opt(v));
                    rr = self.resolve(cx, &ret);
                }
                if !matches!(rr, Ty::Option(_) | Ty::Error) {
                    let s = self.prog.show(&rr);
                    self.err(sp, format!("`try` on an optional value needs the function to return an optional value, but it returns `{}`", s));
                }
                if wrap.is_some() {
                    self.err(sp, "`try ... else` is only for results");
                }
                TExpr::new(TK::Try { e: Box::new(e), wrap: None }, inner_t, sp)
            }
            Ty::Error => TExpr::error(sp),
            _ => {
                let s = self.prog.show(&ety);
                self.err(sp, format!("`try` needs a result (`T ! E`) or optional value, found `{}`", s));
                TExpr::error(sp)
            }
        }
    }

    fn struct_lit(&mut self, cx: &mut FnCx, tyexpr: &Expr, fields: &[(String, Span, Expr)], positional: &[Expr], kv: &[(Expr, Expr)], sp: Span, expect: Option<&Ty>) -> TExpr {
        let inferred = matches!(&tyexpr.kind, ExprKind::Ident(n) if n == "_");
        let res = if inferred { None } else { self.resolve_path(cx, tyexpr) };
        let ty = match res {
            _ if inferred => match expect.map(|t| self.resolve(cx, t)) {
                Some(t @ (Ty::Adt(..) | Ty::Vec(_) | Ty::Map(..))) => t,
                Some(Ty::Option(t)) if matches!(*t, Ty::Adt(..)) => (*t).clone(),
                Some(t) if !matches!(t, Ty::Var(_) | Ty::Error) => {
                    let s = self.prog.show(&t);
                    self.err(sp, format!("`.{{...}}` builds a struct, but `{}` is expected here", s));
                    return TExpr::error(sp);
                }
                _ => {
                    self.err(sp, "cannot tell which struct `.{...}` builds here; name it: `Point{x: 1}`");
                    for (_, _, v) in fields {
                        let _ = self.expr(cx, v, None);
                    }
                    return TExpr::error(sp);
                }
            },
            Some(PathRes::TypeCon(t, gd)) => self.generic_type_instance(cx, t, gd),
            Some(PathRes::Pkg(u32::MAX, name)) => {
                // `Vec{...}` / `Map{...}` without type args: infer
                let a = self.fresh(cx);
                match name.as_str() {
                    "Vec" => Ty::vec(a),
                    "Map" => {
                        let b = self.fresh(cx);
                        Ty::Map(Rc::new(a), Rc::new(b))
                    }
                    _ => {
                        self.err(sp, format!("`{}` cannot be built with a literal; call `{}(x)`", name, name));
                        return TExpr::error(sp);
                    }
                }
            }
            _ => {
                self.err(tyexpr.span, "expected a struct type before `{`");
                return TExpr::error(sp);
            }
        };
        if let Some(e) = expect {
            let e = e.clone();
            self.try_unify(cx, &ty, &e);
        }
        let ty = self.resolve(cx, &ty);
        match &ty {
            Ty::Vec(elem) => {
                if !fields.is_empty() || !kv.is_empty() {
                    self.err(sp, "a Vec literal lists elements: `Vec[int]{1, 2, 3}`");
                }
                let elem = (**elem).clone();
                if let (Ty::Var(_), Some(first)) = (self.resolve(cx, &elem), positional.first()) {
                    if matches!(first.kind, ExprKind::Str(_)) {
                        self.unify(cx, &elem, &Ty::String);
                    }
                }
                let mut xs = vec![];
                for p in positional {
                    let r = self.resolve(cx, &elem);
                    let x = self.expr(cx, p, Some(&r));
                    xs.push(self.coerce(cx, x, &r, Use::Value));
                }
                let ty = self.resolve(cx, &ty);
                return TExpr::new(TK::VecLit(xs), ty, sp);
            }
            Ty::Map(k, v) => {
                let (k, v) = ((**k).clone(), (**v).clone());
                if !positional.is_empty() {
                    self.err(sp, "a Map literal lists `key: value` pairs");
                }
                for (kexpr, vexpr) in kv.iter().take(1) {
                    if let (Ty::Var(_), ExprKind::Str(_)) = (self.resolve(cx, &k), &kexpr.kind) {
                        self.unify(cx, &k, &Ty::String);
                    }
                    if let (Ty::Var(_), ExprKind::Str(_)) = (self.resolve(cx, &v), &vexpr.kind) {
                        self.unify(cx, &v, &Ty::String);
                    }
                }
                let mut pairs = vec![];
                for (name, nsp, val) in fields {
                    // `Map{name: v}` with identifier keys refers to variables
                    let ke = Expr { kind: ExprKind::Ident(name.clone()), span: *nsp };
                    let kk = self.resolve(cx, &k);
                    let ke = self.expr(cx, &ke, Some(&kk));
                    let ke = self.coerce(cx, ke, &kk, Use::Value);
                    let vv = self.resolve(cx, &v);
                    let ve = self.expr(cx, val, Some(&vv));
                    let ve = self.coerce(cx, ve, &vv, Use::Value);
                    pairs.push((ke, ve));
                }
                for (ke, ve) in kv {
                    let kk = self.resolve(cx, &k);
                    let ke = self.expr(cx, ke, Some(&kk));
                    let ke = self.coerce(cx, ke, &kk, Use::Value);
                    let vv = self.resolve(cx, &v);
                    let ve = self.expr(cx, ve, Some(&vv));
                    let ve = self.coerce(cx, ve, &vv, Use::Value);
                    pairs.push((ke, ve));
                }
                let ty = self.resolve(cx, &ty);
                let kt = self.resolve(cx, &k);
                if !kt.has_vars() && !self.is_hash(&kt) {
                    let s = self.prog.show(&kt);
                    self.err(sp, format!("`{}` cannot be a Map key: it does not satisfy `hash`", s));
                }
                return TExpr::new(TK::MapLit(pairs), ty, sp);
            }
            Ty::Adt(d, args) if self.prog.def(*d).is_struct() => {
                let (d, args) = (*d, args.clone());
                let def = self.prog.def(d).clone();
                if def.pkg != cx.env.pkg && def.fields().iter().any(|f| !f.is_pub) {
                    self.err(sp, format!("cannot build `{}` with a literal outside its package: it has private fields", def.name));
                }
                // `Point{x, y}` is `Point{x: x, y: y}`
                let mut shorthand = vec![];
                if !positional.is_empty() {
                    if def.newtype && positional.len() == 1 {
                        // allow Celsius{3.0}
                    } else {
                        for p in positional {
                            match &p.kind {
                                ExprKind::Ident(n) => shorthand.push((n.clone(), p.span, p.clone())),
                                _ => self.err(p.span, "struct literals name their fields: `Point{x: 1, y: 2}`, or `Point{x, y}` for variables named like the fields"),
                            }
                        }
                    }
                }
                let ftys = self.prog.field_tys(d, &args);
                let mut out: Vec<Option<TExpr>> = vec![None; ftys.len()];
                for (name, nsp, val) in fields.iter().chain(shorthand.iter()) {
                    match ftys.iter().position(|f| f.0 == *name) {
                        Some(i) => {
                            if out[i].is_some() {
                                self.err(*nsp, format!("field `{}` is set twice", name));
                            }
                            if let (Ty::Var(_), ExprKind::Str(_)) = (self.resolve(cx, &ftys[i].1), &val.kind) {
                                let ft = ftys[i].1.clone();
                                self.unify(cx, &ft, &Ty::String);
                            }
                            let ft = self.resolve(cx, &ftys[i].1);
                            let v = self.expr(cx, val, Some(&ft));
                            out[i] = Some(self.coerce(cx, v, &ft, Use::Value));
                        }
                        None => {
                            self.err(*nsp, format!("`{}` has no field `{}`", def.name, name));
                            let _ = self.expr(cx, val, None);
                        }
                    }
                }
                if def.newtype && positional.len() == 1 {
                    let ft = ftys[0].1.clone();
                    let v = self.expr(cx, &positional[0], Some(&ft));
                    out[0] = Some(self.coerce(cx, v, &ft, Use::Value));
                }
                let mut fes = vec![];
                let mut missing = vec![];
                for (i, o) in out.into_iter().enumerate() {
                    match o {
                        Some(e) => fes.push(e),
                        None => {
                            let ft = self.resolve(cx, &ftys[i].1);
                            if def.fields()[i].default.is_some() || matches!(ft, Ty::Option(_)) {
                                fes.push(TExpr::new(TK::DefaultOf, ft, sp));
                            } else {
                                missing.push(ftys[i].0.clone());
                                fes.push(TExpr::error(sp));
                            }
                        }
                    }
                }
                if !missing.is_empty() {
                    self.err(sp, format!("missing field{} {} in `{}` literal (fields without a default must be set)", if missing.len() > 1 { "s" } else { "" }, missing.iter().map(|m| format!("`{}`", m)).collect::<Vec<_>>().join(", "), def.name));
                }
                let ty = self.resolve(cx, &ty);
                let targs = match &ty {
                    Ty::Adt(_, a) => a.to_vec(),
                    _ => vec![],
                };
                if targs.iter().any(|t| t.has_vars()) {
                    self.err(sp, format!("cannot infer type arguments for `{}`", def.name));
                }
                TExpr::new(TK::StructLit { def: d, targs, fields: fes }, ty, sp)
            }
            Ty::Error => TExpr::error(sp),
            _ => {
                let s = self.prog.show(&ty);
                self.err(sp, format!("`{}` cannot be built with a struct literal", s));
                TExpr::error(sp)
            }
        }
    }

    /// `[a, if c { b }, for x in xs { f(x) }, ..rest]`, checked as pushes onto a
    /// hidden `Vec`:
    /// `{ var v = Vec[T]{}; v.push(a); if c { v.push(b) }; for x in xs { v.push(f(x)) }; v.extend(rest); v }`.
    /// An element's block may hold statements before its value, and end in a
    /// nested `if` (without `else`) or `for`.
    fn list_comp(&mut self, cx: &mut FnCx, elems: &[ast::ListElem], sp: Span, expect: Option<&Ty>) -> TExpr {
        let expect = expect.map(|t| self.resolve(cx, t));
        if let Some(t @ Ty::Adt(..)) = &expect {
            let Ty::Adt(d, args) = t.clone() else { unreachable!() };
            if Some(d) == self.prog.prelude_type("Set") {
                if let Some(Sym::Func(f)) = self.prog.packages[self.prog.prelude as usize].items.get("setOf").cloned() {
                    let list = Expr { kind: ExprKind::ListComp(elems.to_vec()), span: sp };
                    return self.call_fn(cx, f, Some(args.to_vec()), None, &[list], sp, Some(t));
                }
            }
        }
        let elem = match &expect {
            Some(Ty::Vec(t)) | Some(Ty::Slice(t)) => (**t).clone(),
            _ => self.fresh(cx),
        };
        let first_str = elems.iter().find_map(|e| match e {
            ast::ListElem::Expr(x) => Some(matches!(x.kind, ExprKind::Str(_) | ExprKind::Interp(_))),
            _ => None,
        });
        if let (Ty::Var(_), Some(true)) = (self.resolve(cx, &elem), first_str) {
            self.unify(cx, &elem, &Ty::String);
        }
        let vt = Ty::vec(elem.clone());
        let h = self.hidden_local(cx, vt.clone(), LocalKind::Var, sp);
        let name = cx.locals[h as usize].name.clone();
        let mut stmts = vec![TStmt::Let { local: h, init: TExpr::new(TK::VecLit(vec![]), vt.clone(), sp) }];
        let mut missing = vec![];
        for el in elems {
            let st = match el {
                ast::ListElem::Expr(x) => comp_expr(&name, x, &mut missing),
                ast::ListElem::Spread(x) => comp_call(&name, "extend", x.clone()),
                ast::ListElem::For(fl) => {
                    let ast::ForLoop::In { first, second, iter, by_ref, body } = fl else { unreachable!() };
                    let body = comp_block(&name, body, &mut missing);
                    Stmt { kind: StmtKind::For(ast::ForLoop::In { first: first.clone(), second: second.clone(), iter: iter.clone(), by_ref: *by_ref, body }, None), span: sp }
                }
            };
            if let Some(ts) = self.stmt(cx, &st) {
                stmts.push(ts);
            }
        }
        let incomplete = !missing.is_empty();
        for m in missing {
            self.err(m, "this `if` / `for` element gives no value: end its block with the element to add");
        }
        let elem = self.resolve(cx, &elem);
        if incomplete && elem.has_vars() {
            return TExpr::error(sp);
        }
        if elem.has_vars() {
            self.err(sp, "cannot infer the element type of this list; give it a type: `let xs: Vec[T] = [...]`");
            return TExpr::error(sp);
        }
        if self.prog.contains_view(&elem) {
            let s = self.prog.show(&elem);
            self.err(sp, format!("a list cannot hold borrowed views (`{}`); use owned values such as String", s));
            return TExpr::error(sp);
        }
        let vt = Ty::vec(elem);
        cx.locals[h as usize].ty = vt.clone();
        let v = self.value(cx, TExpr::new(TK::Local(h), vt.clone(), sp));
        TExpr::new(TK::Block(TBlock { stmts, value: Some(Box::new(v)), span: sp }), vt, sp)
    }

    fn vec_lit(&mut self, cx: &mut FnCx, xs: &[Expr], sp: Span, expect: Option<&Ty>) -> TExpr {
        // a list where a `Set` is expected builds the set
        if let Some(t @ Ty::Adt(..)) = expect.map(|t| self.resolve(cx, t)) {
            let Ty::Adt(d, args) = t.clone() else { unreachable!() };
            if Some(d) == self.prog.prelude_type("Set") {
                let set = self.prog.packages[self.prog.prelude as usize].items.get("setOf").cloned();
                if let Some(Sym::Func(f)) = set {
                    let list = Expr { kind: ExprKind::VecLit(xs.to_vec()), span: sp };
                    return self.call_fn(cx, f, Some(args.to_vec()), None, &[list], sp, Some(&t));
                }
            }
        }
        let (elem, is_array, n) = match expect {
            Some(Ty::Vec(t)) => ((**t).clone(), false, 0),
            Some(Ty::Array(t, n)) => ((**t).clone(), true, *n),
            Some(Ty::Slice(t)) => ((**t).clone(), false, 0),
            _ => (self.fresh(cx), false, 0),
        };
        if let (Ty::Var(_), Some(first)) = (self.resolve(cx, &elem), xs.first()) {
            if matches!(first.kind, ExprKind::Str(_)) {
                // string literals in a list are owned Strings
                self.unify(cx, &elem, &Ty::String);
            }
        }
        let mut out = vec![];
        for x in xs {
            let r = self.resolve(cx, &elem);
            let e = self.expr(cx, x, Some(&r));
            out.push(self.coerce(cx, e, &r, Use::Value));
        }
        let elem = self.resolve(cx, &elem);
        if self.prog.contains_view(&elem) && !is_array {
            let s = self.prog.show(&elem);
            self.err(sp, format!("a list cannot hold borrowed views (`{}`); use owned values such as String", s));
            return TExpr::error(sp);
        }
        if elem.has_vars() {
            self.err(sp, "cannot infer the element type of an empty list; write `Vec[T]{}`");
            return TExpr::error(sp);
        }
        if is_array {
            if out.len() as u64 != n {
                self.err(sp, format!("array literal has {} elements, expected {}", out.len(), n));
            }
            return TExpr::new(TK::ArrayLit(out), Ty::Array(Rc::new(elem), n), sp);
        }
        TExpr::new(TK::VecLit(out), Ty::vec(elem), sp)
    }

    fn dot_variant(&mut self, cx: &mut FnCx, name: &str, args: &[Expr], called: bool, sp: Span, expect: Option<&Ty>) -> TExpr {
        // the enum, or the one an optional or a result (`E ! Err`, success
        // first) holds
        let ty = match expect.map(|t| self.resolve(cx, t)) {
            Some(Ty::Option(inner)) if matches!(&*inner, Ty::Adt(..)) => (*inner).clone(),
            Some(Ty::Result(ok, _)) if matches!(&*ok, Ty::Adt(..)) => (*ok).clone(),
            Some(Ty::Result(ok, _)) if matches!(&*ok, Ty::Option(i) if matches!(&**i, Ty::Adt(..))) => match &*ok {
                Ty::Option(i) => (**i).clone(),
                _ => unreachable!(),
            },
            Some(t @ Ty::Adt(..)) => t,
            _ => {
                self.err(sp, format!("cannot infer the enum type of `.{}`; write `Type.{}`", name, name));
                return TExpr::error(sp);
            }
        };
        let d = match &ty {
            Ty::Adt(d, _) => *d,
            _ => unreachable!(),
        };
        match self.prog.def(d).variants().iter().position(|v| v.name == name) {
            Some(vi) => self.variant_value(cx, ty, vi, args, called, sp, None),
            None => {
                let s = self.prog.show(&ty);
                self.err(sp, format!("`{}` has no variant `{}`", s, name));
                TExpr::error(sp)
            }
        }
    }

    fn variant_value(&mut self, cx: &mut FnCx, ty: Ty, vi: usize, args: &[Expr], called: bool, sp: Span, expect: Option<&Ty>) -> TExpr {
        if let Some(e) = expect {
            let e = e.clone();
            self.try_unify(cx, &ty, &e);
        }
        let (d, targs) = match &ty {
            Ty::Adt(d, a) => (*d, a.clone()),
            _ => return TExpr::error(sp),
        };
        let fields = self.prog.variant_tys(d, &targs, vi);
        let vname = self.prog.def(d).variants()[vi].name.clone();
        if !called && !fields.is_empty() {
            self.err(sp, format!("variant `{}` needs {} value(s): `{}(...)`", vname, fields.len(), vname));
            return TExpr::error(sp);
        }
        if called && fields.is_empty() {
            self.err(sp, format!("variant `{}` takes no values; write `{}` without parentheses", vname, vname));
            return TExpr::error(sp);
        }
        if args.len() != fields.len() {
            self.err(sp, format!("variant `{}` takes {} value(s), {} given", vname, fields.len(), args.len()));
            return TExpr::error(sp);
        }
        let mut out = vec![];
        for (a, (_, ft)) in args.iter().zip(fields.iter()) {
            let ft = self.resolve(cx, ft);
            let e = self.expr(cx, a, Some(&ft));
            out.push(self.coerce(cx, e, &ft, Use::Value));
        }
        let ty = self.resolve(cx, &ty);
        let targs = match &ty {
            Ty::Adt(_, a) => a.to_vec(),
            _ => vec![],
        };
        if targs.iter().any(|t| t.has_vars()) && expect.is_none() {
            // may still be inferred by the context; leave vars
        }
        TExpr::new(TK::EnumLit { def: d, targs, variant: vi, args: out }, ty, sp)
    }

    // ------------------------------------------------------------------
    // Closures
    // ------------------------------------------------------------------

    fn closure(&mut self, cx: &mut FnCx, params: &[ast::ClosureParam], ret: Option<&ast::TypeExpr>, body: &Expr, sp: Span, expect: Option<&Ty>) -> TExpr {
        let ef = match expect.map(|t| self.resolve(cx, t)) {
            Some(Ty::Func(f)) => Some(f),
            _ => None,
        };
        if let Some(f) = &ef {
            if f.params.len() != params.len() {
                self.err(sp, format!("closure takes {} parameter(s), but {} are expected here", params.len(), f.params.len()));
                return TExpr::error(sp);
            }
        }
        let loop_body = cx.loop_body_next.take();
        let saved_defer = cx.defer_depth;
        if loop_body.is_none() {
            cx.defer_depth = 0;
        }
        let depth = cx.depth() + 1;
        let saved_ret = cx.ret.clone();
        let ret_ty = match (ret, &ef) {
            (Some(t), _) => {
                let env = cx.env.clone();
                self.resolve_type(t, &env)
            }
            (None, Some(f)) => f.ret.clone(),
            (None, None) => self.fresh(cx),
        };
        if let Some((lid, label)) = &loop_body {
            // a loop body: outer loops stay reachable and `return` / `try`
            // keep referring to the enclosing function
            cx.frames.push(Frame { depth, captures: vec![], saved_ret, saved_loops: cx.loops.clone(), saved_yields: cx.yields });
            cx.loops.push((label.clone(), *lid));
        } else {
            cx.frames.push(Frame { depth, captures: vec![], saved_ret, saved_loops: std::mem::take(&mut cx.loops), saved_yields: cx.yields });
            cx.ret = ret_ty.clone();
        }
        cx.yields = None;
        self.push_scope(cx);
        let mut ps = vec![];
        let mut pty = vec![];
        for (i, p) in params.iter().enumerate() {
            let (mode, t) = match (&p.ty, &ef) {
                (Some(t), _) => {
                    let env = cx.env.clone();
                    let t = self.resolve_type(t, &env);
                    if let Some(f) = &ef {
                        let ft = f.params[i].1.clone();
                        if !self.unify(cx, &t, &ft) {
                            let (a, b) = (self.prog.show(&t), self.show_ty(cx, &ft));
                            self.err(p.span, format!("closure parameter has type `{}`, expected `{}`", a, b));
                        }
                    }
                    (p.mode, t)
                }
                (None, Some(f)) => {
                    let t = self.resolve(cx, &f.params[i].1);
                    if t.has_vars() {
                        self.err(p.span, format!("cannot infer the type of closure parameter `{}`; annotate it: `({}: T) => ...`", p.name, p.name));
                    }
                    (f.params[i].0, t)
                }
                (None, None) => {
                    self.err(p.span, format!("cannot infer the type of closure parameter `{}`; annotate it: `({}: T) => ...`", p.name, p.name));
                    (p.mode, Ty::Error)
                }
            };
            let mode = if let Some(f) = &ef { f.params[i].0 } else { mode };
            let id = self.declare(cx, &p.name, t.clone(), LocalKind::Param(mode), p.span, true);
            ps.push(id);
            pty.push((mode, t));
        }
        let rr = self.resolve(cx, &ret_ty);
        let tb = match &body.kind {
            ExprKind::Block(b) if loop_body.is_some() => self.block_in_scope(cx, b, Expect::NoValue, false),
            ExprKind::Block(b) => {
                let ex = if rr == Ty::Unit { Expect::NoValue } else if matches!(rr, Ty::Var(_)) { Expect::Infer } else { Expect::Ty(rr.clone()) };
                let mut tb = self.block_in_scope(cx, b, ex, false);
                if let Some(v) = &tb.value {
                    let vt = v.ty.clone();
                    if matches!(self.resolve(cx, &ret_ty), Ty::Var(_)) && vt != Ty::Never {
                        self.unify(cx, &ret_ty, &vt);
                    }
                }
                if matches!(self.resolve(cx, &ret_ty), Ty::Var(_)) {
                    self.unify(cx, &ret_ty, &Ty::Unit);
                }
                // a body that only uses `try` and runs off its end: `! E`
                if tb.value.is_none() {
                    if let Ty::Result(ok, _) = self.resolve(cx, &ret_ty) {
                        if matches!(*ok, Ty::Var(_)) {
                            self.unify(cx, &ok, &Ty::Unit);
                        }
                    }
                }
                let rt = ret_ty.clone();
                self.finish_fn_body(cx, &mut tb, &rt, b.span, false);
                tb
            }
            _ => {
                let ex_t = if matches!(rr, Ty::Var(_)) { None } else { Some(rr.clone()) };
                let e = if rr == Ty::Unit { self.expr_novalue(cx, body) } else { self.expr(cx, body, ex_t.as_ref()) };
                let e = match &ex_t {
                    Some(Ty::Unit) => self.borrow(cx, e),
                    Some(t) => {
                        let t = t.clone();
                        self.coerce(cx, e, &t, Use::Value)
                    }
                    None => {
                        let e = self.value(cx, e);
                        let et = e.ty.clone();
                        self.unify(cx, &ret_ty, &et);
                        e
                    }
                };
                let rt = self.resolve(cx, &ret_ty);
                if rt == Ty::Unit && e.ty != Ty::Unit {
                    TBlock { stmts: vec![TStmt::Expr(e)], value: None, span: body.span }
                } else {
                    TBlock { stmts: vec![], value: Some(Box::new(e)), span: body.span }
                }
            }
        };
        self.pop_scope(cx);
        cx.defer_depth = saved_defer;
        let fr = cx.frames.pop().unwrap();
        cx.ret = fr.saved_ret;
        cx.loops = fr.saved_loops;
        cx.yields = fr.saved_yields;
        let ret_final = self.resolve(cx, &ret_ty);
        let id = cx.closure_ids;
        cx.closure_ids += 1;
        // `send fn`: what it captures can be shared, read-only, by many tasks
        let mut sendable = true;
        for (l, u) in &fr.captures {
            let t = cx.locals[*l as usize].ty.clone();
            let t = self.resolve(cx, &t);
            if *u != CapUse::Read || !self.is_send(&t) {
                sendable = false;
                if ef.as_ref().is_some_and(|f| f.send) {
                    let n = cx.locals[*l as usize].name.clone();
                    let lsp = cx.locals[*l as usize].span;
                    if cx.par_frames.last() == Some(&depth) {
                        let d = if *u != CapUse::Read {
                            Diag::new(sp, format!("a `par for` body can't change `{}`: its iterations run at the same time", n)).note(Some(lsp), "declared here").note(None, "write results into the elements (`par for x in &v { x = ... }`), or share a `Mutex`")
                        } else {
                            Diag::new(sp, format!("a `par for` body uses `{}` of type `{}`, which can't be shared between tasks", n, self.prog.show(&t))).note(Some(lsp), "declared here")
                        };
                        self.diags.push(d);
                    } else {
                        let why = if *u != CapUse::Read { "changes it".to_string() } else { format!("its type `{}` is not `send`", self.prog.show(&t)) };
                        self.diags.push(Diag::new(sp, format!("this closure must be a `send fn`, but it captures `{}`, and {}", n, why)).note(None, "a `send fn` may run in many tasks at once: it can only read what it captures, and only values that can cross tasks"));
                    }
                }
            }
        }
        let send = match &ef {
            Some(f) => f.send,
            None => sendable,
        };
        let fty = Ty::Func(Rc::new(FuncTy { params: pty, ret: ret_final.clone(), send }));
        if let Some(e) = expect {
            let e = e.clone();
            self.unify(cx, &e, &fty);
        }
        let c = TClosure { id, params: ps, ret: ret_final, body: tb, captures: fr.captures, owned: false, scoped: false, by_value: vec![], loop_body: loop_body.map(|l| l.0), span: sp };
        TExpr::new(TK::Closure(Box::new(c)), fty, sp)
    }

    pub fn finalize_closure(&mut self, cx: &mut FnCx, c: &mut TClosure, owned: bool) {
        c.owned = owned;
        for (l, u) in c.captures.clone() {
            // captures declared inside an enclosing closure that is the same frame are fine
            let local = cx.locals[l as usize].clone();
            if u == CapUse::Move {
                if c.loop_body.is_some() {
                    self.diags.push(Diag::new(c.span, format!("cannot move `{}` inside this loop: the body of a `for` over a type with `each` may run many times", local.name)).note(None, "`.clone()` it, or iterate with `cursor` / `next`, whose loops allow moves"));
                } else {
                    self.err(c.span, format!("closure moves captured variable `{}` out; closures may only read or (if not escaping) modify captured variables", local.name));
                }
            }
            if owned {
                let lt = self.resolve(cx, &local.ty);
                if self.prog.contains_view(&lt) {
                    self.diags.push(Diag::new(c.span, format!("escaping closure cannot capture the borrowed view `{}`", local.name)).note(None, "the closure may outlive the data it points into; capture an owned copy (`.toString()` / `.toVec()`) instead"));
                }
                if u == CapUse::Mutate {
                    self.diags.push(Diag::new(c.span, format!("escaping closure cannot modify captured variable `{}`", local.name)).note(None, "captured variables are moved (or copied) into an escaping closure; use a Mutex or Cell for shared mutable state"));
                }
                let t = self.resolve(cx, &local.ty);
                if !self.is_copy(&t) && matches!(local.kind, LocalKind::Ref { .. } | LocalKind::Param(Mode::Borrow | Mode::Inout)) {
                    self.diags.push(Diag::new(c.span, format!("escaping closure captures the borrowed `{}`", local.name)).note(None, "only owned values can be moved into an escaping closure; `.clone()` it into a local first"));
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Calls
    // ------------------------------------------------------------------

    fn call(&mut self, cx: &mut FnCx, callee: &Expr, args: &[Expr], sp: Span, expect: Option<&Ty>) -> TExpr {
        // builtins some/ok/err
        if let ExprKind::Ident(n) = &callee.kind {
            if n.starts_with("__lazy") && self.lookup_local_quiet(cx, n).is_none() {
                if let Some(r) = self.lazy_intrinsic(cx, n, args, sp, expect) {
                    return r;
                }
            }
            if self.lookup_local_quiet(cx, n).is_none() {
                match n.as_str() {
                    "some" | "ok" | "err" => return self.ctor_call(cx, n, args, sp, expect),
                    "print" | "println" | "eprint" | "eprintln" if !self.prog.packages[cx.env.pkg as usize].items.contains_key(n) && !self.file_cx[cx.env.file].names.contains_key(n) => {
                        if let Some(r) = self.fmt_call(cx, n, args, sp) {
                            return r;
                        }
                    }
                    _ => {}
                }
            }
        }
        if let ExprKind::DotVariant(name) = &callee.kind {
            return self.dot_variant(cx, name, args, true, sp, expect);
        }
        if let ExprKind::OptField(base, name) = &callee.kind {
            return self.opt_call(cx, base, name, callee.span, args, sp);
        }
        if let ExprKind::Field(base, name, nsp) = &callee.kind {
            if let Some(r) = self.lazy_terminal(cx, base, name, args, sp, expect) {
                return r;
            }
            // package function / variant constructor / static
            let br = self.resolve_path(cx, base);
            match br {
                Some(PathRes::Pkg(p, ref pname)) if p != u32::MAX => {
                    if self.prog.packages[p as usize].name == "fmt" || pname == "fmt" {
                        if let Some(r) = self.fmt_call(cx, name, args, sp) {
                            return r;
                        }
                    }
                    if self.prog.packages[p as usize].name == "time" && name == "sleep" && self.prog.packages[p as usize].is_std {
                        self.no_wait_under_lock(cx, sp, "this sleep waits");
                    }
                    if self.prog.packages[p as usize].name == "time" && (name == "after" || name == "at") && self.prog.packages[p as usize].is_std && !self.prog.packages[p as usize].items.contains_key(name.as_str()) {
                        self.diags.push(Diag::new(sp, format!("`time.{}` is a `select` arm: `select {{ time.{}(d) => ... }}`", name, name)).note(None, "to wait, call `time.sleep(d)`"));
                        return TExpr::error(sp);
                    }
                    if self.prog.packages[p as usize].name == "task" && name == "group" {
                        self.diags.push(Diag::new(sp, "`task.group` is gone: write `scope { go ... }`").note(None, "a `scope` block waits for the tasks started in it with `go`; `try scope { }` when they can fail"));
                        return TExpr::error(sp);
                    }
                }
                Some(PathRes::TypeCon(..)) => {
                    if let Some(PathRes::Variant(t, vi)) = self.resolve_path(cx, callee) {
                        return self.variant_value(cx, t, vi, args, true, sp, expect);
                    }
                    if let Some(PathRes::TypeCon(t, gd)) = br.clone() {
                        let key = match (&t, gd) {
                            (_, Some(d)) => Some(TyKey::Adt(d)),
                            _ => TyKey::of(&t),
                        };
                        if let Some(&f) = key.and_then(|k| self.prog.assoc.get(&(k, name.clone()))) {
                            let owner = self.generic_type_instance(cx, t, gd);
                            if !self.prog.func(f).is_pub && self.prog.func(f).pkg != cx.env.pkg {
                                self.err(*nsp, format!("`{}` is not public", name));
                            }
                            cx.call_owner = Some(owner);
                            return self.call_fn(cx, f, None, None, args, sp, expect);
                        }
                    }
                    if name == "default" && args.is_empty() {
                        if let Some(PathRes::TypeCon(t, gd)) = br {
                            let t = self.generic_type_instance(cx, t, gd);
                            if let Some(e) = expect {
                                let e = e.clone();
                                self.try_unify(cx, &t, &e);
                            }
                            let t = self.resolve(cx, &t);
                            if !self.is_default(&t) {
                                let s = self.prog.show(&t);
                                self.err(sp, format!("`{}` has no default value", s));
                            }
                            return TExpr::new(TK::DefaultOf, t, sp);
                        }
                    }
                    self.err(*nsp, format!("unknown associated item `{}`", name));
                    return TExpr::error(sp);
                }
                Some(PathRes::Value) | Some(PathRes::Global(_)) | None => return self.method_call(cx, base, name, *nsp, None, args, sp, expect),
                _ => {}
            }
        }
        if let ExprKind::TypeArgs(b, _) | ExprKind::Index(b, _) = &callee.kind {
            if let ExprKind::Field(base, name, nsp) = &b.kind {
                if matches!(self.resolve_path(cx, base), Some(PathRes::Value) | Some(PathRes::Global(_)) | None) {
                    let targs = match &callee.kind {
                        ExprKind::TypeArgs(_, ts) => {
                            let env = cx.env.clone();
                            Some(ts.iter().map(|t| self.resolve_type(t, &env)).collect::<Vec<_>>())
                        }
                        ExprKind::Index(_, xs) => {
                            let env = cx.env.clone();
                            let mut v = vec![];
                            for x in xs {
                                match expr_to_type(x) {
                                    Some(t) => v.push(self.resolve_type(&t, &env)),
                                    None => {
                                        self.err(x.span, "expected a type argument");
                                        return TExpr::error(sp);
                                    }
                                }
                            }
                            Some(v)
                        }
                        _ => None,
                    };
                    return self.method_call(cx, base, name, *nsp, targs, args, sp, expect);
                }
            }
        }
        match self.resolve_path(cx, callee) {
            Some(PathRes::Func(f, targs)) => return self.call_fn(cx, f, targs, None, args, sp, expect),
            Some(PathRes::Variant(t, vi)) => return self.variant_value(cx, t, vi, args, true, sp, expect),
            Some(PathRes::TypeCon(t, gd)) => {
                // `T(args)` calls `T.new(args)` (`RwLock(x)`, `Atomic(0)`, `Stack[int]()`)
                let did = match (&t, gd) {
                    (_, Some(g)) => Some(g),
                    (Ty::Adt(d, _), _) => Some(*d),
                    _ => None,
                };
                if let Some(d) = did {
                    let def = self.prog.def(d);
                    if !def.newtype && Some(d) != self.prog.prelude_type("Chan") && self.prog.assoc.contains_key(&(TyKey::Adt(d), "new".to_string())) {
                        let f = Expr { kind: ExprKind::Field(Box::new(callee.clone()), "new".into(), callee.span), span: callee.span };
                        return self.call(cx, &f, args, sp, expect);
                    }
                }
                return self.type_call(cx, t, gd, None, args, sp, expect);
            }
            Some(PathRes::Pkg(u32::MAX, name)) => return self.type_call(cx, Ty::Error, None, Some(name), args, sp, expect),
            Some(PathRes::Const(u32::MAX)) => return TExpr::error(sp),
            _ => {}
        }
        // call a func-typed value
        let f = self.expr(cx, callee, None);
        let ft = self.resolve(cx, &f.ty);
        match &ft {
            Ty::Func(fty) => {
                let fty = fty.clone();
                if fty.params.len() != args.len() {
                    self.err(sp, format!("function value takes {} argument(s), {} given", fty.params.len(), args.len()));
                    return TExpr::error(sp);
                }
                let f = self.borrow(cx, f);
                let mut targs = vec![TArg { mode: ArgMode::Borrow, e: f }];
                for (a, (m, t)) in args.iter().zip(fty.params.iter()) {
                    let p = FuncParam { name: String::new(), mode: *m, ty: t.clone(), span: sp };
                    targs.push(self.check_arg(cx, a, &p));
                }
                TExpr::new(TK::Call { callee: Callee::Value, args: targs }, fty.ret.clone(), sp)
            }
            Ty::Error => TExpr::error(sp),
            _ => {
                let s = self.prog.show(&ft);
                self.err(callee.span, format!("cannot call a value of type `{}`", s));
                TExpr::error(sp)
            }
        }
    }

    fn lookup_local_quiet(&self, cx: &FnCx, name: &str) -> Option<LocalId> {
        for sc in cx.scopes.iter().rev() {
            for (n, id) in sc.iter().rev() {
                if n == name {
                    return Some(*id);
                }
            }
        }
        None
    }

    fn ctor_call(&mut self, cx: &mut FnCx, n: &str, args: &[Expr], sp: Span, expect: Option<&Ty>) -> TExpr {
        if args.len() != 1 {
            self.err(sp, format!("`{}` takes exactly one value", n));
            return TExpr::error(sp);
        }
        match n {
            "some" => {
                let inner = match expect {
                    Some(Ty::Option(t)) => Some((**t).clone()),
                    _ => None,
                };
                let e = self.expr_value(cx, &args[0], inner.as_ref());
                let t = Ty::opt(self.resolve(cx, &e.ty));
                TExpr::new(TK::SomeLit(Box::new(e)), t, sp)
            }
            _ => {
                let (okt, errt) = match expect {
                    Some(Ty::Result(a, b)) => ((**a).clone(), (**b).clone()),
                    _ => {
                        let mut r = cx.ret.clone();
                        r = self.resolve(cx, &r);
                        match r {
                            Ty::Result(a, b) => ((*a).clone(), (*b).clone()),
                            _ => (self.fresh(cx), self.fresh(cx)),
                        }
                    }
                };
                if n == "ok" {
                    let e = self.expr_value(cx, &args[0], Some(&okt));
                    let t = Ty::result(self.resolve(cx, &e.ty), errt);
                    TExpr::new(TK::OkLit(Box::new(e)), t, sp)
                } else {
                    let e = self.expr_value(cx, &args[0], Some(&errt));
                    let t = Ty::result(okt, self.resolve(cx, &e.ty));
                    TExpr::new(TK::ErrLit(Box::new(e)), t, sp)
                }
            }
        }
    }

    fn fmt_call(&mut self, cx: &mut FnCx, name: &str, args: &[Expr], sp: Span) -> Option<TExpr> {
        let ret = match name {
            "print" | "println" | "eprint" | "eprintln" | "printf" => Ty::Unit,
            "sprint" | "sprintln" | "format" => Ty::String,
            _ => return None,
        };
        if name == "format" || name == "printf" {
            return Some(self.format_call(cx, name, args, ret, sp));
        }
        let mut targs = vec![];
        for a in args {
            let e = self.expr(cx, a, None);
            let e = self.borrow(cx, e);
            let t = self.resolve(cx, &e.ty);
            if matches!(t, Ty::Func(_)) {
                self.err(a.span, "functions cannot be printed");
            }
            targs.push(TArg { mode: ArgMode::Borrow, e });
        }
        Some(TExpr::new(TK::Intrinsic { name: format!("fmt.{}", name), targs: vec![], args: targs }, ret, sp))
    }

    /// `with` in expression position. With a value, `let n = with v = m.lock() { v.len() }`
    /// is checked as `var h: ?T = none; with v = m.lock() { h = some(v.len()) };
    /// h.unwrap()`, so the borrow rules of the body (nothing may leave a lock) apply as they
    /// are; the value can't be a view.
    fn with_expr(&mut self, cx: &mut FnCx, inner: &Stmt, ex: Expect, sp: Span) -> TExpr {
        let want_value = !matches!(ex, Expect::NoValue);
        if want_value {
            cx.with_value = Some(ex);
        }
        let Some(mut ts) = self.stmt(cx, inner) else { return TExpr::error(sp) };
        cx.with_value = None;
        let body = match &mut ts {
            TStmt::With { body, .. } | TStmt::WithLocks { body, .. } => body,
            _ => return TExpr::error(sp),
        };
        let v = body.value.take();
        let Some(mut v) = v.map(|b| *b) else {
            let ty = if block_diverges(body) { Ty::Never } else { Ty::Unit };
            return TExpr::new(TK::Block(TBlock { stmts: vec![ts], value: None, span: sp }), ty, sp);
        };
        // a unit value wrapped to fit an expected `! E` or `?T` is no value
        if let TK::OkLit(inner) | TK::SomeLit(inner) = &v.kind {
            if self.resolve(cx, &inner.ty) == Ty::Unit {
                let (TK::OkLit(inner) | TK::SomeLit(inner)) = std::mem::replace(&mut v.kind, TK::Unit) else { unreachable!() };
                v = *inner;
            }
        }
        let vt = self.resolve(cx, &v.ty);
        if matches!(vt, Ty::Never | Ty::Error | Ty::Unit) {
            body.stmts.push(TStmt::Expr(v));
            let ty = if vt == Ty::Unit { Ty::Unit } else { vt };
            return TExpr::new(TK::Block(TBlock { stmts: vec![ts], value: None, span: sp }), ty, sp);
        }
        if self.prog.contains_view(&vt) {
            let s = self.prog.show(&vt);
            self.diags.push(Diag::new(v.span, format!("the value of a `with` block can't be a view (`{}`): what it borrows is only reachable inside the block", s)).note(None, "give back an owned copy (`.toString()`, `.clone()`) or a value computed from it"));
            return TExpr::error(sp);
        }
        let ot = Ty::opt(vt.clone());
        let h = self.hidden_local(cx, ot.clone(), LocalKind::Var, sp);
        let vsp = v.span;
        body.stmts.push(TStmt::Assign { place: TExpr::new(TK::Local(h), ot.clone(), vsp), op: None, value: TExpr::new(TK::SomeLit(Box::new(v)), ot.clone(), vsp), span: vsp });
        let un = self.method_call_on(cx, TExpr::new(TK::Local(h), ot.clone(), sp), "unwrap", sp, None, &[], sp, None);
        TExpr::new(TK::Block(TBlock { stmts: vec![TStmt::Let { local: h, init: TExpr::new(TK::NoneLit, ot, sp) }, ts], value: Some(Box::new(un)), span: sp }), vt, sp)
    }

    /// `"x = ${x:>4}"`: the same as `fmt.format("x = {:>4}", x)`, a `String`.
    fn interp(&mut self, cx: &mut FnCx, parts: &[ast::InterpPart], sp: Span) -> TExpr {
        let mut tmpl = String::new();
        let mut args = vec![Expr { kind: ExprKind::Str(String::new()), span: sp }];
        for p in parts {
            match p {
                ast::InterpPart::Lit(l) => tmpl.push_str(&l.replace('{', "{{").replace('}', "}}")),
                ast::InterpPart::Expr(e, spec, psp) => {
                    match spec {
                        Some(s) => {
                            if let Err(m) = crate::fmtspec::parse(&format!("{{:{}}}", s)) {
                                self.err(*psp, m);
                                let _ = self.expr(cx, e, None);
                                return TExpr::error(sp);
                            }
                            tmpl.push_str(&format!("{{:{}}}", s));
                        }
                        None => tmpl.push_str("{}"),
                    }
                    args.push((**e).clone());
                }
            }
        }
        args[0].kind = ExprKind::Str(tmpl);
        self.format_call(cx, "format", &args, Ty::String, sp)
    }

    /// `fmt.format("x = {:>4}", x)`: the template is checked here, against the
    /// arguments (see fmtspec.rs).
    fn format_call(&mut self, cx: &mut FnCx, name: &str, args: &[Expr], ret: Ty, sp: Span) -> TExpr {
        let Some(first) = args.first() else {
            self.err(sp, format!("`fmt.{}` needs a template: `fmt.{}(\"x = {{}}\", x)`", name, name));
            return TExpr::error(sp);
        };
        let parsed = match &first.kind {
            ExprKind::Str(t) => crate::fmtspec::parse(t).map(|p| (t.clone(), p)),
            _ => Err("the template must be a string literal, so that the compiler can check it".to_string()),
        };
        let (t, pieces) = match parsed {
            Ok(x) => x,
            Err(m) => {
                self.err(first.span, m);
                // still check the arguments (and count their variables as used)
                for a in args {
                    let _ = self.expr(cx, a, None);
                }
                return TExpr::error(sp);
            }
        };
        let t = &t;
        let specs: Vec<crate::fmtspec::Spec> = pieces.iter().filter_map(|p| if let crate::fmtspec::Piece::Arg(s) = p { Some(s.clone()) } else { None }).collect();
        if specs.len() != args.len() - 1 {
            self.err(sp, format!("the template has {} placeholder{}, but {} value{} {} given", specs.len(), if specs.len() == 1 { "" } else { "s" }, args.len() - 1, if args.len() == 2 { "" } else { "s" }, if args.len() == 2 { "is" } else { "are" }));
        }
        let mut targs = vec![TArg { mode: ArgMode::Borrow, e: TExpr::new(TK::Str(t.clone()), Ty::Str, first.span) }];
        for (i, a) in args[1..].iter().enumerate() {
            let e = self.expr(cx, a, None);
            let e = self.borrow(cx, e);
            let ty = self.resolve(cx, &e.ty);
            if matches!(ty, Ty::Func(_)) {
                self.err(a.span, "functions cannot be printed");
            }
            if let Some(s) = specs.get(i) {
                let shown = self.prog.show(&ty);
                if s.radix.is_some() && !ty.is_int() && !ty.is_error() {
                    self.err(a.span, format!("`{{:{}}}` formats integers, not `{}`", s.radix.unwrap_or('x'), shown));
                }
                if s.precision.is_some() && !matches!(ty, Ty::F32 | Ty::F64 | Ty::Error) {
                    self.err(a.span, format!("a precision (`{{:.N}}`) is for floats, not `{}`", shown));
                }
                if s.zero && !ty.is_numeric() && !ty.is_error() {
                    self.err(a.span, format!("zero padding (`{{:0N}}`) is for numbers, not `{}`", shown));
                }
            }
            targs.push(TArg { mode: ArgMode::Borrow, e });
        }
        TExpr::new(TK::Intrinsic { name: format!("fmt.{}", name), targs: vec![], args: targs }, ret, sp)
    }

    /// `T(x)`: conversions and wrapper constructors.
    fn type_call(&mut self, cx: &mut FnCx, t: Ty, gd: Option<DefId>, builtin: Option<String>, args: &[Expr], sp: Span, expect: Option<&Ty>) -> TExpr {
        if let Some(name) = builtin {
            if args.len() != 1 {
                self.err(sp, format!("`{}(x)` takes one value", name));
                return TExpr::error(sp);
            }
            if let (true, ExprKind::Ref(inner)) = (name == "Mutex", &args[0].kind) {
                // `Mutex(&x)`: lock a borrowed place
                let p = self.expr(cx, inner, None);
                if !p.is_place() {
                    self.err(inner.span, "`Mutex(&x)` needs a variable or another place to lock");
                    return TExpr::error(sp);
                }
                self.require_mut(cx, &p, "lock");
                let t = Rc::new(self.resolve(cx, &p.ty));
                return TExpr::new(TK::MutexRef(Box::new(p)), Ty::MutexRef(t), sp);
            }
            // `let x ?Box[String] = Box("a")`: the wrapper inside the optional
            let expect = match expect.map(|t| self.resolve(cx, t)) {
                Some(Ty::Option(t)) => Some((*t).clone()),
                e => e,
            };
            let expect = expect.as_ref();
            let inner_expect = match (name.as_str(), expect) {
                ("Box", Some(Ty::Box(t))) | ("Rc", Some(Ty::Rc(t))) | ("Arc", Some(Ty::Arc(t))) | ("Mutex", Some(Ty::Mutex(t))) | ("Cell", Some(Ty::Cell(t))) => Some((**t).clone()),
                // like `["a"]`, a wrapped string literal is a `String`: views can't be stored
                _ if matches!(args[0].kind, ExprKind::Str(_) | ExprKind::Interp(_)) => Some(Ty::String),
                _ => None,
            };
            let e = self.expr(cx, &args[0], inner_expect.as_ref());
            let e = match &inner_expect {
                Some(t) => self.coerce(cx, e, t, Use::Value),
                None => self.value(cx, e),
            };
            let it = Rc::new(self.resolve(cx, &e.ty));
            let ty = match name.as_str() {
                "Box" => Ty::Box(it.clone()),
                "Rc" => Ty::Rc(it.clone()),
                "Arc" => Ty::Arc(it.clone()),
                "Mutex" => Ty::Mutex(it.clone()),
                "Cell" => {
                    if !self.is_copy(&it) {
                        self.err(sp, "Cell[T] needs a copy type; use Mutex or Rc for other types");
                    }
                    Ty::Cell(it.clone())
                }
                "Vec" | "Map" | "Weak" => {
                    self.err(sp, format!("`{}` cannot be called; use a literal like `{}[T]{{}}`", name, name));
                    return TExpr::error(sp);
                }
                _ => return TExpr::error(sp),
            };
            if matches!(ty, Ty::Arc(_)) {
                if !self.is_send(&it) {
                    let s = self.prog.show(&it);
                    self.err(sp, format!("Arc[{}] would share a non-thread-safe value; use Rc[T] for single-threaded sharing", s));
                }
            }
            return TExpr::new(TK::Wrap(Box::new(e)), ty, sp);
        }
        let t = self.generic_type_instance(cx, t, gd);
        match &t {
            Ty::Adt(d, targs) if Some(*d) == self.prog.prelude_type("Chan") => {
                let elem = targs[0].clone();
                if !self.is_send(&elem) {
                    let s = self.prog.show(&elem);
                    self.err(sp, format!("channel element type `{}` cannot be sent between tasks", s));
                }
                let n = if args.is_empty() {
                    TExpr::new(TK::Int(0), Ty::int(), sp)
                } else {
                    self.expr_value(cx, &args[0], Some(&Ty::int()))
                };
                return TExpr::new(TK::MakeChan(Box::new(n)), t.clone(), sp);
            }
            Ty::Int(_) | Ty::F32 | Ty::F64 => {
                if args.len() != 1 {
                    self.err(sp, "conversion takes one value");
                    return TExpr::error(sp);
                }
                // The target type only types literals (`u64(18446744073709551615)`,
                // `u64(1 << 63)`); anything else converts from its own type
                // (`int(~b)` for `b u8`).
                let hint = if literal_only(&args[0]) { Some(&t) } else { None };
                let e = self.expr(cx, &args[0], hint);
                let e = self.value(cx, e);
                let et = self.resolve(cx, &e.ty);
                let ok = et.is_numeric() || et.is_error() || (matches!(et, Ty::Bool) && t.is_int()) || matches!(&et, Ty::Adt(d, _) if self.prog.def(*d).is_simple_enum()) || matches!(&et, Ty::Adt(d, _) if self.prog.def(*d).newtype) || self.param_numeric(&et);
                if !ok {
                    let (a, b) = (self.prog.show(&et), self.prog.show(&t));
                    self.err(sp, format!("cannot convert `{}` to `{}`", a, b));
                }
                if let Ty::Adt(d, _) = &et {
                    if self.prog.def(*d).newtype {
                        let ft = self.prog.def(*d).fields()[0].ty.clone();
                        let inner = TExpr::new(TK::Field { base: Box::new(e), index: 0 }, ft, sp);
                        return TExpr::new(TK::Cast(Box::new(inner)), t.clone(), sp);
                    }
                }
                return TExpr::new(TK::Cast(Box::new(e)), t.clone(), sp);
            }
            Ty::String => {
                if args.len() != 1 {
                    self.err(sp, "`String(x)` takes one value");
                    return TExpr::error(sp);
                }
                let e = self.expr(cx, &args[0], None);
                let et = self.resolve(cx, &e.ty);
                if et.is_stringish() {
                    let e = self.coerce(cx, e, &Ty::Str, Use::Borrow);
                    return TExpr::new(TK::Intrinsic { name: "str_to_string".into(), targs: vec![], args: vec![TArg { mode: ArgMode::Borrow, e }] }, Ty::String, sp);
                }
                if matches!(et, Ty::Int(IntTy::I32)) {
                    let e = self.value(cx, e);
                    return TExpr::new(TK::Intrinsic { name: "rune_to_string".into(), targs: vec![], args: vec![TArg { mode: ArgMode::Value, e }] }, Ty::String, sp);
                }
                if let Ty::Slice(el) | Ty::Vec(el) = &et {
                    if **el == Ty::Int(IntTy::U8) {
                        let e = self.coerce(cx, e, &Ty::Slice(Rc::new(Ty::Int(IntTy::U8))), Use::Borrow);
                        return TExpr::new(TK::Intrinsic { name: "bytes_to_string".into(), targs: vec![], args: vec![TArg { mode: ArgMode::Borrow, e }] }, Ty::String, sp);
                    }
                }
                let s = self.prog.show(&et);
                self.err(sp, format!("cannot convert `{}` to String; use fmt.sprint for formatting", s));
                return TExpr::error(sp);
            }
            Ty::Adt(d, _) if self.prog.def(*d).newtype => {
                if args.len() != 1 {
                    self.err(sp, "conversion takes one value");
                    return TExpr::error(sp);
                }
                let d = *d;
                let ft = self.prog.def(d).fields()[0].ty.clone();
                let e = self.expr(cx, &args[0], Some(&ft));
                let e = self.coerce(cx, e, &ft, Use::Value);
                return TExpr::new(TK::StructLit { def: d, targs: vec![], fields: vec![e] }, t.clone(), sp);
            }
            Ty::Error => return TExpr::error(sp),
            _ => {}
        }
        let s = self.prog.show(&t);
        self.err(sp, format!("`{}` cannot be called; build structs with `{}{{...}}`", s, s));
        TExpr::error(sp)
    }

    pub fn check_arg(&mut self, cx: &mut FnCx, a: &Expr, p: &FuncParam) -> TArg {
        let pty = self.resolve(cx, &p.ty);
        match p.mode {
            Mode::Inout => {
                let inner = match &a.kind {
                    ExprKind::Ref(inner) => inner,
                    _ => {
                        self.diags.push(Diag::new(a.span, format!("parameter `{}` is inout: pass a mutable reference with `&`", p.name)).note(None, "write `&x` so that modifications are visible at the call site"));
                        let e = self.expr(cx, a, Some(&pty));
                        return TArg { mode: ArgMode::Inout, e };
                    }
                };
                let e = self.expr(cx, inner, Some(&pty));
                let et = self.resolve(cx, &e.ty);
                let e = match (&et, &pty) {
                    (Ty::Vec(x), Ty::Slice(y)) if self.try_unify(cx, x, y) => {
                        self.require_mut(cx, &e, "mutably borrow");
                        let sp = e.span;
                        TExpr::new(TK::Coerce(Coercion::VecToSlice, Box::new(e)), pty.clone(), sp)
                    }
                    (Ty::Array(x, _) | Ty::ArrayT(x, _), Ty::Slice(y)) if self.try_unify(cx, x, y) => {
                        self.require_mut(cx, &e, "mutably borrow");
                        let sp = e.span;
                        TExpr::new(TK::Coerce(Coercion::ArrayToSlice, Box::new(e)), pty.clone(), sp)
                    }
                    (Ty::Iface(..), Ty::Iface(..)) => {
                        let ok = self.unify(cx, &et, &pty);
                        if !ok {
                            let (x, y) = (self.prog.show(&et), self.prog.show(&pty));
                            self.err(e.span, format!("mismatched types: expected `{}`, found `{}`", y, x));
                        }
                        self.require_mut(cx, &e, "mutably borrow");
                        e
                    }
                    (_, Ty::Iface(d, iargs)) if !matches!(et, Ty::Var(_) | Ty::Error) => {
                        let (d, iargs) = (*d, iargs.clone());
                        if let Err(m) = self.implements(&et, d, &iargs) {
                            self.err(e.span, m);
                        }
                        self.require_mut(cx, &e, "mutably borrow");
                        let sp = e.span;
                        TExpr::new(TK::Coerce(Coercion::ToIface { owned: false }, Box::new(e)), pty.clone(), sp)
                    }
                    _ => {
                        if !self.unify(cx, &et, &pty) {
                            let (x, y) = (self.prog.show(&et), self.show_ty(cx, &pty));
                            self.err(e.span, format!("mismatched types: expected `{}`, found `{}`", y, x));
                        }
                        self.require_mut(cx, &e, "mutably borrow");
                        e
                    }
                };
                TArg { mode: ArgMode::Inout, e }
            }
            Mode::Borrow => {
                if let ExprKind::Ref(_) = &a.kind {
                    self.err(a.span, format!("parameter `{}` is borrowed read-only; `&` is only for inout parameters", p.name));
                }
                let e = self.expr(cx, a, Some(&pty));
                let e = self.coerce(cx, e, &pty, Use::Borrow);
                TArg { mode: ArgMode::Borrow, e }
            }
            Mode::Sink => {
                if let ExprKind::Ref(_) = &a.kind {
                    self.err(a.span, format!("parameter `{}` takes ownership (sink); pass the value without `&`", p.name));
                }
                let e = self.expr(cx, a, Some(&pty));
                let e = self.coerce(cx, e, &pty, Use::Value);
                TArg { mode: ArgMode::Value, e }
            }
        }
    }

    pub fn call_fn(&mut self, cx: &mut FnCx, fid: FuncId, explicit: Option<Vec<Ty>>, recv: Option<TExpr>, args: &[Expr], sp: Span, expect: Option<&Ty>) -> TExpr {
        let fd = self.prog.func(fid).clone();
        if fd.kind == FuncKind::Test {
            self.err(sp, "tests cannot be called");
            return TExpr::error(sp);
        }
        if matches!(fd.kind, FuncKind::ExternC { .. }) {
            self.unsafe_op(cx, sp, format!("calling the extern function `{}` requires an `unsafe` block", fd.name));
        } else if fd.is_unsafe {
            self.unsafe_op(cx, sp, format!("calling the unsafe function `{}` requires an `unsafe` block", fd.name));
        }
        let mut s: HashMap<TParamId, Ty> = HashMap::new();
        let n_recv_params = match &fd.recv {
            Some(r) => {
                let mut tmp = vec![];
                r.ty.walk(&mut |t| {
                    if let Ty::Param(p) = t {
                        if !tmp.contains(p) {
                            tmp.push(*p)
                        }
                    }
                });
                tmp.len()
            }
            None => 0,
        };
        for &tp in &fd.tparams {
            let v = self.fresh(cx);
            s.insert(tp, v);
        }
        if let (Some(owner), Some(pat)) = (cx.call_owner.take(), &fd.owner_ty) {
            let pat = pat.subst(&s);
            if !self.unify(cx, &pat, &owner) {
                let (a, b) = (self.show_ty(cx, &owner), self.show_ty(cx, &pat));
                self.err(sp, format!("`{}` is defined for `{}`, not `{}`", fd.name, b, a));
            }
        }
        if let Some(ex) = explicit {
            let own = &fd.tparams[n_recv_params.min(fd.tparams.len())..];
            if ex.len() != own.len() {
                self.err(sp, format!("`{}` expects {} type argument(s), {} given", fd.name, own.len(), ex.len()));
            } else {
                for (tp, t) in own.iter().zip(ex) {
                    let v = s[tp].clone();
                    self.unify(cx, &v, &t);
                }
            }
        }
        let mut out_args = vec![];
        if let Some(r) = recv {
            let rp = fd.recv.clone().unwrap();
            let rty = rp.ty.subst(&s);
            let rt = r.ty.clone();
            if !self.unify(cx, &rty, &rt) {
                let (a, b) = (self.show_ty(cx, &rt), self.show_ty(cx, &rty));
                self.err(r.span, format!("receiver type mismatch: `{}` vs `{}`", a, b));
            }
            let arg = match rp.mode {
                Mode::Borrow => {
                    let r = self.borrow(cx, r);
                    TArg { mode: ArgMode::Borrow, e: r }
                }
                Mode::Inout => {
                    let what = format!("call `{}` (it modifies its receiver) on", fd.name);
                    self.require_mut(cx, &r, &what);
                    TArg { mode: ArgMode::Inout, e: r }
                }
                Mode::Sink => {
                    let r = self.value(cx, r);
                    TArg { mode: ArgMode::Value, e: r }
                }
            };
            out_args.push(arg);
        }
        if let Some(ex) = expect {
            if fd.ret.has_params() && fd.yields.is_none() {
                let rt = fd.ret.subst(&s);
                let ex = ex.clone();
                self.try_unify(cx, &rt, &ex);
            }
        }
        if args.len() != fd.params.len() {
            self.err(sp, format!("`{}` takes {} argument(s), {} given", fd.name, fd.params.len(), args.len()));
            for a in args {
                let _ = self.expr(cx, a, None);
            }
            return TExpr::error(sp);
        }
        let mut checked: Vec<Option<TArg>> = vec![None; args.len()];
        for pass in 0..2 {
            for (i, a) in args.iter().enumerate() {
                if is_closure_expr(a) != (pass == 1) {
                    continue;
                }
                let p = &fd.params[i];
                let p2 = FuncParam { name: p.name.clone(), mode: p.mode, ty: p.ty.subst(&s), span: p.span };
                checked[i] = Some(self.check_arg(cx, a, &p2));
            }
        }
        out_args.extend(checked.into_iter().map(|a| a.unwrap()));
        let mut targs = vec![];
        for &tp in &fd.tparams {
            let t = self.resolve(cx, &s[&tp]);
            targs.push(t);
        }
        for (i, &tp) in fd.tparams.iter().enumerate() {
            if targs[i].has_vars() {
                // may be resolved later by the context (e.g. assignment)
                continue;
            }
            let bounds = self.prog.tparam(tp).bounds.clone();
            for b in bounds {
                let b = match b {
                    Bound::Iface(d, a) => Bound::Iface(d, a.iter().map(|x| x.subst(&s)).collect()),
                    c => c,
                };
                let t = targs[i].clone();
                if let Err(m) = self.satisfies(&t, &b) {
                    let tpn = self.prog.tparam(tp).name.clone();
                    self.err(sp, format!("`{}` requires `{}`: {}", fd.name, tpn, m));
                }
            }
        }
        let ret = self.resolve(cx, &fd.ret.subst(&s));
        if !self.prog.packages[fd.pkg as usize].is_std || fd.kind != FuncKind::Intrinsic {
            let mut sig_tys = vec![ret.clone()];
            for p in &fd.params {
                sig_tys.push(self.resolve(cx, &p.ty.subst(&s)));
            }
            if let Some(t) = sig_tys.iter().find(|t| self.prog.has_nested_view(&t)) {
                let ts = self.prog.show(t);
                self.err(sp, format!("calling `{}` here would store a borrowed view inside `{}`; use owned values", fd.name, ts));
            }
        }
        let call = TExpr::new(TK::Call { callee: Callee::Func(fid, targs.clone()), args: out_args }, ret.clone(), sp);
        if targs.iter().any(|t| t.has_vars()) {
            let names: Vec<String> = fd.tparams.iter().zip(targs.iter()).filter(|(_, t)| t.has_vars()).map(|(p, _)| self.prog.tparam(*p).name.clone()).collect();
            self.pending_infer.push((sp, fd.name.clone(), names, call.clone()));
        }
        if let Some(y) = fd.yields {
            let ty = if fd.yield_opt { Ty::opt(ret) } else { ret };
            return TExpr::new(TK::Proj { call: Box::new(call), mutable: y == YieldKind::Mut, opt: fd.yield_opt }, ty, sp);
        }
        call
    }

    fn method_call(&mut self, cx: &mut FnCx, base: &Expr, name: &str, nsp: Span, targs: Option<Vec<Ty>>, args: &[Expr], sp: Span, expect: Option<&Ty>) -> TExpr {
        let b = self.expr(cx, base, None);
        self.method_call_on(cx, b, name, nsp, targs, args, sp, expect)
    }

    pub fn method_call_on(&mut self, cx: &mut FnCx, b: TExpr, name: &str, nsp: Span, targs: Option<Vec<Ty>>, args: &[Expr], sp: Span, expect: Option<&Ty>) -> TExpr {
        let bty = self.resolve(cx, &b.ty);
        if bty.is_error() {
            for a in args {
                let _ = self.expr(cx, a, None);
            }
            return TExpr::error(sp);
        }
        if let (TK::Proj { opt: true, .. }, "unwrap" | "expect") = (&b.kind, name) {
            let inner = match &bty {
                Ty::Option(t) => (**t).clone(),
                _ => bty.clone(),
            };
            let msg = if name == "expect" {
                if args.len() != 1 {
                    self.err(sp, "`expect` takes a message");
                    return TExpr::error(sp);
                }
                Some(Box::new(self.expr_value(cx, &args[0], Some(&Ty::Str))))
            } else {
                None
            };
            return TExpr::new(TK::ProjUnwrap { proj: Box::new(b), msg }, inner, sp);
        }
        if name == "go" && bty == Ty::Scope {
            return self.scope_value_go(cx, b, args, sp);
        }
        if name == "join" && matches!(bty, Ty::Task(_)) {
            self.no_wait_under_lock(cx, sp, "this `join` waits for a task");
        }
        // clone: user method or structural
        if name == "clone" && args.is_empty() {
            let user = match &bty {
                Ty::Adt(d, _) => self.prog.methods.get(&(TyKey::Adt(*d), "clone".into())).cloned(),
                _ => None,
            };
            if user.is_none() {
                if let Ty::Param(p) = &bty {
                    if !self.has_bound(*p, Cap::Clone) {
                        let n = self.prog.tparam(*p).name.clone();
                        self.err(sp, format!("cannot clone `{}`: add the constraint `{}: clone`", n, n));
                    }
                } else if !self.is_clone(&bty) {
                    let s = self.prog.show(&bty);
                    self.err(sp, format!("`{}` cannot be cloned", s));
                }
                let b = self.borrow(cx, b);
                return TExpr::new(TK::Intrinsic { name: "clone".into(), targs: vec![], args: vec![TArg { mode: ArgMode::Borrow, e: b }] }, bty, sp);
            }
        }
        if let Ty::Iface(d, iargs) = &bty {
            let (d, iargs) = (*d, iargs.clone());
            let methods = match &self.prog.def(d).kind {
                TypeDefKind::Iface { methods, .. } => methods.clone(),
                _ => vec![],
            };
            if let Some(mi) = methods.iter().position(|m| m.name == name) {
                let m = methods[mi].clone();
                let s = self.prog.def_subst(d, &iargs);
                if args.len() != m.params.len() {
                    self.err(sp, format!("`{}` takes {} argument(s), {} given", name, m.params.len(), args.len()));
                    return TExpr::error(sp);
                }
                let recv = match m.recv_mode {
                    Mode::Inout => {
                        self.require_mut(cx, &b, &format!("call `{}` (it modifies its receiver) on", name));
                        TArg { mode: ArgMode::Inout, e: b }
                    }
                    _ => {
                        let b = self.borrow(cx, b);
                        TArg { mode: ArgMode::Borrow, e: b }
                    }
                };
                let mut out = vec![recv];
                for (a, (pn, pm, pt)) in args.iter().zip(m.params.iter()) {
                    let p = FuncParam { name: pn.clone(), mode: *pm, ty: pt.subst(&s), span: sp };
                    out.push(self.check_arg(cx, a, &p));
                }
                return TExpr::new(TK::Call { callee: Callee::Iface { def: d, method: mi }, args: out }, m.ret.subst(&s), sp);
            }
        }
        if let Ty::Param(p) = &bty {
            let p = *p;
            let bounds = self.prog.tparam(p).bounds.clone();
            for bd in &bounds {
                if let Bound::Iface(d, iargs) = bd {
                    let methods = match &self.prog.def(*d).kind {
                        TypeDefKind::Iface { methods, .. } => methods.clone(),
                        _ => vec![],
                    };
                    if let Some(m) = methods.iter().find(|m| m.name == name) {
                        let s = self.prog.def_subst(*d, iargs);
                        if args.len() != m.params.len() {
                            self.err(sp, format!("`{}` takes {} argument(s), {} given", name, m.params.len(), args.len()));
                            return TExpr::error(sp);
                        }
                        let recv = match m.recv_mode {
                            Mode::Inout => {
                                self.require_mut(cx, &b, &format!("call `{}` on", name));
                                TArg { mode: ArgMode::Inout, e: b }
                            }
                            Mode::Sink => {
                                let b = self.value(cx, b);
                                TArg { mode: ArgMode::Value, e: b }
                            }
                            Mode::Borrow => {
                                let b = self.borrow(cx, b);
                                TArg { mode: ArgMode::Borrow, e: b }
                            }
                        };
                        let mut out = vec![recv];
                        for (a, (pn, pm, pt)) in args.iter().zip(m.params.iter()) {
                            let pp = FuncParam { name: pn.clone(), mode: *pm, ty: pt.subst(&s), span: sp };
                            out.push(self.check_arg(cx, a, &pp));
                        }
                        return TExpr::new(TK::Call { callee: Callee::ParamMethod { name: name.to_string(), recv_ty: bty.clone(), method_targs: vec![] }, args: out }, m.ret.subst(&s), sp);
                    }
                }
            }
            let n = self.prog.tparam(p).name.clone();
            self.err(nsp, format!("type parameter `{}` has no method `{}`; add an interface constraint that declares it", n, name));
            return TExpr::error(sp);
        }
        let mut cur = b;
        loop {
            let cty = self.resolve(cx, &cur.ty);
            if let Some(key) = TyKey::of(&cty) {
                if let Some(list) = self.prog.methods.get(&(key, name.to_string())).cloned() {
                    for fid in list {
                        let fd = self.prog.func(fid);
                        let rp = fd.recv.as_ref().unwrap().ty.clone();
                        let mut sm = HashMap::new();
                        if bind_params(&rp, &cty, &mut sm) {
                            if !fd.is_pub && fd.pkg != cx.env.pkg {
                                let (n, s) = (fd.name.clone(), self.prog.show(&cty));
                                self.err(nsp, format!("method `{}` of `{}` is private", n, s));
                            }
                            let fd = self.prog.func(fid);
                            // conditional methods: check where-bounds on receiver params
                            let tps = fd.tparams.clone();
                            let fname = fd.name.clone();
                            let mut unmet = false;
                            for tp in tps {
                                if let Some(t) = sm.get(&tp).cloned() {
                                    for bd in self.prog.tparam(tp).bounds.clone() {
                                        if let Err(m) = self.satisfies(&t, &bd) {
                                            let tn = self.prog.tparam(tp).name.clone();
                                            let s = self.prog.show(&cty);
                                            self.err(nsp, format!("method `{}` of `{}` requires `{}: {}`: {}", fname, s, tn, bound_name(&self.prog, &bd), m));
                                            unmet = true;
                                        }
                                    }
                                }
                            }
                            if unmet {
                                // the generic bound check in call_fn would report the same thing again
                                let n = self.diags.len();
                                let r = self.call_fn(cx, fid, targs, Some(cur), args, sp, expect);
                                let dup = format!("`{}` requires `", fname);
                                let mut i = n;
                                while i < self.diags.len() {
                                    if self.diags[i].msg.starts_with(&dup) {
                                        self.diags.remove(i);
                                    } else {
                                        i += 1;
                                    }
                                }
                                return r;
                            }
                            return self.call_fn(cx, fid, targs, Some(cur), args, sp, expect);
                        }
                    }
                }
            }
            // a struct field holding a function
            if let Ty::Adt(d, a) = &cty {
                let fields = self.prog.field_tys(*d, a);
                if let Some(i) = fields.iter().position(|f| f.0 == name) {
                    if let Ty::Func(fty) = &fields[i].1 {
                        let fty = fty.clone();
                        let fe = TExpr::new(TK::Field { base: Box::new(cur), index: i }, fields[i].1.clone(), sp);
                        if fty.params.len() != args.len() {
                            self.err(sp, format!("`{}` takes {} argument(s), {} given", name, fty.params.len(), args.len()));
                            return TExpr::error(sp);
                        }
                        let fe = self.borrow(cx, fe);
                        let mut out = vec![TArg { mode: ArgMode::Borrow, e: fe }];
                        for (a, (m, t)) in args.iter().zip(fty.params.iter()) {
                            let p = FuncParam { name: String::new(), mode: *m, ty: t.clone(), span: sp };
                            out.push(self.check_arg(cx, a, &p));
                        }
                        return TExpr::new(TK::Call { callee: Callee::Value, args: out }, fty.ret.clone(), sp);
                    }
                }
                // promoted methods from embedded fields
                let defc = self.prog.def(*d).clone();
                for (i, f) in defc.fields().iter().enumerate() {
                    if f.embedded {
                        let ft = fields[i].1.clone();
                        if let Some(k2) = TyKey::of(&ft) {
                            if self.prog.methods.contains_key(&(k2, name.to_string())) {
                                let inner = TExpr::new(TK::Field { base: Box::new(cur), index: i }, ft, sp);
                                return self.method_call_on(cx, inner, name, nsp, targs, args, sp, expect);
                            }
                        }
                    }
                }
            }
            let sp2 = cur.span;
            cur = match &cty {
                Ty::Box(t) | Ty::Rc(t) | Ty::Arc(t) => {
                    let t = (**t).clone();
                    TExpr::new(TK::Deref(Box::new(cur)), t, sp2)
                }
                Ty::String => TExpr::new(TK::Coerce(Coercion::StringToStr, Box::new(cur)), Ty::Str, sp2),
                Ty::Vec(t) => {
                    let t = Ty::Slice(t.clone());
                    TExpr::new(TK::Coerce(Coercion::VecToSlice, Box::new(cur)), t, sp2)
                }
                Ty::Array(t, _) | Ty::ArrayT(t, _) => {
                    let t = Ty::Slice(t.clone());
                    TExpr::new(TK::Coerce(Coercion::ArrayToSlice, Box::new(cur)), t, sp2)
                }
                _ => break,
            };
        }
        let s = self.prog.show(&bty);
        let mut d = Diag::new(nsp, format!("type `{}` has no method `{}`", s, name));
        if name == "len" || name == "size" || name == "length" {
            d = d.note(None, "use `.len` or `.len()`");
        }
        if name == "iter" {
            d = d.note(None, super::lazy::CONSUME_NOTE);
        }
        let is_chan = matches!(bty, Ty::Sender(_) | Ty::Receiver(_)) || matches!(&bty, Ty::Adt(dd, _) if Some(*dd) == self.prog.prelude_type("Chan"));
        if bty == Ty::Str && self.prog.methods.contains_key(&(TyKey::String, name.to_string())) {
            d = d.note(None, format!("`{}` changes a `String`; this is a `str` view (a string literal without an expected type is one): declare it `var s: String = \"\"`, or `.toString()` it", name));
        }
        if is_chan && name == "close" {
            d = d.note(None, "a channel closes when its last sender is dropped: `drop(ch.tx)` (and every clone of it); there is no `close`");
        }
        if let Some(k) = TyKey::of(&bty) {
            if self.prog.assoc.contains_key(&(k, name.to_string())) {
                d = d.note(None, format!("`{}` has no `self` parameter; call it on the type: `{}.{}(...)`", name, s, name));
            }
        }
        self.diags.push(d);
        for a in args {
            let _ = self.expr(cx, a, None);
        }
        TExpr::error(sp)
    }

    /// `isolate { body }`: from outside, the body may only use values that
    /// can cross tasks; nothing it creates can then be reached from outside
    /// but through its value, which may be sent as a whole: `Iso[T]`.
    fn isolate_expr(&mut self, cx: &mut FnCx, b: &ast::Block, sp: Span) -> TExpr {
        let start = cx.locals.len() as LocalId;
        let tb = self.block(cx, b, Expect::Infer);
        let t = self.block_ty(&tb);
        let t = self.resolve(cx, &t);
        let mut outer: Vec<LocalId> = vec![];
        visit_block(&tb, &mut |e| {
            if let TK::Local(l) = e.kind {
                if l < start && !outer.contains(&l) {
                    outer.push(l);
                }
            }
            if let TK::Closure(c) = &e.kind {
                for (l, _) in &c.captures {
                    if *l < start && !outer.contains(l) {
                        outer.push(*l);
                    }
                }
            }
        });
        for l in outer {
            let lt = cx.locals[l as usize].ty.clone();
            let lt = self.resolve(cx, &lt);
            if !self.is_send(&lt) {
                let (n, lsp) = (cx.locals[l as usize].name.clone(), cx.locals[l as usize].span);
                let s = self.prog.show(&lt);
                self.diags.push(Diag::new(sp, format!("`isolate` uses `{}` of type `{}` from outside", n, s)).note(Some(lsp), "declared here").note(None, "an isolated value may only be built from values that can cross tasks: then nothing outside points into it. Pass a copy or a `send` value in"));
            }
        }
        if self.prog.contains_view(&t) {
            let s = self.prog.show(&t);
            self.err(sp, format!("`isolate` can't give back the borrowed view `{}`", s));
        }
        TExpr::new(TK::Isolate(tb), Ty::Iso(Rc::new(t)), sp)
    }

    /// The element type of a channel end used to send.
    fn sender_elem(&mut self, t: &Ty, sp: Span, what: &str) -> Ty {
        match t {
            Ty::Sender(e) => (**e).clone(),
            Ty::Error => Ty::Error,
            Ty::Adt(d, _) if Some(*d) == self.prog.prelude_type("Chan") => {
                self.diags.push(Diag::new(sp, format!("send on the channel's sending end: `{}.tx <- v`", what)).note(None, "a `Chan[T]` has a `tx Sender[T]` and an `rx Receiver[T]`; pass each end where it's needed"));
                Ty::Error
            }
            Ty::Receiver(_) => {
                self.err(sp, "cannot send on a `Receiver`: send on the channel's `Sender` (its `tx`)");
                Ty::Error
            }
            _ => {
                let s = self.prog.show(t);
                self.err(sp, format!("cannot send on `{}`: it isn't a channel's `Sender`", s));
                Ty::Error
            }
        }
    }

    /// The element type of a channel end used to receive.
    fn receiver_elem(&mut self, t: &Ty, sp: Span, what: &str) -> Ty {
        match t {
            Ty::Receiver(e) => (**e).clone(),
            Ty::Error => Ty::Error,
            Ty::Adt(d, _) if Some(*d) == self.prog.prelude_type("Chan") => {
                self.diags.push(Diag::new(sp, format!("receive from the channel's receiving end: `<-{}.rx`", what)).note(None, "a `Chan[T]` has a `tx Sender[T]` and an `rx Receiver[T]`; pass each end where it's needed"));
                Ty::Error
            }
            Ty::Sender(_) => {
                self.err(sp, "cannot receive from a `Sender`: receive from the channel's `Receiver` (its `rx`)");
                Ty::Error
            }
            _ => {
                let s = self.prog.show(t);
                self.err(sp, format!("cannot receive from `{}`: it isn't a channel's `Receiver`", s));
                Ty::Error
            }
        }
    }

    /// `scope { body }` / `scope s { body }`.
    /// Waiting while a lock is held makes every task that wants the lock wait
    /// too, and is the usual way to deadlock: an error where it can be seen.
    fn no_wait_under_lock(&mut self, cx: &mut FnCx, sp: Span, what: &str) {
        if let Some((d, name)) = cx.locked.last().cloned() {
            if d == cx.depth() {
                self.diags.push(Diag::new(sp, format!("{} while `{}` is locked", what, name)).note(None, "every task that wants the lock would wait too, and a task that this one waits for may need the lock: a deadlock. Release the lock first: do this after the `with` block"));
            }
        }
    }

    pub fn scope_expr(&mut self, cx: &mut FnCx, name: Option<&(String, Span)>, body: &ast::Block, implicit: bool, ex: Expect, sp: Span) -> TExpr {
        if !implicit {
            self.no_wait_under_lock(cx, sp, "this `scope` waits for its tasks");
        }
        let id = cx.next_scope;
        cx.next_scope += 1;
        self.push_scope(cx);
        let handle = name.map(|(n, nsp)| self.declare(cx, n, Ty::Scope, LocalKind::Let, *nsp, false));
        let anchor = handle.unwrap_or_else(|| {
            let id = cx.locals.len() as LocalId;
            let name = format!("__scope{}", id);
            cx.locals.push(Local { name: name.clone(), ty: Ty::Scope, kind: LocalKind::Let, span: sp, closure_depth: cx.depth(), block_level: cx.scopes.len() as u32, match_arm: None, ref_root: None });
            cx.used.push(true);
            cx.scopes.last_mut().unwrap().push((name, id));
            id
        });
        // `block` opens the body's own level
        cx.task_scopes.push(TaskScopeCx { id, depth: cx.depth(), block: cx.scopes.len() as u32 + 1, errt: None, handle });
        let tb = self.block(cx, body, ex);
        let ts = cx.task_scopes.pop().unwrap();
        self.pop_scope(cx);
        let t = self.block_ty(&tb);
        let errt = ts.errt.clone().map(|(e, _)| self.resolve(cx, &e));
        let ty = match &errt {
            Some(e) => Ty::result(if t == Ty::Never { Ty::Unit } else { t }, e.clone()),
            None => t,
        };
        let fail_sp = ts.errt.as_ref().map(|e| e.1);
        let scope = TExpr::new(TK::Scope { id, handle, anchor, body: tb, errt: errt.clone() }, ty, sp);
        match (implicit, errt) {
            (true, Some(e1)) => {
                // `main` and tests: a failing task fails the function
                let fsp = fail_sp.unwrap_or(sp);
                let rr = self.resolve(cx, &cx.ret.clone());
                let w = match &rr {
                    Ty::Result(_, e2) => {
                        let e2 = (**e2).clone();
                        if self.try_unify(cx, &e1, &e2) {
                            Some(None)
                        } else if matches!(&e2, Ty::Iface(d, _) if Some(*d) == self.iface_error) {
                            Some(Some(TryWrap::ToError))
                        } else {
                            let (a, b) = (self.prog.show(&e1), self.prog.show(&e2));
                            self.diags.push(Diag::new(fsp, format!("this task fails with `{}`, but the function returns errors of type `{}`", a, b)).note(None, "convert the error inside the task: `try f() else Wrap`"));
                            None
                        }
                    }
                    Ty::Error => None,
                    _ => {
                        self.diags.push(Diag::new(fsp, "this task can fail, but the function doesn't return errors").note(None, "make it return `! E`, or handle the error inside the task (`catch`)"));
                        None
                    }
                };
                match w {
                    Some(w) => TExpr::new(TK::Try { e: Box::new(scope), wrap: w }, Ty::Unit, sp),
                    None => TExpr::error(sp),
                }
            }
            _ => scope,
        }
    }

    /// `go call` / `go () => { ... }` in the innermost `scope` of this function.
    pub fn go_expr(&mut self, cx: &mut FnCx, inner: &Expr, handle: bool, sp: Span) -> TExpr {
        let Some(ts) = cx.task_scopes.last().cloned() else {
            self.diags.push(Diag::new(sp, "`go` needs an enclosing `scope { }`").note(None, "start tasks inside `scope { go f() }`: the scope waits for them; `main` and test bodies are scopes already. A function that starts long-running tasks takes a `Scope` parameter and calls `s.go(() => ...)`"));
            self.expr(cx, inner, None);
            return TExpr::error(sp);
        };
        if ts.depth != cx.depth() {
            self.diags.push(Diag::new(sp, "this `go` is inside a closure, but its `scope` is outside it").note(None, "start a `scope { }` inside the closure, or pass the scope in and call `s.go(() => ...)`"));
            return TExpr::error(sp);
        }
        let Some((pre, closure)) = self.task_closure_ast(cx, inner) else {
            self.err(inner.span, "`go` needs a function call or a closure without parameters: `go f(x)`, `go () => { ... }`");
            return TExpr::error(sp);
        };
        // arguments evaluated now, in this task, into locals of their own
        // block: the task takes them
        self.push_scope(cx);
        let mut stmts = vec![];
        for st in &pre {
            if let Some(t) = self.stmt(cx, st) {
                stmts.push(t);
            }
        }
        let rv = self.fresh(cx);
        let fty = Ty::Func(Rc::new(FuncTy { params: vec![], ret: rv.clone(), send: false }));
        let mut ce = self.expr(cx, &closure, Some(&fty));
        // a task that only ever fails (`return err(e)`) succeeds with nothing
        if let Ty::Result(ok, _) = self.resolve(cx, &rv) {
            if matches!(*ok, Ty::Var(_)) {
                self.unify(cx, &ok, &Ty::Unit);
            }
        }
        let ret = self.resolve(cx, &rv);
        if handle && self.prog.contains_view(&ret) {
            let r = self.prog.show(&ret);
            self.diags.push(Diag::new(sp, format!("a task can't give back the borrowed view `{}`", r)).note(None, "its result is stored until `join`; return an owned value (`String(s)`, `.toVec()`)"));
            self.pop_scope(cx);
            return TExpr::error(sp);
        }
        self.task_captures(cx, &mut ce, &ts, false);
        self.pop_scope(cx);
        if !handle {
            if let Ty::Result(_, e) = &ret {
                let e = (**e).clone();
                let cur = cx.task_scopes.last_mut().unwrap();
                match cur.errt.clone() {
                    None => cur.errt = Some((e, sp)),
                    Some((prev, psp)) => {
                        if !self.unify(cx, &prev, &e) {
                            let (a, b) = (self.prog.show(&e), self.prog.show(&prev));
                            self.diags.push(Diag::new(sp, format!("this task fails with `{}`, but another task in the scope fails with `{}`", a, b)).note(Some(psp), "the other task").note(None, "the tasks of one scope must fail with the same error type; convert it inside the task with `try f() else Wrap`"));
                        }
                    }
                }
            }
        }
        let ty = if handle { Ty::Task(Rc::new(ret)) } else { Ty::Unit };
        let spawn = TExpr::new(TK::Spawn { target: SpawnTarget::Lexical(ts.id), task: Box::new(ce), handle }, ty.clone(), sp);
        if stmts.is_empty() {
            spawn
        } else {
            TExpr::new(TK::Block(TBlock { stmts, value: Some(Box::new(spawn)), span: sp }), ty, sp)
        }
    }

    /// The task of `go inner` as a closure without parameters, and the
    /// statements that evaluate its arguments first: `go f(a + 1, x, &y)`
    /// becomes `let __goN = a + 1` then `() => f(__goN, x, &y)`. Arguments that
    /// are places stay in the closure (the task borrows them), unless the
    /// parameter is `sink` (then they move into the task now).
    fn task_closure_ast(&mut self, cx: &mut FnCx, inner: &Expr) -> Option<(Vec<Stmt>, Expr)> {
        match &inner.kind {
            ExprKind::Closure { params, .. } if params.is_empty() => Some((vec![], inner.clone())),
            ExprKind::Closure { .. } => None,
            ExprKind::Call(f, args) => {
                let modes = self.callee_modes(cx, f, args.len());
                let mut pre = vec![];
                let mut nargs = vec![];
                for (i, a) in args.iter().enumerate() {
                    let mode = modes.as_ref().and_then(|m| m.get(i).copied());
                    let keep = match &a.kind {
                        ExprKind::Ref(_) | ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Char(_) | ExprKind::Bool(_) | ExprKind::None | ExprKind::DotVariant(_) => true,
                        _ => ast_is_place(a) && mode != Some(Mode::Sink),
                    };
                    if keep {
                        nargs.push(a.clone());
                        continue;
                    }
                    cx.hidden += 1;
                    let name = format!("__go{}", cx.hidden);
                    pre.push(Stmt { kind: StmtKind::Let { name: name.clone(), name_span: a.span, ty: None, init: Some(a.clone()), mutable: false }, span: a.span });
                    nargs.push(Expr { kind: ExprKind::Ident(name), span: a.span });
                }
                let call = Expr { kind: ExprKind::Call(f.clone(), nargs), span: inner.span };
                let clo = Expr { kind: ExprKind::Closure { params: vec![], ret: None, body: Box::new(call) }, span: inner.span };
                Some((pre, clo))
            }
            _ => None,
        }
    }

    /// The parameter modes of what `f` names, when that can be told without
    /// checking the call.
    fn callee_modes(&mut self, cx: &mut FnCx, f: &Expr, nargs: usize) -> Option<Vec<Mode>> {
        let fid = match self.resolve_path(cx, f) {
            Some(PathRes::Func(fid, _)) => Some(fid),
            _ => None,
        };
        if let Some(fid) = fid {
            let fd = self.prog.func(fid);
            return Some(fd.params.iter().map(|p| p.mode).collect());
        }
        // a method: look at the receiver's type without reporting anything twice
        if let ExprKind::Field(base, name, _) = &f.kind {
            let n = self.diags.len();
            let b = self.expr(cx, base, None);
            self.diags.truncate(n);
            let bt = self.resolve(cx, &b.ty);
            let bt = match &bt {
                Ty::Box(a) | Ty::Rc(a) | Ty::Arc(a) => (**a).clone(),
                t => t.clone(),
            };
            if let Some(k) = TyKey::of(&bt) {
                if let Some(&mid) = self.prog.methods.get(&(k, name.clone())).and_then(|v| v.first()) {
                    let fd = self.prog.func(mid);
                    if fd.params.len() == nargs {
                        return Some(fd.params.iter().map(|p| p.mode).collect());
                    }
                }
            }
        }
        None
    }

    /// Capture rules of a task closure started in scope `ts`. Data that lives
    /// until the scope ends (declared outside it, or directly in its body) is
    /// borrowed, for reading or, by one task only, for writing. Values declared
    /// in blocks inside the body are moved (or copied) into the task. `owned`:
    /// the task may outlive this function (`s.go` on a `Scope` value), so it
    /// must own every capture.
    fn task_captures(&mut self, cx: &mut FnCx, ce: &mut TExpr, ts: &TaskScopeCx, owned: bool) {
        let TK::Closure(c) = &mut ce.kind else { return };
        c.scoped = true;
        let lives = |cx: &FnCx, l: LocalId| -> bool {
            let loc = &cx.locals[l as usize];
            loc.closure_depth < ts.depth || (loc.closure_depth == ts.depth && loc.block_level <= ts.block)
        };
        for (l, u) in c.captures.clone() {
            let local = cx.locals[l as usize].clone();
            let t = self.resolve(cx, &local.ty);
            if !self.is_send(&t) {
                let s = self.prog.show(&t);
                self.diags.push(Diag::new(c.span, format!("a task captures `{}` of type `{}`, which is not safe to share between threads", local.name, s)).note(Some(local.span), "declared here").note(None, "`Rc`, `Cell`, `Weak`, raw pointers and function values can't cross tasks; use `Arc`, `Mutex` or `send fn`"));
            }
            let is_ref = matches!(local.kind, LocalKind::Ref { .. });
            let borrowed = if owned {
                false
            } else if is_ref {
                // a reference bound inside the scope: fine if what it borrows lives on
                let root = local.ref_root.map(|r| cx.locals[r as usize].ref_root.unwrap_or(r));
                lives(cx, l) || root.map(|r| lives(cx, r)).unwrap_or(false)
            } else {
                lives(cx, l)
            };
            if borrowed {
                if u == CapUse::Move {
                    self.diags.push(Diag::new(c.span, format!("a task cannot move `{}`: it borrows it from outside", local.name)).note(Some(local.span), "declared here").note(None, "move it into the task by passing it to a `sink` parameter: `go f(x)`, or declare it inside the scope's inner blocks"));
                }
                continue;
            }
            if is_ref || matches!(local.kind, LocalKind::Param(Mode::Borrow | Mode::Inout)) || self.prog.is_view(&t) {
                if owned {
                    self.diags.push(Diag::new(c.span, format!("a task started through a `Scope` value borrows `{}`", local.name)).note(Some(local.span), "declared here").note(None, "it may outlive this function: give it owned values (`.clone()` them first)"));
                } else {
                    self.diags.push(Diag::new(c.span, format!("a task borrows `{}`, which does not live until the scope ends", local.name)).note(Some(local.span), "declared here").note(None, "declare it directly in the scope's body or outside the scope, or `.clone()` it into a local so the task can take it"));
                }
                continue;
            }
            if u == CapUse::Mutate {
                self.diags.push(Diag::new(c.span, format!("a task cannot modify `{}`: it gets its own copy", local.name)).note(Some(local.span), "declared in a block inside the scope, so it is moved (or copied) into the task").note(None, "to share the result, declare it directly in the scope's body or outside the scope, so the task borrows it"));
            }
            c.by_value.push(l);
        }
        c.owned = false;
    }

    /// `s.go(() => ...)` on a `Scope` value.
    fn scope_value_go(&mut self, cx: &mut FnCx, s: TExpr, args: &[Expr], sp: Span) -> TExpr {
        if args.len() != 1 {
            self.err(sp, "`s.go` takes a closure: `s.go(() => { ... })`");
            return TExpr::error(sp);
        }
        let Some((pre, closure)) = self.task_closure_ast(cx, &args[0]) else {
            self.err(args[0].span, "`s.go` needs a function call or a closure without parameters");
            return TExpr::error(sp);
        };
        // the scope this function itself opened: the same rules as `go`
        let lexical = match &s.kind {
            TK::Local(l) => cx.task_scopes.iter().rev().find(|t| t.handle == Some(*l)).cloned(),
            _ => None,
        };
        if let Some(ts) = lexical.filter(|t| t.depth == cx.depth()) {
            // `go` uses the innermost scope; make sure that's this one
            if cx.task_scopes.last().map(|t| t.id) == Some(ts.id) {
                return self.go_expr(cx, &args[0], false, sp);
            }
            self.err(sp, "`s.go` names an outer scope; start the task with `go` in the innermost one");
            return TExpr::error(sp);
        }
        self.push_scope(cx);
        let mut stmts = vec![];
        for st in &pre {
            if let Some(t) = self.stmt(cx, st) {
                stmts.push(t);
            }
        }
        let fty = Ty::Func(Rc::new(FuncTy { params: vec![], ret: Ty::Unit, send: false }));
        let mut ce = self.expr(cx, &closure, Some(&fty));
        let ts = TaskScopeCx { id: u32::MAX, depth: cx.depth(), block: 0, errt: None, handle: None };
        self.task_captures(cx, &mut ce, &ts, true);
        self.pop_scope(cx);
        let s = self.value(cx, s);
        let spawn = TExpr::new(TK::Spawn { target: SpawnTarget::Value(Box::new(s)), task: Box::new(ce), handle: false }, Ty::Unit, sp);
        if stmts.is_empty() {
            spawn
        } else {
            TExpr::new(TK::Block(TBlock { stmts, value: Some(Box::new(spawn)), span: sp }), Ty::Unit, sp)
        }
    }

    pub fn find_std_type(&self, pkg: &str, name: &str) -> Option<DefId> {
        for p in &self.prog.packages {
            if p.name == pkg && p.is_std {
                if let Some(Sym::Type(d)) = p.items.get(name) {
                    return Some(*d);
                }
            }
        }
        None
    }

    pub fn check_pending_infer(&mut self, cx: &mut FnCx) {
        let pending = std::mem::take(&mut self.pending_infer);
        for (sp, fname, names, call) in pending {
            if let TK::Call { callee: Callee::Func(_, targs), .. } = &call.kind {
                if targs.iter().any(|t| self.resolve(cx, t).has_vars()) {
                    self.err(sp, format!("cannot infer type parameter{} {} of `{}`; specify {}: `{}[T](...)`", if names.len() > 1 { "s" } else { "" }, names.join(", "), fname, if names.len() > 1 { "them" } else { "it" }, fname));
                }
            }
        }
    }
}

fn d_id(t: &Ty) -> DefId {
    match t {
        Ty::Adt(d, _) => *d,
        _ => 0,
    }
}

pub fn bound_name(prog: &Program, b: &Bound) -> String {
    match b {
        Bound::Cap(c) => c.name().to_string(),
        Bound::Iface(d, _) => prog.def(*d).name.clone(),
    }
}

pub fn is_lock_proj(prog: &Program, e: &TExpr) -> bool {
    lock_kind(prog, e).is_some()
}

/// A lock projection: `m.lock()`, `rw.read()`, `rw.write()`. Some(true)
/// when it gives write access.
pub fn lock_kind(prog: &Program, e: &TExpr) -> Option<bool> {
    if let TK::Proj { call, .. } = &e.kind {
        if let TK::Call { callee: Callee::Func(f, _), .. } = &call.kind {
            let fd = prog.func(*f);
            if fd.kind != FuncKind::Intrinsic {
                return None;
            }
            if matches!(fd.recv_key, Some(TyKey::Mutex) | Some(TyKey::MutexRef)) && fd.name == "lock" {
                return Some(true);
            }
            if let Some(TyKey::Adt(d)) = fd.recv_key {
                if Some(d) == prog.prelude_type("RwLock") && (fd.name == "read" || fd.name == "write") {
                    return Some(fd.name == "write");
                }
            }
        }
    }
    None
}

pub fn block_diverges(b: &TBlock) -> bool {
    if let Some(v) = &b.value {
        return v.ty == Ty::Never;
    }
    match b.stmts.last() {
        Some(TStmt::Return(..)) | Some(TStmt::Break(..)) | Some(TStmt::Continue(..)) => true,
        Some(TStmt::Expr(e)) => e.ty == Ty::Never,
        Some(TStmt::While { cond: None, body, id }) => !breaks_out_of(body, *id),
        // a `with` body runs exactly once
        Some(TStmt::With { body, .. }) | Some(TStmt::WithLocks { body, .. }) => block_diverges(body),
        _ => false,
    }
}

/// An expression made only of number literals and operators on them.
fn literal_only(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Int(_) | ExprKind::Float(_) => true,
        ExprKind::Unary(_, x) => literal_only(x),
        ExprKind::Binary(_, a, b) => literal_only(a) && literal_only(b),
        _ => false,
    }
}

fn has_error_ty(t: &Ty) -> bool {
    let mut found = false;
    t.walk(&mut |x| found |= matches!(x, Ty::Error));
    found
}

/// Whether any `break` in the body, however deeply nested (in expressions,
/// inner loops with a label, `select` arms, loop-body closures), leaves loop `id`.
fn breaks_out_of(b: &TBlock, id: u32) -> bool {
    let mut found = false;
    crate::tir::walk_block(b, &mut |n| {
        if let crate::tir::Node::Stmt(TStmt::Break(i, _)) = n {
            found |= *i == id;
        }
    });
    found
}

fn is_const_expr(e: &TExpr) -> bool {
    match &e.kind {
        TK::Int(_) | TK::Float(_) | TK::Bool(_) | TK::Str(_) | TK::Const(_) => true,
        TK::Unary { e, .. } | TK::Cast(e) | TK::Copy(e) | TK::Coerce(Coercion::StrToString, e) => is_const_expr(e),
        TK::Binary { l, r, .. } => is_const_expr(l) && is_const_expr(r),
        _ => false,
    }
}

pub fn expr_to_type(e: &Expr) -> Option<ast::TypeExpr> {
    let kind = match &e.kind {
        ExprKind::Ident(n) => ast::TypeExprKind::Named { path: vec![n.clone()], args: vec![] },
        ExprKind::Field(b, n, _) => match expr_to_type(b)?.kind {
            ast::TypeExprKind::Named { mut path, args } if args.is_empty() => {
                path.push(n.clone());
                ast::TypeExprKind::Named { path, args }
            }
            _ => return None,
        },
        ExprKind::Index(b, xs) => match expr_to_type(b)?.kind {
            ast::TypeExprKind::Named { path, args } if args.is_empty() => {
                let mut a = vec![];
                for x in xs {
                    a.push(expr_to_type(x)?);
                }
                ast::TypeExprKind::Named { path, args: a }
            }
            _ => return None,
        },
        ExprKind::TypeArgs(b, ts) => match expr_to_type(b)?.kind {
            ast::TypeExprKind::Named { path, args } if args.is_empty() => ast::TypeExprKind::Named { path, args: ts.clone() },
            _ => return None,
        },
        ExprKind::TypeExpr(t) => return Some(t.clone()),
        ExprKind::Tuple(xs) => ast::TypeExprKind::Tuple(xs.iter().map(expr_to_type).collect::<Option<Vec<_>>>()?),
        ExprKind::Int(n) => ast::TypeExprKind::IntLit(*n),
        // `Vec[*Node]{}`: the brackets parsed as an index holding `*Node`
        ExprKind::Unary("*", x) => ast::TypeExprKind::Ptr(Box::new(expr_to_type(x)?)),
        _ => return None,
    };
    Some(ast::TypeExpr { kind, span: e.span })
}

// ---------- zonking helpers ----------

pub fn zonk_walk_block(b: &mut TBlock, f: &mut dyn FnMut(&mut Ty)) {
    for s in b.stmts.iter_mut() {
        zonk_walk_stmt(s, f);
    }
    if let Some(v) = &mut b.value {
        zonk_walk_expr(v, f);
    }
}

fn zonk_walk_stmt(s: &mut TStmt, f: &mut dyn FnMut(&mut Ty)) {
    match s {
        TStmt::Let { init, .. } => zonk_walk_expr(init, f),
        TStmt::LetRef { place, .. } => zonk_walk_expr(place, f),
        TStmt::Expr(e) => zonk_walk_expr(e, f),
        TStmt::Assign { place, value, .. } => {
            zonk_walk_expr(place, f);
            zonk_walk_expr(value, f)
        }
        TStmt::MapInsert { map, key, value } => {
            zonk_walk_expr(map, f);
            zonk_walk_expr(key, f);
            zonk_walk_expr(value, f)
        }
        TStmt::Return(Some(e), _) => zonk_walk_expr(e, f),
        TStmt::Defer(b) | TStmt::ErrDefer(b) => zonk_walk_block(b, f),
        TStmt::While { cond, body, .. } => {
            if let Some(c) = cond {
                zonk_walk_expr(c, f)
            }
            zonk_walk_block(body, f)
        }
        TStmt::For { iter, hi, body, .. } => {
            zonk_walk_expr(iter, f);
            if let Some(h) = hi {
                zonk_walk_expr(h, f)
            }
            zonk_walk_block(body, f)
        }
        TStmt::ForEach { call, .. } => zonk_walk_expr(call, f),
        TStmt::WithLocks { places, body, .. } => {
            for p in places {
                zonk_walk_expr(p, f)
            }
            zonk_walk_block(body, f)
        }
        TStmt::ParFor { iter, hi, body, .. } => {
            zonk_walk_expr(iter, f);
            if let Some(h) = hi {
                zonk_walk_expr(h, f)
            }
            zonk_walk_expr(body, f)
        }
        TStmt::Send { chan, value } => {
            zonk_walk_expr(chan, f);
            zonk_walk_expr(value, f)
        }
        TStmt::With { place, body, .. } => {
            zonk_walk_expr(place, f);
            zonk_walk_block(body, f)
        }
        TStmt::Yield(Some(e), _) => zonk_walk_expr(e, f),
        TStmt::Select { arms, .. } => {
            for a in arms {
                match &mut a.kind {
                    TSelectKind::Recv { chan, .. } => zonk_walk_expr(chan, f),
                    TSelectKind::Send { chan, value } => {
                        zonk_walk_expr(chan, f);
                        zonk_walk_expr(value, f)
                    }
                    TSelectKind::Task { task, .. } => zonk_walk_expr(task, f),
                    TSelectKind::Timer { at, .. } => zonk_walk_expr(at, f),
                    TSelectKind::Cancelled | TSelectKind::Default => {}
                }
                if let Some(g) = &mut a.guard {
                    zonk_walk_expr(g, f)
                }
                zonk_walk_block(&mut a.body, f)
            }
        }
        _ => {}
    }
}

pub fn zonk_walk_expr(e: &mut TExpr, f: &mut dyn FnMut(&mut Ty)) {
    f(&mut e.ty);
    match &mut e.kind {
        TK::FuncRef(_, ts) => ts.iter_mut().for_each(|t| f(t)),
        TK::Field { base, .. } | TK::Deref(base) | TK::OptField { base, .. } => zonk_walk_expr(base, f),
        TK::Index { base, index } | TK::StrIndex { base, index } => {
            zonk_walk_expr(base, f);
            zonk_walk_expr(index, f)
        }
        TK::MapIndex { base, key } => {
            zonk_walk_expr(base, f);
            zonk_walk_expr(key, f)
        }
        TK::SliceOf { base, lo, hi } => {
            zonk_walk_expr(base, f);
            if let Some(l) = lo {
                zonk_walk_expr(l, f)
            }
            if let Some(h) = hi {
                zonk_walk_expr(h, f)
            }
        }
        TK::Call { callee, args } => {
            match callee {
                Callee::Func(_, ts) => ts.iter_mut().for_each(|t| f(t)),
                Callee::ParamMethod { recv_ty, method_targs, .. } => {
                    f(recv_ty);
                    method_targs.iter_mut().for_each(|t| f(t))
                }
                _ => {}
            }
            args.iter_mut().for_each(|a| zonk_walk_expr(&mut a.e, f))
        }
        TK::Proj { call, .. } => zonk_walk_expr(call, f),
        TK::ProjUnwrap { proj, msg } => {
            zonk_walk_expr(proj, f);
            if let Some(m) = msg {
                zonk_walk_expr(m, f)
            }
        }
        TK::Intrinsic { targs, args, .. } => {
            targs.iter_mut().for_each(|t| f(t));
            args.iter_mut().for_each(|a| zonk_walk_expr(&mut a.e, f))
        }
        TK::Unary { e: x, .. } | TK::Cast(x) | TK::SomeLit(x) | TK::OkLit(x) | TK::ErrLit(x) | TK::Move(x) | TK::Copy(x) | TK::Coerce(_, x) | TK::Recv(x) | TK::Wrap(x) | TK::MakeChan(x) | TK::Unbox(x) | TK::MutexRef(x) => zonk_walk_expr(x, f),
        TK::Binary { l, r, .. } | TK::Coalesce { l, r } => {
            zonk_walk_expr(l, f);
            zonk_walk_expr(r, f)
        }
        TK::StructLit { targs, fields, .. } => {
            targs.iter_mut().for_each(|t| f(t));
            fields.iter_mut().for_each(|x| zonk_walk_expr(x, f))
        }
        TK::EnumLit { targs, args, .. } => {
            targs.iter_mut().for_each(|t| f(t));
            args.iter_mut().for_each(|x| zonk_walk_expr(x, f))
        }
        TK::VecLit(xs) | TK::ArrayLit(xs) => xs.iter_mut().for_each(|x| zonk_walk_expr(x, f)),
        TK::MapLit(ps) => ps.iter_mut().for_each(|(k, v)| {
            zonk_walk_expr(k, f);
            zonk_walk_expr(v, f)
        }),
        TK::Closure(c) => {
            f(&mut c.ret);
            zonk_walk_block(&mut c.body, f)
        }
        TK::If { cond, then, else_ } => {
            zonk_walk_expr(cond, f);
            zonk_walk_block(then, f);
            if let Some(b) = else_ {
                zonk_walk_block(b, f)
            }
        }
        TK::Match { scrut, arms, .. } => {
            zonk_walk_expr(scrut, f);
            for a in arms {
                zonk_walk_pat(&mut a.pat, f);
                if let Some(g) = &mut a.guard {
                    zonk_walk_expr(g, f)
                }
                zonk_walk_expr(&mut a.body, f)
            }
        }
        TK::Block(b) | TK::Unsafe(b) => zonk_walk_block(b, f),
        TK::Scope { body, errt, .. } => {
            if let Some(e) = errt {
                f(e)
            }
            zonk_walk_block(body, f)
        }
        TK::Isolate(body) => zonk_walk_block(body, f),
        TK::Spawn { target, task, .. } => {
            if let SpawnTarget::Value(v) = target {
                zonk_walk_expr(v, f)
            }
            zonk_walk_expr(task, f)
        }
        TK::Try { e: x, wrap } => {
            zonk_walk_expr(x, f);
            if let Some(TryWrap::Call(_, w)) = wrap {
                zonk_walk_expr(w, f)
            }
        }
        TK::Catch { e: x, handler, .. } => {
            zonk_walk_expr(x, f);
            zonk_walk_block(handler, f)
        }
        _ => {}
    }
}

fn zonk_walk_pat(p: &mut TPat, f: &mut dyn FnMut(&mut Ty)) {
    match p {
        TPat::Variant { subs, .. } | TPat::Tuple(subs) => subs.iter_mut().for_each(|s| zonk_walk_pat(s, f)),
        TPat::Some(x) | TPat::Ok(x) | TPat::Err(x) => zonk_walk_pat(x, f),
        TPat::TypeIs { ty, .. } => f(ty),
        TPat::Or(xs) => xs.iter_mut().for_each(|x| zonk_walk_pat(x, f)),
        _ => {}
    }
}

/// An expression whose value can only be an untyped number literal: `5`,
/// `-1.5`, `'a'`, `(if c { 5 } else { 7 })`, `match k { 0 => 1, _ => 2 }`,
/// `{ 3 }`, `(1 + 2)`. Its type comes from the context, so in `x op y` the
/// other side is checked first and lends it its type.
fn literal_like(e: &Expr) -> bool {
    fn block(b: &crate::ast::Block) -> bool {
        matches!(b.stmts.as_slice(), [crate::ast::Stmt { kind: crate::ast::StmtKind::Expr(v), .. }] if literal_like(v))
    }
    match &e.kind {
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Char(_) => true,
        ExprKind::Unary("-", x) => literal_like(x),
        ExprKind::Binary(op, l, r) => matches!(*op, "+" | "-" | "*" | "/" | "%" | "+%" | "-%" | "*%") && literal_like(l) && literal_like(r),
        ExprKind::If { then, else_: Some(els), .. } => block(then) && literal_like(els),
        ExprKind::Match { arms, .. } => !arms.is_empty() && arms.iter().all(|a| literal_like(&a.body)),
        ExprKind::Block(b) => block(b),
        _ => false,
    }
}

// ---------- list literals with `if` / `for` / `..` elements ----------

fn comp_call(list: &str, method: &str, arg: Expr) -> Stmt {
    let sp = arg.span;
    let recv = Expr { kind: ExprKind::Ident(list.to_string()), span: sp };
    let callee = Expr { kind: ExprKind::Field(Box::new(recv), method.to_string(), sp), span: sp };
    Stmt { kind: StmtKind::Expr(Expr { kind: ExprKind::Call(Box::new(callee), vec![arg]), span: sp }), span: sp }
}

/// An element: an `if` without `else` adds its value only when it holds.
fn comp_expr(list: &str, e: &Expr, missing: &mut Vec<Span>) -> Stmt {
    let sp = e.span;
    let kind = match &e.kind {
        ExprKind::If { cond, then, else_: None } => ExprKind::If { cond: cond.clone(), then: comp_block(list, then, missing), else_: None },
        ExprKind::IfLet { pat, expr, then, else_: None } => ExprKind::IfLet { pat: pat.clone(), expr: expr.clone(), then: comp_block(list, then, missing), else_: None },
        _ => return comp_call(list, "push", e.clone()),
    };
    Stmt { kind: StmtKind::Expr(Expr { kind, span: sp }), span: sp }
}

/// The body of an `if` / `for` element: its statements, then the element.
fn comp_block(list: &str, b: &ast::Block, missing: &mut Vec<Span>) -> ast::Block {
    let mut stmts = b.stmts.clone();
    match stmts.pop() {
        Some(Stmt { kind: StmtKind::Expr(e), .. }) => stmts.push(comp_expr(list, &e, missing)),
        Some(Stmt { kind: StmtKind::For(ast::ForLoop::In { first, second, iter, by_ref, body }, label), span }) => {
            let body = comp_block(list, &body, missing);
            stmts.push(Stmt { kind: StmtKind::For(ast::ForLoop::In { first, second, iter, by_ref, body }, label), span });
        }
        Some(other) => {
            missing.push(other.span);
            stmts.push(other);
        }
        None => missing.push(b.span),
    }
    ast::Block { stmts, span: b.span }
}
