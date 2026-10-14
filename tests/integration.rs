//! Integration tests for diwa CLI.

use std::path::PathBuf;
use std::process::Command;

fn diwa_bin() -> PathBuf {
    // Use the debug binary built by cargo test.
    let mut path = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    path.push("diwa");
    path
}

#[test]
fn test_help() {
    let output = Command::new(diwa_bin()).arg("--help").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("diwa"));
    assert!(stdout.contains("index"));
    assert!(stdout.contains("search"));
    assert!(stdout.contains("add"));
    assert!(stdout.contains("stats"));
}

#[test]
fn test_version() {
    let output = Command::new(diwa_bin()).arg("--version").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("diwa"));
}

#[test]
fn test_search_unknown_repo() {
    let output = Command::new(diwa_bin())
        .args(["search", "nonexistent-repo-xyz", "test query"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("No indexed repo"));
}

#[test]
fn test_stats_unknown_repo() {
    let output = Command::new(diwa_bin())
        .args(["stats", "nonexistent-repo-xyz"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("No indexed repo"));
}

#[test]
fn test_search_help() {
    let output = Command::new(diwa_bin())
        .args(["search", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--deep"));
    assert!(stdout.contains("--json"));
}

#[test]
fn test_index_help() {
    let output = Command::new(diwa_bin())
        .args(["index", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--max-commits"));
    assert!(stdout.contains("--batch-size"));
}

#[test]
fn test_index_not_a_git_repo() {
    let tmp = tempfile::TempDir::new().unwrap();
    let output = Command::new(diwa_bin())
        .args(["index", tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success());
}

/// When `claude` cannot run (expired login), indexing must fail loudly and
/// leave the watermark alone, so the commits are retried rather than lost.
#[test]
fn test_index_does_not_advance_when_claude_fails() {
    let tmp = tempfile::TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let o = Command::new("git").args(args).current_dir(&repo).output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    };
    git(&["init", "-q"]);
    git(&["remote", "add", "origin", "https://github.com/acme/widget.git"]);
    std::fs::write(repo.join("a.txt"), "one").unwrap();
    git(&["add", "."]);
    git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "first"]);

    // A fake `claude` that fails the way an expired login does.
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let fake = bin.join("claude");
    std::fs::write(&fake, "#!/bin/sh\necho 'Failed to authenticate: OAuth session expired'\nexit 1\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

    let home = tmp.path().join("home");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let out = Command::new(diwa_bin())
        .args(["index", repo.to_str().unwrap()])
        .env("DIWA_DIR", &home)
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(!out.status.success(), "index must fail when claude is unavailable");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("without advancing the watermark"), "stderr: {err}");

    let stats = Command::new(diwa_bin())
        .args(["stats", repo.to_str().unwrap()])
        .env("DIWA_DIR", &home)
        .output()
        .unwrap();
    let s = String::from_utf8_lossy(&stats.stdout);
    assert!(s.contains("Insights:      0"), "stats: {s}");
    assert!(!s.contains("Last indexed:  ") || s.contains("Last indexed:  never") || s.contains("Last indexed:  -") || s.contains("none"), "stats: {s}");
}
