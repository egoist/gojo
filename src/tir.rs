// Typed intermediate representation produced by the checker.
#![allow(dead_code)]

use crate::ast::Mode;
use crate::diag::Span;
use crate::types::*;

pub type LocalId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalKind {
    Let,
    Var,
    Param(Mode),
    /// Bound by reference to a place (projection, `if let`, `for`, `with`, match on a place).
    Ref { mutable: bool },
}

#[derive(Clone, Debug)]
pub struct Local {
    pub name: String,
    pub ty: Ty,
    pub kind: LocalKind,
    pub span: Span,
    pub closure_depth: u32,
    /// Block nesting (in the checker's scope stack) where it was declared.
    pub block_level: u32,
    /// Ref binding from a match on an owned place; may be upgraded to a move.
    pub match_arm: Option<u32>,
    /// For references: the local that owns the referenced data.
    pub ref_root: Option<LocalId>,
}

#[derive(Clone, Debug)]
pub struct TFunc {
    pub func: FuncId,
    pub locals: Vec<Local>,
    pub params: Vec<LocalId>,
    pub recv: Option<LocalId>,
    pub body: TBlock,
    pub n_arms: u32,
}

#[derive(Clone, Debug)]
pub struct TBlock {
    pub stmts: Vec<TStmt>,
    pub value: Option<Box<TExpr>>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IterKind {
    Vec,
    Slice,
    Array,
    Map,
    Str,
    Chan,
    Range { inclusive: bool },
}

#[derive(Clone, Debug)]
pub enum TStmt {
    Let { local: LocalId, init: TExpr },
    /// Bind a ref local to a place.
    LetRef { local: LocalId, place: TExpr },
    Expr(TExpr),
    Assign { place: TExpr, op: Option<BinOp>, value: TExpr, span: Span },
    /// `m[k] = v`
    MapInsert { map: TExpr, key: TExpr, value: TExpr },
    Return(Option<TExpr>, Span),
    /// Exit / restart the loop with this id.
    Break(u32, Span),
    Continue(u32, Span),
    Defer(TBlock),
    ErrDefer(TBlock),
    While { id: u32, cond: Option<TExpr>, body: TBlock },
    For { id: u32, kind: IterKind, iter: TExpr, hi: Option<TExpr>, elem: LocalId, index: Option<LocalId>, mutable: bool, body: TBlock, span: Span },
    Send { chan: TExpr, value: TExpr },
    With { local: LocalId, place: TExpr, lock: bool, body: TBlock },
    /// `with a, b = lock(m1, m2)`: `places` are lock projections, all taken
    /// before the body (in address order), released after.
    WithLocks { locals: Vec<LocalId>, places: Vec<TExpr>, body: TBlock },
    Yield(Option<TExpr>, Span),
    Select { arms: Vec<TSelectArm>, span: Span },
    /// `for` over a type with an `each` method: the call, whose last argument
    /// is the loop body closure.
    ForEach { id: u32, call: TExpr },
    /// `par for`: `body` is a `send fn` closure taking (index,) element; for a
    /// range, `iter` is the low end and `hi` the high one.
    ParFor { kind: IterKind, iter: TExpr, hi: Option<TExpr>, mutable: bool, indexed: bool, body: TExpr, span: Span },
}

#[derive(Clone, Debug)]
pub struct TSelectArm {
    pub kind: TSelectKind,
    pub guard: Option<TExpr>,
    pub body: TBlock,
}

#[derive(Clone, Debug)]
pub enum TSelectKind {
    Recv { chan: TExpr, bind: Option<LocalId> },
    Send { chan: TExpr, value: TExpr },
    /// `let r = <-t`: the task behind handle `t` (a local) has finished; the
    /// arm takes its result and the handle.
    Task { task: TExpr, bind: Option<LocalId> },
    /// `time.after(d)` (relative: true) or `time.at(t)`.
    Timer { at: TExpr, relative: bool },
    /// `cancelled`: the current task was cancelled.
    Cancelled,
    Default,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    WAdd,
    WSub,
    WMul,
    And,
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl BinOp {
    pub fn from_str(s: &str) -> Option<BinOp> {
        use BinOp::*;
        Some(match s {
            "+" => Add,
            "-" => Sub,
            "*" => Mul,
            "/" => Div,
            "%" => Rem,
            "+%" => WAdd,
            "-%" => WSub,
            "*%" => WMul,
            "&&" => And,
            "||" => Or,
            "&" => BitAnd,
            "|" => BitOr,
            "^" => BitXor,
            "<<" => Shl,
            ">>" => Shr,
            "==" => Eq,
            "!=" => Ne,
            "<" => Lt,
            "<=" => Le,
            ">" => Gt,
            ">=" => Ge,
            _ => return None,
        })
    }
    pub fn is_cmp(self) -> bool {
        matches!(self, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgMode {
    /// Pass by reference (or by value for copy types).
    Borrow,
    /// Pass a mutable reference.
    Inout,
    /// Pass an owned value (the expression is already a Move/Copy/rvalue).
    Value,
}

#[derive(Clone, Debug)]
pub struct TArg {
    pub mode: ArgMode,
    pub e: TExpr,
}

#[derive(Clone, Debug)]
pub enum Callee {
    Func(FuncId, Vec<Ty>),
    /// Method on a type parameter, resolved after monomorphization.
    ParamMethod { name: String, recv_ty: Ty, method_targs: Vec<Ty> },
    /// Interface method via vtable; receiver is args[0].
    Iface { def: DefId, method: usize },
    /// Calling a func-typed value; callee is args[0].
    Value,
}

#[derive(Clone, Debug)]
pub enum Coercion {
    StrToString,
    StringToStr,
    VecToSlice,
    ArrayToSlice,
    /// Wrap a concrete value as an interface; owned = boxed on the heap.
    ToIface { owned: bool },
    /// Convert func item to func value.
    FuncItem,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapUse {
    Read,
    Mutate,
    Move,
}

#[derive(Clone, Debug)]
pub struct TClosure {
    pub id: u32,
    pub params: Vec<LocalId>,
    pub ret: Ty,
    pub body: TBlock,
    pub captures: Vec<(LocalId, CapUse)>,
    /// Owns its captures (escaping closure).
    pub owned: bool,
    /// Scoped closure for `task.group`'s `g.go`: borrows, must be `send`.
    pub scoped: bool,
    /// Captures stored by value even though the closure borrows (scoped tasks).
    pub by_value: Vec<LocalId>,
    /// The body of a `for` loop over a type with `each`: break / continue /
    /// return / try inside it act on the enclosing function (loop id).
    pub loop_body: Option<u32>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum TPat {
    Wild,
    Bind(LocalId),
    Int(i128),
    Range(i128, i128, bool),
    Bool(bool),
    Str(String),
    Variant { def: DefId, variant: usize, subs: Vec<TPat> },
    /// `(a, b)`: the fields of a tuple, in order.
    Tuple(Vec<TPat>),
    Some(Box<TPat>),
    None,
    Ok(Box<TPat>),
    Err(Box<TPat>),
    /// Interface type match.
    TypeIs { ty: Ty, bind: Option<LocalId> },
    Or(Vec<TPat>),
}

#[derive(Clone, Debug)]
pub struct TArm {
    pub id: u32,
    pub pat: TPat,
    pub guard: Option<TExpr>,
    pub body: TExpr,
    /// Some binding moved out of the (owned) scrutinee.
    pub consuming: bool,
    pub binds: Vec<LocalId>,
}

#[derive(Clone, Debug)]
pub enum TryWrap {
    /// Call a function / variant constructor with the error stored in the local.
    Call(LocalId, Box<TExpr>),
    /// Convert to the `error` interface (boxing).
    ToError,
}

#[derive(Clone, Debug)]
pub enum TK {
    Unit,
    Int(i128),
    Float(f64),
    Bool(bool),
    Str(String),
    NoneLit,
    Local(LocalId),
    Const(u32),
    /// A package-level `let` (a read-only place).
    Global(u32),
    /// The value of an integer type parameter (`R` in `struct Mat[R int, C int]`).
    ConstParam(TParamId),
    FuncRef(FuncId, Vec<Ty>),
    Field { base: Box<TExpr>, index: usize },
    /// Deref through Box / rc / arc / raw pointer (a place).
    Deref(Box<TExpr>),
    Index { base: Box<TExpr>, index: Box<TExpr> },
    /// `s[i]` on strings: a byte value.
    StrIndex { base: Box<TExpr>, index: Box<TExpr> },
    /// `m[k]` read: place, panics when absent.
    MapIndex { base: Box<TExpr>, key: Box<TExpr> },
    SliceOf { base: Box<TExpr>, lo: Option<Box<TExpr>>, hi: Option<Box<TExpr>> },
    Call { callee: Callee, args: Vec<TArg> },
    /// Call to a projection function: a place (deref of returned pointer).
    Proj { call: Box<TExpr>, mutable: bool, opt: bool },
    /// `proj.unwrap()` / `proj.expect(msg)` on an optional projection: a checked place.
    ProjUnwrap { proj: Box<TExpr>, msg: Option<Box<TExpr>> },
    Intrinsic { name: String, targs: Vec<Ty>, args: Vec<TArg> },
    Unary { op: &'static str, e: Box<TExpr> },
    Binary { op: BinOp, l: Box<TExpr>, r: Box<TExpr> },
    Cast(Box<TExpr>),
    StructLit { def: DefId, targs: Vec<Ty>, fields: Vec<TExpr> },
    EnumLit { def: DefId, targs: Vec<Ty>, variant: usize, args: Vec<TExpr> },
    SomeLit(Box<TExpr>),
    OkLit(Box<TExpr>),
    ErrLit(Box<TExpr>),
    VecLit(Vec<TExpr>),
    ArrayLit(Vec<TExpr>),
    MapLit(Vec<(TExpr, TExpr)>),
    Closure(Box<TClosure>),
    If { cond: Box<TExpr>, then: TBlock, else_: Option<TBlock> },
    Match { scrut: Box<TExpr>, owned: bool, arms: Vec<TArm> },
    Block(TBlock),
    Try { e: Box<TExpr>, wrap: Option<TryWrap> },
    Catch { e: Box<TExpr>, bind: Option<LocalId>, handler: TBlock },
    Coalesce { l: Box<TExpr>, r: Box<TExpr> },
    OptField { base: Box<TExpr>, index: usize },
    Move(Box<TExpr>),
    Copy(Box<TExpr>),
    Coerce(Coercion, Box<TExpr>),
    Recv(Box<TExpr>),
    Unsafe(TBlock),
    /// Construct Box/rc/arc/Mutex/Cell around a value.
    Wrap(Box<TExpr>),
    /// `Chan[T](n)`
    MakeChan(Box<TExpr>),
    /// Move the contents out of an owned Box, freeing the allocation.
    Unbox(Box<TExpr>),
    /// `Mutex(&place)`
    MutexRef(Box<TExpr>),
    /// Default value of a type (`var x T`, struct defaults, `T.default()`).
    DefaultOf,
    /// `scope { body }`: tasks started in `body` run until the scope ends.
    /// `errt`: the error a plain `go` task can fail with; the scope's type is
    /// then `T ! E`.
    /// `anchor`: what the scope's task handles and `Scope` values are views of
    /// (the handle when the scope is named, else a hidden local), declared in
    /// the body: they can't leave it.
    Scope { id: u32, handle: Option<LocalId>, anchor: LocalId, body: TBlock, errt: Option<Ty> },
    /// `go ...`: `task` is a closure literal with no parameters. `handle`:
    /// gives a `Task[R]` (`let t = go f()`), else unit.
    Spawn { target: SpawnTarget, task: Box<TExpr>, handle: bool },
    /// `isolate { body }`: the body's value, as an `Iso[T]`.
    Isolate(TBlock),
    Error,
}

/// The scope a `go` starts its task in.
#[derive(Clone, Debug)]
pub enum SpawnTarget {
    /// The `scope` block with this id, in the same function.
    Lexical(u32),
    /// A `Scope` value (`s.go(...)`, a parameter or a captured scope).
    Value(Box<TExpr>),
}

#[derive(Clone, Debug)]
pub struct TExpr {
    pub kind: TK,
    pub ty: Ty,
    pub span: Span,
}

impl TExpr {
    pub fn new(kind: TK, ty: Ty, span: Span) -> TExpr {
        TExpr { kind, ty, span }
    }
    pub fn error(span: Span) -> TExpr {
        TExpr { kind: TK::Error, ty: Ty::Error, span }
    }
    pub fn unit(span: Span) -> TExpr {
        TExpr { kind: TK::Unit, ty: Ty::Unit, span }
    }
    pub fn is_place(&self) -> bool {
        match &self.kind {
            TK::Local(_) | TK::Global(_) | TK::Deref(_) | TK::Index { .. } | TK::MapIndex { .. } | TK::Proj { opt: false, .. } | TK::ProjUnwrap { .. } => true,
            TK::Field { base, .. } => base.is_place(),
            _ => false,
        }
    }
}

// ---------- generic traversal ----------

/// Calls `f` on every expression in the block (pre-order), including closure bodies.
/// Calls `f` on every expression in the block, nested blocks and closures included.
pub fn visit_block(b: &TBlock, f: &mut dyn FnMut(&TExpr)) {
    walk_block(b, &mut |n| if let Node::Expr(e) = n { f(e) })
}

pub fn visit_stmt(s: &TStmt, f: &mut dyn FnMut(&TExpr)) {
    walk_stmt(s, &mut |n| if let Node::Expr(e) = n { f(e) })
}

pub fn visit_expr(e: &TExpr, f: &mut dyn FnMut(&TExpr)) {
    walk_expr(e, &mut |n| if let Node::Expr(e) = n { f(e) })
}

/// A statement or an expression, for `walk_*`.
#[derive(Clone, Copy)]
pub enum Node<'a> {
    Stmt(&'a TStmt),
    Expr(&'a TExpr),
}

/// Like `visit_*`, but also calls `f` on every statement, nested ones included.
pub fn walk_block(b: &TBlock, f: &mut dyn FnMut(Node)) {
    for s in &b.stmts {
        walk_stmt(s, f);
    }
    if let Some(v) = &b.value {
        walk_expr(v, f);
    }
}

pub fn walk_stmt(s: &TStmt, f: &mut dyn FnMut(Node)) {
    f(Node::Stmt(s));
    match s {
        TStmt::Let { init, .. } => walk_expr(init, f),
        TStmt::LetRef { place, .. } => walk_expr(place, f),
        TStmt::Expr(e) => walk_expr(e, f),
        TStmt::Assign { place, value, .. } => {
            walk_expr(place, f);
            walk_expr(value, f)
        }
        TStmt::MapInsert { map, key, value } => {
            walk_expr(map, f);
            walk_expr(key, f);
            walk_expr(value, f)
        }
        TStmt::Return(Some(e), _) | TStmt::Yield(Some(e), _) => walk_expr(e, f),
        TStmt::Defer(b) | TStmt::ErrDefer(b) => walk_block(b, f),
        TStmt::While { cond, body, .. } => {
            if let Some(c) = cond {
                walk_expr(c, f)
            }
            walk_block(body, f)
        }
        TStmt::For { iter, hi, body, .. } => {
            walk_expr(iter, f);
            if let Some(h) = hi {
                walk_expr(h, f)
            }
            walk_block(body, f)
        }
        TStmt::ForEach { call, .. } => walk_expr(call, f),
        TStmt::WithLocks { places, body, .. } => {
            for p in places {
                walk_expr(p, f)
            }
            walk_block(body, f)
        }
        TStmt::ParFor { iter, hi, body, .. } => {
            walk_expr(iter, f);
            if let Some(h) = hi {
                walk_expr(h, f)
            }
            walk_expr(body, f)
        }
        TStmt::Send { chan, value } => {
            walk_expr(chan, f);
            walk_expr(value, f)
        }
        TStmt::With { place, body, .. } => {
            walk_expr(place, f);
            walk_block(body, f)
        }
        TStmt::Select { arms, .. } => {
            for a in arms {
                match &a.kind {
                    TSelectKind::Recv { chan, .. } => walk_expr(chan, f),
                    TSelectKind::Send { chan, value } => {
                        walk_expr(chan, f);
                        walk_expr(value, f)
                    }
                    TSelectKind::Task { task, .. } => walk_expr(task, f),
                    TSelectKind::Timer { at, .. } => walk_expr(at, f),
                    TSelectKind::Cancelled | TSelectKind::Default => {}
                }
                if let Some(g) = &a.guard {
                    walk_expr(g, f)
                }
                walk_block(&a.body, f)
            }
        }
        _ => {}
    }
}

pub fn walk_expr(e: &TExpr, f: &mut dyn FnMut(Node)) {
    f(Node::Expr(e));
    let mut g = |x: &TExpr| walk_expr(x, f);
    match &e.kind {
        TK::Field { base, .. } | TK::Deref(base) | TK::OptField { base, .. } => g(base),
        TK::Index { base, index } | TK::StrIndex { base, index } => {
            g(base);
            g(index)
        }
        TK::MapIndex { base, key } => {
            g(base);
            g(key)
        }
        TK::SliceOf { base, lo, hi } => {
            g(base);
            if let Some(l) = lo {
                g(l)
            }
            if let Some(h) = hi {
                g(h)
            }
        }
        TK::Call { args, .. } | TK::Intrinsic { args, .. } => {
            for a in args {
                g(&a.e)
            }
        }
        TK::Proj { call, .. } => g(call),
        TK::ProjUnwrap { proj, msg } => {
            g(proj);
            if let Some(m) = msg {
                g(m)
            }
        }
        TK::Unary { e: x, .. } | TK::Cast(x) | TK::SomeLit(x) | TK::OkLit(x) | TK::ErrLit(x) | TK::Move(x) | TK::Copy(x) | TK::Coerce(_, x) | TK::Recv(x) | TK::Wrap(x) | TK::MakeChan(x) | TK::Unbox(x) | TK::MutexRef(x) => g(x),
        TK::Binary { l, r, .. } | TK::Coalesce { l, r } => {
            g(l);
            g(r)
        }
        TK::StructLit { fields, .. } => fields.iter().for_each(|x| g(x)),
        TK::EnumLit { args, .. } => args.iter().for_each(|x| g(x)),
        TK::VecLit(xs) | TK::ArrayLit(xs) => xs.iter().for_each(|x| g(x)),
        TK::MapLit(ps) => ps.iter().for_each(|(k, v)| {
            g(k);
            g(v)
        }),
        TK::Closure(c) => walk_block(&c.body, f),
        TK::If { cond, then, else_ } => {
            g(cond);
            walk_block(then, f);
            if let Some(b) = else_ {
                walk_block(b, f)
            }
        }
        TK::Match { scrut, arms, .. } => {
            g(scrut);
            for a in arms {
                if let Some(gd) = &a.guard {
                    walk_expr(gd, f)
                }
                walk_expr(&a.body, f)
            }
        }
        TK::Block(b) | TK::Unsafe(b) => walk_block(b, f),
        TK::Scope { body, .. } | TK::Isolate(body) => walk_block(body, f),
        TK::Spawn { target, task, .. } => {
            if let SpawnTarget::Value(v) = target {
                g(v)
            }
            g(task)
        }
        TK::Try { e: x, wrap } => {
            walk_expr(x, f);
            if let Some(TryWrap::Call(_, w)) = wrap {
                walk_expr(w, f)
            }
        }
        TK::Catch { e: x, handler, .. } => {
            walk_expr(x, f);
            walk_block(handler, f)
        }
        _ => {}
    }
}

/// Locals mentioned anywhere in the statement.
pub fn locals_in_stmt(s: &TStmt, out: &mut Vec<LocalId>) {
    visit_stmt(s, &mut |e| {
        if let TK::Local(l) = e.kind {
            out.push(l)
        }
        if let TK::Closure(c) = &e.kind {
            out.extend(c.captures.iter().map(|c| c.0))
        }
    });
}
