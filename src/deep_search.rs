//! Deep search — Claude-driven multi-query retrieval and synthesis.

use crate::claude;
use crate::db::{IndexDb, SearchResult};
use crate::embed;
use anyhow::Result;
use std::collections::HashMap;

/// Run a deep search: Claude drives retrieval and synthesizes an answer.
pub fn deep_search(db: &IndexDb, query: &str) -> Result<String> {
    // Step 1: Claude generates search strategies.
    let strategies = generate_search_strategies(query)?;

    // Step 2: Run each strategy + original query, collect unique results.
    let mut seen: HashMap<i64, SearchResult> = HashMap::new();

    for strategy in strategies.iter().chain(std::iter::once(&query.to_string())) {
        let query_embedding = embed::embed(strategy).ok();
        let results = db.search_hybrid(strategy, query_embedding.as_deref(), 5)?;
        for r in results {
            seen.entry(r.id).or_insert(r);
        }
    }

    let candidates: Vec<&SearchResult> = seen.values().collect();

    if candidates.is_empty() {
        return Ok(format!("No insights found for: {query}"));
    }

    // Step 3: Claude synthesizes an answer.
    synthesize(query, &candidates)
}

fn generate_search_strategies(query: &str) -> Result<Vec<String>> {
    let prompt = format!(
        r#"I need to search a knowledge base of software development insights. The user's query is:

"{query}"

Generate 3-5 alternative search queries that would help find relevant insights. Think about:
- Different phrasings of the same question
- Related technical terms
- Broader and narrower versions of the question
- The underlying concepts, not just the surface words

Output ONLY a JSON array of strings. No explanation."#
    );

    let response = claude::prompt(&prompt)?;
    claude::parse_json_array::<String>(&response)
        .or_else(|_| Ok(vec![query.to_string()]))
}

fn synthesize(query: &str, candidates: &[&SearchResult]) -> Result<String> {
    let mut prompt = format!(
        r#"A user asked this question about a software project's history:

"{query}"

Below are relevant insights extracted from the git history. Synthesize a clear, concise answer.

Rules:
- 1-2 short paragraphs. No more.
- Reference specific commits inline like (abc1234).
- Tell the story — what happened, why, what was learned.
- If something was tried and failed, include it — that's the interesting part.
- If the insights don't fully answer the question, say so briefly.
- No markdown headers. No bullet lists. End with a one-line "Sources:" listing the insight titles you drew from.

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

    claude::prompt(&prompt)
}
