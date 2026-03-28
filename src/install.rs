//! Install diwa into a repo via git hooks.

use anyhow::{Context, Result};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

const POST_COMMIT_HOOK: &str = r#"#!/usr/bin/env sh
# diwa: index new commits automatically
# Runs in background so it doesn't slow down your workflow.
if command -v diwa >/dev/null 2>&1; then
  diwa index . --batch-size 1 >/dev/null 2>&1 &
fi
"#;

const DIWA_MARKER: &str = "# diwa:";

/// Install diwa's post-commit hook into a repo.
///
/// If a post-commit hook already exists, appends the diwa block.
/// If not, creates one.
pub fn install_hook(repo_path: &Path) -> Result<()> {
    // Support both .git/hooks and custom hooksPath.
    let hooks_dir = find_hooks_dir(repo_path)?;
    fs::create_dir_all(&hooks_dir)?;

    let hook_path = hooks_dir.join("post-commit");

    if hook_path.exists() {
        let existing = fs::read_to_string(&hook_path)?;

        if existing.contains(DIWA_MARKER) {
            println!("diwa hook already installed in {}", hook_path.display());
            return Ok(());
        }

        // Append to existing hook.
        let appended = format!(
            "{}\n\n{}\n",
            existing.trim_end(),
            POST_COMMIT_HOOK
                .lines()
                .skip(1) // skip shebang since hook already has one
                .collect::<Vec<_>>()
                .join("\n")
        );
        fs::write(&hook_path, appended)?;
        println!("Appended diwa hook to existing {}", hook_path.display());
    } else {
        fs::write(&hook_path, POST_COMMIT_HOOK)?;
        println!("Created {}", hook_path.display());
    }

    // Ensure executable.
    let mut perms = fs::metadata(&hook_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&hook_path, perms)?;

    println!("diwa will now index new commits automatically.");
    Ok(())
}

/// Remove diwa's post-commit hook from a repo.
pub fn uninstall_hook(repo_path: &Path) -> Result<()> {
    let hooks_dir = find_hooks_dir(repo_path)?;
    let hook_path = hooks_dir.join("post-commit");

    if !hook_path.exists() {
        println!("No post-commit hook found.");
        return Ok(());
    }

    let existing = fs::read_to_string(&hook_path)?;

    if !existing.contains(DIWA_MARKER) {
        println!("No diwa hook found in {}", hook_path.display());
        return Ok(());
    }

    // Remove the diwa block.
    let cleaned: String = existing
        .lines()
        .collect::<Vec<_>>()
        .split(|line| line.contains(DIWA_MARKER))
        .next()
        .unwrap_or(&[])
        .join("\n");

    let cleaned = cleaned.trim_end().to_string();

    if cleaned.is_empty() || cleaned == "#!/usr/bin/env sh" || cleaned == "#!/bin/sh" {
        // Hook was only diwa — remove the file.
        fs::remove_file(&hook_path)?;
        println!("Removed {}", hook_path.display());
    } else {
        fs::write(&hook_path, format!("{cleaned}\n"))?;
        println!("Removed diwa hook from {}", hook_path.display());
    }

    Ok(())
}

fn find_hooks_dir(repo_path: &Path) -> Result<std::path::PathBuf> {
    // Check for custom hooksPath (e.g. .husky).
    let output = std::process::Command::new("git")
        .args([
            "-C",
            &repo_path.to_string_lossy(),
            "config",
            "--get",
            "core.hooksPath",
        ])
        .output()
        .context("failed to run git config")?;

    if output.status.success() {
        let custom = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !custom.is_empty() {
            let custom_path = if Path::new(&custom).is_absolute() {
                std::path::PathBuf::from(custom)
            } else {
                repo_path.join(custom)
            };
            return Ok(custom_path);
        }
    }

    // Default: .git/hooks
    Ok(repo_path.join(".git").join("hooks"))
}
