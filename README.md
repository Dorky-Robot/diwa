# diwa

*Tagalog: spirit, essence, deeper meaning*

diwa extracts the deeper meaning from your git history. It reads commits and diffs, uses Claude to extract structured insights — decisions, learnings, patterns, architectural direction — and stores them in a local database with hybrid keyword + semantic vector search.

It replaces ADRs, changelogs, and tribal knowledge with a living, searchable knowledge base derived from what actually happened in your codebase.

## Try it yourself

```bash
# Install
brew tap Dorky-Robot/diwa && brew install diwa

# Index a repo (uses Claude CLI for insight extraction)
cd your-repo
diwa index

# Search with natural language
diwa search "why did they switch programming languages"
```

## Example: Real results from a real repo

Here's diwa run against [Dorky-Robot/yelo](https://github.com/Dorky-Robot/yelo), a 38-commit S3/Glacier CLI that was rewritten from Go to Rust mid-development. Clone it and try yourself:

```bash
git clone https://github.com/Dorky-Robot/yelo.git
cd yelo
diwa index
```

32 meaningful commits produce 17 structured insights. Then search:

```
$ diwa search "why did they switch programming languages"

1. [pattern] Complete TUI rewrite after one day reveals the first attempt was a learning prototype
   The entire TUI was rewritten the same day it was introduced, replacing the initial
   split-pane layout with a tunnels-inspired mode machine architecture. This pattern —
   build a quick prototype to understand the problem space, then immediately rewrite
   with proper architecture — is a deliberate rapid-iteration strategy.
   commit: f7f6a49 | 2026-03-12 | git
   tags: pattern rewrite tui state-machine architecture iteration

2. [migration] Full language migration from Go to Rust for an S3/Glacier CLI
   The entire Go codebase (32 files, ~4000 lines) was deleted in a single commit,
   indicating the Rust rewrite had already reached feature parity. The Go version had
   a cobra-style CLI with a bubbletea TUI, AWS client abstraction, config/state
   management, and Glacier restore support.
   commit: 7142a7b | 2026-03-13 | git
   tags: migration go rust rewrite language-switch s3 glacier

3. [migration] Full rewrite from Go to Rust after reaching TUI complexity ceiling
   After three commits refining the Go/bubbletea TUI, the entire application was
   rewritten in Rust with ratatui. This suggests the Go implementation hit friction —
   likely around async daemon work and the immediate-mode rendering model of ratatui
   being a better fit for the multi-tab layout with background Glacier restore tracking.
   commit: 4934193 | 2026-03-13 | git
   tags: migration go rust rewrite ratatui tui daemon glacier architecture
```

The query "why did they switch programming languages" contains none of the words in the results — no "Go", no "Rust", no "migration". Semantic vector search found them anyway.

Other queries that work:

```bash
diwa search "what failed and was abandoned"
diwa search "architecture decisions"
diwa search "things they tried that didn't work"
```

## How it works

### Indexing

```
git log + diffs
  -> Claude reads commits in batches, extracts structured insights
  -> BGE-small-en-v1.5 generates 384-dim vector embeddings (in-process, no server)
  -> SQLite stores insights with FTS5 full-text index + embedding vectors
```

### Searching: hybrid keyword + semantic

When you search, diwa runs two searches in parallel and merges the results:

**FTS5 keyword search (30% weight)** — SQLite's built-in full-text search with BM25 ranking. Fast, exact. If your query contains words that appear in an insight, this finds them.

**Vector similarity search (70% weight)** — Each insight and your query are converted to 384-dimensional vectors by BGE-small-en-v1.5. Cosine similarity measures how close they are in *meaning space*, not word space.

This is why `"why did they switch programming languages"` finds an insight about `"Full language migration from Go to Rust"` — the words don't overlap at all, but the embedding model learned during training that these concepts (language switching, migration, Go, Rust) live in the same neighborhood of meaning.

The two scores are combined: `0.3 * keyword_score + 0.7 * semantic_score`. This means:
- Exact keyword matches still rank highly
- Vague, natural-language queries work through semantic similarity
- Results that match on both dimensions rank highest

### Three input layers (automatic)

| Layer | Source | When |
|-------|--------|------|
| **git** | Commit messages + diffs | Always |
| **git+gh** | Adds PR titles, descriptions, review comments | When `gh` CLI is authenticated |
| **rich** | Dev-diary-style commit messages with full context | When repo opts into rich commits |

Each layer makes the insights richer, but layer 1 alone works on any repo — even ones with terse commit messages.

### What gets extracted

Each insight has:
- **category**: `decision`, `pattern`, `learning`, `architecture`, `migration`, `bugfix`
- **title**: one-line summary of the deeper meaning (not the commit message)
- **body**: 2-4 sentences on the reasoning, context, and what was learned
- **tags**: for keyword discovery
- **commit ref**: traceable back to the source

### What it replaces

| Before | After |
|--------|-------|
| ADRs that go stale | Insights derived from what actually happened |
| Changelogs no one reads | Searchable decisions and learnings |
| "Ask Sarah, she was here when we built that" | `diwa search "why does auth work this way"` |
| Onboarding docs that drift | Living knowledge base that updates with every `diwa index` |

## Install

### Homebrew (macOS)

```bash
brew tap Dorky-Robot/diwa && brew install diwa
```

### Install script (Linux/macOS)

```bash
curl -fsSL https://raw.githubusercontent.com/Dorky-Robot/diwa/main/install.sh | sh
```

### Docker

```bash
# Index a repo
docker run --rm -v $(pwd):/repo -v ~/.diwa:/root/.diwa ghcr.io/dorky-robot/diwa index /repo

# Search
docker run --rm -v $(pwd):/repo -v ~/.diwa:/root/.diwa ghcr.io/dorky-robot/diwa search "your query" --dir /repo
```

### From source

```bash
cargo install --path .
```

## Commands

```
diwa index [dir]                  Index git history (incremental)
diwa reindex [dir]                Rebuild index from scratch
diwa search "query" [--json]      Search with natural language
diwa stats [dir]                  Show index info
```

### Options

```
diwa index --max-commits 1000     Limit commits to process
diwa index --batch-size 5         Commits per Claude batch
diwa search "query" -n 5          Limit results
diwa search "query" --json        Machine-readable output for agents
```

## Requirements

- **[Claude Code](https://docs.anthropic.com/en/docs/claude-code)** (required) — diwa shells out to `claude` for insight extraction and reflections. Claude reads your commits and diffs and does the hard work of understanding *why* code changed, not just *what* changed. This is a frontier-model task — no local model can do it. You need Claude Code installed and authenticated.
- **git** (obviously)
- **gh CLI** (optional) — when authenticated, diwa pulls PR titles, descriptions, and review comments for richer insights

The embedding model (BGE-small-en-v1.5, ~33MB) runs locally in-process via ONNX — no server, no API key. It's only used for turning insight text into vectors for semantic search, not for generating insights. It downloads automatically on first use and caches in `~/.diwa/models/`.

## Storage

Everything lives under `~/.diwa/`:

```
~/.diwa/
  models/                          ONNX embedding model (downloaded once)
  Dorky-Robot--yelo/
    index.db                       SQLite: insights + FTS5 index + embeddings
  Dorky-Robot--katulong/
    index.db
```

The index is a cache. The source of truth is always git. Run `diwa reindex` to rebuild from scratch.

## Part of the Dorky Robot stack

diwa is built by [Dorky Robot](https://dorkyrobot.com), a software consultancy.

```
kubo (think)  ->  sipag (do)  ->  diwa (remember)
```

## License

MIT
