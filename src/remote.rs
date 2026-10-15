//! A diwa whose store lives on another machine.
//!
//! `diwa remote set <host>` points this machine at a diwa reached over ssh.
//! From then on `search`, `ls`, `stats`, `show` and `insight` run there and
//! print here exactly what they print there; indexing stays with the store.
//! `diwa remote unset` makes this machine's diwa local again.
//!
//! The setting is `remote.json` in the diwa dir. `DIWA_REMOTE=<host>`
//! overrides it for one shell, and `DIWA_REMOTE=local` ignores it (the
//! forwarded command sets that, so a store that itself has a remote set can
//! never bounce a command onward).

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::{Command, Stdio};

const FILE: &str = "remote.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Remote {
    /// ssh destination: a host alias from ~/.ssh/config, a tailnet name, user@host.
    pub host: String,
    /// diwa binary on the remote. Default `diwa`, looked up on a PATH that
    /// includes ~/.cargo/bin, /opt/homebrew/bin and /usr/local/bin, because
    /// non-login ssh shells often lack them. `~/` and relative paths resolve
    /// against the remote home.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bin: Option<String>,
    /// DIWA_DIR on the remote, when the store is not the remote's ~/.diwa.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
}

fn non_empty(key: &str) -> Option<String> {
    std::env::var(key).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// The remote this diwa uses, or `None` when it is local.
pub fn configured(diwa: &Path) -> Option<Remote> {
    match non_empty("DIWA_REMOTE").as_deref() {
        Some("local") => None,
        Some(host) => Some(Remote {
            host: host.to_string(),
            bin: non_empty("DIWA_REMOTE_BIN"),
            dir: non_empty("DIWA_REMOTE_DIR"),
        }),
        None => std::fs::read_to_string(diwa.join(FILE))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok()),
    }
}

pub fn save(diwa: &Path, r: &Remote) -> Result<()> {
    std::fs::create_dir_all(diwa)?;
    std::fs::write(diwa.join(FILE), serde_json::to_string_pretty(r)? + "\n")
        .with_context(|| format!("writing {}", diwa.join(FILE).display()))
}

/// Forget the remote. True when there was one.
pub fn clear(diwa: &Path) -> Result<bool> {
    match std::fs::remove_file(diwa.join(FILE)) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Quote for a POSIX shell: the remote side of ssh is always one.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A remote path: `~/x` and relative paths live under the remote $HOME.
fn remote_path(p: &str) -> String {
    let rel = p.strip_prefix("~/").or_else(|| (!p.starts_with('/')).then_some(p));
    match rel {
        // Inside double quotes only $ ` \ " and ! are special; escape them.
        Some(rel) => {
            let esc: String = rel
                .chars()
                .flat_map(|c| match c {
                    '$' | '`' | '\\' | '"' => vec!['\\', c],
                    _ => vec![c],
                })
                .collect();
            format!("\"$HOME/{esc}\"")
        }
        None => quote(p),
    }
}

/// The shell line the remote runs for `diwa <args>`.
pub fn command_line(r: &Remote, args: &[String]) -> String {
    let mut c = String::from(
        "DIWA_REMOTE=local PATH=\"$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH\"",
    );
    if let Some(dir) = &r.dir {
        c.push_str(" DIWA_DIR=");
        c.push_str(&remote_path(dir));
    }
    c.push_str(" exec ");
    match r.bin.as_deref() {
        Some(b) if b.contains('/') => c.push_str(&remote_path(b)),
        Some(b) => c.push_str(&quote(b)),
        None => c.push_str("diwa"),
    }
    for a in args {
        c.push(' ');
        c.push_str(&quote(a));
    }
    c
}

fn ssh(r: &Remote, args: &[String]) -> Command {
    let prog = non_empty("DIWA_SSH").unwrap_or_else(|| "ssh".to_string());
    let mut cmd = Command::new(prog);
    cmd.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "--", &r.host])
        .arg(command_line(r, args));
    cmd
}

/// ssh's own failures (unreachable, auth) exit 255.
const SSH_FAILED: i32 = 255;

fn unreachable_hint(r: &Remote) {
    eprintln!(
        "diwa: could not reach the store on {} over ssh. `diwa remote show` says where it points; `diwa remote unset` makes this diwa local again.",
        r.host
    );
}

/// Run `diwa <args>` on the remote with its output on ours; returns its exit
/// code. stdin is passed through only when the command reads it.
pub fn run(r: &Remote, args: &[String], stdin: bool) -> Result<i32> {
    let status = ssh(r, args)
        .stdin(if stdin { Stdio::inherit() } else { Stdio::null() })
        .status()
        .context("failed to run ssh")?;
    let code = status.code().unwrap_or(1);
    if code == SSH_FAILED {
        unreachable_hint(r);
    }
    Ok(code)
}

/// The remote's `diwa --version`, which also proves the whole path works.
pub fn probe(r: &Remote) -> Result<String> {
    let out = ssh(r, &["--version".to_string()])
        .stdin(Stdio::null())
        .output()
        .context("failed to run ssh")?;
    if !out.status.success() {
        bail!(
            "`diwa --version` on {} failed: {}",
            r.host,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(bin: Option<&str>, dir: Option<&str>) -> Remote {
        Remote { host: "hub".into(), bin: bin.map(Into::into), dir: dir.map(Into::into) }
    }

    #[test]
    fn arguments_reach_the_remote_unmangled() {
        let line = command_line(&r(None, None), &["search".into(), "it's $HOME; `x`".into()]);
        let out = Command::new("sh")
            .args(["-c", &format!("printf '%s\\n' {}", line.split_once(" exec diwa ").unwrap().1)])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "search\nit's $HOME; `x`\n");
    }

    #[test]
    fn home_relative_paths_expand_on_the_remote() {
        let line = command_line(&r(Some("~/.diwa-hub/bin/diwa"), Some(".diwa-hub")), &[]);
        assert!(line.contains("DIWA_DIR=\"$HOME/.diwa-hub\""), "{line}");
        assert!(line.contains("exec \"$HOME/.diwa-hub/bin/diwa\""), "{line}");
        let line = command_line(&r(Some("/opt/diwa"), None), &[]);
        assert!(line.contains("exec '/opt/diwa'"), "{line}");
        assert!(!line.contains("DIWA_DIR"));
        assert!(line.starts_with("DIWA_REMOTE=local "), "a store never forwards onward");
    }

    #[test]
    fn saved_setting_round_trips_and_clears() {
        let tmp = tempfile::tempdir().unwrap();
        let want = r(Some("~/bin/diwa"), None);
        save(tmp.path(), &want).unwrap();
        let got: Remote =
            serde_json::from_str(&std::fs::read_to_string(tmp.path().join(FILE)).unwrap()).unwrap();
        assert_eq!(got, want);
        assert!(clear(tmp.path()).unwrap());
        assert!(!clear(tmp.path()).unwrap());
    }
}
