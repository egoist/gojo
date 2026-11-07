// Kite compiler: library interface shared by the `kite` driver and the fuzzer.

pub mod ast;
pub mod borrowck;
pub mod check;
pub mod codegen;
pub mod diag;
pub mod fmtspec;
pub mod formatter;
pub mod doc;
pub mod manifest;
pub mod lexer;
pub mod parser;
pub mod tir;
pub mod types;

use diag::{Diag, SourceMap, Span};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub const STD: &[(&str, &str)] = &[
    ("prelude", include_str!("../std/prelude.kite")),
    ("fmt", include_str!("../std/fmt.kite")),
    ("os", include_str!("../std/os.kite")),
    ("math", include_str!("../std/math.kite")),
    ("time", include_str!("../std/time.kite")),
    ("task", include_str!("../std/task.kite")),
    ("net", include_str!("../std/net.kite")),
    ("io", include_str!("../std/io.kite")),
    ("bufio", include_str!("../std/bufio.kite")),
    ("json", include_str!("../std/json.kite")),
    ("http", include_str!("../std/http.kite")),
    ("strings", include_str!("../std/strings.kite")),
];

pub const RUNTIME_H: &str = include_str!("../runtime/kite_rt.h");
pub const RUNTIME_C: &str = concat!(include_str!("../runtime/kite_rt.c"), include_str!("../runtime/kite_sched.c"), include_str!("../runtime/kite_os.c"));

pub fn report(sm: &SourceMap, diags: &[Diag]) {
    use std::io::IsTerminal;
    let color = std::io::stderr().is_terminal();
    let mut ds: Vec<&Diag> = diags.iter().collect();
    ds.sort_by_key(|d| (d.span.file, d.span.lo));
    ds.dedup_by_key(|d| (d.span, d.msg.clone()));
    let n = ds.len();
    for d in ds.iter().take(50) {
        eprint!("{}", diag::render(sm, d, color));
    }
    if n > 50 {
        eprintln!("... and {} more errors", n - 50);
    }
    eprintln!("{} error{} found", n, if n == 1 { "" } else { "s" });
}

pub struct Loaded {
    pub sm: SourceMap,
    pub units: Vec<check::Unit>,
}

fn parse_source(sm: &mut SourceMap, path: String, text: String) -> Result<ast::File, Diag> {
    let id = sm.add(path, text);
    let toks = lexer::lex(id, &sm.file(id).text.clone())?;
    parser::parse_file(id, toks)
}

/// The manifest of the package rooted at `root`, read once. `None` inside
/// means it had errors (already reported).
fn manifest_of<'m>(root: &Path, sm: &mut SourceMap, diags: &mut Vec<Diag>, manifests: &'m mut HashMap<PathBuf, Option<manifest::Manifest>>) -> Option<&'m Option<manifest::Manifest>> {
    if !manifests.contains_key(root) {
        let m = match manifest::read(sm, root) {
            Ok(m) => Some(m),
            Err(ds) => {
                diags.extend(ds);
                None
            }
        };
        manifests.insert(root.to_path_buf(), m);
    }
    manifests.get(root)
}

/// `to` relative to `from`, for messages (`../store`).
fn pathdiff(to: &Path, from: &Path) -> Option<String> {
    let to: Vec<_> = to.components().collect();
    let from: Vec<_> = from.components().collect();
    let common = to.iter().zip(from.iter()).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = vec!["..".into(); from.len() - common];
    parts.extend(to[common..].iter().map(|c| c.as_os_str().to_string_lossy().into_owned()));
    Some(if parts.is_empty() { ".".into() } else { parts.join("/") })
}

/// Loads `input` (a file or a package directory) and everything it imports.
/// With `source`, `input` names a single file whose text is `source` instead of
/// being read from disk.
pub fn load(input: &str, source: Option<&str>) -> Result<Loaded, (SourceMap, Vec<Diag>)> {
    let mut sm = SourceMap::default();
    let mut diags = vec![];
    let in_path = PathBuf::from(input);
    let (root, main_files): (PathBuf, Vec<PathBuf>) = if source.is_none() && in_path.is_dir() {
        let mut fs: Vec<PathBuf> = std::fs::read_dir(&in_path).map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().map(|e| e == "kite").unwrap_or(false)).collect()).unwrap_or_default();
        fs.sort();
        (in_path.clone(), fs)
    } else {
        (in_path.parent().map(|p| p.to_path_buf()).unwrap_or_default(), vec![in_path.clone()])
    };
    if main_files.is_empty() {
        eprintln!("error: no .kite files found in {}", input);
        std::process::exit(1);
    }

    // parse std
    let mut std_files: HashMap<&str, ast::File> = HashMap::new();
    for (name, src) in STD {
        match parse_source(&mut sm, format!("<std>/{}.kite", name), src.to_string()) {
            Ok(f) => {
                std_files.insert(name, f);
            }
            Err(d) => diags.push(d),
        }
    }

    // parse user packages (main + imports, recursively)
    let mut user: Vec<(String, String, Vec<ast::File>)> = vec![]; // (path key, pkg name, files)
    let mut main_parsed = vec![];
    for f in &main_files {
        let text = match source {
            Some(s) => Ok(s.to_string()),
            None => std::fs::read_to_string(f),
        };
        match text {
            Ok(text) => match parse_source(&mut sm, f.display().to_string(), text) {
                Ok(file) => main_parsed.push(file),
                Err(d) => diags.push(d),
            },
            Err(e) => {
                eprintln!("error: cannot read {}: {}", f.display(), e);
                std::process::exit(1);
            }
        }
    }
    if !diags.is_empty() {
        return Err((sm, diags));
    }
    // dependency discovery: `use name` is a standard module, `use ./dir` a package in this project
    let mut order: Vec<String> = vec![];
    let mut visiting: HashSet<String> = HashSet::new();
    let mut parsed: HashMap<String, (String, Vec<ast::File>)> = HashMap::new();
    let mut std_needed: Vec<String> = vec![];
    let mut names_used: HashSet<String> = ["main".to_string()].into_iter().chain(STD.iter().map(|(n, _)| n.to_string())).collect();
    let main_key = if source.is_none() && in_path.is_dir() { in_path.canonicalize().map(|p| p.display().to_string()).unwrap_or_default() } else { String::new() };

    fn std_module(name: &str) -> bool {
        name != "prelude" && STD.iter().any(|(n, _)| *n == name)
    }
    fn std_list() -> String {
        STD.iter().filter(|(n, _)| *n != "prelude").map(|(n, _)| format!("`{}`", n)).collect::<Vec<_>>().join(", ")
    }

    fn is_ident(s: &str) -> bool {
        let mut cs = s.chars();
        matches!(cs.next(), Some(c) if c.is_alphabetic() || c == '_') && cs.all(|c| c.is_alphanumeric() || c == '_')
    }

    #[allow(clippy::too_many_arguments)]
    fn visit(
        key: &str,
        dir: &Path,
        files: &mut [ast::File],
        main_key: &str,
        sm: &mut SourceMap,
        diags: &mut Vec<Diag>,
        order: &mut Vec<String>,
        visiting: &mut HashSet<String>,
        parsed: &mut HashMap<String, (String, Vec<ast::File>)>,
        std_needed: &mut Vec<String>,
        names_used: &mut HashSet<String>,
        manifests: &mut HashMap<PathBuf, Option<manifest::Manifest>>,
        pkg: Option<&Path>,
    ) {
        visiting.insert(key.to_string());
        for f in files.iter_mut() {
            for imp in f.imports.iter_mut() {
                let last = imp.path.rsplit('/').next().unwrap_or("").to_string();
                if imp.alias.is_none() {
                    if !is_ident(&last) {
                        diags.push(Diag::new(imp.span, format!("`{}` is not a valid name; bind the package with `as`", last)));
                        continue;
                    }
                    imp.alias = Some(last.clone());
                }
                if !imp.path.starts_with("./") && !imp.path.starts_with("../") {
                    if let Some(m) = imp.path.strip_prefix("std/") {
                        if std_module(m) {
                            if !std_needed.iter().any(|n| n == m) {
                                std_needed.push(m.to_string());
                            }
                            // from here on, the standard module's own name
                            imp.path = m.to_string();
                        } else {
                            diags.push(Diag::new(imp.span, format!("there is no standard module `{}` (the standard library has {})", m, std_list())));
                        }
                        continue;
                    }
                }
                // the directory imported, and the package it belongs to
                let (target, target_pkg) = if imp.path.starts_with("./") || imp.path.starts_with("../") {
                    let target = match dir.join(&imp.path).canonicalize() {
                        Ok(t) if t.is_dir() => t,
                        _ => {
                            let shown: PathBuf = dir.join(&imp.path).components().filter(|c| !matches!(c, std::path::Component::CurDir)).collect();
                            diags.push(Diag::new(imp.span, format!("cannot find package `{}` (looked in {})", imp.path, shown.display())));
                            continue;
                        }
                    };
                    if let Some(p) = pkg {
                        if !target.starts_with(p) {
                            diags.push(Diag::new(imp.span, format!("`{}` is outside this package (the directory with its kite.toml, {}): make it a package and depend on it instead", imp.path, p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())));
                            continue;
                        }
                    }
                    let tp = manifest::find_root(&target);
                    if tp.as_deref() != pkg && tp.is_some() {
                        let rel = tp.as_ref().and_then(|t| pathdiff(t, pkg.unwrap_or(dir))).unwrap_or_else(|| imp.path.clone());
                        diags.push(Diag::new(imp.span, format!("`{}` is a separate package (it has its own kite.toml): add `{} = {{ path = \"{}\" }}` to [dependencies] and write `use {}`", imp.path, last, rel, last)));
                        continue;
                    }
                    (target, pkg.map(|p| p.to_path_buf()))
                } else {
                    // a dependency, or a package inside one
                    let (name, sub) = imp.path.split_once('/').unwrap_or((imp.path.as_str(), ""));
                    // (a standard module's name, when it isn't a dependency's)
                    let std_hint = std_module(&imp.path).then(|| format!("standard modules are written with `std/`: `use std/{}`", imp.path));
                    let Some(root) = pkg else {
                        let msg = std_hint.clone().unwrap_or_else(|| format!("unknown package `{}`: a package in this project is written `use ./{}`, and dependencies are listed in a kite.toml (the standard library has {})", imp.path, imp.path, std_list()));
                        diags.push(Diag::new(imp.span, msg));
                        continue;
                    };
                    let Some(Some(m)) = manifest_of(root, sm, diags, manifests) else { continue };
                    let pname = m.package.as_ref().map(|p| p.name.clone()).unwrap_or_default();
                    let Some(dep) = m.dependency(name).cloned() else {
                        let msg = std_hint.clone().unwrap_or_else(|| format!("`{}` is not a dependency of `{}`: add it to [dependencies] in its kite.toml", name, pname));
                        diags.push(Diag::new(imp.span, msg));
                        continue;
                    };
                    let droot = match &dep.source {
                        manifest::Source::Path(d) => d.clone(),
                        manifest::Source::Registry(v) => {
                            diags.push(Diag::new(imp.span, format!("`{}` = \"{}\" comes from the registry, which isn't available yet: use `{} = {{ path = \"...\" }}` for now", name, v, name)));
                            continue;
                        }
                        manifest::Source::Git(g) => {
                            diags.push(Diag::new(imp.span, format!("`{}` comes from git ({}), which isn't available yet: use `{} = {{ path = \"...\" }}` for now", name, g, name)));
                            continue;
                        }
                    };
                    let Some(Some(dm)) = manifest_of(&droot, sm, diags, manifests) else { continue };
                    let Some(dpkg) = &dm.package else {
                        diags.push(Diag::new(imp.span, format!("`{}` ({}) is a workspace, not a package", name, droot.display())));
                        continue;
                    };
                    if !dpkg.exports.iter().any(|e| e == sub) {
                        let list = dpkg.exports.iter().map(|e| if e.is_empty() { format!("`{}`", name) } else { format!("`{}/{}`", name, e) }).collect::<Vec<_>>().join(", ");
                        let what = if sub.is_empty() { format!("`{}` doesn't export its root package", name) } else { format!("`{}` doesn't export `{}`", name, sub) };
                        diags.push(Diag::new(imp.span, format!("{} (it exports {}; see `exports` in its kite.toml)", what, if list.is_empty() { "nothing".to_string() } else { list })));
                        continue;
                    }
                    let target = match droot.join(sub).canonicalize() {
                        Ok(t) if t.is_dir() => t,
                        _ => {
                            diags.push(Diag::new(imp.span, format!("`{}` has no package `{}`", name, sub)));
                            continue;
                        }
                    };
                    (target, Some(droot))
                };
                let tkey = target.display().to_string();
                let tkey = if tkey == main_key { "\0main".to_string() } else { tkey };
                imp.path = tkey.clone();
                if visiting.contains(&tkey) {
                    diags.push(Diag::new(imp.span, "import cycle: this package is already being imported".to_string()));
                    continue;
                }
                if parsed.contains_key(&tkey) {
                    continue;
                }
                let mut fs: Vec<PathBuf> = std::fs::read_dir(&target).map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().map(|e| e == "kite").unwrap_or(false)).collect()).unwrap_or_default();
                fs.sort();
                if fs.is_empty() {
                    diags.push(Diag::new(imp.span, format!("package `{}` has no .kite files ({})", last, target.display())));
                    continue;
                }
                let mut pfs = vec![];
                for p in fs {
                    let text = std::fs::read_to_string(&p).unwrap_or_default();
                    match parse_source(sm, p.display().to_string(), text) {
                        Ok(file) => pfs.push(file),
                        Err(d) => diags.push(d),
                    }
                }
                // internal package name: the directory name, made unique
                let base: String = last.chars().map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' }).collect();
                let mut name = base.clone();
                let mut n = 2;
                while names_used.contains(&name) {
                    name = format!("{}_{}", base, n);
                    n += 1;
                }
                names_used.insert(name.clone());
                parsed.insert(tkey.clone(), (name.clone(), vec![]));
                visit(&tkey, &target, &mut pfs, main_key, sm, diags, order, visiting, parsed, std_needed, names_used, manifests, target_pkg.as_deref());
                parsed.insert(tkey.clone(), (name, pfs));
                order.push(tkey);
            }
        }
        visiting.remove(key);
    }
    // the package the program belongs to: the nearest kite.toml above it
    let mut manifests: HashMap<PathBuf, Option<manifest::Manifest>> = HashMap::new();
    let main_pkg = if source.is_none() { manifest::find_root(&root) } else { None };
    if let Some(r) = &main_pkg {
        if let Some(Some(m)) = manifest_of(r, &mut sm, &mut diags, &mut manifests) {
            if m.package.is_none() {
                diags.push(Diag::new(Span::new(m.file, 0, 0), format!("{} belongs to a workspace without a [package]: run a member of it", input)));
            }
        }
    }
    if !diags.is_empty() {
        return Err((sm, diags));
    }
    visit("\0main", &root, &mut main_parsed, &main_key, &mut sm, &mut diags, &mut order, &mut visiting, &mut parsed, &mut std_needed, &mut names_used, &mut manifests, main_pkg.as_deref());
    for key in &order {
        let (name, files) = parsed.remove(key).unwrap_or_default();
        user.push((key.clone(), name, files));
    }
    if !diags.is_empty() {
        return Err((sm, diags));
    }

    // std modules: the prelude, everything used, and what std itself depends on
    let mut units = vec![];
    for f in std_files.values_mut() {
        for imp in f.imports.iter_mut() {
            // std imports std as `use std/io`, like any other code
            if let Some(m) = imp.path.strip_prefix("std/") {
                imp.path = m.to_string();
            }
            if imp.alias.is_none() {
                imp.alias = Some(imp.path.clone());
            }
        }
    }
    let add_std = |name: &str, units: &mut Vec<check::Unit>, std_files: &mut HashMap<&str, ast::File>| {
        if let Some(f) = std_files.remove(name) {
            units.push(check::Unit { name: name.to_string(), path: name.to_string(), is_std: true, files: vec![f] });
        }
    };
    add_std("prelude", &mut units, &mut std_files);
    // the modules used, after the std modules they import
    fn visit_std(name: &str, std_files: &mut HashMap<&str, ast::File>, units: &mut Vec<check::Unit>, add: &dyn Fn(&str, &mut Vec<check::Unit>, &mut HashMap<&str, ast::File>)) {
        let deps: Vec<String> = match std_files.get(name) {
            Some(f) => f.imports.iter().map(|i| i.path.clone()).collect(),
            None => return,
        };
        // taken out first: an import cycle in std must not loop
        let f = std_files.remove(name);
        for d in deps {
            visit_std(&d, std_files, units, add);
        }
        if let Some(f) = f {
            std_files.insert(STD.iter().find(|(n, _)| *n == name).map(|(n, _)| *n).unwrap_or("?"), f);
            add(name, units, std_files);
        }
    }
    let mut wanted: Vec<String> = vec!["fmt".into(), "os".into()];
    wanted.extend(std_needed.iter().cloned());
    for s in &wanted {
        visit_std(s, &mut std_files, &mut units, &add_std);
    }
    for (key, name, files) in user {
        units.push(check::Unit { name, path: key, is_std: false, files });
    }
    units.push(check::Unit { name: "main".into(), path: "\0main".into(), is_std: false, files: main_parsed });
    Ok(Loaded { sm, units })
}

/// Stack for the compiler's thread. Every phase walks the syntax tree recursively;
/// the parser bounds its depth (`parser::MAX_NEST`) and this is enough for that
/// depth in every phase, debug builds included. Only the pages used are committed.
pub const COMPILER_STACK: usize = 1 << 30;

/// Runs `f` on a thread with `COMPILER_STACK` bytes of stack.
pub fn with_compiler_stack<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
    match std::thread::Builder::new().name("kite".into()).stack_size(COMPILER_STACK).spawn(f) {
        Ok(h) => match h.join() {
            Ok(r) => r,
            Err(e) => std::panic::resume_unwind(e),
        },
        Err(e) => {
            eprintln!("error: cannot start the compiler thread: {}", e);
            std::process::exit(1);
        }
    }
}

/// Everything the backend needs: a checked program.
pub struct Checked {
    pub sm: SourceMap,
    pub prog: types::Program,
    pub tfuncs: Vec<tir::TFunc>,
    pub tests: Vec<(String, types::FuncId)>,
}

/// Parse, type check and borrow check.
pub fn frontend(input: &str, source: Option<&str>) -> Result<Checked, (SourceMap, Vec<Diag>)> {
    let loaded = load(input, source)?;
    let sm = loaded.sm;
    let (prog, tfuncs, tests) = match check::check_program(loaded.units) {
        Ok(r) => r,
        Err(diags) => return Err((sm, diags)),
    };
    let bdiags = borrowck::check(&prog, &tfuncs);
    if !bdiags.is_empty() {
        return Err((sm, bdiags));
    }
    Ok(Checked { sm, prog, tfuncs, tests })
}
