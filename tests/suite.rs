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
