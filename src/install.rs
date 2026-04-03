//! Install diwa into a repo via git hooks.

use anyhow::{Context, Result};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

const POST_COMMIT_HOOK: &str = r#"#!/usr/bin/env sh
# diwa: index new commits automatically
# Runs in background so it doesn't slow down your workflow.
if command -v diwa >/dev/null 2>&1; then
  _diwa_log="${HOME}/.diwa/hooks.log"
  mkdir -p "$(dirname "$_diwa_log")"
  diwa index . --batch-size 1 >> "$_diwa_log" 2>&1 &
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

    // The diwa block without the shebang, for embedding into existing hooks.
    let diwa_block: String = POST_COMMIT_HOOK
        .lines()
        .skip(1) // skip shebang
        .collect::<Vec<_>>()
        .join("\n");

    if hook_path.exists() {
        let existing = fs::read_to_string(&hook_path)?;

        if existing.contains(DIWA_MARKER) {
            // Already installed — replace the diwa block in case the script changed.
            let mut before_diwa = Vec::new();
            let mut after_diwa = Vec::new();
            let mut in_diwa_block = false;
            let mut past_diwa_block = false;

            for line in existing.lines() {
                if line.contains(DIWA_MARKER) {
                    in_diwa_block = true;
                    continue;
                }
                if in_diwa_block {
                    // The diwa block ends at the next blank line or non-diwa content.
                    // We consume everything until we hit a line that doesn't look like
                    // part of our block (not indented, not a comment, not blank).
                    if line.is_empty()
                        || line.starts_with('#')
                        || line.starts_with(' ')
                        || line.starts_with("fi")
                        || line.starts_with("if ")
                    {
                        continue;
                    }
                    in_diwa_block = false;
                    past_diwa_block = true;
                }
                if past_diwa_block {
                    after_diwa.push(line);
                } else {
                    before_diwa.push(line);
                }
            }

            let mut updated = before_diwa.join("\n");
            updated = updated.trim_end().to_string();
            updated.push_str("\n\n");
            updated.push_str(&diwa_block);
            if !after_diwa.is_empty() {
                updated.push_str("\n\n");
                updated.push_str(&after_diwa.join("\n"));
            }
            updated.push('\n');

            fs::write(&hook_path, updated)?;
            println!("Updated diwa hook in {}", hook_path.display());
        } else {
            // Append to existing hook.
            let appended = format!("{}\n\n{}\n", existing.trim_end(), diwa_block);
            fs::write(&hook_path, appended)?;
            println!("Appended diwa hook to existing {}", hook_path.display());
        }
    } else {
        fs::write(&hook_path, POST_COMMIT_HOOK)?;
        println!("Created {}", hook_path.display());
    }

    // Ensure executable.
    let mut perms = fs::metadata(&hook_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&hook_path, perms)?;

    // Clean up stale hooks in the other location (e.g. .git/hooks when
    // we just installed to .husky/, or vice versa).
    remove_stale_hook(repo_path, &hooks_dir);

    println!("diwa will now index new commits automatically.");
    Ok(())
}

/// Remove a diwa hook from any hooks directory that isn't the active one.
///
/// This handles the case where `core.hooksPath` changed after the initial
/// `diwa init` — the old hook in `.git/hooks/` would never fire because git
/// only looks at the hooksPath location.
fn remove_stale_hook(repo_path: &Path, active_hooks_dir: &Path) {
    // Collect candidate directories that might have stale hooks.
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();

    // Always check .git/hooks as a candidate.
    let git_dir_output = std::process::Command::new("git")
        .args([
            "-C",
            &repo_path.to_string_lossy(),
            "rev-parse",
            "--git-dir",
        ])
        .output();
    if let Ok(output) = git_dir_output {
        if output.status.success() {
            let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let git_dir = if Path::new(&raw).is_absolute() {
                std::path::PathBuf::from(raw)
            } else {
                repo_path.join(raw)
            };
            candidates.push(git_dir.join("hooks"));
        }
    }

    // Also check common custom hook dirs.
    for name in &[".husky"] {
        candidates.push(repo_path.join(name));
    }

    for candidate in candidates {
        // Canonicalize both to compare correctly.
        let canon_candidate = candidate.canonicalize().unwrap_or_else(|_| candidate.clone());
        let canon_active = active_hooks_dir
            .canonicalize()
            .unwrap_or_else(|_| active_hooks_dir.to_path_buf());

        if canon_candidate == canon_active {
            continue;
        }

        let stale_hook = candidate.join("post-commit");
        if !stale_hook.exists() {
            continue;
        }

        if let Ok(contents) = fs::read_to_string(&stale_hook) {
            if !contents.contains(DIWA_MARKER) {
                continue;
            }

            // Remove just the diwa block from the stale hook.
            let cleaned: String = contents
                .lines()
                .collect::<Vec<_>>()
                .split(|line| line.contains(DIWA_MARKER))
                .next()
                .unwrap_or(&[])
                .join("\n");
            let cleaned = cleaned.trim_end().to_string();

            if cleaned.is_empty()
                || cleaned == "#!/usr/bin/env sh"
                || cleaned == "#!/bin/sh"
            {
                let _ = fs::remove_file(&stale_hook);
            } else {
                let _ = fs::write(&stale_hook, format!("{cleaned}\n"));
            }
            println!(
                "Removed stale diwa hook from {}",
                stale_hook.display()
            );
        }
    }
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

    // Also clean up any stale hooks in other locations.
    remove_stale_hook(repo_path, &hooks_dir);

    Ok(())
}

/// Exposed for testing.
pub fn find_hooks_dir(repo_path: &Path) -> Result<std::path::PathBuf> {
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
                std::path::PathBuf::from(&custom)
            } else {
                repo_path.join(&custom)
            };

            // Verify the path is writable. If core.hooksPath points to a
            // non-existent or read-only location (e.g. a stale Docker mount
            // like /work/repo/.husky from a sipag worker), fall back to
            // .git/hooks instead.
            if custom_path.exists() || fs::create_dir_all(&custom_path).is_ok() {
                return Ok(custom_path);
            }

            eprintln!(
                "Warning: core.hooksPath '{}' is not writable, using .git/hooks instead",
                custom
            );
        }
    }

    // Resolve the actual git directory (handles worktrees and submodules
    // where .git is a file, not a directory).
    let git_dir_output = std::process::Command::new("git")
        .args(["-C", &repo_path.to_string_lossy(), "rev-parse", "--git-dir"])
        .output()
        .context("failed to run git rev-parse --git-dir")?;

    let git_dir = if git_dir_output.status.success() {
        let raw = String::from_utf8_lossy(&git_dir_output.stdout)
            .trim()
            .to_string();
        let p = Path::new(&raw);
        if p.is_absolute() {
            std::path::PathBuf::from(raw)
        } else {
            repo_path.join(raw)
        }
    } else {
        repo_path.join(".git")
    };

    Ok(git_dir.join("hooks"))
}
