// Records the commit and host target for `kite version`.
use std::process::Command;

fn main() {
    let commit = Command::new("git")
        .args(["rev-parse", "--short=10", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown commit".into());
    println!("cargo:rustc-env=KITE_COMMIT={}", commit);
    println!("cargo:rustc-env=KITE_HOST={}", std::env::var("TARGET").unwrap_or_default());
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads");
}
