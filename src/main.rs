mod browse;
mod db;
mod deep_search;
mod embed;
mod extract;
mod git;
mod github;
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
        /// Search query
        query: String,

        /// Output as JSON
        #[arg(long, default_value_t = false)]
        json: bool,

        /// Maximum results
        #[arg(short, default_value = "10")]
        n: usize,

        /// Target directory (default: current dir)
        #[arg(long, default_value = ".")]
        dir: PathBuf,

        /// Deep search: Claude synthesizes an answer from multiple queries
        #[arg(long, default_value_t = false)]
        deep: bool,
    },

    /// Browse insights in a scrollable TUI
    Browse {
        /// Target directory (default: current dir)
        #[arg(default_value = ".")]
        dir: PathBuf,
    },

    /// Show index stats
    Stats {
        /// Target directory (default: current dir)
        #[arg(default_value = ".")]
        dir: PathBuf,
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
            query,
            json,
            n,
            dir,
            deep,
        } => run_search(&dir, &query, n, json, deep),
        Commands::Browse { dir } => run_browse(&dir),
        Commands::Stats { dir } => run_stats(&dir),
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

    for (i, batch) in batches.iter().enumerate() {
        print!("  Batch {}/{total_batches}...", i + 1);

        let insights = extract::extract_insights(batch);

        if !insights.is_empty() {
            let texts: Vec<String> = insights.iter().map(|ins| ins.embedding_text()).collect();
            match embed::embed_batch(&texts) {
                Ok(embeddings) => {
                    db.insert_insights_with_embeddings(&insights, Some(&embeddings))?;
                    print!(" {} insights + embeddings", insights.len());
                }
                Err(e) => {
                    eprintln!("\n  Warning: embedding failed ({e}), storing without vectors");
                    db.insert_insights(&insights)?;
                    print!(" {} insights (no vectors)", insights.len());
                }
            }
            total_insights += insights.len();
        } else {
            print!(" (no insights)");
        }
        println!();

        if let Some(last) = batch.last() {
            db.set_last_indexed_sha(&last.sha)?;
        }
    }

    println!(
        "\n{} insights indexed for {}.",
        total_insights,
        resolved.full_name,
    );

    // Reflection pass: generate deeper cross-cutting insights.
    let all_insights = db.list_all()?;
    if all_insights.len() >= 3 {
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
    }

    println!(
        "\nTotal: {} insights for {}.",
        db.count()?,
        resolved.full_name,
    );

    Ok(())
}

fn run_search(dir: &Path, query: &str, limit: usize, json_output: bool, deep: bool) -> Result<()> {
    let diwa = diwa_dir();
    let resolved = repo::resolve_repo(dir)?;
    let slug = format!("{}--{}", resolved.owner, resolved.name);

    let db = db::IndexDb::open(&diwa, &slug)?;

    // Deep search: Claude drives retrieval and synthesizes an answer.
    if deep {
        let answer = deep_search::deep_search(&db, query)?;
        println!("{answer}");
        return Ok(());
    }

    // Hybrid search: FTS5 keywords + vector similarity.
    let query_embedding = if db.count_with_embeddings()? > 0 {
        embed::embed(query).ok()
    } else {
        None
    };

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
        for (i, r) in results.iter().enumerate() {
            println!(
                "\x1b[1m{}. [{}] {}\x1b[0m",
                i + 1,
                r.category,
                r.title
            );
            println!("   {}", r.body);
            println!(
                "   \x1b[90mcommit: {} | {} | {}\x1b[0m",
                &r.commit_sha[..7.min(r.commit_sha.len())],
                r.commit_date.split('T').next().unwrap_or(&r.commit_date),
                r.source_type
            );
            if !r.tags.is_empty() {
                println!("   \x1b[90mtags: {}\x1b[0m", r.tags);
            }
            println!();
        }
        println!("{} results for: {query}", results.len());
    }

    Ok(())
}

fn run_browse(dir: &Path) -> Result<()> {
    let diwa = diwa_dir();
    let resolved = repo::resolve_repo(dir)?;
    let slug = format!("{}--{}", resolved.owner, resolved.name);

    let db = db::IndexDb::open(&diwa, &slug)?;
    let insights = db.list_all()?;

    browse::run_browse(insights, &resolved.full_name)
}

fn run_stats(dir: &Path) -> Result<()> {
    let diwa = diwa_dir();
    let resolved = repo::resolve_repo(dir)?;
    let slug = format!("{}--{}", resolved.owner, resolved.name);

    let db = db::IndexDb::open(&diwa, &slug)?;

    let total = db.count()?;
    let with_embeddings = db.count_with_embeddings()?;
    let last_sha = db.last_indexed_sha()?.unwrap_or_else(|| "(none)".into());

    println!("diwa index for {}", resolved.full_name);
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
