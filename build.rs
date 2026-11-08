// Records the version, commit and host target for `gojo version`. The
// version is Cargo.toml's unless GOJO_VERSION overrides it (nightly builds).
use std::process::Command;

fn main() {
    let commit = Command::new("git")
        .args(["rev-parse", "--short=10", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown commit".into());
    let version = std::env::var("GOJO_VERSION")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| std::env::var("CARGO_PKG_VERSION").unwrap_or_default());
    println!("cargo:rustc-env=GOJO_VERSION={}", version);
    println!("cargo:rustc-env=GOJO_COMMIT={}", commit);
    println!("cargo:rustc-env=GOJO_HOST={}", std::env::var("TARGET").unwrap_or_default());
    println!("cargo:rerun-if-env-changed=GOJO_VERSION");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads");
}
