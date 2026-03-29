//! Generate deeper reflections from existing insights.
//!
//! Level 1: per-commit insights (what happened, why)
//! Level 2: reflections across many commits (patterns, arcs, journey)

use crate::claude;
use crate::db::{Insight, SearchResult};
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

/// Generate reflections from existing insights, grounded in repo evidence.
pub fn generate_reflections(
    insights: &[SearchResult],
    repo_name: &str,
    repo_path: &Path,
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
    repo_path: &Path,
    period: &str,
) -> anyhow::Result<Vec<Insight>> {
    if insights.is_empty() {
        return Ok(Vec::new());
    }

    let ground_truth = gather_ground_truth(repo_path, repo_name, insights);
    let prompt = build_prompt(insights, repo_name, period, &ground_truth);
    let response = claude::prompt(&prompt)?;
    let raw: Vec<RawReflection> = claude::parse_json_array(&response)?;
    Ok(hydrate(raw, insights))
}

// --- Ground truth gathering ---

fn gather_ground_truth(
    repo_path: &Path,
    repo_name: &str,
    insights: &[SearchResult],
) -> String {
    let mut ctx = String::new();

    if let Some(log) = git_log(repo_path) {
        ctx.push_str("ACTUAL GIT LOG (most recent 50 commits):\n");
        ctx.push_str(&log);
        ctx.push_str("\n\n");
    }

    if let Some(tree) = file_tree(repo_path) {
        ctx.push_str("CURRENT FILE TREE:\n");
        ctx.push_str(&tree);
        ctx.push_str("\n\n");
    }

    ctx.push_str(&file_snippets(repo_path, insights));
    ctx.push_str(&pr_data(repo_name));

    ctx
}

fn git_log(repo_path: &Path) -> Option<String> {
    Command::new("git")
        .args(["-C", &repo_path.to_string_lossy(), "log", "--oneline", "--no-merges", "-50"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
}

fn file_tree(repo_path: &Path) -> Option<String> {
    Command::new("git")
        .args(["-C", &repo_path.to_string_lossy(), "ls-tree", "--name-only", "-r", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .take(100)
                .collect::<Vec<_>>()
                .join("\n")
        })
}

fn file_snippets(repo_path: &Path, insights: &[SearchResult]) -> String {
    let mut ctx = String::new();
    let mut seen = HashSet::new();

    for insight in insights {
        for file in &insight.files {
            if !seen.insert(file.as_str()) || seen.len() > 10 {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(repo_path.join(file)) {
                let snippet: String = content.lines().take(30).collect::<Vec<_>>().join("\n");
                ctx.push_str(&format!("FILE: {file} (first 30 lines):\n{snippet}\n\n"));
            }
        }
    }

    ctx
}

fn pr_data(repo_name: &str) -> String {
    if !crate::github::gh_available() {
        return String::new();
    }

    let output = Command::new("gh")
        .args([
            "pr", "list", "--repo", repo_name, "--state", "merged",
            "--limit", "20", "--json", "number,title,body",
        ])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let prs = String::from_utf8_lossy(&o.stdout);
            if prs.len() > 10 {
                let truncated = if prs.len() > 3000 {
                    format!("{}...", &prs[..3000])
                } else {
                    prs.to_string()
                };
                format!("MERGED PRs (from GitHub):\n{truncated}\n\n")
            } else {
                String::new()
            }
        }
        _ => String::new(),
    }
}

// --- Prompt building ---

fn build_prompt(
    insights: &[SearchResult],
    repo_name: &str,
    period: &str,
    ground_truth: &str,
) -> String {
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
        let date = insight.commit_date.split('T').next().unwrap_or(&insight.commit_date);
        prompt.push_str(&format!(
            "{}. [{}] {} ({})\n   {}\n   tags: {}\n\n",
            i + 1, insight.category, insight.title, date, insight.body, insight.tags,
        ));
    }

    prompt.push_str("\nOutput the JSON array now. No markdown fences, no explanation, just the JSON.\n");
    prompt
}

// --- Parsing ---

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
    let fallback_sha = insights.first().map(|i| i.commit_sha.clone()).unwrap_or_default();
    let fallback_date = insights.first().map(|i| i.commit_date.clone()).unwrap_or_default();

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
