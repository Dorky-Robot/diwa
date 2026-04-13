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

    let log_path = diwa.join("daemon.log");

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
        <string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
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

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
