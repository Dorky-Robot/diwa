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

```bash
brew install dorky-robot/tap/diwa
```

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
diwa show <repo> <sha>                  Every insight on a commit + related ones from all repos

diwa insight add <repo> <sha> "title"   Add an insight by hand (--body, --category, --tags)
diwa insight edit <id> --title ...      Change any insight; survives reindex
diwa insight rm <id>                    Remove any insight; stays removed after reindex

diwa remote set <host>                  Use the store on another machine over ssh
diwa remote unset                       Use the local store again

diwa daemon status                      Check if the background daemon is running
diwa daemon install                     Install the launchd agent
diwa daemon uninstall                   Remove the launchd agent

diwa update                             Fetch and index all registered repos
```

The `<repo>` argument accepts a short name (`yelo`), full name (`Dorky-Robot/yelo`), or a path (`.`, `/path/to/repo`).

### Indexing is idempotent

`diwa index` is safe to re-run — it queries existing commit SHAs and skips anything already processed. If the daemon was down, the next trigger fills the gap. No duplicates.

## Insights by hand

Most insights are extracted, but some things are only known to the person who was there. Add them to the commit they belong to, and correct or remove what extraction got wrong:

```bash
diwa insight add yelo 3f9a2c1 "Pull rendering only works because the PTY resize is debounced" \
    --body "Without the 50ms debounce the race comes back on iPad." --category learning --tags "rendering ipad"
diwa show yelo 3f9a2c1                    # every insight on the commit, with ids, plus related ones
diwa insight edit 5d0b8ddf --title "..."  # any unique start of an id; works on extracted insights too
diwa insight rm 9a3759f8
```

`--body -` reads the body from stdin. An edit or removal of an extracted insight is kept as an override, so `diwa reindex` extracts the commit again and then applies it; manual insights are restored after a reindex. All of this lives in the index (`curations` table), so back the index up.

`diwa show <repo> <sha>` lists each insight with how it came to be (extracted, edited, reflection, manual) and then the nearest insights from every indexed repo, with repo and sha, so the next `diwa show` is one copy-paste away.

## One store, many machines

Run diwa with its store on one machine (the hub) and point the others at it:

```bash
diwa remote set hub-host                  # any ssh destination: config alias, tailnet name, user@host
diwa remote set hub-host --bin '~/.local/bin/diwa' --dir '/srv/diwa'     # override what the hub advertises
diwa remote                               # where is the store, and does it answer
diwa remote unset                         # back to the local store
```

The hub's daemon advertises its binary and data dir in `~/.diwa/hub.json` when it starts (`diwa remote whereami` prints it), so a plain `remote set <host>` finds the store wherever it lives; with no daemon there, the defaults are `diwa` on the hub's PATH and its `~/.diwa`.

After `remote set`, `search`, `ls`, `stats`, `show` and `insight add|edit|rm` run on the hub over ssh and print exactly what they print there; local paths (`diwa search . "..."`) are turned into `org/name` first. Indexing commands are refused: the hub indexes. The setting is `~/.diwa/remote.json`; `DIWA_REMOTE=<host>` overrides it for one shell and `DIWA_REMOTE=local` ignores it. ssh must work non-interactively (keys, `BatchMode`).

## Running the hub

- **Data dir:** `DIWA_DIR` (default `~/.diwa`). **Label:** `com.dorky-robot.diwa-hub` (`DIWA_LABEL` overrides). The plist is `Nice 10`, `ProcessType Background`, and carries the `DIWA_*` settings it was installed with. Extraction is strictly sequential: one `claude` call at a time.
- **Install/restart:** `diwa daemon install`. A first install bootstraps; a repeat with an unchanged plist is `launchctl kickstart -k`; a changed plist reloads in a detached shell. It never runs a foreground bootout then bootstrap, which would cut an ssh session running it.
- **Model:** `DIWA_MODEL` (default `sonnet`), `DIWA_EFFORT` (default `low`). `claude -p` runs with `--setting-sources project` and an explicit `--model`, from an empty working dir under the data dir, so user settings and hooks do not apply.
- **Throttle:** `DIWA_SWEEP_MAX_COMMITS` caps commits per repo per sweep, so a backfill spends in bounded slices; `cost.jsonl` logs tokens per call. A capped sweep reads the OLDEST unread commits and the next sweep goes on from there (before 0.6.2 it kept the newest and skipped the rest for good). Every commit read is recorded (`read_commits`), with or without an insight, and the watermark moves only over commits read.
- **Catch-up:** commits an older diwa skipped are read again by setting a horizon: `diwa index --catch-up-since 2026-08-01`, or for the daemon `DIWA_CATCH_UP_SINCE`, or a date on the first line of `<data dir>/catch-up-since` (read every sweep; remove the file to stop). Each sweep then also reads, oldest first and within the throttle, every commit since that date that was never read.
- **Private repos:** mirrors are cloned with git, so the host needs a working git credential; `DIWA_GITHUB_BASE=git@github.com:` uses ssh. `gh` is optional.
- **If `claude` is logged out**, indexing stops without advancing the watermark and logs the error; the next poll retries. Nothing is lost.
- **Low memory:** `DIWA_EMBED=off` skips the embedding model; search falls back to keywords.
- **Backups:** `scripts/hub-replicate.sh` takes `sqlite3 .backup` copies of every index to a mounted volume (`HUB_VOLUME`) and refuses when it is not mounted. The mirrors are not copied; they re-clone.

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

Extracted insights are a cache of git: `diwa reindex` rebuilds them from scratch. Insights added, edited or removed by hand are not; they live in the index too and survive a reindex, so they are what backups protect.

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
