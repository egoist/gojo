// Bounds-check elimination.
//
// An index `x[e]` (or a slice `x[a..b]`) whose range the compiler can prove
// is emitted without its check. The proof uses facts about loop variables and
// the shape of the index expression; nothing else:
//
// - `for i in lo..hi` (or `lo..=hi`) gives `i` a lower bound when `lo` has one
//   and an upper bound when `hi` has one. `for i, _ in x` gives `0 <= i < x.len`.
//   `let n = e` gives `n` the bounds of `e` (`let n = x.len; for i in 0..n`).
// - An upper bound is a constant (`i <= 7`) or relative to the length of a
//   place (`i <= x.len - 1`). A place is a local, its fields, and the
//   contents of a `Box` along the way; nothing reached through an index, a
//   map, an `rc`/`arc`, a lock or a pointer.
// - Bounds combine through `+` and `-` (the checked ones) with a constant,
//   `&` with a non-negative constant, `%` by a positive constant, `.len`,
//   integer types (`a[b]` with `b: u8` and `a: [256]T`), conversions between
//   integer types (not through a float, which rounds) and
//   constants. A literal index into an array is checked by the type checker.
//
// A bound relative to `x.len` holds only while `x` keeps its length. A loop's
// fact counts only if its body can't change that length (a `let`'s, only if
// nothing in the function can), which needs it to
// name `x` (or what `x` borrows from): an assignment to `x` or a place that
// contains it, an `inout` argument or receiver (`x.push(1)`, `f(&x)`), a
// move, a mutable binding, a closure that captures it to modify or move.
// Elements may change (`x[j] = v`, `&x[j]`). The borrow checker makes
// sure there is no other way: no other task or closure can change `x` while
// the loop reads it. Bodies with `unsafe` blocks, or that `yield`, get no
// facts at all.
//
// `gojo build --verify-bce` keeps every removed check and reports a failed one
// as a compiler bug; the test suite and gojo-fuzz build that way.

use super::{Fs, Gen};
use crate::tir::*;
use crate::types::*;

/// A step from a local to a place inside it that has a fixed location.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Step {
    Field(usize),
    /// The contents of a `Box`.
    Unbox,
}

/// A place with a fixed location while nothing assigns to it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct Path {
    pub root: LocalId,
    pub steps: Vec<Step>,
}

/// An upper bound (inclusive) on an integer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Upper {
    Const(i128),
    /// `len(path) - m`
    Len(Path, i128),
}

impl Upper {
    /// The bound of `v + c` when `v` has this one.
    fn plus(self, c: i128) -> Option<Upper> {
        Some(match self {
            Upper::Const(u) => Upper::Const(u.checked_add(c)?),
            Upper::Len(p, m) => Upper::Len(p, m.checked_sub(c)?),
        })
    }
}

/// What is known about a loop variable inside the loop's body.
#[derive(Clone, Debug)]
pub(super) struct Fact {
    pub var: LocalId,
    pub lo: Option<i128>,
    pub hi: Option<Upper>,
}

/// Lengths the analysis looks at are below this, and so are the bounds it
/// derives from them, so no arithmetic on them overflows an i128.
const LIMIT: i128 = 1 << 100;

fn strip(e: &TExpr) -> &TExpr {
    match &e.kind {
        TK::Copy(x) => strip(x),
        _ => e,
    }
}

impl<'a> Gen<'a> {
    /// The value of a constant integer expression.
    pub(super) fn bce_const(&self, fs: &Fs, e: &TExpr) -> Option<i128> {
        self.bce_const_d(fs, e, 0)
    }

    fn bce_const_d(&self, fs: &Fs, e: &TExpr, depth: u32) -> Option<i128> {
        if depth > 32 {
            return None;
        }
        let v = match &strip(e).kind {
            TK::ConstParam(p) => match self.sub(fs, &Ty::Param(*p)) {
                Ty::ConstInt(n) => n as i128,
                _ => return None,
            },
            TK::Unary { op: "-", e: x } => {
                // (negating the type's minimum overflows: the program panics)
                let v = self.bce_const_d(fs, x, depth + 1)?.checked_neg()?;
                let (lo, hi) = self.int_range(fs, &e.ty)?;
                if v < lo || v > hi {
                    return None;
                }
                v
            }
            _ => match self.bce_len_of(e).map(|x| self.sub(fs, &x.ty)) {
                Some(Ty::Array(_, n)) => n as i128,
                _ => const_int(e, self.prog)?,
            },
        };
        (v.abs() < LIMIT).then_some(v)
    }

    /// The place `e` names, when it is one with a fixed location.
    pub(super) fn bce_path(&self, e: &TExpr) -> Option<Path> {
        match &strip(e).kind {
            TK::Local(l) => Some(Path { root: *l, steps: vec![] }),
            TK::Field { base, index } => {
                let mut p = self.bce_path(base)?;
                p.steps.push(Step::Field(*index));
                Some(p)
            }
            TK::Deref(base) if matches!(base.ty, Ty::Box(_)) => {
                let mut p = self.bce_path(base)?;
                p.steps.push(Step::Unbox);
                Some(p)
            }
            _ => None,
        }
    }

    /// `x` when `e` is `x.len` or `x.len()` of a vector, slice, array or string.
    fn bce_len_of<'e>(&self, e: &'e TExpr) -> Option<&'e TExpr> {
        let arg = match &strip(e).kind {
            TK::Intrinsic { name, args, .. } if name == "len" && args.len() == 1 => &args[0].e,
            TK::Call { callee: Callee::Func(f, _), args } if args.len() == 1 => {
                let fd = self.prog.func(*f);
                if fd.kind != FuncKind::Intrinsic || fd.pkg != self.prog.prelude || fd.name != "len" || !matches!(fd.recv_key, Some(TyKey::Slice) | Some(TyKey::Str)) {
                    return None;
                }
                &args[0].e
            }
            _ => return None,
        };
        let mut x = arg;
        while let TK::Coerce(Coercion::VecToSlice | Coercion::ArrayToSlice | Coercion::StringToStr, inner) = &x.kind {
            x = inner;
        }
        match &x.ty {
            Ty::Vec(_) | Ty::Slice(_) | Ty::Array(..) | Ty::ArrayT(..) | Ty::Str | Ty::String => Some(x),
            _ => None,
        }
    }

    fn bce_fact<'f>(&self, fs: &'f Fs, l: LocalId) -> Option<&'f Fact> {
        fs.bce.iter().rev().find(|f| f.var == l)
    }

    /// Whether `to` converts `from` from one integer type to another.
    fn int_cast(&self, fs: &Fs, from: &TExpr, to: &TExpr) -> bool {
        matches!(self.sub(fs, &from.ty), Ty::Int(_)) && matches!(self.sub(fs, &to.ty), Ty::Int(_))
    }

    fn int_range(&self, fs: &Fs, t: &Ty) -> Option<(i128, i128)> {
        match self.sub(fs, t) {
            Ty::Int(k) => Some((k.min(), k.max())),
            _ => None,
        }
    }

    /// A lower bound on the value of `e`.
    pub(super) fn bce_lower(&self, fs: &Fs, e: &TExpr) -> Option<i128> {
        self.bce_lower_d(fs, e, 0)
    }

    fn bce_lower_d(&self, fs: &Fs, e: &TExpr, depth: u32) -> Option<i128> {
        if depth > 32 {
            return None;
        }
        if let Some(c) = self.bce_const(fs, e) {
            return Some(c);
        }
        let by_type = self.int_range(fs, &e.ty).map(|r| r.0);
        let e = strip(e);
        let derived = match &e.kind {
            TK::Local(l) => self.bce_fact(fs, *l).and_then(|f| f.lo),
            TK::Binary { op: BinOp::Add, l, r } => match (self.bce_const(fs, l), self.bce_const(fs, r)) {
                (_, Some(c)) => self.bce_lower_d(fs, l, depth + 1).and_then(|x| x.checked_add(c)),
                (Some(c), _) => self.bce_lower_d(fs, r, depth + 1).and_then(|x| x.checked_add(c)),
                _ => None,
            },
            TK::Binary { op: BinOp::Sub, l, r } => match self.bce_const(fs, r) {
                Some(c) => self.bce_lower_d(fs, l, depth + 1).and_then(|x| x.checked_sub(c)),
                None => None,
            },
            TK::Binary { op: BinOp::BitAnd, l, r } => match (self.bce_const(fs, l), self.bce_const(fs, r)) {
                (Some(c), _) | (_, Some(c)) if c >= 0 => Some(0),
                _ => None,
            },
            TK::Binary { op: BinOp::Rem, l, r } => match self.bce_const(fs, r) {
                Some(c) if c > 0 && self.bce_lower_d(fs, l, depth + 1).is_some_and(|x| x >= 0) => Some(0),
                _ => None,
            },
            // a conversion between integer types panics unless the value
            // fits, so it keeps it (one through a float rounds)
            TK::Cast(x) if self.int_cast(fs, x, e) => self.bce_lower_d(fs, x, depth + 1),
            _ if self.bce_len_of(e).is_some() => Some(0),
            _ => None,
        };
        match (derived, by_type) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }

    /// An upper bound on the value of `e`.
    pub(super) fn bce_upper(&self, fs: &Fs, e: &TExpr) -> Option<Upper> {
        self.bce_upper_d(fs, e, 0)
    }

    fn bce_upper_d(&self, fs: &Fs, e: &TExpr, depth: u32) -> Option<Upper> {
        if depth > 32 {
            return None;
        }
        if let Some(c) = self.bce_const(fs, e) {
            return Some(Upper::Const(c));
        }
        let by_type = self.int_range(fs, &e.ty).map(|r| r.1).filter(|&m| m < LIMIT);
        let e = strip(e);
        let derived = match &e.kind {
            TK::Local(l) => self.bce_fact(fs, *l).and_then(|f| f.hi.clone()),
            TK::Binary { op: BinOp::Add, l, r } => match (self.bce_const(fs, l), self.bce_const(fs, r)) {
                (_, Some(c)) => self.bce_upper_d(fs, l, depth + 1).and_then(|u| u.plus(c)),
                (Some(c), _) => self.bce_upper_d(fs, r, depth + 1).and_then(|u| u.plus(c)),
                _ => None,
            },
            TK::Binary { op: BinOp::Sub, l, r } => match self.bce_const(fs, r) {
                Some(c) => self.bce_upper_d(fs, l, depth + 1).and_then(|u| u.plus(c.checked_neg()?)),
                None => None,
            },
            TK::Binary { op: BinOp::BitAnd, l, r } => match (self.bce_const(fs, l), self.bce_const(fs, r)) {
                (Some(c), _) | (_, Some(c)) if c >= 0 => Some(Upper::Const(c)),
                _ => None,
            },
            TK::Binary { op: BinOp::Rem, l, r } => match self.bce_const(fs, r) {
                Some(c) if c > 0 && self.bce_lower_d(fs, l, depth + 1).is_some_and(|x| x >= 0) => Some(Upper::Const(c - 1)),
                _ => None,
            },
            TK::Cast(x) if self.int_cast(fs, x, e) => self.bce_upper_d(fs, x, depth + 1),
            _ => match self.bce_len_of(e) {
                Some(x) => match self.sub(fs, &x.ty) {
                    Ty::Array(_, n) => Some(Upper::Const(n as i128)),
                    _ => self.bce_path(x).map(|p| Upper::Len(p, 0)),
                },
                None => None,
            },
        };
        match (derived, by_type) {
            // a length-relative bound is the useful one for vectors
            (Some(Upper::Const(a)), Some(b)) => Some(Upper::Const(a.min(b))),
            (Some(u), _) => Some(u),
            (None, b) => b.map(Upper::Const),
        }
    }

    /// Whether `v <= len(base) - m` for every value `v` that has bound `u`.
    fn bce_within(&self, fs: &Fs, base: &TExpr, u: &Upper, m: i128) -> bool {
        match (self.sub(fs, &base.ty), u) {
            (Ty::Array(_, n), Upper::Const(c)) => *c <= n as i128 - m,
            (Ty::Vec(_) | Ty::Slice(_) | Ty::Str | Ty::String, Upper::Len(p, k)) => *k >= m && self.bce_path(base).as_ref() == Some(p),
            _ => false,
        }
    }

    /// Whether `base[idx]` is always in range: the check can go.
    pub(super) fn bce_index_ok(&self, fs: &Fs, base: &TExpr, idx: &TExpr) -> bool {
        if !self.bce_lower(fs, idx).is_some_and(|l| l >= 0) {
            return false;
        }
        self.bce_upper(fs, idx).is_some_and(|u| self.bce_within(fs, base, &u, 1))
    }

    /// Whether `base[lo..hi]` (of a vector, slice or array) is always in range.
    pub(super) fn bce_slice_ok(&self, fs: &Fs, base: &TExpr, lo: Option<&TExpr>, hi: Option<&TExpr>) -> bool {
        // 0 <= lo
        if let Some(lo) = lo {
            if !self.bce_lower(fs, lo).is_some_and(|l| l >= 0) {
                return false;
            }
        }
        // hi <= len
        if let Some(hi) = hi {
            if !self.bce_upper(fs, hi).is_some_and(|u| self.bce_within(fs, base, &u, 0)) {
                return false;
            }
        }
        // lo <= hi
        match (lo, hi) {
            (None, None) => true,
            (None, Some(hi)) => self.bce_lower(fs, hi).is_some_and(|l| l >= 0),
            (Some(lo), None) => self.bce_upper(fs, lo).is_some_and(|u| self.bce_within(fs, base, &u, 0)),
            (Some(lo), Some(hi)) => {
                if let (Some(Upper::Const(a)), Some(b)) = (self.bce_upper(fs, lo), self.bce_lower(fs, hi)) {
                    if a <= b {
                        return true;
                    }
                }
                // `x[i..i + c]` with a constant c >= 0
                if let TK::Binary { op: BinOp::Add, l, r } = &strip(hi).kind {
                    let same = |a: &TExpr| match (&strip(a).kind, &strip(lo).kind) {
                        (TK::Local(x), TK::Local(y)) => x == y,
                        _ => false,
                    };
                    if (same(l) && self.bce_const(fs, r).is_some_and(|c| c >= 0)) || (same(r) && self.bce_const(fs, l).is_some_and(|c| c >= 0)) {
                        return true;
                    }
                }
                false
            }
        }
    }

    /// The fact for a loop's variable inside `body`: the counter of
    /// `for v in lo..hi`, or the index of `for v, _ in iter`.
    pub(super) fn bce_loop_fact(&self, fs: &Fs, kind: IterKind, iter: &TExpr, hi: Option<&TExpr>, elem: LocalId, index: Option<LocalId>, body: &TBlock) -> Option<Fact> {
        let (var, lo, hi) = match kind {
            IterKind::Range { inclusive } => {
                let lo = self.bce_lower(fs, iter);
                let hi = hi.and_then(|h| self.bce_upper(fs, h));
                // exclusive: v < hi, so v <= hi - 1
                let hi = if inclusive { hi } else { hi.and_then(|u| u.plus(-1)) };
                (elem, lo, hi)
            }
            IterKind::Vec | IterKind::Slice | IterKind::Array | IterKind::Str => {
                let var = index?;
                let hi = match self.sub(fs, &iter.ty) {
                    Ty::Array(_, n) => Some(Upper::Const(n as i128 - 1)),
                    _ => self.bce_path(iter).map(|p| Upper::Len(p, 1)),
                };
                (var, Some(0), hi)
            }
            _ => return None,
        };
        let hi = match hi {
            Some(Upper::Len(p, m)) if !no_facts(body) && !changes_len(fs.tf, body, &p) => Some(Upper::Len(p, m)),
            Some(Upper::Const(c)) => Some(Upper::Const(c)),
            _ => None,
        };
        if lo.is_none() && hi.is_none() {
            return None;
        }
        Some(Fact { var, lo, hi })
    }
}

impl<'a> Gen<'a> {
    /// The fact for `let v = init` (an immutable local). One relative to a
    /// length holds while `v` lives only if nothing in the whole function can
    /// change that length: `v` is in scope only after `init` ran, and if its
    /// block runs again, so does `init`.
    pub(super) fn bce_let_fact(&self, fs: &mut Fs, var: LocalId, init: &TExpr) -> Option<Fact> {
        if !matches!(fs.tf.locals.get(var as usize).map(|l| l.kind), Some(LocalKind::Let)) || !matches!(self.sub(fs, &init.ty), Ty::Int(_)) {
            return None;
        }
        let lo = self.bce_lower(fs, init);
        let hi = match self.bce_upper(fs, init) {
            Some(Upper::Len(p, m)) => {
                let tf = fs.tf;
                let ok = *fs.bce_steady.entry(p.clone()).or_insert_with(|| !no_facts(&tf.body) && !changes_len(tf, &tf.body, &p));
                ok.then_some(Upper::Len(p, m))
            }
            u => u,
        };
        if lo.is_none() && hi.is_none() {
            return None;
        }
        Some(Fact { var, lo, hi })
    }
}

/// A step from a local to a place, for deciding whether writing one place
/// can change another.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PStep {
    Field(usize),
    Unbox,
    /// An element of a vector, slice, array or string.
    Elem,
    /// Through a map, a projection, a pointer, an rc...
    Other,
}

/// The local a place is in, and the steps to it.
fn chain(e: &TExpr) -> Option<(LocalId, Vec<PStep>)> {
    let (inner, step) = match &e.kind {
        TK::Local(l) => return Some((*l, vec![])),
        TK::Copy(x) | TK::Move(x) | TK::Coerce(_, x) => return chain(x),
        TK::Field { base, index } => (&**base, PStep::Field(*index)),
        TK::Deref(base) => (&**base, if matches!(base.ty, Ty::Box(_)) { PStep::Unbox } else { PStep::Other }),
        TK::Index { base, .. } | TK::StrIndex { base, .. } | TK::SliceOf { base, .. } => (&**base, PStep::Elem),
        TK::MapIndex { base, .. } | TK::OptField { base, .. } => (&**base, PStep::Other),
        TK::Proj { call, .. } => match &call.kind {
            TK::Call { args, .. } | TK::Intrinsic { args, .. } => (&args.first()?.e, PStep::Other),
            _ => return None,
        },
        TK::ProjUnwrap { proj, .. } => (&**proj, PStep::Other),
        _ => return None,
    };
    let (root, mut steps) = chain(inner)?;
    steps.push(step);
    Some((root, steps))
}

/// Whether writing to (or mutably borrowing, or moving out of) place `p` may
/// change the length of `x`. `elem_ok`: writing an element of `x` doesn't.
fn touches(tf: &TFunc, p: &TExpr, x: &Path, elem_ok: bool) -> bool {
    let Some((root, steps)) = chain(p) else {
        return false;
    };
    if root != x.root {
        let owner = |l: LocalId| tf.locals.get(l as usize).and_then(|v| v.ref_root).unwrap_or(l);
        return owner(root) == owner(x.root);
    }
    for (k, s) in steps.iter().enumerate() {
        match (s, x.steps.get(k)) {
            // beyond `x`: an element of it, or something else
            (s, None) => return !(elem_ok && *s == PStep::Elem),
            (PStep::Field(a), Some(Step::Field(b))) if a == b => {}
            (PStep::Field(_), Some(Step::Field(_))) => return false,
            (PStep::Unbox, Some(Step::Unbox)) => {}
            _ => return true,
        }
    }
    // `p` is `x` or contains it
    true
}

/// Whether facts can't be trusted anywhere in `body`.
fn no_facts(body: &TBlock) -> bool {
    let mut bad = false;
    walk_block(body, &mut |n| match n {
        Node::Stmt(TStmt::Yield(..)) => bad = true,
        Node::Expr(e) if matches!(e.kind, TK::Unsafe(_)) => bad = true,
        _ => {}
    });
    bad
}

/// Whether running `body` may change the length of `x` (see the top).
pub(super) fn changes_len(tf: &TFunc, body: &TBlock, x: &Path) -> bool {
    let owner = |l: LocalId| tf.locals.get(l as usize).and_then(|v| v.ref_root).unwrap_or(l);
    let mutable_ref = |l: LocalId| matches!(tf.locals.get(l as usize).map(|v| v.kind), Some(LocalKind::Ref { mutable: true }));
    let mut hit = false;
    walk_block(body, &mut |n| {
        if hit {
            return;
        }
        hit = match n {
            Node::Stmt(s) => match s {
                TStmt::Assign { place, .. } => touches(tf, place, x, true),
                TStmt::MapInsert { map, .. } => touches(tf, map, x, true),
                TStmt::LetRef { local, place } => mutable_ref(*local) && touches(tf, place, x, true),
                TStmt::With { local, place, lock, .. } => (*lock || mutable_ref(*local)) && touches(tf, place, x, true),
                TStmt::WithLocks { places, .. } => places.iter().any(|p| touches(tf, p, x, true)),
                TStmt::For { iter, mutable: true, .. } | TStmt::ParFor { iter, mutable: true, .. } => touches(tf, iter, x, true),
                _ => false,
            },
            Node::Expr(e) => match &e.kind {
                TK::Call { args, .. } | TK::Intrinsic { args, .. } => args.iter().any(|a| a.mode == ArgMode::Inout && touches(tf, &a.e, x, true)),
                TK::Move(p) | TK::Unbox(p) => touches(tf, p, x, false),
                TK::Match { scrut, owned: true, .. } => touches(tf, scrut, x, false),
                TK::MutexRef(p) => touches(tf, p, x, true),
                // a mutable view from a projection may be into any of its arguments
                TK::Proj { mutable: true, call, .. } => match &call.kind {
                    TK::Call { args, .. } | TK::Intrinsic { args, .. } => args.iter().any(|a| touches(tf, &a.e, x, false)),
                    _ => true,
                },
                TK::Closure(c) => c.captures.iter().any(|(l, u)| *u != CapUse::Read && owner(*l) == owner(x.root)),
                _ => false,
            },
        };
    });
    hit
}
