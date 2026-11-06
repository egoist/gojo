// Kite compiler driver.

use kite::diag::Diag;
use kite::{codegen, report, RUNTIME_C, RUNTIME_H};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Instant;

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
    let cmd = cmd.to_string();
    let r = kite::with_compiler_stack(move || drive(&cmd, &input, &opts, &prog_args));
    match r {
        Ok(code) => code,
        Err(()) => ExitCode::from(1),
    }
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
        return Ok((obj, is_clang));
    }
    // per process: another `kite` may be compiling the same runtime right now
    let c = dir.join(format!("kite_rt_{:016x}.{}.c", key, std::process::id()));
    let inc = runtime_include_dir()?;
    std::fs::write(&c, RUNTIME_C).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!("kite_rt_{:016x}.{}.o", key, std::process::id()));
    let out = cc_command(&cc, &pre).arg("-c").arg(flags).arg("-w").arg("-I").arg(&inc).arg(&c).arg("-o").arg(&tmp).output().map_err(|e| format!("cannot run C compiler `{}`: {}", cc, e))?;
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
        println!("{}", c_src);
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
    let mut flags: Vec<&str> = vec![if opts.release { "-O2" } else { "-O0" }, "-w", "-fwrapv"];
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
    std::fs::write(&c_path, &c_src).map_err(|e| eprintln!("error: {}", e))?;
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
    publish_binary(&out_path, &cached);
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
