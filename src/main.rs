mod claude;
mod curate;
mod daemon;
mod db;
mod deep_search;
mod embed;
mod extract;
mod git;
mod github;
mod manifest;
mod mirror;
mod reflect;
mod repo;
mod sanitize;
mod spinner;
mod staleness;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "diwa",
    version,
    about = "The deeper meaning behind your git history",
    long_about = "diwa indexes git commits into a searchable knowledge base.\nIt extracts decisions, learnings, and architectural patterns — not just changelogs."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Register org/name on the hub: clone it bare, then index it
    Add {
        /// GitHub repo as org/name
        full_name: String,
        /// Max commits to process
        #[arg(long, default_value = "5000")]
        max_commits: usize,
        /// Commits per Claude batch
        #[arg(long, default_value = "8")]
        batch_size: usize,
        /// Only mirror and register; leave indexing to the daemon
        #[arg(long)]
        no_index: bool,
    },

    /// Index git history (incremental)
    Index {
        /// Target directory (default: current dir)
        #[arg(default_value = ".")]
        dir: PathBuf,

        /// Maximum commits to process
        #[arg(long, default_value = "5000")]
        max_commits: usize,

        /// Commits per Claude batch
        #[arg(long, default_value = "8")]
        batch_size: usize,
    },

    /// Rebuild index from scratch
    Reindex {
        /// Target directory (default: current dir)
        #[arg(default_value = ".")]
        dir: PathBuf,

        /// Maximum commits to process
        #[arg(long, default_value = "5000")]
        max_commits: usize,

        /// Commits per Claude batch
        #[arg(long, default_value = "8")]
        batch_size: usize,
    },

    /// Search indexed git history
    Search {
        /// Repo name or path (e.g. "yelo", "Dorky-Robot/yelo", or a directory path)
        repo: String,

        /// Search query
        query: String,

        /// Output as JSON
        #[arg(long, default_value_t = false)]
        json: bool,

        /// Maximum results
        #[arg(short, default_value = "10")]
        n: usize,

        /// Deep search: Claude synthesizes an answer from multiple queries
        #[arg(long, default_value_t = false)]
        deep: bool,
    },

    /// Force-regenerate reflections (deeper cross-commit insights)
    Reflect {
        /// Repo name or path (default: current dir)
        #[arg(default_value = ".")]
        repo: String,
    },

    /// Fetch and reindex all registered repos now (the daemon does this on a timer)
    Update,

    /// List all indexed repos
    Ls,

    /// Show index stats
    Stats {
        /// Repo name or path (default: current dir)
        #[arg(default_value = ".")]
        repo: String,
    },

    /// Every insight on one commit, and the closest insights from every repo
    Show {
        /// Repo name or path (e.g. "yelo", "Dorky-Robot/yelo", or ".")
        repo: String,
        /// Commit sha (abbreviated is fine)
        sha: String,
        /// How many related insights to list
        #[arg(short, default_value = "8")]
        n: usize,
        /// Output as JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// Add, edit or remove insights by hand
    Insight {
        #[command(subcommand)]
        action: InsightAction,
    },

    /// Manage the background indexing daemon
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum InsightAction {
    /// Add an insight to a commit
    Add {
        /// Repo name or path
        repo: String,
        /// Commit sha (abbreviated is fine)
        sha: String,
        /// One-line title
        title: String,
        /// Longer text; `-` reads it from stdin
        #[arg(long)]
        body: Option<String>,
        /// decision, learning, pattern, architecture, ...
        #[arg(long, default_value = "note")]
        category: String,
        /// Space-separated tags
        #[arg(long, default_value = "")]
        tags: String,
    },
    /// Change an insight, extracted or manual (the change survives reindex)
    Edit {
        /// Insight id, or enough of its start to be unique (`diwa show` lists ids)
        id: String,
        #[arg(long)]
        title: Option<String>,
        /// New body; `-` reads it from stdin
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        category: Option<String>,
        #[arg(long)]
        tags: Option<String>,
        /// Only look in this repo
        #[arg(long)]
        repo: Option<String>,
    },
    /// Remove an insight, extracted or manual (an extracted one stays removed after reindex)
    Rm {
        /// Insight id, or enough of its start to be unique
        id: String,
        /// Only look in this repo
        #[arg(long)]
        repo: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum DaemonAction {
    /// Install the launchd agent so the daemon starts on login
    Install,
    /// Remove the launchd agent
    Uninstall,
    /// Show daemon status (running / loaded)
    Status,
    /// Run the watcher in the foreground (used by launchd)
    Run,
}

fn main() {
    let cli = Cli::parse();

    if let Err(e) = run(cli) {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}

/// What one indexing batch hands back to the pipeline: the insights Claude
/// extracted, their embeddings, and the batch's last SHA (the new watermark).
type BatchPayload = anyhow::Result<(Vec<db::Insight>, Option<Vec<Vec<f32>>>, String)>;

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Index {
            dir,
            max_commits,
            batch_size,
        } => run_index(&dir, max_commits, batch_size, false),
        Commands::Add {
            full_name,
            max_commits,
            batch_size,
            no_index,
        } => run_add(&full_name, max_commits, batch_size, no_index),
        Commands::Reindex {
            dir,
            max_commits,
            batch_size,
        } => run_index(&dir, max_commits, batch_size, true),
        Commands::Search {
            repo,
            query,
            json,
            n,
            deep,
        } => run_search(&repo, &query, n, json, deep),
        Commands::Reflect { repo } => run_reflect(&repo),
        Commands::Update => run_update(),
        Commands::Ls => run_ls(),
        Commands::Stats { repo } => run_stats(&repo),
        Commands::Show { repo, sha, n, json } => {
            curate::show(&diwa_dir(), &resolve_slug(&repo)?, &sha, n, json)
        }
        Commands::Insight { action } => run_insight(action),
        Commands::Daemon { action } => match action {
            DaemonAction::Install => daemon::install(),
            DaemonAction::Uninstall => daemon::uninstall(),
            DaemonAction::Status => daemon::status(),
            DaemonAction::Run => daemon::run(&diwa_dir()),
        },
    }
}

/// Hub entry point: make sure a bare mirror exists and is fresh, register it, index it.
fn run_add(full_name: &str, max_commits: usize, batch_size: usize, no_index: bool) -> Result<()> {
    let diwa = diwa_dir();
    let path = mirror::ensure(&diwa, full_name)?;
    let (owner, name) = mirror::parse_full_name(full_name)?;
    manifest::register_repo(&diwa, &format!("{owner}--{name}"), &path)?;
    if no_index {
        println!("Registered {owner}/{name} -> {}", path.display());
        return Ok(());
    }
    run_index(&path, max_commits, batch_size, false)
}

fn run_insight(action: InsightAction) -> Result<()> {
    let diwa = diwa_dir();
    match action {
        InsightAction::Add { repo, sha, title, body, category, tags } => {
            let slug = resolve_slug(&repo)?;
            curate::add(&diwa, &slug, curate::AddArgs { repo, sha, title, body, category, tags })
        }
        InsightAction::Edit { id, title, body, category, tags, repo } => {
            let only = repo.as_deref().map(resolve_slug).transpose()?;
            let change = db::InsightEdit { category, title, body, tags };
            curate::edit(&diwa, &id, only.as_deref(), change)
        }
        InsightAction::Rm { id, repo } => {
            let only = repo.as_deref().map(resolve_slug).transpose()?;
            curate::rm(&diwa, &id, only.as_deref())
        }
    }
}

fn diwa_dir() -> PathBuf {
    dirs_or_default("DIWA_DIR", ".diwa")
}

fn dirs_or_default(env_key: &str, fallback_name: &str) -> PathBuf {
    if let Ok(dir) = std::env::var(env_key) {
        PathBuf::from(dir)
    } else if let Some(home) = home_dir() {
        home.join(fallback_name)
    } else {
        PathBuf::from(fallback_name)
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Resolve a repo argument to a slug for the index.
///
/// Accepts:
///   "yelo"              → finds matching slug in ~/.diwa/ (e.g. "Dorky-Robot--yelo")
///   "Dorky-Robot/yelo"  → "Dorky-Robot--yelo"
///   "."                 → resolves via git remote
///   "/path/to/repo"     → resolves via git remote
fn resolve_slug(repo_arg: &str) -> Result<String> {
    let diwa = diwa_dir();

    // If it looks like a path (starts with . or /), resolve via git remote.
    if repo_arg.starts_with('.') || repo_arg.starts_with('/') {
        let resolved = repo::resolve_repo(Path::new(repo_arg))?;
        return Ok(format!("{}--{}", resolved.owner, resolved.name));
    }

    // If it contains a slash, treat as owner/repo.
    if repo_arg.contains('/') {
        return Ok(repo_arg.replace('/', "--"));
    }

    // Otherwise, search ~/.diwa/ for a matching slug.
    if diwa.exists() {
        let entries: Vec<String> = std::fs::read_dir(&diwa)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|name| name != "models")
            .collect();

        // Exact suffix match: "yelo" matches "Dorky-Robot--yelo"
        let matches: Vec<&String> = entries
            .iter()
            .filter(|name| {
                name.split("--")
                    .last()
                    .map(|n| n.eq_ignore_ascii_case(repo_arg))
                    .unwrap_or(false)
            })
            .collect();

        if matches.len() == 1 {
            return Ok(matches[0].clone());
        }
        if matches.len() > 1 {
            anyhow::bail!(
                "Ambiguous repo name '{repo_arg}'. Matches: {}",
                matches
                    .iter()
                    .map(|m| m.replace("--", "/"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        // Fuzzy: contains the name anywhere
        let fuzzy: Vec<&String> = entries
            .iter()
            .filter(|name| name.to_lowercase().contains(&repo_arg.to_lowercase()))
            .collect();

        if fuzzy.len() == 1 {
            return Ok(fuzzy[0].clone());
        }
        if fuzzy.len() > 1 {
            anyhow::bail!(
                "Ambiguous repo name '{repo_arg}'. Did you mean one of: {}",
                fuzzy
                    .iter()
                    .map(|m| m.replace("--", "/"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }

    anyhow::bail!("No indexed repo matching '{repo_arg}'. Run `diwa index` in the repo first.")
}

pub(crate) fn run_index(
    dir: &Path,
    max_commits: usize,
    batch_size: usize,
    reindex: bool,
) -> Result<()> {
    let diwa = diwa_dir();
    std::fs::create_dir_all(&diwa)?;

    let resolved = repo::resolve_repo(dir)?;
    let slug = format!("{}--{}", resolved.owner, resolved.name);

    let db = db::IndexDb::open(&diwa, &slug)?;

    if reindex {
        println!(
            "Rebuilding index for {} from scratch...",
            resolved.full_name
        );
        db.reset()?;
    }

    let since_sha = if reindex {
        None
    } else {
        db.last_indexed_sha()?
    };
    // A watermark the repo no longer has (it lived on an unpushed branch or
    // was rewritten away) makes `git log <sha>..HEAD` fail forever. Walk from
    // the start instead; commits that already have insights are skipped below.
    let since_sha = match since_sha {
        Some(sha) if !git::commit_exists(&resolved.local_path, &sha) => {
            println!(
                "Watermark {} is not in this repo; walking its history, skipping commits already indexed.",
                &sha[..7.min(sha.len())]
            );
            None
        }
        other => other,
    };

    if let Some(ref sha) = since_sha {
        println!(
            "Indexing {} incrementally (since {})...",
            resolved.full_name,
            &sha[..7.min(sha.len())]
        );
    } else {
        println!("Indexing {} (full history)...", resolved.full_name);
    }

    let commits = git::list_commits(
        &resolved.local_path,
        since_sha.as_deref(),
        Some(max_commits),
    )?;
    let commits = git::filter_noise(commits);

    // The newest commit git offered us, captured BEFORE dedup filtering. If
    // every commit turns out to be already-indexed we still need this to move
    // the watermark — see the `commits.is_empty()` branch below.
    let newest_seen = commits.last().map(|c| c.sha.clone());

    // Deduplicate: skip commits that already have insights (makes `diwa index` idempotent).
    let existing_shas = db.indexed_shas()?;
    let commits: Vec<_> = if existing_shas.is_empty() {
        commits
    } else {
        let before = commits.len();
        let filtered: Vec<_> = commits
            .into_iter()
            .filter(|c| !existing_shas.contains(&c.sha))
            .collect();
        if before != filtered.len() {
            println!(
                "Skipping {} already-indexed commits.",
                before - filtered.len()
            );
        }
        filtered
    };

    if commits.is_empty() {
        // Everything git offered is already indexed — but the WATERMARK was
        // never advanced past whatever it pointed at, so `last_indexed_sha`
        // kept naming an old commit while the content was fully current.
        //
        // That is not cosmetic. staleness.rs measures the gap from this value,
        // so the banner said "BEHIND HEAD" and told the user to run an index
        // that would immediately answer "No new commits to index" — a warning
        // that cannot be cleared by obeying it. It also made every incremental
        // run re-walk and re-skip the same commits, growing forever.
        //
        // Having verified they are all present, say so.
        if let Some(sha) = newest_seen {
            db.set_last_indexed_sha(&sha)?;
        }
        println!("No new commits to index.");
        return Ok(());
    }

    println!("Found {} commits to process.", commits.len());

    // Enrich with GitHub PR data if available. A failure here degrades the
    // insight quality but shouldn't abort indexing, so we warn and continue.
    let mut commits = commits;
    if github::gh_available() {
        println!("Enriching with GitHub PR data...");
        if let Err(e) = github::enrich_with_prs(&mut commits, &resolved.full_name) {
            eprintln!("Warning: PR enrichment failed ({e}) — indexing without PR context.");
        }
    }

    println!("Generating embeddings with BGE-small-en-v1.5 (first run downloads ~33MB model).");

    let batches = git::batch_commits(commits, batch_size);
    let total_batches = batches.len();
    let mut total_insights = 0;

    // Pipeline: while Claude processes batch N+1, embed + store batch N in a background thread.
    let mut pending: Option<std::thread::JoinHandle<BatchPayload>> = None;

    for (i, batch) in batches.iter().enumerate() {
        print!("  Batch {}/{total_batches}...", i + 1);

        // Start Claude extraction (blocking — this is the slow part).
        let insights = match extract::extract_insights(batch) {
            Ok(i) => i,
            Err(e) => {
                // Claude could not run. Store what earlier batches produced,
                // leave the watermark where it is, and let the next run retry.
                println!();
                if let Some(handle) = pending.take() {
                    if let Ok(Ok((pi, pe, psha))) = handle.join() {
                        db.insert_insights_with_embeddings(&pi, pe.as_deref())?;
                        db.set_last_indexed_sha(&psha)?;
                    }
                }
                anyhow::bail!("insight extraction unavailable, stopping without advancing the watermark: {e:#}");
            }
        };
        let last_sha = batch.last().map(|c| c.sha.clone()).unwrap_or_default();
        // Re-extraction must come back the way a person left it: apply their
        // edits and deletes before the text is embedded.
        let insights = db.curate(insights)?;

        // While we wait for nothing here, flush the previous batch's embeddings.
        if let Some(handle) = pending.take() {
            match handle.join() {
                Ok(Ok((prev_insights, prev_embeddings, prev_sha))) => {
                    db.insert_insights_with_embeddings(&prev_insights, prev_embeddings.as_deref())?;
                    db.set_last_indexed_sha(&prev_sha)?;
                    total_insights += prev_insights.len();
                }
                Ok(Err(e)) => eprintln!("\n  Warning: embedding failed ({e})"),
                Err(_) => eprintln!("\n  Warning: embedding thread panicked"),
            }
        }

        if !insights.is_empty() {
            print!(" {} insights", insights.len());
            // Spawn embedding in background thread.
            let insights_clone = insights.clone();
            let sha = last_sha.clone();
            pending = Some(std::thread::spawn(move || {
                let texts: Vec<String> = insights_clone
                    .iter()
                    .map(|ins| ins.embedding_text())
                    .collect();
                // Claude already spent quota on these; if embedding fails keep
                // the insights (keyword search still works) rather than drop them.
                let embeddings = match embed::embed_batch(&texts) {
                    Ok(e) => Some(e),
                    Err(e) => {
                        eprintln!("\n  Warning: embedding failed, storing without vectors ({e:#})");
                        None
                    }
                };
                Ok((insights_clone, embeddings, sha))
            }));
        } else {
            print!(" (no insights)");
            if !last_sha.is_empty() {
                db.set_last_indexed_sha(&last_sha)?;
            }
        }
        println!();
    }

    // Flush the last pending batch.
    if let Some(handle) = pending.take() {
        match handle.join() {
            Ok(Ok((prev_insights, prev_embeddings, prev_sha))) => {
                db.insert_insights_with_embeddings(&prev_insights, prev_embeddings.as_deref())?;
                db.set_last_indexed_sha(&prev_sha)?;
                total_insights += prev_insights.len();
            }
            Ok(Err(e)) => eprintln!("  Warning: embedding failed ({e})"),
            Err(_) => eprintln!("  Warning: embedding thread panicked"),
        }
    }

    println!(
        "\n{} insights indexed for {}.",
        total_insights, resolved.full_name,
    );

    // Reflection pass: two triggers.
    // 1. Claude decides new material warrants it (event-driven).
    // 2. It's been 7+ days since last reflection (periodic — step back and look at the big picture).
    let level1_count = db.count_level1()?;
    let last_reflection_at = db.last_reflection_count()?;
    let new_insights = db.list_insights_since_count(last_reflection_at)?;
    let existing_reflections = db.list_reflections()?;

    let days_since_reflection = db
        .last_reflection_time()?
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(&t).ok())
        .map(|t| (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_days())
        .unwrap_or(i64::MAX); // Never reflected → treat as overdue.

    let periodic_due = days_since_reflection >= 7 && level1_count >= 3;
    let event_driven =
        !new_insights.is_empty() && reflect::should_reflect(&new_insights, &existing_reflections);

    let should_reflect = periodic_due || event_driven;

    if should_reflect {
        let reason = if periodic_due && !event_driven {
            format!("periodic ({days_since_reflection} days since last reflection)")
        } else if event_driven && !periodic_due {
            "new material warrants it".to_string()
        } else {
            format!("new material + {days_since_reflection} days since last")
        };
        println!("Reflecting: {reason}.");

        let cleared = db.clear_reflections()?;
        if cleared > 0 {
            println!("Cleared {cleared} stale reflections.");
        }

        let all_insights = db.list_all()?;
        println!("Reflecting on {} insights...", all_insights.len());

        let period_label = if periodic_due {
            "the full project history"
        } else {
            "recent changes"
        };

        let reflections = reflect::generate_reflections(
            &all_insights,
            &resolved.full_name,
            &resolved.local_path,
            period_label,
        );
        let reflections = db.curate(reflections)?;

        if !reflections.is_empty() {
            let texts: Vec<String> = reflections.iter().map(|r| r.embedding_text()).collect();
            match embed::embed_batch(&texts) {
                Ok(embeddings) => {
                    db.insert_insights_with_embeddings(&reflections, Some(&embeddings))?;
                }
                Err(_) => {
                    db.insert_insights(&reflections)?;
                }
            }

            println!("{} reflections added:", reflections.len());
            for r in &reflections {
                println!("  - {}", r.title);
            }
        }

        db.set_last_reflection_count(level1_count)?;
        db.set_last_reflection_time()?;
    } else if !new_insights.is_empty() {
        println!("New insights don't warrant reflection yet.");
    }

    println!(
        "\nTotal: {} insights for {}.",
        db.count()?,
        resolved.full_name,
    );

    Ok(())
}

/// Emit the staleness warning for `slug`, if there is one to emit.
///
/// Best-effort by design: no manifest entry, no last-indexed sha, or a repo
/// path that has moved all mean we simply say nothing. A warning we cannot
/// substantiate would train people to ignore the ones we can.
fn warn_if_stale(diwa: &Path, slug: &str, db: &db::IndexDb) {
    let Some(path) = manifest::read_manifest(diwa).get(slug).cloned() else {
        return;
    };
    if !path.exists() {
        return;
    }
    let Ok(Some(sha)) = db.last_indexed_sha() else {
        return;
    };
    // Slugs are stored `owner--name`; show the repo the way people say it.
    let display = slug.replacen("--", "/", 1);
    if let Some(msg) = staleness::warning(&display, &staleness::check(&path, &sha)) {
        eprintln!("{msg}\n");
    }
}

fn run_search(
    repo_arg: &str,
    query: &str,
    limit: usize,
    json_output: bool,
    deep: bool,
) -> Result<()> {
    let diwa = diwa_dir();
    let slug = resolve_slug(repo_arg)?;
    let db = db::IndexDb::open(&diwa, &slug)?;

    // A stale index answers confidently and obsoletely, so say so BEFORE the
    // results — and on stderr, so `--json` stdout stays parseable for agents.
    warn_if_stale(&diwa, &slug, &db);

    // Deep search: agentic research loop — Claude plans, investigates, synthesizes.
    if deep {
        // Try to resolve repo path for git/file access.
        let repo_path = manifest::read_manifest(&diwa)
            .get(&slug)
            .cloned()
            .filter(|p| p.exists());

        let answer = deep_search::deep_search(&db, query, repo_path.as_deref())?;
        println!("{answer}");
        return Ok(());
    }

    // Start embedding in background while we prepare the search.
    let has_embeddings = db.count_with_embeddings()? > 0;
    let query_owned = query.to_string();
    let embed_handle = if has_embeddings {
        Some(std::thread::spawn(move || match embed::embed(&query_owned) {
            Ok(v) => Some(v),
            Err(e) => {
                // Keyword search still answers; say the semantic half is off
                // so a quietly worse result list is not mistaken for the best one.
                eprintln!("diwa: semantic search unavailable, showing keyword matches only: {e:#}");
                None
            }
        }))
    } else {
        None
    };

    let query_embedding = embed_handle.and_then(|h| h.join().ok()).flatten();
    let results = db.search_hybrid(query, query_embedding.as_deref(), limit)?;

    if results.is_empty() {
        if json_output {
            println!("[]");
        } else {
            println!("No results for: {query}");
        }
        return Ok(());
    }

    if json_output {
        println!("{}", serde_json::to_string_pretty(&results)?);
    } else {
        // Collect unique commits for the index.
        let mut commit_index: Vec<(String, String, Vec<usize>)> = Vec::new();

        for (i, r) in results.iter().enumerate() {
            let short_sha = r.commit_sha[..7.min(r.commit_sha.len())].to_string();

            // Track which results reference which commits.
            if let Some(entry) = commit_index
                .iter_mut()
                .find(|(sha, _, _)| *sha == short_sha)
            {
                entry.2.push(i + 1);
            } else {
                let ts: String = r.commit_date.replace('T', " ").chars().take(19).collect();
                commit_index.push((short_sha.clone(), ts, vec![i + 1]));
            }

            // Strip control chars before display — insight text originates
            // from commit messages / PR comments via Claude, which can
            // contain ANSI escapes or other terminal-hijacking sequences.
            let safe_category = sanitize::strip_display_controls(&r.category);
            let safe_title = sanitize::strip_display_controls(&r.title);
            let safe_body = sanitize::strip_display_controls(&r.body);
            let safe_tags = sanitize::strip_display_controls(&r.tags);

            println!(
                "\x1b[1m{}. [{}] {}\x1b[0m  \x1b[90m[{}]\x1b[0m",
                i + 1,
                safe_category,
                safe_title,
                short_sha,
            );
            println!("   {safe_body}");
            if !safe_tags.is_empty() {
                println!("   \x1b[90mtags: {safe_tags}\x1b[0m");
            }
            println!();
        }

        // Commit index footer.
        println!("\x1b[90m---\x1b[0m");
        println!("{} results for: {query}\n", results.len());
        println!("\x1b[90mCommits:\x1b[0m");
        for (sha, date, refs) in &commit_index {
            let ref_list: Vec<String> = refs.iter().map(|r| format!("[{r}]")).collect();
            println!(
                "  \x1b[90m{sha}  {}  cited by {}\x1b[0m",
                date,
                ref_list.join(" ")
            );
        }
    }

    Ok(())
}

fn run_reflect(repo_arg: &str) -> Result<()> {
    let diwa = diwa_dir();
    let slug = resolve_slug(repo_arg)?;
    let display_name = slug.replace("--", "/");
    let db = db::IndexDb::open(&diwa, &slug)?;

    let level1_count = db.count_level1()?;
    if level1_count < 3 {
        println!("Not enough insights to reflect on ({level1_count}). Need at least 3.");
        return Ok(());
    }

    // Clear old reflections.
    let cleared = db.clear_reflections()?;
    if cleared > 0 {
        println!("Cleared {cleared} stale reflections.");
    }

    let all_insights = db.list_all()?;
    println!(
        "Reflecting on {} insights for {display_name}...",
        all_insights.len()
    );

    // Try to get repo path for ground truth. Works if repo_arg is a path or cwd.
    // If canonicalize fails (path doesn't exist), fall through to the no-path
    // branch — silently defaulting to "" (PathBuf::default) would make
    // reflect read files relative to cwd, which is wrong.
    let repo_path = if repo_arg.starts_with('.') || repo_arg.starts_with('/') {
        std::path::PathBuf::from(repo_arg).canonicalize().ok()
    } else {
        repo::resolve_repo(std::path::Path::new("."))
            .ok()
            .map(|r| r.local_path)
    };

    let reflections = match repo_path {
        Some(ref path) => {
            reflect::generate_reflections(&all_insights, &display_name, path, "the indexed history")
        }
        None => {
            // No local path — reflect without ground truth.
            reflect::generate_reflections(
                &all_insights,
                &display_name,
                std::path::Path::new("."),
                "the indexed history",
            )
        }
    };

    let reflections = db.curate(reflections)?;
    if reflections.is_empty() {
        println!("No reflections generated.");
        return Ok(());
    }

    let texts: Vec<String> = reflections.iter().map(|r| r.embedding_text()).collect();
    match embed::embed_batch(&texts) {
        Ok(embeddings) => {
            db.insert_insights_with_embeddings(&reflections, Some(&embeddings))?;
        }
        Err(_) => {
            db.insert_insights(&reflections)?;
        }
    }

    db.set_last_reflection_count(level1_count)?;

    println!("\n{} reflections added:", reflections.len());
    for r in &reflections {
        println!("  - {}", r.title);
    }

    println!("\nTotal: {} insights for {display_name}.", db.count()?);
    Ok(())
}

fn run_update() -> Result<()> {
    let diwa = diwa_dir();

    println!("diwa v{}", env!("CARGO_PKG_VERSION"));

    // Fetch and index all registered repos.
    let repos = manifest::read_manifest(&diwa);

    if repos.is_empty() {
        println!("\nNo repos registered. Run `diwa add org/name` first.");
        return Ok(());
    }

    println!("\nUpdating {} registered repos:\n", repos.len());

    // Track per-repo failures so CI/scripts see a non-zero exit when any
    // repo failed. Printing "index failed" inline is nice for the human
    // watching, but returning Ok(()) would hide real breakage from
    // automation (cron jobs, fleet update scripts).
    let mut failed: Vec<String> = Vec::new();

    for (slug, path) in &repos {
        let display = slug.replace("--", "/");
        print!("  {display}... ");

        if !path.exists() {
            println!("path not found ({}), skipping.", path.display());
            continue;
        }

        if path.extension().is_some_and(|e| e == "git") {
            if let Err(e) = mirror::fetch(path) {
                println!("fetch failed ({e}), skipping.");
                failed.push(display);
                continue;
            }
        }

        // Run incremental index (picks up new commits + latest prompts).
        match run_index(path, 5000, 8, false) {
            Ok(_) => println!("done."),
            Err(e) => {
                println!("index failed ({e}).");
                failed.push(display);
            }
        }
    }

    if failed.is_empty() {
        println!("\nAll repos updated.");
        Ok(())
    } else {
        anyhow::bail!(
            "{} repo(s) failed to update: {}",
            failed.len(),
            failed.join(", ")
        )
    }
}

fn run_ls() -> Result<()> {
    let diwa = diwa_dir();
    let repos = manifest::read_manifest(&diwa);

    if repos.is_empty() {
        println!("No indexed repos. Run `diwa add org/name` to get started.");
        return Ok(());
    }

    let mut entries: Vec<_> = repos.into_iter().collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let home = std::env::var("HOME").unwrap_or_default();
    let shorten = |p: &Path| -> String {
        let s = p.display().to_string();
        if !home.is_empty() {
            if let Some(rest) = s.strip_prefix(&home) {
                return format!("~{rest}");
            }
        }
        s
    };

    for (slug, path) in &entries {
        let display = slug.replace("--", "/");
        let db_path = diwa.join(slug).join("index.db");
        let count = if db_path.exists() {
            db::IndexDb::open(&diwa, slug)
                .and_then(|db| db.count())
                .unwrap_or(0)
        } else {
            0
        };
        println!(
            "  {display}  \x1b[90m{} insights  {}\x1b[0m",
            count,
            shorten(path)
        );
    }

    println!("\n{} repos indexed.", entries.len());
    Ok(())
}

fn run_stats(repo_arg: &str) -> Result<()> {
    let diwa = diwa_dir();
    let slug = resolve_slug(repo_arg)?;
    let display_name = slug.replace("--", "/");

    let db = db::IndexDb::open(&diwa, &slug)?;

    let total = db.count()?;
    let with_embeddings = db.count_with_embeddings()?;
    let last_sha = db.last_indexed_sha()?.unwrap_or_else(|| "(none)".into());

    println!("diwa index for {}", display_name);
    println!("  Insights:      {total}");
    println!("  With vectors:  {with_embeddings}");
    println!("  Last indexed:  {}", &last_sha[..7.min(last_sha.len())]);
    println!("  Database:      {}/{slug}/index.db", diwa.display());

    Ok(())
}
