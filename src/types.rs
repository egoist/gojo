// Semantic types and program-wide definitions.
#![allow(dead_code)]

use crate::ast::{Mode, YieldKind};
use crate::diag::Span;
use crate::tir::TExpr;
use std::collections::HashMap;
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
    pub fn is_view(&self) -> bool {
        match self {
            Ty::Str | Ty::Slice(_) | Ty::MutexRef(_) | Ty::Task(_) | Ty::Scope => true,
            Ty::Option(t) | Ty::Array(t, _) | Ty::ArrayT(t, _) => t.is_view(),
            Ty::Result(a, b) => a.is_view() || b.is_view(),
            _ => false,
        }
    }
    /// A borrowed view anywhere inside the type.
    pub fn contains_view(&self) -> bool {
        match self {
            Ty::Str | Ty::Slice(_) | Ty::MutexRef(_) | Ty::Task(_) | Ty::Scope => true,
            // function values borrow their parameters; captures are checked separately
            Ty::Func(_) => false,
            Ty::Vec(a) | Ty::Array(a, _) | Ty::ArrayT(a, _) | Ty::Option(a) | Ty::Box(a) | Ty::Rc(a) | Ty::Arc(a) | Ty::Weak(a) | Ty::Cell(a) | Ty::Mutex(a) | Ty::Sender(a) | Ty::Receiver(a) | Ty::Iso(a) | Ty::Ptr(a) => a.contains_view(),
            Ty::Map(a, b) | Ty::Result(a, b) => a.contains_view() || b.contains_view(),
            Ty::Adt(_, args) | Ty::Iface(_, args) => args.iter().any(|a| a.contains_view()),
            _ => false,
        }
    }
    /// A view stored inside a container (not just at the top / in an option).
    pub fn has_nested_view(&self) -> bool {
        match self {
            Ty::Str | Ty::Slice(_) | Ty::Scope => false,
            Ty::Task(a) => a.contains_view(),
            Ty::Option(a) => a.has_nested_view(),
            Ty::Result(a, b) => a.has_nested_view() || b.has_nested_view(),
            t => t.contains_view(),
        }
    }
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
    /// Associated functions: the owning type (with this function's type params).
    pub owner_ty: Option<Ty>,
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
    pub packages: Vec<Package>,
    pub methods: HashMap<(TyKey, String), Vec<FuncId>>,
    /// Associated functions: `Point.new(...)`.
    pub assoc: HashMap<(TyKey, String), FuncId>,
    pub prelude: PkgId,
    pub main_pkg: PkgId,
}

impl Program {
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
            Ty::Rc(a) => format!("rc[{}]", self.show(a)),
            Ty::Arc(a) => format!("arc[{}]", self.show(a)),
            Ty::Weak(a) => format!("weak[{}]", self.show(a)),
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
                    format!("{}({}) {}", kw, ps.join(", "), self.show(&ft.ret))
                }
            }
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
