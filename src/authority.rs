//! What a dependency may do: the `[authority]` table of gojo.toml (spec 2.1).
//!
//! A program reaches the file system, the environment, other programs and
//! the network only through a few functions of the standard library (C is
//! behind `[unsafe]`). The table limits which of these each dependency may
//! call:
//!
//! ```toml
//! [authority]
//! leftpad = []            # none of them
//! s3 = ["net"]
//! "*" = []                # every dependency not listed
//! ```
//!
//! The check is on calls (and references) to those functions, in the code of
//! the restricted package and of every package it depends on, directly or
//! not: a dependency's code acts for whoever calls it. Values handed in are
//! not limited: a package without `fs` may still write to the `os.File` or
//! `io.Writer` its caller passes it, which is how authority is delegated.
//! Unlisted dependencies (with no `"*"`) and the package being built may do
//! anything.

use crate::diag::{Diag, Span};
use crate::tir::{self, Callee, TExpr, TK};
use crate::types::{FuncId, Program, TypeDefKind};
use crate::UnsafeInfo;

/// The kinds of authority, and what they cover (for messages).
pub const KINDS: [(&str, &str); 4] = [("fs", "the file system"), ("env", "environment variables"), ("process", "other programs and signals"), ("net", "the network")];

/// What a standard library function needs, by module and name. Methods need
/// nothing: calling one needs a value, which only these functions make or
/// the caller hands over.
pub fn needs(module: &str, name: &str) -> &'static [&'static str] {
    match (module, name) {
        ("os", "open" | "create" | "append" | "readFile" | "readText" | "writeFile" | "writeText" | "stat" | "lstat" | "exists" | "readDir" | "mkdir" | "mkdirAll" | "remove" | "removeAll" | "rename" | "cwd" | "chdir" | "tempDir" | "walk") => &["fs"],
        ("os", "getenv" | "setenv" | "unsetenv" | "environ") => &["env"],
        ("os", "run" | "runWithInput" | "call" | "command" | "notify") => &["process"],
        ("path", "abs") => &["fs"],
        ("net", "listen" | "dial" | "listenUdp" | "dialUdp" | "lookup") => &["net"],
        ("tls", "dial" | "dialWith") => &["net"],
        // and reads the certificate and key files
        ("tls", "listen") => &["net", "fs"],
        ("http", "request" | "get" | "post") => &["net"],
        _ => &[],
    }
}

/// The modules with functions in `needs`: every public function of theirs
/// is in it or in `FREE` (a test keeps it so).
pub const MODULES: [&str; 5] = ["os", "path", "net", "tls", "http"];

/// The public functions of `MODULES` that need nothing: pure ones, the
/// standard streams, the program's arguments, exiting (which `panic` can do
/// anyway), and those that take the listener or connection they use.
pub const FREE: &[(&str, &str)] = &[
    ("os", "args"),
    ("os", "exit"),
    ("os", "stdin"),
    ("os", "stdout"),
    ("os", "stderr"),
    ("os", "joinPath"),
    ("os", "dirName"),
    ("os", "baseName"),
    ("os", "pid"),
    ("tls", "client"),
    ("http", "findHeader"),
    ("http", "text"),
    ("http", "json"),
    ("http", "statusText"),
    ("http", "serve"),
    ("http", "serveCounting"),
    ("http", "serveTls"),
    ("http", "serveTlsCounting"),
    ("http", "decode"),
    ("http", "encode"),
    ("path", "join"),
    ("path", "joinAll"),
    ("path", "clean"),
    ("path", "base"),
    ("path", "dir"),
    ("path", "ext"),
    ("path", "stem"),
    ("path", "withExt"),
    ("path", "isAbsolute"),
    ("path", "components"),
    ("path", "relative"),
    ("path", "match"),
];

/// A call that needs authority, in a package's code.
pub struct Use {
    /// Index into `UnsafeInfo::packages`.
    pub pkg: usize,
    pub kind: &'static str,
    /// `os.readText`.
    pub what: String,
    pub span: Span,
}

fn std_need(prog: &Program, f: FuncId) -> Option<(&'static [&'static str], String)> {
    let d = &prog.funcs[f as usize];
    let p = &prog.packages[d.pkg as usize];
    if !p.is_std || d.recv.is_some() || d.owner_ty.is_some() {
        return None;
    }
    let need = needs(&p.name, &d.name);
    if need.is_empty() {
        return None;
    }
    Some((need, format!("{}.{}", p.name, d.name)))
}

/// Every use of authority in the packages' code (the standard library's own
/// is not counted), in source order per package.
pub fn uses(prog: &Program, tfuncs: &[tir::TFunc], info: &UnsafeInfo) -> Vec<Use> {
    let pkg_of = |file: u32| info.packages.iter().position(|p| p.files.contains(&file));
    let mut out: Vec<Use> = vec![];
    let scan = |pkg: usize, e: &TExpr, out: &mut Vec<Use>| {
        let f = match &e.kind {
            TK::Call { callee: Callee::Func(f, _), .. } => *f,
            TK::FuncRef(f, _) => *f,
            _ => return,
        };
        if let Some((need, what)) = std_need(prog, f) {
            for k in need {
                out.push(Use { pkg, kind: k, what: what.clone(), span: e.span });
            }
        }
    };
    for tf in tfuncs {
        let Some(pkg) = pkg_of(prog.funcs[tf.func as usize].span.file) else { continue };
        tir::visit_block(&tf.body, &mut |e| scan(pkg, e, &mut out));
    }
    // field defaults run where the value is built, maybe in another package
    for d in &prog.types {
        let Some(pkg) = pkg_of(d.span.file) else { continue };
        if let TypeDefKind::Struct { fields } = &d.kind {
            for fi in fields {
                if let Some(e) = &fi.default {
                    tir::visit_expr(e, &mut |e| scan(pkg, e, &mut out));
                }
            }
        }
    }
    for c in &prog.consts {
        let Some(pkg) = pkg_of(c.span.file) else { continue };
        if let Some(e) = &c.value {
            tir::visit_expr(e, &mut |e| scan(pkg, e, &mut out));
        }
    }
    out.sort_by_key(|u| (u.pkg, u.span.file, u.span.lo));
    out
}

/// What each package needs, by name: the kinds its own code uses and those
/// of the packages it depends on, directly or not (what `[authority]` would
/// have to grant it), sorted as in `KINDS`. The package being built is left out.
pub fn needed(info: &UnsafeInfo, uses: &[Use]) -> Vec<(String, Vec<&'static str>)> {
    let mut out = vec![];
    for p in info.packages.iter().filter(|p| !p.is_root) {
        let mut reach = vec![p.name.as_str()];
        let mut i = 0;
        while i < reach.len() {
            for (from, to) in &info.deps {
                if from == reach[i] && !reach.contains(&to.as_str()) {
                    reach.push(to);
                }
            }
            i += 1;
        }
        let kinds: Vec<&'static str> = KINDS.iter().map(|(k, _)| *k).filter(|k| uses.iter().any(|u| u.kind == *k && reach.contains(&info.packages[u.pkg].name.as_str()))).collect();
        out.push((p.name.clone(), kinds));
    }
    out
}

/// What limits a package: the `[authority]` entries that apply to it, as
/// (the package whose entry it is, whether that entry is `"*"`, the kinds
/// granted).
pub fn limits<'i>(info: &'i UnsafeInfo, pkg: &'i str) -> Vec<(&'i str, bool, &'i [String])> {
    let grants = &info.authority.grants;
    let entry = |name: &'i str| -> Option<(&'i str, bool, &'i [String])> {
        if let Some((_, _, g)) = grants.iter().find(|(n, _, _)| n == name) {
            return Some((name, false, g));
        }
        grants.iter().find(|(n, _, _)| n == "*").map(|(_, _, g)| (name, true, g.as_slice()))
    };
    let root = info.packages.iter().find(|p| p.is_root).map(|p| p.name.as_str());
    // the package and every package that depends on it, directly or not
    let mut seen: Vec<&str> = vec![pkg];
    let mut todo = vec![pkg];
    while let Some(n) = todo.pop() {
        for (from, to) in &info.deps {
            if to == n && !seen.contains(&from.as_str()) && Some(from.as_str()) != root {
                seen.push(from);
                todo.push(from);
            }
        }
    }
    seen.into_iter().filter_map(entry).collect()
}

/// Errors for the table's names: each is a dependency, by the name in its own
/// gojo.toml, or `*`. Runs with the `[unsafe]` checks, before type checking.
pub fn check_table(info: &UnsafeInfo) -> Vec<Diag> {
    let mut diags = vec![];
    let root = info.packages.iter().find(|p| p.is_root).map(|p| p.name.clone());
    for (name, sp, _) in &info.authority.grants {
        if name == "*" || info.known.contains(name) {
            continue;
        }
        if Some(name) == root.as_ref() {
            diags.push(Diag::new(*sp, format!("`{}` is the package being built, which [authority] doesn't limit: it lists dependencies", name)));
            continue;
        }
        let real: Vec<&String> = info.aliases.iter().filter(|(a, _)| a == name).map(|(_, r)| r).collect();
        if let Some(r) = real.first() {
            diags.push(Diag::new(*sp, format!("`{}` is what a dependency is imported as; [authority] lists packages by the name in their own gojo.toml: `{}`", name, r)));
        } else {
            let mut d = Diag::new(*sp, format!("`{}` is not a dependency of this package, directly or through others", name));
            if !info.known.is_empty() {
                d = d.note(None, format!("the dependencies are {}", info.known.iter().map(|k| format!("`{}`", k)).collect::<Vec<_>>().join(", ")));
            }
            diags.push(d);
        }
    }
    diags
}

/// Errors for uses of authority the table doesn't grant (after `check_table`).
pub fn check(prog: &Program, tfuncs: &[tir::TFunc], info: &UnsafeInfo) -> Vec<Diag> {
    if info.authority.grants.is_empty() {
        return vec![];
    }
    let mut diags = vec![];
    let file = info.root.as_ref().map(|m| m.join(crate::manifest::FILE).display().to_string()).unwrap_or_else(|| "gojo.toml".into());
    let mut reported: Vec<(usize, String)> = vec![];
    for u in uses(prog, tfuncs, info) {
        let p = &info.packages[u.pkg];
        if p.is_root {
            continue;
        }
        let lim = limits(info, &p.name);
        let Some((by, star, granted)) = lim.iter().find(|(_, _, g)| !g.iter().any(|k| k == u.kind)) else { continue };
        // one error per function per package
        let key = (u.pkg, u.what.clone());
        if reported.contains(&key) {
            continue;
        }
        reported.push(key);
        let what = KINDS.iter().find(|(k, _)| *k == u.kind).map(|(_, w)| *w).unwrap_or(u.kind);
        let list = |g: &[String]| format!("[{}]", g.iter().map(|k| format!("\"{}\"", k)).collect::<Vec<_>>().join(", "));
        let mut more: Vec<String> = granted.to_vec();
        more.push(u.kind.to_string());
        let d = if *by == p.name {
            let why = if *star { format!("it isn't listed in the [authority] table of {}, and `\"*\" = {}`", file, list(granted)) } else { format!("[authority] in {} gives it {}", file, list(granted)) };
            Diag::new(u.span, format!("`{}` uses {} (`{}`), which dependency `{}` may not: {}", u.what, what, u.kind, p.name, why)).note(None, format!("once you have checked that it should, allow it in {}:\n  [authority]\n  {} = {}", file, quoted(&p.name), list(&more)))
        } else {
            let why = if *star { format!("`{}` isn't listed in the [authority] table of {}, and `\"*\" = {}`", by, file, list(granted)) } else { format!("[authority] in {} gives `{}` {}", file, by, list(granted)) };
            Diag::new(u.span, format!("`{}` uses {} (`{}`), and `{}` may not: `{}` depends on it, and {}", u.what, what, u.kind, p.name, by, why)).note(None, format!("a dependency's code acts for every package that uses it: allow `{}` for `{}` too, or depend on something else", u.kind, by))
        };
        diags.push(d);
    }
    diags
}

/// A package name as a TOML key.
pub fn quoted(name: &str) -> String {
    if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') { name.to_string() } else { format!("\"{}\"", name) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every public function of the modules with authority is classified, so
    /// a new one can't slip through as free.
    #[test]
    fn std_functions_classified() {
        let srcs = [("os", include_str!("../std/os.gojo")), ("path", include_str!("../std/path.gojo")), ("net", include_str!("../std/net.gojo")), ("tls", include_str!("../std/tls.gojo")), ("http", include_str!("../std/http.gojo"))];
        let mut missing = vec![];
        for (m, src) in srcs {
            assert!(MODULES.contains(&m));
            for line in src.lines() {
                // associated functions would need a case in `std_need`
                assert!(!(line.starts_with("    pub fn ") && !line.contains("self")), "{}: an associated function in a module with authority: {}", m, line);
                let Some(rest) = line.strip_prefix("pub fn ") else { continue };
                let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
                if needs(m, &name).is_empty() && !FREE.contains(&(m, name.as_str())) {
                    missing.push(format!("{}.{}", m, name));
                }
            }
        }
        assert!(missing.is_empty(), "classify these in authority::needs or FREE: {:?}", missing);
    }
}
