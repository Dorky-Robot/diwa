//! Deep search — like `diwa search`, but deeper.
//!
//! Starts with a single search, reads through the results, and follows threads:
//! "oh that's interesting, let me search for that..." or "that commit looks
//! relevant, let me see the actual diff." One action at a time, like a human
//! researcher pulling on threads until the question is answered.

use crate::claude;
use crate::db::{IndexDb, SearchResult};
use crate::embed;
use crate::spinner::Spinner;
use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

/// Maximum research steps before forcing synthesis.
const MAX_STEPS: usize = 10;

/// Maximum results per vector search.
const SEARCH_LIMIT: usize = 8;

#[derive(Debug, Deserialize)]
struct Step {
    status: String, // "search", "git_show", "read_file", "done"
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    sha: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    args: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    answer: Option<String>,
}

/// Run a deep search. Starts from one search, follows the trail.
pub fn deep_search(db: &IndexDb, query: &str, repo_path: Option<&Path>) -> Result<String> {
    let spinner = Spinner::start("Searching...");

    // Track all search results we encounter for the commit index.
    let mut seen_results: HashMap<String, SearchResult> = HashMap::new();

    // Start with the user's actual query.
    let (initial_text, initial_hits) = run_search(db, query)?;

    if initial_hits.is_empty() {
        spinner.stop();
        return Ok(format!("No insights found for: {query}"));
    }

    collect_results(&mut seen_results, initial_hits);

    // Build up a research trail — each entry is (label, content).
    let mut trail: Vec<(String, String)> = vec![(
        format!("search: \"{query}\""),
        initial_text,
    )];

    // Now let Claude read the results and decide what to do next.
    for step_num in 1..=MAX_STEPS {
        spinner.set_message(&format!("Following threads ({step_num})..."));

        let next = decide_next_step(query, &trail)?;

        if let Some(thinking) = &next.thinking {
            if !thinking.is_empty() {
                spinner.set_message(thinking);
            }
        }

        match next.status.as_str() {
            "done" => {
                spinner.stop();
                let answer = next.answer.unwrap_or_else(|| {
                    "Deep search completed but produced no answer.".to_string()
                });
                return Ok(append_commit_index(&answer, &seen_results));
            }
            "search" => {
                let q = next.query.as_deref().unwrap_or(query);
                let (text, hits) = run_search(db, q)?;
                collect_results(&mut seen_results, hits);
                trail.push((format!("search: \"{q}\""), text));
            }
            "git_show" => {
                let sha = next.sha.as_deref().unwrap_or("HEAD");
                let result = run_git_show(sha, repo_path)?;
                let short = &sha[..7.min(sha.len())];
                trail.push((format!("git_show: {short}"), result));
            }
            "git_log" => {
                let args = next.args.as_deref().unwrap_or("-n 20");
                let result = run_git_log(args, repo_path)?;
                trail.push((format!("git_log: {args}"), result));
            }
            "read_file" => {
                let path = next.path.as_deref().unwrap_or("README.md");
                let result = run_read_file(path, repo_path)?;
                trail.push((format!("read_file: {path}"), result));
            }
            _ => break,
        }
    }

    // Hit the step limit — force synthesis with what we have.
    spinner.set_message("Synthesizing...");
    let answer = force_synthesize(query, &trail)?;
    spinner.stop();
    Ok(append_commit_index(&answer, &seen_results))
}

fn decide_next_step(query: &str, trail: &[(String, String)]) -> Result<Step> {
    let mut prompt = format!(
        r#"You are researching a question about a software project. You work like a curious human: you read results, notice something interesting, and pull on that thread.

The question: "{query}"

Here is your research trail so far:

"#
    );

    for (label, content) in trail {
        prompt.push_str(&format!("=== {label} ===\n{content}\n\n"));
    }

    prompt.push_str(
        r#"Read through what you've found. What's your next move?

Your options:
- "search" — search the insight index with a NEW query. Use this to follow a thread you noticed ("hm, that mentions a rewrite, let me search for that...") or to approach the question from a different angle. Don't repeat a search you already did.
- "git_show" — look at an actual commit diff. Use this when an insight references a commit and you want to see what really changed. Pass the full SHA from the insight.
- "git_log" — scan raw commit history. Pass "args" like "-n 30" or "--since=2025-03-01". Use sparingly.
- "read_file" — read a source file. Use when insights mention a file and you want to see the current state.
- "done" — you've followed enough threads and can write a thorough answer.

Think about: did something in the results surprise you? Is there a term or concept you could search to get a different angle? Did a commit SHA come up that's worth inspecting? Or do you have the full picture?

Output ONLY valid JSON (no markdown fences). ONE action — pick the single most valuable next step:

To continue researching:
{"status": "search", "query": "...", "thinking": "brief reason for this step"}
{"status": "git_show", "sha": "full_sha_from_results", "thinking": "want to see what this commit actually changed"}
{"status": "read_file", "path": "src/something.rs", "thinking": "checking current state of this module"}

To finish:
{"status": "done", "answer": "Your answer here. 1-3 paragraphs. Reference commits inline like (abc1234). Tell the story — what happened, why, what was learned. If something was tried and failed, include it. No markdown headers. No bullet lists. End with Sources: [insight titles]."}"#,
    );

    let response = claude::prompt(&prompt)?;
    parse_step(&response)
}

fn force_synthesize(query: &str, trail: &[(String, String)]) -> Result<String> {
    let mut prompt = format!(
        r#"You've been researching: "{query}"

Here is everything you found:

"#
    );

    for (label, content) in trail {
        prompt.push_str(&format!("=== {label} ===\n{content}\n\n"));
    }

    prompt.push_str(
        r#"Synthesize your answer now.

Rules:
- 1-3 paragraphs.
- Reference specific commits inline like (abc1234).
- Tell the story — what happened, why, what was learned.
- If something was tried and failed, include it — that's the interesting part.
- If the findings don't fully answer the question, say so briefly.
- No markdown headers. No bullet lists. End with a one-line "Sources:" listing the insight titles you drew from."#,
    );

    claude::prompt(&prompt)
}

fn parse_step(response: &str) -> Result<Step> {
    let trimmed = response.trim();

    if let Ok(step) = serde_json::from_str::<Step>(trimmed) {
        return Ok(step);
    }

    if let Some(json) = extract_json_block(trimmed) {
        if let Ok(step) = serde_json::from_str::<Step>(&json) {
            return Ok(step);
        }
    }

    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            let slice = &trimmed[start..=end];
            if let Ok(step) = serde_json::from_str::<Step>(slice) {
                return Ok(step);
            }
        }
    }

    // Can't parse — treat the whole thing as a final answer.
    Ok(Step {
        status: "done".into(),
        query: None,
        sha: None,
        path: None,
        args: None,
        thinking: None,
        answer: Some(trimmed.to_string()),
    })
}

// --- Tool execution ---

fn run_search(db: &IndexDb, query: &str) -> Result<(String, Vec<SearchResult>)> {
    let query_embedding = embed::embed(query).ok();
    let results = db.search_hybrid(query, query_embedding.as_deref(), SEARCH_LIMIT)?;

    if results.is_empty() {
        return Ok(("No results found.".into(), vec![]));
    }

    let mut out = String::new();
    for (i, r) in results.iter().enumerate() {
        let date = r.commit_date.split('T').next().unwrap_or(&r.commit_date);
        let sha = &r.commit_sha[..7.min(r.commit_sha.len())];
        out.push_str(&format!(
            "{}. [{}] {} (commit: {}, date: {})\n   {}\n   tags: {}\n\n",
            i + 1,
            r.category,
            r.title,
            sha,
            date,
            r.body,
            r.tags,
        ));
    }

    Ok((out, results))
}

fn collect_results(seen: &mut HashMap<String, SearchResult>, results: Vec<SearchResult>) {
    for r in results {
        let short = r.commit_sha[..7.min(r.commit_sha.len())].to_string();
        seen.entry(short).or_insert(r);
    }
}

/// Scan the answer for commit SHAs like (abc1234) and append a commit index footer.
fn append_commit_index(answer: &str, seen: &HashMap<String, SearchResult>) -> String {
    // Extract all 7-char hex strings in parentheses from the answer.
    let mut cited: Vec<(String, String)> = Vec::new();
    let mut cited_set = std::collections::HashSet::new();

    let mut i = 0;
    let bytes = answer.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'(' && i + 8 < bytes.len() && bytes[i + 8] == b')' {
            let candidate = &answer[i + 1..i + 8];
            if candidate.chars().all(|c| c.is_ascii_hexdigit()) && !cited_set.contains(candidate) {
                cited_set.insert(candidate.to_string());
                let date = seen
                    .get(candidate)
                    .map(|r| {
                        r.commit_date
                            .split('T')
                            .next()
                            .unwrap_or(&r.commit_date)
                            .to_string()
                    })
                    .unwrap_or_else(|| "git".into());
                cited.push((candidate.to_string(), date));
            }
            i += 9;
        } else {
            i += 1;
        }
    }

    if cited.is_empty() {
        return answer.to_string();
    }

    let mut out = answer.to_string();
    out.push_str("\n\n\x1b[90m---\x1b[0m\n");
    out.push_str("\x1b[90mCommits:\x1b[0m\n");
    for (sha, date) in &cited {
        out.push_str(&format!("  \x1b[90m{sha}  {date}\x1b[0m\n"));
    }

    out
}

fn run_git_show(sha: &str, repo_path: Option<&Path>) -> Result<String> {
    let Some(path) = repo_path else {
        return Ok("(repo path not available — run diwa search from inside the repo)".into());
    };

    let output = Command::new("git")
        .args([
            "-C",
            &path.to_string_lossy(),
            "show",
            "--stat",
            "--patch",
            "--no-color",
            sha,
        ])
        .output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);

    if stdout.len() > 6000 {
        let mut truncated = stdout[..6000].to_string();
        truncated.push_str("\n... (truncated)");
        Ok(truncated)
    } else {
        Ok(stdout.to_string())
    }
}

fn run_git_log(args: &str, repo_path: Option<&Path>) -> Result<String> {
    let Some(path) = repo_path else {
        return Ok("(repo path not available — run diwa search from inside the repo)".into());
    };

    let mut cmd = Command::new("git");
    cmd.args(["-C", &path.to_string_lossy(), "log", "--oneline"]);

    for arg in args.split_whitespace() {
        cmd.arg(arg);
    }

    let output = cmd.output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    if stdout.len() > 4000 {
        let mut truncated = stdout[..4000].to_string();
        truncated.push_str("\n... (truncated)");
        Ok(truncated)
    } else {
        Ok(stdout.to_string())
    }
}

fn run_read_file(rel_path: &str, repo_path: Option<&Path>) -> Result<String> {
    let Some(repo) = repo_path else {
        return Ok("(repo path not available — run diwa search from inside the repo)".into());
    };

    if rel_path.contains("..") {
        return Ok("(path traversal not allowed)".into());
    }

    let full_path = repo.join(rel_path);

    match std::fs::read_to_string(&full_path) {
        Ok(content) => {
            if content.len() > 8000 {
                let mut truncated = content[..8000].to_string();
                truncated.push_str("\n... (truncated)");
                Ok(truncated)
            } else {
                Ok(content)
            }
        }
        Err(e) => Ok(format!("(could not read: {e})")),
    }
}

fn extract_json_block(text: &str) -> Option<String> {
    let markers = ["```json\n", "```json\r\n", "```\n", "```\r\n"];
    for marker in &markers {
        if let Some(start) = text.find(marker) {
            let json_start = start + marker.len();
            if let Some(end) = text[json_start..].find("```") {
                return Some(text[json_start..json_start + end].to_string());
            }
        }
    }
    None
}
