//! Optional GitHub enrichment — add PR context to commits when `gh` is available.

use crate::git::CommitData;
use anyhow::Result;
use std::process::Command;

/// Check if `gh` CLI is authenticated and available.
pub fn gh_available() -> bool {
    Command::new("gh")
        .args(["auth", "status"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Enrich commits with PR data from GitHub.
///
/// For each commit that references a PR (via commit message or merge history),
/// fetches the PR title, body, and review comments.
pub fn enrich_with_prs(commits: &mut [CommitData], repo: &str) -> Result<()> {
    if !gh_available() {
        return Ok(());
    }

    // Bulk-fetch merged PRs.
    let prs = fetch_merged_prs(repo, 200)?;

    for commit in commits.iter_mut() {
        // Match by PR number in commit message (e.g., "(#417)" or "Fixes #417").
        if let Some(pr_num) = extract_pr_number(&commit.message) {
            if let Some(pr) = prs.iter().find(|p| p.number == pr_num) {
                commit.pr_title = Some(pr.title.clone());
                commit.pr_body = Some(pr.body.clone());
                if !pr.review_comments.is_empty() {
                    commit.review_comments = Some(pr.review_comments.clone());
                }
            }
        }
    }

    Ok(())
}

struct PrData {
    number: u64,
    title: String,
    body: String,
    review_comments: Vec<String>,
}

fn fetch_merged_prs(repo: &str, limit: usize) -> Result<Vec<PrData>> {
    let output = Command::new("gh")
        .args([
            "pr",
            "list",
            "--repo",
            repo,
            "--state",
            "merged",
            "--limit",
            &limit.to_string(),
            "--json",
            "number,title,body,reviews",
        ])
        .output()?;

    if !output.status.success() {
        return Ok(Vec::new());
    }

    let parsed: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap_or_default();

    Ok(parsed
        .into_iter()
        .map(|pr| {
            let reviews = pr["reviews"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|r| r["body"].as_str().map(|s| s.to_string()))
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();

            PrData {
                number: pr["number"].as_u64().unwrap_or(0),
                title: pr["title"].as_str().unwrap_or("").to_string(),
                body: pr["body"].as_str().unwrap_or("").to_string(),
                review_comments: reviews,
            }
        })
        .collect())
}

/// Extract a PR number from a commit message.
///
/// Matches patterns like "(#123)", "Fixes #123", "Closes #123", "#123".
fn extract_pr_number(message: &str) -> Option<u64> {
    // Look for (#N) pattern first (most common in merge commits).
    for (i, _) in message.match_indices("(#") {
        let rest = &message[i + 2..];
        let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = num_str.parse::<u64>() {
            return Some(n);
        }
    }

    // Look for standalone #N.
    for (i, _) in message.match_indices('#') {
        // Skip if preceded by an alphanumeric char (e.g., "color#fff").
        if i > 0 && message.as_bytes()[i - 1].is_ascii_alphanumeric() {
            continue;
        }
        let rest = &message[i + 1..];
        let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = num_str.parse::<u64>() {
            return Some(n);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_pr_number_parens() {
        assert_eq!(extract_pr_number("fix: resolve bug (#417)"), Some(417));
    }

    #[test]
    fn test_extract_pr_number_closes() {
        assert_eq!(extract_pr_number("Closes #42"), Some(42));
    }

    #[test]
    fn test_extract_pr_number_standalone() {
        assert_eq!(extract_pr_number("feat: new thing #99"), Some(99));
    }

    #[test]
    fn test_extract_pr_number_none() {
        assert_eq!(extract_pr_number("just a commit"), None);
    }

    #[test]
    fn test_extract_pr_number_color_hash() {
        assert_eq!(extract_pr_number("color#fff is wrong"), None);
    }
}
