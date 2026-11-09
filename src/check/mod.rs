// Declaration collection, type resolution and capability rules.
//
// Everything here works from signatures and type declarations only; function
// bodies are checked one at a time in `body.rs` and never influence other
// functions' checking.

pub mod body;
mod isolation;
pub mod lazy;
pub mod pattern;

use crate::ast::{self, Mode, TypeExpr, TypeExprKind};
use crate::diag::{Diag, Span};
use crate::tir::*;
use crate::types::*;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// The longest array type. Arrays are values (on the stack, or inline in other
/// values); code generation also bounds their total size in bytes.
pub const MAX_ARRAY_LEN: u64 = u32::MAX as u64;

/// How deeply capability checks follow types into their fields: only types
/// whose arguments grow at each level go deeper.
const MAX_CAP_DEPTH: usize = 200;

pub struct Unit {
    pub name: String,
    pub path: String,
    pub is_std: bool,
    pub files: Vec<ast::File>,
}

pub struct FileCx {
    pub pkg: PkgId,
    /// `use os` / `use ./geom as g`: package bindings.
    pub imports: HashMap<String, (PkgId, Span)>,
    /// `use ./geom.{Vec2}`: names brought directly into scope.
    pub names: HashMap<String, (PkgId, Span)>,
    pub used_imports: HashSet<String>,
}

pub struct Checker {
    pub prog: Program,
    pub files: Vec<ast::File>,
    pub file_cx: Vec<FileCx>,
    pub diags: Vec<Diag>,
    pub tfuncs: Vec<TFunc>,
    pub tests: Vec<(String, FuncId)>,
    /// Memo for recursive capability checks.
    send_memo: HashMap<DefId, bool>,
    pub iface_error: Option<DefId>,
    pub pending_infer: Vec<(Span, String, Vec<String>, TExpr)>,
    /// Types that are `copy` because the compiler inferred it.
    pub inferred_copy: Vec<DefId>,
    /// set while resolving a constraint: interfaces with type sets are allowed there
    allow_typeset: bool,
    /// Was `copy` set on this type by inference (not declared)?
    copy_candidates: Vec<DefId>,
    /// Types whose capabilities are being checked (for recursive types).
    cap_stack: std::cell::RefCell<Vec<(DefId, Vec<Ty>, &'static str)>>,
    /// Answers found during the current outermost capability query. Without
    /// it, a type like `Chan[Chan[...T]]` (two fields per level) costs 2^depth.
    /// Within one query an answer that relied on an "in progress" assumption is
    /// only wrong when that ancestor turns out false, and then so does the
    /// query; the cache is dropped when the query ends.
    cap_cache: std::cell::RefCell<HashMap<(DefId, Vec<Ty>, &'static str), bool>>,
    /// Per constant: 0 not checked yet, 1 being checked, 2 checked.
    pub const_state: Vec<u8>,
    pub global_state: Vec<u8>,
}

#[derive(Clone)]
pub struct TyEnv {
    pub file: usize,
    pub pkg: PkgId,
    pub tparams: HashMap<String, Ty>,
}

pub fn check_program(units: Vec<Unit>) -> Result<(Program, Vec<TFunc>, Vec<(String, FuncId)>), Vec<Diag>> {
    let mut c = Checker {
        prog: Program::default(),
        files: vec![],
        file_cx: vec![],
        diags: vec![],
        tfuncs: vec![],
        tests: vec![],
        send_memo: HashMap::new(),
        iface_error: None,
        pending_infer: vec![],
        cap_stack: std::cell::RefCell::new(vec![]),
        cap_cache: std::cell::RefCell::new(HashMap::new()),
        inferred_copy: vec![],
        allow_typeset: false,
        copy_candidates: vec![],
        const_state: vec![],
        global_state: vec![],
    };
    c.collect(units);
    if !c.diags.is_empty() {
        return Err(without_cascades(c.diags));
    }
    c.check_bodies();
    c.check_unused_imports();
    if !c.diags.is_empty() {
        return Err(without_cascades(c.diags));
    }
    Ok((c.prog, c.tfuncs, c.tests))
}

/// Drops messages about types that failed to resolve (shown as `<error>`): each
/// repeats an error reported where the type failed.
fn without_cascades(diags: Vec<Diag>) -> Vec<Diag> {
    let mentions = |d: &Diag| d.msg.contains("<error>") || d.notes.iter().any(|n| n.1.contains("<error>"));
    if diags.iter().all(mentions) {
        return diags;
    }
    diags.into_iter().filter(|d| !mentions(d)).collect()
}

impl Checker {
    pub fn err(&mut self, span: Span, msg: impl Into<String>) {
        self.diags.push(Diag::new(span, msg));
    }

    // ------------------------------------------------------------------
    // Collection
    // ------------------------------------------------------------------

    fn collect(&mut self, units: Vec<Unit>) {
        // 1. packages and files
        let mut pkg_by_path: HashMap<String, PkgId> = HashMap::new();
        for u in &units {
            let id = self.prog.packages.len() as PkgId;
            self.prog.packages.push(Package { name: u.name.clone(), path: u.path.clone(), items: HashMap::new(), is_std: u.is_std });
            pkg_by_path.insert(u.path.clone(), id);
            if u.path == "prelude" {
                self.prog.prelude = id;
            }
        }
        self.prog.main_pkg = (units.len() - 1) as PkgId;
        for (pi, u) in units.into_iter().enumerate() {
            for f in u.files {
                self.file_cx.push(FileCx { pkg: pi as PkgId, imports: HashMap::new(), names: HashMap::new(), used_imports: HashSet::new() });
                self.files.push(f);
            }
        }
        // imports
        for fi in 0..self.files.len() {
            let imports = self.files[fi].imports.clone();
            for imp in imports {
                match pkg_by_path.get(&imp.path) {
                    Some(&p) => match &imp.names {
                        Some(ns) => {
                            for (n, sp) in ns {
                                if self.file_cx[fi].names.insert(n.clone(), (p, *sp)).is_some() {
                                    self.err(*sp, format!("`{}` is imported twice", n));
                                }
                            }
                        }
                        None => {
                            let name = imp.alias.clone().unwrap_or_else(|| self.prog.packages[p as usize].name.clone());
                            if self.file_cx[fi].imports.insert(name.clone(), (p, imp.span)).is_some() {
                                self.err(imp.span, format!("`{}` is imported twice", name));
                            }
                        }
                    },
                    None => self.err(imp.span, format!("cannot find package `{}`", imp.path)),
                }
            }
        }

        // 2. type names
        let mut type_items: Vec<(usize, usize, DefId)> = vec![];
        let mut alias_items: Vec<(usize, usize)> = vec![];
        for fi in 0..self.files.len() {
            let pkg = self.file_cx[fi].pkg;
            for ii in 0..self.files[fi].items.len() {
                let item = &self.files[fi].items[ii];
                let (name, kind, tps, is_copy, unsafe_send, view, linear) = match &item.kind {
                    ast::ItemKind::Struct(s) => (s.name.clone(), TypeDefKind::Struct { fields: vec![] }, s.tparams.clone(), s.is_copy || s.is_view, s.unsafe_send, s.is_view, s.is_linear),
                    ast::ItemKind::Enum(e) => (e.name.clone(), TypeDefKind::Enum { variants: vec![] }, e.tparams.clone(), e.is_copy || e.is_view, false, e.is_view, e.is_linear),
                    // (for interfaces, `unsafe_send` records `send interface`)
                    ast::ItemKind::Interface(i) => (i.name.clone(), TypeDefKind::Iface { methods: vec![], type_set: None }, i.tparams.clone(), false, i.is_send, false, false),
                    ast::ItemKind::TypeAlias { newtype: true, name, tparams, .. } => (name.clone(), TypeDefKind::Struct { fields: vec![] }, tparams.clone(), false, false, false, false),
                    ast::ItemKind::TypeAlias { .. } => {
                        alias_items.push((fi, ii));
                        continue;
                    }
                    _ => continue,
                };
                let span = item.span;
                let is_pub = item.is_pub;
                let newtype = matches!(item.kind, ast::ItemKind::TypeAlias { .. });
                let id = self.prog.types.len() as DefId;
                let mut tparams = vec![];
                for tp in &tps {
                    tparams.push(self.new_tparam(&tp.name, tp.is_int));
                }
                let tuple = self.prog.packages[pkg as usize].is_std && self.prog.packages[pkg as usize].name == "prelude" && name.starts_with("Tuple");
                self.prog.types.push(TypeDef { name: name.clone(), pkg, tparams, kind, is_copy, is_pub, unsafe_send, newtype, span, has_drop: false, tuple, view, linear });
                if self.prog.packages[pkg as usize].items.insert(name.clone(), Sym::Type(id)).is_some() {
                    self.err(span, format!("`{}` is declared twice", name));
                }
                type_items.push((fi, ii, id));
            }
        }
        if let Some(Sym::Type(d)) = self.prog.packages[self.prog.prelude as usize].items.get("error") {
            self.iface_error = Some(*d);
        }
        // aliases, in dependency order: one may use another declared later
        let mut by_name: HashMap<(PkgId, String), (usize, usize)> = HashMap::new();
        for &(fi, ii) in &alias_items {
            if let ast::ItemKind::TypeAlias { name, .. } = &self.files[fi].items[ii].kind {
                by_name.insert((self.file_cx[fi].pkg, name.clone()), (fi, ii));
            }
        }
        let mut state: HashMap<(usize, usize), u8> = HashMap::new();
        for &(fi, ii) in &alias_items {
            self.resolve_alias(fi, ii, &by_name, &mut state);
        }

        // 3. type bodies
        for &(fi, ii, id) in &type_items {
            let item = self.files[fi].items[ii].clone();
            let mut env = self.file_env(fi);
            let tps = self.prog.def(id).tparams.clone();
            let decl_tps: Vec<ast::TParam> = match &item.kind {
                ast::ItemKind::Struct(s) => s.tparams.clone(),
                ast::ItemKind::Enum(e) => e.tparams.clone(),
                ast::ItemKind::Interface(i) => i.tparams.clone(),
                ast::ItemKind::TypeAlias { tparams, .. } => tparams.clone(),
                _ => vec![],
            };
            for (tp, &pid) in decl_tps.iter().zip(tps.iter()) {
                env.tparams.insert(tp.name.clone(), if tp.is_int { Ty::Param(pid) } else { Ty::Param(pid) });
            }
            for (tp, &pid) in decl_tps.iter().zip(tps.iter()) {
                let bounds = tp.bounds.iter().filter_map(|b| self.resolve_bound(b, &env)).collect();
                self.prog.tparams[pid as usize].bounds = bounds;
            }
            match &item.kind {
                ast::ItemKind::Struct(s) => {
                    let mut fields = vec![];
                    let mut seen = HashSet::new();
                    for f in &s.fields {
                        if !seen.insert(f.name.clone()) {
                            self.err(f.span, format!("duplicate field `{}`", f.name));
                        }
                        let ty = self.resolve_type(&f.ty, &env);
                        self.check_field_view(id, &ty, f.ty.span, &f.name);
                        fields.push(FieldInfo { name: f.name.clone(), ty, default: None, is_pub: f.is_pub || self.prog.packages[self.prog.def(id).pkg as usize].is_std, declared_pub: f.is_pub, embedded: f.embedded, span: f.span });
                    }
                    self.prog.types[id as usize].kind = TypeDefKind::Struct { fields };
                }
                ast::ItemKind::TypeAlias { ty, .. } => {
                    let t = self.resolve_type(ty, &env);
                    let is_copy = self.is_copy(&t);
                    self.prog.types[id as usize].kind = TypeDefKind::Struct { fields: vec![FieldInfo { name: "value".into(), ty: t, default: None, is_pub: true, declared_pub: false, embedded: false, span: item.span }] };
                    self.prog.types[id as usize].is_copy = is_copy;
                }
                ast::ItemKind::Enum(e) => {
                    let mut variants = vec![];
                    let mut seen = HashSet::new();
                    for v in &e.variants {
                        if !seen.insert(v.name.clone()) {
                            self.err(v.span, format!("duplicate variant `{}`", v.name));
                        }
                        let mut fs = vec![];
                        let mut names = HashSet::new();
                        for (n, t) in &v.fields {
                            if !names.insert(n.clone()) {
                                self.err(t.span, format!("variant `{}` has two values named `{}`", v.name, n));
                            }
                            let ty = self.resolve_type(t, &env);
                            self.check_field_view(id, &ty, t.span, n);
                            fs.push((n.clone(), ty));
                        }
                        variants.push(VariantInfo { name: v.name.clone(), fields: fs, span: v.span });
                    }
                    self.prog.types[id as usize].kind = TypeDefKind::Enum { variants };
                }
                ast::ItemKind::Interface(i) => {
                    let mut methods = vec![];
                    for m in &i.methods {
                        let params = m.params.iter().map(|p| (p.name.clone(), p.mode, self.resolve_type(&p.ty, &env))).collect();
                        let ret = m.ret.as_ref().map(|t| self.resolve_type(t, &env)).unwrap_or(Ty::Unit);
                        methods.push(IfaceMethodInfo { name: m.name.clone(), recv_mode: m.recv_mode, params, ret });
                    }
                    let type_set = if i.type_set.is_empty() { None } else { Some(i.type_set.iter().map(|(a, t)| (*a, self.resolve_type(t, &env))).collect()) };
                    self.prog.types[id as usize].kind = TypeDefKind::Iface { methods, type_set };
                }
                _ => {}
            }
        }
        // interface embedding (second pass so embedded ifaces are resolved)
        for &(fi, ii, id) in &type_items {
            if let ast::ItemKind::Interface(i) = &self.files[fi].items[ii].kind.clone() {
                let env = self.file_env(fi);
                for e in &i.embeds {
                    self.allow_typeset = true;
                    let t = self.resolve_type(e, &env);
                    match t {
                        Ty::Iface(d, _) => {
                            let extra = match &self.prog.def(d).kind {
                                TypeDefKind::Iface { methods, .. } => methods.clone(),
                                _ => vec![],
                            };
                            if let TypeDefKind::Iface { methods, .. } = &mut self.prog.types[id as usize].kind {
                                methods.extend(extra);
                            }
                        }
                        Ty::Error => {}
                        _ => {
                            // single non-interface type: a one-element type set
                            if let TypeDefKind::Iface { type_set, .. } = &mut self.prog.types[id as usize].kind {
                                type_set.get_or_insert_with(Vec::new).push((false, t));
                            }
                        }
                    }
                }
            }
        }

        // 4. functions & consts
        for fi in 0..self.files.len() {
            let pkg = self.file_cx[fi].pkg;
            for ii in 0..self.files[fi].items.len() {
                let item = self.files[fi].items[ii].clone();
                match &item.kind {
                    ast::ItemKind::Func(f) => {
                        let kind = if f.body.is_none() {
                            if self.prog.packages[pkg as usize].is_std {
                                FuncKind::Intrinsic
                            } else {
                                self.err(f.name_span, "function declaration is missing a body");
                                FuncKind::Intrinsic
                            }
                        } else {
                            FuncKind::Normal
                        };
                        self.declare_func(fi, ii, None, f, item.is_pub, kind);
                    }
                    ast::ItemKind::Extern(ext) => {
                        for (ei, f) in ext.funcs.iter().enumerate() {
                            self.declare_func(fi, ii, Some(ei), f, item.is_pub, FuncKind::ExternC { lib: ext.lib.clone() });
                        }
                    }
                    ast::ItemKind::Test { name, .. } => {
                        let id = self.prog.funcs.len() as FuncId;
                        self.prog.funcs.push(FuncDef {
                            name: format!("test_{}", id),
                            pkg,
                            file: self.files[fi].file_id,
                            recv: None,
                            tparams: vec![],
                            params: vec![],
                            ret: Ty::Unit,
                            ret_of: None,
                            yields: None,
                            yield_opt: false,
                            is_pub: false,
                            kind: FuncKind::Test,
                            span: item.span,
                            ast: Some((fi, ii, None)),
                            recv_key: None,
                            is_extern_c_export: false,
                            is_unsafe: false,
                            owner_ty: None,
                            gen_of: None,
                            checkfree: false,
                        });
                        if pkg == self.prog.main_pkg {
                            self.tests.push((name.clone(), id));
                        }
                    }
                    ast::ItemKind::Global { name, ty, .. } => {
                        let env = self.file_env(fi);
                        let t = ty.as_ref().map(|t| self.resolve_type(t, &env)).unwrap_or(Ty::Error);
                        let fid = self.prog.funcs.len() as FuncId;
                        self.prog.funcs.push(FuncDef {
                            name: format!("__init_{}", name),
                            pkg,
                            file: self.files[fi].file_id,
                            recv: None,
                            tparams: vec![],
                            params: vec![],
                            ret: t.clone(),
                            ret_of: None,
                            yields: None,
                            yield_opt: false,
                            is_pub: false,
                            kind: FuncKind::Normal,
                            span: item.span,
                            ast: None,
                            recv_key: None,
                            is_extern_c_export: false,
                            is_unsafe: false,
                            owner_ty: None,
                            gen_of: None,
                            checkfree: false,
                        });
                        let id = self.prog.globals.len() as u32;
                        self.prog.globals.push(GlobalDef { name: name.clone(), ty: t, pkg, is_pub: item.is_pub, span: item.span, init: fid });
                        if self.prog.packages[pkg as usize].items.insert(name.clone(), Sym::Global(id)).is_some() {
                            self.err(item.span, format!("`{}` is declared twice", name));
                        }
                    }
                    ast::ItemKind::Const { name, ty, .. } => {
                        let env = self.file_env(fi);
                        let t = ty.as_ref().map(|t| self.resolve_type(t, &env)).unwrap_or(Ty::Error);
                        let id = self.prog.consts.len() as u32;
                        self.prog.consts.push(ConstDef { name: name.clone(), ty: t, value: None, pkg, is_pub: item.is_pub, span: item.span });
                        if self.prog.packages[pkg as usize].items.insert(name.clone(), Sym::Const(id)).is_some() {
                            self.err(item.span, format!("`{}` is declared twice", name));
                        }
                    }
                    _ => {}
                }
            }
        }

        // selected names must exist, be public and not clash with the package's own items
        for fi in 0..self.file_cx.len() {
            let mut names: Vec<(String, (PkgId, Span))> = self.file_cx[fi].names.iter().map(|(k, v)| (k.clone(), *v)).collect();
            names.sort_by_key(|x| x.1 .1.lo);
            let own = self.file_cx[fi].pkg;
            for (n, (p, sp)) in names {
                let pname = self.prog.packages[p as usize].name.clone();
                match self.prog.packages[p as usize].items.get(&n).cloned() {
                    None => self.err(sp, format!("package `{}` has no `{}`", pname, n)),
                    Some(sym) => {
                        if !self.sym_is_pub(&sym) {
                            self.err(sp, format!("`{}` is not public in package `{}`", n, pname));
                        }
                        if self.prog.packages[own as usize].items.contains_key(&n) {
                            self.err(sp, format!("`{}` is imported but this package also declares `{}`", n, n));
                        }
                    }
                }
            }
        }

        // 5. drop methods, copy validation, recursive types
        for id in 0..self.prog.types.len() {
            let key = TyKey::Adt(id as DefId);
            if self.prog.methods.contains_key(&(key, "drop".to_string())) {
                self.prog.types[id].has_drop = true;
            }
            self.check_protocol_methods(id as DefId);
        }
        // Package-private, non-generic types are `copy` when all their fields
        // are (exported types must say `copy`, so adding a field to a public
        // type never silently changes how downstream code compiles).
        let mut auto: Vec<DefId> = (0..self.prog.types.len() as DefId)
            .filter(|&d| {
                let def = self.prog.def(d);
                !def.is_pub && !def.is_copy && !def.linear && def.tparams.is_empty() && !def.has_drop && !matches!(def.kind, TypeDefKind::Iface { .. }) && !self.prog.packages[def.pkg as usize].is_std
            })
            .collect();
        for &d in &auto {
            self.prog.types[d as usize].is_copy = true;
        }
        self.copy_candidates = auto.clone();
        loop {
            let before = auto.len();
            auto.retain(|&d| {
                let def = self.prog.def(d);
                let tys: Vec<Ty> = match &def.kind {
                    TypeDefKind::Struct { fields } => fields.iter().map(|f| f.ty.clone()).collect(),
                    TypeDefKind::Enum { variants } => variants.iter().flat_map(|v| v.fields.iter().map(|f| f.1.clone())).collect(),
                    _ => vec![],
                };
                tys.iter().all(|t| self.is_copy(t))
            });
            for d in 0..self.prog.types.len() as DefId {
                let def = self.prog.def(d);
                if !def.is_pub && def.tparams.is_empty() && !auto.contains(&d) && self.copy_inferred(d) {
                    self.prog.types[d as usize].is_copy = false;
                }
            }
            if auto.len() == before {
                break;
            }
        }
        self.inferred_copy = auto;
        for id in 0..self.prog.types.len() as DefId {
            let def = self.prog.def(id).clone();
            if def.is_copy && !self.inferred_copy.contains(&id) {
                let tys: Vec<Ty> = match &def.kind {
                    TypeDefKind::Struct { fields } => fields.iter().map(|f| f.ty.clone()).collect(),
                    TypeDefKind::Enum { variants } => variants.iter().flat_map(|v| v.fields.iter().map(|f| f.1.clone())).collect(),
                    _ => vec![],
                };
                for t in tys {
                    if !self.is_copy(&t) {
                        let s = self.prog.show(&t);
                        if def.view {
                            self.diags.push(Diag::new(def.span, format!("`{}` is a view type, so it can only hold views and copy values, and `{}` is neither", def.name, s)).note(None, "a view type is copied like `str` and never dropped: keep owned data (`String`, `Vec[T]`) in an ordinary variable or struct beside it"));
                        } else {
                            self.err(def.span, format!("`{}` is declared `copy` but contains `{}`, which is not copy", def.name, s));
                        }
                    }
                }
                if def.has_drop {
                    if def.view {
                        self.err(def.span, format!("`{}` is a view type, which is never dropped: it can't have a `drop` method", def.name));
                    } else {
                        self.err(def.span, format!("`{}` is declared `copy` but has a `drop` method", def.name));
                    }
                }
            }
            if def.linear && def.has_drop {
                self.err(def.span, format!("`{}` is linear, so it is never dropped implicitly: it can't have a `drop` method; its `sink self` methods end its values", def.name));
            }
            if !def.linear && !matches!(def.kind, TypeDefKind::Iface { .. }) {
                // a type that owns a linear value is linear itself, and says so
                // (its own `any` type parameters aside: `Stack[T: any]` is
                // linear exactly when its argument is)
                let unit: HashMap<TParamId, Ty> = def.tparams.iter().map(|&p| (p, Ty::Unit)).collect();
                let tys: Vec<(String, Ty)> = match &def.kind {
                    TypeDefKind::Struct { fields } => fields.iter().map(|f| (f.name.clone(), f.ty.subst(&unit))).collect(),
                    TypeDefKind::Enum { variants } => variants.iter().flat_map(|v| v.fields.iter().map(|f| (v.name.clone(), f.1.subst(&unit)))).collect(),
                    _ => vec![],
                };
                if let Some((n, t)) = tys.iter().find(|(_, t)| self.prog.is_linear(t)) {
                    let (ts, kw) = (self.prog.show(t), if def.is_struct() { "struct" } else { "enum" });
                    let what = if def.is_struct() { format!("field `{}`", n) } else { format!("variant `{}`", n) };
                    self.diags.push(Diag::new(def.span, format!("`{}` holds the linear `{}` ({}), so it must be consumed too: declare it `linear {} {}`", def.name, ts, what, kw, def.name)));
                }
            }
            if self.is_infinite(id, &mut vec![]) {
                self.err(def.span, format!("recursive type `{}` has infinite size; use Box[T] or Vec[T] for the recursive field", def.name));
            }
        }
    }

    fn is_infinite(&self, id: DefId, stack: &mut Vec<DefId>) -> bool {
        if stack.contains(&id) {
            return true;
        }
        stack.push(id);
        let def = self.prog.def(id);
        let tys: Vec<Ty> = match &def.kind {
            TypeDefKind::Struct { fields } => fields.iter().map(|f| f.ty.clone()).collect(),
            TypeDefKind::Enum { variants } => variants.iter().flat_map(|v| v.fields.iter().map(|f| f.1.clone())).collect(),
            _ => vec![],
        };
        let mut r = false;
        for t in tys {
            if self.inline_contains(&t, stack) {
                r = true;
                break;
            }
        }
        stack.pop();
        r
    }

    fn inline_contains(&self, t: &Ty, stack: &mut Vec<DefId>) -> bool {
        match t {
            Ty::Adt(d, _) => self.is_infinite(*d, stack),
            Ty::Option(a) | Ty::Array(a, _) | Ty::ArrayT(a, _) => self.inline_contains(a, stack),
            Ty::Result(a, b) => self.inline_contains(a, stack) || self.inline_contains(b, stack),
            _ => false,
        }
    }

    /// A method's receiver `Box[T]` names fresh type parameters; they have the
    /// bounds the type declared for its own (`struct Box[T Reader]`).
    fn inherit_bounds(&mut self, recv: &Ty, fresh: &[TParamId]) {
        let Ty::Adt(d, args) = recv else { return };
        let d = *d;
        let s = self.prog.def_subst(d, args);
        let own = self.prog.def(d).tparams.clone();
        for (i, a) in args.iter().enumerate() {
            let Ty::Param(p) = a else { continue };
            if !fresh.contains(p) {
                continue;
            }
            let Some(&dp) = own.get(i) else { continue };
            let bounds: Vec<Bound> = self.prog.tparam(dp).bounds.iter().map(|b| match b {
                Bound::Cap(c) => Bound::Cap(*c),
                Bound::Iface(id, targs) => Bound::Iface(*id, targs.iter().map(|t| t.subst(&s)).collect()),
            }).collect();
            let is_int = self.prog.tparam(dp).is_int;
            let info = &mut self.prog.tparams[*p as usize];
            info.is_int |= is_int;
            for b in bounds {
                info.bounds.push(b);
            }
        }
    }

    pub fn new_tparam(&mut self, name: &str, is_int: bool) -> TParamId {
        self.prog.tparams.push(TParamInfo { name: name.to_string(), bounds: vec![], is_int });
        (self.prog.tparams.len() - 1) as TParamId
    }

    pub fn file_env(&self, fi: usize) -> TyEnv {
        TyEnv { file: fi, pkg: self.file_cx[fi].pkg, tparams: HashMap::new() }
    }

    fn declare_func(&mut self, fi: usize, ii: usize, ei: Option<usize>, f: &ast::FuncDecl, is_pub: bool, kind: FuncKind) {
        let pkg = self.file_cx[fi].pkg;
        if f.is_unsafe {
            // The compiler calls these itself (`==`, `print`, `for`, drops...),
            // where no `unsafe` block could be written.
            if f.recv.is_some() && IMPLICIT_METHODS.contains(&f.name.as_str()) {
                self.diags.push(Diag::new(f.name_span, format!("`{}` can't be `unsafe`: the compiler calls it implicitly, outside any `unsafe` block", f.name)).note(None, "keep the method safe and do the unsafe part in an `unsafe` block inside it"));
            }
            if f.recv.is_none() && f.owner.is_none() && f.name == "main" && pkg == self.prog.main_pkg {
                self.err(f.name_span, "`main` can't be `unsafe`: nothing calls it from an `unsafe` block");
            }
        }
        let mut env = self.file_env(fi);
        let mut tparams = vec![];
        // receiver: unknown names in type-argument position introduce type params
        let mut recv = None;
        let mut recv_key = None;
        let mut owner_ty = None;
        // `extend T: Integer`: the method goes on every built-in type in the
        // bound's type set, and `T` is bound to the one it's called on
        let mut ext_keys: Vec<TyKey> = vec![];
        if !f.ext_tparams.is_empty() {
            if !self.prog.packages[pkg as usize].is_std {
                self.err(f.name_span, "only the standard library can extend every type of a type set");
            }
            for tp in &f.ext_tparams {
                let id = self.new_tparam(&tp.name, false);
                env.tparams.insert(tp.name.clone(), Ty::Param(id));
                tparams.push(id);
            }
            for tp in &f.ext_tparams {
                let Some(Ty::Param(id)) = env.tparams.get(&tp.name).cloned() else { continue };
                let bounds: Vec<Bound> = tp.bounds.iter().filter_map(|b| self.resolve_bound(b, &env)).collect();
                for b in &bounds {
                    if let Bound::Iface(d, _) = b {
                        if let TypeDefKind::Iface { type_set: Some(ts), .. } = &self.prog.def(*d).kind {
                            for (_, t) in ts {
                                if let Some(k) = TyKey::of(t) {
                                    if !ext_keys.contains(&k) {
                                        ext_keys.push(k);
                                    }
                                }
                            }
                        }
                    }
                }
                self.prog.tparams[id as usize].bounds.extend(bounds);
            }
            if ext_keys.is_empty() {
                self.err(f.name_span, "`extend T: ...` needs a bound with a type set, like `Integer`");
            }
        }
        if let Some(o) = &f.owner {
            let mut intro = vec![];
            self.collect_recv_params(o, &env, &mut intro, true);
            for n in intro {
                let id = self.new_tparam(&n, false);
                env.tparams.insert(n, Ty::Param(id));
                tparams.push(id);
            }
            let ty = self.resolve_type(o, &env);
            self.inherit_bounds(&ty, &tparams);
            recv_key = TyKey::of(&ty);
            if let Some(&k) = ext_keys.first() {
                recv_key = Some(k);
            } else if let Some(TyKey::Adt(d)) = recv_key {
                if self.prog.def(d).pkg != pkg {
                    self.err(o.span, "functions can only be added to a type in the package that defines it");
                }
            } else if recv_key.is_some() && !self.prog.packages[pkg as usize].is_std {
                self.err(o.span, format!("cannot add functions to built-in type `{}`", self.prog.show(&ty)));
            }
            owner_ty = Some(ty);
        }
        if let Some(r) = &f.recv {
            let mut intro = vec![];
            self.collect_recv_params(&r.ty, &env, &mut intro, true);
            for n in intro {
                let id = self.new_tparam(&n, false);
                env.tparams.insert(n, Ty::Param(id));
                tparams.push(id);
            }
            let ty = self.resolve_type(&r.ty, &env);
            self.inherit_bounds(&ty, &tparams);
            recv_key = TyKey::of(&ty);
            if let Some(&k) = ext_keys.first() {
                recv_key = Some(k);
            }
            match recv_key {
                _ if !ext_keys.is_empty() => {}
                Some(TyKey::Adt(d)) => {
                    if self.prog.def(d).pkg != pkg {
                        self.err(r.ty.span, "methods can only be declared in the package that defines the type");
                    }
                }
                Some(_) => {
                    if !self.prog.packages[pkg as usize].is_std {
                        self.err(r.ty.span, format!("cannot define methods on built-in type `{}`", self.prog.show(&ty)));
                    }
                }
                None => {
                    if !ty.is_error() {
                        self.err(r.ty.span, format!("invalid receiver type `{}`", self.prog.show(&ty)));
                    }
                }
            }
            recv = Some(FuncParam { name: r.name.clone(), mode: r.mode, ty, span: r.span });
        }
        for tp in &f.tparams {
            let id = self.new_tparam(&tp.name, tp.is_int);
            env.tparams.insert(tp.name.clone(), Ty::Param(id));
            tparams.push(id);
        }
        for tp in &f.tparams {
            let id = match env.tparams.get(&tp.name) {
                Some(Ty::Param(p)) => *p,
                _ => continue,
            };
            let bounds: Vec<Bound> = tp.bounds.iter().filter_map(|b| self.resolve_bound(b, &env)).collect();
            self.prog.tparams[id as usize].bounds.extend(bounds);
        }
        for (n, bs, sp) in &f.wheres {
            match env.tparams.get(n).cloned() {
                Some(Ty::Param(p)) => {
                    let bounds: Vec<Bound> = bs.iter().filter_map(|b| self.resolve_bound(b, &env)).collect();
                    self.prog.tparams[p as usize].bounds.extend(bounds);
                }
                _ => self.err(*sp, format!("`{}` is not a type parameter of this function", n)),
            }
        }
        let mut params = vec![];
        let mut seen = HashSet::new();
        if let Some(r) = &recv {
            seen.insert(r.name.clone());
        }
        for p in &f.params {
            if !seen.insert(p.name.clone()) && p.name != "_" {
                self.err(p.span, format!("duplicate parameter `{}`", p.name));
            }
            let ty = self.resolve_type(&p.ty, &env);
            if p.mode == Mode::Sink && self.prog.is_view(&ty) {
                self.err(p.ty.span, "`sink` cannot take a borrowed view; take an owned type (String / Vec[T]) instead");
            }
            params.push(FuncParam { name: p.name.clone(), mode: p.mode, ty, span: p.span });
        }
        let mut ret = match &f.ret {
            Some(t) => self.resolve_type(t, &env),
            None => Ty::Unit,
        };
        if matches!(&f.ret, Some(TypeExpr { kind: TypeExprKind::Named { path, .. }, .. }) if path.len() == 1 && path[0] == "never") {
            ret = Ty::Never;
        }
        if f.yields.is_some() && f.yield_opt {
            // stored as the element type; Proj with opt
        }
        if f.is_extern_c {
            for (p, ast_p) in params.iter().zip(&f.params) {
                if !self.ffi_safe(&p.ty, 0) {
                    let s = self.prog.show(&p.ty);
                    self.err(ast_p.ty.span, format!("`{}` has no C representation; `extern \"C\"` functions take numbers, `bool`, raw pointers and structs of those", s));
                }
            }
            if !self.ffi_safe(&ret, 0) {
                let s = self.prog.show(&ret);
                let sp = f.ret.as_ref().map(|t| t.span).unwrap_or(f.name_span);
                self.err(sp, format!("`{}` has no C representation; `extern \"C\"` functions return numbers, `bool`, raw pointers and structs of those", s));
            }
        }
        let ret_of = self.ret_of(f, recv.as_ref(), &params, &ret);
        // a generator yields `ret`; a `for` passes it the loop body, which it
        // calls once per value (spec 7.3)
        let mut params = params;
        let gen_of = if f.is_gen {
            if f.yields.is_some() || f.is_extern_c {
                self.err(f.name_span, "a `gen fn` yields values to a `for`: it can't be a projection or `extern`");
            }
            let flow = self.prog.prelude_type("Flow").map(|d| Ty::Adt(d, Rc::new(vec![]))).unwrap_or(Ty::Error);
            let elem = ret.clone();
            params.push(FuncParam { name: "__body".into(), mode: Mode::Borrow, ty: Ty::Func(Rc::new(FuncTy { params: vec![(Mode::Borrow, elem.clone())], ret: flow, send: false })), span: f.name_span });
            ret = Ty::Unit;
            Some(elem)
        } else {
            None
        };
        let id = self.prog.funcs.len() as FuncId;
        self.prog.funcs.push(FuncDef {
            name: f.name.clone(),
            pkg,
            file: self.files[fi].file_id,
            recv,
            tparams,
            params,
            ret,
            ret_of,
            yields: f.yields,
            yield_opt: f.yield_opt,
            is_pub,
            kind,
            span: f.name_span,
            ast: Some((fi, ii, ei)),
            recv_key,
            is_extern_c_export: f.is_extern_c && f.body.is_some(),
            is_unsafe: f.is_unsafe,
            owner_ty: owner_ty.clone(),
            gen_of,
            checkfree: f.checkfree,
        });
        if owner_ty.is_some() {
            let keys: Vec<TyKey> = if ext_keys.is_empty() { recv_key.into_iter().collect() } else { ext_keys };
            for k in keys {
                if self.prog.assoc.insert((k, f.name.clone()), id).is_some() || self.prog.methods.contains_key(&(k, f.name.clone())) {
                    self.err(f.name_span, format!("`{}` is declared twice for this type", f.name));
                }
            }
            return;
        }
        for &k in ext_keys.iter().skip(1) {
            self.prog.methods.entry((k, f.name.clone())).or_default().push(id);
        }
        match recv_key {
            Some(k) => {
                if self.prog.assoc.contains_key(&(k, f.name.clone())) {
                    self.err(f.name_span, format!("`{}` is declared twice for this type", f.name));
                }
                let list = self.prog.methods.entry((k, f.name.clone())).or_default();
                list.push(id);
                if let TyKey::Adt(d) = k {
                    if list.len() > 1 {
                        self.err(f.name_span, format!("method `{}` is declared twice for `{}`", f.name, self.prog.def(d).name));
                    }
                    let def = self.prog.def(d);
                    if def.fields().iter().any(|fl| fl.name == f.name) {
                        self.err(f.name_span, format!("`{}` has both a field and a method named `{}`", self.prog.def(d).name.clone(), f.name));
                    }
                }
            }
            None if f.recv.is_none() => {
                if self.prog.packages[pkg as usize].items.insert(f.name.clone(), Sym::Func(id)).is_some() {
                    self.err(f.name_span, format!("`{}` is declared twice", f.name));
                }
            }
            None => {}
        }
    }

    fn collect_recv_params(&self, te: &TypeExpr, env: &TyEnv, out: &mut Vec<String>, top: bool) {
        match &te.kind {
            TypeExprKind::Named { path, args } => {
                if !top && path.len() == 1 && args.is_empty() && self.lookup_type_name(&path[0], env).is_none() && !out.contains(&path[0]) {
                    out.push(path[0].clone());
                }
                for a in args {
                    self.collect_recv_params(a, env, out, false);
                }
            }
            TypeExprKind::Option(a) | TypeExprKind::Slice(a) | TypeExprKind::Ptr(a) | TypeExprKind::Chan(a) => self.collect_recv_params(a, env, out, false),
            TypeExprKind::Array(_, a) => self.collect_recv_params(a, env, out, false),
            TypeExprKind::Tuple(ts) => ts.iter().for_each(|t| self.collect_recv_params(t, env, out, false)),
            TypeExprKind::Result(a, b) => {
                self.collect_recv_params(a, env, out, false);
                self.collect_recv_params(b, env, out, false);
            }
            _ => {}
        }
    }

    fn lookup_type_name(&self, name: &str, env: &TyEnv) -> Option<()> {
        if env.tparams.contains_key(name) || builtin_type(name).is_some() || is_generic_builtin(name) {
            return Some(());
        }
        if self.prog.packages[env.pkg as usize].items.contains_key(name) || self.file_cx[env.file].names.contains_key(name) || self.prog.packages[self.prog.prelude as usize].items.contains_key(name) {
            return Some(());
        }
        None
    }

    /// Integer type parameters take integer arguments, other parameters take types.
    /// A parameter introduced by a receiver / `extend` type in an integer
    /// position becomes an integer parameter.
    pub fn check_targ_kinds(&mut self, d: DefId, targs: &[Ty], span: Span) {
        let tps = self.prog.def(d).tparams.clone();
        for (tp, a) in tps.iter().zip(targs.iter()) {
            let want_int = self.prog.tparam(*tp).is_int;
            match a {
                Ty::ConstInt(_) if !want_int => {
                    let n = self.prog.tparam(*tp).name.clone();
                    self.err(span, format!("`{}` is a type parameter; it takes a type, not a number", n));
                }
                Ty::Param(p) if want_int => self.prog.tparams[*p as usize].is_int = true,
                Ty::Param(_) | Ty::ConstInt(_) | Ty::Var(_) | Ty::Error => {}
                _ if want_int => {
                    let n = self.prog.tparam(*tp).name.clone();
                    self.err(span, format!("`{}` is an integer parameter; it takes a number like `3`", n));
                }
                _ => {}
            }
        }
    }

    fn copy_inferred(&self, d: DefId) -> bool {
        self.copy_candidates.contains(&d)
    }

    pub fn sym_is_pub(&self, sym: &Sym) -> bool {
        match sym {
            Sym::Type(d) => self.prog.def(*d).is_pub,
            Sym::Func(f) => self.prog.func(*f).is_pub,
            Sym::Const(c) => self.prog.consts[*c as usize].is_pub,
            Sym::Global(g) => self.prog.globals[*g as usize].is_pub,
            Sym::Alias(_) => true,
        }
    }

    /// An unqualified name: this package's items, then names from `use x.{...}`, then the prelude.
    pub fn lookup_unqualified(&mut self, fi: usize, pkg: PkgId, name: &str) -> Option<Sym> {
        if let Some(sym) = self.prog.packages[pkg as usize].items.get(name) {
            return Some(sym.clone());
        }
        if let Some((p, _)) = self.file_cx[fi].names.get(name).cloned() {
            if let Some(sym) = self.prog.packages[p as usize].items.get(name).cloned() {
                self.file_cx[fi].used_imports.insert(name.to_string());
                return Some(sym);
            }
        }
        self.prog.packages[self.prog.prelude as usize].items.get(name).cloned()
    }

    // ------------------------------------------------------------------
    // Type resolution
    // ------------------------------------------------------------------

    pub fn resolve_bound(&mut self, te: &TypeExpr, env: &TyEnv) -> Option<Bound> {
        if let TypeExprKind::Named { path, args } = &te.kind {
            if path.len() == 1 && args.is_empty() {
                if let Some(c) = Cap::from_name(&path[0]) {
                    return Some(Bound::Cap(c));
                }
            }
        }
        self.allow_typeset = true;
        match self.resolve_type(te, env) {
            Ty::Iface(d, args) => Some(Bound::Iface(d, args.to_vec())),
            Ty::Error => None,
            t => {
                let s = self.prog.show(&t);
                self.err(te.span, format!("`{}` is not a constraint; use an interface or a capability (copy, clone, ordered, ...)", s));
                None
            }
        }
    }

    pub fn resolve_type(&mut self, te: &TypeExpr, env: &TyEnv) -> Ty {
        let allow_typeset = std::mem::replace(&mut self.allow_typeset, false);
        let t = self.resolve_type_inner(te, env);
        if let Ty::Iface(d, _) = &t {
            if !allow_typeset {
                if let TypeDefKind::Iface { type_set: Some(_), .. } = &self.prog.def(*d).kind {
                    let n = self.prog.def(*d).name.clone();
                    self.err(te.span, format!("interface `{}` has a type set and can only be used as a constraint; use an enum for a value that is one of several types", n));
                    return Ty::Error;
                }
            }
        }
        t
    }

    fn resolve_type_inner(&mut self, te: &TypeExpr, env: &TyEnv) -> Ty {
        match &te.kind {
            TypeExprKind::Unit => Ty::Unit,
            TypeExprKind::IntLit(n) => match self.array_len((*n).min(i128::MAX as u128) as i128, *n, te.span) {
                Some(v) => Ty::ConstInt(v),
                None => Ty::Error,
            },
            TypeExprKind::Option(a) => {
                let t = self.resolve_type(a, env);
                if matches!(t, Ty::Option(_)) {
                    self.err(te.span, "nested optional `??T` is not allowed");
                }
                Ty::opt(t)
            }
            TypeExprKind::Result(a, b) => Ty::result(self.resolve_type(a, env), self.resolve_type(b, env)),
            TypeExprKind::Tuple(ts) => {
                let args: Vec<Ty> = ts.iter().map(|t| self.resolve_type(t, env)).collect();
                if args.iter().any(|a| self.prog.contains_view(&a)) {
                    self.err(te.span, "a tuple cannot hold a borrowed view; use an owned type (String / Vec[T])");
                }
                match self.tuple_def(args.len()) {
                    Some(d) => Ty::Adt(d, Rc::new(args)),
                    None => {
                        self.err(te.span, "tuples have 2 to 6 elements; use a struct for more");
                        Ty::Error
                    }
                }
            }
            TypeExprKind::Slice(a) => Ty::Slice(Rc::new(self.resolve_type(a, env))),
            TypeExprKind::Ptr(a) => Ty::Ptr(Rc::new(self.resolve_type(a, env))),
            // `chan T`: both ends, `Chan[T] { tx Sender[T], rx Receiver[T] }`
            TypeExprKind::Chan(a) => {
                let t = self.resolve_type(a, env);
                if self.prog.contains_view(&t) {
                    self.err(te.span, "channels cannot carry borrowed views; send owned values");
                }
                match self.prog.prelude_type("Chan") {
                    Some(d) => Ty::Adt(d, Rc::new(vec![t])),
                    None => Ty::Error,
                }
            }
            TypeExprKind::Array(n, a) => {
                let elem = self.resolve_type(a, env);
                match &n.kind {
                    ast::ExprKind::Int(v) => match self.array_len((*v).min(i128::MAX as u128) as i128, *v, n.span) {
                        Some(v) => Ty::Array(Rc::new(elem), v),
                        None => Ty::Error,
                    },
                    ast::ExprKind::Ident(name) => match env.tparams.get(name) {
                        Some(Ty::Param(p)) => {
                            let p = *p;
                            if !self.prog.tparam(p).is_int {
                                self.err(n.span, format!("`{}` is a type parameter; an array length must be an integer parameter like `{} int`", name, name));
                            }
                            Ty::ArrayT(Rc::new(elem), Rc::new(Ty::Param(p)))
                        }
                        _ => match self.lookup_const_int(name, env) {
                            Some(v) => match self.array_len(v, v.max(0) as u128, n.span) {
                                Some(v) => Ty::Array(Rc::new(elem), v),
                                None => Ty::Error,
                            },
                            None => {
                                self.err(n.span, "array length must be an integer constant");
                                Ty::Error
                            }
                        },
                    },
                    _ => {
                        self.err(n.span, "array length must be an integer constant");
                        Ty::Error
                    }
                }
            }
            TypeExprKind::Func { params, ret, send } => {
                let ps = params.iter().map(|(m, t)| (*m, self.resolve_type(t, env))).collect();
                let r = ret.as_ref().map(|t| self.resolve_type(t, env)).unwrap_or(Ty::Unit);
                Ty::Func(Rc::new(FuncTy { params: ps, ret: r, send: *send }))
            }
            TypeExprKind::Named { path, args } => self.resolve_named(path, args, te.span, env),
        }
    }

    fn lookup_const_int(&self, name: &str, env: &TyEnv) -> Option<i128> {
        let sel = self.file_cx[env.file].names.get(name).map(|x| x.0);
        for p in [Some(env.pkg), sel, Some(self.prog.prelude)].into_iter().flatten() {
            if let Some(v) = self.const_int_ast(p, name, 0) {
                return Some(v);
            }
        }
        None
    }

    fn resolve_alias(&mut self, fi: usize, ii: usize, by_name: &HashMap<(PkgId, String), (usize, usize)>, state: &mut HashMap<(usize, usize), u8>) {
        match state.get(&(fi, ii)) {
            Some(2) => return,
            Some(1) => {
                let item = &self.files[fi].items[ii];
                if let ast::ItemKind::TypeAlias { name, .. } = &item.kind {
                    let (name, sp) = (name.clone(), item.span);
                    self.diags.push(Diag::new(sp, format!("type alias `{}` refers to itself", name)).note(None, "an alias is another name for an existing type; a recursive type needs a struct or an enum"));
                    let pkg = self.file_cx[fi].pkg;
                    self.prog.packages[pkg as usize].items.insert(name, Sym::Alias(Ty::Error));
                }
                return;
            }
            _ => {}
        }
        state.insert((fi, ii), 1);
        let ast::ItemKind::TypeAlias { name, ty, .. } = self.files[fi].items[ii].kind.clone() else { return };
        let pkg = self.file_cx[fi].pkg;
        // the aliases it mentions first
        fn names(t: &TypeExpr, out: &mut Vec<String>) {
            match &t.kind {
                TypeExprKind::Named { path, args } => {
                    if path.len() == 1 {
                        out.push(path[0].clone());
                    }
                    args.iter().for_each(|a| names(a, out));
                }
                TypeExprKind::Option(a) | TypeExprKind::Slice(a) | TypeExprKind::Ptr(a) | TypeExprKind::Chan(a) | TypeExprKind::Array(_, a) => names(a, out),
                TypeExprKind::Result(a, b) => {
                    names(a, out);
                    names(b, out);
                }
                TypeExprKind::Tuple(ts) => ts.iter().for_each(|t| names(t, out)),
                TypeExprKind::Func { params, ret, .. } => {
                    params.iter().for_each(|p| names(&p.1, out));
                    if let Some(r) = ret {
                        names(r, out);
                    }
                }
                TypeExprKind::Unit | TypeExprKind::IntLit(_) => {}
            }
        }
        let mut deps = vec![];
        names(&ty, &mut deps);
        for d in deps {
            if let Some(&(dfi, dii)) = by_name.get(&(pkg, d)) {
                self.resolve_alias(dfi, dii, by_name, state);
            }
        }
        if state.get(&(fi, ii)) == Some(&1) {
            let env = self.file_env(fi);
            let t = self.resolve_type(&ty, &env);
            // a cycle through this alias was reported (and the alias set) already
            if !matches!(self.prog.packages[pkg as usize].items.get(&name), Some(Sym::Alias(Ty::Error))) {
                self.prog.packages[pkg as usize].items.insert(name, Sym::Alias(t));
            }
        }
        state.insert((fi, ii), 2);
    }

    /// Methods that replace built-in behavior must have exactly the signature
    /// the compiler calls them with: `equal` (`==`), `compare` (ordering),
    /// `hash` (map keys), `clone` and `drop`.
    fn check_protocol_methods(&mut self, d: DefId) {
        let def = self.prog.def(d).clone();
        if matches!(def.kind, TypeDefKind::Iface { .. }) {
            return;
        }
        let me = Ty::Adt(d, Rc::new(def.tparams.iter().map(|p| Ty::Param(*p)).collect()));
        let tname = def.name.clone();
        let hasher = self.prog.prelude_type("Hasher").map(|h| Ty::Adt(h, Rc::new(vec![])));
        for (name, recv, params, ret, what) in [
            ("equal", Mode::Borrow, vec![(Mode::Borrow, me.clone())], Some(Ty::Bool), "`==`"),
            ("compare", Mode::Borrow, vec![(Mode::Borrow, me.clone())], self.prog.prelude_type("Ordering").map(|o| Ty::Adt(o, Rc::new(vec![]))), "ordering"),
            ("hash", Mode::Borrow, hasher.iter().map(|h| (Mode::Inout, h.clone())).collect(), Some(Ty::Unit), "hashing (map keys)"),
            ("clone", Mode::Borrow, vec![], Some(me.clone()), "`.clone()`"),
            ("drop", Mode::Inout, vec![], Some(Ty::Unit), "dropping"),
        ] {
            let Some(fs) = self.prog.methods.get(&(TyKey::Adt(d), name.to_string())).cloned() else { continue };
            let Some(ret) = ret else { continue };
            for f in fs {
                let fd = self.prog.func(f).clone();
                if self.prog.packages[fd.pkg as usize].is_std {
                    continue;
                }
                let Some(r) = &fd.recv else { continue };
                // the method's own type parameters come after the type's
                let ok = r.mode == recv
                    && fd.tparams.len() == def.tparams.len()
                    && fd.params.len() == params.len()
                    && fd.params.iter().zip(&params).all(|(p, (m, t))| p.mode == *m && p.ty == *t)
                    && fd.ret == ret;
                if !ok {
                    let self_part = match recv {
                        Mode::Inout => "inout self",
                        _ => "self",
                    };
                    let ps: Vec<String> = params.iter().map(|(m, t)| format!("{}{}{}", if name == "hash" { "h: " } else { "other: " }, if *m == Mode::Inout { "inout " } else { "" }, self.prog.show(t))).collect();
                    let sig = format!("fn {}({}{}{}){}", name, self_part, if ps.is_empty() { "" } else { ", " }, ps.join(", "), if ret == Ty::Unit { String::new() } else { format!(" -> {}", self.prog.show(&ret)) });
                    self.diags.push(Diag::new(fd.span, format!("`{}` on `{}` must be `{}`", name, tname, sig)).note(None, format!("a method named `{}` defines {} for the type; give a method with another meaning a different name", name, what)));
                }
            }
        }
    }

    /// Checks an array length (or integer type argument). `big` is the literal's
    /// full value, which may not fit in `v`.
    fn array_len(&mut self, v: i128, big: u128, sp: Span) -> Option<u64> {
        if v < 0 {
            self.err(sp, format!("array length must not be negative (it is {})", v));
            return None;
        }
        if big > MAX_ARRAY_LEN as u128 || v > MAX_ARRAY_LEN as i128 {
            self.err(sp, format!("array length {} is too large: arrays hold at most {} elements; use a `Vec` for more", big, MAX_ARRAY_LEN));
            return None;
        }
        Some(v as u64)
    }

    /// Evaluate an integer constant from its source (array lengths are needed
    /// before constants are type-checked).
    fn const_int_ast(&self, pkg: PkgId, name: &str, depth: u32) -> Option<i128> {
        if depth > 32 {
            return None;
        }
        for (fi, f) in self.files.iter().enumerate() {
            if self.file_cx[fi].pkg != pkg {
                continue;
            }
            for item in &f.items {
                if let ast::ItemKind::Const { name: n, value, .. } = &item.kind {
                    if n == name {
                        return self.eval_int(pkg, value, depth + 1);
                    }
                }
            }
        }
        None
    }

    fn eval_int(&self, pkg: PkgId, e: &ast::Expr, depth: u32) -> Option<i128> {
        use ast::ExprKind as E;
        // checked: values this big are rejected as array lengths anyway
        Some(match &e.kind {
            E::Int(v) => i128::try_from(*v).ok()?,
            E::Ident(n) => self.const_int_ast(pkg, n, depth)?,
            E::Unary("-", x) => self.eval_int(pkg, x, depth)?.checked_neg()?,
            E::Binary(op, a, b) => {
                let (x, y) = (self.eval_int(pkg, a, depth)?, self.eval_int(pkg, b, depth)?);
                match *op {
                    "+" => x.checked_add(y)?,
                    "-" => x.checked_sub(y)?,
                    "*" => x.checked_mul(y)?,
                    "/" => x.checked_div(y)?,
                    "%" => x.checked_rem(y)?,
                    "<<" if (0..64).contains(&y) => x.checked_mul(1i128 << y)?,
                    _ => return None,
                }
            }
            _ => return None,
        })
    }


    fn resolve_named(&mut self, path: &[String], args: &[TypeExpr], span: Span, env: &TyEnv) -> Ty {
        let targs: Vec<Ty> = args.iter().map(|a| self.resolve_type(a, env)).collect();
        if path.len() == 1 {
            let name = &path[0];
            if let Some(t) = env.tparams.get(name) {
                if !args.is_empty() {
                    self.err(span, "type parameters cannot take type arguments");
                }
                return t.clone();
            }
            if let Some(t) = builtin_type(name) {
                if !args.is_empty() {
                    self.err(span, format!("`{}` does not take type arguments", name));
                }
                return t;
            }
            if is_generic_builtin(name) {
                let need = if name == "Map" { 2 } else { 1 };
                if targs.len() != need {
                    self.err(span, format!("`{}` expects {} type argument{}", name, need, if need == 1 { "" } else { "s" }));
                    return Ty::Error;
                }
                let a = Rc::new(targs[0].clone());
                // `Vec[str]`, `Map[str, int]`: view collections, which live
                // in variables like the views they hold
                let coll = match name.as_str() {
                    "Vec" => self.prog.is_view(&Ty::Vec(a.clone())),
                    "Map" => targs.len() == 2 && self.prog.is_view(&Ty::Map(a.clone(), Rc::new(targs[1].clone()))),
                    _ => false,
                };
                if self.prog.contains_view(&a) && !coll {
                    self.err(span, format!("`{}` cannot hold the borrowed view `{}`; use an owned type", name, self.prog.show(&a)));
                }
                return match name.as_str() {
                    "Vec" => Ty::Vec(a),
                    "Map" => {
                        let v = Rc::new(targs[1].clone());
                        if self.prog.contains_view(&v) && !coll {
                            self.err(span, "`Map` values cannot be borrowed views");
                        }
                        Ty::Map(a, v)
                    }
                    "Box" => Ty::Box(a),
                    "Rc" => Ty::Rc(a),
                    "Arc" => Ty::Arc(a),
                    "Weak" => Ty::Weak(a),
                    "Cell" => Ty::Cell(a),
                    "Mutex" => Ty::Mutex(a),
                    "MutexRef" => Ty::MutexRef(a),
                    "Task" => Ty::Task(a),
                    "Sender" => Ty::Sender(a),
                    "Receiver" => Ty::Receiver(a),
                    "Iso" => Ty::Iso(a),
                    _ => unreachable!(),
                };
            }
            if let Some(sym) = self.lookup_unqualified(env.file, env.pkg, name) {
                return self.type_from_sym(sym, name, targs, span);
            }
            let mut d = Diag::new(span, format!("unknown type `{}`", name));
            if let Some(h) = renamed(name) {
                d = d.note(None, h);
                if matches!(&name[..], "rc" | "arc" | "weak") {
                    d = d.fix(Span { file: span.file, lo: span.lo, hi: span.lo + name.len() as u32 }, format!("{}{}", name[..1].to_uppercase(), &name[1..]));
                }
            }
            self.diags.push(d);
            return Ty::Error;
        }
        if path.len() == 2 {
            if let Some((p, _)) = self.file_cx[env.file].imports.get(&path[0]).cloned() {
                self.file_cx[env.file].used_imports.insert(path[0].clone());
                match self.prog.packages[p as usize].items.get(&path[1]).cloned() {
                    Some(sym) => {
                        if let Sym::Type(d) = &sym {
                            if !self.prog.def(*d).is_pub {
                                self.err(span, format!("type `{}` is not public in package `{}`", path[1], path[0]));
                            }
                        }
                        return self.type_from_sym(sym, &path[1], targs, span);
                    }
                    None => {
                        self.err(span, format!("package `{}` has no type `{}`", path[0], path[1]));
                        return Ty::Error;
                    }
                }
            }
            self.err(span, format!("unknown package `{}`", path[0]));
            return Ty::Error;
        }
        self.err(span, "invalid type path");
        Ty::Error
    }

    fn type_from_sym(&mut self, sym: Sym, name: &str, targs: Vec<Ty>, span: Span) -> Ty {
        match sym {
            Sym::Type(d) => {
                self.check_targ_kinds(d, &targs, span);
                if targs.iter().any(|t| self.prog.contains_view(&t)) && !matches!(self.prog.def(d).kind, TypeDefKind::Iface { .. }) {
                    self.err(span, format!("`{}` cannot be instantiated with a borrowed view type; use an owned type (String / Vec[T])", name));
                }
                let def = self.prog.def(d);
                if def.tparams.len() != targs.len() {
                    if targs.is_empty() {
                        // generic type without args: resolved as fresh params by caller context
                        self.err(span, format!("`{}` expects {} type argument(s)", name, def.tparams.len()));
                    } else {
                        self.err(span, format!("`{}` expects {} type argument(s), found {}", name, def.tparams.len(), targs.len()));
                    }
                    return Ty::Error;
                }
                match def.kind {
                    TypeDefKind::Iface { .. } => Ty::Iface(d, Rc::new(targs)),
                    _ => Ty::Adt(d, Rc::new(targs)),
                }
            }
            Sym::Alias(t) => t,
            _ => {
                self.err(span, format!("`{}` is not a type", name));
                Ty::Error
            }
        }
    }

    // ------------------------------------------------------------------
    // Capabilities
    // ------------------------------------------------------------------

    pub fn has_bound(&self, p: TParamId, cap: Cap) -> bool {
        let info = self.prog.tparam(p);
        info.bounds.iter().any(|b| match b {
            Bound::Cap(c) => {
                *c == cap
                    || (cap == Cap::Clone && *c == Cap::Copy)
                    || (cap == Cap::Comparable && matches!(c, Cap::Ordered | Cap::Hash))
            }
            Bound::Iface(d, _) => {
                // interfaces with type sets of numbers/strings imply ordered/comparable/copy
                if let TypeDefKind::Iface { type_set: Some(ts), .. } = &self.prog.def(*d).kind {
                    ts.iter().all(|(_, t)| match cap {
                        Cap::Copy | Cap::Clone => self.is_copy(t),
                        Cap::Comparable => self.is_comparable(t),
                        Cap::Ordered => self.is_ordered(t),
                        Cap::Hash => self.is_hash(t),
                        Cap::Default => self.is_default(t),
                        Cap::Send => true,
                        Cap::Any => true,
                    })
                } else {
                    // a `send interface` bound makes the type parameter `send`
                    cap == Cap::Send && self.prog.def(*d).unsafe_send
                }
            }
        })
    }

    pub fn is_copy(&self, t: &Ty) -> bool {
        match t {
            Ty::Unit | Ty::Never | Ty::Bool | Ty::Int(_) | Ty::F32 | Ty::F64 | Ty::Str | Ty::Slice(_) | Ty::Ptr(_) | Ty::Error | Ty::ConstInt(_) | Ty::Scope => true,
            Ty::Option(a) | Ty::Array(a, _) | Ty::ArrayT(a, _) => self.is_copy(a),
            Ty::Result(a, b) => self.is_copy(a) && self.is_copy(b),
            Ty::Adt(d, args) => self.prog.def(*d).is_copy || (self.prog.def(*d).tuple && args.iter().all(|a| self.is_copy(a))),
            Ty::Param(p) => self.has_bound(*p, Cap::Copy),
            _ => false,
        }
    }

    /// Can this type cross a C call as it is?
    /// A field or enum payload `name` of type `ty` in type `owner`. Only a
    /// `view` type may hold views, and only views that can sit in a variable
    /// (not task handles, scopes or lock guards, which their blocks track).
    fn check_field_view(&mut self, owner: DefId, ty: &Ty, sp: Span, name: &str) {
        if !self.prog.is_view(ty) {
            return;
        }
        let def = self.prog.def(owner);
        let (tn, view, kind) = (def.name.clone(), def.view, if def.is_struct() { "struct" } else { "enum" });
        let shown = self.prog.show(ty);
        if !view {
            let what = if kind == "struct" { format!("field `{}`", name) } else { format!("payload `{}`", name) };
            self.diags.push(
                Diag::new(sp, format!("{} has borrowed view type `{}`; fields must own their data", what, shown))
                    .note(None, format!("store an owned `String` / `Vec[T]`, or make `{}` a view itself (`view {} {}`), which then lives only in variables, like `str`", tn, kind, tn)),
            );
            return;
        }
        if self.prog.reaches(ty, &|t| matches!(t, Ty::Task(_) | Ty::Scope | Ty::MutexRef(_))) {
            self.err(sp, format!("`{}` can't hold `{}`: task handles, scopes and lock guards live only in the variables their blocks track", tn, shown));
        }
    }

    /// `-> str of a, b`: the parameters (receiver first, as indices) the
    /// returned view may point into. Checked against the body by the borrow
    /// checker; callers rely on it.
    fn ret_of(&mut self, f: &ast::FuncDecl, recv: Option<&FuncParam>, params: &[FuncParam], ret: &Ty) -> Option<Vec<usize>> {
        if f.ret_of.is_empty() {
            return None;
        }
        let sp = f.ret_of[0].1;
        if !self.prog.is_view(&ret) {
            let s = self.prog.show(ret);
            self.err(sp, format!("`of` says what a returned view points into, but `{}` is not a view", s));
            return None;
        }
        let all: Vec<&FuncParam> = recv.into_iter().chain(params.iter()).collect();
        let mut out = vec![];
        for (n, nsp) in &f.ret_of {
            let Some(i) = all.iter().position(|p| &p.name == n) else {
                self.err(*nsp, format!("`{}` is not a parameter of `{}`", n, f.name));
                continue;
            };
            let p = all[i];
            if p.mode == Mode::Sink {
                self.err(*nsp, format!("`{}` is a `sink` parameter: it is dropped when the function returns, so the returned view can't point into it", n));
                continue;
            }
            if matches!(p.ty, Ty::Bool | Ty::Int(_) | Ty::F32 | Ty::F64 | Ty::Unit) {
                let s = self.prog.show(&p.ty);
                self.err(*nsp, format!("`{}` is a `{}`, passed by value: a view can't point into it", n, s));
                continue;
            }
            if !out.contains(&i) {
                out.push(i);
            }
        }
        out.sort_unstable();
        Some(out)
    }

    fn ffi_safe(&self, t: &Ty, depth: u32) -> bool {
        match t {
            Ty::Unit | Ty::Never | Ty::Bool | Ty::Int(_) | Ty::F32 | Ty::F64 | Ty::Ptr(_) | Ty::Error => true,
            // fields may not be resolved yet for recursive types; those are rejected elsewhere
            Ty::Adt(d, args) if self.prog.def(*d).is_struct() && depth < 32 => self.prog.field_tys(*d, args).iter().all(|f| self.ffi_safe(&f.1, depth + 1)),
            _ => false,
        }
    }

    /// The underlying type of a new type (`type Celsius f64`), with type arguments applied.
    pub fn newtype_inner(&self, d: DefId, args: &[Ty]) -> Option<Ty> {
        if !self.prog.def(d).newtype {
            return None;
        }
        self.prog.field_tys(d, args).into_iter().next().map(|f| f.1)
    }

    /// A new type over a number: supports the same operators as the number.
    pub fn numeric_newtype(&self, t: &Ty) -> Option<Ty> {
        match t {
            Ty::Adt(d, args) => self.newtype_inner(*d, args).filter(|i| i.is_numeric()),
            _ => None,
        }
    }

    pub fn is_comparable(&self, t: &Ty) -> bool {
        match t {
            Ty::Unit | Ty::Bool | Ty::Int(_) | Ty::F32 | Ty::F64 | Ty::Str | Ty::String | Ty::Error | Ty::Ptr(_) => true,
            Ty::Option(a) | Ty::Vec(a) | Ty::Slice(a) | Ty::Array(a, _) | Ty::ArrayT(a, _) | Ty::Box(a) => self.is_comparable(a),
            Ty::Result(a, b) => self.is_comparable(a) && self.is_comparable(b),
            Ty::Adt(d, args) => {
                if self.prog.methods.contains_key(&(TyKey::Adt(*d), "equal".into())) {
                    return true;
                }
                self.all_fields(*d, args, "comparable", &|c, t| c.is_comparable(t))
            }
            Ty::Param(p) => self.has_bound(*p, Cap::Comparable),
            _ => false,
        }
    }

    pub fn is_ordered(&self, t: &Ty) -> bool {
        match t {
            Ty::Int(_) | Ty::F32 | Ty::F64 | Ty::Str | Ty::String | Ty::Error | Ty::Bool => true,
            Ty::Adt(d, args) => {
                if let Some(inner) = self.newtype_inner(*d, args) {
                    return self.prog.methods.contains_key(&(TyKey::Adt(*d), "compare".into())) || self.is_ordered(&inner);
                }
                self.prog.methods.contains_key(&(TyKey::Adt(*d), "compare".into())) || self.prog.def(*d).is_simple_enum()
            }
            Ty::Param(p) => self.has_bound(*p, Cap::Ordered),
            _ => false,
        }
    }

    pub fn is_hash(&self, t: &Ty) -> bool {
        match t {
            Ty::Unit | Ty::Bool | Ty::Int(_) | Ty::Str | Ty::String | Ty::Error => true,
            Ty::Option(a) | Ty::Vec(a) | Ty::Slice(a) | Ty::Array(a, _) | Ty::ArrayT(a, _) | Ty::Box(a) => self.is_hash(a),
            Ty::Adt(d, args) => {
                if self.prog.methods.contains_key(&(TyKey::Adt(*d), "hash".into())) {
                    return true;
                }
                if self.prog.methods.contains_key(&(TyKey::Adt(*d), "equal".into())) {
                    // custom equality without custom hash would break the map invariant
                    return false;
                }
                self.all_fields(*d, args, "hash", &|c, t| c.is_hash(t))
            }
            Ty::Param(p) => self.has_bound(*p, Cap::Hash),
            _ => false,
        }
    }

    pub fn is_clone(&self, t: &Ty) -> bool {
        if self.is_copy(t) {
            return true;
        }
        match t {
            Ty::String | Ty::Rc(_) | Ty::Arc(_) | Ty::Weak(_) | Ty::Sender(_) | Ty::Receiver(_) => true,
            Ty::Vec(a) | Ty::Option(a) | Ty::Box(a) | Ty::Cell(a) => self.is_clone(a),
            Ty::Map(a, b) | Ty::Result(a, b) => self.is_clone(a) && self.is_clone(b),
            Ty::Adt(d, args) => {
                if self.prog.methods.contains_key(&(TyKey::Adt(*d), "clone".into())) {
                    return true;
                }
                // a linear value is consumed once: copies only where its
                // own `clone` method says how
                if self.prog.def(*d).has_drop || self.prog.def(*d).linear {
                    return false;
                }
                self.all_fields(*d, args, "clone", &|c, t| c.is_clone(t))
            }
            Ty::Param(p) => self.has_bound(*p, Cap::Clone),
            _ => false,
        }
    }

    /// Default values: scalars, String, Vec, Map, ?T, and structs whose fields all have defaults.
    pub fn is_default(&self, t: &Ty) -> bool {
        match t {
            Ty::Unit | Ty::Bool | Ty::Int(_) | Ty::F32 | Ty::F64 | Ty::String | Ty::Vec(_) | Ty::Map(..) | Ty::Option(_) | Ty::Error | Ty::Str => true,
            Ty::Array(a, _) | Ty::ArrayT(a, _) | Ty::Mutex(a) | Ty::Cell(a) => self.is_default(a),
            Ty::Adt(d, args) => {
                if let Some(inner) = self.newtype_inner(*d, args) {
                    return self.is_default(&inner);
                }
                let def = self.prog.def(*d);
                match &def.kind {
                    TypeDefKind::Struct { fields } => fields.iter().all(|f| f.default.is_some() || matches!(f.ty, Ty::Option(_))),
                    _ => false,
                }
            }
            Ty::Param(p) => self.has_bound(*p, Cap::Default),
            _ => false,
        }
    }

    pub fn is_send(&mut self, t: &Ty) -> bool {
        match t {
            Ty::Func(f) => f.send,
            Ty::Rc(_) | Ty::Weak(_) | Ty::Cell(_) | Ty::Ptr(_) => false,
            Ty::Vec(a) | Ty::Slice(a) | Ty::Array(a, _) | Ty::ArrayT(a, _) | Ty::Option(a) | Ty::Box(a) | Ty::Arc(a) | Ty::Mutex(a) | Ty::MutexRef(a) | Ty::Sender(a) | Ty::Receiver(a) | Ty::Task(a) => self.is_send(a),
            Ty::Map(a, b) | Ty::Result(a, b) => self.is_send(a) && self.is_send(b),
            Ty::Adt(d, args) => {
                let d = *d;
                // `unsafe send`: the author vouches for the type's own fields
                // (its raw pointers), not for whatever its type parameters are
                // given: `Wrap[Rc[int]]` is no more `send` than `Rc[int]`
                if self.prog.def(d).unsafe_send {
                    return args.clone().iter().all(|a| self.is_send(a));
                }
                if args.is_empty() {
                    if let Some(&v) = self.send_memo.get(&d) {
                        return v;
                    }
                }
                // A type already being checked (it contains itself) counts as
                // `send` while its other fields decide. Types whose arguments grow
                // at every level never end and are rejected when instantiated, so
                // giving up on them here is safe.
                let key = (d, args.to_vec(), "send");
                if self.cap_stack.borrow().contains(&key) || self.cap_stack.borrow().len() > MAX_CAP_DEPTH {
                    return true;
                }
                if let Some(&r) = self.cap_cache.borrow().get(&key) {
                    return r;
                }
                self.cap_stack.borrow_mut().push(key.clone());
                let def = self.prog.def(d).clone();
                let s = self.prog.def_subst(d, args);
                let tys: Vec<Ty> = match &def.kind {
                    TypeDefKind::Struct { fields } => fields.iter().map(|f| f.ty.subst(&s)).collect(),
                    TypeDefKind::Enum { variants } => variants.iter().flat_map(|v| v.fields.iter().map(|f| f.1.subst(&s))).collect(),
                    _ => vec![],
                };
                let r = tys.iter().all(|t| self.is_send(t));
                self.cap_done(key, r);
                // valid under the assumption above (the greatest fixed point)
                if args.is_empty() {
                    self.send_memo.insert(d, r);
                }
                r
            }
            Ty::Iface(d, _) => self.prog.def(*d).unsafe_send,
            Ty::Iso(_) => true,
            Ty::Param(p) => self.has_bound(*p, Cap::Send),
            _ => true,
        }
    }

    /// Does every field (of every variant) satisfy `f`? A type that is already
    /// being checked counts as satisfying it, so recursive types terminate.
    fn all_fields(&self, d: DefId, args: &[Ty], cap: &'static str, f: &dyn Fn(&Checker, &Ty) -> bool) -> bool {
        let key = (d, args.to_vec(), cap);
        // (see `is_send` for the depth limit)
        if self.cap_stack.borrow().contains(&key) || self.cap_stack.borrow().len() > MAX_CAP_DEPTH {
            return true;
        }
        if let Some(&r) = self.cap_cache.borrow().get(&key) {
            return r;
        }
        self.cap_stack.borrow_mut().push(key.clone());
        let def = self.prog.def(d);
        let s = self.prog.def_subst(d, args);
        let r = match &def.kind {
            TypeDefKind::Struct { fields } => fields.iter().all(|fl| f(self, &fl.ty.subst(&s))),
            TypeDefKind::Enum { variants } => variants.iter().all(|v| v.fields.iter().all(|(_, t)| f(self, &t.subst(&s)))),
            _ => false,
        };
        self.cap_done(key, r);
        r
    }

    /// Ends the check of `key` (on top of `cap_stack`) with answer `r`.
    fn cap_done(&self, key: (DefId, Vec<Ty>, &'static str), r: bool) {
        let mut st = self.cap_stack.borrow_mut();
        st.pop();
        if st.is_empty() {
            self.cap_cache.borrow_mut().clear();
        } else {
            self.cap_cache.borrow_mut().insert(key, r);
        }
    }

    pub fn satisfies(&mut self, t: &Ty, b: &Bound) -> Result<(), String> {
        match b {
            Bound::Cap(c) => {
                let ok = match c {
                    Cap::Any => true,
                    Cap::Copy => self.is_copy(t),
                    Cap::Clone => self.is_clone(t),
                    Cap::Default => self.is_default(t),
                    Cap::Send => self.is_send(t),
                    Cap::Comparable => self.is_comparable(t),
                    Cap::Ordered => self.is_ordered(t),
                    Cap::Hash => self.is_hash(t),
                };
                if ok {
                    Ok(())
                } else {
                    Err(format!("`{}` does not satisfy `{}`", self.prog.show(t), c.name()))
                }
            }
            Bound::Iface(d, args) => {
                if let Ty::Param(p) = t {
                    let info = self.prog.tparam(*p).clone();
                    if info.bounds.iter().any(|b2| matches!(b2, Bound::Iface(d2, _) if d2 == d)) {
                        return Ok(());
                    }
                }
                let def = self.prog.def(*d).clone();
                if let TypeDefKind::Iface { type_set: Some(ts), .. } = &def.kind {
                    let ok = ts.iter().any(|(approx, et)| {
                        if et == t {
                            return true;
                        }
                        if *approx {
                            if let Ty::Adt(td, _) = t {
                                let tdef = self.prog.def(*td);
                                if tdef.newtype && tdef.fields().first().map(|f| &f.ty) == Some(et) {
                                    return true;
                                }
                            }
                        }
                        false
                    });
                    if !ok {
                        return Err(format!("`{}` is not in the type set of `{}`", self.prog.show(t), def.name));
                    }
                }
                self.implements(t, *d, args)
            }
        }
    }

    /// Structural interface satisfaction.
    pub fn implements(&mut self, t: &Ty, iface: DefId, iargs: &[Ty]) -> Result<(), String> {
        let def = self.prog.def(iface).clone();
        let methods = match &def.kind {
            TypeDefKind::Iface { methods, .. } => methods.clone(),
            _ => return Err("not an interface".into()),
        };
        let s = self.prog.def_subst(iface, iargs);
        if def.unsafe_send && !self.is_send(t) {
            return Err(format!("`{}` can't be a `{}`: it is a `send interface`, and `{}` can't be sent between tasks", self.prog.show(t), def.name, self.prog.show(t)));
        }
        for m in &methods {
            let found = self.find_method_for_iface(t, &m.name);
            let fid = match found {
                Some(f) => f,
                None => return Err(format!("`{}` does not implement `{}`: missing method `{}`", self.prog.show(t), def.name, m.name)),
            };
            let fd = self.prog.func(fid).clone();
            if fd.is_unsafe {
                return Err(format!("method `{}` of `{}` is an `unsafe fn`, so it can't satisfy `{}`: code calling it through the interface couldn't know to uphold its conditions", m.name, self.prog.show(t), def.name));
            }
            if !fd.tparams.is_empty() && fd.recv.as_ref().map(|r| !r.ty.has_params()).unwrap_or(true) {
                return Err(format!("method `{}` has type parameters and cannot satisfy an interface", m.name));
            }
            let recv_mode = fd.recv.as_ref().map(|r| r.mode).unwrap_or(Mode::Borrow);
            let mode_name = |m: Mode| match m {
                Mode::Borrow => "borrowed",
                Mode::Inout => "inout",
                Mode::Sink => "sink",
            };
            let article = |m: Mode| if m == Mode::Inout { "an" } else { "a" };
            // a method that only reads its receiver can stand in for one that may change it
            if recv_mode != m.recv_mode && !(recv_mode == Mode::Borrow && m.recv_mode == Mode::Inout) {
                return Err(format!("method `{}` of `{}` has {} {} receiver, but `{}` requires {}", m.name, self.prog.show(t), article(recv_mode), mode_name(recv_mode), def.name, mode_name(m.recv_mode)));
            }
            // substitute receiver params from the concrete type
            let mut rs = HashMap::new();
            if let Some(r) = &fd.recv {
                bind_params(&r.ty, t, &mut rs);
            }
            if fd.params.len() != m.params.len() {
                return Err(format!("method `{}` has {} parameter(s), interface `{}` expects {}", m.name, fd.params.len(), def.name, m.params.len()));
            }
            for (fp, (_, im, it)) in fd.params.iter().zip(m.params.iter()) {
                if fp.mode != *im || fp.ty.subst(&rs) != it.subst(&s) {
                    return Err(format!("method `{}` has a different signature than interface `{}` requires", m.name, def.name));
                }
            }
            if fd.ret.subst(&rs) != m.ret.subst(&s) {
                return Err(format!("method `{}` returns `{}`, interface `{}` requires `{}`", m.name, self.prog.show(&fd.ret.subst(&rs)), def.name, self.prog.show(&m.ret.subst(&s))));
            }
        }
        Ok(())
    }

    pub fn find_method_for_iface(&self, t: &Ty, name: &str) -> Option<FuncId> {
        let key = TyKey::of(t)?;
        let list = self.prog.methods.get(&(key, name.to_string()))?;
        for &f in list {
            let fd = self.prog.func(f);
            if let Some(r) = &fd.recv {
                let mut s = HashMap::new();
                if bind_params(&r.ty, t, &mut s) {
                    return Some(f);
                }
            }
        }
        None
    }

    fn check_unused_imports(&mut self) {
        for fi in 0..self.file_cx.len() {
            let pkg = self.file_cx[fi].pkg;
            if self.prog.packages[pkg as usize].is_std {
                continue;
            }
            let mut unused: Vec<(String, Span)> = vec![];
            for (name, (_, sp)) in self.file_cx[fi].imports.iter().chain(self.file_cx[fi].names.iter()) {
                if !self.file_cx[fi].used_imports.contains(name) {
                    unused.push((name.clone(), *sp));
                }
            }
            unused.sort_by_key(|u| u.1.lo);
            for (n, sp) in unused {
                self.err(sp, format!("`{}` is imported but not used", n));
            }
        }
    }
}

/// Methods the compiler calls by itself: protocol methods (`==`, ordering,
/// hashing, `.clone()`, drops), how `print` shows a value, and the iteration
/// protocols behind `for`.
pub const IMPLICIT_METHODS: &[&str] = &["equal", "compare", "hash", "clone", "drop", "toString", "message", "cursor", "next", "nextMut", "each", "eachMut"];

/// Match a pattern type containing params against a concrete type, binding params.
pub fn bind_params(pat: &Ty, t: &Ty, s: &mut HashMap<TParamId, Ty>) -> bool {
    match (pat, t) {
        (Ty::Param(p), _) => {
            if let Some(prev) = s.get(p) {
                return prev == t;
            }
            s.insert(*p, t.clone());
            true
        }
        (Ty::Vec(a), Ty::Vec(b)) | (Ty::Slice(a), Ty::Slice(b)) | (Ty::Option(a), Ty::Option(b)) | (Ty::Box(a), Ty::Box(b)) | (Ty::Rc(a), Ty::Rc(b)) | (Ty::Arc(a), Ty::Arc(b)) | (Ty::Weak(a), Ty::Weak(b)) | (Ty::Cell(a), Ty::Cell(b)) | (Ty::Mutex(a), Ty::Mutex(b)) | (Ty::MutexRef(a), Ty::MutexRef(b)) | (Ty::Sender(a), Ty::Sender(b)) | (Ty::Receiver(a), Ty::Receiver(b)) | (Ty::Task(a), Ty::Task(b)) | (Ty::Iso(a), Ty::Iso(b)) | (Ty::Ptr(a), Ty::Ptr(b)) => bind_params(a, b, s),
        (Ty::Array(a, n), Ty::Array(b, m)) => n == m && bind_params(a, b, s),
        (Ty::ArrayT(a, n), Ty::Array(b, m)) => bind_params(n, &Ty::ConstInt(*m), s) && bind_params(a, b, s),
        (Ty::ArrayT(a, n), Ty::ArrayT(b, m)) => bind_params(n, m, s) && bind_params(a, b, s),
        (Ty::Map(a, b), Ty::Map(c, d)) | (Ty::Result(a, b), Ty::Result(c, d)) => bind_params(a, c, s) && bind_params(b, d, s),
        (Ty::Adt(d1, a1), Ty::Adt(d2, a2)) | (Ty::Iface(d1, a1), Ty::Iface(d2, a2)) => d1 == d2 && a1.iter().zip(a2.iter()).all(|(x, y)| bind_params(x, y, s)),
        (Ty::Func(f1), Ty::Func(f2)) => f1.params.len() == f2.params.len() && f1.params.iter().zip(f2.params.iter()).all(|((m1, x), (m2, y))| m1 == m2 && bind_params(x, y, s)) && bind_params(&f1.ret, &f2.ret, s),
        (a, b) => a == b,
    }
}

pub fn builtin_type(name: &str) -> Option<Ty> {
    Some(match name {
        "int" | "i64" => Ty::Int(IntTy::I64),
        "i8" => Ty::Int(IntTy::I8),
        "i16" => Ty::Int(IntTy::I16),
        "i32" | "rune" => Ty::Int(IntTy::I32),
        "uint" | "u64" => Ty::Int(IntTy::U64),
        "u8" | "byte" => Ty::Int(IntTy::U8),
        "u16" => Ty::Int(IntTy::U16),
        "u32" => Ty::Int(IntTy::U32),
        "f32" => Ty::F32,
        "f64" => Ty::F64,
        "bool" => Ty::Bool,
        "String" => Ty::String,
        "str" => Ty::Str,
        "never" => Ty::Never,
        "void" => Ty::Unit,
        "Scope" => Ty::Scope,
        _ => return None,
    })
}

pub fn is_generic_builtin(name: &str) -> bool {
    matches!(name, "Vec" | "Map" | "Box" | "Rc" | "Arc" | "Weak" | "Cell" | "Mutex" | "MutexRef" | "Task" | "Sender" | "Receiver" | "Iso")
}

/// Names that older versions of Gojo used, and what to write now.
pub fn renamed(name: &str) -> Option<String> {
    Some(match name {
        "rc" | "arc" | "weak" => format!("built-in types are capitalized: write `{}{}`", name[..1].to_uppercase(), &name[1..]),
        "chan" => "channels are made with `Chan[T](n)`, and the type is `Chan[T]`".into(),
        _ => return None,
    })
}

impl Checker {
    /// The prelude struct behind `(A, B, ...)` with `n` elements.
    pub fn tuple_def(&self, n: usize) -> Option<DefId> {
        if (2..=6).contains(&n) {
            self.prog.prelude_type(&format!("Tuple{}", n))
        } else {
            None
        }
    }
}
