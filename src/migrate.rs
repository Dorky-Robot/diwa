//! One-shot migration to the daemon-based indexing model.
//!
//! Rewrites hooks in every registered repo to use `diwa enqueue` (the
//! cheap path) instead of the old `diwa index .` (the expensive path
//! that loaded ORT on every commit). Also installs `post-merge` so
//! fast-forward pulls get indexed. Finally, loads the launchd agent.
//!
//! Idempotent and does NOT touch indexed data — just flips plumbing.
//! Safe to run as a homebrew post_install step and from the self-check
//! path in user-facing commands.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use crate::{daemon, install, manifest};

const META_FILE: &str = "meta.json";

#[derive(Debug, Serialize, Deserialize, Default)]
struct Meta {
    #[serde(default)]
    last_migrated_version: String,
}

pub fn run(diwa_dir: &Path) -> Result<()> {
    fs::create_dir_all(diwa_dir)?;
    fs::create_dir_all(diwa_dir.join("queue"))?;

    let repos = manifest::read_manifest(diwa_dir);
    if repos.is_empty() {
        println!("No registered repos — nothing to migrate.");
    } else {
        println!("Migrating {} registered repos...\n", repos.len());
        let mut ok = 0usize;
        let mut skipped = 0usize;
        let mut failed = 0usize;

        for (slug, path) in &repos {
            let display = slug.replace("--", "/");
            if !path.exists() {
                println!("  {display}: path missing ({}), skipping.", path.display());
                skipped += 1;
                continue;
            }
            match install::install_hook(path) {
                Ok(_) => {
                    println!("  {display}: hooks updated.");
                    ok += 1;
                }
                Err(e) => {
                    println!("  {display}: FAILED ({e}).");
                    failed += 1;
                }
            }
        }

        println!("\n{ok} updated, {skipped} skipped, {failed} failed.");
    }

    // Install/reload the launchd agent. Ignore failures on non-macOS.
    if cfg!(target_os = "macos") {
        println!("\nInstalling background daemon...");
        if let Err(e) = daemon::install() {
            eprintln!("Warning: daemon install failed: {e:#}");
            eprintln!("You can retry with `diwa daemon install`.");
        }
    } else {
        println!("\nSkipping daemon install (non-macOS). Run `diwa daemon run` from a systemd user unit.");
    }

    write_meta(diwa_dir, env!("CARGO_PKG_VERSION"))?;
    println!("\nMigration complete.");
    Ok(())
}

/// Read the recorded last-migrated version. Empty string if none.
pub fn last_migrated_version(diwa_dir: &Path) -> String {
    let path = diwa_dir.join(META_FILE);
    fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<Meta>(&s).ok())
        .map(|m| m.last_migrated_version)
        .unwrap_or_default()
}

fn write_meta(diwa_dir: &Path, version: &str) -> Result<()> {
    let path = diwa_dir.join(META_FILE);
    let meta = Meta {
        last_migrated_version: version.to_string(),
    };
    fs::write(path, serde_json::to_string_pretty(&meta)?)?;
    Ok(())
}

/// If the current binary version doesn't match the last-migrated version,
/// run migrate now. Called from user-facing commands so upgrades heal
/// themselves even if the homebrew post_install step didn't fire.
pub fn auto_migrate_if_needed(diwa_dir: &Path) {
    let current = env!("CARGO_PKG_VERSION");
    let last = last_migrated_version(diwa_dir);
    if last == current {
        return;
    }

    // Only auto-migrate when coming FROM a pre-daemon version. If `last`
    // is empty, we treat the manifest's mere existence as the signal that
    // this is an upgrade (fresh installs with no repos are a no-op).
    let manifest_exists = diwa_dir.join("repos.json").exists();
    if last.is_empty() && !manifest_exists {
        // Fresh install — just stamp the version.
        let _ = write_meta(diwa_dir, current);
        return;
    }

    eprintln!("diwa v{current}: migrating from v{}…", if last.is_empty() { "<old>" } else { last.as_str() });
    if let Err(e) = run(diwa_dir) {
        eprintln!("Warning: auto-migration failed: {e:#}");
    }
}
