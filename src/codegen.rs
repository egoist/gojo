// C code generation.
//
// Generic functions are instantiated per set of concrete type arguments.
// Every owned value gets a deterministic drop at scope exit; a drop flag is
// only introduced for paths that may be moved.

use crate::ast::{Mode, YieldKind};
use crate::check::bind_params;
use crate::diag::{SourceMap, Span};
use crate::tir::*;
use crate::types::*;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

mod bce;
mod drops;

/// The largest value, in bytes. Values live on stacks or inline in other values;
/// one this big could never be used, and C compilers reject sizes near 2^63.
pub const MAX_VALUE_BYTES: u128 = 1 << 40;

/// Instantiated types nest at most this deeply. Written types are bounded by
/// the parser's nesting limit, well below; deeper ones come from a generic
/// function or type that instantiates itself with a bigger type every time
/// (`grow[T]` calling `grow[Vec[T]]`), which would never end.
const MAX_TYPE_DEPTH: usize = 400;

fn ty_depth(t: &Ty) -> usize {
    let mut d = 0;
    match t {
        Ty::Vec(a) | Ty::Slice(a) | Ty::Array(a, _) | Ty::ArrayT(a, _) | Ty::Option(a) | Ty::Box(a) | Ty::Rc(a) | Ty::Arc(a) | Ty::Weak(a) | Ty::Cell(a) | Ty::Mutex(a) | Ty::MutexRef(a) | Ty::Sender(a) | Ty::Receiver(a) | Ty::Ptr(a) => d = ty_depth(a),
        Ty::Map(a, b) | Ty::Result(a, b) => d = ty_depth(a).max(ty_depth(b)),
        Ty::Adt(_, args) | Ty::Iface(_, args) => d = args.iter().map(ty_depth).max().unwrap_or(0),
        Ty::Func(f) => d = f.params.iter().map(|p| ty_depth(&p.1)).chain(std::iter::once(ty_depth(&f.ret))).max().unwrap_or(0),
        _ => {}
    }
    d + 1
}

/// Generates the program's C. Fails only for types too large to exist (their
/// sizes are known only once generics are instantiated).
/// Ends the Gojo-attributed part of the C: [`resolve_lines`] makes it a
/// `#line` back to the C file itself.
const C_LINE_MARK: &str = "//@gojo-c-line";

/// The final C for a file at `c_path`: each mark becomes `#line` back to the C
/// file, so code the compiler generates around Gojo functions (drop
/// functions, helpers, `main`) is not blamed on the last Gojo line.
pub fn resolve_lines(c: &str, c_path: &str) -> String {
    let mut out = String::with_capacity(c.len() + 1024);
    for (i, l) in c.lines().enumerate() {
        if l == C_LINE_MARK {
            out.push_str(&format!("#line {} {}\n", i + 2, c_string(c_path)));
        } else {
            out.push_str(l);
            out.push('\n');
        }
    }
    out
}

/// The C without `#line` directives, to read or debug the generated C itself.
pub fn strip_lines(c: &str) -> String {
    let mut out = String::with_capacity(c.len());
    for l in c.lines() {
        if l != C_LINE_MARK && !l.trim_start().starts_with("#line ") {
            out.push_str(l);
            out.push('\n');
        }
    }
    out
}

fn stmt_span(s: &TStmt) -> Option<Span> {
    Some(match s {
        TStmt::Let { init, .. } => init.span,
        TStmt::LetRef { place, .. } => place.span,
        TStmt::Expr(e) => e.span,
        TStmt::Assign { span, .. } => *span,
        TStmt::MapInsert { map, .. } => map.span,
        TStmt::Return(_, sp) | TStmt::Break(_, sp) | TStmt::Continue(_, sp) | TStmt::Yield(_, sp) => *sp,
        TStmt::Defer(b) | TStmt::ErrDefer(b) => b.span,
        TStmt::While { cond, body, .. } => cond.as_ref().map_or(body.span, |c| c.span),
        TStmt::For { span, .. } | TStmt::Select { span, .. } => *span,
        TStmt::Send { chan, .. } => chan.span,
        TStmt::With { place, .. } => place.span,
        TStmt::ForEach { call, .. } => call.span,
        TStmt::ParFor { span, .. } => *span,
        TStmt::WithLocks { places, .. } => places.first().map(|p| p.span).unwrap_or_default(),
    })
}

/// The bounds checks of one function: where the compiler removed them, and
/// where they stay.
#[derive(Default, Clone, Debug)]
pub struct Claims {
    pub removed: Vec<Span>,
    pub kept: Vec<Span>,
}

/// What `gojo claims` reports: the program's code is generated (and thrown
/// away) to see which bounds checks the compiler removes.
pub fn claims(prog: &Program, tfuncs: &[TFunc], sm: &SourceMap) -> Result<HashMap<FuncId, Claims>, String> {
    let mut out = HashMap::new();
    generate_with(prog, tfuncs, sm, None, true, &mut |g| out = std::mem::take(&mut g.claims))?;
    Ok(out)
}

pub fn generate(prog: &Program, tfuncs: &[TFunc], sm: &SourceMap, tests: Option<&Vec<(String, FuncId)>>, release: bool) -> Result<String, String> {
    generate_with(prog, tfuncs, sm, tests, release, &mut |_| {})
}

fn generate_with(prog: &Program, tfuncs: &[TFunc], sm: &SourceMap, tests: Option<&Vec<(String, FuncId)>>, release: bool, done: &mut dyn FnMut(&mut Gen)) -> Result<String, String> {
    let mut g = Gen::new(prog, tfuncs, sm, release);
    let mut entry = String::new();
    match tests {
        Some(ts) => {
            let _ = writeln!(entry, "int main(int argc, char **argv) {{\n    gojo_rt_init(argc, argv);\n    gojo_debug_checks = {};\n    int failed = 0;", if release { 0 } else { 1 });
            for (name, f) in ts {
                let n = g.instance(*f, vec![]);
                let _ = writeln!(entry, "    failed += gojo_run_test({}, {});", n, c_string(name));
            }
            let _ = writeln!(entry, "    fprintf(stderr, \"%d passed, %d failed\\n\", {} - failed, failed);\n    return failed ? 1 : 0;\n}}", ts.len());
        }
        None => {
            let main = match prog.packages[prog.main_pkg as usize].items.get("main") {
                Some(Sym::Func(f)) => *f,
                _ => panic!("no main"),
            };
            let n = g.instance(main, vec![]);
            let ret = prog.func(main).ret.clone();
            // main runs as the first goroutine
            let _ = writeln!(entry, "static void gojo_entry(void) {{");
            if let Ty::Result(_, e) = &ret {
                let rt = g.cty(&ret);
                let et = (**e).clone();
                let fe = g.fmt_fn(&et);
                let trace = if release { "" } else { "\n        gojo_etrace_print();" };
                let _ = writeln!(entry, "    {} r = {}();\n    if (!r.ok) {{\n        gojo_string b = {{0}};\n        gojo_fmt_cstr(&b, \"error: \");\n        {}(&b, &r.u.err, 0);\n        gojo_fmt_cstr(&b, \"\\n\");\n        gojo_write(2, &b);{}\n        fflush(stdout);\n        exit(1);\n    }}", rt, n, fe, trace);
            } else {
                let _ = writeln!(entry, "    {}();", n);
            }
            let _ = writeln!(entry, "}}\n\nint main(int argc, char **argv) {{\n    gojo_rt_init(argc, argv);\n    gojo_debug_checks = {};\n    gojo_sched_main(gojo_entry);\n}}", if release { 0 } else { 1 });
        }
    }
    g.drain();
    done(&mut g);
    if let Some(msg) = g.runaway.take() {
        return Err(msg);
    }
    if let Some(msg) = g.static_err.take() {
        return Err(msg);
    }
    if !g.checkfree_errs.is_empty() {
        let mut v: Vec<String> = g.checkfree_errs.iter().map(|e| e.1.clone()).collect();
        v.sort();
        return Err(v.join("\nerror: "));
    }
    if let Some(msg) = g.json_err.take() {
        return Err(msg);
    }
    if let Some(t) = g.too_large.first() {
        return Err(format!("the type `{}` is too large: values can be at most {} bytes; use a `Vec` for large data", prog.show(t), MAX_VALUE_BYTES));
    }
    let mut out = String::new();
    out.push_str("// generated by the Gojo compiler\n#include <gojo_rt.h>\n#include <stdio.h>\n#include <stddef.h>\n\n");
    out.push_str(&g.fwd);
    out.push('\n');
    out.push_str(&g.tdefs);
    out.push('\n');
    out.push_str(&g.protos);
    out.push('\n');
    out.push_str(&g.statics);
    out.push('\n');
    out.push_str(&g.bodies);
    out.push('\n');
    out.push_str(&entry);
    Ok(out)
}

fn c_string(s: &str) -> String {
    let mut o = String::from("\"");
    for b in s.bytes() {
        match b {
            b'"' => o.push_str("\\\""),
            b'\\' => o.push_str("\\\\"),
            b'\n' => o.push_str("\\n"),
            b'\t' => o.push_str("\\t"),
            b'\r' => o.push_str("\\r"),
            b'?' => o.push_str("\\?"),
            0x20..=0x7e => o.push(b as char),
            _ => {
                let _ = write!(o, "\\{:03o}", b);
            }
        }
    }
    o.push('"');
    o
}

#[derive(Clone)]
enum Repr {
    Val(String),
    Ptr(String),
}

#[derive(Clone)]
enum Cl {
    DropLocal(LocalId),
    DropTemp(String, Ty),
    /// drop guarded by a flag variable
    DropFlag(String, Ty, String),
    /// A temporary of a branch's value (see `SK::Tail`): declared at the top of
    /// the function with a flag, so the enclosing statement can drop it after
    /// the branch's C block has closed.
    DropLater(String, Ty, String),
    Defer(TBlock),
    /// Runs only on error returns.
    ErrDefer(TBlock),
    Unlock(String),
    /// An RwLock (its handle), read or write.
    UnlockRw(String, bool),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SK {
    Block,
    /// A loop body with its loop id.
    Loop(u32),
    Stmt,
    /// The value of a branch (a block's last expression, a match arm, the right
    /// side of `??`) whose type may hold a view: the view may point into the
    /// branch's temporaries, so they live until the enclosing statement is done
    /// (`out.push(if c { "${x}" } else { "" })`).
    Tail,
    Func,
}

struct Scope {
    kind: SK,
    items: Vec<Cl>,
    /// The body of a `scope` block: the C name of its `gojo_group *`. Leaving
    /// the block waits for its tasks (cancelling them first when leaving
    /// early), before anything declared in the body is dropped.
    wait: Option<String>,
}

struct Fs<'a> {
    tf: &'a TFunc,
    subst: HashMap<TParamId, Ty>,
    out: String,
    /// Declarations emitted at the top of the body, before `out`.
    decls: String,
    ind: usize,
    scopes: Vec<Scope>,
    repr: HashMap<LocalId, Repr>,
    flagged: HashSet<(LocalId, Vec<usize>)>,
    flags: HashMap<(LocalId, Vec<usize>), String>,
    ret: Ty,
    yields: bool,
    /// While emitting return-path cleanups: C condition for "returning an error".
    err_mode: Option<String>,
    /// Enclosing loops: (loop id, break label, continue label).
    loop_labels: Vec<(u32, String, String)>,
    /// Inside the body closure of a push-style `for` loop.
    lb: Option<LoopBody>,
    /// The Gojo source line the last `#line` directive named: (file, line).
    line: (u32, u32),
    /// `scope` blocks being generated: id -> C name of their state.
    tscopes: HashMap<u32, String>,
    /// What is known about the enclosing loops' variables (bce.rs).
    bce: Vec<bce::Fact>,
    /// Places whose length nothing in the function changes (bce.rs).
    bce_steady: HashMap<bce::Path, bool>,
    /// `var` integers that never go below a value anywhere (bce.rs).
    bce_floor: std::rc::Rc<HashMap<LocalId, i128>>,
    /// Where drop flags have known values, and drops moved to the ends of
    /// branches (drops.rs). `None` in closure bodies: flags everywhere.
    dinfo: Option<std::rc::Rc<drops::DropInfo>>,
    /// The place drops are being emitted for, while they are.
    site: Option<drops::Site>,
    /// Drop flags some drop tests: the others are never declared or set
    /// (`resolve_flags`).
    used_flags: HashSet<String>,
    /// Join drops emitted (each must be, or the value would leak).
    jd_done: HashSet<(usize, usize)>,
    /// Last-use frees emitted (each must be: a value moved elsewhere would
    /// leak otherwise).
    fd_done: HashSet<(usize, usize)>,
    /// The field each enclosing `static for` (or `static T { }`) is at, by
    /// its type parameter.
    static_cur: HashMap<TParamId, String>,
}

/// The body of `for x in t` over a type with `each`, compiled as a closure.
/// `ctl` and `ret` are lvalues (in the closure) of the control word and the
/// enclosing function's return slot.
#[derive(Clone)]
struct LoopBody {
    id: u32,
    ctl: String,
    ret: String,
}

/// Control word values. 0: keep going.
const CTL_RETURN: u32 = 1;
/// The loop's own `break`: `each` may ignore the returned Flow, so the body
/// has to remember that it must not run again.
const CTL_STOP: i32 = -1;
fn ctl_break(id: u32) -> u32 {
    2 + 2 * id
}
fn ctl_continue(id: u32) -> u32 {
    3 + 2 * id
}

struct Gen<'a> {
    prog: &'a Program,
    sm: &'a SourceMap,
    tf: HashMap<FuncId, &'a TFunc>,
    /// Functions that may call themselves, directly, through others, or through
    /// a function value or interface: they poll for preemption on entry.
    may_recurse: HashSet<FuncId>,
    bounded: HashSet<FuncId>,
    fwd: String,
    tdefs: String,
    protos: String,
    statics: String,
    bodies: String,
    names: HashMap<Ty, String>,
    defined: HashSet<String>,
    instances: HashMap<(FuncId, Vec<Ty>), String>,
    work: Vec<(FuncId, Vec<Ty>, String)>,
    helpers: HashSet<String>,
    ntmp: u32,
    nlabel: u32,
    type_ids: HashMap<Ty, u64>,
    drop_memo: HashMap<Ty, bool>,
    loc_cache: HashMap<Span, String>,
    /// For the loop-body closure about to be generated: (control word pointer, return slot pointer).
    pending_lb: Option<(String, String)>,
    /// Array types over `MAX_VALUE_BYTES`.
    too_large: Vec<Ty>,
    /// A generic function or type instantiating itself without end.
    runaway: Option<String>,
    /// A type `json.encode` / `json.decode` can't handle (the first one).
    json_err: Option<String>,
    /// The `json.encode` / `json.decode` call being generated, and the
    /// fields leading to the type at hand, for that error.
    json_site: (Span, &'static str),
    json_ctx: Vec<String>,
    size_memo: HashMap<Ty, u128>,
    /// A `static for` or `static T { }` over a type that isn't a struct.
    static_err: Option<String>,
    /// Bounds checks, by function: `gojo claims`.
    pub claims: HashMap<FuncId, Claims>,
    /// Checks left in `checkfree` functions.
    checkfree_errs: Vec<(Span, String)>,
    /// Debug builds record where errors are made and passed up by `try`
    /// (error return traces), printed when one leaves `main`.
    etrace: bool,
}

impl<'a> Gen<'a> {
    fn new(prog: &'a Program, tfuncs: &'a [TFunc], sm: &'a SourceMap, release: bool) -> Gen<'a> {
        let recursive = may_recurse(tfuncs);
        Gen {
            prog,
            sm,
            tf: tfuncs.iter().map(|t| (t.func, t)).collect(),
            bounded: bounded_funcs(tfuncs, &recursive),
            may_recurse: recursive,
            fwd: String::new(),
            tdefs: String::new(),
            protos: String::new(),
            statics: String::new(),
            bodies: String::new(),
            names: HashMap::new(),
            defined: HashSet::new(),
            instances: HashMap::new(),
            work: vec![],
            helpers: HashSet::new(),
            ntmp: 0,
            nlabel: 0,
            type_ids: HashMap::new(),
            drop_memo: HashMap::new(),
            loc_cache: HashMap::new(),
            pending_lb: None,
            too_large: vec![],
            runaway: None,
            json_err: None,
            json_site: (Span::default(), "encode"),
            json_ctx: vec![],
            size_memo: HashMap::new(),
            etrace: !release,
            static_err: None,
            claims: HashMap::new(),
            checkfree_errs: vec![],
        }
    }

    fn tmp(&mut self) -> String {
        self.ntmp += 1;
        format!("t{}", self.ntmp)
    }

    fn label(&mut self) -> String {
        self.nlabel += 1;
        format!("L{}", self.nlabel)
    }

    fn loc(&mut self, sp: Span) -> String {
        if let Some(s) = self.loc_cache.get(&sp) {
            return s.clone();
        }
        let (path, l, c) = if (sp.file as usize) < self.sm.files.len() {
            let f = self.sm.file(sp.file);
            let (l, c) = f.line_col(sp.lo);
            (f.path.clone(), l, c)
        } else {
            ("?".into(), 0, 0)
        };
        let s = c_string(&format!("{}:{}:{}", path, l, c));
        self.loc_cache.insert(sp, s.clone());
        s
    }

    // ------------------------------------------------------------------
    // Types
    // ------------------------------------------------------------------

    fn is_copy(&self, t: &Ty) -> bool {
        match t {
            Ty::Unit | Ty::Never | Ty::Bool | Ty::Int(_) | Ty::F32 | Ty::F64 | Ty::Str | Ty::Slice(_) | Ty::Ptr(_) | Ty::Error | Ty::ConstInt(_) => true,
            Ty::Option(a) | Ty::Array(a, _) => self.is_copy(a),
            Ty::Result(a, b) => self.is_copy(a) && self.is_copy(b),
            Ty::Adt(d, args) => self.prog.def(*d).is_copy || (self.prog.def(*d).tuple && args.iter().all(|a| self.is_copy(a))),
            _ => false,
        }
    }

    fn is_scalar(&self, t: &Ty) -> bool {
        matches!(t, Ty::Unit | Ty::Bool | Ty::Int(_) | Ty::F32 | Ty::F64 | Ty::Ptr(_))
    }

    fn needs_drop(&mut self, t: &Ty) -> bool {
        if let Some(&b) = self.drop_memo.get(t) {
            return b;
        }
        self.drop_memo.insert(t.clone(), false);
        let r = match t {
            Ty::String | Ty::Vec(_) | Ty::Map(..) | Ty::Box(_) | Ty::Rc(_) | Ty::Arc(_) | Ty::Weak(_) | Ty::Sender(_) | Ty::Receiver(_) | Ty::Func(_) | Ty::Iface(..) | Ty::Mutex(_) | Ty::MutexRef(_) | Ty::Task(_) => true,
            Ty::Option(a) | Ty::Array(a, _) | Ty::Cell(a) | Ty::Iso(a) => self.needs_drop(a),
            Ty::Result(a, b) => self.needs_drop(a) || self.needs_drop(b),
            Ty::Adt(d, args) => {
                let def = self.prog.def(*d);
                if def.has_drop {
                    true
                } else {
                    let tys: Vec<Ty> = match &def.kind {
                        TypeDefKind::Struct { .. } => self.prog.field_tys(*d, args).into_iter().map(|f| f.1).collect(),
                        TypeDefKind::Enum { variants } => (0..variants.len()).flat_map(|v| self.prog.variant_tys(*d, args, v).into_iter().map(|f| f.1)).collect(),
                        _ => vec![],
                    };
                    tys.iter().any(|t| self.needs_drop(t))
                }
            }
            _ => false,
        };
        self.drop_memo.insert(t.clone(), r);
        r
    }

    fn mangle(&self, t: &Ty) -> String {
        let s = match t {
            Ty::Unit => "unit".into(),
            Ty::Never => "never".into(),
            Ty::Bool => "bool".into(),
            Ty::Int(i) => i.name().replace("int", "i64"),
            Ty::F32 => "f32".into(),
            Ty::F64 => "f64".into(),
            Ty::Str => "str".into(),
            Ty::String => "String".into(),
            Ty::Vec(a) => format!("Vec_{}", self.mangle(a)),
            Ty::Slice(a) => format!("Sl_{}", self.mangle(a)),
            Ty::Array(a, n) => format!("A{}_{}", n, self.mangle(a)),
            Ty::ArrayT(a, n) => format!("AT{}_{}", self.mangle(n), self.mangle(a)),
            Ty::Map(a, b) => format!("Map_{}_{}", self.mangle(a), self.mangle(b)),
            Ty::Option(a) => format!("Opt_{}", self.mangle(a)),
            Ty::Result(a, b) => format!("Res_{}_{}", self.mangle(a), self.mangle(b)),
            Ty::Box(a) => format!("Box_{}", self.mangle(a)),
            Ty::Rc(a) => format!("Rc_{}", self.mangle(a)),
            Ty::Arc(a) => format!("Arc_{}", self.mangle(a)),
            Ty::Weak(a) => format!("Weak_{}", self.mangle(a)),
            Ty::Cell(a) => format!("Cell_{}", self.mangle(a)),
            Ty::Mutex(a) => format!("Mtx_{}", self.mangle(a)),
            Ty::MutexRef(a) => format!("MtxRef_{}", self.mangle(a)),
            Ty::Sender(a) => format!("Tx_{}", self.mangle(a)),
            Ty::Receiver(a) => format!("Rx_{}", self.mangle(a)),
            Ty::Task(a) => format!("Task_{}", self.mangle(a)),
            Ty::Iso(a) => format!("Iso_{}", self.mangle(a)),
            Ty::Scope => "Scope".into(),
            Ty::Ptr(a) => format!("Ptr_{}", self.mangle(a)),
            Ty::Func(f) => {
                // (`send fn` and `fn` are different types: their instances too)
                let mut s = if f.send { "SFn".to_string() } else { "Fn".to_string() };
                for (m, p) in &f.params {
                    let _ = write!(s, "_{}{}", match m {
                        Mode::Borrow => "",
                        Mode::Inout => "io",
                        Mode::Sink => "sk",
                    }, self.mangle(p));
                }
                let _ = write!(s, "_R{}", self.mangle(&f.ret));
                s
            }
            Ty::Adt(d, args) | Ty::Iface(d, args) => {
                let def = self.prog.def(*d);
                let pkg = &self.prog.packages[def.pkg as usize].name;
                let mut s = format!("{}_{}", pkg, def.name);
                for a in args.iter() {
                    s.push('_');
                    s.push_str(&self.mangle(a));
                }
                s
            }
            Ty::ConstInt(n) => format!("N{}", n),
            Ty::Param(p) => format!("P{}", p),
            Ty::Var(v) => format!("V{}", v),
            Ty::Error => "err".into(),
        };
        if s.len() > 120 {
            let mut h: u64 = 0xcbf29ce484222325;
            for b in s.bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x100000001b3);
            }
            return format!("{}_{:x}", &s[..40], h);
        }
        s
    }

    /// C type name; composite by-value types are fully defined.
    fn cty(&mut self, t: &Ty) -> String {
        let n = self.cname(t);
        self.ensure(t);
        n
    }

    /// C type name without requiring a complete definition.
    fn cname(&mut self, t: &Ty) -> String {
        match t {
            Ty::Unit | Ty::Never | Ty::Error | Ty::ConstInt(_) => "gojo_unit".into(),
            Ty::Bool => "bool".into(),
            Ty::Int(i) => i.c_name().into(),
            Ty::F32 => "float".into(),
            Ty::F64 => "double".into(),
            Ty::Str => "gojo_str".into(),
            Ty::String => "gojo_string".into(),
            Ty::Vec(_) => "gojo_vec".into(),
            Ty::Slice(_) => "gojo_slice".into(),
            Ty::Map(..) => "gojo_map".into(),
            Ty::Sender(_) | Ty::Receiver(_) => "gojo_chan*".into(),
            Ty::Task(_) => "gojo_task*".into(),
            // the same as the value; isolation only matters to the checker
            Ty::Iso(a) => self.cname(a),
            Ty::Scope => "gojo_group*".into(),
            Ty::Func(_) => "gojo_func".into(),
            Ty::Iface(..) => "gojo_iface".into(),
            Ty::Cell(a) => self.cname(a),
            Ty::Box(a) => {
                let inner = self.cname(a);
                format!("{}*", inner)
            }
            Ty::Ptr(a) => {
                let inner = self.cname(a);
                if inner == "gojo_unit" {
                    "void*".into()
                } else {
                    format!("{}*", inner)
                }
            }
            Ty::Rc(a) | Ty::Arc(a) | Ty::Weak(a) => {
                let n = format!("RcBox_{}", self.mangle(a));
                self.fwd_struct(&n);
                format!("{}*", n)
            }
            Ty::Param(_) | Ty::Var(_) => panic!("unsubstituted type {:?}", t),
            Ty::Option(inner) if self.niche(t) => self.cname(inner),
            _ => {
                if let Some(n) = self.names.get(t) {
                    return n.clone();
                }
                let n = format!("K_{}", self.mangle(t));
                self.fwd_struct(&n);
                self.names.insert(t.clone(), n.clone());
                n
            }
        }
    }

    /// An upper bound on the type's size in bytes (every field rounded up to 8
    /// bytes and padded by 8 more), saturating.
    fn size_bound(&mut self, t: &Ty) -> u128 {
        if let Some(&n) = self.size_memo.get(t) {
            return n;
        }
        let n = match t {
            Ty::Array(a, len) => self.size_bound(a).saturating_mul((*len).max(1) as u128),
            Ty::Option(a) | Ty::Mutex(a) | Ty::Cell(a) => self.size_bound(a).saturating_add(16),
            Ty::Result(a, b) => self.size_bound(a).max(self.size_bound(b)).saturating_add(16),
            Ty::Adt(d, args) => {
                let def = self.prog.def(*d).clone();
                match &def.kind {
                    TypeDefKind::Struct { .. } => {
                        let fs = self.prog.field_tys(*d, args);
                        fs.iter().fold(16u128, |acc, (_, ft)| acc.saturating_add(self.size_bound(ft)).saturating_add(8))
                    }
                    TypeDefKind::Enum { variants } => {
                        let mut m = 0u128;
                        for vi in 0..variants.len() {
                            let fs = self.prog.variant_tys(*d, args, vi);
                            let v = fs.iter().fold(0u128, |acc, (_, ft)| acc.saturating_add(self.size_bound(ft)).saturating_add(8));
                            m = m.max(v);
                        }
                        m.saturating_add(16)
                    }
                    _ => 64,
                }
            }
            // pointers, handles and small fixed-size values
            _ => 64,
        };
        self.size_memo.insert(t.clone(), n);
        n
    }

    fn fwd_struct(&mut self, n: &str) {
        if self.helpers.insert(format!("fwd:{}", n)) {
            let _ = writeln!(self.fwd, "typedef struct {} {};", n, n);
        }
    }

    /// Whether `t` is too deep to come from anything but a runaway generic
    /// type (recorded as the error to report).
    fn runaway_type(&mut self, t: &Ty) -> bool {
        if self.runaway.is_some() {
            return true;
        }
        if ty_depth(t) <= MAX_TYPE_DEPTH {
            return false;
        }
        let mut name: String = self.prog.show(t).chars().take(40).collect();
        let mut cur = t;
        // the outermost user type in it is the one that grows
        loop {
            match cur {
                Ty::Adt(d, _) => {
                    name = self.prog.def(*d).name.clone();
                    break;
                }
                Ty::Vec(a) | Ty::Option(a) | Ty::Box(a) | Ty::Rc(a) | Ty::Arc(a) | Ty::Weak(a) | Ty::Cell(a) | Ty::Mutex(a) | Ty::Sender(a) | Ty::Receiver(a) | Ty::Array(a, _) => cur = a,
                _ => break,
            }
        }
        self.runaway = Some(format!("the type `{}` never ends: it contains itself with a bigger type argument (like `{}[Vec[T]]` inside `{}[T]`)", name, name, name));
        true
    }

    /// `T` as a type whose accesses may alias anything.
    fn may_alias_ty(&mut self, t: &Ty) -> String {
        let n = format!("MayAlias_{}", self.mangle(t));
        if self.helpers.insert(n.clone()) {
            let c = self.cty(t);
            let _ = writeln!(self.tdefs, "typedef {} __attribute__((may_alias)) {};", c, n);
        }
        n
    }

    fn ensure(&mut self, t: &Ty) {
        if self.niche(t) {
            return;
        }
        match t {
            Ty::Array(..) | Ty::Option(_) | Ty::Result(..) | Ty::Mutex(_) | Ty::MutexRef(_) | Ty::Adt(..) => {}
            Ty::Cell(a) => return self.ensure(a),
            _ => return,
        }
        if self.runaway_type(t) {
            return;
        }
        let n = self.cname(t);
        if self.defined.contains(&n) {
            return;
        }
        self.defined.insert(n.clone());
        if self.size_bound(t) > MAX_VALUE_BYTES {
            self.too_large.push(t.clone());
        }
        let mut body = String::new();
        match t {
            Ty::Array(a, len) => {
                let e = self.cty(a);
                let _ = write!(body, "struct {} {{ {} a[{}]; }};\n", n, e, (*len).max(1));
            }
            Ty::Option(a) => {
                let e = self.cty(a);
                let _ = write!(body, "struct {} {{ bool some; {} val; }};\n", n, e);
            }
            Ty::Result(a, b) => {
                let x = self.cty(a);
                let y = self.cty(b);
                let _ = write!(body, "struct {} {{ bool ok; union {{ {} val; {} err; }} u; }};\n", n, x, y);
            }
            Ty::Mutex(a) => {
                let e = self.cty(a);
                let _ = write!(body, "struct {} {{ gojo_mutex *mu; {} val; }};\n", n, e);
            }
            Ty::MutexRef(a) => {
                let e = self.cname(a);
                let _ = write!(body, "struct {} {{ gojo_mutex *mu; {} *ptr; }};\n", n, e);
            }
            Ty::Adt(d, args) => {
                let def = self.prog.def(*d).clone();
                match &def.kind {
                    TypeDefKind::Struct { .. } => {
                        let fs = self.prog.field_tys(*d, args);
                        let _ = write!(body, "struct {} {{", n);
                        if fs.is_empty() {
                            body.push_str(" gojo_unit _empty;");
                        }
                        for (fname, ft) in &fs {
                            let c = self.cty(ft);
                            let _ = write!(body, " {} f_{};", c, fname);
                        }
                        body.push_str(" };\n");
                    }
                    TypeDefKind::Enum { variants } => {
                        let _ = write!(body, "struct {} {{ int32_t tag;", n);
                        let any = variants.iter().any(|v| !v.fields.is_empty());
                        if any {
                            body.push_str(" union {");
                            for vi in 0..variants.len() {
                                let fs = self.prog.variant_tys(*d, args, vi);
                                if fs.is_empty() {
                                    continue;
                                }
                                body.push_str(" struct {");
                                for (fname, ft) in &fs {
                                    let c = self.cty(ft);
                                    let _ = write!(body, " {} f_{};", c, fname);
                                }
                                let _ = write!(body, " }} v{};", vi);
                            }
                            body.push_str(" } u;");
                        }
                        body.push_str(" };\n");
                    }
                    TypeDefKind::Iface { .. } => {}
                }
            }
            _ => {}
        }
        self.tdefs.push_str(&body);
    }

    fn ensure_rcbox(&mut self, inner: &Ty) -> String {
        let n = format!("RcBox_{}", self.mangle(inner));
        self.fwd_struct(&n);
        if self.defined.insert(n.clone()) {
            let e = self.cty(inner);
            let _ = writeln!(self.tdefs, "struct {} {{ int64_t strong; int64_t weak; {} val; }};", n, e);
        }
        n
    }

    /// Entry type of `Map[K, V]` plus lookup / insert / remove functions
    /// specialized for the key type. Returns (entry type, function suffix):
    /// `mf_<m>` finds, `ms_<m>` finds or inserts, `mr_<m>` removes.
    fn map_ent(&mut self, k: &Ty, v: &Ty) -> (String, String) {
        let m = format!("{}_{}", self.mangle(k), self.mangle(v));
        let ent = format!("MapEnt_{}", m);
        self.fwd_struct(&ent);
        if self.defined.insert(ent.clone()) {
            let kc = self.cty(k);
            let vc = self.cty(v);
            let _ = writeln!(self.tdefs, "struct {} {{ uint64_t h; int64_t live; {} k; {} v; }};", ent, kc, vc);
            let (hash, eq) = match k {
                Ty::Int(_) | Ty::Bool => ("gojo_hash_u64((uint64_t)*(p))".to_string(), "(*(a) == *(b))".to_string()),
                Ty::Str => ("gojo_str_hash(*(p))".to_string(), "gojo_str_eq(*(a), *(b))".to_string()),
                Ty::String => ("gojo_str_hash(gojo_string_view(p))".to_string(), "gojo_str_eq(gojo_string_view(a), gojo_string_view(b))".to_string()),
                _ => {
                    let hf = self.hash_fn(k);
                    let ef = self.eq_fn(k);
                    (format!("{}(({} *)(p))", hf, kc), format!("{}(({} *)(a), ({} *)(b))", ef, kc, kc))
                }
            };
            let _ = writeln!(self.protos, "static {e} *mf_{m}(const gojo_map *mp, const {k} *key);\nstatic {e} *ms_{m}(gojo_map *mp, const {k} *key, bool *existed);\nstatic bool mr_{m}(gojo_map *mp, const {k} *key, {e} *out);", e = ent, m = m, k = kc);
            let _ = writeln!(
                self.bodies,
                "static inline uint64_t mh_{m}(const {k} *p) {{ return {hash}; }}\n\
static inline bool meq_{m}(const {k} *a, const {k} *b) {{ return {eq}; }}\n\
static inline int64_t ml_{m}(const gojo_map *mp, const {k} *key, uint64_t h, int64_t *slot_out) {{\n\
    if (!mp->index_cap) {{ if (slot_out) *slot_out = -1; return -1; }}\n\
    const {e} *es = (const {e} *)mp->entries;\n\
    uint64_t mask = (uint64_t)mp->index_cap - 1;\n\
    int64_t s = (int64_t)(h & mask), tomb = -1;\n\
    for (;;) {{\n\
        int64_t ix = mp->index[s];\n\
        if (ix == -1) {{ if (slot_out) *slot_out = tomb >= 0 ? tomb : s; return -1; }}\n\
        if (ix == -2) {{ if (tomb < 0) tomb = s; }}\n\
        else if (es[ix].h == h && meq_{m}(&es[ix].k, key)) {{ if (slot_out) *slot_out = s; return ix; }}\n\
        s = (int64_t)(((uint64_t)s + 1) & mask);\n\
    }}\n\
}}\n\
static {e} *mf_{m}(const gojo_map *mp, const {k} *key) {{\n\
    if (!mp->len) return NULL;\n\
    int64_t ix = ml_{m}(mp, key, mh_{m}(key), NULL);\n\
    return ix >= 0 ? (({e} *)mp->entries) + ix : NULL;\n\
}}\n\
static {e} *ms_{m}(gojo_map *mp, const {k} *key, bool *existed) {{\n\
    uint64_t h = mh_{m}(key);\n\
    int64_t ix = ml_{m}(mp, key, h, NULL);\n\
    if (ix >= 0) {{ *existed = true; return (({e} *)mp->entries) + ix; }}\n\
    *existed = false;\n\
    gojo_map_reserve1(mp, sizeof({e}));\n\
    int64_t slot;\n\
    ml_{m}(mp, key, h, &slot);\n\
    int64_t ni = mp->n_entries++;\n\
    mp->index[slot] = ni;\n\
    {e} *e = (({e} *)mp->entries) + ni;\n\
    e->h = h; e->live = 1; mp->len++;\n\
    return e;\n\
}}\n\
static bool mr_{m}(gojo_map *mp, const {k} *key, {e} *out) {{\n\
    if (!mp->len) return false;\n\
    int64_t slot;\n\
    int64_t ix = ml_{m}(mp, key, mh_{m}(key), &slot);\n\
    if (ix < 0) return false;\n\
    {e} *e = (({e} *)mp->entries) + ix;\n\
    *out = *e; e->live = 0; mp->index[slot] = -2; mp->len--;\n\
    if (ix == mp->n_entries - 1) mp->n_entries--;\n\
    return true;\n\
}}",
                m = m, k = kc, e = ent, hash = hash, eq = eq
            );
        }
        (ent, m)
    }

    /// `?Box[T]`, `?rc[T]`, `?arc[T]`, `?weak[T]` are a single pointer; none is NULL.
    fn niche(&self, t: &Ty) -> bool {
        matches!(t, Ty::Option(i) if matches!(**i, Ty::Box(_) | Ty::Rc(_) | Ty::Arc(_) | Ty::Weak(_)))
    }
    /// C condition: the optional value at `p` holds a value.
    fn osome(&self, t: &Ty, p: &str) -> String {
        if self.niche(t) { format!("({} != NULL)", p) } else { format!("{}.some", p) }
    }
    /// The payload of the optional value at `p` (an lvalue).
    fn oval(&self, t: &Ty, p: &str) -> String {
        if self.niche(t) { p.to_string() } else { format!("{}.val", p) }
    }
    fn omake(&mut self, t: &Ty, v: &str) -> String {
        if self.niche(t) {
            format!("({})", v)
        } else {
            let c = self.cty(t);
            format!("(({}){{ 1, {} }})", c, v)
        }
    }
    /// Statement storing `some(v)` into `p`.
    fn oset(&self, t: &Ty, p: &str, v: &str) -> String {
        if self.niche(t) { format!("{} = {};", p, v) } else { format!("{}.some = 1; {}.val = {};", p, p, v) }
    }

    fn type_id(&mut self, t: &Ty) -> u64 {
        let n = self.type_ids.len() as u64 + 1;
        *self.type_ids.entry(t.clone()).or_insert(n)
    }

    fn param_cty(&mut self, m: Mode, t: &Ty) -> String {
        let c = self.cty(t);
        match m {
            Mode::Borrow if self.is_copy(t) => c,
            Mode::Borrow | Mode::Inout => format!("{}*", c),
            Mode::Sink => c,
        }
    }

    fn ret_cty(&mut self, t: &Ty) -> String {
        match t {
            Ty::Unit | Ty::Never => "void".into(),
            _ => self.cty(t),
        }
    }

    /// A loop that doesn't poll for preemption: no nested loops, and calls only
    /// to bounded functions. One that runs forever is caught by the monitor's
    /// fallback (it is treated like a long C call).
    fn leaf_loop(&self, cond: Option<&TExpr>, body: &TBlock) -> bool {
        leaf_body(&self.bounded, cond, body)
    }

    /// A new type over a number, as `Some(number type)`.
    fn numeric_newtype(&self, t: &Ty) -> Option<Ty> {
        match t {
            Ty::Adt(d, args) if self.prog.def(*d).newtype => self.prog.field_tys(*d, args).into_iter().next().map(|f| f.1).filter(|i| i.is_numeric()),
            _ => None,
        }
    }

    fn zero(&mut self, t: &Ty) -> String {
        let c = self.cty(t);
        if self.is_scalar(t) {
            format!("(({})0)", c)
        } else {
            format!("(({}){{0}})", c)
        }
    }

    // ------------------------------------------------------------------
    // Function instances
    // ------------------------------------------------------------------

    fn instance(&mut self, f: FuncId, targs: Vec<Ty>) -> String {
        if let Some(n) = self.instances.get(&(f, targs.clone())) {
            return n.clone();
        }
        if targs.iter().any(|t| ty_depth(t) > MAX_TYPE_DEPTH) {
            if self.runaway.is_none() {
                let name = self.prog.func(f).name.clone();
                self.runaway = Some(format!("instantiating `{}` never ends: it calls itself with a bigger type argument each time (like `{}[Vec[T]]` inside `{}[T]`)", name, name, name));
            }
            return "gojo_runaway".into();
        }
        let fd = self.prog.func(f);
        let pkg = &self.prog.packages[fd.pkg as usize].name;
        let mut n = match fd.recv_key {
            Some(k) => format!("k_{}_{}_{}", pkg, self.key_name(k), fd.name),
            None => format!("k_{}_{}", pkg, fd.name),
        };
        if fd.is_extern_c_export {
            n = fd.name.clone();
        }
        for t in &targs {
            n.push_str("__");
            n.push_str(&self.mangle(t));
        }
        if n.len() > 150 {
            n = format!("{}_{}", &n[..60], self.instances.len());
        }
        self.instances.insert((f, targs.clone()), n.clone());
        self.work.push((f, targs, n.clone()));
        n
    }

    fn key_name(&self, k: TyKey) -> String {
        match k {
            TyKey::Adt(d) | TyKey::Iface(d) => self.prog.def(d).name.clone(),
            TyKey::Int(i) => i.name().to_string(),
            TyKey::Str => "str".into(),
            TyKey::String => "String".into(),
            k => format!("{:?}", k),
        }
    }

    fn drain(&mut self) {
        while let Some((f, targs, name)) = self.work.pop() {
            if self.runaway.is_some() {
                return;
            }
            self.gen_function(f, targs, name);
        }
    }

    fn fn_signature(&mut self, f: FuncId, subst: &HashMap<TParamId, Ty>, name: &str) -> (String, Vec<(String, Mode, Ty)>) {
        let fd = self.prog.func(f).clone();
        let mut ps = vec![];
        if let Some(r) = &fd.recv {
            ps.push((r.name.clone(), r.mode, r.ty.subst(subst)));
        }
        for p in &fd.params {
            ps.push((p.name.clone(), p.mode, p.ty.subst(subst)));
        }
        let ret = fd.ret.subst(subst);
        let rc = if fd.yields.is_some() {
            let c = self.cty(&ret);
            format!("{}*", c)
        } else {
            self.ret_cty(&ret)
        };
        let mut s = String::new();
        // small functions get an inline hint: overflow and bounds checks make them
        // look bigger to the C compiler than they are
        let small = self.tf.get(&f).map(|tf| {
            let mut n = 0usize;
            let mut b = tf.body.clone();
            crate::check::body::zonk_walk_block(&mut b, &mut |_| n += 1);
            n <= 60
        }).unwrap_or(false);
        let linkage = if fd.is_extern_c_export { "" } else if small { "static inline " } else { "static " };
        let _ = write!(s, "{}{} {}(", linkage, rc, name);
        if ps.is_empty() {
            s.push_str("void");
        }
        for (i, (pn, m, t)) in ps.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            let c = self.param_cty(*m, t);
            let _ = write!(s, "{} p_{}", c, sanitize(pn));
        }
        s.push(')');
        (s, ps)
    }

    fn gen_function(&mut self, f: FuncId, targs: Vec<Ty>, name: String) {
        let fd = self.prog.func(f).clone();
        let subst: HashMap<TParamId, Ty> = fd.tparams.iter().copied().zip(targs.iter().cloned()).collect();
        if let FuncKind::ExternC { lib } = &fd.kind {
            // lazily resolved function pointer
            let ret = fd.ret.subst(&subst);
            let rc = self.ret_cty(&ret);
            let mut ptys = vec![];
            for p in &fd.params {
                let t = p.ty.subst(&subst);
                ptys.push(self.param_cty(p.mode, &t));
            }
            let plist = if ptys.is_empty() { "void".to_string() } else { ptys.join(", ") };
            let args: Vec<String> = (0..ptys.len()).map(|i| format!("a{}", i)).collect();
            let params: Vec<String> = ptys.iter().enumerate().map(|(i, t)| format!("{} a{}", t, i)).collect();
            let _ = writeln!(self.protos, "static {} {}({});", rc, name, plist);
            let (call_target, setup) = match lib {
                Some(l) if !l.starts_with("static:") => {
                    // a library loaded at run time: no build-time dependency on it
                    let setup = format!("    static {} (*fp)({});\n    if (!fp) fp = ({} (*)({}))gojo_dlsym({}, {});\n", rc, plist, rc, plist, c_string(l), c_string(&fd.name));
                    ("fp".to_string(), setup)
                }
                _ => {
                    // libc, or a library linked statically: an ordinary call the linker
                    // resolves. The C name is ours, bound to the real symbol, so it
                    // can't clash with a system header's declaration.
                    if let Some(l) = lib.as_ref().and_then(|l| l.strip_prefix("static:")) {
                        let _ = writeln!(self.protos, "// gojo-link: -l{}", l);
                    }
                    let xn = format!("kx_{}", name);
                    let _ = writeln!(self.protos, "extern {} {}({}) __asm__(GOJO_ASM_NAME({}));", rc, xn, plist, c_string(&fd.name));
                    (xn, String::new())
                }
            };
            let _ = writeln!(
                self.bodies,
                "static {} {}({}) {{\n{}    gojo_ffi_enter();\n    {}{}({});\n    gojo_ffi_exit();{}\n}}",
                rc,
                name,
                if params.is_empty() { "void".to_string() } else { params.join(", ") },
                setup,
                if rc == "void" { String::new() } else { format!("{} r = ", rc) },
                call_target,
                args.join(", "),
                if rc == "void" { "" } else { "\n    return r;" }
            );
            return;
        }
        let tf = match self.tf.get(&f) {
            Some(t) => *t,
            None => panic!("no body for {}", fd.name),
        };
        let (sig, ps) = self.fn_signature(f, &subst, &name);
        let _ = writeln!(self.protos, "{};", sig);
        let ret = if fd.yields.is_some() { fd.ret.subst(&subst) } else { fd.ret.subst(&subst) };
        let mut fs = Fs { tf, subst, out: String::new(), decls: String::new(), ind: 1, scopes: vec![], repr: HashMap::new(), flagged: HashSet::new(), flags: HashMap::new(), ret, yields: fd.yields.is_some(), err_mode: None, loop_labels: vec![], lb: None, line: (u32::MAX, 0), tscopes: HashMap::new(), bce: vec![], bce_steady: HashMap::new(), bce_floor: Default::default(), dinfo: None, site: None, used_flags: HashSet::new(), jd_done: HashSet::new(), fd_done: HashSet::new(), static_cur: HashMap::new() };
        collect_moves_block(&tf.body, &mut fs.flagged, tf);
        fs.scopes.push(Scope { kind: SK::Func, items: vec![], wait: None });
        let mut lids = vec![];
        if let Some(r) = tf.recv {
            lids.push(r);
        }
        lids.extend(tf.params.iter().copied());
        let sinks: Vec<LocalId> = lids.iter().zip(ps.iter()).filter(|(_, p)| p.1 == Mode::Sink).map(|(l, _)| *l).collect();
        for (lid, (pn, m, t)) in lids.iter().zip(ps.iter()) {
            let cn = format!("p_{}", sanitize(pn));
            let byptr = match m {
                Mode::Borrow => !self.is_copy(t),
                Mode::Inout => true,
                Mode::Sink => false,
            };
            fs.repr.insert(*lid, if byptr { Repr::Ptr(cn.clone()) } else { Repr::Val(cn.clone()) });
            if *m == Mode::Sink {
                self.declare_flags(&mut fs, *lid);
                if self.needs_drop(t) {
                    fs.scopes[0].items.push(Cl::DropLocal(*lid));
                }
            }
        }
        let body = tf.body.clone();
        // (on the copy codegen walks: the analysis keys its results by address)
        self.analyze_drops(&mut fs, &body, &sinks);
        fs.bce_floor = std::rc::Rc::new(self.bce_floors(&mut fs));
        self.line_directive(&mut fs, body.span);
        // the prelude's lazy-chain steps only call the closure they are given,
        // which polls in its own body if it can recurse
        let lazy_step = self.prog.func(f).pkg == self.prog.prelude && self.prog.func(f).name.starts_with("__lazy");
        if self.may_recurse.contains(&f) && !lazy_step {
            self.emit(&mut fs, "GOJO_POLL();");
        }
        self.fn_body(&mut fs, &body);
        if let Some(info) = &fs.dinfo {
            // a join drop not emitted would leak its value
            let missing = info.join_drops.keys().filter(|k| !fs.jd_done.contains(k)).count() + info.after_stmt.keys().filter(|k| !fs.fd_done.contains(k)).count();
            assert!(missing == 0, "internal error: {} drops at the ends of branches or at last uses were not emitted in {}", missing, fd.name);
        }
        let out = reuse_boxes(&resolve_flags(&fs.out, &fs.used_flags));
        let _ = writeln!(self.bodies, "{} {{\n{}{}}}\n{}", sig, fs.decls, out, C_LINE_MARK);
    }

    fn fn_body(&mut self, fs: &mut Fs, body: &TBlock) {
        let ret = fs.ret.clone();
        self.push_scope(fs, SK::Block);
        for (i, s) in body.stmts.iter().enumerate() {
            self.stmt(fs, s);
            self.free_after(fs, body, i);
        }
        if fs.yields {
            self.at_site(fs, drops::Site::block(body), |g, fs| g.pop_scope(fs));
            let l = self.loc(body.span);
            self.emit(fs, &format!("gojo_panic(\"projection ended without yielding\", {});", l));
            return;
        }
        match &body.value {
            Some(v) if ret != Ty::Unit && ret != Ty::Never && v.ty != Ty::Never => {
                let rc = self.cty(&ret);
                let val = self.expr(fs, v);
                let r = self.tmp();
                self.emit(fs, &format!("{} {} = {};", rc, r, val));
                fs.err_mode = match &ret {
                    Ty::Result(..) => Some(format!("!{}.ok", r)),
                    Ty::Option(_) => Some(format!("!{}", self.osome(&ret, &r))),
                    _ => None,
                };
                self.at_site(fs, drops::Site::block(body), |g, fs| g.cleanup_to(fs, 0));
                fs.err_mode = None;
                self.emit(fs, &format!("return {};", r));
                fs.scopes.pop();
            }
            Some(v) => {
                let s = self.expr(fs, v);
                self.discard(fs, &s, &v.ty, v.is_place());
                self.at_site(fs, drops::Site::block(body), |g, fs| {
                    g.pop_scope(fs);
                    g.cleanup_to(fs, 0)
                });
                self.end_without_value(fs, &ret, body.span);
            }
            None => {
                self.at_site(fs, drops::Site::block(body), |g, fs| {
                    g.pop_scope(fs);
                    g.cleanup_to(fs, 0)
                });
                self.end_without_value(fs, &ret, body.span);
            }
        }
    }

    /// The checker proves that a function returning a value never reaches its
    /// end; if that proof is ever wrong, stop instead of returning garbage. (The C
    /// compiler removes this wherever the end is unreachable.)
    fn end_without_value(&mut self, fs: &mut Fs, ret: &Ty, sp: Span) {
        if *ret != Ty::Unit {
            let l = self.loc(sp);
            self.emit(fs, &format!("gojo_panic(\"internal error: function ended without returning a value\", {});", l));
        }
    }

    // ------------------------------------------------------------------
    // Emission helpers & scopes
    // ------------------------------------------------------------------

    /// `#line` for the Gojo line `sp` starts on, when it differs from the last
    /// one: debuggers and sanitizer reports then show Gojo source lines.
    fn line_directive(&mut self, fs: &mut Fs, sp: Span) {
        if (sp.file as usize) >= self.sm.files.len() {
            return;
        }
        let f = self.sm.file(sp.file);
        let (l, _) = f.line_col(sp.lo);
        if fs.line.0 != sp.file {
            // the file once; `emit` repeats the line before every C line
            let path = std::fs::canonicalize(&f.path).map(|p| p.display().to_string()).unwrap_or_else(|_| f.path.clone());
            fs.out.push_str(&format!("#line {} {}\n", l, c_string(&path)));
        }
        fs.line = (sp.file, l as u32);
    }

    fn emit(&mut self, fs: &mut Fs, line: &str) {
        // One Gojo statement is often several C lines: each gets the Gojo
        // line, or the C compiler would count on from it.
        for part in line.split('\n') {
            if fs.line.0 != u32::MAX {
                let _ = writeln!(fs.out, "#line {}", fs.line.1);
            }
            for _ in 0..fs.ind {
                fs.out.push_str("    ");
            }
            fs.out.push_str(part);
            fs.out.push('\n');
        }
    }

    fn push_scope(&mut self, fs: &mut Fs, kind: SK) {
        fs.scopes.push(Scope { kind, items: vec![], wait: None });
    }

    fn pop_scope(&mut self, fs: &mut Fs) {
        let top = fs.scopes.len() - 1;
        self.emit_scope_cleanups_at(fs, top, false);
        fs.scopes.pop();
    }

    /// The scope for the value of a branch of type `t`.
    fn push_value_scope(&mut self, fs: &mut Fs, t: &Ty) {
        self.push_scope(fs, if self.prog.contains_view(&t) { SK::Tail } else { SK::Stmt });
    }

    /// Like `pop_scope`, but returns the branch-value temporaries (`DropLater`)
    /// instead of dropping them; the caller hands them to the enclosing scope.
    fn pop_scope_keeping_later(&mut self, fs: &mut Fs) -> Vec<Cl> {
        let items = std::mem::take(&mut fs.scopes.last_mut().unwrap().items);
        let (later, now) = items.into_iter().partition(|c| matches!(c, Cl::DropLater(..)));
        fs.scopes.last_mut().unwrap().items = now;
        self.pop_scope(fs);
        later
    }

    fn emit_scope_cleanups(&mut self, fs: &mut Fs, idx: usize) {
        self.emit_scope_cleanups_at(fs, idx, false)
    }

    /// `early`: leaving by `return`, `break`, `continue` or `try`.
    fn emit_scope_cleanups_at(&mut self, fs: &mut Fs, idx: usize, early: bool) {
        if let Some(g) = fs.scopes[idx].wait.clone() {
            if early {
                self.emit(fs, &format!("gojo_group_cancel({});", g));
            }
            self.emit(fs, &format!("gojo_group_wait({});", g));
        }
        let items: Vec<Cl> = fs.scopes[idx].items.iter().rev().cloned().collect();
        for it in items {
            self.emit_cleanup(fs, &it);
        }
    }

    /// Emit cleanups for all scopes from the top down to (and including) `idx`.
    fn cleanup_to(&mut self, fs: &mut Fs, idx: usize) {
        let n = fs.scopes.len();
        for i in (idx..n).rev() {
            self.emit_scope_cleanups_at(fs, i, true);
        }
    }

    fn emit_cleanup(&mut self, fs: &mut Fs, c: &Cl) {
        match c {
            Cl::DropLocal(l) => {
                let t = self.local_ty(fs, *l);
                self.drop_path(fs, *l, vec![], &t);
            }
            Cl::DropTemp(n, t) => {
                let d = self.drop_fn(t);
                self.emit(fs, &format!("{}(&{});", d, n));
            }
            Cl::DropFlag(n, t, flag) => {
                let d = self.drop_fn(t);
                self.emit(fs, &format!("if ({}) {}(&{});", flag, d, n));
            }
            Cl::DropLater(n, t, flag) => {
                // the flag lives as long as the function: clear it for the next
                // time the statement runs (in a loop) and takes another branch
                let d = self.drop_fn(t);
                self.emit(fs, &format!("if ({}) {{ {}(&{}); {} = 0; }}", flag, d, n, flag));
            }
            Cl::Defer(b) => {
                let site = fs.site.take();
                // (deferred code emitted here runs only on this way out:
                // what it says about indexes stays on it)
                let facts = fs.bce.clone();
                match self.defer_guard(fs, b) {
                    Some(f) => self.emit(fs, &format!("if ({}) {{", f)),
                    None => self.emit(fs, "{"),
                }
                fs.ind += 1;
                // the deferred code can't leave its own block, so it must not see the
                // enclosing scopes: their cleanups include this very `defer`
                let outer = std::mem::replace(&mut fs.scopes, vec![Scope { kind: SK::Func, items: vec![], wait: None }]);
                self.push_scope(fs, SK::Block);
                for s in &b.stmts {
                    self.stmt(fs, s);
                }
                self.pop_scope(fs);
                fs.scopes = outer;
                fs.ind -= 1;
                self.emit(fs, "}");
                fs.site = site;
                fs.bce = facts;
            }
            Cl::ErrDefer(b) => {
                if let Some(cond) = fs.err_mode.clone() {
                    let site = fs.site.take();
                    let facts = fs.bce.clone();
                    let cond = match self.defer_guard(fs, b) {
                        Some(f) => format!("({}) && {}", cond, f),
                        None => cond,
                    };
                    self.emit(fs, &format!("if ({}) {{", cond));
                    fs.ind += 1;
                    let outer = std::mem::replace(&mut fs.scopes, vec![Scope { kind: SK::Func, items: vec![], wait: None }]);
                    let saved_mode = fs.err_mode.take();
                    self.push_scope(fs, SK::Block);
                    for s in &b.stmts {
                        self.stmt(fs, s);
                    }
                    self.pop_scope(fs);
                    fs.scopes = outer;
                    fs.err_mode = saved_mode;
                    fs.ind -= 1;
                    self.emit(fs, "}");
                    fs.site = site;
                    fs.bce = facts;
                }
            }
            Cl::Unlock(m) => self.emit(fs, &format!("gojo_mutex_unlock({});", m)),
            Cl::UnlockRw(m, w) => self.emit(fs, &format!("gojo_rw_{}unlock({});", if *w { "w" } else { "r" }, m)),
        }
    }

    fn register(&mut self, fs: &mut Fs, c: Cl) {
        fs.scopes.last_mut().unwrap().items.push(c);
    }

    /// Register a cleanup in the innermost statement scope (or the innermost scope).
    fn register_stmt(&mut self, fs: &mut Fs, c: Cl) {
        fs.scopes.last_mut().unwrap().items.push(c);
    }

    fn local_ty(&self, fs: &Fs, l: LocalId) -> Ty {
        fs.tf.locals[l as usize].ty.subst(&fs.subst)
    }

    fn sub(&self, fs: &Fs, t: &Ty) -> Ty {
        t.subst(&fs.subst)
    }

    fn local_place(&self, fs: &Fs, l: LocalId) -> String {
        match fs.repr.get(&l) {
            Some(Repr::Val(n)) => n.clone(),
            Some(Repr::Ptr(n)) => format!("(*{})", n),
            None => format!("/*missing local {}*/l{}", l, l),
        }
    }

    fn local_cname(&self, fs: &Fs, l: LocalId) -> String {
        format!("l{}_{}", l, sanitize(&fs.tf.locals[l as usize].name))
    }

    /// A `defer` that consumes a linear variable runs only while the variable
    /// still holds its value (spec 5.7): the drop flag that says so.
    fn defer_guard(&mut self, fs: &mut Fs, b: &TBlock) -> Option<String> {
        let g = crate::linear::defer_guards(self.prog, fs.tf, b);
        if g.len() != 1 {
            return None;
        }
        // all of the place is there: neither it, nor a part of it, nor
        // something it is part of has been moved
        let mut fl: Vec<String> = fs.flags.iter().filter(|(p, _)| crate::linear::paths_overlap(p, &g[0])).map(|(_, f)| f.clone()).collect();
        if fl.is_empty() {
            return None;
        }
        fl.sort();
        for f in &fl {
            fs.used_flags.insert(f.clone());
        }
        Some(fl.join(" && "))
    }

    fn declare_flags(&mut self, fs: &mut Fs, l: LocalId) {
        let paths: Vec<Vec<usize>> = fs.flagged.iter().filter(|(r, _)| *r == l).map(|(_, p)| p.clone()).collect();
        let mut paths = paths;
        paths.sort();
        for p in paths {
            let n = format!("df{}_{}", l, p.iter().map(|x| x.to_string()).collect::<Vec<_>>().join("_"));
            self.emit(fs, &format!("{}bool {} = 1;", flag_mark(&n), n));
            fs.flags.insert((l, p), n);
        }
    }

    fn drop_path(&mut self, fs: &mut Fs, l: LocalId, path: Vec<usize>, t: &Ty) {
        if !self.needs_drop(t) {
            return;
        }
        let mut flag = fs.flags.get(&(l, path.clone())).cloned();
        if flag.is_some() {
            // what the analysis knows at this site makes the test unneeded
            let known = fs.dinfo.as_ref().map_or(drops::Known::Unknown, |i| i.known(fs.site, &(l, path.clone())));
            match known {
                drops::Known::Dead | drops::Known::Moved => return,
                drops::Known::Live => flag = None,
                drops::Known::Unknown => {
                    fs.used_flags.insert(flag.clone().unwrap());
                }
            }
        }
        let has_sub = fs.flags.keys().any(|(r, p)| *r == l && p.len() > path.len() && p.starts_with(&path));
        let mut place = self.local_place(fs, l);
        let mut cur = fs.tf.locals[l as usize].ty.subst(&fs.subst);
        for &i in &path {
            if let Ty::Adt(d, args) = &cur {
                let fsx = self.prog.field_tys(*d, args);
                place = format!("{}.f_{}", place, fsx[i].0);
                cur = fsx[i].1.clone();
            }
        }
        if let Some(f) = &flag {
            self.emit(fs, &format!("if ({}) {{", f));
            fs.ind += 1;
        }
        if has_sub {
            if let Ty::Adt(d, args) = t {
                let fsx = self.prog.field_tys(*d, args);
                for (i, (_, ft)) in fsx.iter().enumerate() {
                    let mut p2 = path.clone();
                    p2.push(i);
                    self.drop_path(fs, l, p2, ft);
                }
            }
        } else {
            let d = self.drop_fn(t);
            self.emit(fs, &format!("{}(&{});", d, place));
        }
        if flag.is_some() {
            fs.ind -= 1;
            self.emit(fs, "}");
        }
    }

    /// After assigning a path, mark it and its sub-paths live.
    fn set_live(&mut self, fs: &mut Fs, l: LocalId, path: &[usize]) {
        let names: Vec<String> = fs.flags.iter().filter(|((r, p), _)| *r == l && p.starts_with(path)).map(|(_, n)| n.clone()).collect();
        for n in names {
            self.set_flag(fs, &n, true);
        }
    }

    /// Set a drop flag: `live` where the value is (re)initialized, not where it
    /// is moved. Dropped from the C if no drop ends up testing the flag.
    fn set_flag(&mut self, fs: &mut Fs, flag: &str, live: bool) {
        self.emit(fs, &format!("{}{} = {};", flag_mark(flag), flag, live as u8));
    }

    /// Work out where drop flags have known values (drops.rs) for `body`, in
    /// which `owned` (the owned parameters) are live from the start.
    fn analyze_drops(&mut self, fs: &mut Fs, body: &TBlock, owned: &[LocalId]) {
        let tf = fs.tf;
        let facts: Vec<drops::LocalFact> = (0..tf.locals.len() as LocalId)
            .map(|l| {
                let t = self.local_ty(fs, l);
                let borrows = matches!(tf.locals[l as usize].kind, LocalKind::Ref { .. }) || self.prog.may_borrow(&t);
                drops::LocalFact { copy: self.is_copy(&t), needs_drop: self.needs_drop(&t), silent: self.prog.drop_silent(&t), borrows }
            })
            .collect();
        let prog = self.prog;
        let info = drops::analyze(body, &fs.flagged, &facts, owned, &|t: &Ty| prog.may_borrow(&t.subst(&fs.subst)));
        fs.dinfo = Some(std::rc::Rc::new(info));
    }

    /// Drops moved to the end of a branch (drops.rs): the values `key`'s
    /// branch `branch` didn't move, where other branches did.
    fn join_drops(&mut self, fs: &mut Fs, key: usize, branch: usize) -> Vec<LocalId> {
        let ls = fs.dinfo.as_ref().and_then(|i| i.join_drops.get(&(key, branch)).cloned()).unwrap_or_default();
        if !ls.is_empty() {
            fs.jd_done.insert((key, branch));
        }
        ls
    }

    /// Free what statement `i` of `b` used last (drops.rs): its drop at the
    /// end of the scope goes, so nothing leaving the scope later drops it again.
    fn free_after(&mut self, fs: &mut Fs, b: &TBlock, i: usize) {
        let key = (b as *const TBlock as usize, i);
        let Some(ls) = fs.dinfo.as_ref().and_then(|d| d.after_stmt.get(&key).cloned()) else { return };
        fs.fd_done.insert(key);
        for l in ls {
            let mut registered = false;
            for sc in fs.scopes.iter_mut().rev() {
                if let Some(pos) = sc.items.iter().position(|c| matches!(c, Cl::DropLocal(x) if *x == l)) {
                    sc.items.remove(pos);
                    registered = true;
                    break;
                }
            }
            if !registered {
                continue;
            }
            let t = self.local_ty(fs, l);
            let d = self.drop_fn(&t);
            let p = self.local_place(fs, l);
            self.emit(fs, &format!("{}(&{}); /* last use */", d, p));
            if let Some(f) = fs.flags.get(&(l, vec![])).cloned() {
                self.set_flag(fs, &f, false);
            }
        }
    }

    fn emit_join_drops(&mut self, fs: &mut Fs, ls: &[LocalId]) {
        for &l in ls {
            let t = self.local_ty(fs, l);
            let d = self.drop_fn(&t);
            let p = self.local_place(fs, l);
            self.emit(fs, &format!("{}(&{}); /* moved on another branch */", d, p));
            if let Some(f) = fs.flags.get(&(l, vec![])).cloned() {
                self.set_flag(fs, &f, false);
            }
        }
    }

    /// Run `f` with drops being emitted for `site`.
    fn at_site<R>(&mut self, fs: &mut Fs, site: drops::Site, f: impl FnOnce(&mut Self, &mut Fs) -> R) -> R {
        let saved = fs.site.replace(site);
        let r = f(self, fs);
        fs.site = saved;
        r
    }

    fn materialize(&mut self, fs: &mut Fs, s: &str, t: &Ty, owned: bool) -> String {
        let c = self.cty(t);
        let n = self.tmp();
        if owned && self.needs_drop(t) && fs.scopes.last().unwrap().kind == SK::Tail {
            let f = self.tmp();
            let _ = writeln!(fs.decls, "    {} {}; bool {} = 0;", c, n, f);
            self.emit(fs, &format!("{} = {}; {} = 1;", n, s, f));
            self.register(fs, Cl::DropLater(n.clone(), t.clone(), f));
            return n;
        }
        self.emit(fs, &format!("{} {} = {};", c, n, s));
        if owned && self.needs_drop(t) {
            self.register_stmt(fs, Cl::DropTemp(n.clone(), t.clone()));
        }
        n
    }

    /// Owned rvalue result discarded.
    fn discard(&mut self, fs: &mut Fs, s: &str, t: &Ty, is_place: bool) {
        if is_place || matches!(t, Ty::Unit | Ty::Never) {
            if !s.is_empty() && s != "0" && !is_place && !s.starts_with('t') {
                self.emit(fs, &format!("(void)({});", s));
            }
            return;
        }
        if self.needs_drop(t) {
            let c = self.cty(t);
            let n = self.tmp();
            let d = self.drop_fn(t);
            self.emit(fs, &format!("{{ {} {} = {}; {}(&{}); }}", c, n, s, d, n));
        } else if !s.starts_with('t') {
            self.emit(fs, &format!("(void)({});", s));
        }
    }

    // ------------------------------------------------------------------
    // Statements
    // ------------------------------------------------------------------

    fn stmt(&mut self, fs: &mut Fs, s: &TStmt) {
        if let Some(sp) = stmt_span(s) {
            self.line_directive(fs, sp);
        }
        // (what a condition said about a place ends where it may change)
        self.bce_kill(fs, s);
        match s {
            TStmt::Let { local, init } => {
                self.push_scope(fs, SK::Stmt);
                let t = self.local_ty(fs, *local);
                let mut v = if matches!(init.kind, TK::DefaultOf) { self.default_value(fs, &t) } else { self.expr(fs, init) };
                let c = self.cty(&t);
                // the value may still read the statement's temporaries: compute it
                // before they are dropped
                if init.ty != Ty::Never && fs.scopes.last().map_or(false, |sc| !sc.items.is_empty()) {
                    let tv = self.tmp();
                    self.emit(fs, &format!("{} {} = {};", c, tv, v));
                    v = tv;
                }
                self.pop_scope(fs);
                let n = self.local_cname(fs, *local);
                if init.ty == Ty::Never {
                    let z = self.zero(&t);
                    self.emit(fs, &format!("{} {} = {};", c, n, z));
                } else {
                    self.emit(fs, &format!("{} {} = {};", c, n, v));
                }
                fs.repr.insert(*local, Repr::Val(n));
                self.declare_flags(fs, *local);
                if self.needs_drop(&t) {
                    self.register(fs, Cl::DropLocal(*local));
                }
                if let Some(f) = self.bce_let_fact(fs, *local, init) {
                    fs.bce.push(f);
                }
            }
            TStmt::LetRef { local, place } => {
                let t = self.local_ty(fs, *local);
                let c = self.cty(&t);
                let n = self.local_cname(fs, *local);
                // temporaries created for the place live as long as the reference
                let p = self.lval_scoped(fs, place);
                self.emit(fs, &format!("{} *{} = &{};", c, n, p));
                fs.repr.insert(*local, Repr::Ptr(n));
            }
            TStmt::Expr(e) => {
                self.push_scope(fs, SK::Stmt);
                let v = self.expr(fs, e);
                let t = self.sub(fs, &e.ty);
                self.discard(fs, &v, &t, e.is_place());
                self.pop_scope(fs);
                // what the rest of the block knows from it: `if i >= n {
                // return }`, `assert(i < n)`
                let after = self.bce_after(fs, e);
                fs.bce.extend(after);
            }
            TStmt::Assign { place, op, value, span } => {
                self.push_scope(fs, SK::Stmt);
                self.assign(fs, place, *op, value, *span, drops::Site::stmt(s));
                self.pop_scope(fs);
            }
            TStmt::MapInsert { map, key, value } => {
                self.push_scope(fs, SK::Stmt);
                let mt = self.sub(fs, &map.ty);
                let (k, v) = match &mt {
                    Ty::Map(k, v) => ((**k).clone(), (**v).clone()),
                    _ => unreachable!(),
                };
                let kv = self.expr(fs, key);
                let vv = self.expr(fs, value);
                let m = self.lval(fs, map);
                let old = self.map_insert(fs, &format!("&{}", m), &k, &v, &kv, &vv);
                let ot = Ty::opt(v.clone());
                self.discard(fs, &old, &ot, false);
                self.pop_scope(fs);
            }
            TStmt::Return(e, _) if fs.lb.is_some() => {
                self.push_scope(fs, SK::Stmt);
                let ret = fs.ret.clone();
                match e {
                    Some(e) if ret != Ty::Unit && ret != Ty::Never && e.ty != Ty::Never => {
                        let v = self.expr(fs, e);
                        self.emit_return(fs, &v);
                    }
                    // `return return x`: the value never arrives
                    Some(e) if e.ty == Ty::Never => {
                        let v = self.expr(fs, e);
                        self.discard(fs, &v, &Ty::Never, e.is_place());
                        self.emit(fs, "__builtin_unreachable();");
                    }
                    Some(e) => {
                        let v = self.expr(fs, e);
                        let t = self.sub(fs, &e.ty);
                        self.discard(fs, &v, &t, e.is_place());
                        self.emit_return(fs, "");
                    }
                    None => self.emit_return(fs, ""),
                }
                fs.scopes.pop();
            }
            TStmt::Return(e, _) => {
                self.push_scope(fs, SK::Stmt);
                let ret = fs.ret.clone();
                match e {
                    Some(e) if ret != Ty::Unit && ret != Ty::Never && e.ty != Ty::Never => {
                        let v = self.expr(fs, e);
                        let rc = self.cty(&ret);
                        let r = self.tmp();
                        self.emit(fs, &format!("{} {} = {};", rc, r, v));
                        fs.err_mode = match &ret {
                            Ty::Result(..) => Some(format!("!{}.ok", r)),
                            Ty::Option(_) => Some(format!("!{}", self.osome(&ret, &r))),
                            _ => None,
                        };
                        self.at_site(fs, drops::Site::stmt(s), |g, fs| g.cleanup_to(fs, 0));
                        fs.err_mode = None;
                        self.emit(fs, &format!("return {};", r));
                    }
                    // `return return x`: the value never arrives
                    Some(e) if e.ty == Ty::Never => {
                        let v = self.expr(fs, e);
                        self.discard(fs, &v, &Ty::Never, e.is_place());
                        self.emit(fs, "__builtin_unreachable();");
                    }
                    Some(e) => {
                        let v = self.expr(fs, e);
                        let t = self.sub(fs, &e.ty);
                        self.discard(fs, &v, &t, e.is_place());
                        self.at_site(fs, drops::Site::stmt(s), |g, fs| g.cleanup_to(fs, 0));
                        self.emit(fs, "return;");
                    }
                    None => {
                        self.at_site(fs, drops::Site::stmt(s), |g, fs| g.cleanup_to(fs, 0));
                        self.emit(fs, "return;");
                    }
                }
                fs.scopes.pop();
            }
            TStmt::Yield(e, _) => {
                self.push_scope(fs, SK::Stmt);
                match e {
                    Some(e) => {
                        let p = self.lval(fs, e);
                        let t = self.sub(fs, &e.ty);
                        let c = self.cty(&t);
                        let r = self.tmp();
                        self.emit(fs, &format!("{} *{} = &{};", c, r, p));
                        self.at_site(fs, drops::Site::stmt(s), |g, fs| g.cleanup_to(fs, 0));
                        self.emit(fs, &format!("return {};", r));
                    }
                    None => {
                        self.at_site(fs, drops::Site::stmt(s), |g, fs| g.cleanup_to(fs, 0));
                        self.emit(fs, "return NULL;");
                    }
                }
                fs.scopes.pop();
            }
            TStmt::Break(id, _) | TStmt::Continue(id, _) if !fs.loop_labels.iter().any(|l| l.0 == *id) && fs.lb.is_some() => {
                // leaving a push-style loop body: tell `each` to stop, and the
                // caller where to go next
                let is_break = matches!(s, TStmt::Break(..));
                let lb = fs.lb.clone().unwrap();
                // the control word is set after cleanups: a `defer` may run its own push loop
                if *id == lb.id {
                    self.cleanup_to(fs, 0);
                    if is_break {
                        self.emit(fs, &format!("{} = {};", lb.ctl, CTL_STOP));
                    }
                    let f = self.flow(is_break);
                    self.emit(fs, &format!("return {};", f));
                } else {
                    let code = if is_break { ctl_break(*id) } else { ctl_continue(*id) };
                    self.cleanup_to(fs, 0);
                    self.emit(fs, &format!("{} = {};", lb.ctl, code));
                    let f = self.flow(true);
                    self.emit(fs, &format!("return {};", f));
                }
            }
            TStmt::Break(id, _) | TStmt::Continue(id, _) => {
                let is_break = matches!(s, TStmt::Break(..));
                let idx = fs.scopes.iter().rposition(|s| s.kind == SK::Loop(*id)).unwrap_or(0);
                let (lb, lc) = fs.loop_labels.iter().rev().find(|l| l.0 == *id).map(|l| (l.1.clone(), l.2.clone())).unwrap_or_default();
                if is_break {
                    // leave the loop: run the loop scope's own cleanups too
                    self.at_site(fs, drops::Site::stmt(s), |g, fs| g.cleanup_to(fs, idx));
                    self.emit(fs, &format!("goto {};", lb));
                } else {
                    self.at_site(fs, drops::Site::stmt(s), |g, fs| g.cleanup_to(fs, idx + 1));
                    self.emit(fs, &format!("goto {};", lc));
                }
            }
            TStmt::Defer(b) => self.register(fs, Cl::Defer(b.clone())),
            TStmt::ErrDefer(b) => self.register(fs, Cl::ErrDefer(b.clone())),
            TStmt::While { id, cond, body } => {
                let (lb, lc) = (self.label(), self.label());
                fs.loop_labels.push((*id, lb.clone(), lc.clone()));
                self.emit(fs, "for (;;) {");
                fs.ind += 1;
                self.push_scope(fs, SK::Loop(*id));
                // an innermost loop without calls is polled by the loop around it
                // (and, if it never ends, the monitor's fallback catches it)
                if !self.leaf_loop(cond.as_ref(), body) {
                    self.emit(fs, "GOJO_POLL();");
                }
                let nb = fs.bce.len();
                if let Some(c) = cond {
                    self.push_scope(fs, SK::Stmt);
                    let cv = self.expr(fs, c);
                    let t = self.tmp();
                    self.emit(fs, &format!("bool {} = {};", t, cv));
                    self.pop_scope(fs);
                    self.emit(fs, &format!("if (!{}) break;", t));
                    // the condition holds where each run of the body starts
                    let pos = self.bce_guard(fs, c, true);
                    fs.bce.extend(pos);
                }
                self.block_stmts(fs, body);
                fs.bce.truncate(nb);
                self.emit(fs, &format!("{}:;", lc));
                fs.scopes.pop();
                fs.ind -= 1;
                self.emit(fs, "}");
                self.emit(fs, &format!("{}:;", lb));
                fs.loop_labels.pop();
            }
            TStmt::For { id, kind, iter, hi, elem, index, mutable: _, body, span } => self.for_loop(fs, *id, *kind, iter, hi.as_ref(), *elem, *index, body, *span),
            TStmt::Send { chan, value } => {
                self.push_scope(fs, SK::Stmt);
                let ct = self.sub(fs, &chan.ty);
                let et = match &ct {
                    Ty::Sender(t) => (**t).clone(),
                    _ => unreachable!(),
                };
                let v = self.expr(fs, value);
                let c = self.expr(fs, chan);
                let ec = self.cty(&et);
                let t = self.tmp();
                self.emit(fs, &format!("{} {} = {};", ec, t, v));
                let l = self.loc(value.span);
                let d = self.cancelled_send_drop(&et, &t);
                self.emit(fs, &format!("if (!gojo_chan_send({}, &{}, {})) {{{}}}", c, t, l, d));
                self.pop_scope(fs);
            }
            TStmt::With { local, place, lock, body } => {
                self.emit(fs, "{");
                fs.ind += 1;
                self.push_scope(fs, SK::Block);
                let t = self.local_ty(fs, *local);
                let c = self.cty(&t);
                let n = self.local_cname(fs, *local);
                if let Some(w) = self.rw_lock_kind(place) {
                    let (m, v) = self.lock_target(fs, place);
                    self.emit(fs, &format!("gojo_rw_{}lock({}->f_lk);", if w { "w" } else { "r" }, m));
                    self.register(fs, Cl::UnlockRw(format!("{}->f_lk", m), w));
                    self.emit(fs, &format!("{} *{} = {};", c, n, v));
                } else if *lock {
                    let (m, v) = self.lock_target(fs, place);
                    self.emit(fs, &format!("gojo_mutex_lock({}->mu);", m));
                    self.register(fs, Cl::Unlock(format!("{}->mu", m)));
                    self.emit(fs, &format!("{} *{} = {};", c, n, v));
                } else {
                    let p = self.lval_scoped(fs, place);
                    self.emit(fs, &format!("{} *{} = &{};", c, n, p));
                }
                fs.repr.insert(*local, Repr::Ptr(n));
                self.block_stmts(fs, body);
                self.pop_scope(fs);
                fs.ind -= 1;
                self.emit(fs, "}");
            }
            TStmt::WithLocks { locals, places, body } => {
                self.emit(fs, "{");
                fs.ind += 1;
                self.push_scope(fs, SK::Block);
                let mut ms = vec![];
                let mut vs = vec![];
                for p in places {
                    let (m, v) = self.lock_target(fs, p);
                    ms.push(format!("{}->mu", m));
                    vs.push(v);
                }
                let arr = self.tmp();
                let l = self.loc(places[0].span);
                self.emit(fs, &format!("gojo_mutex *{}[{}] = {{ {} }};", arr, ms.len(), ms.join(", ")));
                // one order for everyone: by address
                self.emit(fs, &format!("gojo_mutex_lock_all({}, {}, {});", arr, ms.len(), l));
                for m in &ms {
                    self.register(fs, Cl::Unlock(m.clone()));
                }
                for (local, v) in locals.iter().zip(vs) {
                    let t = self.local_ty(fs, *local);
                    let c = self.cty(&t);
                    let n = self.local_cname(fs, *local);
                    self.emit(fs, &format!("{} *{} = {};", c, n, v));
                    fs.repr.insert(*local, Repr::Ptr(n));
                }
                self.block_stmts(fs, body);
                self.pop_scope(fs);
                fs.ind -= 1;
                self.emit(fs, "}");
            }
            TStmt::Select { arms, span } => self.select(fs, arms, *span),
            TStmt::ForEach { call, .. } => self.for_each(fs, call),
            TStmt::ParFor { kind, iter, hi, mutable, indexed, body, .. } => self.par_for(fs, *kind, iter, hi.as_ref(), *mutable, *indexed, body),
        }
    }

    fn flow(&mut self, brk: bool) -> String {
        let d = self.prog.prelude_type("Flow").unwrap();
        let c = self.cty(&Ty::Adt(d, std::rc::Rc::new(vec![])));
        format!("(({}){{ .tag = {} }})", c, if brk { 1 } else { 0 })
    }

    /// Return `v` (empty: no value) from the enclosing function. Inside a
    /// push-style loop body the value goes to the return slot instead.
    fn emit_return(&mut self, fs: &mut Fs, v: &str) {
        let ret = fs.ret.clone();
        let has_val = !v.is_empty() && !matches!(ret, Ty::Unit | Ty::Never);
        let r = if has_val {
            let rc = self.cty(&ret);
            let r = self.tmp();
            self.emit(fs, &format!("{} {} = {};", rc, r, v));
            fs.err_mode = match &ret {
                Ty::Result(..) => Some(format!("!{}.ok", r)),
                Ty::Option(_) => Some(format!("!{}", self.osome(&ret, &r))),
                _ => None,
            };
            Some(r)
        } else {
            None
        };
        match fs.lb.clone() {
            Some(lb) => {
                if let Some(r) = &r {
                    self.emit(fs, &format!("{} = {};", lb.ret, r));
                }
                self.cleanup_to(fs, 0);
                fs.err_mode = None;
                self.emit(fs, &format!("{} = {};", lb.ctl, CTL_RETURN));
                let f = self.flow(true);
                self.emit(fs, &format!("return {};", f));
            }
            None => {
                self.cleanup_to(fs, 0);
                fs.err_mode = None;
                match r {
                    Some(r) => self.emit(fs, &format!("return {};", r)),
                    None => self.emit(fs, "return;"),
                }
            }
        }
    }

    /// `for x in t` with `t.each(body)`: call `each`, then act on what the body asked for.
    fn for_each(&mut self, fs: &mut Fs, call: &TExpr) {
        let ret = fs.ret.clone();
        let has_ret = !matches!(ret, Ty::Unit | Ty::Never);
        let (ctl, retslot) = match fs.lb.clone() {
            Some(lb) => (lb.ctl.clone(), lb.ret.clone()),
            None => {
                let c = self.tmp();
                self.emit(fs, &format!("int {} = 0;", c));
                let r = if has_ret {
                    let rc = self.cty(&ret);
                    let r = self.tmp();
                    self.emit(fs, &format!("{} {};", rc, r));
                    r
                } else {
                    String::new()
                };
                (c, r)
            }
        };
        self.push_scope(fs, SK::Stmt);
        self.pending_lb = Some((format!("&{}", ctl), if has_ret { format!("&{}", retslot) } else { "NULL".into() }));
        let v = self.call_expr(fs, call);
        self.pending_lb = None;
        let _ = v;
        self.pop_scope(fs);
        // dispatch on the control word
        self.emit(fs, &format!("if ({} == {}) {} = 0;", ctl, CTL_STOP, ctl));
        self.emit(fs, &format!("if ({}) {{", ctl));
        fs.ind += 1;
        self.emit(fs, &format!("if ({} == {}) {{", ctl, CTL_RETURN));
        fs.ind += 1;
        match fs.lb.clone() {
            Some(_) => {
                // still inside an outer loop body: keep passing it outward
                self.leave_body(fs, &ctl);
            }
            None => {
                let v = if has_ret { retslot.clone() } else { String::new() };
                self.emit_return(fs, &v);
            }
        }
        fs.ind -= 1;
        self.emit(fs, "}");
        for (id, lb_, lc_) in fs.loop_labels.clone() {
            for (code, label, is_break) in [(ctl_break(id), lb_, true), (ctl_continue(id), lc_, false)] {
                self.emit(fs, &format!("if ({} == {}) {{", ctl, code));
                fs.ind += 1;
                self.emit(fs, &format!("{} = 0;", ctl));
                let idx = fs.scopes.iter().rposition(|s| s.kind == SK::Loop(id)).unwrap_or(0);
                self.cleanup_to(fs, if is_break { idx } else { idx + 1 });
                self.emit(fs, &format!("goto {};", label));
                fs.ind -= 1;
                self.emit(fs, "}");
            }
        }
        if let Some(lb) = fs.lb.clone() {
            for (code, brk) in [(ctl_break(lb.id), true), (ctl_continue(lb.id), false)] {
                self.emit(fs, &format!("if ({} == {}) {{", ctl, code));
                fs.ind += 1;
                self.emit(fs, &format!("{} = 0;", ctl));
                self.cleanup_to(fs, 0);
                if brk {
                    self.emit(fs, &format!("{} = {};", ctl, CTL_STOP));
                }
                let f = self.flow(brk);
                self.emit(fs, &format!("return {};", f));
                fs.ind -= 1;
                self.emit(fs, "}");
            }
            self.leave_body(fs, &ctl);
        }
        fs.ind -= 1;
        self.emit(fs, "}");
    }

    /// Leave a loop body with a jump still pending. The control word is cleared
    /// during cleanups, so a `defer` that runs its own push loop is not skipped.
    fn leave_body(&mut self, fs: &mut Fs, ctl: &str) {
        let saved = self.tmp();
        self.emit(fs, &format!("int {} = {};", saved, ctl));
        self.emit(fs, &format!("{} = 0;", ctl));
        self.cleanup_to(fs, 0);
        self.emit(fs, &format!("{} = {};", ctl, saved));
        let f = self.flow(true);
        self.emit(fs, &format!("return {};", f));
    }

    /// Statements of a block in a new C block + scope.
    fn block_stmts(&mut self, fs: &mut Fs, b: &TBlock) {
        self.emit(fs, "{");
        fs.ind += 1;
        self.push_scope(fs, SK::Block);
        // (facts end with their block)
        let nb = fs.bce.len();
        for (i, s) in b.stmts.iter().enumerate() {
            self.stmt(fs, s);
            self.free_after(fs, b, i);
        }
        if let Some(v) = &b.value {
            self.bce_kill_expr(fs, v);
            self.push_scope(fs, SK::Stmt);
            let s = self.expr(fs, v);
            let t = self.sub(fs, &v.ty);
            self.discard(fs, &s, &t, v.is_place());
            self.pop_scope(fs);
        }
        self.at_site(fs, drops::Site::block(b), |g, fs| g.pop_scope(fs));
        fs.bce.truncate(nb);
        fs.ind -= 1;
        self.emit(fs, "}");
    }

    /// Evaluate a block, storing its value into `dest` (if any).
    fn block_into(&mut self, fs: &mut Fs, b: &TBlock, dest: Option<&str>) {
        self.emit(fs, "{");
        fs.ind += 1;
        self.push_scope(fs, SK::Block);
        // (facts end with their block)
        let nb = fs.bce.len();
        for (i, s) in b.stmts.iter().enumerate() {
            self.stmt(fs, s);
            self.free_after(fs, b, i);
        }
        let mut later = vec![];
        if let Some(v) = &b.value {
            self.bce_kill_expr(fs, v);
            let t = self.sub(fs, &v.ty);
            self.push_value_scope(fs, &t);
            let s = self.expr(fs, v);
            match dest {
                Some(d) if v.ty != Ty::Never && t != Ty::Unit => self.emit(fs, &format!("{} = {};", d, s)),
                _ => self.discard(fs, &s, &t, v.is_place()),
            }
            later = self.pop_scope_keeping_later(fs);
        }
        self.at_site(fs, drops::Site::block(b), |g, fs| g.pop_scope(fs));
        fs.bce.truncate(nb);
        fs.ind -= 1;
        self.emit(fs, "}");
        fs.scopes.last_mut().unwrap().items.extend(later);
    }

    fn assign(&mut self, fs: &mut Fs, place: &TExpr, op: Option<BinOp>, value: &TExpr, span: Span, site: drops::Site) {
        let pt = self.sub(fs, &place.ty);
        match op {
            None => {
                let v = self.expr(fs, value);
                let c = self.cty(&pt);
                let tv = self.tmp();
                self.emit(fs, &format!("{} {} = {};", c, tv, v));
                let lp = local_path(place);
                let p = self.lval(fs, place);
                if self.needs_drop(&pt) {
                    match &lp {
                        Some((l, path)) => {
                            let (l, path) = (*l, path.clone());
                            self.at_site(fs, site, |g, fs| g.drop_path(fs, l, path, &pt));
                        }
                        None => {
                            let d = self.drop_fn(&pt);
                            self.emit(fs, &format!("{}(&{});", d, p));
                        }
                    }
                }
                self.emit(fs, &format!("{} = {};", p, tv));
                if let Some((l, path)) = lp {
                    self.set_live(fs, l, &path);
                }
            }
            Some(BinOp::Add) if pt == Ty::String => {
                let v = self.expr(fs, value);
                let p = self.lval(fs, place);
                self.emit(fs, &format!("gojo_string_push(&{}, {});", p, v));
            }
            Some(op) => {
                let v = self.expr(fs, value);
                let v = self.materialize(fs, &v, &pt, false);
                let p = self.lval(fs, place);
                let c = self.cty(&pt);
                let pp = self.tmp();
                self.emit(fs, &format!("{} *{} = &{};", c, pp, p));
                let r = self.arith(op, &format!("(*{})", pp), &v, &pt, span);
                self.emit(fs, &format!("*{} = {};", pp, r));
            }
        }
    }

    fn for_loop(&mut self, fs: &mut Fs, id: u32, kind: IterKind, iter: &TExpr, hi: Option<&TExpr>, elem: LocalId, index: Option<LocalId>, body: &TBlock, span: Span) {
        if kind == IterKind::StaticFields {
            // `static for f in fields(T)`: the body once per field of this
            // instance's `T`, with `f`'s type parameter bound to the field's type
            let owner = self.sub(fs, &iter.ty);
            let Some(TExpr { kind: TK::ConstParam(fp), .. }) = hi else { unreachable!() };
            let fp = *fp;
            let here = self.prog.func(fs.tf.func).pkg;
            let fields = match &owner {
                Ty::Adt(d, args) if self.prog.def(*d).is_struct() && !self.prog.def(*d).newtype => {
                    // outside the type's package, only its public fields
                    let def = self.prog.def(*d);
                    let vis: Vec<bool> = def.fields().iter().map(|f| f.declared_pub || def.pkg == here || def.tuple).collect();
                    self.prog.field_tys(*d, args).into_iter().zip(vis).filter(|(_, v)| *v).map(|(f, _)| f).collect()
                }
                _ => {
                    if self.static_err.is_none() {
                        let l = self.sm.file(span.file).line_col(span.lo);
                        self.static_err = Some(format!("{}:{}:{}: `static for` over `{}` (in `{}`), which isn't a struct: guard it with `static if T: struct`", self.sm.file(span.file).path, l.0, l.1, self.prog.show(&owner), self.prog.func(fs.tf.func).name));
                    }
                    vec![]
                }
            };
            let saved = (fs.subst.get(&fp).cloned(), fs.static_cur.get(&fp).cloned());
            for (fname, fty) in fields {
                fs.subst.insert(fp, fty);
                fs.static_cur.insert(fp, fname);
                self.emit(fs, "{");
                fs.ind += 1;
                self.push_scope(fs, SK::Block);
                self.block_stmts(fs, body);
                self.pop_scope(fs);
                fs.ind -= 1;
                self.emit(fs, "}");
            }
            match saved.0 {
                Some(t) => fs.subst.insert(fp, t),
                None => fs.subst.remove(&fp),
            };
            match saved.1 {
                Some(n) => fs.static_cur.insert(fp, n),
                None => fs.static_cur.remove(&fp),
            };
            return;
        }
        let n = fs.bce.len();
        if let Some(f) = self.bce_loop_fact(fs, kind, iter, hi, elem, index, body) {
            fs.bce.push(f);
        }
        self.for_loop_inner(fs, id, kind, iter, hi, elem, index, body, span);
        fs.bce.truncate(n);
    }

    fn for_loop_inner(&mut self, fs: &mut Fs, id: u32, kind: IterKind, iter: &TExpr, hi: Option<&TExpr>, elem: LocalId, index: Option<LocalId>, body: &TBlock, _span: Span) {
        self.emit(fs, "{");
        fs.ind += 1;
        self.push_scope(fs, SK::Block);
        let en = self.local_cname(fs, elem);
        let et = self.local_ty(fs, elem);
        let ec = self.cty(&et);
        // Loops poll for preemption every iteration, except leaves (no nested
        // loops, only bounded calls). A counted leaf is emitted twice: a loop
        // over blocks of 65536 iterations that polls between blocks, then the
        // plain loop for the rest, so short loops run exactly as without polls.
        // Other leaves rely on the loop around them.
        let leaf = self.leaf_loop(None, body);
        // (counter, limit, counter type, per-iteration bindings) of a counted leaf
        let mut counted: Option<(String, String, String, Vec<(String, LocalId, Repr)>)> = None;
        match kind {
            IterKind::Range { inclusive } => {
                let lo = self.expr(fs, iter);
                let hiv = self.expr(fs, hi.unwrap());
                let i = self.tmp();
                let e = self.tmp();
                if inclusive {
                    let g = self.tmp();
                    self.emit(fs, &format!("{} {} = {}, {} = {}; bool {} = {} <= {};", ec, i, lo, e, hiv, g, i, e));
                    self.emit(fs, &format!("for (; {}; {} = ({} != {}), {} += {}) {{", g, g, i, e, i, g));
                } else {
                    self.emit(fs, &format!("{} {} = {}, {} = {};", ec, i, lo, e, hiv));
                    if leaf {
                        let bind = vec![(format!("{} {} = {};", ec, en, i), elem, Repr::Val(en.clone()))];
                        counted = Some((i.clone(), e.clone(), ec.clone(), bind));
                    } else {
                        self.emit(fs, &format!("for (; {} < {}; {}++) {{", i, e, i));
                    }
                }
                if counted.is_none() {
                    fs.ind += 1;
                    self.emit(fs, &format!("{} {} = {};", ec, en, i));
                    fs.repr.insert(elem, Repr::Val(en));
                }
            }
            IterKind::Vec | IterKind::Slice | IterKind::Array => {
                let b = self.lval_scoped(fs, iter);
                let it = self.sub(fs, &iter.ty);
                let p = self.tmp();
                let n = self.tmp();
                let i = self.tmp();
                match kind {
                    IterKind::Array => {
                        let len = match &it {
                            Ty::Array(_, n) => *n,
                            _ => 0,
                        };
                        self.emit(fs, &format!("{} *{} = {}.a; int64_t {} = {};", ec, p, b, n, len));
                    }
                    _ => self.emit(fs, &format!("{} *{} = ({} *){}.ptr; int64_t {} = {}.len;", ec, p, ec, b, n, b)),
                }
                if leaf {
                    self.emit(fs, &format!("int64_t {} = 0;", i));
                    let mut bind = vec![(format!("{} *{} = &{}[{}];", ec, en, p, i), elem, Repr::Ptr(en.clone()))];
                    if let Some(ix) = index {
                        let xn = self.local_cname(fs, ix);
                        bind.push((format!("int64_t {} = {};", xn, i), ix, Repr::Val(xn)));
                    }
                    counted = Some((i.clone(), n.clone(), "int64_t".into(), bind));
                } else {
                    self.emit(fs, &format!("for (int64_t {} = 0; {} < {}; {}++) {{", i, i, n, i));
                    fs.ind += 1;
                    self.emit(fs, &format!("{} *{} = &{}[{}];", ec, en, p, i));
                    fs.repr.insert(elem, Repr::Ptr(en));
                    if let Some(ix) = index {
                        let xn = self.local_cname(fs, ix);
                        self.emit(fs, &format!("int64_t {} = {};", xn, i));
                        fs.repr.insert(ix, Repr::Val(xn));
                    }
                }
            }
            IterKind::Map => {
                let b = self.lval_scoped(fs, iter);
                let mt = self.sub(fs, &iter.ty);
                let (k, v) = match &mt {
                    Ty::Map(k, v) => ((**k).clone(), (**v).clone()),
                    _ => unreachable!(),
                };
                let (ent, _) = self.map_ent(&k, &v);
                let m = self.tmp();
                let i = self.tmp();
                let e = self.tmp();
                self.emit(fs, &format!("gojo_map *{} = &{};", m, b));
                self.emit(fs, &format!("for (int64_t {} = 0; {} < {}->n_entries; {}++) {{", i, i, m, i));
                fs.ind += 1;
                self.emit(fs, &format!("{} *{} = (({} *){}->entries) + {}; if (!{}->live) continue;", ent, e, ent, m, i, e));
                self.emit(fs, &format!("{} *{} = &{}->v;", ec, en, e));
                fs.repr.insert(elem, Repr::Ptr(en));
                if let Some(ix) = index {
                    let kc = self.cty(&k);
                    let xn = self.local_cname(fs, ix);
                    self.emit(fs, &format!("{} *{} = &{}->k;", kc, xn, e));
                    fs.repr.insert(ix, Repr::Ptr(xn));
                }
            }
            IterKind::Str => {
                let s = self.expr(fs, iter);
                let it = self.sub(fs, &iter.ty);
                let sv = if it == Ty::String {
                    let p = self.lval_scoped_str(fs, iter, &s);
                    format!("gojo_string_view(&{})", p)
                } else {
                    s
                };
                let st = self.tmp();
                let i = self.tmp();
                self.emit(fs, &format!("gojo_str {} = {};", st, sv));
                self.emit(fs, &format!("for (int64_t {} = 0; {} < {}.len;) {{", i, i, st));
                fs.ind += 1;
                if let Some(ix) = index {
                    let xn = self.local_cname(fs, ix);
                    self.emit(fs, &format!("int64_t {} = {};", xn, i));
                    fs.repr.insert(ix, Repr::Val(xn));
                }
                self.emit(fs, &format!("int32_t {} = gojo_utf8_decode({}, &{});", en, st, i));
                fs.repr.insert(elem, Repr::Val(en));
            }
            // (unrolled by `for_loop`)
            IterKind::StaticFields => unreachable!(),
            IterKind::Chan => {
                let c = self.expr(fs, iter);
                let ch = self.tmp();
                self.emit(fs, &format!("gojo_chan *{} = {};", ch, c));
                self.emit(fs, "for (;;) {");
                fs.ind += 1;
                self.emit(fs, &format!("{} {};", ec, en));
                self.emit(fs, &format!("if (!gojo_chan_recv({}, &{})) break;", ch, en));
                fs.repr.insert(elem, Repr::Val(en));
            }
        }
        if let Some((i, e, ity, bind)) = counted {
            let lb = self.label();
            let stop = self.tmp();
            let lo = self.tmp();
            self.emit(fs, &format!("const {} {} = {};", ity, lo, i));
            // Short loops only pay for the first comparison. `i < e` is still needed:
            // an empty range like i64::MAX..i64::MIN can wrap to a small difference.
            // A range too long for the difference to fit just stays in the plain loop.
            self.emit(fs, &format!("while ((int64_t)((uint64_t){} - (uint64_t){}) > 65536 && {} < {}) {{", e, i, i, e));
            fs.ind += 1;
            self.emit(fs, &format!("{} {} = ({})({} + 65536);", ity, stop, ity, i));
            // what the C compiler can't see, and needs to drop bounds checks
            self.emit(fs, &format!("GOJO_ASSUME({} >= {} && {} <= {});", i, lo, stop, e));
            self.emit(fs, &format!("for (; {} < {}; {}++) {{", i, stop, i));
            self.counted_body(fs, id, &lb, &bind, body);
            self.emit(fs, "GOJO_POLL();");
            fs.ind -= 1;
            self.emit(fs, "}");
            self.emit(fs, &format!("GOJO_ASSUME({} >= {});", i, lo));
            self.emit(fs, &format!("for (; {} < {}; {}++) {{", i, e, i));
            self.counted_body(fs, id, &lb, &bind, body);
            self.emit(fs, &format!("{}:;", lb));
            self.pop_scope(fs);
            fs.ind -= 1;
            self.emit(fs, "}");
            return;
        }
        let (lb, lc) = (self.label(), self.label());
        fs.loop_labels.push((id, lb.clone(), lc.clone()));
        self.push_scope(fs, SK::Loop(id));
        if !leaf {
            self.emit(fs, "GOJO_POLL();");
        }
        if kind == IterKind::Chan && self.needs_drop(&et) {
            self.declare_flags(fs, elem);
            self.register(fs, Cl::DropLocal(elem));
        }
        self.block_stmts(fs, body);
        self.emit(fs, &format!("{}:;", lc));
        let top = fs.scopes.len() - 1;
        self.emit_scope_cleanups(fs, top);
        fs.scopes.pop();
        fs.ind -= 1;
        self.emit(fs, "}");
        self.emit(fs, &format!("{}:;", lb));
        fs.loop_labels.pop();
        self.pop_scope(fs);
        fs.ind -= 1;
        self.emit(fs, "}");
    }

    /// One copy of a counted leaf loop's body, after its `for (...) {` line.
    fn counted_body(&mut self, fs: &mut Fs, id: u32, lb: &str, bind: &[(String, LocalId, Repr)], body: &TBlock) {
        fs.ind += 1;
        for (line, l, r) in bind {
            self.emit(fs, line);
            fs.repr.insert(*l, r.clone());
        }
        let lc = self.label();
        fs.loop_labels.push((id, lb.to_string(), lc.clone()));
        self.push_scope(fs, SK::Loop(id));
        self.block_stmts(fs, body);
        self.emit(fs, &format!("{}:;", lc));
        let top = fs.scopes.len() - 1;
        self.emit_scope_cleanups(fs, top);
        fs.scopes.pop();
        fs.loop_labels.pop();
        fs.ind -= 1;
        self.emit(fs, "}");
    }

    fn lval_scoped_str(&mut self, fs: &mut Fs, e: &TExpr, s: &str) -> String {
        if e.is_place() {
            s.to_string()
        } else {
            let t = self.sub(fs, &e.ty);
            let n = self.materialize(fs, s, &t, false);
            if self.needs_drop(&t) {
                self.register(fs, Cl::DropTemp(n.clone(), t));
            }
            n
        }
    }

    // ------------------------------------------------------------------
    // Expressions
    // ------------------------------------------------------------------

    /// An lvalue for `e`; rvalues are stored in a temporary dropped at the end of the statement.
    fn lval(&mut self, fs: &mut Fs, e: &TExpr) -> String {
        if e.is_place() {
            return self.place(fs, e);
        }
        let s = self.expr(fs, e);
        let t = self.sub(fs, &e.ty);
        let owned = self.is_owned_rvalue(e);
        self.materialize(fs, &s, &t, owned)
    }

    /// Like `lval`, but temporaries live until the end of the enclosing block.
    fn lval_scoped(&mut self, fs: &mut Fs, e: &TExpr) -> String {
        if e.is_place() {
            self.push_scope(fs, SK::Stmt);
            let p = self.place(fs, e);
            // move statement-level cleanups (e.g. temporaries, locks) to the block
            let items = fs.scopes.pop().unwrap().items;
            fs.scopes.last_mut().unwrap().items.extend(items);
            return p;
        }
        self.push_scope(fs, SK::Stmt);
        let s = self.expr(fs, e);
        let items = fs.scopes.pop().unwrap().items;
        fs.scopes.last_mut().unwrap().items.extend(items);
        let t = self.sub(fs, &e.ty);
        let owned = self.is_owned_rvalue(e);
        let c = self.cty(&t);
        let n = self.tmp();
        self.emit(fs, &format!("{} {} = {};", c, n, s));
        if owned && self.needs_drop(&t) {
            self.register(fs, Cl::DropTemp(n.clone(), t));
        }
        n
    }

    fn is_owned_rvalue(&self, e: &TExpr) -> bool {
        if let TK::Proj { opt: true, .. } = &e.kind {
            return false;
        }
        if let TK::OptField { base, .. } = &e.kind {
            if (base.is_place() || !self.is_owned_rvalue(base)) && !self.is_copy(&e.ty) {
                return false;
            }
        }
        !matches!(e.kind, TK::Coerce(Coercion::ToIface { owned: false }, _) | TK::Coerce(Coercion::StringToStr | Coercion::VecToSlice | Coercion::ArrayToSlice, _))
            && !matches!(&e.kind, TK::Closure(c) if !c.owned)
    }

    fn addr(&mut self, fs: &mut Fs, e: &TExpr) -> String {
        let p = self.lval(fs, e);
        format!("&{}", p)
    }

    /// The accessor of package-level value `g`: computes it on first use.
    fn global_get(&mut self, g: u32) -> String {
        let n = format!("gget_{}", g);
        if self.helpers.insert(n.clone()) {
            let gd = self.prog.globals[g as usize].clone();
            let c = self.cty(&gd.ty);
            let init = self.instance(gd.init, vec![]);
            let _ = writeln!(self.protos, "static {} *{}(void);", c, n);
            let _ = writeln!(self.bodies, "static {c} gval_{g};\nstatic gojo_once gonce_{g};\nstatic void ginit_{g}(void) {{ GOJO_UNTYPE(&gval_{g}, sizeof gval_{g}); gval_{g} = {init}(); }}\nstatic {c} *{n}(void) {{\n    if (__builtin_expect(__atomic_load_n(&gonce_{g}.state, __ATOMIC_ACQUIRE) != 2, 0)) gojo_once_run(&gonce_{g}, ginit_{g}, {name});\n    return &gval_{g};\n}}", c = c, g = g, init = init, n = n, name = c_string(&gd.name));
        }
        n
    }

    fn place(&mut self, fs: &mut Fs, e: &TExpr) -> String {
        match &e.kind {
            TK::Local(l) => self.local_place(fs, *l),
            TK::Global(g) => {
                let f = self.global_get(*g);
                format!("(*{}())", f)
            }
            TK::StaticField { base, fparam } => {
                let fname = fs.static_cur.get(fparam).cloned().unwrap_or_else(|| "?".into());
                let b = if base.is_place() { self.place(fs, base) } else { self.lval(fs, base) };
                format!("{}.f_{}", b, fname)
            }
            TK::Field { base, index } => {
                let bt = self.sub(fs, &base.ty);
                let fname = match &bt {
                    Ty::Adt(d, args) => self.prog.field_tys(*d, args)[*index].0.clone(),
                    _ => "?".into(),
                };
                if base.is_place() {
                    let b = self.place(fs, base);
                    format!("{}.f_{}", b, fname)
                } else {
                    let b = self.lval(fs, base);
                    format!("{}.f_{}", b, fname)
                }
            }
            TK::Deref(base) => {
                let bt = self.sub(fs, &base.ty);
                let b = if base.is_place() { self.place(fs, base) } else { self.lval(fs, base) };
                match &bt {
                    Ty::Rc(inner) | Ty::Arc(inner) => {
                        self.ensure_rcbox(inner);
                        format!("({}->val)", b)
                    }
                    Ty::Box(inner) => {
                        self.ensure(inner);
                        format!("(*{})", b)
                    }
                    // Unsafe code may read memory through a pointer of another
                    // type than it was written with (spec 11.1), so raw pointers
                    // are dereferenced through a `may_alias` type, which clang and
                    // gcc treat like `char` for alias analysis. (A struct reached
                    // only through a raw pointer is defined here.)
                    Ty::Ptr(inner) => {
                        self.ensure(inner);
                        let ma = self.may_alias_ty(inner);
                        format!("(*({} *){})", ma, b)
                    }
                    _ => format!("(*{})", b),
                }
            }
            TK::Index { base, index } => {
                let bt = self.sub(fs, &base.ty);
                let b = self.lval(fs, base);
                let i = self.expr(fs, index);
                let l = self.loc(e.span);
                let proven = self.bce_index_ok(fs, base, index);
                self.note_check(fs, e.span, proven);
                let check = if proven { "GOJO_IDX_PROVEN" } else { "gojo_idx" };
                match &bt {
                    Ty::Vec(t) | Ty::Slice(t) => {
                        let c = self.cty(t);
                        format!("(({} *){}.ptr)[{}({}, {}.len, {})]", c, b, check, i, b, l)
                    }
                    Ty::Array(_, n) => format!("{}.a[{}({}, {}, {})]", b, check, i, n, l),
                    _ => "?".into(),
                }
            }
            TK::MapIndex { base, key } => {
                let mt = self.sub(fs, &base.ty);
                let (k, v) = match &mt {
                    Ty::Map(k, v) => ((**k).clone(), (**v).clone()),
                    _ => unreachable!(),
                };
                let b = self.lval(fs, base);
                let kp = self.borrow_ptr(fs, key);
                let (ent, desc) = self.map_ent(&k, &v);
                let _ = v;
                let r = self.tmp();
                let l = self.loc(e.span);
                self.emit(fs, &format!("{} *{} = mf_{}(&{}, {});", ent, r, desc, b, kp));
                self.emit(fs, &format!("if (!{}) gojo_panic(\"key not found in map\", {});", r, l));
                format!("{}->v", r)
            }
            TK::ProjUnwrap { proj, msg } => {
                let call = match &proj.kind {
                    TK::Proj { call, .. } => call.clone(),
                    _ => unreachable!(),
                };
                let p = self.call_expr(fs, &call);
                let t = self.sub(fs, &e.ty);
                let c = self.cty(&t);
                let r = self.tmp();
                let l = self.loc(e.span);
                self.emit(fs, &format!("{} *{} = {};", c, r, p));
                match msg {
                    Some(m) => {
                        let mv = self.expr(fs, m);
                        self.emit(fs, &format!("if (!{}) gojo_panic_str({}, {});", r, mv, l));
                    }
                    None => self.emit(fs, &format!("if (!{}) gojo_panic(\"unwrap() called on none\", {});", r, l)),
                }
                format!("(*{})", r)
            }
            TK::Proj { call, opt: false, .. } => {
                let p = self.call_expr(fs, call);
                let t = self.sub(fs, &e.ty);
                let c = self.cty(&t);
                let r = self.tmp();
                self.emit(fs, &format!("{} *{} = {};", c, r, p));
                format!("(*{})", r)
            }
            _ => self.lval(fs, e),
        }
    }

    /// Pointer to a borrowed operand (place or temporary).
    fn borrow_ptr(&mut self, fs: &mut Fs, e: &TExpr) -> String {
        let p = self.lval(fs, e);
        format!("&{}", p)
    }

    fn expr(&mut self, fs: &mut Fs, e: &TExpr) -> String {
        let t = self.sub(fs, &e.ty);
        match &e.kind {
            TK::Unit => "0".into(),
            TK::Int(v) => int_lit(*v, &t),
            TK::Float(f) => {
                let s = if f.is_nan() {
                    "NAN".to_string()
                } else if f.is_infinite() {
                    if *f > 0.0 { "INFINITY".into() } else { "(-INFINITY)".into() }
                } else {
                    let mut s = format!("{:?}", f);
                    if !s.contains('.') && !s.contains('e') && !s.contains("inf") {
                        s.push_str(".0");
                    }
                    s
                };
                if t == Ty::F32 {
                    format!("((float){})", s)
                } else {
                    s
                }
            }
            TK::Bool(b) => if *b { "true".into() } else { "false".into() },
            TK::Str(s) => format!("gojo_str_lit({}, {})", c_string(s), s.len()),
            TK::NoneLit => self.zero(&t),
            TK::StaticFieldName(fp) => {
                let n = fs.static_cur.get(fp).cloned().unwrap_or_default();
                format!("gojo_str_lit({}, {})", c_string(&n), n.len())
            }
            TK::Local(_) | TK::Global(_) | TK::Field { .. } | TK::StaticField { .. } | TK::Deref(_) | TK::Index { .. } | TK::MapIndex { .. } | TK::ProjUnwrap { .. } => {
                if let TK::Field { base, index } = &e.kind {
                    if !base.is_place() {
                        return self.rvalue_field(fs, base, *index, &t);
                    }
                }
                self.place(fs, e)
            }
            TK::Const(c) => {
                let v = self.prog.consts[*c as usize].value.clone().unwrap();
                self.expr(fs, &v)
            }
            TK::ConstParam(p) => match self.sub(fs, &Ty::Param(*p)) {
                Ty::ConstInt(n) => format!("INT64_C({})", n),
                t => panic!("integer parameter bound to {:?}", t),
            },
            TK::FuncRef(f, targs) => {
                let targs: Vec<Ty> = targs.iter().map(|x| self.sub(fs, x)).collect();
                self.func_item(*f, targs)
            }
            TK::StrIndex { base, index } => {
                let bt = self.sub(fs, &base.ty);
                let b = self.expr(fs, base);
                let sv = if bt == Ty::String {
                    let p = self.lval_scoped_str(fs, base, &b);
                    format!("gojo_string_view(&{})", p)
                } else {
                    b
                };
                let i = self.expr(fs, index);
                let st = self.tmp();
                let l = self.loc(e.span);
                self.emit(fs, &format!("gojo_str {} = {};", st, sv));
                let proven = self.bce_index_ok(fs, base, index);
                self.note_check(fs, e.span, proven);
                let check = if proven { "GOJO_IDX_PROVEN" } else { "gojo_idx" };
                format!("{}.ptr[{}({}, {}.len, {})]", st, check, i, st, l)
            }
            TK::SliceOf { base, lo, hi } => {
                let bt = self.sub(fs, &base.ty);
                let l = self.loc(e.span);
                match &bt {
                    Ty::Str | Ty::String => {
                        let b = self.expr(fs, base);
                        let sv = if bt == Ty::String {
                            let p = self.lval_scoped_str(fs, base, &b);
                            format!("gojo_string_view(&{})", p)
                        } else {
                            b
                        };
                        let st = self.tmp();
                        self.emit(fs, &format!("gojo_str {} = {};", st, sv));
                        let lo = match lo {
                            Some(x) => self.expr(fs, x),
                            None => "0".into(),
                        };
                        let hi = match hi {
                            Some(x) => self.expr(fs, x),
                            None => format!("{}.len", st),
                        };
                        format!("gojo_str_slice({}, {}, {}, {})", st, lo, hi, l)
                    }
                    Ty::Vec(el) | Ty::Slice(el) | Ty::Array(el, _) => {
                        let el = (**el).clone();
                        let (lo_e, hi_e) = (lo.as_deref(), hi.as_deref());
                        let b = self.lval(fs, base);
                        let (ptr, len) = match &bt {
                            Ty::Array(_, n) => (format!("{}.a", b), n.to_string()),
                            _ => (format!("{}.ptr", b), format!("{}.len", b)),
                        };
                        let lo = match lo {
                            Some(x) => self.expr(fs, x),
                            None => "0".into(),
                        };
                        let hi = match hi {
                            Some(x) => self.expr(fs, x),
                            None => len.clone(),
                        };
                        let c = self.cty(&el);
                        let a = self.tmp();
                        let z = self.tmp();
                        self.emit(fs, &format!("int64_t {} = {}, {} = {};", a, lo, z, hi));
                        let proven = self.bce_slice_ok(fs, base, lo_e, hi_e);
                        self.note_check(fs, e.span, proven);
                        if proven {
                            self.emit(fs, &format!("GOJO_SLICE_PROVEN({}, {}, {}, {});", a, z, len, l));
                        } else {
                            self.emit(fs, &format!("if ({} < 0 || {} > {} || {} > {}) gojo_panic(\"slice bounds out of range\", {});", a, z, len, a, z, l));
                        }
                        format!("((gojo_slice){{ ({} *){} + {}, {} - {} }})", c, ptr, a, z, a)
                    }
                    _ => "?".into(),
                }
            }
            TK::Call { .. } => self.call_expr(fs, e),
            TK::Proj { call, opt, .. } => {
                if !*opt {
                    return self.place(fs, e);
                }
                // optional projection used as a value: copy out
                let p = self.call_expr(fs, call);
                let inner = match &t {
                    Ty::Option(i) => (**i).clone(),
                    _ => t.clone(),
                };
                let ic = self.cty(&inner);
                let oc = self.cty(&t);
                let r = self.tmp();
                self.emit(fs, &format!("{} *{} = {};", ic, r, p));
                let some = self.omake(&t, &format!("*{}", r));
                format!("({} ? {} : ({}){{0}})", r, some, oc)
            }
            TK::Intrinsic { name, targs, args } => self.magic(fs, name, targs, args, &t, e.span),
            TK::Unary { op, e: x } => {
                let v = self.expr(fs, x);
                let xt = self.sub(fs, &x.ty);
                if let Some(inner) = self.numeric_newtype(&xt) {
                    let c = self.cty(&xt);
                    let iv = format!("({}).f_value", v);
                    let r = match (&inner, *op) {
                        (Ty::Int(k), "-") => format!("gojo_neg_{}({}, {})", int_suffix(*k), iv, self.loc(e.span)),
                        (_, "-") => format!("(-({}))", iv),
                        (_, "~") => format!("(({})~{})", self.cty(&inner), iv),
                        _ => iv,
                    };
                    return format!("(({}){{ {} }})", c, r);
                }
                match *op {
                    "-" => match &xt {
                        Ty::Int(k) => {
                            let l = self.loc(e.span);
                            format!("gojo_neg_{}({}, {})", int_suffix(*k), v, l)
                        }
                        // `-(-1.0)` must not become `--1.0`
                        _ => format!("(-({}))", v),
                    },
                    "!" => format!("(!{})", v),
                    "~" => {
                        let c = self.cty(&xt);
                        format!("(({})~{})", c, v)
                    }
                    _ => v,
                }
            }
            TK::Binary { op, l, r } => self.binary(fs, *op, l, r, e.span),
            TK::Cast(x) => self.cast(fs, x, &t, e.span),
            TK::StructLit { def, targs, fields } => {
                let targs: Vec<Ty> = targs.iter().map(|x| self.sub(fs, x)).collect();
                let c = self.cty(&t);
                let ftys = self.prog.field_tys(*def, &targs);
                let mut parts = vec![];
                let mut held = vec![];
                for (i, f) in fields.iter().enumerate() {
                    let v = if matches!(f.kind, TK::DefaultOf) { self.field_default(fs, *def, &targs, i) } else { self.expr(fs, f) };
                    let v = if f.ty == Ty::Never { self.zero(&ftys[i].1) } else { v };
                    // a later field's `try` may leave before the struct is built
                    let v = if fields[i + 1..].iter().any(may_leave) {
                        let (v, flag) = self.hold(fs, &v, &ftys[i].1);
                        held.extend(flag);
                        v
                    } else {
                        v
                    };
                    parts.push(format!(".f_{} = {}", ftys[i].0, v));
                }
                for f in held {
                    self.emit(fs, &format!("{} = 0;", f));
                }
                if parts.is_empty() {
                    return format!("(({}){{0}})", c);
                }
                let n = self.tmp();
                self.emit(fs, &format!("{} {} = {{ {} }};", c, n, parts.join(", ")));
                n
            }
            TK::EnumLit { def, targs, variant, args } => {
                let targs: Vec<Ty> = targs.iter().map(|x| self.sub(fs, x)).collect();
                let c = self.cty(&t);
                let ftys = self.prog.variant_tys(*def, &targs, *variant);
                if args.is_empty() {
                    return format!("(({}){{ .tag = {} }})", c, variant);
                }
                let mut parts = vec![];
                let mut held = vec![];
                for (i, a) in args.iter().enumerate() {
                    let v = self.expr(fs, a);
                    let v = if args[i + 1..].iter().any(may_leave) {
                        let (v, flag) = self.hold(fs, &v, &ftys[i].1);
                        held.extend(flag);
                        v
                    } else {
                        v
                    };
                    parts.push(format!(".f_{} = {}", ftys[i].0, v));
                }
                for f in held {
                    self.emit(fs, &format!("{} = 0;", f));
                }
                let n = self.tmp();
                self.emit(fs, &format!("{} {} = {{ .tag = {}, .u.v{} = {{ {} }} }};", c, n, variant, variant, parts.join(", ")));
                n
            }
            TK::SomeLit(x) => {
                let v = self.expr(fs, x);
                self.omake(&t, &v)
            }
            // Built in a temporary of their own, like enum literals: clang
            // builds an initializer field by field straight into its
            // destination, and a destination that held the other member (a
            // caller's result slot reused in a loop) would be written through
            // a type it doesn't have. Copying the temporary copies its types.
            TK::OkLit(x) => {
                let v = self.expr(fs, x);
                let c = self.cty(&t);
                let n = self.tmp();
                self.emit(fs, &format!("{} {} = {{ .ok = 1, .u.val = {} }};", c, n, v));
                n
            }
            TK::ErrLit(x) => {
                let v = self.expr(fs, x);
                if self.etrace {
                    let l = self.loc(e.span);
                    self.emit(fs, &format!("gojo_etrace_origin({});", l));
                }
                let c = self.cty(&t);
                let n = self.tmp();
                self.emit(fs, &format!("{} {} = {{ .ok = 0, .u.err = {} }};", c, n, v));
                n
            }
            TK::VecLit(xs) => {
                let el = match &t {
                    Ty::Vec(el) => (**el).clone(),
                    _ => unreachable!(),
                };
                let ec = self.cty(&el);
                let mut vals = vec![];
                let mut held = vec![];
                for (i, x) in xs.iter().enumerate() {
                    let v = self.expr(fs, x);
                    let v = if xs[i + 1..].iter().any(may_leave) {
                        let (v, flag) = self.hold(fs, &v, &el);
                        held.extend(flag);
                        v
                    } else {
                        v
                    };
                    vals.push(v);
                }
                for f in held {
                    self.emit(fs, &format!("{} = 0;", f));
                }
                let n = self.tmp();
                self.emit(fs, &format!("gojo_vec {} = {{0}};", n));
                if !vals.is_empty() {
                    self.emit(fs, &format!("gojo_vec_reserve(&{}, {}, sizeof({}));", n, vals.len(), ec));
                    for (i, v) in vals.iter().enumerate() {
                        self.emit(fs, &format!("(({} *){}.ptr)[{}] = {};", ec, n, i, v));
                    }
                    self.emit(fs, &format!("{}.len = {};", n, vals.len()));
                }
                n
            }
            TK::ArrayLit(xs) => {
                let c = self.cty(&t);
                let el = match &t {
                    Ty::Array(e, _) => (**e).clone(),
                    _ => Ty::Unit,
                };
                let mut vals = vec![];
                let mut held = vec![];
                for (i, x) in xs.iter().enumerate() {
                    let v = self.expr(fs, x);
                    let v = if xs[i + 1..].iter().any(may_leave) {
                        let (v, flag) = self.hold(fs, &v, &el);
                        held.extend(flag);
                        v
                    } else {
                        v
                    };
                    vals.push(v);
                }
                for f in held {
                    self.emit(fs, &format!("{} = 0;", f));
                }
                let n = self.tmp();
                self.emit(fs, &format!("{} {} = {{ .a = {{ {} }} }};", c, n, vals.join(", ")));
                n
            }
            TK::MapLit(pairs) => {
                let (k, v) = match &t {
                    Ty::Map(k, v) => ((**k).clone(), (**v).clone()),
                    _ => unreachable!(),
                };
                let n = self.tmp();
                self.emit(fs, &format!("gojo_map {} = {{0}};", n));
                let mut held_map: Option<String> = None;
                // (dropped if a later key's or value's `try` leaves first)
                if pairs.iter().any(|(a, b)| may_leave(a) || may_leave(b)) {
                    let mt = Ty::Map(std::rc::Rc::new(k.clone()), std::rc::Rc::new(v.clone()));
                    let flag = format!("{}_held", n);
                    self.emit(fs, &format!("bool {} = 1;", flag));
                    self.register(fs, Cl::DropFlag(n.clone(), mt, flag.clone()));
                    held_map = Some(flag);
                }
                for (ke, ve) in pairs {
                    let kv = self.expr(fs, ke);
                    let (kv, kflag) = if may_leave(ve) { self.hold(fs, &kv, &k) } else { (kv, None) };
                    let vv = self.expr(fs, ve);
                    if let Some(f) = kflag {
                        self.emit(fs, &format!("{} = 0;", f));
                    }
                    let old = self.map_insert(fs, &format!("&{}", n), &k, &v, &kv, &vv);
                    let ot = Ty::opt(v.clone());
                    self.discard(fs, &old, &ot, false);
                }
                if let Some(f) = held_map {
                    self.emit(fs, &format!("{} = 0;", f));
                }
                n
            }
            TK::Closure(c) => self.closure(fs, c, &t),
            TK::If { cond, then, else_ } if matches!(&cond.kind, TK::Intrinsic { name, .. } if name.starts_with("static_if")) => {
                // decided for this instance: only the arm that holds is generated
                let TK::Intrinsic { name, targs, .. } = &cond.kind else { unreachable!() };
                let mut binds: Vec<(TParamId, Ty)> = vec![];
                let holds = self.static_cond(fs, name, targs, &mut binds);
                let b = if holds { Some(then) } else { else_.as_ref() };
                if let Some(b) = b {
                    let saved: Vec<(TParamId, Option<Ty>)> = binds.iter().map(|(p, _)| (*p, fs.subst.get(p).cloned())).collect();
                    for (p, t) in binds {
                        fs.subst.insert(p, t);
                    }
                    self.emit(fs, "{");
                    fs.ind += 1;
                    self.push_scope(fs, SK::Block);
                    self.block_stmts(fs, b);
                    self.pop_scope(fs);
                    fs.ind -= 1;
                    self.emit(fs, "}");
                    for (p, old) in saved {
                        match old {
                            Some(t) => fs.subst.insert(p, t),
                            None => fs.subst.remove(&p),
                        };
                    }
                }
                "0".into()
            }
            TK::If { cond, then, else_ } => {
                let cv = self.expr(fs, cond);
                let has_val = !matches!(t, Ty::Unit | Ty::Never);
                let r = if has_val {
                    let c = self.cty(&t);
                    let r = self.tmp();
                    self.emit(fs, &format!("{} {};", c, r));
                    Some(r)
                } else {
                    None
                };
                // values moved on one branch and not the other are dropped at
                // the end of the other (drops.rs)
                let key = e as *const TExpr as usize;
                let (jd0, jd1) = (self.join_drops(fs, key, 0), self.join_drops(fs, key, 1));
                // what the condition says inside each branch (bce.rs)
                let (pos, neg) = (self.bce_guard(fs, cond, true), self.bce_guard(fs, cond, false));
                let nb = fs.bce.len();
                fs.bce.extend(pos);
                if jd0.is_empty() {
                    self.emit(fs, &format!("if ({})", cv));
                    self.block_into(fs, then, r.as_deref());
                } else {
                    self.emit(fs, &format!("if ({}) {{", cv));
                    fs.ind += 1;
                    self.block_into(fs, then, r.as_deref());
                    self.emit_join_drops(fs, &jd0);
                    fs.ind -= 1;
                    self.emit(fs, "}");
                }
                fs.bce.truncate(nb);
                fs.bce.extend(neg);
                match else_ {
                    Some(eb) if jd1.is_empty() => {
                        self.emit(fs, "else");
                        self.block_into(fs, eb, r.as_deref());
                    }
                    Some(eb) => {
                        self.emit(fs, "else {");
                        fs.ind += 1;
                        self.block_into(fs, eb, r.as_deref());
                        self.emit_join_drops(fs, &jd1);
                        fs.ind -= 1;
                        self.emit(fs, "}");
                    }
                    None if !jd1.is_empty() => {
                        self.emit(fs, "else {");
                        fs.ind += 1;
                        self.emit_join_drops(fs, &jd1);
                        fs.ind -= 1;
                        self.emit(fs, "}");
                    }
                    None => {}
                }
                fs.bce.truncate(nb);
                r.unwrap_or_else(|| "0".into())
            }
            TK::Match { scrut, owned, arms } => self.match_expr(fs, scrut, *owned, arms, &t, e.span, e as *const TExpr as usize),
            TK::Block(b) | TK::Unsafe(b) | TK::Isolate(b) => {
                let has_val = !matches!(t, Ty::Unit | Ty::Never);
                let r = if has_val {
                    let c = self.cty(&t);
                    let r = self.tmp();
                    self.emit(fs, &format!("{} {};", c, r));
                    Some(r)
                } else {
                    None
                };
                self.block_into(fs, b, r.as_deref());
                r.unwrap_or_else(|| "0".into())
            }
            TK::Scope { id, handle, body, errt, .. } => self.scope_block(fs, *id, *handle, body, errt.as_ref(), &t, e.span),
            TK::Spawn { target, task, handle } => self.spawn(fs, target, task, *handle, e.span),
            TK::Try { e: x, wrap } => self.try_expr(fs, x, wrap.as_ref(), &t, drops::Site::expr(e)),
            TK::Catch { e: x, bind, handler } => {
                let xt = self.sub(fs, &x.ty);
                let v = self.expr(fs, x);
                let xc = self.cty(&xt);
                let tv = self.tmp();
                self.emit(fs, &format!("{} {} = {};", xc, tv, v));
                let has_val = !matches!(t, Ty::Unit | Ty::Never);
                let r = if has_val {
                    let c = self.cty(&t);
                    let r = self.tmp();
                    self.emit(fs, &format!("{} {};", c, r));
                    Some(r)
                } else {
                    None
                };
                let is_opt = matches!(xt, Ty::Option(_));
                let okf = if is_opt { self.osome(&xt, &tv) } else { format!("{}.ok", tv) };
                let valf = if is_opt { self.oval(&xt, &tv) } else { format!("{}.u.val", tv) };
                self.emit(fs, &format!("if ({}) {{", okf));
                if let Some(r) = &r {
                    self.emit(fs, &format!("    {} = {};", r, valf));
                }
                self.emit(fs, "} else {");
                fs.ind += 1;
                self.push_scope(fs, SK::Block);
                if let Ty::Result(_, et) = &xt {
                    let et = (**et).clone();
                    let ec = self.cty(&et);
                    match bind {
                        Some(b) => {
                            let n = self.local_cname(fs, *b);
                            self.emit(fs, &format!("{} {} = {}.u.err;", ec, n, tv));
                            fs.repr.insert(*b, Repr::Val(n));
                            self.declare_flags(fs, *b);
                            if self.needs_drop(&et) {
                                self.register(fs, Cl::DropLocal(*b));
                            }
                        }
                        None => {
                            if self.needs_drop(&et) {
                                let d = self.drop_fn(&et);
                                self.emit(fs, &format!("{}(&{}.u.err);", d, tv));
                            }
                        }
                    }
                }
                self.block_into(fs, handler, r.as_deref());
                let later = self.pop_scope_keeping_later(fs);
                fs.ind -= 1;
                self.emit(fs, "}");
                fs.scopes.last_mut().unwrap().items.extend(later);
                r.unwrap_or_else(|| "0".into())
            }
            TK::Coalesce { l, r } => {
                let lt = self.sub(fs, &l.ty);
                let lv = self.expr(fs, l);
                let lc = self.cty(&lt);
                let tv = self.tmp();
                self.emit(fs, &format!("{} {} = {};", lc, tv, lv));
                let c = self.cty(&t);
                let res = self.tmp();
                self.emit(fs, &format!("{} {};", c, res));
                let flat = matches!(t, Ty::Option(_)) && t == lt;
                let cond = self.osome(&lt, &tv);
                let val = if flat { tv.clone() } else { self.oval(&lt, &tv) };
                self.emit(fs, &format!("if ({}) {} = {}; else {{", cond, res, val));
                fs.ind += 1;
                self.push_value_scope(fs, &t);
                let rv = self.expr(fs, r);
                if r.ty != Ty::Never {
                    self.emit(fs, &format!("{} = {};", res, rv));
                }
                let later = self.pop_scope_keeping_later(fs);
                fs.ind -= 1;
                self.emit(fs, "}");
                fs.scopes.last_mut().unwrap().items.extend(later);
                res
            }
            TK::OptField { base, index } => {
                let bt = self.sub(fs, &base.ty);
                let inner = match &bt {
                    Ty::Option(i) => (**i).clone(),
                    _ => unreachable!(),
                };
                let (d, args) = match &inner {
                    Ty::Adt(d, a) => (*d, a.clone()),
                    _ => unreachable!(),
                };
                let fsx = self.prog.field_tys(d, &args);
                let (fname, fty) = fsx[*index].clone();
                let owned = !base.is_place();
                let bv = self.expr(fs, base);
                let bc = self.cty(&bt);
                let tb = self.tmp();
                self.emit(fs, &format!("{} {} = {};", bc, tb, bv));
                let c = self.cty(&t);
                let res = self.tmp();
                let flat = matches!(fty, Ty::Option(_));
                self.emit(fs, &format!("{} {} = {{0}};", c, res));
                self.emit(fs, &format!("if ({}.some) {{", tb));
                fs.ind += 1;
                if flat {
                    self.emit(fs, &format!("{} = {}.val.f_{};", res, tb, fname));
                } else {
                    let st = self.oset(&t, &res, &format!("{}.val.f_{}", tb, fname));
                    self.emit(fs, &st);
                }
                if owned {
                    for (i, (n2, t2)) in fsx.iter().enumerate() {
                        if i != *index && self.needs_drop(t2) {
                            let dfn = self.drop_fn(t2);
                            self.emit(fs, &format!("{}(&{}.val.f_{});", dfn, tb, n2));
                        }
                    }
                }
                fs.ind -= 1;
                self.emit(fs, "}");
                res
            }
            TK::Move(x) => {
                if let Some((l, path)) = local_path(x) {
                    if let Some(f) = fs.flags.get(&(l, path.clone())).cloned() {
                        self.set_flag(fs, &f, false);
                    }
                }
                self.place(fs, x)
            }
            TK::Copy(x) => self.place(fs, x),
            TK::Coerce(k, x) => self.coerce(fs, k, x, &t),
            TK::Recv(ch) => {
                let c = self.expr(fs, ch);
                let oc = self.cty(&t);
                let r = self.tmp();
                self.emit(fs, &format!("{} {} = {{0}};", oc, r));
                if self.niche(&t) {
                    self.emit(fs, &format!("if (!gojo_chan_recv({}, &{})) {} = NULL;", c, r, r));
                } else {
                    self.emit(fs, &format!("{}.some = gojo_chan_recv({}, &{}.val);", r, c, r));
                }
                r
            }
            TK::Wrap(x) => {
                let v = self.expr(fs, x);
                match &t {
                    Ty::Box(inner) => {
                        let ic = self.cty(inner);
                        let r = self.tmp();
                        self.emit(fs, &format!("{} *{} = GOJO_BOX_NEW_R({}); *{} = {};", ic, r, ic, r, v));
                        r
                    }
                    Ty::Rc(inner) | Ty::Arc(inner) => {
                        let bx = self.ensure_rcbox(inner);
                        let r = self.tmp();
                        self.emit(fs, &format!("{} *{} = gojo_box_alloc(sizeof({})); {}->strong = 1; {}->weak = 0; {}->val = {};", bx, r, bx, r, r, r, v));
                        r
                    }
                    Ty::Mutex(_) => {
                        let c = self.cty(&t);
                        let l = self.loc(e.span);
                        format!("(({}){{ gojo_mutex_new({}), {} }})", c, l, v)
                    }
                    _ => v,
                }
            }
            TK::MakeChan(n) => {
                let v = self.expr(fs, n);
                let el = match &t {
                    Ty::Adt(_, args) => args[0].clone(),
                    _ => unreachable!(),
                };
                let ec = self.cty(&el);
                let l = self.loc(e.span);
                let n = self.tmp();
                self.emit(fs, &format!("int64_t {} = {};", n, v));
                self.emit(fs, &format!("if ({} < 0) gojo_panic(\"negative channel capacity\", {});", n, l));
                self.emit(fs, &format!("if ({n} > (int64_t)(PTRDIFF_MAX / (sizeof({ec}) ? sizeof({ec}) : 1))) gojo_panic(\"channel capacity too large\", {l});"));
                // `Chan[T] { tx, rx }`: both ends of the new channel
                let c = self.tmp();
                self.emit(fs, &format!("gojo_chan *{} = gojo_chan_new(sizeof({}), {}, {});", c, ec, n, l));
                let st = self.cty(&t);
                format!("(({}){{ {}, {} }})", st, c, c)
            }
            TK::DefaultOf => self.default_value(fs, &t),
            TK::MutexRef(x) => {
                let p = self.lval(fs, x);
                let c = self.cty(&t);
                let l = self.loc(e.span);
                format!("(({}){{ gojo_mutex_new({}), &{} }})", c, l, p)
            }
            TK::Unbox(x) => {
                let b = self.expr(fs, x);
                let c = self.cty(&t);
                let bp = self.tmp();
                let r = self.tmp();
                // (reuse_boxes may hand the block to a `Box(...)` that follows)
                self.emit(fs, &format!("{} *{} = {}; {} {} = *{}; GOJO_BOX_FREE_R({}, {});", c, bp, b, c, r, bp, bp, c));
                r
            }
            TK::Error => "0".into(),
        }
    }

    fn rvalue_field(&mut self, fs: &mut Fs, base: &TExpr, index: usize, t: &Ty) -> String {
        let bt = self.sub(fs, &base.ty);
        let bv = self.expr(fs, base);
        let bc = self.cty(&bt);
        let n = self.tmp();
        self.emit(fs, &format!("{} {} = {};", bc, n, bv));
        let (d, args) = match &bt {
            Ty::Adt(d, a) => (*d, a.clone()),
            _ => unreachable!(),
        };
        let fsx = self.prog.field_tys(d, &args);
        if self.is_copy(t) {
            if self.needs_drop(&bt) {
                self.register_stmt(fs, Cl::DropTemp(n.clone(), bt.clone()));
            }
            return format!("{}.f_{}", n, fsx[index].0);
        }
        let fc = self.cty(t);
        let r = self.tmp();
        self.emit(fs, &format!("{} {} = {}.f_{};", fc, r, n, fsx[index].0));
        for (i, (fname, ft)) in fsx.iter().enumerate() {
            if i != index && self.needs_drop(ft) {
                let dfn = self.drop_fn(ft);
                self.emit(fs, &format!("{}(&{}.f_{});", dfn, n, fname));
            }
        }
        r
    }

    fn field_default(&mut self, fs: &mut Fs, d: DefId, targs: &[Ty], i: usize) -> String {
        let def = self.prog.def(d);
        let fi = &def.fields()[i];
        let ft = self.prog.field_tys(d, targs)[i].1.clone();
        match &fi.default {
            Some(dexpr) => {
                let dexpr = dexpr.clone();
                let saved = fs.subst.clone();
                for (p, a) in def.tparams.iter().zip(targs.iter()) {
                    fs.subst.insert(*p, a.clone());
                }
                let v = self.expr(fs, &dexpr);
                fs.subst = saved;
                v
            }
            None => self.default_value(fs, &ft),
        }
    }

    fn default_value(&mut self, fs: &mut Fs, t: &Ty) -> String {
        match t {
            Ty::Adt(d, args) if self.prog.def(*d).newtype => {
                let inner = self.prog.field_tys(*d, args)[0].1.clone();
                let v = self.default_value(fs, &inner);
                let c = self.cty(t);
                format!("(({}){{ {} }})", c, v)
            }
            Ty::Adt(d, args) if self.prog.def(*d).is_struct() => {
                let c = self.cty(t);
                let n = self.prog.def(*d).fields().len();
                let fsx = self.prog.field_tys(*d, args);
                let mut parts = vec![];
                for i in 0..n {
                    let v = self.field_default(fs, *d, args, i);
                    parts.push(format!(".f_{} = {}", fsx[i].0, v));
                }
                if parts.is_empty() {
                    return format!("(({}){{0}})", c);
                }
                let r = self.tmp();
                self.emit(fs, &format!("{} {} = {{ {} }};", c, r, parts.join(", ")));
                r
            }
            Ty::Array(el, n) => {
                let c = self.cty(t);
                let r = self.tmp();
                self.emit(fs, &format!("{} {};", c, r));
                let i = self.tmp();
                self.emit(fs, &format!("for (int64_t {} = 0; {} < {}; {}++) {{", i, i, n, i));
                fs.ind += 1;
                let el = (**el).clone();
                let v = self.default_value(fs, &el);
                self.emit(fs, &format!("{}.a[{}] = {};", r, i, v));
                fs.ind -= 1;
                self.emit(fs, "}");
                r
            }
            Ty::Mutex(inner) => {
                let inner = (**inner).clone();
                let v = self.default_value(fs, &inner);
                let c = self.cty(t);
                format!("(({}){{ gojo_mutex_new(NULL), {} }})", c, v)
            }
            Ty::Cell(inner) => {
                let inner = (**inner).clone();
                self.default_value(fs, &inner)
            }
            _ => self.zero(t),
        }
    }

    fn coerce(&mut self, fs: &mut Fs, k: &Coercion, x: &TExpr, t: &Ty) -> String {
        match k {
            Coercion::StrToString => {
                let v = self.expr(fs, x);
                format!("gojo_string_from({})", v)
            }
            Coercion::StringToStr => {
                let p = self.lval(fs, x);
                format!("gojo_string_view(&{})", p)
            }
            Coercion::VecToSlice => {
                let p = self.lval(fs, x);
                format!("((gojo_slice){{ {}.ptr, {}.len }})", p, p)
            }
            Coercion::ArrayToSlice => {
                let xt = self.sub(fs, &x.ty);
                let n = match &xt {
                    Ty::Array(_, n) => *n,
                    _ => 0,
                };
                let p = self.lval(fs, x);
                format!("((gojo_slice){{ {}.a, {} }})", p, n)
            }
            Coercion::ToIface { owned } => {
                let xt = self.sub(fs, &x.ty);
                let vt = self.vtable(&xt, t);
                if *owned {
                    let v = self.expr(fs, x);
                    let c = self.cty(&xt);
                    let b = self.tmp();
                    self.emit(fs, &format!("{} *{} = GOJO_BOX_NEW_R({}); *{} = {};", c, b, c, b, v));
                    format!("((gojo_iface){{ {}, &{} }})", b, vt)
                } else {
                    let p = self.addr(fs, x);
                    format!("((gojo_iface){{ {}, &{} }})", p, vt)
                }
            }
            Coercion::FuncItem => {
                let xt = self.sub(fs, &x.ty);
                let _ = xt;
                match &x.kind {
                    TK::FuncRef(f, targs) => {
                        let targs: Vec<Ty> = targs.iter().map(|a| self.sub(fs, a)).collect();
                        self.func_item(*f, targs)
                    }
                    _ => self.expr(fs, x),
                }
            }
        }
    }

    fn func_item(&mut self, f: FuncId, targs: Vec<Ty>) -> String {
        let name = self.instance(f, targs.clone());
        let tramp = format!("tr_{}", name);
        if self.helpers.insert(tramp.clone()) {
            let fd = self.prog.func(f).clone();
            let s: HashMap<TParamId, Ty> = fd.tparams.iter().copied().zip(targs.iter().cloned()).collect();
            let ret = fd.ret.subst(&s);
            let rc = self.ret_cty(&ret);
            let mut params = vec!["void *env".to_string()];
            let mut args = vec![];
            for (i, p) in fd.params.iter().enumerate() {
                let t = p.ty.subst(&s);
                let c = self.param_cty(p.mode, &t);
                params.push(format!("{} a{}", c, i));
                args.push(format!("a{}", i));
            }
            let _ = writeln!(self.protos, "static {} {}({});", rc, tramp, params.join(", "));
            let _ = writeln!(self.bodies, "static {} {}({}) {{ (void)env; {}{}({}); }}", rc, tramp, params.join(", "), if rc == "void" { "" } else { "return " }, name, args.join(", "));
        }
        format!("((gojo_func){{ (void *){}, NULL, NULL }})", tramp)
    }

    fn int_cast_check(&mut self, v: &str, from: &Ty, to: IntTy, loc: &str) -> String {
        match from {
            Ty::Int(f) => {
                let f = *f;
                if f == to {
                    return v.to_string();
                }
                let fits = (f.signed() == to.signed() && to.bits() >= f.bits()) || (!f.signed() && to.signed() && to.bits() > f.bits());
                if fits {
                    return format!("(({}){})", to.c_name(), v);
                }
                if f.signed() {
                    let (lo, hi) = (to.min(), to.max().min(i64::MAX as i128));
                    format!("({{ int64_t _v = (int64_t){}; gojo_check_irange(_v, {}, {}, {}); ({})_v; }})", v, int_lit(lo, &Ty::int()), int_lit(hi, &Ty::int()), loc, to.c_name())
                } else {
                    format!("({{ uint64_t _v = (uint64_t){}; gojo_check_urange(_v, {}ULL, {}); ({})_v; }})", v, to.max(), loc, to.c_name())
                }
            }
            Ty::F32 | Ty::F64 => {
                let (lo, hi) = (to.min() as f64, to.max() as f64);
                if to == IntTy::U64 {
                    format!("(({})gojo_check_range((double){}, 0.0, 18446744073709549568.0, {}) , ({}){})", to.c_name(), v, loc, to.c_name(), v)
                } else {
                    format!("(({})gojo_check_range((double){}, {:?}, {:?}, {}))", to.c_name(), v, lo, if to == IntTy::I64 { 9223372036854774784.0 } else { hi }, loc)
                }
            }
            Ty::Bool => format!("(({})({} ? 1 : 0))", to.c_name(), v),
            Ty::Adt(..) => format!("(({})({}).tag)", to.c_name(), v),
            _ => format!("(({}){})", to.c_name(), v),
        }
    }

    fn cast(&mut self, fs: &mut Fs, x: &TExpr, t: &Ty, sp: Span) -> String {
        let mut xt = self.sub(fs, &x.ty);
        let mut v = self.expr(fs, x);
        let l = self.loc(sp);
        // new types (reached through generic code): unwrap the source, wrap the target
        if let Some(inner) = self.numeric_newtype(&xt) {
            v = format!("({}).f_value", v);
            xt = inner;
        }
        if let Some(inner) = self.numeric_newtype(t) {
            let iv = match &inner {
                Ty::Int(k) => self.int_cast_check(&v, &xt, *k, &l),
                Ty::F32 => format!("((float){})", v),
                Ty::F64 => format!("((double){})", v),
                _ => v,
            };
            let c = self.cty(t);
            return format!("(({}){{ {} }})", c, iv);
        }
        match t {
            Ty::Int(k) => self.int_cast_check(&v, &xt, *k, &l),
            Ty::F32 => format!("((float){})", v),
            Ty::F64 => format!("((double){})", v),
            _ => v,
        }
    }

    fn arith(&mut self, op: BinOp, a: &str, b: &str, t: &Ty, sp: Span) -> String {
        if let Some(inner) = self.numeric_newtype(t) {
            let v = self.arith(op, &format!("({}).f_value", a), &format!("({}).f_value", b), &inner, sp);
            let c = self.cty(t);
            return format!("(({}){{ {} }})", c, v);
        }
        let l = self.loc(sp);
        match t {
            Ty::Int(k) => {
                let s = int_suffix(*k);
                let uc = match k.bits() {
                    8 => "uint8_t",
                    16 => "uint16_t",
                    32 => "uint32_t",
                    _ => "uint64_t",
                };
                let c = k.c_name();
                match op {
                    BinOp::Add => format!("gojo_add_{}({}, {}, {})", s, a, b, l),
                    BinOp::Sub => format!("gojo_sub_{}({}, {}, {})", s, a, b, l),
                    BinOp::Mul => format!("gojo_mul_{}({}, {}, {})", s, a, b, l),
                    BinOp::Div => format!("gojo_div_{}({}, {}, {})", s, a, b, l),
                    BinOp::Rem => format!("gojo_rem_{}({}, {}, {})", s, a, b, l),
                    BinOp::WAdd => format!("(({})(({})({}) + ({})({})))", c, uc, a, uc, b),
                    BinOp::WSub => format!("(({})(({})({}) - ({})({})))", c, uc, a, uc, b),
                    BinOp::WMul => format!("(({})(({})({}) * ({})({})))", c, uc, a, uc, b),
                    BinOp::BitAnd => format!("(({})({} & {}))", c, a, b),
                    BinOp::BitOr => format!("(({})({} | {}))", c, a, b),
                    BinOp::BitXor => format!("(({})({} ^ {}))", c, a, b),
                    BinOp::Shl => format!("gojo_shl_{}({}, (int64_t)({}), {})", s, a, b, l),
                    BinOp::Shr => format!("gojo_shr_{}({}, (int64_t)({}), {})", s, a, b, l),
                    _ => unreachable!(),
                }
            }
            _ => {
                let o = match op {
                    BinOp::Add => "+",
                    BinOp::Sub => "-",
                    BinOp::Mul => "*",
                    BinOp::Div => "/",
                    BinOp::Rem => return format!("fmod({}, {})", a, b),
                    _ => "+",
                };
                format!("({} {} {})", a, o, b)
            }
        }
    }

    fn binary(&mut self, fs: &mut Fs, op: BinOp, l: &TExpr, r: &TExpr, sp: Span) -> String {
        if matches!(op, BinOp::And | BinOp::Or) {
            let a = self.expr(fs, l);
            let t = self.tmp();
            self.emit(fs, &format!("bool {} = {};", t, a));
            self.emit(fs, &format!("if ({}{}) {{", if op == BinOp::And { "" } else { "!" }, t));
            fs.ind += 1;
            self.push_scope(fs, SK::Stmt);
            // `r` runs only where `l` is true (`&&`) or false (`||`)
            let facts = self.bce_guard(fs, l, op == BinOp::And);
            let facts = self.bce_surviving(fs, facts, r);
            let nb = fs.bce.len();
            fs.bce.extend(facts);
            let b = self.expr(fs, r);
            fs.bce.truncate(nb);
            self.emit(fs, &format!("{} = {};", t, b));
            self.pop_scope(fs);
            fs.ind -= 1;
            self.emit(fs, "}");
            return t;
        }
        let lt = self.sub(fs, &l.ty);
        if self.numeric_newtype(&lt).is_some() {
            let a = self.expr(fs, l);
            let a = if has_effects(r) { self.materialize(fs, &a, &lt, false) } else { a };
            let b = self.expr(fs, r);
            if op.is_cmp() {
                return format!("(({}).f_value {} ({}).f_value)", a, cmp_op(op), b);
            }
            return self.arith(op, &a, &b, &lt, sp);
        }
        if op.is_cmp() {
            if self.is_scalar(&lt) {
                let a = self.expr(fs, l);
                let a = if has_effects(r) { self.materialize(fs, &a, &lt, false) } else { a };
                let b = self.expr(fs, r);
                return format!("({} {} {})", a, cmp_op(op), b);
            }
            if lt == Ty::Str {
                let a = self.expr(fs, l);
                let a = if has_effects(r) { self.materialize(fs, &a, &lt, false) } else { a };
                let b = self.expr(fs, r);
                return match op {
                    BinOp::Eq => format!("gojo_str_eq({}, {})", a, b),
                    BinOp::Ne => format!("(!gojo_str_eq({}, {}))", a, b),
                    _ => format!("(gojo_str_cmp({}, {}) {} 0)", a, b, cmp_op(op)),
                };
            }
            if let Ty::Adt(d, _) = &lt {
                if self.prog.def(*d).is_simple_enum() && !self.prog.methods.contains_key(&(TyKey::Adt(*d), "equal".into())) && !self.prog.methods.contains_key(&(TyKey::Adt(*d), "compare".into())) {
                    let a = self.lval(fs, l);
                    let b = self.lval(fs, r);
                    return format!("({}.tag {} {}.tag)", a, cmp_op(op), b);
                }
            }
            let a = self.addr(fs, l);
            let b = self.addr(fs, r);
            return match op {
                BinOp::Eq | BinOp::Ne => {
                    let f = self.eq_fn(&lt);
                    format!("({}{}({}, {}))", if op == BinOp::Ne { "!" } else { "" }, f, a, b)
                }
                _ => {
                    let f = self.cmp_fn(&lt);
                    format!("({}({}, {}) {} 0)", f, a, b, cmp_op(op))
                }
            };
        }
        let a = self.expr(fs, l);
        let a = if has_effects(r) { self.materialize(fs, &a, &lt, false) } else { a };
        let b = self.expr(fs, r);
        self.arith(op, &a, &b, &lt, sp)
    }

    // ------------------------------------------------------------------
    // Calls
    // ------------------------------------------------------------------

    fn call_expr(&mut self, fs: &mut Fs, e: &TExpr) -> String {
        let (callee, args) = match &e.kind {
            TK::Call { callee, args } => (callee, args),
            TK::Proj { call, .. } => return self.call_expr(fs, call),
            _ => return self.expr(fs, e),
        };
        let t = self.sub(fs, &e.ty);
        match callee {
            Callee::Func(f, targs) => {
                let targs: Vec<Ty> = targs.iter().map(|x| self.sub(fs, x)).collect();
                self.call_func(fs, *f, targs, args, &t, e.span)
            }
            Callee::ParamMethod { name, recv_ty, .. } => {
                let rt = self.sub(fs, recv_ty);
                if let Ty::Iface(d, _) = &rt {
                    let mi = match &self.prog.def(*d).kind {
                        TypeDefKind::Iface { methods, .. } => methods.iter().position(|m| m.name == *name).unwrap(),
                        _ => 0,
                    };
                    return self.call_iface(fs, *d, mi, args, &t, &rt);
                }
                let key = TyKey::of(&rt).unwrap();
                let list = self.prog.methods.get(&(key, name.clone())).cloned().unwrap_or_default();
                for f in list {
                    let fd = self.prog.func(f);
                    let mut s = HashMap::new();
                    if bind_params(&fd.recv.as_ref().unwrap().ty, &rt, &mut s) {
                        let targs: Vec<Ty> = fd.tparams.iter().map(|p| s.get(p).cloned().unwrap_or(Ty::Unit)).collect();
                        // the bound's method may take `self inout` where this
                        // type's only reads it: pass the receiver the way it wants
                        if fd.recv.as_ref().is_some_and(|r| r.mode == Mode::Borrow) && args.first().is_some_and(|a| a.mode == ArgMode::Inout) {
                            let mut a2 = args.to_vec();
                            a2[0].mode = ArgMode::Borrow;
                            return self.call_func(fs, f, targs, &a2, &t, e.span);
                        }
                        return self.call_func(fs, f, targs, args, &t, e.span);
                    }
                }
                panic!("unresolved method {} on {}", name, self.prog.show(&rt));
            }
            Callee::Iface { def, method } => {
                let rt = self.sub(fs, &args[0].e.ty);
                self.call_iface(fs, *def, *method, args, &t, &rt)
            }
            Callee::Value => {
                let ft = self.sub(fs, &args[0].e.ty);
                let fty = match &ft {
                    Ty::Func(f) => f.clone(),
                    _ => unreachable!(),
                };
                let fp = self.borrow_ptr(fs, &args[0].e);
                let mut ptys = vec!["void*".to_string()];
                for (m, p) in &fty.params {
                    ptys.push(self.param_cty(*m, p));
                }
                let modes: Vec<Mode> = fty.params.iter().map(|p| p.0).collect();
                let pts: Vec<Ty> = fty.params.iter().map(|p| p.1.clone()).collect();
                let avs = self.args(fs, &args[1..], &modes, &pts);
                let rc = self.ret_cty(&fty.ret);
                let fpn = self.tmp();
                self.emit(fs, &format!("gojo_func *{} = {};", fpn, fp));
                let mut all = vec![format!("{}->env", fpn)];
                all.extend(avs);
                let call = format!("(({} (*)({})){}->fn)({})", rc, ptys.join(", "), fpn, all.join(", "));
                self.finish_call(fs, call, &fty.ret)
            }
        }
    }

    fn finish_call(&mut self, fs: &mut Fs, call: String, ret: &Ty) -> String {
        if matches!(ret, Ty::Unit | Ty::Never) {
            self.emit(fs, &format!("{};", call));
            return "0".into();
        }
        let c = self.cty(ret);
        let r = self.tmp();
        self.emit(fs, &format!("{} {} = {};", c, r, call));
        r
    }

    /// Evaluate arguments left to right into C expressions.
    fn args(&mut self, fs: &mut Fs, args: &[TArg], modes: &[Mode], tys: &[Ty]) -> Vec<String> {
        let mut out = vec![];
        let mut held: Vec<String> = vec![];
        for (i, a) in args.iter().enumerate() {
            let later_effects = args[i + 1..].iter().any(|x| has_effects(&x.e));
            let pt = tys.get(i).cloned().unwrap_or_else(|| self.sub(fs, &a.e.ty));
            let m = modes.get(i).copied().unwrap_or(Mode::Borrow);
            let s = match a.mode {
                ArgMode::Value => {
                    let v = self.expr(fs, &a.e);
                    let t = self.sub(fs, &a.e.ty);
                    if args[i + 1..].iter().any(|x| may_leave(&x.e)) {
                        // held until the call: a later argument's `try` may leave first
                        let (v, flag) = self.hold(fs, &v, &t);
                        held.extend(flag);
                        v
                    } else if later_effects && !is_simple_c(&v) {
                        self.materialize(fs, &v, &t, false)
                    } else {
                        v
                    }
                }
                ArgMode::Borrow => {
                    if m == Mode::Borrow && self.is_copy(&pt) {
                        let v = self.expr(fs, &a.e);
                        if later_effects && !is_simple_c(&v) {
                            self.materialize(fs, &v, &pt, false)
                        } else {
                            v
                        }
                    } else {
                        self.addr(fs, &a.e)
                    }
                }
                ArgMode::Inout => self.addr(fs, &a.e),
            };
            out.push(s);
        }
        for f in held {
            self.emit(fs, &format!("{} = 0;", f));
        }
        out
    }

    fn call_func(&mut self, fs: &mut Fs, f: FuncId, targs: Vec<Ty>, args: &[TArg], ret: &Ty, sp: Span) -> String {
        let fd = self.prog.func(f).clone();
        if fd.kind == FuncKind::Intrinsic {
            return self.intrinsic(fs, f, &targs, args, ret, sp);
        }
        // unwrap/expect report the caller's location
        if fd.pkg == self.prog.prelude && matches!(fd.recv_key, Some(TyKey::Option) | Some(TyKey::Result)) && matches!(fd.name.as_str(), "unwrap" | "expect") {
            let rt = fd.recv.as_ref().unwrap().ty.subst(&fd.tparams.iter().copied().zip(targs.iter().cloned()).collect());
            let v = self.expr(fs, &args[0].e);
            let c = self.cty(&rt);
            let t = self.tmp();
            self.emit(fs, &format!("{} {} = {};", c, t, v));
            let l = self.loc(sp);
            let msg = if fd.name == "expect" { Some(self.expr(fs, &args[1].e)) } else { None };
            match &rt {
                Ty::Option(_) => {
                    let m = msg.unwrap_or_else(|| "gojo_str_lit(\"unwrap() called on none\", 23)".into());
                    let cond = self.osome(&rt, &t);
                    self.emit(fs, &format!("if (!{}) gojo_panic_str({}, {});", cond, m, l));
                    return if *ret == Ty::Unit { "0".into() } else { self.oval(&rt, &t) };
                }
                Ty::Result(_, e) => {
                    let e = (**e).clone();
                    let fm = self.fmt_fn(&e);
                    let m = msg.unwrap_or_else(|| "gojo_str_lit(\"unwrap() called on an error\", 27)".into());
                    let b = self.tmp();
                    self.emit(fs, &format!("if (!{}.ok) {{ gojo_string {} = {{0}}; gojo_string_push(&{}, {}); gojo_fmt_cstr(&{}, \": \"); {}(&{}, &{}.u.err, 0); gojo_panic_buf(&{}, {}); }}", t, b, b, m, b, fm, b, t, b, l));
                    return if *ret == Ty::Unit { "0".into() } else { format!("{}.u.val", t) };
                }
                _ => {}
            }
        }
        let s: HashMap<TParamId, Ty> = fd.tparams.iter().copied().zip(targs.iter().cloned()).collect();
        let mut modes = vec![];
        let mut tys = vec![];
        if let Some(r) = &fd.recv {
            modes.push(r.mode);
            tys.push(r.ty.subst(&s));
        }
        for p in &fd.params {
            modes.push(p.mode);
            tys.push(p.ty.subst(&s));
        }
        let avs = self.args(fs, args, &modes, &tys);
        let name = self.instance(f, targs);
        let call = format!("{}({})", name, avs.join(", "));
        if fd.yields.is_some() {
            let r = self.tmp();
            let rt = fd.ret.subst(&s);
            let c = self.cty(&rt);
            self.emit(fs, &format!("{} *{} = {};", c, r, call));
            return r;
        }
        self.finish_call(fs, call, ret)
    }

    fn call_iface(&mut self, fs: &mut Fs, d: DefId, mi: usize, args: &[TArg], ret: &Ty, rt: &Ty) -> String {
        let (vtn, ms) = self.vt_struct(d, rt);
        let m = ms.method(mi).clone();
        // a `sink self` method takes the value out of the box: the box goes
        // after the call, without dropping what was in it
        let owned = args[0].mode == ArgMode::Value;
        let recv = if owned {
            let v = self.expr(fs, &args[0].e);
            let t = self.tmp();
            self.emit(fs, &format!("gojo_iface {} = {};", t, v));
            t
        } else {
            self.lval(fs, &args[0].e)
        };
        let modes: Vec<Mode> = m.1.iter().map(|p| p.1).collect();
        let tys: Vec<Ty> = m.1.iter().map(|p| p.2.clone()).collect();
        let avs = self.args(fs, &args[1..], &modes, &tys);
        let mut all = vec![format!("{}.data", recv)];
        all.extend(avs);
        let call = format!("(({} *){}.vt)->m{}({})", vtn, recv, mi, all.join(", "));
        let r = self.finish_call(fs, call, ret);
        if owned {
            self.emit(fs, &format!("gojo_box_free({}.data, ((const gojo_vt_base *){}.vt)->size);", recv, recv));
        }
        r
    }

    // ------------------------------------------------------------------
    // Interfaces
    // ------------------------------------------------------------------

    /// vtable struct for an interface type; returns (name, method infos substituted)
    fn vt_struct(&mut self, d: DefId, it: &Ty) -> (String, IfaceMethodSub) {
        let args = match it {
            Ty::Iface(_, a) => a.to_vec(),
            _ => vec![],
        };
        let def = self.prog.def(d).clone();
        let s = self.prog.def_subst(d, &args);
        let methods = match &def.kind {
            TypeDefKind::Iface { methods, .. } => methods.clone(),
            _ => vec![],
        };
        let n = format!("VT_{}", self.mangle(it));
        if self.helpers.insert(n.clone()) {
            // The common part is a `gojo_vt_base` member rather than the same
            // fields repeated, so code that reads it through a `gojo_vt_base *`
            // (drops, printing, type tests) accesses an object of that type.
            let mut body = format!("typedef struct {{ gojo_vt_base base;");
            for (i, m) in methods.iter().enumerate() {
                let ret = m.ret.subst(&s);
                let rc = self.ret_cty(&ret);
                let mut ps = vec!["void *".to_string()];
                for (_, pm, pt) in &m.params {
                    let pt = pt.subst(&s);
                    ps.push(self.param_cty(*pm, &pt));
                }
                let _ = write!(body, " {} (*m{})({});", rc, i, ps.join(", "));
            }
            let _ = writeln!(body, " }} {};", n);
            self.tdefs.push_str(&body);
        }
        let ms = methods.iter().map(|m| (m.name.clone(), m.params.iter().map(|(a, b, c)| (a.clone(), *b, c.subst(&s))).collect::<Vec<_>>(), m.ret.subst(&s))).collect::<Vec<_>>();
        (n, IfaceMethodSub::all(ms))
    }


    fn vtable(&mut self, concrete: &Ty, it: &Ty) -> String {
        let (d, _) = match it {
            Ty::Iface(d, a) => (*d, a.clone()),
            _ => unreachable!(),
        };
        let (vtn, ms) = self.vt_struct(d, it);
        let name = format!("vt_{}_{}", self.mangle(it), self.mangle(concrete));
        if self.helpers.insert(name.clone()) {
            let cc = self.cty(concrete);
            let dropf = self.drop_fn(concrete);
            let fmtf = self.fmt_fn(concrete);
            let tid = self.type_id(concrete);
            let wd = format!("{}_drop", name);
            let wf = format!("{}_fmt", name);
            let _ = writeln!(self.protos, "static void {}(void *p);\nstatic void {}(gojo_string *b, void *p, int n);", wd, wf);
            let _ = writeln!(self.bodies, "static void {}(void *p) {{ {}(({} *)p); }}\nstatic void {}(gojo_string *b, void *p, int n) {{ {}(b, ({} *)p, n); }}", wd, dropf, cc, wf, fmtf, cc);
            let mut entries = vec![format!("{{ {}, {}, {}, sizeof({}) }}", wd, wf, tid, cc)];
            for (i, (mname, params, ret)) in ms.methods.iter().enumerate() {
                // find the concrete method
                let key = TyKey::of(concrete).unwrap();
                let mut target = None;
                for f in self.prog.methods.get(&(key, mname.clone())).cloned().unwrap_or_default() {
                    let fd = self.prog.func(f);
                    let mut s = HashMap::new();
                    if bind_params(&fd.recv.as_ref().unwrap().ty, concrete, &mut s) {
                        let targs: Vec<Ty> = fd.tparams.iter().map(|p| s.get(p).cloned().unwrap_or(Ty::Unit)).collect();
                        target = Some((f, targs));
                        break;
                    }
                }
                let (f, targs) = target.expect("vtable method");
                let fd = self.prog.func(f).clone();
                let fname = self.instance(f, targs);
                let rc = self.ret_cty(ret);
                let mut ps = vec!["void *self".to_string()];
                let rm = fd.recv.as_ref().unwrap().mode;
                let selfarg = match rm {
                    Mode::Borrow if self.is_copy(concrete) => format!("*({} *)self", cc),
                    Mode::Sink => format!("*({} *)self", cc),
                    _ => format!("({} *)self", cc),
                };
                let mut call_args = vec![selfarg];
                for (j, (_, pm, pt)) in params.iter().enumerate() {
                    let c = self.param_cty(*pm, pt);
                    ps.push(format!("{} a{}", c, j));
                    call_args.push(format!("a{}", j));
                }
                let w = format!("{}_m{}", name, i);
                let _ = writeln!(self.protos, "static {} {}({});", rc, w, ps.join(", "));
                let _ = writeln!(self.bodies, "static {} {}({}) {{ {}{}({}); }}", rc, w, ps.join(", "), if rc == "void" { "" } else { "return " }, fname, call_args.join(", "));
                entries.push(w);
            }
            let _ = writeln!(self.statics, "static const {} {} = {{ {} }};", vtn, name, entries.join(", "));
        }
        name
    }

    // ------------------------------------------------------------------
    // Closures
    // ------------------------------------------------------------------

    fn closure(&mut self, fs: &mut Fs, c: &TClosure, t: &Ty) -> String {
        let fty = match t {
            Ty::Func(f) => f.clone(),
            _ => unreachable!(),
        };
        self.ntmp += 1;
        let id = self.ntmp;
        let fname = format!("clo{}", id);
        let envn = format!("Env{}", id);
        // capture layout
        let mut fields = vec![];
        for (l, _) in &c.captures {
            let lt = self.local_ty(fs, *l);
            let lc = self.cty(&lt);
            fields.push((*l, lt, lc));
        }
        let by_value = c.owned;
        let byv = |l: &LocalId| by_value || c.by_value.contains(l);
        let lbp = if c.loop_body.is_some() { self.pending_lb.take() } else { None };
        let outer_rc = if lbp.is_some() && !matches!(fs.ret, Ty::Unit | Ty::Never) { Some(self.cty(&fs.ret.clone())) } else { None };
        if !fields.is_empty() || lbp.is_some() {
            let mut body = format!("typedef struct {{");
            for (l, _, lc) in &fields {
                if byv(l) {
                    let _ = write!(body, " {} c{};", lc, l);
                } else {
                    let _ = write!(body, " {} *c{};", lc, l);
                }
            }
            if lbp.is_some() {
                let _ = write!(body, " int *kctl; {} *kret;", outer_rc.clone().unwrap_or_else(|| "void".into()));
            }
            let _ = writeln!(body, " }} {};", envn);
            self.tdefs.push_str(&body);
        }
        // function
        let ret = fty.ret.clone();
        let rc = self.ret_cty(&ret);
        let mut ps = vec!["void *envp".to_string()];
        let mut nfs = Fs { tf: fs.tf, subst: fs.subst.clone(), out: String::new(), decls: String::new(), ind: 1, scopes: vec![], repr: HashMap::new(), flagged: fs.flagged.clone(), flags: HashMap::new(), ret: ret.clone(), yields: false, err_mode: None, loop_labels: vec![], lb: None, line: (u32::MAX, 0), tscopes: HashMap::new(), bce: vec![], bce_steady: HashMap::new(), bce_floor: Default::default(), dinfo: None, site: None, used_flags: HashSet::new(), jd_done: HashSet::new(), fd_done: HashSet::new(), static_cur: fs.static_cur.clone() };
        // (the floors are about the whole function, closures included)
        nfs.bce_floor = fs.bce_floor.clone();
        if let (Some(lid), Some(_)) = (c.loop_body, &lbp) {
            // `return` / `try` in the body act on the enclosing function
            nfs.ret = fs.ret.clone();
            nfs.lb = Some(LoopBody { id: lid, ctl: "(*env->kctl)".into(), ret: "(*env->kret)".into() });
        }
        nfs.scopes.push(Scope { kind: SK::Func, items: vec![], wait: None });
        for (i, p) in c.params.iter().enumerate() {
            let (m, pt) = fty.params[i].clone();
            let pc = self.param_cty(m, &pt);
            let pn = format!("a{}_{}", p, sanitize(&fs.tf.locals[*p as usize].name));
            ps.push(format!("{} {}", pc, pn));
            let byptr = match m {
                Mode::Borrow => !self.is_copy(&pt),
                Mode::Inout => true,
                Mode::Sink => false,
            };
            nfs.repr.insert(*p, if byptr { Repr::Ptr(pn.clone()) } else { Repr::Val(pn.clone()) });
            if m == Mode::Sink {
                self.declare_flags(&mut nfs, *p);
                if self.needs_drop(&pt) {
                    nfs.scopes[0].items.push(Cl::DropLocal(*p));
                }
            }
        }
        if !fields.is_empty() || lbp.is_some() {
            self.emit(&mut nfs, &format!("{} *env = envp;", envn));
        } else {
            self.emit(&mut nfs, "(void)envp;");
        }
        for (l, lt, _) in &fields {
            if byv(l) {
                nfs.repr.insert(*l, Repr::Val(format!("env->c{}", l)));
                // a task runs once and owns what was moved into it: those are
                // its locals, dropped (unless moved on) when it ends
                if c.scoped && c.by_value.contains(l) && self.needs_drop(lt) {
                    self.declare_flags(&mut nfs, *l);
                    nfs.scopes[0].items.push(Cl::DropLocal(*l));
                }
            } else {
                nfs.repr.insert(*l, Repr::Ptr(format!("env->c{}", l)));
            }
        }
        let sig = format!("static {} {}({})", rc, fname, ps.join(", "));
        let _ = writeln!(self.protos, "{};", sig);
        let body = c.body.clone();
        if nfs.lb.is_some() {
            // a body that already asked to leave must not run again
            let stop = self.flow(true);
            self.emit(&mut nfs, &format!("if (*env->kctl) return {};", stop));
            self.push_scope(&mut nfs, SK::Block);
            for st in &body.stmts {
                self.stmt(&mut nfs, st);
            }
            self.pop_scope(&mut nfs);
            self.cleanup_to(&mut nfs, 0);
            let go = self.flow(false);
            self.emit(&mut nfs, &format!("return {};", go));
        } else {
            // owned from the start: the parameters passed by value, and
            // what a task owns (as declared above)
            let mut owned: Vec<LocalId> = c.params.iter().enumerate().filter(|(i, _)| fty.params[*i].0 == Mode::Sink).map(|(_, p)| *p).collect();
            for (l, lt, _) in &fields {
                if byv(l) && c.scoped && c.by_value.contains(l) && self.needs_drop(lt) {
                    owned.push(*l);
                }
            }
            self.analyze_drops(&mut nfs, &body, &owned);
            self.fn_body(&mut nfs, &body);
            if let Some(info) = &nfs.dinfo {
                let missing = info.join_drops.keys().filter(|k| !nfs.jd_done.contains(k)).count() + info.after_stmt.keys().filter(|k| !nfs.fd_done.contains(k)).count();
                assert!(missing == 0, "internal error: {} drops at the ends of branches or at last uses were not emitted in a closure", missing);
            }
        }
        let nout = reuse_boxes(&resolve_flags(&nfs.out, &nfs.used_flags));
        let _ = writeln!(self.bodies, "{} {{\n{}{}}}\n{}", sig, nfs.decls, nout, C_LINE_MARK);
        // environment value
        if let Some((ctlp, retp)) = &lbp {
            let mut inits = vec![];
            for (l, _, _) in &fields {
                let p = self.local_place(fs, *l);
                inits.push(format!("&{}", p));
            }
            inits.push(ctlp.clone());
            inits.push(retp.clone());
            let e = self.tmp();
            self.emit(fs, &format!("{} {} = {{ {} }};", envn, e, inits.join(", ")));
            return format!("((gojo_func){{ (void *){}, &{}, NULL }})", fname, e);
        }
        if fields.is_empty() {
            return format!("((gojo_func){{ (void *){}, NULL, NULL }})", fname);
        }
        if by_value {
            let e = self.tmp();
            self.emit(fs, &format!("{} *{} = gojo_alloc(sizeof({}));", envn, e, envn));
            for (l, lt, _) in &fields {
                let p = self.local_place(fs, *l);
                if !self.is_copy(lt) {
                    if let Some(f) = fs.flags.get(&(*l, vec![])).cloned() {
                        self.set_flag(fs, &f, false);
                    }
                }
                self.emit(fs, &format!("{}->c{} = {};", e, l, p));
            }
            // drop function for the environment
            let dn = format!("drop_{}", envn);
            let mut db = String::new();
            let _ = writeln!(db, "static void {}(void *p) {{\n    {} *env = p;", dn, envn);
            for (l, lt, _) in &fields {
                if self.needs_drop(lt) {
                    let d = self.drop_fn(lt);
                    let _ = writeln!(db, "    {}(&env->c{});", d, l);
                }
            }
            let _ = writeln!(db, "    free(env);\n}}");
            let _ = writeln!(self.protos, "static void {}(void *p);", dn);
            self.bodies.push_str(&db);
            format!("((gojo_func){{ (void *){}, {}, {} }})", fname, e, dn)
        } else {
            let mut inits = vec![];
            for (l, _, _) in &fields {
                let p = self.local_place(fs, *l);
                if c.by_value.contains(l) {
                    inits.push(p);
                } else {
                    inits.push(format!("&{}", p));
                }
            }
            if c.scoped {
                // tasks outlive the statement: the environment lives on the heap,
                // and owns the values moved into the task
                // (the task drops what it owns itself)
                let drops = String::new();
                for (l, lt, _) in &fields {
                    if c.by_value.contains(l) && !self.is_copy(lt) {
                        if let Some(f) = fs.flags.get(&(*l, vec![])).cloned() {
                            self.set_flag(fs, &f, false);
                        }
                    }
                }
                let e = self.tmp();
                self.emit(fs, &format!("{} *{} = gojo_alloc(sizeof({})); *{} = ({}){{ {} }};", envn, e, envn, e, envn, inits.join(", ")));
                if drops.is_empty() {
                    return format!("((gojo_func){{ (void *){}, {}, free }})", fname, e);
                }
                let dn = format!("drop_{}", envn);
                let _ = writeln!(self.protos, "static void {}(void *p);", dn);
                let _ = writeln!(self.bodies, "static void {}(void *p) {{\n    {} *env = p;\n{}    free(env);\n}}", dn, envn, drops);
                return format!("((gojo_func){{ (void *){}, {}, {} }})", fname, e, dn);
            }
            let e = self.tmp();
            self.emit(fs, &format!("{} {} = {{ {} }};", envn, e, inits.join(", ")));
            format!("((gojo_func){{ (void *){}, &{}, NULL }})", fname, e)
        }
    }

    // ------------------------------------------------------------------
    // match
    // ------------------------------------------------------------------

    /// `key`: the address of the `match` (join drops are keyed by it).
    fn match_expr(&mut self, fs: &mut Fs, scrut: &TExpr, owned: bool, arms: &[TArm], t: &Ty, sp: Span, key: usize) -> String {
        let st = self.sub(fs, &scrut.ty);
        let optptr = matches!(scrut.kind, TK::Proj { opt: true, .. });
        let has_val = !matches!(t, Ty::Unit | Ty::Never);
        let res = if has_val {
            let c = self.cty(t);
            let r = self.tmp();
            self.emit(fs, &format!("{} {};", c, r));
            Some(r)
        } else {
            None
        };
        // scrutinee place
        let mut scrut_flag: Option<String> = None;
        let mut scrut_path: Option<(LocalId, Vec<usize>)> = None;
        let place = if optptr {
            let p = self.call_expr(fs, scrut);
            let inner = match &st {
                Ty::Option(i) => (**i).clone(),
                _ => st.clone(),
            };
            let ic = self.cty(&inner);
            let r = self.tmp();
            self.emit(fs, &format!("{} *{} = {};", ic, r, p));
            r
        } else if scrut.is_place() {
            if owned {
                scrut_path = local_path(scrut);
            }
            let p = self.place(fs, scrut);
            if is_simple_c(&p) || p.starts_with("(*") {
                p
            } else {
                let c = self.cty(&st);
                let r = self.tmp();
                self.emit(fs, &format!("{} *{} = &{};", c, r, p));
                format!("(*{})", r)
            }
        } else {
            let v = self.expr(fs, scrut);
            let c = self.cty(&st);
            let r = self.tmp();
            self.emit(fs, &format!("{} {} = {};", c, r, v));
            if self.needs_drop(&st) {
                let f = self.tmp();
                self.emit(fs, &format!("bool {} = 1;", f));
                scrut_flag = Some(f.clone());
                self.register_stmt(fs, Cl::DropFlag(r.clone(), st.clone(), f));
            }
            r
        };
        let end = self.label();
        for (ai, arm) in arms.iter().enumerate() {
            let cond = self.pat_cond(&arm.pat, &place, &st, optptr);
            self.emit(fs, &format!("if ({}) {{", cond));
            fs.ind += 1;
            self.push_scope(fs, SK::Block);
            // bindings
            self.bind_pat(fs, &arm.pat, &place, &st, optptr, arm.consuming);
            let mut guard_open = false;
            if let Some(g) = &arm.guard {
                self.push_scope(fs, SK::Stmt);
                let gv = self.expr(fs, g);
                let gt = self.tmp();
                self.emit(fs, &format!("bool {} = {};", gt, gv));
                self.pop_scope(fs);
                self.emit(fs, &format!("if ({}) {{", gt));
                fs.ind += 1;
                guard_open = true;
            }
            if arm.consuming {
                if let Some(f) = &scrut_flag {
                    self.emit(fs, &format!("{} = 0;", f));
                }
                if let Some((l, path)) = &scrut_path {
                    if let Some(f) = fs.flags.get(&(*l, path.clone())).cloned() {
                        self.set_flag(fs, &f, false);
                    }
                }
                self.residual_drop(fs, &arm.pat, &place, &st);
                for &b in &arm.binds {
                    let bt = self.local_ty(fs, b);
                    if self.needs_drop(&bt) {
                        self.declare_flags(fs, b);
                        self.register(fs, Cl::DropLocal(b));
                    }
                }
            }
            let bt = self.sub(fs, &arm.body.ty);
            self.push_value_scope(fs, &bt);
            let bv = self.expr(fs, &arm.body);
            match &res {
                Some(r) if arm.body.ty != Ty::Never => self.emit(fs, &format!("{} = {};", r, bv)),
                _ => self.discard(fs, &bv, &bt, arm.body.is_place()),
            }
            let later = self.pop_scope_keeping_later(fs);
            self.at_site(fs, drops::Site::arm(arm), |g, fs| g.pop_scope(fs));
            fs.scopes.last_mut().unwrap().items.extend(later);
            let jd = self.join_drops(fs, key, ai);
            self.emit_join_drops(fs, &jd);
            self.emit(fs, &format!("goto {};", end));
            if guard_open {
                fs.ind -= 1;
                self.emit(fs, "}");
            }
            fs.ind -= 1;
            self.emit(fs, "}");
        }
        let l = self.loc(sp);
        self.emit(fs, &format!("gojo_panic(\"no match arm matched\", {});", l));
        self.emit(fs, &format!("{}:;", end));
        res.unwrap_or_else(|| "0".into())
    }

    fn sub_places(&mut self, pat: &TPat, place: &str, ty: &Ty, optptr: bool) -> Vec<(String, Ty)> {
        match pat {
            TPat::Variant { def, variant, subs } => {
                let args = match ty {
                    Ty::Adt(_, a) => a.to_vec(),
                    _ => vec![],
                };
                let fsx = self.prog.variant_tys(*def, &args, *variant);
                subs.iter().enumerate().map(|(i, _)| (format!("{}.u.v{}.f_{}", place, variant, fsx[i].0), fsx[i].1.clone())).collect()
            }
            TPat::Tuple(subs) => {
                let args = match ty {
                    Ty::Adt(_, a) => a.to_vec(),
                    _ => vec![],
                };
                let d = match ty {
                    Ty::Adt(d, _) => *d,
                    _ => unreachable!(),
                };
                let fsx = self.prog.field_tys(d, &args);
                subs.iter().enumerate().map(|(i, _)| (format!("{}.f_{}", place, fsx[i].0), fsx[i].1.clone())).collect()
            }
            TPat::Some(_) => {
                let inner = match ty {
                    Ty::Option(i) => (**i).clone(),
                    _ => ty.clone(),
                };
                if optptr {
                    vec![(format!("(*{})", place), inner)]
                } else {
                    vec![(self.oval(ty, place), inner)]
                }
            }
            TPat::Ok(_) => match ty {
                Ty::Result(a, _) => vec![(format!("{}.u.val", place), (**a).clone())],
                _ => vec![],
            },
            TPat::Err(_) => match ty {
                Ty::Result(_, b) => vec![(format!("{}.u.err", place), (**b).clone())],
                _ => vec![],
            },
            _ => vec![],
        }
    }

    fn pat_cond(&mut self, pat: &TPat, place: &str, ty: &Ty, optptr: bool) -> String {
        match pat {
            TPat::Wild | TPat::Bind(_) => "1".into(),
            TPat::Int(v) => format!("({} == {})", place, int_lit(*v, ty)),
            TPat::Range(lo, hi, inc) => format!("({} >= {} && {} {} {})", place, int_lit(*lo, ty), place, if *inc { "<=" } else { "<" }, int_lit(*hi, ty)),
            TPat::Bool(b) => if *b { format!("({})", place) } else { format!("(!{})", place) },
            TPat::Str(s) => {
                let v = if *ty == Ty::String { format!("gojo_string_view(&{})", place) } else { place.to_string() };
                format!("gojo_str_eq({}, gojo_str_lit({}, {}))", v, c_string(s), s.len())
            }
            TPat::None => if optptr { format!("({} == NULL)", place) } else { format!("(!{})", self.osome(ty, place)) },
            TPat::Some(sub) | TPat::Ok(sub) | TPat::Err(sub) => {
                let head = match pat {
                    TPat::Some(_) => if optptr { format!("({} != NULL)", place) } else { self.osome(ty, place) },
                    TPat::Ok(_) => format!("{}.ok", place),
                    _ => format!("(!{}.ok)", place),
                };
                let sp = self.sub_places(pat, place, ty, optptr);
                let inner = self.pat_cond(sub, &sp[0].0, &sp[0].1.clone(), false);
                if inner == "1" {
                    head
                } else {
                    format!("({} && {})", head, inner)
                }
            }
            TPat::Tuple(subs) => {
                let mut parts = vec![];
                let sp = self.sub_places(pat, place, ty, false);
                for (s, (p, t)) in subs.iter().zip(sp.iter()) {
                    let c = self.pat_cond(s, p, &t.clone(), false);
                    if c != "1" {
                        parts.push(c);
                    }
                }
                if parts.is_empty() {
                    "1".into()
                } else {
                    format!("({})", parts.join(" && "))
                }
            }
            TPat::Variant { variant, subs, .. } => {
                let mut parts = vec![format!("{}.tag == {}", place, variant)];
                let sp = self.sub_places(pat, place, ty, false);
                for (s, (p, t)) in subs.iter().zip(sp.iter()) {
                    let c = self.pat_cond(s, p, &t.clone(), false);
                    if c != "1" {
                        parts.push(c);
                    }
                }
                format!("({})", parts.join(" && "))
            }
            TPat::TypeIs { ty: ct, .. } => {
                let tid = self.type_id(ct);
                format!("(((const gojo_vt_base *){}.vt)->tid == {})", place, tid)
            }
            TPat::Or(alts) => {
                let cs: Vec<String> = alts.iter().map(|a| self.pat_cond(a, place, ty, optptr)).collect();
                format!("({})", cs.join(" || "))
            }
        }
    }

    fn bind_pat(&mut self, fs: &mut Fs, pat: &TPat, place: &str, ty: &Ty, optptr: bool, consuming: bool) {
        match pat {
            TPat::Bind(l) => {
                let lt = self.local_ty(fs, *l);
                let c = self.cty(&lt);
                let n = self.local_cname(fs, *l);
                if consuming {
                    self.emit(fs, &format!("{} {} = {};", c, n, place));
                    fs.repr.insert(*l, Repr::Val(n));
                } else {
                    self.emit(fs, &format!("{} *{} = &{};", c, n, place));
                    fs.repr.insert(*l, Repr::Ptr(n));
                }
            }
            TPat::TypeIs { ty: ct, bind: Some(l) } => {
                let c = self.cty(ct);
                let n = self.local_cname(fs, *l);
                self.emit(fs, &format!("{} *{} = ({} *){}.data;", c, n, c, place));
                fs.repr.insert(*l, Repr::Ptr(n));
            }
            TPat::Some(sub) | TPat::Ok(sub) | TPat::Err(sub) => {
                let sp = self.sub_places(pat, place, ty, optptr);
                self.bind_pat(fs, sub, &sp[0].0, &sp[0].1.clone(), false, consuming);
            }
            TPat::Variant { subs, .. } | TPat::Tuple(subs) => {
                let sp = self.sub_places(pat, place, ty, false);
                for (s, (p, t)) in subs.iter().zip(sp.iter()) {
                    self.bind_pat(fs, s, p, &t.clone(), false, consuming);
                }
            }
            _ => {}
        }
    }

    fn residual_drop(&mut self, fs: &mut Fs, pat: &TPat, place: &str, ty: &Ty) {
        match pat {
            TPat::Bind(_) => {}
            TPat::Some(sub) | TPat::Ok(sub) | TPat::Err(sub) => {
                let sp = self.sub_places(pat, place, ty, false);
                self.residual_drop(fs, sub, &sp[0].0, &sp[0].1.clone());
            }
            TPat::Variant { subs, .. } | TPat::Tuple(subs) => {
                let sp = self.sub_places(pat, place, ty, false);
                for (s, (p, t)) in subs.iter().zip(sp.iter()) {
                    self.residual_drop(fs, s, p, &t.clone());
                }
            }
            _ => {
                if self.needs_drop(ty) {
                    let d = self.drop_fn(ty);
                    self.emit(fs, &format!("{}(&{});", d, place));
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // try
    // ------------------------------------------------------------------

    fn try_expr(&mut self, fs: &mut Fs, x: &TExpr, wrap: Option<&TryWrap>, t: &Ty, site: drops::Site) -> String {
        let xt = self.sub(fs, &x.ty);
        let v = self.expr(fs, x);
        let xc = self.cty(&xt);
        let tv = self.tmp();
        self.emit(fs, &format!("{} {} = {};", xc, tv, v));
        let ret = fs.ret.clone();
        let rc = self.cty(&ret);
        match &xt {
            Ty::Option(_) => {
                let cond = self.osome(&xt, &tv);
                self.emit(fs, &format!("if (!{}) {{", cond));
                fs.ind += 1;
                let none = format!("(({}){{0}})", rc);
                self.at_site(fs, site, |g, fs| g.emit_return(fs, &none));
                fs.ind -= 1;
                self.emit(fs, "}");
                if *t == Ty::Unit {
                    "0".into()
                } else {
                    self.oval(&xt, &tv)
                }
            }
            Ty::Result(_, e1) => {
                let e1 = (**e1).clone();
                self.emit(fs, &format!("if (!{}.ok) {{", tv));
                fs.ind += 1;
                if self.etrace {
                    let l = self.loc(x.span);
                    self.emit(fs, &format!("gojo_etrace_push({});", l));
                }
                let errv = match wrap {
                    None => format!("{}.u.err", tv),
                    Some(TryWrap::ToError) => {
                        let et = match &ret {
                            Ty::Result(_, e) => (**e).clone(),
                            _ => unreachable!(),
                        };
                        let vt = self.vtable(&e1, &et);
                        let ec = self.cty(&e1);
                        let b = self.tmp();
                        self.emit(fs, &format!("{} *{} = gojo_box_alloc(sizeof({})); *{} = {}.u.err;", ec, b, ec, b, tv));
                        format!("((gojo_iface){{ {}, &{} }})", b, vt)
                    }
                    Some(TryWrap::Call(hid, w)) => {
                        let ec = self.cty(&e1);
                        let n = self.local_cname(fs, *hid);
                        self.emit(fs, &format!("{} {} = {}.u.err;", ec, n, tv));
                        fs.repr.insert(*hid, Repr::Val(n));
                        self.push_scope(fs, SK::Stmt);
                        let wv = self.expr(fs, w);
                        let wt = self.sub(fs, &w.ty);
                        let r = self.materialize(fs, &wv, &wt, false);
                        fs.scopes.pop();
                        r
                    }
                };
                let r = self.tmp();
                self.emit(fs, &format!("{} {} = {{ .ok = 0, .u.err = {} }};", rc, r, errv));
                self.at_site(fs, site, |g, fs| g.emit_return(fs, &r));
                fs.ind -= 1;
                self.emit(fs, "}");
                if *t == Ty::Unit {
                    "0".into()
                } else {
                    format!("{}.u.val", tv)
                }
            }
            _ => v,
        }
    }

    // ------------------------------------------------------------------
    // go / select
    // ------------------------------------------------------------------

    /// `par for`: the body closure is called once per element, from up to
    /// one task per CPU, through a trampoline that finds the element.
    fn par_for(&mut self, fs: &mut Fs, kind: IterKind, iter: &TExpr, hi: Option<&TExpr>, mutable: bool, indexed: bool, body: &TExpr) {
        self.emit(fs, "{");
        fs.ind += 1;
        self.push_scope(fs, SK::Block);
        let ft = self.sub(fs, &body.ty);
        let pts: Vec<(Mode, Ty)> = match &ft {
            Ty::Func(f) => f.params.clone(),
            _ => vec![],
        };
        let f = self.expr(fs, body);
        self.ntmp += 1;
        let id = self.ntmp;
        let cn = format!("ParCtx{}", id);
        let tn = format!("parbody{}", id);
        let (et, data, n, lo) = if let IterKind::Range { inclusive } = kind {
            let it = self.sub(fs, &iter.ty);
            let ic = self.cty(&it);
            let l = self.expr(fs, iter);
            let h = self.expr(fs, hi.expect("range end"));
            let (lv, hv) = (self.tmp(), self.tmp());
            self.emit(fs, &format!("{} {} = {}; {} {} = {};", ic, lv, l, ic, hv, h));
            let n = if inclusive { format!("({} < {} ? 0 : (int64_t)({} - {}) + 1)", hv, lv, hv, lv) } else { format!("({} <= {} ? 0 : (int64_t)({} - {}))", hv, lv, hv, lv) };
            (it, "NULL".to_string(), n, lv)
        } else {
            let et = pts.last().map(|p| p.1.clone()).unwrap_or(Ty::Unit);
            let ec = self.cty(&et);
            let b = self.lval_scoped(fs, iter);
            let it = self.sub(fs, &iter.ty);
            let (d, n) = match (kind, &it) {
                (IterKind::Array, Ty::Array(_, len)) => (format!("(void *){}.a", b), len.to_string()),
                _ => (format!("(void *)({} *){}.ptr", ec, b), format!("{}.len", b)),
            };
            (et, d, n, "0".to_string())
        };
        let ec = self.cty(&et);
        let lc = if matches!(kind, IterKind::Range { .. }) { ec.clone() } else { "int64_t".to_string() };
        let _ = writeln!(self.tdefs, "typedef struct {{ gojo_func f; void *data; {} lo; }} {};", lc, cn);
        // arguments: (index,) element
        let mut sig = vec!["void *".to_string()];
        let mut args = vec!["c->f.env".to_string()];
        if matches!(kind, IterKind::Range { .. }) {
            sig.push(ec.clone());
            args.push(format!("({})(c->lo + ({})i)", ec, ec));
        } else {
            if indexed {
                sig.push("int64_t".into());
                args.push("i".into());
            }
            let byptr = mutable || !self.is_copy(&et);
            if byptr {
                sig.push(format!("{} *", ec));
                args.push(format!("&(({} *)c->data)[i]", ec));
            } else {
                sig.push(ec.clone());
                args.push(format!("(({} *)c->data)[i]", ec));
            }
        }
        let _ = writeln!(self.protos, "static void {}(void *p, int64_t i);", tn);
        let _ = writeln!(self.bodies, "static void {}(void *p, int64_t i) {{\n    {} *c = p;\n    ((void (*)({}))c->f.fn)({});\n}}", tn, cn, sig.join(", "), args.join(", "));
        let cv = self.tmp();
        self.emit(fs, &format!("{} {} = {{ {}, {}, {} }};", cn, cv, f, data, lo));
        self.emit(fs, &format!("gojo_par_for({}, {}, &{});", n, tn, cv));
        self.pop_scope(fs);
        fs.ind -= 1;
        self.emit(fs, "}");
    }

    /// A send in a cancelled task doesn't happen; the value is dropped instead.
    fn cancelled_send_drop(&mut self, t: &Ty, v: &str) -> String {
        if self.needs_drop(t) {
            let d = self.drop_fn(t);
            format!(" {}(&{}); ", d, v)
        } else {
            String::new()
        }
    }

    fn select(&mut self, fs: &mut Fs, arms: &[TSelectArm], sp: Span) {
        self.emit(fs, "{");
        fs.ind += 1;
        self.push_scope(fs, SK::Block);
        // evaluate guards, channels, values, tasks and deadlines once, in order
        let mut guards = vec![];
        let mut chans: Vec<Option<String>> = vec![];
        let mut sendvals = vec![];
        let mut tasks: Vec<Option<String>> = vec![];
        let mut timers: Vec<Option<String>> = vec![];
        for a in arms {
            let g = match &a.guard {
                Some(g) => {
                    let v = self.expr(fs, g);
                    let n = self.tmp();
                    self.emit(fs, &format!("bool {} = {};", n, v));
                    n
                }
                None => "1".to_string(),
            };
            guards.push(g);
            let (mut c, mut sv, mut tk, mut tm) = (None, None, None, None);
            match &a.kind {
                TSelectKind::Recv { chan, .. } => {
                    let x = self.expr(fs, chan);
                    let n = self.tmp();
                    self.emit(fs, &format!("gojo_chan *{} = {};", n, x));
                    c = Some(n);
                }
                TSelectKind::Send { chan, value } => {
                    let x = self.expr(fs, chan);
                    let n = self.tmp();
                    self.emit(fs, &format!("gojo_chan *{} = {};", n, x));
                    let vt = self.sub(fs, &value.ty);
                    let v = self.expr(fs, value);
                    let vc = self.cty(&vt);
                    let vn = self.tmp();
                    let fl = self.tmp();
                    self.emit(fs, &format!("{} {} = {}; bool {} = 1;", vc, vn, v, fl));
                    if self.needs_drop(&vt) {
                        self.register(fs, Cl::DropFlag(vn.clone(), vt.clone(), fl.clone()));
                    }
                    c = Some(n);
                    sv = Some((vn, fl));
                }
                TSelectKind::Task { task, .. } => {
                    let p = self.lval(fs, task);
                    tk = Some(p);
                }
                TSelectKind::Timer { at, relative } => {
                    let v = self.expr(fs, at);
                    let n = self.tmp();
                    if *relative {
                        self.emit(fs, &format!("int64_t {} = gojo_time_now() + ({} > 0 ? {} : 0);", n, v, v));
                    } else {
                        self.emit(fs, &format!("int64_t {} = {};", n, v));
                    }
                    tm = Some(n);
                }
                TSelectKind::Cancelled | TSelectKind::Default => {}
            }
            chans.push(c);
            sendvals.push(sv);
            tasks.push(tk);
            timers.push(tm);
        }
        let end = self.label();
        let l = self.loc(sp);
        let nch = chans.iter().filter(|c| c.is_some()).count();
        let ntk = tasks.iter().filter(|c| c.is_some()).count();
        // the arms taking part, filled in at run time (guards)
        let cases = self.tmp();
        let carm = self.tmp();
        let ncs = self.tmp();
        let tks = self.tmp();
        let tarm = self.tmp();
        let nts = self.tmp();
        let (dl, dlarm, wc, hd) = (self.tmp(), self.tmp(), self.tmp(), self.tmp());
        self.emit(fs, &format!("gojo_sel_case {}[{}]; int {}[{}]; int {} = 0;", cases, nch.max(1), carm, nch.max(1), ncs));
        self.emit(fs, &format!("gojo_task *{}[{}]; int {}[{}]; int {} = 0;", tks, ntk.max(1), tarm, ntk.max(1), nts));
        self.emit(fs, &format!("int64_t {} = -1; int {} = -1; bool {} = 0; bool {} = 0;", dl, dlarm, wc, hd));
        let mut slots = vec![None; arms.len()];
        let mut cancel_arm = -1i64;
        let mut default_arm = -1i64;
        for (i, a) in arms.iter().enumerate() {
            let g = guards[i].clone();
            match &a.kind {
                TSelectKind::Recv { chan, .. } => {
                    let ct = self.sub(fs, &chan.ty);
                    let et = match &ct {
                        Ty::Receiver(t) => (**t).clone(),
                        _ => unreachable!(),
                    };
                    let ot = Ty::opt(et.clone());
                    let oc = self.cty(&ot);
                    let r = self.tmp();
                    self.emit(fs, &format!("{} {} = {{0}};", oc, r));
                    let slot = self.oval(&ot, &r);
                    self.emit(fs, &format!("if ({}) {{ {}[{}] = (gojo_sel_case){{ .c = {}, .elem = &{}, .send = 0 }}; {}[{}++] = {}; }}", g, cases, ncs, chans[i].as_ref().unwrap(), slot, carm, ncs, i));
                    slots[i] = Some((r, ot));
                }
                TSelectKind::Send { .. } => {
                    let (vn, _) = sendvals[i].clone().unwrap();
                    self.emit(fs, &format!("if ({}) {{ {}[{}] = (gojo_sel_case){{ .c = {}, .elem = &{}, .send = 1 }}; {}[{}++] = {}; }}", g, cases, ncs, chans[i].as_ref().unwrap(), vn, carm, ncs, i));
                }
                TSelectKind::Task { .. } => {
                    self.emit(fs, &format!("if ({}) {{ {}[{}] = {}; {}[{}++] = {}; }}", g, tks, nts, tasks[i].as_ref().unwrap(), tarm, nts, i));
                }
                TSelectKind::Timer { .. } => {
                    let t = timers[i].clone().unwrap();
                    self.emit(fs, &format!("if ({} && ({} < 0 || {} < {})) {{ {} = {}; {} = {}; }}", g, dl, t, dl, dl, t, dlarm, i));
                }
                TSelectKind::Cancelled => {
                    cancel_arm = i as i64;
                    self.emit(fs, &format!("{} = {};", wc, g));
                }
                TSelectKind::Default => {
                    default_arm = i as i64;
                    self.emit(fs, &format!("{} = {};", hd, g));
                }
            }
        }
        let which = self.tmp();
        let arm = self.tmp();
        self.emit(fs, &format!("int {} = gojo_select({}, {}, {}, {}, {}, {}, {}, {});", which, cases, ncs, tks, nts, dl, wc, hd, l));
        self.emit(fs, &format!("int {} = {} >= 0 ? ({} < {} ? {}[{}] : {}[{} - {}]) : {} == -1 ? {} : {} == -3 ? {} : {} == -4 ? {} : -1;", arm, which, which, ncs, carm, which, tarm, which, ncs, which, default_arm, which, dlarm, which, cancel_arm));
        for (i, a) in arms.iter().enumerate() {
            self.emit(fs, &format!("if ({} == {}) {{", arm, i));
            fs.ind += 1;
            self.push_scope(fs, SK::Block);
            match &a.kind {
                TSelectKind::Recv { bind, .. } => {
                    let (r, ot) = slots[i].clone().unwrap();
                    if self.niche(&ot) {
                        self.emit(fs, &format!("if (!{}[{}].ok) {} = NULL;", cases, which, r));
                    } else {
                        self.emit(fs, &format!("{}.some = {}[{}].ok;", r, cases, which));
                    }
                    match bind {
                        Some(b) => {
                            let n = self.local_cname(fs, *b);
                            let oc = self.cty(&ot);
                            self.emit(fs, &format!("{} {} = {};", oc, n, r));
                            fs.repr.insert(*b, Repr::Val(n));
                            self.declare_flags(fs, *b);
                            if self.needs_drop(&ot) {
                                self.register(fs, Cl::DropLocal(*b));
                            }
                        }
                        None => {
                            if self.needs_drop(&ot) {
                                self.register(fs, Cl::DropTemp(r.clone(), ot.clone()));
                            }
                        }
                    }
                }
                TSelectKind::Send { .. } => {
                    let (_, fl) = sendvals[i].clone().unwrap();
                    self.emit(fs, &format!("{} = 0;", fl));
                }
                TSelectKind::Task { task, bind } => {
                    // the task is done: take its result, and the handle
                    let tv = tasks[i].clone().unwrap();
                    let tt = self.sub(fs, &task.ty);
                    let rt = match &tt {
                        Ty::Task(r) => (**r).clone(),
                        _ => Ty::Unit,
                    };
                    if let Some(p) = local_path(task) {
                        if let Some(f) = fs.flags.get(&p).cloned() {
                            self.set_flag(fs, &f, false);
                        }
                    }
                    let has = !matches!(rt, Ty::Unit | Ty::Never);
                    match bind {
                        Some(b) if has => {
                            let rc = self.cty(&rt);
                            let n = self.local_cname(fs, *b);
                            self.emit(fs, &format!("{} {} = *({} *)gojo_task_result({});", rc, n, rc, tv));
                            fs.repr.insert(*b, Repr::Val(n));
                            self.declare_flags(fs, *b);
                            if self.needs_drop(&rt) {
                                self.register(fs, Cl::DropLocal(*b));
                            }
                        }
                        Some(b) => {
                            let n = self.local_cname(fs, *b);
                            self.emit(fs, &format!("gojo_unit {} = 0; (void){};", n, n));
                            fs.repr.insert(*b, Repr::Val(n));
                        }
                        None if has && self.needs_drop(&rt) => {
                            let d = self.drop_fn(&rt);
                            let rc = self.cty(&rt);
                            self.emit(fs, &format!("{}(({} *)gojo_task_result({}));", d, rc, tv));
                        }
                        None => {}
                    }
                    self.emit(fs, &format!("gojo_task_free({});", tv));
                }
                TSelectKind::Timer { .. } | TSelectKind::Cancelled | TSelectKind::Default => {}
            }
            self.block_stmts(fs, &a.body);
            self.pop_scope(fs);
            self.emit(fs, &format!("goto {};", end));
            fs.ind -= 1;
            self.emit(fs, "}");
        }
        self.emit(fs, &format!("{}:;", end));
        self.pop_scope(fs);
        fs.ind -= 1;
        self.emit(fs, "}");
    }

    // ------------------------------------------------------------------
    // Maps
    // ------------------------------------------------------------------

    fn map_insert(&mut self, fs: &mut Fs, mptr: &str, k: &Ty, v: &Ty, kv: &str, vv: &str) -> String {
        let (ent, desc) = self.map_ent(k, v);
        let kc = self.cty(k);
        let vc = self.cty(v);
        let ot = Ty::opt(v.clone());
        let oc = self.cty(&ot);
        let tk = self.tmp();
        let tvv = self.tmp();
        let ex = self.tmp();
        let e = self.tmp();
        let old = self.tmp();
        self.emit(fs, &format!("{} {} = {}; {} {} = {}; bool {}; {} {} = {{0}};", kc, tk, kv, vc, tvv, vv, ex, oc, old));
        self.emit(fs, &format!("{} *{} = ms_{}({}, &{}, &{});", ent, e, desc, mptr, tk, ex));
        let dk = if self.needs_drop(k) {
            let d = self.drop_fn(k);
            format!(" {}(&{});", d, tk)
        } else {
            String::new()
        };
        let set = self.oset(&ot, &old, &format!("{}->v", e));
        self.emit(fs, &format!("if ({}) {{ {}{} }} else {{ {}->k = {}; }}", ex, set, dk, e, tk));
        self.emit(fs, &format!("{}->v = {};", e, tvv));
        old
    }

    // ------------------------------------------------------------------
    // Intrinsics
    // ------------------------------------------------------------------

    fn arg_vals(&mut self, fs: &mut Fs, args: &[TArg]) -> Vec<String> {
        let tys: Vec<Ty> = args.iter().map(|a| self.sub(fs, &a.e.ty)).collect();
        let modes: Vec<Mode> = args.iter().map(|a| match a.mode {
            ArgMode::Borrow => Mode::Borrow,
            ArgMode::Inout => Mode::Inout,
            ArgMode::Value => Mode::Sink,
        }).collect();
        self.args(fs, args, &modes, &tys)
    }

    /// A bounds check at `sp`, removed (`proven`) or kept: for `gojo claims`,
    /// and an error in a `checkfree fn`.
    fn note_check(&mut self, fs: &Fs, sp: Span, proven: bool) {
        let f = fs.tf.func;
        let e = self.claims.entry(f).or_default();
        let list = if proven { &mut e.removed } else { &mut e.kept };
        if !list.contains(&sp) {
            list.push(sp);
        }
        if !proven && self.prog.func(f).checkfree && !self.checkfree_errs.iter().any(|(s, _)| *s == sp) {
            let file = self.sm.file(sp.file);
            let (l, c) = file.line_col(sp.lo);
            self.checkfree_errs.push((sp, format!("{}:{}:{}: `{}` is `checkfree`, but this bounds check remains: the compiler can't prove the index is in range here (state it with a guard, an `assert` or the function's `requires`, or loop over the indexes of what is indexed)", file.path, l, c, self.prog.func(f).name)));
        }
    }

    /// Keeps an owned value in a temporary that is dropped if the code leaves
    /// before it is used (a later `try` returning an error): the temporary,
    /// and the flag to clear once the value has been taken.
    fn hold(&mut self, fs: &mut Fs, v: &str, t: &Ty) -> (String, Option<String>) {
        if !self.needs_drop(t) {
            return (v.to_string(), None);
        }
        let c = self.cty(t);
        let n = self.tmp();
        let flag = format!("{}_held", n);
        self.emit(fs, &format!("{} {} = {}; bool {} = 1;", c, n, v, flag));
        self.register(fs, Cl::DropFlag(n.clone(), t.clone(), flag.clone()));
        (n, Some(flag))
    }

    /// Whether a `static if` condition holds for this instance; a type
    /// pattern's `_`s are bound in `binds`.
    fn static_cond(&mut self, fs: &mut Fs, name: &str, targs: &[Ty], binds: &mut Vec<(TParamId, Ty)>) -> bool {
        let t = self.sub(fs, &targs[0]);
        if let Some(k) = name.strip_prefix("static_if:kind:") {
            return match &t {
                // (a newtype is its own kind of type, not a struct)
                Ty::Adt(d, _) => if k == "struct" { self.prog.def(*d).is_struct() && !self.prog.def(*d).newtype } else { matches!(self.prog.def(*d).kind, TypeDefKind::Enum { .. }) },
                _ => false,
            };
        }
        if let Some(d) = name.strip_prefix("static_if:iface:") {
            let d: DefId = d.parse().unwrap_or(0);
            let TypeDefKind::Iface { type_set: Some(ts), .. } = &self.prog.def(d).kind else { return false };
            return ts.iter().any(|(approx, et)| {
                *et == t || (*approx && matches!(&t, Ty::Adt(td, _) if self.prog.def(*td).newtype && self.prog.def(*td).fields().first().map(|f| &f.ty) == Some(et)))
            });
        }
        // `static if let v: P = x`
        let p = self.sub(fs, &targs[1]);
        fn bind(p: &Ty, t: &Ty, fs: &Fs, binds: &mut Vec<(TParamId, Ty)>) -> bool {
            match (p, t) {
                (Ty::Param(q), _) if !fs.subst.contains_key(q) => match binds.iter().find(|b| b.0 == *q) {
                    Some((_, b)) => b == t,
                    None => {
                        binds.push((*q, t.clone()));
                        true
                    }
                },
                (Ty::Vec(a), Ty::Vec(b)) | (Ty::Slice(a), Ty::Slice(b)) | (Ty::Option(a), Ty::Option(b)) | (Ty::Box(a), Ty::Box(b)) | (Ty::Rc(a), Ty::Rc(b)) | (Ty::Arc(a), Ty::Arc(b)) | (Ty::Cell(a), Ty::Cell(b)) | (Ty::Ptr(a), Ty::Ptr(b)) | (Ty::Weak(a), Ty::Weak(b)) | (Ty::Mutex(a), Ty::Mutex(b)) | (Ty::MutexRef(a), Ty::MutexRef(b)) | (Ty::Sender(a), Ty::Sender(b)) | (Ty::Receiver(a), Ty::Receiver(b)) | (Ty::Task(a), Ty::Task(b)) | (Ty::Iso(a), Ty::Iso(b)) => bind(a, b, fs, binds),
                // (a function matches whether or not it is a `send fn`)
                (Ty::Func(f), Ty::Func(g)) => f.params.len() == g.params.len() && f.params.iter().zip(g.params.iter()).all(|((m1, x), (m2, y))| m1 == m2 && bind(x, y, fs, binds)) && bind(&f.ret, &g.ret, fs, binds),
                (Ty::Array(a, n), Ty::Array(b, m)) => n == m && bind(a, b, fs, binds),
                (Ty::Map(a, b), Ty::Map(c, d)) | (Ty::Result(a, b), Ty::Result(c, d)) => bind(a, c, fs, binds) && bind(b, d, fs, binds),
                (Ty::Adt(d1, a1), Ty::Adt(d2, a2)) => d1 == d2 && a1.len() == a2.len() && a1.iter().zip(a2.iter()).all(|(x, y)| bind(x, y, fs, binds)),
                _ => p == t,
            }
        }
        bind(&p, &t, fs, binds)
    }

    fn magic(&mut self, fs: &mut Fs, name: &str, _targs: &[Ty], args: &[TArg], _t: &Ty, _sp: Span) -> String {
        match name {
            // `static T { f => value }`: one value per field of this instance's `T`
            "static_build" => {
                let t = self.sub(fs, &_targs[0]);
                let Ty::Param(fp) = &_targs[1] else { unreachable!() };
                let fp = *fp;
                let here = self.prog.func(fs.tf.func).pkg;
                let fields = match &t {
                    Ty::Adt(d, a) if self.prog.def(*d).is_struct() && !self.prog.def(*d).newtype && !self.prog.def(*d).view && (self.prog.def(*d).pkg == here || self.prog.def(*d).tuple || self.prog.def(*d).fields().iter().all(|f| f.declared_pub)) => self.prog.field_tys(*d, a),
                    _ => {
                        if self.static_err.is_none() {
                            let l = self.sm.file(_sp.file).line_col(_sp.lo);
                            let why = match &t {
                                Ty::Adt(d, _) if self.prog.def(*d).view => "is a view struct (its views would come from nowhere the borrow checker sees)",
                                Ty::Adt(d, _) if self.prog.def(*d).is_struct() && !self.prog.def(*d).newtype => "has fields private to its package",
                                _ => "isn't a struct: guard it with `static if T: struct`",
                            };
                            self.static_err = Some(format!("{}:{}:{}: `static {} {{ }}` (in `{}`) can't build it: `{}` {}", self.sm.file(_sp.file).path, l.0, l.1, self.prog.show(&t), self.prog.func(fs.tf.func).name, self.prog.show(&t), why));
                        }
                        return self.zero(&t);
                    }
                };
                let saved = (fs.subst.get(&fp).cloned(), fs.static_cur.get(&fp).cloned());
                let mut parts = vec![];
                let mut flags = vec![];
                for (fname, fty) in &fields {
                    fs.subst.insert(fp, fty.clone());
                    fs.static_cur.insert(fp, fname.clone());
                    let v = self.expr(fs, &args[0].e);
                    // held until the struct is built: a later field's `try`
                    // may leave first
                    let (v, flag) = self.hold(fs, &v, fty);
                    flags.extend(flag);
                    parts.push(format!(".f_{} = {}", fname, v));
                }
                match saved.0 {
                    Some(x) => fs.subst.insert(fp, x),
                    None => fs.subst.remove(&fp),
                };
                match saved.1 {
                    Some(n) => fs.static_cur.insert(fp, n),
                    None => fs.static_cur.remove(&fp),
                };
                for f in flags {
                    self.emit(fs, &format!("{} = 0;", f));
                }
                let c = self.cty(&t);
                if parts.is_empty() {
                    return format!("(({}){{0}})", c);
                }
                let n = self.tmp();
                self.emit(fs, &format!("{} {} = {{ {} }};", c, n, parts.join(", ")));
                n
            }
            // `T ! never`: always ok
            "never_ok" => {
                let r = self.expr(fs, &args[0].e);
                format!("({}).u.val", r)
            }
            "len" => {
                let at = self.sub(fs, &args[0].e.ty);
                match &at {
                    Ty::Array(_, n) => n.to_string(),
                    _ => {
                        let p = self.lval(fs, &args[0].e);
                        format!("((int64_t){}.len)", p)
                    }
                }
            }
            "clone" => {
                let at = self.sub(fs, &args[0].e.ty);
                if self.is_copy(&at) {
                    return self.expr(fs, &args[0].e);
                }
                let p = self.addr(fs, &args[0].e);
                let f = self.clone_fn(&at);
                format!("{}({})", f, p)
            }
            "str_concat" => {
                let a = self.expr(fs, &args[0].e);
                let a = self.materialize(fs, &a, &Ty::Str, false);
                let b = self.expr(fs, &args[1].e);
                let r = self.tmp();
                self.emit(fs, &format!("gojo_string {} = gojo_str_concat({}, {});", r, a, b));
                r
            }
            "str_to_string" => {
                let a = self.expr(fs, &args[0].e);
                format!("gojo_string_from({})", a)
            }
            "rune_to_string" => {
                let a = self.expr(fs, &args[0].e);
                format!("gojo_rune_to_string({})", a)
            }
            "bytes_to_string" => {
                let a = self.expr(fs, &args[0].e);
                let s = self.tmp();
                self.emit(fs, &format!("gojo_slice {} = {};", s, a));
                format!("gojo_string_from((gojo_str){{ (const uint8_t *){}.ptr, {}.len }})", s, s)
            }
            "fmt.print" | "fmt.println" | "fmt.eprint" | "fmt.eprintln" | "fmt.sprint" | "fmt.sprintln" => {
                let b = self.tmp();
                self.emit(fs, &format!("gojo_string {} = {{0}};", b));
                for (i, a) in args.iter().enumerate() {
                    let at = self.sub(fs, &a.e.ty);
                    if i > 0 {
                        self.emit(fs, &format!("gojo_string_push_byte(&{}, ' ');", b));
                    }
                    let p = self.addr(fs, &a.e);
                    let f = self.fmt_fn(&at);
                    self.emit(fs, &format!("{}(&{}, {}, 0);", f, b, p));
                }
                if name.ends_with("ln") {
                    self.emit(fs, &format!("gojo_string_push_byte(&{}, '\\n');", b));
                }
                match name {
                    "fmt.sprint" | "fmt.sprintln" => b,
                    "fmt.eprint" | "fmt.eprintln" => {
                        self.emit(fs, &format!("gojo_write(2, &{});", b));
                        "0".into()
                    }
                    _ => {
                        self.emit(fs, &format!("gojo_write(1, &{});", b));
                        "0".into()
                    }
                }
            }
            "fmt.format" | "fmt.printf" => {
                let tmpl = match &args[0].e.kind {
                    TK::Str(t) => t.clone(),
                    _ => String::new(),
                };
                let pieces = crate::fmtspec::parse(&tmpl).unwrap_or_default();
                let b = self.tmp();
                self.emit(fs, &format!("gojo_string {} = {{0}};", b));
                // (dropped if an argument's `try` leaves before it is done)
                let held = if args[1..].iter().any(|a| may_leave(&a.e)) {
                    let flag = format!("{}_held", b);
                    self.emit(fs, &format!("bool {} = 1;", flag));
                    self.register(fs, Cl::DropFlag(b.clone(), Ty::String, flag.clone()));
                    Some(flag)
                } else {
                    None
                };
                let mut next = 1;
                for p in pieces {
                    match p {
                        crate::fmtspec::Piece::Lit(l) => {
                            self.emit(fs, &format!("gojo_string_push(&{}, gojo_str_lit({}, {}));", b, c_string(&l), l.len()));
                        }
                        crate::fmtspec::Piece::Arg(spec) => {
                            let a = &args[next].e;
                            next += 1;
                            let at = self.sub(fs, &a.ty);
                            let ptr = self.addr(fs, a);
                            let plain = spec.width == 0 && spec.radix.is_none() && spec.precision.is_none();
                            let target = if plain { b.clone() } else { self.tmp() };
                            if !plain {
                                self.emit(fs, &format!("gojo_string {} = {{0}};", target));
                            }
                            match (&at, spec.radix, spec.precision) {
                                (Ty::Int(k), Some(r), _) => {
                                    let radix = match r { 'x' | 'X' => 16, 'o' => 8, _ => 2 };
                                    self.emit(fs, &format!("gojo_fmt_radix(&{}, (int64_t)*{}, {}, {}, {});", target, ptr, k.signed(), radix, r == 'X'));
                                }
                                (Ty::F32 | Ty::F64, _, Some(pr)) => {
                                    self.emit(fs, &format!("gojo_fmt_fixed(&{}, (double)*{}, {});", target, ptr, pr));
                                }
                                _ => {
                                    let f = self.fmt_fn(&at);
                                    self.emit(fs, &format!("{}(&{}, {}, 0);", f, target, ptr));
                                }
                            }
                            if !plain {
                                let align = spec.align.map(|c| c as u32).unwrap_or(0);
                                let numeric = matches!(at, Ty::Int(_) | Ty::F32 | Ty::F64);
                                self.emit(fs, &format!("gojo_fmt_pad(&{}, &{}, {}, {}, {}, {}, {});", b, target, spec.width, align, spec.fill as u32, spec.zero, numeric));
                            }
                        }
                    }
                }
                if let Some(f) = held {
                    self.emit(fs, &format!("{} = 0;", f));
                }
                if name == "fmt.printf" {
                    self.emit(fs, &format!("gojo_write(1, &{});", b));
                    "0".into()
                } else {
                    b
                }
            }
            _ => panic!("unknown intrinsic {}", name),
        }
    }

    fn intrinsic(&mut self, fs: &mut Fs, f: FuncId, targs: &[Ty], args: &[TArg], ret: &Ty, sp: Span) -> String {
        let fd = self.prog.func(f).clone();
        let pkg = self.prog.packages[fd.pkg as usize].name.clone();
        let key = fd.recv_key.map(|k| self.key_name(k));
        let qn = match &key {
            Some(k) => format!("{}.{}", k, fd.name),
            None => format!("{}.{}", pkg, fd.name),
        };
        let l = self.loc(sp);
        let s: HashMap<TParamId, Ty> = fd.tparams.iter().copied().zip(targs.iter().cloned()).collect();
        let recv_ty = fd.recv.as_ref().map(|r| r.ty.subst(&s));
        // element type helpers
        let elem = |t: &Option<Ty>| -> Ty {
            match t {
                Some(Ty::Vec(e)) | Some(Ty::Slice(e)) | Some(Ty::Option(e)) | Some(Ty::Rc(e)) | Some(Ty::Arc(e)) | Some(Ty::Weak(e)) | Some(Ty::Cell(e)) | Some(Ty::Mutex(e)) | Some(Ty::Sender(e)) | Some(Ty::Receiver(e)) => (**e).clone(),
                _ => Ty::Unit,
            }
        };
        if let Some(r) = self.num_intrinsic(fs, &qn, targs, args) {
            return r;
        }
        if self.prog.packages[fd.pkg as usize].is_std {
            if let Some(r) = self.json_intrinsic(fs, &qn, targs, args, sp) {
                return r;
            }
        }
        match qn.as_str() {
            "prelude.panic" => {
                let a = self.expr(fs, &args[0].e);
                self.emit(fs, &format!("gojo_panic_str({}, {});", a, l));
                "0".into()
            }
            "prelude.unreachable" => {
                self.emit(fs, &format!("gojo_panic(\"entered unreachable code\", {});", l));
                "0".into()
            }
            "prelude.assert" => {
                let a = self.expr(fs, &args[0].e);
                self.emit(fs, &format!("if (!({})) gojo_panic(\"assertion failed\", {});", a, l));
                "0".into()
            }
            "prelude.assertEq" => {
                let t = targs[0].clone();
                let a = self.addr(fs, &args[0].e);
                let b = self.addr(fs, &args[1].e);
                let eq = self.eq_fn(&t);
                let fm = self.fmt_fn(&t);
                let m = self.tmp();
                self.emit(fs, &format!("if (!{}({}, {})) {{ gojo_string {} = {{0}}; gojo_fmt_cstr(&{}, \"assertion failed: \"); {}(&{}, {}, 1); gojo_fmt_cstr(&{}, \" != \"); {}(&{}, {}, 1); gojo_panic_buf(&{}, {}); }}", eq, a, b, m, m, fm, m, a, m, fm, m, b, m, l));
                "0".into()
            }
            "prelude.__panicErr" => {
                let t = targs[0].clone();
                let msg = self.expr(fs, &args[0].e);
                let e = self.addr(fs, &args[1].e);
                let fm = self.fmt_fn(&t);
                let m = self.tmp();
                self.emit(fs, &format!("{{ gojo_string {} = {{0}}; gojo_string_push(&{}, {}); gojo_fmt_cstr(&{}, \": \"); {}(&{}, {}, 0); gojo_panic_buf(&{}, {}); }}", m, m, msg, m, fm, m, e, m, l));
                "0".into()
            }
            "prelude.swap" => {
                let t = targs[0].clone();
                let c = self.cty(&t);
                let a = self.addr(fs, &args[0].e);
                let b = self.addr(fs, &args[1].e);
                self.emit(fs, &format!("{{ {} *_a = {}, *_b = {}; {} _t = *_a; *_a = *_b; *_b = _t; }}", c, a, b, c));
                "0".into()
            }
            "prelude.replace" => {
                let t = targs[0].clone();
                let c = self.cty(&t);
                let a = self.addr(fs, &args[0].e);
                let v = self.expr(fs, &args[1].e);
                let r = self.tmp();
                self.emit(fs, &format!("{} {} = *{}; *{} = {};", c, r, a, a, v));
                r
            }
            "prelude.take" => {
                let t = targs[0].clone();
                let c = self.cty(&t);
                let a = self.addr(fs, &args[0].e);
                let d = self.default_value(fs, &t);
                let r = self.tmp();
                self.emit(fs, &format!("{} {} = *{}; *{} = {};", c, r, a, a, d));
                r
            }
            "str.len" => {
                let a = self.expr(fs, &args[0].e);
                format!("({}).len", a)
            }
            "String.asStr" => {
                let a = self.addr(fs, &args[0].e);
                format!("gojo_string_view({})", a)
            }
            "Slice.reverse" | "Slice.swap" | "Slice.sort" | "Slice.sortBy" => {
                let v = self.addr(fs, &args[0].e);
                let vp = self.tmp();
                self.emit(fs, &format!("gojo_slice *{} = {};", vp, v));
                self.inplace_op(fs, &qn[6..], &elem(&recv_ty), &vp, args, &l)
            }
            "Slice.len" => {
                let a = self.expr(fs, &args[0].e);
                format!("({}).len", a)
            }
            "Slice.ptr" | "str.ptr" => {
                let a = self.expr(fs, &args[0].e);
                let et = elem(&recv_ty);
                let c = if qn == "str.ptr" { "uint8_t".to_string() } else { self.cty(&et) };
                // (pins the goroutine whose stack it points into, if any)
                format!("(({} *)gojo_ptr_escape(({}).ptr))", c, a)
            }
            "str.bytes" => {
                let a = self.expr(fs, &args[0].e);
                let r = self.tmp();
                self.emit(fs, &format!("gojo_str {} = {};", r, a));
                format!("((gojo_slice){{ (void *){}.ptr, {}.len }})", r, r)
            }
            "Hasher.write" => {
                let t = targs.last().cloned().unwrap();
                let h = self.addr(fs, &args[0].e);
                let x = self.addr(fs, &args[1].e);
                let hf = self.hash_fn(&t);
                self.emit(fs, &format!("({})->f_state = gojo_hash_mix(({})->f_state, {}({}));", h, h, hf, x));
                "0".into()
            }
            q if q.starts_with("Vec.") => self.vec_intrinsic(fs, &q[4..], &elem(&recv_ty), args, ret, &l),
            q if q.starts_with("Map.") => {
                let (k, v) = match &recv_ty {
                    Some(Ty::Map(k, v)) => ((**k).clone(), (**v).clone()),
                    _ => unreachable!(),
                };
                self.map_intrinsic(fs, &q[4..], &k, &v, args, ret, &l)
            }
            "Rc.count" | "Arc.count" => {
                let a = self.expr(fs, &args[0].e);
                let e = elem(&recv_ty);
                self.ensure_rcbox(&e);
                if qn == "Arc.count" {
                    format!("__atomic_load_n(&({})->strong, __ATOMIC_SEQ_CST)", a)
                } else {
                    format!("({})->strong", a)
                }
            }
            "Rc.downgrade" => {
                let a = self.expr(fs, &args[0].e);
                let e = elem(&recv_ty);
                self.ensure_rcbox(&e);
                let r = self.tmp();
                let c = self.cty(ret);
                self.emit(fs, &format!("{} {} = {}; {}->weak++;", c, r, a, r));
                r
            }
            "Weak.upgrade" => {
                let a = self.expr(fs, &args[0].e);
                let e = elem(&recv_ty);
                self.ensure_rcbox(&e);
                let c = self.cty(ret);
                let r = self.tmp();
                let set = self.oset(ret, &r, &a);
                self.emit(fs, &format!("{} {} = {{0}}; if (({})->strong > 0) {{ ({})->strong++; {} }}", c, r, a, a, set));
                r
            }
            "Cell.get" => {
                let a = self.lval(fs, &args[0].e);
                let c = self.cty(ret);
                let _ = c;
                self.materialize(fs, &a, ret, false)
            }
            "Cell.set" => {
                let v = self.expr(fs, &args[1].e);
                let a = self.lval(fs, &args[0].e);
                self.emit(fs, &format!("{} = {};", a, v));
                "0".into()
            }
            "Atomic.load" | "Atomic.store" | "Atomic.add" | "Atomic.sub" | "Atomic.swap" | "Atomic.compareSwap" => {
                // tasks share an `Atomic` by borrowing it: every access is atomic
                let a = self.lval(fs, &args[0].e);
                let vals: Vec<String> = args[1..].iter().map(|x| self.expr(fs, &x.e)).collect();
                self.emit(fs, "if (__builtin_expect(gojo_sim_on, 0)) gojo_sim_point();");
                let p = format!("&({}).f_v", a);
                match &qn[7..] {
                    "load" => format!("__atomic_load_n({}, __ATOMIC_SEQ_CST)", p),
                    "store" => {
                        self.emit(fs, &format!("__atomic_store_n({}, {}, __ATOMIC_SEQ_CST);", p, vals[0]));
                        "0".into()
                    }
                    "add" => format!("__atomic_add_fetch({}, {}, __ATOMIC_SEQ_CST)", p, vals[0]),
                    "sub" => format!("__atomic_sub_fetch({}, {}, __ATOMIC_SEQ_CST)", p, vals[0]),
                    "swap" => format!("__atomic_exchange_n({}, {}, __ATOMIC_SEQ_CST)", p, vals[0]),
                    _ => {
                        let et = match &recv_ty {
                            Some(Ty::Adt(_, a)) => a[0].clone(),
                            _ => Ty::int(),
                        };
                        let ec = self.cty(&et);
                        let o = self.tmp();
                        self.emit(fs, &format!("{} {} = {};", ec, o, vals[0]));
                        format!("__atomic_compare_exchange_n({}, &{}, {}, false, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)", p, o, vals[1])
                    }
                }
            }
            "task.with" => {
                // bind the key for the body (and the tasks it starts), run it, unbind
                let vt = fd.params[1].ty.subst(&s);
                let rt = ret.clone();
                let key = self.expr(fs, &args[0].e);
                let v = self.expr(fs, &args[1].e);
                let vc = self.cty(&vt);
                let vv = self.tmp();
                self.emit(fs, &format!("{} {} = {};", vc, vv, v));
                let b = self.tmp();
                self.emit(fs, &format!("uint8_t *{} = gojo_task_bind_enter(({}).f_id, &{});", b, key, vv));
                let f = self.addr(fs, &args[2].e);
                let fp = self.tmp();
                self.emit(fs, &format!("gojo_func *{} = {};", fp, f));
                let res = if matches!(rt, Ty::Unit | Ty::Never) {
                    self.emit(fs, &format!("((void (*)(void *)){}->fn)({}->env);", fp, fp));
                    "0".to_string()
                } else {
                    let rc = self.cty(&rt);
                    let r = self.tmp();
                    self.emit(fs, &format!("{} {} = (({} (*)(void *)){}->fn)({}->env);", rc, r, rc, fp, fp));
                    r
                };
                self.emit(fs, &format!("gojo_task_bind_exit({});", b));
                if self.needs_drop(&vt) {
                    let d = self.drop_fn(&vt);
                    self.emit(fs, &format!("{}(&{});", d, vv));
                }
                res
            }
            "Local.get" => {
                let et = match &recv_ty {
                    Some(Ty::Adt(_, a)) => a[0].clone(),
                    _ => Ty::Unit,
                };
                let key = self.expr(fs, &args[0].e);
                let ec = self.cty(&et);
                let p = self.tmp();
                self.emit(fs, &format!("{} *{} = gojo_task_lookup(({}).f_id);", ec, p, key));
                let copied = if self.is_copy(&et) {
                    format!("(*{})", p)
                } else {
                    let cf = self.clone_fn(&et);
                    format!("{}({})", cf, p)
                };
                let oc = self.cty(ret);
                let r = self.tmp();
                if self.niche(ret) {
                    self.emit(fs, &format!("{} {} = {{0}}; if ({}) {} = {};", oc, r, p, r, copied));
                } else {
                    self.emit(fs, &format!("{} {} = {{0}}; if ({}) {{ {}.some = 1; {}.val = {}; }}", oc, r, p, r, r, copied));
                }
                r
            }
            "Task.join" => {
                let r = match &recv_ty {
                    Some(Ty::Task(r)) => (**r).clone(),
                    _ => Ty::Unit,
                };
                let x = self.expr(fs, &args[0].e);
                let tv = self.tmp();
                self.emit(fs, &format!("gojo_task *{} = {}; gojo_task_join({});", tv, x, tv));
                if matches!(r, Ty::Unit | Ty::Never) {
                    self.emit(fs, &format!("gojo_task_free({});", tv));
                    return "0".into();
                }
                let rc = self.cty(&r);
                let v = self.tmp();
                self.emit(fs, &format!("{} {} = *({} *)gojo_task_result({}); gojo_task_free({});", rc, v, rc, tv, tv));
                v
            }
            "Iso.take" => self.expr(fs, &args[0].e),
            "Task.cancel" => {
                let tt = recv_ty.clone().unwrap_or(Ty::Unit);
                let x = self.expr(fs, &args[0].e);
                let d = self.drop_fn(&tt);
                let tv = self.tmp();
                self.emit(fs, &format!("gojo_task *{} = {}; {}(&{});", tv, x, d, tv));
                "0".into()
            }
            "Scope.cancel" => {
                let x = self.expr(fs, &args[0].e);
                self.emit(fs, &format!("gojo_group_cancel({});", x));
                "0".into()
            }
            "RwLock.read" | "RwLock.write" => {
                let w = qn.ends_with("write");
                let (m, v) = self.lock_target(fs, &args[0].e);
                self.emit(fs, &format!("gojo_rw_{}lock({}->f_lk);", if w { "w" } else { "r" }, m));
                self.register_stmt(fs, Cl::UnlockRw(format!("{}->f_lk", m), w));
                format!("({})", v)
            }
            "Mutex.lock" | "MutexRef.lock" => {
                let (m, v) = self.lock_target(fs, &args[0].e);
                self.emit(fs, &format!("gojo_mutex_lock({}->mu);", m));
                self.register_stmt(fs, Cl::Unlock(format!("{}->mu", m)));
                format!("({})", v)
            }
            "Sender.send" => {
                let e = elem(&recv_ty);
                let c = self.expr(fs, &args[0].e);
                let v = self.expr(fs, &args[1].e);
                let ec = self.cty(&e);
                let t = self.tmp();
                let ok = self.tmp();
                let d = self.cancelled_send_drop(&e, &t);
                self.emit(fs, &format!("{} {} = {}; bool {} = gojo_chan_send({}, &{}, {}); if (!{}) {{{}}}", ec, t, v, ok, c, t, l, ok, d));
                ok
            }
            "Receiver.recv" => {
                let c = self.expr(fs, &args[0].e);
                let oc = self.cty(ret);
                let r = self.tmp();
                if self.niche(ret) {
                    self.emit(fs, &format!("{} {} = {{0}}; if (!gojo_chan_recv({}, &{})) {} = NULL;", oc, r, c, r, r));
                } else {
                    self.emit(fs, &format!("{} {} = {{0}}; {}.some = gojo_chan_recv({}, &{}.val);", oc, r, r, c, r));
                }
                r
            }
            "Sender.len" | "Receiver.len" => {
                let c = self.expr(fs, &args[0].e);
                format!("gojo_chan_len({})", c)
            }
            "math.sqrt" | "math.pow" | "math.floor" | "math.ceil" | "math.round" | "math.sin" | "math.cos" | "math.tan" | "math.atan2" | "math.log" | "math.exp" | "math.trunc" | "math.cbrt" | "math.hypot" | "math.fmod" | "math.asin" | "math.acos" | "math.atan" | "math.sinh" | "math.cosh" | "math.tanh" | "math.log2" | "math.log10" | "math.log1p" | "math.expm1" => {
                let vs = self.arg_vals(fs, args);
                format!("{}({})", &qn[5..], vs.join(", "))
            }
            "math.abs" => {
                let vs = self.arg_vals(fs, args);
                format!("fabs({})", vs[0])
            }
            "math.isNaN" => {
                let vs = self.arg_vals(fs, args);
                format!("isnan({})", vs[0])
            }
            "math.isInf" => {
                let vs = self.arg_vals(fs, args);
                format!("(isinf({}) != 0)", vs[0])
            }
            "math.isFinite" => {
                let vs = self.arg_vals(fs, args);
                format!("(isfinite({}) != 0)", vs[0])
            }
            "math.inf" => "INFINITY".into(),
            "math.nan" => "NAN".into(),
            "os.exit" => {
                let vs = self.arg_vals(fs, args);
                self.emit(fs, &format!("gojo_os_exit({});", vs[0]));
                "0".into()
            }
            _ => {
                // runtime function with the standard ABI
                let cname = match &key {
                    Some(k) => format!("gojo_{}_{}", k, fd.name),
                    None => format!("gojo_{}_{}", pkg, fd.name),
                };
                let mut modes = vec![];
                let mut tys = vec![];
                if let Some(r) = &fd.recv {
                    modes.push(r.mode);
                    tys.push(r.ty.subst(&s));
                }
                for p in &fd.params {
                    modes.push(p.mode);
                    tys.push(p.ty.subst(&s));
                }
                let avs = self.args(fs, args, &modes, &tys);
                let call = format!("{}({})", cname, avs.join(", "));
                self.finish_call(fs, call, ret)
            }
        }
    }

    /// The bit-level and checked integer operations and the float functions
    /// behind the methods of `extend T: Integer` / `extend T: Float`, for the
    /// concrete type in `targs[0]`.
    fn num_intrinsic(&mut self, fs: &mut Fs, qn: &str, targs: &[Ty], args: &[TArg]) -> Option<String> {
        let name = qn.strip_prefix("prelude.__")?;
        let t = targs.first()?.clone();
        let c = self.cty(&t);
        if name == "toString" {
            let v = self.expr(fs, &args[0].e);
            let x = self.tmp();
            self.emit(fs, &format!("{} {} = {};", c, x, v));
            let f = self.fmt_fn(&t);
            let r = self.tmp();
            self.emit(fs, &format!("gojo_string {} = {{0}}; {}(&{}, &{}, 0);", r, f, r, x));
            return Some(r);
        }
        if let Some(op) = name.strip_prefix("f_") {
            let f32 = t == Ty::F32;
            let vs = self.arg_vals(fs, args);
            return Some(match op {
                "isnan" | "isinf" => format!("({}({}) != 0)", op, vs[0]),
                "parse" => {
                    let d = self.tmp();
                    let ok = self.tmp();
                    self.emit(fs, &format!("double {} = 0; bool {} = gojo_prelude___parseFloat({}, &{}); if ({}) *({}) = ({}){};", d, ok, vs[0], d, ok, vs[1], c, d));
                    ok
                }
                _ => format!("{}{}({})", op, if f32 { "f" } else { "" }, vs.join(", ")),
            });
        }
        let Ty::Int(k) = t else { return None };
        let bits = k.bits();
        let ut = format!("uint{}_t", bits);
        let (min, max) = if k.signed() { (format!("INT{}_MIN", bits), format!("INT{}_MAX", bits)) } else { ("0".to_string(), format!("UINT{}_MAX", bits)) };
        let r = match name {
            "countOnes" | "leadingZeros" | "trailingZeros" | "bitsOf" | "rotateLeft" | "swapBytes" | "minOf" | "maxOf" | "addOverflows" | "subOverflows" | "mulOverflows" | "parseRadix" | "formatRadix" => {
                let vs = self.arg_vals(fs, args);
                match name {
                    "countOnes" => format!("((int64_t)__builtin_popcountll((uint64_t)({})({})))", ut, vs[0]),
                    "leadingZeros" | "trailingZeros" => {
                        let u = self.tmp();
                        self.emit(fs, &format!("{} {} = ({})({});", ut, u, ut, vs[0]));
                        if name == "leadingZeros" {
                            format!("({} ? (int64_t)__builtin_clzll((uint64_t){}) - {} : {})", u, u, 64 - bits, bits)
                        } else {
                            format!("({} ? (int64_t)__builtin_ctzll((uint64_t){}) : {})", u, u, bits)
                        }
                    }
                    "bitsOf" => format!("((int64_t){})", bits),
                    "rotateLeft" => {
                        let u = self.tmp();
                        let n = self.tmp();
                        self.emit(fs, &format!("{} {} = ({})({}); int64_t {} = (({}) % {} + {}) % {};", ut, u, ut, vs[0], n, vs[1], bits, bits, bits));
                        format!("(({})({} ? ({})(({} << {}) | ({} >> ({} - {}))) : {}))", c, n, ut, u, n, u, bits, n, u)
                    }
                    "swapBytes" => match bits {
                        8 => vs[0].clone(),
                        _ => format!("(({})__builtin_bswap{}((uint{}_t)({})))", c, bits, bits, vs[0]),
                    },
                    "minOf" => format!("(({}){})", c, min),
                    "maxOf" => format!("(({}){})", c, max),
                    "addOverflows" => format!("__builtin_add_overflow({}, {}, {})", vs[0], vs[1], vs[2]),
                    "subOverflows" => format!("__builtin_sub_overflow({}, {}, {})", vs[0], vs[1], vs[2]),
                    "mulOverflows" => format!("__builtin_mul_overflow({}, {}, {})", vs[0], vs[1], vs[2]),
                    "parseRadix" => {
                        let u = self.tmp();
                        let ok = self.tmp();
                        self.emit(fs, &format!("uint64_t {} = 0; bool {} = gojo_parse_int({}, {}, {}, (uint64_t){}, &{}); if ({}) *({}) = ({}){};", u, ok, vs[0], vs[1], k.signed(), max, u, ok, vs[2], c, u));
                        ok
                    }
                    _ => {
                        let r = self.tmp();
                        self.emit(fs, &format!("gojo_string {} = {{0}}; gojo_fmt_radix(&{}, (int64_t)({}), {}, {}, false);", r, r, vs[0], k.signed(), vs[1]));
                        r
                    }
                }
            }
            _ => return None,
        };
        Some(r)
    }

    /// Pointer to the Mutex struct behind a (possibly arc-wrapped) receiver expression.
    /// The Mutex struct behind a lock projection's receiver, and a pointer to
    /// the guarded value.
    fn lock_target(&mut self, fs: &mut Fs, e: &TExpr) -> (String, String) {
        let recv = match &e.kind {
            TK::Proj { call, .. } => match &call.kind {
                TK::Call { args, .. } => args[0].e.clone(),
                _ => e.clone(),
            },
            _ => e.clone(),
        };
        let mt = self.sub(fs, &recv.ty);
        let c = self.cty(&mt);
        let p = self.lval(fs, &recv);
        let m = self.tmp();
        self.emit(fs, &format!("{} *{} = &{};", c, m, p));
        let v = if matches!(mt, Ty::MutexRef(_)) {
            format!("{}->ptr", m)
        } else if matches!(mt, Ty::Adt(..)) {
            format!("&{}->f_val", m)
        } else {
            format!("&{}->val", m)
        };
        (m, v)
    }

    /// `rw.read()` / `rw.write()`: Some(true) for write.
    fn rw_lock_kind(&self, e: &TExpr) -> Option<bool> {
        match crate::check::body::lock_kind(self.prog, e) {
            Some(w) => match &e.kind {
                TK::Proj { call, .. } => match &call.kind {
                    TK::Call { callee: Callee::Func(f, _), .. } if matches!(self.prog.func(*f).recv_key, Some(TyKey::Adt(_))) => Some(w),
                    _ => None,
                },
                _ => None,
            },
            None => None,
        }
    }


    /// `reverse`, `swap`, `sort` and `sortBy` on the elements behind `vp`, a
    /// `gojo_vec *` or `gojo_slice *` (both start with `ptr` and `len`).
    fn inplace_op(&mut self, fs: &mut Fs, name: &str, el: &Ty, vp: &str, args: &[TArg], l: &str) -> String {
        let ec = self.cty(el);
        let arr = format!("(({} *){}->ptr)", ec, vp);
        match name {
            "reverse" => {
                self.emit(fs, &format!("for (int64_t _i = 0, _j = {}->len - 1; _i < _j; _i++, _j--) {{ {} _t = {}[_i]; {}[_i] = {}[_j]; {}[_j] = _t; }}", vp, ec, arr, arr, arr, arr));
            }
            "swap" => {
                let i = self.expr(fs, &args[1].e);
                let j = self.expr(fs, &args[2].e);
                self.emit(fs, &format!("{{ int64_t _i = gojo_idx({}, {}->len, {}), _j = gojo_idx({}, {}->len, {}); {} _t = {}[_i]; {}[_i] = {}[_j]; {}[_j] = _t; }}", i, vp, l, j, vp, l, ec, arr, arr, arr, arr));
            }
            "sort" => {
                let f = self.sort_fn(el);
                self.emit(fs, &format!("{}(({} *){}->ptr, {}->len);", f, ec, vp, vp));
            }
            "sortBy" => {
                let f = self.addr(fs, &args[1].e);
                let w = format!("sortby_{}", self.mangle(el));
                if self.helpers.insert(w.clone()) {
                    let pc = self.param_cty(Mode::Borrow, el);
                    let (aa, bb) = if self.is_copy(el) { (format!("*({} *)a", ec), format!("*({} *)b", ec)) } else { (format!("({} *)a", ec), format!("({} *)b", ec)) };
                    let _ = writeln!(self.protos, "static int {}(const void *a, const void *b, void *c);", w);
                    let _ = writeln!(self.bodies, "static int {}(const void *a, const void *b, void *c) {{\n    gojo_func *f = c;\n    bool (*fn)(void *, {}, {}) = (bool (*)(void *, {}, {}))f->fn;\n    if (fn(f->env, {}, {})) return -1;\n    if (fn(f->env, {}, {})) return 1;\n    return 0;\n}}", w, pc, pc, pc, pc, aa, bb, bb, aa);
                }
                self.emit(fs, &format!("gojo_sort({}->ptr, {}->len, sizeof({}), {}, {});", vp, vp, ec, w, f));
            }
            _ => unreachable!(),
        }
        "0".into()
    }

    fn vec_intrinsic(&mut self, fs: &mut Fs, name: &str, el: &Ty, args: &[TArg], ret: &Ty, l: &str) -> String {
        if name == "finish" {
            // ends an empty vector of linear values: there is nothing to drop
            let x = self.expr(fs, &args[0].e);
            let t = self.tmp();
            self.emit(fs, &format!("gojo_vec {} = {}; if ({}.len) gojo_panic(\"finish() on a vector that still holds linear values\", {}); free({}.ptr);", t, x, t, l, t));
            return "0".into();
        }
        let ec = self.cty(el);
        let v = self.addr(fs, &args[0].e);
        let vp = self.tmp();
        self.emit(fs, &format!("gojo_vec *{} = {};", vp, v));
        let arr = format!("(({} *){}->ptr)", ec, vp);
        match name {
            "push" => {
                let x = self.expr(fs, &args[1].e);
                let t = self.tmp();
                self.emit(fs, &format!("{} {} = {}; gojo_vec_reserve({}, 1, sizeof({})); {}[{}->len++] = {};", ec, t, x, vp, ec, arr, vp, t));
                "0".into()
            }
            "pop" => {
                let oc = self.cty(ret);
                let r = self.tmp();
                let set = self.oset(ret, &r, &format!("{}[--{}->len]", arr, vp));
                self.emit(fs, &format!("{} {} = {{0}}; if ({}->len) {{ {} }}", oc, r, vp, set));
                r
            }
            "insert" => {
                let i = self.expr(fs, &args[1].e);
                let x = self.expr(fs, &args[2].e);
                let it = self.tmp();
                let t = self.tmp();
                self.emit(fs, &format!("int64_t {} = {}; {} {} = {};", it, i, ec, t, x));
                self.emit(fs, &format!("if ({} < 0 || {} > {}->len) gojo_panic_index({}, {}->len, {});", it, it, vp, it, vp, l));
                self.emit(fs, &format!("gojo_vec_reserve({}, 1, sizeof({})); memmove({} + {} + 1, {} + {}, (size_t)({}->len - {}) * sizeof({})); {}[{}] = {}; {}->len++;", vp, ec, arr, it, arr, it, vp, it, ec, arr, it, t, vp));
                "0".into()
            }
            "remove" | "swapRemove" => {
                let i = self.expr(fs, &args[1].e);
                let it = self.tmp();
                let r = self.tmp();
                self.emit(fs, &format!("int64_t {} = gojo_idx({}, {}->len, {}); {} {} = {}[{}];", it, i, vp, l, ec, r, arr, it));
                if name == "remove" {
                    self.emit(fs, &format!("memmove({} + {}, {} + {} + 1, (size_t)({}->len - {} - 1) * sizeof({})); {}->len--;", arr, it, arr, it, vp, it, ec, vp));
                } else {
                    self.emit(fs, &format!("{}[{}] = {}[{}->len - 1]; {}->len--;", arr, it, arr, vp, vp));
                }
                r
            }
            "clear" | "truncate" => {
                let n = if name == "truncate" { self.expr(fs, &args[1].e) } else { "0".into() };
                let nt = self.tmp();
                self.emit(fs, &format!("int64_t {} = {};", nt, n));
                if name == "truncate" {
                    self.emit(fs, &format!("if ({} < 0) gojo_panic(\"truncate to a negative length\", {});", nt, l));
                }
                if self.needs_drop(el) {
                    let d = self.drop_fn(el);
                    self.emit(fs, &format!("for (int64_t _i = {}; _i < {}->len; _i++) {}(&{}[_i]);", nt, vp, d, arr));
                }
                self.emit(fs, &format!("if ({} < {}->len) {}->len = {};", nt, vp, vp, nt));
                "0".into()
            }
            "extend" => {
                let o = self.expr(fs, &args[1].e);
                let ot = self.tmp();
                self.emit(fs, &format!("gojo_vec {} = {}; gojo_vec_reserve({}, {}.len, sizeof({})); if ({}.len) memcpy({} + {}->len, {}.ptr, (size_t){}.len * sizeof({})); {}->len += {}.len; free({}.ptr);", ot, o, vp, ot, ec, ot, arr, vp, ot, ot, ec, vp, ot, ot));
                "0".into()
            }
            "reverse" | "swap" | "sort" | "sortBy" => self.inplace_op(fs, name, el, &vp, args, l),
            "reserve" => {
                let n = self.expr(fs, &args[1].e);
                let nt = self.tmp();
                self.emit(fs, &format!("int64_t {} = {};", nt, n));
                self.emit(fs, &format!("if ({nt} > (int64_t)(PTRDIFF_MAX / (sizeof({ec}) ? sizeof({ec}) : 1)) - {vp}->len) gojo_panic(\"capacity overflow\", {l});"));
                self.emit(fs, &format!("if ({} > 0) gojo_vec_reserve({}, {}, sizeof({}));", nt, vp, nt, ec));
                "0".into()
            }
            "cap" => format!("{}->cap", vp),
            _ => panic!("unknown Vec intrinsic {}", name),
        }
    }

    fn map_intrinsic(&mut self, fs: &mut Fs, name: &str, k: &Ty, v: &Ty, args: &[TArg], ret: &Ty, _l: &str) -> String {
        let (ent, desc) = self.map_ent(k, v);
        let mp = self.addr(fs, &args[0].e);
        let m = self.tmp();
        self.emit(fs, &format!("gojo_map *{} = {};", m, mp));
        match name {
            "len" => format!("{}->len", m),
            "get" | "getMut" | "contains" => {
                let kp = self.borrow_ptr(fs, &args[1].e);
                let e = self.tmp();
                self.emit(fs, &format!("{} *{} = mf_{}({}, {});", ent, e, desc, m, kp));
                if name == "contains" {
                    format!("({} != NULL)", e)
                } else {
                    format!("({} ? &{}->v : NULL)", e, e)
                }
            }
            "insert" => {
                let kv = self.expr(fs, &args[1].e);
                let vv = self.expr(fs, &args[2].e);
                self.map_insert(fs, &m, k, v, &kv, &vv)
            }
            "remove" => {
                let kp = self.borrow_ptr(fs, &args[1].e);
                let oc = self.cty(ret);
                let r = self.tmp();
                let tmpe = self.tmp();
                self.emit(fs, &format!("{} {} = {{0}}; {} {};", oc, r, ent, tmpe));
                let dk = if self.needs_drop(k) {
                    let d = self.drop_fn(k);
                    format!(" {}(&{}.k);", d, tmpe)
                } else {
                    String::new()
                };
                let set = self.oset(ret, &r, &format!("{}.v", tmpe));
                self.emit(fs, &format!("if (mr_{}({}, {}, &{})) {{ {}{} }}", desc, m, kp, tmpe, set, dk));
                r
            }
            "entry" => {
                let kv = self.expr(fs, &args[1].e);
                let kc = self.cty(k);
                let tk = self.tmp();
                let ex = self.tmp();
                let e = self.tmp();
                self.emit(fs, &format!("{} {} = {}; bool {};", kc, tk, kv, ex));
                self.emit(fs, &format!("{} *{} = ms_{}({}, &{}, &{});", ent, e, desc, m, tk, ex));
                let dv = self.default_value(fs, v);
                let dk = if self.needs_drop(k) {
                    let d = self.drop_fn(k);
                    format!("{}(&{});", d, tk)
                } else {
                    String::new()
                };
                self.emit(fs, &format!("if (!{}) {{ {}->k = {}; {}->v = {}; }} else {{ {} }}", ex, e, tk, e, dv, dk));
                format!("(&{}->v)", e)
            }
            "clear" => {
                let d = self.drop_fn(&Ty::Map(std::rc::Rc::new(k.clone()), std::rc::Rc::new(v.clone())));
                self.emit(fs, &format!("{}({}); memset({}, 0, sizeof(gojo_map));", d, m, m));
                "0".into()
            }
            _ => panic!("unknown Map intrinsic {}", name),
        }
    }

    // ------------------------------------------------------------------
    // task groups
    // ------------------------------------------------------------------

    /// The C struct holding a scope's state: its group, and the first error
    /// of a failing plain `go` task.
    fn scope_state_ty(&mut self, errt: &Ty) -> String {
        let ec = self.cty(errt);
        let gs = format!("ScopeSt_{}", self.mangle(errt));
        if self.helpers.insert(gs.clone()) {
            let _ = writeln!(self.tdefs, "typedef struct {{ gojo_group *g; bool failed; {} err; }} {};", ec, gs);
        }
        gs
    }

    fn scope_block(&mut self, fs: &mut Fs, id: u32, handle: Option<LocalId>, body: &TBlock, errt: Option<&Ty>, t: &Ty, sp: Span) -> String {
        // a scope that never starts a task (and has no handle to start one
        // through) is just a block
        let mut spawns = false;
        visit_block(body, &mut |e| {
            if let TK::Spawn { target: SpawnTarget::Lexical(i), .. } = &e.kind {
                spawns |= *i == id;
            }
        });
        if !spawns && handle.is_none() && errt.is_none() {
            let has_val = !matches!(t, Ty::Unit | Ty::Never);
            let r = if has_val {
                let c = self.cty(t);
                let r = self.tmp();
                self.emit(fs, &format!("{} {};", c, r));
                Some(r)
            } else {
                None
            };
            self.block_into(fs, body, r.as_deref());
            return r.unwrap_or_else(|| "0".into());
        }
        let errt = errt.map(|e| self.sub(fs, e));
        let et = errt.clone().unwrap_or(Ty::Unit);
        let gs = self.scope_state_ty(&et);
        let st = self.tmp();
        let loc = self.loc(sp);
        self.emit(fs, &format!("{} {} = {{ gojo_group_new({}), 0 }};", gs, st, loc));
        fs.tscopes.insert(id, st.clone());
        if let Some(h) = handle {
            let n = self.local_cname(fs, h);
            self.emit(fs, &format!("gojo_group *{} = {}.g;", n, st));
            fs.repr.insert(h, Repr::Val(n));
        }
        // the body's value
        let vt = match (&errt, t) {
            (Some(_), Ty::Result(v, _)) => (**v).clone(),
            _ => t.clone(),
        };
        let has_val = !matches!(vt, Ty::Unit | Ty::Never);
        let v = if has_val {
            let c = self.cty(&vt);
            let r = self.tmp();
            self.emit(fs, &format!("{} {};", c, r));
            Some(r)
        } else {
            None
        };
        self.emit(fs, "{");
        fs.ind += 1;
        self.push_scope(fs, SK::Block);
        fs.scopes.last_mut().unwrap().wait = Some(format!("{}.g", st));
        let nb = fs.bce.len();
        for (i, s) in body.stmts.iter().enumerate() {
            self.stmt(fs, s);
            self.free_after(fs, body, i);
        }
        if let Some(bv) = &body.value {
            self.push_scope(fs, SK::Stmt);
            let x = self.expr(fs, bv);
            let xt = self.sub(fs, &bv.ty);
            match &v {
                Some(d) if bv.ty != Ty::Never && xt != Ty::Unit => self.emit(fs, &format!("{} = {};", d, x)),
                _ => self.discard(fs, &x, &xt, bv.is_place()),
            }
            self.pop_scope(fs);
        }
        // (the scope's tasks are waited for first)
        self.at_site(fs, drops::Site::block(body), |g, fs| g.pop_scope(fs));
        fs.bce.truncate(nb);
        fs.ind -= 1;
        self.emit(fs, "}");
        fs.tscopes.remove(&id);
        if errt.is_none() {
            return v.unwrap_or_else(|| "0".into());
        }
        let rc = self.cty(t);
        let r = self.tmp();
        self.emit(fs, &format!("{} {};", rc, r));
        let okv = match &v {
            Some(x) => format!(" {}.u.val = {};", r, x),
            None => String::new(),
        };
        // a failed scope drops the body's value
        let dropv = match &v {
            Some(x) if self.needs_drop(&vt) => {
                let d = self.drop_fn(&vt);
                format!(" {}(&{});", d, x)
            }
            _ => String::new(),
        };
        self.emit(fs, &format!("if ({}.failed) {{ {}.ok = 0; {}.u.err = {}.err;{} }} else {{ {}.ok = 1;{} }}", st, r, r, st, dropv, r, okv));
        r
    }

    /// `go ...`: the closure runs as a task of the target scope.
    fn spawn(&mut self, fs: &mut Fs, target: &SpawnTarget, task: &TExpr, handle: bool, sp: Span) -> String {
        let loc = self.loc(sp);
        let (g, st, errt) = match target {
            SpawnTarget::Lexical(id) => {
                let st = fs.tscopes.get(id).cloned().expect("scope of a go");
                (format!("{}.g", st), Some(format!("&{}", st)), None)
            }
            SpawnTarget::Value(v) => (self.expr(fs, v), None, Some(Ty::Unit)),
        };
        let ft = self.sub(fs, &task.ty);
        let ret = match &ft {
            Ty::Func(f) => f.ret.clone(),
            _ => Ty::Unit,
        };
        let f = self.expr(fs, task);
        let rc = self.ret_cty(&ret);
        let call = format!("(({} (*)(void *))a->f.fn)(a->f.env)", rc);
        if handle {
            // the thunk reads the block through the same named type that
            // filled it in (two anonymous structs would be different types)
            if self.helpers.insert("TaskArgs".into()) {
                self.tdefs.push_str("typedef struct { gojo_func f; gojo_group *g; gojo_task *t; } TaskArgs;\n");
            }
            let rt = if ret == Ty::Never { Ty::Unit } else { ret.clone() };
            let rcty = self.cty(&rt);
            let tn = format!("taskhthunk_{}", self.mangle(&rt));
            if self.helpers.insert(tn.clone()) {
                let _ = writeln!(self.protos, "static void {}(void *p);", tn);
                let mut b = format!("static void {}(void *p) {{\n    TaskArgs *a = p;\n", tn);
                if matches!(rt, Ty::Unit) {
                    let _ = writeln!(b, "    {};", call);
                } else {
                    let _ = writeln!(b, "    {} r = {};", rcty, call);
                    if matches!(rt, Ty::Result(..)) {
                        // a failure cancels the scope; the error goes to `join`
                        b.push_str("    if (!r.ok) gojo_group_cancel_failed(a->g);\n");
                    }
                    let _ = writeln!(b, "    *({} *)gojo_task_result(a->t) = r;", rcty);
                }
                b.push_str("    if (a->f.drop) a->f.drop(a->f.env);\n");
                b.push_str("    if (gojo_task_finish(a->t)) {\n");
                if self.needs_drop(&rt) {
                    let d = self.drop_fn(&rt);
                    let _ = writeln!(b, "        {}(({} *)gojo_task_result(a->t));", d, rcty);
                }
                b.push_str("        gojo_task_free(a->t);\n    }\n    free(a);\n}\n");
                self.bodies.push_str(&b);
            }
            let a = self.tmp();
            let tv = self.tmp();
            self.emit(fs, &format!("gojo_task *{} = gojo_task_new(sizeof({}));", tv, rcty));
            self.emit(fs, &format!("TaskArgs *{} = gojo_alloc(sizeof(TaskArgs));", a));
            self.emit(fs, &format!("{}->f = {}; {}->g = {}; {}->t = {};", a, f, a, g, a, tv));
            self.emit(fs, &format!("gojo_group_go_task({}, {}, {}, {}, {});", g, tv, tn, a, loc));
            return tv;
        }
        // a plain task with nothing to report: run the closure directly
        if matches!(ret, Ty::Unit | Ty::Never) {
            let fv = self.tmp();
            self.emit(fs, &format!("gojo_func {} = {};", fv, f));
            self.emit(fs, &format!("gojo_group_go_env({}, (void (*)(void *)){}.fn, {}.env, {}.drop, {});", g, fv, fv, fv, loc));
            return "0".into();
        }
        // a plain task: its error (if any) is the scope's
        let et = match (&errt, &ret) {
            (Some(e), _) => e.clone(),
            (None, Ty::Result(_, e)) => self.sub(fs, e),
            _ => Ty::Unit,
        };
        let gs = self.scope_state_ty(&et);
        let ta = format!("TaskArgs_{}", gs);
        if self.helpers.insert(ta.clone()) {
            let _ = writeln!(self.tdefs, "typedef struct {{ gojo_func f; {} *st; gojo_group *g; }} {};", gs, ta);
        }
        let tn = format!("taskthunk_{}_{}", self.mangle(&ret), self.mangle(&et));
        if self.helpers.insert(tn.clone()) {
            let _ = writeln!(self.protos, "static void {}(void *p);", tn);
            let mut b = format!("static void {}(void *p) {{\n    {} *a = p;\n", tn, ta);
            match &ret {
                Ty::Unit | Ty::Never => {
                    let _ = writeln!(b, "    {};", call);
                }
                Ty::Result(_, _) => {
                    let _ = writeln!(b, "    {} r = {};", rc, call);
                    let dropf = if self.needs_drop(&et) {
                        let d = self.drop_fn(&et);
                        format!(" else {}(&r.u.err);", d)
                    } else {
                        String::new()
                    };
                    let dropok = if self.needs_drop(&ret) {
                        // the success value of a plain task is not used
                        let ok = match &ret {
                            Ty::Result(v, _) => (**v).clone(),
                            _ => Ty::Unit,
                        };
                        if self.needs_drop(&ok) {
                            let d = self.drop_fn(&ok);
                            format!(" else {}(&r.u.val);", d)
                        } else {
                            String::new()
                        }
                    } else {
                        String::new()
                    };
                    let _ = writeln!(b, "    if (!r.ok) {{ if (a->st && gojo_group_fail(a->g)) a->st->err = r.u.err;{} if (a->st) a->st->failed = 1; }}{}", dropf, dropok);
                }
                _ => {
                    let _ = writeln!(b, "    {} r = {};", rc, call);
                    if self.needs_drop(&ret) {
                        let d = self.drop_fn(&ret);
                        let _ = writeln!(b, "    {}(&r);", d);
                    }
                }
            }
            b.push_str("    if (a->f.drop) a->f.drop(a->f.env);\n    free(a);\n}\n");
            self.bodies.push_str(&b);
        }
        let a = self.tmp();
        self.emit(fs, &format!("{} *{} = gojo_alloc(sizeof({}));", ta, a, ta));
        let stv = match &st {
            Some(x) => format!("({} *){}", gs, x),
            None => "NULL".into(),
        };
        self.emit(fs, &format!("{}->f = {}; {}->st = {}; {}->g = {};", a, f, a, stv, a, g));
        self.emit(fs, &format!("gojo_group_go({}, {}, {}, {});", g, tn, a, loc));
        "0".into()
    }

    // ------------------------------------------------------------------
    // Type helpers: drop / clone / eq / cmp / hash / fmt
    // ------------------------------------------------------------------

    fn helper_header(&mut self, name: &str, sig: &str) -> bool {
        if self.helpers.contains(name) {
            return false;
        }
        self.helpers.insert(name.to_string());
        let _ = writeln!(self.protos, "{};", sig);
        true
    }

    fn drop_fn(&mut self, t: &Ty) -> String {
        if self.runaway_type(t) {
            return "gojo_runaway".into();
        }
        let m = self.mangle(t);
        let name = format!("drop_{}", m);
        let c = self.cty(t);
        let sig = format!("static void {}({} *p)", name, c);
        if !self.helper_header(&name, &sig) {
            return name;
        }
        let mut b = String::new();
        let _ = writeln!(b, "{} {{", sig);
        match t {
            Ty::String => b.push_str("    free(p->ptr);\n"),
            Ty::Vec(el) => {
                let el = (**el).clone();
                if self.needs_drop(&el) {
                    let ec = self.cty(&el);
                    let d = self.drop_fn(&el);
                    let _ = writeln!(b, "    for (int64_t i = 0; i < p->len; i++) {}(&(({} *)p->ptr)[i]);", d, ec);
                }
                b.push_str("    free(p->ptr);\n");
            }
            Ty::Map(k, v) => {
                let (k, v) = ((**k).clone(), (**v).clone());
                let (ent, _) = self.map_ent(&k, &v);
                let dk = if self.needs_drop(&k) { Some(self.drop_fn(&k)) } else { None };
                let dv = if self.needs_drop(&v) { Some(self.drop_fn(&v)) } else { None };
                if dk.is_some() || dv.is_some() {
                    let _ = writeln!(b, "    for (int64_t i = 0; i < p->n_entries; i++) {{ {} *e = (({} *)p->entries) + i; if (!e->live) continue;", ent, ent);
                    if let Some(d) = dk {
                        let _ = writeln!(b, "        {}(&e->k);", d);
                    }
                    if let Some(d) = dv {
                        let _ = writeln!(b, "        {}(&e->v);", d);
                    }
                    b.push_str("    }\n");
                }
                b.push_str("    gojo_map_free(p);\n");
            }
            Ty::Box(inner) => {
                let inner = (**inner).clone();
                self.ensure(&inner);
                if self.needs_drop(&inner) {
                    let d = self.drop_fn(&inner);
                    let _ = writeln!(b, "    if (*p) {}(*p);", d);
                }
                b.push_str("    gojo_box_free(*p, sizeof(**p));\n");
            }
            Ty::Rc(inner) | Ty::Arc(inner) => {
                let inner = (**inner).clone();
                self.ensure_rcbox(&inner);
                let dec = if matches!(t, Ty::Arc(_)) { "__atomic_sub_fetch(&(*p)->strong, 1, __ATOMIC_ACQ_REL) == 0".to_string() } else { "--(*p)->strong == 0".to_string() };
                let _ = writeln!(b, "    if (!*p) return;\n    if ({}) {{", dec);
                if self.needs_drop(&inner) {
                    let d = self.drop_fn(&inner);
                    let _ = writeln!(b, "        {}(&(*p)->val);", d);
                }
                b.push_str("        if ((*p)->weak == 0) gojo_box_free(*p, sizeof(**p));\n    }\n");
            }
            Ty::Weak(inner) => {
                let inner = (**inner).clone();
                self.ensure_rcbox(&inner);
                b.push_str("    if (*p && --(*p)->weak == 0 && (*p)->strong == 0) gojo_box_free(*p, sizeof(**p));\n");
            }
            Ty::Sender(el) | Ty::Receiver(el) => {
                let tx = matches!(t, Ty::Sender(_));
                let el = (**el).clone();
                let df = if self.needs_drop(&el) {
                    let d = self.drop_fn(&el);
                    let ec = self.cty(&el);
                    let w = format!("chdrop_{}", self.mangle(&el));
                    if self.helpers.insert(w.clone()) {
                        let _ = writeln!(self.protos, "static void {}(void *p);", w);
                        let _ = writeln!(self.bodies, "static void {}(void *p) {{ {}(({} *)p); }}", w, d, ec);
                    }
                    w
                } else {
                    "NULL".into()
                };
                let _ = writeln!(b, "    gojo_chan_release_{}(*p, {});", if tx { "tx" } else { "rx" }, df);
            }
            Ty::Func(_) => b.push_str("    if (p->drop) p->drop(p->env);\n"),
            Ty::Iso(inner) => {
                let inner = (**inner).clone();
                if self.needs_drop(&inner) {
                    let d = self.drop_fn(&inner);
                    let _ = writeln!(b, "    {}(p);", d);
                }
            }
            Ty::Task(r) => {
                // an unjoined handle: the task is cancelled; the last of the
                // two drops the result
                let r = (**r).clone();
                let _ = writeln!(b, "    if (gojo_task_abandon(*p)) {{");
                if self.needs_drop(&r) {
                    let d = self.drop_fn(&r);
                    let rc = self.cty(&r);
                    let _ = writeln!(b, "        {}(({} *)gojo_task_result(*p));", d, rc);
                }
                b.push_str("        gojo_task_free(*p);\n    }\n");
            }
            Ty::Iface(..) => b.push_str("    if (p->data) { const gojo_vt_base *vt = p->vt; vt->drop(p->data); gojo_box_free(p->data, vt->size); }\n"),
            Ty::Mutex(inner) => {
                let inner = (**inner).clone();
                if self.needs_drop(&inner) {
                    let d = self.drop_fn(&inner);
                    let _ = writeln!(b, "    {}(&p->val);", d);
                }
                b.push_str("    gojo_mutex_free(p->mu);\n");
            }
            Ty::MutexRef(_) => b.push_str("    gojo_mutex_free(p->mu);\n"),
            Ty::Option(inner) => {
                let inner = (**inner).clone();
                if self.needs_drop(&inner) {
                    let d = self.drop_fn(&inner);
                    let _ = writeln!(b, "    if ({}) {}(&{});", self.osome(t, "(*p)"), d, self.oval(t, "(*p)"));
                }
            }
            Ty::Result(a, e) => {
                let (a, e) = ((**a).clone(), (**e).clone());
                if self.needs_drop(&a) {
                    let d = self.drop_fn(&a);
                    let _ = writeln!(b, "    if (p->ok) {}(&p->u.val);", d);
                }
                if self.needs_drop(&e) {
                    let d = self.drop_fn(&e);
                    let _ = writeln!(b, "    if (!p->ok) {}(&p->u.err);", d);
                }
            }
            Ty::Array(el, n) => {
                let el = (**el).clone();
                if self.needs_drop(&el) {
                    let d = self.drop_fn(&el);
                    let _ = writeln!(b, "    for (int64_t i = 0; i < {}; i++) {}(&p->a[i]);", n, d);
                }
            }
            Ty::Cell(inner) => {
                let inner = (**inner).clone();
                if self.needs_drop(&inner) {
                    let d = self.drop_fn(&inner);
                    let _ = writeln!(b, "    {}(p);", d);
                }
            }
            Ty::Adt(d, args) => {
                let def = self.prog.def(*d).clone();
                if def.has_drop {
                    let f = self.prog.methods.get(&(TyKey::Adt(*d), "drop".into())).unwrap()[0];
                    let fd = self.prog.func(f).clone();
                    let mut s = HashMap::new();
                    bind_params(&fd.recv.as_ref().unwrap().ty, t, &mut s);
                    let targs: Vec<Ty> = fd.tparams.iter().map(|p| s.get(p).cloned().unwrap_or(Ty::Unit)).collect();
                    let n = self.instance(f, targs);
                    // in a cancelled task, cleanup may still block (for a while)
                    let _ = writeln!(b, "    uint8_t *sh = gojo_drop_shield();\n    {}(p);\n    gojo_drop_unshield(sh);", n);
                }
                match &def.kind {
                    TypeDefKind::Struct { .. } => {
                        for (fname, ft) in self.prog.field_tys(*d, args) {
                            if self.needs_drop(&ft) {
                                let df = self.drop_fn(&ft);
                                let _ = writeln!(b, "    {}(&p->f_{});", df, fname);
                            }
                        }
                    }
                    TypeDefKind::Enum { variants } => {
                        b.push_str("    switch (p->tag) {\n");
                        for vi in 0..variants.len() {
                            let fsx = self.prog.variant_tys(*d, args, vi);
                            let mut lines = String::new();
                            for (fname, ft) in fsx {
                                if self.needs_drop(&ft) {
                                    let df = self.drop_fn(&ft);
                                    let _ = write!(lines, " {}(&p->u.v{}.f_{});", df, vi, fname);
                                }
                            }
                            if !lines.is_empty() {
                                let _ = writeln!(b, "    case {}:{} break;", vi, lines);
                            }
                        }
                        b.push_str("    default: break;\n    }\n");
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        b.push_str("}\n");
        self.bodies.push_str(&b);
        name
    }

    /// A method that customizes printing: exactly `fn toString(self) String`
    /// (or `message`). Methods of that name with other signatures are ordinary
    /// methods, and the value prints the default way.
    fn printing_method(&mut self, t: &Ty, name: &str) -> Option<(String, Mode)> {
        if let Ty::Adt(d, _) = t {
            let list = self.prog.methods.get(&(TyKey::Adt(*d), name.to_string()))?;
            let fd = self.prog.func(list[0]);
            let recv_ok = fd.recv.as_ref().is_some_and(|r| r.mode == Mode::Borrow);
            if !recv_ok || !fd.params.is_empty() || fd.ret != Ty::String || fd.tparams.len() != self.prog.def(*d).tparams.len() {
                return None;
            }
        }
        self.user_method(t, name)
    }

    fn user_method(&mut self, t: &Ty, name: &str) -> Option<(String, Mode)> {
        if let Ty::Adt(d, _) = t {
            if let Some(list) = self.prog.methods.get(&(TyKey::Adt(*d), name.to_string())).cloned() {
                let f = list[0];
                let fd = self.prog.func(f).clone();
                let mut s = HashMap::new();
                bind_params(&fd.recv.as_ref().unwrap().ty, t, &mut s);
                let targs: Vec<Ty> = fd.tparams.iter().map(|p| s.get(p).cloned().unwrap_or(Ty::Unit)).collect();
                let mode = fd.recv.as_ref().unwrap().mode;
                return Some((self.instance(f, targs), mode));
            }
        }
        None
    }

    fn clone_fn(&mut self, t: &Ty) -> String {
        if self.runaway_type(t) {
            return "gojo_runaway".into();
        }
        let m = self.mangle(t);
        let name = format!("clone_{}", m);
        let c = self.cty(t);
        let sig = format!("static {} {}({} *p)", c, name, c);
        if !self.helper_header(&name, &sig) {
            return name;
        }
        let mut b = format!("{} {{\n", sig);
        if self.is_copy(t) {
            b.push_str("    return *p;\n}\n");
            self.bodies.push_str(&b);
            return name;
        }
        if let Some((f, mode)) = self.user_method(t, "clone") {
            let a = if mode == Mode::Borrow && self.is_copy(t) { "*p" } else { "p" };
            let _ = writeln!(b, "    return {}({});\n}}", f, a);
            self.bodies.push_str(&b);
            return name;
        }
        match t {
            Ty::String => b.push_str("    return gojo_string_clone(p);\n"),
            Ty::Vec(el) => {
                let el = (**el).clone();
                let ec = self.cty(&el);
                let _ = writeln!(b, "    gojo_vec r = {{0}};\n    gojo_vec_reserve(&r, p->len, sizeof({}));\n    r.len = p->len;", ec);
                if self.is_copy(&el) {
                    let _ = writeln!(b, "    if (p->len) memcpy(r.ptr, p->ptr, (size_t)p->len * sizeof({}));", ec);
                } else {
                    let cf = self.clone_fn(&el);
                    let _ = writeln!(b, "    for (int64_t i = 0; i < p->len; i++) (({} *)r.ptr)[i] = {}(&(({} *)p->ptr)[i]);", ec, cf, ec);
                }
                b.push_str("    return r;\n");
            }
            Ty::Map(k, v) => {
                let (k, v) = ((**k).clone(), (**v).clone());
                let (ent, desc) = self.map_ent(&k, &v);
                let ck = self.clone_fn(&k);
                let cv = self.clone_fn(&v);
                let _ = writeln!(b, "    gojo_map r = {{0}};\n    for (int64_t i = 0; i < p->n_entries; i++) {{\n        {} *e = (({} *)p->entries) + i; if (!e->live) continue;\n        bool ex; {} *n = ms_{}(&r, &e->k, &ex);\n        n->k = {}(&e->k); n->v = {}(&e->v);\n    }}\n    return r;", ent, ent, ent, desc, ck, cv);
            }
            Ty::Option(inner) => {
                let inner = (**inner).clone();
                let cf = self.clone_fn(&inner);
                let set = self.oset(t, "r", &format!("{}(&{})", cf, self.oval(t, "(*p)")));
                let _ = writeln!(b, "    {} r = {{0}};\n    if ({}) {{ {} }}\n    return r;", c, self.osome(t, "(*p)"), set);
            }
            Ty::Result(a, e) => {
                let (a, e) = ((**a).clone(), (**e).clone());
                let ca = self.clone_fn(&a);
                let ce = self.clone_fn(&e);
                let _ = writeln!(b, "    {} r; r.ok = p->ok;\n    if (p->ok) r.u.val = {}(&p->u.val); else r.u.err = {}(&p->u.err);\n    return r;", c, ca, ce);
            }
            Ty::Box(inner) => {
                let inner = (**inner).clone();
                let ic = self.cty(&inner);
                let cf = self.clone_fn(&inner);
                let _ = writeln!(b, "    {} *r = gojo_box_alloc(sizeof({})); *r = {}(*p);\n    return r;", ic, ic, cf);
            }
            Ty::Rc(inner) => {
                let inner = (**inner).clone();
                self.ensure_rcbox(&inner);
                b.push_str("    (*p)->strong++;\n    return *p;\n");
            }
            Ty::Weak(inner) => {
                let inner = (**inner).clone();
                self.ensure_rcbox(&inner);
                b.push_str("    (*p)->weak++;\n    return *p;\n");
            }
            Ty::Arc(inner) => {
                let inner = (**inner).clone();
                self.ensure_rcbox(&inner);
                b.push_str("    __atomic_add_fetch(&(*p)->strong, 1, __ATOMIC_RELAXED);\n    return *p;\n");
            }
            Ty::Sender(_) => b.push_str("    gojo_chan_retain_tx(*p);\n    return *p;\n"),
            Ty::Receiver(_) => b.push_str("    gojo_chan_retain_rx(*p);\n    return *p;\n"),
            Ty::Cell(inner) => {
                let inner = (**inner).clone();
                let cf = self.clone_fn(&inner);
                let _ = writeln!(b, "    return {}(p);", cf);
            }
            Ty::Array(el, n) => {
                let el = (**el).clone();
                let cf = self.clone_fn(&el);
                let _ = writeln!(b, "    {} r;\n    for (int64_t i = 0; i < {}; i++) r.a[i] = {}(&p->a[i]);\n    return r;", c, n, cf);
            }
            Ty::Adt(d, args) => {
                let def = self.prog.def(*d).clone();
                match &def.kind {
                    TypeDefKind::Struct { .. } => {
                        let _ = writeln!(b, "    {} r;", c);
                        for (fname, ft) in self.prog.field_tys(*d, args) {
                            let cf = self.clone_fn(&ft);
                            let _ = writeln!(b, "    r.f_{} = {}(&p->f_{});", fname, cf, fname);
                        }
                        b.push_str("    return r;\n");
                    }
                    TypeDefKind::Enum { variants } => {
                        let _ = writeln!(b, "    {} r = *p;\n    switch (p->tag) {{", c);
                        for vi in 0..variants.len() {
                            let fsx = self.prog.variant_tys(*d, args, vi);
                            let mut lines = String::new();
                            for (fname, ft) in fsx {
                                if !self.is_copy(&ft) {
                                    let cf = self.clone_fn(&ft);
                                    let _ = write!(lines, " r.u.v{}.f_{} = {}(&p->u.v{}.f_{});", vi, fname, cf, vi, fname);
                                }
                            }
                            if !lines.is_empty() {
                                let _ = writeln!(b, "    case {}:{} break;", vi, lines);
                            }
                        }
                        b.push_str("    default: break;\n    }\n    return r;\n");
                    }
                    _ => b.push_str("    return *p;\n"),
                }
            }
            _ => b.push_str("    return *p;\n"),
        }
        b.push_str("}\n");
        self.bodies.push_str(&b);
        name
    }

    fn eq_fn(&mut self, t: &Ty) -> String {
        if self.runaway_type(t) {
            return "gojo_runaway".into();
        }
        let m = self.mangle(t);
        let name = format!("eq_{}", m);
        let c = self.cty(t);
        let sig = format!("static bool {}({} *a, {} *b)", name, c, c);
        if !self.helper_header(&name, &sig) {
            return name;
        }
        let mut b = format!("{} {{\n", sig);
        if let Some((f, mode)) = self.user_method(t, "equal") {
            let (x, y) = if mode == Mode::Borrow && self.is_copy(t) { ("*a", "*b") } else { ("a", "b") };
            let _ = writeln!(b, "    return {}({}, {});\n}}", f, x, if self.is_copy(t) { y.to_string() } else { "b".into() });
            self.bodies.push_str(&b);
            return name;
        }
        match t {
            Ty::Unit => b.push_str("    return true;\n"),
            Ty::Bool | Ty::Int(_) | Ty::F32 | Ty::F64 | Ty::Ptr(_) => b.push_str("    return *a == *b;\n"),
            Ty::Str => b.push_str("    return gojo_str_eq(*a, *b);\n"),
            Ty::String => b.push_str("    return gojo_str_eq(gojo_string_view(a), gojo_string_view(b));\n"),
            Ty::Vec(el) | Ty::Slice(el) => {
                let el = (**el).clone();
                let ec = self.cty(&el);
                let ef = self.eq_fn(&el);
                let _ = writeln!(b, "    if (a->len != b->len) return false;\n    for (int64_t i = 0; i < a->len; i++) if (!{}(&(({} *)a->ptr)[i], &(({} *)b->ptr)[i])) return false;\n    return true;", ef, ec, ec);
            }
            Ty::Array(el, n) => {
                let el = (**el).clone();
                let ef = self.eq_fn(&el);
                let _ = writeln!(b, "    for (int64_t i = 0; i < {}; i++) if (!{}(&a->a[i], &b->a[i])) return false;\n    return true;", n, ef);
            }
            Ty::Option(inner) => {
                let inner = (**inner).clone();
                let ef = self.eq_fn(&inner);
                let (sa, sb) = (self.osome(t, "(*a)"), self.osome(t, "(*b)"));
                let (va, vb) = (self.oval(t, "(*a)"), self.oval(t, "(*b)"));
                let _ = writeln!(b, "    if ({} != {}) return false;\n    return !{} || {}(&{}, &{});", sa, sb, sa, ef, va, vb);
            }
            Ty::Result(x, y) => {
                let (x, y) = ((**x).clone(), (**y).clone());
                let ex = self.eq_fn(&x);
                let ey = self.eq_fn(&y);
                let _ = writeln!(b, "    if (a->ok != b->ok) return false;\n    return a->ok ? {}(&a->u.val, &b->u.val) : {}(&a->u.err, &b->u.err);", ex, ey);
            }
            Ty::Box(inner) => {
                let inner = (**inner).clone();
                let ef = self.eq_fn(&inner);
                let _ = writeln!(b, "    return {}(*a, *b);", ef);
            }
            Ty::Adt(d, args) => {
                let def = self.prog.def(*d).clone();
                match &def.kind {
                    TypeDefKind::Struct { .. } => {
                        for (fname, ft) in self.prog.field_tys(*d, args) {
                            let ef = self.eq_fn(&ft);
                            let _ = writeln!(b, "    if (!{}(&a->f_{}, &b->f_{})) return false;", ef, fname, fname);
                        }
                        b.push_str("    return true;\n");
                    }
                    TypeDefKind::Enum { variants } => {
                        b.push_str("    if (a->tag != b->tag) return false;\n    switch (a->tag) {\n");
                        for vi in 0..variants.len() {
                            let fsx = self.prog.variant_tys(*d, args, vi);
                            if fsx.is_empty() {
                                continue;
                            }
                            let _ = write!(b, "    case {}:", vi);
                            for (fname, ft) in fsx {
                                let ef = self.eq_fn(&ft);
                                let _ = write!(b, " if (!{}(&a->u.v{}.f_{}, &b->u.v{}.f_{})) return false;", ef, vi, fname, vi, fname);
                            }
                            b.push_str(" break;\n");
                        }
                        b.push_str("    default: break;\n    }\n    return true;\n");
                    }
                    _ => b.push_str("    return false;\n"),
                }
            }
            _ => b.push_str("    return a == b;\n"),
        }
        b.push_str("}\n");
        self.bodies.push_str(&b);
        name
    }

    /// A sort specialized for the element type. Scalars and strings use an
    /// introsort (their order among equals is unobservable); other types use a
    /// stable merge sort. Comparisons are direct, never through a pointer.
    fn sort_fn(&mut self, t: &Ty) -> String {
        if self.runaway_type(t) {
            return "gojo_runaway".into();
        }
        let m = self.mangle(t);
        let name = format!("sort_{}", m);
        let c = self.cty(t);
        let sig = format!("static void {}({} *a, int64_t n)", name, c);
        if !self.helper_header(&name, &sig) {
            return name;
        }
        let lt = match t {
            Ty::Int(_) | Ty::F32 | Ty::F64 | Ty::Bool => "(*(x) < *(y))".to_string(),
            Ty::Str => "(gojo_str_cmp(*(x), *(y)) < 0)".to_string(),
            Ty::String => "(gojo_str_cmp(gojo_string_view(x), gojo_string_view(y)) < 0)".to_string(),
            _ => {
                let cf = self.cmp_fn(t);
                format!("({}((x), (y)) < 0)", cf)
            }
        };
        let unstable_ok = matches!(t, Ty::Int(_) | Ty::F32 | Ty::F64 | Ty::Bool | Ty::Str | Ty::String);
        let mut b = String::new();
        let _ = writeln!(b, "#define LT_{m}(x, y) {lt}");
        let _ = writeln!(b, "static void isort_{m}({c} *a, int64_t n) {{\n    for (int64_t i = 1; i < n; i++) {{\n        {c} t = a[i]; int64_t j = i;\n        while (j > 0 && LT_{m}(&t, &a[j - 1])) {{ a[j] = a[j - 1]; j--; }}\n        a[j] = t;\n    }}\n}}");
        if unstable_ok {
            let _ = writeln!(
                b,
                "static void sift_{m}({c} *a, int64_t i, int64_t n) {{\n    for (;;) {{\n        int64_t l = 2 * i + 1, big = i;\n        if (l < n && LT_{m}(&a[big], &a[l])) big = l;\n        if (l + 1 < n && LT_{m}(&a[big], &a[l + 1])) big = l + 1;\n        if (big == i) return;\n        {c} t = a[i]; a[i] = a[big]; a[big] = t; i = big;\n    }}\n}}\n\
static void intro_{m}({c} *a, int64_t n, int depth) {{\n    while (n > 24) {{\n        if (depth-- == 0) {{\n            for (int64_t i = n / 2 - 1; i >= 0; i--) sift_{m}(a, i, n);\n            for (int64_t e = n - 1; e > 0; e--) {{ {c} t = a[0]; a[0] = a[e]; a[e] = t; sift_{m}(a, 0, e); }}\n            return;\n        }}\n        int64_t mid = n / 2;\n        if (LT_{m}(&a[mid], &a[0])) {{ {c} t = a[mid]; a[mid] = a[0]; a[0] = t; }}\n        if (LT_{m}(&a[n - 1], &a[mid])) {{ {c} t = a[mid]; a[mid] = a[n - 1]; a[n - 1] = t; }}\n        if (LT_{m}(&a[mid], &a[0])) {{ {c} t = a[mid]; a[mid] = a[0]; a[0] = t; }}\n        {c} p = a[mid];\n        int64_t i = -1, j = n;\n        for (;;) {{\n            do i++; while (LT_{m}(&a[i], &p));\n            do j--; while (LT_{m}(&p, &a[j]));\n            if (i >= j) break;\n            {c} t = a[i]; a[i] = a[j]; a[j] = t;\n        }}\n        int64_t left = j + 1;\n        if (left < n - left) {{ intro_{m}(a, left, depth); a += left; n -= left; }}\n        else {{ intro_{m}(a + left, n - left, depth); n = left; }}\n    }}\n    isort_{m}(a, n);\n}}"
            );
            let _ = writeln!(b, "{sig} {{\n    int depth = 0;\n    for (int64_t k = n; k > 1; k >>= 1) depth += 2;\n    intro_{m}(a, n, depth);\n}}");
        } else {
            let _ = writeln!(
                b,
                "static void msort_{m}({c} *a, {c} *tmp, int64_t n) {{\n    if (n <= 16) {{ isort_{m}(a, n); return; }}\n    int64_t h = n / 2;\n    msort_{m}(a, tmp, h);\n    msort_{m}(a + h, tmp, n - h);\n    if (!LT_{m}(&a[h], &a[h - 1])) return;\n    memcpy(tmp, a, (size_t)n * sizeof({c}));\n    int64_t i = 0, j = h, k = 0;\n    while (i < h && j < n) a[k++] = LT_{m}(&tmp[j], &tmp[i]) ? tmp[j++] : tmp[i++];\n    while (i < h) a[k++] = tmp[i++];\n    while (j < n) a[k++] = tmp[j++];\n}}"
            );
            let _ = writeln!(b, "{sig} {{\n    if (n < 2) return;\n    {c} *tmp = gojo_alloc((size_t)n * sizeof({c}));\n    msort_{m}(a, tmp, n);\n    free(tmp);\n}}");
        }
        self.bodies.push_str(&b);
        name
    }

    fn cmp_fn(&mut self, t: &Ty) -> String {
        if self.runaway_type(t) {
            return "gojo_runaway".into();
        }
        let m = self.mangle(t);
        let name = format!("cmp_{}", m);
        let c = self.cty(t);
        let sig = format!("static int {}({} *a, {} *b)", name, c, c);
        if !self.helper_header(&name, &sig) {
            return name;
        }
        let mut b = format!("{} {{\n", sig);
        if let Some((f, mode)) = self.user_method(t, "compare") {
            let (x, y) = if mode == Mode::Borrow && self.is_copy(t) { ("*a", "*b") } else { ("a", "b") };
            // Ordering: Less=0, Equal=1, Greater=2
            let _ = writeln!(b, "    return {}({}, {}).tag - 1;\n}}", f, x, y);
            self.bodies.push_str(&b);
            return name;
        }
        if let Ty::Adt(d, args) = t {
            if self.prog.def(*d).newtype {
                let inner = self.prog.field_tys(*d, args)[0].1.clone();
                let f = self.cmp_fn(&inner);
                let _ = writeln!(b, "    return {}(&a->f_value, &b->f_value);\n}}", f);
                self.bodies.push_str(&b);
                return name;
            }
        }
        match t {
            Ty::Str => b.push_str("    return gojo_str_cmp(*a, *b);\n"),
            Ty::String => b.push_str("    return gojo_str_cmp(gojo_string_view(a), gojo_string_view(b));\n"),
            Ty::Adt(..) => b.push_str("    return a->tag < b->tag ? -1 : a->tag > b->tag;\n"),
            _ => b.push_str("    return *a < *b ? -1 : *a > *b;\n"),
        }
        b.push_str("}\n");
        self.bodies.push_str(&b);
        name
    }

    fn hash_fn(&mut self, t: &Ty) -> String {
        if self.runaway_type(t) {
            return "gojo_runaway".into();
        }
        let m = self.mangle(t);
        let name = format!("hash_{}", m);
        let c = self.cty(t);
        let sig = format!("static uint64_t {}({} *p)", name, c);
        if !self.helper_header(&name, &sig) {
            return name;
        }
        let mut b = format!("{} {{\n", sig);
        if let Some((f, mode)) = self.user_method(t, "hash") {
            let hasher = self.prog.prelude_type("Hasher").unwrap();
            let ht = Ty::Adt(hasher, std::rc::Rc::new(vec![]));
            let hc = self.cty(&ht);
            let x = if mode == Mode::Borrow && self.is_copy(t) { "*p" } else { "p" };
            let _ = writeln!(b, "    {} h = {{ 14695981039346656037ULL }};\n    {}({}, &h);\n    return gojo_hash_u64(h.f_state);\n}}", hc, f, x);
            self.bodies.push_str(&b);
            return name;
        }
        match t {
            Ty::Unit => b.push_str("    return 0;\n"),
            Ty::Bool | Ty::Int(_) => b.push_str("    return gojo_hash_u64((uint64_t)*p);\n"),
            Ty::Str => b.push_str("    return gojo_str_hash(*p);\n"),
            Ty::String => b.push_str("    return gojo_str_hash(gojo_string_view(p));\n"),
            Ty::Vec(el) | Ty::Slice(el) => {
                let el = (**el).clone();
                let ec = self.cty(&el);
                let hf = self.hash_fn(&el);
                let _ = writeln!(b, "    uint64_t h = 7;\n    for (int64_t i = 0; i < p->len; i++) h = gojo_hash_mix(h, {}(&(({} *)p->ptr)[i]));\n    return h;", hf, ec);
            }
            Ty::Array(el, n) => {
                let el = (**el).clone();
                let hf = self.hash_fn(&el);
                let _ = writeln!(b, "    uint64_t h = 7;\n    for (int64_t i = 0; i < {}; i++) h = gojo_hash_mix(h, {}(&p->a[i]));\n    return h;", n, hf);
            }
            Ty::Option(inner) => {
                let inner = (**inner).clone();
                let hf = self.hash_fn(&inner);
                let _ = writeln!(b, "    return {} ? gojo_hash_mix(1, {}(&{})) : 0;", self.osome(t, "(*p)"), hf, self.oval(t, "(*p)"));
            }
            Ty::Box(inner) => {
                let inner = (**inner).clone();
                let hf = self.hash_fn(&inner);
                let _ = writeln!(b, "    return {}(*p);", hf);
            }
            Ty::Adt(d, args) => {
                let def = self.prog.def(*d).clone();
                match &def.kind {
                    TypeDefKind::Struct { .. } => {
                        b.push_str("    uint64_t h = 7;\n");
                        for (fname, ft) in self.prog.field_tys(*d, args) {
                            let hf = self.hash_fn(&ft);
                            let _ = writeln!(b, "    h = gojo_hash_mix(h, {}(&p->f_{}));", hf, fname);
                        }
                        b.push_str("    return h;\n");
                    }
                    TypeDefKind::Enum { variants } => {
                        b.push_str("    uint64_t h = gojo_hash_u64((uint64_t)p->tag);\n    switch (p->tag) {\n");
                        for vi in 0..variants.len() {
                            let fsx = self.prog.variant_tys(*d, args, vi);
                            if fsx.is_empty() {
                                continue;
                            }
                            let _ = write!(b, "    case {}:", vi);
                            for (fname, ft) in fsx {
                                let hf = self.hash_fn(&ft);
                                let _ = write!(b, " h = gojo_hash_mix(h, {}(&p->u.v{}.f_{}));", hf, vi, fname);
                            }
                            b.push_str(" break;\n");
                        }
                        b.push_str("    default: break;\n    }\n    return h;\n");
                    }
                    _ => b.push_str("    return 0;\n"),
                }
            }
            _ => b.push_str("    return 0;\n"),
        }
        b.push_str("}\n");
        self.bodies.push_str(&b);
        name
    }

    fn fmt_fn(&mut self, t: &Ty) -> String {
        if self.runaway_type(t) {
            return "gojo_runaway".into();
        }
        let m = self.mangle(t);
        let name = format!("fmt_{}", m);
        let c = self.cty(t);
        let sig = format!("static void {}(gojo_string *b, {} *p, int nested)", name, c);
        if !self.helper_header(&name, &sig) {
            return name;
        }
        let mut b = format!("{} {{\n    (void)nested;\n", sig);
        let user = self.printing_method(t, "toString").or_else(|| self.printing_method(t, "message"));
        if let Some((f, mode)) = user {
            let x = if mode == Mode::Borrow && self.is_copy(t) { "*p" } else { "p" };
            let _ = writeln!(b, "    gojo_string s = {}({});\n    gojo_fmt_str(b, gojo_string_view(&s), 0);\n    free(s.ptr);\n}}", f, x);
            self.bodies.push_str(&b);
            return name;
        }
        match t {
            Ty::Unit => b.push_str("    gojo_fmt_cstr(b, \"()\");\n"),
            Ty::Bool => b.push_str("    gojo_fmt_bool(b, *p);\n"),
            Ty::Int(k) => {
                if k.signed() {
                    b.push_str("    gojo_fmt_i64(b, (int64_t)*p);\n")
                } else {
                    b.push_str("    gojo_fmt_u64(b, (uint64_t)*p);\n")
                }
            }
            Ty::F32 | Ty::F64 => b.push_str("    gojo_fmt_f64(b, (double)*p);\n"),
            Ty::Str => b.push_str("    gojo_fmt_str(b, *p, nested);\n"),
            Ty::String => b.push_str("    gojo_fmt_str(b, gojo_string_view(p), nested);\n"),
            Ty::Vec(el) | Ty::Slice(el) => {
                let el = (**el).clone();
                let ec = self.cty(&el);
                let ff = self.fmt_fn(&el);
                let _ = writeln!(b, "    gojo_fmt_cstr(b, \"[\");\n    for (int64_t i = 0; i < p->len; i++) {{ if (i) gojo_fmt_cstr(b, \", \"); {}(b, &(({} *)p->ptr)[i], 1); }}\n    gojo_fmt_cstr(b, \"]\");", ff, ec);
            }
            Ty::Array(el, n) => {
                let el = (**el).clone();
                let ff = self.fmt_fn(&el);
                let _ = writeln!(b, "    gojo_fmt_cstr(b, \"[\");\n    for (int64_t i = 0; i < {}; i++) {{ if (i) gojo_fmt_cstr(b, \", \"); {}(b, &p->a[i], 1); }}\n    gojo_fmt_cstr(b, \"]\");", n, ff);
            }
            Ty::Map(k, v) => {
                let (k, v) = ((**k).clone(), (**v).clone());
                let (ent, _) = self.map_ent(&k, &v);
                let fk = self.fmt_fn(&k);
                let fv = self.fmt_fn(&v);
                let _ = writeln!(b, "    gojo_fmt_cstr(b, \"{{\");\n    int first = 1;\n    for (int64_t i = 0; i < p->n_entries; i++) {{ {} *e = (({} *)p->entries) + i; if (!e->live) continue; if (!first) gojo_fmt_cstr(b, \", \"); first = 0; {}(b, &e->k, 1); gojo_fmt_cstr(b, \": \"); {}(b, &e->v, 1); }}\n    gojo_fmt_cstr(b, \"}}\");", ent, ent, fk, fv);
            }
            Ty::Option(inner) => {
                let inner = (**inner).clone();
                let ff = self.fmt_fn(&inner);
                let _ = writeln!(b, "    if ({}) {{ gojo_fmt_cstr(b, \"some(\"); {}(b, &{}, 1); gojo_fmt_cstr(b, \")\"); }} else gojo_fmt_cstr(b, \"none\");", self.osome(t, "(*p)"), ff, self.oval(t, "(*p)"));
            }
            Ty::Result(x, y) => {
                let (x, y) = ((**x).clone(), (**y).clone());
                let fx = self.fmt_fn(&x);
                let fy = self.fmt_fn(&y);
                let _ = writeln!(b, "    if (p->ok) {{ gojo_fmt_cstr(b, \"ok(\"); {}(b, &p->u.val, 1); }} else {{ gojo_fmt_cstr(b, \"err(\"); {}(b, &p->u.err, 1); }}\n    gojo_fmt_cstr(b, \")\");", fx, fy);
            }
            Ty::Box(inner) => {
                let inner = (**inner).clone();
                let ff = self.fmt_fn(&inner);
                let _ = writeln!(b, "    {}(b, *p, nested);", ff);
            }
            Ty::Rc(inner) | Ty::Arc(inner) => {
                let inner = (**inner).clone();
                self.ensure_rcbox(&inner);
                let ff = self.fmt_fn(&inner);
                let _ = writeln!(b, "    {}(b, &(*p)->val, nested);", ff);
            }
            Ty::Weak(_) => b.push_str("    gojo_fmt_cstr(b, \"(weak)\");\n"),
            Ty::Cell(inner) => {
                let inner = (**inner).clone();
                let ff = self.fmt_fn(&inner);
                let _ = writeln!(b, "    {}(b, p, nested);", ff);
            }
            Ty::Mutex(_) | Ty::MutexRef(_) => b.push_str("    gojo_fmt_cstr(b, \"Mutex{..}\");\n"),
            Ty::Sender(_) => b.push_str("    gojo_fmt_cstr(b, \"Sender\");\n"),
            Ty::Iso(_) => b.push_str("    gojo_fmt_cstr(b, \"Iso\");\n"),
            Ty::Receiver(_) => b.push_str("    gojo_fmt_cstr(b, \"Receiver\");\n"),
            Ty::Func(_) => b.push_str("    gojo_fmt_cstr(b, \"<fn>\");\n"),
            Ty::Ptr(_) => b.push_str("    gojo_fmt_ptr(b, *p);\n"),
            Ty::Iface(..) => b.push_str("    ((const gojo_vt_base *)p->vt)->fmt(b, p->data, nested);\n"),
            Ty::Adt(d, args) => {
                let def = self.prog.def(*d).clone();
                match &def.kind {
                    TypeDefKind::Struct { .. } => {
                        let fsx = self.prog.field_tys(*d, args);
                        if def.newtype {
                            let ff = self.fmt_fn(&fsx[0].1);
                            let _ = writeln!(b, "    {}(b, &p->f_value, nested);", ff);
                        } else if Some(*d) == self.prog.prelude_type("Set") && fsx.len() == 1 && matches!(fsx[0].1, Ty::Map(..)) {
                            // a set prints its elements: `{"a", "b"}`
                            let Ty::Map(k, v) = fsx[0].1.clone() else { unreachable!() };
                            let (ent, _) = self.map_ent(&k, &v);
                            let fk = self.fmt_fn(&k);
                            let _ = writeln!(b, "    gojo_fmt_cstr(b, \"{{\");\n    int first = 1;\n    for (int64_t i = 0; i < p->f_{}.n_entries; i++) {{ {} *e = (({} *)p->f_{}.entries) + i; if (!e->live) continue; if (!first) gojo_fmt_cstr(b, \", \"); first = 0; {}(b, &e->k, 1); }}\n    gojo_fmt_cstr(b, \"}}\");", fsx[0].0, ent, ent, fsx[0].0, fk);
                        } else if def.tuple {
                            b.push_str("    gojo_fmt_cstr(b, \"(\");\n");
                            for (i, (fname, ft)) in fsx.iter().enumerate() {
                                let ff = self.fmt_fn(ft);
                                let _ = writeln!(b, "    gojo_fmt_cstr(b, \"{}\"); {}(b, &p->f_{}, 1);", if i > 0 { ", " } else { "" }, ff, fname);
                            }
                            b.push_str("    gojo_fmt_cstr(b, \")\");\n");
                        } else {
                            let _ = writeln!(b, "    gojo_fmt_cstr(b, \"{}{{\");", def.name);
                            for (i, (fname, ft)) in fsx.iter().enumerate() {
                                let ff = self.fmt_fn(ft);
                                let _ = writeln!(b, "    gojo_fmt_cstr(b, \"{}{}: \"); {}(b, &p->f_{}, 1);", if i > 0 { ", " } else { "" }, fname, ff, fname);
                            }
                            b.push_str("    gojo_fmt_cstr(b, \"}\");\n");
                        }
                    }
                    TypeDefKind::Enum { variants } => {
                        b.push_str("    switch (p->tag) {\n");
                        for (vi, v) in variants.iter().enumerate() {
                            let fsx = self.prog.variant_tys(*d, args, vi);
                            let _ = write!(b, "    case {}: gojo_fmt_cstr(b, \"{}\");", vi, v.name);
                            if !fsx.is_empty() {
                                b.push_str(" gojo_fmt_cstr(b, \"(\");");
                                for (i, (fname, ft)) in fsx.iter().enumerate() {
                                    let ff = self.fmt_fn(ft);
                                    if i > 0 {
                                        b.push_str(" gojo_fmt_cstr(b, \", \");");
                                    }
                                    let _ = write!(b, " {}(b, &p->u.v{}.f_{}, 1);", ff, vi, fname);
                                }
                                b.push_str(" gojo_fmt_cstr(b, \")\");");
                            }
                            b.push_str(" break;\n");
                        }
                        b.push_str("    default: break;\n    }\n");
                    }
                    _ => {}
                }
            }
            _ => b.push_str("    gojo_fmt_cstr(b, \"?\");\n"),
        }
        b.push_str("}\n");
        self.bodies.push_str(&b);
        name
    }
}

// ------------------------------------------------------------------
// JSON: `json.encode` and `json.decode` of any type
// ------------------------------------------------------------------
//
// One encoder and one decoder per type, like the `fmt_` helpers: the
// encoder writes compact JSON straight into a String, and the decoder pulls
// tokens from the text (runtime/gojo_lib.c) straight into the value, with no
// tree in between. Structs are objects with their fields' names as keys,
// enums without data are strings, other enums are a string for a variant
// without fields and `{"Variant": {fields}}` for one with fields, tuples
// and sets are arrays, `?T` is null or the value, and `json.Value` is any
// JSON. A decoder leaves nothing to drop when it fails.

impl<'a> Gen<'a> {
    fn json_intrinsic(&mut self, fs: &mut Fs, qn: &str, targs: &[Ty], args: &[TArg], sp: Span) -> Option<String> {
        let op = match qn {
            "json.encode" | "json.encodePretty" | "json.toValue" => "encode",
            "json.decode" | "json.fromValue" => "decode",
            _ => return None,
        };
        let t = targs[0].clone();
        self.json_site = (sp, op);
        self.json_ctx.clear();
        let r = self.tmp();
        match qn {
            "json.encode" | "json.encodePretty" | "json.toValue" => {
                let f = self.jenc_fn(&t);
                let p = self.addr(fs, &args[0].e);
                self.emit(fs, &format!("gojo_string {} = {{0}}; {}(&{}, {});", r, f, r, p));
                if qn == "json.encodePretty" {
                    let q = self.tmp();
                    self.emit(fs, &format!("gojo_string {q} = {{0}}; gojo_json___indent(gojo_string_view(&{r}), 2, &{q}); free({r}.ptr);"));
                    return Some(q);
                }
                if qn == "json.toValue" {
                    // what the encoder wrote, read back as a tree
                    let vt = self.json_value_ty();
                    let res = self.jdec_call(fs, &vt, &format!("gojo_string_view(&{})", r));
                    let l = self.loc(sp);
                    self.emit(fs, &format!("free({r}.ptr); if (!{res}.ok) gojo_panic(\"json.toValue: the value is nested too deep\", {l});"));
                    return Some(format!("{}.u.val", res));
                }
                Some(r)
            }
            _ => {
                if qn == "json.fromValue" {
                    let vt = self.json_value_ty();
                    let (f, _) = self.printing_method(&vt, "encode").expect("json.Value.encode");
                    let p = self.addr(fs, &args[0].e);
                    self.emit(fs, &format!("gojo_string {} = {}({});", r, f, p));
                    let res = self.jdec_call(fs, &t, &format!("gojo_string_view(&{})", r));
                    self.emit(fs, &format!("free({}.ptr);", r));
                    return Some(res);
                }
                let text = self.expr(fs, &args[0].e);
                self.emit(fs, &format!("gojo_str {} = {};", r, text));
                Some(self.jdec_call(fs, &t, &r))
            }
        }
    }

    fn json_value_ty(&self) -> Ty {
        let pkg = self.prog.packages.iter().find(|p| p.is_std && p.name == "json").expect("std/json");
        let Some(Sym::Type(d)) = pkg.items.get("Value") else { unreachable!() };
        Ty::Adt(*d, std::rc::Rc::new(vec![]))
    }

    /// Decodes a `t` from the C `gojo_str` expression `text` (evaluated
    /// once): the C name of a `t ! json.Error` result.
    fn jdec_call(&mut self, fs: &mut Fs, t: &Ty, text: &str) -> String {
        let pkg = self.prog.packages.iter().find(|p| p.is_std && p.name == "json").expect("std/json");
        let Some(Sym::Type(ed)) = pkg.items.get("Error") else { unreachable!() };
        let et = Ty::Adt(*ed, std::rc::Rc::new(vec![]));
        let rt = Ty::Result(std::rc::Rc::new(t.clone()), std::rc::Rc::new(et));
        let rc = self.cty(&rt);
        let tf: &'a TFunc = self.tf[&fs.tf.func];
        let f = self.jdec_fn(tf, t);
        let c = self.cty(t);
        let drop = if self.needs_drop(t) { format!(" else {}(&v);", self.drop_fn(t)) } else { String::new() };
        let r = self.tmp();
        self.emit(fs, &format!("{rc} {r}; {{ gojo_str s = {text}; gojo_jdec d; gojo_jdec_init(&d, s); {c} v; if ({f}(&d, &v)) {{ if (gojo_jdec_end(&d)) {{ {r}.ok = 1; {r}.u.val = v; }}{drop} }} if (d.errpos >= 0) {{ gojo_string msg = {{0}}; int64_t at = gojo_jdec_result(&d, &msg), line, col; gojo_jdec_linecol(s, at, &line, &col); {r}.ok = 0; {r}.u.err.f_msg = msg; {r}.u.err.f_line = line; {r}.u.err.f_column = col; }} gojo_jdec_free(&d); }}"));
        r
    }

    fn is_json_value(&self, t: &Ty) -> bool {
        match t {
            Ty::Adt(d, _) => {
                let def = self.prog.def(*d);
                let pkg = &self.prog.packages[def.pkg as usize];
                def.name == "Value" && pkg.name == "json" && pkg.is_std
            }
            _ => false,
        }
    }

    /// Records that `t` can't go to or from JSON (the first such type).
    fn json_bad(&mut self, t: &Ty, why: &str) {
        if self.json_err.is_some() {
            return;
        }
        let (sp, op) = self.json_site;
        let at = if (sp.file as usize) < self.sm.files.len() {
            let f = self.sm.file(sp.file);
            let (l, c) = f.line_col(sp.lo);
            format!("{}:{}:{}: ", f.path, l, c)
        } else {
            String::new()
        };
        let via = if self.json_ctx.is_empty() { String::new() } else { format!(" (in {})", self.json_ctx.join(" in ")) };
        self.json_err = Some(format!("{}`json.{}` can't handle `{}`{}: {}", at, op, self.prog.show(t), via, why));
    }

    fn json_unsupported(&self, t: &Ty) -> &'static str {
        match t {
            Ty::Result(..) => "a result has no JSON form (use a struct or an enum)",
            Ty::Iface(..) => "an interface value could be any type (use an enum)",
            Ty::Func(_) => "a function has no JSON form",
            Ty::Ptr(_) => "a raw pointer has no JSON form",
            Ty::Weak(_) => "a weak reference has no JSON form",
            Ty::Mutex(_) | Ty::MutexRef(_) => "a lock has no JSON form (convert what it guards)",
            Ty::Sender(_) | Ty::Receiver(_) => "a channel has no JSON form",
            Ty::Iso(_) => "an isolated value has no JSON form (take it out first)",
            _ => "it has no JSON form",
        }
    }

    /// `static void jenc_M(gojo_string *b, T *p)`: `*p` as compact JSON.
    fn jenc_fn(&mut self, t: &Ty) -> String {
        if self.runaway_type(t) {
            return "gojo_runaway".into();
        }
        let m = self.mangle(t);
        let name = format!("jenc_{}", m);
        let c = self.cty(t);
        let sig = format!("static void {}(gojo_string *b, {} *p)", name, c);
        if !self.helper_header(&name, &sig) {
            return name;
        }
        let mut b = format!("{} {{\n", sig);
        match t {
            Ty::Unit => b.push_str("    (void)p; gojo_fmt_cstr(b, \"null\");\n"),
            Ty::Bool => b.push_str("    gojo_fmt_cstr(b, *p ? \"true\" : \"false\");\n"),
            Ty::Int(k) => {
                if k.signed() {
                    b.push_str("    gojo_fmt_i64(b, (int64_t)*p);\n")
                } else {
                    b.push_str("    gojo_fmt_u64(b, (uint64_t)*p);\n")
                }
            }
            Ty::F32 | Ty::F64 => b.push_str("    gojo_jenc_f64(b, (double)*p);\n"),
            Ty::Str => b.push_str("    gojo_jenc_str(b, *p);\n"),
            Ty::String => b.push_str("    gojo_jenc_str(b, gojo_string_view(p));\n"),
            Ty::Vec(el) | Ty::Slice(el) => {
                let el = (**el).clone();
                let ec = self.cty(&el);
                let f = self.jenc_fn(&el);
                let _ = writeln!(b, "    gojo_string_push_byte(b, '[');\n    for (int64_t i = 0; i < p->len; i++) {{ if (i) gojo_string_push_byte(b, ','); {}(b, &(({} *)p->ptr)[i]); }}\n    gojo_string_push_byte(b, ']');", f, ec);
            }
            Ty::Array(el, n) => {
                let el = (**el).clone();
                let f = self.jenc_fn(&el);
                let _ = writeln!(b, "    gojo_string_push_byte(b, '[');\n    for (int64_t i = 0; i < {}; i++) {{ if (i) gojo_string_push_byte(b, ','); {}(b, &p->a[i]); }}\n    gojo_string_push_byte(b, ']');", n, f);
            }
            Ty::Map(k, v) => {
                let (k, v) = ((**k).clone(), (**v).clone());
                let (ent, _) = self.map_ent(&k, &v);
                let fk = self.jkeyenc_fn(&k);
                let fv = self.jenc_fn(&v);
                let _ = writeln!(b, "    gojo_string_push_byte(b, '{{');\n    int first = 1;\n    for (int64_t i = 0; i < p->n_entries; i++) {{ {e} *e = (({e} *)p->entries) + i; if (!e->live) continue; if (!first) gojo_string_push_byte(b, ','); first = 0; {fk}(b, &e->k); gojo_string_push_byte(b, ':'); {fv}(b, &e->v); }}\n    gojo_string_push_byte(b, '}}');", e = ent, fk = fk, fv = fv);
            }
            Ty::Option(inner) => {
                let inner = (**inner).clone();
                let f = self.jenc_fn(&inner);
                let _ = writeln!(b, "    if ({}) {}(b, &{}); else gojo_fmt_cstr(b, \"null\");", self.osome(t, "(*p)"), f, self.oval(t, "(*p)"));
            }
            Ty::Box(inner) => {
                let inner = (**inner).clone();
                let f = self.jenc_fn(&inner);
                let _ = writeln!(b, "    {}(b, *p);", f);
            }
            Ty::Rc(inner) | Ty::Arc(inner) => {
                let inner = (**inner).clone();
                self.ensure_rcbox(&inner);
                let f = self.jenc_fn(&inner);
                let _ = writeln!(b, "    {}(b, &(*p)->val);", f);
            }
            Ty::Cell(inner) => {
                let inner = (**inner).clone();
                let f = self.jenc_fn(&inner);
                let _ = writeln!(b, "    {}(b, p);", f);
            }
            Ty::Adt(d, args) if self.is_json_value(t) => {
                let _ = (d, args);
                let (f, _) = self.printing_method(t, "encode").expect("json.Value.encode");
                let _ = writeln!(b, "    gojo_string s = {}(p);\n    gojo_string_push(b, gojo_string_view(&s));\n    free(s.ptr);", f);
            }
            Ty::Adt(d, args) => {
                let def = self.prog.def(*d).clone();
                match &def.kind {
                    TypeDefKind::Struct { .. } => {
                        let fsx = self.prog.field_tys(*d, args);
                        if def.newtype {
                            let f = self.jenc_fn(&fsx[0].1);
                            let _ = writeln!(b, "    {}(b, &p->f_{});", f, fsx[0].0);
                        } else if Some(*d) == self.prog.prelude_type("Set") && fsx.len() == 1 && matches!(fsx[0].1, Ty::Map(..)) {
                            let Ty::Map(k, v) = fsx[0].1.clone() else { unreachable!() };
                            let (ent, _) = self.map_ent(&k, &v);
                            let fk = self.jenc_fn(&k);
                            let _ = writeln!(b, "    gojo_string_push_byte(b, '[');\n    int first = 1;\n    for (int64_t i = 0; i < p->f_{f}.n_entries; i++) {{ {e} *e = (({e} *)p->f_{f}.entries) + i; if (!e->live) continue; if (!first) gojo_string_push_byte(b, ','); first = 0; {fk}(b, &e->k); }}\n    gojo_string_push_byte(b, ']');", f = fsx[0].0, e = ent, fk = fk);
                        } else if def.tuple {
                            b.push_str("    gojo_string_push_byte(b, '[');\n");
                            for (i, (fname, ft)) in fsx.iter().enumerate() {
                                let f = self.jenc_fn(ft);
                                let _ = writeln!(b, "    {}{}(b, &p->f_{});", if i > 0 { "gojo_string_push_byte(b, ','); " } else { "" }, f, fname);
                            }
                            b.push_str("    gojo_string_push_byte(b, ']');\n");
                        } else {
                            b.push_str("    gojo_string_push_byte(b, '{');\n");
                            for (i, (fname, ft)) in fsx.iter().enumerate() {
                                self.json_ctx.push(format!("field `{}` of `{}`", fname, def.name));
                                let f = self.jenc_fn(ft);
                                self.json_ctx.pop();
                                let key = format!("{}\"{}\":", if i > 0 { "," } else { "" }, fname);
                                let _ = writeln!(b, "    gojo_fmt_cstr(b, {}); {}(b, &p->f_{});", c_string(&key), f, fname);
                            }
                            b.push_str("    gojo_string_push_byte(b, '}');\n");
                        }
                    }
                    TypeDefKind::Enum { variants } => {
                        b.push_str("    switch (p->tag) {\n");
                        for (vi, v) in variants.iter().enumerate() {
                            let fsx = self.prog.variant_tys(*d, args, vi);
                            if fsx.is_empty() {
                                let _ = writeln!(b, "    case {}: gojo_fmt_cstr(b, {}); break;", vi, c_string(&format!("\"{}\"", v.name)));
                                continue;
                            }
                            let _ = write!(b, "    case {}: gojo_fmt_cstr(b, {});", vi, c_string(&format!("{{\"{}\":{{", v.name)));
                            for (i, (fname, ft)) in fsx.iter().enumerate() {
                                self.json_ctx.push(format!("field `{}` of `{}.{}`", fname, def.name, v.name));
                                let f = self.jenc_fn(ft);
                                self.json_ctx.pop();
                                let key = format!("{}\"{}\":", if i > 0 { "," } else { "" }, fname);
                                let _ = write!(b, " gojo_fmt_cstr(b, {}); {}(b, &p->u.v{}.f_{});", c_string(&key), f, vi, fname);
                            }
                            b.push_str(" gojo_fmt_cstr(b, \"}}\"); break;\n");
                        }
                        b.push_str("    default: break;\n    }\n");
                    }
                    _ => self.json_bad(t, "it has no JSON form"),
                }
            }
            _ => {
                let why = self.json_unsupported(t);
                self.json_bad(t, why);
            }
        }
        b.push_str("}\n");
        self.bodies.push_str(&b);
        name
    }

    /// The types a map key can be: written as a JSON string.
    fn json_key_ok(&self, k: &Ty) -> bool {
        match k {
            Ty::String | Ty::Str | Ty::Int(_) => true,
            Ty::Adt(d, args) => {
                let def = self.prog.def(*d);
                if def.newtype {
                    return self.json_key_ok(&self.prog.field_tys(*d, args)[0].1);
                }
                def.is_simple_enum()
            }
            _ => false,
        }
    }

    /// `static void jkey_M(gojo_string *b, K *p)`: a map key as a JSON string.
    fn jkeyenc_fn(&mut self, k: &Ty) -> String {
        if !self.json_key_ok(k) {
            self.json_bad(k, "a map key must be a string, an integer or an enum without data");
            return "gojo_runaway".into();
        }
        let name = format!("jkey_{}", self.mangle(k));
        let c = self.cty(k);
        let sig = format!("static void {}(gojo_string *b, {} *p)", name, c);
        if !self.helper_header(&name, &sig) {
            return name;
        }
        let mut b = format!("{} {{\n", sig);
        match k {
            Ty::Int(_) => {
                let f = self.jenc_fn(k);
                let _ = writeln!(b, "    gojo_string_push_byte(b, '\"'); {}(b, p); gojo_string_push_byte(b, '\"');", f);
            }
            Ty::Adt(d, args) if self.prog.def(*d).newtype => {
                let (fname, ft) = self.prog.field_tys(*d, args)[0].clone();
                let f = self.jkeyenc_fn(&ft);
                let _ = writeln!(b, "    {}(b, &p->f_{});", f, fname);
            }
            _ => {
                let f = self.jenc_fn(k);
                let _ = writeln!(b, "    {}(b, p);", f);
            }
        }
        b.push_str("}\n");
        self.bodies.push_str(&b);
        name
    }

    /// `static bool jkd_M(gojo_jdec *d, gojo_str s, K *out)`: a map key from
    /// the text of a JSON key.
    fn jkeydec_fn(&mut self, k: &Ty) -> String {
        if !self.json_key_ok(k) || *k == Ty::Str {
            self.json_bad(k, if *k == Ty::Str { "a `str` is a view of other memory: decode into a `String`" } else { "a map key must be a string, an integer or an enum without data" });
            return "gojo_runaway".into();
        }
        let name = format!("jkd_{}", self.mangle(k));
        let c = self.cty(k);
        let sig = format!("static bool {}(gojo_jdec *d, gojo_str s, {} *out)", name, c);
        if !self.helper_header(&name, &sig) {
            return name;
        }
        let mut b = format!("{} {{\n", sig);
        match k {
            Ty::String => b.push_str("    (void)d; *out = gojo_string_from(s);\n    return true;\n"),
            Ty::Int(_) => {
                // the key's text, read as a JSON number
                let f = self.jdec_scalar(k);
                let _ = writeln!(b, "    gojo_jdec kd; gojo_jdec_init(&kd, s);\n    bool ok = {}(&kd, out) && kd.pos == kd.len;\n    gojo_jdec_free(&kd);\n    if (!ok) return gojo_jdec_fail_str(d, \"the key \\\"\", s, {});\n    return true;", f, c_string(&format!("\" isn't a valid `{}`", self.prog.show(k))));
            }
            Ty::Adt(d, args) if self.prog.def(*d).newtype => {
                let (fname, ft) = self.prog.field_tys(*d, args)[0].clone();
                let f = self.jkeydec_fn(&ft);
                let _ = writeln!(b, "    return {}(d, s, &out->f_{});", f, fname);
            }
            Ty::Adt(d, _) => {
                let def = self.prog.def(*d).clone();
                for (vi, v) in def.variants().iter().enumerate() {
                    let _ = writeln!(b, "    if (s.len == {} && memcmp(s.ptr, {}, {}) == 0) {{ memset(out, 0, sizeof *out); out->tag = {}; return true; }}", v.name.len(), c_string(&v.name), v.name.len(), vi);
                }
                let names: Vec<&str> = def.variants().iter().map(|v| v.name.as_str()).collect();
                let _ = writeln!(b, "    return gojo_jdec_fail_str(d, \"unknown key \\\"\", s, {});", c_string(&format!("\" (a `{}`: one of {})", def.name, names.join(", "))));
            }
            _ => unreachable!(),
        }
        b.push_str("}\n");
        self.bodies.push_str(&b);
        name
    }

    /// The decoder of a number or bool type (no allocation, no tf needed).
    fn jdec_scalar(&mut self, t: &Ty) -> String {
        let name = format!("jdec_{}", self.mangle(t));
        let c = self.cty(t);
        let sig = format!("static bool {}(gojo_jdec *d, {} *out)", name, c);
        if !self.helper_header(&name, &sig) {
            return name;
        }
        let mut b = format!("{} {{\n", sig);
        match t {
            Ty::Bool => b.push_str("    return gojo_jdec_bool(d, out);\n"),
            Ty::Int(k) => {
                let bits = k.bits();
                let ty = c_string(&self.prog.show(t));
                if k.signed() {
                    let _ = writeln!(b, "    int64_t v;\n    if (!gojo_jdec_i64(d, &v, INT{bits}_MIN, INT{bits}_MAX, {ty})) return false;\n    *out = ({c})v;\n    return true;");
                } else {
                    let _ = writeln!(b, "    uint64_t v;\n    if (!gojo_jdec_u64(d, &v, UINT{bits}_MAX, {ty})) return false;\n    *out = ({c})v;\n    return true;");
                }
            }
            Ty::F64 => b.push_str("    return gojo_jdec_f64(d, out);\n"),
            Ty::F32 => b.push_str("    return gojo_jdec_f32(d, out);\n"),
            _ => unreachable!(),
        }
        b.push_str("}\n");
        self.bodies.push_str(&b);
        name
    }

    /// `static bool jdec_M(gojo_jdec *d, T *out)`: reads a `t`; on success
    /// `*out` holds it, on failure there is nothing to drop and `d` has the
    /// error. `tf` is any function, for generating field defaults.
    fn jdec_fn(&mut self, tf: &'a TFunc, t: &Ty) -> String {
        if self.runaway_type(t) {
            return "gojo_runaway".into();
        }
        if matches!(t, Ty::Bool | Ty::Int(_) | Ty::F32 | Ty::F64) {
            return self.jdec_scalar(t);
        }
        let m = self.mangle(t);
        let name = format!("jdec_{}", m);
        let c = self.cty(t);
        let sig = format!("static bool {}(gojo_jdec *d, {} *out)", name, c);
        if !self.helper_header(&name, &sig) {
            return name;
        }
        let mut b = format!("{} {{\n", sig);
        match t {
            Ty::Unit => b.push_str("    if (!gojo_jdec_null(d)) return gojo_jdec_expected(d, \"null\");\n    memset(out, 0, sizeof *out);\n    return true;\n"),
            Ty::String => b.push_str("    return gojo_jdec_str(d, out);\n"),
            Ty::Vec(el) => {
                let el = (**el).clone();
                let ec = self.cty(&el);
                let f = self.jdec_fn(tf, &el);
                let dv = self.drop_fn(t);
                let _ = writeln!(b, "    if (!gojo_jdec_open(d, '[')) return false;\n    gojo_vec v = {{0}};\n    for (int64_t i = 0;; i++) {{\n        int r = gojo_jdec_item(d, i);\n        if (r == 0) break;\n        if (r < 0) goto fail;\n        gojo_vec_reserve(&v, 1, sizeof({ec}));\n        if (!{f}(d, (({ec} *)v.ptr) + v.len)) {{ gojo_jdec_at_index(d, i); goto fail; }}\n        v.len++;\n    }}\n    *out = v;\n    return true;\nfail:\n    {dv}(&v);\n    return false;");
            }
            Ty::Array(el, n) => {
                let el = (**el).clone();
                let f = self.jdec_fn(tf, &el);
                let drop = if self.needs_drop(&el) { format!("for (int64_t k = 0; k < got; k++) {}(&a.a[k]);", self.drop_fn(&el)) } else { String::new() };
                let _ = writeln!(b, "    if (!gojo_jdec_open(d, '[')) return false;\n    {c} a;\n    int64_t got = 0;\n    for (int64_t i = 0;; i++) {{\n        int r = gojo_jdec_item(d, i);\n        if (r == 0) break;\n        if (r < 0) goto fail;\n        if (i >= {n}) {{ gojo_jdec_fail(d, {more}); goto fail; }}\n        if (!{f}(d, &a.a[i])) {{ gojo_jdec_at_index(d, i); goto fail; }}\n        got++;\n    }}\n    if (got < {n}) {{ gojo_jdec_fail(d, {fewer}); goto fail; }}\n    *out = a;\n    return true;\nfail:\n    {drop}\n    return false;", more = c_string(&format!("more than {} elements", n)), fewer = c_string(&format!("fewer than {} elements", n)));
            }
            Ty::Map(k, v) => {
                let (k, v) = ((**k).clone(), (**v).clone());
                let (ent, mm) = self.map_ent(&k, &v);
                let kc = self.cty(&k);
                let vc = self.cty(&v);
                let fk = self.jkeydec_fn(&k);
                let fv = self.jdec_fn(tf, &v);
                let dm = self.drop_fn(t);
                let dk = if self.needs_drop(&k) { format!("{}(&kv);", self.drop_fn(&k)) } else { String::new() };
                let dv = if self.needs_drop(&v) { format!("{}(&e->v);", self.drop_fn(&v)) } else { String::new() };
                // the key's text for the path of an error: a String key is
                // its own text; other keys keep a copy (the view is reused)
                let (keep, path, free) = if k == Ty::String {
                    (String::new(), "gojo_jdec_at_key(d, kv.ptr, kv.len);".to_string(), String::new())
                } else {
                    ("gojo_string kt = gojo_string_from(key);".to_string(), "gojo_jdec_at_key(d, kt.ptr, kt.len);".to_string(), "free(kt.ptr);".to_string())
                };
                let _ = writeln!(b, "    if (!gojo_jdec_open(d, '{{')) return false;\n    gojo_map m = {{0}};\n    for (int64_t i = 0;; i++) {{\n        gojo_str key;\n        int r = gojo_jdec_key(d, i, &key);\n        if (r == 0) break;\n        if (r < 0) goto fail;\n        {kc} kv;\n        if (!{fk}(d, key, &kv)) goto fail;\n        {keep}\n        {vc} vv;\n        if (!{fv}(d, &vv)) {{ {path} {dk} {free} goto fail; }}\n        {free}\n        bool ex;\n        {ent} *e = ms_{mm}(&m, &kv, &ex);\n        if (ex) {{ {dv} {dk} }} else e->k = kv;\n        e->v = vv;\n    }}\n    *out = m;\n    return true;\nfail:\n    {dm}(&m);\n    return false;");
            }
            Ty::Option(inner) => {
                let inner = (**inner).clone();
                let ic = self.cty(&inner);
                let f = self.jdec_fn(tf, &inner);
                let z = self.zero(t);
                let set = self.oset(t, "(*out)", "v");
                let _ = writeln!(b, "    if (gojo_jdec_null(d)) {{ *out = {z}; return true; }}\n    {ic} v;\n    if (!{f}(d, &v)) return false;\n    {set}\n    return true;");
            }
            Ty::Box(inner) => {
                let inner = (**inner).clone();
                let ic = self.cty(&inner);
                let f = self.jdec_fn(tf, &inner);
                let _ = writeln!(b, "    {ic} v;\n    if (!{f}(d, &v)) return false;\n    {ic} *bx = gojo_box_alloc(sizeof({ic}));\n    *bx = v;\n    *out = bx;\n    return true;");
            }
            Ty::Rc(inner) | Ty::Arc(inner) => {
                let inner = (**inner).clone();
                let ic = self.cty(&inner);
                let bx = self.ensure_rcbox(&inner);
                let f = self.jdec_fn(tf, &inner);
                let _ = writeln!(b, "    {ic} v;\n    if (!{f}(d, &v)) return false;\n    {bx} *r = gojo_box_alloc(sizeof({bx}));\n    r->strong = 1; r->weak = 0; r->val = v;\n    *out = r;\n    return true;");
            }
            Ty::Cell(inner) => {
                let inner = (**inner).clone();
                let f = self.jdec_fn(tf, &inner);
                let _ = writeln!(b, "    return {}(d, out);", f);
            }
            Ty::Adt(..) if self.is_json_value(t) => {
                // any JSON: checked by skipping it, then parsed by json.parse
                let Some(Sym::Func(pf)) = self.prog.packages.iter().find(|p| p.is_std && p.name == "json").and_then(|p| p.items.get("parse").cloned()) else { unreachable!() };
                let pr = self.prog.func(pf).ret.clone();
                let rc = self.cty(&pr);
                let f = self.instance(pf, vec![]);
                let de = match &pr {
                    Ty::Result(_, e) if self.needs_drop(e) => format!("{}(&r.u.err);", self.drop_fn(e)),
                    _ => String::new(),
                };
                let _ = writeln!(b, "    if (gojo_jdec_peek(d) < 0) return gojo_jdec_expected(d, \"a value\");\n    int64_t lo = d->pos;\n    if (!gojo_jdec_skip(d)) return false;\n    gojo_str s = {{ d->s + lo, d->pos - lo }};\n    {rc} r = {f}(s);\n    if (!r.ok) {{ {de} d->pos = lo; return gojo_jdec_fail(d, \"invalid JSON\"); }}\n    *out = r.u.val;\n    return true;");
            }
            Ty::Adt(d, args) => {
                let def = self.prog.def(*d).clone();
                match &def.kind {
                    TypeDefKind::Struct { .. } => {
                        let fsx = self.prog.field_tys(*d, args);
                        if def.newtype {
                            let f = self.jdec_fn(tf, &fsx[0].1);
                            let _ = writeln!(b, "    return {}(d, &out->f_{});", f, fsx[0].0);
                        } else if Some(*d) == self.prog.prelude_type("Set") && fsx.len() == 1 && matches!(fsx[0].1, Ty::Map(..)) {
                            let Ty::Map(k, v) = fsx[0].1.clone() else { unreachable!() };
                            let (ent, mm) = self.map_ent(&k, &v);
                            let kc = self.cty(&k);
                            let fk = self.jdec_fn(tf, &k);
                            let dm = self.drop_fn(&fsx[0].1);
                            let dk = if self.needs_drop(&k) { format!("{}(&kv);", self.drop_fn(&k)) } else { String::new() };
                            let _ = writeln!(b, "    if (!gojo_jdec_open(d, '[')) return false;\n    gojo_map m = {{0}};\n    for (int64_t i = 0;; i++) {{\n        int r = gojo_jdec_item(d, i);\n        if (r == 0) break;\n        if (r < 0) goto fail;\n        {kc} kv;\n        if (!{fk}(d, &kv)) {{ gojo_jdec_at_index(d, i); goto fail; }}\n        bool ex;\n        {ent} *e = ms_{mm}(&m, &kv, &ex);\n        if (ex) {{ {dk} }} else {{ e->k = kv; e->v = true; }}\n    }}\n    out->f_{f} = m;\n    return true;\nfail:\n    {dm}(&m);\n    return false;", f = fsx[0].0);
                        } else if def.tuple {
                            let n = fsx.len();
                            b.push_str("    if (!gojo_jdec_open(d, '[')) return false;\n    int64_t got = 0;\n    int r;\n");
                            for (i, (fname, ft)) in fsx.iter().enumerate() {
                                let fc = self.cty(ft);
                                let f = self.jdec_fn(tf, ft);
                                let _ = writeln!(b, "    {fc} v{i};\n    r = gojo_jdec_item(d, {i});\n    if (r < 0) goto fail;\n    if (r == 0) {{ gojo_jdec_fail(d, {few}); goto fail; }}\n    if (!{f}(d, &v{i})) {{ gojo_jdec_at_index(d, {i}); goto fail; }}\n    got++;\n    out->f_{fname} = v{i};", few = c_string(&format!("expected {} elements, found {}", n, i)));
                            }
                            let _ = writeln!(b, "    r = gojo_jdec_item(d, {n});\n    if (r < 0) goto fail;\n    if (r > 0) {{ gojo_jdec_fail(d, {more}); goto fail; }}\n    return true;\nfail:", more = c_string(&format!("more than {} elements", n)));
                            for (i, (fname, ft)) in fsx.iter().enumerate() {
                                if self.needs_drop(ft) {
                                    let df = self.drop_fn(ft);
                                    let _ = writeln!(b, "    if (got > {}) {}(&out->f_{});", i, df, fname);
                                }
                            }
                            b.push_str("    return false;\n");
                        } else {
                            let fields: Vec<(String, Ty, Option<usize>)> = fsx.iter().enumerate().map(|(i, (n, t))| (n.clone(), t.clone(), def.fields()[i].default.as_ref().map(|_| i))).collect();
                            let targets: Vec<String> = fsx.iter().map(|(n, _)| format!("out->f_{}", n)).collect();
                            self.jdec_object(tf, &mut b, &def.name, *d, args, &fields, &targets, "");
                        }
                    }
                    TypeDefKind::Enum { variants } => {
                        let names: Vec<String> = variants.iter().map(|v| v.name.clone()).collect();
                        let simple = def.is_simple_enum();
                        let what = c_string(&format!("{} (a `{}`: one of {})", if simple { "a string" } else { "a string or an object" }, def.name, names.join(", ")));
                        let unknown = c_string(&format!("\" (a `{}`: one of {})", def.name, names.join(", ")));
                        let _ = writeln!(b, "    int c = gojo_jdec_peek(d);\n    gojo_str s;\n    if (c == '\"') {{\n        if (!gojo_jdec_strview(d, &s)) return false;");
                        for (vi, v) in variants.iter().enumerate() {
                            let m = format!("s.len == {} && memcmp(s.ptr, {}, {}) == 0", v.name.len(), c_string(&v.name), v.name.len());
                            if v.fields.is_empty() {
                                let _ = writeln!(b, "        if ({}) {{ memset(out, 0, sizeof *out); out->tag = {}; return true; }}", m, vi);
                            } else {
                                let _ = writeln!(b, "        if ({}) return gojo_jdec_fail(d, {});", m, c_string(&format!("the variant `{}` of `{}` has fields: write {{\"{}\": {{...}}}}", v.name, def.name, v.name)));
                            }
                        }
                        let _ = writeln!(b, "        return gojo_jdec_fail_str(d, \"unknown variant \\\"\", s, {unknown});\n    }}");
                        if !simple {
                            let _ = writeln!(b, "    if (c == '{{') {{\n        if (!gojo_jdec_open(d, '{{')) return false;\n        int r = gojo_jdec_key(d, 0, &s);\n        if (r < 0) return false;\n        if (r == 0) return gojo_jdec_fail(d, {empty});\n        bool ok;", empty = c_string(&format!("an empty object isn't a `{}`", def.name)));
                            for (vi, v) in variants.iter().enumerate() {
                                let f = self.jdec_variant(tf, &def, *d, args, vi);
                                let null = if v.fields.is_empty() { format!("if (gojo_jdec_null(d)) {{ memset(out, 0, sizeof *out); out->tag = {}; ok = true; }} else ", vi) } else { String::new() };
                                let _ = writeln!(b, "        if (s.len == {n} && memcmp(s.ptr, {q}, {n}) == 0) {{ {null}ok = {f}(d, out); if (!ok) {{ gojo_jdec_at_key(d, (const uint8_t *){q}, {n}); return false; }} }} else", n = v.name.len(), q = c_string(&v.name));
                            }
                            let dr = if self.needs_drop(t) { format!("{}(out);", self.drop_fn(t)) } else { String::new() };
                            let _ = writeln!(b, "        return gojo_jdec_fail_str(d, \"unknown variant \\\"\", s, {unknown});\n        r = gojo_jdec_key(d, 1, &s);\n        if (r != 0) {{ if (r > 0) gojo_jdec_fail(d, {one}); {dr} return false; }}\n        return true;\n    }}", one = c_string(&format!("an object holding a `{}` has a single key, the variant", def.name)));
                        }
                        let _ = writeln!(b, "    return gojo_jdec_expected(d, {});", what);
                    }
                    _ => self.json_bad(t, "it has no JSON form"),
                }
            }
            Ty::Str | Ty::Slice(_) => self.json_bad(t, "a view of other memory can't be decoded: use `String` or `Vec`"),
            _ => {
                let why = self.json_unsupported(t);
                self.json_bad(t, why);
            }
        }
        b.push_str("}\n");
        self.bodies.push_str(&b);
        name
    }

    /// `static bool jdecv_M_i(gojo_jdec *d, T *out)`: the object of fields of
    /// variant `vi`, into `*out`.
    fn jdec_variant(&mut self, tf: &'a TFunc, def: &TypeDef, d: DefId, args: &std::rc::Rc<Vec<Ty>>, vi: usize) -> String {
        let t = Ty::Adt(d, args.clone());
        let name = format!("jdecv_{}_{}", self.mangle(&t), vi);
        let c = self.cty(&t);
        let sig = format!("static bool {}(gojo_jdec *d, {} *out)", name, c);
        if !self.helper_header(&name, &sig) {
            return name;
        }
        let fsx = self.prog.variant_tys(d, args, vi);
        let fields: Vec<(String, Ty, Option<usize>)> = fsx.iter().map(|(n, t)| (n.clone(), t.clone(), None)).collect();
        let targets: Vec<String> = fsx.iter().map(|(n, _)| format!("out->u.v{}.f_{}", vi, n)).collect();
        let mut b = format!("{} {{\n", sig);
        let owner = format!("{}.{}", def.name, def.variants()[vi].name);
        self.jdec_object(tf, &mut b, &owner, d, args, &fields, &targets, &format!("memset(out, 0, sizeof *out); out->tag = {};", vi));
        b.push_str("}\n");
        self.bodies.push_str(&b);
        name
    }

    /// The body of a decoder of an object whose keys are `fields`: each is
    /// read into a local, and on success stored to its target. A field left
    /// out takes its default (`Some(i)`: field `i` of struct `d` has one),
    /// or none for an optional field; any other is an error. Unknown keys
    /// are skipped, and a repeated key keeps its last value.
    #[allow(clippy::too_many_arguments)]
    fn jdec_object(&mut self, tf: &'a TFunc, b: &mut String, owner: &str, d: DefId, args: &std::rc::Rc<Vec<Ty>>, fields: &[(String, Ty, Option<usize>)], targets: &[String], init: &str) {
        b.push_str("    if (!gojo_jdec_open(d, '{')) return false;\n");
        let mut decs = vec![];
        let mut drops = vec![];
        for (i, (fname, ft, _)) in fields.iter().enumerate() {
            let fc = self.cty(ft);
            let _ = writeln!(b, "    {} v{}; bool h{} = false;", fc, i, i);
            self.json_ctx.push(format!("field `{}` of `{}`", fname, owner));
            decs.push(self.jdec_fn(tf, ft));
            self.json_ctx.pop();
            drops.push(if self.needs_drop(ft) { Some(self.drop_fn(ft)) } else { None });
        }
        b.push_str("    for (int64_t i = 0;; i++) {\n        gojo_str key;\n        int r = gojo_jdec_key(d, i, &key);\n        if (r == 0) break;\n        if (r < 0) goto fail;\n");
        for (i, (fname, _, _)) in fields.iter().enumerate() {
            let again = match &drops[i] {
                Some(df) => format!("if (h{i}) {{ {df}(&v{i}); h{i} = false; }} "),
                None => String::new(),
            };
            let _ = writeln!(b, "        if (key.len == {n} && memcmp(key.ptr, {q}, {n}) == 0) {{ {again}if (!{f}(d, &v{i})) {{ gojo_jdec_at_key(d, (const uint8_t *){q}, {n}); goto fail; }} h{i} = true; continue; }}", n = fname.len(), q = c_string(fname), f = decs[i]);
        }
        b.push_str("        if (!gojo_jdec_skip(d)) goto fail;\n    }\n");
        for (i, (fname, ft, dflt)) in fields.iter().enumerate() {
            let fill = if let Some(fi) = dflt {
                let f = self.jdflt_fn(tf, d, args, *fi);
                format!("v{} = {}();", i, f)
            } else if matches!(ft, Ty::Option(_)) {
                format!("v{} = {};", i, self.zero(ft))
            } else {
                format!("{{ gojo_jdec_fail(d, {}); goto fail; }}", c_string(&format!("missing field `{}` of `{}`", fname, owner)))
            };
            let _ = writeln!(b, "    if (!h{i}) {fill}");
        }
        if !init.is_empty() {
            let _ = writeln!(b, "    {}", init);
        }
        for (i, tg) in targets.iter().enumerate() {
            let _ = writeln!(b, "    {} = v{};", tg, i);
        }
        b.push_str("    return true;\nfail:\n");
        for (i, df) in drops.iter().enumerate() {
            if let Some(df) = df {
                let _ = writeln!(b, "    if (h{i}) {df}(&v{i});");
            }
        }
        b.push_str("    return false;\n");
    }

    /// `static T dflt_M_i(void)`: the default value of field `i` of struct
    /// `d`, its expression generated in a function of its own.
    fn jdflt_fn(&mut self, tf: &'a TFunc, d: DefId, args: &std::rc::Rc<Vec<Ty>>, i: usize) -> String {
        let st = Ty::Adt(d, args.clone());
        let name = format!("dflt_{}_{}", self.mangle(&st), i);
        let ft = self.prog.field_tys(d, args)[i].1.clone();
        let c = self.cty(&ft);
        let sig = format!("static {} {}(void)", c, name);
        if !self.helper_header(&name, &sig) {
            return name;
        }
        let mut nfs = Fs { tf, subst: HashMap::new(), out: String::new(), decls: String::new(), ind: 1, scopes: vec![], repr: HashMap::new(), flagged: HashSet::new(), flags: HashMap::new(), ret: ft.clone(), yields: false, err_mode: None, loop_labels: vec![], lb: None, line: (u32::MAX, 0), tscopes: HashMap::new(), bce: vec![], bce_steady: HashMap::new(), bce_floor: Default::default(), dinfo: None, site: None, used_flags: HashSet::new(), jd_done: HashSet::new(), fd_done: HashSet::new(), static_cur: HashMap::new() };
        nfs.scopes.push(Scope { kind: SK::Func, items: vec![], wait: None });
        self.push_scope(&mut nfs, SK::Block);
        let v = self.field_default(&mut nfs, d, args.as_slice(), i);
        let r = self.tmp();
        self.emit(&mut nfs, &format!("{} {} = {};", c, r, v));
        self.cleanup_to(&mut nfs, 0);
        self.emit(&mut nfs, &format!("return {};", r));
        let nout = reuse_boxes(&resolve_flags(&nfs.out, &nfs.used_flags));
        let _ = writeln!(self.bodies, "{} {{\n{}{}}}\n{}", sig, nfs.decls, nout, C_LINE_MARK);
        name
    }
}

struct IfaceMethodSub {
    methods: Vec<(String, Vec<(String, Mode, Ty)>, Ty)>,
}

impl IfaceMethodSub {
    fn all(ms: Vec<(String, Vec<(String, Mode, Ty)>, Ty)>) -> IfaceMethodSub {
        IfaceMethodSub { methods: ms }
    }
}

// accessor used by call_iface: the method's params
impl IfaceMethodSub {
    fn method(&self, i: usize) -> &(String, Vec<(String, Mode, Ty)>, Ty) {
        &self.methods[i]
    }
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect()
}

fn int_suffix(k: IntTy) -> &'static str {
    match k {
        IntTy::I8 => "i8",
        IntTy::I16 => "i16",
        IntTy::I32 => "i32",
        IntTy::I64 => "i64",
        IntTy::U8 => "u8",
        IntTy::U16 => "u16",
        IntTy::U32 => "u32",
        IntTy::U64 => "u64",
    }
}

fn int_lit(v: i128, t: &Ty) -> String {
    match t {
        Ty::Int(IntTy::U64) => format!("UINT64_C({})", v as u64),
        Ty::Int(IntTy::I64) | Ty::Int(_) => {
            if v == i64::MIN as i128 {
                "INT64_MIN".into()
            } else if let Ty::Int(k) = t {
                if *k == IntTy::I64 {
                    format!("INT64_C({})", v)
                } else {
                    format!("(({}){})", k.c_name(), v)
                }
            } else {
                v.to_string()
            }
        }
        Ty::F32 | Ty::F64 => format!("{}.0", v),
        _ => v.to_string(),
    }
}

fn cmp_op(op: BinOp) -> &'static str {
    match op {
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        _ => "?",
    }
}

fn is_simple_c(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || s.starts_with("gojo_str_lit(")
}

/// Root local and field path of a place (only through struct fields).
fn local_path(e: &TExpr) -> Option<(LocalId, Vec<usize>)> {
    match &e.kind {
        TK::Local(l) => Some((*l, vec![])),
        TK::Field { base, index } => {
            let (l, mut p) = local_path(base)?;
            p.push(*index);
            Some((l, p))
        }
        _ => None,
    }
}

/// Drop flags are declared and set on lines starting with this mark; once a
/// function is generated, `resolve_flags` keeps the lines of the flags some
/// drop tests and removes the others.
fn flag_mark(flag: &str) -> String {
    format!("/*?{}*/", flag)
}

/// Reuse on last-use frees (the static half of Perceus): a box emptied by a
/// move out of it (`*b`, `GOJO_BOX_FREE_R`) and a box of the same type made
/// later in the same C block (`GOJO_BOX_NEW_R`) are one block, when nothing
/// in between can leave or enter the block: no `return`, `goto`, `break`,
/// `continue`, label or `case`, and no `}` closing it. Then every path from
/// the free reaches the allocation, so the free can be dropped and the old
/// block written instead. A match arm that rebuilds a node (`Add(l, r) =>
/// .Add(Box(f(*l)), Box(f(*r)))`) allocates nothing.
fn reuse_boxes(out: &str) -> String {
    if !out.contains("GOJO_BOX_FREE_R(") || !out.contains("GOJO_BOX_NEW_R(") {
        return out.to_string();
    }
    let mut lines: Vec<String> = out.split_inclusive('\n').map(|l| l.to_string()).collect();
    // braces outside string and character literals: (opened, closed below 0)
    fn braces(line: &str, depth: &mut i64) -> bool {
        let b = line.as_bytes();
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                q @ (b'"' | b'\'') => {
                    i += 1;
                    while i < b.len() && b[i] != q {
                        if b[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                b'/' if b.get(i + 1) == Some(&b'*') => {
                    i += 2;
                    while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                        i += 1;
                    }
                    i += 1;
                }
                b'{' => *depth += 1,
                b'}' => {
                    *depth -= 1;
                    if *depth < 0 {
                        return false;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        true
    }
    fn word(line: &str, w: &str) -> bool {
        let ident = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        line.match_indices(w).any(|(i, _)| !ident(line[..i].chars().next_back()) && !ident(line[i + w.len()..].chars().next()))
    }
    fn jumps(line: &str) -> bool {
        let t = line.trim_start();
        if t.starts_with('#') {
            return false;
        }
        // a label (`L4:;`, `done:`) can be jumped to from elsewhere
        let label = t.split_once(':').is_some_and(|(l, rest)| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !rest.starts_with(':'));
        label || word(t, "return") || word(t, "goto") || word(t, "break") || word(t, "continue") || word(t, "case") || word(t, "default") || word(t, "longjmp")
    }
    for i in 0..lines.len() {
        let Some(at) = lines[i].find("GOJO_BOX_FREE_R(") else { continue };
        let rest = &lines[i][at + "GOJO_BOX_FREE_R(".len()..];
        let Some(close) = rest.find(");") else { continue };
        let Some((ptr, ty)) = rest[..close].split_once(", ") else { continue };
        let (ptr, ty) = (ptr.to_string(), ty.to_string());
        let call = format!("GOJO_BOX_FREE_R({}, {});", ptr, ty);
        let want = format!("{} *", ty);
        let new = format!("GOJO_BOX_NEW_R({})", ty);
        let mut depth = 0i64;
        for j in i + 1..lines.len() {
            let l = &lines[j];
            if depth == 0 && l.trim_start().starts_with(&want) && l.contains(&new) {
                lines[j] = l.replacen(&new, &ptr, 1);
                lines[i] = lines[i].replacen(&call, "/* reused below */", 1);
                break;
            }
            if jumps(l) || !braces(l, &mut depth) {
                break;
            }
        }
    }
    lines.concat()
}

fn resolve_flags(out: &str, used: &HashSet<String>) -> String {
    let mut r = String::with_capacity(out.len());
    for line in out.split_inclusive('\n') {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("/*?") {
            if let Some(end) = rest.find("*/") {
                if !used.contains(&rest[..end]) {
                    continue;
                }
                r.push_str(&line[..line.len() - t.len()]);
                r.push_str(&rest[end + 2..]);
                continue;
            }
        }
        r.push_str(line);
    }
    r
}

/// Whether evaluating `e` may leave the enclosing code early: a `try`, or a
/// `return`, `break` or `continue` in a block inside it. A value computed
/// before it must not leak then.
fn may_leave(e: &TExpr) -> bool {
    let mut found = false;
    walk_expr(e, &mut |n| match n {
        Node::Expr(x) => {
            if matches!(x.kind, TK::Try { .. }) {
                found = true;
            }
        }
        Node::Stmt(st) => {
            if matches!(st, TStmt::Return(..) | TStmt::Break(..) | TStmt::Continue(..)) {
                found = true;
            }
        }
    });
    found
}

fn has_effects(e: &TExpr) -> bool {
    match &e.kind {
        TK::Unit | TK::Int(_) | TK::Float(_) | TK::Bool(_) | TK::Str(_) | TK::NoneLit | TK::Local(_) | TK::Const(_) | TK::FuncRef(..) | TK::DefaultOf => false,
        TK::Field { base, .. } | TK::Deref(base) | TK::Copy(base) | TK::Move(base) => has_effects(base),
        TK::Coerce(_, x) | TK::SomeLit(x) | TK::OkLit(x) | TK::ErrLit(x) | TK::Cast(x) | TK::Unary { e: x, .. } => has_effects(x),
        TK::Binary { l, r, .. } => has_effects(l) || has_effects(r),
        _ => true,
    }
}

// ---------- move collection (which paths need drop flags) ----------

fn collect_moves_block(b: &TBlock, out: &mut HashSet<(LocalId, Vec<usize>)>, tf: &TFunc) {
    for s in &b.stmts {
        collect_moves_stmt(s, out, tf);
    }
    if let Some(v) = &b.value {
        collect_moves_expr(v, out, tf);
    }
}

fn collect_moves_stmt(s: &TStmt, out: &mut HashSet<(LocalId, Vec<usize>)>, tf: &TFunc) {
    match s {
        TStmt::Let { init, .. } => collect_moves_expr(init, out, tf),
        TStmt::LetRef { place, .. } => collect_moves_expr(place, out, tf),
        TStmt::Expr(e) => collect_moves_expr(e, out, tf),
        TStmt::Assign { place, value, .. } => {
            collect_moves_expr(place, out, tf);
            collect_moves_expr(value, out, tf)
        }
        TStmt::MapInsert { map, key, value } => {
            collect_moves_expr(map, out, tf);
            collect_moves_expr(key, out, tf);
            collect_moves_expr(value, out, tf)
        }
        TStmt::Return(Some(e), _) | TStmt::Yield(Some(e), _) => collect_moves_expr(e, out, tf),
        TStmt::Defer(b) | TStmt::ErrDefer(b) => collect_moves_block(b, out, tf),
        TStmt::While { cond, body, .. } => {
            if let Some(c) = cond {
                collect_moves_expr(c, out, tf)
            }
            collect_moves_block(body, out, tf)
        }
        TStmt::For { iter, hi, body, .. } => {
            collect_moves_expr(iter, out, tf);
            if let Some(h) = hi {
                collect_moves_expr(h, out, tf)
            }
            collect_moves_block(body, out, tf)
        }
        TStmt::ForEach { call, .. } => collect_moves_expr(call, out, tf),
        TStmt::WithLocks { places, body, .. } => {
            for p in places {
                collect_moves_expr(p, out, tf);
            }
            collect_moves_block(body, out, tf);
        }
        TStmt::ParFor { iter, hi, body, .. } => {
            collect_moves_expr(iter, out, tf);
            if let Some(h) = hi {
                collect_moves_expr(h, out, tf);
            }
            collect_moves_expr(body, out, tf);
        }
        TStmt::Send { chan, value } => {
            collect_moves_expr(chan, out, tf);
            collect_moves_expr(value, out, tf)
        }
        TStmt::With { place, body, .. } => {
            collect_moves_expr(place, out, tf);
            collect_moves_block(body, out, tf)
        }
        TStmt::Select { arms, .. } => {
            for a in arms {
                match &a.kind {
                    TSelectKind::Recv { chan, .. } => collect_moves_expr(chan, out, tf),
                    TSelectKind::Send { chan, value } => {
                        collect_moves_expr(chan, out, tf);
                        collect_moves_expr(value, out, tf)
                    }
                    TSelectKind::Task { task, .. } => {
                        if let Some(p) = local_path(task) {
                            out.insert(p);
                        }
                    }
                    TSelectKind::Timer { at, .. } => collect_moves_expr(at, out, tf),
                    _ => {}
                }
                if let Some(g) = &a.guard {
                    collect_moves_expr(g, out, tf);
                }
                collect_moves_block(&a.body, out, tf)
            }
        }
        _ => {}
    }
}

fn collect_moves_expr(e: &TExpr, out: &mut HashSet<(LocalId, Vec<usize>)>, tf: &TFunc) {
    let mut f = |x: &TExpr| collect_moves_expr(x, out, tf);
    match &e.kind {
        TK::Move(x) => {
            if let Some(p) = local_path(x) {
                out.insert(p);
            }
            collect_moves_expr(x, out, tf);
        }
        TK::Match { scrut, arms, owned } => {
            if *owned && arms.iter().any(|a| a.consuming) {
                if let Some(p) = local_path(scrut) {
                    out.insert(p);
                }
            }
            collect_moves_expr(scrut, out, tf);
            for a in arms {
                if let Some(g) = &a.guard {
                    collect_moves_expr(g, out, tf);
                }
                collect_moves_expr(&a.body, out, tf);
            }
        }
        TK::Closure(c) => {
            for (l, _) in &c.captures {
                // values moved into the closure (copies don't need a flag)
                if c.owned || (c.scoped && c.by_value.contains(l)) {
                    out.insert((*l, vec![]));
                }
            }
            collect_moves_block(&c.body, out, tf);
        }
        TK::Field { base, .. } | TK::Deref(base) | TK::OptField { base, .. } | TK::Copy(base) => f(base),
        TK::Index { base, index } | TK::StrIndex { base, index } => {
            f(base);
            f(index)
        }
        TK::MapIndex { base, key } => {
            f(base);
            f(key)
        }
        TK::SliceOf { base, lo, hi } => {
            f(base);
            if let Some(l) = lo {
                f(l)
            }
            if let Some(h) = hi {
                f(h)
            }
        }
        TK::Call { args, .. } | TK::Intrinsic { args, .. } => {
            for a in args {
                f(&a.e)
            }
        }
        TK::Proj { call, .. } => f(call),
        TK::ProjUnwrap { proj, msg } => {
            f(proj);
            if let Some(m) = msg {
                f(m)
            }
        }
        TK::Unary { e: x, .. } | TK::Cast(x) | TK::SomeLit(x) | TK::OkLit(x) | TK::ErrLit(x) | TK::Coerce(_, x) | TK::Recv(x) | TK::Wrap(x) | TK::MakeChan(x) | TK::Unbox(x) | TK::MutexRef(x) => f(x),
        TK::Binary { l, r, .. } | TK::Coalesce { l, r } => {
            f(l);
            f(r)
        }
        TK::StructLit { fields, .. } => fields.iter().for_each(|x| collect_moves_expr(x, out, tf)),
        TK::EnumLit { args, .. } => args.iter().for_each(|x| collect_moves_expr(x, out, tf)),
        TK::VecLit(xs) | TK::ArrayLit(xs) => xs.iter().for_each(|x| collect_moves_expr(x, out, tf)),
        TK::MapLit(ps) => ps.iter().for_each(|(k, v)| {
            collect_moves_expr(k, out, tf);
            collect_moves_expr(v, out, tf)
        }),
        TK::If { cond, then, else_ } => {
            collect_moves_expr(cond, out, tf);
            collect_moves_block(then, out, tf);
            if let Some(b) = else_ {
                collect_moves_block(b, out, tf)
            }
        }
        TK::Block(b) | TK::Unsafe(b) => collect_moves_block(b, out, tf),
        TK::Scope { body, .. } | TK::Isolate(body) => collect_moves_block(body, out, tf),
        TK::Spawn { target, task, .. } => {
            if let SpawnTarget::Value(v) = target {
                f(v)
            }
            f(task)
        }
        TK::Try { e: x, wrap } => {
            collect_moves_expr(x, out, tf);
            if let Some(TryWrap::Call(_, w)) = wrap {
                collect_moves_expr(w, out, tf)
            }
        }
        TK::Catch { e: x, handler, .. } => {
            collect_moves_expr(x, out, tf);
            collect_moves_block(handler, out, tf)
        }
        _ => {}
    }
}

#[allow(dead_code)]
fn yield_kind_unused(_: YieldKind) {}

/// Functions in a cycle of direct calls, or making indirect calls (a cycle may
/// go through a function value, an interface or a type parameter's method).
fn may_recurse(tfuncs: &[TFunc]) -> HashSet<FuncId> {
    let mut edges: HashMap<FuncId, Vec<FuncId>> = HashMap::new();
    let mut out = HashSet::new();
    for t in tfuncs {
        let mut callees = vec![];
        let mut indirect = false;
        crate::tir::visit_block(&t.body, &mut |e| {
            if let TK::Call { callee, .. } = &e.kind {
                match callee {
                    Callee::Func(f, _) => callees.push(*f),
                    _ => indirect = true,
                }
            }
        });
        if indirect {
            out.insert(t.func);
        }
        edges.insert(t.func, callees);
    }
    // Tarjan's strongly connected components, iteratively
    let mut index: HashMap<FuncId, usize> = HashMap::new();
    let mut low: HashMap<FuncId, usize> = HashMap::new();
    let mut on_stack: HashSet<FuncId> = HashSet::new();
    let mut stack: Vec<FuncId> = vec![];
    let mut next = 0;
    let empty = vec![];
    for t in tfuncs {
        if index.contains_key(&t.func) {
            continue;
        }
        let mut work: Vec<(FuncId, usize)> = vec![(t.func, 0)];
        index.insert(t.func, next);
        low.insert(t.func, next);
        next += 1;
        stack.push(t.func);
        on_stack.insert(t.func);
        while let Some(&mut (v, ref mut i)) = work.last_mut() {
            let succ = edges.get(&v).unwrap_or(&empty);
            if *i < succ.len() {
                let w = succ[*i];
                *i += 1;
                if w == v {
                    out.insert(v);
                }
                if !edges.contains_key(&w) {
                    continue;
                }
                if !index.contains_key(&w) {
                    index.insert(w, next);
                    low.insert(w, next);
                    next += 1;
                    stack.push(w);
                    on_stack.insert(w);
                    work.push((w, 0));
                } else if on_stack.contains(&w) {
                    let lw = index[&w];
                    let lv = low.get_mut(&v).unwrap();
                    *lv = (*lv).min(lw);
                }
            } else {
                work.pop();
                if let Some(&(p, _)) = work.last() {
                    let lv = low[&v];
                    let lp = low.get_mut(&p).unwrap();
                    *lp = (*lp).min(lv);
                }
                if low[&v] == index[&v] {
                    let mut comp = vec![];
                    loop {
                        let w = stack.pop().unwrap();
                        on_stack.remove(&w);
                        comp.push(w);
                        if w == v {
                            break;
                        }
                    }
                    if comp.len() > 1 {
                        out.extend(comp);
                    }
                }
            }
        }
    }
    out
}

/// What a function or loop body does, from one walk over it.
struct BodyFacts {
    /// a nested loop, a map insert, or a runtime operation (see `leaf_body`)
    heavy: bool,
    /// calls through a function value or interface
    indirect: bool,
    /// functions called directly
    callees: Vec<FuncId>,
}

fn body_facts(cond: Option<&TExpr>, body: &TBlock) -> BodyFacts {
    use crate::tir::Node;
    let mut f = BodyFacts { heavy: false, indirect: false, callees: vec![] };
    let mut check = |n: Node| match n {
        Node::Stmt(TStmt::While { .. } | TStmt::For { .. } | TStmt::ForEach { .. } | TStmt::ParFor { .. } | TStmt::MapInsert { .. }) => f.heavy = true,
        Node::Stmt(_) => {}
        Node::Expr(e) => match &e.kind {
            TK::Call { callee: Callee::Func(id, _), .. } => f.callees.push(*id),
            TK::Call { .. } => f.indirect = true,
            TK::Intrinsic { name, .. } if name != "len" => f.heavy = true,
            TK::MapIndex { .. } | TK::MapLit(_) | TK::VecLit(_) | TK::Closure(_) => f.heavy = true,
            _ => {}
        },
    };
    if let Some(c) = cond {
        crate::tir::walk_expr(c, &mut check);
    }
    crate::tir::walk_block(body, &mut check);
    f
}

/// Pure computation: no nested loops, and calls only to functions that finish in
/// bounded time. Runtime operations (maps, allocation, formatting) don't count:
/// next to one of those a poll costs nothing, and emitting such a body twice would
/// stop the C compiler from inlining functions that had a single call site.
fn leaf_body(bounded: &HashSet<FuncId>, cond: Option<&TExpr>, body: &TBlock) -> bool {
    let f = body_facts(cond, body);
    !f.heavy && !f.indirect && f.callees.iter().all(|c| bounded.contains(c))
}

/// Functions without loops that only call such functions: their running time
/// is bounded, so a loop calling them doesn't need to poll for preemption.
/// (Functions that may recurse are excluded, so the calls between the rest form
/// a DAG, processed bottom-up.)
fn bounded_funcs(tfuncs: &[TFunc], may_recurse: &HashSet<FuncId>) -> HashSet<FuncId> {
    let mut pending: HashMap<FuncId, usize> = HashMap::new();
    let mut callers: HashMap<FuncId, Vec<FuncId>> = HashMap::new();
    let mut ready: Vec<FuncId> = vec![];
    let candidates: HashSet<FuncId> = tfuncs.iter().map(|t| t.func).filter(|f| !may_recurse.contains(f)).collect();
    for t in tfuncs {
        if !candidates.contains(&t.func) {
            continue;
        }
        let mut f = body_facts(None, &t.body);
        f.callees.sort_unstable();
        f.callees.dedup();
        if f.heavy || f.indirect || f.callees.iter().any(|c| !candidates.contains(c)) {
            continue;
        }
        if f.callees.is_empty() {
            ready.push(t.func);
        }
        pending.insert(t.func, f.callees.len());
        for c in f.callees {
            callers.entry(c).or_default().push(t.func);
        }
    }
    let mut out = HashSet::new();
    while let Some(f) = ready.pop() {
        out.insert(f);
        for caller in callers.get(&f).into_iter().flatten() {
            if let Some(n) = pending.get_mut(caller) {
                *n -= 1;
                if *n == 0 {
                    ready.push(*caller);
                }
            }
        }
    }
    out
}
