// Random well-typed Gojo programs for differential testing.
//
// Every program is valid by construction (no unused variables, no shadowing, no
// moves of variables that are used again), terminates (loops have constant
// bounds, functions only call functions defined before them) and is
// deterministic. Most arithmetic wraps or is masked so that programs usually
// run to the end; some is checked and may panic, which must then happen the
// same way in every build.

use super::Rng;
use std::fmt::Write as _;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum T {
    I64,
    I32,
    U8,
    U64,
    Bool,
    Str,
    VecI,
    OptI,
    P,
    E,
    VecP,
    MapI,
    /// `int ! String`: only as a return type
    ResI,
}

const INTS: [T; 4] = [T::I64, T::I32, T::U8, T::U64];

impl T {
    fn name(self) -> &'static str {
        match self {
            T::I64 => "int",
            T::I32 => "i32",
            T::U8 => "u8",
            T::U64 => "u64",
            T::Bool => "bool",
            T::Str => "String",
            T::VecI => "Vec[int]",
            T::OptI => "?int",
            T::P => "P",
            T::E => "E",
            T::VecP => "Vec[P]",
            T::MapI => "Map[int, int]",
            T::ResI => "int ! String",
        }
    }
    fn is_int(self) -> bool {
        INTS.contains(&self)
    }
    fn bits(self) -> u32 {
        match self {
            T::I32 => 32,
            T::U8 => 8,
            _ => 64,
        }
    }
}

#[derive(Clone)]
struct Var {
    name: String,
    ty: T,
    mutable: bool,
    used: bool,
}

#[derive(Clone)]
struct Func {
    name: String,
    params: Vec<(T, Mode)>,
    ret: T,
    /// Has a loop or calls a function that does: not called inside loops,
    /// where costs would multiply down the call chain.
    heavy: bool,
    /// Prints, or calls a function that does: not called from tasks, whose
    /// output would come in an order that depends on the schedule.
    prints: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Borrow,
    Inout,
}

pub struct Gen<'a> {
    rng: &'a mut Rng,
    out: String,
    scopes: Vec<Vec<Var>>,
    funcs: Vec<Func>,
    names: u32,
    ind: usize,
    /// Vectors being iterated: they can't be modified in the loop body.
    iterating: Vec<String>,
    /// Loop nesting (bounds the work a program does).
    loops: u32,
    /// Statement budget for the current function.
    budget: i32,
    /// The current function has a loop or calls a heavy function.
    heavy: bool,
    /// Inside the argument of a conversion like `i32(...)`, which passes its
    /// type on to untyped literals.
    in_conv: u32,
    /// The current function returns `int ! String`: `try` is allowed.
    in_result: bool,
    /// Inside a closure or a `defer`, where `try` isn't allowed.
    no_try: u32,
}

impl<'a> Gen<'a> {
    pub fn new(rng: &'a mut Rng) -> Gen<'a> {
        Gen { rng, out: String::new(), scopes: vec![], funcs: vec![], names: 0, ind: 0, iterating: vec![], loops: 0, budget: 0, heavy: false, in_conv: 0, in_result: false, no_try: 0 }
    }

    fn fresh(&mut self, base: &str) -> String {
        self.names += 1;
        format!("{}{}", base, self.names)
    }

    fn line(&mut self, s: &str) {
        for _ in 0..self.ind {
            self.out.push_str("    ");
        }
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn chance(&mut self, n: u64, d: u64) -> bool {
        self.rng.chance(n, d)
    }

    fn below(&mut self, n: usize) -> usize {
        self.rng.below(n)
    }

    // ------------------------------------------------------------------
    // Variables

    fn declare(&mut self, name: &str, ty: T, mutable: bool) {
        self.scopes.last_mut().unwrap().push(Var { name: name.to_string(), ty, mutable, used: false });
    }

    fn vars(&self, pred: impl Fn(&Var) -> bool) -> Vec<(usize, usize)> {
        let mut out = vec![];
        for (i, s) in self.scopes.iter().enumerate() {
            for (j, v) in s.iter().enumerate() {
                if pred(v) {
                    out.push((i, j));
                }
            }
        }
        out
    }

    /// A variable of type `ty` (marked used), if there is one.
    fn pick_var(&mut self, ty: T, mutable: bool) -> Option<String> {
        let iterating = self.iterating.clone();
        let vs = self.vars(|v| v.ty == ty && (!mutable || (v.mutable && !iterating.contains(&v.name))));
        if vs.is_empty() {
            return None;
        }
        let (i, j) = vs[self.below(vs.len())];
        // writing to a variable doesn't use it
        if !mutable {
            self.scopes[i][j].used = true;
        }
        Some(self.scopes[i][j].name.clone())
    }

    fn used_flags(&self) -> Vec<Vec<bool>> {
        self.scopes.iter().map(|s| s.iter().map(|v| v.used).collect()).collect()
    }

    fn restore_used(&mut self, saved: Vec<Vec<bool>>) {
        for (s, fl) in self.scopes.iter_mut().zip(saved) {
            for (v, u) in s.iter_mut().zip(fl) {
                v.used = u;
            }
        }
    }

    fn push_scope(&mut self) {
        self.scopes.push(vec![]);
    }

    /// Ends a scope, using every variable nobody used (unused variables are
    /// errors), which also prints their final values.
    fn pop_scope(&mut self) {
        let s = self.scopes.pop().unwrap();
        for v in s {
            if !v.used {
                let e = self.show_var(&v.name, v.ty);
                self.line(&format!("println(\"{}\", {})", v.name, e));
            }
        }
    }

    fn show_var(&self, name: &str, ty: T) -> String {
        match ty {
            T::VecI => format!("{}.len(), {}.sum()", name, name),
            T::P => format!("{}.a, {}.b, {}.s", name, name, name),
            T::E => format!("{}.code()", name),
            T::OptI => format!("{} ?? -1", name),
            T::VecP => format!("{}.len()", name),
            T::MapI => format!("{}.len()", name),
            _ => name.to_string(),
        }
    }

    // ------------------------------------------------------------------
    // Expressions

    fn int_lit(&mut self, ty: T) -> String {
        let v: i128 = match self.below(6) {
            0 => 0,
            1 => 1,
            2 => self.below(10) as i128,
            3 => self.below(1000) as i128,
            4 => match ty {
                T::U8 => 255,
                T::I32 => 2147483647,
                T::U64 => 18446744073709551615,
                _ => 9223372036854775807,
            },
            _ => (self.rng.next() % (1u64 << (ty.bits().min(63) - 1))) as i128,
        };
        let v = match ty {
            T::U8 => v % 256,
            T::I32 => v % 2147483648,
            _ => v,
        };
        let neg = matches!(ty, T::I64 | T::I32) && self.chance(1, 4) && v > 0;
        let lit = if neg { format!("-{}", v) } else { format!("{}", v) };
        // a literal takes its type from the context, which an `if` or `match`
        // around it doesn't pass on: spell the type out
        if ty == T::I64 && self.in_conv == 0 && self.chance(1, 2) {
            lit
        } else {
            format!("{}({})", ty.name(), lit)
        }
    }

    /// An integer expression of type `ty`.
    fn int_expr(&mut self, ty: T, d: u32) -> String {
        if d == 0 || self.chance(1, 4) {
            if self.chance(1, 2)
                && let Some(v) = self.pick_var(ty, false) {
                    return v;
                }
            return self.int_lit(ty);
        }
        let d = d - 1;
        match self.below(16) {
            0..=3 => {
                let op = *self.rng.pick(&["+%", "-%", "*%", "&", "|", "^"]);
                let a = self.int_expr(ty, d);
                let b = self.int_expr(ty, d);
                format!("({} {} {})", a, op, b)
            }
            4 => {
                // checked, on small values: can't overflow
                let op = *self.rng.pick(&["+", "-", "*"]);
                // small enough that the product fits too
                let m = match ty {
                    T::U8 => "15",
                    T::I32 => "32767",
                    _ => "65535",
                };
                let a = self.int_expr(ty, d);
                let b = self.int_expr(ty, d);
                if op == "-" && matches!(ty, T::U8 | T::U64) {
                    return format!("(({} & {}) | {}) - ({} & {})", a, m, if ty == T::U8 { "16" } else { "65536" }, b, m);
                }
                format!("(({} & {}) {} ({} & {}))", a, m, op, b, m)
            }
            5 => {
                // checked, unmasked: may panic, deterministically
                if self.chance(1, 4) {
                    let op = *self.rng.pick(&["+", "-", "*"]);
                    let a = self.int_expr(ty, d);
                    let b = self.int_expr(ty, d);
                    format!("({} {} {})", a, op, b)
                } else {
                    let a = self.int_expr(ty, d);
                    let b = self.int_expr(ty, d);
                    format!("({} +% {})", a, b)
                }
            }
            6 => {
                let op = *self.rng.pick(&["/", "%"]);
                let a = self.int_expr(ty, d);
                let b = self.int_expr(ty, d);
                let m = if ty == T::U8 { "7" } else { "1023" };
                format!("({} {} (({} & {}) + 1))", a, op, b, m)
            }
            7 => {
                let op = *self.rng.pick(&["<<", ">>"]);
                let a = self.int_expr(ty, d);
                let s = self.int_expr(T::I64, d);
                format!("({} {} ({} & {}))", a, op, s, ty.bits() - 1)
            }
            8 => {
                // conversions that can't fail
                let from = *self.rng.pick(&INTS);
                self.in_conv += 1;
                let x = self.int_expr(from, d);
                self.in_conv -= 1;
                match (from, ty) {
                    (a, b) if a == b => x,
                    (T::U64, T::I64) => format!("int({} & 9223372036854775807)", x),
                    (_, T::I64) => format!("int({})", x),
                    (_, T::U8) => format!("u8({} & 255)", x),
                    (T::U8, _) => format!("{}({})", ty.name(), x),
                    (_, T::I32) => format!("i32({} & 2147483647)", x),
                    (T::I32, T::U64) => format!("u64({} & 2147483647)", x),
                    _ => format!("u64({} & 9223372036854775807)", x),
                }
            }
            9 => {
                let c = self.bool_expr(d);
                let a = self.int_expr(ty, d);
                let b = self.int_expr(ty, d);
                format!("(if {} {{ {} }} else {{ {} }})", c, a, b)
            }
            10 => {
                let x = self.int_expr(T::I64, d);
                let a = self.int_expr(ty, d);
                let b = self.int_expr(ty, d);
                let c = self.int_expr(ty, d);
                format!("(match {} & 3 {{\n{}    0 => {},\n{}    1 | 2 => {},\n{}    _ => {},\n{}}})", x, self.indent(), a, self.indent(), b, self.indent(), c, self.indent())
            }
            11 => {
                if let Some(c) = self.call_returning(ty, d) {
                    return c;
                }
                self.int_lit(ty)
            }
            12 if ty == T::I64 => {
                if let Some(v) = self.pick_var(T::VecI, false) {
                    return match self.below(3) {
                        0 => format!("{}.len()", v),
                        1 => format!("(if {}.len() > 0 {{ {}[({} & 1023) % {}.len()] }} else {{ 7 }})", v, v, self.int_expr(T::I64, d), v),
                        _ => format!("{}.sum()", v),
                    };
                }
                self.int_lit(ty)
            }
            13 if ty == T::I64 => {
                if let Some(o) = self.pick_var(T::OptI, false) {
                    let b = self.int_expr(T::I64, d);
                    return format!("({} ?? {})", o, b);
                }
                if let Some(s) = self.pick_var(T::Str, false) {
                    return format!("{}.len()", s);
                }
                self.int_lit(ty)
            }
            14 if ty == T::I64 => {
                if let Some(p) = self.pick_var(T::P, false) {
                    return if self.chance(1, 2) { format!("{}.a", p) } else { format!("{}.total()", p) };
                }
                if let Some(e) = self.pick_var(T::E, false) {
                    return format!("{}.code()", e);
                }
                self.int_lit(ty)
            }
            15 if ty == T::I64 && self.chance(1, 2) => {
                if let Some(v) = self.pick_var(T::VecP, false) {
                    let i = self.int_expr(T::I64, d);
                    return format!("(if {}.len() > 0 {{ {}[({} & 1023) % {}.len()].a }} else {{ 3 }})", v, v, i, v);
                }
                if let Some(m) = self.pick_var(T::MapI, false) {
                    let k = self.int_expr(T::I64, d);
                    return format!("(if {}.contains({} & 7) {{ {}.len() }} else {{ 0 }})", m, k, m);
                }
                if let Some(c) = self.result_call(d) {
                    return c;
                }
                self.int_lit(ty)
            }
            15 if ty == T::I64 && self.funcs.iter().any(|f| f.name == "apply") => {
                let p = self.fresh("q");
                self.no_try += 1;
                self.push_scope();
                self.declare(&p, T::I64, false);
                self.scopes.last_mut().unwrap()[0].used = true;
                let body = self.int_expr(T::I64, d);
                self.scopes.pop();
                self.no_try -= 1;
                let x = self.int_expr(T::I64, d);
                format!("apply(({}) => {}, {})", p, body, x)
            }
            _ => {
                let a = self.int_expr(ty, d);
                if matches!(ty, T::I64 | T::I32) {
                    format!("(0 -% {})", a)
                } else {
                    format!("(~{})", a)
                }
            }
        }
    }

    fn indent(&self) -> String {
        "    ".repeat(self.ind)
    }

    fn bool_expr(&mut self, d: u32) -> String {
        if d == 0 || self.chance(1, 5) {
            if self.chance(1, 2)
                && let Some(v) = self.pick_var(T::Bool, false) {
                    return v;
                }
            return if self.chance(1, 2) { "true".into() } else { "false".into() };
        }
        let d = d - 1;
        match self.below(7) {
            0..=2 => {
                let ty = *self.rng.pick(&INTS);
                let op = *self.rng.pick(&["==", "!=", "<", "<=", ">", ">="]);
                let a = self.int_expr(ty, d);
                let b = self.int_expr(ty, d);
                format!("({} {} {})", a, op, b)
            }
            3 => {
                let op = *self.rng.pick(&["&&", "||"]);
                let a = self.bool_expr(d);
                let b = self.bool_expr(d);
                format!("({} {} {})", a, op, b)
            }
            4 => format!("!{}", self.bool_expr(d)),
            5 => {
                if let Some(s) = self.pick_var(T::Str, false) {
                    let lit = *self.rng.pick(&["\"a\"", "\"ab\"", "\"\"", "\"7\""]);
                    return format!("{}.contains({})", s, lit);
                }
                if let Some(v) = self.pick_var(T::VecI, false) {
                    let x = self.int_expr(T::I64, d);
                    return format!("{}.contains({})", v, x);
                }
                "true".into()
            }
            _ => {
                if let Some(c) = self.call_returning(T::Bool, d) {
                    return c;
                }
                "false".into()
            }
        }
    }

    fn str_expr(&mut self, d: u32) -> String {
        if d == 0 || self.chance(1, 4) {
            if self.chance(1, 2)
                && let Some(v) = self.pick_var(T::Str, false) {
                    return format!("{}.clone()", v);
                }
            let lit = *self.rng.pick(&["\"\"", "\"a\"", "\"gojo\"", "\"x y\"", "\"\\u{e9}t\\u{e9}\"", "\"7\""]);
            return format!("{}.toString()", lit);
        }
        let d = d - 1;
        match self.below(6) {
            0 => {
                let a = self.str_expr(d);
                let b = self.str_expr(d);
                format!("({} + {})", a, b)
            }
            1 => {
                let ty = *self.rng.pick(&INTS);
                let x = self.int_expr(ty, d);
                format!("fmt.sprint({})", x)
            }
            2 => {
                let s = self.str_expr(d);
                let n = self.int_expr(T::I64, d);
                format!("{}.repeat({} & 3)", s, n)
            }
            3 => format!("{}.toUpper()", self.str_expr(d)),
            4 => {
                if let Some(c) = self.call_returning(T::Str, d) {
                    return c;
                }
                "\"f\".toString()".into()
            }
            _ => {
                if let Some(p) = self.pick_var(T::P, false) {
                    return format!("{}.s.clone()", p);
                }
                "\"p\".toString()".into()
            }
        }
    }

    fn vec_expr(&mut self, d: u32) -> String {
        if self.chance(1, 3)
            && let Some(v) = self.pick_var(T::VecI, false) {
                return format!("{}.clone()", v);
            }
        let n = self.below(5);
        let xs: Vec<String> = (0..n).map(|_| self.int_expr(T::I64, d.saturating_sub(1))).collect();
        if xs.is_empty() {
            "Vec[int]{}".into()
        } else {
            format!("[{}]", xs.join(", "))
        }
    }

    fn opt_expr(&mut self, d: u32) -> String {
        if self.chance(1, 3) {
            "none".into()
        } else {
            format!("some({})", self.int_expr(T::I64, d))
        }
    }

    fn p_expr(&mut self, d: u32) -> String {
        if self.chance(1, 3)
            && let Some(v) = self.pick_var(T::P, false) {
                return format!("{}.clone()", v);
            }
        let a = self.int_expr(T::I64, d);
        let b = self.int_expr(T::I32, d);
        let s = self.str_expr(d);
        format!("P{{a: {}, b: {}, s: {}}}", a, b, s)
    }

    fn e_expr(&mut self, d: u32) -> String {
        match self.below(3) {
            0 => "E.A".into(),
            1 => format!("E.B({})", self.int_expr(T::I64, d)),
            _ => format!("E.C({})", self.str_expr(d)),
        }
    }

    /// A call to a function returning `int ! String`, handled the way the
    /// context allows: `try` in a result function, otherwise `catch` or
    /// `unwrapOr`.
    fn result_call(&mut self, d: u32) -> Option<String> {
        let in_loop = self.loops > 0;
        let cands: Vec<Func> = self.funcs.iter().filter(|f| f.ret == T::ResI && !(in_loop && f.heavy) && f.params.iter().all(|p| p.1 == Mode::Borrow)).cloned().collect();
        if cands.is_empty() {
            return None;
        }
        let f = cands[self.below(cands.len())].clone();
        self.heavy |= f.heavy;
        let args: Vec<String> = f.params.iter().map(|(t, _)| self.expr(*t, d.min(1))).collect();
        let call = format!("{}({})", f.name, args.join(", "));
        Some(if self.in_result && self.no_try == 0 && self.chance(2, 3) {
            format!("(try {})", call)
        } else if self.chance(1, 2) {
            let e = self.fresh("e");
            format!("({} catch {} {{ {}.len() +% 100 }})", call, e, e)
        } else {
            format!("{}.unwrapOr(-2)", call)
        })
    }

    fn vecp_expr(&mut self, d: u32) -> String {
        if self.chance(1, 3)
            && let Some(v) = self.pick_var(T::VecP, false) {
                return format!("{}.clone()", v);
            }
        let n = self.below(4);
        let xs: Vec<String> = (0..n).map(|_| self.p_expr(d.saturating_sub(1))).collect();
        if xs.is_empty() {
            "Vec[P]{}".into()
        } else {
            format!("[{}]", xs.join(", "))
        }
    }

    fn mapi_expr(&mut self, d: u32) -> String {
        if self.chance(1, 3)
            && let Some(v) = self.pick_var(T::MapI, false) {
                return format!("{}.clone()", v);
            }
        let n = self.below(4);
        let xs: Vec<String> = (0..n).map(|i| format!("{}: {}", i, self.int_expr(T::I64, d.saturating_sub(1)))).collect();
        format!("Map[int, int]{{{}}}", xs.join(", "))
    }

    fn expr(&mut self, ty: T, d: u32) -> String {
        match ty {
            t if t.is_int() => self.int_expr(t, d),
            T::Bool => self.bool_expr(d),
            T::Str => self.str_expr(d),
            T::VecI => self.vec_expr(d),
            T::OptI => self.opt_expr(d),
            T::P => self.p_expr(d),
            T::E => self.e_expr(d),
            T::VecP => self.vecp_expr(d),
            T::MapI => self.mapi_expr(d),
            _ => unreachable!(),
        }
    }

    /// A call to a generated function returning `ty`.
    fn call_returning(&mut self, ty: T, d: u32) -> Option<String> {
        let in_loop = self.loops > 0;
        // inout arguments only in statements (see `stmt`): in an expression they
        // could alias other operands
        let cands: Vec<Func> = self.funcs.iter().filter(|f| f.ret == ty && f.name != "apply" && !(in_loop && f.heavy) && f.params.iter().all(|p| p.1 == Mode::Borrow)).cloned().collect();
        if cands.is_empty() {
            return None;
        }
        let f = cands[self.below(cands.len())].clone();
        self.heavy |= f.heavy;
        let mut args = vec![];
        for (pt, m) in &f.params {
            match m {
                Mode::Inout => unreachable!(),
                Mode::Borrow => {
                    let a = match pt {
                        T::Str | T::VecI | T::P | T::E => match self.pick_var(*pt, false) {
                            Some(v) => v,
                            None => self.expr(*pt, d),
                        },
                        _ => self.expr(*pt, d),
                    };
                    args.push(a);
                }
            }
        }
        Some(format!("{}({})", f.name, args.join(", ")))
    }

    // ------------------------------------------------------------------
    // Statements

    fn stmt(&mut self, d: u32) {
        self.budget -= 1;
        match self.below(20) {
            0..=2 => {
                let ty = *self.rng.pick(&[T::I64, T::I64, T::I32, T::U8, T::U64, T::Bool, T::Str, T::VecI, T::OptI, T::P, T::E, T::VecP, T::MapI]);
                let name = self.fresh("v");
                let mutable = self.chance(1, 2);
                let e = self.expr(ty, 3);
                let ann = if ty == T::OptI || self.chance(1, 3) { format!(": {}", ty.name()) } else { String::new() };
                self.line(&format!("{} {}{} = {}", if mutable { "var" } else { "let" }, name, ann, e));
                self.declare(&name, ty, mutable);
            }
            3 | 4 => {
                // assignment
                let ty = *self.rng.pick(&[T::I64, T::I32, T::U8, T::U64, T::Bool, T::Str, T::OptI, T::P]);
                if let Some(v) = self.pick_var(ty, true) {
                    let e = self.expr(ty, 3);
                    if ty.is_int() && self.chance(1, 2) {
                        let op = *self.rng.pick(&["+%=", "-%=", "*%=", "^=", "|=", "&="]);
                        self.line(&format!("{} {} {}", v, op, e));
                    } else {
                        self.line(&format!("{} = {}", v, e));
                    }
                } else if let Some(p) = self.pick_var(T::P, true) {
                    let e = self.int_expr(T::I64, 2);
                    self.line(&format!("{}.a = {}", p, e));
                }
            }
            5 => {
                if let Some(v) = self.pick_var(T::VecI, true) {
                    match self.below(5) {
                        0 | 1 => {
                            let e = self.int_expr(T::I64, 2);
                            self.line(&format!("{}.push({})", v, e));
                        }
                        2 => self.line(&format!("_ = {}.pop()", v)),
                        3 => self.line(&format!("{}.sort()", v)),
                        _ => self.line(&format!("{}.reverse()", v)),
                    }
                } else if let Some(s) = self.pick_var(T::Str, true) {
                    let lit = *self.rng.pick(&["\"!\"", "\"ab\"", "\"\""]);
                    self.line(&format!("{}.push({})", s, lit));
                }
            }
            6 => {
                let a: Vec<String> = (0..1 + self.below(3))
                    .map(|_| {
                        let ty = *self.rng.pick(&[T::I64, T::I32, T::U8, T::U64, T::Bool, T::Str]);
                        
                        self.expr(ty, 2)
                    })
                    .collect();
                self.line(&format!("println({})", a.join(", ")));
            }
            7 if d > 0 => {
                let c = self.bool_expr(2);
                self.line(&format!("if {} {{", c));
                self.block(d - 1, 3);
                if self.chance(1, 2) {
                    self.line("} else {");
                    self.block(d - 1, 3);
                }
                self.line("}");
            }
            8 if d > 0 && self.loops < 2 => {
                let i = self.fresh("n");
                let n = 1 + self.below(6);
                let label = if self.chance(1, 4) { Some(self.fresh("l")) } else { None };
                self.line(&format!("{}for {} in 0..{} {{", label.as_ref().map(|l| format!("{}: ", l)).unwrap_or_default(), i, n));
                self.loops += 1;
                self.heavy = true;
                self.ind += 1;
                self.push_scope();
                self.declare(&i, T::I64, false);
                for _ in 0..1 + self.below(3) {
                    self.stmt(d - 1);
                }
                if self.chance(1, 3) {
                    let c = self.bool_expr(2);
                    let what = *self.rng.pick(&["break", "continue"]);
                    let l = label.clone().map(|l| format!(" {}", l)).unwrap_or_default();
                    self.line(&format!("if {} {{ {}{} }}", c, what, l));
                }
                self.pop_scope();
                self.ind -= 1;
                self.loops -= 1;
                self.line("}");
            }
            9 if d > 0 && self.loops < 2 => {
                if let Some(v) = self.pick_var(T::VecI, false) {
                    let x = self.fresh("x");
                    self.line(&format!("for {} in {} {{", x, v));
                    self.loops += 1;
                    self.heavy = true;
                    self.iterating.push(v.clone());
                    self.ind += 1;
                    self.push_scope();
                    self.declare(&x, T::I64, false);
                    for _ in 0..1 + self.below(2) {
                        self.stmt(d - 1);
                    }
                    self.pop_scope();
                    self.ind -= 1;
                    self.iterating.pop();
                    self.loops -= 1;
                    self.line("}");
                }
            }
            10 => {
                if let Some(e) = self.pick_var(T::E, false) {
                    let (x, s) = (self.fresh("x"), self.fresh("s"));
                    let a = self.int_expr(T::I64, 2);
                    self.line(&format!("match {} {{", e));
                    self.line(&format!("    .A => println(\"A\", {})", a));
                    if self.chance(1, 2) {
                        let g = self.fresh("g");
                        let k = self.below(50);
                        self.line(&format!("    .B({}) if {} > {} => println(\"big B\", {})", g, g, k, g));
                    }
                    self.line(&format!("    .B({}) => println(\"B\", {})", x, x));
                    self.line(&format!("    .C({}) => println(\"C\", {})", s, s));
                    self.line("}");
                }
            }
            11 => {
                if let Some(o) = self.pick_var(T::OptI, false) {
                    let z = self.fresh("z");
                    self.line(&format!("if let {} = {} {{", z, o));
                    self.ind += 1;
                    self.line(&format!("println(\"some\", {})", z));
                    self.ind -= 1;
                    self.line("} else {");
                    self.line("    println(\"none\")");
                    self.line("}");
                }
            }
            12 => {
                // a call through an inout parameter, as a statement
                let in_loop = self.loops > 0;
                let cands: Vec<Func> = self.funcs.iter().filter(|f| f.params.iter().any(|p| p.1 == Mode::Inout) && !(in_loop && f.heavy)).cloned().collect();
                if !cands.is_empty() {
                    let f = cands[self.below(cands.len())].clone();
                    let saved = self.used_flags();
                    let mut args = vec![];
                    let mut ok = true;
                    let mut taken: Vec<String> = vec![];
                    for (pt, m) in &f.params {
                        match m {
                            Mode::Inout => match self.pick_var(*pt, true) {
                                Some(v) if !taken.contains(&v) => {
                                    taken.push(v.clone());
                                    args.push(format!("&{}", v));
                                }
                                _ => ok = false,
                            },
                            Mode::Borrow => {
                                let e = self.expr(*pt, 1);
                                if taken.iter().any(|t| e.contains(t.as_str())) {
                                    ok = false;
                                }
                                args.push(e);
                            }
                        }
                    }
                    if !ok {
                        // nothing is emitted: the variables weren't used
                        self.restore_used(saved);
                    } else {
                        self.heavy |= f.heavy;
                        let call = format!("{}({})", f.name, args.join(", "));
                        if f.ret == T::Bool || f.ret.is_int() || f.ret == T::Str {
                            self.line(&format!("println({})", call));
                        } else {
                            self.line(&format!("_ = {}", call));
                        }
                    }
                }
            }
            14 => {
                if let Some(v) = self.pick_var(T::VecP, true) {
                    match self.below(5) {
                        0 | 1 => {
                            let e = self.p_expr(1);
                            self.line(&format!("{}.push({})", v, e));
                        }
                        2 => self.line(&format!("_ = {}.pop()", v)),
                        3 => self.line(&format!("{}.sortBy((x, y) => x.a < y.a)", v)),
                        _ => {
                            let e = self.int_expr(T::I64, 2);
                            self.line(&format!("if {}.len() > 0 {{ {}[0].a = {} }}", v, v, e));
                        }
                    }
                } else if let Some(m) = self.pick_var(T::MapI, true) {
                    let k = self.int_expr(T::I64, 1);
                    if self.chance(1, 3) {
                        self.line(&format!("_ = {}.remove({} & 7)", m, k));
                    } else {
                        let v = self.int_expr(T::I64, 2);
                        self.line(&format!("_ = {}.insert({} & 7, {})", m, k, v));
                    }
                }
            }
            15 if d > 0 && self.loops < 2 => {
                let ty = if self.chance(1, 2) { T::VecP } else { T::MapI };
                if let Some(v) = self.pick_var(ty, false) {
                    self.loops += 1;
                    self.heavy = true;
                    self.iterating.push(v.clone());
                    let (a, b) = (self.fresh("k"), self.fresh("w"));
                    if ty == T::VecP {
                        self.line(&format!("for {} in {} {{", a, v));
                    } else {
                        self.line(&format!("for {}, {} in {} {{", a, b, v));
                    }
                    self.ind += 1;
                    self.push_scope();
                    if ty == T::VecP {
                        self.declare(&a, T::P, false);
                    } else {
                        self.declare(&a, T::I64, false);
                        self.declare(&b, T::I64, false);
                    }
                    for _ in 0..1 + self.below(2) {
                        self.stmt(d - 1);
                    }
                    self.pop_scope();
                    self.ind -= 1;
                    self.iterating.pop();
                    self.loops -= 1;
                    self.line("}");
                }
            }
            16 => {
                if let Some(m) = self.pick_var(T::MapI, false) {
                    let z = self.fresh("z");
                    let k = self.int_expr(T::I64, 1);
                    self.line(&format!("if let {} = {}.get({} & 7) {{", z, m, k));
                    self.line(&format!("    println(\"got\", {})", z));
                    self.line("}");
                }
            }
            17 => {
                // runs at the end of the enclosing block, after anything below
                self.no_try += 1;
                let e = self.int_expr(T::I64, 2);
                self.no_try -= 1;
                self.line(&format!("defer println(\"deferred\", {})", e));
            }
            18 if d > 0 && self.loops < 2 => {
                let w = self.fresh("c");
                let n = 1 + self.below(5);
                self.line(&format!("var {} = 0", w));
                self.line(&format!("while {} < {} {{", w, n));
                self.loops += 1;
                self.heavy = true;
                self.ind += 1;
                self.push_scope();
                for _ in 0..1 + self.below(2) {
                    self.stmt(d - 1);
                }
                self.pop_scope();
                self.line(&format!("{} += 1", w));
                self.ind -= 1;
                self.loops -= 1;
                self.line("}");
            }
            19 if self.in_result && self.no_try == 0 && self.chance(1, 2) => {
                if let Some(c) = self.result_call(1) {
                    let v = self.fresh("t");
                    self.line(&format!("let {} = {}", v, c));
                    self.declare(&v, T::I64, false);
                }
            }
            19 if self.in_result && self.no_try == 0 => {
                let c = self.bool_expr(2);
                let msg = *self.rng.pick(&["\"bad\"", "\"worse\"", "\"\""]);
                self.line(&format!("if {} {{", c));
                self.line(&format!("    return err({}.toString())", msg));
                self.line("}");
            }
            _ => {
                let ty = *self.rng.pick(&[T::I64, T::Str, T::Bool]);
                if let Some(c) = self.call_returning(ty, 2) {
                    self.line(&format!("println({})", c));
                }
            }
        }
    }

    fn block(&mut self, d: u32, n: usize) {
        self.ind += 1;
        self.push_scope();
        for _ in 0..1 + self.below(n) {
            self.stmt(d);
        }
        self.pop_scope();
        self.ind -= 1;
    }

    // ------------------------------------------------------------------
    // Functions and the program

    fn func(&mut self, idx: usize) {
        let name = format!("f{}", idx);
        let ret = *self.rng.pick(&[T::I64, T::I64, T::Bool, T::Str, T::I32, T::U64, T::ResI, T::ResI]);
        self.in_result = ret == T::ResI;
        let mut params = vec![];
        for _ in 0..self.below(4) {
            let ty = *self.rng.pick(&[T::I64, T::I32, T::U8, T::U64, T::Bool, T::Str, T::VecI, T::P, T::E, T::OptI, T::VecP, T::MapI]);
            let mode = if matches!(ty, T::VecI | T::Str | T::I64) && self.chance(1, 4) { Mode::Inout } else { Mode::Borrow };
            params.push((ty, mode));
        }
        let pnames: Vec<String> = params.iter().map(|_| self.fresh("a")).collect();
        let sig: Vec<String> = params.iter().zip(&pnames).map(|((t, m), n)| format!("{}: {}{}", n, if *m == Mode::Inout { "inout " } else { "" }, t.name())).collect();
        let start = self.out.len();
        self.line(&format!("fn {}({}) -> {} {{", name, sig.join(", "), ret.name()));
        self.ind += 1;
        self.push_scope();
        for ((t, m), n) in params.iter().zip(&pnames) {
            self.declare(n, *t, *m == Mode::Inout);
        }
        self.budget = 6 + self.below(10) as i32;
        self.heavy = false;
        while self.budget > 0 {
            self.stmt(2);
        }
        let r = self.expr(if ret == T::ResI { T::I64 } else { ret }, 3);
        // the return value is computed before the scope's leftovers print, so
        // its variables count as used
        let tmp = self.fresh("r");
        self.line(&format!("let {} = {}", tmp, r));
        self.pop_scope();
        self.line(&format!("return {}", tmp));
        self.ind -= 1;
        self.line("}");
        self.line("");
        let heavy = self.heavy;
        self.in_result = false;
        let text = &self.out[start..];
        let prints = text.contains("print") || self.funcs.iter().any(|f| f.prints && text.contains(&format!("{}(", f.name)));
        self.funcs.push(Func { name, params, ret, heavy, prints });
    }

    /// A closed integer expression (no variables) and its value, computed
    /// here with Gojo's semantics: the program checks that it gets the same.
    /// Catches wrong values that every build agrees on (literal typing,
    /// conversions, C integer promotions in generated code).
    fn closed(&mut self, ty: T, d: u32) -> (String, i128) {
        let bits = ty.bits();
        let signed = matches!(ty, T::I64 | T::I32);
        let wrap = |v: i128| -> i128 {
            let m = 1i128 << bits;
            let r = v.rem_euclid(m);
            if signed && r >= m / 2 { r - m } else { r }
        };
        if d == 0 || self.chance(1, 4) {
            // with `in_conv` set, literals are spelled `T(value)`
            let lit = self.int_lit(ty);
            let v: i128 = lit.split_once('(').and_then(|(_, r)| r.strip_suffix(')')).and_then(|x| x.parse().ok()).expect("typed literal");
            return (lit, v);
        }
        let d = d - 1;
        let (a, x) = self.closed(ty, d);
        let (b, y) = self.closed(ty, d);
        match self.below(10) {
            0 => (format!("({} +% {})", a, b), wrap(x + y)),
            1 => (format!("({} -% {})", a, b), wrap(x - y)),
            2 => (format!("({} *% {})", a, b), wrap(x.wrapping_mul(y))),
            3 => (format!("({} & {})", a, b), wrap(x & y)),
            4 => (format!("({} | {})", a, b), wrap(x | y)),
            5 => (format!("({} ^ {})", a, b), wrap(x ^ y)),
            6 => (format!("(~{})", a), wrap(!x)),
            7 => {
                let k = (y.rem_euclid(bits as i128)) as u32;
                let e = format!("({} >> ({} & {}))", a, b, bits - 1);
                // arithmetic for signed types, logical for unsigned
                let v = if signed { x >> k } else { (x.rem_euclid(1i128 << bits)) >> k };
                (e, wrap(v))
            }
            8 => {
                let k = (y.rem_euclid(bits as i128)) as u32;
                (format!("({} << ({} & {}))", a, b, bits - 1), wrap(x.rem_euclid(1i128 << bits) << k))
            }
            _ => {
                let m: i128 = if ty == T::U8 { 7 } else { 1023 };
                let dv = (y & m) + 1;
                if self.chance(1, 2) {
                    // truncating division, remainder with the dividend's sign
                    (format!("({} / (({} & {}) + 1))", a, b, m), wrap(x / dv))
                } else {
                    (format!("({} % (({} & {}) + 1))", a, b, m), wrap(x % dv))
                }
            }
        }
    }

    fn known_value_check(&mut self) {
        let ty = *self.rng.pick(&INTS);
        self.in_conv += 1; // literals spelled with their type
        let (e, v) = self.closed(ty, 3);
        self.in_conv -= 1;
        let want = if ty == T::I64 { format!("int({})", v) } else { format!("{}({})", ty.name(), v) };
        self.names += 1;
        self.line(&format!("if {} != {} {{", e, want));
        self.line(&format!("    panic(\"gojo-fuzz: wrong value #{}\")", self.names));
        self.line("}");
    }

    /// Tasks of a group add into a total under a lock. Wrapping addition
    /// commutes, so the result doesn't depend on the schedule.
    fn parallel_sum(&mut self) {
        let cands: Vec<Func> = self.funcs.iter().filter(|f| f.name != "apply" && !f.prints && f.ret == T::I64 && f.params.iter().all(|p| p.0 == T::I64 && p.1 == Mode::Borrow)).cloned().collect();
        let (tot, mu, i, t, v) = (self.fresh("tot"), self.fresh("mu"), self.fresh("n"), self.fresh("t"), self.fresh("v"));
        let n = 1 + self.below(8);
        let work = if cands.is_empty() {
            format!("{} *% 7", i)
        } else {
            let f = cands[self.below(cands.len())].clone();
            let args: Vec<String> = f.params.iter().map(|_| if self.chance(1, 2) { i.clone() } else { format!("{} +% {}", i, self.below(100)) }).collect();
            format!("{}({}) +% {}", f.name, args.join(", "), i)
        };
        self.line(&format!("var {} = 0", tot));
        self.line(&format!("let {} = Mutex(&{})", mu, tot));
        self.line("scope {");
        self.line(&format!("    for {} in 0..{} {{", i, n));
        self.line("        go () => {");
        self.line(&format!("            let {} = {}", v, work));
        self.line(&format!("            with {} = {}.lock() {{", t, mu));
        self.line(&format!("                {} +%= {}", t, v));
        self.line("            }");
        self.line("        }");
        self.line("    }");
        self.line("}");
        self.line(&format!("println(\"parallel\", {})", tot));
    }

    pub fn program(mut self) -> String {
        let _ = writeln!(
            self.out,
            "// generated by gojo-fuzz diff\nuse std/fmt\n\nstruct P {{\n    a: int\n    b: i32\n    s: String\n\n    fn total(self) -> int {{\n        return self.a +% int(self.b) +% self.s.len()\n    }}\n}}\n\nenum E {{\n    A\n    B(x: int)\n    C(s: String)\n\n    fn code(self) -> int {{\n        return match self {{\n            .A => 1,\n            .B(x) => (x & 1023) + 2,\n            .C(s) => s.len() +% 3,\n        }}\n    }}\n}}\n\nfn apply(f: fn(int) -> int, x: int) -> int {{\n    return f(f(x))\n}}\n\nfn show(x: int) -> String {{\n    return fmt.sprint(\"<\", x, \">\")\n}}\n"
        );
        self.funcs.push(Func { name: "apply".into(), params: vec![], ret: T::I64, heavy: false, prints: false });
        let n = 2 + self.below(6);
        for i in 0..n {
            self.func(i);
        }
        self.line("fn main() {");
        self.ind += 1;
        self.push_scope();
        self.budget = 10 + self.below(15) as i32;
        while self.budget > 0 {
            self.stmt(2);
            if self.chance(1, 12) {
                self.parallel_sum();
            }
            if self.chance(1, 4) {
                self.known_value_check();
            }
        }
        // call every function once at the end too
        for f in self.funcs.clone() {
            if f.name == "apply" {
                continue;
            }
            if f.params.iter().any(|p| p.1 == Mode::Inout) {
                continue;
            }
            let args: Vec<String> = f.params.iter().map(|(t, _)| self.expr(*t, 1)).collect();
            let call = format!("{}({})", f.name, args.join(", "));
            self.line(&format!("println(\"{}\", {})", f.name, call));
        }
        self.pop_scope();
        self.ind -= 1;
        self.line("}");
        self.out
    }
}
