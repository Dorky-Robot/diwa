//! Deep search — Claude-driven multi-query retrieval and synthesis.
//!
//! Instead of returning raw search results, Claude reads the query,
//! generates multiple search strategies, retrieves candidates, and
//! synthesizes a coherent answer grounded in the evidence.

use crate::db::{IndexDb, SearchResult};
use crate::embed;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::io::Write;
use std::process::Command;

/// Run a deep search: Claude drives retrieval and synthesizes an answer.
pub fn deep_search(db: &IndexDb, query: &str) -> Result<String> {
    // Step 1: Ask Claude to generate search strategies.
    let strategies = generate_search_strategies(query)?;

    // Step 2: Run each strategy against the DB, collect unique results.
    let mut seen = HashMap::new();
    for strategy in &strategies {
        let query_embedding = embed::embed(strategy).ok();
        let results = db.search_hybrid(strategy, query_embedding.as_deref(), 5)?;
        for r in results {
            seen.entry(r.id).or_insert(r);
        }
    }

    // Also run the original query.
    let query_embedding = embed::embed(query).ok();
    let original_results = db.search_hybrid(query, query_embedding.as_deref(), 10)?;
    for r in original_results {
        seen.entry(r.id).or_insert(r);
    }

    let candidates: Vec<&SearchResult> = seen.values().collect();

    if candidates.is_empty() {
        return Ok(format!("No insights found for: {query}"));
    }

    // Step 3: Ask Claude to synthesize an answer from the candidates.
    synthesize(query, &candidates)
}

/// Ask Claude to decompose the query into multiple search strategies.
fn generate_search_strategies(query: &str) -> Result<Vec<String>> {
    let prompt = format!(
        r#"I need to search a knowledge base of software development insights. The user's query is:

"{query}"

Generate 3-5 alternative search queries that would help find relevant insights. Think about:
- Different phrasings of the same question
- Related technical terms
- Broader and narrower versions of the question
- The underlying concepts, not just the surface words

Output ONLY a JSON array of strings. No explanation.

Example: ["query 1", "query 2", "query 3"]"#
    );

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
        .context("failed to open stdin")?
        .write_all(prompt.as_bytes())?;

    let output = child.wait_with_output()?;
    let response = String::from_utf8_lossy(&output.stdout).to_string();
    let trimmed = response.trim();

    // Parse JSON array of strings.
    if let Ok(strategies) = serde_json::from_str::<Vec<String>>(trimmed) {
        return Ok(strategies);
    }

    // Try extracting from fences.
    if let Some(start) = trimmed.find('[') {
        if let Some(end) = trimmed.rfind(']') {
            if let Ok(strategies) = serde_json::from_str::<Vec<String>>(&trimmed[start..=end]) {
                return Ok(strategies);
            }
        }
    }

    // Fallback: just use the original query.
    Ok(vec![query.to_string()])
}

/// Ask Claude to synthesize a coherent answer from candidate insights.
fn synthesize(query: &str, candidates: &[&SearchResult]) -> Result<String> {
    let mut prompt = format!(
        r#"A user asked this question about a software project's history:

"{query}"

Below are relevant insights extracted from the git history. Synthesize a clear, coherent answer that:
- Directly answers the question
- References specific commits (by SHA) as evidence
- Tells the story, not just lists facts
- Notes what was tried and didn't work, if relevant
- Is honest about uncertainty — if the insights don't fully answer the question, say so

Write 2-4 paragraphs. No markdown headers. Cite commits inline like (commit abc1234).

At the end, add a "Sources:" section listing the insight titles you drew from.

Here are the insights:

"#
    );

    for (i, r) in candidates.iter().enumerate() {
        let date = r.commit_date.split('T').next().unwrap_or(&r.commit_date);
        prompt.push_str(&format!(
            "{}. [{}] {} (commit: {}, date: {})\n   {}\n   tags: {}\n\n",
            i + 1,
            r.category,
            r.title,
            &r.commit_sha[..7.min(r.commit_sha.len())],
            date,
            r.body,
            r.tags,
        ));
    }

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
        .context("failed to open stdin")?
        .write_all(prompt.as_bytes())?;

    let output = child.wait_with_output()?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
