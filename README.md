# diwa

*Tagalog: spirit, essence, deeper meaning*

diwa turns your git history into a searchable knowledge base. It uses Claude to read your commits and extract the decisions, learnings, and patterns buried in diffs — then makes them searchable with natural language.

It replaces ADRs, changelogs, and tribal knowledge with something that actually stays current: your git history, understood.

## Quick start

diwa runs as one hub: it keeps bare mirror clones under `~/.diwa/repos/`, polls them, and indexes new commits with Claude.

```bash
diwa add Dorky-Robot/yelo        # mirror, register, index full history
diwa daemon install              # keep every registered repo fresh (poll loop)
diwa search yelo "why did we switch to pull-based rendering"
```

Model and effort come from the environment: `DIWA_MODEL` (default `sonnet`), `DIWA_EFFORT` (default `low`; `off` disables thinking), `DIWA_CLAUDE_TIMEOUT_SECS` (default 300), `DIWA_POLL_SECS` (default 600).

## How it works

```
diwa add org/name
  1. git clone --bare into ~/.diwa/repos/org--name.git (or fetch if present)
  2. Registers the repo in ~/.diwa/repos.json
  3. Backfills by indexing full history

daemon (poll loop)
  every DIWA_POLL_SECS: fetch each mirror, index new commits

indexing (per repo)
  1. Daemon (or `diwa update`) fetches new commits
  2. Daemon picks it up, debounces bursts (e.g. git pull of 50 commits)
  3. Claude reads commits + diffs (pulls PR titles/comments if gh is set up)
  4. Claude extracts structured insights — decision, learning, pattern, etc.
  5. Every 10 new insights (or every 7 days), Claude re-reflects across
     the full insight set + repo ground truth for cross-cutting patterns
  6. BGE-small-en-v1.5 generates embeddings locally (in-process, no server)
  7. Stored in SQLite with FTS5 keyword index + vector column

diwa search
  1. Two-pass vector scan: score all embeddings first without
     deserializing, then load full rows only for top-N
  2. Hybrid rank: 30% BM25 keyword matching + 70% cosine similarity
  3. With --deep: agentic loop, up to 10 steps, with citations
```

Deeper writeups live in [docs/](docs/):

- [Architecture](docs/architecture.md) — push-based indexing, daemon, fleet manager
- [Deep search](docs/deep-search.md) — the agentic research loop
- [Reflections](docs/reflections.md) — level 2 cross-cutting insights
- [Troubleshooting](docs/troubleshooting.md) — PATH shadows, stale hooks, ONNX on Intel Mac

### Why semantic search matters

`"why did they switch programming languages"` finds `"Full language migration from Go to Rust"` despite zero word overlap. The embedding model (BGE-small) learned during training that these concepts live in the same neighborhood of meaning. Combined with keyword matching, both exact and fuzzy queries work.

### What gets extracted

Two levels of insight, generated automatically during indexing:

**Level 1 — per-commit insights:** What happened here, why this decision, what was learned.

**Level 2 — reflections:** Patterns across many commits. The kind of thing a senior dev would say in a retro. Every reflection is verified against the actual code, file tree, and PR data to prevent hallucinations. See [docs/reflections.md](docs/reflections.md).

Categories: `decision`, `pattern`, `learning`, `architecture`, `migration`, `bugfix`, `reflection`

### What it replaces

| Before | After |
|--------|-------|
| ADRs that go stale | Insights derived from what actually happened |
| Changelogs no one reads | Searchable decisions and learnings |
| "Ask Sarah, she was here when we built that" | `diwa search repo "why does auth work this way"` |
| Onboarding docs that drift | Living knowledge base that updates every commit |

## Install

### From source

```bash
git clone https://github.com/Dorky-Robot/diwa.git
cd diwa
cargo install --path .
```

## Commands

```bash
diwa add <org/name>                     Mirror, register and index a GitHub repo
diwa index [dir]                        Index new commits (incremental, idempotent)
diwa reindex [dir]                      Rebuild from scratch
diwa reflect [repo]                     Force-regenerate level 2 reflections

diwa search <repo> "query"              Fast local search (hybrid keyword + vector)
diwa search <repo> "query" --deep       Claude-synthesized answer with citations
diwa search <repo> "query" --json       Machine-readable output for agents
diwa search <repo> "query" -n 5         Limit results

diwa stats <repo>                       Show index info
diwa ls                                 List all indexed repos

diwa daemon status                      Check if the background daemon is running
diwa daemon install                     Install the launchd agent
diwa daemon uninstall                   Remove the launchd agent

diwa update                             Fetch and index all registered repos
```

The `<repo>` argument accepts a short name (`yelo`), full name (`Dorky-Robot/yelo`), or a path (`.`, `/path/to/repo`).

### Indexing is idempotent

`diwa index` is safe to re-run — it queries existing commit SHAs and skips anything already processed. If the daemon was down, the next trigger fills the gap. No duplicates.

## Requirements

- **[Claude Code](https://docs.anthropic.com/en/docs/claude-code)** (required) — Claude reads your commits and diffs and does the hard work of understanding *why* code changed, not just *what* changed. This is a frontier-model task. You need Claude Code installed and authenticated.
- **git**
- **gh CLI** (optional) — adds PR titles, descriptions, and review comments for richer insights

The embedding model (BGE-small-en-v1.5, ~33MB) runs locally in-process via ONNX. No server, no API key. It downloads on first use and caches in `~/.diwa/models/`.

## Storage

Everything lives under `~/.diwa/`:

```
~/.diwa/
  models/                        ONNX embedding model (downloaded once)
  repos/                         bare mirror clones (org--name.git)
  repos.json                     registry of installed repos
  daemon.log                     daemon log
  Dorky-Robot--yelo/
    index.db                     SQLite: insights + FTS5 index + embeddings
  Dorky-Robot--katulong/
    index.db
```

The index is a cache. The source of truth is always git. `diwa reindex` rebuilds from scratch.

## Part of the Dorky Robot stack

Built by [Dorky Robot](https://dorkyrobot.com).

```
kubo (think)  ->  sipag (do)  ->  diwa (remember)
```

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
