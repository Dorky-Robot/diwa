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
    assert!(stdout.contains("browse"));
    assert!(stdout.contains("init"));
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
fn test_init_not_a_git_repo() {
    let tmp = tempfile::TempDir::new().unwrap();
    let output = Command::new(diwa_bin())
        .args(["init", tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not a git repo") || stderr.contains("No GitHub remote"));
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
