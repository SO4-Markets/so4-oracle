use std::process::Command;

/// Capture the identity of the running binary at compile time so /metrics can
/// carry a constant-1 `oracle_build_info` gauge. A dashboard can then tie a
/// regression to the exact commit and toolchain that produced the deploy.
fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");

    let git_sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|sha| sha.trim().to_string())
        .filter(|sha| !sha.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=GIT_SHA={git_sha}");

    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let rustc_version = Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|out| out.split_whitespace().nth(1).map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=RUSTC_VERSION={rustc_version}");
}
