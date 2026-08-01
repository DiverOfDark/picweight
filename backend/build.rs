//! Build script: stamps a human-readable version into the binary.
//!
//! Resolution order (first hit wins):
//!   1. `PICWEIGHT_VERSION` env var (set by CI / the Docker build)
//!   2. exact git tag
//!   3. git branch name
//!   4. `sha-<short sha>`
//!   5. `"unknown"`
//!
//! Ported from phos `backend/build.rs`.

use std::process::Command;

fn main() {
    // Re-run if git state or the env var changes.
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs/");
    println!("cargo:rerun-if-env-changed=PICWEIGHT_VERSION");
    println!("cargo:rerun-if-changed=migrations");

    let version = std::env::var("PICWEIGHT_VERSION")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(git_tag)
        .or_else(git_branch)
        .or_else(git_sha)
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=PICWEIGHT_VERSION={}", version);
}

fn git_tag() -> Option<String> {
    git(&["describe", "--tags", "--exact-match"]).map(|s| s.to_string())
}

fn git_branch() -> Option<String> {
    git(&["symbolic-ref", "--short", "HEAD"])
}

fn git_sha() -> Option<String> {
    git(&["rev-parse", "--short", "HEAD"]).map(|s| format!("sha-{}", s))
}

fn git(args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
