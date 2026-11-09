// Runs every program in tests/run and compares its stdout with the `.out` file,
// and checks that every program in tests/errors fails with the expected messages.
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn gojo() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_gojo"))
}

fn files(dir: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join(dir))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|e| e == "gojo").unwrap_or(false))
        .collect();
    v.sort();
    v
}

/// Runs a command with stdin closed, killing it after `limit`: sanitizers slow
/// programs down, and a hang should fail the test, not stall it.
fn output_within(cmd: &mut Command, limit: Duration) -> Result<Output, String> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    // read the pipes on other threads so a chatty child can't block on them
    let mut so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let t1 = std::thread::spawn(move || {
        let mut v = vec![];
        let _ = std::io::Read::read_to_end(&mut so, &mut v);
        v
    });
    let t2 = std::thread::spawn(move || {
        let mut v = vec![];
        let _ = std::io::Read::read_to_end(&mut se, &mut v);
        v
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(Output { status, stdout: t1.join().unwrap_or_default(), stderr: t2.join().unwrap_or_default() }),
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("still running after {} s; killed", limit.as_secs()));
            }
        }
    }
}

/// A command for a program built with ASan. On arm64 Linux, the fake stacks that
/// catch use after return leak every frame with gcc 13 (Ubuntu 24.04): each call
/// then scans the whole fake stack, and loops slow down without bound, so that
/// one check is left to the other platforms there.
fn asan_command(bin: &Path) -> Command {
    let mut c = Command::new(bin);
    if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        let opts = std::env::var("ASAN_OPTIONS").unwrap_or_default();
        c.env("ASAN_OPTIONS", if opts.is_empty() { "detect_stack_use_after_return=0".to_string() } else { format!("{}:detect_stack_use_after_return=0", opts) });
    }
    c
}

/// Every program in tests/run, and every single-file example, prints its `.out`:
/// as built, and with the bounds checks the compiler removed put back
/// (`--verify-bce`), where one that fails is a compiler bug.
#[test]
fn run_programs() {
    let mut failures = vec![];
    for f in files("tests/run").into_iter().chain(files("examples")) {
        let Ok(expected) = std::fs::read_to_string(f.with_extension("out")) else {
            failures.push(format!("{}: no .out file", f.display()));
            continue;
        };
        for flags in [&[][..], &["--verify-bce"][..]] {
            let out = Command::new(gojo()).arg("run").args(flags).arg(&f).output().unwrap();
            let stdout = String::from_utf8_lossy(&out.stdout);
            if stdout != expected {
                failures.push(format!("{} {:?}:\n--- expected\n{}\n--- got\n{}\n--- stderr\n{}", f.display(), flags, expected, stdout, String::from_utf8_lossy(&out.stderr)));
            }
        }
    }
    if !failures.is_empty() {
        panic!("{} program(s) failed:\n{}", failures.len(), failures.join("\n"));
    }
}

#[test]
fn compile_errors() {
    let mut failures = vec![];
    for f in files("tests/errors") {
        let src = std::fs::read_to_string(&f).unwrap();
        let wanted: Vec<String> = src.lines().filter_map(|l| l.trim().strip_prefix("// error: ").map(|s| s.to_string())).collect();
        let out = Command::new(gojo()).arg("check").arg(&f).output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        if out.status.success() {
            failures.push(format!("{}: expected a compile error", f.display()));
            continue;
        }
        if stderr.contains("internal error") || stderr.contains("panicked") {
            failures.push(format!("{}: compiler crashed:\n{}", f.display(), stderr));
            continue;
        }
        for w in &wanted {
            if !stderr.contains(w.as_str()) {
                failures.push(format!("{}: missing error `{}` in:\n{}", f.display(), w, stderr));
            }
        }
    }
    if !failures.is_empty() {
        panic!("{} error test(s) failed:\n{}", failures.len(), failures.join("\n"));
    }
}

/// Every program in tests/warnings builds, with a warning on each line that
/// ends in `// warning` and on no other line.
#[test]
fn compile_warnings() {
    let mut failures = vec![];
    for f in files("tests/warnings") {
        let src = std::fs::read_to_string(&f).unwrap();
        let wanted: Vec<usize> = src.lines().enumerate().filter(|(_, l)| l.trim_end().ends_with("// warning")).map(|(i, _)| i + 1).collect();
        let out = Command::new(gojo()).arg("check").arg(&f).output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() {
            failures.push(format!("{}: expected it to build:\n{}", f.display(), stderr));
            continue;
        }
        // the line of each warning: `  --> path:line:col` after `warning: ...`
        let mut got: Vec<usize> = vec![];
        let mut lines = stderr.lines();
        while let Some(l) = lines.next() {
            if l.starts_with("warning: ") {
                let at = lines.next().and_then(|x| x.trim().strip_prefix("--> ")).and_then(|x| x.rsplitn(3, ':').nth(1)).and_then(|n| n.parse::<usize>().ok());
                got.extend(at);
            }
        }
        if got != wanted {
            failures.push(format!("{}: warnings on lines {:?}, wanted {:?}:\n{}", f.display(), got, wanted, stderr));
        }
    }
    if !failures.is_empty() {
        panic!("{} warning test(s) failed:\n{}", failures.len(), failures.join("\n"));
    }
}

/// `gojo check --json` prints each diagnostic as a line of JSON, with the
/// edits that fix it where there is one obvious fix.
#[test]
fn check_json() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for f in files("tests/json") {
        let rel = f.strip_prefix(root).unwrap();
        let want = std::fs::read_to_string(f.with_extension("jsonl")).unwrap();
        let out = Command::new(gojo()).arg("check").arg("--json").arg(rel).current_dir(root).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{}", f.display());
        assert!(out.stderr.is_empty(), "{}: {}", f.display(), String::from_utf8_lossy(&out.stderr));
    }
}

/// An error that leaves `main` in a debug build prints where it was made and
/// each `try` that passed it up (spec 8); release builds print only the error.
#[test]
fn error_return_traces() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut failures = vec![];
    for f in files("tests/traces") {
        let rel = f.strip_prefix(root).unwrap();
        let src = std::fs::read_to_string(&f).unwrap();
        let want: Vec<&str> = src.lines().filter_map(|l| l.strip_prefix("// stderr: ")).collect();
        let out = Command::new(gojo()).arg("run").arg(rel).current_dir(root).output().unwrap();
        let got = String::from_utf8_lossy(&out.stderr);
        if out.status.code() != Some(1) || got.lines().collect::<Vec<_>>() != want {
            failures.push(format!("{}: exit {:?}\n--- expected\n{}\n--- got\n{}", f.display(), out.status.code(), want.join("\n"), got));
        }
        let rel_out = Command::new(gojo()).arg("run").arg("--release").arg(rel).current_dir(root).output().unwrap();
        let got = String::from_utf8_lossy(&rel_out.stderr);
        if got.lines().collect::<Vec<_>>() != want[..1] {
            failures.push(format!("{} --release: expected only `{}`, got\n{}", f.display(), want[0], got));
        }
    }
    if !failures.is_empty() {
        panic!("{} trace test(s) failed:\n{}", failures.len(), failures.join("\n"));
    }
}

/// Every program in tests/panic panics as its `// panic:` lines say, as built
/// and with `--verify-bce` (see `run_programs`).
#[test]
fn runtime_panics() {
    let mut failures = vec![];
    for (f, flags) in files("tests/panic").into_iter().flat_map(|f| [(f.clone(), &[][..]), (f, &["--verify-bce"][..])]) {
        let src = std::fs::read_to_string(&f).unwrap();
        let wanted: Vec<String> = src.lines().filter_map(|l| l.trim().strip_prefix("// panic: ").map(|s| s.to_string())).collect();
        let out = Command::new(gojo()).arg("run").args(flags).arg(&f).output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("bounds check the compiler removed") {
            failures.push(format!("{} {:?}: {}", f.display(), flags, stderr));
            continue;
        }
        if out.status.code() != Some(2) {
            failures.push(format!("{}: expected exit code 2, got {:?}\n{}", f.display(), out.status.code(), stderr));
            continue;
        }
        for w in &wanted {
            if !stderr.contains(w.as_str()) {
                failures.push(format!("{}: missing panic message `{}` in:\n{}", f.display(), w, stderr));
            }
        }
        // a panic ends the process at once: deferred code must not have run
        let stdout = String::from_utf8_lossy(&out.stdout);
        for w in src.lines().filter_map(|l| l.trim().strip_prefix("// panic-not: ")) {
            if stdout.contains(w) {
                failures.push(format!("{}: `{}` should not have been printed:\n{}", f.display(), w, stdout));
            }
        }
        if stderr.contains("<std>") {
            failures.push(format!("{}: panic location points into the standard library:\n{}", f.display(), stderr));
        }
    }
    if !failures.is_empty() {
        panic!("{} panic test(s) failed:\n{}", failures.len(), failures.join("\n"));
    }
}

/// Runs every program under AddressSanitizer: `cargo test -- --ignored`. The
/// panic tests too: each must panic before anything goes wrong in memory.
#[test]
#[ignore]
fn address_sanitizer() {
    let dir = std::env::temp_dir().join("gojo_asan");
    let _ = std::fs::create_dir_all(&dir);
    // (not stack overflows: ASan catches those itself, before the guard page)
    let panics = files("tests/panic").into_iter().filter(|f| !std::fs::read_to_string(f).unwrap().contains("panic: stack overflow"));
    let all: Vec<PathBuf> = files("tests/run").into_iter().chain(panics).collect();
    let failures: Vec<String> = par_each(&all, |f| {
        // (tests/panic and tests/run share names: keep their binaries apart)
        let bin = dir.join(format!("{}_{}", f.parent().unwrap().file_name().unwrap().to_string_lossy(), f.file_stem().unwrap().to_string_lossy()));
        let out = Command::new(gojo()).arg("build").arg("--cc").arg("cc -fsanitize=address,undefined -g").arg(f).arg("-o").arg(&bin).output().unwrap();
        if !out.status.success() {
            return Some(format!("{}: build failed\n{}", f.display(), String::from_utf8_lossy(&out.stderr)));
        }
        let run = match output_within(&mut asan_command(&bin), Duration::from_secs(300)) {
            Ok(run) => run,
            Err(e) => return Some(format!("{}: {}", f.display(), e)),
        };
        let stderr = String::from_utf8_lossy(&run.stderr);
        if stderr.contains("Sanitizer") || stderr.contains("runtime error") {
            return Some(format!("{}:\n{}", f.display(), stderr.lines().take(20).collect::<Vec<_>>().join("\n")));
        }
        None
    })
    .into_iter()
    .flatten()
    .collect();
    if !failures.is_empty() {
        panic!("{} program(s) failed under sanitizers:\n{}", failures.len(), failures.join("\n"));
    }
}

/// Runs `f` on each item, up to `GOJO_SAN_JOBS` (default: the CPUs, at most
/// 4) at a time: sanitizer runs build and run every program, one process per
/// program, so they spread over the machine. Each item's result, in order.
fn par_each<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let jobs = std::env::var("GOJO_SAN_JOBS").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).min(4)).max(1);
    let next = AtomicUsize::new(0);
    let out: std::sync::Mutex<Vec<Option<R>>> = std::sync::Mutex::new((0..items.len()).map(|_| None).collect());
    std::thread::scope(|s| {
        for _ in 0..jobs {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= items.len() {
                    break;
                }
                let r = f(&items[i]);
                out.lock().unwrap()[i] = Some(r);
            });
        }
    });
    out.into_inner().unwrap().into_iter().map(|r| r.unwrap()).collect()
}

/// Runs the concurrent programs under ThreadSanitizer: `cargo test -- --ignored`.
#[test]
#[ignore]
fn thread_sanitizer() {
    let dir = std::env::temp_dir().join("gojo_tsan");
    let _ = std::fs::create_dir_all(&dir);
    let all: Vec<PathBuf> = files("tests/run")
        .into_iter()
        .filter(|f| {
            let src = std::fs::read_to_string(f).unwrap();
            src.contains("go ") || src.contains("task.") || src.contains("Chan[")
        })
        .collect();
    let failures: Vec<String> = par_each(&all, |f| {
        let bin = dir.join(f.file_stem().unwrap());
        let out = Command::new(gojo()).arg("build").arg("--cc").arg("cc -fsanitize=thread -g -O1").arg(f).arg("-o").arg(&bin).output().unwrap();
        if !out.status.success() {
            return Some(format!("{}: build failed\n{}", f.display(), String::from_utf8_lossy(&out.stderr)));
        }
        let run = match output_within(&mut Command::new(&bin), Duration::from_secs(300)) {
            Ok(run) => run,
            Err(e) => return Some(format!("{}: {}", f.display(), e)),
        };
        let stderr = String::from_utf8_lossy(&run.stderr);
        if stderr.contains("ThreadSanitizer") {
            return Some(format!("{}:\n{}", f.display(), stderr.lines().take(30).collect::<Vec<_>>().join("\n")));
        }
        None
    })
    .into_iter()
    .flatten()
    .collect();
    if !failures.is_empty() {
        panic!("{} program(s) failed under ThreadSanitizer:\n{}", failures.len(), failures.join("\n"));
    }
}

/// Runs every program under clang's TypeSanitizer, in a debug and a release
/// build: `cargo test -- --ignored`. The program and the runtime are compiled
/// with strict aliasing (no `-fno-strict-aliasing`), so the code they are
/// generated as must not access memory through a type it doesn't have (spec
/// 11.1). Needs a clang with `-fsanitize=type` (LLVM 20 or later, not Apple's):
/// `GOJO_TYSAN_CC`, else `clang`; without one the test says so and passes.
/// Not on arm64 Linux: TypeSanitizer's shadow there (LLVM 21, 48-bit address
/// space) is indexed by the low 35 bits of an address, so memory 32 GB apart,
/// such as a goroutine stack and the heap, shares one shadow, and one's types
/// are reported against the other.
#[test]
#[ignore]
fn type_sanitizer() {
    if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        eprintln!("skipped: TypeSanitizer's arm64 Linux shadow mapping aliases addresses 32 GB apart");
        return;
    }
    let clang = std::env::var("GOJO_TYSAN_CC").unwrap_or_else(|_| "clang".into());
    let dir = std::env::temp_dir().join("gojo_tysan");
    let _ = std::fs::create_dir_all(&dir);
    let probe = dir.join("probe.c");
    std::fs::write(&probe, "int main(void) { return 0; }\n").unwrap();
    let ok = Command::new(&clang).arg("-fsanitize=type").arg(&probe).arg("-o").arg(dir.join("probe")).output().map(|o| o.status.success()).unwrap_or(false);
    if !ok {
        eprintln!("skipped: `{} -fsanitize=type` doesn't work; set GOJO_TYSAN_CC to an LLVM clang (20 or later)", clang);
        return;
    }
    let cc = format!("{} -fsanitize=type -g", clang);
    let all: Vec<(PathBuf, bool)> = files("tests/run").into_iter().flat_map(|f| [(f.clone(), false), (f, true)]).collect();
    let failures: Vec<String> = par_each(&all, |(f, release)| {
        let release = *release;
        let bin = dir.join(format!("{}{}", f.file_stem().unwrap().to_string_lossy(), if release { "_rel" } else { "" }));
        let mut c = Command::new(gojo());
        c.arg("build").arg("--cc").arg(&cc).arg(f).arg("-o").arg(&bin).env("GOJO_CACHE", dir.join("cache"));
        if release {
            c.arg("--release");
        }
        let out = c.output().unwrap();
        if !out.status.success() {
            return Some(format!("{}: build failed\n{}", f.display(), String::from_utf8_lossy(&out.stderr)));
        }
        let mut run = Command::new(&bin);
        run.current_dir(f.parent().unwrap());
        let run = match output_within(&mut run, Duration::from_secs(300)) {
            Ok(run) => run,
            Err(e) => return Some(format!("{}: {}", f.display(), e)),
        };
        let stderr = String::from_utf8_lossy(&run.stderr);
        if stderr.contains("TypeSanitizer") {
            return Some(format!("{}{}:\n{}", f.display(), if release { " (release)" } else { "" }, stderr.lines().take(12).collect::<Vec<_>>().join("\n")));
        }
        None
    })
    .into_iter()
    .flatten()
    .collect();
    if !failures.is_empty() {
        panic!("{} program(s) failed under TypeSanitizer:\n{}", failures.len(), failures.join("\n"));
    }
}

/// Multi-package projects: each directory in tests/projects and examples is built as a whole.
#[test]
fn projects() {
    let mut failures = vec![];
    let base = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut dirs: Vec<PathBuf> = vec![];
    for root in [base.join("tests/projects"), base.join("examples")] {
        dirs.extend(std::fs::read_dir(&root).unwrap().filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_dir()));
    }
    dirs.sort();
    for d in dirs {
        // a command-line tool: `runs.txt` lists argument lines; each runs in the
        // example's directory, and the transcript is compared
        if let Ok(runs) = std::fs::read_to_string(d.join("runs.txt")) {
            let bin = std::env::temp_dir().join(format!("gojo_tool_{}_{}", d.file_name().unwrap().to_string_lossy(), std::process::id()));
            let b = Command::new(gojo()).arg("build").arg(&d).arg("-o").arg(&bin).output().unwrap();
            if !b.status.success() {
                failures.push(format!("{}: build failed\n{}", d.display(), String::from_utf8_lossy(&b.stderr)));
                continue;
            }
            let mut transcript = String::new();
            for line in runs.lines().filter(|l| !l.trim().is_empty()) {
                let out = Command::new(&bin).args(line.split_whitespace()).current_dir(&d).output().unwrap();
                transcript.push_str(&format!("$ {}\n{}{}", line, String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)));
                if out.status.code() != Some(0) {
                    transcript.push_str(&format!("[exit {:?}]\n", out.status.code()));
                }
            }
            let _ = std::fs::remove_file(&bin);
            let expected = std::fs::read_to_string(d.join("expected.out")).unwrap_or_default();
            if transcript != expected {
                failures.push(format!("{}:\n--- expected\n{}\n--- got\n{}", d.display(), expected, transcript));
            }
            continue;
        }
        let out = Command::new(gojo()).arg("run").arg(&d).output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        if let Ok(expected) = std::fs::read_to_string(d.join("expected.out")) {
            if stdout != expected {
                failures.push(format!("{}:\n--- expected\n{}\n--- got\n{}\n--- stderr\n{}", d.display(), expected, stdout, stderr));
            }
        }
        if let Ok(expected) = std::fs::read_to_string(d.join("expected.err")) {
            for w in expected.lines().filter(|l| !l.is_empty()) {
                if out.status.success() || !stderr.contains(w) {
                    failures.push(format!("{}: missing error `{}` in:\n{}", d.display(), w, stderr));
                }
            }
        }
    }
    if !failures.is_empty() {
        panic!("{} project(s) failed:\n{}", failures.len(), failures.join("\n"));
    }
}

/// Parallel `gojo` runs on an empty cache share the runtime and the binary being
/// built: each must work, whichever finishes first.
#[test]
fn cold_cache_parallel() {
    let f = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/run/lastuse.gojo");
    let expected = std::fs::read_to_string(f.with_extension("out")).unwrap();
    for round in 0..3 {
        let cache = std::env::temp_dir().join(format!("gojo_cold_{}_{}", std::process::id(), round));
        let _ = std::fs::remove_dir_all(&cache);
        let runs: Vec<_> = (0..8).map(|_| Command::new(gojo()).arg("run").arg(&f).env("GOJO_CACHE", &cache).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap()).collect();
        for r in runs {
            let out = r.wait_with_output().unwrap();
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(out.status.success(), "{:?}: {}", out.status, stderr);
            assert_eq!(String::from_utf8_lossy(&out.stdout), expected, "{}", stderr);
        }
        let leftovers: Vec<_> = std::fs::read_dir(&cache).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| n.ends_with(".tmp") || (n.starts_with("gojo_rt_") && n.ends_with(".c"))).collect();
        assert!(leftovers.is_empty(), "{:?}", leftovers);
        let _ = std::fs::remove_dir_all(&cache);
    }
}

/// The C compiler is told the semantics Gojo's C is written for, both for the
/// program and for the runtime (see `C_SEMANTICS` in main.rs): a wrapper `cc`
/// records the arguments of every compilation.
#[test]
fn c_semantics_flags() {
    let dir = std::env::temp_dir().join(format!("gojo_ccflags_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("args.log");
    let spy = dir.join("cc-spy");
    std::fs::write(&spy, format!("#!/bin/sh\necho \"$@\" >> {}\nexec cc \"$@\"\n", log.display())).unwrap();
    std::fs::set_permissions(&spy, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let f = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/run/lastuse.gojo");
    for release in [false, true] {
        let mut c = Command::new(gojo());
        c.arg("build").arg("--cc").arg(&spy).arg(&f).arg("-o").arg(dir.join("prog")).env("GOJO_CACHE", dir.join("cache"));
        if release {
            c.arg("--release");
        }
        let out = c.output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
    let args = std::fs::read_to_string(&log).unwrap();
    let compiles: Vec<&str> = args.lines().filter(|l| l.contains(".c ") || l.ends_with(".c")).collect();
    // the runtime and the program, in a debug and a release build
    assert_eq!(compiles.len(), 4, "{}", args);
    for l in compiles {
        for flag in ["-fwrapv", "-fno-delete-null-pointer-checks"] {
            assert!(l.split_whitespace().any(|a| a == flag), "`{}` missing from: {}", flag, l);
        }
        // strict aliasing stays on: the generated code is written for it (spec 11.1)
        assert!(!l.split_whitespace().any(|a| a == "-fno-strict-aliasing"), "{}", l);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `gojo test`: each test runs in its own process, so a panic fails only that test.
#[test]
fn test_blocks() {
    let f = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/testblocks/blocks.gojo");
    let out = Command::new(gojo()).arg("test").arg(&f).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{}", stderr);
    assert!(stderr.contains("ok   adds"), "{}", stderr);
    assert!(stderr.contains("FAIL panics are isolated"), "{}", stderr);
    assert!(stderr.contains("ok   still runs after a failure"), "{}", stderr);
    assert!(stderr.contains("2 passed, 1 failed"), "{}", stderr);
    // `gojo run` ignores test blocks, including the one that panics
    let run = Command::new(gojo()).arg("run").arg(&f).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
}

/// The simulated scheduler: `--explore` finds an interleaving that breaks a
/// test and prints its seed; `--seed` replays it exactly; the same seed gives
/// the same run; time is virtual.
#[test]
fn simulated_scheduler() {
    let f = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/testblocks/sched.gojo");
    let out = Command::new(gojo()).arg("test").arg("--explore").arg("400").arg(&f).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(1), "{}", stderr);
    assert!(stderr.contains("ok   virtual time (400 schedules"), "{}", stderr);
    let seed = stderr.split("FAIL never overdrawn (seed ").nth(1).and_then(|r| r.split(';').next()).unwrap_or_else(|| panic!("no seed printed:\n{}", stderr)).to_string();
    for _ in 0..3 {
        let again = Command::new(gojo()).arg("test").arg("--seed").arg(&seed).arg(&f).output().unwrap();
        let e = String::from_utf8_lossy(&again.stderr);
        assert!(e.contains(&format!("FAIL never overdrawn (seed {};", seed)), "seed {} did not replay the failure:\n{}", seed, e);
    }
    // a program whose output depends on the interleaving
    let dir = std::env::temp_dir().join(format!("gojo_sim_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let prog = dir.join("order.gojo");
    std::fs::write(&prog, "fn main() {\n    let ch = Chan[int](16)\n    scope {\n        for i in 0..8 {\n            let tx = ch.tx.clone()\n            go () => {\n                tx <- i\n            }\n        }\n    }\n    drop(ch.tx)\n    var s = Vec[int]{}\n    for v in ch.rx {\n        s.push(v)\n    }\n    println(s)\n}\n").unwrap();
    let run = |seed: &str| String::from_utf8_lossy(&Command::new(gojo()).arg("run").arg("--seed").arg(seed).arg(&prog).output().unwrap().stdout).to_string();
    let a = run("11");
    assert_eq!(a, run("11"), "same seed, different runs");
    let distinct: std::collections::HashSet<String> = (1..12).map(|s| run(&s.to_string())).collect();
    assert!(distinct.len() > 1, "every seed gave {}", a);
    // an hour of virtual sleeping
    let t0 = std::time::Instant::now();
    let ok = Command::new(gojo()).arg("test").arg("--seed").arg("3").arg(&f).output().unwrap();
    assert!(String::from_utf8_lossy(&ok.stderr).contains("ok   virtual time"));
    assert!(t0.elapsed() < std::time::Duration::from_secs(30));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `gojo test` simulates the network: in memory, delays and short reads from
/// the seed; `--faults` adds resets, refused dials, lost datagrams and disk
/// errors, and a failing seed replays.
#[test]
fn simulated_network() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/simnet");
    let net = Command::new(gojo()).arg("test").arg("--explore").arg("100").arg(dir.join("net.gojo")).output().unwrap();
    let e = String::from_utf8_lossy(&net.stderr);
    assert!(net.status.success() && e.contains("7 passed, 0 failed") && e.contains("ok   http (100 schedules"), "{}", e);
    let faults = Command::new(gojo()).arg("test").arg("--faults").arg("--explore").arg("200").arg(dir.join("faults.gojo")).output().unwrap();
    let e = String::from_utf8_lossy(&faults.stderr);
    assert!(e.contains("ok   retries survive faults (200 schedules") && e.contains("ok   disk retries (200 schedules") && e.contains("2 passed, 1 failed"), "{}", e);
    let seed = e.split("FAIL naive client (seed ").nth(1).and_then(|r| r.split(';').next()).unwrap_or_else(|| panic!("no seed printed:\n{}", e)).to_string();
    assert!(e.contains(&format!("rerun it with `gojo test --seed {} --faults`", seed)), "{}", e);
    let again = Command::new(gojo()).arg("test").arg("--faults").arg("--seed").arg(&seed).arg(dir.join("faults.gojo")).output().unwrap();
    assert!(String::from_utf8_lossy(&again.stderr).contains(&format!("FAIL naive client (seed {};", seed)), "{}", String::from_utf8_lossy(&again.stderr));
    // without faults, the same seed passes
    let calm = Command::new(gojo()).arg("test").arg("--seed").arg(&seed).arg(dir.join("faults.gojo")).output().unwrap();
    assert!(calm.status.success(), "{}", String::from_utf8_lossy(&calm.stderr));
    // two tasks reading from each other: a deadlock, reported
    let tmp = std::env::temp_dir().join(format!("gojo_simnet_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let f = tmp.join("deadlock.gojo");
    std::fs::write(&f, "use std/{fmt, net}\n\ntest \"both read\" {\n    let ln = net.listen(\":0\") catch e { panic(e.message()) }\n    let c = net.dial(fmt.format(\":{}\", ln.port())) catch e { panic(e.message()) }\n    let s = ln.accept() catch e { panic(e.message()) }\n    scope {\n        go () => {\n            var b = Vec[byte]{}\n            _ = s.read(&b, 1)\n        }\n        var b = Vec[byte]{}\n        _ = c.read(&b, 1)\n    }\n}\n").unwrap();
    let dl = Command::new(gojo()).arg("test").arg(&f).output().unwrap();
    let e = String::from_utf8_lossy(&dl.stderr);
    assert!(!dl.status.success() && e.contains("deadlock") && e.matches("waiting for the simulated network").count() == 2, "{}", e);
    let run = Command::new(gojo()).arg("run").arg("--faults").arg(&f).output().unwrap();
    assert_eq!(run.status.code(), Some(2));
    let _ = std::fs::remove_dir_all(&tmp);
}

/// `gojo claims`: what the compiler proved, for every function of a program,
/// a library or a file of tests; a `checkfree` claim that fails is listed
/// and then reported (as JSON lines by `check --json`).
#[test]
fn claims() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(gojo()).arg("claims").arg("--json").arg(root.join("tests/run/contracts.gojo")).output().unwrap();
    let j = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success() && j.contains("{\"function\": \"dot\"") && j.contains("\"checkfree\": true") && j.contains("\"kept\": []"), "{}{}", j, String::from_utf8_lossy(&out.stderr));
    let dir = std::env::temp_dir().join(format!("gojo_claims_{}", std::process::id()));
    let lib = dir.join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    // a library: no `main`, and a function nothing calls
    std::fs::write(lib.join("lib.gojo"), "pub checkfree fn sum(v: []int) -> int {\n    var s = 0\n    for i in 0..v.len() {\n        s += v[i]\n    }\n    return s\n}\n\npub checkfree fn none() -> int {\n    return 1\n}\n").unwrap();
    let out = Command::new(gojo()).arg("claims").arg(&lib).output().unwrap();
    let o = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success() && o.contains("sum (checkfree)") && o.contains("1 bounds check removed, 0 kept") && o.contains("none (checkfree)"), "{}{}", o, String::from_utf8_lossy(&out.stderr));
    // tests only, with a claim that fails: the list, then the error
    let t = dir.join("t.gojo");
    std::fs::write(&t, "checkfree fn at(v: []int, i: int) -> int {\n    return v[i]\n}\n\ntest \"at\" {\n    assert(at([1], 0) == 1)\n}\n").unwrap();
    let out = Command::new(gojo()).arg("claims").arg(&t).output().unwrap();
    let (o, e) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.code() == Some(1) && o.contains("at (checkfree)") && o.contains("0 bounds checks removed, 1 kept") && e.contains("`at` is `checkfree`, but this bounds check remains"), "{}{}", o, e);
    let out = Command::new(gojo()).arg("check").arg("--json").arg(&t).output().unwrap();
    let e = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.code() == Some(1) && e.lines().any(|l| l.starts_with("{\"severity\": \"error\", \"message\": \"`at` is `checkfree`") && l.contains("\"line\": 2")), "{}", e);
    // `gojo check` covers a library's functions too
    let out = Command::new(gojo()).arg("check").arg(&t).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A box moved out of and a box of the same type made after it in the same
/// block share one allocation; anything that may leave the block between
/// them keeps them apart.
#[test]
fn box_reuse() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let c = Command::new(gojo()).arg("emit-c").arg(root.join("tests/run/box_reuse.gojo")).output().unwrap();
    let c = String::from_utf8_lossy(&c.stdout);
    assert_eq!(c.matches("/* reused below */").count(), 4, "{}", c);
    assert_eq!(c.matches("GOJO_BOX_FREE_R(").count(), 2, "{}", c);
    let dir = std::env::temp_dir().join(format!("gojo_reuse_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("apart.gojo");
    // a return and a loop exit between the free and the new box
    std::fs::write(&f, "fn id(s: sink String) -> String {\n    return s\n}\n\nfn a(b: sink Box[String], stop: bool) -> Box[String] {\n    let v = id(*b)\n    if stop {\n        return Box(\"stop\")\n    }\n    return Box(v + \"!\")\n}\n\nfn c(b: sink Box[String], xs: []String) -> ?Box[String] {\n    let v = id(*b)\n    for x in xs {\n        if x == v {\n            break\n        }\n    }\n    return Box(v)\n}\n\nfn d(b: sink Box[String]) -> Box[String] {\n    let v = id(*b)\n    return Box(v + \"?\")\n}\n\nfn main() {\n    println(*a(Box(\"a\"), false), *a(Box(\"b\"), true))\n    println(c(Box(\"x\"), [\"w\", \"x\"]) != none, *d(Box(\"d\")))\n}\n").unwrap();
    let out = Command::new(gojo()).arg("emit-c").arg(&f).output().unwrap();
    let c = String::from_utf8_lossy(&out.stdout);
    // (only `d` has nothing between them)
    assert!(out.status.success() && c.matches("reused below").count() == 1 && c.matches("GOJO_BOX_FREE_R(").count() == 2, "{}{}", c, String::from_utf8_lossy(&out.stderr));
    let run = Command::new(gojo()).arg("run").arg(&f).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout), "a! stop\ntrue d?\n");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Parked stacks are put away and brought back at the same address, also
/// while channels write values into them.
#[test]
fn stack_hibernation() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/hibernate/stress.gojo");
    let dir = std::env::temp_dir().join(format!("gojo_hib_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("stress");
    let b = Command::new(gojo()).arg("build").arg("--release").arg(&src).arg("-o").arg(&bin).output().unwrap();
    assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
    for _ in 0..3 {
        let out = Command::new(&bin).env("GOJO_HIBERNATE", "2").output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{}{}", stdout, String::from_utf8_lossy(&out.stderr));
        assert!(stdout.contains("hibernated before round 2: true"), "{}", stdout);
        assert!(stdout.contains("ok 6000 bad 0 locks 6000"), "{}", stdout);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// SIGINT cancels the program's root scope: tasks wind down, their values are
/// dropped, and the program exits with 130.
/// C keeps pointers into parked tasks' stacks and writes through them; those
/// stacks are never hibernated, while the others still are (tests/ffi_pin).
#[test]
fn ffi_pinned_stacks() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ffi_pin");
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("ffi_pin_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let lib = dir.join(if cfg!(target_os = "macos") { "libkeep.dylib" } else { "libkeep.so" });
    let cc = Command::new("cc").arg("-shared").arg("-fPIC").arg(base.join("keep.c")).arg("-o").arg(&lib).output().unwrap();
    assert!(cc.status.success(), "{}", String::from_utf8_lossy(&cc.stderr));
    let src = std::fs::read_to_string(base.join("pinned.gojo")).unwrap().replace("\"LIB\"", &format!("\"{}\"", lib.display()));
    let prog = dir.join("pinned.gojo");
    std::fs::write(&prog, src).unwrap();
    let out = Command::new(gojo()).arg("run").arg(&prog).env("GOJO_HIBERNATE", "2").output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}\n{}", stdout, String::from_utf8_lossy(&out.stderr));
    assert_eq!(stdout, "control group put away: true\npinned stacks left alone: true\ntasks that saw C's write: 600 of 600\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn shutdown_signal() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/signals/shutdown.gojo");
    let dir = std::env::temp_dir().join(format!("gojo_sig_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("shutdown");
    let b = Command::new(gojo()).arg("build").arg(&src).arg("-o").arg(&bin).output().unwrap();
    assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
    let child = Command::new(&bin).stdout(std::process::Stdio::piped()).spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let _ = Command::new("kill").arg("-INT").arg(child.id().to_string()).status();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(130), "{}", stdout);
    for id in 0..3 {
        assert!(stdout.contains(&format!("closing {}", id)), "{}", stdout);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Reads of a pipe with nothing in it park like network reads: a timeout
/// cancels them, and many readers don't take a thread each. stdin is a pipe
/// this test holds open without writing.
#[test]
fn stdin_pipe_cancel() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = root.join("tests/stdin/stdin_cancel.gojo");
    let dir = std::env::temp_dir().join(format!("gojo_stdin_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("stdin_cancel");
    let b = Command::new(gojo()).arg("build").arg(&src).arg("-o").arg(&bin).output().unwrap();
    assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
    let mut child = Command::new(&bin).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
    let stdin = child.stdin.take();
    let start = std::time::Instant::now();
    let out = child.wait_with_output().unwrap();
    drop(stdin);
    let expected = std::fs::read_to_string(root.join("tests/stdin/stdin_cancel.out")).unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), expected, "{}", String::from_utf8_lossy(&out.stderr));
    assert!(start.elapsed() < std::time::Duration::from_secs(10));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Programs nested in every way the grammar allows: moderately deep ones compile
/// and run (no exponential passes, no stack overflow in the compiler or in the C
/// compiler), and absurdly deep ones get an error instead of a crash.
#[test]
fn nesting_limits() {
    fn shape(name: &str, n: usize) -> String {
        let r = |s: &str| s.repeat(n);
        match name {
            "parens" => format!("fn main() {{\n  let x = {}1{}\n  println(x)\n}}\n", r("("), r(")")),
            "chain" => format!("fn main() {{\n  let x = 1{}\n  println(x)\n}}\n", r(" + 1")),
            "coalesce" => format!("fn main() {{\n  let a: ?int = none\n  let x = a{} ?? 1\n  println(x)\n}}\n", r(" ?? a")),
            "blocks" => format!("fn main() {{\n  var x = 0\n{}{}\n  println(x)\n}}\n", r("{\n x += 1\n"), r("}")),
            "elseif" => format!("fn main() {{\n  var x = 0\n{}{{ x += 2 }}\n  println(x)\n}}\n", r("if x == 1 { x += 1 } else ")),
            "types" => format!("fn main() {{\n  let v = Vec[{}int{}]{{}}\n  println(v.len())\n}}\n", r("Vec["), r("]")),
            "methods" => format!("fn main() {{\n  let s = \"a\".toString(){}\n  println(s)\n}}\n", r(".clone()")),
            "unary" => format!("fn main() {{\n  let x = {}1\n  let y = {}true\n  println(x, y)\n}}\n", r("-"), r("!")),
            "calls" => format!("fn f(x: int) -> int {{\n  return x\n}}\nfn main() {{\n  println({}1{})\n}}\n", r("f("), r(")")),
            "loops" => format!("fn main() {{\n  var x = 0\n{}{}\n  println(x)\n}}\n", (0..n).map(|k| format!("for i{} in 0..1 {{\n x += i{}\n", k, k)).collect::<String>(), r("}")),
            "closures" => format!("fn main() {{\n  let _f = {}1\n  println(1)\n}}\n", r("() => ")),
            "match" => format!("fn main() {{\n  let x = 1\n  let y = {}2{}\n  println(y)\n}}\n", r("match x { 1 => "), r(", _ => 0 }")),
            "lists" => format!("fn main() {{\n  let v = {}1{}\n  println(v.len())\n}}\n", r("["), r("]")),
            "arrays" => format!("fn main() {{\n  var v: {}int\n  println(v.len())\n}}\n", r("[1]")),
            "patterns" => format!("enum W[T] {{\n  V(x: T)\n}}\nfn main() {{\n  let w: {}int{} = {}7{}\n  match w {{\n    {}v{} => println(v)\n  }}\n}}\n", r("W["), r("]"), r(".V("), r(")"), r(".V("), r(")")),
            _ => unreachable!(),
        }
    }
    let dir = std::env::temp_dir().join(format!("gojo_nesting_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut failures = vec![];
    for name in ["parens", "chain", "coalesce", "blocks", "elseif", "types", "methods", "unary", "calls", "loops", "closures", "match", "lists", "arrays", "patterns"] {
        let f = dir.join(format!("{}.gojo", name));
        std::fs::write(&f, shape(name, 120)).unwrap();
        let start = std::time::Instant::now();
        let out = Command::new(gojo()).arg("run").arg(&f).output().unwrap();
        if !out.status.success() {
            failures.push(format!("{} at depth 120 failed:\n{}", name, String::from_utf8_lossy(&out.stderr).lines().take(10).collect::<Vec<_>>().join("\n")));
        } else if start.elapsed().as_secs() > 20 {
            failures.push(format!("{} at depth 120 took {:?}", name, start.elapsed()));
        }
        std::fs::write(&f, shape(name, 5000)).unwrap();
        let out = Command::new(gojo()).arg("check").arg(&f).output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        if out.status.code() != Some(1) || !stderr.contains("nested too deeply") {
            failures.push(format!("{} at depth 5000: expected a nesting error, got {:?}:\n{}", name, out.status, stderr.lines().take(10).collect::<Vec<_>>().join("\n")));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    if !failures.is_empty() {
        panic!("{} nesting case(s) failed:\n{}", failures.len(), failures.join("\n"));
    }
}

/// The calls in generated C that start at `pat`, with the location each one
/// reports (its last argument, a string literal `"file:line:col"`).
fn c_call_locs<'a>(c: &'a str, pat: &str) -> Vec<&'a str> {
    let mut out = vec![];
    for (pos, _) in c.match_indices(pat) {
        let start = pos + pat.len();
        let (mut depth, mut in_str, mut end) = (1, false, start);
        let bytes = c.as_bytes();
        let mut k = start;
        while k < bytes.len() {
            match bytes[k] {
                b'\\' if in_str => k += 1,
                b'"' => in_str = !in_str,
                b'(' if !in_str => depth += 1,
                b')' if !in_str => {
                    depth -= 1;
                    if depth == 0 {
                        end = k;
                        break;
                    }
                }
                _ => {}
            }
            k += 1;
        }
        let args = &c[start..end];
        if let Some(close) = args.rfind('"') {
            if let Some(open) = args[..close].rfind('"') {
                out.push(&args[open + 1..close]);
            }
        }
    }
    out
}

/// Where drops go (codegen/drops.rs), in the C for tests/run/drop_states.gojo:
/// flags only where a value may or may not have been moved, values that only
/// free memory freed at the end of a branch that didn't move them or after
/// their last use, and nothing freed early that a view, raw pointer, task or
/// `defer` may still reach. (`run_programs` checks the output, and the
/// sanitizer tests that nothing is freed too early.)
#[test]
fn drop_placement() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/run/drop_states.gojo");
    let out = Command::new(gojo()).arg("emit-c").arg(&path).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let c = String::from_utf8_lossy(&out.stdout);
    let body = |name: &str| -> String {
        let head = format!(" k_main_{}(", name);
        let start = c.match_indices(&head).map(|(i, _)| i).find(|&i| c[i..].split('\n').next().is_some_and(|l| l.ends_with('{'))).unwrap_or_else(|| panic!("no function {} in the C", name));
        let end = c[start..].find("\n}\n").map_or(c.len(), |e| start + e);
        c[start..end].lines().filter(|l| !l.starts_with("#line")).collect::<Vec<_>>().join("\n")
    };
    // (function, text, how many times it must appear)
    let expect: &[(&str, &str, usize)] = &[
        // a value with a `drop` method keeps its flag and its place
        ("observable", "if (df", 1),
        ("observable", "/* moved on another branch */", 0),
        // moved on every path, or known to be there: no flag
        ("plain", "bool df", 0),
        ("leaves", "bool df", 0),
        ("inLoop", "bool df", 0),
        ("reassigned", "bool df", 0),
        // freed at the end of the branch that didn't move it
        ("early", "/* moved on another branch */", 2),
        ("returned", "/* moved on another branch */", 1),
        ("reassigned", "/* moved on another branch */", 1),
        // a view may point into it: kept, with its flag
        ("viewKept", "/* moved on another branch */", 0),
        ("viewKept", "if (df", 1),
        // freed after the last use
        ("phases", "/* last use */", 1),
        ("sunk", "&p_s); /* last use */", 1),
        ("perIteration", "/* last use */", 2),
        // only `other`: a raw pointer may read `s` later
        ("pointedTo", "/* last use */", 1),
        ("pointedTo", "_other); /* last use */", 1),
        // a view, a task and a `defer` reach it later
        ("viewedLater", "/* last use */", 0),
        ("borrowedByTask", "/* last use */", 0),
        ("deferred", "/* last use */", 0),
        // a value with a `drop` method may reach it through a pointer
        ("pinnedByDrop", "/* last use */", 0),
    ];
    let mut failures = vec![];
    for (f, text, n) in expect {
        let b = body(f);
        let got = b.matches(text).count();
        if got != *n {
            failures.push(format!("{}: `{}` {} times, expected {}:\n{}", f, text, got, n, b));
        }
    }
    if !failures.is_empty() {
        panic!("drops in tests/run/drop_states.gojo:\n{}", failures.join("\n\n"));
    }
}

/// Bounds-check elimination: tests/run/bce.gojo marks every index and slice
/// `// bce: proven` (the compiler removes its check) or `// bce: checked`, in
/// order along the line, and the generated C must agree. (`run_programs` and
/// `runtime_panics` also run every program with the removed checks put back.)
#[test]
fn bounds_check_elimination() {
    use std::collections::BTreeMap;
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/run/bce.gojo");
    let src = std::fs::read_to_string(&path).unwrap();
    let out = Command::new(gojo()).arg("emit-c").arg(&path).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let c = String::from_utf8_lossy(&out.stdout);
    // (line, col, slice before index) -> what the C does there
    let mut sites: BTreeMap<(usize, usize, u8), Vec<&str>> = BTreeMap::new();
    let kinds = [("GOJO_IDX_PROVEN(", 1, "proven"), ("gojo_idx(", 1, "checked"), ("GOJO_SLICE_PROVEN(", 0, "proven"), ("gojo_panic(\"slice bounds out of range\", ", 0, "checked")];
    for (pat, order, kind) in kinds {
        for loc in c_call_locs(&c, pat) {
            let mut parts = loc.rsplitn(3, ':');
            let (Some(col), Some(line), Some(file)) = (parts.next(), parts.next(), parts.next()) else { continue };
            if !file.ends_with("bce.gojo") {
                continue;
            }
            let v = sites.entry((line.parse().unwrap(), col.parse().unwrap(), order)).or_default();
            if !v.contains(&kind) {
                v.push(kind);
            }
        }
    }
    let mut failures = vec![];
    let mut by_line: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for ((line, col, _), ks) in &sites {
        if ks.len() > 1 {
            failures.push(format!("line {}:{}: both proven and checked", line, col));
        }
        by_line.entry(*line).or_default().push(ks.join("+"));
    }
    for (i, l) in src.lines().enumerate() {
        let want: Vec<String> = l.split_once("// bce: ").map(|(_, w)| w.split_whitespace().map(String::from).collect()).unwrap_or_default();
        let got = by_line.remove(&(i + 1)).unwrap_or_default();
        if want != got {
            failures.push(format!("line {}: expected [{}], the C has [{}]\n    {}", i + 1, want.join(" "), got.join(" "), l.trim()));
        }
    }
    if !failures.is_empty() {
        panic!("bounds checks in tests/run/bce.gojo:\n{}", failures.join("\n"));
    }
}

/// Inputs found by `gojo-fuzz` that once crashed the compiler (reduced). Each
/// must now be rejected with diagnostics, or compile.
#[test]
fn fuzz_regressions() {
    let mut failures = vec![];
    let dir = std::env::temp_dir().join(format!("gojo_fuzzreg_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for f in files("tests/fuzz") {
        let out = Command::new(gojo()).arg("build").arg(&f).arg("-o").arg(dir.join("prog")).output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        let crashed = out.status.code().is_none_or(|c| c > 1) || ["panicked", "internal error", "overflowed"].iter().any(|w| stderr.contains(w));
        let reported = out.status.success() || stderr.contains("error");
        if crashed || !reported {
            failures.push(format!("{}: {:?}\n{}", f.display(), out.status, stderr.lines().take(15).collect::<Vec<_>>().join("\n")));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    if !failures.is_empty() {
        panic!("{} fuzz regression(s) failed:\n{}", failures.len(), failures.join("\n"));
    }
}

/// Programs written to get around the safety checks (dangling views, views
/// escaping locks and loops, moves hidden in `defer`s and closures, data races):
/// every one must be rejected. Each was checked to be really unsafe, or at
/// least unsound to accept, when it was written; several once compiled.
#[test]
fn unsafe_programs_rejected() {
    let mut failures = vec![];
    for f in files("tests/unsafe") {
        let out = Command::new(gojo()).arg("check").arg(&f).output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        if out.status.code() != Some(1) || stderr.contains("panicked") || stderr.contains("internal error") {
            failures.push(format!("{}: {:?}\n{}", f.display(), out.status, stderr.lines().take(8).collect::<Vec<_>>().join("\n")));
        } else if String::from_utf8_lossy(&Command::new(gojo()).arg("fmt").arg("--check").arg(&f).output().unwrap().stderr).contains("only files without syntax errors") {
            // (the formatter only takes files that parse)
            failures.push(format!("{}: rejected only for its syntax:\n{}", f.display(), stderr.lines().next().unwrap_or("")));
        }
    }
    if !failures.is_empty() {
        panic!("{} unsafe program(s) were not rejected:\n{}", failures.len(), failures.join("\n"));
    }
}

/// Every well-formed Gojo file in the repository is formatted, and formatting
/// is stable. (The fuzz regressions keep the shape they were found in.)
#[test]
fn formatted() {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "gojo") {
                out.push(p);
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut paths = vec![];
    for d in ["std", "examples", "bench/gojo", "tests/run", "tests/errors", "tests/panic", "tests/projects", "tests/packages", "tests/testblocks", "tests/unsafe", "tests/stdin", "tests/signals", "tests/ffi_pin", "tests/warnings", "tests/traces", "tests/json", "tests/simnet"] {
        walk(&root.join(d), &mut paths);
    }
    let mut bad = vec![];
    let mut formatted = 0;
    for p in &paths {
        let src = std::fs::read_to_string(p).unwrap();
        let Ok(out) = gojo::formatter::format(&src) else {
            // only the deliberately malformed error tests
            assert!(p.starts_with(root.join("tests/errors")), "{} does not parse", p.display());
            continue;
        };
        formatted += 1;
        if out != src {
            bad.push(p.display().to_string());
        }
        assert_eq!(gojo::formatter::format(&out).as_deref(), Ok(out.as_str()), "{}: formatting is not stable", p.display());
    }
    assert!(formatted > 100, "only {} files", formatted);
    assert!(bad.is_empty(), "not formatted (run `gojo fmt`):\n{}", bad.join("\n"));
}

/// docs/std is what `gojo doc --out` generates from the standard library.
#[test]
fn std_docs_current() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = std::env::temp_dir().join(format!("gojo_std_docs_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    let r = Command::new(gojo()).arg("doc").arg("--out").arg(&out).output().unwrap();
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let mut names: Vec<String> = std::fs::read_dir(&out).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    let mut have: Vec<String> = std::fs::read_dir(root.join("docs/std")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    have.sort();
    assert_eq!(names, have, "run `gojo doc --out docs/std`");
    for n in names {
        let want = std::fs::read_to_string(out.join(&n)).unwrap();
        let got = std::fs::read_to_string(root.join("docs/std").join(&n)).unwrap();
        assert!(want == got, "docs/std/{} is out of date: run `gojo doc --out docs/std`", n);
    }
    let _ = std::fs::remove_dir_all(&out);
}

/// Every program in docs/tour.md (a ```gojo block with `fn main`) runs and
/// prints the ```text block after it, and is formatted.
#[test]
fn tour_examples() {
    doc_programs("docs/tour.md", 15);
}

/// The one-page summary (`gojo spec`): its programs run like the tour's, its
/// other Gojo blocks type check, and it stays short enough to hand to a
/// coding agent whole.
#[test]
fn brief_examples() {
    doc_programs("docs/brief.md", 3);
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let brief = std::fs::read_to_string(root.join("docs/brief.md")).unwrap();
    let words = brief.split_whitespace().count();
    assert!(words <= 2600, "docs/brief.md has {} words; keep it under 2600 (about 4k tokens)", words);
    let out = Command::new(gojo()).arg("spec").output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), brief);
}

/// Every ```gojo block of a document with `fn main` runs and prints the
/// ```text block after it, and is formatted; the other ```gojo blocks type
/// check (with an empty `main` added).
fn doc_programs(doc: &str, min: usize) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let tour = std::fs::read_to_string(root.join(doc)).unwrap();
    // (language, body) of each fenced block
    let mut blocks: Vec<(String, String)> = vec![];
    let mut lines = tour.lines();
    while let Some(l) = lines.next() {
        if let Some(lang) = l.strip_prefix("```") {
            let mut body = String::new();
            for b in lines.by_ref() {
                if b.starts_with("```") {
                    break;
                }
                body.push_str(b);
                body.push('\n');
            }
            blocks.push((lang.to_string(), body));
        }
    }
    let dir = std::env::temp_dir().join(format!("gojo_doc_{}_{}", doc.replace('/', "_"), std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut failures = vec![];
    let mut programs = 0;
    for (i, (lang, body)) in blocks.iter().enumerate() {
        if lang != "gojo" {
            continue;
        }
        let first = body.lines().next().unwrap_or("");
        if !body.contains("fn main") {
            if doc.ends_with("brief.md") {
                let f = dir.join(format!("s{}.gojo", i));
                std::fs::write(&f, format!("{}\nfn main() {{\n}}\n", body)).unwrap();
                let out = Command::new(gojo()).arg("check").arg(&f).output().unwrap();
                if !out.status.success() {
                    failures.push(format!("block starting `{}` doesn't check:\n{}", first, String::from_utf8_lossy(&out.stderr)));
                }
            }
            continue;
        }
        programs += 1;
        if gojo::formatter::format(body).as_deref() != Ok(body.as_str()) {
            failures.push(format!("program starting `{}` is not formatted", first));
        }
        let f = dir.join(format!("p{}.gojo", i));
        std::fs::write(&f, body).unwrap();
        let out = Command::new(gojo()).arg("run").arg(&f).output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let want = match blocks.get(i + 1) {
            Some((l, b)) if l == "text" => b.clone(),
            _ => {
                failures.push(format!("program starting `{}` has no ```text output after it", first));
                continue;
            }
        };
        if !out.status.success() || stdout != want {
            failures.push(format!("program starting `{}`:\n--- want\n{}--- got\n{}--- stderr\n{}", first, want, stdout, String::from_utf8_lossy(&out.stderr)));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(programs >= min, "{}: only {} programs", doc, programs);
    assert!(failures.is_empty(), "{}: {} program(s) failed:\n{}", doc, failures.len(), failures.join("\n"));
}

/// Debug builds carry Gojo source lines: a debugger (when there is one)
/// resolves `file.gojo:line`, also inside a loop that is many C lines long.
/// `emit-c` shows the plain C.
#[test]
fn debug_info_lines() {
    let dir = std::env::temp_dir().join(format!("gojo_dbg_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("dbgline.gojo");
    std::fs::write(&src, "fn square(x: int) -> int {\n    let y = x * x\n    return y\n}\n\nfn main() {\n    var total = 0\n    for i in 0..3 {\n        total += square(i)\n    }\n    println(total)\n}\n").unwrap();
    let bin = dir.join("dbgline");
    let out = Command::new(gojo()).arg("build").arg(&src).arg("-o").arg(&bin).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    // the source file is named in the debug info
    // (macOS: in the dSYM beside the binary, under the name it was built with)
    let dwarf = if cfg!(target_os = "macos") {
        std::fs::read_dir(dir.join("dbgline.dSYM/Contents/Resources/DWARF")).unwrap().next().unwrap().unwrap().path()
    } else {
        bin.clone()
    };
    let bytes = std::fs::read(&dwarf).unwrap_or_else(|e| panic!("{}: {}", dwarf.display(), e));
    assert!(bytes.windows(12).any(|w| w == b"dbgline.gojo"), "no Gojo file in the debug info");
    let c = Command::new(gojo()).arg("emit-c").arg(&src).output().unwrap();
    assert!(!String::from_utf8_lossy(&c.stdout).contains("#line"), "emit-c should show plain C");
    // stopped in `square`, the caller's frame is line 9: the call inside the
    // loop (a C line count drifting inside the loop would show another line)
    let probe = if cfg!(target_os = "macos") {
        Command::new("lldb").arg("-b").arg("-o").arg("breakpoint set --file dbgline.gojo --line 2").arg("-o").arg("run").arg("-o").arg("bt 2").arg(&bin).output()
    } else {
        Command::new("gdb").arg("-batch").arg("-ex").arg("break dbgline.gojo:2").arg("-ex").arg("run").arg("-ex").arg("bt 2").arg(&bin).output()
    };
    if let Ok(p) = probe {
        let text = String::from_utf8_lossy(&p.stdout).to_string() + &String::from_utf8_lossy(&p.stderr);
        let caller = text.lines().find(|l| l.contains("frame #1") || l.starts_with("#1 "));
        assert!(caller.is_some_and(|l| l.contains("dbgline.gojo:9")), "the caller should be at dbgline.gojo:9:\n{}", text);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Packages with gojo.toml: each case in tests/packages runs `gojo run
/// <case>/app` and compares `expected.out`, or finds every line of
/// `expected.err` in the errors.
#[test]
fn packages() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/packages");
    let mut failures = vec![];
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&base).unwrap().filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_dir()).collect();
    cases.sort();
    for c in &cases {
        let out = Command::new(gojo()).arg("run").arg(c.join("app")).current_dir(&base).output().unwrap();
        let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        if let Ok(want) = std::fs::read_to_string(c.join("expected.out")) {
            if stdout != want || !out.status.success() {
                failures.push(format!("{}:\n--- want\n{}--- got\n{}--- stderr\n{}", c.display(), want, stdout, stderr));
            }
        } else if let Ok(want) = std::fs::read_to_string(c.join("expected.err")) {
            if out.status.success() {
                failures.push(format!("{}: expected errors, but it ran", c.display()));
            }
            for w in want.lines().filter(|l| !l.is_empty()) {
                if !stderr.contains(w) {
                    failures.push(format!("{}: missing `{}` in:\n{}", c.display(), w, stderr));
                }
            }
            if stderr.contains("panicked") {
                failures.push(format!("{}: the compiler crashed:\n{}", c.display(), stderr));
            }
        } else {
            failures.push(format!("{}: no expected.out or expected.err", c.display()));
        }
    }
    // a workspace: `check` and `test` cover every member, `run` asks for one
    let ws = base.join("workspace");
    let check = Command::new(gojo()).arg("check").arg(&ws).output().unwrap();
    let test = Command::new(gojo()).arg("test").arg(&ws).output().unwrap();
    let run = Command::new(gojo()).arg("run").arg(&ws).output().unwrap();
    let test_err = String::from_utf8_lossy(&test.stderr);
    if !check.status.success() || !test.status.success() || !test_err.contains("1 passed, 0 failed") || run.status.success() || !String::from_utf8_lossy(&run.stderr).contains("is a workspace") {
        failures.push(format!("workspace:\ncheck {:?} {}\ntest {:?} {}\nrun {:?} {}", check.status, String::from_utf8_lossy(&check.stderr), test.status, test_err, run.status, String::from_utf8_lossy(&run.stderr)));
    }
    // `gojo unsafe` reports by package, and fails when the policy does
    let denied = Command::new(gojo()).arg("unsafe").arg("unsafe_denied/app").current_dir(&base).output().unwrap();
    let allowed = Command::new(gojo()).arg("unsafe").arg("--json").arg("unsafe_allowed/app").current_dir(&base).output().unwrap();
    let (dout, aout) = (String::from_utf8_lossy(&denied.stdout), String::from_utf8_lossy(&allowed.stdout));
    if denied.status.code() != Some(1)
        || !dout.contains("@c/fastabs 1.2.0 (unsafe_denied/fastabs): 1 `unsafe` block, 1 `extern` block; 4 lines (NOT ALLOWED")
        || !dout.contains("unsafe_denied/fastabs/fastabs.gojo:7    unsafe block")
        || !dout.contains("mathx 0.4.0 (unsafe_denied/mathx): no unsafe code")
    {
        failures.push(format!("gojo unsafe (denied): {:?}\n{}{}", denied.status, dout, String::from_utf8_lossy(&denied.stderr)));
    }
    if !allowed.status.success() || !aout.contains("\"name\": \"@c/fastabs\", \"version\": \"1.2.0\", \"path\": \"unsafe_allowed/fastabs\", \"root\": false, \"status\": \"allowed\", \"lines\": 4") || !aout.ends_with("\"ok\": true}\n") {
        failures.push(format!("gojo unsafe --json (allowed): {:?}\n{}{}", allowed.status, aout, String::from_utf8_lossy(&allowed.stderr)));
    }
    // `gojo authority` reports what each dependency does, and fails when
    // [authority] doesn't allow it
    let adenied = Command::new(gojo()).arg("authority").arg("authority_denied/app").current_dir(&base).output().unwrap();
    let aallowed = Command::new(gojo()).arg("authority").arg("--json").arg("authority_allowed/app").current_dir(&base).output().unwrap();
    let (dout, aout) = (String::from_utf8_lossy(&adenied.stdout), String::from_utf8_lossy(&aallowed.stdout));
    if adenied.status.code() != Some(1)
        || !dout.contains("leftpad 0.1.0 (authority_denied/leftpad): fs, env (NOT ALLOWED by [authority])")
        || !dout.contains("authority_denied/leftpad/leftpad.gojo:4  env      os.getenv")
        || !dout.contains("s3 0.1.0 (authority_denied/s3): fs (NOT ALLOWED by [authority])\n    fs through its dependencies")
        || !dout.contains("[authority]\nleftpad = [\"fs\", \"env\"]\nutil = [\"fs\"]\ns3 = [\"fs\"]")
    {
        failures.push(format!("gojo authority (denied): {:?}\n{}{}", adenied.status, dout, String::from_utf8_lossy(&adenied.stderr)));
    }
    if !aallowed.status.success() || !aout.contains("{\"name\": \"util\", \"version\": \"0.1.0\", \"path\": \"authority_allowed/util\", \"needs\": [\"fs\"], \"status\": \"allowed\", \"uses\": [{\"kind\": \"fs\", \"what\": \"os.readText\", \"file\": \"authority_allowed/util/util.gojo\", \"line\": 4}]}") || !aout.ends_with("\"ok\": true}\n") {
        failures.push(format!("gojo authority --json (allowed): {:?}\n{}{}", aallowed.status, aout, String::from_utf8_lossy(&aallowed.stderr)));
    }
    // ...and in a workspace, member by member
    let wsu = Command::new(gojo()).arg("unsafe").arg("workspace").current_dir(&base).output().unwrap();
    let wsout = String::from_utf8_lossy(&wsu.stdout);
    if !wsu.status.success() || wsout.matches("\n== ").count() + usize::from(wsout.starts_with("== ")) != 3 || !wsout.contains("greet 0.1.0 (workspace/libs/greet): no unsafe code") {
        failures.push(format!("gojo unsafe (workspace): {:?}\n{}{}", wsu.status, wsout, String::from_utf8_lossy(&wsu.stderr)));
    }
    // `gojo init` writes a manifest that loads
    let dir = std::env::temp_dir().join(format!("gojo_init_{}", std::process::id())).join("my-tool");
    std::fs::create_dir_all(&dir).unwrap();
    let init = Command::new(gojo()).arg("init").current_dir(&dir).output().unwrap();
    std::fs::write(dir.join("main.gojo"), "fn main() {\n    println(\"hi\")\n}\n").unwrap();
    let r = Command::new(gojo()).arg("run").arg(&dir).output().unwrap();
    let again = Command::new(gojo()).arg("init").current_dir(&dir).output().unwrap();
    if !init.status.success() || String::from_utf8_lossy(&r.stdout) != "hi\n" || again.status.success() || !std::fs::read_to_string(dir.join("gojo.toml")).unwrap_or_default().contains("name = \"my-tool\"") {
        failures.push(format!("gojo init: {:?} / {:?} {} / second init {:?}", init.status, r.status, String::from_utf8_lossy(&r.stderr), again.status));
    }
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    assert!(cases.len() >= 9, "only {} cases", cases.len());
    assert!(failures.is_empty(), "{} package case(s) failed:\n{}", failures.len(), failures.join("\n"));
}
