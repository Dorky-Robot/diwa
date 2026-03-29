//! Index database — SQLite with FTS5 and vector embeddings.

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::embed::{cosine_similarity, embedding_from_bytes, embedding_to_bytes};

/// A structured insight extracted from one or more commits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Insight {
    pub commit_sha: String,
    pub commit_date: String,
    pub category: String,
    pub title: String,
    pub body: String,
    pub files: Vec<String>,
    pub tags: String,
    pub source_type: String,
    pub pr_number: Option<u64>,
}

impl Insight {
    /// Build the text used for embedding: title + body + tags.
    pub fn embedding_text(&self) -> String {
        format!("{} {} {}", self.title, self.body, self.tags)
    }
}

/// Search result returned by queries.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub id: i64,
    pub commit_sha: String,
    pub commit_date: String,
    pub category: String,
    pub title: String,
    pub body: String,
    pub files: Vec<String>,
    pub tags: String,
    pub source_type: String,
    pub pr_number: Option<u64>,
    pub rank: f64,
}

/// Handle to a per-repo index database.
pub struct IndexDb {
    conn: Connection,
}

impl IndexDb {
    /// Open (or create) the index database for a repo.
    ///
    /// Database lives at `{diwa_dir}/{slug}/index.db`.
    pub fn open(diwa_dir: &Path, repo_slug: &str) -> Result<Self> {
        let dir = diwa_dir.join(repo_slug);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create index dir: {}", dir.display()))?;

        let db_path = dir.join("index.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("failed to open index db: {}", db_path.display()))?;

        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    /// Open an in-memory database (for testing).
    #[cfg(test)]
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS insights (
                id          INTEGER PRIMARY KEY,
                commit_sha  TEXT NOT NULL,
                commit_date TEXT NOT NULL,
                category    TEXT NOT NULL,
                title       TEXT NOT NULL,
                body        TEXT NOT NULL,
                files       TEXT NOT NULL DEFAULT '[]',
                tags        TEXT NOT NULL DEFAULT '',
                source_type TEXT NOT NULL DEFAULT 'git',
                pr_number   INTEGER,
                embedding   BLOB,
                created_at  TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            ",
        )?;

        // Add embedding column to existing databases that don't have it.
        let has_embedding: bool = self.conn.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('insights') WHERE name='embedding'",
            [],
            |row| row.get(0),
        )?;
        if !has_embedding {
            let _ = self
                .conn
                .execute("ALTER TABLE insights ADD COLUMN embedding BLOB", []);
        }

        // FTS5 virtual table.
        let fts_exists: bool = self.conn.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='insights_fts'",
            [],
            |row| row.get(0),
        )?;

        if !fts_exists {
            self.conn.execute_batch(
                "
                CREATE VIRTUAL TABLE insights_fts USING fts5(
                    title, body, tags,
                    content=insights, content_rowid=id
                );
                ",
            )?;
        }

        Ok(())
    }

    /// Insert a batch of insights with optional embeddings.
    pub fn insert_insights(&self, insights: &[Insight]) -> Result<()> {
        self.insert_insights_with_embeddings(insights, None)
    }

    /// Insert insights with pre-computed embeddings.
    pub fn insert_insights_with_embeddings(
        &self,
        insights: &[Insight],
        embeddings: Option<&[Vec<f32>]>,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;

        for (i, insight) in insights.iter().enumerate() {
            let files_json = serde_json::to_string(&insight.files)?;
            let now = chrono::Utc::now().to_rfc3339();

            let embedding_bytes: Option<Vec<u8>> =
                embeddings.and_then(|e| e.get(i)).map(|v| embedding_to_bytes(v));

            tx.execute(
                "INSERT INTO insights (commit_sha, commit_date, category, title, body, files, tags, source_type, pr_number, embedding, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    insight.commit_sha,
                    insight.commit_date,
                    insight.category,
                    insight.title,
                    insight.body,
                    files_json,
                    insight.tags,
                    insight.source_type,
                    insight.pr_number,
                    embedding_bytes,
                    now,
                ],
            )?;

            let rowid = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO insights_fts (rowid, title, body, tags) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![rowid, insight.title, insight.body, insight.tags],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    /// Search using FTS5 keyword matching (BM25 ranking).
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        let sanitized = sanitize_fts_query(query);

        let mut stmt = self.conn.prepare(
            "SELECT i.id, i.commit_sha, i.commit_date, i.category, i.title, i.body,
                    i.files, i.tags, i.source_type, i.pr_number, rank
             FROM insights_fts fts
             JOIN insights i ON i.id = fts.rowid
             WHERE insights_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )?;

        let results = stmt
            .query_map(rusqlite::params![sanitized, limit], |row| {
                let files_str: String = row.get(6)?;
                let files: Vec<String> = serde_json::from_str(&files_str).unwrap_or_default();
                Ok(SearchResult {
                    id: row.get(0)?,
                    commit_sha: row.get(1)?,
                    commit_date: row.get(2)?,
                    category: row.get(3)?,
                    title: row.get(4)?,
                    body: row.get(5)?,
                    files,
                    tags: row.get(7)?,
                    source_type: row.get(8)?,
                    pr_number: row.get(9)?,
                    rank: row.get(10)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .context("failed to read search results")?;

        Ok(results)
    }

    /// Search using vector similarity (cosine distance).
    ///
    /// Requires embeddings to be stored and a query embedding to be provided.
    pub fn search_semantic(
        &self,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        // Two-pass approach: first compute cosine similarity on raw bytes (cheap),
        // then load full rows only for top-N candidates (avoids deserializing
        // JSON files arrays and allocating strings for rows we'll discard).

        // Pass 1: score all embeddings, keep only top-N IDs.
        let mut stmt = self.conn.prepare(
            "SELECT id, embedding FROM insights WHERE embedding IS NOT NULL",
        )?;

        let mut scored: Vec<(i64, f32)> = stmt
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let bytes: Vec<u8> = row.get(1)?;
                Ok((id, bytes))
            })?
            .filter_map(|r| r.ok())
            .map(|(id, bytes)| {
                let stored = embedding_from_bytes(&bytes);
                let sim = cosine_similarity(query_embedding, &stored);
                (id, sim)
            })
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);

        if scored.is_empty() {
            return Ok(Vec::new());
        }

        // Pass 2: load full rows for top-N only.
        let ids: Vec<String> = scored.iter().map(|(id, _)| id.to_string()).collect();
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id, commit_sha, commit_date, category, title, body,
                    files, tags, source_type, pr_number
             FROM insights WHERE id IN ({placeholders})"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::types::ToSql> =
            scored.iter().map(|(id, _)| id as &dyn rusqlite::types::ToSql).collect();

        let mut results_map: std::collections::HashMap<i64, SearchResult> = stmt
            .query_map(params.as_slice(), |row| {
                let id: i64 = row.get(0)?;
                let files_str: String = row.get(6)?;
                let files: Vec<String> = serde_json::from_str(&files_str).unwrap_or_default();
                Ok((id, SearchResult {
                    id,
                    commit_sha: row.get(1)?,
                    commit_date: row.get(2)?,
                    category: row.get(3)?,
                    title: row.get(4)?,
                    body: row.get(5)?,
                    files,
                    tags: row.get(7)?,
                    source_type: row.get(8)?,
                    pr_number: row.get(9)?,
                    rank: 0.0,
                }))
            })?
            .filter_map(|r| r.ok())
            .collect();

        // Reassemble in score order.
        Ok(scored
            .into_iter()
            .filter_map(|(id, sim)| {
                results_map.remove(&id).map(|mut r| {
                    r.rank = sim as f64;
                    r
                })
            })
            .collect())
    }

    /// Hybrid search: combine FTS5 keyword results + vector similarity.
    ///
    /// If `query_embedding` is None, falls back to FTS5 only.
    pub fn search_hybrid(
        &self,
        query: &str,
        query_embedding: Option<&[f32]>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let fts_results = self.search(query, limit * 2)?;

        let semantic_results = match query_embedding {
            Some(emb) => self.search_semantic(emb, limit * 2)?,
            None => return Ok(fts_results.into_iter().take(limit).collect()),
        };

        // Merge: normalize scores, combine with weights.
        // FTS5 rank is negative (lower = better), semantic is 0..1 (higher = better).
        let mut combined: std::collections::HashMap<i64, (SearchResult, f64)> =
            std::collections::HashMap::new();

        // FTS weight: 0.3, Semantic weight: 0.7
        let fts_weight = 0.3;
        let semantic_weight = 0.7;

        // Normalize FTS ranks to 0..1 (invert since lower = better).
        let fts_min = fts_results
            .iter()
            .map(|r| r.rank)
            .fold(f64::INFINITY, f64::min);
        let fts_max = fts_results
            .iter()
            .map(|r| r.rank)
            .fold(f64::NEG_INFINITY, f64::max);
        let fts_range = (fts_max - fts_min).max(1e-6);

        for r in fts_results {
            let normalized = 1.0 - (r.rank - fts_min) / fts_range;
            let score = normalized * fts_weight;
            combined.insert(r.id, (r, score));
        }

        for r in semantic_results {
            let semantic_score = r.rank * semantic_weight;
            combined
                .entry(r.id)
                .and_modify(|(_, score)| *score += semantic_score)
                .or_insert_with(|| (r, semantic_score));
        }

        let mut results: Vec<_> = combined.into_values().collect();
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);

        Ok(results
            .into_iter()
            .map(|(mut r, score)| {
                r.rank = score;
                r
            })
            .collect())
    }

    /// List all insights, ordered by commit date descending.
    pub fn list_all(&self) -> Result<Vec<SearchResult>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, commit_sha, commit_date, category, title, body,
                    files, tags, source_type, pr_number
             FROM insights
             ORDER BY commit_date DESC",
        )?;

        let results = stmt
            .query_map([], |row| {
                let files_str: String = row.get(6)?;
                let files: Vec<String> = serde_json::from_str(&files_str).unwrap_or_default();
                Ok(SearchResult {
                    id: row.get(0)?,
                    commit_sha: row.get(1)?,
                    commit_date: row.get(2)?,
                    category: row.get(3)?,
                    title: row.get(4)?,
                    body: row.get(5)?,
                    files,
                    tags: row.get(7)?,
                    source_type: row.get(8)?,
                    pr_number: row.get(9)?,
                    rank: 0.0,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .context("failed to list insights")?;

        Ok(results)
    }

    pub fn last_indexed_sha(&self) -> Result<Option<String>> {
        let result = self.conn.query_row(
            "SELECT value FROM meta WHERE key = 'last_indexed_sha'",
            [],
            |row| row.get(0),
        );
        match result {
            Ok(sha) => Ok(Some(sha)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn set_last_indexed_sha(&self, sha: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('last_indexed_sha', ?1)",
            [sha],
        )?;
        Ok(())
    }

    /// Get the insight count at which reflections were last generated.
    pub fn last_reflection_count(&self) -> Result<usize> {
        let result = self.conn.query_row(
            "SELECT value FROM meta WHERE key = 'last_reflection_count'",
            [],
            |row| row.get::<_, String>(0),
        );
        match result {
            Ok(s) => Ok(s.parse().unwrap_or(0)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
            Err(e) => Err(e.into()),
        }
    }

    pub fn set_last_reflection_count(&self, count: usize) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('last_reflection_count', ?1)",
            [count.to_string()],
        )?;
        Ok(())
    }

    /// Count non-reflection insights (Level 1 only).
    pub fn count_level1(&self) -> Result<usize> {
        let count: usize = self.conn.query_row(
            "SELECT COUNT(*) FROM insights WHERE source_type != 'reflection'",
            [],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Delete existing reflections (before regenerating).
    pub fn clear_reflections(&self) -> Result<usize> {
        // Remove from FTS first.
        self.conn.execute(
            "DELETE FROM insights_fts WHERE rowid IN (SELECT id FROM insights WHERE source_type = 'reflection')",
            [],
        )?;
        let deleted = self.conn.execute(
            "DELETE FROM insights WHERE source_type = 'reflection'",
            [],
        )?;
        Ok(deleted)
    }

    pub fn count(&self) -> Result<usize> {
        let count: usize = self
            .conn
            .query_row("SELECT COUNT(*) FROM insights", [], |row| row.get(0))?;
        Ok(count)
    }

    /// Count insights that have embeddings.
    pub fn count_with_embeddings(&self) -> Result<usize> {
        let count: usize = self.conn.query_row(
            "SELECT COUNT(*) FROM insights WHERE embedding IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    pub fn reset(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            DROP TABLE IF EXISTS insights_fts;
            DROP TABLE IF EXISTS insights;
            DROP TABLE IF EXISTS meta;
            ",
        )?;
        self.init_schema()?;
        Ok(())
    }
}

fn sanitize_fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|term| {
            let cleaned: String = term.chars().filter(|c| *c != '"').collect();
            if cleaned.is_empty() {
                String::new()
            } else {
                format!("\"{cleaned}\"")
            }
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_insight() -> Insight {
        Insight {
            commit_sha: "abc1234".to_string(),
            commit_date: "2026-03-28T00:00:00Z".to_string(),
            category: "decision".to_string(),
            title: "Switched to pull-based terminal rendering".to_string(),
            body: "Push-based rendering caused garbled text on iPad tab switches due to a race condition between PTY resize and buffer serialization.".to_string(),
            files: vec!["lib/session-manager.js".to_string(), "lib/pull-manager.js".to_string()],
            tags: "rendering architecture ipad".to_string(),
            source_type: "git".to_string(),
            pr_number: Some(417),
        }
    }

    #[test]
    fn test_open_memory() {
        let db = IndexDb::open_memory().unwrap();
        assert_eq!(db.count().unwrap(), 0);
    }

    #[test]
    fn test_insert_and_count() {
        let db = IndexDb::open_memory().unwrap();
        db.insert_insights(&[sample_insight()]).unwrap();
        assert_eq!(db.count().unwrap(), 1);
    }

    #[test]
    fn test_insert_with_embeddings() {
        let db = IndexDb::open_memory().unwrap();
        let insight = sample_insight();
        let embedding = vec![0.1f32; 768];
        db.insert_insights_with_embeddings(&[insight], Some(&[embedding]))
            .unwrap();
        assert_eq!(db.count().unwrap(), 1);
        assert_eq!(db.count_with_embeddings().unwrap(), 1);
    }

    #[test]
    fn test_search() {
        let db = IndexDb::open_memory().unwrap();
        db.insert_insights(&[sample_insight()]).unwrap();

        let results = db.search("pull-based rendering", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].commit_sha, "abc1234");
    }

    #[test]
    fn test_search_by_body() {
        let db = IndexDb::open_memory().unwrap();
        db.insert_insights(&[sample_insight()]).unwrap();

        let results = db.search("garbled text iPad", 10).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_by_tags() {
        let db = IndexDb::open_memory().unwrap();
        db.insert_insights(&[sample_insight()]).unwrap();

        let results = db.search("architecture", 10).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_no_results() {
        let db = IndexDb::open_memory().unwrap();
        db.insert_insights(&[sample_insight()]).unwrap();

        let results = db.search("authentication oauth", 10).unwrap();
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_semantic_search() {
        let db = IndexDb::open_memory().unwrap();
        let insight = sample_insight();
        let embedding = vec![0.5f32, 0.3, 0.1, 0.8];
        db.insert_insights_with_embeddings(&[insight], Some(&[embedding.clone()]))
            .unwrap();

        // Search with similar embedding.
        let query_emb = vec![0.5f32, 0.3, 0.1, 0.7];
        let results = db.search_semantic(&query_emb, 10).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].rank > 0.9); // high similarity
    }

    #[test]
    fn test_hybrid_search_fts_only() {
        let db = IndexDb::open_memory().unwrap();
        db.insert_insights(&[sample_insight()]).unwrap();

        let results = db
            .search_hybrid("pull-based rendering", None, 10)
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_meta_sha() {
        let db = IndexDb::open_memory().unwrap();
        assert_eq!(db.last_indexed_sha().unwrap(), None);

        db.set_last_indexed_sha("abc1234").unwrap();
        assert_eq!(db.last_indexed_sha().unwrap(), Some("abc1234".to_string()));
    }

    #[test]
    fn test_reset() {
        let db = IndexDb::open_memory().unwrap();
        db.insert_insights(&[sample_insight()]).unwrap();
        assert_eq!(db.count().unwrap(), 1);
        db.reset().unwrap();
        assert_eq!(db.count().unwrap(), 0);
    }

    #[test]
    fn test_sanitize_fts_query() {
        assert_eq!(sanitize_fts_query("hello world"), "\"hello\" \"world\"");
        assert_eq!(sanitize_fts_query("pull-based"), "\"pull-based\"");
        assert_eq!(sanitize_fts_query(""), "");
    }

    #[test]
    fn test_list_all() {
        let db = IndexDb::open_memory().unwrap();
        db.insert_insights(&[sample_insight()]).unwrap();
        let all = db.list_all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].title, "Switched to pull-based terminal rendering");
    }

    #[test]
    fn test_list_all_empty() {
        let db = IndexDb::open_memory().unwrap();
        let all = db.list_all().unwrap();
        assert!(all.is_empty());
    }

    #[test]
    fn test_multiple_inserts() {
        let db = IndexDb::open_memory().unwrap();
        let mut a = sample_insight();
        a.commit_sha = "aaa".into();
        a.title = "First insight".into();
        let mut b = sample_insight();
        b.commit_sha = "bbb".into();
        b.title = "Second insight".into();

        db.insert_insights(&[a]).unwrap();
        db.insert_insights(&[b]).unwrap();
        assert_eq!(db.count().unwrap(), 2);
    }

    #[test]
    fn test_search_respects_limit() {
        let db = IndexDb::open_memory().unwrap();
        for i in 0..10 {
            let mut insight = sample_insight();
            insight.commit_sha = format!("sha{i}");
            insight.title = format!("Rendering insight number {i}");
            db.insert_insights(&[insight]).unwrap();
        }
        let results = db.search("rendering", 3).unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_count_with_embeddings_mixed() {
        let db = IndexDb::open_memory().unwrap();
        // One with embedding, one without.
        let mut a = sample_insight();
        a.commit_sha = "aaa".into();
        db.insert_insights_with_embeddings(&[a], Some(&[vec![0.1, 0.2, 0.3]]))
            .unwrap();

        let mut b = sample_insight();
        b.commit_sha = "bbb".into();
        db.insert_insights(&[b]).unwrap();

        assert_eq!(db.count().unwrap(), 2);
        assert_eq!(db.count_with_embeddings().unwrap(), 1);
    }

    #[test]
    fn test_hybrid_search_with_embeddings() {
        let db = IndexDb::open_memory().unwrap();
        let insight = sample_insight();
        let emb = vec![0.5f32, 0.3, 0.1, 0.8];
        db.insert_insights_with_embeddings(&[insight], Some(&[emb]))
            .unwrap();

        let query_emb = vec![0.5f32, 0.3, 0.1, 0.7];
        let results = db
            .search_hybrid("pull-based rendering", Some(&query_emb), 10)
            .unwrap();
        assert_eq!(results.len(), 1);
        // Hybrid score should be > 0 (combined FTS + semantic).
        assert!(results[0].rank > 0.0);
    }

    #[test]
    fn test_semantic_search_empty_db() {
        let db = IndexDb::open_memory().unwrap();
        let results = db.search_semantic(&[0.1, 0.2], 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_reset_clears_meta() {
        let db = IndexDb::open_memory().unwrap();
        db.set_last_indexed_sha("abc").unwrap();
        assert!(db.last_indexed_sha().unwrap().is_some());
        db.reset().unwrap();
        assert!(db.last_indexed_sha().unwrap().is_none());
    }

    #[test]
    fn test_embedding_text() {
        let insight = sample_insight();
        let text = insight.embedding_text();
        assert!(text.contains("pull-based"));
        assert!(text.contains("garbled text"));
        assert!(text.contains("rendering"));
    }

    #[test]
    fn test_open_on_disk() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = IndexDb::open(tmp.path(), "test--repo").unwrap();
        db.insert_insights(&[sample_insight()]).unwrap();
        assert_eq!(db.count().unwrap(), 1);

        // Reopen and verify persistence.
        drop(db);
        let db2 = IndexDb::open(tmp.path(), "test--repo").unwrap();
        assert_eq!(db2.count().unwrap(), 1);
    }
}
