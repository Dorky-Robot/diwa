#!/usr/bin/env python3
"""Compare Qwen vs the Claude-generated diwa insights for the same commits.

Standalone: does not touch diwa's source. Ports build_prompt / format_commit
(src/extract.rs), commit_diff (src/git.rs) and parse_json_array (src/claude.rs)
verbatim in behaviour, and the RawInsight serde shape, so "parse failure" means
"diwa's parser would reject it".

usage: compare.py [--n 25] [--think-n 5] [--endpoint URL] [--out DIR]
"""
import argparse, json, os, re, sqlite3, subprocess, sys, time, urllib.request

REPOS = {
    "Dorky-Robot/diwa": os.path.expanduser("~/Projects/dorky_robot/diwa"),
    "Dorky-Robot/tunnels": os.path.expanduser("~/Projects/dorky_robot/tunnels"),
}
MAX_DIFF = 2000
MAX_PROMPT_CHARS = 12000

# Extracted from src/extract.rs build_prompt (kept in sync by reading the file).
def load_prompt_head():
    src = open(os.path.expanduser("~/Projects/dorky_robot/diwa/src/extract.rs")).read()
    m = re.search(r'String::from\(\s*r#"(.*?)"#,', src, re.S)
    return m.group(1)

PROMPT_HEAD = load_prompt_head()
PROMPT_TAIL = "\nOutput the JSON array now. No markdown fences, no explanation, just the JSON.\n"

def git(repo, *a):
    return subprocess.run(["git", "-C", repo, *a], capture_output=True, text=True).stdout

def commit_data(repo, sha):
    sep = "\x1f"
    meta = git(repo, "log", "-1", f"--pretty=format:%H{sep}%s%n%b{sep}%aI{sep}%an", sha).split(sep)
    full = git(repo, "diff", f"{sha}~1..{sha}", "--stat", "--patch", "--no-color")
    if len(full) > MAX_DIFF:
        full = full[:MAX_DIFF] + "\n... (truncated)"
    files = [f for f in git(repo, "diff-tree", "--no-commit-id", "--name-only", "-r", sha).splitlines() if f]
    return dict(sha=meta[0].strip(), message=meta[1].strip(), date=meta[2].strip(), author=meta[3].strip(), diff=full, files=files)

def format_commit(c):
    e = f"<untrusted_commit_data>\n### {c['sha']} by {c['author']} on {c['date']}\n{c['message']}\n"
    if c["files"]:
        e += f"Files: {', '.join(c['files'])}\n"
    if c["diff"]:
        e += f"Diff:\n{c['diff']}\n"
    return e + "</untrusted_commit_data>\n\n"

def build_prompt(commits):
    p = PROMPT_HEAD
    for c in commits:
        p += format_commit(c)
    return p + PROMPT_TAIL

def parse_json_array(resp):
    t = resp.strip()
    cands = [t]
    for mk in ("```json\n", "```json\r\n", "```\n", "```\r\n"):
        i = t.find(mk)
        if i >= 0:
            j = t.find("```", i + len(mk))
            if j >= 0:
                cands.append(t[i + len(mk):j]); break
    s, e = t.find("["), t.rfind("]")
    if s >= 0 and e > s:
        cands.append(t[s:e + 1])
    for c in cands:
        try:
            v = json.loads(c)
        except Exception:
            continue
        if isinstance(v, list) and all(shape_ok(x) for x in v):
            return v
    raise ValueError("could not parse JSON array (diwa parser would fail)")

def shape_ok(x):
    """serde RawInsight: title,body String required; sha/category/tags Option<String>; files Option<Vec<String>>."""
    if not isinstance(x, dict): return False
    if not isinstance(x.get("title"), str) or not isinstance(x.get("body"), str): return False
    for k in ("commit_sha", "category", "tags"):
        if x.get(k) is not None and not isinstance(x[k], str): return False
    f = x.get("files")
    if f is not None and not (isinstance(f, list) and all(isinstance(i, str) for i in f)): return False
    return True

def call(endpoint, prompt, think):
    body = {
        "model": "qwen3.8-27b",
        "messages": [{"role": "user", "content": prompt}],
        "temperature": 0.7 if not think else 1.0,
        "top_p": 0.8 if not think else 0.95,
        "presence_penalty": 1.5 if not think else 0.0,
        "max_tokens": 12000 if think else 1500,
        "chat_template_kwargs": {"enable_thinking": bool(think)},
    }
    req = urllib.request.Request(endpoint + "/v1/chat/completions", json.dumps(body).encode(), {"Content-Type": "application/json"})
    t0 = time.time()
    with urllib.request.urlopen(req, timeout=1800) as r:
        d = json.load(r)
    wall = time.time() - t0
    m = d["choices"][0]["message"]
    return dict(content=m.get("content") or "", reasoning=m.get("reasoning_content") or "",
                finish=d["choices"][0].get("finish_reason"), usage=d.get("usage", {}),
                timings=d.get("timings", {}), wall=wall)

def pick(n):
    """Evenly spaced commits per repo that have source_type='git' insights and exist in git."""
    per = {"Dorky-Robot/diwa": (n + 1) // 2, "Dorky-Robot/tunnels": n // 2}
    out = []
    for repo, k in per.items():
        db = sqlite3.connect(os.path.expanduser(f"~/.diwa/{repo.replace('/', '--')}/index.db"))
        rows = db.execute("select commit_sha,category,title,body,source_type from insights where source_type='git' order by commit_date").fetchall()
        by = {}
        for r in rows: by.setdefault(r[0], []).append(r)
        shas = []
        for sha in by:
            if subprocess.run(["git", "-C", REPOS[repo], "rev-parse", "--verify", "-q", sha + "^{commit}"], capture_output=True).returncode == 0 \
               and subprocess.run(["git", "-C", REPOS[repo], "rev-parse", "--verify", "-q", sha + "~1"], capture_output=True).returncode == 0:
                shas.append(sha)
        step = max(1, len(shas) // k)
        for sha in shas[::step][:k]:
            out.append((repo, sha, by[sha]))
    return out

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=int, default=25)
    ap.add_argument("--think-n", type=int, default=5)
    ap.add_argument("--endpoint", default="http://100.104.239.33:8627")
    ap.add_argument("--out", default=os.path.dirname(os.path.abspath(__file__)))
    a = ap.parse_args()
    sel = pick(a.n)
    raw = open(os.path.join(a.out, "raw.jsonl"), "w")
    results = []
    for i, (repo, sha, claude) in enumerate(sel):
        c = commit_data(REPOS[repo], sha)
        prompt = build_prompt([c])
        row = dict(repo=repo, sha=sha, subject=c["message"].splitlines()[0], prompt_chars=len(prompt), claude=[dict(category=r[1], title=r[2], body=r[3]) for r in claude])
        for mode in ["nothink"] + (["think"] if i < a.think_n else []):
            try:
                r = call(a.endpoint, prompt, mode == "think")
                try:
                    parsed = parse_json_array(r["content"]); err = None
                except Exception as ex:
                    parsed, err = None, str(ex)
            except Exception as ex:
                r, parsed, err = dict(content="", reasoning="", finish="error", usage={}, timings={}, wall=0), None, f"request failed: {ex}"
            row[mode] = dict(parsed=parsed, parse_error=err, **r)
            raw.write(json.dumps(dict(repo=repo, sha=sha, mode=mode, prompt=prompt, **r, parse_error=err)) + "\n"); raw.flush()
            t = r["timings"]
            print(f"[{i+1}/{len(sel)}] {repo.split('/')[1]} {sha[:8]} {mode}: in={r['usage'].get('prompt_tokens')} out={r['usage'].get('completion_tokens')} "
                  f"gen={t.get('predicted_per_second', 0):.1f} t/s wall={r['wall']:.0f}s parse={'OK' if parsed is not None else 'FAIL'}", flush=True)
        results.append(row)
    json.dump(results, open(os.path.join(a.out, "results.json"), "w"), indent=1)
    write_md(results, os.path.join(a.out, "results.md"))

def write_md(results, path):
    nt = [r["nothink"] for r in results]
    fails = sum(1 for x in nt if x["parsed"] is None)
    th = [r for r in results if "think" in r]
    L = ["# Qwen3.8-27B vs Claude diwa insights\n",
         f"- commits: {len(results)}; non-thinking parse failures: **{fails}**; thinking runs: {len(th)}, parse failures {sum(1 for r in th if r['think']['parsed'] is None)}",
         f"- mean prompt tokens {avg([x['usage'].get('prompt_tokens', 0) for x in nt]):.0f}, mean output tokens {avg([x['usage'].get('completion_tokens', 0) for x in nt]):.0f}, "
         f"mean gen speed {avg([x['timings'].get('predicted_per_second', 0) for x in nt]):.1f} t/s, mean wall {avg([x['wall'] for x in nt]):.1f}s",
         f"- thinking: mean output tokens {avg([r['think']['usage'].get('completion_tokens', 0) for r in th]):.0f}, mean wall {avg([r['think']['wall'] for r in th]):.1f}s\n",
         "| # | repo | sha | subject | prompt tok | out tok | wall s | parse | think wall s | think parse |", "|---|---|---|---|---|---|---|---|---|---|"]
    for i, r in enumerate(results, 1):
        n = r["nothink"]; t = r.get("think")
        L.append(f"| {i} | {r['repo'].split('/')[1]} | {r['sha'][:8]} | {r['subject'][:60].replace('|','/')} | {n['usage'].get('prompt_tokens')} | {n['usage'].get('completion_tokens')} | {n['wall']:.0f} | {'ok' if n['parsed'] is not None else 'FAIL'} | "
                 + (f"{t['wall']:.0f} | {'ok' if t['parsed'] is not None else 'FAIL'} |" if t else "- | - |"))
    L.append("\n## Side by side\n")
    for i, r in enumerate(results, 1):
        L.append(f"### {i}. {r['repo']} {r['sha'][:8]} - {r['subject']}\n")
        for c in r["claude"]:
            L.append(f"**Claude** [{c['category']}] {c['title']}\n> {c['body']}\n")
        for mode in ("nothink", "think"):
            if mode not in r: continue
            x = r[mode]
            if x["parsed"] is None:
                L.append(f"**Qwen {mode}** PARSE FAIL ({x['parse_error']}); raw head: `{x['content'][:200]!r}`\n")
            elif not x["parsed"]:
                L.append(f"**Qwen {mode}** [] (skipped as trivial)\n")
            else:
                for q in x["parsed"]:
                    L.append(f"**Qwen {mode}** [{q.get('category')}] {q['title']}\n> {q['body']}\n")
    open(path, "w").write("\n".join(L))

def avg(v): return sum(v) / len(v) if v else 0

if __name__ == "__main__":
    main()
