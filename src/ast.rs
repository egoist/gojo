// Untyped syntax tree produced by the parser.
#![allow(dead_code)]

use crate::diag::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    Borrow,
    Inout,
    Sink,
}

#[derive(Clone, Debug)]
pub struct File {
    pub package: String,
    pub imports: Vec<Import>,
    pub items: Vec<Item>,
    pub file_id: u32,
}

#[derive(Clone, Debug)]
pub struct Import {
    /// As written (`fmt`, `./geom`); the loader rewrites it to a package key.
    pub path: String,
    /// Name the package is bound to (defaults to the last path segment).
    pub alias: Option<String>,
    /// `use ./geom.{Vec2, Shape}`: names brought into scope instead of the package.
    pub names: Option<Vec<(String, Span)>>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub kind: ItemKind,
    pub is_pub: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ItemKind {
    Func(FuncDecl),
    Struct(StructDecl),
    Enum(EnumDecl),
    Interface(IfaceDecl),
    TypeAlias { name: String, tparams: Vec<TParam>, ty: TypeExpr, newtype: bool },
    Const { name: String, ty: Option<TypeExpr>, value: Expr },
    /// `let name T = value` at package level: computed once, on first use.
    Global { name: String, name_span: Span, ty: Option<TypeExpr>, value: Expr },
    Extern(ExternBlock),
    Test { name: String, body: Block },
}

#[derive(Clone, Debug)]
pub struct ExternBlock {
    pub abi: String,
    pub lib: Option<String>,
    pub funcs: Vec<FuncDecl>,
}

#[derive(Clone, Debug)]
pub struct TParam {
    pub name: String,
    pub bounds: Vec<TypeExpr>,
    pub is_int: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub mode: Mode,
    pub ty: TypeExpr,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum YieldKind {
    Ref,
    Mut,
}

#[derive(Clone, Debug)]
pub struct FuncDecl {
    pub name: String,
    pub name_span: Span,
    pub recv: Option<Param>,
    pub tparams: Vec<TParam>,
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    pub yields: Option<YieldKind>,
    /// `yield ?&T`: the projection may produce nothing.
    pub yield_opt: bool,
    pub wheres: Vec<(String, Vec<TypeExpr>, Span)>,
    pub body: Option<Block>,
    pub is_extern_c: bool,
    /// `unsafe fn`: callers must uphold preconditions the compiler can't check,
    /// so every call needs an `unsafe` block.
    pub is_unsafe: bool,
    /// Associated function (declared in a type body without `self`): the owning type.
    pub owner: Option<TypeExpr>,
    /// `extend T: Integer { ... }`: the block's type parameter. The
    /// methods are added to every built-in type in the bound's type set.
    pub ext_tparams: Vec<TParam>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct FieldDecl {
    pub name: String,
    pub ty: TypeExpr,
    pub default: Option<Expr>,
    pub is_pub: bool,
    pub embedded: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct StructDecl {
    pub name: String,
    pub tparams: Vec<TParam>,
    pub fields: Vec<FieldDecl>,
    pub is_copy: bool,
    pub unsafe_send: bool,
}

#[derive(Clone, Debug)]
pub struct VariantDecl {
    pub name: String,
    /// (name, type); unnamed payloads get names `_0`, `_1`...
    pub fields: Vec<(String, TypeExpr)>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct EnumDecl {
    pub name: String,
    pub tparams: Vec<TParam>,
    pub variants: Vec<VariantDecl>,
    pub is_copy: bool,
}

#[derive(Clone, Debug)]
pub struct IfaceMethod {
    pub name: String,
    pub recv_mode: Mode,
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct IfaceDecl {
    pub name: String,
    pub tparams: Vec<TParam>,
    pub methods: Vec<IfaceMethod>,
    pub embeds: Vec<TypeExpr>,
    /// Type-set elements: (approx `~`, type)
    pub type_set: Vec<(bool, TypeExpr)>,
    /// `send interface`: only `send` types implement it, so its values are `send`.
    pub is_send: bool,
}

#[derive(Clone, Debug)]
pub enum TypeExprKind {
    /// `a.b.C[args]`
    Named { path: Vec<String>, args: Vec<TypeExpr> },
    Option(Box<TypeExpr>),
    Result(Box<TypeExpr>, Box<TypeExpr>),
    Slice(Box<TypeExpr>),
    Array(Box<Expr>, Box<TypeExpr>),
    /// `fn(A) R`; `send fn(A) R` may be shared between tasks.
    Func { params: Vec<(Mode, TypeExpr)>, ret: Option<Box<TypeExpr>>, send: bool },
    Ptr(Box<TypeExpr>),
    Chan(Box<TypeExpr>),
    Unit,
    /// `(A, B)`
    Tuple(Vec<TypeExpr>),
    IntLit(u128),
}

#[derive(Clone, Debug)]
pub struct TypeExpr {
    pub kind: TypeExprKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum StmtKind {
    Let { name: String, name_span: Span, ty: Option<TypeExpr>, init: Option<Expr>, mutable: bool },
    /// `let .Circle(r) = s else { return }`, before the parser folds the rest of
    /// the block into an `ExprKind::LetElse`.
    LetElse { pat: Pattern, expr: Expr, else_: Block },
    Expr(Expr),
    Assign { place: Expr, op: &'static str, value: Expr },
    Return(Option<Expr>),
    /// `break` / `break label`
    Break(Option<(String, Span)>),
    Continue(Option<(String, Span)>),
    Defer(Box<Stmt>),
    /// Runs only when the function returns an error.
    ErrDefer(Box<Stmt>),
    /// `label: for ...`
    For(ForLoop, Option<(String, Span)>),
    /// `par for x in &v { }`: the iterations run in parallel.
    ParFor { first: (String, Span), second: Option<(String, Span)>, iter: Expr, by_ref: bool, body: Block },
    Send { chan: Expr, value: Expr },
    With { name: String, name_span: Span, expr: Expr, body: Block },
    /// `with a, b = lock(m1, m2) { }`: several locks, taken in one order.
    WithLocks { names: Vec<(String, Span)>, mutexes: Vec<Expr>, body: Block },
    Discard(Expr),
    Yield(Option<Expr>),
    Select(Vec<SelectArm>),
}

#[derive(Clone, Debug)]
pub struct SelectArm {
    /// `let x = <-ch`, `<-ch`, `ch <- v`, `time.after(d)`, `cancelled`, `_`
    pub kind: SelectKind,
    /// `... if cond =>`: the arm takes part only when `cond` holds.
    pub guard: Option<Expr>,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum SelectKind {
    Recv { bind: Option<(String, Span)>, chan: Expr },
    Send { chan: Expr, value: Expr },
    /// Anything else: `time.after(d)`, `time.at(t)`, `cancelled`.
    Event(Expr),
    Default,
}

#[derive(Clone, Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ForLoop {
    Infinite(Block),
    Cond(Expr, Block),
    In { first: (String, Span), second: Option<(String, Span)>, iter: Expr, by_ref: bool, body: Block },
}

/// An element of a list literal.
#[derive(Clone, Debug)]
pub enum ListElem {
    /// One value, or, for an `if` without `else`, none or one.
    Expr(Expr),
    /// `..xs`: every element of the `Vec` `xs` (moved).
    Spread(Expr),
    /// `for x in xs { value }`: one value per iteration (`ForLoop::In`).
    For(ForLoop),
}

#[derive(Clone, Debug)]
pub struct Arm {
    pub pat: Pattern,
    pub guard: Option<Expr>,
    pub body: Expr,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum PatternKind {
    Wild,
    /// A bare identifier: either a binding or a unit variant / constant (resolved by checker).
    Ident(String),
    /// `.Name` or `.Name(pats)`
    DotVariant(String, Option<Vec<Pattern>>),
    /// `A.B` / `A.B(pats)` / `Name(pats)` / `ok(p)` etc.
    Path(Vec<String>, Option<Vec<Pattern>>),
    Int(i128),
    Str(String),
    Char(u32),
    Bool(bool),
    None,
    Range(i128, i128, bool),
    /// `name Type` (interface type match)
    Typed(String, TypeExpr),
    /// `(a, b)`
    Tuple(Vec<Pattern>),
    Or(Vec<Pattern>),
}

#[derive(Clone, Debug)]
pub struct Pattern {
    pub kind: PatternKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ClosureParam {
    pub name: String,
    pub mode: Mode,
    pub ty: Option<TypeExpr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Int(u128),
    Float(f64),
    Str(String),
    /// `"x = ${x}"`: literal text and embedded expressions (with an optional
    /// format, `${x:>8}`).
    Interp(Vec<InterpPart>),
    Char(u32),
    Bool(bool),
    None,
    Ident(String),
    Field(Box<Expr>, String, Span),
    OptField(Box<Expr>, String),
    /// `a[b, c]` — index or explicit type arguments (resolved by checker).
    Index(Box<Expr>, Vec<Expr>),
    /// `a[T, U]` where the arguments only parse as types.
    TypeArgs(Box<Expr>, Vec<TypeExpr>),
    Slice(Box<Expr>, Option<Box<Expr>>, Option<Box<Expr>>),
    Call(Box<Expr>, Vec<Expr>),
    Unary(&'static str, Box<Expr>),
    Binary(&'static str, Box<Expr>, Box<Expr>),
    /// `&x` (inout argument / mutable iteration)
    Ref(Box<Expr>),
    Closure { params: Vec<ClosureParam>, ret: Option<TypeExpr>, body: Box<Expr> },
    If { cond: Box<Expr>, then: Block, else_: Option<Box<Expr>> },
    IfLet { pat: Pattern, expr: Box<Expr>, then: Block, else_: Option<Box<Expr>> },
    Match { scrut: Box<Expr>, arms: Vec<Arm> },
    Block(Block),
    Unsafe(Block),
    Try { expr: Box<Expr>, wrap: Option<Box<Expr>> },
    Catch { expr: Box<Expr>, bind: Option<(String, Span)>, body: Block },
    Coalesce(Box<Expr>, Box<Expr>),
    Range { lo: Box<Expr>, hi: Box<Expr>, inclusive: bool },
    Recv(Box<Expr>),
    DotVariant(String),
    StructLit { ty: Box<Expr>, fields: Vec<(String, Span, Expr)>, positional: Vec<Expr>, kv: Vec<(Expr, Expr)> },
    VecLit(Vec<Expr>),
    /// A list literal with `if` / `for` / `..` elements:
    /// `[a, if c { b }, for x in xs { f(x) }, ..rest]`
    ListComp(Vec<ListElem>),
    /// `[k: v, ...]`, `[:]`: a `Map`
    MapLit(Vec<(Expr, Expr)>),
    /// `scope { }` / `scope s { }`: waits for the tasks started in it.
    /// `implicit`: the body of `main` or a test.
    Scope { name: Option<(String, Span)>, body: Block, implicit: bool },
    /// `go f(x)` / `go () => { }`: starts a task in the enclosing scope.
    Go(Box<Expr>),
    /// `isolate { }`: a value built only from what can cross tasks; `Iso[T]`.
    Isolate(Block),
    /// `(a, b)`
    Tuple(Vec<Expr>),
    /// `with v = m.lock() { ... }` (a `StmtKind::With` / `WithLocks`): a
    /// statement, or an expression with its body's value.
    With(Box<Stmt>),
    /// `let pat = expr else { else_ }` followed by `rest`, the remaining
    /// statements of the block, where the pattern's names are bound.
    LetElse { pat: Pattern, expr: Box<Expr>, else_: Block, rest: Block },
    /// A type in expression position, e.g. `Chan[int]`, `[]int`, `?int`.
    TypeExpr(TypeExpr),
}

#[derive(Clone, Debug)]
pub enum InterpPart {
    Lit(String),
    Expr(Box<Expr>, Option<String>, Span),
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}
