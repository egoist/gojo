// Runs every program in tests/run and compares its stdout with the `.out` file,
// and checks that every program in tests/errors fails with the expected messages.
use std::path::{Path, PathBuf};
use std::process::Command;

fn kite() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_kite"))
}

fn files(dir: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join(dir))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|e| e == "kite").unwrap_or(false))
        .collect();
    v.sort();
    v
}

#[test]
fn run_programs() {
    let mut failures = vec![];
    for f in files("tests/run") {
        let expected = std::fs::read_to_string(f.with_extension("out")).unwrap_or_default();
        let out = Command::new(kite()).arg("run").arg(&f).output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        if stdout != expected {
            failures.push(format!("{}:\n--- expected\n{}\n--- got\n{}\n--- stderr\n{}", f.display(), expected, stdout, String::from_utf8_lossy(&out.stderr)));
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
        let out = Command::new(kite()).arg("check").arg(&f).output().unwrap();
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

#[test]
fn runtime_panics() {
    let mut failures = vec![];
    for f in files("tests/panic") {
        let src = std::fs::read_to_string(&f).unwrap();
        let wanted: Vec<String> = src.lines().filter_map(|l| l.trim().strip_prefix("// panic: ").map(|s| s.to_string())).collect();
        let out = Command::new(kite()).arg("run").arg(&f).output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
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

/// Runs every program under AddressSanitizer: `cargo test -- --ignored`.
#[test]
#[ignore]
fn address_sanitizer() {
    let mut failures = vec![];
    let dir = std::env::temp_dir().join("kite_asan");
    let _ = std::fs::create_dir_all(&dir);
    for f in files("tests/run") {
        let bin = dir.join(f.file_stem().unwrap());
        let out = Command::new(kite()).arg("build").arg("--cc").arg("cc -fsanitize=address,undefined -g").arg(&f).arg("-o").arg(&bin).output().unwrap();
        if !out.status.success() {
            failures.push(format!("{}: build failed\n{}", f.display(), String::from_utf8_lossy(&out.stderr)));
            continue;
        }
        let run = Command::new(&bin).output().unwrap();
        let stderr = String::from_utf8_lossy(&run.stderr);
        if stderr.contains("Sanitizer") || stderr.contains("runtime error") {
            failures.push(format!("{}:\n{}", f.display(), stderr.lines().take(20).collect::<Vec<_>>().join("\n")));
        }
    }
    if !failures.is_empty() {
        panic!("{} program(s) failed under sanitizers:\n{}", failures.len(), failures.join("\n"));
    }
}

/// Runs the concurrent programs under ThreadSanitizer: `cargo test -- --ignored`.
#[test]
#[ignore]
fn thread_sanitizer() {
    let mut failures = vec![];
    let dir = std::env::temp_dir().join("kite_tsan");
    let _ = std::fs::create_dir_all(&dir);
    for f in files("tests/run") {
        let src = std::fs::read_to_string(&f).unwrap();
        if !(src.contains("go ") || src.contains("task.") || src.contains("chan[")) {
            continue;
        }
        let bin = dir.join(f.file_stem().unwrap());
        let out = Command::new(kite()).arg("build").arg("--cc").arg("cc -fsanitize=thread -g -O1").arg(&f).arg("-o").arg(&bin).output().unwrap();
        if !out.status.success() {
            failures.push(format!("{}: build failed\n{}", f.display(), String::from_utf8_lossy(&out.stderr)));
            continue;
        }
        let run = Command::new(&bin).output().unwrap();
        let stderr = String::from_utf8_lossy(&run.stderr);
        if stderr.contains("ThreadSanitizer") {
            failures.push(format!("{}:\n{}", f.display(), stderr.lines().take(30).collect::<Vec<_>>().join("\n")));
        }
    }
    if !failures.is_empty() {
        panic!("{} program(s) failed under ThreadSanitizer:\n{}", failures.len(), failures.join("\n"));
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
        let out = Command::new(kite()).arg("run").arg(&d).output().unwrap();
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

/// Parallel `kite` runs on an empty cache share the runtime and the binary being
/// built: each must work, whichever finishes first.
#[test]
fn cold_cache_parallel() {
    let f = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/run/lastuse.kite");
    let expected = std::fs::read_to_string(f.with_extension("out")).unwrap();
    for round in 0..3 {
        let cache = std::env::temp_dir().join(format!("kite_cold_{}_{}", std::process::id(), round));
        let _ = std::fs::remove_dir_all(&cache);
        let runs: Vec<_> = (0..8).map(|_| Command::new(kite()).arg("run").arg(&f).env("KITE_CACHE", &cache).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap()).collect();
        for r in runs {
            let out = r.wait_with_output().unwrap();
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(out.status.success(), "{:?}: {}", out.status, stderr);
            assert_eq!(String::from_utf8_lossy(&out.stdout), expected, "{}", stderr);
        }
        let leftovers: Vec<_> = std::fs::read_dir(&cache).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| n.ends_with(".tmp") || (n.starts_with("kite_rt_") && n.ends_with(".c"))).collect();
        assert!(leftovers.is_empty(), "{:?}", leftovers);
        let _ = std::fs::remove_dir_all(&cache);
    }
}

/// `kite test`: each test runs in its own process, so a panic fails only that test.
#[test]
fn test_blocks() {
    let f = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/testblocks/blocks.kite");
    let out = Command::new(kite()).arg("test").arg(&f).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{}", stderr);
    assert!(stderr.contains("ok   adds"), "{}", stderr);
    assert!(stderr.contains("FAIL panics are isolated"), "{}", stderr);
    assert!(stderr.contains("ok   still runs after a failure"), "{}", stderr);
    assert!(stderr.contains("2 passed, 1 failed"), "{}", stderr);
    // `kite run` ignores test blocks, including the one that panics
    let run = Command::new(kite()).arg("run").arg(&f).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
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
            "coalesce" => format!("fn main() {{\n  let a ?int = none\n  let x = a{} ?? 1\n  println(x)\n}}\n", r(" ?? a")),
            "blocks" => format!("fn main() {{\n  var x = 0\n{}{}\n  println(x)\n}}\n", r("{\n x += 1\n"), r("}")),
            "elseif" => format!("fn main() {{\n  var x = 0\n{}{{ x += 2 }}\n  println(x)\n}}\n", r("if x == 1 { x += 1 } else ")),
            "types" => format!("fn main() {{\n  let v = Vec[{}int{}]{{}}\n  println(v.len())\n}}\n", r("Vec["), r("]")),
            "methods" => format!("fn main() {{\n  let s = \"a\".toString(){}\n  println(s)\n}}\n", r(".clone()")),
            "unary" => format!("fn main() {{\n  let x = {}1\n  let y = {}true\n  println(x, y)\n}}\n", r("-"), r("!")),
            "calls" => format!("fn f(x int) int {{\n  return x\n}}\nfn main() {{\n  println({}1{})\n}}\n", r("f("), r(")")),
            "loops" => format!("fn main() {{\n  var x = 0\n{}{}\n  println(x)\n}}\n", (0..n).map(|k| format!("for i{} in 0..1 {{\n x += i{}\n", k, k)).collect::<String>(), r("}")),
            "closures" => format!("fn main() {{\n  let _f = {}1\n  println(1)\n}}\n", r("() => ")),
            "match" => format!("fn main() {{\n  let x = 1\n  let y = {}2{}\n  println(y)\n}}\n", r("match x { 1 => "), r(", _ => 0 }")),
            "lists" => format!("fn main() {{\n  let v = {}1{}\n  println(v.len())\n}}\n", r("["), r("]")),
            "arrays" => format!("fn main() {{\n  var v {}int\n  println(v.len())\n}}\n", r("[1]")),
            "patterns" => format!("enum W[T] {{\n  V(x T)\n}}\nfn main() {{\n  let w {}int{} = {}7{}\n  match w {{\n    {}v{} => println(v)\n  }}\n}}\n", r("W["), r("]"), r(".V("), r(")"), r(".V("), r(")")),
            _ => unreachable!(),
        }
    }
    let dir = std::env::temp_dir().join(format!("kite_nesting_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut failures = vec![];
    for name in ["parens", "chain", "coalesce", "blocks", "elseif", "types", "methods", "unary", "calls", "loops", "closures", "match", "lists", "arrays", "patterns"] {
        let f = dir.join(format!("{}.kite", name));
        std::fs::write(&f, shape(name, 120)).unwrap();
        let start = std::time::Instant::now();
        let out = Command::new(kite()).arg("run").arg(&f).output().unwrap();
        if !out.status.success() {
            failures.push(format!("{} at depth 120 failed:\n{}", name, String::from_utf8_lossy(&out.stderr).lines().take(10).collect::<Vec<_>>().join("\n")));
        } else if start.elapsed().as_secs() > 20 {
            failures.push(format!("{} at depth 120 took {:?}", name, start.elapsed()));
        }
        std::fs::write(&f, shape(name, 5000)).unwrap();
        let out = Command::new(kite()).arg("check").arg(&f).output().unwrap();
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

/// Inputs found by `kite-fuzz` that once crashed the compiler (reduced). Each
/// must now be rejected with diagnostics, or compile.
#[test]
fn fuzz_regressions() {
    let mut failures = vec![];
    let dir = std::env::temp_dir().join(format!("kite_fuzzreg_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for f in files("tests/fuzz") {
        let out = Command::new(kite()).arg("build").arg(&f).arg("-o").arg(dir.join("prog")).output().unwrap();
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
        let out = Command::new(kite()).arg("check").arg(&f).output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        if out.status.code() != Some(1) || stderr.contains("panicked") || stderr.contains("internal error") {
            failures.push(format!("{}: {:?}\n{}", f.display(), out.status, stderr.lines().take(8).collect::<Vec<_>>().join("\n")));
        } else if stderr.lines().next().is_some_and(|l| l.starts_with("error: expected")) {
            failures.push(format!("{}: rejected only for its syntax:\n{}", f.display(), stderr.lines().next().unwrap_or("")));
        }
    }
    if !failures.is_empty() {
        panic!("{} unsafe program(s) were not rejected:\n{}", failures.len(), failures.join("\n"));
    }
}
