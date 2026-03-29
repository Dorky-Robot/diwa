# diwa

*Tagalog: spirit, essence, deeper meaning*

diwa turns your git history into a searchable knowledge base. It uses Claude to read your commits and extract the decisions, learnings, and patterns buried in diffs — then makes them searchable with natural language.

It replaces ADRs, changelogs, and tribal knowledge with something that actually stays current: your git history, understood.

## Quick start

```bash
brew tap Dorky-Robot/tap && brew install diwa

cd your-repo
diwa init
```

That's it. `diwa init` indexes your full commit history and installs a `post-commit` git hook so future commits are indexed automatically — the knowledge base stays current without you thinking about it. Now search:

```bash
diwa search your-repo "why did we switch to pull-based rendering"
```

## Try it on a real repo

[Dorky-Robot/yelo](https://github.com/Dorky-Robot/yelo) is a 38-commit S3/Glacier CLI that was rewritten from Go to Rust mid-development. It's a good test case because the git history tells a story.

```bash
git clone https://github.com/Dorky-Robot/yelo.git
cd yelo
diwa init
```

32 commits become 17 insights + reflections. Now ask questions:

```
$ diwa search yelo "why did they switch programming languages"

1. [migration] Full language migration from Go to Rust for an S3/Glacier CLI
   The entire Go codebase (32 files, ~4000 lines) was deleted in a single
   commit, indicating the Rust rewrite had already reached feature parity.
   commit: 7142a7b | 2026-03-13 | git
   tags: migration go rust rewrite language-switch s3 glacier

2. [migration] Full rewrite from Go to Rust after reaching TUI complexity ceiling
   After three commits refining the Go/bubbletea TUI, the entire application
   was rewritten in Rust with ratatui. The Go implementation hit friction —
   likely around async daemon work and the immediate-mode rendering model.
   commit: 4934193 | 2026-03-13 | git
   tags: migration go rust rewrite ratatui tui daemon glacier architecture

3. [reflection] The Go codebase was a disposable prototype that discovered the real requirements
   The choreography-over-orchestration principle survived the rewrite intact,
   but the TUI, type system, and daemon architecture were all rebuilt from
   scratch in Rust — the Go version existed to learn what to build.
   commit: 4934193 | reflection
   tags: go rust prototype learning architecture
```

The query contains none of the words in the results — no "Go", no "Rust", no "migration". Semantic vector search found them by meaning, not keywords.

### Deep search

For complex questions, `--deep` researches like a human would — starts with one search, reads the results, follows interesting threads, and spot-checks commits and code when needed:

```
$ diwa search yelo "why did they rewrite in rust" --deep

The Go codebase served as a rapid prototype that discovered the real
requirements. On a single day, the Go TUI went through six iterations —
built, rewritten with a mode machine, bug-fixed, refactored to bubbles.
The very next day, the full Rust rewrite landed (4934193). The bubbles
refactor likely revealed that Go's bubbletea couldn't cleanly support the
daemon and library features the project needed. The Rust rewrite wasn't a
line-by-line port — it systematically replaced stringly-typed patterns with
enums, made the TUI the primary interface, and added features the Go version
never had.

Sources: "The Go codebase was a disposable prototype," "Full rewrite from
Go to Rust," "Full language migration completed"
```

### Browse

Scroll through all insights like a dev diary:

```bash
diwa browse yelo
```

## How it works

```
diwa init
  1. Installs a post-commit hook (indexes new commits automatically)
  2. Reads git history (commits + diffs)
  3. If gh is authenticated, pulls PR descriptions and review comments
  4. Claude extracts structured insights (decisions, learnings, patterns)
  5. Claude reflects across all insights for deeper cross-cutting patterns
  6. BGE-small-en-v1.5 generates vector embeddings (locally, in-process)
  7. Everything stored in SQLite with FTS5 full-text index

diwa search
  1. Hybrid search: 30% BM25 keyword matching + 70% cosine vector similarity
  2. With --deep: Claude reads results, follows threads, spot-checks
     commits and code, and synthesizes a narrative answer with citations
```

### Why semantic search matters

`"why did they switch programming languages"` finds `"Full language migration from Go to Rust"` despite zero word overlap. The embedding model (BGE-small) learned during training that these concepts live in the same neighborhood of meaning. Combined with keyword matching, both exact and fuzzy queries work.

### What gets extracted

Two levels of insight, generated automatically during indexing:

**Level 1 — per-commit insights:** What happened here, why this decision, what was learned.

**Level 2 — reflections:** Patterns across many commits. The kind of thing a senior dev would say in a retro. These are verified against the actual code, file tree, and PR data to prevent hallucinations.

Categories: `decision`, `pattern`, `learning`, `architecture`, `migration`, `bugfix`, `reflection`

### What it replaces

| Before | After |
|--------|-------|
| ADRs that go stale | Insights derived from what actually happened |
| Changelogs no one reads | Searchable decisions and learnings |
| "Ask Sarah, she was here when we built that" | `diwa search repo "why does auth work this way"` |
| Onboarding docs that drift | Living knowledge base that updates every commit |

## Install

### Homebrew (macOS / Linux)

```bash
# Add the Dorky Robot tap (one-time)
brew tap Dorky-Robot/tap

# Install diwa
brew install diwa

# If you already have the tap but diwa isn't found, refresh it:
cd /opt/homebrew/Library/Taps/dorky-robot/homebrew-tap && git pull
brew install diwa
```

### Install script (Linux/macOS)

```bash
curl -fsSL https://raw.githubusercontent.com/Dorky-Robot/diwa/main/install.sh | sh
```

Falls back to `cargo install` if no prebuilt binary is available for your platform.

### Docker

```bash
docker run --rm -v $(pwd):/repo -v ~/.diwa:/root/.diwa ghcr.io/dorky-robot/diwa index /repo
docker run --rm -v $(pwd):/repo -v ~/.diwa:/root/.diwa ghcr.io/dorky-robot/diwa search /repo "your query"
```

### From source

```bash
git clone https://github.com/Dorky-Robot/diwa.git
cd diwa
cargo install --path .
```

## Commands

```bash
diwa init [dir]                         Install into a repo (hook + full index)
diwa uninit [dir]                       Remove from a repo

diwa index [dir]                        Index new commits (incremental)
diwa reindex [dir]                      Rebuild from scratch

diwa search <repo> "query"              Fast local search (hybrid keyword + vector)
diwa search <repo> "query" --deep       Claude-synthesized answer
diwa search <repo> "query" --json       Machine-readable output for agents
diwa search <repo> "query" -n 5         Limit results

diwa browse <repo>                      Scroll through insights in a TUI
diwa stats <repo>                       Show index info
```

The `<repo>` argument accepts a short name (`yelo`), full name (`Dorky-Robot/yelo`), or a path (`.`, `/path/to/repo`).

## Requirements

- **[Claude Code](https://docs.anthropic.com/en/docs/claude-code)** (required) — Claude reads your commits and diffs and does the hard work of understanding *why* code changed, not just *what* changed. This is a frontier-model task. You need Claude Code installed and authenticated.
- **git**
- **gh CLI** (optional) — adds PR titles, descriptions, and review comments for richer insights

The embedding model (BGE-small-en-v1.5, ~33MB) runs locally in-process via ONNX. No server, no API key. It downloads on first use and caches in `~/.diwa/models/`.

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

The index is a cache. The source of truth is always git. `diwa reindex` rebuilds from scratch.

## Part of the Dorky Robot stack

Built by [Dorky Robot](https://dorkyrobot.com).

```
kubo (think)  ->  sipag (do)  ->  diwa (remember)
```

## License

MIT
