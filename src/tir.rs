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
    Break(Span),
    Continue(Span),
    Defer(TBlock),
    ErrDefer(TBlock),
    While { cond: Option<TExpr>, body: TBlock },
    For { kind: IterKind, iter: TExpr, hi: Option<TExpr>, elem: LocalId, index: Option<LocalId>, mutable: bool, body: TBlock, span: Span },
    Go { call: TExpr, span: Span },
    Send { chan: TExpr, value: TExpr },
    With { local: LocalId, place: TExpr, lock: bool, body: TBlock },
    Yield(Option<TExpr>, Span),
    Select { arms: Vec<TSelectArm>, span: Span },
}

#[derive(Clone, Debug)]
pub struct TSelectArm {
    pub kind: TSelectKind,
    pub body: TBlock,
}

#[derive(Clone, Debug)]
pub enum TSelectKind {
    Recv { chan: TExpr, bind: Option<LocalId> },
    Send { chan: TExpr, value: TExpr },
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
    /// `chan[T](n)`
    MakeChan(Box<TExpr>),
    /// Move the contents out of an owned Box, freeing the allocation.
    Unbox(Box<TExpr>),
    /// Default value of a type (`var x T`, struct defaults, `T.default()`).
    DefaultOf,
    Error,
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
            TK::Local(_) | TK::Deref(_) | TK::Index { .. } | TK::MapIndex { .. } | TK::Proj { opt: false, .. } | TK::ProjUnwrap { .. } => true,
            TK::Field { base, .. } => base.is_place(),
            _ => false,
        }
    }
}
