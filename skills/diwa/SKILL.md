---
name: diwa
description: Use `diwa` as the FIRST step for ANY context-gathering about a feature, branch, PR, or area of a repo — BEFORE reaching for `git log`, `git branch -a`, `git log --grep`, or `grep`. diwa indexes git history into searchable insights and returns commit SHAs you then feed into `git show` / file reads for the rest of the picture. Treat the first search as a SEED, not the answer — clues in the returned commits (other SHAs, PR numbers, file names, flags, "follows up on…" phrases) become follow-up `diwa search` queries, and when independent threads emerge, fork parallel `Agent` subagents (Explore type) to walk each rabbit hole and report back. TRIGGER on ALL of these phrasings (non-exhaustive): "catch me up", "get caught up", "caught up on X", "where were we", "what's the state of X", "keep working on X", "resume X", "sync me on X", "remind me about X", "what's happening with X", plus rationale questions ("why did we…", "what was the reason for…"), and any time you're about to run `git log --grep` or `git branch -a | grep` to reconstruct context. Also TRIGGER when starting work in an unfamiliar repo. DO NOT TRIGGER for trivial single-file edits or questions answerable by reading one specific file.
---

# diwa — git history knowledge base

`diwa` (`/opt/homebrew/bin/diwa`) indexes git commits into a searchable knowledge base of decisions, learnings, and architectural patterns. It extracts *why*, not just *what*. Most importantly, **it returns commit SHAs alongside insights**, which you then use as jumping-off points for deeper investigation with `git show`, `git log <sha>`, and file reads.

## CRITICAL: diwa is stage 1, not the whole job

When the user asks you to get caught up on a feature, your instinct will be `git log --grep=<feature>` or `git branch -a | grep <feature>`. **Resist that instinct.** The correct sequence is:

1. **`diwa search`** — get insights + commit SHAs
2. **`git show <sha>`** for each relevant commit returned by diwa — get the actual diffs and full commit messages
3. **Follow the strings** — clues in the diffs/messages become new `diwa search` queries. Iterate until the picture is whole.
4. **File reads / `gh pr view`** — fill in anything diwa + git show + iteration didn't cover (PR description, review comments, current state of files)

Running `git log` before diwa means you're reconstructing context the slow way when diwa already did the work. Running diwa without then pulling the referenced commits means you have insights without grounding. Stopping after one search means you missed the rabbit holes that actually explain the *why*.

## Trigger phrases that MUST invoke diwa

Before touching `git log`, `git branch`, or `grep` for context reconstruction, check: did the user say any of these?

- "catch me up" / "get caught up" / "caught up on X"
- "keep working on X" / "resume X" / "continue with X"
- "where were we" / "what's the state of X" / "what's happening with X"
- "sync me" / "remind me about X"
- "why did we…" / "what was the reason for…"
- Any mention of a feature/branch name combined with "context" or "history"
- Starting work in an unfamiliar repo

If yes → `diwa search` FIRST. No exceptions absent the DO NOT TRIGGER conditions below.

## Workflow

### Step 1 — Confirm the repo is indexed

```
diwa ls
```

Lists indexed repos with insight counts. If the current repo isn't listed, skip diwa and fall back to normal exploration. Don't suggest indexing unless the user asks.

### Step 2 — Search

```
diwa search <repo> "<query>"
```

- `<repo>` is the name (`katulong`, `Dorky-Robot/katulong`) or a path
- `-n <N>` caps results (default 10)
- Query should be substantive — feature names, concepts, decisions. Not single keywords.
- Run 2–4 searches with different angles if the topic is broad (the example run showed "dispatch visible sessions refine execute", "dispatch v2 batch refine multi-project", "dispatch inert pipeline wiring" — different facets of the same feature)

Use `--deep` when:
- Shallow results are thin
- The question is a "why" that needs synthesis
- You need diwa to cross-reference multiple commits for you

```
diwa search <repo> "<query>" --deep
```

### Step 3 — Follow the commit SHAs

diwa's output includes `[<sha>]` references next to each insight. **These are the real payoff.** For each SHA that looks relevant:

```
git -C <repo-path> show <sha>                       # full diff + commit message
git -C <repo-path> log <sha>^..<sha> --stat         # just the changed files + stats
git -C <repo-path> log <sha>~5..<sha> --oneline     # nearby commits for context
```

Read commits in batch — but don't summarize yet. The next step is what turns a list of commits into a real understanding.

### Step 4 — Follow the strings (iterate)

The first diwa search is rarely the whole answer. Commits surfaced in step 3 contain clues that point to other threads. **Each clue is a potential new `diwa search`.** Treat the investigation as a tree, not a single query.

Clues to watch for in commit messages and diffs:

- Other commit SHAs referenced ("follows up on abc123", "reverts def456", "see also …")
- PR / issue numbers (`#475`, `PR-123`)
- Branch names you haven't seen yet
- Function, class, or file names that hint at a related subsystem
- Config keys, feature flags, env vars, migration names
- Phrases like "as discussed in", "part of the X effort", "blocked on", "depends on"
- Author / co-author handles whose other work might be related
- Earlier or later dates that suggest a multi-phase rollout

For each clue that feels load-bearing, run a follow-up:

```
diwa search <repo> "<concrete name or phrase from the clue>"
```

Then `git show` any new SHAs and look for *their* clues. Repeat.

**When to stop iterating:**

- New searches return SHAs you've already inspected (the tree has converged)
- Insights start repeating themselves
- You have enough to answer the user's original question
- You're ~3 hops deep — diminishing returns kick in fast after that

**When to keep going:**

- A clue points to a *why* you still don't understand
- The user asked an open-ended "catch me up" and there are still untouched threads
- You found a SHA that looks central but you haven't read its predecessors/successors

### Step 5 — Fill gaps with PRs, files, and targeted git

Only after diwa + git show, if context is still incomplete:
- `gh pr view <num>` / `gh pr view <num> --comments` for PR body and discussion
- Read specific files mentioned in commits
- `git log <branch> --oneline` only now, to see recent commits not yet indexed by diwa

## Forking rabbit holes in parallel

When step 4 surfaces multiple *independent* threads, don't walk them serially in the main conversation — fan out. Spawn one subagent per thread with the `Agent` tool (`Explore` is the right `subagent_type` for this work). Each subagent runs its own diwa+git-show iteration loop on its assigned thread, then reports a concise synthesis back. The main thread stays clean for the final assembly.

**Fork when:**

- Two or more threads emerge that don't share SHAs or files (e.g., a feature pulls in a refactor of subsystem A *and* a migration to subsystem B — each is a self-contained rabbit hole)
- Each thread is worth more than one diwa search of work
- You want to preserve main-thread context for the final synthesis instead of burning it on raw `git show` output
- The user asked an open-ended "catch me up" where breadth matters

**Don't fork when:**

- The threads reference each other heavily (they need shared context — splitting just causes duplicate work)
- Only one obvious thread exists
- The original question is narrow and a single chain of `diwa search → git show` answers it
- You'd be spawning an agent just to run a single command

**How to brief each subagent** — the prompt must be self-contained because the subagent has none of your context:

- The repo name/path and how to invoke diwa against it
- The exact thread it owns (one sentence: "investigate the migration from X to Y")
- Seed SHAs and seed search queries you already know are relevant
- The workflow: `diwa search` → `git show` on returned SHAs → follow clues with more `diwa search` → stop when the tree converges
- An explicit list of threads it should *not* touch (the other agents own those — no duplicate work)
- The shape of the report you want back: a tight synthesis (under ~300 words), with SHAs cited, surprises called out, and open questions flagged
- Tell it to read commits but not to make changes

Launch all the subagents in a single message (parallel tool calls). When their reports come back, synthesize them into one answer for the user, citing SHAs from each thread. If a subagent's report points to a *new* thread that crosses into another agent's territory, decide whether to fan out a second wave or pull the thread yourself in the main conversation.

## When to prefer diwa over alternatives

| Question | First tool | Then |
|---|---|---|
| "Catch me up on feature X" | `diwa search <repo> "X"` | `git show <sha>` on returned commits |
| "What's the state of PR #475?" | `diwa search <repo> "<PR topic>"` | `gh pr view 475` + `git show` on SHAs |
| "Why was X designed this way?" | `diwa search … --deep` | `git show <sha>` |
| "What was the reason we switched from A to B?" | `diwa search …` | `git show <sha>` |
| "Has this bug happened before?" | `diwa search …` | `git show <sha>` of prior incidents |
| "What does this function do right now?" | Read the file | — |
| "Who last touched line 42?" | `git blame` | — |
| "What changed in the last commit?" | `git log -1` / `git diff HEAD^` | — |

## DO NOT TRIGGER for

- Trivial single-file edits ("fix this typo", "rename this variable")
- Questions answerable by reading one specific file
- Reading the current state of code (that's what file reads are for)
- When the user explicitly says to skip history / ignore context research

## Caveats

- Only works in indexed repos — `diwa ls` first.
- Insights reflect the last index run. Very recent commits may not be covered — check with `git log <branch> --oneline` as a final step if recency matters.
- Never fabricate commit SHAs. If diwa returns no hits, say so and fall back to `git log --grep`.
