# Reflections

diwa extracts insights at two levels:

- **Level 1 — per-commit insights.** What happened, why this decision, what was learned. One or more per commit.
- **Level 2 — reflections.** Patterns across many commits. The kind of thing a senior dev would say in a retro. These look at 20+ insights at once and find the arc, the recurring antagonist, the architectural tension that explains a bunch of individual changes.

Level 1 is mechanical — commit in, insight out. Level 2 is slower, more expensive, and much more valuable.

## Why two levels

Per-commit insights tell you what happened. They're dense but narrow. If you search "why did we switch from Go to Rust," level 1 gives you the migration commit and maybe the commits leading up to it.

Reflections tell you what it *meant*. They're the answer to questions like:

- "What pattern keeps repeating in this project's bug history?"
- "Why is there so much churn in the hook installation code?"
- "What's the common thread across all the ONNX-related commits?"

The reflections diwa extracted about itself are a good preview — one reflection identified that "ONNX Runtime distribution was the recurring antagonist — solved differently on every platform," which pulls together three commits across two months into one coherent story. That's not something you'd get from reading any single commit.

## Grounding: reflections can't hallucinate

The risk with a level 2 pass is obvious: ask an LLM to find patterns and it will find patterns, real or not. The solution is that every reflection must be grounded in evidence from the actual repo.

During a reflection pass, Claude gets four inputs:

1. The full set of level 1 insights.
2. `git log` output (summary of the history being reflected on).
3. The current file tree.
4. Contents of key files referenced in the insights.
5. Any merged PRs (via `gh`) for title/description/review comments.

Claude is instructed to only emit a reflection when the claim is verifiable against at least one of those grounding sources. In practice this means reflections cite commit SHAs, reference file names, and quote specific lines from PR descriptions. Claims that can't be tied to evidence get dropped before the reflection is stored.

This grounding pass is what separates reflections from "generic AI hot takes about your repo."

## When reflections regenerate

Reflections went through four iterations — each recalibrating the same tradeoff between freshness and Claude API cost:

| Trigger | Problem |
|---------|---------|
| Every commit | Expensive on busy branches. Reflections don't even change commit-to-commit. |
| LLM-decided ("reflect now?") | Fine in theory, but the extra Claude call runs on every commit. Same cost problem. |
| Every 10 new insights | Better — but still wasteful if a repo is idle. |
| **Every 7 days, regardless of insight count** | Current. Reflections capture slow-moving arcs, not individual events — periodic is the right granularity. |

On the indexing path, after insights are persisted, diwa checks whether the last reflection pass was more than 7 days ago. If yes, it runs a reflection. If no, it skips.

This means:

- A repo you commit to daily gets fresh reflections weekly without any cost spikes.
- A repo that's been idle for months gets fresh reflections on your next commit — no stale reflections hanging around.
- You can force regeneration with `diwa reflect <repo>` whenever you want.

Old reflections are *cleared* before new ones are written. No accumulation of stale level 2 entries.

## Escape hatch

```bash
diwa reflect <repo>
```

Forces a full reflection regeneration right now. Useful after a big batch of commits (like completing a feature) when you want the retro-style view immediately instead of waiting for the 7-day timer. Also useful if reflections seem stale — they'll clear and regenerate.

## What reflections look like

Reflections look like regular insights in the UI but are tagged `reflection` and cite multiple commits instead of one:

```
[reflection] ONNX Runtime distribution was the recurring antagonist —
solved differently on every platform
  The ort-sys dependency created platform-specific problems that required
  three distinct solutions: dropping x86_64-apple-darwin from CI entirely
  (a2f413d), adding load-dynamic as a Cargo feature for glibc decoupling
  on Linux (45fd18b), then re-adding Intel Mac (32cc287) by combining
  load-dynamic with a Homebrew onnxruntime dependency. ...

  commit: 32cc287 | reflection
  tags: onnx distribution platform-portability recurring-cost
```

They're generally 3–5x longer than level 1 insights because they're telling a story, not describing a change.

## Cost tuning

If you want to turn reflections off entirely (e.g., running diwa offline without Claude access for level 2), the simplest route is just to never run `diwa reflect` and let the 7-day timer fire with no reflections generated. The level 1 pipeline doesn't depend on level 2.

If you want reflections *more* often than every 7 days, `diwa reflect` is the lever — wire it into CI or a cron job on your preferred cadence.
