//! Bare clones the hub keeps fresh itself.
//!
//! The hub has no working checkouts and no git hooks: it registers
//! `org/name`, clones it bare under `~/.diwa/repos/<org>--<name>.git`, and
//! `git fetch`es it before every index pass. `git log`, `git diff sha~1..sha`
//! and `remote get-url origin` all work unchanged on a bare repo, so the
//! indexer needs no other change.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where clones are fetched from. Override with `DIWA_GITHUB_BASE`
/// (e.g. `git@github.com:` for private repos over ssh, or a file path in tests).
fn base_url() -> String {
    std::env::var("DIWA_GITHUB_BASE").unwrap_or_else(|_| "https://github.com/".to_string())
}

/// Split and validate `org/name`. The value becomes a path component and a
/// git argument, so anything that is not plain repo-name characters is refused.
pub fn parse_full_name(full_name: &str) -> Result<(String, String)> {
    let ok = |s: &str| {
        !s.is_empty()
            && !s.starts_with(['-', '.'])
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    match full_name.split_once('/') {
        Some((o, n)) if ok(o) && ok(n) => Ok((o.to_string(), n.to_string())),
        _ => bail!("expected org/name, got '{full_name}'"),
    }
}

pub fn mirror_path(diwa_dir: &Path, owner: &str, name: &str) -> PathBuf {
    diwa_dir.join("repos").join(format!("{owner}--{name}.git"))
}

/// Bring a bare mirror up to date with origin. Force-updates branches so a
/// rebase upstream follows (staleness.rs reports the Detached case).
pub fn fetch(path: &Path) -> Result<()> {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["fetch", "--prune", "--quiet", "origin", "+refs/heads/*:refs/heads/*"])
        .output()
        .context("failed to run git fetch")?;
    if !out.status.success() {
        bail!(
            "git fetch failed in {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Clone `org/name` bare if it is not there yet, otherwise fetch. Returns the mirror path.
pub fn ensure(diwa_dir: &Path, full_name: &str) -> Result<PathBuf> {
    let (owner, name) = parse_full_name(full_name)?;
    let path = mirror_path(diwa_dir, &owner, &name);
    if path.join("HEAD").exists() {
        fetch(&path)?;
        return Ok(path);
    }
    std::fs::create_dir_all(path.parent().unwrap())?;
    let url = format!("{}{owner}/{name}.git", base_url());
    let out = Command::new("git")
        .args(["clone", "--bare", "--quiet", &url])
        .arg(&path)
        .output()
        .context("failed to run git clone")?;
    if !out.status.success() {
        let _ = std::fs::remove_dir_all(&path);
        bail!(
            "git clone {url} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let s = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(args)
            .status()
            .unwrap();
        assert!(s.success(), "git {args:?}");
    }

    #[test]
    fn full_name_is_validated() {
        assert!(parse_full_name("Dorky-Robot/diwa").is_ok());
        assert!(parse_full_name("diwa").is_err());
        assert!(parse_full_name("a/b/c").is_err());
        assert!(parse_full_name("--upload-pack=x/y").is_err());
        assert!(parse_full_name("a/../b").is_err());
    }

    #[test]
    fn clone_then_fetch_picks_up_new_commits() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        git(&work, &["init", "-q", "-b", "main"]);
        git(&work, &["commit", "-q", "--allow-empty", "-m", "one"]);
        let origin = tmp.path().join("remote/org/name.git");
        std::fs::create_dir_all(origin.parent().unwrap()).unwrap();
        git(tmp.path(), &["clone", "-q", "--bare", work.to_str().unwrap(), origin.to_str().unwrap()]);

        // SAFETY of env use: only this test sets DIWA_GITHUB_BASE.
        std::env::set_var("DIWA_GITHUB_BASE", format!("{}/", tmp.path().join("remote").display()));
        let diwa = tmp.path().join("diwa");
        let mirror = ensure(&diwa, "org/name").unwrap();
        let count = |p: &Path| {
            let o = Command::new("git").arg("-C").arg(p).args(["rev-list", "--count", "HEAD"]).output().unwrap();
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        assert_eq!(count(&mirror), "1");

        git(&work, &["commit", "-q", "--allow-empty", "-m", "two"]);
        git(&work, &["push", "-q", origin.to_str().unwrap(), "main"]);
        ensure(&diwa, "org/name").unwrap();
        assert_eq!(count(&mirror), "2");
    }
}
