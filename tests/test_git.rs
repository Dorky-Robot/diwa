//! Tests for git module against real temp repositories.

use std::fs;
use std::process::Command;
use tempfile::TempDir;

fn diwa_bin() -> std::path::PathBuf {
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

/// Create a temp git repo with some commits.
fn make_repo_with_commits(commits: &[(&str, &str, &str)]) -> TempDir {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().to_str().unwrap();

    Command::new("git").args(["init", dir]).output().unwrap();
    Command::new("git")
        .args(["-C", dir, "config", "user.email", "test@test.com"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", dir, "config", "user.name", "Test"])
        .output()
        .unwrap();

    for (filename, content, message) in commits {
        let file_path = tmp.path().join(filename);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&file_path, content).unwrap();
        Command::new("git")
            .args(["-C", dir, "add", filename])
            .output()
            .unwrap();
        Command::new("git")
            .args(["-C", dir, "commit", "-m", message])
            .output()
            .unwrap();
    }

    tmp
}

#[test]
fn test_index_real_repo_no_remote() {
    let repo = make_repo_with_commits(&[
        ("main.rs", "fn main() {}", "feat: initial commit"),
        ("lib.rs", "pub fn hello() {}", "feat: add lib"),
    ]);

    // Index should fail gracefully (no GitHub remote).
    let output = Command::new(diwa_bin())
        .args(["index", repo.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("No GitHub remote"));
}

#[test]
fn test_stats_on_empty_index() {
    // Stats for an indexed repo that exists but is empty.
    let output = Command::new(diwa_bin())
        .args(["stats", "nonexistent-xyz"])
        .output()
        .unwrap();
    assert!(!output.status.success());
}
