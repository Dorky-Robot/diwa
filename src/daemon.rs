//! Background indexing daemon.
//!
//! The hub's only loop: every `DIWA_POLL_SECS` (default 600) fetch each
//! registered bare mirror and index whatever is new. There are no git hooks
//! and no queue; GitHub is the source of truth and we poll it.
//!
//! Managed by launchd on macOS. The `run` subcommand is launchd's
//! entrypoint; `install`/`uninstall`/`status` manage the LaunchAgent.

use anyhow::{bail, Context, Result};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::{manifest, mirror};

/// Default launchd label. Deliberately NOT the pre-hub `com.dorky-robot.diwa`,
/// so a hub can be installed beside an old per-machine daemon. Override with
/// `DIWA_LABEL`.
const DEFAULT_LABEL: &str = "com.dorky-robot.diwa-hub";

fn plist_label() -> String {
    std::env::var("DIWA_LABEL")
        .ok()
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| DEFAULT_LABEL.to_string())
}

/// What `install` does to launchd. Never a bootout followed by a separate
/// bootstrap in the foreground: over ssh that pair can kill the path it runs on.
#[derive(Debug, PartialEq, Eq)]
enum ReloadPlan {
    /// Not loaded: a single `bootstrap` (nothing to lose).
    Bootstrap,
    /// Loaded, plist unchanged: one atomic `kickstart -k`.
    Kickstart,
    /// Loaded, plist changed: launchd must re-read it, so run bootout and
    /// bootstrap together in a detached shell that outlives this process.
    DetachedReload,
}

fn reload_plan(loaded: bool, plist_changed: bool) -> ReloadPlan {
    match (loaded, plist_changed) {
        (false, _) => ReloadPlan::Bootstrap,
        (true, false) => ReloadPlan::Kickstart,
        (true, true) => ReloadPlan::DetachedReload,
    }
}

/// Run the poll loop. This is what launchd invokes.
pub fn run(diwa_dir: &Path) -> Result<()> {
    let poll = Duration::from_secs(
        std::env::var("DIWA_POLL_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(600),
    );
    log_line(diwa_dir, &format!("daemon starting (poll every {}s)", poll.as_secs()));
    // Let other machines find this store with a plain `diwa remote set <host>`.
    if let Err(e) = crate::remote::advertise(diwa_dir) {
        log_line(diwa_dir, &format!("could not advertise the store: {e:#}"));
    }
    loop {
        sweep(diwa_dir);
        std::thread::sleep(poll);
    }
}

/// One pass over every registered repo. A failure on one repo is logged and
/// never stops the others (a private repo losing access must not freeze the rest).
fn sweep(diwa_dir: &Path) {
    let mut repos: Vec<_> = manifest::read_manifest(diwa_dir).into_iter().collect();
    repos.sort();
    let max_commits = sweep_max_commits();
    let horizon = catch_up_since(diwa_dir);
    if let Some(h) = &horizon {
        log_line(diwa_dir, &format!("catching up on commits since {h} that were never read"));
    }
    for (slug, path) in repos {
        if !path.exists() {
            log_line(diwa_dir, &format!("path missing for '{slug}': {}", path.display()));
            continue;
        }
        // Only hub-managed mirrors are ours to fetch; a registered working
        // checkout (pre-hub entry) is indexed as it is.
        if path.extension().is_some_and(|e| e == "git") {
            if let Err(e) = mirror::fetch(&path) {
                log_line(diwa_dir, &format!("fetch failed {slug}: {e:#}"));
                continue;
            }
        }
        match crate::run_index(&path, max_commits, 8, false, horizon.as_deref()) {
            Ok(_) => log_line(diwa_dir, &format!("done {slug}")),
            Err(e) => log_line(diwa_dir, &format!("error {slug}: {e:#}")),
        }
    }
    if let Ok(log) = fs::read_to_string(diwa_dir.join("cost.jsonl")) {
        let (calls, tin, tout) = crate::claude::usage_totals(&log);
        log_line(diwa_dir, &format!("sweep over; usage to date: {calls} claude calls, {tin} tokens in, {tout} out"));
    }
}

/// How far back each sweep also reads commits that were never read
/// (`DIWA_CATCH_UP_SINCE`, else the first line of `<store>/catch-up-since`,
/// a date git understands). Unset: only what is new since the watermark. The
/// file can be written or removed while the daemon runs; nothing restarts.
fn catch_up_since(diwa_dir: &Path) -> Option<String> {
    std::env::var("DIWA_CATCH_UP_SINCE")
        .ok()
        .or_else(|| fs::read_to_string(diwa_dir.join("catch-up-since")).ok())
        .map(|s| s.lines().next().unwrap_or("").trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Commits one repo may index per sweep (`DIWA_SWEEP_MAX_COMMITS`, default
/// 5000). Lowering it throttles a backfill: each sweep does a bounded slice
/// and the next one resumes from the watermark, so spend is watchable.
fn sweep_max_commits() -> usize {
    std::env::var("DIWA_SWEEP_MAX_COMMITS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(5000)
}

fn log_line(diwa_dir: &Path, msg: &str) {
    let _ = fs::create_dir_all(diwa_dir);
    let log_path = diwa_dir.join("daemon.log");
    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let line = format!("[{timestamp}] {msg}\n");

    if let Ok(mut f) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

// ---------------------------------------------------------------------------
// launchd integration
// ---------------------------------------------------------------------------

pub fn install() -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("daemon install is currently macOS-only (launchd). On Linux, run `diwa daemon run` from a systemd user unit.");
    }

    let exe = std::env::current_exe().context("couldn't find current diwa binary")?;
    let diwa = diwa_dir();
    fs::create_dir_all(&diwa)?;

    // Ad-hoc sign before launchd grabs the binary. macOS TCC tracks
    // unsigned LaunchAgents by path+cdhash and re-prompts speculatively
    // (Photos / Contacts / Calendar) every time the binary is replaced —
    // a stable ad-hoc identity quiets that. We don't auto-escalate to
    // sudo here: `daemon install` is interactive, and a hint is friendlier
    // than a surprise password prompt when the binary is root-owned.
    codesign_adhoc_best_effort(&exe, false);

    let log_path = diwa.join("daemon.log");
    let path_env = build_daemon_path();
    let ort_dylib_env = find_ort_dylib();

    let ort_dylib_entry = if let Some(ref dylib) = ort_dylib_env {
        format!(
            "\n        <key>ORT_DYLIB_PATH</key>\n        <string>{}</string>",
            xml_escape(dylib)
        )
    } else {
        String::new()
    };

    // Carry the settings that make this instance what it is into the plist.
    let mut extra_env = String::new();
    for key in ["DIWA_DIR", "DIWA_MODEL", "DIWA_EFFORT", "DIWA_POLL_SECS", "DIWA_CLAUDE_TIMEOUT_SECS", "DIWA_GITHUB_BASE", "DIWA_SWEEP_MAX_COMMITS", "DIWA_CATCH_UP_SINCE"] {
        if let Ok(v) = std::env::var(key) {
            extra_env.push_str(&format!(
                "\n        <key>{key}</key>\n        <string>{}</string>",
                xml_escape(&v)
            ));
        }
    }
    let label = plist_label();

    let plist_body = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>daemon</string>
        <string>run</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>{path}</string>{ort_dylib}{extra_env}
    </dict>
    <key>WorkingDirectory</key>
    <string>{workdir}</string>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
    <key>ProcessType</key>
    <string>Background</string>
    <key>Nice</key>
    <integer>10</integer>
</dict>
</plist>
"#,
        label = label,
        extra_env = extra_env,
        exe = xml_escape(&exe.to_string_lossy()),
        // launchd's default cwd is `/`. Children we spawn (git, gh, claude)
        // inherit it, and anything that walks or resolves paths from cwd
        // then starts at the filesystem root — claude's project-context walk
        // reached ~/Desktop that way and drew a TCC prompt blamed on diwa.
        workdir = xml_escape(&diwa.to_string_lossy()),
        log = xml_escape(&log_path.to_string_lossy()),
        path = xml_escape(&path_env),
        ort_dylib = ort_dylib_entry,
    );

    let plist_file = plist_path()?;
    fs::create_dir_all(plist_file.parent().unwrap())?;
    let changed = fs::read_to_string(&plist_file).map(|old| old != plist_body).unwrap_or(true);
    fs::write(&plist_file, &plist_body)?;
    println!("Wrote {}", plist_file.display());

    let uid = current_uid()?;
    let domain = format!("gui/{uid}");
    let target = format!("{domain}/{label}");
    let loaded = Command::new("launchctl")
        .args(["print", &target])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    let plist_str = plist_file.to_string_lossy().into_owned();

    match reload_plan(loaded, changed) {
        ReloadPlan::Bootstrap => {
            let out = Command::new("launchctl")
                .args(["bootstrap", &domain, &plist_str])
                .output()
                .context("launchctl bootstrap failed")?;
            if !out.status.success() {
                bail!("launchctl bootstrap failed: {}", String::from_utf8_lossy(&out.stderr).trim());
            }
            println!("Loaded LaunchAgent {label}");
        }
        ReloadPlan::Kickstart => {
            let out = Command::new("launchctl")
                .args(["kickstart", "-k", &target])
                .output()
                .context("launchctl kickstart failed")?;
            if !out.status.success() {
                bail!("launchctl kickstart failed: {}", String::from_utf8_lossy(&out.stderr).trim());
            }
            println!("Restarted LaunchAgent {label} (kickstart -k)");
        }
        ReloadPlan::DetachedReload => {
            let script = format!(
                "launchctl bootout {target}; sleep 2; launchctl bootstrap {domain} '{plist_str}'"
            );
            Command::new("nohup")
                .args(["sh", "-c", &script])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .context("couldn't start detached reload")?;
            println!("Plist changed: reloading {label} in a detached shell (give it a few seconds)");
        }
    }
    println!("Logs: {}", log_path.display());
    Ok(())
}

/// Ad-hoc code-sign a binary so macOS TCC has a stable cdhash to track.
/// Without this, the unsigned LaunchAgent binary that gets swapped in by
/// `diwa upgrade` / `brew upgrade diwa` triggers speculative Photos /
/// Contacts / Calendar prompts on Sonoma+. Ad-hoc signing is free (no
/// Developer ID required) and silences those prompts.
///
/// `use_sudo` should be true when the caller already prompted for sudo
/// for an adjacent step (e.g. `cmd_upgrade` doing a `sudo cp` into
/// /usr/local/bin) — sudo's auth ticket is still warm so codesign won't
/// re-prompt. Pass false from non-sudo contexts; if the binary is
/// root-owned the sign attempt will fail and we hint at the manual fix
/// rather than escalating silently.
///
/// Best-effort: a permission failure becomes a soft hint, not a hard
/// error — TCC prompts are annoying but non-blocking, so a noisy failure
/// here would be worse than the prompts themselves. No-op on non-macOS.
/// Stable ad-hoc signing identity. MUST NOT track the binary's content or
/// name — that is the whole point (see `codesign_adhoc_best_effort`).
const SIGNING_IDENTIFIER: &str = "com.dorky-robot.diwa";

pub fn codesign_adhoc_best_effort(path: &Path, use_sudo: bool) {
    if !cfg!(target_os = "macos") {
        return;
    }

    let mut cmd = if use_sudo {
        let mut c = Command::new("sudo");
        c.arg("codesign");
        c
    } else {
        Command::new("codesign")
    };

    let result = cmd
        .args([
            "--sign",
            "-",
            "--force",
            // Without this, codesign DERIVES the identifier from the file name
            // and content hash, so every release is a different program as far
            // as TCC is concerned and the Photos/Contacts prompts start over.
            // Measured on two real release binaries:
            //   no -i  →  diwa-555549443024dbeb…  and  diwa-5555494470e57363…
            //   with   →  com.dorky-robot.diwa    both times
            "--identifier",
            SIGNING_IDENTIFIER,
            "--preserve-metadata=entitlements,flags",
        ])
        .arg(path)
        .output();

    if matches!(&result, Ok(out) if out.status.success()) {
        return;
    }

    eprintln!(
        "Note: could not ad-hoc sign {0}. Run\n  \
         sudo codesign --sign - --force --identifier com.dorky-robot.diwa \\\n           --preserve-metadata=entitlements,flags {0}\n\
         once to silence macOS TCC prompts (Photos / Contacts) on the unsigned binary.",
        path.display()
    );
}

pub fn uninstall() -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("daemon uninstall is currently macOS-only");
    }

    let plist_file = plist_path()?;
    let uid = current_uid()?;
    let domain = format!("gui/{uid}");

    if plist_file.exists() {
        // Captured, not printed — see the note in `install()`. The plist is
        // removed either way, so a bootout error here is unactionable.
        let _ = Command::new("launchctl")
            .args(["bootout", &domain, &plist_file.to_string_lossy()])
            .output();
        fs::remove_file(&plist_file)?;
        println!("Removed {}", plist_file.display());
    } else {
        println!("LaunchAgent not installed.");
    }

    Ok(())
}

pub fn status() -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("daemon status is currently macOS-only");
    }

    let plist_file = plist_path()?;
    if plist_file.exists() {
        println!("Plist installed: {}", plist_file.display());
    } else {
        println!("Plist NOT installed. Run `diwa daemon install`.");
    }

    let uid = current_uid()?;
    let target = format!("gui/{uid}/{}", plist_label());
    let output = Command::new("launchctl")
        .args(["print", &target])
        .output()
        .context("launchctl print failed")?;

    if output.status.success() {
        let text = String::from_utf8_lossy(&output.stdout);
        // Summarize — full output is verbose.
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("state =")
                || trimmed.starts_with("pid =")
                || trimmed.starts_with("last exit code")
            {
                println!("  {trimmed}");
            }
        }
    } else {
        println!("  (not loaded in launchd)");
    }

    let log_path = diwa_dir().join("daemon.log");
    if log_path.exists() {
        println!("Log: {}", log_path.display());
    }
    Ok(())
}

fn plist_path() -> Result<PathBuf> {
    let home = home_dir().context("$HOME not set")?;
    Ok(home.join("Library/LaunchAgents").join(format!("{}.plist", plist_label())))
}

fn current_uid() -> Result<u32> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .context("id -u failed")?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    text.parse()
        .with_context(|| format!("couldn't parse uid: {text}"))
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn diwa_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("DIWA_DIR") {
        PathBuf::from(dir)
    } else if let Some(home) = home_dir() {
        home.join(".diwa")
    } else {
        PathBuf::from(".diwa")
    }
}

/// PATH for the launchd-spawned daemon. Must include `~/.local/bin` so the
/// indexer can spawn `claude` (the CLI's default install location), plus the
/// usual system + homebrew dirs. Common user bin dirs are prepended only if
/// they actually exist on this machine — keeps PATH tight on fresh installs.
/// Probe well-known Homebrew locations for the ONNX Runtime dylib.
/// Returns the first path found, or `None` if the library isn't installed.
fn find_ort_dylib() -> Option<String> {
    for path in [
        "/usr/local/lib/libonnxruntime.dylib",    // Intel Homebrew
        "/opt/homebrew/lib/libonnxruntime.dylib", // Apple Silicon Homebrew
    ] {
        if std::path::Path::new(path).exists() {
            return Some(path.to_string());
        }
    }
    None
}

fn build_daemon_path() -> String {
    let mut dirs: Vec<String> = Vec::new();
    if let Some(home) = home_dir() {
        for sub in [".local/bin", "bin", ".cargo/bin"] {
            let p = home.join(sub);
            if p.exists() {
                dirs.push(p.to_string_lossy().into_owned());
            }
        }
    }
    for sys in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"] {
        dirs.push(sys.to_string());
    }
    dirs.join(":")
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reload_never_does_a_foreground_two_step() {
        assert_eq!(reload_plan(false, true), ReloadPlan::Bootstrap);
        assert_eq!(reload_plan(false, false), ReloadPlan::Bootstrap);
        assert_eq!(reload_plan(true, false), ReloadPlan::Kickstart);
        assert_eq!(reload_plan(true, true), ReloadPlan::DetachedReload);
    }

    #[test]
    fn default_label_does_not_collide_with_the_old_daemon() {
        assert_ne!(DEFAULT_LABEL, "com.dorky-robot.diwa");
    }

    #[test]
    fn install_source_has_no_foreground_bootout_then_bootstrap() {
        // The only bootout in install() lives inside the detached shell string.
        let src = include_str!("daemon.rs");
        let install = &src[src.find("pub fn install()").unwrap()..src.find("pub fn uninstall()").unwrap()];
        assert!(!install.contains(r#"args(["bootout""#));
    }
}
