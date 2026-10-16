//! Insights by hand, and everything known about one commit.
//!
//! `diwa insight add|edit|rm` lets a person add an insight to a commit, or
//! correct or remove one, extracted or not. The store is the index itself:
//! manual insights and overrides live in the `curations` table, which
//! `reindex` does not drop (see db.rs).
//!
//! `diwa show <repo> <sha>` prints every insight on a commit and the nearest
//! insights from every indexed repo, each with its repo and sha, so a reader
//! can hop from commit to commit.

use anyhow::{bail, Context, Result};
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::db::{self, IndexDb, InsightEdit, SearchResult};
use crate::{embed, git, manifest, sanitize};

/// Ids are 16 hex digits; this many is plenty to type and to show.
pub const SHORT_ID: usize = 8;

fn short(s: &str, n: usize) -> &str {
    &s[..n.min(s.len())]
}

/// Every repo with an index under `diwa`, as slugs, sorted.
pub fn indexed_slugs(diwa: &Path) -> Vec<String> {
    let mut slugs: Vec<String> = std::fs::read_dir(diwa)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().join("index.db").is_file())
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    slugs.sort();
    slugs
}

fn display(slug: &str) -> String {
    slug.replacen("--", "/", 1)
}

/// The git repo (bare mirror or checkout) registered for `slug`, if it is here.
fn repo_path(diwa: &Path, slug: &str) -> Option<PathBuf> {
    manifest::read_manifest(diwa).get(slug).cloned().filter(|p| p.exists())
}

/// Resolve `rev` in the repo at `path`. When it is not there and the repo is a
/// hub mirror, fetch origin once (the daemon's own fetch, under the same lock)
/// and look again: the daemon only polls every 10 minutes, so a commit pushed
/// a moment ago is on GitHub but not yet here. Returns the commit and a note
/// saying what the fetch did, for the error or the output.
fn resolve_or_fetch(path: &Path, rev: &str) -> (Option<git::CommitRef>, Option<String>) {
    if let Some(c) = git::resolve_commit(path, rev) {
        return (Some(c), None);
    }
    let looks_like_sha = (4..=40).contains(&rev.len()) && rev.chars().all(|c| c.is_ascii_hexdigit());
    if !looks_like_sha || !path.extension().is_some_and(|e| e == "git") {
        return (None, None);
    }
    match crate::mirror::fetch(path) {
        Ok(()) => {
            let c = git::resolve_commit(path, rev);
            (c, Some("fetched origin just now".to_string()))
        }
        Err(e) => (None, Some(format!("tried to fetch origin and could not: {e:#}"))),
    }
}

/// `--body -` reads the body from stdin, so long text needs no shell quoting.
fn read_body(body: Option<String>) -> Result<Option<String>> {
    match body.as_deref() {
        Some("-") => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s).context("reading body from stdin")?;
            Ok(Some(s.trim_end().to_string()))
        }
        _ => Ok(body),
    }
}

fn embed_text(title: &str, body: &str, tags: &str) -> Option<Vec<f32>> {
    match embed::embed(&format!("{title} {body} {tags}")) {
        Ok(v) => Some(v),
        Err(e) => {
            eprintln!("diwa: no vector for this insight (keyword search still finds it): {e:#}");
            None
        }
    }
}

pub struct AddArgs {
    pub repo: String,
    pub sha: String,
    pub title: String,
    pub body: Option<String>,
    pub category: String,
    pub tags: String,
}

pub fn add(diwa: &Path, slug: &str, a: AddArgs) -> Result<()> {
    if a.title.trim().is_empty() {
        bail!("an insight needs a title");
    }
    if !diwa.join(slug).join("index.db").is_file() && repo_path(diwa, slug).is_none() {
        bail!("No indexed repo matching '{}'.", a.repo);
    }
    // The commit must be real: from git when the repo is here, else from the index.
    let (sha, date) = if let Some(path) = repo_path(diwa, slug) {
        let (c, note) = resolve_or_fetch(&path, &a.sha);
        let c = c.with_context(|| match &note {
            Some(n) => format!("{} has no commit {} ({n}; it is not on origin either)", display(slug), a.sha),
            None => format!("{} has no commit {}", display(slug), a.sha),
        })?;
        if note.is_some() {
            eprintln!("diwa: {} was not in the mirror; fetched origin and found it", short(&c.sha, 7));
        }
        (c.sha, c.date)
    } else {
        let db = IndexDb::open(diwa, slug)?;
        let known = db.insights_for_commit(&a.sha)?;
        let first = known.first().with_context(|| {
            format!("{} is not here to check, and its index knows no commit {}", display(slug), a.sha)
        })?;
        (first.commit_sha.clone(), first.commit_date.clone())
    };
    let body = read_body(a.body)?.unwrap_or_default();
    let ins = db::Insight {
        commit_sha: sha.clone(),
        commit_date: date,
        category: a.category,
        title: a.title.trim().to_string(),
        body,
        files: Vec::new(),
        tags: a.tags,
        source_type: db::MANUAL.to_string(),
        pr_number: None,
        uid: None,
    };
    let embedding = embed_text(&ins.title, &ins.body, &ins.tags);
    let db = IndexDb::open(diwa, slug)?;
    let uid = db.add_manual(&ins, embedding.as_deref())?;
    println!(
        "Added {} to {}@{}: {}",
        short(&uid, SHORT_ID),
        display(slug),
        short(&sha, 7),
        sanitize::strip_display_controls(&ins.title)
    );
    Ok(())
}

/// Find the one insight whose id starts with `prefix`, in `only` or in every repo.
fn locate(diwa: &Path, prefix: &str, only: Option<&str>) -> Result<(IndexDb, SearchResult)> {
    let prefix = prefix.trim().to_ascii_lowercase();
    if prefix.len() < 4 || !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("'{prefix}' is not an insight id (at least 4 hex digits; `diwa show` lists them)");
    }
    let slugs = match only {
        Some(s) => vec![s.to_string()],
        None => indexed_slugs(diwa),
    };
    let mut found: Vec<(String, SearchResult)> = Vec::new();
    for slug in slugs {
        let db = IndexDb::open(diwa, &slug)?;
        found.extend(db.find_by_uid_prefix(&prefix)?.into_iter().map(|r| (slug.clone(), r)));
    }
    match found.len() {
        0 => bail!("no insight with id {prefix}"),
        1 => {
            let (slug, r) = found.pop().unwrap();
            Ok((IndexDb::open(diwa, &slug)?, r))
        }
        n => bail!(
            "id {prefix} matches {n} insights: {}; type more of it",
            found
                .iter()
                .map(|(slug, r)| format!("{} ({})", short(&r.uid, 12), display(slug)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

pub fn edit(diwa: &Path, id: &str, only: Option<&str>, change: InsightEdit) -> Result<()> {
    let change = InsightEdit { body: read_body(change.body)?, ..change };
    if change.is_empty() {
        bail!("nothing to change: pass --title, --body, --category or --tags");
    }
    let (db, cur) = locate(diwa, id, only)?;
    let title = change.title.as_deref().unwrap_or(&cur.title);
    let body = change.body.as_deref().unwrap_or(&cur.body);
    let tags = change.tags.as_deref().unwrap_or(&cur.tags);
    let embedding = embed_text(title, body, tags);
    let r = db.edit(&cur.uid, &change, embedding.as_deref())?;
    println!(
        "Edited {} on {}@{}: {}",
        short(&r.uid, SHORT_ID),
        display(db.slug()),
        short(&r.commit_sha, 7),
        sanitize::strip_display_controls(&r.title)
    );
    Ok(())
}

pub fn rm(diwa: &Path, id: &str, only: Option<&str>) -> Result<()> {
    let (db, cur) = locate(diwa, id, only)?;
    let r = db.delete(&cur.uid)?;
    println!(
        "Removed {} from {}@{}: {}",
        short(&r.uid, SHORT_ID),
        display(db.slug()),
        short(&r.commit_sha, 7),
        sanitize::strip_display_controls(&r.title)
    );
    Ok(())
}

/// How an insight came to be, as `show` labels it.
fn origin(db: &IndexDb, r: &SearchResult) -> &'static str {
    match r.source_type.as_str() {
        db::MANUAL => "manual",
        "reflection" if db.is_edited(&r.uid).unwrap_or(false) => "reflection, edited",
        "reflection" => "reflection",
        _ if db.is_edited(&r.uid).unwrap_or(false) => "extracted, edited",
        _ => "extracted",
    }
}

#[derive(serde::Serialize)]
struct ShownInsight {
    #[serde(flatten)]
    insight: SearchResult,
    origin: &'static str,
}

#[derive(serde::Serialize)]
struct Related {
    repo: String,
    #[serde(flatten)]
    insight: SearchResult,
}

#[derive(serde::Serialize)]
struct Shown {
    repo: String,
    commit: String,
    date: String,
    subject: String,
    insights: Vec<ShownInsight>,
    related: Vec<Related>,
}

fn normalized_mean(vs: &[Vec<f32>]) -> Option<Vec<f32>> {
    let dim = vs.first()?.len();
    let mut m = vec![0f32; dim];
    for v in vs.iter().filter(|v| v.len() == dim) {
        for (a, b) in m.iter_mut().zip(v) {
            *a += b;
        }
    }
    let norm = m.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm == 0.0 {
        return None;
    }
    Some(m.into_iter().map(|x| x / norm).collect())
}

fn same_commit(a: &str, b: &str) -> bool {
    a.len() >= 7 && b.len() >= 7 && (a.starts_with(b) || b.starts_with(a))
}

/// The `n` insights from every indexed repo closest in meaning to `query`,
/// leaving out the commit being shown.
fn related(diwa: &Path, query: &[f32], slug: &str, sha: &str, n: usize) -> Result<Vec<Related>> {
    let mut all = Vec::new();
    for other in indexed_slugs(diwa) {
        let db = IndexDb::open(diwa, &other)?;
        for r in db.search_semantic(query, n + 8)? {
            if other == slug && same_commit(&r.commit_sha, sha) {
                continue;
            }
            all.push(Related { repo: display(&other), insight: r });
        }
    }
    all.sort_by(|a, b| b.insight.rank.partial_cmp(&a.insight.rank).unwrap_or(std::cmp::Ordering::Equal));
    all.truncate(n);
    Ok(all)
}

pub fn show(diwa: &Path, slug: &str, rev: &str, n: usize, json: bool) -> Result<()> {
    let db = IndexDb::open(diwa, slug)?;
    let (commit, fetch_note) = match repo_path(diwa, slug) {
        Some(p) => resolve_or_fetch(&p, rev),
        None => (None, None),
    };
    let insights = db.insights_for_commit(commit.as_ref().map(|c| c.sha.as_str()).unwrap_or(rev))?;
    let (sha, date, subject) = match (&commit, insights.first()) {
        (Some(c), _) => (c.sha.clone(), c.date.clone(), c.subject.clone()),
        (None, Some(i)) => (i.commit_sha.clone(), i.commit_date.clone(), String::new()),
        (None, None) => bail!(
            "{} has no commit {rev} that diwa knows of{}",
            display(slug),
            fetch_note.map(|n| format!(" ({n})")).unwrap_or_default()
        ),
    };

    // Related: the meaning of this commit's insights, or of its message when it has none.
    let ids: Vec<i64> = insights.iter().map(|r| r.id).collect();
    let mut query = normalized_mean(&db.embeddings_of(&ids)?);
    let mut related_note = None;
    if query.is_none() {
        let text: String = if insights.is_empty() {
            subject.clone()
        } else {
            insights.iter().map(|r| format!("{} {} ", r.title, r.body)).collect()
        };
        if !text.trim().is_empty() {
            match embed::embed(&text) {
                Ok(v) => query = Some(v),
                Err(e) => related_note = Some(format!("related insights unavailable: {e:#}")),
            }
        }
    }
    let related = match &query {
        Some(q) if n > 0 => related(diwa, q, slug, &sha, n)?,
        _ => Vec::new(),
    };

    if json {
        let shown = Shown {
            repo: display(slug),
            commit: sha,
            date,
            subject,
            insights: insights
                .into_iter()
                .map(|r| ShownInsight { origin: origin(&db, &r), insight: r })
                .collect(),
            related,
        };
        println!("{}", serde_json::to_string_pretty(&shown)?);
        return Ok(());
    }

    let clean = sanitize::strip_display_controls;
    let ts: String = date.replace('T', " ").chars().take(19).collect();
    println!("\x1b[1m{} {}\x1b[0m  \x1b[90m{ts}\x1b[0m", display(slug), short(&sha, 7));
    if !subject.is_empty() {
        println!("{}", clean(&subject));
    }
    println!();
    if insights.is_empty() {
        println!("No insights on this commit.\n");
    }
    for (i, r) in insights.iter().enumerate() {
        println!(
            "\x1b[1m{}. [{}] {}\x1b[0m  \x1b[90m{} · id {}\x1b[0m",
            i + 1,
            clean(&r.category),
            clean(&r.title),
            origin(&db, r),
            short(&r.uid, SHORT_ID)
        );
        if !r.body.is_empty() {
            println!("   {}", clean(&r.body));
        }
        if !r.tags.is_empty() {
            println!("   \x1b[90mtags: {}\x1b[0m", clean(&r.tags));
        }
        println!();
    }
    if let Some(note) = related_note {
        eprintln!("diwa: {note}");
    }
    if !related.is_empty() {
        println!("\x1b[90m---\x1b[0m");
        println!("Related, from every indexed repo:\n");
        for (i, r) in related.iter().enumerate() {
            println!(
                "  {}. [{}] {}  \x1b[90m{} {} · id {} · {:.2}\x1b[0m",
                i + 1,
                clean(&r.insight.category),
                clean(&r.insight.title),
                r.repo,
                short(&r.insight.commit_sha, 7),
                short(&r.insight.uid, SHORT_ID),
                r.insight.rank
            );
        }
        println!("\n\x1b[90mHop: diwa show <repo> <sha>\x1b[0m");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean_is_unit_length_and_skips_odd_dimensions() {
        let m = normalized_mean(&[vec![1.0, 0.0], vec![0.0, 1.0], vec![5.0]]).unwrap();
        let len: f32 = m.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((len - 1.0).abs() < 1e-5);
        assert!(normalized_mean(&[]).is_none());
    }

    #[test]
    fn same_commit_accepts_abbreviations_only_when_long_enough() {
        assert!(same_commit("e1598a578d5c", "e1598a5"));
        assert!(!same_commit("e1598a578d5c", "e1598"));
        assert!(!same_commit("e1598a5", "f1598a5"));
    }
}
