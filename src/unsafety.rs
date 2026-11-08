// What a package does that the compiler can't check: its `unsafe` blocks,
// `unsafe fn` declarations, `unsafe send` types and `extern` blocks. `gojo
// unsafe` lists them, and the `[unsafe]` table of gojo.toml decides which
// packages may have any (see docs/packages.md).
//
// The sites are found in the tokens, so they don't depend on type checking,
// and an `unsafe` block inside a string interpolation counts too.

use crate::diag::{Diag, SourceMap, Span};
use crate::lexer::{self, IPart, Kw, Tok, Token};
use std::collections::BTreeSet;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `unsafe { ... }`
    Block,
    /// `unsafe fn name(...)`: callers uphold conditions the compiler can't check
    Func,
    /// `unsafe send struct T`: the author vouches that T may cross tasks
    Send,
    /// `extern "C" { ... }`: C functions, which may do anything
    Extern,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Block => "unsafe block",
            Kind::Func => "unsafe fn",
            Kind::Send => "unsafe send",
            Kind::Extern => "extern block",
        }
    }
    pub fn plural(self, n: usize) -> String {
        let (one, many) = match self {
            Kind::Block => ("`unsafe` block", "`unsafe` blocks"),
            Kind::Func => ("`unsafe fn`", "`unsafe fn`s"),
            Kind::Send => ("`unsafe send` type", "`unsafe send` types"),
            Kind::Extern => ("`extern` block", "`extern` blocks"),
        };
        format!("{} {}", n, if n == 1 { one } else { many })
    }
}

#[derive(Clone, Debug)]
pub struct Site {
    pub kind: Kind,
    pub span: Span,
    /// The lines it covers (1-based, inclusive): a block or an `extern` block
    /// as a whole, the signature of an `unsafe fn`, the line of `unsafe send`.
    pub lines: (usize, usize),
    /// The function, type or library it is about, when there is one.
    pub what: Option<String>,
}

/// The sites in one source file, in order.
pub fn scan(sm: &SourceMap, file: u32) -> Vec<Site> {
    let f = sm.file(file);
    let Ok(toks) = lexer::lex(file, &f.text) else { return vec![] };
    let mut out = vec![];
    scan_toks(&toks, &mut out);
    for s in &mut out {
        let first = f.line_col(s.span.lo).0;
        let last = f.line_col(s.span.hi.saturating_sub(1).max(s.span.lo)).0;
        s.lines = (first, last);
    }
    out.sort_by_key(|s| s.span.lo);
    out
}

fn scan_toks(toks: &[Token], out: &mut Vec<Site>) {
    let p = |i: usize, s: &str| matches!(toks.get(i).map(|t| &t.tok), Some(Tok::P(x)) if *x == s);
    let mut i = 0;
    while i < toks.len() {
        match &toks[i].tok {
            Tok::Interp(parts) => {
                for part in parts {
                    if let IPart::Expr { toks, .. } = part {
                        scan_toks(toks, out);
                    }
                }
            }
            Tok::Kw(Kw::Unsafe) if p(i + 1, "{") => {
                let end = matching(toks, i + 1);
                out.push(Site { kind: Kind::Block, span: toks[i].span.to(toks[end].span), lines: (0, 0), what: None });
                // (blocks inside it are sites of their own)
            }
            Tok::Kw(Kw::Unsafe) if matches!(toks.get(i + 1).map(|t| &t.tok), Some(Tok::Kw(Kw::Func))) => {
                // the signature: up to the body's `{`
                let mut j = i + 1;
                let mut depth = 0i32;
                while j + 1 < toks.len() {
                    match &toks[j + 1].tok {
                        Tok::P("(" | "[") => depth += 1,
                        Tok::P(")" | "]") => depth -= 1,
                        Tok::P("{") if depth <= 0 => break,
                        Tok::Eof => break,
                        _ => {}
                    }
                    j += 1;
                }
                let what = match toks.get(i + 2).map(|t| &t.tok) {
                    Some(Tok::Ident(n)) => Some(n.clone()),
                    _ => None,
                };
                out.push(Site { kind: Kind::Func, span: toks[i].span.to(toks[j].span), lines: (0, 0), what });
                i = j;
            }
            Tok::Kw(Kw::Unsafe) if matches!(toks.get(i + 1).map(|t| &t.tok), Some(Tok::Ident(s)) if s == "send") => {
                // `unsafe send [copy] struct Name`
                let mut j = i + 2;
                while matches!(toks.get(j).map(|t| &t.tok), Some(Tok::Kw(_))) {
                    j += 1;
                }
                let j = j.min(toks.len() - 1);
                let what = match toks.get(j).map(|t| &t.tok) {
                    Some(Tok::Ident(n)) => Some(n.clone()),
                    _ => None,
                };
                out.push(Site { kind: Kind::Send, span: toks[i].span.to(toks[j].span), lines: (0, 0), what });
                i = j;
            }
            Tok::Kw(Kw::Extern) => {
                // `extern "C" [lib "z" | static "z"] { ... }`; not a function
                // exported to C (`extern "C" fn`), which is ordinary Gojo code
                let mut j = i + 1;
                let mut lib = None;
                while j < toks.len() {
                    match &toks[j].tok {
                        Tok::Str(s) if lib.is_none() && j > i + 1 => lib = Some(s.clone()),
                        Tok::Str(_) | Tok::Ident(_) => {}
                        _ => break,
                    }
                    j += 1;
                }
                if p(j, "{") {
                    let end = matching(toks, j);
                    out.push(Site { kind: Kind::Extern, span: toks[i].span.to(toks[end].span), lines: (0, 0), what: lib.or(Some("libc".into())) });
                    i = end;
                }
            }
            _ => {}
        }
        i += 1;
    }
}

/// The `}` that closes the `{` at `open` (or the last token).
fn matching(toks: &[Token], open: usize) -> usize {
    let mut depth = 0i32;
    for (j, t) in toks.iter().enumerate().skip(open) {
        match t.tok {
            Tok::P("{") => depth += 1,
            Tok::P("}") => {
                depth -= 1;
                if depth == 0 {
                    return j;
                }
            }
            _ => {}
        }
    }
    toks.len().saturating_sub(1)
}

/// A package of the program being built, and its unsafe code.
#[derive(Clone, Debug)]
pub struct Package {
    /// Its name in gojo.toml; without one, the program's directory or file.
    pub name: String,
    pub version: Option<String>,
    /// The directory with its gojo.toml.
    pub root: Option<PathBuf>,
    /// The package being built.
    pub is_root: bool,
    pub files: Vec<u32>,
    pub sites: Vec<(u32, Site)>,
}

impl Package {
    /// Lines covered by its sites (each line once).
    pub fn lines(&self) -> usize {
        let mut set = BTreeSet::new();
        for (f, s) in &self.sites {
            for l in s.lines.0..=s.lines.1 {
                set.insert((*f, l));
            }
        }
        set.len()
    }
    /// "2 `unsafe` blocks, 1 `extern` block; 14 lines"
    pub fn summary(&self) -> String {
        let mut parts = vec![];
        for k in [Kind::Block, Kind::Func, Kind::Send, Kind::Extern] {
            let n = self.sites.iter().filter(|(_, s)| s.kind == k).count();
            if n > 0 {
                parts.push(k.plural(n));
            }
        }
        let n = self.lines();
        format!("{}; {} line{}", parts.join(", "), n, if n == 1 { "" } else { "s" })
    }
}

/// Fills in the sites of each package.
pub fn collect(sm: &SourceMap, pkgs: &mut [Package]) {
    for p in pkgs.iter_mut() {
        p.sites = p.files.iter().flat_map(|&f| scan(sm, f).into_iter().map(move |s| (f, s))).collect();
    }
}

/// The `[unsafe]` policy of the package being built: dependencies with unsafe
/// code need to be in its `allow` list, and a package with `forbid = true`
/// may have none of its own.
pub fn check_policy(info: &crate::UnsafeInfo) -> Vec<Diag> {
    let (pkgs, policy, known, forbidding) = (&info.packages, &info.policy, &info.known, &info.forbidding);
    let mut diags = vec![];
    let file = info.root.as_ref().map(|m| m.join(crate::manifest::FILE).display().to_string()).unwrap_or_else(|| "gojo.toml".into());
    for (name, sp) in &policy.allow {
        if known.iter().any(|k| k == name) {
            continue;
        }
        let real: Vec<&String> = info.aliases.iter().filter(|(a, _)| a == name).map(|(_, r)| r).collect();
        if let Some(r) = real.first() {
            diags.push(Diag::new(*sp, format!("`{}` is what a dependency is imported as; `allow` lists packages by the name in their own gojo.toml: `{}`", name, r)));
        } else {
            let mut d = Diag::new(*sp, format!("`{}` is not a dependency of this package, directly or through others", name));
            if !known.is_empty() {
                d = d.note(None, format!("the dependencies are {}", known.iter().map(|k| format!("`{}`", k)).collect::<Vec<_>>().join(", ")));
            }
            diags.push(d);
        }
    }
    for p in pkgs.iter() {
        let Some((_, first)) = p.sites.first() else { continue };
        if forbidding.contains(&p.name) {
            for (_, s) in &p.sites {
                diags.push(Diag::new(s.span, format!("`{}` forbids unsafe code (`forbid = true` in the [unsafe] table of its gojo.toml), and this is {}", p.name, article(s.kind))).note(None, "write it without `unsafe`, or move it into a dependency that is allowed to have unsafe code"));
            }
            continue;
        }
        if p.is_root || policy.allow.iter().any(|(n, _)| *n == p.name) {
            continue;
        }
        let at = p.root.as_ref().map(|r| format!(" ({})", r.display())).unwrap_or_default();
        diags.push(
            Diag::new(first.span, format!("dependency `{}`{} has unsafe code ({}), and this package's gojo.toml doesn't allow it", p.name, at, p.summary()))
                .note(None, format!("`gojo unsafe` lists every site; once you have reviewed them, allow it in {}:\n  [unsafe]\n  allow = [\"{}\"]", file, p.name)),
        );
    }
    diags
}

fn article(k: Kind) -> &'static str {
    match k {
        Kind::Block => "an `unsafe` block",
        Kind::Func => "an `unsafe fn`",
        Kind::Send => "an `unsafe send` type",
        Kind::Extern => "an `extern` block",
    }
}
