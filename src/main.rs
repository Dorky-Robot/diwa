mod browse;
mod claude;
mod db;
mod deep_search;
mod embed;
mod extract;
mod git;
mod github;
mod install;
mod manifest;
mod reflect;
mod repo;
mod spinner;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

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

    /// Refresh hooks and reindex all registered repos
    Update,

    /// List all indexed repos
    Ls,

    /// Show index stats
    Stats {
        /// Repo name or path (default: current dir)
        #[arg(default_value = ".")]
        repo: String,
    },

    /// Upgrade diwa to the latest release
    Upgrade,
}

fn main() {
    let cli = Cli::parse();

    // Check for updates in the background while the command runs
    let is_upgrade = matches!(cli.command, Commands::Upgrade);
    let update_rx = if is_upgrade {
        None
    } else {
        spawn_update_check()
    };

    if let Err(e) = run(cli) {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }

    // After the command finishes, show a hint if a newer version was found
    if let Some(rx) = update_rx {
        if let Ok(Some(latest)) = rx.try_recv() {
            eprintln!("\n  diwa v{latest} available — run `diwa upgrade` to update");
        }
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
        Commands::Update => run_update(),
        Commands::Browse { repo } => run_browse(&repo),
        Commands::Ls => run_ls(),
        Commands::Stats { repo } => run_stats(&repo),
        Commands::Upgrade => cmd_upgrade(),
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

fn run_index(dir: &Path, max_commits: usize, batch_size: usize, reindex: bool) -> Result<()> {
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
    let mut pending: Option<
        std::thread::JoinHandle<anyhow::Result<(Vec<db::Insight>, Vec<Vec<f32>>, String)>>,
    > = None;

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
                let texts: Vec<String> = insights_clone
                    .iter()
                    .map(|ins| ins.embedding_text())
                    .collect();
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
            if let Some(entry) = commit_index
                .iter_mut()
                .find(|(sha, _, _)| *sha == short_sha)
            {
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
    let repo_path = if repo_arg.starts_with('.') || repo_arg.starts_with('/') {
        Some(
            std::path::PathBuf::from(repo_arg)
                .canonicalize()
                .unwrap_or_default(),
        )
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

    // Register in manifest so `diwa update` can find it.
    let resolved = repo::resolve_repo(&dir)?;
    let slug = format!("{}--{}", resolved.owner, resolved.name);
    manifest::register_repo(&diwa_dir(), &slug, &dir)?;

    // Run initial index.
    println!("\nRunning initial index...");
    run_index(&dir, 5000, 8, false)?;

    println!("\ndiwa is installed. New commits will be indexed automatically.");
    Ok(())
}

fn run_uninit(dir: &Path) -> Result<()> {
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    install::uninstall_hook(&dir)?;

    // Unregister from manifest.
    if let Ok(resolved) = repo::resolve_repo(&dir) {
        let slug = format!("{}--{}", resolved.owner, resolved.name);
        let _ = manifest::unregister_repo(&diwa_dir(), &slug);
    }

    Ok(())
}

fn run_update() -> Result<()> {
    let diwa = diwa_dir();

    println!("diwa v{}", env!("CARGO_PKG_VERSION"));

    // Refresh hooks and index for all registered repos.
    let repos = manifest::read_manifest(&diwa);

    if repos.is_empty() {
        println!("\nNo repos registered. Run `diwa init` in a repo first.");
        return Ok(());
    }

    println!("\nUpdating {} registered repos:\n", repos.len());

    for (slug, path) in &repos {
        let display = slug.replace("--", "/");
        print!("  {display}... ");

        if !path.exists() {
            println!("path not found ({}), skipping.", path.display());
            continue;
        }

        // Refresh the hook (picks up any changes to the hook script).
        match install::install_hook(path) {
            Ok(_) => {}
            Err(e) => {
                println!("hook update failed ({e}), skipping.");
                continue;
            }
        }

        // Run incremental index (picks up new commits + latest prompts).
        match run_index(path, 5000, 8, false) {
            Ok(_) => println!("done."),
            Err(e) => println!("index failed ({e})."),
        }
    }

    println!("\nAll repos updated.");
    Ok(())
}

fn run_browse(repo_arg: &str) -> Result<()> {
    let diwa = diwa_dir();
    let slug = resolve_slug(repo_arg)?;
    let db = db::IndexDb::open(&diwa, &slug)?;
    let insights = db.list_all()?;
    let display_name = slug.replace("--", "/");

    browse::run_browse(insights, &display_name)
}

fn run_ls() -> Result<()> {
    let diwa = diwa_dir();
    let repos = manifest::read_manifest(&diwa);

    if repos.is_empty() {
        println!("No indexed repos. Run `diwa init` in a git repo to get started.");
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

/// Spawn a background thread to check for a newer diwa release.
/// Returns a receiver that yields `Some(latest_version)` if an update is
/// available, or `None` if already current. Skipped if checked within 24h.
fn spawn_update_check() -> Option<mpsc::Receiver<Option<String>>> {
    let cache_dir = cache_dir();
    let cache_file = cache_dir.join("update-check");

    // Rate-limit: skip if checked within the last 24 hours
    let cache_fresh = std::fs::metadata(&cache_file)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|e| e < std::time::Duration::from_secs(86400));

    if cache_fresh {
        if let Ok(cached) = std::fs::read_to_string(&cache_file) {
            let latest = cached.trim().to_string();
            let current = env!("CARGO_PKG_VERSION");
            if !latest.is_empty() && latest != current {
                let (tx, rx) = mpsc::channel();
                let _ = tx.send(Some(latest));
                return Some(rx);
            }
        }
        return None;
    }

    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        let result = (|| -> Option<String> {
            let output = std::process::Command::new("curl")
                .args([
                    "-fsSL",
                    "--connect-timeout",
                    "3",
                    "--max-time",
                    "5",
                    "https://api.github.com/repos/Dorky-Robot/diwa/releases/latest",
                ])
                .output()
                .ok()?;

            if !output.status.success() {
                return None;
            }

            let body = String::from_utf8_lossy(&output.stdout);
            let tag = body
                .lines()
                .find(|l| l.contains("\"tag_name\""))
                .and_then(|l| l.split('"').nth(3))?;

            let latest = tag.strip_prefix('v').unwrap_or(tag).to_string();

            // Cache the result
            let _ = std::fs::create_dir_all(&cache_dir);
            let _ = std::fs::write(&cache_file, &latest);

            let current = env!("CARGO_PKG_VERSION");
            if latest != current {
                Some(latest)
            } else {
                None
            }
        })();

        let _ = tx.send(result);
    });

    Some(rx)
}

fn cache_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".cache").join("diwa")
}

fn cmd_upgrade() -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");

    eprintln!("Checking for updates...");
    let output = std::process::Command::new("curl")
        .args([
            "-fsSL",
            "https://api.github.com/repos/Dorky-Robot/diwa/releases/latest",
        ])
        .output()?;

    anyhow::ensure!(
        output.status.success(),
        "failed to check for updates — could not reach GitHub"
    );

    let body = String::from_utf8_lossy(&output.stdout);
    let tag = body
        .lines()
        .find(|l| l.contains("\"tag_name\""))
        .and_then(|l| l.split('"').nth(3))
        .ok_or_else(|| anyhow::anyhow!("could not parse latest release tag"))?
        .to_string();

    let latest = tag.strip_prefix('v').unwrap_or(&tag);

    if latest == current {
        eprintln!("Already on the latest version (v{current}).");
        return Ok(());
    }

    eprintln!("Upgrading v{current} → v{latest}...");

    // Detect platform
    let arch = std::env::consts::ARCH;
    let os = std::env::consts::OS;

    let target = match (arch, os) {
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu",
        ("aarch64", "linux") => "aarch64-unknown-linux-gnu",
        ("aarch64", "macos") => "aarch64-apple-darwin",
        _ => anyhow::bail!("unsupported platform: {arch}-{os}"),
    };

    let url =
        format!("https://github.com/Dorky-Robot/diwa/releases/download/{tag}/diwa-{target}.tar.gz");

    // Download and extract to temp dir
    let tmpdir = std::env::temp_dir().join(format!("diwa-upgrade-{}", std::process::id()));
    std::fs::create_dir_all(&tmpdir)?;

    let tarball = tmpdir.join("diwa.tar.gz");
    let dl = std::process::Command::new("curl")
        .args(["-fsSL", &url, "-o"])
        .arg(&tarball)
        .status()?;

    if !dl.success() {
        std::fs::remove_dir_all(&tmpdir).ok();
        anyhow::bail!("failed to download {url}");
    }

    let extract = std::process::Command::new("tar")
        .args(["xzf"])
        .arg(&tarball)
        .arg("-C")
        .arg(&tmpdir)
        .status()?;

    if !extract.success() {
        std::fs::remove_dir_all(&tmpdir).ok();
        anyhow::bail!("failed to extract archive");
    }

    // diwa archives contain the binary directly (no subdirectory)
    let extracted = tmpdir.join("diwa");
    if !extracted.exists() {
        std::fs::remove_dir_all(&tmpdir).ok();
        anyhow::bail!("diwa binary not found in archive");
    }

    // Replace current binary
    let current_exe = std::env::current_exe()?;
    let install_dir = current_exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("could not determine install directory"))?;

    let dest = install_dir.join("diwa");
    let needs_sudo = !is_writable(&dest);

    if needs_sudo {
        eprintln!("Installing to {} (requires sudo)...", install_dir.display());
        let cp = std::process::Command::new("sudo")
            .args(["cp", "-f"])
            .arg(&extracted)
            .arg(&dest)
            .status()?;

        anyhow::ensure!(cp.success(), "failed to install binary (sudo cp failed)");

        std::process::Command::new("sudo")
            .args(["chmod", "+x"])
            .arg(&dest)
            .status()?;
    } else {
        std::fs::copy(&extracted, &dest)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))?;
        }
    }

    std::fs::remove_dir_all(&tmpdir).ok();
    eprintln!("Upgraded to v{latest}.");
    Ok(())
}

/// Check if the parent directory is writable by the current user.
fn is_writable(path: &Path) -> bool {
    // Test the directory, not the file — opening a running binary for writing
    // fails on macOS even if you own it.
    let dir = match path.parent() {
        Some(d) => d,
        None => return false,
    };
    let probe = dir.join(".diwa-write-test");
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&probe)
        .map(|_| {
            std::fs::remove_file(&probe).ok();
            true
        })
        .unwrap_or(false)
}
