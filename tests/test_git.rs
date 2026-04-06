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

/// Indexing a repo should register it in the manifest, so that `diwa ls`
/// lists it even if the user never ran `diwa init` on it. This is the
/// self-healing behavior relied on by the post-commit hook.
#[test]
fn test_index_registers_repo_in_manifest() {
    let repo = make_repo_with_commits(&[
        ("main.rs", "fn main() {}", "feat: initial commit"),
    ]);
    // Give it a GitHub-looking remote so resolve_repo succeeds.
    Command::new("git")
        .args([
            "-C",
            repo.path().to_str().unwrap(),
            "remote",
            "add",
            "origin",
            "https://github.com/fake-owner/fake-repo.git",
        ])
        .output()
        .unwrap();

    let diwa_home = TempDir::new().unwrap();

    // Run index. We don't care whether the full pipeline succeeds (Claude/embed
    // model likely unavailable in test env). We only care that *before* doing
    // any of that, diwa wrote the slug into the manifest.
    let _ = Command::new(diwa_bin())
        .env("DIWA_DIR", diwa_home.path())
        .args(["index", repo.path().to_str().unwrap(), "--max-commits", "1"])
        .output()
        .unwrap();

    let manifest_path = diwa_home.path().join("repos.json");
    assert!(
        manifest_path.exists(),
        "expected {} to exist after `diwa index`",
        manifest_path.display()
    );
    let contents = fs::read_to_string(&manifest_path).unwrap();
    assert!(
        contents.contains("fake-owner--fake-repo"),
        "manifest should contain slug 'fake-owner--fake-repo', got: {contents}"
    );
}
