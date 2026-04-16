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

That's it. `diwa init` indexes your full commit history, installs `post-commit` and `post-merge` git hooks, and starts a background daemon — the knowledge base stays current without you thinking about it. Hooks are sub-10ms; indexing happens off the critical path. Now search:

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

Deep search runs an agentic loop: each step picks one action (search, `git show`, read a file) based on what it's learned so far. See [docs/deep-search.md](docs/deep-search.md) for how the loop works.

### Browse

Scroll through all insights like a dev diary:

```bash
diwa browse yelo
```

## Use diwa as a Claude Code skill

This is the biggest unlock. diwa ships with a [Claude Code skill](skills/diwa/SKILL.md) that teaches Claude to reach for `diwa search` *first* whenever the context is a feature, branch, or area of a repo — before it falls back to `git log --grep` or `grep`. The skill turns diwa from "a CLI you remember to run" into "the default way Claude gets caught up."

Install it globally (all your projects) with one command:

```bash
mkdir -p ~/.claude/skills/diwa
curl -fsSL https://raw.githubusercontent.com/Dorky-Robot/diwa/main/skills/diwa/SKILL.md \
  -o ~/.claude/skills/diwa/SKILL.md
```

Or per-project, drop it in `.claude/skills/diwa/SKILL.md`.

Now any of these phrases make Claude run `diwa search` as step one:

- *"catch me up on the auth refactor"*
- *"why did we switch to pull-based rendering?"*
- *"what's the state of the payments branch?"*
- *"keep working on the dispatch pipeline"*

Claude uses diwa's returned commit SHAs as jumping-off points — `git show <sha>`, then follows clues in the diffs to drive follow-up searches. When independent threads emerge, it fans out parallel subagents to walk each rabbit hole. The skill file describes the full loop; the short version is: **diwa is stage one of context gathering, not the whole job.**

Indexing every repo you work in ahead of time makes Claude *dramatically* faster at onboarding into unfamiliar areas — commit history is the most underused onboarding doc in software, and this makes it searchable by default.

## How it works

```
diwa init
  1. Installs post-commit + post-merge hooks (fire-and-forget, sub-10ms)
  2. Starts a launchd daemon that watches ~/.diwa/queue/ (macOS)
  3. Registers the repo in ~/.diwa/repos.json
  4. Backfills by indexing full history

indexing (per repo)
  1. Hook writes a flag file to ~/.diwa/queue/<repo-slug>
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

### Homebrew (macOS / Linux)

```bash
brew tap Dorky-Robot/tap
brew install diwa
```

If you already have the tap but diwa isn't found, refresh it:

```bash
cd /opt/homebrew/Library/Taps/dorky-robot/homebrew-tap && git pull
brew install diwa
```

The Homebrew formula pulls in `onnxruntime` on Intel Mac, where `ort-sys` has no prebuilt binary. On Apple Silicon and Linux, ORT is statically linked.

### Install script (Linux/macOS)

```bash
curl -fsSL https://raw.githubusercontent.com/Dorky-Robot/diwa/main/install.sh | sh
```

Falls back to `cargo install` if no prebuilt binary is available for your platform.

### Updating

Once installed, diwa can update itself:

```bash
diwa upgrade    # replace the binary with the latest release
diwa update     # refresh hooks + reindex all registered repos
```

Two separate commands by design — binary distribution is independent from refreshing your indexed repos, so `diwa update` doesn't hard-depend on Homebrew.

diwa also checks for updates in the background and shows a hint when a newer version is available.

### Docker

```bash
docker run --rm -v $(pwd):/repo -v ~/.diwa:/root/.diwa \
  ghcr.io/dorky-robot/diwa index /repo

docker run --rm -v $(pwd):/repo -v ~/.diwa:/root/.diwa \
  ghcr.io/dorky-robot/diwa search /repo "your query"
```

The image ships with Claude CLI and `gh` preinstalled.

### From source

```bash
git clone https://github.com/Dorky-Robot/diwa.git
cd diwa
cargo install --path .
```

## Commands

```bash
diwa init [dir]                         Install into a repo (hooks + daemon + full index)
diwa uninit [dir]                       Remove from a repo

diwa index [dir]                        Index new commits (incremental, idempotent)
diwa reindex [dir]                      Rebuild from scratch
diwa reflect [repo]                     Force-regenerate level 2 reflections

diwa search <repo> "query"              Fast local search (hybrid keyword + vector)
diwa search <repo> "query" --deep       Claude-synthesized answer with citations
diwa search <repo> "query" --json       Machine-readable output for agents
diwa search <repo> "query" -n 5         Limit results

diwa browse <repo>                      Scroll through insights in a TUI
diwa stats <repo>                       Show index info
diwa ls                                 List all indexed repos

diwa daemon status                      Check if the background daemon is running
diwa daemon install                     Install the launchd agent (auto-runs on login)
diwa daemon uninstall                   Remove the launchd agent

diwa update                             Refresh hooks + reindex all registered repos
diwa migrate                            Upgrade pre-0.4.0 repos to the daemon model
diwa upgrade                            Update the diwa binary itself
```

The `<repo>` argument accepts a short name (`yelo`), full name (`Dorky-Robot/yelo`), or a path (`.`, `/path/to/repo`).

### Indexing is idempotent

`diwa index` is safe to re-run — it queries existing commit SHAs and skips anything already processed. If a hook run was interrupted or the daemon was down, the next trigger fills the gap. No duplicates.

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
  queue/                         flag files dropped by hooks (picked up by daemon)
  repos.json                     registry of installed repos
  hooks.log                      diagnostic log for hook invocations
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

MIT
