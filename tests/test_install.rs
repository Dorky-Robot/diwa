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
        .args([
            "-C",
            tmp.path().to_str().unwrap(),
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ])
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
    assert!(content.contains("diwa enqueue"));

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
            "-C",
            repo.path().to_str().unwrap(),
            "config",
            "core.hooksPath",
            ".custom-hooks",
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
fn test_update_renames_stale_shadow_in_home() {
    // Simulates the `~/.local/bin/diwa` (pre-0.4.0) shadowing the installed
    // binary scenario: a shell script earlier on PATH rejects `enqueue`, so
    // git hooks silently fail. `diwa update` should rename it aside.
    let home = TempDir::new().unwrap();
    let shadow_dir = home.path().join(".local/bin");
    fs::create_dir_all(&shadow_dir).unwrap();
    let shadow = shadow_dir.join("diwa");

    // Fake "old" diwa: behaves like pre-0.4.0 for `enqueue`.
    fs::write(
        &shadow,
        "#!/bin/sh\nif [ \"$1\" = \"enqueue\" ]; then\n  echo 'error: unrecognized subcommand '\"'\"'enqueue'\"'\"'' 1>&2\n  exit 2\nfi\nexit 0\n",
    )
    .unwrap();
    let mut perms = fs::metadata(&shadow).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&shadow, perms).unwrap();

    // Put the shadow *before* the real diwa on PATH.
    let real_dir = diwa_bin().parent().unwrap().to_path_buf();
    let path = format!(
        "{}:{}",
        shadow_dir.display(),
        real_dir.display(),
    );

    let output = Command::new(diwa_bin())
        .env("HOME", home.path())
        .env("PATH", &path)
        .arg("update")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "diwa update failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Shadow should have been moved aside.
    assert!(
        !shadow.exists(),
        "shadow binary still at {} — repair did not run",
        shadow.display()
    );
    let backup = shadow_dir.join("diwa.stale-bak");
    assert!(
        backup.exists(),
        "expected shadow to be renamed to {}",
        backup.display()
    );
}

#[test]
fn test_update_leaves_non_shadowing_path_alone() {
    // If the only `diwa` on PATH is the current binary, update should be a
    // no-op wrt PATH repair.
    let home = TempDir::new().unwrap();
    let real_dir = diwa_bin().parent().unwrap().to_path_buf();

    let output = Command::new(diwa_bin())
        .env("HOME", home.path())
        .env("PATH", real_dir.display().to_string())
        .arg("update")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("Moved stale"),
        "unexpected repair in clean-PATH scenario: {stdout}"
    );
}

#[test]
fn test_stale_hooks_path_fallback() {
    let repo = make_git_repo();

    // Set hooksPath to a non-existent absolute path (simulates stale Docker mount).
    Command::new("git")
        .args([
            "-C",
            repo.path().to_str().unwrap(),
            "config",
            "core.hooksPath",
            "/nonexistent/stale/path/.husky",
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
