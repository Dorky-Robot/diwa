//! Tests for the install/uninstall hook system.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use tempfile::TempDir;

/// Create a temp git repo for testing.
fn make_git_repo() -> TempDir {
    let tmp = TempDir::new().unwrap();
    Command::new("git")
        .args(["init", tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    // Need at least one commit for git to work properly.
    Command::new("git")
        .args(["-C", tmp.path().to_str().unwrap(), "commit", "--allow-empty", "-m", "init"])
        .output()
        .unwrap();
    tmp
}

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

#[test]
fn test_install_creates_hook() {
    let repo = make_git_repo();
    let hook_path = repo.path().join(".git/hooks/post-commit");

    assert!(!hook_path.exists());

    // Run diwa init — it will fail at indexing (no GitHub remote) but hook should be created.
    let _ = Command::new(diwa_bin())
        .args(["init", repo.path().to_str().unwrap()])
        .output();

    assert!(hook_path.exists());
    let content = fs::read_to_string(&hook_path).unwrap();
    assert!(content.contains("# diwa:"));
    assert!(content.contains("diwa index"));

    // Should be executable.
    let perms = fs::metadata(&hook_path).unwrap().permissions();
    assert_ne!(perms.mode() & 0o111, 0);
}

#[test]
fn test_install_idempotent() {
    let repo = make_git_repo();
    let hook_path = repo.path().join(".git/hooks/post-commit");

    // Install twice.
    let _ = Command::new(diwa_bin())
        .args(["init", repo.path().to_str().unwrap()])
        .output();
    let _ = Command::new(diwa_bin())
        .args(["init", repo.path().to_str().unwrap()])
        .output();

    let content = fs::read_to_string(&hook_path).unwrap();
    // Should only contain the diwa block once.
    assert_eq!(content.matches("# diwa:").count(), 1);
}

#[test]
fn test_install_appends_to_existing_hook() {
    let repo = make_git_repo();
    let hooks_dir = repo.path().join(".git/hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    let hook_path = hooks_dir.join("post-commit");

    // Write an existing hook.
    fs::write(&hook_path, "#!/usr/bin/env sh\necho 'existing hook'\n").unwrap();

    let _ = Command::new(diwa_bin())
        .args(["init", repo.path().to_str().unwrap()])
        .output();

    let content = fs::read_to_string(&hook_path).unwrap();
    assert!(content.contains("existing hook"));
    assert!(content.contains("# diwa:"));
    // Should NOT have a second shebang.
    assert_eq!(content.matches("#!/usr/bin/env sh").count(), 1);
}

#[test]
fn test_uninit_removes_hook() {
    let repo = make_git_repo();
    let hook_path = repo.path().join(".git/hooks/post-commit");

    // Install.
    let _ = Command::new(diwa_bin())
        .args(["init", repo.path().to_str().unwrap()])
        .output();
    assert!(hook_path.exists());

    // Uninit.
    let _ = Command::new(diwa_bin())
        .args(["uninit", repo.path().to_str().unwrap()])
        .output();

    // Hook file should be removed (was diwa-only).
    assert!(!hook_path.exists());
}

#[test]
fn test_uninit_preserves_other_hooks() {
    let repo = make_git_repo();
    let hooks_dir = repo.path().join(".git/hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    let hook_path = hooks_dir.join("post-commit");

    // Write an existing hook.
    fs::write(&hook_path, "#!/usr/bin/env sh\necho 'keep me'\n").unwrap();

    // Install diwa.
    let _ = Command::new(diwa_bin())
        .args(["init", repo.path().to_str().unwrap()])
        .output();

    // Uninit diwa.
    let _ = Command::new(diwa_bin())
        .args(["uninit", repo.path().to_str().unwrap()])
        .output();

    // File should still exist with the original content.
    assert!(hook_path.exists());
    let content = fs::read_to_string(&hook_path).unwrap();
    assert!(content.contains("keep me"));
    assert!(!content.contains("# diwa:"));
}

#[test]
fn test_uninit_noop_without_hook() {
    let repo = make_git_repo();
    let output = Command::new(diwa_bin())
        .args(["uninit", repo.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());
}

#[test]
fn test_custom_hooks_path() {
    let repo = make_git_repo();
    let custom_hooks = repo.path().join(".custom-hooks");

    // Set custom hooksPath.
    Command::new("git")
        .args([
            "-C", repo.path().to_str().unwrap(),
            "config", "core.hooksPath", ".custom-hooks",
        ])
        .output()
        .unwrap();

    let _ = Command::new(diwa_bin())
        .args(["init", repo.path().to_str().unwrap()])
        .output();

    // Should install in the custom path.
    let hook_path = custom_hooks.join("post-commit");
    assert!(hook_path.exists());
}

#[test]
fn test_stale_hooks_path_fallback() {
    let repo = make_git_repo();

    // Set hooksPath to a non-existent absolute path (simulates stale Docker mount).
    Command::new("git")
        .args([
            "-C", repo.path().to_str().unwrap(),
            "config", "core.hooksPath", "/nonexistent/stale/path/.husky",
        ])
        .output()
        .unwrap();

    let _ = Command::new(diwa_bin())
        .args(["init", repo.path().to_str().unwrap()])
        .output();

    // Should fall back to .git/hooks.
    let hook_path = repo.path().join(".git/hooks/post-commit");
    assert!(hook_path.exists());
}
