//! Extract structured insights from commits using the `claude` CLI.

use crate::git::CommitData;
use crate::db::Insight;
use anyhow::{Context, Result};
use std::io::Write;
use std::process::Command;

/// Maximum total prompt size (chars) sent to claude per batch.
const MAX_PROMPT_CHARS: usize = 12_000;

/// Extract insights from a batch of commits using `claude -p`.
///
/// Returns a Vec of insights. On failure (claude not available, malformed output),
/// returns an empty vec and prints a warning rather than failing the whole indexing run.
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

fn try_extract(commits: &[CommitData]) -> Result<Vec<Insight>> {
    if commits.is_empty() {
        return Ok(Vec::new());
    }

    let prompt = build_prompt(commits);

    // Shell out to `claude -p` (print mode, no interactive session).
    let mut child = Command::new("claude")
        .args(["-p", "--output-format", "text"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("failed to run claude CLI — is it installed?")?;

    child
        .stdin
        .as_mut()
        .context("failed to open claude stdin")?
        .write_all(prompt.as_bytes())?;

    let output = child.wait_with_output().context("claude process failed")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("claude exited with error: {stderr}");
    }

    let response = String::from_utf8_lossy(&output.stdout).to_string();
    parse_insights(&response, commits)
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
            // Truncate this entry's diff portion.
            let header = format!(
                "### {} ({}) by {} on {}\n{}\nFiles: {}\n(diff truncated)\n\n",
                commit.sha,
                commit.date,
                commit.author,
                commit.date,
                commit.message,
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

/// Parse Claude's response into Insight structs.
///
/// Tries direct JSON parse first, then looks for JSON inside markdown fences.
fn parse_insights(response: &str, commits: &[CommitData]) -> Result<Vec<Insight>> {
    let trimmed = response.trim();

    // Try direct parse.
    if let Ok(raw) = serde_json::from_str::<Vec<RawInsight>>(trimmed) {
        return Ok(hydrate(raw, commits));
    }

    // Try extracting from markdown code fences.
    if let Some(json_str) = extract_json_from_fences(trimmed) {
        if let Ok(raw) = serde_json::from_str::<Vec<RawInsight>>(&json_str) {
            return Ok(hydrate(raw, commits));
        }
    }

    // Try finding a JSON array anywhere in the response.
    if let Some(start) = trimmed.find('[') {
        if let Some(end) = trimmed.rfind(']') {
            let slice = &trimmed[start..=end];
            if let Ok(raw) = serde_json::from_str::<Vec<RawInsight>>(slice) {
                return Ok(hydrate(raw, commits));
            }
        }
    }

    anyhow::bail!("could not parse insights from claude response")
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
            // Find the matching commit for date and source_type.
            let matching_commit = r
                .commit_sha
                .as_deref()
                .and_then(|sha| commits.iter().find(|c| c.sha.starts_with(sha)));

            let (date, source_type, pr_number) = match matching_commit {
                Some(c) => {
                    let st = if c.pr_body.is_some() {
                        "git+gh"
                    } else {
                        "git"
                    };
                    let pr = c.pr_body.as_ref().and(None); // PR number set by github enrichment
                    (c.date.clone(), st.to_string(), pr)
                }
                None => {
                    let date = commits
                        .first()
                        .map(|c| c.date.clone())
                        .unwrap_or_default();
                    ("git".to_string(), date, None)
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
                pr_number,
            }
        })
        .collect()
}

fn extract_json_from_fences(text: &str) -> Option<String> {
    let start_markers = ["```json\n", "```json\r\n", "```\n", "```\r\n"];

    for marker in &start_markers {
        if let Some(start) = text.find(marker) {
            let json_start = start + marker.len();
            if let Some(end) = text[json_start..].find("```") {
                return Some(text[json_start..json_start + end].to_string());
            }
        }
    }
    None
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
    fn test_parse_clean_json() {
        let response = r#"[{"commit_sha":"abc1234","category":"decision","title":"Adopted pull-based rendering","body":"Switched to avoid race conditions.","files":["lib/pull-manager.js"],"tags":"rendering"}]"#;

        let results = parse_insights(response, &dummy_commits()).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Adopted pull-based rendering");
    }

    #[test]
    fn test_parse_fenced_json() {
        let response = "Here are the insights:\n```json\n[{\"title\":\"Test\",\"body\":\"Body\"}]\n```\n";
        let results = parse_insights(response, &dummy_commits()).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_parse_json_with_preamble() {
        let response = "I found these insights:\n\n[{\"title\":\"Test\",\"body\":\"Body\"}]";
        let results = parse_insights(response, &dummy_commits()).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_parse_garbage_fails() {
        let response = "I don't know what to say";
        assert!(parse_insights(response, &dummy_commits()).is_err());
    }
}
