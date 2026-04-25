//! Background indexing daemon.
//!
//! Watches `~/.diwa/queue/` for flag files dropped by git hooks (via
//! `diwa enqueue`). Each flag file is named after a repo slug. When one
//! appears, we delete it first (so commits landing during indexing
//! create a fresh flag we'll catch next pass) then run the indexer.
//!
//! Managed by launchd on macOS. The `run` subcommand is launchd's
//! entrypoint; `install`/`uninstall`/`status` manage the LaunchAgent.

use anyhow::{bail, Context, Result};
use notify::{RecursiveMode, Watcher};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use crate::manifest;

const PLIST_LABEL: &str = "com.dorky-robot.diwa";
const PLIST_FILENAME: &str = "com.dorky-robot.diwa.plist";

/// Run the watcher loop. This is what launchd invokes.
pub fn run(diwa_dir: &Path) -> Result<()> {
    let queue = diwa_dir.join("queue");
    fs::create_dir_all(&queue)?;

    log_line(diwa_dir, "daemon starting");

    // Initial sweep: pick up anything that accumulated while we were down.
    process_all_pending(diwa_dir, &queue);

    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            let _ = tx.send(ev);
        }
    })?;
    watcher
        .watch(&queue, RecursiveMode::NonRecursive)
        .with_context(|| format!("failed to watch {}", queue.display()))?;

    log_line(diwa_dir, &format!("watching {}", queue.display()));

    loop {
        match rx.recv_timeout(Duration::from_secs(300)) {
            Ok(_) => {
                // Drain any burst of events so we coalesce into one pass.
                while rx.recv_timeout(Duration::from_millis(200)).is_ok() {}
                process_all_pending(diwa_dir, &queue);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Safety sweep in case fs events were missed (rare, but
                // fs watching isn't 100% reliable across all filesystems).
                process_all_pending(diwa_dir, &queue);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                log_line(diwa_dir, "watcher channel closed, exiting");
                break;
            }
        }
    }

    Ok(())
}

fn process_all_pending(diwa_dir: &Path, queue: &Path) {
    let slugs = match fs::read_dir(queue) {
        Ok(dir) => dir
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect::<Vec<_>>(),
        Err(_) => return,
    };

    if slugs.is_empty() {
        return;
    }

    let repos = manifest::read_manifest(diwa_dir);

    for slug in slugs {
        let flag = queue.join(&slug);

        // Delete BEFORE indexing so any commits landing during the index
        // run create a fresh flag that the next pass picks up.
        let _ = fs::remove_file(&flag);

        let Some(path) = repos.get(&slug) else {
            log_line(
                diwa_dir,
                &format!("unknown repo slug '{slug}' (not in manifest), skipping"),
            );
            continue;
        };

        if !path.exists() {
            log_line(
                diwa_dir,
                &format!("path missing for '{slug}': {}", path.display()),
            );
            continue;
        }

        log_line(diwa_dir, &format!("indexing {slug}"));
        match crate::run_index(path, 5000, 8, false) {
            Ok(_) => log_line(diwa_dir, &format!("done {slug}")),
            Err(e) => log_line(diwa_dir, &format!("error {slug}: {e:#}")),
        }
    }
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
    fs::create_dir_all(diwa.join("queue"))?;

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
        <string>{path}</string>{ort_dylib}
    </dict>
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
</dict>
</plist>
"#,
        label = PLIST_LABEL,
        exe = xml_escape(&exe.to_string_lossy()),
        log = xml_escape(&log_path.to_string_lossy()),
        path = xml_escape(&path_env),
        ort_dylib = ort_dylib_entry,
    );

    let plist_file = plist_path()?;
    fs::create_dir_all(plist_file.parent().unwrap())?;
    fs::write(&plist_file, plist_body)?;
    println!("Wrote {}", plist_file.display());

    // Reload: bootout (if loaded) then bootstrap.
    let uid = current_uid()?;
    let domain = format!("gui/{uid}");

    // Ignore bootout errors — it fails if not currently loaded.
    let _ = Command::new("launchctl")
        .args(["bootout", &domain, &plist_file.to_string_lossy()])
        .status();

    let status = Command::new("launchctl")
        .args(["bootstrap", &domain, &plist_file.to_string_lossy()])
        .status()
        .context("launchctl bootstrap failed")?;

    if !status.success() {
        bail!("launchctl bootstrap returned non-zero status");
    }

    println!("Loaded LaunchAgent {PLIST_LABEL}");
    println!("Logs: {}", log_path.display());
    Ok(())
}

/// Bootout the LaunchAgent if currently loaded. Used during `diwa upgrade`
/// to release the running daemon's hold on the old binary cleanly before
/// the new tarball is written into place — without this, the running
/// daemon keeps the old inode mapped while a new binary appears at the
/// same path, which is the exact condition that triggers macOS TCC to
/// re-prompt for Photos / Contacts / Calendar on Sonoma+. No-op on
/// non-macOS or if the agent isn't installed.
pub fn bootout_if_loaded() {
    if !cfg!(target_os = "macos") {
        return;
    }
    let Ok(plist_file) = plist_path() else {
        return;
    };
    if !plist_file.exists() {
        return;
    }
    let Ok(uid) = current_uid() else {
        return;
    };
    let domain = format!("gui/{uid}");
    let _ = Command::new("launchctl")
        .args(["bootout", &domain, &plist_file.to_string_lossy()])
        .status();
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
            "--preserve-metadata=entitlements,flags",
        ])
        .arg(path)
        .output();

    if matches!(&result, Ok(out) if out.status.success()) {
        return;
    }

    eprintln!(
        "Note: could not ad-hoc sign {0}. Run\n  \
         sudo codesign --sign - --force --preserve-metadata=entitlements,flags {0}\n\
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
        let _ = Command::new("launchctl")
            .args(["bootout", &domain, &plist_file.to_string_lossy()])
            .status();
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
    let target = format!("gui/{uid}/{PLIST_LABEL}");
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
    Ok(home.join("Library/LaunchAgents").join(PLIST_FILENAME))
}

fn current_uid() -> Result<u32> {
    let output = Command::new("id").arg("-u").output().context("id -u failed")?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    text.parse().with_context(|| format!("couldn't parse uid: {text}"))
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
        "/usr/local/lib/libonnxruntime.dylib",   // Intel Homebrew
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
