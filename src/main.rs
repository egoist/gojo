// Kite compiler driver.

use kite::diag::Diag;
use kite::{codegen, report, RUNTIME_C, RUNTIME_H};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Instant;

#[derive(Clone)]
struct Options {
    release: bool,
    output: Option<String>,
    verbose: bool,
    cc: Option<String>,
    target: Option<String>,
    keep_c: bool,
}

const USAGE: &str = "Kite — a systems language with Go's simplicity, no GC and no nil.

usage:
  kite run   <file.kite|dir> [args...]   compile and run
  kite build <file.kite|dir> [-o out]     compile to a native binary
  kite check <file.kite|dir>              type and borrow check only
  kite test  <file.kite|dir>              run `test \"...\" {}` blocks
  kite emit-c <file.kite|dir>             print the generated C
  kite fmt [--check] [file.kite|dir...]  format in place (default: .); --check lists
                                          files that aren't formatted and fails
  kite doc [module|path] [--out dir]      API docs as Markdown: the standard library
                                          index, one std module, or a package;
                                          --out writes every std module to dir
  kite init [name]                        write a kite.toml here: a package named after
                                          the directory (or `name`)
  kite version                            print the version

options:
  --release        optimize (overflow checks stay on)
  --cc <compiler>  C compiler for the backend (default: $KITE_CC, else the
                   first of cc, clang, gcc and `zig cc` on the PATH)
  --target <t>     cross-compile with `zig cc -target <t>` (Linux or macOS,
                   e.g. x86_64-linux-musl, aarch64-macos)
  -v               print timings

Kite compiles to C: it needs a C compiler (cc, clang, gcc, or zig) on the PATH.";

fn usage() -> ExitCode {
    eprintln!("{}", USAGE);
    ExitCode::from(2)
}

fn version() -> String {
    format!("kite {} ({}, {})", env!("CARGO_PKG_VERSION"), env!("KITE_COMMIT"), env!("KITE_HOST"))
}

/// Why the C compiler could not be started, and what to do about it.
fn cc_missing(cc: &str, e: &std::io::Error) -> String {
    let mut m = format!("cannot run the C compiler `{}`: {}", cc, e);
    if e.kind() == std::io::ErrorKind::NotFound {
        m.push_str(if cc == "zig" {
            "\n  --target cross-compiles with zig: install it from https://ziglang.org/download/"
        } else if cfg!(target_os = "macos") {
            "\n  Kite compiles to C: install a C compiler with `xcode-select --install`,\n  or install zig (https://ziglang.org), which is used when no `cc` is found"
        } else {
            "\n  Kite compiles to C: install a C compiler (e.g. `apt install gcc` or `apk add gcc musl-dev`),\n  or install zig (https://ziglang.org), which is used when no `cc` is found"
        });
    }
    m
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("fmt") => return fmt_command(&args[2..]),
        Some("doc") => return doc_command(&args[2..]),
        Some("init") => return init_command(&args[2..]),
        Some("version" | "--version" | "-V") => {
            println!("{}", version());
            return ExitCode::SUCCESS;
        }
        Some("help" | "--help" | "-h") => {
            println!("{}", USAGE);
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
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
            _ if a.starts_with('-') && a.len() > 1 => {
                eprintln!("error: unknown option `{}` (see `kite help`)", a);
                return ExitCode::from(2);
            }
            _ if input.is_some() => {
                eprintln!("error: unexpected argument `{}`: `kite {}` takes one file or directory", a, cmd);
                return ExitCode::from(2);
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
        _ => {
            eprintln!("error: unknown command `{}`\n", cmd);
            return usage();
        }
    }
    if let Some(t) = &opts.target {
        if t.contains("windows") || t.contains("mingw") {
            eprintln!("error: Windows is not supported: Kite's runtime needs a POSIX system (Linux or macOS)");
            return ExitCode::from(2);
        }
        if !(t.contains("linux") || t.contains("macos") || t.contains("darwin")) {
            eprintln!("error: unsupported target `{}`: Kite targets Linux and macOS (e.g. x86_64-linux-musl, aarch64-linux-gnu, aarch64-macos)", t);
            return ExitCode::from(2);
        }
    }
    // a workspace without a package of its own: `check` and `test` run on
    // every member; anything else needs a member
    if let Some(members) = workspace_members(&input) {
        if !matches!(cmd, "check" | "test") {
            let names: Vec<String> = members.iter().map(|m| m.display().to_string()).collect();
            eprintln!("error: {} is a workspace: `kite {}` one of its members:\n  {}", input, cmd, names.join("\n  "));
            return ExitCode::from(2);
        }
        let mut failed = false;
        for m in members {
            eprintln!("== {}", m.display());
            let (cmd, m, opts) = (cmd.to_string(), m.display().to_string(), opts.clone());
            let r = kite::with_compiler_stack(move || drive(&cmd, &m, &opts, &[]));
            failed |= !matches!(r, Ok(c) if c == ExitCode::SUCCESS);
        }
        return if failed { ExitCode::from(1) } else { ExitCode::SUCCESS };
    }
    let cmd = cmd.to_string();
    let r = kite::with_compiler_stack(move || drive(&cmd, &input, &opts, &prog_args));
    match r {
        Ok(code) => code,
        Err(()) => ExitCode::from(1),
    }
}


/// The directory of `name` (`yaml`, `yaml/schema`) when it is a dependency of
/// the package in the current directory: `None` when it isn't one.
fn dependency_dir(name: &str) -> Option<Result<PathBuf, String>> {
    if name.starts_with('.') || name.starts_with('/') || name.starts_with("std/") {
        return None;
    }
    let root = kite::manifest::find_root(Path::new("."))?;
    let mut sm = kite::diag::SourceMap::default();
    let m = kite::manifest::read(&mut sm, &root).ok()?;
    let (dep, sub) = name.split_once('/').unwrap_or((name, ""));
    let d = m.dependency(dep)?;
    Some(match &d.source {
        kite::manifest::Source::Path(dir) => {
            let exported = kite::manifest::read(&mut sm, dir).ok().and_then(|dm| dm.package).is_some_and(|p| p.exports.iter().any(|e| e == sub));
            if exported { Ok(dir.join(sub)) } else { Err(format!("`{}` doesn't export `{}`", dep, if sub.is_empty() { "." } else { sub })) }
        }
        _ => Err(format!("`{}` isn't available locally yet (only path dependencies are)", dep)),
    })
}

/// `kite init`: a kite.toml for the current directory.
fn init_command(args: &[String]) -> ExitCode {
    let path = Path::new(kite::manifest::FILE);
    if path.exists() {
        eprintln!("error: there is already a kite.toml here");
        return ExitCode::from(1);
    }
    let name = match args.first() {
        Some(n) => n.clone(),
        None => {
            let dir = std::env::current_dir().ok().and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_default();
            let n: String = dir.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
            n.trim_matches('-').to_string()
        }
    };
    if !kite::manifest::valid_package_name(&name) {
        eprintln!("error: `{}` is not a package name: lower-case letters, digits, `-` and `_`, optionally scoped (`@scope/name`); give one: `kite init my-app`", name);
        return ExitCode::from(1);
    }
    let text = format!("[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = \"{}\"\n\n[dependencies]\n", name, kite::manifest::LATEST_EDITION);
    if let Err(e) = std::fs::write(path, text) {
        eprintln!("error: cannot write kite.toml: {}", e);
        return ExitCode::from(1);
    }
    println!("wrote kite.toml for package `{}`", name);
    ExitCode::SUCCESS
}

/// The members of the workspace at `dir`, when `dir`'s kite.toml is a
/// workspace without a [package].
fn workspace_members(dir: &str) -> Option<Vec<PathBuf>> {
    let d = Path::new(dir);
    if !d.join(kite::manifest::FILE).is_file() {
        return None;
    }
    let mut sm = kite::diag::SourceMap::default();
    let m = kite::manifest::read(&mut sm, d).ok()?;
    (m.package.is_none() && !m.members.is_empty()).then_some(m.members)
}

/// `kite doc`: API documentation, as Markdown.
fn doc_command(args: &[String]) -> ExitCode {
    let std_files = |name: &str| -> Vec<(String, String)> { kite::STD.iter().filter(|(n, _)| *n == name).map(|(n, s)| (format!("{}.kite", n), s.to_string())).collect() };
    let index = || {
        let mut s = String::from("# The standard library

The prelude is available everywhere; other modules are imported with `use std/name` (`use std/{os, json}` for several).

| module | |
| --- | --- |
");
        for (name, _) in kite::STD {
            s.push_str(&format!("| [`{}`]({}.md) | {} |
", name, name, kite::doc::summary(&std_files(name))));
        }
        s
    };
    let fail = |e: String| {
        eprintln!("error: {}", e);
        ExitCode::from(1)
    };
    if let Some(i) = args.iter().position(|a| a == "--out") {
        let Some(dir) = args.get(i + 1) else {
            return fail("--out needs a directory".into());
        };
        let dir = Path::new(dir);
        if let Err(e) = std::fs::create_dir_all(dir) {
            return fail(format!("cannot create {}: {}", dir.display(), e));
        }
        let mut pages = vec![("README".to_string(), index())];
        for (name, _) in kite::STD {
            match kite::doc::package(name, &std_files(name)) {
                Ok(d) => pages.push((name.to_string(), d)),
                Err(e) => return fail(e),
            }
        }
        for (name, text) in pages {
            let p = dir.join(format!("{}.md", name));
            if let Err(e) = std::fs::write(&p, text + "\n") {
                return fail(format!("cannot write {}: {}", p.display(), e));
            }
        }
        return ExitCode::SUCCESS;
    }
    let text = match args.first() {
        None => Ok(index()),
        // `std/os` (or just `os`, when it isn't a dependency)
        Some(name) if name.strip_prefix("std/").is_some_and(|m| kite::STD.iter().any(|(n, _)| *n == m)) => {
            let m = &name[4..];
            kite::doc::package(m, &std_files(m))
        }
        Some(name) if dependency_dir(name).is_none() && kite::STD.iter().any(|(n, _)| n == name) => kite::doc::package(name, &std_files(name)),
        Some(path) => {
            // a dependency of the package here (`yaml`, `yaml/schema`), or a path
            let dep = dependency_dir(path);
            if let Some(Err(e)) = &dep {
                return fail(e.clone());
            }
            let resolved = dep.and_then(|d| d.ok());
            let p = resolved.as_deref().unwrap_or(Path::new(path));
            let mut files: Vec<PathBuf> = if p.is_dir() {
                std::fs::read_dir(p).map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|f| f.extension().is_some_and(|x| x == "kite")).collect()).unwrap_or_default()
            } else if p.is_file() {
                vec![p.to_path_buf()]
            } else {
                return fail(format!("`{}` is neither a standard module nor a file or directory (modules: {})", path, kite::STD.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")));
            };
            files.sort();
            let mut srcs = vec![];
            for f in &files {
                match std::fs::read_to_string(f) {
                    Ok(s) => srcs.push((f.display().to_string(), s)),
                    Err(e) => return fail(format!("cannot read {}: {}", f.display(), e)),
                }
            }
            let canon = p.canonicalize().unwrap_or(p.to_path_buf());
            let name = if resolved.is_some() { path.to_string() } else { if p.is_dir() { canon.file_name() } else { canon.file_stem() }.map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() };
            kite::doc::package(&name, &srcs)
        }
    };
    match text {
        Ok(t) => {
            println!("{}", t);
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    }
}

/// `kite fmt`: formats files (directories: every .kite file below them).
fn fmt_command(args: &[String]) -> ExitCode {
    let check = args.iter().any(|a| a == "--check");
    let mut paths: Vec<PathBuf> = args.iter().filter(|a| !a.starts_with("--")).map(PathBuf::from).collect();
    if paths.is_empty() {
        paths.push(PathBuf::from("."));
    }
    fn collect(p: &Path, out: &mut Vec<PathBuf>) {
        if p.is_dir() {
            let mut es: Vec<PathBuf> = std::fs::read_dir(p).map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect()).unwrap_or_default();
            es.sort();
            for e in es {
                let hidden = e.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'));
                if !hidden && (e.is_dir() || e.extension().is_some_and(|x| x == "kite")) {
                    collect(&e, out);
                }
            }
        } else {
            out.push(p.to_path_buf());
        }
    }
    let mut files = vec![];
    for p in &paths {
        collect(p, &mut files);
    }
    let (mut bad, mut failed) = (0, false);
    for f in files {
        let src = match std::fs::read_to_string(&f) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: cannot read {}: {}", f.display(), e);
                failed = true;
                continue;
            }
        };
        match kite::formatter::format(&src) {
            Ok(out) if out == src => {}
            Ok(out) => {
                bad += 1;
                if check {
                    println!("{}", f.display());
                } else if let Err(e) = std::fs::write(&f, out) {
                    eprintln!("error: cannot write {}: {}", f.display(), e);
                    failed = true;
                }
            }
            Err(e) => {
                eprintln!("error: {}: {}", f.display(), e);
                failed = true;
            }
        }
    }
    if failed || (check && bad > 0) { ExitCode::from(1) } else { ExitCode::SUCCESS }
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
        // the first of cc, clang, gcc and zig on the PATH
        None => {
            let found = |name: &str| std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| is_executable(&d.join(name))));
            match ["cc", "clang", "gcc"].into_iter().find(|c| found(c)) {
                Some(c) => (c.into(), vec![]),
                None if found("zig") => ("zig".into(), vec!["cc".into()]),
                None => ("cc".into(), vec![]),
            }
        }
    }
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
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
    } else {
        touch(&h);
    }
    Ok(dir)
}

/// Marks a cache entry as used, so that eviction keeps it.
fn touch(p: &Path) {
    let _ = std::fs::File::open(p).and_then(|f| f.set_modified(std::time::SystemTime::now()));
}

fn targets_linux(opts: &Options) -> bool {
    match &opts.target {
        Some(t) => t.contains("linux"),
        None => cfg!(target_os = "linux"),
    }
}

/// A C compiler command with room to recurse: generated C nests about as deeply
/// as the Kite source (up to `parser::MAX_NEST` levels, a few C levels each), and
/// clang and gcc parse nested expressions recursively, so the 8 MB default stack
/// isn't enough at the limit. The higher limit only affects the compiler process.
fn cc_command(cc: &str, pre: &[String]) -> Command {
    use std::os::unix::process::CommandExt;
    let mut c = Command::new(cc);
    c.args(pre);
    // SAFETY: only async-signal-safe calls between fork and exec
    unsafe {
        c.pre_exec(|| {
            raise_stack_limit(64 << 20);
            Ok(())
        });
    }
    c
}

#[repr(C)]
struct RLimit {
    cur: u64,
    max: u64,
}

unsafe extern "C" {
    fn getrlimit(resource: i32, rlim: *mut RLimit) -> i32;
    fn setrlimit(resource: i32, rlim: *const RLimit) -> i32;
}

/// `RLIMIT_STACK` is 3 on Linux and on macOS.
fn raise_stack_limit(want: u64) {
    let mut r = RLimit { cur: 0, max: 0 };
    // SAFETY: plain system calls on a local struct
    unsafe {
        if getrlimit(3, &mut r) == 0 && r.cur < want {
            r.cur = want.min(r.max);
            setrlimit(3, &r);
        }
    }
}

/// The runtime object for this compiler and optimization level, compiled once
/// and cached, and whether the compiler is clang (which caps nesting with
/// `-fbracket-depth`, while gcc rejects the flag).
fn runtime_object(opts: &Options) -> Result<(PathBuf, bool), String> {
    let (cc, pre) = compiler(opts);
    let flags = if opts.release { "-O2" } else { "-O1" };
    let key = hash_str(&format!("{}{}{:?}{}{}", RUNTIME_C, RUNTIME_H, pre, cc, flags));
    let dir = cache_dir();
    let obj = dir.join(format!("kite_rt_{:016x}.o", key));
    let info = dir.join(format!("kite_rt_{:016x}.cc", key));
    let is_clang = match std::fs::read_to_string(&info) {
        Ok(s) => s == "clang",
        Err(_) => {
            let clang = cc_command(&cc, &pre).arg("--version").output().map(|o| String::from_utf8_lossy(&o.stdout).contains("clang")).unwrap_or(false);
            let tmp = dir.join(format!("kite_rt_{:016x}.{}.cc", key, std::process::id()));
            std::fs::write(&tmp, if clang { "clang" } else { "other" }).map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, &info).map_err(|e| e.to_string())?;
            clang
        }
    };
    if obj.exists() {
        touch(&obj);
        touch(&info);
        return Ok((obj, is_clang));
    }
    // per process: another `kite` may be compiling the same runtime right now
    let c = dir.join(format!("kite_rt_{:016x}.{}.c", key, std::process::id()));
    let inc = runtime_include_dir()?;
    std::fs::write(&c, RUNTIME_C).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!("kite_rt_{:016x}.{}.o", key, std::process::id()));
    let out = cc_command(&cc, &pre).arg("-c").arg(flags).arg("-w").arg("-I").arg(&inc).arg(&c).arg("-o").arg(&tmp).output().map_err(|e| cc_missing(&cc, &e))?;
    let _ = std::fs::remove_file(&c);
    if !out.status.success() {
        return Err(format!("runtime compilation failed:\n{}", String::from_utf8_lossy(&out.stderr)));
    }
    std::fs::rename(&tmp, &obj).map_err(|e| e.to_string())?;
    Ok((obj, is_clang))
}

fn drive(cmd: &str, input: &str, opts: &Options, prog_args: &[String]) -> Result<ExitCode, ()> {
    let t0 = Instant::now();
    let loaded = match kite::load(input, None) {
        Ok(l) => l,
        Err((sm, diags)) => {
            report(&sm, &diags);
            return Err(());
        }
    };
    let sm = loaded.sm;
    let t_parse = t0.elapsed();
    let (prog, tfuncs, tests) = match kite::check::check_program(loaded.units) {
        Ok(r) => r,
        Err(diags) => {
            report(&sm, &diags);
            return Err(());
        }
    };
    let t_check = t0.elapsed();
    let bdiags: Vec<Diag> = kite::borrowck::check(&prog, &tfuncs);
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
    let c_src = match codegen::generate(&prog, &tfuncs, &sm, if is_test { Some(&tests) } else { None }, opts.release) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {}", e);
            return Err(());
        }
    };
    let t_gen = t0.elapsed();
    if cmd == "emit-c" {
        print!("{}", codegen::strip_lines(&c_src));
        return Ok(ExitCode::SUCCESS);
    }
    let (rt, is_clang) = match runtime_object(opts) {
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
    // debug builds carry debug info, with Kite source lines (`#line`)
    let mut flags: Vec<&str> = if opts.release { vec!["-O2"] } else { vec!["-O0", "-g", "-gno-column-info"] };
    flags.extend(["-w", "-fwrapv"]);
    if is_clang {
        flags.push("-fbracket-depth=16384");
    }
    if targets_linux(opts) {
        // A stack frame bigger than the guard page below a goroutine's stack
        // (a large array local) could skip over it and write into whatever
        // lies below. With probes, every page of a big frame is touched in
        // order, so an overflow always hits the guard page and panics.
        // (Darwin compilers probe large frames by default.)
        flags.push("-fstack-clash-protection");
    }
    let key = hash_str(&format!("{}{}{}{:?}{:?}{}", c_src, RUNTIME_C, cc0, pre0, flags, rt.display()));
    let cached = dir.join(format!("bin_{:016x}", key));
    if cached.exists() && !opts.keep_c {
        // refresh the timestamp so eviction keeps recently used binaries
        // (read-only: on Linux, a binary open for writing can't be started by others)
        let _ = std::fs::File::open(&cached).and_then(|f| f.set_modified(std::time::SystemTime::now()));
        if opts.verbose {
            eprintln!("parse {:?}  check {:?}  borrowck {:?}  codegen {:?}  cc (cached)", t_parse, t_check - t_parse, t_borrow - t_check, t_gen - t_borrow);
        }
        return finish(cmd, &cached, opts, prog_args, &stem);
    }
    let c_path = dir.join(format!("{}_{}.c", stem, std::process::id()));
    // --keep-c is for reading and debugging the C itself: C lines, not Kite's
    let final_c = if opts.keep_c { codegen::strip_lines(&c_src) } else { codegen::resolve_lines(&c_src, &c_path.display().to_string()) };
    std::fs::write(&c_path, &final_c).map_err(|e| eprintln!("error: {}", e))?;
    let out_path = dir.join(format!("bin_{:016x}.{}.tmp", key, std::process::id()));
    let (cc, pre) = compiler(opts);
    let mut c = cc_command(&cc, &pre);
    c.args(&flags);
    let inc = runtime_include_dir().map_err(|e| eprintln!("error: {}", e))?;
    c.arg("-I").arg(&inc).arg(&c_path).arg(&rt).arg("-o").arg(&out_path).arg("-lm").arg("-lpthread");
    if cfg!(target_os = "linux") {
        c.arg("-ldl");
    }
    // libraries named by `extern "C" static "z"`
    for l in c_src.lines().filter_map(|l| l.strip_prefix("// kite-link: ")) {
        c.arg(l);
    }
    let out = match c.output() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {}", cc_missing(&cc, &e));
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
        } else {
            eprintln!("(the locations above are Kite lines; rerun with --keep-c to keep the C and see C lines)");
        }
        return Err(());
    }
    let t_cc = t0.elapsed();
    if opts.verbose {
        eprintln!("parse {:?}  check {:?}  borrowck {:?}  codegen {:?}  cc {:?}", t_parse, t_check - t_parse, t_borrow - t_check, t_gen - t_borrow, t_cc - t_gen);
    }
    publish_binary(&out_path, &cached);
    // Apple's clang puts debug info in `<output>.dSYM` next to the binary
    let tmp_dsym = dsym_of(&out_path);
    if tmp_dsym.exists() {
        if dsym_of(&cached).exists() || std::fs::rename(&tmp_dsym, dsym_of(&cached)).is_err() {
            let _ = std::fs::remove_dir_all(&tmp_dsym);
        }
    }
    evict_old_binaries(&dir);
    finish(cmd, &cached, opts, prog_args, &stem)
}

/// Moves a finished binary into the cache. Another `kite` may have built the same
/// one, and may be starting it: the first one stays and is never replaced. (The
/// binaries aren't byte-identical, since macOS code signatures name the output
/// file, and swapping one out under a starting process gets that process killed.)
fn publish_binary(tmp: &Path, cached: &Path) {
    if std::fs::hard_link(tmp, cached).is_ok() || cached.exists() {
        let _ = std::fs::remove_file(tmp);
    } else {
        // a file system without hard links
        let _ = std::fs::rename(tmp, cached);
    }
}

/// Keep the binary cache bounded.
/// Where the debug info of a macOS binary goes.
fn dsym_of(bin: &Path) -> PathBuf {
    let mut s = bin.as_os_str().to_owned();
    s.push(".dSYM");
    PathBuf::from(s)
}

/// Keeps the 64 most recently used binaries (with their debug info), and
/// removes what interrupted builds left behind.
fn evict_old_binaries(dir: &Path) {
    let now = std::time::SystemTime::now();
    let old = |m: &std::fs::Metadata| m.modified().ok().and_then(|t| now.duration_since(t).ok()).is_some_and(|a| a.as_secs() > 3600);
    let mut bins: Vec<(std::time::SystemTime, PathBuf)> = vec![];
    for e in std::fs::read_dir(dir).into_iter().flatten().filter_map(|e| e.ok()) {
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(m) = e.metadata() else { continue };
        // runtimes of other compiler versions, C compilers or flags: kept
        // while used in the last 30 days
        let unused = |p: &Path| std::fs::metadata(p).ok().and_then(|m| m.modified().ok()).and_then(|t| now.duration_since(t).ok()).is_some_and(|a| a.as_secs() > 30 * 86400);
        if name.starts_with("kite_rt_") && unused(&e.path()) {
            let _ = std::fs::remove_file(e.path());
            continue;
        }
        if name.starts_with("include_") && unused(&e.path().join("kite_rt.h")) {
            let _ = std::fs::remove_dir_all(e.path());
            continue;
        }
        if !name.starts_with("bin_") {
            // C files of interrupted builds (and old `--keep-c` ones)
            if name.ends_with(".c") && name.contains('_') && m.modified().ok().and_then(|t| now.duration_since(t).ok()).is_some_and(|a| a.as_secs() > 86400) {
                let _ = std::fs::remove_file(e.path());
            }
            continue;
        }
        if name.ends_with(".dSYM") {
            // debug info whose binary is gone (or an interrupted build's)
            let bin = dir.join(name.trim_end_matches(".dSYM"));
            if (name.contains(".tmp") || !bin.exists()) && old(&m) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        } else if name.ends_with(".tmp") {
            if old(&m) {
                let _ = std::fs::remove_file(e.path());
            }
        } else if let Ok(t) = m.modified() {
            bins.push((t, e.path()));
        }
    }
    if bins.len() <= 64 {
        return;
    }
    bins.sort();
    for (_, p) in bins.iter().take(bins.len() - 64) {
        let _ = std::fs::remove_file(p);
        let _ = std::fs::remove_dir_all(dsym_of(p));
    }
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let t = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &t)?;
        } else {
            std::fs::copy(e.path(), t)?;
        }
    }
    Ok(())
}

fn finish(cmd: &str, bin: &Path, opts: &Options, prog_args: &[String], stem: &str) -> Result<ExitCode, ()> {
    if cmd == "build" {
        let out = opts.output.clone().unwrap_or_else(|| stem.to_string());
        if let Err(e) = std::fs::copy(bin, &out) {
            eprintln!("error: cannot write {}: {}", out, e);
            return Err(());
        }
        // and its debug info (macOS keeps it beside the binary)
        let out_dsym = dsym_of(Path::new(&out));
        let _ = std::fs::remove_dir_all(&out_dsym);
        if dsym_of(bin).exists() {
            if let Err(e) = copy_dir(&dsym_of(bin), &out_dsym) {
                eprintln!("warning: cannot write {}: {}", out_dsym.display(), e);
            }
        }
        return Ok(ExitCode::SUCCESS);
    }
    let out_path = bin.to_path_buf();
    let status = Command::new(&out_path).args(prog_args).status();
    match status {
        Ok(s) => match s.code() {
            Some(c) => Ok(ExitCode::from(c as u8)),
            None => {
                use std::os::unix::process::ExitStatusExt;
                let sig = s.signal().unwrap_or(0);
                eprintln!("process terminated by signal {}", sig);
                Ok(ExitCode::from((128 + sig) as u8))
            }
        },
        Err(e) => {
            eprintln!("error: cannot run {}: {}", out_path.display(), e);
            Err(())
        }
    }
}
