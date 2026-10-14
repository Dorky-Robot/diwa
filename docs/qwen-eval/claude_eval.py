#!/usr/bin/env python3
"""Run the qwen-eval 25-commit sample through `claude -p` the way diwa will call it.
usage: claude_eval.py <label> <model> [effort|-]   -> writes claude_<label>.json next to this file
"""
import json, os, subprocess, sys, time
from concurrent.futures import ThreadPoolExecutor
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import compare as C

HERE = os.path.dirname(os.path.abspath(__file__))
label, model, effort = sys.argv[1], sys.argv[2], (sys.argv[3] if len(sys.argv) > 3 else "-")
rows = json.load(open(os.path.join(HERE, "results.json")))

def lenient(t):
    """valid JSON array of {title, body} objects, ignoring the tags/files type"""
    for cand in (t.strip(), t[t.find("["):t.rfind("]") + 1]):
        try:
            v = json.loads(cand)
            return isinstance(v, list) and all(isinstance(x, dict) and isinstance(x.get("title"), str) and isinstance(x.get("body"), str) for x in v)
        except Exception: pass
    return False

def run(row):
    c = C.commit_data(C.REPOS[row["repo"]], row["sha"])
    prompt = C.build_prompt([c])
    cmd = ["claude", "-p", "--output-format", "json", "--setting-sources", "project", "--model", model,
           "--no-session-persistence", "--tools", "", "--strict-mcp-config", "--disable-slash-commands", "--system-prompt", "You extract structured data from git commits. Output only what is asked."]
    if effort != "-": cmd += ["--effort", effort]
    t0 = time.time()
    p = subprocess.run(cmd, input=prompt, capture_output=True, text=True, cwd="/tmp", timeout=300)
    wall = time.time() - t0
    out = dict(repo=row["repo"], sha=row["sha"], subject=row["subject"], wall=wall)
    try:
        d = json.loads(p.stdout); u = d["usage"]
        out.update(result=d["result"], in_tok=u["input_tokens"] + u["cache_creation_input_tokens"] + u["cache_read_input_tokens"],
                   out_tok=u["output_tokens"], think_tok=u.get("output_tokens_details", {}).get("thinking_tokens", 0), cost=d["total_cost_usd"], api_ms=d["duration_ms"], is_error=d["is_error"])
        try: out["parsed"] = C.parse_json_array(d["result"]); out["parse_error"] = None
        except Exception as e: out["parsed"] = None; out["parse_error"] = str(e)
        out["lenient"] = lenient(d["result"])
    except Exception as e:
        out.update(parsed=None, parse_error=f"call failed: {e} {p.stderr[:200]} {p.stdout[:200]}")
    return out

with ThreadPoolExecutor(4) as ex: res = list(ex.map(run, rows))
json.dump(res, open(os.path.join(HERE, f"claude_{label}.json"), "w"), indent=1)
ok = [r for r in res if r["parsed"] is not None]
print("lenient-valid", sum(1 for r in res if r.get("lenient")), "/", len(res), "median in", sorted(r.get("in_tok",0) for r in res)[len(res)//2])
print(label, "valid", len(ok), "/", len(res), "mean wall", sum(r["wall"] for r in res)/len(res),
      "mean in", sum(r.get("in_tok",0) for r in res)/len(res), "mean out", sum(r.get("out_tok",0) for r in res)/len(res),
      "cost", sum(r.get("cost",0) for r in res), "insights", sum(len(r["parsed"]) for r in ok))
