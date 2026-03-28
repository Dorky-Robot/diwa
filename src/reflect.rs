//! Generate deeper reflections from existing insights.
//!
//! Level 1: per-commit insights (what happened, why)
//! Level 2: reflections across many commits (patterns, arcs, journey)

use crate::db::{Insight, SearchResult};
use anyhow::{Context, Result};
use std::io::Write;
use std::process::Command;

/// Generate reflections from a set of existing insights.
///
/// Reads all Level 1 insights, gathers ground truth from the repo
/// (actual file contents, git log, PR data), and asks Claude to
/// reflect — but only on things it can verify from the code.
pub fn generate_reflections(
    insights: &[SearchResult],
    repo_name: &str,
    repo_path: &std::path::Path,
    period: &str,
) -> Vec<Insight> {
    match try_reflect(insights, repo_name, repo_path, period) {
        Ok(reflections) => reflections,
        Err(e) => {
            eprintln!("Warning: reflection generation failed: {e}");
            Vec::new()
        }
    }
}

fn try_reflect(
    insights: &[SearchResult],
    repo_name: &str,
    repo_path: &std::path::Path,
    period: &str,
) -> Result<Vec<Insight>> {
    if insights.is_empty() {
        return Ok(Vec::new());
    }

    let ground_truth = gather_ground_truth(repo_path, insights);
    let prompt = build_prompt(insights, repo_name, period, &ground_truth);

    let mut child = Command::new("claude")
        .args(["-p", "--output-format", "text"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("failed to run claude CLI")?;

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
    parse_reflections(&response, insights)
}

/// Gather ground truth from the repo to verify reflections against.
fn gather_ground_truth(
    repo_path: &std::path::Path,
    insights: &[SearchResult],
) -> String {
    let mut context = String::new();

    // Git log summary: the actual commit sequence
    if let Ok(output) = std::process::Command::new("git")
        .args([
            "-C",
            &repo_path.to_string_lossy(),
            "log",
            "--oneline",
            "--no-merges",
            "-50",
        ])
        .output()
    {
        if output.status.success() {
            let log = String::from_utf8_lossy(&output.stdout);
            context.push_str("ACTUAL GIT LOG (most recent 50 commits):\n");
            context.push_str(&log);
            context.push_str("\n\n");
        }
    }

    // Current file tree (top-level structure)
    if let Ok(output) = std::process::Command::new("git")
        .args([
            "-C",
            &repo_path.to_string_lossy(),
            "ls-tree",
            "--name-only",
            "-r",
            "HEAD",
        ])
        .output()
    {
        if output.status.success() {
            let tree = String::from_utf8_lossy(&output.stdout);
            // Truncate to avoid blowing up prompt
            let truncated: String = tree.lines().take(100).collect::<Vec<_>>().join("\n");
            context.push_str("CURRENT FILE TREE:\n");
            context.push_str(&truncated);
            context.push_str("\n\n");
        }
    }

    // Key files referenced in insights — read a snippet of each
    let mut seen_files = std::collections::HashSet::new();
    for insight in insights {
        for file in &insight.files {
            if seen_files.contains(file.as_str()) || seen_files.len() >= 10 {
                continue;
            }
            seen_files.insert(file.as_str());
            let full_path = repo_path.join(file);
            if let Ok(content) = std::fs::read_to_string(&full_path) {
                let snippet: String = content.lines().take(30).collect::<Vec<_>>().join("\n");
                context.push_str(&format!("FILE: {file} (first 30 lines):\n{snippet}\n\n"));
            }
        }
    }

    // PR data if gh is available
    if crate::github::gh_available() {
        if let Ok(output) = std::process::Command::new("gh")
            .args([
                "pr",
                "list",
                "--repo",
                &format!(
                    "{}",
                    repo_path
                        .to_string_lossy()
                ),
                "--state",
                "merged",
                "--limit",
                "20",
                "--json",
                "number,title,body",
            ])
            .output()
        {
            if output.status.success() {
                let prs = String::from_utf8_lossy(&output.stdout);
                if prs.len() > 10 {
                    // Truncate PR data
                    let truncated = if prs.len() > 3000 {
                        format!("{}...", &prs[..3000])
                    } else {
                        prs.to_string()
                    };
                    context.push_str("MERGED PRs (from GitHub):\n");
                    context.push_str(&truncated);
                    context.push_str("\n\n");
                }
            }
        }
    }

    context
}

fn build_prompt(insights: &[SearchResult], repo_name: &str, period: &str, ground_truth: &str) -> String {
    let mut prompt = format!(
        r#"You are reflecting on the development history of {repo_name} over {period}. Below are individual insights extracted from commits, PLUS ground truth data from the actual repository (git log, file tree, file contents, PRs).

Your job is to find DEEPER patterns — arcs and journeys that only become visible across many changes.

CRITICAL RULES:
- Every claim you make MUST be verifiable from the ground truth data provided (git log, file contents, PRs).
- Do NOT invent details, file names, or events that aren't in the evidence.
- If you're unsure about something, don't include it.
- Reference specific commits (by SHA) or files to anchor your reflections.
- If the insights mention something but the ground truth contradicts it, trust the ground truth.

Think like a senior engineer doing a retrospective:
- What was the real journey this {period}? Not the commits — the story.
- What patterns keep repeating? Are they intentional or accidental?
- What was tried, abandoned, and what eventually stuck? What does that reveal?
- What architectural direction is emerging that no single commit shows?
- What hard-won lessons emerged from the sequence of changes?

Output ONLY a JSON array. Each element must have:
- "category": always "reflection"
- "title": a one-line insight that could only come from seeing the full arc
- "body": 3-6 sentences of deep analysis. Reference specific commits or files as evidence.
- "tags": space-separated tags
- "commit_sha": use the SHA from the most relevant commit
- "files": array of key files relevant to the reflection

Generate 3-7 reflections. Quality over quantity. Each must be grounded in the evidence.

=== GROUND TRUTH (verify your claims against this) ===

{ground_truth}

=== INSIGHTS TO REFLECT ON ===

"#
    );

    for (i, insight) in insights.iter().enumerate() {
        let date = insight
            .commit_date
            .split('T')
            .next()
            .unwrap_or(&insight.commit_date);
        prompt.push_str(&format!(
            "{}. [{}] {} ({})\n   {}\n   tags: {}\n\n",
            i + 1,
            insight.category,
            insight.title,
            date,
            insight.body,
            insight.tags,
        ));
    }

    prompt.push_str("\nOutput the JSON array now. No markdown fences, no explanation, just the JSON.\n");
    prompt
}

fn parse_reflections(response: &str, insights: &[SearchResult]) -> Result<Vec<Insight>> {
    let trimmed = response.trim();

    // Try direct parse.
    if let Ok(raw) = serde_json::from_str::<Vec<RawReflection>>(trimmed) {
        return Ok(hydrate(raw, insights));
    }

    // Try extracting from markdown fences.
    if let Some(json_str) = extract_json_from_fences(trimmed) {
        if let Ok(raw) = serde_json::from_str::<Vec<RawReflection>>(&json_str) {
            return Ok(hydrate(raw, insights));
        }
    }

    // Try finding JSON array anywhere.
    if let Some(start) = trimmed.find('[') {
        if let Some(end) = trimmed.rfind(']') {
            let slice = &trimmed[start..=end];
            if let Ok(raw) = serde_json::from_str::<Vec<RawReflection>>(slice) {
                return Ok(hydrate(raw, insights));
            }
        }
    }

    anyhow::bail!("could not parse reflections from claude response")
}

#[derive(serde::Deserialize)]
struct RawReflection {
    category: Option<String>,
    title: String,
    body: String,
    tags: Option<String>,
    commit_sha: Option<String>,
    files: Option<Vec<String>>,
}

fn hydrate(raw: Vec<RawReflection>, insights: &[SearchResult]) -> Vec<Insight> {
    let fallback_sha = insights
        .first()
        .map(|i| i.commit_sha.clone())
        .unwrap_or_default();
    let fallback_date = insights
        .first()
        .map(|i| i.commit_date.clone())
        .unwrap_or_default();

    raw.into_iter()
        .map(|r| {
            let sha = r.commit_sha.unwrap_or_else(|| fallback_sha.clone());
            let date = insights
                .iter()
                .find(|i| i.commit_sha == sha)
                .map(|i| i.commit_date.clone())
                .unwrap_or_else(|| fallback_date.clone());

            Insight {
                commit_sha: sha,
                commit_date: date,
                category: r.category.unwrap_or_else(|| "reflection".to_string()),
                title: r.title,
                body: r.body,
                files: r.files.unwrap_or_default(),
                tags: r.tags.unwrap_or_default(),
                source_type: "reflection".to_string(),
                pr_number: None,
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
