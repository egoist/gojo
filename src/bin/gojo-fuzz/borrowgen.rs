// Small random programs that juggle owners and views of them, for attacking
// the borrow checker. They are not valid by construction: most are rejected,
// which is the point. Every one the compiler accepts must be memory-safe, so
// running it under AddressSanitizer checks the checker.

use super::Rng;

const STR_LITS: &[&str] = &["\"a\"", "\"abcdefghijklmnopqrstuvwxyz0123456789\"", "\"\"", "\"xy\""];

pub struct BorrowGen<'a> {
    rng: &'a mut Rng,
    out: String,
    ind: usize,
    /// owned strings, owned vectors, views of strings, views of vectors,
    /// vectors of strings: the variables declared so far (by name)
    strs: Vec<String>,
    vecs: Vec<String>,
    svs: Vec<String>,
    vvs: Vec<String>,
    lists: Vec<String>,
    funcs: Vec<String>,
    /// structs with a String field, maps of strings
    recs: Vec<String>,
    maps: Vec<String>,
    /// `?Task[String]` and `?Scope` variables, which a scope may try to leave
    /// its handles or itself in
    handles: Vec<String>,
    scopes: Vec<String>,
    n: u32,
    depth: u32,
}

impl<'a> BorrowGen<'a> {
    pub fn new(rng: &'a mut Rng) -> BorrowGen<'a> {
        BorrowGen { rng, out: String::new(), ind: 1, strs: vec![], vecs: vec![], svs: vec![], vvs: vec![], lists: vec![], funcs: vec![], recs: vec![], maps: vec![], handles: vec![], scopes: vec![], n: 0, depth: 0 }
    }

    fn fresh(&mut self, p: &str) -> String {
        self.n += 1;
        format!("{}{}", p, self.n)
    }

    fn line(&mut self, s: &str) {
        self.out.push_str(&"    ".repeat(self.ind));
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn pick(&mut self, xs: &[String]) -> Option<String> {
        if xs.is_empty() {
            None
        } else {
            Some(xs[self.rng.below(xs.len())].clone())
        }
    }

    fn str_lit(&mut self) -> String {
        format!("{}.toString()", self.rng.pick(STR_LITS))
    }

    /// An expression for a view of a string.
    fn str_view(&mut self) -> String {
        let strs = self.strs.clone();
        let svs = self.svs.clone();
        let lists = self.lists.clone();
        let funcs = self.funcs.clone();
        match self.rng.below(7) {
            0 | 1 => match self.pick(&strs) {
                Some(s) => format!("{}.asStr()", s),
                None => "\"lit\"".into(),
            },
            2 => match self.pick(&svs) {
                Some(w) => if self.rng.chance(1, 2) { w } else { format!("{}.trim()", w) },
                None => "\"lit\"".into(),
            },
            3 => match self.pick(&strs) {
                Some(s) => format!("{}[0..{}.len()]", s, s),
                None => "\"lit\"".into(),
            },
            4 => match self.pick(&lists) {
                Some(l) => format!("(if {}.len() > 0 {{ {}[0].asStr() }} else {{ \"e\" }})", l, l),
                None => "\"lit\"".into(),
            },
            5 => match self.pick(&funcs) {
                Some(f) => format!("{}()", f),
                None => match self.pick(&lists) {
                    Some(l) => format!("firstOf({})", l),
                    None => "\"lit\"".into(),
                },
            },
            _ => {
                let recs = self.recs.clone();
                if self.rng.chance(1, 2) {
                    if let Some(r) = self.pick(&recs) {
                        return format!("{}.name.asStr()", r);
                    }
                }
                match self.pick(&strs) {
                    Some(s) => format!("run(() => {}.asStr())", s),
                    None => "\"lit\"".into(),
                }
            }
        }
    }

    fn vec_view(&mut self) -> String {
        let vecs = self.vecs.clone();
        let vvs = self.vvs.clone();
        match self.rng.below(3) {
            0 => match self.pick(&vecs) {
                Some(v) => v,
                None => "e0".into(),
            },
            1 => match self.pick(&vecs) {
                Some(v) => format!("{}[0..{}.len()]", v, v),
                None => "e0".into(),
            },
            _ => match self.pick(&vvs) {
                Some(w) => w,
                None => "e0".into(),
            },
        }
    }

    fn stmt(&mut self) {
        let strs = self.strs.clone();
        let vecs = self.vecs.clone();
        let svs = self.svs.clone();
        let vvs = self.vvs.clone();
        let lists = self.lists.clone();
        match self.rng.below(30) {
            0 | 1 => {
                let n = self.fresh("s");
                let e = self.str_lit();
                self.line(&format!("var {} = {}", n, e));
                self.strs.push(n);
            }
            2 => {
                let n = self.fresh("v");
                self.line(&format!("var {} = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]", n));
                self.vecs.push(n);
            }
            3 => {
                let n = self.fresh("l");
                let (a, b) = (self.str_lit(), self.str_lit());
                self.line(&format!("var {} = [{}, {}]", n, a, b));
                self.lists.push(n);
            }
            4 | 5 => {
                let n = self.fresh("w");
                let e = self.str_view();
                self.line(&format!("var {}: str = {}", n, e));
                self.svs.push(n);
            }
            6 => {
                let n = self.fresh("u");
                let e = self.vec_view();
                self.line(&format!("var {}: []int = {}", n, e));
                self.vvs.push(n);
            }
            7 | 8 => {
                // reassign a view
                if let Some(w) = self.pick(&svs) {
                    let e = self.str_view();
                    self.line(&format!("{} = {}", w, e));
                } else if let Some(u) = self.pick(&vvs) {
                    let e = self.vec_view();
                    self.line(&format!("{} = {}", u, e));
                }
            }
            9 | 10 => {
                // change an owner
                if let Some(s) = self.pick(&strs) {
                    let e = match self.rng.below(5) {
                        0 => format!("{}.push(\"a much longer piece of text that makes the buffer grow\")", s),
                        1 => format!("{}.clear()", s),
                        2 => format!("{} = {}", s, self.str_lit()),
                        3 => format!("drop({})", s),
                        _ => format!("take({})", s),
                    };
                    self.line(&e);
                }
            }
            11 => {
                if let Some(v) = self.pick(&vecs) {
                    let e = match self.rng.below(4) {
                        0 => format!("for i in 0..100 {{ {}.push(i) }}", v),
                        1 => format!("{}.clear()", v),
                        2 => format!("{} = [5]", v),
                        _ => format!("drop({})", v),
                    };
                    self.line(&e);
                } else if let Some(l) = self.pick(&lists) {
                    let e = if self.rng.chance(1, 2) { format!("{}.clear()", l) } else { format!("{}.push({})", l, self.str_lit()) };
                    self.line(&e);
                }
            }
            12 | 13 => {
                // use a view
                if let Some(w) = self.pick(&svs) {
                    self.line(&format!("println({})", w));
                } else if let Some(u) = self.pick(&vvs) {
                    self.line(&format!("println({}.len())", u));
                }
            }
            14 if self.depth < 3 => {
                self.line("{");
                self.block();
                self.line("}");
            }
            15 if self.depth < 3 => {
                let c = if self.rng.chance(1, 2) { "e0.len() > 0" } else { "e0.len() > 99" };
                self.line(&format!("if {} {{", c));
                self.block();
                self.line("}");
            }
            16 | 25 | 26 if self.depth < 3 => {
                // loops are where views outlive what they saw (several
                // iterations, a `break` before or after reassigning)
                let i = self.fresh("i");
                self.line(&format!("for {} in 0..3 {{", i));
                self.ind += 1;
                match self.rng.below(3) {
                    0 => self.line(&format!("if {} == 2 {{ break }}", i)),
                    1 => self.line(&format!("if {} == 1 {{ continue }}", i)),
                    _ => self.line(&format!("println({})", i)),
                }
                self.ind -= 1;
                self.block();
                self.line("}");
            }
            17 => {
                let what = match self.rng.below(4) {
                    0 => self.pick(&svs).map(|w| format!("println({})", w)),
                    1 => self.pick(&strs).map(|s| format!("{}.push(\"x\")", s)),
                    // moves where the block ends, or where a jump leaves it
                    2 => self.pick(&strs).map(|s| format!("take({})", s)),
                    _ => self.pick(&strs).map(|s| format!("drop({})", s)),
                };
                self.line(&format!("defer {}", what.unwrap_or_else(|| "println(0)".into())));
                if self.depth > 0 && self.rng.chance(1, 3) {
                    let j = *self.rng.pick(&["break", "continue", "return"]);
                    self.line(&format!("if e0.len() > 0 {{ {} }}", j));
                }
            }
            18 => {
                if let Some(s) = self.pick(&strs) {
                    let f = self.fresh("f");
                    self.line(&format!("let {} = () => {}.asStr()", f, s));
                    self.funcs.push(f);
                }
            }
            19 if self.depth < 3 => {
                if let Some(l) = self.pick(&lists) {
                    let e = self.fresh("e");
                    self.line(&format!("for {} in {} {{", e, l));
                    self.ind += 1;
                    if let Some(w) = self.pick(&svs) {
                        self.line(&format!("{} = {}.asStr()", w, e));
                    } else {
                        self.line(&format!("println({})", e));
                    }
                    if self.rng.chance(1, 3) {
                        self.line("break");
                    }
                    self.ind -= 1;
                    self.line("}");
                }
            }
            20 => {
                if let Some(s) = self.pick(&strs) {
                    let (mu, g) = (self.fresh("mu"), self.fresh("g"));
                    self.line(&format!("let {} = Mutex({}.clone())", mu, s));
                    self.line(&format!("with {} = {}.lock() {{", g, mu));
                    self.ind += 1;
                    match self.pick(&svs) {
                        Some(w) => self.line(&format!("{} = {}.asStr()", w, g)),
                        None => self.line(&format!("println({})", g)),
                    }
                    self.ind -= 1;
                    self.line("}");
                }
            }
            21 => {
                if let (Some(a), Some(b)) = (self.pick(&strs), self.pick(&strs)) {
                    if a != b {
                        self.line(&format!("swap(&{}, &{})", a, b));
                    }
                }
            }
            22 => {
                if let Some(s) = self.pick(&strs) {
                    let w = self.pick(&svs);
                    match w {
                        Some(w) => self.line(&format!("{} = pick(&{})", w, s)),
                        None => self.line(&format!("println(pick(&{}))", s)),
                    }
                }
            }
            23 => {
                // records and maps
                let recs = self.recs.clone();
                let maps = self.maps.clone();
                match self.rng.below(6) {
                    0 => {
                        let n = self.fresh("p");
                        let e = self.str_lit();
                        self.line(&format!("var {} = P{{name: {}}}", n, e));
                        self.recs.push(n);
                    }
                    1 => {
                        let n = self.fresh("m");
                        let e = self.str_lit();
                        self.line(&format!("var {} = Map[int, String]{{1: {}}}", n, e));
                        self.maps.push(n);
                    }
                    2 => {
                        if let Some(r) = self.pick(&recs) {
                            let e = if self.rng.chance(1, 2) { format!("{}.name.push(\"grow it with a long piece of text\")", r) } else { format!("{} = P{{name: {}}}", r, self.str_lit()) };
                            self.line(&e);
                        }
                    }
                    3 => {
                        if let Some(m) = self.pick(&maps) {
                            let x = self.fresh("x");
                            self.line(&format!("if let {} = {}.get(1) {{", x, m));
                            self.ind += 1;
                            match self.pick(&svs) {
                                Some(w) => self.line(&format!("{} = {}.asStr()", w, x)),
                                None => self.line(&format!("println({})", x)),
                            }
                            self.ind -= 1;
                            self.line("}");
                        }
                    }
                    4 => {
                        if let Some(m) = self.pick(&maps) {
                            let e = if self.rng.chance(1, 2) { format!("{}.clear()", m) } else { format!("_ = {}.insert(1, {})", m, self.str_lit()) };
                            self.line(&e);
                        }
                    }
                    _ => {
                        if let Some(s) = self.pick(&strs) {
                            self.line(&format!("grow(&{})", s));
                        }
                    }
                }
            }
            27 if self.depth < 3 => {
                // custom iterators: a cursor over `self`, a lending cursor,
                // and a push-style `each`
                let kind = *self.rng.pick(&["Ring", "Lend", "Each"]);
                let it = self.fresh("it");
                let e = self.fresh("e");
                let init = if kind == "Lend" { "Lend{n: 3}".to_string() } else { format!("{}{{items: [{}, {}]}}", kind, self.str_lit(), self.str_lit()) };
                self.line(&format!("let {} = {}", it, init));
                self.line(&format!("for {} in {} {{", e, it));
                self.ind += 1;
                match self.pick(&svs) {
                    Some(w) => self.line(&format!("{} = {}.asStr()", w, e)),
                    None => self.line(&format!("println({})", e)),
                }
                self.ind -= 1;
                self.line("}");
            }
            28 | 29 if self.depth < 2 => self.handles_stmt(),
            24 if self.depth < 2 => {
                // tasks read a view while the scope's body changes the owner
                if let (Some(w), Some(s)) = (self.pick(&svs), self.pick(&strs)) {
                    self.line("scope {");
                    self.line(&format!("    go () => println({}.len())", w));
                    if self.rng.chance(1, 2) {
                        self.line(&format!("    {}.push(\"more\")", s));
                    }
                    self.line("}");
                }
            }
            _ => {
                if let Some(l) = self.pick(&lists) {
                    let u = self.fresh("x");
                    self.line(&format!("if let {} = {}.get(0) {{", u, l));
                    self.ind += 1;
                    match self.pick(&svs) {
                        Some(w) => self.line(&format!("{} = {}.asStr()", w, u)),
                        None => self.line(&format!("println({})", u)),
                    }
                    self.ind -= 1;
                    self.line("}");
                }
            }
        }
    }

    /// Task handles and `Scope` values, kept where they could outlive their
    /// scope (most of these must be rejected), or used inside it.
    fn handles_stmt(&mut self) {
        match self.rng.below(7) {
            0 => {
                let h = self.fresh("h");
                self.line(&format!("var {}: ?Task[String] = none", h));
                self.handles.push(h);
            }
            1 => {
                let c = self.fresh("c");
                self.line(&format!("var {}: ?Scope = none", c));
                self.scopes.push(c);
            }
            2 | 3 => {
                // a scope that stores its handle or itself somewhere
                let handles = self.handles.clone();
                let scopes = self.scopes.clone();
                let sc = self.fresh("sc");
                let lit = self.str_lit();
                let kind = self.rng.below(5);
                // (named only when the body uses the name)
                if matches!(kind, 0 | 3) {
                    self.line("scope {");
                } else {
                    self.line(&format!("scope {} {{", sc));
                }
                self.ind += 1;
                match kind {
                    0 => match self.pick(&handles) {
                        Some(h) => self.line(&format!("{} = some(go () => {})", h, lit)),
                        None => self.line(&format!("println((go () => {}).join())", lit)),
                    },
                    1 => match self.pick(&scopes) {
                        Some(c) => self.line(&format!("{} = some({})", c, sc)),
                        None => self.line(&format!("{}.go(() => println(1))", sc)),
                    },
                    2 => match self.pick(&scopes) {
                        Some(c) => self.line(&format!("{} = some(passScope({}))", c, sc)),
                        None => self.line(&format!("passScope({}).go(() => println(2))", sc)),
                    },
                    3 => {
                        // legitimate: kept in a variable of the scope, joined there
                        let k = self.fresh("k");
                        self.line(&format!("var {}: ?Task[String] = none", k));
                        self.line(&format!("{} = some(go () => {})", k, lit));
                        self.line(&format!("if let t = {} {{ println(t.join()) }}", k));
                    }
                    _ => {
                        let s = self.pick(&self.strs.clone());
                        match s {
                            Some(s) => self.line(&format!("spawnOn({}, {}.clone())", sc, s)),
                            None => self.line(&format!("spawnOn({}, {})", sc, lit)),
                        }
                    }
                }
                if self.rng.chance(1, 3) {
                    let s = self.pick(&self.strs.clone());
                    if let Some(s) = s {
                        self.line(&format!("{}.push(\"grow it while the tasks run\")", s));
                    }
                }
                self.ind -= 1;
                self.line("}");
            }
            4 => {
                // the scope's value is its handle or itself
                let x = self.fresh("x");
                let lit = self.str_lit();
                if self.rng.chance(1, 2) {
                    self.line(&format!("let {} = scope {{", x));
                    self.line(&format!("    let t = go () => {}", lit));
                    self.line("    t");
                    self.line("}");
                    self.line(&format!("println({}.join())", x));
                } else {
                    let sc = self.fresh("sc");
                    self.line(&format!("let {} = scope {} {{ {} }}", x, sc, sc));
                    self.line(&format!("{}.go(() => println(3))", x));
                }
            }
            _ => {
                // use what was kept: after its scope, that is the bug
                let handles = self.handles.clone();
                let scopes = self.scopes.clone();
                if self.rng.chance(1, 2) {
                    if let Some(h) = self.pick(&handles) {
                        self.line(&format!("if let t = {} {{ println(t.join()) }}", h));
                        self.line(&format!("{} = none", h));
                    }
                } else if let Some(c) = self.pick(&scopes) {
                    self.line(&format!("if let s = {} {{ s.go(() => println(4)) }}", c));
                }
            }
        }
    }

    fn block(&mut self) {
        // declarations inside stay inside
        let saved = (self.strs.clone(), self.vecs.clone(), self.svs.clone(), self.vvs.clone(), self.lists.clone(), self.funcs.clone());
        let saved2 = (self.recs.clone(), self.maps.clone());
        let saved3 = (self.handles.clone(), self.scopes.clone());
        self.ind += 1;
        self.depth += 1;
        for _ in 0..1 + self.rng.below(5) {
            self.stmt();
        }
        // use what was declared in here
        self.use_new(&saved);
        self.depth -= 1;
        self.ind -= 1;
        (self.strs, self.vecs, self.svs, self.vvs, self.lists, self.funcs) = saved;
        for r in self.recs.clone().iter().filter(|r| !saved2.0.contains(r)) {
            self.line(&format!("println({}.name)", r));
        }
        for m in self.maps.clone().iter().filter(|m| !saved2.1.contains(m)) {
            self.line(&format!("println({}.len())", m));
        }
        (self.recs, self.maps) = saved2;
        for h in self.handles.clone().iter().filter(|h| !saved3.0.contains(h)) {
            self.line(&format!("if let t = {} {{ println(t.join()) }}", h));
        }
        for c in self.scopes.clone().iter().filter(|c| !saved3.1.contains(c)) {
            self.line(&format!("_ = {}", c));
        }
        (self.handles, self.scopes) = saved3;
    }

    #[allow(clippy::type_complexity)]
    fn use_new(&mut self, before: &(Vec<String>, Vec<String>, Vec<String>, Vec<String>, Vec<String>, Vec<String>)) {
        let new = |now: &Vec<String>, old: &Vec<String>| -> Vec<String> { now.iter().filter(|x| !old.contains(x)).cloned().collect() };
        for v in new(&self.svs, &before.2).into_iter().chain(new(&self.strs, &before.0)) {
            self.line(&format!("println({})", v));
        }
        for v in new(&self.vvs, &before.3).into_iter().chain(new(&self.vecs, &before.1)).chain(new(&self.lists, &before.4)) {
            self.line(&format!("println({}.len())", v));
        }
        for f in new(&self.funcs, &before.5) {
            self.line(&format!("println({}())", f));
        }
    }

    pub fn program(mut self) -> String {
        let mut head = String::from(
            "// generated by gojo-fuzz borrow\nuse std/task\n\nstruct P {\n    name: String\n}\n\nfn take(s: sink String) {}\n\nfn passScope(s: Scope) -> Scope {\n    return s\n}\n\nfn spawnOn(s: Scope, v: sink String) {\n    s.go(() => println(v))\n}\n\nfn grow(s: inout String) {\n    s.push(\"a long piece of text that moves the buffer\")\n}\n\nfn firstOf(l: []String) -> str {\n    if l.len() == 0 {\n        return \"none\"\n    }\n    return l[0].asStr()\n}\n\nfn run(f: fn() -> str) -> str {\n    return f()\n}\n\nstruct Ring {\n    items: Vec[String]\n\n    fn cursor(self) -> int {\n        return 0\n    }\n\n    fn next(self, c: inout int) yield ?&String {\n        if c >= self.items.len() {\n            yield none\n        }\n        c += 1\n        yield self.items[c - 1]\n    }\n}\n\nstruct LendCur {\n    i: int\n    buf: String\n}\n\nstruct Lend {\n    n: int\n\n    fn cursor(self) -> LendCur {\n        return LendCur{i: 0, buf: \"\".toString()}\n    }\n\n    fn next(self, c: inout LendCur) yield ?&String {\n        if c.i >= self.n {\n            yield none\n        }\n        c.i += 1\n        c.buf = \"a line long enough to need its own allocation\".toString()\n        yield c.buf\n    }\n}\n\nstruct Each {\n    items: Vec[String]\n\n    fn each(self, body: fn(&String) -> Flow) {\n        for x in self.items {\n            let t = x.clone()\n            if body(t) == Flow.Break {\n                return\n            }\n        }\n    }\n}\n\nfn pick(s: inout String) -> str {\n",
        );
        // a function returning a view of its parameter, with a defer
        head.push_str(match self.rng.below(12) {
            0 => "    defer s.push(\"grow the buffer with a long piece of text\")\n",
            1..=3 => "    defer println(s.len())\n",
            _ => "",
        });
        head.push_str("    return s.asStr()\n}\n\nfn main() {\n    let e0 = [0]\n");
        for _ in 0..4 + self.rng.below(16) {
            self.stmt();
        }
        // use what's left, so that fewer programs fail only for unused variables
        let rest: Vec<String> = self.svs.iter().chain(&self.strs).cloned().collect();
        for v in rest {
            self.line(&format!("println({})", v));
        }
        for v in self.vvs.clone().iter().chain(&self.vecs.clone()).chain(&self.lists.clone()) {
            self.line(&format!("println({}.len())", v));
        }
        for f in self.funcs.clone() {
            self.line(&format!("println({}())", f));
        }
        for r in self.recs.clone() {
            self.line(&format!("println({}.name)", r));
        }
        for m in self.maps.clone() {
            self.line(&format!("println({}.len())", m));
        }
        for h in self.handles.clone() {
            self.line(&format!("if let t = {} {{ println(t.join()) }}", h));
        }
        for c in self.scopes.clone() {
            self.line(&format!("if let s = {} {{ s.go(() => println(5)) }}", c));
        }
        // `task` must be used
        self.line("_ = task.cancelled()");
        head.push_str(&self.out);
        head.push_str("    println(e0.len())\n}\n");
        head
    }
}
