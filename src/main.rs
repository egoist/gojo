// Kite compiler driver.

mod ast;
mod borrowck;
mod check;
mod codegen;
mod diag;
mod lexer;
mod parser;
mod tir;
mod types;

use diag::{Diag, SourceMap};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Instant;

const STD: &[(&str, &str)] = &[
    ("prelude", include_str!("../std/prelude.kite")),
    ("fmt", include_str!("../std/fmt.kite")),
    ("os", include_str!("../std/os.kite")),
    ("math", include_str!("../std/math.kite")),
    ("time", include_str!("../std/time.kite")),
    ("task", include_str!("../std/task.kite")),
    ("net", include_str!("../std/net.kite")),
    ("strings", include_str!("../std/strings.kite")),
];

pub const RUNTIME_H: &str = include_str!("../runtime/kite_rt.h");
pub const RUNTIME_C: &str = concat!(include_str!("../runtime/kite_rt.c"), include_str!("../runtime/kite_sched.c"));

struct Options {
    release: bool,
    output: Option<String>,
    verbose: bool,
    cc: Option<String>,
    target: Option<String>,
    keep_c: bool,
}

fn usage() -> ExitCode {
    eprintln!(
        "Kite — a systems language with Go's simplicity, no GC and no nil.

usage:
  kite run   <file.kite|dir> [args...]   compile and run
  kite build <file.kite|dir> [-o out]     compile to a native binary
  kite check <file.kite|dir>              type and borrow check only
  kite test  <file.kite|dir>              run `test \"...\" {{}}` blocks
  kite emit-c <file.kite|dir>             print the generated C

options:
  --release        optimize (overflow checks stay on)
  --cc <compiler>  C compiler for the backend (default: $KITE_CC, cc)
  --target <t>     cross-compile with `zig cc -target <t>`
  -v               print timings"
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        return usage();
    }
    let cmd = args[1].as_str();
    let mut opts = Options { release: false, output: None, verbose: false, cc: std::env::var("KITE_CC").ok(), target: None, keep_c: false };
    let mut input = None;
    let mut prog_args = vec![];
    let mut i = 2;
    while i < args.len() {
        let a = &args[i];
        if input.is_some() && cmd == "run" {
            prog_args.push(a.clone());
            i += 1;
            continue;
        }
        match a.as_str() {
            "--release" => opts.release = true,
            "-v" => opts.verbose = true,
            "--keep-c" => opts.keep_c = true,
            "-o" => {
                i += 1;
                opts.output = args.get(i).cloned();
            }
            "--cc" => {
                i += 1;
                opts.cc = args.get(i).cloned();
            }
            "--target" => {
                i += 1;
                opts.target = args.get(i).cloned();
            }
            _ => input = Some(a.clone()),
        }
        i += 1;
    }
    let input = match input {
        Some(i) => i,
        None => return usage(),
    };
    match cmd {
        "run" | "build" | "check" | "test" | "emit-c" => {}
        _ => return usage(),
    }
    match drive(cmd, &input, &opts, &prog_args) {
        Ok(code) => code,
        Err(()) => ExitCode::from(1),
    }
}

fn report(sm: &SourceMap, diags: &[Diag]) {
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

struct Loaded {
    sm: SourceMap,
    units: Vec<check::Unit>,
}

fn parse_source(sm: &mut SourceMap, path: String, text: String) -> Result<ast::File, Diag> {
    let id = sm.add(path, text);
    let toks = lexer::lex(id, &sm.file(id).text.clone())?;
    parser::parse_file(id, toks)
}

fn load(input: &str) -> Result<Loaded, (SourceMap, Vec<Diag>)> {
    let mut sm = SourceMap::default();
    let mut diags = vec![];
    let in_path = PathBuf::from(input);
    let (root, main_files): (PathBuf, Vec<PathBuf>) = if in_path.is_dir() {
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
        match std::fs::read_to_string(f) {
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
    let main_key = if in_path.is_dir() { in_path.canonicalize().map(|p| p.display().to_string()).unwrap_or_default() } else { String::new() };

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
                    if STD.iter().any(|(n, _)| *n == imp.path && *n != "prelude") {
                        if !std_needed.contains(&imp.path) {
                            std_needed.push(imp.path.clone());
                        }
                    } else {
                        diags.push(Diag::new(imp.span, format!("unknown standard module `{}` (packages in this project are written `use ./{}`)", imp.path, imp.path)));
                    }
                    continue;
                }
                let target = match dir.join(&imp.path).canonicalize() {
                    Ok(t) if t.is_dir() => t,
                    _ => {
                        let shown: PathBuf = dir.join(&imp.path).components().filter(|c| !matches!(c, std::path::Component::CurDir)).collect();
                        diags.push(Diag::new(imp.span, format!("cannot find package `{}` (looked in {})", imp.path, shown.display())));
                        continue;
                    }
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
                visit(&tkey, &target, &mut pfs, main_key, sm, diags, order, visiting, parsed, std_needed, names_used);
                parsed.insert(tkey.clone(), (name, pfs));
                order.push(tkey);
            }
        }
        visiting.remove(key);
    }
    visit("\0main", &root, &mut main_parsed, &main_key, &mut sm, &mut diags, &mut order, &mut visiting, &mut parsed, &mut std_needed, &mut names_used);
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
    for s in ["fmt", "os", "math", "time", "task", "strings", "net"] {
        if s == "fmt" || s == "os" || std_needed.iter().any(|n| n == s) {
            add_std(s, &mut units, &mut std_files);
        }
    }
    for (key, name, files) in user {
        units.push(check::Unit { name, path: key, is_std: false, files });
    }
    units.push(check::Unit { name: "main".into(), path: "\0main".into(), is_std: false, files: main_parsed });
    Ok(Loaded { sm, units })
}

fn cache_dir() -> PathBuf {
    let base = std::env::var("KITE_CACHE").map(PathBuf::from).unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        PathBuf::from(home).join(".cache").join("kite")
    });
    let _ = std::fs::create_dir_all(&base);
    base
}

fn hash_str(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn compiler(opts: &Options) -> (String, Vec<String>) {
    if let Some(t) = &opts.target {
        return ("zig".into(), vec!["cc".into(), "-target".into(), t.clone()]);
    }
    match &opts.cc {
        Some(cc) => {
            let mut parts = cc.split_whitespace().map(|s| s.to_string());
            let first = parts.next().unwrap_or_else(|| "cc".into());
            (first, parts.collect())
        }
        None => ("cc".into(), vec![]),
    }
}

/// The runtime header, in a directory of its own per runtime version, so that
/// compilers of different versions can share the cache.
fn runtime_include_dir() -> Result<PathBuf, String> {
    let dir = cache_dir().join(format!("include_{:016x}", hash_str(RUNTIME_H)));
    let h = dir.join("kite_rt.h");
    if !h.exists() {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let tmp = dir.join(format!("kite_rt.h.{}", std::process::id()));
        std::fs::write(&tmp, RUNTIME_H).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &h).map_err(|e| e.to_string())?;
    }
    Ok(dir)
}

/// Compile the runtime once per (compiler, flags) and cache the object file.
fn runtime_object(opts: &Options) -> Result<PathBuf, String> {
    let (cc, pre) = compiler(opts);
    let flags = if opts.release { "-O2" } else { "-O1" };
    let key = hash_str(&format!("{}{}{:?}{}{}", RUNTIME_C, RUNTIME_H, pre, cc, flags));
    let dir = cache_dir();
    let obj = dir.join(format!("kite_rt_{:016x}.o", key));
    if obj.exists() {
        return Ok(obj);
    }
    let c = dir.join(format!("kite_rt_{:016x}.c", key));
    let inc = runtime_include_dir()?;
    std::fs::write(&c, RUNTIME_C).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!("kite_rt_{:016x}.{}.o", key, std::process::id()));
    let out = Command::new(&cc).args(&pre).arg("-c").arg(flags).arg("-w").arg("-I").arg(&inc).arg(&c).arg("-o").arg(&tmp).output().map_err(|e| format!("cannot run C compiler `{}`: {}", cc, e))?;
    if !out.status.success() {
        return Err(format!("runtime compilation failed:\n{}", String::from_utf8_lossy(&out.stderr)));
    }
    std::fs::rename(&tmp, &obj).map_err(|e| e.to_string())?;
    Ok(obj)
}

fn drive(cmd: &str, input: &str, opts: &Options, prog_args: &[String]) -> Result<ExitCode, ()> {
    let t0 = Instant::now();
    let loaded = match load(input) {
        Ok(l) => l,
        Err((sm, diags)) => {
            report(&sm, &diags);
            return Err(());
        }
    };
    let sm = loaded.sm;
    let t_parse = t0.elapsed();
    let (prog, tfuncs, tests) = match check::check_program(loaded.units) {
        Ok(r) => r,
        Err(diags) => {
            report(&sm, &diags);
            return Err(());
        }
    };
    let t_check = t0.elapsed();
    let bdiags = borrowck::check(&prog, &tfuncs);
    if !bdiags.is_empty() {
        report(&sm, &bdiags);
        return Err(());
    }
    let t_borrow = t0.elapsed();
    if cmd == "check" {
        if opts.verbose {
            eprintln!("parse {:?}, check {:?}, borrowck {:?}", t_parse, t_check - t_parse, t_borrow - t_check);
        }
        eprintln!("ok");
        return Ok(ExitCode::SUCCESS);
    }
    let is_test = cmd == "test";
    if !is_test && !prog.packages[prog.main_pkg as usize].items.contains_key("main") {
        eprintln!("error: package has no `main` function");
        return Err(());
    }
    if is_test && tests.is_empty() {
        eprintln!("no tests found");
        return Ok(ExitCode::SUCCESS);
    }
    let c_src = codegen::generate(&prog, &tfuncs, &sm, if is_test { Some(&tests) } else { None }, opts.release);
    let t_gen = t0.elapsed();
    if cmd == "emit-c" {
        println!("{}", c_src);
        return Ok(ExitCode::SUCCESS);
    }
    let rt = match runtime_object(opts) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {}", e);
            return Err(());
        }
    };
    let dir = cache_dir();
    let stem = Path::new(input).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "out".into());
    // identical C + identical toolchain = identical binary
    let (cc0, pre0) = compiler(opts);
    let key = hash_str(&format!("{}{}{}{:?}{}{}", c_src, RUNTIME_C, cc0, pre0, opts.release, rt.display()));
    let cached = dir.join(format!("bin_{:016x}", key));
    if cached.exists() && !opts.keep_c {
        // refresh the timestamp so eviction keeps recently used binaries
        let _ = std::fs::File::options().append(true).open(&cached).and_then(|f| f.set_modified(std::time::SystemTime::now()));
        if opts.verbose {
            eprintln!("parse {:?}  check {:?}  borrowck {:?}  codegen {:?}  cc (cached)", t_parse, t_check - t_parse, t_borrow - t_check, t_gen - t_borrow);
        }
        return finish(cmd, &cached, opts, prog_args, &stem);
    }
    let c_path = dir.join(format!("{}_{}.c", stem, std::process::id()));
    std::fs::write(&c_path, &c_src).map_err(|e| eprintln!("error: {}", e))?;
    let out_path = dir.join(format!("bin_{:016x}.{}.tmp", key, std::process::id()));
    let (cc, pre) = compiler(opts);
    let mut c = Command::new(&cc);
    c.args(&pre);
    c.arg(if opts.release { "-O2" } else { "-O0" });
    let inc = runtime_include_dir().map_err(|e| eprintln!("error: {}", e))?;
    c.arg("-w").arg("-fwrapv").arg("-I").arg(&inc).arg(&c_path).arg(&rt).arg("-o").arg(&out_path).arg("-lm").arg("-lpthread");
    if cfg!(target_os = "linux") {
        c.arg("-ldl");
    }
    let out = match c.output() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: cannot run C compiler `{}`: {}", cc, e);
            return Err(());
        }
    };
    if !opts.keep_c {
        let _ = std::fs::remove_file(&c_path);
    }
    if !out.status.success() {
        eprintln!("internal error: generated C failed to compile (this is a compiler bug)\n{}", String::from_utf8_lossy(&out.stderr).lines().take(40).collect::<Vec<_>>().join("\n"));
        if opts.keep_c {
            eprintln!("C source kept at {}", c_path.display());
        }
        return Err(());
    }
    let t_cc = t0.elapsed();
    if opts.verbose {
        eprintln!("parse {:?}  check {:?}  borrowck {:?}  codegen {:?}  cc {:?}", t_parse, t_check - t_parse, t_borrow - t_check, t_gen - t_borrow, t_cc - t_gen);
    }
    let _ = std::fs::rename(&out_path, &cached);
    evict_old_binaries(&dir);
    finish(cmd, &cached, opts, prog_args, &stem)
}

/// Keep the binary cache bounded.
fn evict_old_binaries(dir: &Path) {
    let mut bins: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().starts_with("bin_"))
                .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
                .collect()
        })
        .unwrap_or_default();
    if bins.len() <= 64 {
        return;
    }
    bins.sort();
    for (_, p) in bins.iter().take(bins.len() - 64) {
        let _ = std::fs::remove_file(p);
    }
}

fn finish(cmd: &str, bin: &Path, opts: &Options, prog_args: &[String], stem: &str) -> Result<ExitCode, ()> {
    if cmd == "build" {
        let out = opts.output.clone().unwrap_or_else(|| stem.to_string());
        if let Err(e) = std::fs::copy(bin, &out) {
            eprintln!("error: cannot write {}: {}", out, e);
            return Err(());
        }
        return Ok(ExitCode::SUCCESS);
    }
    let out_path = bin.to_path_buf();
    let status = Command::new(&out_path).args(prog_args).status();
    match status {
        Ok(s) => match s.code() {
            Some(c) => Ok(ExitCode::from(c as u8)),
            None => {
                eprintln!("process terminated by signal");
                Ok(ExitCode::from(134))
            }
        },
        Err(e) => {
            eprintln!("error: cannot run {}: {}", out_path.display(), e);
            Err(())
        }
    }
}
