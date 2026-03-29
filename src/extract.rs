//! Extract structured insights from commits using the `claude` CLI.

use crate::claude;
use crate::db::Insight;
use crate::git::CommitData;

/// Maximum total prompt size (chars) sent to claude per batch.
const MAX_PROMPT_CHARS: usize = 12_000;

/// Extract insights from a batch of commits using `claude -p`.
///
/// On failure, returns an empty vec and prints a warning.
pub fn extract_insights(commits: &[CommitData]) -> Vec<Insight> {
    match try_extract(commits) {
        Ok(insights) => insights,
        Err(e) => {
            eprintln!(
                "Warning: insight extraction failed for batch ({} commits): {e}",
                commits.len()
            );
            Vec::new()
        }
    }
}

fn try_extract(commits: &[CommitData]) -> anyhow::Result<Vec<Insight>> {
    if commits.is_empty() {
        return Ok(Vec::new());
    }

    let prompt = build_prompt(commits);
    let response = claude::prompt(&prompt)?;
    let raw: Vec<RawInsight> = claude::parse_json_array(&response)?;
    Ok(hydrate(raw, commits))
}

fn build_prompt(commits: &[CommitData]) -> String {
    let mut prompt = String::from(
        r#"You are analyzing git commits to extract structured insights for a searchable knowledge base. For each meaningful change, extract the underlying decision, learning, or architectural pattern.

Output ONLY a JSON array. Each element must have these fields:
- "commit_sha": the commit hash this insight is about
- "category": one of "decision", "pattern", "learning", "architecture", "migration", "bugfix"
- "title": a one-line summary of the insight (not the commit message — the deeper meaning)
- "body": 2-4 sentences explaining the reasoning, context, and what was learned
- "files": array of key file paths involved
- "tags": space-separated relevant tags

Rules:
- Skip trivial commits (typos, version bumps, formatting)
- Group related commits into a single insight when they tell one story
- Focus on the WHY, not the WHAT
- If commits show a pattern of trying and reverting, that IS an insight about what doesn't work
- If a commit fixes a bug, explain what caused it and what was learned

Commits:

"#,
    );

    let mut total_chars = prompt.len();

    for commit in commits {
        let entry = format_commit(commit);
        if total_chars + entry.len() > MAX_PROMPT_CHARS {
            let header = format!(
                "### {} by {} on {}\n{}\nFiles: {}\n(diff truncated)\n\n",
                commit.sha, commit.author, commit.date, commit.message,
                commit.files.join(", "),
            );
            prompt.push_str(&header);
            break;
        }
        total_chars += entry.len();
        prompt.push_str(&entry);
    }

    prompt.push_str("\nOutput the JSON array now. No markdown fences, no explanation, just the JSON.\n");
    prompt
}

fn format_commit(commit: &CommitData) -> String {
    let mut entry = format!(
        "### {} by {} on {}\n{}\n",
        commit.sha, commit.author, commit.date, commit.message,
    );

    if let Some(ref pr_title) = commit.pr_title {
        entry.push_str(&format!("PR: {pr_title}\n"));
    }
    if let Some(ref pr_body) = commit.pr_body {
        let body = if pr_body.len() > 500 {
            format!("{}...", &pr_body[..500])
        } else {
            pr_body.clone()
        };
        entry.push_str(&format!("PR body: {body}\n"));
    }
    if let Some(ref comments) = commit.review_comments {
        for (i, comment) in comments.iter().take(3).enumerate() {
            let c = if comment.len() > 200 {
                format!("{}...", &comment[..200])
            } else {
                comment.clone()
            };
            entry.push_str(&format!("Review comment {}: {c}\n", i + 1));
        }
    }

    if !commit.files.is_empty() {
        entry.push_str(&format!("Files: {}\n", commit.files.join(", ")));
    }

    if !commit.diff.is_empty() {
        entry.push_str(&format!("Diff:\n{}\n", commit.diff));
    }

    entry.push('\n');
    entry
}

#[derive(serde::Deserialize)]
struct RawInsight {
    commit_sha: Option<String>,
    category: Option<String>,
    title: String,
    body: String,
    files: Option<Vec<String>>,
    tags: Option<String>,
}

fn hydrate(raw: Vec<RawInsight>, commits: &[CommitData]) -> Vec<Insight> {
    raw.into_iter()
        .map(|r| {
            let matching_commit = r
                .commit_sha
                .as_deref()
                .and_then(|sha| commits.iter().find(|c| c.sha.starts_with(sha)));

            let (date, source_type) = match matching_commit {
                Some(c) => {
                    let st = if c.pr_body.is_some() { "git+gh" } else { "git" };
                    (c.date.clone(), st.to_string())
                }
                None => {
                    let date = commits
                        .first()
                        .map(|c| c.date.clone())
                        .unwrap_or_default();
                    (date, "git".to_string())
                }
            };

            Insight {
                commit_sha: r
                    .commit_sha
                    .unwrap_or_else(|| commits.first().map(|c| c.sha.clone()).unwrap_or_default()),
                commit_date: date,
                category: r.category.unwrap_or_else(|| "learning".to_string()),
                title: r.title,
                body: r.body,
                files: r.files.unwrap_or_default(),
                tags: r.tags.unwrap_or_default(),
                source_type,
                pr_number: None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_commits() -> Vec<CommitData> {
        vec![CommitData {
            sha: "abc1234".to_string(),
            message: "feat: add pull-based rendering".to_string(),
            diff: "+function pull() {}".to_string(),
            date: "2026-03-28T00:00:00Z".to_string(),
            author: "dev".to_string(),
            files: vec!["lib/pull-manager.js".to_string()],
            pr_title: None,
            pr_body: None,
            review_comments: None,
        }]
    }

    #[test]
    fn test_hydrate_with_matching_commit() {
        let raw = vec![RawInsight {
            commit_sha: Some("abc1234".into()),
            category: Some("decision".into()),
            title: "Test".into(),
            body: "Body".into(),
            files: Some(vec!["a.rs".into()]),
            tags: Some("test".into()),
        }];
        let results = hydrate(raw, &dummy_commits());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source_type, "git");
        assert!(results[0].pr_number.is_none());
    }

    #[test]
    fn test_hydrate_without_matching_commit() {
        let raw = vec![RawInsight {
            commit_sha: Some("zzz0000".into()),
            category: None,
            title: "Test".into(),
            body: "Body".into(),
            files: None,
            tags: None,
        }];
        let results = hydrate(raw, &dummy_commits());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].category, "learning");
        assert!(results[0].files.is_empty());
    }

    #[test]
    fn test_format_commit_with_pr() {
        let commit = CommitData {
            sha: "abc".into(),
            message: "feat: thing".into(),
            diff: String::new(),
            date: "2026-01-01".into(),
            author: "dev".into(),
            files: vec!["a.rs".into()],
            pr_title: Some("Add thing (#42)".into()),
            pr_body: Some("This PR adds thing.".into()),
            review_comments: Some(vec!["Looks good".into()]),
        };
        let formatted = format_commit(&commit);
        assert!(formatted.contains("PR: Add thing"));
        assert!(formatted.contains("PR body:"));
        assert!(formatted.contains("Review comment 1:"));
    }
}
