# Architecture

diwa indexes git history asynchronously via a background daemon and treats everything under `~/.diwa/` as a cache that can be rebuilt from git at any time. This doc walks through how the pieces fit together.

## The shift to push-based indexing

Pre-0.4, every `post-commit` hook ran `diwa index .` synchronously. That was simple but had two problems:

1. **Hook latency.** Loading the ORT model and opening the SQLite DB cost hundreds of ms on every commit. On a busy branch, that's painful.
2. **Missed fast-forward pulls.** `git pull` landing 50 commits only fired one hook — worse, fast-forward merges never fired `post-commit` at all. When this was discovered on the `katulong` repo, 133 commits had silently gone unindexed.

0.4.0 rewrote indexing as push-based:

```
    git commit                git pull (fast-forward)
         │                            │
         ▼                            ▼
    post-commit               post-merge
         │                            │
         └───────── diwa enqueue ─────┘
                         │
                         ▼
               ~/.diwa/queue/<repo-slug>   (flag file)
                         │
                         ▼
               launchd daemon (fs watcher)
                         │
                         ▼
                  diwa index <repo>
```

Hooks are now ~10 lines of shell that drop a flag file into `~/.diwa/queue/`. They do no work beyond that — sub-10ms on a warm disk. The daemon watches the queue, debounces bursts (a `git pull` landing N commits collapses into one indexing pass), and runs the heavy ORT-loading, Claude-calling, SQLite-writing logic off the critical path.

Both `post-commit` *and* `post-merge` are installed, which is what fixed the fast-forward gap.

## The daemon

On macOS, the daemon is a launchd LaunchAgent (`com.dorky-robot.diwa`) started automatically on login. Install/uninstall/status are exposed as subcommands:

```bash
diwa daemon install     # writes ~/Library/LaunchAgents/<plist>
diwa daemon status      # checks launchctl + process state
diwa daemon uninstall
```

On Linux, launchd obviously doesn't exist — run `diwa daemon run` from a systemd user unit (or any supervisor) pointing at the same queue directory.

The daemon:

1. On startup, sweeps `~/.diwa/queue/` for any flag files that accumulated while it was down.
2. Uses `notify::Watcher` to get fs events for new flags.
3. On event, drains any burst for 200ms to coalesce, then processes all pending flags.
4. Has a 5-minute safety sweep in case fs events were dropped (fs watching isn't 100% reliable across all filesystems).
5. Logs to `~/.diwa/daemon.log`.

The daemon deletes the flag file *before* running the indexer — so if new commits arrive during indexing, a fresh flag is created and caught on the next pass. No commits are lost.

## Idempotent indexing

`diwa index` queries the set of already-indexed commit SHAs before processing anything. Any commit already in the DB is skipped. This means:

- Re-running `diwa index` fills gaps from partial or failed runs without duplicating.
- Daemon crashes are safe — on restart, the initial queue sweep picks up where things left off.
- `diwa update` (bulk refresh) just re-runs `diwa index` on every registered repo.

Hook logs go to `~/.diwa/hooks.log`. That's not cosmetic — silent hook failures are undiagnosable without it, and the iteration of hook bugs from 0.3.5 through 0.4.3 would have been impossible to fix otherwise.

## The fleet model

Every `diwa init` registers the repo in `~/.diwa/repos.json` — a ~20-line JSON manifest tracking every repo that has diwa installed. Small file, big effect: it turns diwa from a per-repo tool into a fleet manager.

Once repos are registered:

- `diwa update` iterates them all — refreshes hooks (important when hook format changes between versions), reindexes incrementals, repairs stale binaries on PATH.
- `diwa ls` lists them with insight counts.
- `diwa migrate` upgrades every registered repo to the current hook/daemon shape when they evolve.

The manifest is also why `diwa upgrade` had to grow careful PATH handling — if an older `diwa` sits earlier on PATH (common after a prior `cargo install` to `~/.local/bin`), every hook in every registered repo silently breaks. `diwa update` probes the shadowing binary and renames it if it's under `$HOME`.

## The indexing pipeline

For each batch of commits:

1. **Gather context** — commit, full diff, author, date, and if `gh` is authenticated, the PR title/description/review comments.
2. **Extract (Claude)** — Claude reads the context and emits structured insights: `{category, title, body, tags, commit_sha}`. Categories are `decision`, `pattern`, `learning`, `architecture`, `migration`, `bugfix`.
3. **Embed (local)** — BGE-small-en-v1.5 via ONNX generates a 384-dim vector for each insight. Runs in-process, no network.
4. **Persist** — SQLite insert with an FTS5 virtual table for keyword search and a `BLOB` column for the embedding.

Steps 2 and 3 run as a pipeline. Claude is the bottleneck (~5s per batch) and embedding is fast (~50ms), so embedding of batch N happens on a background thread while Claude is still processing batch N+1. Claude calls are also deduplicated through `src/claude.rs` — the three modules that talk to Claude (extract, reflect, deep_search) all share one code path for invocation and JSON parsing.

## The search pipeline

Two-pass by design:

1. **Cheap scan:** a SQL query pulls only `(id, embedding)` tuples, computes cosine similarity against the query embedding, and keeps the top-N. No JSON deserialization, no string allocation for rows that are going to be discarded. Critical as the index grows.
2. **Load full rows:** only for the top-N winners. Plus a BM25 keyword pass over the FTS5 index.
3. **Hybrid rank:** 30% BM25 + 70% cosine. That weighting favors semantic matches but still rewards exact keyword hits when they exist.

`diwa search --deep` wraps this with an agentic loop — see [deep-search.md](deep-search.md).

`diwa search --json` skips the human-readable formatting and emits structured output. Used by agents and by the diwa Claude Code skill for downstream tooling.

## Storage layout

```
~/.diwa/
  models/                        ONNX embedding model (downloaded once, ~33MB)
  queue/                         flag files dropped by hooks (owned by daemon)
  repos.json                     fleet registry
  hooks.log                      hook diagnostic log
  daemon.log                     daemon log
  Dorky-Robot--yelo/
    index.db                     SQLite: insights + FTS5 + embeddings
```

SQLite schema (simplified):

```sql
CREATE TABLE insights (
  id INTEGER PRIMARY KEY,
  commit_sha TEXT NOT NULL,
  category TEXT NOT NULL,
  title TEXT NOT NULL,
  body TEXT NOT NULL,
  tags TEXT NOT NULL,       -- space-delimited for FTS
  pr_number INTEGER,
  embedding BLOB NOT NULL,  -- f32 vector, little-endian
  created_at TEXT NOT NULL
);

CREATE VIRTUAL TABLE insights_fts USING fts5(title, body, tags, content='insights');
```

The index is a cache. `diwa reindex` rebuilds it from scratch. `diwa uninit` removes the repo from the manifest but leaves the DB in place — it's cheap insurance against accidental uninit.
