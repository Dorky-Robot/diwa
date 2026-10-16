//! Integration tests for diwa CLI.

use std::path::PathBuf;
use std::process::Command;

/// The binary under test, local even on a machine whose own diwa points at a
/// remote store (`~/.diwa/remote.json`), so results do not depend on the box.
fn local_diwa() -> Command {
    let mut c = Command::new(diwa_bin());
    c.env("DIWA_REMOTE", "local");
    c
}

fn diwa_bin() -> PathBuf {
    // Use the debug binary built by cargo test.
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
fn test_help() {
    let output = local_diwa().arg("--help").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("diwa"));
    assert!(stdout.contains("index"));
    assert!(stdout.contains("search"));
    assert!(stdout.contains("add"));
    assert!(stdout.contains("stats"));
}

#[test]
fn test_version() {
    let output = local_diwa().arg("--version").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("diwa"));
}

#[test]
fn test_search_unknown_repo() {
    let output = local_diwa()
        .args(["search", "nonexistent-repo-xyz", "test query"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("No indexed repo"));
}

#[test]
fn test_stats_unknown_repo() {
    let output = local_diwa()
        .args(["stats", "nonexistent-repo-xyz"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("No indexed repo"));
}

#[test]
fn test_search_help() {
    let output = local_diwa()
        .args(["search", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--deep"));
    assert!(stdout.contains("--json"));
}

#[test]
fn test_index_help() {
    let output = local_diwa()
        .args(["index", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--max-commits"));
    assert!(stdout.contains("--batch-size"));
}

#[test]
fn test_index_not_a_git_repo() {
    let tmp = tempfile::TempDir::new().unwrap();
    let output = local_diwa()
        .args(["index", tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success());
}

/// When `claude` cannot run (expired login), indexing must fail loudly and
/// leave the watermark alone, so the commits are retried rather than lost.
#[test]
fn test_index_does_not_advance_when_claude_fails() {
    let tmp = tempfile::TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let o = Command::new("git").args(args).current_dir(&repo).output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    };
    git(&["init", "-q"]);
    git(&["remote", "add", "origin", "https://github.com/acme/widget.git"]);
    std::fs::write(repo.join("a.txt"), "one").unwrap();
    git(&["add", "."]);
    git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "first"]);

    // A fake `claude` that fails the way an expired login does.
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let fake = bin.join("claude");
    std::fs::write(&fake, "#!/bin/sh\necho 'Failed to authenticate: OAuth session expired'\nexit 1\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

    let home = tmp.path().join("home");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let out = local_diwa()
        .args(["index", repo.to_str().unwrap()])
        .env("DIWA_DIR", &home)
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(!out.status.success(), "index must fail when claude is unavailable");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("without advancing the watermark"), "stderr: {err}");

    let stats = local_diwa()
        .args(["stats", repo.to_str().unwrap()])
        .env("DIWA_DIR", &home)
        .output()
        .unwrap();
    let s = String::from_utf8_lossy(&stats.stdout);
    assert!(s.contains("Insights:      0"), "stats: {s}");
    assert!(!s.contains("Last indexed:  ") || s.contains("Last indexed:  never") || s.contains("Last indexed:  -") || s.contains("none"), "stats: {s}");
}

// ---- every commit is read: oldest first, nothing dropped (the hub's 25-commit sweeps lost ~3,000) ----

struct Repo {
    tmp: tempfile::TempDir,
    repo: std::path::PathBuf,
    home: std::path::PathBuf,
    path: String,
    sent: std::path::PathBuf,
}

/// A repo of `n` commits (dated a day apart from 2026-01-01) and a fake
/// `claude` that writes the sha of every commit it is sent to a file and
/// answers with no insights, so nothing needs embedding.
fn repo_with(n: usize, body: &str) -> Repo {
    let tmp = tempfile::TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let r = Repo {
        repo: repo.clone(),
        home: tmp.path().join("home"),
        sent: tmp.path().join("sent.txt"),
        path: String::new(),
        tmp,
    };
    git_in(&repo, &["init", "-q"]);
    git_in(&repo, &["remote", "add", "origin", "https://github.com/acme/widget.git"]);
    for i in 1..=n {
        commit_in(&repo, i, body);
    }
    let bin = r.tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let fake = bin.join("claude");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\ncat | sed -n 's/^### \\([0-9a-f]\\{{40\\}}\\) by .*/\\1/p' >> '{}'\necho '[]'\n",
            r.sent.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    Repo { path: format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()), ..r }
}

fn git_in(repo: &std::path::Path, args: &[&str]) -> String {
    let o = Command::new("git").args(args).current_dir(repo).output().unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn commit_in(repo: &std::path::Path, i: usize, body: &str) {
    std::fs::write(repo.join(format!("f{i}.txt")), format!("{i}\n{body}")).unwrap();
    git_in(repo, &["add", "."]);
    let date = format!("2026-01-{:02}T12:00:00Z", i.min(28));
    let o = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", &format!("change {i}\n\n{body}")])
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(o.status.success());
}

impl Repo {
    fn index(&self, extra: &[&str]) -> String {
        let mut args = vec!["index", self.repo.to_str().unwrap()];
        args.extend_from_slice(extra);
        let out = local_diwa()
            .args(&args)
            .env("DIWA_DIR", &self.home)
            .env("PATH", &self.path)
            .env("GH_TOKEN", "")
            .output()
            .unwrap();
        assert!(out.status.success(), "index {extra:?}: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
    fn sent(&self) -> Vec<String> {
        std::fs::read_to_string(&self.sent).unwrap_or_default().lines().map(String::from).collect()
    }
    fn oldest_first(&self) -> Vec<String> {
        git_in(&self.repo, &["log", "--reverse", "--format=%H"]).lines().map(String::from).collect()
    }
    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.home.join("acme--widget").join("index.db")).unwrap()
    }
}

#[test]
fn a_capped_run_reads_the_oldest_commits_and_the_next_run_goes_on_from_there() {
    let r = repo_with(10, "");
    let all = r.oldest_first();
    let out = r.index(&["--max-commits", "3"]);
    assert!(out.contains("7 more wait for the next run"), "{out}");
    assert_eq!(r.sent(), all[..3], "the oldest three, not the newest");
    r.index(&["--max-commits", "3"]);
    r.index(&["--max-commits", "3"]);
    r.index(&["--max-commits", "3"]);
    assert_eq!(r.sent(), all, "every commit read once, in order, across capped runs");
    assert!(r.index(&["--max-commits", "3"]).contains("No new commits"));

    // the bug's shape: more new commits than one run may take
    for i in 11..=16 {
        commit_in(&r.repo, i, "");
    }
    let all = r.oldest_first();
    r.index(&["--max-commits", "2"]);
    r.index(&["--max-commits", "2"]);
    r.index(&["--max-commits", "2"]);
    assert_eq!(r.sent(), all, "the six new commits, oldest first, none skipped");
}

#[test]
fn a_batch_too_big_for_one_prompt_is_split_and_every_commit_is_sent() {
    // each commit carries ~2,000 chars of diff and ~1,500 of message: three or
    // so fit the 12,000-char prompt, and a batch of 8 used to drop the rest
    let r = repo_with(8, &"x".repeat(1500));
    let out = r.index(&["--batch-size", "8"]);
    assert!(out.contains("Batch 3/"), "split into several prompts: {out}");
    let mut sent = r.sent();
    sent.sort();
    let mut all = r.oldest_first();
    all.sort();
    assert_eq!(sent, all, "every commit went into a prompt");
}

#[test]
fn catch_up_reads_what_an_older_sweep_skipped_and_never_resends_what_was_read() {
    let r = repo_with(6, "");
    let all = r.oldest_first();
    r.index(&[]);
    assert_eq!(r.sent().len(), 6);

    // a store as the old sweeps left it: the watermark at HEAD, three
    // commits never read (no insight, no read record)
    let db = r.db();
    for sha in &all[1..4] {
        db.execute("DELETE FROM read_commits WHERE sha = ?1", [sha]).unwrap();
    }
    drop(db);
    assert!(r.index(&[]).contains("No new commits"), "an incremental run cannot see behind the watermark");
    assert_eq!(r.sent().len(), 6);

    let out = r.index(&["--catch-up-since", "2025-12-01"]);
    assert!(out.contains("Skipping 3 commits already read"), "{out}");
    let sent = r.sent();
    assert_eq!(sent[6..], all[1..4], "exactly the three skipped commits, oldest first");

    // read and empty is still read: a second catch-up sends nothing
    assert!(r.index(&["--catch-up-since", "2025-12-01"]).contains("No new commits"));
    assert_eq!(r.sent().len(), 9);

    // the horizon bounds it: only commits made since the date
    let db = r.db();
    db.execute("DELETE FROM read_commits", []).unwrap();
    drop(db);
    r.index(&["--catch-up-since", "2026-01-05"]);
    assert_eq!(r.sent()[9..], all[4..], "commits of Jan 5 and 6 only");
}

/// A commit pushed after the hub last fetched is found by `insight add` and
/// `show`, which fetch origin once; a sha that is nowhere still fails, and
/// says it fetched.
#[test]
fn insight_add_fetches_a_commit_the_mirror_has_not_seen() {
    let tmp = tempfile::tempdir().unwrap();
    let remote = tmp.path().join("remote");
    let origin = remote.join("org/thing.git");
    std::fs::create_dir_all(&origin).unwrap();
    git_in(&origin, &["init", "-q", "--bare", "-b", "main"]);
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    git_in(&work, &["init", "-q", "-b", "main"]);
    git_in(&work, &["remote", "add", "origin", origin.to_str().unwrap()]);
    git_in(&work, &["commit", "-q", "--allow-empty", "-m", "first"]);
    git_in(&work, &["push", "-q", "origin", "main"]);

    let hub = tmp.path().join("hub");
    let run = |args: &[&str]| {
        local_diwa()
            .env("DIWA_DIR", &hub)
            .env("DIWA_GITHUB_BASE", format!("{}/", remote.display()))
            .args(args)
            .output()
            .unwrap()
    };
    assert!(run(&["add", "org/thing", "--no-index"]).status.success());

    git_in(&work, &["commit", "-q", "--allow-empty", "-m", "second"]);
    git_in(&work, &["push", "-q", "origin", "main"]);
    let sha = git_in(&work, &["rev-parse", "HEAD"]);

    let out = run(&["insight", "add", "org/thing", &sha[..9], "Late", "--body", "why"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let shown = run(&["show", "org/thing", &sha[..9], "-n", "0"]);
    assert!(String::from_utf8_lossy(&shown.stdout).contains("Late"));

    let out = run(&["insight", "add", "org/thing", "deadbeef1", "Nope"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("fetched origin"));
}

/// Output into a closed pipe ends the process quietly instead of panicking.
#[test]
fn a_closed_pipe_is_not_a_panic() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!("DIWA_REMOTE=local DIWA_DIR={} {} --help | true", tmp.path().display(), diwa_bin().display()))
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&out.stderr).contains("panicked"));
}
