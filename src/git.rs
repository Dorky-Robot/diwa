//! Read commits and diffs from git history.

use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

/// Raw commit data from git log.
#[derive(Debug, Clone)]
pub struct CommitData {
    pub sha: String,
    pub message: String,
    pub diff: String,
    pub date: String,
    pub author: String,
    pub files: Vec<String>,
    pub pr_title: Option<String>,
    pub pr_body: Option<String>,
    pub review_comments: Option<Vec<String>>,
}

const RECORD_SEP: &str = "---SIPAG-COMMIT-SEP---";
const FIELD_SEP: &str = "---SIPAG-FIELD-SEP---";

/// Maximum diff size per commit (chars). Larger diffs are truncated.
const MAX_DIFF_CHARS: usize = 2000;

/// List commits from a repo, optionally starting after a given SHA.
///
/// Returns commits in chronological order (oldest first) for natural batching.
pub fn list_commits(
    repo_path: &Path,
    since_sha: Option<&str>,
    max_commits: Option<usize>,
) -> Result<Vec<CommitData>> {
    let dir = repo_path.to_string_lossy();

    // Build git log range.
    let range = match since_sha {
        Some(sha) => format!("{sha}..HEAD"),
        None => "HEAD".to_string(),
    };

    let max_count = max_commits.unwrap_or(5000).to_string();

    let format = format!("{RECORD_SEP}%H{FIELD_SEP}%s%n%b{FIELD_SEP}%aI{FIELD_SEP}%an");

    let output = Command::new("git")
        .args([
            "-C",
            &dir,
            "log",
            "--no-merges",
            "--reverse",
            &format!("--max-count={max_count}"),
            &format!("--pretty=format:{format}"),
            &range,
        ])
        .output()
        .context("failed to run git log")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git log failed: {stderr}");
    }

    let log = String::from_utf8_lossy(&output.stdout);
    let mut commits = Vec::new();

    for record in log.split(RECORD_SEP).skip(1) {
        let fields: Vec<&str> = record.splitn(4, FIELD_SEP).collect();
        if fields.len() < 4 {
            continue;
        }

        let sha = fields[0].trim().to_string();
        let message = fields[1].trim().to_string();
        let date = fields[2].trim().to_string();
        let author = fields[3].trim().to_string();

        // Get diff for this commit (truncated).
        let diff = commit_diff(repo_path, &sha)?;

        // Get changed file list.
        let files = commit_files(repo_path, &sha)?;

        commits.push(CommitData {
            sha,
            message,
            diff,
            date,
            author,
            files,
            pr_title: None,
            pr_body: None,
            review_comments: None,
        });
    }

    Ok(commits)
}

/// Get the diff for a single commit, truncated to MAX_DIFF_CHARS.
fn commit_diff(repo_path: &Path, sha: &str) -> Result<String> {
    let output = Command::new("git")
        .args([
            "-C",
            &repo_path.to_string_lossy(),
            "diff",
            &format!("{sha}~1..{sha}"),
            "--stat",
            "--patch",
            "--no-color",
        ])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let full = String::from_utf8_lossy(&o.stdout).to_string();
            if full.len() > MAX_DIFF_CHARS {
                // Find a valid UTF-8 char boundary near MAX_DIFF_CHARS.
                let boundary = full
                    .char_indices()
                    .take_while(|(i, _)| *i <= MAX_DIFF_CHARS)
                    .last()
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                let mut truncated = full[..boundary].to_string();
                truncated.push_str("\n... (truncated)");
                Ok(truncated)
            } else {
                Ok(full)
            }
        }
        _ => Ok(String::new()), // first commit or error
    }
}

/// Get the list of files changed in a commit.
fn commit_files(repo_path: &Path, sha: &str) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args([
            "-C",
            &repo_path.to_string_lossy(),
            "diff-tree",
            "--no-commit-id",
            "-r",
            "--name-only",
            sha,
        ])
        .output()
        .context("failed to run git diff-tree")?;

    if !output.status.success() {
        return Ok(Vec::new());
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect())
}

/// Split commits into batches for processing.
pub fn batch_commits(commits: Vec<CommitData>, batch_size: usize) -> Vec<Vec<CommitData>> {
    commits
        .chunks(batch_size)
        .map(|chunk| chunk.to_vec())
        .collect()
}

/// Filter out noise commits (formula updates, version bumps, release tags).
pub fn filter_noise(commits: Vec<CommitData>) -> Vec<CommitData> {
    commits
        .into_iter()
        .filter(|c| {
            let msg = c.message.to_lowercase();
            !msg.starts_with("formula:")
                && !msg.starts_with("release:")
                && !msg.starts_with("chore: bump version")
                && !msg.starts_with("chore: update formula")
                && !msg.starts_with("merge pull request")
                && !msg.starts_with("merge branch")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_batch_commits() {
        let commits: Vec<CommitData> = (0..23)
            .map(|i| CommitData {
                sha: format!("sha{i}"),
                message: format!("commit {i}"),
                diff: String::new(),
                date: String::new(),
                author: String::new(),
                files: Vec::new(),
                pr_title: None,
                pr_body: None,
                review_comments: None,
            })
            .collect();

        let batches = batch_commits(commits, 5);
        assert_eq!(batches.len(), 5); // 5+5+5+5+3
        assert_eq!(batches[0].len(), 5);
        assert_eq!(batches[4].len(), 3);
    }

    #[test]
    fn test_filter_noise() {
        let commits = vec![
            CommitData {
                sha: "a".into(),
                message: "feat: add new feature".into(),
                diff: String::new(),
                date: String::new(),
                author: String::new(),
                files: Vec::new(),
                pr_title: None,
                pr_body: None,
                review_comments: None,
            },
            CommitData {
                sha: "b".into(),
                message: "formula: update to v0.43.3".into(),
                diff: String::new(),
                date: String::new(),
                author: String::new(),
                files: Vec::new(),
                pr_title: None,
                pr_body: None,
                review_comments: None,
            },
            CommitData {
                sha: "c".into(),
                message: "release: v0.43.3".into(),
                diff: String::new(),
                date: String::new(),
                author: String::new(),
                files: Vec::new(),
                pr_title: None,
                pr_body: None,
                review_comments: None,
            },
        ];

        let filtered = filter_noise(commits);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].sha, "a");
    }

    fn make_commit(sha: &str, msg: &str) -> CommitData {
        CommitData {
            sha: sha.into(),
            message: msg.into(),
            diff: String::new(),
            date: String::new(),
            author: String::new(),
            files: Vec::new(),
            pr_title: None,
            pr_body: None,
            review_comments: None,
        }
    }

    #[test]
    fn test_filter_noise_merge_commits() {
        let commits = vec![
            make_commit("a", "feat: something"),
            make_commit("b", "Merge pull request #42 from owner/branch"),
            make_commit("c", "Merge branch 'main' into feature"),
        ];
        let filtered = filter_noise(commits);
        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn test_filter_noise_keeps_fixes_and_refactors() {
        let commits = vec![
            make_commit("a", "fix: resolve race condition"),
            make_commit("b", "refactor: simplify state machine"),
            make_commit("c", "chore: bump version to 0.5.0"),
        ];
        let filtered = filter_noise(commits);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].sha, "a");
        assert_eq!(filtered[1].sha, "b");
    }

    #[test]
    fn test_batch_empty() {
        let batches = batch_commits(vec![], 5);
        assert!(batches.is_empty());
    }

    #[test]
    fn test_batch_single() {
        let commits = vec![make_commit("a", "one")];
        let batches = batch_commits(commits, 5);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].len(), 1);
    }

    #[test]
    fn test_batch_exact_fit() {
        let commits: Vec<_> = (0..10)
            .map(|i| make_commit(&format!("{i}"), "msg"))
            .collect();
        let batches = batch_commits(commits, 5);
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].len(), 5);
        assert_eq!(batches[1].len(), 5);
    }
}
