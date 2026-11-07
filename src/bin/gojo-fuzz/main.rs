// gojo-fuzz: robustness and soundness fuzzer for the Gojo compiler.
//
//   gojo-fuzz [--jobs N] [--time SECS] [--out DIR] [--seed N] [--exec-rate R]
//   gojo-fuzz reduce <file> [-o out.gojo]      shrink a failing input
//   gojo-fuzz check <file>                    print the input's outcome
//   gojo-fuzz diff [--jobs N] [--time SECS]   differential testing (see below)
//   gojo-fuzz gen [--seed N]                  print one generated program
//   gojo-fuzz borrow [--jobs N] [--time SECS]  attack the borrow checker (below)
//
// Workers mutate the programs in the repository's tests, examples, benchmarks and
// standard library at the token level and run the compiler's frontend and code
// generator in-process. Findings go to `<out>/findings`:
//
//   panic-*      the compiler panicked
//   signal-*     the compiler died (stack overflow, abort)
//   hang-*       the compiler took longer than the time limit
//   cc-*         an accepted program's generated C didn't compile
//   asan-*       an accepted program without `unsafe` misbehaved at run time
//                under AddressSanitizer / UBSan: a hole in the safety checks or
//                a miscompilation
//
// Mutants that the compiler accepts, or that produce an error message not seen
// before, join the corpus, which is kept in `<out>/corpus` across runs.
//
// `diff` generates random well-typed programs (progen.rs) and builds each four
// ways: debug, release, and both under AddressSanitizer + UBSan. All four must
// print the same output and end the same way (and the sanitizers must stay
// quiet), and the values the generator computed itself must come out (else
// `wrong-*`). Findings go to `<out>/findings` as `diff-*`, `asan-*`, and
// `rejected-*` (a generated program the compiler refused: a bug in the
// generator or a false positive in the compiler's checks).
//
// `borrow` generates small programs that juggle owners and views of them
// (borrowgen.rs) without trying to be valid. Every one the compiler accepts is
// run under AddressSanitizer: a report means the borrow checker let an unsafe
// program through.

mod borrowgen;
mod progen;

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Random numbers

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() % n as u64) as usize }
    }
    fn chance(&mut self, num: u64, den: u64) -> bool {
        self.next() % den < num
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

fn hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

// ---------------------------------------------------------------------------
// Running one input

/// What compiling an input did.
#[derive(Debug, Clone, PartialEq)]
enum Outcome {
    /// rejected with diagnostics (the normalized first message)
    Rejected(String),
    /// accepted; the generated C
    Accepted(String),
    /// the compiler panicked at this location, with this message
    Panic(String, String),
}

thread_local! {
    static PANIC: std::cell::RefCell<Option<(String, String)>> = const { std::cell::RefCell::new(None) };
}

fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let loc = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            String::new()
        };
        PANIC.with(|p| *p.borrow_mut() = Some((loc, msg)));
    }));
}

/// A file name that can't resolve `use ./pkg` imports to real directories.
const FAKE_PATH: &str = "/nonexistent/gojo-fuzz/main.gojo";

fn compile(text: &str) -> Outcome {
    let r = std::panic::catch_unwind(AssertUnwindSafe(|| match gojo::frontend(FAKE_PATH, Some(text)) {
        Ok(c) => {
            let has_main = c.prog.packages[c.prog.main_pkg as usize].items.contains_key("main");
            if !has_main {
                return Outcome::Rejected("no main".into());
            }
            let release = hash(text) & 1 == 1;
            match gojo::codegen::generate(&c.prog, &c.tfuncs, &c.sm, None, release) {
                Ok(c) => Outcome::Accepted(c),
                Err(e) => Outcome::Rejected(e),
            }
        }
        Err((_, diags)) => {
            let first = diags.iter().min_by_key(|d| (d.span.file, d.span.lo)).map(|d| d.msg.clone()).unwrap_or_default();
            Outcome::Rejected(first)
        }
    }));
    match r {
        Ok(o) => o,
        Err(_) => {
            let (loc, msg) = PANIC.with(|p| p.borrow_mut().take()).unwrap_or_default();
            Outcome::Panic(loc, msg)
        }
    }
}

/// An error message with names and numbers taken out, so that messages that
/// differ only in those count as one.
fn normalize(msg: &str) -> String {
    let mut out = String::new();
    let mut in_tick = false;
    for c in msg.chars() {
        if c == '`' {
            in_tick = !in_tick;
            if in_tick {
                out.push_str("`_`");
            }
            continue;
        }
        if in_tick {
            continue;
        }
        out.push(if c.is_ascii_digit() { '0' } else { c });
    }
    out
}

// ---------------------------------------------------------------------------
// Mutation

#[derive(Clone)]
struct Piece {
    /// whitespace and comments before the token
    gap: String,
    text: String,
    kind: PieceKind,
}

#[derive(Clone, Copy, PartialEq)]
enum PieceKind {
    Ident,
    Int,
    Other,
}

fn pieces(text: &str) -> Option<Vec<Piece>> {
    let toks = gojo::lexer::lex(0, text).ok()?;
    let mut out = vec![];
    let mut pos = 0usize;
    for t in toks {
        if matches!(t.tok, gojo::lexer::Tok::Newline | gojo::lexer::Tok::Eof) {
            continue;
        }
        let (lo, hi) = (t.span.lo as usize, t.span.hi as usize);
        if lo < pos || hi > text.len() || !text.is_char_boundary(lo) || !text.is_char_boundary(hi) {
            return None;
        }
        let kind = match t.tok {
            gojo::lexer::Tok::Ident(_) => PieceKind::Ident,
            gojo::lexer::Tok::Int(_) => PieceKind::Int,
            _ => PieceKind::Other,
        };
        out.push(Piece { gap: text[pos..lo].to_string(), text: text[lo..hi].to_string(), kind });
        pos = hi;
    }
    if let Some(last) = out.last_mut() {
        last.text.push_str(&text[pos..]);
    }
    Some(out)
}

fn unparse(ps: &[Piece]) -> String {
    let mut s = String::new();
    for p in ps {
        s.push_str(&p.gap);
        s.push_str(&p.text);
    }
    s
}

const INTERESTING_INTS: &[&str] = &[
    "0", "1", "-1", "2", "7", "8", "63", "64", "127", "128", "255", "256", "1000000", "2147483647", "2147483648", "4294967295", "4294967296",
    "9223372036854775807", "9223372036854775808", "18446744073709551615", "18446744073709551616", "0x7f", "0b1", "1_000",
];

const SNIPPETS: &[&str] = &[
    "{", "}", "(", ")", "[", "]", "\n", ";", ",", ".", "&", "<-", "=>", "..", "..=", "?", "!", "??", "?.", "=", ":=", "+", "-", "*", "/", "%", "+%",
    "==", "<", "|", "_", "self", "move", "clone()", ".clone()", "inout", "sink", "&mut", "let", "var", "fn", "return", "break", "continue",
    "defer", "errdefer", "go", "try", "catch", "with", "yield", "unsafe", "match", "if", "else", "for", "in", "select", "chan", "none", "some(",
    "ok(", "err(", "true", "false", "struct", "enum", "interface", "copy", "pub", "extend", "where", "const", "type", "Vec[int]", "String",
    "str", "[]int", "?int", "int ! String", "Box[int]", "rc[int]", "Map[int, int]", "Mutex", "scope", "drop(", "assert(", "println(",
    "0..10", "x", "v", "s", "i", "\"s\"", "'c'", "1.5", "[1, 2, 3]", "Vec[String]{}", "(x) => x", "() => {}", "fn() {}", "#",
];

struct Mutator<'a> {
    corpus: &'a [String],
    dict: &'a [String],
}

impl Mutator<'_> {
    fn mutate(&self, rng: &mut Rng, base: &str) -> String {
        let mut ps = match pieces(base) {
            Some(p) if !p.is_empty() => p,
            _ => return self.byte_mutate(rng, base),
        };
        let max = if rng.chance(1, 4) { 8 } else { 3 };
        let n = 1 + rng.below(max);
        for _ in 0..n {
            self.mutate_once(rng, &mut ps);
            if ps.is_empty() {
                break;
            }
        }
        let mut s = unparse(&ps);
        if rng.chance(1, 50) {
            s = self.byte_mutate(rng, &s);
        }
        s
    }

    fn range(rng: &mut Rng, len: usize) -> (usize, usize) {
        let lo = rng.below(len);
        let max = if rng.chance(1, 8) { 64 } else { 6 };
        let hi = (lo + 1 + rng.below(max)).min(len);
        (lo, hi)
    }

    fn mutate_once(&self, rng: &mut Rng, ps: &mut Vec<Piece>) {
        let len = ps.len();
        if len == 0 {
            return;
        }
        match rng.below(12) {
            0 | 1 => {
                // delete a range
                let (lo, hi) = Self::range(rng, len);
                ps.drain(lo..hi);
            }
            2 => {
                // duplicate a range
                let (lo, hi) = Self::range(rng, len);
                let copy: Vec<Piece> = ps[lo..hi].to_vec();
                let at = rng.below(len + 1);
                ps.splice(at..at, copy);
            }
            3 | 4 => {
                // replace one token with a token seen in the corpus
                let i = rng.below(len);
                ps[i].text = rng.pick(self.dict).clone();
                ps[i].kind = PieceKind::Other;
            }
            5 => {
                // insert a token or snippet
                let at = rng.below(len + 1);
                let text = if rng.chance(1, 2) { rng.pick(self.dict).clone() } else { rng.pick(SNIPPETS).to_string() };
                ps.insert(at, Piece { gap: " ".into(), text, kind: PieceKind::Other });
            }
            6 | 7 => {
                // splice in a range of another program
                let other = rng.pick(self.corpus);
                if let Some(ops) = pieces(other)
                    && !ops.is_empty() {
                        let (olo, ohi) = Self::range(rng, ops.len());
                        let (lo, hi) = if rng.chance(1, 3) {
                            let at = rng.below(len + 1);
                            (at, at)
                        } else {
                            Self::range(rng, len)
                        };
                        ps.splice(lo..hi, ops[olo..ohi].iter().cloned());
                    }
            }
            8 => {
                // swap two ranges
                let (a, b) = Self::range(rng, len);
                let (c, d) = Self::range(rng, len);
                if b <= c {
                    let second: Vec<Piece> = ps[c..d].to_vec();
                    let first: Vec<Piece> = ps[a..b].to_vec();
                    ps.splice(c..d, first);
                    ps.splice(a..b, second);
                }
            }
            9 | 10 => {
                // rename an identifier to another one from the same program:
                // keeps the program well-formed often enough to reach the
                // later phases
                let idents: Vec<usize> = (0..len).filter(|&i| ps[i].kind == PieceKind::Ident).collect();
                if idents.len() >= 2 {
                    let a = *rng.pick(&idents);
                    let b = *rng.pick(&idents);
                    let name = ps[b].text.clone();
                    if rng.chance(1, 3) {
                        // every occurrence
                        let old = ps[a].text.clone();
                        for p in ps.iter_mut() {
                            if p.kind == PieceKind::Ident && p.text == old {
                                p.text = name.clone();
                            }
                        }
                    } else {
                        ps[a].text = name;
                    }
                }
            }
            _ => {
                // an interesting integer
                let ints: Vec<usize> = (0..len).filter(|&i| ps[i].kind == PieceKind::Int).collect();
                if !ints.is_empty() {
                    let i = *rng.pick(&ints);
                    ps[i].text = rng.pick(INTERESTING_INTS).to_string();
                }
            }
        }
    }

    fn byte_mutate(&self, rng: &mut Rng, s: &str) -> String {
        let mut b = s.as_bytes().to_vec();
        for _ in 0..1 + rng.below(4) {
            if b.is_empty() {
                break;
            }
            let i = rng.below(b.len());
            match rng.below(3) {
                0 => b[i] = rng.next() as u8,
                1 => {
                    b.remove(i);
                }
                _ => b.insert(i, *rng.pick(b"{}()[]\"'`\\/*\n\0\xff\xc3")),
            }
        }
        String::from_utf8_lossy(&b).into_owned()
    }
}

// ---------------------------------------------------------------------------
// Corpus

fn gojo_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut es: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    es.sort();
    for p in es {
        if p.is_dir() {
            gojo_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "gojo") {
            out.push(p);
        }
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn load_corpus(out: &Path) -> Vec<String> {
    let root = repo_root();
    let mut files = vec![];
    for d in ["tests", "examples", "bench/gojo", "std"] {
        gojo_files(&root.join(d), &mut files);
    }
    gojo_files(&out.join("corpus"), &mut files);
    files.iter().filter_map(|f| std::fs::read_to_string(f).ok()).collect()
}

fn build_dict(corpus: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = vec![];
    for c in corpus {
        for p in pieces(c).unwrap_or_default() {
            if p.text.len() <= 40 && seen.insert(p.text.clone()) {
                out.push(p.text);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// `gojo-fuzz fmt [N]`: the formatter (and `gojo doc`) on generated programs,
// corpus files and mutants. For every file that parses: formatting succeeds, keeps the tokens,
// still parses, checks the same way, is stable, and ignores how the input
// was indented.

fn fmt_mode(n: usize, seed: u64, out: &Path) {
    install_panic_hook();
    let corpus = load_corpus(Path::new("/nonexistent"));
    let dict = build_dict(&corpus);
    let mut rng = Rng(seed | 1);
    let dir = out.join("fmt-findings");
    let (mut tried, mut found) = (0, 0);
    for i in 0..n {
        let src = match i % 4 {
            0 => progen::Gen::new(&mut rng).program(),
            1 => borrowgen::BorrowGen::new(&mut rng).program(),
            2 => rng.pick(&corpus).clone(),
            _ => {
                let base = rng.pick(&corpus).clone();
                Mutator { corpus: &corpus, dict: &dict }.mutate(&mut rng, &base)
            }
        };
        let shifted = reindent(&mut rng, &src);
        let problem = std::panic::catch_unwind(AssertUnwindSafe(|| fmt_problem(&src, &shifted)))
            .unwrap_or_else(|_| Some("the formatter panicked".into()))
            .or_else(|| {
                // and `gojo doc`, on the same input
                let files = [("fuzz.gojo".to_string(), src.clone())];
                std::panic::catch_unwind(AssertUnwindSafe(|| gojo::doc::package("fuzz", &files))).err().map(|_| "`gojo doc` panicked".to_string())
            });
        if !gojo::lexer::lex(0, &src).is_ok_and(|t| gojo::parser::parse_file(0, t).is_ok()) {
            continue;
        }
        tried += 1;
        if let Some(p) = problem {
            found += 1;
            let _ = std::fs::create_dir_all(&dir);
            let path = dir.join(format!("{:016x}.gojo", hash(&src)));
            let _ = std::fs::write(&path, &src);
            eprintln!("{}: {}", path.display(), p);
        }
    }
    eprintln!("gojo-fuzz fmt: {} well-formed inputs, {} problems", tried, found);
    if found > 0 {
        std::process::exit(1);
    }
}

/// `gojo-fuzz toml [N]`: the gojo.toml reader on mutated manifests. It must
/// return a manifest or errors with valid spans, never panic.
fn toml_mode(n: usize, seed: u64, out: &Path) {
    install_panic_hook();
    let base = repo_root().join("tests/packages");
    let mut seeds = vec![];
    for e in walk_files(&base) {
        if e.file_name().is_some_and(|f| f == "gojo.toml") {
            if let Ok(t) = std::fs::read_to_string(&e) {
                seeds.push((e.parent().unwrap().to_path_buf(), t));
            }
        }
    }
    let pieces = ["[", "]", "{", "}", "=", "\"", "'", ",", ".", "\n", "#", "\\u00e9", "\\", "[[a]]", "a.b.c", "\"\"\"", "-", "1_000", "true", "path = \"../x\"", "@s/n", "\u{e9}", "\r\n", "[dependencies]", "x = { }"];
    let mut rng = Rng(seed | 1);
    let (mut ok, mut errs, mut found) = (0, 0, 0);
    for _ in 0..n {
        let (root, base_text) = rng.pick(&seeds).clone();
        let mut t: Vec<char> = base_text.chars().collect();
        for _ in 0..1 + rng.below(4) {
            let at = rng.below(t.len() + 1);
            match rng.below(3) {
                0 if !t.is_empty() => {
                    let end = (at + 1 + rng.below(6)).min(t.len());
                    t.drain(at.min(end)..end);
                }
                1 => {
                    let p: Vec<char> = rng.pick(&pieces).chars().collect();
                    t.splice(at..at, p);
                }
                _ => {
                    let other = &rng.pick(&seeds).1;
                    let cs: Vec<char> = other.chars().collect();
                    let from = rng.below(cs.len().max(1));
                    let piece: Vec<char> = cs[from..(from + rng.below(20)).min(cs.len())].to_vec();
                    t.splice(at..at, piece);
                }
            }
        }
        let text: String = t.into_iter().collect();
        let r = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let mut sm = gojo::diag::SourceMap::default();
            match gojo::manifest::from_text(&mut sm, &root, "gojo.toml".into(), text.clone()) {
                Ok(_) => (true, true),
                // every error must render (its span inside the file)
                Err(ds) => (false, ds.iter().all(|d| !gojo::diag::render(&sm, d, false).is_empty())),
            }
        }));
        match r {
            Ok((true, _)) => ok += 1,
            Ok((false, true)) => errs += 1,
            _ => {
                found += 1;
                let dir = out.join("toml-findings");
                let _ = std::fs::create_dir_all(&dir);
                let p = dir.join(format!("{:016x}.toml", hash(&text)));
                let _ = std::fs::write(&p, &text);
                eprintln!("{}: the manifest reader panicked", p.display());
            }
        }
    }
    eprintln!("gojo-fuzz toml: {} accepted, {} rejected with errors, {} problems", ok, errs, found);
    if found > 0 {
        std::process::exit(1);
    }
}

fn walk_files(d: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    for e in std::fs::read_dir(d).into_iter().flatten().filter_map(|e| e.ok()) {
        let p = e.path();
        if p.is_dir() { out.extend(walk_files(&p)) } else { out.push(p) }
    }
    out
}

/// What is wrong with formatting `src`, if anything. `shifted` is `src` with
/// different indentation and trailing spaces.
fn fmt_problem(src: &str, shifted: &str) -> Option<String> {
    let parses = |s: &str| gojo::lexer::lex(0, s).is_ok_and(|t| gojo::parser::parse_file(0, t).is_ok());
    if !parses(src) {
        return None;
    }
    let f = match gojo::formatter::format(src) {
        Ok(f) => f,
        Err(e) => return Some(format!("not formatted: {}", e)),
    };
    if !gojo::formatter::same_tokens(src, &f) {
        return Some("the tokens changed".into());
    }
    if !parses(&f) {
        return Some("the output does not parse".into());
    }
    if gojo::formatter::format(&f).as_deref() != Ok(f.as_str()) {
        return Some("formatting is not stable".into());
    }
    let checks = |s: &str| gojo::frontend(FAKE_PATH, Some(s)).map_err(|(_, d)| d.len());
    if checks(src).is_ok() != checks(&f).is_ok() {
        return Some("the checker disagrees about the output".into());
    }
    if gojo::formatter::same_tokens(src, shifted) && gojo::formatter::format(shifted).as_deref() != Ok(f.as_str()) {
        return Some("the result depends on the indentation".into());
    }
    None
}

/// `src` with random indentation and trailing spaces, except inside block
/// comments (their text is kept as written).
fn reindent(rng: &mut Rng, src: &str) -> String {
    let mut out = String::new();
    let mut in_comment = false;
    for line in src.split_inclusive('\n') {
        if in_comment || line.contains("/*") {
            in_comment = !line.contains("*/") || line.rfind("/*") > line.rfind("*/");
            out.push_str(line);
            continue;
        }
        let (body, nl) = line.strip_suffix('\n').map_or((line, ""), |b| (b, "\n"));
        let body = body.trim_start_matches([' ', '\t']);
        if !body.is_empty() {
            for _ in 0..rng.below(9) {
                out.push(' ');
            }
        }
        out.push_str(body);
        if rng.below(4) == 0 {
            out.push_str("  ");
        }
        out.push_str(nl);
    }
    out
}

// ---------------------------------------------------------------------------
// Findings

struct Findings {
    dir: PathBuf,
    seen: Mutex<HashSet<String>>,
}

impl Findings {
    fn new(out: &Path) -> Findings {
        let dir = out.join("findings");
        let _ = std::fs::create_dir_all(&dir);
        // signatures found by earlier runs
        let mut seen = HashSet::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "txt")
                    && let Ok(s) = std::fs::read_to_string(&p)
                        && let Some(sig) = s.lines().next() {
                            seen.insert(sig.to_string());
                        }
            }
        }
        Findings { dir, seen: Mutex::new(seen) }
    }

    /// Records a finding unless one with the same signature exists. Returns
    /// whether it was new.
    fn report(&self, kind: &str, sig: &str, input: &str, detail: &str) -> bool {
        let sig = format!("{} {}", kind, sig);
        if !self.seen.lock().unwrap().insert(sig.clone()) {
            return false;
        }
        let name = format!("{}-{:016x}", kind, hash(&sig));
        let _ = std::fs::write(self.dir.join(format!("{}.gojo", name)), input);
        let _ = std::fs::write(self.dir.join(format!("{}.txt", name)), format!("{}\n{}\n", sig, detail));
        eprintln!("\n*** new finding: {} ({})", name, sig);
        true
    }
}

// ---------------------------------------------------------------------------
// Running accepted programs

fn gojo_bin() -> PathBuf {
    let me = std::env::current_exe().unwrap();
    me.with_file_name("gojo")
}

/// Runs a command with a time limit. None when it ran out of time.
fn run_limited(cmd: &mut Command, limit: Duration) -> Option<std::process::Output> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().ok()?;
    // read the pipes on other threads so a chatty child can't block on them
    let mut so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let t1 = std::thread::spawn(move || {
        let mut v = vec![];
        let _ = std::io::Read::read_to_end(&mut so, &mut v);
        v.truncate(1 << 20);
        v
    });
    let t2 = std::thread::spawn(move || {
        let mut v = vec![];
        let _ = std::io::Read::read_to_end(&mut se, &mut v);
        v.truncate(1 << 20);
        v
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Some(std::process::Output { status, stdout: t1.join().unwrap_or_default(), stderr: t2.join().unwrap_or_default() });
            }
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Whether a program may be run: its behavior has to be defined by the safety
/// rules alone (no `unsafe`, no C), and it mustn't touch files or the network.
fn runnable(text: &str) -> bool {
    // (a module that is imported is used: unused imports don't compile)
    !["unsafe", "extern", "os.", "net.", "http.", "use ./", "use ../"].iter().any(|w| text.contains(w))
}

fn exec_check(text: &str, work: &Path, findings: &Findings) {
    let src = work.join("main.gojo");
    let bin = work.join("prog");
    let _ = std::fs::remove_file(&bin);
    if std::fs::write(&src, text).is_err() {
        return;
    }
    let mut build = Command::new(gojo_bin());
    build.arg("build").arg("--cc").arg("cc -fsanitize=address,undefined -fno-sanitize-recover=undefined -g").arg(&src).arg("-o").arg(&bin);
    build.env("GOJO_CACHE", work.join("cache"));
    let Some(out) = run_limited(&mut build, Duration::from_secs(120)) else {
        findings.report("hang", "c-compile", text, "the C compiler took more than 120 s");
        return;
    };
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        if stderr.contains("internal error") || stderr.contains("panicked") {
            let first = stderr.lines().find(|l| l.contains("error:") && !l.contains("internal error")).unwrap_or("").trim().to_string();
            findings.report("cc", &normalize(&first), text, &stderr);
        }
        return;
    }
    let mut run = Command::new(&bin);
    run.current_dir(work);
    run.env("ASAN_OPTIONS", "detect_leaks=0:abort_on_error=0:halt_on_error=1");
    run.env("UBSAN_OPTIONS", "print_stacktrace=1:halt_on_error=1");
    run.env("GOJO_MAXPROCS", "2");
    let Some(out) = run_limited(&mut run, Duration::from_secs(5)) else { return };
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(l) = stderr.lines().find(|l| l.contains("ERROR: AddressSanitizer") || l.contains("runtime error:") || l.contains("ERROR: UndefinedBehaviorSanitizer")) {
        // signature: the kind of error and the first frame in generated code
        let kind = l.split(" on ").next().unwrap_or(l).split("runtime error:").last().unwrap_or(l).trim();
        let frame = stderr.lines().find(|l| l.contains(" in kx_") || l.contains(" in gojo_")).and_then(|l| l.split(" in ").nth(1)).and_then(|f| f.split_whitespace().next()).unwrap_or("");
        findings.report("asan", &format!("{} @ {}", normalize(kind), frame), text, &stderr);
    }
    use std::os::unix::process::ExitStatusExt;
    if let Some(sig) = out.status.signal() {
        // a Gojo program only dies by a signal when something is badly wrong
        // (panics exit with code 2, stack overflows are reported as panics)
        if sig != 9 {
            findings.report("runsignal", &format!("{}", sig), text, &stderr);
        }
    }
}

// ---------------------------------------------------------------------------
// Worker

/// Accepted programs kept on disk for later runs (overwritten at random).
const CORPUS_SLOTS: usize = 3000;

struct Config {
    out: PathBuf,
    seed: u64,
    exec_rate: u64,
    limit: Duration,
}

struct Stats {
    execs: AtomicU64,
    accepted: AtomicU64,
    executed: AtomicU64,
}

fn worker(id: usize, cfg: Config) {
    install_panic_hook();
    let mut corpus = load_corpus(&cfg.out);
    let dict = build_dict(&corpus);
    let findings = Findings::new(&cfg.out);
    let work = cfg.out.join(format!("work-{}", id));
    let _ = std::fs::create_dir_all(&work);
    let cur_path = cfg.out.join(format!("cur-{}.gojo", id));
    let corpus_dir = cfg.out.join("corpus");
    let _ = std::fs::create_dir_all(&corpus_dir);
    let stats = Arc::new(Stats { execs: AtomicU64::new(0), accepted: AtomicU64::new(0), executed: AtomicU64::new(0) });
    let mut rng = Rng(cfg.seed.wrapping_mul(0x9E3779B97F4A7C15) ^ (id as u64 + 1));
    let mut seen_msgs: HashSet<String> = HashSet::new();
    let mut seen_c: HashSet<u64> = HashSet::new();

    // watchdog: a case that runs too long is a hang -- counted in CPU time, so
    // that a stopped or starved process (a loaded machine) isn't mistaken for one
    let started = Arc::new(Mutex::new(Instant::now()));
    let started_cpu = Arc::new(AtomicU64::new(process_cpu_ns()));
    {
        let started = started.clone();
        let started_cpu = started_cpu.clone();
        let cur_path = cur_path.clone();
        let limit = cfg.limit;
        let findings_dir = cfg.out.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(200));
                let cpu = process_cpu_ns().saturating_sub(started_cpu.load(Ordering::Relaxed));
                if started.lock().unwrap().elapsed() > limit && cpu > limit.as_nanos() as u64 / 2 {
                    let input = std::fs::read_to_string(&cur_path).unwrap_or_default();
                    let f = Findings::new(&findings_dir);
                    f.report("hang", &format!("{:016x}", hash(&input)), &input, &format!("compiling took more than {:?}", limit));
                    std::process::exit(3);
                }
            }
        });
    }
    // stats for the supervisor
    {
        let stats = stats.clone();
        let path = cfg.out.join(format!("stats-{}", id));
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let _ = std::fs::write(&path, format!("{} {} {}", stats.execs.load(Ordering::Relaxed), stats.accepted.load(Ordering::Relaxed), stats.executed.load(Ordering::Relaxed)));
            }
        });
    }

    loop {
        let base = rng.pick(&corpus).clone();
        let text = {
            let m = Mutator { corpus: &corpus, dict: &dict };
            m.mutate(&mut rng, &base)
        };
        let _ = std::fs::write(&cur_path, &text);
        *started.lock().unwrap() = Instant::now();
        started_cpu.store(process_cpu_ns(), Ordering::Relaxed);
        let outcome = compile(&text);
        *started.lock().unwrap() = Instant::now();
        stats.execs.fetch_add(1, Ordering::Relaxed);
        match outcome {
            Outcome::Panic(loc, msg) => {
                findings.report("panic", &loc, &text, &msg);
            }
            Outcome::Rejected(msg) => {
                let n = normalize(&msg);
                if seen_msgs.insert(n) && corpus.len() < 20000 {
                    corpus.push(text);
                }
            }
            Outcome::Accepted(c) => {
                stats.accepted.fetch_add(1, Ordering::Relaxed);
                let h = hash(&c);
                if seen_c.insert(h) {
                    if corpus.len() < 20000 {
                        corpus.push(text.clone());
                    }
                    // kept for later runs in a bounded set of slots
                    if rng.chance(1, 8) {
                        let _ = std::fs::write(corpus_dir.join(format!("c{:04}.gojo", rng.below(CORPUS_SLOTS))), &text);
                    }
                    if cfg.exec_rate > 0 && rng.chance(1, cfg.exec_rate) && runnable(&text) {
                        stats.executed.fetch_add(1, Ordering::Relaxed);
                        // the C compiler and the program run with no watchdog
                        *started.lock().unwrap() = Instant::now() + Duration::from_secs(3600);
                        exec_check(&text, &work, &findings);
                        *started.lock().unwrap() = Instant::now();
                    }
                }
            }
        }
    }
}

#[repr(C)]
struct Timespec {
    sec: i64,
    nsec: i64,
}

unsafe extern "C" {
    fn clock_gettime(clock: i32, ts: *mut Timespec) -> i32;
}

/// CPU time used by this process, in ns.
fn process_cpu_ns() -> u64 {
    const CLOCK_PROCESS_CPUTIME_ID: i32 = if cfg!(target_os = "macos") { 12 } else { 2 };
    let mut ts = Timespec { sec: 0, nsec: 0 };
    // SAFETY: a plain system call on a local struct
    if unsafe { clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &mut ts) } != 0 {
        return 0;
    }
    ts.sec as u64 * 1_000_000_000 + ts.nsec as u64
}

// ---------------------------------------------------------------------------
// Supervisor

fn supervise(jobs: usize, time: Option<Duration>, out: &Path, seed: u64, exec_rate: u64, limit: Duration) {
    let _ = std::fs::create_dir_all(out);
    let me = std::env::current_exe().unwrap();
    let spawn = |id: usize, generation: u64| {
        let log = std::fs::File::create(out.join(format!("worker-{}.log", id))).unwrap();
        Command::new(&me)
            .arg("--worker")
            .arg(id.to_string())
            .arg("--out")
            .arg(out)
            .arg("--seed")
            .arg((seed ^ generation.wrapping_mul(0xD1B54A32D192ED03)).to_string())
            .arg("--exec-rate")
            .arg(exec_rate.to_string())
            .arg("--limit")
            .arg(limit.as_secs().to_string())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .expect("cannot start worker")
    };
    let mut children: Vec<std::process::Child> = (0..jobs).map(|i| spawn(i, i as u64)).collect();
    let mut gens: Vec<u64> = (0..jobs as u64).collect();
    let start = Instant::now();
    let mut last_print = Instant::now();
    let mut base: HashMap<usize, (u64, u64, u64)> = HashMap::new();
    let findings = Findings::new(out);
    loop {
        std::thread::sleep(Duration::from_millis(100));
        for i in 0..jobs {
            if let Ok(Some(status)) = children[i].try_wait() {
                // a worker died: a stack overflow or abort while compiling, or a hang
                use std::os::unix::process::ExitStatusExt;
                if status.code() != Some(3) {
                    let input = std::fs::read_to_string(out.join(format!("cur-{}.gojo", i))).unwrap_or_default();
                    let log = std::fs::read_to_string(out.join(format!("worker-{}.log", i))).unwrap_or_default();
                    let tail: String = log.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
                    let why = if log.contains("overflowed its stack") { "stack-overflow".to_string() } else { format!("{:?}/{:?}", status.signal(), status.code()) };
                    findings.report("signal", &why, &input, &tail);
                }
                let (e, a, x) = read_stats(out, i);
                let b = base.entry(i).or_default();
                b.0 += e;
                b.1 += a;
                b.2 += x;
                let _ = std::fs::remove_file(out.join(format!("stats-{}", i)));
                gens[i] += jobs as u64;
                children[i] = spawn(i, gens[i]);
            }
        }
        if last_print.elapsed() >= Duration::from_secs(10) {
            last_print = Instant::now();
            let (mut e, mut a, mut x) = (0, 0, 0);
            for i in 0..jobs {
                let (e1, a1, x1) = read_stats(out, i);
                let b = base.get(&i).copied().unwrap_or_default();
                e += e1 + b.0;
                a += a1 + b.1;
                x += x1 + b.2;
            }
            let secs = start.elapsed().as_secs_f64();
            let nf = std::fs::read_dir(out.join("findings")).map(|r| r.filter(|e| e.as_ref().is_ok_and(|e| e.path().extension().is_some_and(|x| x == "gojo"))).count()).unwrap_or(0);
            eprintln!("[{:>6.0}s] {} execs ({:.0}/s), {} accepted, {} run under sanitizers, {} findings", secs, e, e as f64 / secs, a, x, nf);
            let _ = std::io::stderr().flush();
        }
        if let Some(t) = time
            && start.elapsed() >= t {
                for c in children.iter_mut() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
                return;
            }
    }
}

fn read_stats(out: &Path, i: usize) -> (u64, u64, u64) {
    let s = std::fs::read_to_string(out.join(format!("stats-{}", i))).unwrap_or_default();
    let v: Vec<u64> = s.split_whitespace().filter_map(|x| x.parse().ok()).collect();
    (v.first().copied().unwrap_or(0), v.get(1).copied().unwrap_or(0), v.get(2).copied().unwrap_or(0))
}

// ---------------------------------------------------------------------------
// check / reduce

/// The outcome of one input as a short signature, computed in a child process
/// so that stack overflows and hangs can be observed too.
fn signature_of(text: &str, scratch: &Path, limit: Duration, deep: bool) -> String {
    let f = scratch.join("reduce-candidate.gojo");
    if std::fs::write(&f, text).is_err() {
        return "io".into();
    }
    if deep {
        return deep_signature(&f, scratch);
    }
    let me = std::env::current_exe().unwrap();
    match run_limited(Command::new(me).arg("check").arg(&f), limit) {
        None => "hang".into(),
        Some(out) => {
            use std::os::unix::process::ExitStatusExt;
            if let Some(sig) = out.status.signal() {
                return format!("signal {}", sig);
            }
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
    }
}

/// For findings past the frontend: build with sanitizers and run.
fn deep_signature(f: &Path, scratch: &Path) -> String {
    let bin = scratch.join("prog");
    let _ = std::fs::remove_file(&bin);
    let mut build = Command::new(gojo_bin());
    build.arg("build").arg("--cc").arg("cc -fsanitize=address,undefined -fno-sanitize-recover=undefined -g").arg(f).arg("-o").arg(&bin);
    build.env("GOJO_CACHE", scratch.join("cache"));
    let Some(out) = run_limited(&mut build, Duration::from_secs(120)) else { return "hang".into() };
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        if stderr.contains("internal error") || stderr.contains("panicked") {
            let first = stderr.lines().find(|l| l.contains("error:") && !l.contains("internal error")).unwrap_or("").trim();
            // the message without the file name
            let msg = first.split(" error: ").nth(1).unwrap_or(first);
            return format!("cc {}", normalize(msg));
        }
        return "rejected".into();
    }
    let mut run = Command::new(&bin);
    run.current_dir(scratch);
    run.env("ASAN_OPTIONS", "detect_leaks=0:halt_on_error=1");
    let Some(out) = run_limited(&mut run, Duration::from_secs(5)) else { return "timeout".into() };
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(l) = stderr.lines().find(|l| l.contains("ERROR: AddressSanitizer") || l.contains("runtime error:")) {
        let kind = l.split(" on ").next().unwrap_or(l).split("runtime error:").last().unwrap_or(l).trim();
        return format!("asan {}", normalize(kind));
    }
    "ok".into()
}

fn check_one(path: &str) {
    install_panic_hook();
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("cannot read {}: {}", path, e);
        std::process::exit(1)
    });
    let o = gojo::with_compiler_stack(move || compile(&text));
    match o {
        Outcome::Rejected(_) => println!("rejected"),
        Outcome::Accepted(_) => println!("accepted"),
        Outcome::Panic(loc, _) => println!("panic {}", loc),
    }
}

/// Delta debugging over tokens, then lines: removes pieces while the outcome's
/// signature stays the same.
fn reduce(path: &str, out_path: Option<&str>, limit: Duration) {
    let text = std::fs::read_to_string(path).expect("cannot read input");
    let scratch = std::env::temp_dir().join(format!("gojo-reduce-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&scratch);
    let mut deep = false;
    let mut want = signature_of(&text, &scratch, limit, false);
    if want == "accepted" {
        // the problem is in the generated C or at run time
        deep = true;
        want = signature_of(&text, &scratch, limit, true);
    }
    eprintln!("signature: {}", want);
    if want == "rejected" || want == "ok" {
        eprintln!("nothing to reduce: the compiler handles this input");
        return;
    }
    let mut cur = text.clone();
    loop {
        let before = cur.len();
        // tokens
        if let Some(mut ps) = pieces(&cur) {
            let mut chunk = (ps.len() / 2).max(1);
            while chunk >= 1 {
                let mut i = 0;
                while i < ps.len() {
                    let hi = (i + chunk).min(ps.len());
                    let mut cand = ps.clone();
                    cand.drain(i..hi);
                    if signature_of(&unparse(&cand), &scratch, limit, deep) == want {
                        ps = cand;
                    } else {
                        i += chunk;
                    }
                }
                if chunk == 1 {
                    break;
                }
                chunk /= 2;
            }
            cur = unparse(&ps);
        }
        // lines
        let mut lines: Vec<String> = cur.lines().map(|l| l.to_string()).collect();
        let mut i = 0;
        while i < lines.len() {
            let mut cand = lines.clone();
            cand.remove(i);
            if signature_of(&cand.join("\n"), &scratch, limit, deep) == want {
                lines = cand;
            } else {
                i += 1;
            }
        }
        cur = lines.join("\n") + "\n";
        // whitespace and comments between tokens
        if let Some(mut ps) = pieces(&cur) {
            for i in 0..ps.len() {
                let g = ps[i].gap.clone();
                let simple = if g.contains('\n') { "\n".to_string() } else if g.is_empty() { String::new() } else { " ".to_string() };
                if g != simple {
                    ps[i].gap = simple;
                    if signature_of(&unparse(&ps), &scratch, limit, deep) != want {
                        ps[i].gap = g;
                    }
                }
            }
            cur = unparse(&ps);
        }
        if cur.len() >= before {
            break;
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    match out_path {
        Some(o) => std::fs::write(o, &cur).expect("cannot write output"),
        None => print!("{}", cur),
    }
    eprintln!("reduced {} -> {} bytes", text.len(), cur.len());
}

// ---------------------------------------------------------------------------
// Differential testing

#[derive(Debug, PartialEq, Clone)]
struct RunResult {
    /// exit code, or 128 + signal; None: timed out
    status: Option<i32>,
    stdout: String,
    /// the first line of a panic message, without the location
    panic: String,
}

fn run_variant(src: &Path, work: &Path, name: &str, flags: &[&str], findings: &Findings, text: &str) -> Option<RunResult> {
    let bin = work.join(name);
    let _ = std::fs::remove_file(&bin);
    let mut build = Command::new(gojo_bin());
    build.arg("build").args(flags).arg(src).arg("-o").arg(&bin);
    build.env("GOJO_CACHE", work.join("cache"));
    let out = run_limited(&mut build, Duration::from_secs(300))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let first = stderr.lines().find(|l| l.contains("error:")).unwrap_or("").trim().to_string();
        let msg = first.split(" error: ").nth(1).unwrap_or(&first).to_string();
        let kind = if stderr.contains("internal error") || stderr.contains("panicked") { "cc" } else { "rejected" };
        findings.report(kind, &normalize(&msg), text, &stderr);
        return None;
    }
    let mut run = Command::new(&bin);
    run.current_dir(work);
    run.env("ASAN_OPTIONS", "detect_leaks=0:halt_on_error=1");
    run.env("UBSAN_OPTIONS", "print_stacktrace=1:halt_on_error=1");
    let r = match run_limited(&mut run, Duration::from_secs(20)) {
        None => RunResult { status: None, stdout: String::new(), panic: String::new() },
        Some(o) => {
            use std::os::unix::process::ExitStatusExt;
            let stderr = String::from_utf8_lossy(&o.stderr).to_string();
            if let Some(l) = stderr.lines().find(|l| l.contains("ERROR: AddressSanitizer") || l.contains("runtime error:") || l.contains("ERROR: UndefinedBehaviorSanitizer")) {
                let kind = l.split(" on ").next().unwrap_or(l).split("runtime error:").last().unwrap_or(l).trim();
                findings.report("asan", &format!("{} ({})", normalize(kind), name), text, &stderr);
            }
            let status = o.status.code().or_else(|| o.status.signal().map(|s| 128 + s));
            let panic = stderr.lines().find(|l| l.starts_with("panic:")).unwrap_or("").to_string();
            RunResult { status, stdout: String::from_utf8_lossy(&o.stdout).to_string(), panic }
        }
    };
    Some(r)
}

fn diff_one(text: &str, work: &Path, findings: &Findings) -> bool {
    let src = work.join("main.gojo");
    if std::fs::write(&src, text).is_err() {
        return false;
    }
    let san = "cc -fsanitize=address,undefined -fno-sanitize-recover=undefined -g";
    let variants: [(&str, Vec<&str>); 4] = [("debug", vec![]), ("release", vec!["--release"]), ("asan", vec!["--cc", san]), ("asan-release", vec!["--release", "--cc", san])];
    let mut results: Vec<(&str, RunResult)> = vec![];
    for (name, flags) in &variants {
        match run_variant(&src, work, name, flags, findings, text) {
            Some(r) => results.push((name, r)),
            None => return false,
        }
    }
    let (base_name, base) = &results[0];
    // the generator knows these values: a failed check is a miscompilation
    // even when every build agrees
    for (name, r) in &results {
        if r.panic.contains("gojo-fuzz: wrong value") {
            findings.report("wrong", &format!("{} {:016x}", name, hash(text)), text, &format!("{}: {}\n{}", name, r.panic, r.stdout));
            break;
        }
    }
    if base.status.is_none() {
        // a program too slow even for the debug build: the generator's fault
        return true;
    }
    for (name, r) in &results[1..] {
        if r.status != base.status || r.stdout != base.stdout || r.panic != base.panic {
            let first_diff = base.stdout.lines().zip(r.stdout.lines()).position(|(a, b)| a != b).unwrap_or(base.stdout.lines().count().min(r.stdout.lines().count()));
            let detail = format!(
                "{} vs {}: status {:?} vs {:?}, panic {:?} vs {:?}, first differing output line {}\n--- {}\n{}\n--- {}\n{}",
                base_name, name, base.status, r.status, base.panic, r.panic, first_diff + 1, base_name, base.stdout, name, r.stdout
            );
            findings.report("diff", &format!("{} vs {} {:016x}", base_name, name, hash(text)), text, &detail);
        }
    }
    true
}

fn borrow_mode(jobs: usize, time: Option<Duration>, out: &Path, seed: u64) {
    let findings = Arc::new(Findings::new(out));
    let (tried, accepted) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
    let start = Instant::now();
    eprintln!("gojo-fuzz borrow: {} workers, findings in {}", jobs, out.join("findings").display());
    let mut hs = vec![];
    for id in 0..jobs {
        let (findings, tried, accepted) = (findings.clone(), tried.clone(), accepted.clone());
        let work = out.join(format!("borrow-{}", id));
        let _ = std::fs::create_dir_all(&work);
        let h = std::thread::Builder::new()
            .stack_size(gojo::COMPILER_STACK)
            .spawn(move || {
                install_panic_hook();
                let mut rng = Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) ^ (id as u64 + 11) | 1);
                while !time.is_some_and(|t| start.elapsed() >= t) {
                    let text = borrowgen::BorrowGen::new(&mut rng).program();
                    tried.fetch_add(1, Ordering::Relaxed);
                    match compile(&text) {
                        Outcome::Panic(loc, msg) => {
                            findings.report("panic", &loc, &text, &msg);
                        }
                        Outcome::Accepted(_) => {
                            accepted.fetch_add(1, Ordering::Relaxed);
                            exec_check(&text, &work, &findings);
                        }
                        Outcome::Rejected(_) => {}
                    }
                }
            })
            .unwrap();
        hs.push(h);
    }
    {
        let (tried, accepted) = (tried.clone(), accepted.clone());
        let out = out.to_path_buf();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(30));
            let nf = std::fs::read_dir(out.join("findings")).map(|r| r.filter(|e| e.as_ref().is_ok_and(|e| e.path().extension().is_some_and(|x| x == "gojo"))).count()).unwrap_or(0);
            eprintln!("[{:>6.0}s] {} programs, {} accepted and run under ASan, {} findings", start.elapsed().as_secs_f64(), tried.load(Ordering::Relaxed), accepted.load(Ordering::Relaxed), nf);
        });
    }
    for h in hs {
        let _ = h.join();
    }
}

fn diff_mode(jobs: usize, time: Option<Duration>, out: &Path, seed: u64) {
    let findings = Arc::new(Findings::new(out));
    let done = Arc::new(AtomicU64::new(0));
    let start = Instant::now();
    eprintln!("gojo-fuzz diff: {} workers, findings in {}", jobs, out.join("findings").display());
    let mut hs = vec![];
    for id in 0..jobs {
        let findings = findings.clone();
        let done = done.clone();
        let out = out.to_path_buf();
        hs.push(std::thread::spawn(move || {
            let work = out.join(format!("diff-{}", id));
            let _ = std::fs::create_dir_all(&work);
            let mut rng = Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) ^ (id as u64 + 7) | 1);
            loop {
                if time.is_some_and(|t| start.elapsed() >= t) {
                    return;
                }
                let text = progen::Gen::new(&mut rng).program();
                if diff_one(&text, &work, &findings) {
                    done.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }
    let printer = {
        let done = done.clone();
        let out = out.to_path_buf();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(30));
            let nf = std::fs::read_dir(out.join("findings")).map(|r| r.filter(|e| e.as_ref().is_ok_and(|e| e.path().extension().is_some_and(|x| x == "gojo"))).count()).unwrap_or(0);
            eprintln!("[{:>6.0}s] {} programs compared, {} findings", start.elapsed().as_secs_f64(), done.load(Ordering::Relaxed), nf);
        })
    };
    for h in hs {
        let _ = h.join();
    }
    drop(printer);
}

// ---------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut jobs = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).saturating_sub(2).max(1);
    let mut time = None;
    let mut out = PathBuf::from("fuzz-out");
    let mut seed: u64 = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1) | 1;
    let mut exec_rate = 20;
    let mut limit = Duration::from_secs(10);
    let mut worker_id = None;
    let mut rest = vec![];
    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        let mut val = || {
            i += 1;
            args.get(i).cloned().unwrap_or_else(|| {
                eprintln!("missing value for {}", a);
                std::process::exit(2)
            })
        };
        match a {
            "--jobs" | "-j" => jobs = val().parse().expect("--jobs N"),
            "--time" => time = Some(Duration::from_secs(val().parse().expect("--time SECS"))),
            "--out" => out = PathBuf::from(val()),
            "--seed" => seed = val().parse().expect("--seed N"),
            "--exec-rate" => exec_rate = val().parse().expect("--exec-rate N (run 1 in N new accepted programs; 0: never)"),
            "--limit" => limit = Duration::from_secs(val().parse().expect("--limit SECS")),
            "--worker" => worker_id = Some(val().parse::<usize>().expect("--worker N")),
            _ => rest.push(args[i].clone()),
        }
        i += 1;
    }
    if let Some(id) = worker_id {
        let cfg = Config { out, seed, exec_rate, limit };
        gojo::with_compiler_stack(move || worker(id, cfg));
        return;
    }
    match rest.first().map(|s| s.as_str()) {
        Some("check") if rest.len() == 2 => check_one(&rest[1]),
        Some("diff") => diff_mode(jobs, time, &out, seed),
        Some("borrow") => borrow_mode(jobs, time, &out, seed),
        Some("borrowgen") => {
            let mut rng = Rng(seed | 1);
            print!("{}", borrowgen::BorrowGen::new(&mut rng).program());
        }
        Some("msgs") => {
            // the most common first error messages for random mutants, with an
            // example of each: for reviewing what users see
            let n: usize = rest.get(1).and_then(|x| x.parse().ok()).unwrap_or(5000);
            gojo::with_compiler_stack(move || {
                install_panic_hook();
                let corpus = load_corpus(Path::new("/nonexistent"));
                let dict = build_dict(&corpus);
                let mut rng = Rng(seed | 1);
                let mut seen: HashMap<String, (usize, String, String)> = HashMap::new();
                for _ in 0..n {
                    let base = rng.pick(&corpus).clone();
                    let m = Mutator { corpus: &corpus, dict: &dict };
                    let text = m.mutate(&mut rng, &base);
                    if let Ok(Err((sm, diags))) = std::panic::catch_unwind(AssertUnwindSafe(|| gojo::frontend(FAKE_PATH, Some(&text)).map(|_| ())))
                        && let Some(d) = diags.iter().min_by_key(|d| (d.span.file, d.span.lo)) {
                            let e = seen.entry(normalize(&d.msg)).or_insert((0, String::new(), String::new()));
                            e.0 += 1;
                            if e.1.is_empty() {
                                e.1 = gojo::diag::render(&sm, d, false);
                                e.2 = text.clone();
                            }
                        }
                }
                let mut v: Vec<_> = seen.into_iter().collect();
                v.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
                for (m, (c, ex, _)) in v {
                    println!("{:>6}  {}\n{}", c, m, ex.lines().take(6).map(|l| format!("        {}", l)).collect::<Vec<_>>().join("\n"));
                }
            });
        }
        Some("toml") => {
            let n: usize = rest.get(1).and_then(|x| x.parse().ok()).unwrap_or(100000);
            toml_mode(n, seed, &out);
        }
        Some("fmt") => {
            let n: usize = rest.get(1).and_then(|x| x.parse().ok()).unwrap_or(2000);
            gojo::with_compiler_stack(move || fmt_mode(n, seed, &out));
        }
        Some("gen") => {
            let mut rng = Rng(seed | 1);
            print!("{}", progen::Gen::new(&mut rng).program());
        }
        Some("reduce") if rest.len() >= 2 => {
            let o = rest.iter().position(|a| a == "-o").and_then(|p| rest.get(p + 1)).map(|s| s.as_str());
            reduce(&rest[1], o, limit)
        }
        None => {
            eprintln!("gojo-fuzz: {} workers, findings in {}", jobs, out.join("findings").display());
            supervise(jobs, time, &out, seed, exec_rate, limit);
        }
        _ => {
            eprintln!("usage: gojo-fuzz [--jobs N] [--time SECS] [--out DIR] [--seed N] [--exec-rate N] [--limit SECS]\n       gojo-fuzz check <file>\n       gojo-fuzz reduce <file> [-o out]\n       gojo-fuzz fmt [N]\n       gojo-fuzz toml [N]");
            std::process::exit(2);
        }
    }
}
