// Semantic types and program-wide definitions.
#![allow(dead_code)]

use crate::ast::{Mode, YieldKind};
use crate::diag::Span;
use crate::tir::TExpr;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

pub type DefId = u32;
pub type FuncId = u32;
pub type TParamId = u32;
pub type PkgId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IntTy {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
}

impl IntTy {
    pub fn signed(self) -> bool {
        matches!(self, IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64)
    }
    pub fn bits(self) -> u32 {
        match self {
            IntTy::I8 | IntTy::U8 => 8,
            IntTy::I16 | IntTy::U16 => 16,
            IntTy::I32 | IntTy::U32 => 32,
            IntTy::I64 | IntTy::U64 => 64,
        }
    }
    pub fn min(self) -> i128 {
        if self.signed() {
            -(1i128 << (self.bits() - 1))
        } else {
            0
        }
    }
    pub fn max(self) -> i128 {
        if self.signed() {
            (1i128 << (self.bits() - 1)) - 1
        } else {
            (1i128 << self.bits()) - 1
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            IntTy::I8 => "i8",
            IntTy::I16 => "i16",
            IntTy::I32 => "i32",
            IntTy::I64 => "int",
            IntTy::U8 => "u8",
            IntTy::U16 => "u16",
            IntTy::U32 => "u32",
            IntTy::U64 => "u64",
        }
    }
    pub fn c_name(self) -> &'static str {
        match self {
            IntTy::I8 => "int8_t",
            IntTy::I16 => "int16_t",
            IntTy::I32 => "int32_t",
            IntTy::I64 => "int64_t",
            IntTy::U8 => "uint8_t",
            IntTy::U16 => "uint16_t",
            IntTy::U32 => "uint32_t",
            IntTy::U64 => "uint64_t",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FuncTy {
    pub params: Vec<(Mode, Ty)>,
    pub ret: Ty,
    /// `send fn`: captures only `send` values, and only reads them, so any
    /// number of tasks may call it at once. Function items are `send`.
    pub send: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Ty {
    Unit,
    Never,
    Bool,
    Int(IntTy),
    F32,
    F64,
    Str,
    String,
    Vec(Rc<Ty>),
    Slice(Rc<Ty>),
    Array(Rc<Ty>, u64),
    /// An array whose length is an integer type parameter or inference
    /// variable; becomes `Array` once the length is known.
    ArrayT(Rc<Ty>, Rc<Ty>),
    Map(Rc<Ty>, Rc<Ty>),
    Option(Rc<Ty>),
    Result(Rc<Ty>, Rc<Ty>),
    Box(Rc<Ty>),
    Rc(Rc<Ty>),
    Arc(Rc<Ty>),
    Weak(Rc<Ty>),
    Cell(Rc<Ty>),
    Mutex(Rc<Ty>),
    /// `Mutex(&x)`: a lock guarding a borrowed place. A view: it cannot be
    /// stored, and while it exists `x` is reachable only through it.
    MutexRef(Rc<Ty>),
    /// The sending end of a channel: when the last one is dropped, the
    /// channel closes.
    Sender(Rc<Ty>),
    /// The receiving end: when the last one is dropped, sends fail.
    Receiver(Rc<Ty>),
    /// `let t = go f()`: a handle to a task started in a `scope`. A view: it
    /// can't leave the scope, and must be joined or cancelled.
    Task(Rc<Ty>),
    /// `scope s { }`: a handle to a running scope. A view.
    Scope,
    /// `isolate { }`: nothing outside the value points into it, so it may be
    /// sent even when `T` can't be.
    Iso(Rc<Ty>),
    Func(Rc<FuncTy>),
    Adt(DefId, Rc<Vec<Ty>>),
    Iface(DefId, Rc<Vec<Ty>>),
    Param(TParamId),
    Var(u32),
    Ptr(Rc<Ty>),
    ConstInt(u64),
    Error,
}

impl Ty {
    pub fn int() -> Ty {
        Ty::Int(IntTy::I64)
    }
    pub fn vec(t: Ty) -> Ty {
        Ty::Vec(Rc::new(t))
    }
    pub fn opt(t: Ty) -> Ty {
        Ty::Option(Rc::new(t))
    }
    pub fn result(t: Ty, e: Ty) -> Ty {
        Ty::Result(Rc::new(t), Rc::new(e))
    }
    pub fn is_numeric(&self) -> bool {
        matches!(self, Ty::Int(_) | Ty::F32 | Ty::F64)
    }
    pub fn is_int(&self) -> bool {
        matches!(self, Ty::Int(_))
    }
    pub fn is_float(&self) -> bool {
        matches!(self, Ty::F32 | Ty::F64)
    }
    pub fn is_error(&self) -> bool {
        matches!(self, Ty::Error)
    }
    pub fn is_stringish(&self) -> bool {
        matches!(self, Ty::Str | Ty::String)
    }
    /// Borrowed views: may not be stored in fields or collections.
    pub fn has_vars(&self) -> bool {
        let mut found = false;
        self.walk(&mut |t| {
            if matches!(t, Ty::Var(_)) {
                found = true
            }
        });
        found
    }
    pub fn has_params(&self) -> bool {
        let mut found = false;
        self.walk(&mut |t| {
            if matches!(t, Ty::Param(_)) {
                found = true
            }
        });
        found
    }
    pub fn walk(&self, f: &mut dyn FnMut(&Ty)) {
        f(self);
        match self {
            Ty::Vec(a) | Ty::Slice(a) | Ty::Array(a, _) | Ty::Option(a) | Ty::Box(a) | Ty::Rc(a) | Ty::Arc(a) | Ty::Weak(a) | Ty::Cell(a) | Ty::Mutex(a) | Ty::MutexRef(a) | Ty::Sender(a) | Ty::Receiver(a) | Ty::Task(a) | Ty::Iso(a) | Ty::Ptr(a) => a.walk(f),
            Ty::Map(a, b) | Ty::Result(a, b) | Ty::ArrayT(a, b) => {
                a.walk(f);
                b.walk(f)
            }
            Ty::Func(ft) => {
                for (_, p) in &ft.params {
                    p.walk(f)
                }
                ft.ret.walk(f)
            }
            Ty::Adt(_, args) | Ty::Iface(_, args) => {
                for a in args.iter() {
                    a.walk(f)
                }
            }
            _ => {}
        }
    }
    pub fn map(&self, f: &mut dyn FnMut(&Ty) -> Option<Ty>) -> Ty {
        if let Some(t) = f(self) {
            return t;
        }
        let m = |a: &Rc<Ty>, f: &mut dyn FnMut(&Ty) -> Option<Ty>| Rc::new(a.map(f));
        match self {
            Ty::Vec(a) => Ty::Vec(m(a, f)),
            Ty::Slice(a) => Ty::Slice(m(a, f)),
            Ty::Array(a, n) => Ty::Array(m(a, f), *n),
            Ty::ArrayT(a, n) => {
                let len = n.map(f);
                let elem = m(a, f);
                match len {
                    Ty::ConstInt(k) => Ty::Array(elem, k),
                    l => Ty::ArrayT(elem, Rc::new(l)),
                }
            }
            Ty::Option(a) => Ty::Option(m(a, f)),
            Ty::Box(a) => Ty::Box(m(a, f)),
            Ty::Rc(a) => Ty::Rc(m(a, f)),
            Ty::Arc(a) => Ty::Arc(m(a, f)),
            Ty::Weak(a) => Ty::Weak(m(a, f)),
            Ty::Cell(a) => Ty::Cell(m(a, f)),
            Ty::Mutex(a) => Ty::Mutex(m(a, f)),
            Ty::MutexRef(a) => Ty::MutexRef(m(a, f)),
            Ty::Sender(a) => Ty::Sender(m(a, f)),
            Ty::Receiver(a) => Ty::Receiver(m(a, f)),
            Ty::Task(a) => Ty::Task(m(a, f)),
            Ty::Iso(a) => Ty::Iso(m(a, f)),
            Ty::Ptr(a) => Ty::Ptr(m(a, f)),
            Ty::Map(a, b) => Ty::Map(m(a, f), m(b, f)),
            Ty::Result(a, b) => Ty::Result(m(a, f), m(b, f)),
            Ty::Func(ft) => Ty::Func(Rc::new(FuncTy { params: ft.params.iter().map(|(md, p)| (*md, p.map(f))).collect(), ret: ft.ret.map(f), send: ft.send })),
            Ty::Adt(d, args) => Ty::Adt(*d, Rc::new(args.iter().map(|a| a.map(f)).collect())),
            Ty::Iface(d, args) => Ty::Iface(*d, Rc::new(args.iter().map(|a| a.map(f)).collect())),
            t => t.clone(),
        }
    }
    pub fn subst(&self, s: &HashMap<TParamId, Ty>) -> Ty {
        if s.is_empty() {
            return self.clone();
        }
        self.map(&mut |t| match t {
            Ty::Param(p) => s.get(p).cloned(),
            _ => None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cap {
    Any,
    Copy,
    Clone,
    Default,
    Send,
    Comparable,
    Ordered,
    Hash,
}

impl Cap {
    pub fn from_name(s: &str) -> Option<Cap> {
        Some(match s {
            "any" => Cap::Any,
            "copy" => Cap::Copy,
            "clone" => Cap::Clone,
            "default" => Cap::Default,
            "send" => Cap::Send,
            "comparable" => Cap::Comparable,
            "ordered" => Cap::Ordered,
            "hash" => Cap::Hash,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Cap::Any => "any",
            Cap::Copy => "copy",
            Cap::Clone => "clone",
            Cap::Default => "default",
            Cap::Send => "send",
            Cap::Comparable => "comparable",
            Cap::Ordered => "ordered",
            Cap::Hash => "hash",
        }
    }
}

#[derive(Clone, Debug)]
pub enum Bound {
    Cap(Cap),
    Iface(DefId, Vec<Ty>),
}

#[derive(Clone, Debug)]
pub struct TParamInfo {
    pub name: String,
    pub bounds: Vec<Bound>,
    pub is_int: bool,
}

#[derive(Clone, Debug)]
pub struct FieldInfo {
    pub name: String,
    pub ty: Ty,
    pub default: Option<TExpr>,
    pub is_pub: bool,
    pub embedded: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct VariantInfo {
    pub name: String,
    pub fields: Vec<(String, Ty)>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct IfaceMethodInfo {
    pub name: String,
    pub recv_mode: Mode,
    pub params: Vec<(String, Mode, Ty)>,
    pub ret: Ty,
}

#[derive(Clone, Debug)]
pub enum TypeDefKind {
    Struct { fields: Vec<FieldInfo> },
    Enum { variants: Vec<VariantInfo> },
    Iface { methods: Vec<IfaceMethodInfo>, type_set: Option<Vec<(bool, Ty)>> },
}

#[derive(Clone, Debug)]
pub struct TypeDef {
    pub name: String,
    pub pkg: PkgId,
    pub tparams: Vec<TParamId>,
    pub kind: TypeDefKind,
    pub is_copy: bool,
    pub is_pub: bool,
    pub unsafe_send: bool,
    pub newtype: bool,
    pub span: Span,
    /// Has a `drop` method.
    pub has_drop: bool,
    /// One of the prelude's `TupleN` structs: written `(A, B)`, copy when its
    /// elements are.
    pub tuple: bool,
    /// `view struct` / `view enum`: may hold views and is one itself, so it
    /// lives only in variables, like `str` (spec 5.4). Always copy.
    pub view: bool,
    /// `linear struct` / `linear enum`: never dropped implicitly. Every value
    /// must be consumed: moved into a `sink` parameter (its own `sink self`
    /// methods above all), returned, or stored in something that is (spec 5.7).
    pub linear: bool,
}

impl TypeDef {
    pub fn is_struct(&self) -> bool {
        matches!(self.kind, TypeDefKind::Struct { .. })
    }
    pub fn fields(&self) -> &[FieldInfo] {
        match &self.kind {
            TypeDefKind::Struct { fields } => fields,
            _ => &[],
        }
    }
    pub fn variants(&self) -> &[VariantInfo] {
        match &self.kind {
            TypeDefKind::Enum { variants } => variants,
            _ => &[],
        }
    }
    pub fn is_simple_enum(&self) -> bool {
        match &self.kind {
            TypeDefKind::Enum { variants } => variants.iter().all(|v| v.fields.is_empty()),
            _ => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FuncKind {
    Normal,
    /// Implemented by the compiler / runtime.
    Intrinsic,
    /// `extern "C"` declaration loaded from a library (or libc).
    ExternC { lib: Option<String> },
    Test,
}

#[derive(Clone, Debug)]
pub struct FuncParam {
    pub name: String,
    pub mode: Mode,
    pub ty: Ty,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct FuncDef {
    pub name: String,
    pub pkg: PkgId,
    pub file: u32,
    pub recv: Option<FuncParam>,
    pub tparams: Vec<TParamId>,
    pub params: Vec<FuncParam>,
    pub ret: Ty,
    /// `-> str of a, b`: the arguments (receiver first) the returned view may
    /// point into, as declared. Callers rely on it instead of what the
    /// borrow checker infers from the body, so the body may change without
    /// changing what callers see.
    pub ret_of: Option<Vec<usize>>,
    pub yields: Option<YieldKind>,
    pub yield_opt: bool,
    pub is_pub: bool,
    pub kind: FuncKind,
    pub span: Span,
    /// Index into the parsed AST for the body: (file index, item index, extern index)
    pub ast: Option<(usize, usize, Option<usize>)>,
    /// Receiver key (for methods).
    pub recv_key: Option<TyKey>,
    pub is_extern_c_export: bool,
    /// `unsafe fn`: every call needs an `unsafe` block.
    pub is_unsafe: bool,
    /// Associated functions: the owning type (with this function's type params).
    pub owner_ty: Option<Ty>,
    /// `gen fn`: what it yields. Its last parameter is then the hidden loop
    /// body (`fn(T) -> Flow`) a `for` passes it, and it returns nothing.
    pub gen_of: Option<Ty>,
    /// `checkfree fn`: codegen reports any bounds check left in it.
    pub checkfree: bool,
}

impl FuncDef {
    /// Whether a call needs an `unsafe` block: an `unsafe fn`, or a C function.
    pub fn needs_unsafe(&self) -> bool {
        self.is_unsafe || matches!(self.kind, FuncKind::ExternC { .. })
    }
}

/// Head constructor used to look up methods.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TyKey {
    Adt(DefId),
    Iface(DefId),
    Vec,
    Slice,
    Array,
    Map,
    Option,
    Result,
    Box,
    Rc,
    Arc,
    Weak,
    Cell,
    Mutex,
    MutexRef,
    Sender,
    Receiver,
    Task,
    Scope,
    Iso,
    Str,
    String,
    Int(IntTy),
    F32,
    F64,
    Bool,
    Ptr,
}

impl TyKey {
    pub fn of(t: &Ty) -> Option<TyKey> {
        Some(match t {
            Ty::Adt(d, _) => TyKey::Adt(*d),
            Ty::Iface(d, _) => TyKey::Iface(*d),
            Ty::Vec(_) => TyKey::Vec,
            Ty::Slice(_) => TyKey::Slice,
            Ty::Array(..) | Ty::ArrayT(..) => TyKey::Array,
            Ty::Map(..) => TyKey::Map,
            Ty::Option(_) => TyKey::Option,
            Ty::Result(..) => TyKey::Result,
            Ty::Box(_) => TyKey::Box,
            Ty::Rc(_) => TyKey::Rc,
            Ty::Arc(_) => TyKey::Arc,
            Ty::Weak(_) => TyKey::Weak,
            Ty::Cell(_) => TyKey::Cell,
            Ty::Mutex(_) => TyKey::Mutex,
            Ty::MutexRef(_) => TyKey::MutexRef,
            Ty::Sender(_) => TyKey::Sender,
            Ty::Receiver(_) => TyKey::Receiver,
            Ty::Task(_) => TyKey::Task,
            Ty::Scope => TyKey::Scope,
            Ty::Iso(_) => TyKey::Iso,
            Ty::Str => TyKey::Str,
            Ty::String => TyKey::String,
            Ty::Int(i) => TyKey::Int(*i),
            Ty::F32 => TyKey::F32,
            Ty::F64 => TyKey::F64,
            Ty::Bool => TyKey::Bool,
            Ty::Ptr(_) => TyKey::Ptr,
            _ => return None,
        })
    }
}

#[derive(Debug, Default)]
pub struct Package {
    pub name: String,
    pub path: String,
    pub items: HashMap<String, Sym>,
    pub is_std: bool,
}

#[derive(Clone, Debug)]
pub enum Sym {
    Type(DefId),
    Alias(Ty),
    Func(FuncId),
    Const(u32),
    Global(u32),
}

/// `let name = value` at package level: `init` computes the value, once, on
/// first use, whichever task gets there first.
#[derive(Clone, Debug)]
pub struct GlobalDef {
    pub name: String,
    pub ty: Ty,
    pub pkg: PkgId,
    pub is_pub: bool,
    pub span: Span,
    pub init: FuncId,
}

#[derive(Clone, Debug)]
pub struct ConstDef {
    pub name: String,
    pub ty: Ty,
    pub value: Option<TExpr>,
    pub pkg: PkgId,
    pub is_pub: bool,
    pub span: Span,
}

#[derive(Default)]
pub struct Program {
    pub types: Vec<TypeDef>,
    pub funcs: Vec<FuncDef>,
    pub tparams: Vec<TParamInfo>,
    pub consts: Vec<ConstDef>,
    pub globals: Vec<GlobalDef>,
    pub packages: Vec<Package>,
    pub methods: HashMap<(TyKey, String), Vec<FuncId>>,
    /// Associated functions: `Point.new(...)`.
    pub assoc: HashMap<(TyKey, String), FuncId>,
    pub prelude: PkgId,
    pub main_pkg: PkgId,
}

impl Program {
    /// A borrowed view: `str`, `[]T`, a `view struct` or `view enum`, a lock
    /// guard (`MutexRef`), task handle or `Scope`, or an optional, result or
    /// array of one. A view lives only in variables (spec 5.4).
    pub fn is_view(&self, t: &Ty) -> bool {
        match t {
            Ty::Str | Ty::Slice(_) | Ty::MutexRef(_) | Ty::Task(_) | Ty::Scope => true,
            Ty::Adt(d, _) => self.def(*d).view,
            Ty::Option(t) | Ty::Array(t, _) | Ty::ArrayT(t, _) => self.is_view(t),
            Ty::Result(a, b) => self.is_view(a) || self.is_view(b),
            // a view collection: owns its buffer, borrows what its elements
            // view, and lives only in variables like any view (spec 5.4)
            Ty::Vec(a) => self.is_coll_elem_view(a),
            Ty::Map(k, v) => (self.is_coll_elem_view(k) || self.is_coll_elem_view(v)) && !(self.contains_view(k) && !self.is_coll_elem_view(k)) && !(self.contains_view(v) && !self.is_coll_elem_view(v)),
            _ => false,
        }
    }

    /// A view that a `Vec` or `Map` may hold: a plain view (`str`, `[]T`, a
    /// view struct), or a view collection in turn. Not task handles, scopes
    /// or lock guards, which their blocks track.
    fn is_coll_elem_view(&self, t: &Ty) -> bool {
        match t {
            Ty::Str | Ty::Slice(_) => true,
            Ty::Adt(d, _) => self.def(*d).view,
            Ty::Option(a) => self.is_coll_elem_view(a),
            Ty::Vec(_) | Ty::Map(..) => self.is_view(t),
            _ => false,
        }
    }

    /// A view that owns memory: a view collection (`Vec[str]`). References
    /// into it point into its buffer, as well as into what it views.
    pub fn owning_view(&self, t: &Ty) -> bool {
        match t {
            Ty::Vec(_) | Ty::Map(..) => self.is_view(t),
            Ty::Option(a) => self.owning_view(a),
            Ty::Result(a, b) => self.owning_view(a) || self.owning_view(b),
            _ => false,
        }
    }

    /// A borrowed view anywhere inside the type.
    pub fn contains_view(&self, t: &Ty) -> bool {
        match t {
            Ty::Str | Ty::Slice(_) | Ty::MutexRef(_) | Ty::Task(_) | Ty::Scope => true,
            // function values borrow their parameters; captures are checked separately
            Ty::Func(_) => false,
            Ty::Vec(a) | Ty::Array(a, _) | Ty::ArrayT(a, _) | Ty::Option(a) | Ty::Box(a) | Ty::Rc(a) | Ty::Arc(a) | Ty::Weak(a) | Ty::Cell(a) | Ty::Mutex(a) | Ty::Sender(a) | Ty::Receiver(a) | Ty::Iso(a) | Ty::Ptr(a) => self.contains_view(a),
            Ty::Map(a, b) | Ty::Result(a, b) => self.contains_view(a) || self.contains_view(b),
            Ty::Adt(d, args) => self.def(*d).view || args.iter().any(|a| self.contains_view(a)),
            Ty::Iface(_, args) => args.iter().any(|a| self.contains_view(a)),
            _ => false,
        }
    }

    /// A view stored inside a container (not just at the top / in an option).
    pub fn has_nested_view(&self, t: &Ty) -> bool {
        match t {
            Ty::Str | Ty::Slice(_) | Ty::Scope => false,
            Ty::Vec(_) | Ty::Map(..) if self.is_view(t) => false,
            Ty::Adt(d, args) if self.def(*d).view => args.iter().any(|a| self.contains_view(a)),
            Ty::Task(a) => self.contains_view(a),
            Ty::Option(a) => self.has_nested_view(a),
            Ty::Result(a, b) => self.has_nested_view(a) || self.has_nested_view(b),
            t => self.contains_view(t),
        }
    }

    /// Whether some type reachable from `t` (its fields, elements, and what
    /// it boxes or points to) satisfies `bad`. Each type is visited once, so
    /// recursive types end; a type that grows as it recurses
    /// (`struct Node[T] { next: ?Box[Node[Vec[T]]] }`) has endless types in
    /// it, so after `REACH_LIMIT` of them the answer is yes: every caller
    /// takes "reaches" as the cautious answer.
    pub fn reaches(&self, t: &Ty, bad: &dyn Fn(&Ty) -> bool) -> bool {
        const REACH_LIMIT: usize = 512;
        let mut seen: HashSet<Ty> = HashSet::new();
        let mut todo = vec![t.clone()];
        while let Some(t) = todo.pop() {
            if !seen.insert(t.clone()) {
                continue;
            }
            if seen.len() > REACH_LIMIT || bad(&t) {
                return true;
            }
            match &t {
                Ty::Vec(a) | Ty::Slice(a) | Ty::Array(a, _) | Ty::ArrayT(a, _) | Ty::Option(a) | Ty::Box(a) | Ty::Rc(a) | Ty::Arc(a) | Ty::Weak(a) | Ty::Cell(a) | Ty::Mutex(a) | Ty::MutexRef(a) | Ty::Sender(a) | Ty::Receiver(a) | Ty::Task(a) | Ty::Iso(a) | Ty::Ptr(a) => todo.push((**a).clone()),
                Ty::Map(a, b) | Ty::Result(a, b) => {
                    todo.push((**a).clone());
                    todo.push((**b).clone());
                }
                Ty::Adt(d, args) => match &self.def(*d).kind {
                    TypeDefKind::Struct { .. } => todo.extend(self.field_tys(*d, args).into_iter().map(|f| f.1)),
                    TypeDefKind::Enum { variants } => {
                        for v in 0..variants.len() {
                            todo.extend(self.variant_tys(*d, args, v).into_iter().map(|f| f.1));
                        }
                    }
                    TypeDefKind::Iface { .. } => {}
                },
                _ => {}
            }
        }
        false
    }

    /// Whether a value of this type must be consumed instead of dropped
    /// (spec 5.7): a `linear` type, a type parameter declared `any` (it may
    /// stand for one), or anything that owns one. References and views
    /// (`[]T`, raw pointers, `Weak`) don't own what they point to.
    pub fn is_linear(&self, t: &Ty) -> bool {
        const LIMIT: usize = 512;
        let mut seen: HashSet<Ty> = HashSet::new();
        let mut todo = vec![t.clone()];
        while let Some(t) = todo.pop() {
            if !seen.insert(t.clone()) || seen.len() > LIMIT {
                continue;
            }
            match &t {
                Ty::Param(p) => {
                    if self.tparam(*p).bounds.iter().any(|b| matches!(b, Bound::Cap(Cap::Any))) {
                        return true;
                    }
                }
                Ty::Vec(a) | Ty::Array(a, _) | Ty::ArrayT(a, _) | Ty::Option(a) | Ty::Box(a) | Ty::Rc(a) | Ty::Arc(a) | Ty::Cell(a) | Ty::Mutex(a) | Ty::Sender(a) | Ty::Receiver(a) | Ty::Task(a) | Ty::Iso(a) => todo.push((**a).clone()),
                Ty::Map(a, b) | Ty::Result(a, b) => {
                    todo.push((**a).clone());
                    todo.push((**b).clone());
                }
                Ty::Adt(d, args) => {
                    let def = self.def(*d);
                    if def.linear {
                        return true;
                    }
                    match &def.kind {
                        TypeDefKind::Struct { .. } => todo.extend(self.field_tys(*d, args).into_iter().map(|f| f.1)),
                        TypeDefKind::Enum { variants } => {
                            for v in 0..variants.len() {
                                todo.extend(self.variant_tys(*d, args, v).into_iter().map(|f| f.1));
                            }
                        }
                        TypeDefKind::Iface { .. } => {}
                    }
                }
                _ => {}
            }
        }
        false
    }

    /// Whether type parameter `p` may stand for a linear type: declared
    /// `[T: any]` (or `where T: any`).
    pub fn tparam_any(&self, p: TParamId) -> bool {
        self.tparam(p).bounds.iter().any(|b| matches!(b, Bound::Cap(Cap::Any)))
    }

    /// Dropping a value of this type only frees memory, which no program can
    /// observe: no `drop` method runs, no channel closes, no task is
    /// cancelled, no shared count changes (a `Weak` could tell), and no
    /// unknown code (interfaces, function values, type parameters) is
    /// involved. Such a value may be dropped earlier than the end of its
    /// scope (codegen/drops.rs), and moved instead of copied (a needless
    /// `.clone()`, borrowck.rs).
    pub fn drop_silent(&self, t: &Ty) -> bool {
        !self.reaches(t, &|t| match t {
            Ty::Rc(_) | Ty::Arc(_) | Ty::Weak(_) | Ty::Mutex(_) | Ty::MutexRef(_) | Ty::Sender(_) | Ty::Receiver(_) | Ty::Task(_) | Ty::Scope | Ty::Func(_) | Ty::Iface(..) | Ty::Param(_) | Ty::Var(_) | Ty::ArrayT(..) | Ty::Error => true,
            Ty::Adt(d, _) => {
                let def = self.def(*d);
                def.has_drop || matches!(def.kind, TypeDefKind::Iface { .. })
            }
            _ => false,
        })
    }

    /// Whether `.clone()` of this type only copies: no `clone` method of a
    /// type in it runs code of its own.
    pub fn plain_clone(&self, t: &Ty) -> bool {
        !self.reaches(t, &|t| match t {
            Ty::Adt(d, _) => self.methods.contains_key(&(TyKey::Adt(*d), "clone".to_string())) || matches!(self.def(*d).kind, TypeDefKind::Iface { .. }),
            Ty::Iface(..) | Ty::Func(_) | Ty::Param(_) | Ty::Var(_) | Ty::Error => true,
            _ => false,
        })
    }

    /// Whether a value of this type may hold a borrow of something else: a
    /// view, reference-like handle, raw pointer, function or interface value.
    pub fn may_borrow(&self, t: &Ty) -> bool {
        self.contains_view(t)
            || self.reaches(t, &|t| matches!(t, Ty::Str | Ty::Slice(_) | Ty::MutexRef(_) | Ty::Task(_) | Ty::Scope | Ty::Func(_) | Ty::Iface(..) | Ty::Ptr(_) | Ty::Param(_) | Ty::Var(_) | Ty::Error) || matches!(t, Ty::Adt(d, _) if matches!(self.def(*d).kind, TypeDefKind::Iface { .. })))
    }

    pub fn def(&self, d: DefId) -> &TypeDef {
        &self.types[d as usize]
    }
    pub fn func(&self, f: FuncId) -> &FuncDef {
        &self.funcs[f as usize]
    }
    pub fn tparam(&self, p: TParamId) -> &TParamInfo {
        &self.tparams[p as usize]
    }
    pub fn prelude_type(&self, name: &str) -> Option<DefId> {
        match self.packages[self.prelude as usize].items.get(name) {
            Some(Sym::Type(d)) => Some(*d),
            _ => None,
        }
    }

    /// Substitution mapping a definition's type params to `args`.
    pub fn def_subst(&self, d: DefId, args: &[Ty]) -> HashMap<TParamId, Ty> {
        self.def(d).tparams.iter().copied().zip(args.iter().cloned()).collect()
    }

    pub fn field_tys(&self, d: DefId, args: &[Ty]) -> Vec<(String, Ty)> {
        let s = self.def_subst(d, args);
        self.def(d).fields().iter().map(|f| (f.name.clone(), f.ty.subst(&s))).collect()
    }

    pub fn variant_tys(&self, d: DefId, args: &[Ty], v: usize) -> Vec<(String, Ty)> {
        let s = self.def_subst(d, args);
        self.def(d).variants()[v].fields.iter().map(|(n, t)| (n.clone(), t.subst(&s))).collect()
    }

    pub fn show(&self, t: &Ty) -> String {
        match t {
            Ty::Unit => "()".into(),
            Ty::Never => "never".into(),
            Ty::Bool => "bool".into(),
            Ty::Int(i) => i.name().into(),
            Ty::F32 => "f32".into(),
            Ty::F64 => "f64".into(),
            Ty::Str => "str".into(),
            Ty::String => "String".into(),
            Ty::Vec(a) => format!("Vec[{}]", self.show(a)),
            Ty::Slice(a) => format!("[]{}", self.show(a)),
            Ty::Array(a, n) => format!("[{}]{}", n, self.show(a)),
            Ty::ArrayT(a, n) => format!("[{}]{}", self.show(n), self.show(a)),
            Ty::Map(a, b) => format!("Map[{}, {}]", self.show(a), self.show(b)),
            Ty::Option(a) => format!("?{}", self.show(a)),
            Ty::Result(a, b) => {
                if **a == Ty::Unit {
                    format!("! {}", self.show(b))
                } else {
                    format!("{} ! {}", self.show(a), self.show(b))
                }
            }
            Ty::Box(a) => format!("Box[{}]", self.show(a)),
            Ty::Rc(a) => format!("Rc[{}]", self.show(a)),
            Ty::Arc(a) => format!("Arc[{}]", self.show(a)),
            Ty::Weak(a) => format!("Weak[{}]", self.show(a)),
            Ty::Cell(a) => format!("Cell[{}]", self.show(a)),
            Ty::Mutex(a) => format!("Mutex[{}]", self.show(a)),
            Ty::MutexRef(a) => format!("MutexRef[{}]", self.show(a)),
            Ty::Sender(a) => format!("Sender[{}]", self.show(a)),
            Ty::Receiver(a) => format!("Receiver[{}]", self.show(a)),
            Ty::Task(a) => format!("Task[{}]", self.show(a)),
            Ty::Scope => "Scope".into(),
            Ty::Iso(a) => format!("Iso[{}]", self.show(a)),
            Ty::Ptr(a) => format!("*{}", self.show(a)),
            Ty::Func(ft) => {
                let ps: Vec<String> = ft
                    .params
                    .iter()
                    .map(|(m, p)| match m {
                        Mode::Borrow => self.show(p),
                        Mode::Inout => format!("inout {}", self.show(p)),
                        Mode::Sink => format!("sink {}", self.show(p)),
                    })
                    .collect();
                let kw = if ft.send { "send fn" } else { "fn" };
                if ft.ret == Ty::Unit {
                    format!("{}({})", kw, ps.join(", "))
                } else {
                    format!("{}({}) -> {}", kw, ps.join(", "), self.show(&ft.ret))
                }
            }
            Ty::Adt(d, args) if self.def(*d).tuple => format!("({})", args.iter().map(|a| self.show(a)).collect::<Vec<_>>().join(", ")),
            Ty::Adt(d, args) | Ty::Iface(d, args) => {
                let def = self.def(*d);
                let pkg = &self.packages[def.pkg as usize];
                let base = if def.pkg == self.main_pkg || def.pkg == self.prelude { def.name.clone() } else { format!("{}.{}", pkg.name, def.name) };
                if args.is_empty() {
                    base
                } else {
                    format!("{}[{}]", base, args.iter().map(|a| self.show(a)).collect::<Vec<_>>().join(", "))
                }
            }
            Ty::Param(p) => self.tparam(*p).name.clone(),
            Ty::Var(_) => "_".into(),
            Ty::ConstInt(n) => n.to_string(),
            Ty::Error => "<error>".into(),
        }
    }
}
