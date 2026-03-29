mod browse;
mod claude;
mod db;
mod deep_search;
mod embed;
mod extract;
mod git;
mod github;
mod install;
mod reflect;
mod repo;

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

    /// Browse insights in a scrollable TUI
    Browse {
        /// Repo name or path (default: current dir)
        #[arg(default_value = ".")]
        repo: String,
    },

    /// Install diwa into a repo (adds post-commit hook)
    Init {
        /// Target directory (default: current dir)
        #[arg(default_value = ".")]
        dir: PathBuf,
    },

    /// Remove diwa from a repo
    Uninit {
        /// Target directory (default: current dir)
        #[arg(default_value = ".")]
        dir: PathBuf,
    },

    /// Show index stats
    Stats {
        /// Repo name or path (default: current dir)
        #[arg(default_value = ".")]
        repo: String,
    },
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Index {
            dir,
            max_commits,
            batch_size,
        } => run_index(&dir, max_commits, batch_size, false),
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
        Commands::Init { dir } => run_init(&dir),
        Commands::Uninit { dir } => run_uninit(&dir),
        Commands::Browse { repo } => run_browse(&repo),
        Commands::Stats { repo } => run_stats(&repo),
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
                matches.iter().map(|m| m.replace("--", "/")).collect::<Vec<_>>().join(", ")
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
                fuzzy.iter().map(|m| m.replace("--", "/")).collect::<Vec<_>>().join(", ")
            );
        }
    }

    anyhow::bail!("No indexed repo matching '{repo_arg}'. Run `diwa index` in the repo first.")
}

fn run_index(dir: &Path, max_commits: usize, batch_size: usize, reindex: bool) -> Result<()> {
    let diwa = diwa_dir();
    std::fs::create_dir_all(&diwa)?;

    let resolved = repo::resolve_repo(dir)?;
    let slug = format!("{}--{}", resolved.owner, resolved.name);

    let db = db::IndexDb::open(&diwa, &slug)?;

    if reindex {
        println!("Rebuilding index for {} from scratch...", resolved.full_name);
        db.reset()?;
    }

    let since_sha = if reindex {
        None
    } else {
        db.last_indexed_sha()?
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

    let commits = git::list_commits(&resolved.local_path, since_sha.as_deref(), Some(max_commits))?;
    let commits = git::filter_noise(commits);

    if commits.is_empty() {
        println!("No new commits to index.");
        return Ok(());
    }

    println!("Found {} commits to process.", commits.len());

    // Enrich with GitHub PR data if available.
    let mut commits = commits;
    if github::gh_available() {
        println!("Enriching with GitHub PR data...");
        let _ = github::enrich_with_prs(&mut commits, &resolved.full_name);
    }

    println!("Generating embeddings with BGE-small-en-v1.5 (first run downloads ~33MB model).");

    let batches = git::batch_commits(commits, batch_size);
    let total_batches = batches.len();
    let mut total_insights = 0;

    // Pipeline: while Claude processes batch N+1, embed + store batch N in a background thread.
    let mut pending: Option<std::thread::JoinHandle<anyhow::Result<(Vec<db::Insight>, Vec<Vec<f32>>, String)>>> = None;

    for (i, batch) in batches.iter().enumerate() {
        print!("  Batch {}/{total_batches}...", i + 1);

        // Start Claude extraction (blocking — this is the slow part).
        let insights = extract::extract_insights(batch);
        let last_sha = batch.last().map(|c| c.sha.clone()).unwrap_or_default();

        // While we wait for nothing here, flush the previous batch's embeddings.
        if let Some(handle) = pending.take() {
            match handle.join() {
                Ok(Ok((prev_insights, prev_embeddings, prev_sha))) => {
                    db.insert_insights_with_embeddings(&prev_insights, Some(&prev_embeddings))?;
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
                let texts: Vec<String> = insights_clone.iter().map(|ins| ins.embedding_text()).collect();
                let embeddings = embed::embed_batch(&texts)?;
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
                db.insert_insights_with_embeddings(&prev_insights, Some(&prev_embeddings))?;
                db.set_last_indexed_sha(&prev_sha)?;
                total_insights += prev_insights.len();
            }
            Ok(Err(e)) => eprintln!("  Warning: embedding failed ({e})"),
            Err(_) => eprintln!("  Warning: embedding thread panicked"),
        }
    }

    println!(
        "\n{} insights indexed for {}.",
        total_insights,
        resolved.full_name,
    );

    // Reflection pass: regenerate when enough new insights have accumulated.
    // Threshold: every 10 new Level 1 insights triggers a fresh reflection.
    const REFLECTION_THRESHOLD: usize = 10;
    let level1_count = db.count_level1()?;
    let last_reflection_at = db.last_reflection_count()?;
    let due_for_reflection = level1_count >= 3
        && (last_reflection_at == 0 || level1_count - last_reflection_at >= REFLECTION_THRESHOLD);

    if due_for_reflection {
        // Clear old reflections — they'll be regenerated from all current insights.
        let cleared = db.clear_reflections()?;
        if cleared > 0 {
            println!("Cleared {cleared} stale reflections.");
        }

        let all_insights = db.list_all()?;
        println!("Reflecting on {} insights...", all_insights.len());
        let reflections = reflect::generate_reflections(
            &all_insights,
            &resolved.full_name,
            &resolved.local_path,
            "the indexed history",
        );

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
    }

    println!(
        "\nTotal: {} insights for {}.",
        db.count()?,
        resolved.full_name,
    );

    Ok(())
}

fn run_search(repo_arg: &str, query: &str, limit: usize, json_output: bool, deep: bool) -> Result<()> {
    let diwa = diwa_dir();
    let slug = resolve_slug(repo_arg)?;
    let db = db::IndexDb::open(&diwa, &slug)?;

    // Deep search: Claude drives retrieval and synthesizes an answer.
    if deep {
        let answer = deep_search::deep_search(&db, query)?;
        println!("{answer}");
        return Ok(());
    }

    // Start embedding in background while we prepare the search.
    let has_embeddings = db.count_with_embeddings()? > 0;
    let query_owned = query.to_string();
    let embed_handle = if has_embeddings {
        Some(std::thread::spawn(move || embed::embed(&query_owned).ok()))
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
            if let Some(entry) = commit_index.iter_mut().find(|(sha, _, _)| *sha == short_sha) {
                entry.2.push(i + 1);
            } else {
                let date = r.commit_date.split('T').next().unwrap_or(&r.commit_date);
                commit_index.push((short_sha.clone(), date.to_string(), vec![i + 1]));
            }

            println!(
                "\x1b[1m{}. [{}] {}\x1b[0m  \x1b[90m[{}]\x1b[0m",
                i + 1,
                r.category,
                r.title,
                short_sha,
            );
            println!("   {}", r.body);
            if !r.tags.is_empty() {
                println!("   \x1b[90mtags: {}\x1b[0m", r.tags);
            }
            println!();
        }

        // Commit index footer.
        println!("\x1b[90m---\x1b[0m");
        println!("{} results for: {query}\n", results.len());
        println!("\x1b[90mCommits:\x1b[0m");
        for (sha, date, refs) in &commit_index {
            let ref_list: Vec<String> = refs.iter().map(|r| format!("[{r}]")).collect();
            println!("  \x1b[90m{sha}  {}  cited by {}\x1b[0m", date, ref_list.join(" "));
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
    println!("Reflecting on {} insights for {display_name}...", all_insights.len());

    // Try to get repo path for ground truth. Works if repo_arg is a path or cwd.
    let repo_path = if repo_arg.starts_with('.') || repo_arg.starts_with('/') {
        Some(std::path::PathBuf::from(repo_arg).canonicalize().unwrap_or_default())
    } else {
        // Try resolving from cwd.
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

fn run_init(dir: &Path) -> Result<()> {
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());

    // Install the hook.
    install::install_hook(&dir)?;

    // Run initial index.
    println!("\nRunning initial index...");
    run_index(&dir, 5000, 8, false)?;

    println!("\ndiwa is installed. New commits will be indexed automatically.");
    Ok(())
}

fn run_uninit(dir: &Path) -> Result<()> {
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    install::uninstall_hook(&dir)
}

fn run_browse(repo_arg: &str) -> Result<()> {
    let diwa = diwa_dir();
    let slug = resolve_slug(repo_arg)?;
    let db = db::IndexDb::open(&diwa, &slug)?;
    let insights = db.list_all()?;
    let display_name = slug.replace("--", "/");

    browse::run_browse(insights, &display_name)
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
    println!(
        "  Last indexed:  {}",
        &last_sha[..7.min(last_sha.len())]
    );
    println!(
        "  Database:      {}/{slug}/index.db",
        diwa.display()
    );

    Ok(())
}
