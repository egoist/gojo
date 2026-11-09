// Static drop states (see `analyze`).
//
// A local that is moved somewhere in its function gets a runtime drop flag:
// cleared where it is moved, set where it is assigned, and tested where it
// would be dropped. This pass works out, for each place where codegen drops
// locals, which of those flags have a value known at compile time, so the
// drop is emitted plainly or not at all, and a flag nobody needs to test is
// never declared.
//
// It also moves drops whose only effect is freeing memory (`String`,
// `Vec[int]`, ...: `Program::drop_silent`) earlier, which no program can
// observe (spec 5.3):
//
// - "Join drops": when such a value is moved on only some branches of an `if`
//   or `match`, it is dropped at the end of the branches that didn't move it
//   (the compiler already rejects any use of it after the branches), and
//   after them it is moved on every path.
// - "Last-use frees": it is freed right after the last statement of its
//   block that mentions it, or anything that may borrow it, instead of where
//   the block ends (docs/drops.md).
//
// Soundness: every state recorded here must hold on every path that reaches
// its site. Where the pass can't tell, it records nothing and codegen falls
// back to the flag, which codegen keeps up to date on every path regardless.
// So conditional parts the pass doesn't model precisely (a `select`, a loop
// body run again) make the states they touch unknown, never wrong.

use super::local_path;
use crate::tir::*;
use crate::types::Ty;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

pub type Path = (LocalId, Vec<usize>);

/// Where codegen emits drops of locals. Keys are addresses in the TIR that
/// codegen walks (they don't move while a function is generated).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Site {
    /// The end of a block, after its value: its own locals are dropped.
    BlockEnd(usize),
    /// `return`, `yield`, `break`, `continue` (every local they leave), and
    /// `x = v` (the old value of `x`).
    Stmt(usize),
    /// `try` leaving with an error.
    Expr(usize),
    /// The end of a `match` arm: the values its pattern moved out.
    Arm(usize),
}

impl Site {
    pub fn block(b: &TBlock) -> Site {
        Site::BlockEnd(b as *const TBlock as usize)
    }
    pub fn stmt(s: &TStmt) -> Site {
        Site::Stmt(s as *const TStmt as usize)
    }
    pub fn expr(e: &TExpr) -> Site {
        Site::Expr(e as *const TExpr as usize)
    }
    pub fn arm(a: &TArm) -> Site {
        Site::Arm(a as *const TArm as usize)
    }
}

/// What is known about a flagged path at a site.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Known {
    /// Use the flag.
    Unknown,
    /// No path reaches the site: nothing needs dropping there.
    Dead,
    Live,
    Moved,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum M {
    Live,
    Moved,
    Maybe,
}

enum At {
    Dead,
    /// Only the paths whose state is certain.
    Known(Rc<HashMap<Path, M>>),
}

/// What codegen needs to know about each local.
pub struct LocalFact {
    pub copy: bool,
    pub needs_drop: bool,
    /// Dropping a value of its type only frees memory: nothing a program
    /// can observe (no `drop` method, channel, task, shared count or
    /// unknown code is involved).
    pub silent: bool,
    /// It may hold a borrow: a reference, view, raw pointer, function value...
    pub borrows: bool,
}

#[derive(Default)]
pub struct DropInfo {
    sites: HashMap<Site, At>,
    /// (address of an `if` / `match`, branch index) -> locals to drop at the
    /// end of that branch. An `if` without `else` has branch 1 all the same.
    pub join_drops: HashMap<(usize, usize), Vec<LocalId>>,
    /// (address of a block, statement index) -> locals to free right after
    /// that statement: their last use.
    pub after_stmt: HashMap<(usize, usize), Vec<LocalId>>,
}

impl DropInfo {
    pub fn known(&self, site: Option<Site>, p: &Path) -> Known {
        let Some(site) = site else { return Known::Unknown };
        match self.sites.get(&site) {
            None => Known::Unknown,
            Some(At::Dead) => Known::Dead,
            Some(At::Known(m)) => match m.get(p) {
                Some(M::Live) => Known::Live,
                Some(M::Moved) => Known::Moved,
                _ => Known::Unknown,
            },
        }
    }
}

#[derive(Clone, PartialEq)]
struct St {
    reachable: bool,
    m: HashMap<Path, M>,
}

impl St {
    fn unreachable() -> St {
        St { reachable: false, m: HashMap::new() }
    }

    /// Only the paths `scope` has (what is still declared outside a loop).
    fn within(mut self, scope: &St) -> St {
        self.m.retain(|p, _| scope.m.contains_key(p));
        self
    }

    fn join(&mut self, o: &St) {
        if !o.reachable {
            return;
        }
        if !self.reachable {
            *self = o.clone();
            return;
        }
        for (p, v) in self.m.iter_mut() {
            if o.m.get(p) != Some(v) {
                *v = M::Maybe;
            }
        }
        for p in o.m.keys() {
            self.m.entry(p.clone()).or_insert(M::Maybe);
        }
    }
}

/// `flagged`: the paths codegen keeps drop flags for. `params`: the owned
/// (sink) parameters and receiver, live when the body starts.
/// `may_borrow`: whether a value of this (unsubstituted) type may hold a borrow.
pub fn analyze(body: &TBlock, flagged: &HashSet<Path>, facts: &[LocalFact], params: &[LocalId], may_borrow: &dyn Fn(&Ty) -> bool) -> DropInfo {
    let mut by_local: HashMap<LocalId, Vec<Vec<usize>>> = HashMap::new();
    for (l, p) in flagged {
        by_local.entry(*l).or_default().push(p.clone());
    }
    let mut a = An { by_local, facts, may_borrow, untracked: HashSet::new(), pinned: HashSet::new(), borrowers: HashMap::new(), info: DropInfo::default(), suppress: 0, loop_depth: 0, decl: HashMap::new(), frames: vec![], loops: vec![], params: params.to_vec(), freed: HashSet::new() };
    a.prepare(body);
    let mut st = St { reachable: true, m: HashMap::new() };
    for &l in params {
        a.declare_at(&mut st, l, 0);
    }
    a.block(&mut st, body, true);
    a.info
}

struct An<'a> {
    by_local: HashMap<LocalId, Vec<Vec<usize>>>,
    facts: &'a [LocalFact],
    may_borrow: &'a dyn Fn(&Ty) -> bool,
    /// Never tracked: mentioned in a `defer`, whose moves happen in the
    /// middle of a block's cleanups.
    untracked: HashSet<LocalId>,
    /// Never dropped early: something may still reach them that the pass
    /// can't see (a closure or task, a `defer`, unsafe code, a raw pointer).
    pinned: HashSet<LocalId>,
    /// Locals that may hold a borrow of a local (directly or through others).
    borrowers: HashMap<LocalId, HashSet<LocalId>>,
    info: DropInfo,
    /// Inside a part whose order or conditions aren't modeled: record nothing.
    suppress: u32,
    loop_depth: u32,
    /// Local -> (index in `frames` of the block it belongs to, loop depth).
    decl: HashMap<LocalId, (usize, u32)>,
    /// The blocks around the current point, with the statement being
    /// analyzed (`usize::MAX`: the block's value).
    frames: Vec<(&'a TBlock, usize)>,
    /// The loops around the current point: (id, what `break` leaves with,
    /// what `continue` goes round with).
    loops: Vec<(u32, St, St)>,
    /// The owned parameters: the body's own values, like its `let`s.
    params: Vec<LocalId>,
    /// Locals freed at their last use already.
    freed: HashSet<LocalId>,
}

fn forget(st: &mut St, ls: &HashSet<LocalId>) {
    for (p, v) in st.m.iter_mut() {
        if ls.contains(&p.0) {
            *v = M::Maybe;
        }
    }
}

/// Loops nested deeper than this are analyzed without iterating (each level
/// of iteration multiplies the work of the levels inside it).
const MAX_ITERATED_DEPTH: u32 = 4;

/// Whether `s` has code that leaves it in ways the analysis doesn't follow:
/// a `select` arm, or a loop body compiled as a closure.
fn has_opaque(s: &TStmt) -> bool {
    let mut found = false;
    walk_stmt(s, &mut |n| {
        if let Node::Stmt(TStmt::Select { .. } | TStmt::ForEach { .. } | TStmt::ParFor { .. }) = n {
            found = true;
        }
    });
    found
}

/// Locals an expression mentions, closure captures included.
fn mentions_expr(e: &TExpr, out: &mut HashSet<LocalId>) {
    visit_expr(e, &mut |x| match &x.kind {
        TK::Local(l) => {
            out.insert(*l);
        }
        TK::Closure(c) => out.extend(c.captures.iter().map(|c| c.0)),
        _ => {}
    });
}

fn mentions_stmt(s: &TStmt, out: &mut HashSet<LocalId>) {
    visit_stmt(s, &mut |x| match &x.kind {
        TK::Local(l) => {
            out.insert(*l);
        }
        TK::Closure(c) => out.extend(c.captures.iter().map(|c| c.0)),
        _ => {}
    });
}

fn mentions_block(b: &TBlock, out: &mut HashSet<LocalId>) {
    for s in &b.stmts {
        mentions_stmt(s, out);
    }
    if let Some(v) = &b.value {
        mentions_expr(v, out);
    }
}

/// Whether evaluating `e` can leave code outside it (a `return`, a `try`, a
/// `break` or `continue` of a loop around it) or assign a variable declared
/// outside it: what makes the order of its siblings matter.
fn exits_or_assigns(e: &TExpr) -> bool {
    let mut loops: HashSet<u32> = HashSet::new();
    let mut declared: HashSet<LocalId> = HashSet::new();
    walk_expr(e, &mut |n| match n {
        Node::Stmt(TStmt::While { id, .. } | TStmt::For { id, .. } | TStmt::ForEach { id, .. }) => {
            loops.insert(*id);
        }
        Node::Stmt(TStmt::Let { local, .. }) => {
            declared.insert(*local);
        }
        _ => {}
    });
    let mut found = false;
    walk_expr(e, &mut |n| match n {
        Node::Stmt(TStmt::Return(..) | TStmt::Yield(..)) => found = true,
        Node::Stmt(TStmt::Break(id, _) | TStmt::Continue(id, _)) if !loops.contains(id) => found = true,
        Node::Stmt(TStmt::Assign { place, .. }) if local_path(place).is_none_or(|p| !declared.contains(&p.0)) => found = true,
        Node::Expr(x) if matches!(x.kind, TK::Try { .. }) => found = true,
        _ => {}
    });
    found
}

/// Whether evaluating `e` moves a local (directly, by a consuming `match`,
/// or into a closure that owns what it captures).
fn moves_something(e: &TExpr) -> bool {
    let mut found = false;
    visit_expr(e, &mut |x| match &x.kind {
        TK::Move(_) => found = true,
        TK::Closure(c) if (c.owned && !c.captures.is_empty()) || (c.scoped && !c.by_value.is_empty()) => found = true,
        TK::Match { owned: true, arms, .. } if arms.iter().any(|a| a.consuming) => found = true,
        _ => {}
    });
    found
}

impl<'a> An<'a> {
    fn prepare(&mut self, body: &'a TBlock) {
        // what defers mention, what closures capture, what unsafe code
        // touches; and which locals may borrow which
        let mut edges: Vec<(HashSet<LocalId>, Vec<LocalId>)> = vec![];
        let facts = self.facts;
        let borrows = |l: LocalId| facts.get(l as usize).is_none_or(|f| f.borrows);
        let mut untracked = HashSet::new();
        let mut pinned = HashSet::new();
        walk_block(body, &mut |n| match n {
            Node::Stmt(s) => match s {
                TStmt::Defer(b) | TStmt::ErrDefer(b) => {
                    let mut m = HashSet::new();
                    mentions_block(b, &mut m);
                    untracked.extend(m.iter().copied());
                    pinned.extend(m);
                }
                TStmt::Let { local, init } if borrows(*local) => {
                    let mut m = HashSet::new();
                    mentions_expr(init, &mut m);
                    edges.push((m, vec![*local]));
                }
                TStmt::LetRef { local, place } => {
                    let mut m = HashSet::new();
                    mentions_expr(place, &mut m);
                    edges.push((m, vec![*local]));
                }
                TStmt::Assign { place, value, .. } => {
                    if let Some((d, _)) = local_path(place).filter(|p| borrows(p.0)) {
                        let mut m = HashSet::new();
                        mentions_expr(value, &mut m);
                        edges.push((m, vec![d]));
                    }
                }
                TStmt::For { iter, hi, elem, index, .. } => {
                    let mut m = HashSet::new();
                    mentions_expr(iter, &mut m);
                    if let Some(h) = hi {
                        mentions_expr(h, &mut m);
                    }
                    let mut d = vec![*elem];
                    d.extend(index.iter().copied());
                    edges.push((m, d));
                }
                // `m[k] = v` on a map of views
                TStmt::MapInsert { map, key, value } => {
                    if let Some((d, _)) = local_path(map).filter(|p| borrows(p.0)) {
                        let mut m = HashSet::new();
                        mentions_expr(key, &mut m);
                        mentions_expr(value, &mut m);
                        edges.push((m, vec![d]));
                    }
                }
                TStmt::With { local, place, .. } => {
                    let mut m = HashSet::new();
                    mentions_expr(place, &mut m);
                    edges.push((m, vec![*local]));
                }
                TStmt::WithLocks { locals, places, .. } => {
                    let mut m = HashSet::new();
                    for p in places {
                        mentions_expr(p, &mut m);
                    }
                    edges.push((m, locals.clone()));
                }
                TStmt::Select { arms, .. } => {
                    for a in arms {
                        let (src, bind) = match &a.kind {
                            TSelectKind::Recv { chan, bind } => (chan, bind),
                            TSelectKind::Task { task, bind } => (task, bind),
                            _ => continue,
                        };
                        if let Some(b) = bind {
                            let mut m = HashSet::new();
                            mentions_expr(src, &mut m);
                            edges.push((m, vec![*b]));
                        }
                    }
                }
                _ => {}
            },
            // a raw pointer made from a value (`v.ptr()`) may be used, by
            // unsafe code or by C, after the value's last mention
            Node::Expr(e) if matches!(e.ty, Ty::Ptr(_)) => mentions_expr(e, &mut pinned),
            Node::Expr(e) => match &e.kind {
                TK::Closure(c) => pinned.extend(c.captures.iter().map(|c| c.0)),
                TK::Unsafe(b) => mentions_block(b, &mut pinned),
                TK::Match { scrut, arms, .. } => {
                    let mut m = HashSet::new();
                    mentions_expr(scrut, &mut m);
                    edges.push((m, arms.iter().flat_map(|a| a.binds.iter().copied()).collect()));
                }
                TK::Catch { e: x, bind: Some(b), .. } => {
                    let mut m = HashSet::new();
                    mentions_expr(x, &mut m);
                    edges.push((m, vec![*b]));
                }
                // a variable passed `inout` may be left viewing the other
                // arguments (`ws.push(s)`, `tokenize(src, &toks)`)
                TK::Call { args, .. } | TK::Intrinsic { args, .. } if args.iter().any(|a| a.mode == ArgMode::Inout) => {
                    for (i, a) in args.iter().enumerate() {
                        if a.mode != ArgMode::Inout {
                            continue;
                        }
                        if let Some((d, _)) = local_path(&a.e).filter(|p| borrows(p.0)) {
                            let mut m = HashSet::new();
                            for (j, b) in args.iter().enumerate() {
                                if j != i {
                                    mentions_expr(&b.e, &mut m);
                                }
                            }
                            edges.push((m, vec![d]));
                        }
                    }
                }
                TK::Try { e: x, wrap: Some(TryWrap::Call(h, _)) } => {
                    let mut m = HashSet::new();
                    mentions_expr(x, &mut m);
                    edges.push((m, vec![*h]));
                }
                _ => {}
            },
        });
        // local -> locals bound from expressions that mention it
        let mut direct: HashMap<LocalId, HashSet<LocalId>> = HashMap::new();
        for (srcs, dsts) in &edges {
            for &s in srcs {
                for &d in dsts {
                    if d != s && borrows(d) {
                        direct.entry(s).or_default().insert(d);
                    }
                }
            }
        }
        // transitively: a view of a view of `s` borrows `s`
        for &s in direct.keys() {
            let mut seen: HashSet<LocalId> = HashSet::new();
            let mut todo: Vec<LocalId> = direct[&s].iter().copied().collect();
            while let Some(x) = todo.pop() {
                // (`seen` is the answer: every borrower goes in, with or
                // without borrowers of its own)
                if !seen.insert(x) {
                    continue;
                }
                if let Some(n) = direct.get(&x) {
                    todo.extend(n.iter().copied());
                }
            }
            self.borrowers.insert(s, seen);
        }
        self.untracked = untracked;
        self.pinned = pinned;
    }

    fn record(&mut self, site: Site, st: &St) {
        if self.suppress > 0 {
            return;
        }
        let at = if st.reachable { At::Known(Rc::new(st.m.iter().filter(|(_, v)| **v != M::Maybe).map(|(p, v)| (p.clone(), *v)).collect())) } else { At::Dead };
        // a site reached twice (the two copies of a counted loop's body):
        // keep what both agree on
        let merged = match (self.info.sites.remove(&site), at) {
            (None, a) | (Some(At::Dead), a) | (Some(a), At::Dead) => a,
            (Some(At::Known(x)), At::Known(y)) => At::Known(Rc::new(x.iter().filter(|(p, v)| y.get(*p) == Some(*v)).map(|(p, v)| (p.clone(), *v)).collect())),
        };
        self.info.sites.insert(site, merged);
    }

    fn declare_at(&mut self, st: &mut St, l: LocalId, frame: usize) {
        self.decl.insert(l, (frame, self.loop_depth));
        if self.untracked.contains(&l) {
            return;
        }
        if let Some(ps) = self.by_local.get(&l) {
            for p in ps {
                st.m.insert((l, p.clone()), M::Live);
            }
        }
    }

    fn declare(&mut self, st: &mut St, l: LocalId) {
        let f = self.frames.len().saturating_sub(1);
        self.declare_at(st, l, f);
    }

    fn undeclare(&mut self, st: &mut St, l: LocalId) {
        st.m.retain(|p, _| p.0 != l);
    }

    /// Mirrors codegen: a move clears the flag of exactly that path.
    fn mv(&mut self, st: &mut St, p: &Path) {
        if let Some(v) = st.m.get_mut(p) {
            *v = M::Moved;
        }
    }

    /// Mirrors codegen: an assignment sets the flags of the path and the
    /// paths inside it.
    fn reinit(&mut self, st: &mut St, p: &Path) {
        for (q, v) in st.m.iter_mut() {
            if q.0 == p.0 && q.1.starts_with(&p.1) {
                *v = M::Live;
            }
        }
    }

    /// Make every tracked path of these locals unknown.
    fn forget(&mut self, st: &mut St, ls: &HashSet<LocalId>) {
        forget(st, ls)
    }

    fn block(&mut self, st: &mut St, b: &'a TBlock, top: bool) {
        self.frames.push((b, 0));
        let mut declared = vec![];
        // (the function's owned parameters belong to its body)
        let owned: Vec<LocalId> = if self.frames.len() == 1 { self.params.clone() } else { vec![] };
        for (i, s) in b.stmts.iter().enumerate() {
            self.frames.last_mut().unwrap().1 = i;
            self.stmt(st, s, top);
            if let TStmt::Let { local, .. } = s {
                declared.push(*local);
            }
            if self.suppress == 0 && st.reachable {
                self.free_after(st, b, i, owned.iter().chain(&declared).copied());
            }
        }
        self.frames.last_mut().unwrap().1 = usize::MAX;
        if let Some(v) = &b.value {
            self.expr(st, v, top);
        }
        self.record(Site::block(b), st);
        for l in declared {
            self.undeclare(st, l);
        }
        self.frames.pop();
    }

    /// Free the memory of the block's own values that statement `i` used
    /// last: those whose drop only frees memory, held on every path, that no
    /// later statement (or the block's value) mentions, and that nothing
    /// pinned could reach (see `pinned`). The same goes for any variable that
    /// may borrow them.
    fn free_after(&mut self, st: &mut St, b: &'a TBlock, i: usize, ls: impl Iterator<Item = LocalId>) {
        let ls: Vec<LocalId> = ls.filter(|l| !self.freed.contains(l)).collect();
        if ls.is_empty() {
            return;
        }
        let mut rest = HashSet::new();
        for s in &b.stmts[i + 1..] {
            mentions_stmt(s, &mut rest);
        }
        if let Some(v) = &b.value {
            mentions_expr(v, &mut rest);
        }
        for l in ls {
            let Some(f) = self.facts.get(l as usize) else { continue };
            if !f.silent || !f.needs_drop || self.untracked.contains(&l) {
                continue;
            }
            let mut watch: HashSet<LocalId> = HashSet::new();
            watch.insert(l);
            if let Some(bs) = self.borrowers.get(&l) {
                watch.extend(bs.iter().copied());
            }
            if watch.iter().any(|x| self.pinned.contains(x) || rest.contains(x)) || self.dropped_with_code(l, &watch) {
                continue;
            }
            // held on every path: never moved, or known to be there now
            match self.by_local.get(&l) {
                None => {}
                Some(ps) if ps.len() == 1 && ps[0].is_empty() => {
                    if st.m.get(&(l, vec![])) != Some(&M::Live) {
                        continue;
                    }
                    st.m.insert((l, vec![]), M::Moved);
                }
                // (parts moved separately)
                Some(_) => continue,
            }
            self.freed.insert(l);
            self.info.after_stmt.entry((b as *const TBlock as usize, i)).or_default().push(l);
        }
    }

    /// Sub-expressions evaluated one after another. Codegen visits them in
    /// this order; where the order could matter (one of them leaves early
    /// or assigns, another moves), nothing inside is recorded and the
    /// locals they mention become unknown, so the order no longer does.
    fn seq(&mut self, st: &mut St, xs: &[&'a TExpr]) {
        let hazard = xs.len() > 1 && xs.iter().enumerate().any(|(i, a)| exits_or_assigns(a) && xs.iter().enumerate().any(|(j, b)| i != j && moves_something(b)));
        if !hazard {
            for x in xs {
                self.expr(st, x, false);
            }
            return;
        }
        self.suppress += 1;
        for x in xs {
            self.expr(st, x, false);
        }
        self.suppress -= 1;
        let mut m = HashSet::new();
        for x in xs {
            mentions_expr(x, &mut m);
        }
        self.forget(st, &m);
        // (and in the states a `break` or `continue` in there left with)
        for l in &mut self.loops {
            forget(&mut l.1, &m);
            forget(&mut l.2, &m);
        }
    }

    fn stmt(&mut self, st: &mut St, s: &'a TStmt, top: bool) {
        match s {
            TStmt::Let { local, init } => {
                self.expr(st, init, top);
                self.declare(st, *local);
            }
            TStmt::LetRef { place, .. } => self.expr(st, place, false),
            TStmt::Expr(e) => self.expr(st, e, top),
            TStmt::Assign { place, op, value, .. } => {
                // codegen: the value, then the place, then the old value's drop
                self.seq(st, &[value, place]);
                if op.is_none() {
                    self.record(Site::stmt(s), st);
                    if let Some(p) = local_path(place) {
                        self.reinit(st, &p);
                    }
                }
            }
            TStmt::MapInsert { map, key, value } => self.seq(st, &[key, value, map]),
            TStmt::Return(e, _) | TStmt::Yield(e, _) => {
                // (a returned value is the whole statement too: nothing else
                // in it runs after the value is computed)
                if let Some(e) = e {
                    self.expr(st, e, top && matches!(s, TStmt::Return(..)));
                }
                self.record(Site::stmt(s), st);
                st.reachable = false;
            }
            TStmt::Break(id, _) | TStmt::Continue(id, _) => {
                self.record(Site::stmt(s), st);
                let brk = matches!(s, TStmt::Break(..));
                if let Some(l) = self.loops.iter_mut().rev().find(|l| l.0 == *id) {
                    if brk {
                        l.1.join(st);
                    } else {
                        l.2.join(st);
                    }
                }
                st.reachable = false;
            }
            // what defers mention is never tracked
            TStmt::Defer(_) | TStmt::ErrDefer(_) => {}
            TStmt::While { id, cond, body } => self.looped(st, s, *id, cond.as_ref(), body, &[]),
            TStmt::For { id, kind, iter, hi, elem, body, .. } => {
                let mut xs: Vec<&TExpr> = vec![iter];
                if let Some(h) = hi {
                    xs.push(h);
                }
                self.seq(st, &xs);
                // only a received value is owned (and flagged)
                let binds = if *kind == IterKind::Chan { vec![*elem] } else { vec![] };
                self.looped(st, s, *id, None, body, &binds);
            }
            // codegen: the value, then the channel
            TStmt::Send { chan, value } => self.seq(st, &[value, chan]),
            TStmt::With { place, body, .. } => {
                self.expr(st, place, false);
                self.block(st, body, top);
            }
            TStmt::WithLocks { places, body, .. } => {
                let xs: Vec<&TExpr> = places.iter().collect();
                self.seq(st, &xs);
                self.block(st, body, top);
            }
            // arms that may or may not run, loop bodies compiled as closures:
            // what they mention becomes unknown
            TStmt::Select { .. } | TStmt::ForEach { .. } | TStmt::ParFor { .. } => {
                let mut m = HashSet::new();
                mentions_stmt(s, &mut m);
                self.forget(st, &m);
            }
        }
    }

    /// A loop. The state at its start joins the state before it with the
    /// states each run of the body ends in (at its end or at a `continue`):
    /// found by running the analysis over the body until that stops
    /// changing (it can only grow towards "unknown", so it ends), then once
    /// more for real. It ends where the condition fails (a `for` where the
    /// elements run out) or at a `break`.
    fn looped(&mut self, st: &mut St, s: &'a TStmt, id: u32, cond: Option<&'a TExpr>, body: &'a TBlock, binds: &[LocalId]) {
        let ends_at_head = cond.is_some() || matches!(s, TStmt::For { .. });
        if self.loop_depth < MAX_ITERATED_DEPTH && !has_opaque(s) {
            let entry = st.clone();
            let mut head = entry.clone();
            for _ in 0..8 {
                self.suppress += 1;
                let (back, _) = self.loop_pass(&head, id, cond, body, binds, ends_at_head);
                self.suppress -= 1;
                let mut next = entry.clone();
                next.join(&back);
                if next == head {
                    let (_, exit) = self.loop_pass(&head, id, cond, body, binds, ends_at_head);
                    *st = exit;
                    return;
                }
                head = next;
            }
        }
        // Without iterating: what the loop moves or assigns is unknown at
        // its start and after it; everything else keeps its state.
        let mut touched = HashSet::new();
        walk_stmt(s, &mut |n| match n {
            Node::Expr(e) => match &e.kind {
                TK::Move(x) => {
                    if let Some(p) = local_path(x) {
                        touched.insert(p.0);
                    }
                }
                TK::Match { scrut, owned: true, .. } => {
                    if let Some(p) = local_path(scrut) {
                        touched.insert(p.0);
                    }
                }
                TK::Closure(c) => touched.extend(c.captures.iter().map(|c| c.0)),
                _ => {}
            },
            Node::Stmt(TStmt::Assign { place, .. }) => {
                if let Some(p) = local_path(place) {
                    touched.insert(p.0);
                }
            }
            Node::Stmt(x @ (TStmt::Select { .. } | TStmt::ForEach { .. } | TStmt::ParFor { .. })) => mentions_stmt(x, &mut touched),
            _ => {}
        });
        self.forget(st, &touched);
        let head = st.clone();
        self.loop_pass(&head, id, cond, body, binds, ends_at_head);
        // the loop may end at any iteration
        *st = head;
    }

    /// One run of a loop's body from `head`: the state going round again,
    /// and the state leaving the loop.
    fn loop_pass(&mut self, head: &St, id: u32, cond: Option<&'a TExpr>, body: &'a TBlock, binds: &[LocalId], ends_at_head: bool) -> (St, St) {
        self.loops.push((id, St::unreachable(), St::unreachable()));
        self.loop_depth += 1;
        let mut b = head.clone();
        if let Some(c) = cond {
            self.expr(&mut b, c, false);
        }
        let mut exit = if ends_at_head { b.clone() } else { St::unreachable() };
        let f = self.frames.len();
        for &l in binds {
            self.declare_at(&mut b, l, f);
        }
        self.block(&mut b, body, true);
        for &l in binds {
            self.undeclare(&mut b, l);
        }
        self.loop_depth -= 1;
        let (_, brk, cont) = self.loops.pop().unwrap();
        // (what the body declared is gone by then)
        b.join(&cont.within(head));
        exit.join(&brk.within(head));
        (b.within(head), exit)
    }

    fn expr(&mut self, st: &mut St, e: &'a TExpr, top: bool) {
        match &e.kind {
            TK::Unit | TK::Int(_) | TK::Float(_) | TK::Bool(_) | TK::Str(_) | TK::NoneLit | TK::Local(_) | TK::Const(_) | TK::Global(_) | TK::ConstParam(_) | TK::FuncRef(..) | TK::DefaultOf | TK::Error | TK::StaticFieldName(_) => {}
            TK::StaticField { base: x, .. } | TK::Field { base: x, .. } | TK::Deref(x) | TK::Copy(x) | TK::OptField { base: x, .. } | TK::Unary { e: x, .. } | TK::Cast(x) | TK::SomeLit(x) | TK::OkLit(x) | TK::ErrLit(x) | TK::Coerce(_, x) | TK::Recv(x) | TK::Wrap(x) | TK::MakeChan(x) | TK::Unbox(x) | TK::MutexRef(x) | TK::Proj { call: x, .. } => self.expr(st, x, false),
            TK::Move(x) => {
                if let Some(p) = local_path(x) {
                    self.mv(st, &p);
                }
                self.expr(st, x, false);
            }
            TK::Index { base, index } | TK::StrIndex { base, index } => self.seq(st, &[base, index]),
            TK::MapIndex { base, key } => self.seq(st, &[base, key]),
            TK::SliceOf { base, lo, hi } => {
                let mut xs: Vec<&TExpr> = vec![base];
                xs.extend(lo.as_deref());
                xs.extend(hi.as_deref());
                self.seq(st, &xs);
            }
            TK::Call { args, .. } | TK::Intrinsic { args, .. } => {
                let xs: Vec<&TExpr> = args.iter().map(|a| &a.e).collect();
                self.seq(st, &xs);
            }
            TK::ProjUnwrap { proj, msg } => {
                self.expr(st, proj, false);
                if let Some(m) = msg {
                    let before = st.clone();
                    self.expr(st, m, false);
                    st.join(&before);
                }
            }
            TK::Binary { op: BinOp::And | BinOp::Or, l, r } | TK::Coalesce { l, r } => {
                self.expr(st, l, false);
                let before = st.clone();
                self.expr(st, r, false);
                st.join(&before);
            }
            TK::Binary { l, r, .. } => self.seq(st, &[l, r]),
            TK::StructLit { fields: xs, .. } | TK::EnumLit { args: xs, .. } | TK::VecLit(xs) | TK::ArrayLit(xs) => {
                let xs: Vec<&TExpr> = xs.iter().collect();
                self.seq(st, &xs);
            }
            TK::MapLit(ps) => {
                let xs: Vec<&TExpr> = ps.iter().flat_map(|(k, v)| [k, v]).collect();
                self.seq(st, &xs);
            }
            TK::Closure(c) => {
                // mirrors codegen: what an escaping closure or a task owns is
                // moved into it where the closure is made (its body is
                // compiled on its own, with flags)
                for (l, _) in &c.captures {
                    let moved = c.owned || (c.scoped && c.by_value.contains(l));
                    if moved && !self.facts.get(*l as usize).is_some_and(|f| f.copy) {
                        self.mv(st, &(*l, vec![]));
                    }
                }
            }
            TK::If { cond, then, else_ } => {
                self.expr(st, cond, false);
                let mut a = st.clone();
                self.block(&mut a, then, top);
                let mut b = st.clone();
                if let Some(eb) = else_ {
                    self.block(&mut b, eb, top);
                }
                *st = self.join_branches(e, vec![a, b], top);
            }
            TK::Match { scrut, owned, arms } => self.match_(st, e, scrut, *owned, arms, top),
            TK::Block(b) | TK::Unsafe(b) | TK::Isolate(b) | TK::Scope { body: b, .. } => self.block(st, b, top),
            TK::Spawn { target, task, .. } => {
                let mut xs: Vec<&TExpr> = vec![];
                if let SpawnTarget::Value(v) = target {
                    xs.push(v);
                }
                xs.push(task);
                self.seq(st, &xs);
            }
            TK::Try { e: x, wrap } => {
                self.expr(st, x, false);
                // leaving with the error (after building it)
                let mut out = st.clone();
                if let Some(TryWrap::Call(_, w)) = wrap {
                    self.suppress += 1;
                    self.expr(&mut out, w, false);
                    self.suppress -= 1;
                }
                self.record(Site::expr(e), &out);
            }
            TK::Catch { e: x, bind, handler } => {
                self.expr(st, x, false);
                let mut h = st.clone();
                let f = self.frames.len();
                if let Some(b) = bind {
                    self.declare_at(&mut h, *b, f);
                }
                self.block(&mut h, handler, false);
                if let Some(b) = bind {
                    self.undeclare(&mut h, *b);
                }
                st.join(&h);
            }
        }
        if e.ty == Ty::Never {
            st.reachable = false;
        }
    }

    fn match_(&mut self, st: &mut St, e: &'a TExpr, scrut: &'a TExpr, owned: bool, arms: &'a [TArm], top: bool) {
        self.expr(st, scrut, false);
        // codegen clears the flag of an owned place matched by a consuming arm
        let scrut_path = if owned && scrut.is_place() { local_path(scrut) } else { None };
        // what reaches the next arm's test: no earlier arm matched, or its
        // guard was false
        let mut next = st.clone();
        let mut ends = vec![];
        for a in arms {
            let mut s = next.clone();
            if let Some(g) = &a.guard {
                self.expr(&mut s, g, false);
                next.join(&s);
            }
            let f = self.frames.len();
            if a.consuming {
                if let Some(p) = &scrut_path {
                    self.mv(&mut s, p);
                }
                for &b in &a.binds {
                    self.declare_at(&mut s, b, f);
                }
            }
            self.expr(&mut s, &a.body, top);
            self.record(Site::arm(a), &s);
            for &b in &a.binds {
                self.undeclare(&mut s, b);
            }
            ends.push(s);
        }
        // no arm matched: a panic
        *st = self.join_branches(e, ends, top);
    }

    /// Join the states at the ends of an `if`'s or `match`'s branches,
    /// first dropping what some branches moved at the end of the others,
    /// where that can't be told apart from dropping it later.
    fn join_branches(&mut self, e: &TExpr, mut ends: Vec<St>, top: bool) -> St {
        let reach: Vec<usize> = (0..ends.len()).filter(|&i| ends[i].reachable).collect();
        // `top`: the construct is a whole statement (or the value of a block
        // that is), so nothing else in its statement runs after it.
        // A value that may borrow could point into what would be dropped.
        if top && self.suppress == 0 && reach.len() > 1 && !(self.may_borrow)(&e.ty) && !self.assigns_borrower(e) {
            let mut cands: Vec<Path> = ends[reach[0]].m.keys().filter(|p| p.1.is_empty()).cloned().collect();
            cands.sort();
            for p in cands {
                let vals: Vec<Option<M>> = reach.iter().map(|&i| ends[i].m.get(&p).copied()).collect();
                if !vals.iter().all(|v| matches!(v, Some(M::Live | M::Moved))) || !vals.contains(&Some(M::Moved)) || !vals.contains(&Some(M::Live)) {
                    continue;
                }
                if !self.can_drop_early(p.0) {
                    continue;
                }
                let key = e as *const TExpr as usize;
                for &i in &reach {
                    if ends[i].m.get(&p) == Some(&M::Live) {
                        self.info.join_drops.entry((key, i)).or_default().push(p.0);
                        ends[i].m.insert(p.clone(), M::Moved);
                    }
                }
            }
        }
        let mut out = St { reachable: false, m: HashMap::new() };
        for s in &ends {
            out.join(s);
        }
        out
    }

    /// An assignment inside `e` to a variable that may hold a borrow: it
    /// could now point into a value of the other branches.
    fn assigns_borrower(&self, e: &TExpr) -> bool {
        let mut found = false;
        walk_expr(e, &mut |n| {
            if let Node::Stmt(TStmt::Assign { place, .. }) = n {
                found |= local_path(place).is_none_or(|(l, _)| self.facts.get(l as usize).is_none_or(|f| f.borrows));
            }
        });
        found
    }

    /// Whether something that may borrow `l` has a drop that runs code (a
    /// `drop` method that may reach `l` through a raw pointer): that drop, at
    /// the end of its scope, uses `l`.
    fn dropped_with_code(&self, l: LocalId, watch: &HashSet<LocalId>) -> bool {
        watch.iter().any(|&x| x != l && self.facts.get(x as usize).is_none_or(|f| f.needs_drop && !f.silent))
    }

    /// Whether `l`, moved on some branches just joined and live on the
    /// others, can be dropped at the end of those others.
    fn can_drop_early(&self, l: LocalId) -> bool {
        let Some(f) = self.facts.get(l as usize) else { return false };
        if !f.silent || !f.needs_drop || self.untracked.contains(&l) {
            return false;
        }
        // only the whole value is flagged (no partial moves to account for)
        if self.by_local.get(&l).is_none_or(|ps| ps.len() != 1 || !ps[0].is_empty()) {
            return false;
        }
        let Some(&(frame, depth)) = self.decl.get(&l) else { return false };
        // a loop around the branches, inside `l`'s scope, may run them again
        if depth != self.loop_depth {
            return false;
        }
        let mut watch: HashSet<LocalId> = HashSet::new();
        watch.insert(l);
        if let Some(bs) = self.borrowers.get(&l) {
            watch.extend(bs.iter().copied());
        }
        if watch.iter().any(|x| self.pinned.contains(x)) || self.dropped_with_code(l, &watch) {
            return false;
        }
        // Nothing after the branches, up to the end of `l`'s scope, may
        // mention it or anything that may borrow it. (The compiler rejects
        // such uses of a value that may have been moved; this keeps the
        // early drop safe even where its view of the branches is
        // approximate.)
        for k in frame..self.frames.len() {
            let (b, i) = self.frames[k];
            if i == usize::MAX {
                continue;
            }
            let mut m = HashSet::new();
            for s in &b.stmts[i + 1..] {
                mentions_stmt(s, &mut m);
            }
            if let Some(v) = &b.value {
                mentions_expr(v, &mut m);
            }
            if m.iter().any(|x| watch.contains(x)) {
                return false;
            }
        }
        true
    }
}
