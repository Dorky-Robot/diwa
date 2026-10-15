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
    /// Stable id. Extracted insights derive it from where they came from
    /// (see `extracted_uid`); `None` means "derive it on insert".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
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
    pub uid: String,
    pub rank: f64,
}

/// Columns every `SearchResult` query selects, in `result_from_row` order.
const RESULT_COLS: &str =
    "id, commit_sha, commit_date, category, title, body, files, tags, source_type, pr_number, uid";

fn result_from_row(row: &rusqlite::Row, rank: f64) -> rusqlite::Result<SearchResult> {
    let files_str: String = row.get(6)?;
    Ok(SearchResult {
        id: row.get(0)?,
        commit_sha: row.get(1)?,
        commit_date: row.get(2)?,
        category: row.get(3)?,
        title: row.get(4)?,
        body: row.get(5)?,
        files: serde_json::from_str(&files_str).unwrap_or_default(),
        tags: row.get(7)?,
        source_type: row.get(8)?,
        pr_number: row.get(9)?,
        uid: row.get::<_, Option<String>>(10)?.unwrap_or_default(),
        rank,
    })
}

/// 64-bit FNV-1a over the parts, each followed by a separator byte. Ids must
/// be identical across builds and platforms, which `DefaultHasher` does not promise.
fn fnv64(parts: &[&str]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for b in p.bytes().chain(std::iter::once(0xffu8)) {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// The id of an insight produced by extraction or reflection. It depends only
/// on where the insight came from and what it was first called, so the same
/// insight extracted again after a reindex gets the same id, and an edit or
/// delete recorded against that id still applies to it.
pub fn extracted_uid(slug: &str, commit_sha: &str, source_type: &str, title: &str) -> String {
    fnv64(&[slug, commit_sha, source_type, title])
}

/// Source type of insights written by a person rather than extracted.
pub const MANUAL: &str = "manual";

/// Field changes for `IndexDb::edit`. `None` leaves a field as it is.
#[derive(Debug, Default, Clone)]
pub struct InsightEdit {
    pub category: Option<String>,
    pub title: Option<String>,
    pub body: Option<String>,
    pub tags: Option<String>,
}

impl InsightEdit {
    pub fn is_empty(&self) -> bool {
        self.category.is_none() && self.title.is_none() && self.body.is_none() && self.tags.is_none()
    }
}

/// A recorded edit or delete, kept apart from the insights table so that
/// `reset` (reindex) cannot lose it.
#[derive(Debug, Clone, PartialEq)]
pub enum Curation {
    Edited { category: String, title: String, body: String, tags: String },
    Deleted,
}

/// Handle to a per-repo index database.
pub struct IndexDb {
    conn: Connection,
    slug: String,
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

        // The daemon writes while people search and curate; wait for its
        // transaction instead of failing with "database is locked".
        conn.busy_timeout(std::time::Duration::from_secs(15))?;

        let db = Self {
            conn,
            slug: repo_slug.to_string(),
        };
        db.init_schema()?;
        Ok(db)
    }

    /// The `owner--name` slug this database belongs to.
    pub fn slug(&self) -> &str {
        &self.slug
    }

    /// Open an in-memory database (for testing).
    #[cfg(test)]
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let db = Self {
            conn,
            slug: "test--repo".to_string(),
        };
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

            -- Every commit extraction has read (how = 'sent'), or skipped on
            -- purpose ('noise'), whether or not it yielded an insight. With it a
            -- walk over the whole history skips what was read instead of
            -- resending commits that had nothing to say, and the watermark is
            -- only a shortcut, never the record of what was read.
            CREATE TABLE IF NOT EXISTS read_commits (
                sha  TEXT PRIMARY KEY,
                how  TEXT NOT NULL,
                at   TEXT NOT NULL
            );

            -- What people did to insights: 'add' (a manual insight, the
            -- source of truth for it), 'edit' and 'delete' (overrides on an
            -- extracted insight, applied again whenever it is re-extracted).
            -- `reset` never drops this table.
            CREATE TABLE IF NOT EXISTS curations (
                uid         TEXT PRIMARY KEY,
                action      TEXT NOT NULL,
                commit_sha  TEXT NOT NULL,
                commit_date TEXT NOT NULL DEFAULT '',
                category    TEXT,
                title       TEXT,
                body        TEXT,
                tags        TEXT,
                embedding   BLOB,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL
            );
            ",
        )?;

        // Migrate: deduplicate existing rows and add unique index.
        self.migrate_deduplicate()?;

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

        let has_uid: bool = self.conn.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('insights') WHERE name='uid'",
            [],
            |row| row.get(0),
        )?;
        if !has_uid {
            self.conn
                .execute("ALTER TABLE insights ADD COLUMN uid TEXT", [])?;
        }
        self.conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_insights_uid ON insights (uid);
             CREATE INDEX IF NOT EXISTS idx_insights_sha ON insights (commit_sha);",
        )?;
        self.backfill_uids()?;

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

    /// Give rows written before ids existed their id. Cheap when there is
    /// nothing to do: one indexed lookup for NULL.
    fn backfill_uids(&self) -> Result<()> {
        let rows: Vec<(i64, String, String, String)> = {
            let mut stmt = self.conn.prepare(
                "SELECT id, commit_sha, source_type, title FROM insights WHERE uid IS NULL",
            )?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        if rows.is_empty() {
            return Ok(());
        }
        let tx = self.conn.unchecked_transaction()?;
        for (id, sha, source_type, title) in rows {
            tx.execute(
                "UPDATE insights SET uid = ?1 WHERE id = ?2",
                rusqlite::params![extracted_uid(&self.slug, &sha, &source_type, &title), id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Deduplicate existing rows and ensure a unique index exists.
    ///
    /// Keeps the row with the lowest id for each (commit_sha, title, source_type)
    /// triple, deletes the rest, and rebuilds the FTS index.
    fn migrate_deduplicate(&self) -> Result<()> {
        // Check if the unique index already exists — nothing to do.
        let idx_exists: bool = self.conn.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='index' AND name='idx_insights_unique'",
            [],
            |row| row.get(0),
        )?;
        if idx_exists {
            return Ok(());
        }

        // Delete duplicates: keep the row with the smallest id per group.
        let deleted: usize = self.conn.execute(
            "DELETE FROM insights WHERE id NOT IN (
                SELECT MIN(id) FROM insights GROUP BY commit_sha, title, source_type
            )",
            [],
        )?;

        if deleted > 0 {
            // Rebuild FTS to match.
            self.conn.execute("DELETE FROM insights_fts", [])?;
            self.conn.execute(
                "INSERT INTO insights_fts (rowid, title, body, tags)
                 SELECT id, title, body, tags FROM insights",
                [],
            )?;
            eprintln!("diwa: cleaned up {deleted} duplicate insights.");
        }

        // Now create the unique index.
        self.conn.execute(
            "CREATE UNIQUE INDEX idx_insights_unique ON insights (commit_sha, title, source_type)",
            [],
        )?;

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
            let uid = insight.uid.clone().unwrap_or_else(|| {
                extracted_uid(&self.slug, &insight.commit_sha, &insight.source_type, &insight.title)
            });
            // A person deleted this insight; extracting it again must not bring it back.
            if insight.source_type != MANUAL
                && matches!(self.curation(&uid)?, Some(Curation::Deleted))
            {
                continue;
            }
            let files_json = serde_json::to_string(&insight.files)?;
            let now = chrono::Utc::now().to_rfc3339();

            let embedding_bytes: Option<Vec<u8>> = embeddings
                .and_then(|e| e.get(i))
                .map(|v| embedding_to_bytes(v));

            let inserted = tx.execute(
                "INSERT OR IGNORE INTO insights (commit_sha, commit_date, category, title, body, files, tags, source_type, pr_number, embedding, created_at, uid)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
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
                    uid,
                ],
            )?;

            // Only update FTS if a row was actually inserted (not a duplicate).
            if inserted > 0 {
                let rowid = tx.last_insert_rowid();
                tx.execute(
                    "INSERT INTO insights_fts (rowid, title, body, tags) VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![rowid, insight.title, insight.body, insight.tags],
                )?;
            }
        }

        tx.commit()?;
        Ok(())
    }

    /// Search using FTS5 keyword matching (BM25 ranking).
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        let sanitized = sanitize_fts_query(query);

        let mut stmt = self.conn.prepare(
            "SELECT i.id, i.commit_sha, i.commit_date, i.category, i.title, i.body,
                    i.files, i.tags, i.source_type, i.pr_number, i.uid, rank
             FROM insights_fts fts
             JOIN insights i ON i.id = fts.rowid
             WHERE insights_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )?;

        let results = stmt
            .query_map(rusqlite::params![sanitized, limit], |row| {
                result_from_row(row, row.get(11)?)
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
        let mut stmt = self
            .conn
            .prepare("SELECT id, embedding FROM insights WHERE embedding IS NOT NULL")?;

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
        let sql = format!("SELECT {RESULT_COLS} FROM insights WHERE id IN ({placeholders})");

        let mut stmt = self.conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::types::ToSql> = scored
            .iter()
            .map(|(id, _)| id as &dyn rusqlite::types::ToSql)
            .collect();

        let mut results_map: std::collections::HashMap<i64, SearchResult> = stmt
            .query_map(params.as_slice(), |row| {
                let r = result_from_row(row, 0.0)?;
                Ok((r.id, r))
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
            &format!("SELECT {RESULT_COLS} FROM insights ORDER BY commit_date DESC"),
        )?;

        let results = stmt
            .query_map([], |row| result_from_row(row, 0.0))?
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

    /// Get the timestamp of the last reflection.
    pub fn last_reflection_time(&self) -> Result<Option<String>> {
        let result = self.conn.query_row(
            "SELECT value FROM meta WHERE key = 'last_reflection_time'",
            [],
            |row| row.get(0),
        );
        match result {
            Ok(time) => Ok(Some(time)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn set_last_reflection_time(&self) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('last_reflection_time', ?1)",
            [chrono::Utc::now().to_rfc3339()],
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
        let deleted = self
            .conn
            .execute("DELETE FROM insights WHERE source_type = 'reflection'", [])?;
        Ok(deleted)
    }

    /// List only reflection insights.
    pub fn list_reflections(&self) -> Result<Vec<SearchResult>> {
        let mut stmt = self.conn.prepare(
            &format!(
                "SELECT {RESULT_COLS} FROM insights WHERE source_type = 'reflection'
                 ORDER BY commit_date DESC"
            ),
        )?;

        let results = stmt
            .query_map([], |row| result_from_row(row, 0.0))?
            .collect::<Result<Vec<_>, _>>()
            .context("failed to list reflections")?;

        Ok(results)
    }

    /// List Level 1 insights added after a given count threshold.
    pub fn list_insights_since_count(&self, after_count: usize) -> Result<Vec<SearchResult>> {
        let mut stmt = self.conn.prepare(
            &format!(
                "SELECT {RESULT_COLS} FROM insights
                 WHERE source_type != 'reflection'
                 ORDER BY id ASC
                 LIMIT -1 OFFSET ?1"
            ),
        )?;

        let results = stmt
            .query_map([after_count], |row| result_from_row(row, 0.0))?
            .collect::<Result<Vec<_>, _>>()
            .context("failed to list insights since count")?;

        Ok(results)
    }

    /// Return the set of commit SHAs that already have insights in the DB.
    /// Used to make indexing idempotent — skip commits we've already processed.
    pub fn indexed_shas(&self) -> Result<std::collections::HashSet<String>> {
        // A manual insight is not an extraction, so it must not stop the
        // commit from being read. A commit whose extracted insights a person
        // edited or deleted has been read, even if nothing of it is left.
        let mut stmt = self.conn.prepare(
            "SELECT commit_sha FROM insights WHERE source_type NOT IN ('reflection', 'manual')
             UNION
             SELECT commit_sha FROM curations WHERE action IN ('edit', 'delete')",
        )?;
        let shas = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<std::collections::HashSet<_>, _>>()?;
        Ok(shas)
    }

    /// Record commits as read (`how`: "sent" to extraction, or "noise").
    pub fn mark_read(&self, shas: &[String], how: &str) -> Result<()> {
        let at = chrono::Utc::now().to_rfc3339();
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare("INSERT OR IGNORE INTO read_commits (sha, how, at) VALUES (?1, ?2, ?3)")?;
            for s in shas {
                stmt.execute(rusqlite::params![s, how, at])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Every commit read: the read record, and (for stores from before it
    /// existed) every commit with an extracted insight or a person's curation.
    pub fn read_shas(&self) -> Result<std::collections::HashSet<String>> {
        let mut shas = self.indexed_shas()?;
        let mut stmt = self.conn.prepare("SELECT sha FROM read_commits")?;
        for s in stmt.query_map([], |row| row.get::<_, String>(0))? {
            shas.insert(s?);
        }
        Ok(shas)
    }

    pub fn count_read(&self) -> Result<usize> {
        let n: i64 = self.conn.query_row("SELECT COUNT(*) FROM read_commits", [], |r| r.get(0))?;
        Ok(n as usize)
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

    /// The recorded edit or delete for an extracted insight, if any.
    pub fn curation(&self, uid: &str) -> Result<Option<Curation>> {
        let row = self.conn.query_row(
            "SELECT action, category, title, body, tags FROM curations
             WHERE uid = ?1 AND action IN ('edit', 'delete')",
            [uid],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            },
        );
        match row {
            Ok((action, c, t, b, g)) if action == "edit" => Ok(Some(Curation::Edited {
                category: c.unwrap_or_default(),
                title: t.unwrap_or_default(),
                body: b.unwrap_or_default(),
                tags: g.unwrap_or_default(),
            })),
            Ok(_) => Ok(Some(Curation::Deleted)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Apply recorded edits and deletes to freshly extracted insights, before
    /// they are embedded, so a re-extraction comes back the way a person left it.
    pub fn curate(&self, insights: Vec<Insight>) -> Result<Vec<Insight>> {
        let mut out = Vec::with_capacity(insights.len());
        for mut ins in insights {
            let uid = ins.uid.clone().unwrap_or_else(|| {
                extracted_uid(&self.slug, &ins.commit_sha, &ins.source_type, &ins.title)
            });
            match self.curation(&uid)? {
                Some(Curation::Deleted) => continue,
                Some(Curation::Edited { category, title, body, tags }) => {
                    ins.category = category;
                    ins.title = title;
                    ins.body = body;
                    ins.tags = tags;
                }
                None => {}
            }
            ins.uid = Some(uid);
            out.push(ins);
        }
        Ok(out)
    }

    /// Add an insight written by a person. Returns its id.
    pub fn add_manual(&self, insight: &Insight, embedding: Option<&[f32]>) -> Result<String> {
        let now = chrono::Utc::now();
        let uid = insight.uid.clone().unwrap_or_else(|| {
            fnv64(&[
                &self.slug,
                &insight.commit_sha,
                MANUAL,
                &insight.title,
                &now.timestamp_nanos_opt().unwrap_or_default().to_string(),
            ])
        });
        let emb = embedding.map(embedding_to_bytes);
        let now = now.to_rfc3339();
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO curations (uid, action, commit_sha, commit_date, category, title, body, tags, embedding, created_at, updated_at)
             VALUES (?1, 'add', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
            rusqlite::params![uid, insight.commit_sha, insight.commit_date, insight.category,
                              insight.title, insight.body, insight.tags, emb, now],
        )?;
        let mut row = insight.clone();
        row.source_type = MANUAL.to_string();
        row.uid = Some(uid.clone());
        // Same connection, so this row is part of `tx`: both land or neither.
        self.insert_manual_row(&row, embedding)?;
        tx.commit()?;
        Ok(uid)
    }

    fn insert_manual_row(&self, ins: &Insight, embedding: Option<&[f32]>) -> Result<()> {
        let inserted = self.conn.execute(
            "INSERT OR IGNORE INTO insights (commit_sha, commit_date, category, title, body, files, tags, source_type, pr_number, embedding, created_at, uid)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10, ?11)",
            rusqlite::params![ins.commit_sha, ins.commit_date, ins.category, ins.title, ins.body,
                              serde_json::to_string(&ins.files)?, ins.tags, MANUAL,
                              embedding.map(embedding_to_bytes), chrono::Utc::now().to_rfc3339(), ins.uid],
        )?;
        if inserted == 0 {
            anyhow::bail!("this commit already has a manual insight titled '{}'", ins.title);
        }
        let rowid = self.conn.last_insert_rowid();
        self.conn.execute(
            "INSERT INTO insights_fts (rowid, title, body, tags) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![rowid, ins.title, ins.body, ins.tags],
        )?;
        Ok(())
    }

    /// Put manual insights back after `reset` dropped the insights table.
    fn restore_manual(&self) -> Result<()> {
        let rows: Vec<(Insight, Option<Vec<u8>>)> = {
            let mut stmt = self.conn.prepare(
                "SELECT uid, commit_sha, commit_date, category, title, body, tags, embedding
                 FROM curations WHERE action = 'add' ORDER BY created_at",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        Insight {
                            uid: Some(r.get(0)?),
                            commit_sha: r.get(1)?,
                            commit_date: r.get(2)?,
                            category: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                            title: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                            body: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                            tags: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                            files: Vec::new(),
                            source_type: MANUAL.to_string(),
                            pr_number: None,
                        },
                        r.get(7)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        for (ins, emb) in rows {
            let emb = emb.map(|b| embedding_from_bytes(&b));
            self.insert_manual_row(&ins, emb.as_deref())?;
        }
        Ok(())
    }

    /// One insight by its full id.
    pub fn get(&self, uid: &str) -> Result<Option<SearchResult>> {
        let r = self.conn.query_row(
            &format!("SELECT {RESULT_COLS} FROM insights WHERE uid = ?1"),
            [uid],
            |row| result_from_row(row, 0.0),
        );
        match r {
            Ok(r) => Ok(Some(r)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Insights whose id starts with `prefix`.
    pub fn find_by_uid_prefix(&self, prefix: &str) -> Result<Vec<SearchResult>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {RESULT_COLS} FROM insights WHERE substr(uid, 1, length(?1)) = ?1"
        ))?;
        let rows = stmt
            .query_map([prefix], |row| result_from_row(row, 0.0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Change an insight. A manual insight is rewritten in place; an extracted
    /// one gets a durable override, so a reindex extracts it and then applies
    /// the edit again. `embedding` is the vector of the edited text, if one
    /// could be computed.
    pub fn edit(&self, uid: &str, change: &InsightEdit, embedding: Option<&[f32]>) -> Result<SearchResult> {
        let cur = self
            .get(uid)?
            .with_context(|| format!("no insight with id {uid}"))?;
        let category = change.category.clone().unwrap_or(cur.category.clone());
        let title = change.title.clone().unwrap_or(cur.title.clone());
        let body = change.body.clone().unwrap_or(cur.body.clone());
        let tags = change.tags.clone().unwrap_or(cur.tags.clone());
        let emb = embedding.map(embedding_to_bytes);
        let now = chrono::Utc::now().to_rfc3339();

        let tx = self.conn.unchecked_transaction()?;
        if cur.source_type == MANUAL {
            tx.execute(
                "UPDATE curations SET category = ?2, title = ?3, body = ?4, tags = ?5,
                        embedding = COALESCE(?6, embedding), updated_at = ?7
                 WHERE uid = ?1",
                rusqlite::params![uid, category, title, body, tags, emb, now],
            )?;
        } else {
            tx.execute(
                "INSERT INTO curations (uid, action, commit_sha, commit_date, category, title, body, tags, embedding, created_at, updated_at)
                 VALUES (?1, 'edit', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)
                 ON CONFLICT(uid) DO UPDATE SET action = 'edit', category = ?4, title = ?5,
                    body = ?6, tags = ?7, embedding = ?8, updated_at = ?9",
                rusqlite::params![uid, cur.commit_sha, cur.commit_date, category, title, body, tags, emb, now],
            )?;
        }
        // External-content FTS: remove the old tokens while the row still holds them.
        tx.execute(
            "INSERT INTO insights_fts (insights_fts, rowid, title, body, tags) VALUES ('delete', ?1, ?2, ?3, ?4)",
            rusqlite::params![cur.id, cur.title, cur.body, cur.tags],
        )?;
        tx.execute(
            "UPDATE insights SET category = ?2, title = ?3, body = ?4, tags = ?5,
                    embedding = CASE WHEN ?6 IS NULL THEN embedding ELSE ?6 END
             WHERE id = ?1",
            rusqlite::params![cur.id, category, title, body, tags, emb],
        )
        .map_err(|e| anyhow::anyhow!("could not save the edit ({e}); does this commit already have an insight with that title?"))?;
        tx.execute(
            "INSERT INTO insights_fts (rowid, title, body, tags) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![cur.id, title, body, tags],
        )?;
        tx.commit()?;
        self.get(uid)?.context("insight vanished during edit")
    }

    /// Remove an insight. A manual one is gone; an extracted one is
    /// tombstoned, so extracting its commit again does not bring it back.
    pub fn delete(&self, uid: &str) -> Result<SearchResult> {
        let cur = self
            .get(uid)?
            .with_context(|| format!("no insight with id {uid}"))?;
        let now = chrono::Utc::now().to_rfc3339();
        let tx = self.conn.unchecked_transaction()?;
        if cur.source_type == MANUAL {
            tx.execute("DELETE FROM curations WHERE uid = ?1", [uid])?;
        } else {
            tx.execute(
                "INSERT INTO curations (uid, action, commit_sha, commit_date, created_at, updated_at)
                 VALUES (?1, 'delete', ?2, ?3, ?4, ?4)
                 ON CONFLICT(uid) DO UPDATE SET action = 'delete', category = NULL, title = NULL,
                    body = NULL, tags = NULL, embedding = NULL, updated_at = ?4",
                rusqlite::params![uid, cur.commit_sha, cur.commit_date, now],
            )?;
        }
        tx.execute(
            "INSERT INTO insights_fts (insights_fts, rowid, title, body, tags) VALUES ('delete', ?1, ?2, ?3, ?4)",
            rusqlite::params![cur.id, cur.title, cur.body, cur.tags],
        )?;
        tx.execute("DELETE FROM insights WHERE id = ?1", [cur.id])?;
        tx.commit()?;
        Ok(cur)
    }

    /// True when a person has edited this (extracted) insight.
    pub fn is_edited(&self, uid: &str) -> Result<bool> {
        Ok(matches!(self.curation(uid)?, Some(Curation::Edited { .. })))
    }

    /// Every insight on one commit. `sha` may be abbreviated, and so may the
    /// stored sha (older extractions kept whatever prefix Claude echoed).
    pub fn insights_for_commit(&self, sha: &str) -> Result<Vec<SearchResult>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {RESULT_COLS} FROM insights
             WHERE length(commit_sha) >= 7 AND length(?1) >= 7
               AND (substr(commit_sha, 1, length(?1)) = ?1 OR substr(?1, 1, length(commit_sha)) = commit_sha)
             ORDER BY CASE source_type WHEN 'manual' THEN 1 WHEN 'reflection' THEN 2 ELSE 0 END, id"
        ))?;
        let rows = stmt
            .query_map([sha], |row| result_from_row(row, 0.0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Stored vectors of the given insights (those that have one).
    pub fn embeddings_of(&self, ids: &[i64]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::new();
        let mut stmt = self
            .conn
            .prepare("SELECT embedding FROM insights WHERE id = ?1 AND embedding IS NOT NULL")?;
        for id in ids {
            if let Ok(bytes) = stmt.query_row([id], |r| r.get::<_, Vec<u8>>(0)) {
                out.push(embedding_from_bytes(&bytes));
            }
        }
        Ok(out)
    }

    pub fn reset(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            DROP TABLE IF EXISTS insights_fts;
            DROP TABLE IF EXISTS insights;
            DROP TABLE IF EXISTS meta;
            DROP TABLE IF EXISTS read_commits;
            ",
        )?;
        self.init_schema()?;
        self.restore_manual()?;
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
            uid: None,
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
        db.insert_insights_with_embeddings(&[insight], Some(std::slice::from_ref(&embedding)))
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

        let results = db.search_hybrid("pull-based rendering", None, 10).unwrap();
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
    fn test_indexed_shas() {
        let db = IndexDb::open_memory().unwrap();
        assert!(db.indexed_shas().unwrap().is_empty());

        let mut a = sample_insight();
        a.commit_sha = "aaa".into();
        let mut b = sample_insight();
        b.commit_sha = "bbb".into();
        // Two insights from the same commit.
        let mut c = sample_insight();
        c.commit_sha = "aaa".into();
        c.title = "Another insight from same commit".into();

        db.insert_insights(&[a, b, c]).unwrap();
        let shas = db.indexed_shas().unwrap();
        assert_eq!(shas.len(), 2);
        assert!(shas.contains("aaa"));
        assert!(shas.contains("bbb"));
    }

    #[test]
    fn test_indexed_shas_excludes_reflections() {
        let db = IndexDb::open_memory().unwrap();
        let mut insight = sample_insight();
        insight.source_type = "reflection".into();
        insight.commit_sha = "reflection-sha".into();
        db.insert_insights(&[insight]).unwrap();

        let shas = db.indexed_shas().unwrap();
        assert!(shas.is_empty());
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

    #[test]
    fn test_idempotent_insert() {
        let db = IndexDb::open_memory().unwrap();
        let insight = sample_insight();

        // Insert the same insight three times.
        db.insert_insights(std::slice::from_ref(&insight)).unwrap();
        db.insert_insights(std::slice::from_ref(&insight)).unwrap();
        db.insert_insights(&[insight]).unwrap();

        // Should only have one row.
        assert_eq!(db.count().unwrap(), 1);

        // FTS should also return exactly one result.
        let results = db.search("pull-based rendering", 10).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_idempotent_insert_with_embeddings() {
        let db = IndexDb::open_memory().unwrap();
        let insight = sample_insight();
        let emb = vec![0.1f32; 4];

        db.insert_insights_with_embeddings(
            std::slice::from_ref(&insight),
            Some(std::slice::from_ref(&emb)),
        )
        .unwrap();
        db.insert_insights_with_embeddings(&[insight], Some(&[emb]))
            .unwrap();

        assert_eq!(db.count().unwrap(), 1);
        assert_eq!(db.count_with_embeddings().unwrap(), 1);
    }

    #[test]
    fn test_migration_deduplicates_existing() {
        // Simulate a pre-migration database with duplicates.
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("test--repo");
        std::fs::create_dir_all(&db_path).unwrap();
        let conn = rusqlite::Connection::open(db_path.join("index.db")).unwrap();

        // Create the old schema (no unique index).
        conn.execute_batch(
            "CREATE TABLE insights (
                id INTEGER PRIMARY KEY, commit_sha TEXT NOT NULL,
                commit_date TEXT NOT NULL, category TEXT NOT NULL,
                title TEXT NOT NULL, body TEXT NOT NULL,
                files TEXT NOT NULL DEFAULT '[]', tags TEXT NOT NULL DEFAULT '',
                source_type TEXT NOT NULL DEFAULT 'git', pr_number INTEGER,
                embedding BLOB, created_at TEXT NOT NULL
            );
            CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE VIRTUAL TABLE insights_fts USING fts5(
                title, body, tags, content=insights, content_rowid=id
            );",
        )
        .unwrap();

        // Insert duplicates manually (with FTS, matching real-world state).
        for _ in 0..3 {
            conn.execute(
                "INSERT INTO insights (commit_sha, commit_date, category, title, body, source_type, created_at)
                 VALUES ('abc', '2026-01-01', 'decision', 'Same title', 'Same body', 'git', '2026-01-01')",
                [],
            ).unwrap();
            let rowid = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO insights_fts (rowid, title, body, tags) VALUES (?1, 'Same title', 'Same body', '')",
                [rowid],
            ).unwrap();
        }
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM insights", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            3
        );
        drop(conn);

        // Open via IndexDb — migration should deduplicate.
        let db = IndexDb::open(tmp.path(), "test--repo").unwrap();
        assert_eq!(db.count().unwrap(), 1);
    }

    fn extracted(sha: &str, title: &str) -> Insight {
        let mut i = sample_insight();
        i.commit_sha = sha.into();
        i.title = title.into();
        i
    }

    fn manual(sha: &str, title: &str) -> Insight {
        let mut i = extracted(sha, title);
        i.source_type = MANUAL.into();
        i.pr_number = None;
        i.files = vec![];
        i
    }

    /// What `diwa reindex` does to one commit: drop everything, extract again.
    fn reindex(db: &IndexDb, again: Vec<Insight>) {
        db.reset().unwrap();
        let curated = db.curate(again).unwrap();
        db.insert_insights(&curated).unwrap();
    }

    #[test]
    fn extracted_ids_are_stable_across_reindex() {
        let db = IndexDb::open_memory().unwrap();
        db.insert_insights(&[extracted("aaa1111", "One")]).unwrap();
        let before = db.list_all().unwrap()[0].uid.clone();
        assert_eq!(before.len(), 16);
        reindex(&db, vec![extracted("aaa1111", "One")]);
        assert_eq!(db.list_all().unwrap()[0].uid, before);
    }

    #[test]
    fn edit_of_extracted_insight_survives_reindex() {
        let db = IndexDb::open_memory().unwrap();
        db.insert_insights(&[extracted("aaa1111", "Original title")]).unwrap();
        let uid = db.list_all().unwrap()[0].uid.clone();
        let change = InsightEdit {
            title: Some("Better title".into()),
            body: Some("Clearer body about caching".into()),
            ..Default::default()
        };
        db.edit(&uid, &change, None).unwrap();
        assert!(db.is_edited(&uid).unwrap());
        // FTS follows the edit: new words found, old ones gone.
        assert_eq!(db.search("caching", 10).unwrap().len(), 1);
        assert!(db.search("Original", 10).unwrap().is_empty());

        reindex(&db, vec![extracted("aaa1111", "Original title")]);
        let all = db.list_all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].title, "Better title");
        assert_eq!(all[0].uid, uid);
        assert_eq!(db.search("caching", 10).unwrap().len(), 1);
    }

    #[test]
    fn delete_of_extracted_insight_survives_reindex() {
        let db = IndexDb::open_memory().unwrap();
        db.insert_insights(&[extracted("aaa1111", "Keep"), extracted("aaa1111", "Drop")])
            .unwrap();
        let drop_uid = db.find_by_uid_prefix("").unwrap().into_iter()
            .find(|r| r.title == "Drop").unwrap().uid;
        db.delete(&drop_uid).unwrap();
        assert_eq!(db.count().unwrap(), 1);
        assert!(db.search("Drop", 10).unwrap().is_empty());

        // Even a raw insert (no curate) must not bring a tombstoned insight back.
        db.insert_insights(&[extracted("aaa1111", "Drop")]).unwrap();
        assert_eq!(db.count().unwrap(), 1);

        reindex(&db, vec![extracted("aaa1111", "Keep"), extracted("aaa1111", "Drop")]);
        let titles: Vec<_> = db.list_all().unwrap().into_iter().map(|r| r.title).collect();
        assert_eq!(titles, vec!["Keep".to_string()]);
    }

    #[test]
    fn commit_whose_insights_were_all_deleted_counts_as_indexed() {
        let db = IndexDb::open_memory().unwrap();
        db.insert_insights(&[extracted("aaa1111", "Only")]).unwrap();
        let uid = db.list_all().unwrap()[0].uid.clone();
        db.delete(&uid).unwrap();
        assert!(db.indexed_shas().unwrap().contains("aaa1111"));
    }

    #[test]
    fn manual_insight_lifecycle_and_reindex() {
        let db = IndexDb::open_memory().unwrap();
        let uid = db.add_manual(&manual("bbb2222", "Why we pinned the parser"), Some(&[0.1, 0.2])).unwrap();
        let got = db.get(&uid).unwrap().unwrap();
        assert_eq!(got.source_type, MANUAL);
        assert_eq!(db.count_with_embeddings().unwrap(), 1);
        // A manual note must not stop the commit from being extracted.
        assert!(!db.indexed_shas().unwrap().contains("bbb2222"));

        reindex(&db, vec![]);
        let got = db.get(&uid).unwrap().expect("manual insight survives reset");
        assert_eq!(got.title, "Why we pinned the parser");
        assert_eq!(db.count_with_embeddings().unwrap(), 1);
        assert_eq!(db.search("parser", 10).unwrap().len(), 1);

        db.edit(&uid, &InsightEdit { body: Some("Upstream broke semver".into()), ..Default::default() }, None)
            .unwrap();
        reindex(&db, vec![]);
        assert_eq!(db.get(&uid).unwrap().unwrap().body, "Upstream broke semver");

        db.delete(&uid).unwrap();
        reindex(&db, vec![]);
        assert!(db.get(&uid).unwrap().is_none());
        assert_eq!(db.count().unwrap(), 0);
    }

    #[test]
    fn duplicate_manual_title_is_refused_without_orphans() {
        let db = IndexDb::open_memory().unwrap();
        db.add_manual(&manual("ccc3333", "Same"), None).unwrap();
        assert!(db.add_manual(&manual("ccc3333", "Same"), None).is_err());
        reindex(&db, vec![]);
        assert_eq!(db.count().unwrap(), 1);
    }

    #[test]
    fn insights_for_commit_matches_short_and_full_shas() {
        let db = IndexDb::open_memory().unwrap();
        let full = "e1598a578d5c8be95d0d213d5b490acc6f784718";
        db.insert_insights(&[extracted(full, "Full sha"), extracted("e1598a5", "Short sha")])
            .unwrap();
        db.insert_insights(&[extracted("ffff000", "Other")]).unwrap();
        assert_eq!(db.insights_for_commit(full).unwrap().len(), 2);
        assert_eq!(db.insights_for_commit("e1598a5").unwrap().len(), 2);
        assert_eq!(db.insights_for_commit("e1598").unwrap().len(), 0, "too short to trust");
    }

    #[test]
    fn rows_from_before_ids_get_one_on_open() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("o--r");
        std::fs::create_dir_all(&dir).unwrap();
        let conn = rusqlite::Connection::open(dir.join("index.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE insights (
                id INTEGER PRIMARY KEY, commit_sha TEXT NOT NULL, commit_date TEXT NOT NULL,
                category TEXT NOT NULL, title TEXT NOT NULL, body TEXT NOT NULL,
                files TEXT NOT NULL DEFAULT '[]', tags TEXT NOT NULL DEFAULT '',
                source_type TEXT NOT NULL DEFAULT 'git', pr_number INTEGER,
                embedding BLOB, created_at TEXT NOT NULL);
             CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE VIRTUAL TABLE insights_fts USING fts5(title, body, tags, content=insights, content_rowid=id);
             INSERT INTO insights (commit_sha, commit_date, category, title, body, created_at)
               VALUES ('abc1234', 'd', 'decision', 'Old row', 'b', 'c');",
        )
        .unwrap();
        drop(conn);
        let db = IndexDb::open(tmp.path(), "o--r").unwrap();
        let r = &db.list_all().unwrap()[0];
        assert_eq!(r.uid, extracted_uid("o--r", "abc1234", "git", "Old row"));
    }
}
