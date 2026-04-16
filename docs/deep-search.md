# Deep search

`diwa search <repo> "query"` is fast local search — hybrid BM25 + cosine similarity, no network calls. Returns insights ranked by relevance.

`diwa search <repo> "query" --deep` is a different beast. It's an agentic research loop that mimics how a senior engineer actually investigates something in an unfamiliar codebase.

## The problem with batch multi-query

The first cut of deep search was a fan-out strategy:

1. Claude decomposes the question into N parallel search queries.
2. Run all N queries against the local index.
3. Collect unique hits.
4. Claude synthesizes an answer from the union.

That works, but it's shallow. Real research isn't parallel — it's iterative. You search, read, follow a thread, search again with better keywords, spot-check a file, come back. Early hits change which questions are worth asking next. Batch retrieval can't do that.

## The agentic loop

Deep search was rewritten in 0.3.0 as an iterative loop. On each step, Claude picks one action based on everything accumulated so far:

- `search <query>` — run another local `diwa search`
- `git_show <sha>` — pull the full diff and message for a commit
- `read_file <path>` — inspect current code

Up to 10 steps. Each step's result feeds into the context for the next decision. The loop terminates when Claude decides it has enough to synthesize, or when the step cap is hit.

```
  user query
      │
      ▼
  ┌─────────────────────────────────────┐
  │  step loop (max 10):                │
  │                                     │
  │  context = everything seen so far   │
  │  action = Claude.decide(context)    │
  │                                     │
  │  match action:                      │
  │    search(q)   → diwa search q      │
  │    git_show(s) → git show s         │
  │    read_file   → fs read            │
  │    answer      → break              │
  └─────────────────────────────────────┘
      │
      ▼
  synthesized narrative + citations
```

Each step is one tool call plus one Claude decision. The decision is cheap (short context, bounded output); the tool calls are what gather real evidence.

## Citation tracking

Every intermediate search hit during the loop gets accumulated into a `seen_results` map keyed by commit SHA. When Claude produces the final answer, any commit SHA referenced in the answer is cross-referenced against the map to produce a **Commits** footer:

```
Sources: "The Go codebase was a disposable prototype," "Full rewrite from
Go to Rust," "Full language migration completed"

Commits:
  7142a7b  2026-03-13  cited by [1]
  4934193  2026-03-13  cited by [2] [3]
```

This matters for trust. AI-synthesized insights feel unverifiable without clear provenance. The footer gives you a flat list of SHAs to run `git show` against if you want to check a claim. The inline short-SHA tags on each individual insight serve the same purpose.

## When to use `--deep`

| Question | Use `--deep`? |
|----------|---------------|
| "why did we switch to pull-based rendering" | Yes — needs synthesis |
| "find the commit that introduced flag X" | No — one search is enough |
| "catch me up on the auth refactor" | Yes — likely multi-thread |
| "what files changed when we added Glacier support" | No — `git log` + one search |
| "what was the reason we moved from Go to Rust" | Yes — historical, cross-cutting |

Rule of thumb: shallow queries want fast search. "Why" questions want `--deep`. Open-ended "catch me up" questions are where the loop really earns its keep.

## Performance

Deep search makes 3–10 Claude calls per query (1 for each step's decision plus a final synthesis). At ~2s per call, expect 10–25 seconds end-to-end. The local search and `git show` calls are near-instant by comparison.

The 10-step cap is intentional. Research has diminishing returns after ~3 hops — if 10 steps weren't enough, the question is probably ambiguous rather than deep, and the answer will reflect that.

## Programmatic use

`--json` combines with `--deep` cleanly:

```bash
diwa search yelo "why did they rewrite in rust" --deep --json
```

Output is the synthesized answer + citation list as structured JSON, suitable for agents or downstream tooling. The [diwa Claude Code skill](../skills/diwa/SKILL.md) uses plain (non-JSON) output — Claude reads it directly and follows the SHAs with `git show`.
