use std::{env, path::Path, process::Command};

/// Run `git` with the supplied arguments and return its trimmed stdout on
/// success. Returns `None` if git is unavailable or the command fails (for
/// example when building from a source tree that is not a git checkout).
fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Build the version string embedded into the binaries:
///
/// * the output of `git describe --dirty` (with a leading `v` stripped, e.g.
///   `0.1-117-gef85e2b-dirty`), and
/// * the date and time of that commit (ISO 8601, e.g. `2026-09-18T17:28:34+01:00`).
///
/// `--always` is added so that describe still produces a commit hash when no
/// tags are present; it does not change the output when tags exist.
///
/// Falls back to the package version when git metadata is unavailable.
fn version() -> String {
    let Some(describe) = git(&["describe", "--dirty", "--always"]).filter(|s| !s.is_empty()) else {
        return env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "unknown".to_string());
    };
    let describe = describe.strip_prefix('v').unwrap_or(&describe);

    let date = git(&["log", "-1", "--format=%cI"]).unwrap_or_else(|| "unknown".to_string());

    format!("{describe} ({date})")
}

fn main() {
    // Re-run whenever the working tree or the checked-out commit changes so
    // that the embedded version (including the `-dirty` suffix) stays current.
    for path in [
        "src",
        "Cargo.toml",
        "build.rs",
        ".git/HEAD",
        ".git/index",
        ".git/packed-refs",
        ".git/refs/heads",
        ".git/refs/tags",
    ] {
        if Path::new(path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }

    println!("cargo:rustc-env=DUTLIB_GIT_VERSION={}", version());
}
