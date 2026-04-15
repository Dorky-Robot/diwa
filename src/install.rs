//! Install diwa into a repo via git hooks.
//!
//! Hooks are thin: they just call `diwa enqueue` which drops a flag file
//! in `~/.diwa/queue/`. The daemon watches that directory and runs the
//! actual indexing in the background. This keeps git operations fast
//! (no ORT init, no DB open) and naturally debounces bursty activity
//! like `git pull` landing many commits at once.

use anyhow::{Context, Result};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Hooks we install. `post-commit` covers local commits; `post-merge`
/// covers the `git pull`/fast-forward path that was missed before.
const HOOK_NAMES: &[&str] = &["post-commit", "post-merge"];

const HOOK_BODY: &str = r#"#!/usr/bin/env sh
# diwa: enqueue repo for indexing
# The daemon (installed via `diwa daemon install`) picks up the flag and
# indexes in the background, so this hook stays fast.
if command -v diwa >/dev/null 2>&1; then
  diwa enqueue . >/dev/null 2>&1 &
fi
"#;

const DIWA_MARKER: &str = "# diwa:";

/// Install diwa's git hooks into a repo.
///
/// Installs both `post-commit` and `post-merge`. If a hook already exists,
/// the diwa block is appended (or replaced in place if already present).
pub fn install_hook(repo_path: &Path) -> Result<()> {
    let hooks_dir = find_hooks_dir(repo_path)?;
    fs::create_dir_all(&hooks_dir)?;

    for name in HOOK_NAMES {
        install_single_hook(&hooks_dir.join(name))?;
    }

    remove_stale_hooks(repo_path, &hooks_dir);
    println!("diwa will now index new commits automatically.");
    Ok(())
}

fn install_single_hook(hook_path: &Path) -> Result<()> {
    let diwa_block: String = HOOK_BODY
        .lines()
        .skip(1) // skip shebang
        .collect::<Vec<_>>()
        .join("\n");

    if hook_path.exists() {
        let existing = fs::read_to_string(hook_path)?;

        if existing.contains(DIWA_MARKER) {
            // Replace existing diwa block in place.
            let updated = replace_diwa_block(&existing, &diwa_block);
            fs::write(hook_path, updated)?;
        } else {
            let appended = format!("{}\n\n{}\n", existing.trim_end(), diwa_block);
            fs::write(hook_path, appended)?;
        }
    } else {
        fs::write(hook_path, HOOK_BODY)?;
    }

    let mut perms = fs::metadata(hook_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(hook_path, perms)?;
    Ok(())
}

/// Splice a new diwa block in place of the existing one, preserving any
/// non-diwa content before and after.
fn replace_diwa_block(existing: &str, new_block: &str) -> String {
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
            // Absorb everything that looks like part of our block.
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
    updated.push_str(new_block);
    if !after_diwa.is_empty() {
        updated.push_str("\n\n");
        updated.push_str(&after_diwa.join("\n"));
    }
    updated.push('\n');
    updated
}

/// Remove diwa hooks from any hooks directory that isn't the active one.
///
/// Handles the case where `core.hooksPath` changed after the initial
/// `diwa init` — the old hook in `.git/hooks/` would never fire because git
/// only looks at the hooksPath location.
fn remove_stale_hooks(repo_path: &Path, active_hooks_dir: &Path) {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();

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

    for name in &[".husky"] {
        candidates.push(repo_path.join(name));
    }

    let canon_active = active_hooks_dir
        .canonicalize()
        .unwrap_or_else(|_| active_hooks_dir.to_path_buf());

    for candidate in candidates {
        let canon_candidate = candidate.canonicalize().unwrap_or_else(|_| candidate.clone());
        if canon_candidate == canon_active {
            continue;
        }

        for hook_name in HOOK_NAMES {
            strip_stale_hook(&candidate.join(hook_name));
        }
    }
}

fn strip_stale_hook(stale_hook: &Path) {
    if !stale_hook.exists() {
        return;
    }
    let Ok(contents) = fs::read_to_string(stale_hook) else {
        return;
    };
    if !contents.contains(DIWA_MARKER) {
        return;
    }

    let cleaned: String = contents
        .lines()
        .collect::<Vec<_>>()
        .split(|line| line.contains(DIWA_MARKER))
        .next()
        .unwrap_or(&[])
        .join("\n");
    let cleaned = cleaned.trim_end().to_string();

    if cleaned.is_empty() || cleaned == "#!/usr/bin/env sh" || cleaned == "#!/bin/sh" {
        let _ = fs::remove_file(stale_hook);
    } else {
        let _ = fs::write(stale_hook, format!("{cleaned}\n"));
    }
    println!("Removed stale diwa hook from {}", stale_hook.display());
}

/// Remove diwa's git hooks from a repo.
pub fn uninstall_hook(repo_path: &Path) -> Result<()> {
    let hooks_dir = find_hooks_dir(repo_path)?;

    let mut removed_any = false;
    for name in HOOK_NAMES {
        let hook_path = hooks_dir.join(name);
        if !hook_path.exists() {
            continue;
        }
        let existing = fs::read_to_string(&hook_path)?;
        if !existing.contains(DIWA_MARKER) {
            continue;
        }

        let cleaned: String = existing
            .lines()
            .collect::<Vec<_>>()
            .split(|line| line.contains(DIWA_MARKER))
            .next()
            .unwrap_or(&[])
            .join("\n");
        let cleaned = cleaned.trim_end().to_string();

        if cleaned.is_empty() || cleaned == "#!/usr/bin/env sh" || cleaned == "#!/bin/sh" {
            fs::remove_file(&hook_path)?;
            println!("Removed {}", hook_path.display());
        } else {
            fs::write(&hook_path, format!("{cleaned}\n"))?;
            println!("Removed diwa hook from {}", hook_path.display());
        }
        removed_any = true;
    }

    if !removed_any {
        println!("No diwa hooks found in {}", hooks_dir.display());
    }

    remove_stale_hooks(repo_path, &hooks_dir);
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

/// Outcome of scanning PATH for a `diwa` binary that shadows the current one.
#[derive(Debug, PartialEq, Eq)]
pub enum ShadowRepair {
    /// Current binary is first on PATH — nothing to do.
    Clean,
    /// A stale shadow was moved aside so hooks will resolve to the new binary.
    Repaired { shadow: PathBuf, backup: PathBuf },
    /// Something shadows us but we won't touch it (system path, or it isn't
    /// obviously stale). Caller should leave a breadcrumb for the user.
    Warned { shadow: PathBuf, reason: &'static str },
}

/// Detect and repair stale `diwa` binaries earlier on `$PATH` than the
/// currently running one.
///
/// Git hooks call `command -v diwa`, so if an older binary sits earlier on
/// PATH, the hook silently executes the old `diwa enqueue` which doesn't
/// exist pre-0.4.0 — indexing stops happening and the user sees nothing in
/// the daemon log. Running this from `diwa update` untangles PATH once per
/// upgrade instead of asking the user to debug their own shell config.
pub fn repair_shadowed_binaries() -> Vec<ShadowRepair> {
    let current = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    let current_canon = current.canonicalize().unwrap_or_else(|_| current.clone());

    let home = std::env::var("HOME").ok().map(PathBuf::from);
    let path_var = match std::env::var_os("PATH") {
        Some(p) => p,
        None => return Vec::new(),
    };

    let mut results = Vec::new();
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join("diwa");
        if !candidate.exists() {
            continue;
        }
        let candidate_canon = candidate
            .canonicalize()
            .unwrap_or_else(|_| candidate.clone());

        if candidate_canon == current_canon {
            // Reached ourselves — no shadow remains.
            if results.is_empty() {
                results.push(ShadowRepair::Clean);
            }
            return results;
        }

        if !is_stale_shadow(&candidate) {
            results.push(ShadowRepair::Warned {
                shadow: candidate,
                reason: "shadowing binary accepts `enqueue` — not stale",
            });
            continue;
        }

        let in_home = home
            .as_ref()
            .map(|h| candidate.starts_with(h))
            .unwrap_or(false);
        if !in_home {
            results.push(ShadowRepair::Warned {
                shadow: candidate,
                reason: "outside $HOME — remove manually",
            });
            continue;
        }

        let backup = stale_backup_path(&candidate);
        let _ = fs::remove_file(&backup);
        match fs::rename(&candidate, &backup) {
            Ok(()) => results.push(ShadowRepair::Repaired {
                shadow: candidate,
                backup,
            }),
            Err(_) => results.push(ShadowRepair::Warned {
                shadow: candidate,
                reason: "could not rename shadow aside",
            }),
        }
    }

    if results.is_empty() {
        results.push(ShadowRepair::Clean);
    }
    results
}

fn is_stale_shadow(shadow: &Path) -> bool {
    // Pre-0.4.0 diwa doesn't know `enqueue`; newer versions print usage and exit 0.
    let Ok(out) = std::process::Command::new(shadow)
        .arg("enqueue")
        .arg("--help")
        .output()
    else {
        // Binary that won't execute at all is as broken as a stale one.
        return true;
    };
    if out.status.success() {
        return false;
    }
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    combined.contains("unrecognized subcommand") || combined.contains("unrecognized")
}

fn stale_backup_path(shadow: &Path) -> PathBuf {
    let mut name = shadow
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "diwa".to_string());
    name.push_str(".stale-bak");
    shadow.with_file_name(name)
}
